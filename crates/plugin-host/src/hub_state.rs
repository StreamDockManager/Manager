//! Separate live context data, authenticated sessions, and copy-on-write visuals
//! Durable binding settings and open draft settings deliberately outlive appearances
use super::*;

#[derive(Default)]
pub(super) struct ContextState {
    pub(super) resources: HashMap<String, Value>,
    pub(super) resource_ids: HashMap<String, Value>,
    pub(super) contexts: HashMap<String, String>,
    pub(super) targets: HashMap<String, (String, String)>,
    pub(super) scopes: HashMap<String, String>,
    pub(super) addresses: HashMap<String, Value>,
    pub(super) settings: HashMap<String, Value>,
    pub(super) execution: HashMap<String, Value>,
    pub(super) reference_sources: HashMap<String, Value>,
    pub(super) drafts: std::collections::HashSet<String>,
    pub(super) transient_scopes: HashMap<String, String>,
    pub(super) visible: HashMap<String, Value>,
    pub(super) configured: HashMap<String, Value>,
    pub(super) binding_contexts: HashMap<String, String>,
    pub(super) binding_settings: HashMap<String, Value>,
    pub(super) pending_appearances: Vec<(String, String, String, String)>,
    pub(super) content_updates: HashMap<String, std::time::Instant>,
    pub(super) context_actions: HashMap<String, String>,
    pub(super) pressed_encoders: HashSet<String>,
    pub(super) title_parameters: HashMap<String, Value>,
    pub(super) vendor_profile_appearances:
        HashMap<String, hardware_core::plugin_display::VendorProfileAppearance>,
}
#[derive(Default)]
pub(super) struct Sessions {
    pub(super) tokens: HashMap<String, String>,
    pub(super) queues: HashMap<String, transport::Outgoing>,
    pub(super) connected: std::collections::HashSet<String>,
    pub(super) files: HashMap<String, FileGrant>,
    pub(super) resource_selections: HashMap<String, browser_resource_grants::ResourceSelections>,
    // Scoped native grants are never served through the shared listener
    pub(super) inspector_origins: HashMap<String, (u16, Arc<AtomicBool>)>,
    pub(super) storage_sessions: HashMap<String, Arc<AtomicBool>>,
    pub(super) shutdowns: HashMap<String, transport::Outgoing>,
    pub(super) retired_contexts: retired_contexts::RetiredContexts,
}
#[derive(Default)]
pub(super) struct VisualState {
    pub(super) visuals: HashMap<String, Arc<Value>>,
    pub(super) software_visuals: HashMap<String, Arc<Value>>,
    pub(super) software_sources: HashMap<String, visual::Sources>,
    pub(super) visual_sources: HashMap<String, visual::Sources>,
    pub(super) visual_order: HashMap<String, u64>,
    pub(super) software_order: HashMap<String, u64>,
    pub(super) visual_sequence: u64,
    pub(super) render_generation: u64,
    pub(super) visual_generation: u64,
    pub(super) visual_cow_copies: u64,
    pub(super) visual_shared_captures: u64,
    pub(super) badges: HashMap<String, std::time::Instant>,
}

impl ContextState {
    // Retire the selected state from active use
    pub(super) fn retire(&mut self, context: &str) -> Option<String> {
        if let Some(key) = self
            .binding_contexts
            .iter()
            .find_map(|(key, active)| (active == context).then(|| key.clone()))
        {
            self.binding_contexts.remove(&key);
            if !self.transient_scopes.contains_key(context) {
                if let Some(settings) = self.settings.get(context).cloned() {
                    self.binding_settings.insert(key, settings);
                }
            }
        }
        self.resources.remove(context);
        self.resource_ids.remove(context);
        self.content_updates.remove(context);
        self.pressed_encoders.remove(context);
        self.title_parameters.remove(context);
        self.vendor_profile_appearances.remove(context);
        if !self.drafts.contains(context) {
            self.settings.remove(context);
        }
        let owner = self.contexts.remove(context)?;
        self.context_actions.remove(context);
        Some(owner)
    }
    // Forget authoring
    pub(super) fn forget_authoring(&mut self, context: &str) {
        self.scopes.remove(context);
        self.addresses.remove(context);
        self.configured.remove(context);
        self.execution.remove(context);
        self.reference_sources.remove(context);
    }
}
impl Sessions {
    // Retain a bounded record of a retired action context
    pub(super) fn remember_retired_context(&mut self, owner: &str, context: &str) {
        let now = std::time::Instant::now();
        for session in &self.connected {
            if self.tokens.get(session).map(String::as_str) == Some(owner)
                && !self
                    .files
                    .get(session)
                    .is_some_and(|grant| grant.context.is_some())
            {
                self.retired_contexts.insert(session, context, now);
            }
        }
    }
    // Revoke storage
    pub(super) fn revoke_storage(&mut self, token: &str) {
        self.resource_selections.remove(token);
        if let Some(alive) = self.storage_sessions.remove(token) {
            alive.store(false, Ordering::Release);
        }
    }
}
impl VisualState {
    // Record update
    pub(super) fn record_update(&mut self, context: &str, message: &Value) {
        let event = message["event"].as_str().unwrap_or("");
        if ![
            "setTitle",
            "setImage",
            "setState",
            "setFeedback",
            "setFeedbackLayout",
            "setTriggerDescription",
            "showAlert",
            "showOk",
        ]
        .contains(&event)
        {
            return;
        }
        let target = if matches!(event, "setTitle" | "setImage") {
            message["payload"]["target"].as_u64().unwrap_or(0)
        } else {
            0
        };
        self.visual_sequence = self.visual_sequence.wrapping_add(1);
        if target != 2 {
            self.visual_order
                .insert(context.into(), self.visual_sequence);
        }
        if target != 1 {
            self.software_order
                .insert(context.into(), self.visual_sequence);
        }
    }

    // Remove for the current operation
    pub(super) fn remove(&mut self, context: &str) {
        self.visuals.remove(context);
        self.software_visuals.remove(context);
        self.software_sources.remove(context);
        self.visual_sources.remove(context);
        self.visual_order.remove(context);
        self.software_order.remove(context);
        self.badges.remove(context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify retirement preserves binding cache and open drafts but not transient settings
    fn retirement_preserves_binding_cache_and_open_drafts_but_not_transient_settings() {
        let mut contexts = ContextState::default();
        for name in ["saved", "draft", "temporary"] {
            contexts.contexts.insert(name.into(), "plugin".into());
            contexts
                .binding_contexts
                .insert(format!("binding:{name}"), name.into());
            contexts.settings.insert(name.into(), json!({"name":name}));
            contexts
                .context_actions
                .insert(name.into(), "action".into());
        }
        contexts.drafts.insert("draft".into());
        contexts
            .transient_scopes
            .insert("temporary".into(), "debug".into());
        for name in ["saved", "draft", "temporary"] {
            assert_eq!(contexts.retire(name).as_deref(), Some("plugin"));
        }
        assert!(contexts.contexts.is_empty());
        assert!(contexts.context_actions.is_empty());
        assert!(contexts.binding_contexts.is_empty());
        assert_eq!(contexts.settings.len(), 1);
        assert_eq!(contexts.settings["draft"], json!({"name":"draft"}));
        assert_eq!(contexts.binding_settings.len(), 2);
        assert!(!contexts.binding_settings.contains_key("binding:temporary"));
    }
    #[test]
    // Verify retired context history belongs only to connected plugin sessions
    fn retired_context_history_belongs_only_to_connected_plugin_sessions() {
        let mut sessions = Sessions::default();
        for (token, owner) in [
            ("active", "plugin"),
            ("inspector", "plugin"),
            ("offline", "plugin"),
            ("other", "another"),
        ] {
            sessions.tokens.insert(token.into(), owner.into());
            if token != "offline" {
                sessions.connected.insert(token.into());
            }
        }
        sessions.files.insert(
            "inspector".into(),
            FileGrant {
                root: PathBuf::new(),
                bootstrap: String::new(),
                context: Some("context".into()),
                setup_runtime: false,
            },
        );
        sessions.remember_retired_context("plugin", "context");
        let now = std::time::Instant::now();
        assert!(sessions.retired_contexts.contains("active", "context", now));
        for token in ["inspector", "offline", "other"] {
            assert!(!sessions.retired_contexts.contains(token, "context", now));
        }
    }
    #[test]
    // Verify revoking one storage session leaves other sessions alive
    fn revoking_one_storage_session_leaves_other_sessions_alive() {
        let mut sessions = Sessions::default();
        let one = Arc::new(AtomicBool::new(true));
        let two = Arc::new(AtomicBool::new(true));
        sessions.storage_sessions.insert("one".into(), one.clone());
        sessions.storage_sessions.insert("two".into(), two.clone());
        sessions.revoke_storage("one");
        assert!(!one.load(Ordering::Acquire));
        assert!(two.load(Ordering::Acquire));
        assert_eq!(sessions.storage_sessions.len(), 1);
    }
}
