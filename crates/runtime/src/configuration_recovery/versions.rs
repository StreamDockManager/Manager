//! Previous profile versions are immutable draft sources, never a whole-workspace restore
use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const VERSION_LIMIT: usize = 64;
const TOTAL_BYTES: usize = 32 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryVersion {
    pub id: String,
    pub label: String,
    pub source: String,
    #[ts(type = "number | null")]
    pub timestamp_ms: Option<u64>,
    pub valid: bool,
    pub issues: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryVersions {
    pub revision: String,
    pub versions: Vec<ConfigurationRecoveryVersion>,
    pub warnings: Vec<String>,
}
struct Candidate {
    item: ConfigurationRecoveryVersion,
    value: Value,
    json: String,
}
#[derive(Default)]
pub(super) struct VersionCache {
    profile: String,
    revision: String,
    entries: Vec<Candidate>,
}
#[derive(Default)]
struct Collector {
    entries: Vec<Candidate>,
    seen: HashSet<String>,
    bytes: usize,
}
impl Collector {
    // Add an entry to versions
    fn add(
        &mut self,
        profile: crate::profiles::Profile,
        label: String,
        source: &str,
        timestamp_ms: Option<u64>,
    ) -> Result<(), String> {
        profile.validate()?;
        let value = serde_json::to_value(profile).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        let digest = hex::encode(Sha256::digest(json.as_bytes()));
        if self.seen.contains(&digest) {
            return Ok(());
        }
        if self.entries.len() >= VERSION_LIMIT
            || self.bytes.saturating_add(json.len()) > TOTAL_BYTES
        {
            return Err("Version inventory reached its 64-version / 32 MiB limit".into());
        }
        self.bytes += json.len();
        self.seen.insert(digest.clone());
        self.entries.push(Candidate {
            item: ConfigurationRecoveryVersion {
                id: digest,
                label,
                source: source.into(),
                timestamp_ms,
                valid: false,
                issues: Vec::new(),
            },
            value,
            json,
        });
        Ok(())
    }
}
impl ConfigurationRecovery {
    // Collect previous versions only while the reviewed configuration revision still matches
    pub fn versions(
        &self,
        id: &str,
        revision: &str,
    ) -> Result<ConfigurationRecoveryVersions, String> {
        let (record, mut candidates, mut warnings) = self.editor.read(|conn, current| {
            if current != revision {
                return Err(
                    "Configuration changed. Reload before browsing previous versions.".into(),
                );
            }
            let record = find(conn, id)?;
            if record.kind != "profile" {
                return Err("Previous versions are available for profiles only".into());
            }
            let mut candidates = Collector::default();
            let mut warnings = Vec::new();
            let index = (|| {
                let header = read(conn, Domain::Workspace, "current")?
                    .ok_or("Workspace metadata is missing")?;
                serde_json::from_value::<crate::workspace::HistoryIndex>(
                    header["document"]["history"].clone(),
                )
                .map_err(|e| e.to_string())
            })()
            .unwrap_or_else(|error| {
                warnings.push(format!("Undo/redo index unavailable: {error}"));
                crate::workspace::HistoryIndex::default()
            });
            candidates.seen.insert(hex::encode(Sha256::digest(
                serde_json::to_string_pretty(&record.value)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            )));
            let mut entries = index.undo.into_iter().chain(index.redo).collect::<Vec<_>>();
            entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp_ms));
            if entries.len() > 80 {
                warnings.push("Only the latest 80 undo/redo entries were inspected.".into());
                entries.truncate(80);
            }
            let mut scanned = 0usize;
            for entry in entries {
                let result = (|| {
                    let value = stream_dock_manager_storage::read_json::<Value>(
                        conn,
                        Domain::History,
                        &entry.id,
                        32 * 1024 * 1024,
                    )?
                    .ok_or("History snapshot is missing")?;
                    scanned = scanned.saturating_add(
                        serde_json::to_vec(&value).map_err(|e| e.to_string())?.len(),
                    );
                    if scanned > 160 * 1024 * 1024 {
                        return Err("History scan reached its 160 MiB limit".into());
                    }
                    let documents = crate::workspace::recovery_history_documents(value)?;
                    for (side, document) in ["Before", "After"].into_iter().zip(documents) {
                        if let Some(profile) =
                            document.profiles.into_iter().find(|p| p.id == record.key)
                        {
                            candidates.add(
                                profile,
                                format!("{side}: {}", entry.description),
                                "history",
                                Some(entry.timestamp_ms),
                            )?;
                        }
                    }
                    Ok::<_, String>(())
                })();
                if let Err(error) = result {
                    warnings.push(format!("History {}: {error}", entry.description));
                }
                if scanned > 160 * 1024 * 1024 {
                    break;
                }
            }
            Ok((record, candidates, warnings))
        })?;
        match self.editor.inspect_backups(32, |name, time, conn| {
            let value = read(conn, Domain::Profiles, &record.key)?;
            let Some(value) = value else {
                return Ok(());
            };
            let profile: crate::profiles::Profile =
                serde_json::from_value(value).map_err(|e| e.to_string())?;
            if profile.id != record.key {
                return Err("Profile identity differs from its saved row".into());
            }
            candidates.add(profile, format!("Backup: {name}"), "backup", time)
        }) {
            Ok((_, skipped)) => warnings.extend(skipped),
            Err(error) => warnings.push(format!("Local backups unavailable: {error}")),
        }
        let mut entries = candidates.entries;
        self.editor.preview(revision, |conn| {
            for candidate in &mut entries {
                let mut issues = record_issues(&record, &candidate.value);
                if issues.is_empty() {
                    write(conn, Domain::Profiles, &record.key, &candidate.value)?;
                    issues.extend(context_issues(conn, &record));
                }
                candidate.item.valid = issues.is_empty();
                candidate.item.issues = issues;
            }
            Ok(())
        })?;
        if !entries.is_empty() {
            warnings.push("Versions restore profile settings only, not media files, installed plugins, or saved macro definitions. Review their availability before saving.".into());
        }
        warnings.sort();
        warnings.dedup();
        let response = ConfigurationRecoveryVersions {
            revision: revision.into(),
            versions: entries.iter().map(|entry| entry.item.clone()).collect(),
            warnings,
        };
        *self
            .versions
            .lock()
            .map_err(|_| "Version cache lock failed")? = VersionCache {
            profile: id.into(),
            revision: revision.into(),
            entries,
        };
        Ok(response)
    }
    // Read the selected cached version after rechecking the configuration revision
    pub fn version(
        &self,
        id: &str,
        revision: &str,
        version_id: &str,
    ) -> Result<ConfigurationRecoveryDocument, String> {
        self.editor.read(|_, current| {
            if current != revision {
                Err("Configuration changed. Reload previous versions.".into())
            } else {
                Ok(())
            }
        })?;
        let cache = self
            .versions
            .lock()
            .map_err(|_| "Version cache lock failed")?;
        if cache.profile != id || cache.revision != revision {
            return Err("Reload previous versions for this profile.".into());
        }
        let candidate = cache
            .entries
            .iter()
            .find(|entry| entry.item.id == version_id)
            .ok_or("Previous version is no longer available. Reload the list.")?;
        Ok(ConfigurationRecoveryDocument {
            id: id.into(),
            revision: revision.into(),
            json: candidate.json.clone(),
            issues: candidate.item.issues.clone(),
            diagnostics: Some(diagnostics(
                "profile",
                &candidate.json,
                &candidate.item.issues,
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ProfileStore;
    use stream_dock_manager_storage::{Storage, DATABASE_NAME};
    #[test]
    // Verify that prior history is real deduplicated nonmutating and load then save restores only profile
    fn prior_history_is_real_deduplicated_nonmutating_and_load_then_save_restores_only_profile() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let data = store.load().unwrap();
        let original = data.profiles[0].clone();
        let changed = store
            .edit(data.configuration_revision, "Rename profile", |data| {
                data.profiles[0].name = "Changed".into();
                Ok(())
            })
            .unwrap();
        drop(store);
        let before = std::fs::read(root.path().join(DATABASE_NAME)).unwrap();
        let service = ConfigurationRecovery::open(root.path().into(), "test".into()).unwrap();
        let id = format!("profile:{}", original.id);
        let current = service.document(&id).unwrap();
        let versions = service.versions(&id, &current.revision).unwrap();
        assert!(versions.versions.iter().any(|v| v.source == "history"
            && v.label == "Before: Rename profile"
            && v.valid
            && v.timestamp_ms.is_some()));
        assert_eq!(
            before,
            std::fs::read(root.path().join(DATABASE_NAME)).unwrap()
        );
        let version = versions
            .versions
            .iter()
            .find(|v| v.label == "Before: Rename profile")
            .unwrap();
        let previous = service
            .version(&id, &current.revision, &version.id)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&previous.json).unwrap()["name"],
            original.name
        );
        service
            .save(&id, &current.revision, &previous.json)
            .unwrap();
        assert!(service
            .version(&id, &current.revision, &version.id)
            .is_err());
        service
            .editor
            .read(|conn, _| {
                let after = crate::profile_database::load(conn)?.unwrap();
                assert_eq!(after.profiles[0].name, original.name);
                assert_eq!(after.history.undo.len(), changed.history.undo.len());
                Ok(())
            })
            .unwrap();
    }
    #[test]
    // Verify that invalid history and nonregular backups do not hide valid same generation backup
    fn invalid_history_and_nonregular_backups_do_not_hide_valid_same_generation_backup() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let data = store.load().unwrap();
        let id = format!("profile:{}", data.profiles[0].id);
        store
            .storage()
            .backup(root.path().join("recovery/previous.sqlite3"))
            .unwrap();
        drop(store);
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .transaction(move |conn| {
                let mut profile = read(conn, Domain::Profiles, &data.profiles[0].id)?.unwrap();
                profile["name"] = Value::String("Now".into());
                write(conn, Domain::Profiles, &data.profiles[0].id, &profile)?;
                let mut header = read(conn, Domain::Workspace, "current")?.unwrap();
                header["document"]["history"] = Value::Null;
                write(conn, Domain::Workspace, "current", &header)
            })
            .unwrap();
        drop(storage);
        std::fs::create_dir(root.path().join("recovery/not-a-file.sqlite3")).unwrap();
        let service = ConfigurationRecovery::open(root.path().into(), "bad".into()).unwrap();
        let doc = service.document(&id).unwrap();
        let versions = service.versions(&id, &doc.revision).unwrap();
        assert!(versions.versions.iter().any(|v| v.source == "backup"));
        assert!(versions
            .warnings
            .iter()
            .any(|w| w.contains("index unavailable")));
        assert!(versions.warnings.iter().any(|w| w.contains("nonregular")));
        assert!(service
            .version("profile:foreign", &doc.revision, &versions.versions[0].id)
            .is_err());
    }
    #[test]
    // Verify that collector deduplicates and caps before retaining more values
    fn collector_deduplicates_and_caps_before_retaining_more_values() {
        let mut collector = Collector::default();
        let profile = crate::profiles::ProfileData::default().profiles.remove(0);
        collector
            .add(profile.clone(), "first".into(), "history", Some(1))
            .unwrap();
        collector
            .add(profile.clone(), "duplicate".into(), "backup", Some(2))
            .unwrap();
        assert_eq!(collector.entries.len(), 1);
        collector.bytes = TOTAL_BYTES;
        let mut changed = profile;
        changed.name = "different".into();
        assert!(collector
            .add(changed, "too much".into(), "history", None)
            .is_err());
        assert_eq!(collector.entries.len(), 1);
    }
    #[test]
    // Verify that tampered history foreign generation and sidecar backups are not candidates
    fn tampered_history_foreign_generation_and_sidecar_backups_are_not_candidates() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let data = store.load().unwrap();
        let id = format!("profile:{}", data.profiles[0].id);
        store
            .storage()
            .backup(root.path().join("recovery/sidecar.sqlite3"))
            .unwrap();
        let changed = store
            .edit(data.configuration_revision, "Real edit", |data| {
                data.profiles[0].name = "New".into();
                Ok(())
            })
            .unwrap();
        let history = changed.history.undo.last().unwrap().id.clone();
        drop(store);
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .transaction(move |conn| {
                let mut entry = stream_dock_manager_storage::read_json::<Value>(
                    conn,
                    Domain::History,
                    &history,
                    32 * 1024 * 1024,
                )?
                .unwrap();
                entry["before_hash"] = Value::String("tampered".into());
                write(conn, Domain::History, &history, &entry)
            })
            .unwrap();
        drop(storage);
        std::fs::write(
            root.path().join("recovery/sidecar.sqlite3-wal"),
            "not a snapshot",
        )
        .unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(foreign.path().join("profiles")).unwrap();
        store.load().unwrap();
        store
            .storage()
            .backup(root.path().join("recovery/foreign.sqlite3"))
            .unwrap();
        drop(store);
        let service = ConfigurationRecovery::open(root.path().into(), "broken".into()).unwrap();
        let current = service.document(&id).unwrap();
        let found = service.versions(&id, &current.revision).unwrap();
        assert!(!found
            .versions
            .iter()
            .any(|item| item.label.contains("Real edit")
                || item.label.contains("foreign.sqlite3")
                || item.label.contains("sidecar.sqlite3")));
        assert!(found.warnings.iter().any(|w| w.contains("integrity")));
        assert!(found
            .warnings
            .iter()
            .any(|w| w.contains("Different storage generation")));
        assert!(found.warnings.iter().any(|w| w.contains("sidecars")));
    }
}
