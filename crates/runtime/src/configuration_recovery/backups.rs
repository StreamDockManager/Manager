//! Selective copies from immutable SQLite backup snapshots. No whole-database restore
use super::*;
use crate::{
    actions::Action,
    profiles::{Profile, ProfileData},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
use stream_dock_manager_storage::inspect_recovery_backups;

const BACKUPS: usize = 32;
const PROFILES: usize = 256;
const BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryBackup {
    pub id: String,
    pub label: String,
    #[ts(type = "number | null")]
    pub timestamp_ms: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryBackups {
    pub revision: String,
    pub backups: Vec<ConfigurationRecoveryBackup>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryBackupProfile {
    pub id: String,
    pub name: String,
    pub model_id: String,
    pub valid: bool,
    pub issues: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryBackupProfiles {
    pub backup_id: String,
    pub revision: String,
    pub profiles: Vec<ConfigurationRecoveryBackupProfile>,
    pub warnings: Vec<String>,
}
#[derive(Clone)]
pub(super) struct Source {
    pub(super) item: ConfigurationRecoveryBackup,
    digest: String,
}
#[derive(Default)]
pub(super) struct BackupCache {
    pub(super) revision: String,
    pub(super) sources: Vec<Source>,
    reviewed: Option<String>,
}
struct Rows {
    profiles: Vec<(String, Value)>,
    deleted: crate::media_deletion::DeletedMedia,
    digest: String,
}
// Read current-format backup rows and hash their validated workspace representation
fn rows(conn: &Connection) -> Result<Rows, String> {
    let profiles =
        stream_dock_manager_storage::list_json::<Value>(conn, Domain::Profiles, PROFILES, LIMIT)?;
    let header = read(conn, Domain::Workspace, "current")?;
    if let Some(header) = &header {
        if header["document"]["schema_version"].as_u64() != Some(1) {
            return Err("Backup workspace is not current-format".into());
        }
        if let Some(required) = header["document"]["minimum_manager"].as_str() {
            if semver::Version::parse(required).map_err(|e| e.to_string())?
                > semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
            {
                return Err("Backup requires a newer Manager".into());
            }
        }
    }
    let deleted = crate::profile_database::DELETED_MEDIA_RECORD
        .read_from(conn)?
        .unwrap_or_default();
    let raw = serde_json::to_vec(&(&profiles, &header, &deleted)).map_err(|e| e.to_string())?;
    if raw.len() > BYTES {
        return Err("Backup profiles exceed the 32 MiB recovery limit".into());
    }
    Ok(Rows {
        profiles,
        deleted,
        digest: hex::encode(Sha256::digest(raw)),
    })
}
// Report whether there is recoverable backups
pub fn has_recoverable_backups(root: &Path) -> Result<bool, String> {
    match root.join("recovery").symlink_metadata() {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Ok(false)
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    }
    let (found, _) = inspect_recovery_backups(root, BACKUPS, |_, _, conn| {
        // A valid initialized workspace can contain only settings/saved items
        // It is still a meaningful full-restore source, unlike an empty schema
        if read(conn, Domain::Workspace, "current")?.is_some() && overall(conn).is_ok() {
            return Ok(true);
        }
        Ok(rows(conn)?.profiles.into_iter().any(|(key, value)| {
            let record = Record {
                id: String::new(),
                key,
                kind: "profile",
                value: value.clone(),
            };
            record_issues(&record, &value).is_empty()
        }))
    })?;
    Ok(found.into_iter().any(|value| value))
}
// Reject an invalid backup profile before decoding it into the typed profile model
fn profile_row(key: &str, value: &Value) -> Result<Profile, String> {
    let record = Record {
        id: String::new(),
        key: key.into(),
        kind: "profile",
        value: value.clone(),
    };
    let issues = record_issues(&record, value);
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }
    serde_json::from_value(value.clone()).map_err(|e| e.to_string())
}
// Check source media
fn check_source_media(
    profile: &Profile,
    deleted: &crate::media_deletion::DeletedMedia,
) -> Result<(), String> {
    let mut data = ProfileData::default();
    data.profiles = vec![profile.clone()];
    data.saved_macros.clear();
    if crate::profiles::referenced_assets(&data)
        .iter()
        .any(|id| deleted.assets.contains(id))
    {
        Err("Backup profile references permanently deleted media".into())
    } else {
        Ok(())
    }
}
// Read current profiles
fn current_profiles(conn: &Connection) -> Result<Vec<(String, Value)>, String> {
    stream_dock_manager_storage::list_json(conn, Domain::Profiles, 4096, LIMIT)
}
// Check references
pub(super) fn check_references(
    conn: &Connection,
    root: &Path,
    profile: &Profile,
    available: &HashSet<String>,
) -> Result<(), String> {
    let macros = stream_dock_manager_storage::list_json::<crate::builtins::SavedMacro>(
        conn,
        Domain::SavedMacros,
        4096,
        LIMIT,
    )?
    .into_iter()
    .map(|(_, v)| v)
    .collect::<Vec<_>>();
    let mut issues = Vec::new();
    let mut used_macros = HashSet::new();
    for page in &profile.pages {
        for binding in page.bindings.values() {
            crate::macro_inputs::validate_action_calls(&binding.action, &macros, &[])?;
            crate::keyboard_macro::validation::expanded_event_count(&binding.action, |id| {
                macros.iter().find(|m| m.id == id).map(|m| &m.action)
            })?;
            // Ordinary runtime validation permits temporarily missing macros;
            // selective recovery must not introduce unresolved dependencies
            let mut pending = vec![&binding.action];
            let mut visited = HashSet::new();
            while let Some(action) = pending.pop() {
                action.visit(&mut |node| match node {
                    Action::Profile {profile_id} if !available.contains(profile_id) => issues.push(format!("Referenced profile {profile_id} is missing; select it from this backup too")),
                    Action::Page {page_id} if !profile.pages.iter().any(|p|p.id==*page_id) => issues.push(format!("Referenced page {page_id} is missing")),
                    Action::Builtin {action} => {
                        if let crate::builtins::BuiltinAction::SavedMacro {macro_id,..}=action.as_ref() {
                            if visited.insert(macro_id.clone()) {
                                used_macros.insert(macro_id.clone());
                                if let Some(saved)=macros.iter().find(|m|m.id==*macro_id) { pending.push(&saved.action); }
                                else { issues.push(format!("Referenced saved macro {macro_id} is missing; macros are not imported from backups")); }
                            }
                        }
                    }
                    _=>{},
                });
            }
        }
    }
    let mut data = ProfileData::default();
    data.profiles = vec![profile.clone()];
    data.saved_macros = macros
        .into_iter()
        .filter(|saved| used_macros.contains(&saved.id))
        .collect();
    issues.extend(check_files(conn, root, &data).err());
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues.join("; "))
    }
}
// Check files
pub(super) fn check_files(
    conn: &Connection,
    root: &Path,
    data: &ProfileData,
) -> Result<(), String> {
    let mut issues = Vec::new();
    let deleted = crate::profile_database::DELETED_MEDIA_RECORD
        .read_from(conn)?
        .unwrap_or_default();
    let directory = root.join("profiles/assets");
    for id in crate::profiles::referenced_assets(data) {
        if deleted.assets.contains(&id) {
            issues.push(format!("Media {id} was permanently deleted"));
            continue;
        }
        let safe_dir = directory
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
        let found = crate::media::ASSET_EXTENSIONS
            .iter()
            .filter(|ext| {
                directory
                    .join(format!("{id}.{ext}"))
                    .symlink_metadata()
                    .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
            })
            .count();
        if !safe_dir || found != 1 {
            issues.push(format!(
                "Media {id} is missing or ambiguous; media is not copied from SQLite backups"
            ));
        }
    }
    let resources = root.join("profiles/plugin-resources");
    for id in crate::vendor_resources::referenced(&data) {
        let safe_dir = resources
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
        let path = resources.join(&id);
        let safe_file = path.symlink_metadata().is_ok_and(|m| {
            m.is_file()
                && !m.file_type().is_symlink()
                && m.len() <= crate::vendor_resources::MAX_FILE
        });
        if !safe_dir || !safe_file {
            issues.push(format!("Managed plugin resource {id} is missing or unsafe"));
            continue;
        }
        let bytes = stream_dock_manager_storage::bounded_io::read_from(
            std::fs::File::open(path).map_err(|e| e.to_string())?,
            crate::vendor_resources::MAX_FILE as usize,
            "Plugin resource exceeds 16 MiB",
        )?;
        if hex::encode(Sha256::digest(bytes)) != id.split('.').next().unwrap_or("") {
            issues.push(format!("Managed plugin resource {id} content changed"));
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(issues.join("; "))
    }
}
impl ConfigurationRecovery {
    // Inspect backup catalog
    pub fn backup_catalog(&self) -> Result<ConfigurationRecoveryBackups, String> {
        self.editor.refresh()?;
        let revision = self.editor.revision()?;
        let (sources, warnings) =
            inspect_recovery_backups(&self.root, BACKUPS, |name, time, conn| {
                // Full restore remains available even if selective profile parsing fails
                let parsed = rows(conn);
                if parsed.as_ref().is_ok_and(|r| r.profiles.is_empty())
                    && read(conn, Domain::Workspace, "current")?.is_none()
                {
                    return Ok(None);
                }
                Ok(Some(Source {
                    item: ConfigurationRecoveryBackup {
                        id: hex::encode(Sha256::digest(name.as_bytes())),
                        label: name.into(),
                        timestamp_ms: time,
                    },
                    digest: parsed.map(|rows| rows.digest).unwrap_or_default(),
                }))
            })?;
        let sources: Vec<_> = sources.into_iter().flatten().collect();
        if self.editor.revision()? != revision {
            return Err("Configuration changed; reload backups".into());
        }
        let response = ConfigurationRecoveryBackups {
            revision: revision.clone(),
            backups: sources.iter().map(|s| s.item.clone()).collect(),
            warnings,
        };
        *self
            .backups
            .lock()
            .map_err(|_| "Backup cache lock failed")? = BackupCache {
            revision,
            sources,
            reviewed: None,
        };
        Ok(response)
    }
    // Inspect backup rows
    fn backup_rows(&self, id: &str, revision: &str, require_review: bool) -> Result<Rows, String> {
        self.editor.read(|_, current| {
            if current == revision {
                Ok(())
            } else {
                Err("Configuration changed; reload backups".into())
            }
        })?;
        let source = {
            let cache = self
                .backups
                .lock()
                .map_err(|_| "Backup cache lock failed")?;
            if cache.revision != revision
                || (require_review && cache.reviewed.as_deref() != Some(id))
            {
                return Err("Reload and review the selected backup".into());
            }
            cache
                .sources
                .iter()
                .find(|s| s.item.id == id)
                .cloned()
                .ok_or("Unknown backup; reload backups")?
        };
        let (found, _) = inspect_recovery_backups(&self.root, BACKUPS, |name, _, conn| {
            if name == source.item.label {
                rows(conn).map(Some)
            } else {
                Ok(None)
            }
        })?;
        let rows = found
            .into_iter()
            .flatten()
            .next()
            .ok_or("Backup is unavailable; reload backups")?;
        if rows.digest != source.digest {
            return Err("Backup profiles changed; reload backups before transferring".into());
        }
        Ok(rows)
    }
    // Inspect backup profiles
    pub fn backup_profiles(
        &self,
        backup_id: &str,
        revision: &str,
    ) -> Result<ConfigurationRecoveryBackupProfiles, String> {
        let rows = self.backup_rows(backup_id, revision, false)?;
        let profiles = self.editor.read(|conn, current| {
            if current != revision {
                return Err("Configuration changed; reload backups".into());
            }
            let mut available: HashSet<_> = current_profiles(conn)?
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            available.extend(
                rows.profiles
                    .iter()
                    .filter(|(id, value)| profile_row(id, value).is_ok())
                    .map(|(id, _)| id.clone()),
            );
            Ok(rows
                .profiles
                .iter()
                .map(|(id, value)| {
                    let result = profile_row(id, value).and_then(|p| {
                        check_source_media(&p, &rows.deleted)?;
                        check_references(conn, &self.root, &p, &available)
                    });
                    let issues: Vec<_> = result.err().into_iter().collect();
                    ConfigurationRecoveryBackupProfile {
                        id: id.clone(),
                        name: value["name"].as_str().unwrap_or(id).into(),
                        model_id: value["model_id"].as_str().unwrap_or("").into(),
                        valid: issues.is_empty(),
                        issues,
                    }
                })
                .collect())
        })?;
        self.backups
            .lock()
            .map_err(|_| "Backup cache lock failed")?
            .reviewed = Some(backup_id.into());
        Ok(ConfigurationRecoveryBackupProfiles{backup_id:backup_id.into(),revision:revision.into(),profiles,warnings:vec!["Only selected profiles are copied. Settings, macros, plugins, media, and history are not restored. Referenced profiles must already exist or be selected together.".into()]})
    }
    // Import only explicitly selected profiles from the reviewed backup
    pub fn transfer_backup_profiles(
        &self,
        backup_id: &str,
        revision: &str,
        profile_ids: &[String],
    ) -> Result<ConfigurationRecoveryReport, String> {
        if profile_ids.is_empty() || profile_ids.len() > PROFILES {
            return Err("Select between 1 and 256 profiles".into());
        }
        let selected: HashSet<_> = profile_ids.iter().collect();
        if selected.len() != profile_ids.len() {
            return Err("Duplicate profile selection".into());
        }
        let source = self.backup_rows(backup_id, revision, true)?;
        let mut profiles = Vec::new();
        for id in profile_ids {
            let (_, value) = source
                .profiles
                .iter()
                .find(|(key, _)| key == id)
                .ok_or("Selected profile missing from backup")?;
            let profile = profile_row(id, value)?;
            check_source_media(&profile, &source.deleted)?;
            profiles.push(profile);
        }
        let ids: HashMap<_, _> = profiles
            .iter()
            .map(|p| (p.id.clone(), uuid::Uuid::new_v4().to_string()))
            .collect();
        let apply = |conn: &Connection| -> Result<(), String> {
            let existing = current_profiles(conn)?;
            if existing.len() + profiles.len() > 4096 {
                return Err("Profile count limit exceeded".into());
            }
            let mut available: HashSet<_> = existing.iter().map(|(id, _)| id.clone()).collect();
            available.extend(ids.values().cloned());
            let mut names: HashSet<_> = existing
                .iter()
                .filter_map(|(_, v)| v["name"].as_str().map(str::to_owned))
                .collect();
            let mut imported = Vec::new();
            for original in &profiles {
                let mut profile = original.clone();
                profile.id = ids[&original.id].clone();
                if existing.iter().any(|(id, _)| id == &profile.id) {
                    return Err("Generated profile identity collision; retry".into());
                }
                for page in &mut profile.pages {
                    for binding in page.bindings.values_mut() {
                        binding.action.visit_mut(&mut |a| {
                            if let Action::Profile { profile_id } = a {
                                if let Some(new) = ids.get(profile_id) {
                                    *profile_id = new.clone();
                                }
                            }
                        });
                    }
                }
                let mut number = 1;
                let base = profile.name.clone();
                while names.contains(&profile.name) {
                    let suffix = format!(" (recovered {number})");
                    let mut prefix = base.clone();
                    while prefix.len() + suffix.len() > 160 {
                        prefix.pop();
                    }
                    profile.name = format!("{prefix}{suffix}");
                    number += 1;
                }
                names.insert(profile.name.clone());
                profile.validate()?;
                check_references(conn, &self.root, &profile, &available)?;
                imported.push(profile);
            }
            if read(conn, Domain::Workspace, "current")?.is_none() {
                if !existing.is_empty() {
                    return Err(
                        "Workspace metadata missing; repair it before transferring profiles".into(),
                    );
                }
                // A missing header is safe to initialize only in an otherwise empty
                // workspace. Never prune orphaned records during selective recovery
                for domain in [
                    Domain::Workspace,
                    Domain::SavedMacros,
                    Domain::AppearancePresets,
                    Domain::ProfileRules,
                    Domain::History,
                    Domain::HistoryPlugins,
                ] {
                    let count: u64 = conn
                        .query_row(
                            &format!("SELECT count(*) FROM {}", domain.table()),
                            [],
                            |r| r.get(0),
                        )
                        .map_err(|e| e.to_string())?;
                    if count != 0 {
                        return Err("Workspace metadata missing but preserved records exist; repair metadata before transferring".into());
                    }
                }
                let mut data = ProfileData::default();
                data.profiles.clear();
                data.saved_macros.clear();
                data.active_profile.clear();
                data.rules.clear();
                data.styles.clear();
                crate::profile_database::save(conn, &data)?;
            }
            let mut header =
                read(conn, Domain::Workspace, "current")?.ok_or("Workspace metadata missing")?;
            let index = header["profiles"]
                .as_array_mut()
                .ok_or("Invalid profile index")?;
            for profile in &imported {
                index.push(Value::String(profile.id.clone()));
                write(
                    conn,
                    Domain::Profiles,
                    &profile.id,
                    &serde_json::to_value(profile).map_err(|e| e.to_string())?,
                )?;
            }
            if header["document"]["active_profile"].as_str() == Some("") {
                header["document"]["active_profile"] = Value::String(imported[0].id.clone());
            }
            let next = header["document"]["configuration_revision"]
                .as_u64()
                .ok_or("Invalid configuration revision")?
                .checked_add(1)
                .ok_or("Configuration revision exhausted")?;
            header["document"]["configuration_revision"] = Value::from(next);
            write(conn, Domain::Workspace, "current", &header)?;
            overall(conn)
        };
        self.editor.preview(revision, |conn| apply(conn))?;
        // Recheck source immediately before destination transaction; commit never edits it
        self.backup_rows(backup_id, revision, true)?;
        let (_, backup) = self.editor.commit(revision, apply)?;
        *self
            .last_backup
            .lock()
            .map_err(|_| "Recovery lock failed")? = Some(backup.display().to_string());
        *self
            .backups
            .lock()
            .map_err(|_| "Backup cache lock failed")? = BackupCache::default();
        self.report()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_storage::Storage;
    // Construct an isolated backups fixture
    fn fixture() -> (tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let mut data = ProfileData::default();
        let mut second = data.profiles[0].clone();
        second.id = "second".into();
        second.name = "Second".into();
        data.profiles.push(second);
        storage
            .transaction(move |conn| crate::profile_database::save(conn, &data))
            .unwrap();
        let name = "selected.sqlite3";
        storage
            .backup(root.path().join("recovery").join(name))
            .unwrap();
        drop(storage);
        (root, name.into())
    }
    // Stage the backups change for explicit review
    fn review(recovery: &ConfigurationRecovery, name: &str) -> (String, String) {
        let catalog = recovery.backup_catalog().unwrap();
        let id = catalog
            .backups
            .iter()
            .find(|s| s.label == name)
            .unwrap()
            .id
            .clone();
        recovery.backup_profiles(&id, &catalog.revision).unwrap();
        (id, catalog.revision)
    }
    #[test]
    // Verify that selected profiles are new copies and source is immutable
    fn selected_profiles_are_new_copies_and_source_is_immutable() {
        let (root, name) = fixture();
        let source = root.path().join("recovery").join(&name);
        let bytes = std::fs::read(&source).unwrap();
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = review(&recovery, &name);
        let report = recovery
            .transfer_backup_profiles(&id, &revision, &["default".into()])
            .unwrap();
        assert!(report.ready, "{:?}", report.issues);
        recovery
            .editor
            .read(|conn, _| {
                let data = crate::profile_database::load(conn)?.unwrap();
                assert_eq!(data.profiles.len(), 3);
                let added = data
                    .profiles
                    .iter()
                    .find(|p| p.id != "default" && p.id != "second")
                    .unwrap();
                assert!(added.name.contains("recovered"));
                assert_eq!(
                    data.profiles
                        .iter()
                        .find(|p| p.id == "default")
                        .unwrap()
                        .name,
                    "Default"
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(std::fs::read(source).unwrap(), bytes);
        assert!(recovery
            .transfer_backup_profiles(&id, &revision, &["second".into()])
            .is_err());
    }
    #[test]
    // Verify that missing primary can copy old generation into empty schema
    fn missing_primary_can_copy_old_generation_into_empty_schema() {
        let (root, name) = fixture();
        std::fs::remove_file(root.path().join(stream_dock_manager_storage::DATABASE_NAME)).unwrap();
        assert!(has_recoverable_backups(root.path()).unwrap());
        drop(Storage::open(root.path().into()).unwrap());
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = review(&recovery, &name);
        let result = recovery
            .transfer_backup_profiles(&id, &revision, &["second".into()])
            .unwrap();
        assert!(result.ready, "{:?}", result.issues);
        recovery
            .editor
            .read(|conn, _| {
                let data = crate::profile_database::load(conn)?.unwrap();
                assert_eq!(data.profiles.len(), 1);
                assert_eq!(data.profiles[0].name, "Second");
                Ok(())
            })
            .unwrap();
    }
    #[test]
    // Verify that unreviewed unknown duplicate and changed source are rejected
    fn unreviewed_unknown_duplicate_and_changed_source_are_rejected() {
        let (root, name) = fixture();
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let catalog = recovery.backup_catalog().unwrap();
        let id = catalog
            .backups
            .iter()
            .find(|s| s.label == name)
            .unwrap()
            .id
            .clone();
        assert!(recovery
            .transfer_backup_profiles(&id, &catalog.revision, &["default".into()])
            .is_err());
        assert!(recovery
            .backup_profiles("../manager.sqlite3", &catalog.revision)
            .is_err());
        recovery.backup_profiles(&id, &catalog.revision).unwrap();
        assert!(recovery
            .transfer_backup_profiles(
                &id,
                &catalog.revision,
                &["default".into(), "default".into()]
            )
            .is_err());
        let conn = Connection::open(root.path().join("recovery").join(name)).unwrap();
        let mut value = read(&conn, Domain::Profiles, "default").unwrap().unwrap();
        value["name"] = Value::String("Changed".into());
        write(&conn, Domain::Profiles, "default", &value).unwrap();
        drop(conn);
        assert!(recovery
            .transfer_backup_profiles(&id, &catalog.revision, &["default".into()])
            .is_err());
        assert_eq!(
            recovery
                .editor
                .read(|conn, _| Ok(current_profiles(conn)?.len()))
                .unwrap(),
            2
        );
    }
    #[test]
    // Verify that empty backup does not route new install into recovery
    fn empty_backup_does_not_route_new_install_into_recovery() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .backup(root.path().join("recovery/empty.sqlite3"))
            .unwrap();
        drop(storage);
        assert!(!has_recoverable_backups(root.path()).unwrap());
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        assert!(recovery.backup_catalog().unwrap().backups.is_empty());
    }
    #[test]
    // Verify that initialized workspace without profiles offers full recovery
    fn initialized_workspace_without_profiles_offers_full_recovery() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        let mut data = ProfileData::default();
        data.profiles.clear();
        data.active_profile.clear();
        storage
            .transaction(move |conn| crate::profile_database::save(conn, &data))
            .unwrap();
        storage
            .backup(root.path().join("recovery/settings-only.sqlite3"))
            .unwrap();
        drop(storage);
        assert!(has_recoverable_backups(root.path()).unwrap());
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let catalog = recovery.backup_catalog().unwrap();
        let source = catalog
            .backups
            .iter()
            .find(|b| b.label == "settings-only.sqlite3")
            .unwrap();
        let preview = recovery
            .preview_backup_restore(&source.id, &catalog.revision)
            .unwrap();
        assert!(preview.valid, "{:?}", preview.issues);
        assert_eq!(preview.profile_count, 0);
    }
    // Modify the fixture backup’s default profile through its SQLite record
    fn mutate_backup(root: &Path, name: &str, change: impl FnOnce(&mut Value)) {
        let conn = Connection::open(root.join("recovery").join(name)).unwrap();
        let mut value = read(&conn, Domain::Profiles, "default").unwrap().unwrap();
        change(&mut value);
        write(&conn, Domain::Profiles, "default", &value).unwrap();
    }
    #[test]
    // Verify that missing dependencies and invalid profiles are not transferable
    fn missing_dependencies_and_invalid_profiles_are_not_transferable() {
        for mode in ["macro", "media", "resource", "invalid"] {
            let (root, name) = fixture();
            mutate_backup(root.path(), &name, |value| match mode {
                "macro" => {
                    value["pages"][0]["bindings"]["key-1"]["action"] =
                        serde_json::to_value(Action::Builtin {
                            action: Box::new(crate::builtins::BuiltinAction::SavedMacro {
                                macro_id: "missing".into(),
                                arguments: Default::default(),
                            }),
                        })
                        .unwrap()
                }
                "media" => value["background_asset"] = Value::String("a".repeat(64)),
                "resource" => {
                    value["pages"][0]["bindings"]["key-1"]["action"] = serde_json::json!({"kind":"plugin","plugin_id":"fixture.plugin","action_id":"fixture.action","settings":{},"resources":{"file":format!("{}.txt","a".repeat(64))}})
                }
                _ => value["name"] = Value::String(String::new()),
            });
            let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
            let (id, revision) = review(&recovery, &name);
            let listed = recovery.backup_profiles(&id, &revision).unwrap();
            assert!(
                !listed
                    .profiles
                    .iter()
                    .find(|p| p.id == "default")
                    .unwrap()
                    .valid,
                "{mode}"
            );
            assert!(
                listed
                    .profiles
                    .iter()
                    .find(|p| p.id == "second")
                    .unwrap()
                    .valid
            );
            assert!(
                recovery
                    .transfer_backup_profiles(&id, &revision, &["second".into(), "default".into()])
                    .is_err(),
                "{mode}"
            );
            assert_eq!(
                recovery
                    .editor
                    .read(|conn, _| Ok(current_profiles(conn)?.len()))
                    .unwrap(),
                2
            );
        }
    }
    #[test]
    // Verify that cross profile targets require selection and are remapped
    fn cross_profile_targets_require_selection_and_are_remapped() {
        let (root, name) = fixture();
        mutate_backup(root.path(), &name, |value| {
            value["pages"][0]["bindings"]["key-1"]["action"] =
                serde_json::json!({"kind":"profile","profile_id":"second"})
        });
        std::fs::remove_file(root.path().join(stream_dock_manager_storage::DATABASE_NAME)).unwrap();
        drop(Storage::open(root.path().into()).unwrap());
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = review(&recovery, &name);
        assert!(recovery
            .transfer_backup_profiles(&id, &revision, &["default".into()])
            .is_err());
        recovery
            .transfer_backup_profiles(&id, &revision, &["default".into(), "second".into()])
            .unwrap();
        recovery.editor.read(|conn,_|{let data=crate::profile_database::load(conn)?.unwrap();let second=data.profiles.iter().find(|p|p.name=="Second").unwrap();let first=data.profiles.iter().find(|p|p.id!=second.id).unwrap();assert!(matches!(&first.pages[0].bindings["key-1"].action,Action::Profile{profile_id} if profile_id==&second.id));Ok(())}).unwrap();
    }
    #[test]
    // Verify that missing header does not prune orphaned evidence
    fn missing_header_does_not_prune_orphaned_evidence() {
        let (root, name) = fixture();
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .transaction(|conn| {
                conn.execute("DELETE FROM profiles", [])
                    .map_err(|e| e.to_string())?;
                conn.execute("DELETE FROM workspace_state", [])
                    .map_err(|e| e.to_string())?;
                write(
                    conn,
                    Domain::AppearancePresets,
                    "preserve",
                    &serde_json::json!({"evidence":"untouched"}),
                )
            })
            .unwrap();
        drop(storage);
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = review(&recovery, &name);
        assert!(recovery
            .transfer_backup_profiles(&id, &revision, &["default".into()])
            .unwrap_err()
            .contains("preserved records"));
        recovery
            .editor
            .read(|conn, _| {
                assert_eq!(
                    read(conn, Domain::AppearancePresets, "preserve")?.unwrap()["evidence"],
                    "untouched"
                );
                assert!(current_profiles(conn)?.is_empty());
                Ok(())
            })
            .unwrap();
    }
    #[test]
    // Verify that changed destination requires reloading and does not add profiles
    fn changed_destination_requires_reloading_and_does_not_add_profiles() {
        let (root, name) = fixture();
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = review(&recovery, &name);
        recovery
            .editor
            .commit(&revision, |conn| {
                write(
                    conn,
                    Domain::LocalPreferences,
                    "restart-recovery-once",
                    &Value::Bool(true),
                )
            })
            .unwrap();
        assert!(recovery
            .transfer_backup_profiles(&id, &revision, &["default".into()])
            .is_err());
        assert_eq!(
            recovery
                .editor
                .read(|conn, _| Ok(current_profiles(conn)?.len()))
                .unwrap(),
            2
        );
    }
    #[test]
    // Verify that deleted media and changed managed resource content are rejected
    fn deleted_media_and_changed_managed_resource_content_are_rejected() {
        let (root, name) = fixture();
        let id = "a".repeat(64);
        let mut profile = ProfileData::default().profiles.remove(0);
        profile.background_asset = Some(id.clone());
        let deleted = crate::media_deletion::DeletedMedia {
            schema_version: 1,
            revision: 1,
            assets: std::collections::BTreeSet::from([id.clone()]),
        };
        assert!(check_source_media(&profile, &deleted).is_err());
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .write_json(Domain::MediaTombstones, "deleted", &deleted, LIMIT)
            .unwrap();
        drop(storage);
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        recovery
            .editor
            .read(|conn, _| {
                assert!(
                    check_references(conn, root.path(), &profile, &HashSet::new())
                        .unwrap_err()
                        .contains("permanently deleted")
                );
                Ok(())
            })
            .unwrap();
        profile.background_asset = None;
        let resource = format!("{}.txt", hex::encode(Sha256::digest(b"expected")));
        profile.pages[0].bindings.get_mut("key-1").unwrap().action = Action::Plugin {
            plugin_id: "fixture.plugin".into(),
            action_id: "fixture.action".into(),
            settings: serde_json::json!({}),
            user_desired_state: None,
            resources: std::collections::BTreeMap::from([("file".into(), resource.clone())]),
        };
        let directory = root.path().join("profiles/plugin-resources");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(&resource), b"changed").unwrap();
        recovery
            .editor
            .read(|conn, _| {
                assert!(
                    check_references(conn, root.path(), &profile, &HashSet::new())
                        .unwrap_err()
                        .contains("content changed")
                );
                Ok(())
            })
            .unwrap();
        std::fs::write(directory.join(&resource), b"expected").unwrap();
        recovery
            .editor
            .read(|conn, _| check_references(conn, root.path(), &profile, &HashSet::new()))
            .unwrap();
        #[cfg(unix)]
        {
            std::fs::remove_file(directory.join(&resource)).unwrap();
            std::os::unix::fs::symlink(
                root.path().join("recovery").join(name),
                directory.join(&resource),
            )
            .unwrap();
            recovery
                .editor
                .read(|conn, _| {
                    assert!(
                        check_references(conn, root.path(), &profile, &HashSet::new())
                            .unwrap_err()
                            .contains("unsafe")
                    );
                    Ok(())
                })
                .unwrap();
        }
        #[cfg(not(unix))]
        let _ = name;
    }
    #[test]
    // Verify that non directory backup evidence does not block new install routing
    fn non_directory_backup_evidence_does_not_block_new_install_routing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("recovery");
        std::fs::write(&path, b"preserved evidence").unwrap();
        assert!(!has_recoverable_backups(root.path()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserved evidence");
        #[cfg(unix)]
        {
            std::fs::rename(&path, root.path().join("evidence")).unwrap();
            std::os::unix::fs::symlink(root.path().join("evidence"), &path).unwrap();
            assert!(!has_recoverable_backups(root.path()).unwrap());
            assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        }
    }
}
