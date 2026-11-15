//! Offline record repair and explicitly reviewed full configuration restore
//! Corrupt primaries can be owned without opening them for writes
//! Opening and inspection touch only a disposable snapshot. The root writer lock
//! is held for the complete session; no ordinary runtime shares this database.
use crate::{maintenance, own_root, schema, RootOwner};
use rusqlite::Connection;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

struct State {
    snapshot: Option<Connection>,
    unavailable: Option<String>,
    fingerprint: schema::Fingerprint,
    revision: String,
}
impl State {
    // Return the opened inspection snapshot or explain why it is unavailable
    fn snapshot(&self) -> Result<&Connection, String> {
        self.snapshot.as_ref().ok_or_else(|| {
            self.unavailable.clone().unwrap_or_else(|| {
                "Primary unavailable; select a validated backup for full restore".into()
            })
        })
    }
}
// Compute the identity used to reject stale repair edits
fn editor_fingerprint(root: &Path) -> Result<schema::Fingerprint, String> {
    let mut fingerprint = schema::fingerprint(&root.join(schema::DATABASE_NAME))?;
    fingerprint.extend(schema::fingerprint(&root.join(schema::RECOVERY_PENDING))?);
    Ok(fingerprint)
}
// Load state
fn load_state(root: &Path) -> Result<State, String> {
    let fingerprint = editor_fingerprint(root)?;
    let result = if root
        .join(schema::RECOVERY_PENDING)
        .symlink_metadata()
        .is_ok()
    {
        Err("An interrupted database restore must be resolved before record repair".into())
    } else {
        schema::snapshot_copy(&root.join(schema::DATABASE_NAME))
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
        let mut copy = Connection::open("").map_err(|e| e.to_string())?;
        maintenance::copy_database(state.snapshot()?, &mut copy)?;
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
        let destination = self.root.join("recovery").join(format!(
            "before-manual-edit-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        maintenance::backup_connection(
            state.snapshot()?,
            &self.root,
            &destination,
            &Default::default(),
        )?;
        self.check(&state, Some(revision))?;
        let mut connection = schema::connect(&self.root.join(schema::DATABASE_NAME))?;
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
        let (snapshot, _fingerprint) =
            schema::snapshot_copy_checked(&self.root.join(schema::DATABASE_NAME)).map_err(|error| {
                if result.is_ok() { format!("Repair was committed. Backup: {}. Could not refresh recovery: {error}. Reopen Manager before further changes.", destination.display()) } else { error }
            })?;
        *state = State {
            snapshot: Some(snapshot),
            unavailable: None,
            fingerprint: editor_fingerprint(&self.root)?,
            revision: uuid::Uuid::new_v4().to_string(),
        };
        result.map(|value| (value, destination))
    }
}

// Compare all application-owned rows under SQLite's write lock, including writers
// which do not honor Manager's root lock or increment the commit sequence.
fn content_identity(connection: &Connection) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for table in std::iter::once("database_metadata")
        .chain(crate::Domain::ALL.map(|d| d.table()))
        .chain([
            "plugin_storage_namespaces",
            "plugin_storage_records",
            "plugin_storage_receipts",
        ])
    {
        digest.update(table.as_bytes());
        let sql = if table == "database_metadata" {
            "SELECT key,value FROM database_metadata ORDER BY key".to_owned()
        } else if table.starts_with("plugin_storage_") {
            format!(
                "SELECT * FROM {table} ORDER BY {}",
                if table == "plugin_storage_namespaces" {
                    "plugin_id,namespace"
                } else if table == "plugin_storage_records" {
                    "plugin_id,namespace,key"
                } else {
                    "plugin_id,namespace,operation_id"
                }
            )
        } else {
            format!("SELECT key,payload,revision FROM {table} ORDER BY key")
        };
        let mut statement = connection.prepare(&sql).map_err(|e| e.to_string())?;
        let count = statement.column_count();
        let mut rows = statement.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            digest.update([0xff]);
            for index in 0..count {
                match row.get_ref(index).map_err(|e| e.to_string())? {
                    rusqlite::types::ValueRef::Text(bytes) => {
                        digest.update([1]);
                        digest.update((bytes.len() as u64).to_le_bytes());
                        digest.update(bytes);
                    }
                    rusqlite::types::ValueRef::Integer(value) => {
                        digest.update([2]);
                        digest.update(value.to_le_bytes());
                    }
                    _ => return Err("Unsupported database value during recovery".into()),
                }
            }
        }
    }
    Ok(digest.finalize().to_vec())
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
        let conn = Connection::open(root.path().join(schema::DATABASE_NAME)).unwrap();
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
        assert!(editor.preview(&new, |_| Ok(())).is_ok());
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
            root.path().join("recovery"),
            root.path().join("existing-recovery"),
        )
        .unwrap();
        std::fs::write(root.path().join("recovery"), "blocked directory").unwrap();
        assert!(editor.commit(&token, |_| Ok(())).is_err());
        std::fs::remove_file(root.path().join("recovery")).unwrap();
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
        storage.run(|conn| {let before=content_identity(conn)?;conn.execute("INSERT INTO plugin_storage_namespaces VALUES ('plugin','namespace','generation','revision')",[]).unwrap();assert_ne!(before,content_identity(conn)?);let before=content_identity(conn)?;conn.execute("INSERT INTO plugin_storage_records VALUES ('plugin','namespace','key','{}',2)",[]).unwrap();assert_ne!(before,content_identity(conn)?);Ok(())}).unwrap();
    }
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
    let directory = root.join("recovery");
    if directory.try_exists().map_err(|e| e.to_string())?
        && !directory
            .symlink_metadata()
            .map_err(|e| e.to_string())?
            .is_dir()
    {
        return Err("Recovery backup directory is not a regular directory".into());
    }
    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    if directory.exists() {
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
