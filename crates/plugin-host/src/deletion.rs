//! Package files are staged outside SQLite; a SQLite journal makes crash recovery explicit
use super::*;
use stream_dock_manager_storage::{Domain, Storage};

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct DeletedBackup {
    pub id: String,
    pub sole_package: bool,
}

// Require a regular directory without following a symlink
fn regular_dir(path: &Path) -> Result<bool, String> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("Invalid plugin backup directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

// Remove the selected plugin backup directories
fn purge_backups(root: &Path, journal: &Value) -> Result<(), String> {
    let Some(backups) = journal.get("backups") else {
        return Ok(());
    };
    let backups: Vec<DeletedBackup> =
        serde_json::from_value(backups.clone()).map_err(|e| e.to_string())?;
    let owner = journal["plugin_id"]
        .as_str()
        .filter(|id| safe_id(id))
        .ok_or("Invalid package deletion owner")?;
    let base = root.join(".backups");
    let index = root.join(".backup-index");
    if !regular_dir(&base)? {
        return Ok(());
    }
    let has_index = regular_dir(&index)?;
    for backup in backups {
        uuid::Uuid::parse_str(&backup.id).map_err(|_| "Invalid backup identity")?;
        let parent = base.join(&backup.id);
        if regular_dir(&parent)? {
            let package = parent.join(owner);
            if regular_dir(&package)? {
                std::fs::remove_dir_all(package).map_err(|e| e.to_string())?;
                sync(&parent)?;
            }
            if backup.sole_package {
                std::fs::remove_dir(&parent).map_err(|e| e.to_string())?;
                sync(&base)?;
            }
        }
        if backup.sole_package && has_index {
            match std::fs::remove_file(index.join(format!("{}.json", backup.id))) {
                Ok(()) => sync(&index)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    Ok(())
}

// Synchronize the managed directory to persistent storage
fn sync(root: &Path) -> Result<(), String> {
    // Match storage's directory-sync policy: ordinary File::open cannot open
    // directory handles on Windows, and directory fsync is a Unix operation.
    #[cfg(unix)]
    std::fs::File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}
// Collect paths owned by the deletion plan
fn paths(root: &Path, key: &str, value: &Value) -> Result<(PathBuf, PathBuf, bool), String> {
    let operation = key
        .strip_prefix("package-deletion:")
        .ok_or("Invalid package deletion journal")?;
    if uuid::Uuid::parse_str(operation).is_err() {
        return Err("Invalid package deletion identity".into());
    }
    let target = value["target"]
        .as_str()
        .filter(|s| safe_relative(s) && !s.contains('/'))
        .ok_or("Invalid package deletion target")?;
    let owner = value["plugin_id"]
        .as_str()
        .filter(|s| safe_id(s))
        .ok_or("Invalid package deletion owner")?;
    let _ = owner;
    let stage = root.join(format!(".deleted-{operation}"));
    let target = root.join(target);
    for path in [&stage, &target] {
        if path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir())
        {
            return Err("Package deletion recovery path is not a regular directory".into());
        }
    }
    Ok((
        target,
        stage,
        value["committed"]
            .as_bool()
            .ok_or("Invalid package deletion state")?,
    ))
}
// Recover for the current operation
pub(super) fn recover(root: &Path, storage: &Storage) -> Result<(), String> {
    for (key, value) in
        storage.list_json::<Value>(Domain::PluginMetadata, 8192, 16 * 1024 * 1024)?
    {
        if !key.starts_with("package-deletion:") {
            continue;
        }
        let (target, stage, committed) = paths(root, &key, &value)?;
        if stage.exists() {
            if committed {
                std::fs::remove_dir_all(&stage).map_err(|e| e.to_string())?;
            } else if !target.exists() {
                std::fs::rename(&stage, &target).map_err(|e| e.to_string())?;
            } else {
                return Err(
                    "Both original and staged package exist; deletion recovery requires inspection"
                        .into(),
                );
            }
            sync(root)?;
        } else if !committed && !target.exists() {
            return Err(
                "Uncommitted package deletion is missing both copies; data preserved".into(),
            );
        }
        if committed {
            purge_backups(root, &value)?;
        }
        storage.delete(Domain::PluginMetadata, &key)?;
    }
    Ok(())
}
impl PluginHost {
    // Delete owned package
    pub(super) fn delete_owned_package(
        &mut self,
        id: &str,
        keep: bool,
        purge_extras: bool,
        retained_bytes: usize,
        commit: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<(), String>
            + Send
            + 'static,
    ) -> Result<(), String> {
        // A previous attempt can have staged the files before returning an
        // error. Restore an uncommitted stage before resolving the live path.
        recover(&self.root, self.storage())?;
        let path = self.deletion_path(id)?;
        self.enable(id, false)?;
        let name = self
            .plugins
            .get(id)
            .map(|p| p.info.name.clone())
            .unwrap_or_else(|| id.into());
        let operation = uuid::Uuid::new_v4().to_string();
        let key = format!("package-deletion:{operation}");
        let mut backups = vec![];
        if purge_extras {
            for (_, backup) in self.backup_ids(id)? {
                let package = self.backup_path(id, &backup)?;
                let parent = package
                    .parent()
                    .ok_or("Missing backup transaction directory")?;
                let sole_package = std::fs::read_dir(parent)
                    .map_err(|e| e.to_string())?
                    .nth(1)
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .is_none();
                backups.push(DeletedBackup {
                    id: backup,
                    sole_package,
                });
            }
            if !backups.is_empty() {
                regular_dir(&self.root.join(".backup-index"))?;
            }
        }
        let mut journal = json!({"plugin_id":id,"target":path.file_name().and_then(|v|v.to_str()).ok_or("Invalid package directory name")?,"committed":false});
        if purge_extras {
            journal["backups"] = json!(backups);
        }
        self.storage()
            .write_json(Domain::PluginMetadata, &key, &journal, 1024 * 1024)?;
        let staged = self.root.join(format!(".deleted-{operation}"));
        std::fs::rename(&path, &staged).map_err(|e| e.to_string())?;
        sync(&self.root)?;
        let previous = {
            let mut hub = self.hub.lock().unwrap();
            if keep {
                hub.retained.insert(id.into(), name)
            } else {
                hub.retained.remove(id)
            }
        };
        if let Err(error) =
            self.forget_plugin_transaction(id, Some((key.clone(), journal)), retained_bytes, commit)
        {
            let mut hub = self.hub.lock().unwrap();
            if let Some(value) = previous {
                hub.retained.insert(id.into(), value);
            } else {
                hub.retained.remove(id);
            }
            drop(hub);
            return Err(match recover(&self.root, self.storage()) {
                Ok(()) => error,
                Err(recovery) => format!("{error}; package deletion recovery needed: {recovery}"),
            });
        }
        // After this point the uninstall is committed, even if inert file cleanup fails
        let cleanup = recover(&self.root, self.storage());
        let logs = if purge_extras {
            self.logs.forget_plugin(id)
        } else {
            Ok(())
        };
        cleanup
            .and(logs)
            .map_err(|e| format!("Plugin removed; cleanup remains pending: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify deletion retry recovers an uncommitted stage and preserves retained data
    fn deletion_retry_recovers_an_uncommitted_stage_and_preserves_retained_data() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("fixture");
        lifecycle_tests::package(&package, "fixture.owner");
        let mut host = lifecycle_tests::host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.globals
                .insert("fixture.owner".into(), json!({"keep": true}));
            hub.persist().unwrap();
        }
        let operation = uuid::Uuid::new_v4().to_string();
        let key = format!("package-deletion:{operation}");
        let stage = root.path().join(format!(".deleted-{operation}"));
        host.storage()
            .write_json(
                Domain::PluginMetadata,
                &key,
                &json!({"plugin_id":"fixture.owner","target":"fixture","committed":false}),
                4096,
            )
            .unwrap();
        std::fs::rename(&package, &stage).unwrap();

        host.delete_with_retention("fixture.owner", true).unwrap();

        assert!(!package.exists());
        assert!(!stage.exists());
        assert!(!host.plugins.contains_key("fixture.owner"));
        let hub = host.hub.lock().unwrap();
        assert_eq!(hub.globals["fixture.owner"], json!({"keep": true}));
        assert!(hub.retained.contains_key("fixture.owner"));
        assert!(host
            .storage()
            .list_json::<Value>(Domain::PluginMetadata, 8192, 16 * 1024 * 1024)
            .unwrap()
            .iter()
            .all(|(key, _)| !key.starts_with("package-deletion:")));
    }

    #[test]
    // Verify uninstall composes scoped preferences and rolls back files on SQL rejection
    fn uninstall_composes_scoped_preferences_and_rolls_back_files_on_sql_rejection() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("fixture");
        lifecycle_tests::package(&package, "fixture.owner");
        let mut host = lifecycle_tests::host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.globals
                .insert("fixture.owner".into(), json!({"keep":true}));
            hub.persist().unwrap();
        }
        host.storage()
            .write_json(
                Domain::LocalPreferences,
                "owned",
                &json!({"before":true}),
                1024,
            )
            .unwrap();
        assert!(host
            .delete_with_retention_sql("fixture.owner", false, 1024, |tx| {
                stream_dock_manager_storage::write_json(
                    tx,
                    Domain::LocalPreferences,
                    "owned",
                    &json!({"after":true}),
                    1024,
                )?;
                Err("injected scoped preference rejection".into())
            })
            .is_err());
        assert!(package.join("manifest.json").is_file());
        assert!(host
            .hub
            .lock()
            .unwrap()
            .globals
            .contains_key("fixture.owner"));
        assert_eq!(
            host.storage()
                .read_json::<Value>(Domain::LocalPreferences, "owned", 1024)
                .unwrap(),
            Some(json!({"before":true}))
        );
        host.delete_with_retention_sql("fixture.owner", false, 1024, |tx| {
            stream_dock_manager_storage::write_json(
                tx,
                Domain::LocalPreferences,
                "owned",
                &json!({"after":true}),
                1024,
            )
        })
        .unwrap();
        assert!(!package.exists());
        assert!(!host
            .hub
            .lock()
            .unwrap()
            .globals
            .contains_key("fixture.owner"));
        assert_eq!(
            host.storage()
                .read_json::<Value>(Domain::LocalPreferences, "owned", 1024)
                .unwrap(),
            Some(json!({"after":true}))
        );
    }
    #[test]
    // Verify recovery uses SQL commit marker to restore or finish file deletion
    fn recovery_uses_sql_commit_marker_to_restore_or_finish_file_deletion() {
        for committed in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let storage = Storage::open(root.path().join(".state")).unwrap();
            let operation = uuid::Uuid::new_v4().to_string();
            let key = format!("package-deletion:{operation}");
            let stage = root.path().join(format!(".deleted-{operation}"));
            std::fs::create_dir(&stage).unwrap();
            std::fs::write(stage.join("payload"), b"package").unwrap();
            storage
                .write_json(
                    Domain::PluginMetadata,
                    &key,
                    &json!({"plugin_id":"fixture","target":"package","committed":committed}),
                    4096,
                )
                .unwrap();
            recover(root.path(), &storage).unwrap();
            assert!(!stage.exists());
            assert_eq!(root.path().join("package/payload").exists(), !committed);
            assert!(storage
                .read_json::<Value>(Domain::PluginMetadata, &key, 4096)
                .unwrap()
                .is_none());
        }
    }
}
