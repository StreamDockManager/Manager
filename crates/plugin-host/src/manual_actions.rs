//! Temporary runtime demand and contexts for actions launched from Manager
use super::*;
use std::time::Instant;

// Standard vendor plugins may handle a pulse asynchronously without sending a receipt
const ACTION_GRACE: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct ManualActions {
    pending: HashMap<uuid::Uuid, String>,
    contexts: HashMap<String, (String, Instant)>,
    changed: bool,
}
impl ManualActions {
    // Check whether manual execution still requires the plugin
    pub(super) fn demanded(&self, id: &str) -> bool {
        self.pending.values().any(|owner| owner == id)
            || self.contexts.values().any(|(owner, _)| owner == id)
    }
}

pub struct ManualActionLease {
    hub: Arc<Mutex<Hub>>,
    request: uuid::Uuid,
    plugin: String,
    action: String,
}
impl Drop for ManualActionLease {
    // Release resources held by this owner
    fn drop(&mut self) {
        let mut hub = self.hub.lock().unwrap();
        if hub.manual_actions.pending.remove(&self.request).is_some()
            && !hub.manual_actions.demanded(&self.plugin)
        {
            hub.manual_actions.changed = true;
            if let Some(wake) = &hub.wake {
                wake();
            }
        }
    }
}

impl Hub {
    // Revoke manual actions
    pub(super) fn revoke_manual_actions(&mut self, id: &str) {
        self.manual_actions.pending.retain(|_, owner| owner != id);
        let contexts: Vec<_> = self
            .manual_actions
            .contexts
            .iter()
            .filter(|(_, (owner, _))| owner == id)
            .map(|(context, _)| context.clone())
            .collect();
        self.retire_manual_contexts(contexts);
    }

    // Forget manual contexts
    pub(super) fn forget_manual_contexts(&mut self, id: &str) {
        self.manual_actions.contexts.retain(|context, (owner, _)| {
            if owner != id {
                return true;
            }
            self.context_state.transient_scopes.remove(context);
            false
        });
    }

    // Retire manual actions whose deadlines have elapsed
    pub(super) fn expire_manual_actions(&mut self, now: Instant) -> bool {
        let expired: Vec<_> = self
            .manual_actions
            .contexts
            .iter()
            .filter(|(_, (_, deadline))| now >= *deadline)
            .map(|(context, _)| context.clone())
            .collect();
        let changed = std::mem::take(&mut self.manual_actions.changed) || !expired.is_empty();
        self.retire_manual_contexts(expired);
        changed
    }

    // Retire contexts owned by completed manual actions
    fn retire_manual_contexts(&mut self, contexts: Vec<String>) {
        for context in contexts {
            self.manual_actions.contexts.remove(&context);
            if let Some(release) = self.retire_temporary_action_context(&context) {
                self.effects.push_back(release);
            }
        }
    }
}

impl PluginHost {
    // Resolve the plugin owning the manual action
    fn manual_action_plugin(&self, id: &str, action: &str) -> Result<&Plugin, String> {
        self.recovery.check()?;
        let plugin = self.plugins.get(id).ok_or("Plugin not installed")?;
        if !plugin.info.enabled {
            return Err(format!(
                "Enable {} before running its macro action",
                plugin.info.name
            ));
        }
        if self.replacement_claims.lock().unwrap().contains(id) {
            return Err(format!("{} is being updated", plugin.info.name));
        }
        let action = plugin
            .info
            .actions
            .iter()
            .find(|item| item.id == action)
            .ok_or("Action is not owned by this plugin")?;
        if !action
            .control_types
            .iter()
            .any(|kind| matches!(kind.as_str(), "button" | "encoder"))
        {
            return Err("This plugin action requires a bound control".into());
        }
        if plugin.info.runtime.state == RuntimeState::Blocked {
            return Err(format!("{} · {}", plugin.info.name, plugin.info.status));
        }
        Ok(plugin)
    }

    // Begin manual action
    pub fn begin_manual_action(&self, id: &str, action: &str) -> Result<ManualActionLease, String> {
        self.manual_action_plugin(id, action)?;
        let request = uuid::Uuid::new_v4();
        let mut hub = self.hub.lock().unwrap();
        let new_demand = !hub.manual_actions.demanded(id);
        hub.manual_actions.pending.insert(request, id.into());
        if new_demand {
            hub.manual_actions.changed = true;
            if let Some(wake) = &hub.wake {
                wake();
            }
        }
        Ok(ManualActionLease {
            hub: self.hub.clone(),
            request,
            plugin: id.into(),
            action: action.into(),
        })
    }

    /// A pending lease lets the regular host poll start the plugin. Send the
    /// pulse only after registration, then keep the runtime warm for asynchronous vendor work.
    #[allow(clippy::too_many_arguments)]
    pub fn try_dispatch_manual_action(
        &mut self,
        lease: &ManualActionLease,
        device: &str,
        control: &str,
        scope: &str,
        durable: &Value,
        resolved: &Value,
        user_desired_state: Option<u8>,
        secrets: impl FnOnce() -> Vec<String>,
    ) -> Result<bool, String> {
        let id = &lease.plugin;
        let action = &lease.action;
        let plugin = self.manual_action_plugin(id, action)?;
        {
            let hub = self.hub.lock().unwrap();
            if !hub.manual_actions.pending.contains_key(&lease.request) {
                return Err("The plugin's manual action was cancelled".into());
            }
            if plugin.info.runtime.state != RuntimeState::Running
                || !hub.sessions.connected.contains(&plugin.token)
            {
                return Ok(false);
            }
        }
        let mut context = self
            .hub
            .lock()
            .unwrap()
            .binding_context(id, scope, device, control);
        {
            let mut hub = self.hub.lock().unwrap();
            if hub
                .context_state
                .context_actions
                .get(&context)
                .is_some_and(|previous| previous != action)
            {
                hub.retire_manual_contexts(vec![context.clone()]);
                context = hub.binding_context(id, scope, device, control);
            }
        }
        self.begin_temporary_context(id, scope, device, control)?;
        {
            let mut hub = self.hub.lock().unwrap();
            if hub.context_state.configured.get(&context) != Some(durable) {
                hub.context_state
                    .configured
                    .insert(context.clone(), durable.clone());
                hub.context_state
                    .settings
                    .insert(context.clone(), durable.clone());
            }
        }
        self.set_execution_settings(id, scope, device, control, durable, resolved, secrets());
        let result = self.dispatch_with_state(
            id,
            action,
            device,
            control,
            scope,
            &hardware_core::Input::Pulse,
            durable,
            user_desired_state,
        );
        let mut hub = self.hub.lock().unwrap();
        if let Err(error) = result {
            if !hub.manual_actions.contexts.contains_key(&context) {
                hub.retire_temporary_action_context(&context);
            }
            return Err(error);
        }
        hub.manual_actions
            .contexts
            .insert(context, (id.clone(), Instant::now() + ACTION_GRACE));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify manual demand expires and preserves profile demand
    fn manual_demand_expires_and_preserves_profile_demand() {
        const PLUGIN: &str = "fixture.manual";
        const ACTION: &str = "fixture.manual.action";
        for profile_demand in [false, true] {
            let root = tempfile::tempdir().unwrap();
            lifecycle_tests::package(&root.path().join("fixture"), PLUGIN);
            let mut host = lifecycle_tests::host(root.path());
            assert!(host.begin_manual_action(PLUGIN, ACTION).is_err());
            host.enable(PLUGIN, true).unwrap();
            // Explicit background mode keeps profile demand alive after a manual lease expires
            host.set_sleep_when_hidden(PLUGIN, false).unwrap();
            let abandoned = host.begin_manual_action(PLUGIN, ACTION).unwrap();
            drop(abandoned);
            host.poll();
            assert!(host.browser_view(PLUGIN).unwrap().is_none());

            let lease = host.begin_manual_action(PLUGIN, ACTION).unwrap();
            host.poll();
            assert!(host.browser_view(PLUGIN).unwrap().is_some());
            let settings = json!({});
            assert!(!host
                .try_dispatch_manual_action(
                    &lease,
                    "",
                    "macro:test",
                    "profile:page@0",
                    &settings,
                    &settings,
                    None,
                    Vec::new,
                )
                .unwrap());
            let session = host.plugins[PLUGIN].token.clone();
            host.hub.lock().unwrap().sessions.connected.insert(session);
            host.poll();
            assert!(host
                .try_dispatch_manual_action(
                    &lease,
                    "",
                    "macro:test",
                    "profile:page@0",
                    &settings,
                    &settings,
                    None,
                    Vec::new,
                )
                .unwrap());
            drop(lease);
            if profile_demand {
                host.reconcile_demand(HashMap::from([(
                    PLUGIN.into(),
                    vec![PluginDemand {
                        device_id: "dock".into(),
                        profile_id: "profile".into(),
                    }],
                )]));
            }
            {
                let mut hub = host.hub.lock().unwrap();
                let context = hub
                    .lookup_binding_context(PLUGIN, "profile:page@0", "", "macro:test")
                    .unwrap();
                assert!(hub.context_state.contexts.contains_key(&context));
                assert!(hub.expire_manual_actions(Instant::now() + ACTION_GRACE));
                assert!(!hub.context_state.contexts.contains_key(&context));
                assert!(hub.manual_actions.contexts.is_empty());
            }
            host.poll();
            assert_eq!(host.browser_view(PLUGIN).unwrap().is_some(), profile_demand);
        }
    }
}
