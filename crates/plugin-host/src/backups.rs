//! Host-owned backup receipts and reviewed rollback. Package files cannot assert provenance
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
const MAX_RECORD: usize = 4 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginBackup {
    pub id: String,
    pub plugin_id: String,
    pub version: Option<String>,
    pub recorded: bool,
    pub trusted_origin: bool,
    pub provenance: Option<Value>,
    pub inventory_sha256: Option<String>,
    pub file_count: usize,
    #[ts(type = "number | null")]
    pub created: Option<u64>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginBackups {
    pub entries: Vec<PluginBackup>,
    pub page: usize,
    pub total: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct InstalledPackageIdentity {
    pub id: String,
    pub version: String,
    pub identity: String,
    pub inventory_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BackupReview {
    pub backup: PluginBackup,
    pub installed: InstalledPackageIdentity,
    pub compatibility: preflight::PluginPreflight,
    pub warnings: Vec<String>,
}
pub struct PreparedRollback {
    pub review: BackupReview,
    pub prepared: PreparedPlugin,
    pub backup_identity: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupRecord {
    schema_version: u32,
    id: String,
    plugin_id: String,
    version: String,
    #[serde(default)]
    provenance: Option<Value>,
    inventory: BTreeMap<String, String>,
    created: u64,
}
/// Prevent demand reconciliation from restarting code while process cleanup is awaited
pub struct ReplacementGuard {
    claims: Arc<Mutex<HashSet<String>>>,
    id: String,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl Drop for ReplacementGuard {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.claims.lock().unwrap().remove(&self.id);
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}
// Validate or create the managed directory
pub(super) fn directory(path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.to_string()),
    }
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Backup metadata must use regular managed directories".into());
    }
    Ok(())
}
// Validate the identifier representation
fn uuid(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id)
        .map_err(|_| "Invalid backup identity")?
        .to_string()
        != id
    {
        return Err("Noncanonical backup identity".into());
    }
    Ok(())
}
// Read record
fn read_record(path: &Path) -> Result<Vec<u8>, String> {
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_RECORD as u64 {
        return Err("Backup record is not a bounded regular file".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() || opened.len() > MAX_RECORD as u64 {
        return Err("Backup record is not a bounded regular file".into());
    }
    stream_dock_manager_storage::bounded_io::read_from(
        file,
        MAX_RECORD,
        "Backup record exceeds 4 MiB",
    )
}

// Hash the supplied bytes for identity checks
fn hash(value: &impl Serialize) -> Result<String, String> {
    stream_dock_manager_storage::json_digest::sha256(value)
}

impl PluginHost {
    // Reserve replacement
    pub fn reserve_replacement(&self, id: &str) -> Result<ReplacementGuard, String> {
        self.plugins.get(id).ok_or("Plugin not installed")?;
        if !self.replacement_claims.lock().unwrap().insert(id.into()) {
            return Err("Another package replacement owns this plugin".into());
        }
        Ok(ReplacementGuard {
            claims: self.replacement_claims.clone(),
            id: id.into(),
            wake: self.hub.lock().unwrap().wake.clone(),
        })
    }
    /// A cheap persistent installation token for local policy binding; it does not verify files
    pub fn installation_token(&self, id: &str) -> Option<String> {
        let plugin = self.plugins.get(id)?;
        let hub = self.hub.lock().unwrap();
        hash(&(
            id,
            &plugin.info.version,
            hub.installations.get(id),
            hub.provenance.get(id),
        ))
        .ok()
    }
    // Inspect the identity and inventory of an installed package
    pub fn installed_package_identity(&self, id: &str) -> Result<InstalledPackageIdentity, String> {
        let plugin = self.plugins.get(id).ok_or("Plugin not installed")?;
        if plugin.path != self.root.join(id) {
            return Err("Plugin is outside its managed installation".into());
        }
        let inventory = packages::inventory(&plugin.path)?;
        let inventory_sha256 = hash_inventory(&inventory)?;
        let identity = hash(&(self.installation_token(id), &inventory_sha256))?;
        Ok(InstalledPackageIdentity {
            id: id.into(),
            version: plugin.info.version.clone(),
            identity,
            inventory_sha256,
        })
    }
    /// Written before moving the old package, then anchored by Hub.persist inside the transaction
    pub(super) fn record_backup(&self, tx: &str, id: &str) -> Result<(String, String), String> {
        uuid(tx)?;
        let plugin = self
            .plugins
            .get(id)
            .ok_or("Installed package is unavailable")?;
        if plugin.path != self.root.join(id) {
            return Err("Only managed installations can be retained".into());
        }
        if self.hub.lock().unwrap().backup_receipts.len() >= 4096 {
            return Err("Backup receipt index exceeds 4,096 records".into());
        }
        let record = BackupRecord {
            schema_version: 1,
            id: tx.into(),
            plugin_id: id.into(),
            version: plugin.info.version.clone(),
            provenance: self.provenance(id),
            inventory: packages::inventory(&plugin.path)?,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_secs(),
        };
        let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_RECORD {
            return Err("Backup inventory exceeds 4 MiB".into());
        }
        let index = self.root.join(".backup-index");
        directory(&index)?;
        let mut file = tempfile::NamedTempFile::new_in(&index).map_err(|e| e.to_string())?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(index.join(format!("{tx}.json")))
            .map_err(|e| e.to_string())?;
        Ok((
            hex::encode(Sha256::digest(bytes)),
            hash_inventory(&record.inventory)?,
        ))
    }
    // Resolve the managed path for this backup
    pub(super) fn backup_path(&self, id: &str, backup: &str) -> Result<PathBuf, String> {
        if !safe_id(id) {
            return Err("Invalid plugin identity".into());
        }
        uuid(backup)?;
        let base = self.root.join(".backups");
        for dir in [&base, &base.join(backup)] {
            let meta = dir.symlink_metadata().map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err("Linked or invalid backup directory".into());
            }
        }
        let path = base.join(backup).join(id);
        let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Linked or invalid backup package".into());
        }
        Ok(path)
    }
    // Read the host-owned backup receipt
    fn recorded_backup(&self, id: &str, backup: &str) -> Result<Option<BackupRecord>, String> {
        let anchor = self
            .hub
            .lock()
            .unwrap()
            .backup_receipts
            .get(backup)
            .cloned();
        let Some(anchor) = anchor else {
            return Ok(None);
        };
        let index = self.root.join(".backup-index");
        let metadata = match index.symlink_metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Invalid backup index directory".into());
        }
        let path = index.join(format!("{backup}.json"));
        match path.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
            Ok(_) => {}
        }
        let bytes = read_record(&path)?;
        if hex::encode(Sha256::digest(&bytes)) != anchor {
            return Err("Backup metadata differs from its host-owned receipt".into());
        }
        let record: BackupRecord =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid backup inventory record")?;
        if record.schema_version != 1
            || record.id != backup
            || record.plugin_id != id
            || record.inventory.is_empty()
            || record.inventory.len() > 4096
        {
            return Err("Backup record identity or bounds differ".into());
        }
        Ok(Some(record))
    }
    // Build the user-visible description of a backup
    fn describe_backup(&self, id: &str, backup: &str) -> Result<PluginBackup, String> {
        self.backup_path(id, backup)?;
        let record = self.recorded_backup(id, backup)?;
        let (version, provenance, inventory_sha256, file_count, created) = match &record {
            Some(r) => (
                Some(r.version.clone()),
                r.provenance.clone(),
                Some(hash_inventory(&r.inventory)?),
                r.inventory.len(),
                Some(r.created),
            ),
            None => (None, None, None, 0, None),
        };
        let trusted_origin = record.as_ref().is_some_and(|r| {
            r.provenance.as_ref().is_some_and(|p| {
                p["repository"].as_str().is_some()
                    && p["version"] == r.version
                    && p["artifact"]["files"]
                        == serde_json::to_value(&r.inventory).unwrap_or(Value::Null)
            })
        });
        Ok(PluginBackup {
            id: backup.into(),
            plugin_id: id.into(),
            version,
            recorded: record.is_some(),
            trusted_origin,
            provenance,
            inventory_sha256,
            file_count,
            created,
            error: None,
        })
    }
    // Enumerate the saved backup identifiers
    pub(super) fn backup_ids(
        &self,
        id: &str,
    ) -> Result<Vec<(Option<std::time::SystemTime>, String)>, String> {
        if !safe_id(id) {
            return Err("Invalid plugin identity".into());
        }
        let base = self.root.join(".backups");
        let meta = match base.symlink_metadata() {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Invalid backup directory".into());
        }
        let mut ids = vec![];
        let mut count = 0;
        for entry in std::fs::read_dir(base).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            count += 1;
            if count > 4096 {
                return Err("Backup scan exceeds 4,096 transactions".into());
            }
            let name = entry
                .file_name()
                .to_str()
                .ok_or("Invalid backup directory name")?
                .to_string();
            let meta = entry.path().symlink_metadata().map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err("Invalid backup transaction directory".into());
            }
            if entry
                .path()
                .join(id)
                .try_exists()
                .map_err(|e| e.to_string())?
            {
                uuid(&name)?;
                ids.push((meta.modified().ok(), name));
            }
        }
        Ok(ids)
    }
    // List the backups belonging to a plugin
    pub fn plugin_backups(&self, id: &str, page: usize) -> Result<PluginBackups, String> {
        let mut ids = self.backup_ids(id)?;
        ids.sort_by(|a, b| b.cmp(a));
        let total = ids.len();
        let page = page.clamp(1, total.div_ceil(12).max(1));
        let mut entries = vec![];
        for (_, backup) in ids.into_iter().skip((page - 1) * 12).take(12) {
            entries.push(
                self.describe_backup(id, &backup)
                    .unwrap_or_else(|error| PluginBackup {
                        id: backup,
                        plugin_id: id.into(),
                        version: None,
                        recorded: false,
                        trusted_origin: false,
                        provenance: None,
                        inventory_sha256: None,
                        file_count: 0,
                        created: None,
                        error: Some(error),
                    }),
            );
        }
        Ok(PluginBackups {
            entries,
            page,
            total,
        })
    }
    // Delete plugin backups
    pub fn delete_plugin_backups(
        &self,
        id: &str,
        backup: Option<&str>,
        all: bool,
    ) -> Result<usize, String> {
        self.plugins.get(id).ok_or("Plugin not installed")?;
        if self.replacement_claims.lock().unwrap().contains(id)
            || transactions::pending(&self.root)?
        {
            return Err("Wait for package replacement or recovery before deleting backups".into());
        }
        let ids = match (backup, all) {
            (Some(backup), false) => vec![backup.to_owned()],
            (None, true) => self.backup_ids(id)?.into_iter().map(|(_, id)| id).collect(),
            _ => return Err("Select one backup or explicitly request all backups".into()),
        };
        let index = self.root.join(".backup-index");
        match index.symlink_metadata() {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => return Err("Invalid backup index directory".into()),
            Err(e) => return Err(e.to_string()),
        }
        // Validate the complete selection before deleting any package files
        let targets = ids
            .iter()
            .map(|backup| {
                let path = self.backup_path(id, backup)?;
                let parent = path
                    .parent()
                    .ok_or("Missing backup transaction directory")?;
                let sole_package = std::fs::read_dir(parent)
                    .map_err(|e| e.to_string())?
                    .nth(1)
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .is_none();
                Ok((backup, path, sole_package))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if targets.is_empty() {
            return Ok(0);
        }
        let stage = tempfile::Builder::new()
            .prefix(".deleted-backups-")
            .tempdir_in(&self.root)
            .map_err(|e| e.to_string())?;
        let mut staged = 0;
        let result = (|| {
            for (backup, path, _) in &targets {
                std::fs::rename(path, stage.path().join(backup))
                    .map_err(|e| format!("Could not delete backup {backup}: {e}"))?;
                staged += 1;
            }
            let mut hub = self.hub.lock().unwrap();
            let receipts = targets
                .iter()
                .filter(|(_, _, sole_package)| *sole_package)
                .filter_map(|(backup, _, _)| {
                    hub.backup_receipts
                        .remove(*backup)
                        .map(|receipt| ((*backup).clone(), receipt))
                })
                .collect::<Vec<_>>();
            if !receipts.is_empty() {
                if let Err(error) = hub.persist() {
                    hub.backup_receipts.extend(receipts);
                    return Err(error);
                }
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            let recovery_needed =
                targets[..staged]
                    .iter()
                    .fold(false, |failed, (backup, path, _)| {
                        std::fs::rename(stage.path().join(backup), path).is_err() || failed
                    });
            if recovery_needed {
                return Err(format!(
                    "{error}; some backups could not be returned to their original folders. Files preserved in {}",
                    stage.keep().display()
                ));
            }
            return Err(error);
        }
        for (backup, _, sole_package) in &targets {
            if *sole_package {
                let _ = std::fs::remove_dir(self.root.join(".backups").join(backup));
                let _ = std::fs::remove_file(index.join(format!("{backup}.json")));
            }
        }
        stage
            .close()
            .map_err(|e| format!("Backups removed from the list, but file cleanup failed: {e}"))?;
        Ok(staged)
    }
    // Verify backup
    fn verify_backup(&self, id: &str, backup: &str) -> Result<(PluginBackup, String), String> {
        let mut description = self.describe_backup(id, backup)?;
        let path = self.backup_path(id, backup)?;
        let inventory = packages::inventory(&path)?;
        let info = read_plugin_with_identity(&path, Some(id))?.info;
        if let Some(record) = self.recorded_backup(id, backup)? {
            if record.inventory != inventory || record.version != info.version {
                return Err("Backup files or version differ from the recorded inventory".into());
            }
        }
        let fingerprint = hash(&(&inventory, &description.provenance, description.recorded))?;
        description.version = Some(info.version);
        description.file_count = inventory.len();
        description.inventory_sha256 = Some(hash_inventory(&inventory)?);
        Ok((description, fingerprint))
    }
    // Prepare rollback
    pub fn prepare_rollback(&self, id: &str, backup: &str) -> Result<PreparedRollback, String> {
        let installed = self.installed_package_identity(id)?;
        let (description, backup_identity) = self.verify_backup(id, backup)?;
        let path = self.backup_path(id, backup)?;
        let mut prepared = Self::prepare_install(&self.root, &path)?;
        let info = Self::prepared_info(&prepared)?;
        if info.id != id || Some(&info.version) != description.version.as_ref() {
            return Err("Backup changed during preparation".into());
        }
        let (_, after) = self.verify_backup(id, backup)?;
        if after != backup_identity {
            return Err("Backup changed during preparation".into());
        }
        prepared.provenance = if description.trusted_origin {
            description.provenance.clone()
        } else {
            Some(
                json!({"trust":"unverified_rollback","version":info.version,"source":{"format":"backup","filename":backup,"sha256":description.inventory_sha256}}),
            )
        };
        let review=BackupReview{backup:description,installed,compatibility:Self::prepared_preflight(&prepared),warnings:vec!["Current Manager-owned global, action and default settings are preserved. Older code can require different settings or dependencies; plugin-owned files inside the backup are restored.".into(),"The restored plugin stays disabled. No package executable runs during review or commit.".into(),"Host-owned inventory records detect changed backup bytes; they are not publisher signatures. Missing trusted metadata means unverified provenance.".into()]};
        Ok(PreparedRollback {
            review,
            prepared,
            backup_identity,
        })
    }
    // Verify rollback
    pub fn verify_rollback(
        &self,
        review: &BackupReview,
        expected_backup: &str,
    ) -> Result<(), String> {
        if self
            .installed_package_identity(&review.installed.id)?
            .identity
            != review.installed.identity
        {
            return Err("Installed package changed since rollback review".into());
        }
        let (_, identity) = self.verify_backup(&review.backup.plugin_id, &review.backup.id)?;
        if identity != expected_backup {
            return Err("Backup changed since rollback review".into());
        }
        Ok(())
    }
    // Restore prepared
    pub fn restore_prepared(&mut self, prepared: PreparedPlugin) -> Result<PluginInfo, String> {
        self.replace_prepared_inner(prepared, false)
    }
}

#[cfg(test)]
mod tests;
