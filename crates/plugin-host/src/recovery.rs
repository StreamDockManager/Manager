//! Configuration-local startup journal. An unfinished attempt describes an
//! interrupted launch, never evidence that a plugin caused the host's exit.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use stream_dock_manager_storage::{Domain, Storage};
const KEY: &str = "plugin-startup-attempts-v1";
const LIMIT: usize = 256 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct InterruptedPlugin {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct RecoveryStatus {
    pub active: bool,
    pub prompt: bool,
    pub interrupted: Vec<InterruptedPlugin>,
}
pub(crate) struct Recovery {
    pub status: RecoveryStatus,
    attempts: BTreeMap<String, InterruptedPlugin>,
    storage: Storage,
}
// Load attempts
fn load_attempts(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<BTreeMap<String, InterruptedPlugin>, String> {
    let attempts: BTreeMap<String, InterruptedPlugin> =
        stream_dock_manager_storage::read_json(connection, Domain::LocalPreferences, KEY, LIMIT)?
            .unwrap_or_default();
    if attempts.len() > 256
        || attempts
            .iter()
            .any(|(id, p)| !super::safe_id(id) || p.name.len() > 1024 || p.id != *id)
    {
        return Err("Invalid bounded plugin startup journal".into());
    }
    Ok(attempts)
}
// Validate snapshot
pub(crate) fn validate_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    load_attempts(connection).map(|_| ())
}
impl Recovery {
    // Open for the current operation
    pub fn open(storage: Storage) -> Result<Self, String> {
        let attempts = storage.run(|connection| load_attempts(connection))?;
        let interrupted = attempts.values().cloned().collect();
        if !attempts.is_empty() {
            use crate::manager_diagnostics::*;
            recovery(RecoveryEvent::InterruptedStartup, attempts.len());
            for id in attempts.keys() {
                plugin(PluginRecord {
                    event: PluginEvent::InterruptedStartup,
                    plugin_id: id,
                    version: "unknown",
                    run_id: None,
                    reason: None,
                    exit_code: None,
                    signal: None,
                    attempt: 0,
                    recovery: true,
                });
            }
        }
        Ok(Self {
            status: RecoveryStatus {
                active: !attempts.is_empty(),
                prompt: !attempts.is_empty(),
                interrupted,
            },
            attempts,
            storage,
        })
    }
    // Check for the current operation
    pub fn check(&self) -> Result<(), String> {
        if self.status.active {
            Err("Plugin execution is paused in recovery mode".into())
        } else {
            Ok(())
        }
    }
    // Begin for the current operation
    pub fn begin(&mut self, id: &str, name: &str) -> Result<(), String> {
        self.check()?;
        if !super::safe_id(id) {
            return Err("Invalid plugin startup identity".into());
        }
        if self.attempts.len() >= 256 {
            return Err("Plugin startup journal is full".into());
        }
        let mut attempts = self.attempts.clone();
        attempts.insert(
            id.into(),
            InterruptedPlugin {
                id: id.into(),
                name: name.chars().take(256).collect(),
            },
        );
        self.storage
            .write_json(Domain::LocalPreferences, KEY, &attempts, LIMIT)?;
        self.attempts = attempts;
        Ok(())
    }
    // Finish for the current operation
    pub fn finish(&mut self, id: &str) -> Result<(), String> {
        if !self.attempts.contains_key(id) {
            return Ok(());
        }
        let mut attempts = self.attempts.clone();
        attempts.remove(id);
        self.storage
            .write_json(Domain::LocalPreferences, KEY, &attempts, LIMIT)?;
        self.attempts = attempts;
        Ok(())
    }
    // Clear for the current operation
    pub fn clear(&mut self) -> Result<(), String> {
        self.storage.write_json(
            Domain::LocalPreferences,
            KEY,
            &BTreeMap::<String, InterruptedPlugin>::new(),
            LIMIT,
        )?;
        self.attempts.clear();
        Ok(())
    }
    // Choose the next action from the current lifecycle state
    pub fn decide(&mut self, normal: bool) -> Result<(), String> {
        if !self.status.prompt && normal {
            return Err("Quit and launch normally to leave recovery mode".into());
        }
        self.clear()?;
        self.status.active = !normal;
        self.status.prompt = false;
        Ok(())
    }
}
impl super::PluginHost {
    // Read the current plugin recovery status
    pub fn recovery_status(&self) -> RecoveryStatus {
        self.recovery.status.clone()
    }
    // Enter recovery instead of executing an inconsistent plugin state
    pub fn enter_recovery(&mut self) {
        crate::manager_diagnostics::recovery(
            crate::manager_diagnostics::RecoveryEvent::EnteredExplicit,
            self.recovery.status.interrupted.len(),
        );
        self.recovery.status.active = true;
        self.recovery.status.prompt = false;
        self.reconcile();
    }
    // Resolve interrupted startup
    pub fn resolve_interrupted_startup(&mut self, normal: bool) -> Result<RecoveryStatus, String> {
        self.recovery.decide(normal)?;
        crate::manager_diagnostics::recovery(
            if normal {
                crate::manager_diagnostics::RecoveryEvent::StartedNormally
            } else {
                crate::manager_diagnostics::RecoveryEvent::KeptActive
            },
            self.recovery.status.interrupted.len(),
        );
        self.reconcile();
        Ok(self.recovery_status())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify snapshot validation rejects malformed or oversized attempts
    fn snapshot_validation_rejects_malformed_or_oversized_attempts() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        for value in [
            serde_json::json!({"native":{"id":"other","name":"Name"}}),
            serde_json::json!({"native":{"id":"native","name":"x".repeat(1025)}}),
        ] {
            storage
                .write_json(Domain::LocalPreferences, KEY, &value, LIMIT)
                .unwrap();
            assert!(storage
                .run(|connection| validate_snapshot(connection))
                .is_err());
            assert!(Recovery::open(storage.clone()).is_err());
        }
        let too_many: BTreeMap<_, _> = (0..257)
            .map(|i| {
                let id = format!("plugin{i}");
                (
                    id.clone(),
                    InterruptedPlugin {
                        id,
                        name: "Name".into(),
                    },
                )
            })
            .collect();
        storage
            .write_json(Domain::LocalPreferences, KEY, &too_many, LIMIT)
            .unwrap();
        assert!(storage
            .run(|connection| validate_snapshot(connection))
            .is_err());
    }
    #[test]
    // Verify interrupted attempt pauses both execution routes until explicit choice
    fn interrupted_attempt_pauses_both_execution_routes_until_explicit_choice() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let mut recovery = Recovery::open(storage.clone()).unwrap();
        recovery.begin("native", "Native plugin").unwrap();
        recovery.begin("browser", "Browser plugin").unwrap();
        recovery.finish("native").unwrap();
        drop(recovery);
        let mut recovery = Recovery::open(storage.clone()).unwrap();
        assert!(recovery.status.active && recovery.status.prompt);
        assert_eq!(recovery.status.interrupted[0].id, "browser");
        assert!(recovery.begin("other", "Other").is_err());
        recovery.decide(false).unwrap();
        assert!(recovery.check().is_err());
        assert!(recovery.decide(true).is_err());
        drop(recovery);
        let mut recovery = Recovery::open(storage).unwrap();
        assert!(recovery.check().is_ok());
        recovery.begin("browser", "Browser").unwrap();
        recovery.finish("browser").unwrap();
        assert!(recovery.attempts.is_empty());
    }
}
#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::{lifecycle_tests, PluginDemand, RuntimeState};
    use std::collections::HashMap;
    #[test]
    // Verify recovery blocks all native and browser entry routes without changing enablement
    fn recovery_blocks_all_native_and_browser_entry_routes_without_changing_enablement() {
        for native in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let package = root.path().join("plugin");
            lifecycle_tests::package(&package, "fixture.recovery");
            let mut host = lifecycle_tests::host(root.path());
            host.enable("fixture.recovery", true).unwrap();
            if native {
                host.plugins.get_mut("fixture.recovery").unwrap().info.kind = "native".into();
            }
            host.enter_recovery();
            host.reconcile_demand(HashMap::from([(
                "fixture.recovery".into(),
                vec![PluginDemand {
                    device_id: "dock".into(),
                    profile_id: "profile".into(),
                }],
            )]));
            host.poll();
            assert!(host.plugins["fixture.recovery"].info.enabled);
            assert!(!matches!(
                host.plugins["fixture.recovery"].info.runtime.state,
                RuntimeState::Starting | RuntimeState::Running
            ));
            assert!(host
                .launch("fixture.recovery", true, false)
                .unwrap_err()
                .contains("recovery"));
            assert!(host
                .browser_view("fixture.recovery")
                .unwrap_err()
                .contains("recovery"));
            assert!(host
                .settings_panel(
                    "fixture.recovery",
                    Some("fixture.recovery.action"),
                    &serde_json::json!({})
                )
                .err()
                .unwrap()
                .contains("recovery"));
            assert!(host
                .setup_settings_panel("fixture.recovery")
                .err()
                .unwrap()
                .contains("recovery"));
            assert!(host
                .begin_manual_action("fixture.recovery", "fixture.recovery.action")
                .is_err());
            assert!(host.recovery.attempts.is_empty());
            for enabled in [false, true, false] {
                host.enable("fixture.recovery", enabled).unwrap();
                host.poll();
                assert_eq!(host.plugins["fixture.recovery"].info.enabled, enabled);
                assert!(!matches!(
                    host.plugins["fixture.recovery"].info.runtime.state,
                    RuntimeState::Starting | RuntimeState::Running
                ));
                assert!(host.recovery.attempts.is_empty());
            }
            host.enable("fixture.recovery", true).unwrap();
            drop(host);
            let mut reopened = lifecycle_tests::host(root.path());
            assert!(
                reopened
                    .list()
                    .iter()
                    .find(|plugin| plugin.id == "fixture.recovery")
                    .unwrap()
                    .enabled
            );
            reopened.enter_recovery();
            reopened.enable("fixture.recovery", false).unwrap();
            drop(reopened);
            assert!(
                !lifecycle_tests::host(root.path())
                    .list()
                    .iter()
                    .find(|plugin| plugin.id == "fixture.recovery")
                    .unwrap()
                    .enabled
            );
        }
    }
    #[test]
    // Verify real browser launch is journaled before entry and cleared on running and stop
    fn real_browser_launch_is_journaled_before_entry_and_cleared_on_running_and_stop() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("plugin"), "fixture.recovery");
        let mut host = lifecycle_tests::host(root.path());
        host.enable("fixture.recovery", true).unwrap();
        host.visible_plugins.insert("fixture.recovery".into());
        host.reconcile_demand(HashMap::from([(
            "fixture.recovery".into(),
            vec![PluginDemand {
                device_id: "dock".into(),
                profile_id: "profile".into(),
            }],
        )]));
        host.poll();
        assert!(host.recovery.attempts.contains_key("fixture.recovery"));
        assert!(host.browser_view("fixture.recovery").unwrap().is_some());
        let next = Recovery::open(host.recovery.storage.clone()).unwrap();
        assert!(next.status.prompt && next.status.active);
        let token = host.plugins["fixture.recovery"].token.clone();
        host.hub.lock().unwrap().sessions.connected.insert(token);
        host.poll();
        assert!(host.recovery.attempts.is_empty());
        assert_eq!(
            host.plugins["fixture.recovery"].info.runtime.state,
            RuntimeState::Running
        );
        host.launch("fixture.recovery", false, false).unwrap();
        assert!(
            !Recovery::open(host.recovery.storage.clone())
                .unwrap()
                .status
                .active
        );
    }
}
