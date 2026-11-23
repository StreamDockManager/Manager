//! Explicit whole-database replacement. Preview never publishes a database and
//! approval is bound to both the current primary and the reviewed backup bytes
use super::*;
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryRestorePreview {
    pub backup_id: String,
    pub revision: String,
    pub review_token: String,
    pub valid: bool,
    pub issues: Vec<String>,
    pub warnings: Vec<String>,
    pub profile_count: usize,
}
pub(super) struct RestoreReview {
    backup_id: String,
    revision: String,
    token: String,
    fingerprint: String,
}
// Validate restored profiles, saved macros and their external resource references
fn validate_restored(conn: &Connection, root: &std::path::Path) -> Result<(), String> {
    overall(conn)?;
    let data = crate::profile_database::load(conn)?.ok_or("Backup has no initialized workspace")?;
    let available: HashSet<_> = data.profiles.iter().map(|p| p.id.clone()).collect();
    for profile in &data.profiles {
        backups::check_references(conn, root, profile, &available)?;
    }
    // Unbound saved macros and appearance presets are restored too, so validate
    // their external resources and macro/profile references, not just live pages
    for saved in &data.saved_macros {
        let mut issues = Vec::new();
        saved.action.visit(&mut |action| match action {
            crate::actions::Action::Profile { profile_id } if !available.contains(profile_id) => {
                issues.push(format!(
                    "Saved macro {} references missing profile {profile_id}",
                    saved.name
                ))
            }
            crate::actions::Action::Builtin { action } => {
                if let crate::builtins::BuiltinAction::SavedMacro { macro_id, .. } = action.as_ref()
                {
                    if !data.saved_macros.iter().any(|m| m.id == *macro_id) {
                        issues.push(format!(
                            "Saved macro {} references missing macro {macro_id}",
                            saved.name
                        ));
                    }
                }
            }
            _ => {}
        });
        if !issues.is_empty() {
            return Err(issues.join("; "));
        }
    }
    backups::check_files(conn, root, &data)
}
// Reconcile full restore state with current persisted evidence
fn reconcile(
    conn: &Connection,
    sources: &[stream_dock_manager_storage::RecoverySource],
    root: &std::path::Path,
) -> Result<Vec<String>, String> {
    let warnings = crate::application_data::reconcile_deletions(conn, sources)?;
    // Remain in the isolated recovery shell after a successful replacement
    stream_dock_manager_storage::write_json(
        conn,
        Domain::LocalPreferences,
        "restart-recovery-once",
        &true,
        16,
    )?;
    stream_dock_manager_storage::delete(
        conn,
        Domain::LocalPreferences,
        "configuration-recovery-bypass",
    )?;
    validate_restored(conn, root)?;
    Ok(warnings)
}
impl ConfigurationRecovery {
    // Resolve the reviewed backup only while its cached configuration revision still matches
    fn restore_source(&self, backup_id: &str, revision: &str) -> Result<PathBuf, String> {
        if self.editor.revision()? != revision {
            return Err("Configuration changed; reload backups".into());
        }
        let cache = self
            .backups
            .lock()
            .map_err(|_| "Backup cache lock failed")?;
        if cache.revision != revision {
            return Err("Reload the backup catalogue".into());
        }
        let source = cache
            .sources
            .iter()
            .find(|s| s.item.id == backup_id)
            .ok_or("Unknown backup; reload the catalogue")?;
        Ok(recovery_directory(&self.root).join(&source.item.label))
    }
    // Validate a full backup and stage a review before any restore is accepted
    pub fn preview_backup_restore(
        &self,
        backup_id: &str,
        revision: &str,
    ) -> Result<ConfigurationRecoveryRestorePreview, String> {
        *self
            .restore_review
            .lock()
            .map_err(|_| "Restore review lock failed")? = None;
        let path = self.restore_source(backup_id, revision)?;
        let count = std::cell::Cell::new(0);
        let result = self
            .editor
            // Reconcile only the isolated preview copy before producing an approval fingerprint
            .preview_restore(revision, path, overall, |conn, sources| {
                let warnings = reconcile(conn, sources, &self.root)?;
                count.set(
                    crate::profile_database::load(conn)?.map_or(0, |data| data.profiles.len()),
                );
                Ok(warnings)
            });
        let mut response=ConfigurationRecoveryRestorePreview {
            backup_id:backup_id.into(),revision:revision.into(),review_token:String::new(),valid:false,
            issues:vec![], warnings:vec!["Replaces the entire configuration database, including profiles, saved items, settings, plugin state and history. Current database evidence and permanent media deletion records are preserved. External media, plugin binaries, credentials and files outside SQLite are not restored. Permanently deleted media references are detached rather than resurrected. Historical or draft media and external plugin installations may be unavailable; their records are retained.".into()],profile_count:count.get(),
        };
        match result {
            Ok(preview) => {
                response.valid = true;
                response.review_token = uuid::Uuid::new_v4().to_string();
                response.warnings.extend(preview.warnings);
                *self
                    .restore_review
                    .lock()
                    .map_err(|_| "Restore review lock failed")? = Some(RestoreReview {
                    backup_id: backup_id.into(),
                    revision: revision.into(),
                    token: response.review_token.clone(),
                    fingerprint: preview.source_fingerprint,
                });
            }
            Err(error) => response.issues.push(error),
        }
        Ok(response)
    }
    // Replace the database only with the approved backup identity, revision and fingerprint
    pub fn restore_backup(
        &self,
        backup_id: &str,
        revision: &str,
        review_token: &str,
    ) -> Result<ConfigurationRecoveryReport, String> {
        let path = self.restore_source(backup_id, revision)?;
        let review = self
            .restore_review
            .lock()
            .map_err(|_| "Restore review lock failed")?
            .take()
            .ok_or("Preview this backup before confirming replacement")?;
        if review.backup_id != backup_id
            || review.revision != revision
            || review.token != review_token
        {
            return Err("Restore approval is stale; preview this backup again".into());
        }
        let result = self.editor.restore_reviewed(
            revision,
            path,
            &review.fingerprint,
            overall,
            |conn, sources| reconcile(conn, sources, &self.root),
        )?;
        *self
            .last_backup
            .lock()
            .map_err(|_| "Recovery lock failed")? =
            Some(result.preserved_directory.display().to_string());
        *self
            .backups
            .lock()
            .map_err(|_| "Backup cache lock failed")? = Default::default();
        *self
            .versions
            .lock()
            .map_err(|_| "Version cache lock failed")? = Default::default();
        self.report()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_storage::Storage;
    // Construct an isolated full restore fixture
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .transaction(|conn| {
                crate::profile_database::save(conn, &crate::profiles::ProfileData::default())
            })
            .unwrap();
        storage
            .backup(recovery_directory(root.path()).join("full.sqlite3"))
            .unwrap();
        drop(storage);
        root
    }
    // Return the underlying full restore source
    fn source(recovery: &ConfigurationRecovery) -> (String, String) {
        let catalog = recovery.backup_catalog().unwrap();
        (
            catalog
                .backups
                .iter()
                .find(|s| s.label == "full.sqlite3")
                .unwrap()
                .id
                .clone(),
            catalog.revision,
        )
    }
    #[test]
    // Verify that corrupt primary preview is readonly and restore is explicit
    fn corrupt_primary_preview_is_readonly_and_restore_is_explicit() {
        let root = fixture();
        let path = database_path(root.path(), DATABASE_NAME);
        std::fs::write(&path, b"corrupt original evidence").unwrap();
        let recovery =
            ConfigurationRecovery::open(root.path().into(), "Unreadable primary".into()).unwrap();
        assert!(!recovery.report().unwrap().primary_readable);
        let (id, revision) = source(&recovery);
        assert!(recovery
            .restore_backup(&id, &revision, "unreviewed")
            .is_err());
        let preview = recovery.preview_backup_restore(&id, &revision).unwrap();
        assert!(preview.valid, "{:?}", preview.issues);
        assert_eq!(preview.profile_count, 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt original evidence");
        let backup_bytes =
            std::fs::read(recovery_directory(root.path()).join("full.sqlite3")).unwrap();
        let report = recovery
            .restore_backup(&id, &revision, &preview.review_token)
            .unwrap();
        assert!(report.ready, "{:?}", report.issues);
        assert!(report.primary_readable);
        assert!(path.is_file());
        assert!(!root.path().join(DATABASE_NAME).exists());
        let preserved = PathBuf::from(report.last_backup.unwrap());
        assert_eq!(
            std::fs::read(preserved.join(DATABASE_NAME)).unwrap(),
            b"corrupt original evidence"
        );
        assert_eq!(
            std::fs::read(recovery_directory(root.path()).join("full.sqlite3")).unwrap(),
            backup_bytes
        );
        recovery
            .editor
            .read(|conn, _| {
                assert_eq!(
                    stream_dock_manager_storage::read_json::<bool>(
                        conn,
                        Domain::LocalPreferences,
                        "restart-recovery-once",
                        16
                    )?,
                    Some(true)
                );
                Ok(())
            })
            .unwrap();
    }
    #[test]
    // Verify that changed source and wrong approval do not replace primary
    fn changed_source_and_wrong_approval_do_not_replace_primary() {
        let root = fixture();
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = source(&recovery);
        let preview = recovery.preview_backup_restore(&id, &revision).unwrap();
        assert!(preview.valid);
        assert!(recovery.restore_backup(&id, &revision, "wrong").is_err());
        let preview = recovery.preview_backup_restore(&id, &revision).unwrap();
        let conn = Connection::open(recovery_directory(root.path()).join("full.sqlite3")).unwrap();
        stream_dock_manager_storage::write_json(
            &conn,
            Domain::LocalPreferences,
            "restart-recovery-once",
            &true,
            16,
        )
        .unwrap();
        drop(conn);
        assert!(recovery
            .restore_backup(&id, &revision, &preview.review_token)
            .is_err());
        assert!(recovery.report().unwrap().ready);
    }
    #[test]
    // Verify that invalid full domain is visible but cannot be restored
    fn invalid_full_domain_is_visible_but_cannot_be_restored() {
        let root = fixture();
        let conn = Connection::open(recovery_directory(root.path()).join("full.sqlite3")).unwrap();
        stream_dock_manager_storage::write_json(
            &conn,
            Domain::ManagerPreferences,
            "unsupported-setting",
            &true,
            16,
        )
        .unwrap();
        drop(conn);
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = source(&recovery);
        let preview = recovery.preview_backup_restore(&id, &revision).unwrap();
        assert!(!preview.valid);
        assert!(!preview.issues.is_empty());
        assert!(preview.review_token.is_empty());
        assert!(recovery
            .restore_backup(&id, &revision, &preview.review_token)
            .is_err());
    }
    #[test]
    // Verify that permanent deletions are reconciled before dependency admission
    fn permanent_deletions_are_reconciled_before_dependency_admission() {
        let root = fixture();
        let asset = uuid::Uuid::new_v4().to_string();
        let conn = Connection::open(recovery_directory(root.path()).join("full.sqlite3")).unwrap();
        let mut data = crate::profile_database::load(&conn).unwrap().unwrap();
        data.profiles[0].background_asset = Some(asset.clone());
        crate::profile_database::save(&conn, &data).unwrap();
        drop(conn);
        let conn = Connection::open(database_path(root.path(), DATABASE_NAME)).unwrap();
        let deleted = crate::media_deletion::DeletedMedia {
            schema_version: 1,
            revision: 5,
            assets: [asset.clone()].into_iter().collect(),
        };
        stream_dock_manager_storage::write_json(
            &conn,
            Domain::MediaTombstones,
            "deleted",
            &deleted,
            4 * 1024 * 1024,
        )
        .unwrap();
        drop(conn);
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        let (id, revision) = source(&recovery);
        let preview = recovery.preview_backup_restore(&id, &revision).unwrap();
        assert!(preview.valid, "{:?}", preview.issues);
        let report = recovery
            .restore_backup(&id, &revision, &preview.review_token)
            .unwrap();
        assert!(report.ready, "{:?}", report.issues);
        recovery
            .editor
            .read(|conn, _| {
                assert!(crate::profile_database::load(conn)?.unwrap().profiles[0]
                    .background_asset
                    .is_none());
                assert!(crate::profile_database::DELETED_MEDIA_RECORD
                    .read_from(conn)?
                    .unwrap()
                    .assets
                    .contains(&asset));
                Ok(())
            })
            .unwrap();
    }
}
