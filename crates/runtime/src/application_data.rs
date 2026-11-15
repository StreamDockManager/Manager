//! Maintenance uses the same configuration owner as desktop and CLI operation
use crate::Runtime;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AccessibilityChoice {
    #[default]
    System,
    On,
    Off,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessibilityPreferences {
    pub enabled: bool,
    pub larger_text: bool,
    pub high_contrast: AccessibilityChoice,
    pub reduced_motion: AccessibilityChoice,
    pub device_labels: bool,
}
impl Default for AccessibilityPreferences {
    // Construct the default application data value
    fn default() -> Self {
        Self {
            enabled: false,
            larger_text: false,
            high_contrast: Default::default(),
            reduced_motion: Default::default(),
            device_labels: true,
        }
    }
}
pub(crate) const ACCESSIBILITY_RECORD: stream_dock_manager_storage::JsonRecord<
    AccessibilityPreferences,
> = stream_dock_manager_storage::JsonRecord {
    domain: stream_dock_manager_storage::Domain::ManagerPreferences,
    key: "accessibility",
    max_bytes: 4096,
    decode: crate::profile_database::decode_record,
};
pub(crate) const OFFLINE_ICONS_RECORD: stream_dock_manager_storage::JsonRecord<bool> =
    stream_dock_manager_storage::JsonRecord {
        domain: stream_dock_manager_storage::Domain::ManagerPreferences,
        key: "offline_icons",
        max_bytes: 16,
        decode: crate::profile_database::decode_record,
    };
pub(crate) const DIAGNOSTICS_RECORD: stream_dock_manager_storage::JsonRecord<bool> =
    stream_dock_manager_storage::JsonRecord {
        domain: stream_dock_manager_storage::Domain::ManagerPreferences,
        key: "diagnostics_sampling",
        max_bytes: 16,
        decode: crate::profile_database::decode_record,
    };
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ClientPreferences {
    pub accessibility: AccessibilityPreferences,
    pub offline_icons: bool,
    pub diagnostics_sampling: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct RecoverySnapshot {
    pub name: String,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub completed_ms: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ApplicationData {
    #[ts(type = "number")]
    pub database_bytes: u64,
    #[ts(type = "number")]
    pub reclaimable_bytes: u64,
    pub sqlite_version: String,
    pub backup_error: Option<String>,
    pub backups: Vec<RecoverySnapshot>,
    pub plugins: Vec<stream_dock_manager_plugin_host::StoredPluginData>,
    pub usage: std::collections::BTreeMap<String, PluginDataUsage>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginDataUsage {
    pub bindings: u32,
    pub saved_macros: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginDataReview {
    pub id: String,
    pub review_token: String,
}

impl Runtime {
    // Delete plugin
    pub fn delete_plugin(&self, id: &str, keep_data: bool) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let _transaction = self.profiles.transaction.lock().unwrap();
        let mut drafts = self.drafts.lock().unwrap();
        let _policies = self.plugin_preferences.lock().unwrap();
        let _library = self.library_preferences.lock().unwrap();
        let mut alerts = self.resource_alerts.lock().unwrap();
        let owner = id.to_owned();
        let styles = std::sync::Arc::new(std::sync::Mutex::new(None));
        let committed_styles = styles.clone();
        let mut host = self.plugins.lock().unwrap();
        let result = host.delete_with_retention_sql(id, keep_data, owner.len(), move |tx| {
            if !keep_data {
                crate::plugin_policy::forget_plugin_in(tx, &owner)?;
                crate::library_organization::forget_plugin_in(tx, &owner)?;
                crate::resource_alerts::forget_plugin_in(tx, &owner)?;
                use stream_dock_manager_storage::{read_json, write_json, Domain};
                const KEY: &str = "plugin-profile-prompts";
                const LIMIT: usize = 16 * 1024 * 1024;
                if let Some(mut receipts) = read_json::<std::collections::BTreeMap<String, String>>(
                    tx,
                    Domain::LocalPreferences,
                    KEY,
                    LIMIT,
                )? {
                    if receipts.remove(&owner).is_some() {
                        write_json(tx, Domain::LocalPreferences, KEY, &receipts, LIMIT)?;
                    }
                }
            }
            *committed_styles.lock().unwrap() = Some(crate::plugin_styles::StyleChanges::clear_in(
                tx, &owner, !keep_data,
            )?);
            Ok(())
        });
        // A committed uninstall can still report pending package file cleanup
        let committed = if host.list().iter().any(|plugin| plugin.id == id) {
            None
        } else {
            styles.lock().unwrap().take()
        };
        let removed = committed.is_some();
        let changed = committed.is_some_and(|styles| styles.publish(&mut state, &mut drafts));
        drop(host);
        let refreshed = alerts.reload();
        drop(alerts);
        drop(_library);
        drop(_policies);
        drop(drafts);
        drop(_transaction);
        drop(state);
        if removed && !keep_data {
            self.cancel_macros();
        }
        if changed {
            self.emit("profiles", None, None, None);
        }
        result.and(refreshed)
    }
    // Read persisted client preferences
    pub fn client_preferences(&self) -> Result<ClientPreferences, String> {
        self.profiles.storage.run(|connection| {
            Ok(ClientPreferences {
                accessibility: ACCESSIBILITY_RECORD
                    .read_from(connection)?
                    .unwrap_or_default(),
                offline_icons: OFFLINE_ICONS_RECORD.read_from(connection)?.unwrap_or(false),
                diagnostics_sampling: DIAGNOSTICS_RECORD.read_from(connection)?.unwrap_or(true),
            })
        })
    }
    // Save client preference
    pub fn save_client_preference(
        &self,
        name: &str,
        value: serde_json::Value,
    ) -> Result<(), String> {
        match name {
            "accessibility" => {
                serde_json::from_value::<AccessibilityPreferences>(value.clone())
                    .map_err(|_| "Invalid accessibility preferences")?;
            }
            "offline_icons" | "diagnostics_sampling" if value.is_boolean() => {}
            _ => return Err("Invalid client preference".into()),
        }
        self.profiles.storage.write_json(
            stream_dock_manager_storage::Domain::ManagerPreferences,
            name,
            &value,
            4096,
        )
    }
    // Summarize application storage and plugin-owned data
    pub fn application_data(&self) -> Result<ApplicationData, String> {
        let stats = self.profiles.storage.stats()?;
        let backups = self
            .profiles
            .storage
            .backup_paths()?
            .into_iter()
            .map(|path| {
                let metadata = path.metadata().map_err(|error| error.to_string())?;
                Ok(RecoverySnapshot {
                    name: path
                        .file_name()
                        .ok_or("Invalid recovery filename")?
                        .to_string_lossy()
                        .into(),
                    bytes: metadata.len(),
                    completed_ms: metadata
                        .modified()
                        .map_err(|error| error.to_string())?
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|error| error.to_string())?
                        .as_millis() as u64,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut usage = std::collections::BTreeMap::<String, PluginDataUsage>::new();
        {
            let state = self.state.lock().unwrap();
            for binding in state
                .profiles
                .iter()
                .flat_map(|profile| &profile.pages)
                .flat_map(|page| page.bindings.values())
            {
                let mut owners = std::collections::BTreeSet::new();
                binding.action.visit(&mut |action| {
                    if let crate::actions::Action::Plugin { plugin_id, .. } = action {
                        owners.insert(plugin_id.clone());
                    }
                });
                for owner in owners {
                    usage.entry(owner).or_default().bindings += 1;
                }
            }
            for saved in &state.saved_macros {
                let mut owners = std::collections::BTreeSet::new();
                saved.action.visit(&mut |action| {
                    if let crate::actions::Action::Plugin { plugin_id, .. } = action {
                        owners.insert(plugin_id.clone());
                    }
                });
                for owner in owners {
                    usage.entry(owner).or_default().saved_macros += 1;
                }
            }
        }
        let plugins = self.plugins.lock().unwrap().stored_data()?;
        Ok(ApplicationData {
            database_bytes: stats.database_bytes,
            reclaimable_bytes: stats.reclaimable_bytes,
            sqlite_version: stats.sqlite_version,
            backup_error: stats.last_backup_error,
            backups,
            plugins,
            usage,
        })
    }
    // Create recovery snapshot
    pub fn create_recovery_snapshot(&self) -> Result<ApplicationData, String> {
        self.profiles.storage.backup_recovery()?;
        self.application_data()
    }
    // Compact application storage and return the resulting data summary
    pub fn compact_application_data(&self) -> Result<ApplicationData, String> {
        {
            let maintenance = self.profiles.storage.maintenance()?;
            maintenance.compact()?;
        }
        self.application_data()
    }
    // Remove the selected plugin’s local application data
    pub fn purge_plugin_data(
        &self,
        id: &str,
        review_token: &str,
    ) -> Result<ApplicationData, String> {
        self.plugins
            .lock()
            .unwrap()
            .purge_stored_data(id, review_token)?;
        self.application_data()
    }
    // Restore plugin data
    pub fn restore_plugin_data(
        &self,
        id: &str,
        review_token: &str,
    ) -> Result<ApplicationData, String> {
        self.plugins
            .lock()
            .unwrap()
            .restore_stored_data(id, review_token)?;
        self.application_data()
    }
    // Remove application data for the explicitly selected plugins
    pub fn purge_plugin_data_batch(
        &self,
        reviews: Vec<PluginDataReview>,
    ) -> Result<ApplicationData, String> {
        self.plugins.lock().unwrap().purge_stored_data_batch(
            reviews
                .into_iter()
                .map(|review| (review.id, review.review_token))
                .collect(),
        )?;
        self.application_data()
    }
}

/// Cold maintenance intentionally does not construct Runtime, launch plugins, or
/// start background refresh/device work. The root lock excludes a running host
pub fn maintain(
    root: PathBuf,
    operation: &str,
    path: Option<PathBuf>,
) -> Result<serde_json::Value, String> {
    if operation == "restore" {
        let report = stream_dock_manager_storage::Storage::recover(
            root,
            path.ok_or("A recovery snapshot path is required")?,
            validate_snapshot,
            reconcile_deletions,
        )?;
        return serde_json::to_value(report).map_err(|error| error.to_string());
    }
    let storage = stream_dock_manager_storage::Storage::open_validated(root, validate_snapshot)?;
    match operation {
        "status" => serde_json::to_value(storage.stats()?).map_err(|error| error.to_string()),
        "backup" => serde_json::to_value(match path {
            Some(path) => storage.backup(path)?,
            None => storage.backup_recovery()?,
        })
        .map_err(|error| error.to_string()),
        "compact" => {
            {
                let maintenance = storage.maintenance()?;
                maintenance.compact()?;
            }
            serde_json::to_value(storage.stats()?).map_err(|error| error.to_string())
        }
        _ => Err("Unknown application data maintenance operation".into()),
    }
}

// Validate snapshot
pub(crate) fn validate_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    // Fixed-key domains must not silently retain or ignore retired settings
    for (domain, allowed) in [
        (
            stream_dock_manager_storage::Domain::ManagerPreferences,
            &[
                "desktop",
                "accessibility",
                "offline_icons",
                "diagnostics_sampling",
            ][..],
        ),
        (
            stream_dock_manager_storage::Domain::RuntimeState,
            &["preferences"][..],
        ),
        (
            stream_dock_manager_storage::Domain::LoggingPreferences,
            &["preferences"][..],
        ),
        (
            stream_dock_manager_storage::Domain::MediaTombstones,
            &["deleted"][..],
        ),
    ] {
        let mut statement = connection
            .prepare(&format!("SELECT key FROM {}", domain.table()))
            .map_err(|e| e.to_string())?;
        for key in statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?
        {
            if !allowed.contains(&key.map_err(|e| e.to_string())?.as_str()) {
                return Err(format!(
                    "Unsupported record in {}; original preserved",
                    domain.table()
                ));
            }
        }
    }
    crate::profile_database::validate_snapshot(connection)?;
    stream_dock_manager_plugin_host::validate_storage_snapshot(connection)?;
    stream_dock_manager_plugin_runtimes::validate_storage_snapshot(connection)
}

// Reconcile deletions
pub(crate) fn reconcile_deletions(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
    sources: &[stream_dock_manager_storage::RecoverySource],
) -> Result<Vec<String>, String> {
    use stream_dock_manager_storage::{read_json, write_json, Domain};
    let mut deleted: crate::media_deletion::DeletedMedia = read_json(
        connection,
        Domain::MediaTombstones,
        "deleted",
        4 * 1024 * 1024,
    )?
    .unwrap_or_default();
    deleted.schema_version = 1;
    let mut warnings = vec![];
    for source in sources {
        let previous = read_json::<crate::media_deletion::DeletedMedia>(
            &source.connection,
            Domain::MediaTombstones,
            "deleted",
            4 * 1024 * 1024,
        );
        match previous {
            Ok(Some(value)) if value.schema_version == 1 && value.assets.len() <= 65536 && value.assets.iter().all(|id| crate::profiles::validate_asset_id(id).is_ok()) => {
                deleted.revision = deleted.revision.max(value.revision);
                deleted.assets.extend(value.assets);
            }
            Ok(None) => {},
            _ => warnings.push(format!("Permanent media deletion metadata could not be recovered from {}; inspect preserved evidence before using old media references.", source.path.display())),
        }
    }
    if deleted.assets.len() > 65536 {
        return Err("Restored deletion inventory exceeds its limit".into());
    }
    let Some(mut data) = crate::profile_database::load(connection)? else {
        return if deleted.assets.is_empty() {
            Ok(warnings)
        } else {
            Err(
                "Cannot restore an uninitialized workspace over permanent media deletion records"
                    .into(),
            )
        };
    };
    let tx = connection
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    write_json(
        &tx,
        Domain::MediaTombstones,
        "deleted",
        &deleted,
        4 * 1024 * 1024,
    )?;
    deleted.detach(&mut data);
    crate::profile_database::save(&tx, &data)?;
    let mut drafts: std::collections::BTreeMap<String, crate::workspace::Draft> =
        read_json(&tx, Domain::LocalPreferences, "drafts", 16 * 1024 * 1024)?.unwrap_or_default();
    for draft in drafts.values_mut() {
        deleted.detach_draft(draft);
    }
    write_json(
        &tx,
        Domain::LocalPreferences,
        "drafts",
        &drafts,
        16 * 1024 * 1024,
    )?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_storage::Domain;

    #[test]
    // Verify that cold restore preserves permanent deletion and rejects invalid snapshot
    fn cold_restore_preserves_permanent_deletion_and_rejects_invalid_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::profiles::ProfileStore::open(root.path().join("profiles")).unwrap();
        let mut data = store.load().unwrap();
        let asset = uuid::Uuid::new_v4().to_string();
        data.profiles[0].background_asset = Some(asset.clone());
        store.save(&data).unwrap();
        let snapshot = root.path().join("saved.sqlite3");
        store.storage.backup(snapshot.clone()).unwrap();
        let mut deleted = crate::media_deletion::DeletedMedia {
            schema_version: 1,
            revision: data.configuration_revision + 1,
            ..Default::default()
        };
        deleted.assets.insert(asset.clone());
        store
            .storage
            .write_json(
                Domain::MediaTombstones,
                "deleted",
                &deleted,
                4 * 1024 * 1024,
            )
            .unwrap();
        deleted.detach(&mut data);
        data.profiles[0].name = "After snapshot".into();
        store.save(&data).unwrap();
        assert!(
            maintain(root.path().into(), "restore", Some(snapshot.clone()))
                .unwrap_err()
                .contains("owns this configuration")
        );
        drop(store);
        maintain(root.path().into(), "restore", Some(snapshot)).unwrap();
        let store = crate::profiles::ProfileStore::open(root.path().join("profiles")).unwrap();
        let restored = store.load().unwrap();
        assert!(restored.profiles[0].background_asset.is_none());
        assert_ne!(restored.profiles[0].name, "After snapshot");
        assert!(crate::media_deletion::DeletedMedia::read(&store.storage)
            .unwrap()
            .assets
            .contains(&asset));
        let before = serde_json::to_value(restored).unwrap();
        drop(store);
        let invalid = root.path().join("invalid.sqlite3");
        std::fs::write(&invalid, "not a SQLite database").unwrap();
        assert!(maintain(root.path().into(), "restore", Some(invalid.clone())).is_err());
        assert_eq!(
            std::fs::read_to_string(invalid).unwrap(),
            "not a SQLite database"
        );
        let store = crate::profiles::ProfileStore::open(root.path().join("profiles")).unwrap();
        assert_eq!(serde_json::to_value(store.load().unwrap()).unwrap(), before);
        assert!(!root.path().join("profiles/profiles.json").exists());
    }

    #[test]
    // Verify that client preferences are typed independent records and fail without reset
    fn client_preferences_are_typed_independent_records_and_fail_without_reset() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime
            .save_client_preference("offline_icons", serde_json::json!(true))
            .unwrap();
        runtime
            .save_client_preference("diagnostics_sampling", serde_json::json!(false))
            .unwrap();
        assert!(runtime
            .save_client_preference("offline_icons", serde_json::json!("false"))
            .is_err());
        assert!(runtime
            .save_client_preference("desktop", serde_json::json!({}))
            .is_err());
        assert!(runtime.client_preferences().unwrap().offline_icons);
        assert!(!runtime.client_preferences().unwrap().diagnostics_sampling);
        runtime.shutdown();
        drop(runtime);
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        assert!(runtime.client_preferences().unwrap().offline_icons);
        runtime
            .profiles
            .storage
            .run(|connection| {
                connection
                    .execute_batch("PRAGMA query_only=ON")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        assert!(runtime
            .save_client_preference("offline_icons", serde_json::json!(false))
            .is_err());
        assert!(runtime.client_preferences().unwrap().offline_icons);
        runtime
            .profiles
            .storage
            .run(|connection| {
                connection
                    .execute_batch("PRAGMA query_only=OFF")
                    .map_err(|error| error.to_string())
            })
            .unwrap();
        runtime.shutdown();
    }
}
