//! User-reviewed backup rollback with current-package binding and existing replacement recovery
use crate::{Runtime, Snapshot};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use stream_dock_manager_plugin_host::backups::{BackupReview, PluginBackups, PreparedRollback};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginRollbackReview {
    pub id: String,
    pub package: BackupReview,
    pub can_restore: bool,
}
pub(crate) struct StagedRollback {
    report: PluginRollbackReview,
    prepared: Option<PreparedRollback>,
    created: Instant,
}
impl crate::review_store::ReviewSession for StagedRollback {
    const LIMIT: usize = 4;
    const FULL: &'static str = "Close another rollback review first";
    const LIFETIME: Option<std::time::Duration> = Some(Duration::from_secs(3600));
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
impl Runtime {
    // List available local rollback packages for the selected plugin
    pub fn plugin_backups(&self, id: &str, page: usize) -> Result<PluginBackups, String> {
        self.plugins.lock().unwrap().plugin_backups(id, page)
    }
    // Delete plugin backups
    pub fn delete_plugin_backups(
        &self,
        id: &str,
        backup: Option<&str>,
        all: bool,
    ) -> Result<usize, String> {
        self.plugins
            .lock()
            .unwrap()
            .delete_plugin_backups(id, backup, all)
    }
    // Prepare plugin rollback
    pub fn prepare_plugin_rollback(
        &self,
        id: &str,
        backup: &str,
    ) -> Result<PluginRollbackReview, String> {
        let mut stages = self.plugin_rollbacks.lock().unwrap();
        stages.check_capacity()?;
        let host = self.plugins.lock().unwrap();
        let mut prepared = host.prepare_rollback(id, backup)?;
        drop(host);
        prepared.review.compatibility = stream_dock_manager_plugin_host::preflight::resolve_runtime(
            prepared.review.compatibility,
            &self.runtimes,
        );
        let report = PluginRollbackReview {
            id: crate::review_store::new_id(),
            can_restore: prepared.review.compatibility.can_install,
            package: prepared.review.clone(),
        };
        stages.insert(
            report.id.clone(),
            StagedRollback {
                report: report.clone(),
                prepared: Some(prepared),
                created: Instant::now(),
            },
        )?;
        Ok(report)
    }
    // Cancel plugin rollback
    pub fn cancel_plugin_rollback(&self, id: &str) {
        self.plugin_rollbacks.lock().unwrap().remove(id);
    }
    // Commit plugin rollback
    pub fn commit_plugin_rollback(
        &self,
        id: &str,
        acknowledge_settings: bool,
        allow_unverified: bool,
    ) -> Result<Snapshot, String> {
        let mut stages = self.plugin_rollbacks.lock().unwrap();
        let stage = stages.active_mut(
            id,
            "Rollback review expired",
            "Rollback review expired; review the backup again",
        )?;
        if !stage.report.can_restore {
            return Err("Backup preflight blocks restoration".into());
        }
        if !acknowledge_settings {
            return Err("Acknowledge settings and state compatibility limits".into());
        }
        if !stage.report.package.backup.trusted_origin && !allow_unverified {
            return Err("Acknowledge this backup's unverified provenance".into());
        }
        let prepared = stage
            .prepared
            .as_ref()
            .ok_or("Prepare the rollback again")?;
        let mut host = self.plugins.lock().unwrap();
        host.verify_rollback(&prepared.review, &prepared.backup_identity)?;
        let plugin = &prepared.review.installed.id;
        let reservation = host.reserve_replacement(plugin)?;
        let waiter = host.stop_for_replacement(plugin)?;
        drop(host);
        self.emit("plugins", None, None, None);
        if let Some(waiter) = waiter {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !waiter.load(std::sync::atomic::Ordering::Acquire) {
                if Instant::now() >= deadline {
                    return Err("Plugin cleanup did not finish; review rollback again".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let mut host = self.plugins.lock().unwrap();
        let prepared = stage
            .prepared
            .as_ref()
            .ok_or("Prepare the rollback again")?;
        host.verify_rollback(&prepared.review, &prepared.backup_identity)?;
        let prepared = stage.prepared.take().unwrap();
        let result = host.restore_prepared(prepared.prepared);
        drop(host);
        drop(reservation);
        stages.remove(id);
        drop(stages);
        let snapshot = self.plugins_changed();
        result.map(|_| snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires disposable loopback host; unverified rollback consent and cancellation only"]
    // Verify that local backup requires both acknowledgments and cancel drops staging
    fn local_backup_requires_both_acknowledgments_and_cancel_drops_staging() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "org.test.local.rollback";
        for (version, folder) in [("1.0.0.0", "one"), ("2.0.0.0", "two")] {
            let source = root.path().join(folder).join(id);
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(source.join("manifest.json"),serde_json::to_vec(&serde_json::json!({"UUID":id,"Name":"Local rollback","Version":version,"Author":"Stream Dock Manager contributors","Description":"Disposable rollback fixture","Icon":"icon","OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],"Software":{"MinimumVersion":"6.9"},"CodePath":"runtime.html","SDKVersion":2,"Actions":[{"UUID":format!("{id}.action"),"Name":"Action","Icon":"icon","States":[{"Image":"icon"}]}]})).unwrap()).unwrap();
            std::fs::write(
                source.join("runtime.html"),
                "<!doctype html><title>Local fixture</title>",
            )
            .unwrap();
            let review = runtime.prepare_plugin_package(&source).unwrap();
            runtime
                .commit_plugin_package(&review.id, false, true)
                .unwrap();
        }
        let backups = runtime.plugin_backups(id, 1).unwrap();
        let review = runtime
            .prepare_plugin_rollback(id, &backups.entries[0].id)
            .unwrap();
        assert!(!review.package.backup.trusted_origin);
        assert!(runtime
            .commit_plugin_rollback(&review.id, true, false)
            .is_err());
        assert!(runtime
            .commit_plugin_rollback(&review.id, false, true)
            .is_err());
        runtime.cancel_plugin_rollback(&review.id);
        assert!(runtime
            .commit_plugin_rollback(&review.id, true, true)
            .is_err());
        let review = runtime
            .prepare_plugin_rollback(id, &backups.entries[0].id)
            .unwrap();
        let snapshot = runtime
            .commit_plugin_rollback(&review.id, true, true)
            .unwrap();
        let plugin = snapshot.plugins.iter().find(|p| p.id == id).unwrap();
        assert_eq!(plugin.version, "1.0.0.0");
        assert!(!plugin.enabled);
        assert!(!runtime.plugin_update_policy(id).unwrap().managed);
        runtime.shutdown();
    }
}
