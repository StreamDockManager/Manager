use crate::timing::{self, Phase};
use crate::{files, schema, Connection, Message, Mode, Storage};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainUsage {
    pub domain: String,
    pub records: u64,
    pub payload_bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatabaseStats {
    pub database_bytes: u64,
    pub reclaimable_bytes: u64,
    pub schema_version: i64,
    pub sqlite_version: String,
    pub storage_generation: String,
    pub domains: Vec<DomainUsage>,
    pub last_backup_error: Option<String>,
    pub recovery_snapshots: Vec<Snapshot>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub path: PathBuf,
    pub bytes: u64,
    pub storage_generation: String,
    pub completed_ms: u64,
    pub revision: String,
}

/// Holds the global admission barrier. Callers must also quiesce external work and
/// rebuild their domain caches before dropping it after restore.
pub struct Maintenance {
    storage: Storage,
    active: bool,
}

/// Directory metadata only. Restore validates the contents of the selected file.
pub struct RecoveryFile {
    pub name: String,
    pub bytes: u64,
    pub completed_ms: u64,
}

impl RecoveryFile {
    fn from_entry(entry: fs::DirEntry) -> Option<Self> {
        let metadata = entry.metadata().ok()?;
        if !metadata.is_file() || crate::database_layout::is_redirect(&metadata) {
            return None;
        }
        let completed_ms = metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis();
        Some(Self {
            name: entry.file_name().to_string_lossy().into_owned(),
            bytes: metadata.len(),
            completed_ms: completed_ms.try_into().ok()?,
        })
    }
}

pub struct StorageSummary {
    pub database_bytes: u64,
    pub reclaimable_bytes: u64,
    pub sqlite_version: String,
    pub last_backup_error: Option<String>,
    pub recovery_files: Vec<RecoveryFile>,
    pub inventory_error: Option<String>,
}

fn database_sizes(conn: &Connection, root: &Path) -> Result<(u64, u64), String> {
    let page_size: u64 = conn
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let free: u64 = conn
        .pragma_query_value(None, "freelist_count", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let path = crate::database_path(root, schema::DATABASE_NAME);
    Ok((
        fs::metadata(path).map_err(|e| e.to_string())?.len(),
        free.saturating_mul(page_size),
    ))
}

impl Storage {
    fn last_backup_error(&self) -> Result<Option<String>, String> {
        Ok(self
            .inner
            .gate
            .lock()
            .map_err(|_| "Storage admission lock failed")?
            .last_backup_error
            .clone())
    }

    /// Report database sizes and list recovery files on the writer, between backups.
    pub fn summary(&self) -> Result<StorageSummary, String> {
        let storage = self.clone();
        self.run(move |conn| {
            let root = storage.root();
            let last_backup_error = storage.last_backup_error()?;
            let (database_bytes, reclaimable_bytes) = database_sizes(conn, root)?;
            let mut recovery_files = Vec::new();
            let mut skipped = 0;
            let result = visit_backup_entries(root, |entry| {
                if let Some(file) = RecoveryFile::from_entry(entry) {
                    recovery_files.push(file);
                } else {
                    skipped += 1;
                }
                Ok(())
            });
            recovery_files.sort_by(|a, b| a.name.cmp(&b.name));
            let inventory_error = match result {
                Err(error) => Some(format!("Recovery inventory incomplete: {error}")),
                Ok(()) if skipped > 0 => Some(format!(
                    "Recovery inventory skipped {skipped} unreadable or non-regular snapshot entries."
                )),
                Ok(()) => None,
            };
            Ok(StorageSummary {
                database_bytes,
                reclaimable_bytes,
                sqlite_version: rusqlite::version().into(),
                last_backup_error,
                recovery_files,
                inventory_error,
            })
        })
    }

    pub fn stats(&self) -> Result<DatabaseStats, String> {
        let storage = self.clone();
        self.run(move |conn| {
            let root = storage.root();
            let last_backup_error = storage.last_backup_error()?;
            let (database_bytes, reclaimable_bytes) = database_sizes(conn, root)?;
            let storage_generation = crate::persisted_generation(conn)?;
            let mut domains = Vec::new();
            for domain in crate::Domain::ALL {
                let sql = format!(
                    "SELECT count(*), coalesce(sum(length(CAST(key AS BLOB)) + length(CAST(payload AS BLOB))), 0) FROM {}",
                    domain.table()
                );
                let usage = conn.query_row(&sql, [], |row| Ok(DomainUsage {
                    domain: domain.table().into(),
                    records: row.get(0)?,
                    payload_bytes: row.get(1)?,
                })).map_err(|e| e.to_string())?;
                domains.push(usage);
            }
            let recovery_snapshots = backup_paths(root)?
                .into_iter().map(snapshot_metadata).collect::<Result<_,_>>()?;
            Ok(DatabaseStats {
                database_bytes,
                reclaimable_bytes,
                schema_version: schema::SCHEMA_VERSION,
                sqlite_version: rusqlite::version().into(),
                storage_generation,
                domains,
                last_backup_error,
                recovery_snapshots,
            })
        })
    }

    // Create a backup of the current database
    pub fn backup(&self, destination: PathBuf) -> Result<Snapshot, String> {
        let root = self.root().to_owned();
        let validators = self.inner.validators.clone();
        self.run(move |conn| backup_connection(conn, &root, &destination, &validators))
    }

    // List the database backup paths
    pub fn backup_paths(&self) -> Result<Vec<PathBuf>, String> {
        backup_paths(self.root())
    }

    /// Explicitly request a managed recovery point. Automatic points are also
    /// created after a 30-second debounce, within five minutes, and on shutdown.
    pub fn backup_recovery(&self) -> Result<Snapshot, String> {
        let root = self.root().to_owned();
        let gate = self.inner.gate.clone();
        let validators = self.inner.validators.clone();
        self.run(move |conn| {
            let result = automatic_backup(conn, &root, &validators);
            match result {
                Ok((snapshot, warning)) => {
                    gate.lock()
                        .map_err(|_| "Storage admission lock failed")?
                        .last_backup_error = warning;
                    Ok(snapshot)
                }
                Err(error) => {
                    gate.lock()
                        .map_err(|_| "Storage admission lock failed")?
                        .last_backup_error = Some(error.clone());
                    Err(error)
                }
            }
        })
    }

    /// Inspect a completed, immutable snapshot. Domain validation belongs to the caller
    pub fn read_snapshot<T, F>(&self, path: &Path, operation: F) -> Result<T, String>
    where
        F: FnOnce(&Connection) -> Result<T, String>,
    {
        let conn = open_snapshot(path)?;
        validate_domains(&conn, &self.inner.validators)?;
        operation(&conn)
    }

    // Run scheduled database maintenance
    pub fn maintenance(&self) -> Result<Maintenance, String> {
        if std::thread::current().id() == self.inner.worker_id {
            return Err("Recursive storage maintenance is not allowed".into());
        }
        {
            let mut gate = self
                .inner
                .gate
                .lock()
                .map_err(|_| "Storage admission lock failed")?;
            if gate.mode != Mode::Open {
                return Err("Storage is already in maintenance or recovery".into());
            }
            gate.mode = Mode::Maintenance;
            gate.generation = gate
                .generation
                .checked_add(1)
                .ok_or("Storage generation exhausted")?;
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let sent = self.inner.sender.send(Message::Job(Box::new(move |_| {
            let _ = sender.send(());
        })));
        if sent.is_err() || receiver.recv().is_err() {
            self.inner
                .gate
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .mode = Mode::Failed;
            return Err("Storage worker stopped while entering maintenance".into());
        }
        Ok(Maintenance {
            storage: self.clone(),
            active: true,
        })
    }
}

impl Maintenance {
    // Run for the current operation
    fn run<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Option<Connection>) -> Result<T, String> + Send + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.storage
            .inner
            .sender
            .send(Message::Job(Box::new(move |connection| {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    operation(connection)
                }))
                .unwrap_or_else(|_| Err("Storage maintenance failed unexpectedly".into()));
                let _ = sender.send(result);
            })))
            .map_err(|_| "Storage worker is unavailable".to_string())?;
        receiver
            .recv()
            .map_err(|_| "Storage worker stopped during maintenance".to_string())?
    }

    // Create a backup of the current database
    pub fn backup(&self, destination: PathBuf) -> Result<Snapshot, String> {
        let root = self.storage.root().to_owned();
        let validators = self.storage.inner.validators.clone();
        self.run(move |connection| {
            backup_connection(
                connection
                    .as_ref()
                    .ok_or("Storage connection is unavailable")?,
                &root,
                &destination,
                &validators,
            )
        })
    }

    /// Restore the exact validated local copy, retaining a pre-restore snapshot
    /// The admission barrier remains active for domain-cache and service rebuilding
    pub fn restore<F>(&self, candidate: PathBuf, validate_domain: F) -> Result<Snapshot, String>
    where
        F: FnOnce(&Connection) -> Result<(), String> + Send + 'static,
    {
        let root = self.storage.root().to_owned();
        crate::vault::check_transfer_path(&root, &candidate)?;
        let gate = self.storage.inner.gate.clone();
        let validators = self.storage.inner.validators.clone();
        self.run(move |live| {
            let source = open_snapshot(&candidate)?;
            let temp = tempfile::NamedTempFile::new_in(crate::database_directory(&root))
                .map_err(|e| e.to_string())?;
            let mut replacement = Connection::open(temp.path()).map_err(|e| e.to_string())?;
            copy_database(&source, &mut replacement)?;
            drop(source);
            schema::validate(&replacement)?;
            validate_domains(&replacement, &validators)?;
            validate_domain(&replacement)?;
            if !replacement.is_autocommit() {
                return Err(
                    "Restore reconciliation left an uncommitted transaction; primary preserved"
                        .into(),
                );
            }
            crate::permissions::discard(&replacement)?;
            // An older snapshot must not roll the host-wide generation back
            replacement
                .execute(
                    "UPDATE database_metadata SET value=?1 WHERE key='storage_generation'",
                    [uuid::Uuid::new_v4().to_string()],
                )
                .map_err(|e| e.to_string())?;
            schema::configure(&replacement)?;
            // Reconciliation and generation updates are now complete. Validate
            // the exact final database before it can replace the primary.
            schema::validate(&replacement)?;
            validate_domains(&replacement, &validators)?;
            drop(replacement);
            temp.as_file().sync_all().map_err(|e| e.to_string())?;
            let recovery = crate::database_layout::ensure_recovery_directory(&root)?;
            let before = backup_connection(
                live.as_ref().ok_or("Storage connection is unavailable")?,
                &root,
                &recovery.join(format!("pre-restore-{}.sqlite3", uuid::Uuid::new_v4())),
                &validators,
            )?;
            let old = live.take().ok_or("Storage connection is unavailable")?;
            if let Err((connection, error)) = old.close() {
                *live = Some(connection);
                return Err(error.to_string());
            }
            let publication = files::atomic_replace(
                temp.path(),
                &crate::database_path(&root, schema::DATABASE_NAME),
            );
            // Whether replacement or directory sync failed, reopen the actual file
            // A post-publication error must never pretend that old data is still live
            match schema::open(&root) {
                Ok(connection) => *live = Some(connection),
                Err(error) => {
                    gate.lock().unwrap_or_else(|e| e.into_inner()).mode = Mode::Failed;
                    return Err(format!(
                        "Restore requires recovery; database preserved: {error}"
                    ));
                }
            }
            if let Err(error) = publication {
                gate.lock().unwrap_or_else(|e| e.into_inner()).mode = Mode::Failed;
                return Err(format!(
                    "Restore publication outcome requires recovery: {error}"
                ));
            }
            Ok(before)
        })
    }

    // Inspect
    pub fn inspect<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, String> + Send + 'static,
    {
        self.run(move |conn| operation(conn.as_ref().ok_or("Storage connection is unavailable")?))
    }

    // Compact the database storage
    pub fn compact(&self) -> Result<(), String> {
        let root = self.storage.root().to_owned();
        self.run(move |connection| {
            files::require_compaction_space(
                &root,
                fs::metadata(crate::database_path(&root, schema::DATABASE_NAME))
                    .map_err(|e| e.to_string())?
                    .len(),
            )?;
            connection
                .as_ref()
                .ok_or("Storage connection is unavailable")?
                .execute_batch("VACUUM")
                .map_err(|e| e.to_string())
        })
    }
}

impl Drop for Maintenance {
    // Release resources held by this owner
    fn drop(&mut self) {
        if self.active {
            let mut gate = self
                .storage
                .inner
                .gate
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if gate.mode == Mode::Maintenance {
                gate.mode = Mode::Open;
                drop(gate);
                // Publish admission before waking a parked worker. If the bounded
                // channel is full, an already queued job supplies the wake instead.
                let _ = self.storage.inner.sender.try_send(Message::Wake);
            }
        }
    }
}

// Open snapshot
pub(crate) fn open_snapshot(path: &Path) -> Result<schema::ScratchDatabase, String> {
    schema::snapshot_copy(path)
}

// Read metadata describing a database snapshot
pub(crate) fn snapshot_metadata(path: PathBuf) -> Result<Snapshot, String> {
    let _inspection = timing::start(Phase::SnapshotInspection);
    let conn = open_snapshot(&path)?;
    let generation = crate::persisted_generation(&conn)?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    let revision = conn
        .query_row(
            "SELECT value FROM database_metadata WHERE key='commit_sequence'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let completed_ms = metadata
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis() as u64;
    Ok(Snapshot {
        path,
        bytes: metadata.len(),
        storage_generation: generation,
        completed_ms,
        revision,
    })
}

// Create a scheduled database backup when due
pub(crate) fn automatic_backup(
    conn: &Connection,
    root: &Path,
    validators: &crate::Validators,
) -> Result<(Snapshot, Option<String>), String> {
    let directory = crate::database_layout::ensure_recovery_directory(root)?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    let snapshot = backup_connection(
        conn,
        root,
        &directory.join(format!(
            "snapshot-{millis}-{}.sqlite3",
            uuid::Uuid::new_v4()
        )),
        validators,
    )?;
    let capture = timing::start(Phase::BackupCapture);
    let (keystore, capture_warning) =
        match crate::installation_key::recovery::capture(conn, root, millis) {
            Ok(path) => (path, None),
            Err(error) => (
                None,
                Some(format!(
                    "Database snapshot saved; encrypted plugin settings backup unavailable: {error}"
                )),
            ),
        };
    drop(capture);
    let _retention = timing::start(Phase::BackupRetention);
    let cleanup = match crate::retention::prune_protected(root, &snapshot.path, keystore.as_deref())
    {
        Ok(warning) => warning,
        // Publication succeeded. Report cleanup trouble without making the
        // worker retry the same commit and accumulate duplicate snapshots.
        Err(error) => Some(format!(
            "Recovery snapshot saved; cleanup deferred: {error}"
        )),
    };
    let warning = match (capture_warning, cleanup) {
        (Some(capture), Some(cleanup)) => Some(format!("{capture}; {cleanup}")),
        (capture, cleanup) => capture.or(cleanup),
    };
    Ok((snapshot, warning))
}

// List the database backup paths
pub(crate) fn backup_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    visit_backup_entries(root, |entry| {
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            return Err("Recovery inventory contains an invalid snapshot".into());
        }
        paths.push(entry.path());
        Ok(())
    })?;
    paths.sort();
    Ok(paths)
}

fn visit_backup_entries(
    root: &Path,
    mut visit: impl FnMut(fs::DirEntry) -> Result<(), String>,
) -> Result<(), String> {
    let Some(recovery) = crate::database_layout::inspect_recovery_directory(root)? else {
        return Ok(());
    };
    let mut count = 0;
    for entry in fs::read_dir(recovery).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.path().extension().is_some_and(|ext| ext == "sqlite3") {
            count += 1;
            if count > 1024 {
                return Err("Recovery snapshot inventory exceeds its limit".into());
            }
            visit(entry)?;
        }
    }
    Ok(())
}

// Copy database
pub(crate) fn copy_database(
    source: &Connection,
    destination: &mut Connection,
) -> Result<(), String> {
    let backup = rusqlite::backup::Backup::new(source, destination).map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        if started.elapsed() > Duration::from_secs(10) {
            return Err("Database snapshot exceeded its maintenance deadline".into());
        }
        match backup.step(256).map_err(|e| e.to_string())? {
            rusqlite::backup::StepResult::Done => return Ok(()),
            rusqlite::backup::StepResult::More => {}
            rusqlite::backup::StepResult::Busy | rusqlite::backup::StepResult::Locked => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => return Err("Unexpected database snapshot result".into()),
        }
    }
}

// Copy the database through the SQLite backup API
pub(crate) fn backup_connection(
    source: &Connection,
    root: &Path,
    destination: &Path,
    validators: &crate::Validators,
) -> Result<Snapshot, String> {
    let parent = destination
        .parent()
        .ok_or("Backup destination has no directory")?;
    files::ensure_directory(parent)?;
    let parent = parent.canonicalize().map_err(|e| e.to_string())?;
    crate::installation_key::check_backup_directory(&parent)?;
    let filename = destination
        .file_name()
        .ok_or("Backup destination has no filename")?;
    let destination = parent.join(filename);
    crate::vault::check_transfer_path(root, &destination)?;
    if destination == crate::database_path(root, schema::DATABASE_NAME)
        || destination == root.join("profiles/writer.lock")
    {
        return Err("A backup cannot replace the live database or ownership lock".into());
    }
    if destination
        .symlink_metadata()
        .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
    {
        return Err("Backup destination is not a regular file".into());
    }
    let temp = tempfile::NamedTempFile::new_in(&parent).map_err(|e| e.to_string())?;
    let mut copy = Connection::open(temp.path()).map_err(|e| e.to_string())?;
    let copy_pages = |from: &Connection, to: &mut Connection| {
        let _copy = timing::start(Phase::BackupCopy);
        copy_database(from, to)
    };
    if crate::permissions::installed(source)? {
        // The external destination never receives a page containing live grants.
        let mut private = schema::ScratchDatabase::new()?;
        copy_pages(source, &mut private)?;
        {
            let _validation = timing::start(Phase::BackupValidation);
            schema::validate(&private)?;
        }
        crate::permissions::discard(&private)?;
        copy_pages(&private, &mut copy)?;
    } else {
        copy_pages(source, &mut copy)?;
    }
    crate::plugin_settings::omit(&copy)?;
    {
        let _validation = timing::start(Phase::BackupValidation);
        schema::validate(&copy)?;
        validate_domains(&copy, validators)?;
    }
    let revision = copy
        .query_row(
            "SELECT value FROM database_metadata WHERE key='commit_sequence'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let generation = crate::persisted_generation(&copy)?;
    drop(copy);
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    let bytes = temp.as_file().metadata().map_err(|e| e.to_string())?.len();
    files::atomic_replace(temp.path(), &destination)?;
    Ok(Snapshot {
        path: destination,
        bytes,
        storage_generation: generation,
        completed_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis() as u64,
        revision,
    })
}

// Validate domains
fn validate_domains(conn: &Connection, registry: &crate::Validators) -> Result<(), String> {
    let validators = registry
        .lock()
        .map_err(|_| "Snapshot validator registry failed")?
        .clone();
    for validator in validators {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| validator(conn)))
            .map_err(|_| "Snapshot domain validation panicked".to_string())??;
    }
    Ok(())
}
