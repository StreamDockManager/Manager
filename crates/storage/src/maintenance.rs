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

impl Storage {
    // Read the current subsystem statistics
    pub fn stats(&self) -> Result<DatabaseStats, String> {
        let path = self.root().join(schema::DATABASE_NAME);
        let last_backup_error = self
            .inner
            .gate
            .lock()
            .map_err(|_| "Storage admission lock failed")?
            .last_backup_error
            .clone();
        let root = self.root().to_owned();
        self.run(move|conn| {
            let page_size:u64=conn.query_row("PRAGMA page_size",[],|r|r.get(0)).map_err(|e|e.to_string())?;
            let free:u64=conn.query_row("PRAGMA freelist_count",[],|r|r.get(0)).map_err(|e|e.to_string())?;
            let generation:String=conn.query_row("SELECT value FROM database_metadata WHERE key='storage_generation'",[],|r|r.get(0)).map_err(|e|e.to_string())?;
            let domains=crate::Domain::ALL.into_iter().map(|domain| {
                conn.query_row(&format!("SELECT count(*),coalesce(sum(length(CAST(key AS BLOB))+length(CAST(payload AS BLOB))),0) FROM {}",domain.table()),[],|r|Ok(DomainUsage{domain:domain.table().into(),records:r.get(0)?,payload_bytes:r.get(1)?})).map_err(|e|e.to_string())
            }).collect::<Result<_,_>>()?;
            let recovery_snapshots=backup_paths(&root)?.into_iter().map(snapshot_metadata).collect::<Result<_,_>>()?;
            Ok(DatabaseStats{database_bytes:fs::metadata(path).map_err(|e|e.to_string())?.len(),reclaimable_bytes:free.saturating_mul(page_size),schema_version:schema::SCHEMA_VERSION,sqlite_version:rusqlite::version().into(),storage_generation:generation,domains,last_backup_error,recovery_snapshots})
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
        let gate = self.storage.inner.gate.clone();
        let validators = self.storage.inner.validators.clone();
        self.run(move |live| {
            let source = open_snapshot(&candidate)?;
            let temp = tempfile::NamedTempFile::new_in(&root).map_err(|e| e.to_string())?;
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
            let recovery = root.join("recovery");
            files::ensure_directory(&recovery)?;
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
            let publication = files::atomic_replace(temp.path(), &root.join(schema::DATABASE_NAME));
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
                fs::metadata(root.join(schema::DATABASE_NAME))
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
            }
        }
    }
}

// Open snapshot
pub(crate) fn open_snapshot(path: &Path) -> Result<Connection, String> {
    schema::snapshot_copy(path)
}

// Read metadata describing a database snapshot
pub(crate) fn snapshot_metadata(path: PathBuf) -> Result<Snapshot, String> {
    let conn = open_snapshot(&path)?;
    let generation = conn
        .query_row(
            "SELECT value FROM database_metadata WHERE key='storage_generation'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
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
    let directory = root.join("recovery");
    files::ensure_directory(&directory)?;
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
    let warning = match crate::retention::prune(root, &snapshot.path) {
        Ok(warning) => warning,
        // Publication succeeded. Report cleanup trouble without making the
        // worker retry the same commit and accumulate duplicate snapshots.
        Err(error) => Some(format!(
            "Recovery snapshot saved; cleanup deferred: {error}"
        )),
    };
    Ok((snapshot, warning))
}

// List the database backup paths
pub(crate) fn backup_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let recovery = root.join("recovery");
    if !recovery.try_exists().map_err(|e| e.to_string())? {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(recovery).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.path().extension().is_some_and(|ext| ext == "sqlite3") {
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("Recovery inventory contains an invalid snapshot".into());
            }
            paths.push(entry.path());
            if paths.len() > 1024 {
                return Err("Recovery snapshot inventory exceeds its limit".into());
            }
        }
    }
    paths.sort();
    Ok(paths)
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
    let filename = destination
        .file_name()
        .ok_or("Backup destination has no filename")?;
    let destination = parent.join(filename);
    if destination == root.join(schema::DATABASE_NAME)
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
    copy_database(source, &mut copy)?;
    schema::validate(&copy)?;
    validate_domains(&copy, validators)?;
    let revision = copy
        .query_row(
            "SELECT value FROM database_metadata WHERE key='commit_sequence'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let generation = copy
        .query_row(
            "SELECT value FROM database_metadata WHERE key='storage_generation'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
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
