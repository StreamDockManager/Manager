//! Offline record repair and explicitly reviewed full configuration restore
//! Corrupt primaries can be owned without opening them for writes
//! Opening and inspection touch only a disposable snapshot. The root writer lock
//! is held for the complete session; no ordinary runtime shares this database.
use crate::{
    installation_key::{self, recovery as settings_recovery, KeystoreInitialization},
    maintenance, own_root, schema, PluginKeystoreBackup, PluginKeystoreReview, RootOwner,
};
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

struct State {
    snapshot: Option<schema::ScratchDatabase>,
    unavailable: Option<String>,
    fingerprint: schema::Fingerprint,
    revision: String,
    settings_status: &'static str,
    settings_review: Option<SettingsReview>,
}

struct SettingsReview {
    token: String,
    operation: SettingsOperation,
}

enum SettingsOperation {
    Restore(String, [u8; 32]),
    Initialize(KeystoreInitialization),
}

impl State {
    // Return the opened inspection snapshot or explain why it is unavailable
    fn snapshot(&self) -> Result<&Connection, String> {
        self.snapshot.as_deref().ok_or_else(|| {
            self.unavailable.clone().unwrap_or_else(|| {
                "Primary unavailable; select a validated backup for full restore".into()
            })
        })
    }
}
// Compute the identity used to reject stale repair edits
fn editor_fingerprint(root: &Path) -> Result<schema::Fingerprint, String> {
    let mut fingerprint = schema::fingerprint(&crate::database_path(root, schema::DATABASE_NAME))?;
    fingerprint.extend(schema::fingerprint(&root.join(schema::RECOVERY_PENDING))?);
    fingerprint.extend(schema::fingerprint(&crate::database_path(
        root,
        installation_key::NAME,
    ))?);
    Ok(fingerprint)
}
// Load state
fn load_state(root: &Path) -> Result<State, String> {
    let fingerprint = editor_fingerprint(root)?;
    let mut settings_status = "unknown";
    let result = if root
        .join(schema::RECOVERY_PENDING)
        .symlink_metadata()
        .is_ok()
    {
        match installation_key::pending_operation(root) {
            Ok(Some(KeystoreInitialization::Create)) => {
                settings_status = "missing";
                Err("An interrupted plugin settings keystore creation must be resumed in Recovery. Original files are preserved.".into())
            }
            Ok(Some(KeystoreInitialization::Reset)) => {
                settings_status = "unavailable";
                Err("An interrupted plugin settings reset must be resumed in Recovery. Original files are preserved.".into())
            }
            Ok(None) => {
                Err("An interrupted database restore must be resolved before record repair".into())
            }
            Err(_) => Err(
                "An interrupted keystore recovery marker is unreadable. Original files are preserved."
                    .into(),
            ),
        }
    } else {
        schema::snapshot_copy_unattached(&crate::database_path(root, schema::DATABASE_NAME))
            .and_then(|(copy, _)| {
                match settings_recovery::is_genuinely_missing(&copy, root) {
                    Ok(true) => settings_status = "missing",
                    Ok(false) => {}
                    Err(error) => {
                        settings_status = "unavailable";
                        return Err(error);
                    }
                }
                match settings_recovery::attach_inspection_copy(&copy, root) {
                    Ok(true) => {
                        settings_status = "ready";
                        Ok(copy)
                    }
                    Ok(false) => Ok(copy),
                    Err(error) => {
                        if settings_status != "missing" {
                            settings_status = "unavailable";
                        }
                        Err(error)
                    }
                }
            })
    };
    if editor_fingerprint(root)? != fingerprint {
        return Err(
            "Configuration changed while opening recovery; retry after closing other writers"
                .into(),
        );
    }
    let (snapshot, unavailable) = match result {
        Ok(snapshot) => (Some(snapshot), None),
        Err(error) => (None, Some(error)),
    };
    Ok(State {
        snapshot,
        unavailable,
        fingerprint,
        revision: uuid::Uuid::new_v4().to_string(),
        settings_status,
        settings_review: None,
    })
}
pub struct RecoveryEditor {
    root: PathBuf,
    state: Mutex<State>,
    _owner: RootOwner,
}
impl RecoveryEditor {
    // Open for the current operation
    pub fn open(root: PathBuf) -> Result<Self, String> {
        Self::open_mode(root, false)
    }
    /// Own the offline root even when the primary cannot be decoded. No primary
    /// is created or reset; only explicit validated restore can replace it.
    pub fn open_for_recovery(root: PathBuf) -> Result<Self, String> {
        Self::open_mode(root, true)
    }
    // Open mode
    fn open_mode(root: PathBuf, allow_unavailable: bool) -> Result<Self, String> {
        let (root, owner) = own_root(&root)?;
        crate::database_layout::prepare_owned(&root)?;
        let state = load_state(&root)?;
        if !allow_unavailable {
            state.snapshot()?;
        }
        Ok(Self {
            root,
            state: Mutex::new(state),
            _owner: owner,
        })
    }
    // Return the current revision identifier
    pub fn revision(&self) -> Result<String, String> {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, None)?;
        Ok(state.revision.clone())
    }
    // Explain why a record cannot currently be repaired
    pub fn unavailable_reason(&self) -> Result<Option<String>, String> {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, None)?;
        Ok(state.unavailable.clone())
    }
    /// Status of the installation settings keystore, separate from personal Vault.
    pub fn plugin_settings_status(&self) -> Result<(String, Option<String>), String> {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, None)?;
        Ok((state.settings_status.into(), state.unavailable.clone()))
    }

    /// Revalidate after the OS credential store has been unlocked without
    /// requiring a change to the database or encrypted file.
    pub fn retry_plugin_settings(&self, revision: &str) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        *state = load_state(&self.root)?;
        Ok(())
    }

    pub fn plugin_settings_backups(&self) -> Result<Vec<PluginKeystoreBackup>, String> {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, None)?;
        settings_recovery::backups(&self.root)
    }

    pub fn preview_plugin_settings_restore(
        &self,
        revision: &str,
        backup_id: &str,
    ) -> Result<PluginKeystoreReview, String> {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        state.settings_review = None;
        if load_state(&self.root)?.settings_status == "ready" {
            return Err("The current plugin settings keystore is already available".into());
        }
        let candidate = settings_recovery::prepare(&self.root, backup_id)?;
        self.check(&state, Some(revision))?;
        let token = uuid::Uuid::new_v4().to_string();
        state.settings_review = Some(SettingsReview {
            token: token.clone(),
            operation: SettingsOperation::Restore(backup_id.to_owned(), candidate.sha256),
        });
        Ok(PluginKeystoreReview {
            token,
            backup_id: Some(backup_id.to_owned()),
        })
    }

    pub fn restore_plugin_settings(
        &self,
        revision: &str,
        backup_id: &str,
        review_token: &str,
    ) -> Result<PathBuf, String> {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        let review = state
            .settings_review
            .take()
            .filter(|review| review.token == review_token)
            .and_then(|review| match review.operation {
                SettingsOperation::Restore(id, digest) => Some((id, digest)),
                SettingsOperation::Initialize(_) => None,
            })
            .ok_or("Review an encrypted plugin settings backup before restoring")?;
        if review.0 != backup_id {
            return Err("Plugin settings backup selection changed; review again".into());
        }
        if load_state(&self.root)?.settings_status == "ready" {
            return Err("The current plugin settings keystore is already available".into());
        }
        let candidate = settings_recovery::prepare(&self.root, backup_id)?;
        if candidate.sha256 != review.1 {
            return Err("Encrypted plugin settings backup changed; review it again".into());
        }
        let result = settings_recovery::publish_restore(&self.root, candidate, || {
            self.check(&state, Some(revision))
        });
        self.finalize_settings_recovery(&mut state, result)
    }

    pub fn preview_plugin_settings_reset(
        &self,
        revision: &str,
    ) -> Result<PluginKeystoreReview, String> {
        self.preview_plugin_settings_initialization(revision, KeystoreInitialization::Reset)
    }

    pub fn preview_plugin_settings_create(
        &self,
        revision: &str,
    ) -> Result<PluginKeystoreReview, String> {
        self.preview_plugin_settings_initialization(revision, KeystoreInitialization::Create)
    }

    fn preview_plugin_settings_initialization(
        &self,
        revision: &str,
        mode: KeystoreInitialization,
    ) -> Result<PluginKeystoreReview, String> {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        state.settings_review = None;
        self.check_plugin_settings_initialization_status(mode)?;
        installation_key::check_reset_available(&self.root, mode)?;
        self.check(&state, Some(revision))?;
        let token = uuid::Uuid::new_v4().to_string();
        state.settings_review = Some(SettingsReview {
            token: token.clone(),
            operation: SettingsOperation::Initialize(mode),
        });
        Ok(PluginKeystoreReview {
            token,
            backup_id: None,
        })
    }

    pub fn reset_plugin_settings(
        &self,
        revision: &str,
        review_token: &str,
    ) -> Result<PathBuf, String> {
        self.initialize_plugin_settings(revision, review_token, KeystoreInitialization::Reset)
    }

    pub fn create_plugin_settings(
        &self,
        revision: &str,
        review_token: &str,
    ) -> Result<PathBuf, String> {
        self.initialize_plugin_settings(revision, review_token, KeystoreInitialization::Create)
    }

    fn check_plugin_settings_initialization_status(
        &self,
        mode: KeystoreInitialization,
    ) -> Result<(), String> {
        // Unlocking the OS store can make a live keystore readable without
        // changing its on-disk fingerprint, so check its current status.
        match (mode, load_state(&self.root)?.settings_status) {
            (KeystoreInitialization::Create, "missing") | (KeystoreInitialization::Reset, "unavailable") => Ok(()),
            (_, "ready") => Err("The current plugin settings keystore is already available".into()),
            (KeystoreInitialization::Create, _) => Err("Create is available only when the plugin settings keystore is missing".into()),
            (KeystoreInitialization::Reset, "missing") => Err("The plugin settings keystore is missing. Choose Create or restore a matching backup.".into()),
            (KeystoreInitialization::Reset, _) => Err("The plugin settings keystore cannot be reset in the current recovery state".into()),
        }
    }

    fn initialize_plugin_settings(
        &self,
        revision: &str,
        review_token: &str,
        mode: KeystoreInitialization,
    ) -> Result<PathBuf, String> {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        if !state.settings_review.take().is_some_and(|review| {
            review.token == review_token
                && matches!(review.operation, SettingsOperation::Initialize(reviewed) if reviewed == mode)
        }) {
            return Err("Review this exact plugin settings operation before applying it".into());
        }
        self.check_plugin_settings_initialization_status(mode)?;
        let result = installation_key::reset_for_recovery(&self.root, mode, || {
            self.check(&state, Some(revision))
        });
        self.finalize_settings_recovery(&mut state, result)
    }

    fn finalize_settings_recovery(
        &self,
        state: &mut State,
        result: Result<PathBuf, String>,
    ) -> Result<PathBuf, String> {
        match load_state(&self.root) {
            Ok(next) => *state = next,
            Err(error) => {
                state.snapshot = None;
                state.unavailable = Some(error.clone());
                state.settings_status = "unknown";
                state.settings_review = None;
                state.revision = uuid::Uuid::new_v4().to_string();
                return result.and_then(|directory| Err(format!(
                    "Plugin settings recovery committed; preserved originals in {}. Reopen Manager: {error}",
                    directory.display()
                )));
            }
        }
        result
    }

    /// Validate the same reconciliation on disposable copies without publishing
    /// a quarantine directory, marker, or primary. No callback may write originals.
    pub fn preview_restore<F, G>(
        &self,
        revision: &str,
        candidate: PathBuf,
        validate: F,
        reconcile: G,
    ) -> Result<crate::RestorePreview, String>
    where
        F: Fn(&Connection) -> Result<(), String>,
        G: FnOnce(&Connection, &[crate::RecoverySource]) -> Result<Vec<String>, String>,
    {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        let warnings =
            crate::recovery::preview_restore(&self.root, &candidate, validate, reconcile)?;
        self.check(&state, Some(revision))?;
        Ok(warnings)
    }
    // Restore for the current operation
    pub fn restore<F, G>(
        &self,
        revision: &str,
        candidate: PathBuf,
        validate: F,
        reconcile: G,
    ) -> Result<crate::RecoveryReport, String>
    where
        F: Fn(&Connection) -> Result<(), String>,
        G: FnOnce(&Connection, &[crate::RecoverySource]) -> Result<Vec<String>, String>,
    {
        self.restore_inner(revision, candidate, None, validate, reconcile)
    }
    // Restore reviewed
    pub fn restore_reviewed<F, G>(
        &self,
        revision: &str,
        candidate: PathBuf,
        source_fingerprint: &str,
        validate: F,
        reconcile: G,
    ) -> Result<crate::RecoveryReport, String>
    where
        F: Fn(&Connection) -> Result<(), String>,
        G: FnOnce(&Connection, &[crate::RecoverySource]) -> Result<Vec<String>, String>,
    {
        self.restore_inner(
            revision,
            candidate,
            Some(source_fingerprint),
            validate,
            reconcile,
        )
    }
    // Restore inner
    fn restore_inner<F, G>(
        &self,
        revision: &str,
        candidate: PathBuf,
        source_fingerprint: Option<&str>,
        validate: F,
        reconcile: G,
    ) -> Result<crate::RecoveryReport, String>
    where
        F: Fn(&Connection) -> Result<(), String>,
        G: FnOnce(&Connection, &[crate::RecoverySource]) -> Result<Vec<String>, String>,
    {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        let result = crate::recovery::recover_locked(
            &self.root,
            candidate,
            source_fingerprint,
            validate,
            reconcile,
        );
        // A failed multi-file publication may leave a crash fence. Never retain
        // a readable pre-restore snapshot or valid old review token after it.
        match load_state(&self.root) {
            Ok(next) => *state = next,
            Err(error) => {
                state.snapshot = None;
                state.unavailable = Some(error.clone());
                state.settings_status = "unknown";
                state.settings_review = None;
                state.revision = uuid::Uuid::new_v4().to_string();
                return Err(if result.is_ok() {
                    format!(
                        "Restore committed but recovery refresh failed: {error}. Reopen Manager."
                    )
                } else {
                    error
                });
            }
        }
        result
    }

    // Check for the current operation
    fn check(&self, state: &State, expected: Option<&str>) -> Result<(), String> {
        if expected.is_some_and(|revision| revision != state.revision)
            || editor_fingerprint(&self.root)? != state.fingerprint
        {
            return Err(
                "Configuration changed. Reload recovery before saving; originals preserved.".into(),
            );
        }
        Ok(())
    }
    /// Explicitly refresh the disposable snapshot; every editor token is invalidated
    pub fn refresh(&self) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        if editor_fingerprint(&self.root)? != state.fingerprint {
            *state = load_state(&self.root)?;
        }
        Ok(())
    }
    // Read for the current operation
    pub fn read<T>(
        &self,
        operation: impl FnOnce(&Connection, &str) -> Result<T, String>,
    ) -> Result<T, String> {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, None)?;
        operation(state.snapshot()?, &state.revision)
    }
    /// Inspect only regular SQLite snapshots owned by this root. Sources from a
    /// different storage generation are not treated as this workspace's history.
    pub fn inspect_backups<T>(
        &self,
        limit: usize,
        operation: impl FnMut(&str, Option<u64>, &Connection) -> Result<T, String>,
    ) -> Result<(Vec<T>, Vec<String>), String> {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, None)?;
        let generation: String = state
            .snapshot()?
            .query_row(
                "SELECT value FROM database_metadata WHERE key='storage_generation'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        let result = inspect_backups(&self.root, limit, Some(&generation), operation)?;
        self.check(&state, None)?;
        Ok(result)
    }

    /// Validate or preview changes against a disposable SQLite copy, never the original
    pub fn preview<T>(
        &self,
        revision: &str,
        operation: impl FnOnce(&mut Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        let mut copy = schema::ScratchDatabase::new()?;
        maintenance::copy_database(state.snapshot()?, &mut copy)?;
        crate::installation_key::attach_preview(&copy, state.snapshot()?)?;
        operation(&mut copy)
    }
    /// Caller validates a candidate first, then repeats validation in this atomic
    /// transaction. A unique SQLite backup is durably published before any edit.
    pub fn commit<T>(
        &self,
        revision: &str,
        operation: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<(T, PathBuf), String> {
        let mut state = self.state.lock().map_err(|_| "Recovery lock failed")?;
        self.check(&state, Some(revision))?;
        let destination = crate::database_layout::ensure_recovery_directory(&self.root)?.join(
            format!("before-manual-edit-{}.sqlite3", uuid::Uuid::new_v4()),
        );
        maintenance::backup_connection(
            state.snapshot()?,
            &self.root,
            &destination,
            &Default::default(),
        )?;
        self.check(&state, Some(revision))?;
        let mut connection =
            schema::connect(&crate::database_path(&self.root, schema::DATABASE_NAME))?;
        crate::installation_key::attach_live(&connection, &self.root)?;
        let result = (|| {
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            schema::validate(&tx)?;
            if content_identity(&tx)? != content_identity(state.snapshot()?)? {
                return Err("Configuration changed before repair could acquire the database lock; reload recovery. Originals preserved.".into());
            }
            let result = operation(&tx)?;
            tx.execute("UPDATE database_metadata SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='commit_sequence' AND CAST(value AS INTEGER)<9223372036854775807", []).map_err(|e|e.to_string())?;
            if tx.changes() != 1 {
                return Err("Storage commit sequence unavailable or exhausted".into());
            }
            tx.commit().map_err(|e| e.to_string())?;
            Ok(result)
        })();
        drop(connection);
        // Even a rolled-back SQLite transaction can change sidecar bytes
        *state = load_state(&self.root).map_err(|error| {
            if result.is_ok() {
                format!("Repair was committed. Backup: {}. Could not refresh recovery: {error}. Reopen Manager before further changes.", destination.display())
            } else {
                error
            }
        })?;
        if let Err(error) = state.snapshot() {
            return Err(match result {
                Ok(_) => format!(
                    "Repair was committed. Backup: {}. Could not refresh recovery: {error}. Reopen Manager before further changes.",
                    destination.display()
                ),
                Err(original) => original,
            });
        }
        result.map(|value| (value, destination))
    }
}

// Compare all application-owned rows under SQLite's write lock, including writers
// which do not honor Manager's root lock or increment the commit sequence.
fn content_identity(connection: &Connection) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    let mut tables: Vec<(&str, &str)> = std::iter::once(("database_metadata", "key"))
        .chain(crate::Domain::ALL.map(|d| (d.table(), "key")))
        .chain([
            ("plugin_health_summary", "owner"),
            ("plugin_health_events", "owner,ordinal"),
            ("plugin_installation_state", "plugin_id"),
            ("plugin_setting_refs", "domain,record_key,pointer"),
        ])
        .collect();
    if crate::installation_key::attached(connection) {
        tables.extend([
            ("settings_vault.plugin_storage_epoch", "singleton"),
            ("settings_vault.plugin_storage_generations", "plugin_id"),
            (
                "settings_vault.plugin_storage_namespaces",
                "plugin_id,namespace",
            ),
            (
                "settings_vault.plugin_storage_records",
                "plugin_id,namespace,key",
            ),
            (
                "settings_vault.plugin_storage_receipts",
                "plugin_id,namespace,operation_id",
            ),
        ]);
    }
    for (table, order) in tables {
        digest.update(table.as_bytes());
        let sql = format!("SELECT * FROM {table} ORDER BY {order}");
        let mut statement = connection.prepare(&sql).map_err(|e| e.to_string())?;
        let count = statement.column_count();
        let mut rows = statement.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            digest.update([0xff]);
            for index in 0..count {
                match row.get_ref(index).map_err(|e| e.to_string())? {
                    rusqlite::types::ValueRef::Null => digest.update([0]),
                    rusqlite::types::ValueRef::Text(bytes) => {
                        digest.update([1]);
                        digest.update((bytes.len() as u64).to_le_bytes());
                        digest.update(bytes);
                    }
                    rusqlite::types::ValueRef::Integer(value) => {
                        digest.update([2]);
                        digest.update(value.to_le_bytes());
                    }
                    rusqlite::types::ValueRef::Blob(bytes) => {
                        digest.update([3]);
                        digest.update((bytes.len() as u64).to_le_bytes());
                        digest.update(bytes);
                    }
                    _ => return Err("Unsupported database value during recovery".into()),
                }
            }
        }
    }
    Ok(digest.finalize().to_vec())
}

/// Inspect bounded, standalone backups without requiring a readable primary or a
/// matching generation. Reads operate on validated disposable copies only. The
/// caller must validate application records and re-inspect a selected source at
/// transfer time; this inventory is not permission to restore an entire database.
pub fn inspect_recovery_backups<T>(
    root: &Path,
    limit: usize,
    operation: impl FnMut(&str, Option<u64>, &Connection) -> Result<T, String>,
) -> Result<(Vec<T>, Vec<String>), String> {
    inspect_backups(root, limit, None, operation)
}

// Inspect backups
fn inspect_backups<T>(
    root: &Path,
    limit: usize,
    generation: Option<&str>,
    mut operation: impl FnMut(&str, Option<u64>, &Connection) -> Result<T, String>,
) -> Result<(Vec<T>, Vec<String>), String> {
    let Some(directory) = crate::database_layout::inspect_recovery_directory(root)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    for (index, entry) in std::fs::read_dir(&directory)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if index >= 1024 {
            warnings.push("Backup directory scan reached its 1024-entry limit.".into());
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("Backup entry unavailable: {error}"));
                continue;
            }
        };
        if !entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "sqlite3")
        {
            continue;
        }
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            warnings.push(format!(
                "Skipped nonregular backup {}",
                entry.file_name().to_string_lossy()
            ));
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort_by_key(|path| {
        std::cmp::Reverse(std::fs::metadata(path).ok().and_then(|m| m.modified().ok()))
    });
    if paths.len() > limit {
        warnings.push(format!(
            "Only the latest {limit} local backups were inspected."
        ));
        paths.truncate(limit);
    }
    let mut results = Vec::new();
    let mut total_bytes = 0u64;
    let started = std::time::Instant::now();
    for path in paths {
        if started.elapsed() > std::time::Duration::from_secs(10) {
            warnings.push("Backup inspection reached its time limit.".into());
            break;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            warnings.push("Skipped backup with a non-UTF8 filename".into());
            continue;
        };
        let result = (|| {
            // Recheck file identity/type immediately before the safe snapshot copy
            if !std::fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .is_file()
            {
                return Err("Not a regular backup file".into());
            }
            // Manager snapshots are standalone. Never let unbounded or active
            // SQLite sidecars bypass the file/total scan budget
            for suffix in ["-wal", "-shm", "-journal"] {
                let sidecar = path.with_file_name(format!("{name}{suffix}"));
                match std::fs::symlink_metadata(sidecar) {
                    Ok(_) => return Err(
                        "Backup has SQLite sidecars; only standalone snapshots can be inspected"
                            .into(),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            let bytes = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            if bytes > 128 * 1024 * 1024 || total_bytes.saturating_add(bytes) > 512 * 1024 * 1024 {
                return Err(
                    "Backup inspection reached its 128 MiB/file or 512 MiB total limit".into(),
                );
            }
            total_bytes += bytes;
            let source = maintenance::open_snapshot(&path)?;
            let source_generation: String = source
                .query_row(
                    "SELECT value FROM database_metadata WHERE key='storage_generation'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if generation.is_some_and(|generation| source_generation != generation) {
                return Err("Different storage generation; not offered as profile history".into());
            }
            let timestamp = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| u64::try_from(d.as_millis()).ok());
            operation(name, timestamp, &source)
        })();
        match result {
            Ok(value) => results.push(value),
            Err(error) => warnings.push(format!("Backup {name}: {error}")),
        }
    }
    Ok((results, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Domain, Storage};
    #[test]
    // Verify external change requires refresh and old revision never becomes valid again
    fn external_change_requires_refresh_and_old_revision_never_becomes_valid_again() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .write_json(Domain::Profiles, "p", &serde_json::json!({"v":1}), 1024)
            .unwrap();
        drop(storage);
        let editor = RecoveryEditor::open(root.path().into()).unwrap();
        let token = editor.read(|_, token| Ok(token.to_owned())).unwrap();
        let conn =
            Connection::open(crate::database_path(root.path(), schema::DATABASE_NAME)).unwrap();
        conn.execute(
            &format!(
                "UPDATE {} SET payload='{{\"v\":2}}'",
                Domain::Profiles.table()
            ),
            [],
        )
        .unwrap();
        drop(conn);
        assert!(editor.preview(&token, |_| Ok(())).is_err());
        editor.refresh().unwrap();
        assert!(editor.preview(&token, |_| Ok(())).is_err());
        let new = editor.read(|_, token| Ok(token.to_owned())).unwrap();
        editor.preview(&new, |_| Ok(())).unwrap();
    }
    #[test]
    // Verify backup failure and failed operation preserve records
    fn backup_failure_and_failed_operation_preserve_records() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .write_json(Domain::Profiles, "p", &serde_json::json!({"v":1}), 1024)
            .unwrap();
        drop(storage);
        let editor = RecoveryEditor::open(root.path().into()).unwrap();
        let token = editor.read(|_, t| Ok(t.to_owned())).unwrap();
        std::fs::rename(
            crate::recovery_directory(root.path()),
            root.path().join("existing-recovery"),
        )
        .unwrap();
        std::fs::write(crate::recovery_directory(root.path()), "blocked directory").unwrap();
        assert!(editor.commit(&token, |_| Ok(())).is_err());
        std::fs::remove_file(crate::recovery_directory(root.path())).unwrap();
        let error = editor.commit(&token, |conn| {
            conn.execute(&format!("DELETE FROM {}", Domain::Profiles.table()), [])
                .unwrap();
            Err::<(), _>("forced rollback".into())
        });
        assert!(error.is_err());
        editor
            .read(|conn, _| {
                assert!(
                    crate::read_json::<serde_json::Value>(conn, Domain::Profiles, "p", 1024)?
                        .is_some()
                );
                Ok(())
            })
            .unwrap();
    }
    #[test]
    // Verify content identity covers plugin namespaces and rows
    fn content_identity_covers_plugin_namespaces_and_rows() {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        storage.run(|conn| {let before=content_identity(conn)?;conn.execute("INSERT INTO settings_vault.plugin_storage_namespaces VALUES ('plugin','namespace','generation',X'0000000000000001')",[]).unwrap();assert_ne!(before,content_identity(conn)?);let before=content_identity(conn)?;conn.execute("INSERT INTO settings_vault.plugin_storage_records VALUES ('plugin','namespace','key','{}',2)",[]).unwrap();assert_ne!(before,content_identity(conn)?);Ok(())}).unwrap();
    }
}
