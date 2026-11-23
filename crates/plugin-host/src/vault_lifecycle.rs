use super::*;

fn all_actions_require_vault(info: &PluginInfo) -> bool {
    !info.actions.is_empty()
        && info.actions.iter().all(|action| {
            info.permissions
                .as_ref()
                .and_then(|d| d.actions.get(&action.id))
                .is_some_and(|g| {
                    g.required.iter().any(|s| {
                        matches!(
                            s,
                            permissions::PermissionScope::WriteSecret { .. }
                                | permissions::PermissionScope::Access { .. }
                                | permissions::PermissionScope::ReadSecret { .. }
                                | permissions::PermissionScope::ReadLocation { .. }
                        )
                    })
                })
        })
}

impl PluginHost {
    // Pause only plugins that explicitly need the Vault without changing enable preferences
    pub fn set_vault_locked(&mut self, locked: bool) {
        let waiting: HashSet<_> = if locked {
            HashSet::new()
        } else {
            self.plugins
                .iter()
                .filter_map(|(id, plugin)| {
                    let slots =
                        plugin.manifest["StreamDockManager"]["SetupSlots"]["slots"].as_array();
                    let incomplete = slots
                        .into_iter()
                        .flatten()
                        .filter(|s| s["required"] == true)
                        .any(|s| {
                            s["action"].as_str().is_none_or(|action| {
                                self.check_action_permissions(id, action).is_err()
                            })
                        });
                    (incomplete
                        || (all_actions_require_vault(&plugin.info)
                            && plugin
                                .info
                                .actions
                                .iter()
                                .all(|a| self.check_action_permissions(id, &a.id).is_err())))
                    .then(|| id.clone())
                })
                .collect()
        };
        if locked {
            self.permission_workflow().close_vault_windows();
        }
        if self.vault_locked == locked && self.vault_setup_waiting == waiting {
            return;
        }
        self.vault_locked = locked;
        self.vault_setup_waiting = waiting;
        {
            let blocked: Vec<_> = self
                .plugins
                .keys()
                .filter(|id| self.is_waiting_for_vault(id))
                .cloned()
                .collect();
            let mut hub = self.hub.lock().unwrap();
            for id in blocked {
                hub.revoke_manual_actions(&id);
            }
        }
        self.reconcile();
        let wake = self.hub.lock().unwrap().wake.clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    // Read the requirement from the validated installed manifest, not a runtime plugin message
    pub(super) fn is_waiting_for_vault(&self, id: &str) -> bool {
        self.vault_setup_waiting.contains(id)
            || self.vault_locked
                && self.plugins.get(id).is_some_and(|plugin| {
                    all_actions_require_vault(&plugin.info)
                        || plugin.manifest["StreamDockManager"]["SetupSlots"]["slots"]
                            .as_array()
                            .is_some_and(|slots| slots.iter().any(|slot| slot["required"] == true))
                })
    }

    // Prevent action dispatch while an enabled plugin is waiting for Vault unlock
    pub(super) fn check_vault_runtime(&self, id: &str) -> Result<(), String> {
        if self.is_waiting_for_vault(id) {
            return Err(if self.vault_locked {
                "Unlock Vault to start this plugin"
            } else {
                "Review required Vault setup to start this plugin"
            }
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_tests::{host, package};

    #[test]
    // Verify skipping Vault unlock defers only required plugins without changing enable preferences
    fn locked_vault_defers_required_plugins_without_pausing_ordinary_or_optional_plugins() {
        let root = tempfile::tempdir().unwrap();
        for id in ["fixture.required", "fixture.optional", "fixture.ordinary"] {
            package(&root.path().join(id), id);
        }
        let mut host = host(root.path());
        for (id, required) in [("fixture.required", true), ("fixture.optional", false)] {
            let manifest = &mut host.plugins.get_mut(id).unwrap().manifest;
            manifest["StreamDockManager"]["SetupSlots"] = json!({
                "version": 1,
                "slots": [{
                    "id": "username",
                    "kind": "value",
                    "entry_kind": "text",
                    "mode": "direct",
                    "required": required,
                    "purpose": "Use the selected username",
                    "field": "username",
                    "action": format!("{id}.action"),
                }],
            });
            assert_eq!(
                crate::vault_rpc::setup_slots(manifest).unwrap()[0].required,
                required
            );
        }
        host.set_vault_locked(true);
        let mut required = HashMap::new();
        for id in ["fixture.required", "fixture.optional", "fixture.ordinary"] {
            host.enable(id, true).unwrap();
            host.visible_plugins.insert(id.into());
            required.insert(
                id.into(),
                vec![PluginDemand {
                    device_id: "device".into(),
                    profile_id: "profile".into(),
                }],
            );
        }
        host.reconcile_demand(required);
        assert!(!host.eligible("fixture.required"));
        assert!(host.check_vault_runtime("fixture.required").is_err());
        assert!(!host.startup.contains(&"fixture.required".into()));
        assert_eq!(
            host.plugins["fixture.required"].info.runtime.reason,
            RuntimeReason::VaultLocked
        );
        for id in ["fixture.optional", "fixture.ordinary"] {
            assert!(host.eligible(id));
            assert!(host.startup.contains(&id.into()));
        }

        host.set_vault_locked(false);
        assert!(!host.eligible("fixture.required"));
        assert!(!host.startup.contains(&"fixture.required".into()));
        assert!(host.plugins["fixture.required"].info.enabled);
        // Unlock alone cannot supply a missing per-action entry grant.
        assert!(host
            .begin_manual_action("fixture.required", "fixture.required.action")
            .is_err());
        let ordinary = host
            .begin_manual_action("fixture.ordinary", "fixture.ordinary.action")
            .unwrap();
        host.set_vault_locked(true);
        assert!(!host.startup.contains(&"fixture.required".into()));
        assert!(host
            .hub
            .lock()
            .unwrap()
            .manual_actions
            .demanded("fixture.ordinary"));
        drop(ordinary);
    }
}
