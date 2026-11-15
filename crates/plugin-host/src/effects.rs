//! In-process effects. Plugin wire messages never carry host routing authority
use super::*;

#[cfg(test)]
#[path = "vendor_profile_effect_tests.rs"]
mod vendor_profile_effect_tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectTarget {
    pub device: String,
    pub control: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackKind {
    Image,
    Title,
    State,
    Feedback,
    Layout,
    TriggerDescription,
    Alert,
    Ok,
    BadgeExpired,
    Unsupported,
    Other,
}
impl FeedbackKind {
    // Classify an effect as persistent state
    pub fn persistent(self) -> bool {
        matches!(
            self,
            Self::Image
                | Self::Title
                | Self::State
                | Self::Feedback
                | Self::Layout
                | Self::TriggerDescription
        )
    }
    // Classify an effect as a transient event
    pub fn transient(self) -> bool {
        matches!(self, Self::Alert | Self::Ok | Self::BadgeExpired)
    }
    // Convert the vendor event into a host effect
    fn from_event(event: &str) -> Self {
        match event {
            "setImage" => Self::Image,
            "setTitle" => Self::Title,
            "setState" => Self::State,
            "setFeedback" => Self::Feedback,
            "setFeedbackLayout" => Self::Layout,
            "setTriggerDescription" => Self::TriggerDescription,
            "showAlert" => Self::Alert,
            "showOk" => Self::Ok,
            "badgeExpired" => Self::BadgeExpired,
            "unsupportedFeature" => Self::Unsupported,
            _ => Self::Other,
        }
    }
}
#[derive(Debug, Clone)]
pub struct FeedbackEffect {
    /// Original wire envelope for diagnostics and acceptance tooling only
    /// Runtime routing always uses the typed fields below
    pub message: Value,
    pub kind: FeedbackKind,
    pub context: Option<String>,
    pub target: Option<EffectTarget>,
    pub visual: Option<Arc<Value>>,
    pub generation: Option<u64>,
    pub error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct DesktopInputEffect {
    pub plugin: String,
    pub session: Option<String>,
    pub context: String,
    pub payload: Value,
}
/// Authority comes from the authenticated plugin connection, never wire context
#[derive(Debug, Clone)]
pub struct ProfileSwitchEffect {
    pub plugin: String,
    pub session: String,
    pub device: String,
    pub profile: Option<String>,
    pub page: Option<u32>,
}
impl ProfileSwitchEffect {
    // Parse for the current operation
    pub(super) fn parse(plugin: &str, session: &str, message: &Value) -> Result<Self, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Payload {
            profile: Option<String>,
            page: Option<u32>,
        }
        let payload: Payload = serde_json::from_value(message["payload"].clone())
            .map_err(|_| "Invalid switchToProfile payload")?;
        let device = message["device"]
            .as_str()
            .filter(|v| !v.is_empty() && v.len() <= 256)
            .ok_or("switchToProfile requires a device")?;
        if payload
            .profile
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control))
            || payload.page.is_some_and(|v| v != 0)
        {
            return Err("Only bundled single-page profiles (page 0) are supported".into());
        }
        Ok(Self {
            plugin: plugin.into(),
            session: session.into(),
            device: device.into(),
            profile: payload.profile,
            page: payload.page,
        })
    }
}
#[derive(Debug, Clone)]
pub enum HostEffect {
    SwitchProfile(ProfileSwitchEffect),
    DesktopInput(DesktopInputEffect),
    OpenUrl {
        context: Option<String>,
        url: String,
    },
    Feedback(FeedbackEffect),
}
impl HostEffect {
    // Resolve the action context used by this operation
    pub fn context(&self) -> Option<&str> {
        match self {
            Self::SwitchProfile(_) => None,
            Self::DesktopInput(effect) => Some(effect.context.as_str()).filter(|s| !s.is_empty()),
            Self::OpenUrl { context, .. } => context.as_deref(),
            Self::Feedback(effect) => effect.context.as_deref(),
        }
    }
    // Convert the effect into a persistent visual update
    pub fn persistent_visual(&self) -> bool {
        matches!(self, Self::Feedback(effect) if effect.kind.persistent())
    }
    // Release context
    pub(super) fn release_context(plugin: &str, context: &str) -> Self {
        Self::DesktopInput(DesktopInputEffect {
            plugin: plugin.into(),
            session: None,
            context: context.into(),
            payload: json!({"op":"release"}),
        })
    }
    // Release session
    pub(super) fn release_session(plugin: &str, session: &str) -> Self {
        Self::DesktopInput(DesktopInputEffect {
            plugin: plugin.into(),
            session: Some(session.into()),
            context: String::new(),
            payload: json!({"op":"releaseOwner"}),
        })
    }
}
impl Hub {
    // Construct a transient feedback effect
    pub(super) fn feedback_effect(
        &self,
        kind: FeedbackKind,
        context: &str,
        generation: Option<u64>,
    ) -> HostEffect {
        HostEffect::Feedback(FeedbackEffect {
            message: json!({"event": match kind { FeedbackKind::BadgeExpired => "badgeExpired", FeedbackKind::Image => "setImage", _ => unreachable!("host-generated feedback kind") }, "context":context}),
            kind,
            context: Some(context.into()),
            target: self
                .context_state
                .targets
                .get(context)
                .map(|(device, control)| EffectTarget {
                    device: device.clone(),
                    control: control.clone(),
                }),
            visual: self.visual_state.visuals.get(context).cloned(),
            generation,
            error: None,
        })
    }
    // Construct a property inspector message effect
    pub(super) fn message_effect(
        &self,
        plugin: &str,
        session: Option<&str>,
        message: &Value,
        generation: Option<u64>,
    ) -> HostEffect {
        let context = message["context"].as_str().map(str::to_owned);
        match message["event"].as_str().unwrap_or("") {
            "desktopInput" => HostEffect::DesktopInput(DesktopInputEffect {
                plugin: plugin.into(),
                session: session.map(str::to_owned),
                context: context.unwrap_or_default(),
                payload: message["payload"].clone(),
            }),
            "openUrl" => HostEffect::OpenUrl {
                context,
                url: message["payload"]["url"].as_str().unwrap_or("").into(),
            },
            event => {
                let kind = FeedbackKind::from_event(event);
                let target = context
                    .as_deref()
                    .and_then(|c| self.context_state.targets.get(c))
                    .map(|(device, control)| EffectTarget {
                        device: device.clone(),
                        control: control.clone(),
                    });
                let visual = (kind.persistent() || kind.transient())
                    .then(|| {
                        context
                            .as_deref()
                            .and_then(|c| self.visual_state.visuals.get(c))
                            .cloned()
                    })
                    .flatten();
                HostEffect::Feedback(FeedbackEffect {
                    message: message.clone(),
                    kind,
                    context,
                    target,
                    visual,
                    generation,
                    error: (kind == FeedbackKind::Unsupported)
                        .then(|| message["payload"]["message"].as_str().map(str::to_owned))
                        .flatten(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify effect drain keeps fifo limit and wakes for remaining work
    fn effect_drain_keeps_fifo_limit_and_wakes_for_remaining_work() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::socket_host(root.path());
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let mut hub = host.hub.lock().unwrap();
            let wakes = wakes.clone();
            hub.wake = Some(Arc::new(move || {
                wakes.fetch_add(1, Ordering::Relaxed);
            }));
            for i in 0..65 {
                hub.effects.push_back(HostEffect::release_context(
                    "plugin",
                    &format!("context:{i}"),
                ));
            }
        }
        let effects = host.drain_effects();
        assert_eq!(effects.len(), 64);
        for (i, effect) in effects.iter().enumerate() {
            assert_eq!(effect.context(), Some(format!("context:{i}").as_str()));
        }
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(host.drain_effects()[0].context(), Some("context:64"));
        assert!(host.drain_effects().is_empty());
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }
    #[test]
    // Verify routing and generation come from host state not wire metadata
    fn routing_and_generation_come_from_host_state_not_wire_metadata() {
        let mut hub = Hub::default();
        hub.context_state
            .targets
            .insert("context".into(), ("device".into(), "key".into()));
        let visual = Arc::new(json!({"title":"prepared"}));
        hub.visual_state
            .visuals
            .insert("context".into(), visual.clone());
        let wire = json!({"event":"setTitle","context":"context","payload":{"title":"prepared"},
            "hostDevice":"wrong","hostControl":"wrong","hostRenderGeneration":999,"hostVisual":{"title":"unprepared"}});
        let HostEffect::Feedback(effect) =
            hub.message_effect("plugin", Some("session"), &wire, Some(12))
        else {
            panic!("feedback expected");
        };
        assert_eq!(
            effect.target,
            Some(EffectTarget {
                device: "device".into(),
                control: "key".into()
            })
        );
        assert_eq!(effect.generation, Some(12));
        assert!(Arc::ptr_eq(effect.visual.as_ref().unwrap(), &visual));
        assert!(effect.kind.persistent());
        let HostEffect::DesktopInput(input) = hub.message_effect("plugin", Some("session"), &json!({"event":"desktopInput","context":"context","hostPlugin":"wrong","hostSession":"wrong","payload":{"op":"press"}}), None) else { panic!("input expected"); };
        assert_eq!(input.plugin, "plugin");
        assert_eq!(input.session.as_deref(), Some("session"));
    }
    #[test]
    // Verify cleanup effects keep session and context scopes distinct
    fn cleanup_effects_keep_session_and_context_scopes_distinct() {
        let context = HostEffect::release_context("plugin", "context");
        assert_eq!(context.context(), Some("context"));
        assert!(!context.persistent_visual());
        let HostEffect::DesktopInput(context) = context else {
            unreachable!()
        };
        assert!(context.session.is_none());
        assert_eq!(context.payload["op"], "release");
        let session = HostEffect::release_session("plugin", "session");
        assert!(session.context().is_none());
        let HostEffect::DesktopInput(session) = session else {
            unreachable!()
        };
        assert_eq!(session.session.as_deref(), Some("session"));
        assert_eq!(session.payload["op"], "releaseOwner");
    }
    #[test]
    // Verify nonvisual messages cannot inject a visual and remain flush barriers
    fn nonvisual_messages_cannot_inject_a_visual_and_remain_flush_barriers() {
        let hub = Hub::default();
        for event in ["setSettings", "setDynamicItems", "unsupportedFeature"] {
            let effect = hub.message_effect(
                "plugin",
                None,
                &json!({"event":event,"hostVisual":{},"hostDevice":"device","hostControl":"key"}),
                None,
            );
            assert!(!effect.persistent_visual());
            let HostEffect::Feedback(effect) = effect else {
                panic!("feedback expected");
            };
            assert!(effect.visual.is_none());
            assert!(effect.target.is_none());
        }
    }
}
