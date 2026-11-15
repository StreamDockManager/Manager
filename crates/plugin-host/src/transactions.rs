//! Recoverable package replacement. Backup files and settings survive upgrades
use super::*;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    prior_provenance: Option<Value>,
    prior_installation: Option<String>,
    prior_inventory: Option<String>,
    id: String,
    backup: String,
    committed: bool,
    had_old: bool,
    was_enabled: bool,
}
// Save for the current operation
fn save(path: &Path, journal: &Journal) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing journal directory")?)
        .map_err(|e| e.to_string())?;
    f.write_all(&serde_json::to_vec(journal).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    f.as_file().sync_all().map_err(|e| e.to_string())?;
    f.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
// Require a regular managed directory
fn regular_directory(path: &Path) -> Result<bool, String> {
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("Replacement recovery requires regular managed directories".into()),
    }
}
// Report the pending work tracked by this owner
pub(super) fn pending(root: &Path) -> Result<bool, String> {
    let directory = root.join(".transactions");
    if !regular_directory(&directory)? {
        return Ok(false);
    }
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        if entry
            .map_err(|e| e.to_string())?
            .path()
            .extension()
            .and_then(|s| s.to_str())
            == Some("json")
        {
            return Ok(true);
        }
    }
    Ok(false)
}
// Recover for the current operation
pub(super) fn recover(
    root: &Path,
    storage: &stream_dock_manager_storage::Storage,
) -> Result<(), String> {
    let dir = root.join(".transactions");
    if !regular_directory(&dir)? {
        return Ok(());
    }
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        count += 1;
        if count > 256 {
            return Err("Replacement recovery exceeds 256 journals".into());
        }
        let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 16 * 1024 * 1024
        {
            return Err("Replacement journal is not a bounded regular file".into());
        }
        let mut bytes = vec![];
        std::fs::File::open(&path)
            .map_err(|e| e.to_string())?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("Replacement journal exceeds 16 MiB".into());
        }
        let raw: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        for field in ["prior_provenance", "prior_installation", "prior_inventory"] {
            if raw.get(field).is_none() {
                return Err(format!(
                    "Unsupported replacement journal: missing {field}; files preserved"
                ));
            }
        }
        let j: Journal = serde_json::from_value(raw).map_err(|e| e.to_string())?;
        if j.had_old && j.prior_installation.is_none() {
            return Err(
                "Unsupported replacement journal: prior installation required; files preserved"
                    .into(),
            );
        }
        if j.prior_installation
            .as_ref()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
        {
            return Err("Invalid prior installation identity; files preserved".into());
        }
        if j.had_old && j.prior_inventory.is_none() {
            return Err(
                "Unsupported replacement journal: prior inventory required; files preserved".into(),
            );
        }
        if !safe_id(&j.id)
            || uuid::Uuid::parse_str(&j.backup)
                .map_err(|_| "Invalid installation journal")?
                .to_string()
                != j.backup
            || j.prior_inventory
                .as_ref()
                .is_some_and(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("Invalid installation journal identity".into());
        }
        if !j.committed {
            let target = root.join(&j.id);
            let base = root.join(".backups");
            let directory = base.join(&j.backup);
            let backup = directory.join(&j.id);
            regular_directory(&base)?;
            regular_directory(&directory)?;
            let target_exists = regular_directory(&target)?;
            let backup_exists = regular_directory(&backup)?;
            if j.had_old {
                let prior = if backup_exists {
                    &backup
                } else if target_exists {
                    &target
                } else {
                    return Err(
                        "Replacement recovery is missing the prior package; files preserved".into(),
                    );
                };
                if let Some(expected) = &j.prior_inventory {
                    if hash_inventory(&packages::inventory(prior)?)? != *expected {
                        return Err("Prior package inventory changed; replacement recovery preserved all files".into());
                    }
                }
            }
            if backup_exists {
                if target_exists {
                    std::fs::rename(&target, root.join(format!(".failed-{}", j.backup)))
                        .map_err(|e| e.to_string())?;
                }
                std::fs::rename(&backup, &target).map_err(|e| e.to_string())?;
            } else if !j.had_old && target_exists {
                std::fs::rename(&target, root.join(format!(".failed-{}", j.backup)))
                    .map_err(|e| e.to_string())?;
            }
            let mut hub = Hub::load(storage)?;
            if j.was_enabled {
                hub.enabled.insert(j.id.clone());
            } else {
                hub.enabled.remove(&j.id);
            }
            if let Some(provenance) = j.prior_provenance {
                hub.provenance.insert(j.id.clone(), provenance);
            } else {
                hub.provenance.remove(&j.id);
            }
            hub.backup_receipts.remove(&j.backup);
            if let Some(token) = j.prior_installation {
                hub.installations.insert(j.id.clone(), token);
            } else {
                hub.installations.remove(&j.id);
            }
            hub.persist()?;
        }
        std::fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl PluginHost {
    // Verify prepared files
    pub fn verify_prepared_files(
        prepared: &PreparedPlugin,
        expected: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        let actual = packages::inventory(&prepared.source)?;
        if &actual != expected {
            return Err("Package file inventory/hash mismatch".into());
        };
        Ok(())
    }

    // Read the identity of the prepared transaction
    pub fn prepared_info(prepared: &PreparedPlugin) -> Result<PluginInfo, String> {
        Ok(read_plugin(&prepared.source)?.info)
    }
    // Write prepared metadata
    pub fn write_prepared_metadata(
        prepared: &mut PreparedPlugin,
        name: &str,
        value: &Value,
    ) -> Result<(), String> {
        if name != ".stream-dock-manager-provenance.json" {
            return Err("Invalid metadata file".into());
        };
        prepared.provenance = Some(value.clone());
        Ok(())
    }
    // Read the recorded package origin
    pub fn provenance(&self, id: &str) -> Option<Value> {
        self.hub.lock().unwrap().provenance.get(id).cloned()
    }
    // Replace prepared
    pub fn replace_prepared(&mut self, prepared: PreparedPlugin) -> Result<PluginInfo, String> {
        self.replace_prepared_inner(prepared, true)
    }
    // Replace prepared inner
    pub(super) fn replace_prepared_inner(
        &mut self,
        prepared: PreparedPlugin,
        preserve_enabled: bool,
    ) -> Result<PluginInfo, String> {
        if pending(&self.root)? {
            return Err(
                "Package recovery is pending; restart Manager before another replacement".into(),
            );
        }
        if prepared.directory.path().parent() != Some(self.root.as_path()) {
            return Err("Foreign staging directory".into());
        };
        Self::verify_prepared_files(&prepared, &prepared.inventory)?;
        let info = Self::prepared_info(&prepared)?;
        for existing in self.plugins.values().filter(|p| p.info.id != info.id) {
            if info.actions.iter().any(|incoming| {
                existing
                    .info
                    .actions
                    .iter()
                    .any(|action| action.id == incoming.id)
            }) {
                return Err("Action UUID is already owned by another installed plugin".into());
            }
        }
        let target = self.root.join(&info.id);
        if target.is_symlink() {
            return Err("Linked installation cannot be replaced".into());
        };
        let old = self.plugins.get(&info.id);
        if old.is_some_and(|p| p.path != target) {
            return Err("Installation path differs from managed target".into());
        };
        if old.is_some_and(|p| p.child.is_some()) {
            return Err("Stop the plugin process and await cleanup before replacement".into());
        }
        if old.is_some_and(|p| {
            p.stopping
                .as_ref()
                .is_some_and(|done| !done.load(Ordering::Acquire))
        }) {
            return Err("Plugin process cleanup is still in progress".into());
        }
        {
            let hub = self.hub.lock().unwrap();
            if !hub.installations.contains_key(&info.id) && hub.installations.len() >= 4096 {
                return Err("Installation identity index exceeds 4,096 entries".into());
            }
        }
        let had_old = target.exists();
        let was_enabled = old.is_some_and(|p| {
            if preserve_enabled {
                self.hub.lock().unwrap().enabled.contains(&p.info.id)
            } else {
                p.info.enabled
            }
        });
        let tx = uuid::Uuid::new_v4().to_string();
        let backup_dir = self.root.join(".backups").join(&tx);
        let backup = backup_dir.join(&info.id);
        let journals = self.root.join(".transactions");
        backups::directory(&journals)?;
        backups::directory(&self.root.join(".backups"))?;
        backups::directory(&backup_dir)?;
        let receipt = if had_old {
            Some(self.record_backup(&tx, &info.id)?)
        } else {
            None
        };
        let journal_path = journals.join(format!("{tx}.json"));
        let mut journal = Journal {
            prior_provenance: self.provenance(&info.id),
            prior_installation: self
                .hub
                .lock()
                .unwrap()
                .installations
                .get(&info.id)
                .cloned(),
            prior_inventory: receipt.as_ref().map(|(_, inventory)| inventory.clone()),
            id: info.id.clone(),
            backup: tx,
            committed: false,
            had_old,
            was_enabled,
        };
        save(&journal_path, &journal)?;
        if old.is_some() {
            self.enable(&info.id, false)?;
        }
        let result = (|| {
            if had_old {
                std::fs::rename(&target, &backup).map_err(|e| e.to_string())?
            };
            std::fs::rename(&prepared.source, &target).map_err(|e| e.to_string())?;
            let mut plugin = read_plugin_with_identity(&target, Some(&info.id))?;
            {
                let mut hub = self.hub.lock().unwrap();
                hub.health
                    .entry(info.id.clone())
                    .or_default()
                    .blocked_reason = None;
                hub.health_dirty = true;
                hub.enabled.remove(&info.id);
                if preserve_enabled && was_enabled {
                    hub.enabled.insert(info.id.clone());
                    plugin.info.enabled = true;
                }
                if let Some(provenance) = &prepared.provenance {
                    hub.provenance.insert(info.id.clone(), provenance.clone());
                } else {
                    hub.provenance.remove(&info.id);
                }
                hub.installations
                    .insert(info.id.clone(), uuid::Uuid::new_v4().to_string());
                if let Some((receipt, _)) = &receipt {
                    hub.backup_receipts
                        .insert(journal.backup.clone(), receipt.clone());
                }
                hub.persist()?;
            };
            journal.committed = true;
            save(&journal_path, &journal)?;
            Ok::<_, String>(plugin)
        })();
        match result {
            Ok(plugin) => {
                let info = plugin.info.clone();
                {
                    let mut hub = self.hub.lock().unwrap();
                    hub.revoke_setup_runtimes(&info.id);
                    hub.roots.insert(info.id.clone(), target);
                    hub.manifest_dialects
                        .insert(info.id.clone(), info.dialect.clone());
                    hub.metadata
                        .insert(info.id.clone(), plugin.metadata.clone());
                }
                self.plugins.insert(info.id.clone(), plugin);
                self.reconcile();
                // The durable committed journal can also be cleaned up on the next startup
                let _ = std::fs::remove_file(journal_path);
                Ok(info)
            }
            Err(error) => {
                if let Err(recovery) = recover(&self.root, self.storage()) {
                    let message = format!("Package recovery required: {recovery}");
                    let mut hub = self.hub.lock().unwrap();
                    hub.enabled.remove(&info.id);
                    hub.health
                        .entry(info.id.clone())
                        .or_default()
                        .blocked_reason = Some(message.clone());
                    hub.health_dirty = true;
                    let persisted = hub.persist();
                    drop(hub);
                    if let Some(plugin) = self.plugins.get_mut(&info.id) {
                        plugin.info.enabled = false;
                        plugin.info.runtime.state = RuntimeState::Blocked;
                        plugin.info.runtime.reason = RuntimeReason::Compatibility;
                        plugin.info.status = message.clone();
                    }
                    return Err(format!(
                        "Replacement failed: {error}. {message}. {}",
                        persisted.err().unwrap_or_default()
                    ));
                }
                {
                    let mut hub = self.hub.lock().unwrap();
                    if preserve_enabled {
                        if was_enabled {
                            hub.enabled.insert(info.id.clone());
                        } else {
                            hub.enabled.remove(&info.id);
                        }
                    }
                    hub.backup_receipts.remove(&journal.backup);
                    if let Some(token) = &journal.prior_installation {
                        hub.installations.insert(info.id.clone(), token.clone());
                    } else {
                        hub.installations.remove(&info.id);
                    }
                    if let Some(provenance) = &journal.prior_provenance {
                        hub.provenance.insert(info.id.clone(), provenance.clone());
                    } else {
                        hub.provenance.remove(&info.id);
                    }
                    hub.persist()?;
                }
                if had_old {
                    let plugin = read_plugin_with_identity(&target, Some(&info.id))?;
                    self.hub
                        .lock()
                        .unwrap()
                        .metadata
                        .insert(info.id.clone(), plugin.metadata.clone());
                    self.plugins.insert(info.id.clone(), plugin);
                    if was_enabled {
                        self.enable(&info.id, true)?;
                    }
                };
                Err(error)
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Open the database used by this operation
    fn database(root: &Path) -> stream_dock_manager_storage::Storage {
        stream_dock_manager_storage::Storage::open(root.into()).unwrap()
    }
    #[test]
    // Verify unsupported journals preserve recovery evidence
    fn unsupported_journals_preserve_recovery_evidence() {
        for field in ["prior_provenance", "prior_installation", "prior_inventory"] {
            let (root, journal, _) = recovery_fixture();
            let path = root.path().join(".transactions/replacement.json");
            let mut value = serde_json::to_value(&journal).unwrap();
            value.as_object_mut().unwrap().remove(field);
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(recover(root.path(), &database(root.path())).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert!(root
                .path()
                .join(".backups")
                .join(&journal.backup)
                .join(&journal.id)
                .exists());
        }
    }
    #[test]
    // Verify interrupted replacement restores files and enable state
    fn interrupted_replacement_restores_files_and_enable_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".transactions")).unwrap();
        let tx = uuid::Uuid::new_v4().to_string();
        let id = "org.test.plugin";
        let backup = root.join(".backups").join(&tx).join(id);
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("old"), "original").unwrap();
        std::fs::create_dir(root.join(id)).unwrap();
        std::fs::write(root.join(id).join("new"), "new").unwrap();
        save(
            &root.join(".transactions/tx.json"),
            &Journal {
                prior_provenance: None,
                prior_installation: Some(uuid::Uuid::new_v4().to_string()),
                prior_inventory: Some(
                    hash_inventory(&packages::inventory(&backup).unwrap()).unwrap(),
                ),
                id: id.into(),
                backup: tx,
                committed: false,
                had_old: true,
                was_enabled: true,
            },
        )
        .unwrap();
        recover(root, &database(root)).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(id).join("old")).unwrap(),
            "original"
        );
        assert!(Hub::load(&database(root)).unwrap().enabled.contains(id));
        assert!(!root.join(id).join("new").exists());
    }
    // Construct an interrupted-transaction recovery fixture
    fn recovery_fixture() -> (tempfile::TempDir, Journal, String) {
        let root = tempfile::tempdir().unwrap();
        let id = "org.test.recovery";
        let tx = uuid::Uuid::new_v4().to_string();
        let backup = root.path().join(".backups").join(&tx).join(id);
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("old"), "original package").unwrap();
        let target = root.path().join(id);
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("new"), "replacement package").unwrap();
        std::fs::create_dir(root.path().join(".transactions")).unwrap();
        let previous = uuid::Uuid::new_v4().to_string();
        let current = uuid::Uuid::new_v4().to_string();
        let mut hub = Hub::load(&database(root.path())).unwrap();
        hub.installations.insert(id.into(), current.clone());
        hub.provenance.insert(id.into(), json!({"source":"new"}));
        hub.backup_receipts.insert(tx.clone(), "a".repeat(64));
        hub.globals
            .insert(id.into(), json!({"keep":"latest settings"}));
        hub.persist().unwrap();
        let journal = Journal {
            prior_provenance: Some(json!({"source":"original"})),
            prior_installation: Some(previous),
            prior_inventory: Some(hash_inventory(&packages::inventory(&backup).unwrap()).unwrap()),
            id: id.into(),
            backup: tx,
            committed: false,
            had_old: true,
            was_enabled: true,
        };
        save(
            &root.path().join(".transactions/replacement.json"),
            &journal,
        )
        .unwrap();
        (root, journal, current)
    }
    #[test]
    // Verify interrupted rollback restores identity but keeps current settings
    fn interrupted_rollback_restores_identity_but_keeps_current_settings() {
        let (root, journal, _) = recovery_fixture();
        recover(root.path(), &database(root.path())).unwrap();
        let hub = Hub::load(&database(root.path())).unwrap();
        assert_eq!(
            hub.installations[&journal.id],
            journal.prior_installation.unwrap()
        );
        assert_eq!(hub.provenance[&journal.id]["source"], "original");
        assert_eq!(hub.globals[&journal.id]["keep"], "latest settings");
        assert!(!hub.backup_receipts.contains_key(&journal.backup));
        assert!(hub.enabled.contains(&journal.id));
        assert_eq!(
            std::fs::read(root.path().join(&journal.id).join("old")).unwrap(),
            b"original package"
        );
    }
    #[test]
    // Verify altered prior inventory blocks recovery and preserves all packages and journal
    fn altered_prior_inventory_blocks_recovery_and_preserves_all_packages_and_journal() {
        let (root, journal, current) = recovery_fixture();
        let backup = root
            .path()
            .join(".backups")
            .join(&journal.backup)
            .join(&journal.id);
        std::fs::write(backup.join("old"), "altered backup").unwrap();
        assert!(recover(root.path(), &database(root.path()))
            .unwrap_err()
            .contains("inventory changed"));
        assert_eq!(
            std::fs::read(root.path().join(&journal.id).join("new")).unwrap(),
            b"replacement package"
        );
        assert_eq!(
            std::fs::read(backup.join("old")).unwrap(),
            b"altered backup"
        );
        assert!(pending(root.path()).unwrap());
        assert_eq!(
            Hub::load(&database(root.path())).unwrap().installations[&journal.id],
            current
        );
    }
    #[test]
    // Verify committed recovery preserves the new identity and retained receipt
    fn committed_recovery_preserves_the_new_identity_and_retained_receipt() {
        let (root, mut journal, current) = recovery_fixture();
        journal.committed = true;
        save(
            &root.path().join(".transactions/replacement.json"),
            &journal,
        )
        .unwrap();
        recover(root.path(), &database(root.path())).unwrap();
        let hub = Hub::load(&database(root.path())).unwrap();
        assert_eq!(hub.installations[&journal.id], current);
        assert!(hub.backup_receipts.contains_key(&journal.backup));
        assert!(root.path().join(&journal.id).join("new").is_file());
        assert!(!pending(root.path()).unwrap());
    }
    #[cfg(unix)]
    #[test]
    // Verify recovery rejects linked journal directories without touching external files
    fn recovery_rejects_linked_journal_directories_without_touching_external_files() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("receipt.json"), b"unrelated").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join(".transactions")).unwrap();
        assert!(recover(root.path(), &database(root.path())).is_err());
        assert_eq!(
            std::fs::read(outside.path().join("receipt.json")).unwrap(),
            b"unrelated"
        );
    }
}

#[cfg(test)]
mod provenance_tests {
    use super::*;
    #[test]
    // Verify dot identities cannot address parent directories
    fn dot_identities_cannot_address_parent_directories() {
        assert!(!safe_id("."));
        assert!(!safe_id(".."));
        assert!(safe_id("com.vendor.plugin"));
    }
    #[test]
    #[ignore = "Requires loopback plugin host; launches no native code"]
    // Verify package metadata cannot forge first party provenance
    fn package_metadata_cannot_forge_first_party_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("org.test.local.sdPlugin");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(
            package.join("manifest.json"),
            serde_json::to_vec(&manifest_schema::fixture("org.test.local", "plugin.html")).unwrap(),
        )
        .unwrap();
        std::fs::write(
            package.join("plugin.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        std::fs::write(
            package.join(".stream-dock-manager-provenance.json"),
            r#"{"repository":"example/plugins","channel":"stable","version":"99.0.0"}"#,
        )
        .unwrap();
        let mut host = PluginHost::new(dir.path().join("plugins")).unwrap();
        let stage = PluginHost::prepare_install(&host.root, &package).unwrap();
        host.replace_prepared(stage).unwrap();
        assert!(host.provenance("org.test.local").is_none());
        host.shutdown();
    }
}
