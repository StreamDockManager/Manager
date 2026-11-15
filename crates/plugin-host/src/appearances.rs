//! Durable binding addresses and ephemeral protocol appearances are separate
use super::*;
use std::time::{Duration, Instant};

impl Hub {
    // Prepare plugin visual
    pub(super) fn prepare_plugin_visual(
        &mut self,
        owner: &str,
        message: &mut Value,
        now: Instant,
    ) -> Result<(), String> {
        visual::prepare(self, owner, message)?;
        if matches!(
            message["event"].as_str(),
            Some("setImage" | "setTitle" | "setState" | "setFeedback")
        ) {
            if let Some(context) = message["context"].as_str() {
                self.context_state
                    .content_updates
                    .insert(context.into(), now);
            }
        }
        Ok(())
    }
    // Hide appearance
    pub(super) fn hide_appearance(&mut self, context: &str) {
        if let Some(owner) = self.context_state.contexts.get(context).cloned() {
            self.effects
                .push_back(HostEffect::release_context(&owner, context));
        }
        if let Some(mut event) = self.context_state.visible.remove(context) {
            self.visual_state.render_generation =
                self.visual_state.render_generation.wrapping_add(1);
            if let Some(owner) = self.retire_context(context) {
                event["event"] = json!("willDisappear");
                let queue = self.sessions.queues.entry(owner).or_default();
                queue.push_back(event);
            }
            self.context_state.targets.remove(context);
            self.visual_state.render_generation =
                self.visual_state.render_generation.wrapping_add(1);
            self.visual_state.remove(context);
            self.context_state.forget_authoring(context);
            let tokens: Vec<_> = self
                .sessions
                .files
                .iter()
                .filter(|(_, g)| g.context.as_deref() == Some(context))
                .map(|(t, _)| t.clone())
                .collect();
            for token in tokens {
                // A draft inspector owns its settings until it is explicitly closed
                if self.context_state.drafts.contains(context) {
                    continue;
                }
                self.revoke_storage_session(&token);
                self.sessions.tokens.remove(&token);
                self.sessions.files.remove(&token);
                self.sessions.queues.remove(&format!("pi:{token}"));
            }
        }
    }

    // Derive the identity key for a plugin binding
    fn binding_key(id: &str, scope: &str, device: &str, control: &str) -> String {
        // Structured identities avoid ambiguity in device paths and nested scopes
        format!(
            "{id}:{}",
            serde_json::to_string(&(scope, device, control)).expect("binding identity")
        )
    }
    // Lookup binding context
    pub(super) fn lookup_binding_context(
        &self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
    ) -> Option<String> {
        self.context_state
            .binding_contexts
            .get(&Self::binding_key(id, scope, device, control))
            .cloned()
    }
    // Resolve the live context for a plugin binding
    pub(super) fn binding_context(
        &mut self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
    ) -> String {
        let key = Self::binding_key(id, scope, device, control);
        if let Some(context) = self.context_state.binding_contexts.get(&key) {
            return context.clone();
        }
        let context = format!("{id}:{}", uuid::Uuid::new_v4());
        if let Some(settings) = self.context_state.binding_settings.remove(&key) {
            self.context_state
                .settings
                .insert(context.clone(), settings);
        }
        self.context_state
            .binding_contexts
            .insert(key, context.clone());
        context
    }
    // Replace the binding context for a new action lifecycle
    pub(super) fn renew_binding_context(
        &mut self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
    ) -> String {
        let key = Self::binding_key(id, scope, device, control);
        let previous = self.context_state.binding_contexts.remove(&key);
        let next = self.binding_context(id, scope, device, control);
        // The synchronizer will retire the old visible appearance. Carry only
        // durable authoring metadata and parameter resolution, never pixels.
        if let Some(old) = previous {
            for map in [
                &mut self.context_state.addresses,
                &mut self.context_state.execution,
                &mut self.context_state.reference_sources,
            ] {
                if let Some(value) = map.get(&old).cloned() {
                    map.insert(next.clone(), value);
                }
            }
        }
        next
    }
}
impl PluginHost {
    /// Only sources actually routed to a rendered surface participate. Failed,
    /// disabled and missing plugins use normal destination fallbacks immediately.
    /// No new wire readiness API is required
    pub fn transition_readiness(
        &self,
        device: &str,
        sources: &[(String, String)],
        now: Instant,
    ) -> bool {
        let hub = self.hub.lock().unwrap();
        if hub
            .context_state
            .pending_appearances
            .iter()
            .any(|(owner, d, control, scope)| {
                d == device
                    && sources.iter().any(|(source, parent)| {
                        source == control
                            && (scope == parent || scope.starts_with(&format!("{parent}@")))
                    })
                    && self.plugins.get(owner).is_some_and(|p| {
                        p.info.enabled
                            && matches!(
                                p.info.runtime.state,
                                RuntimeState::Queued
                                    | RuntimeState::Starting
                                    | RuntimeState::Running
                            )
                    })
            })
        {
            return false;
        }
        hub.context_state
            .visible
            .iter()
            .filter(|(context, _)| {
                hub.context_state
                    .targets
                    .get(*context)
                    .is_some_and(|(d, control)| {
                        d == device
                            && sources.iter().any(|(source, scope)| {
                                source == control
                                    && hub.context_state.scopes.get(*context).is_some_and(|s| {
                                        s == scope || s.starts_with(&format!("{scope}@"))
                                    })
                            })
                    })
            })
            .all(|(context, _)| {
                let active = hub
                    .context_state
                    .contexts
                    .get(context)
                    .and_then(|id| self.plugins.get(id))
                    .is_some_and(|p| {
                        p.info.enabled
                            && matches!(
                                p.info.runtime.state,
                                RuntimeState::Starting | RuntimeState::Running
                            )
                    });
                !active
                    || hub
                        .context_state
                        .content_updates
                        .get(context)
                        .is_some_and(|updated| {
                            now.saturating_duration_since(*updated) >= Duration::from_millis(16)
                        })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Resolve the action associated with this binding
    fn action(scope: &str) -> ActiveAction {
        ActiveAction {
            plugin_id: "fixture.appearance".into(),
            action_id: "fixture.appearance.action".into(),
            device_id: "device".into(),
            control_id: "key".into(),
            scope: scope.into(),
            settings: json!({"saved":true}),
            surface: None,
            column: 0,
            row: 0,
            encoder: false,
        }
    }
    #[test]
    // Verify returning to a binding uses new context and preserves settings and retired authorization
    fn returning_to_a_binding_uses_new_context_and_preserves_settings_and_retired_authorization() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("plugin"), "fixture.appearance");
        let mut host = lifecycle_tests::host(root.path());
        host.enable("fixture.appearance", true).unwrap();
        host.plugins
            .get_mut("fixture.appearance")
            .unwrap()
            .info
            .runtime
            .state = RuntimeState::Running;
        host.synchronize(vec![action("a")], vec![]);
        let old = {
            let mut hub = host.hub.lock().unwrap();
            let context = hub
                .lookup_binding_context("fixture.appearance", "a", "device", "key")
                .unwrap();
            hub.sessions.connected.insert("session".into());
            hub.sessions
                .tokens
                .insert("session".into(), "fixture.appearance".into());
            hub.context_state
                .settings
                .insert(context.clone(), json!({"saved":true,"unknown":{"keep":42}}));
            hub.prepare_plugin_visual(
                "fixture.appearance",
                &mut json!({"event":"setTitle","context":context,"payload":{"title":"Old"}}),
                Instant::now() - Duration::from_millis(20),
            )
            .unwrap();
            context
        };
        assert!(host.transition_readiness("device", &[("key".into(), "a".into())], Instant::now()));
        host.synchronize(vec![action("b")], vec![]);
        host.synchronize(vec![action("a")], vec![]);
        let mut hub = host.hub.lock().unwrap();
        let next = hub
            .lookup_binding_context("fixture.appearance", "a", "device", "key")
            .unwrap();
        assert_ne!(old, next);
        assert_eq!(hub.context_state.settings[&next]["unknown"]["keep"], 42);
        assert!(!hub.context_state.settings.contains_key(&old));
        assert!(!hub.visual_state.visuals.contains_key(&next));
        assert!(!hub.context_state.content_updates.contains_key(&next));
        assert!(hub.discard_retired_response("session", &json!({"context":old})));
        assert!(!hub.discard_retired_response("other", &json!({"context":old})));
        assert!(!hub.discard_retired_response("session", &json!({"context":next})));
        drop(hub);
        assert!(!host.transition_readiness(
            "device",
            &[("key".into(), "a".into())],
            Instant::now()
        ));
    }
    #[test]
    // Verify queued sources wait but failed and unrouted plugins do not
    fn queued_sources_wait_but_failed_and_unrouted_plugins_do_not() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("plugin"), "fixture.appearance");
        let mut host = lifecycle_tests::host(root.path());
        host.enable("fixture.appearance", true).unwrap();
        host.plugins
            .get_mut("fixture.appearance")
            .unwrap()
            .info
            .runtime
            .state = RuntimeState::Queued;
        host.synchronize(vec![action("a")], vec![]);
        let sources = vec![("key".into(), "a".into())];
        assert!(!host.transition_readiness("device", &sources, Instant::now()));
        assert!(host.transition_readiness("other", &sources, Instant::now()));
        host.plugins
            .get_mut("fixture.appearance")
            .unwrap()
            .info
            .runtime
            .state = RuntimeState::Blocked;
        assert!(host.transition_readiness("device", &sources, Instant::now()));
    }

    #[test]
    // Verify readiness uses valid plugin content and quiet interval only for rendered sources
    fn readiness_uses_valid_plugin_content_and_quiet_interval_only_for_rendered_sources() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("plugin"), "fixture.appearance");
        let mut host = lifecycle_tests::host(root.path());
        host.enable("fixture.appearance", true).unwrap();
        host.plugins
            .get_mut("fixture.appearance")
            .unwrap()
            .info
            .runtime
            .state = RuntimeState::Running;
        host.synchronize(vec![action("a")], vec![]);
        let context = host
            .hub
            .lock()
            .unwrap()
            .lookup_binding_context("fixture.appearance", "a", "device", "key")
            .unwrap();
        let now = Instant::now();
        let sources = vec![("key".into(), "a".into())];
        // Host state defaults and transient status do not signal readiness
        {
            let mut hub = host.hub.lock().unwrap();
            let _ = visual::prepare(
                &mut hub,
                "fixture.appearance",
                &mut json!({"event":"setTitle","context":context,"payload":{"title":"Default"}}),
            );
            hub.prepare_plugin_visual(
                "fixture.appearance",
                &mut json!({"event":"showOk","context":context}),
                now,
            )
            .unwrap();
            assert!(hub.prepare_plugin_visual("fixture.appearance", &mut json!({"event":"setImage","context":context,"payload":{"image":"missing.png"}}), now).is_err());
        }
        assert!(!host.transition_readiness("device", &sources, now + Duration::from_millis(50)));
        assert!(host.transition_readiness("device", &[], now));
        assert!(host.transition_readiness("other-device", &sources, now));
        for offset in [0, 10, 20] {
            host.hub
                .lock()
                .unwrap()
                .prepare_plugin_visual(
                    "fixture.appearance",
                    &mut json!({"event":"setTitle","context":context,"payload":{"title":"Ready"}}),
                    now + Duration::from_millis(offset),
                )
                .unwrap();
        }
        assert!(!host.transition_readiness("device", &sources, now + Duration::from_millis(35)));
        assert!(host.transition_readiness("device", &sources, now + Duration::from_millis(36)));
        host.plugins
            .get_mut("fixture.appearance")
            .unwrap()
            .info
            .enabled = false;
        assert!(host.transition_readiness("device", &sources, now));
    }
}
