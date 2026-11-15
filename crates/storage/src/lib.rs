//! Host-owned SQLite persistence. Clones share one bounded worker and root lock
//!
//! Worker closures must only use the supplied connection and owned immutable input
//! They must not acquire runtime locks, call this service recursively, or do external
//! I/O. Keep domain validation outside transactions and recheck revisions inside them
mod files;
mod maintenance;
mod records;
mod recovery;
mod repair;
mod retention;
pub use repair::{inspect_recovery_backups, RecoveryEditor};
mod schema;

pub use files::{replace_file, sync_directory};
pub mod bounded_io;
pub mod json_digest;
pub use maintenance::{DatabaseStats, Maintenance, Snapshot};
pub use records::{
    delete, list_json, read_json, revision, write_json, Domain, EncodedJson, JsonRecord,
    MAX_KEY_BYTES, MAX_RECORD_BYTES,
};
pub use recovery::{RecoveryReport, RecoverySource, RestorePreview};
pub use rusqlite;
pub use schema::{validate as validate_database, APPLICATION_ID, DATABASE_NAME, SCHEMA_VERSION};

use rusqlite::{Connection, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, RecvTimeoutError, SyncSender, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const MAX_PENDING_OPERATIONS: usize = 64;
pub const MAX_ADMITTED_BYTES: usize = 64 * 1024 * 1024;
type Job = Box<dyn FnOnce(&mut Option<Connection>) + Send>;
type SnapshotValidator = Arc<dyn Fn(&Connection) -> Result<(), String> + Send + Sync>;
type Validators = Arc<Mutex<Vec<SnapshotValidator>>>;
enum Message {
    Job(Job),
    Shutdown,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Open,
    Maintenance,
    Failed,
}
struct Gate {
    mode: Mode,
    generation: u64,
    operations: usize,
    bytes: usize,
    last_backup_error: Option<String>,
}
struct Inner {
    root: PathBuf,
    sender: SyncSender<Message>,
    gate: Arc<Mutex<Gate>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    worker_id: thread::ThreadId,
    validators: Validators,
}

impl Drop for Inner {
    // Release resources held by this owner
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Shutdown);
        if let Some(worker) = self.worker.get_mut().ok().and_then(Option::take) {
            if worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

#[derive(Clone)]
pub struct Storage {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for Storage {
    // Format the value for display
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("root", &self.inner.root)
            .finish_non_exhaustive()
    }
}

impl Storage {
    // Open for the current operation
    pub fn open(config_root: PathBuf) -> Result<Self, String> {
        Self::open_validated(config_root, |_| Ok(()))
    }

    /// Validate a recovered disposable copy before opening originals for writes
    pub fn open_validated(
        config_root: PathBuf,
        validator: impl Fn(&Connection) -> Result<(), String> + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let (root, owner) = own_root(&config_root)?;
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_OPERATIONS);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let gate = Arc::new(Mutex::new(Gate {
            mode: Mode::Open,
            generation: 1,
            operations: 0,
            bytes: 0,
            last_backup_error: None,
        }));
        let worker_gate = gate.clone();
        let validator: SnapshotValidator = Arc::new(validator);
        let validators: Validators = Arc::new(Mutex::new(vec![validator.clone()]));
        let worker_validators = validators.clone();
        let worker_root = root.clone();
        let worker = thread::Builder::new()
            .name("stream-dock-manager-storage".into())
            .spawn(move || {
                // Declare ownership before every connection: reverse local drop
                // order closes SQLite before unlock on panic/startup failure as
                // well as the explicit normal-shutdown drop below.
                let _owner: RootOwner = owner;
                let opened = schema::open_validated(&worker_root, &*validator);
                let mut connection = match opened {
                    Ok(connection) => {
                        let _ = ready_tx.send(Ok(()));
                        Some(connection)
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let mut dirty_since: Option<Instant> = None;
                let mut last_change = Instant::now();
                let mut next_attempt = Instant::now();
                loop {
                    let before = connection.as_ref().map(Connection::total_changes);
                    let shutdown = match receiver.recv_timeout(Duration::from_secs(1)) {
                        Ok(Message::Job(job)) => {
                            job(&mut connection);
                            false
                        }
                        Ok(Message::Shutdown) | Err(RecvTimeoutError::Disconnected) => true,
                        Err(RecvTimeoutError::Timeout) => false,
                    };
                    if before != connection.as_ref().map(Connection::total_changes) {
                        let now = Instant::now();
                        dirty_since.get_or_insert(now);
                        last_change = now;
                    }
                    let allowed = worker_gate.lock().is_ok_and(|gate| gate.mode == Mode::Open);
                    let due = dirty_since.is_some_and(|first| {
                        shutdown
                            || (Instant::now() >= next_attempt
                                && (last_change.elapsed() >= Duration::from_secs(30)
                                    || first.elapsed() >= Duration::from_secs(300)))
                    });
                    if allowed && due {
                        if let Some(conn) = connection.as_ref() {
                            let result = maintenance::automatic_backup(
                                conn,
                                &worker_root,
                                &worker_validators,
                            );
                            let error = match result {
                                Ok((_, warning)) => {
                                    dirty_since = None;
                                    warning
                                }
                                Err(error) => Some(error),
                            };
                            if shutdown {
                                if let Some(error) = error.as_ref() {
                                    eprintln!(
                                        "Stream Dock Manager recovery snapshot failed: {error}"
                                    );
                                }
                            }
                            if let Ok(mut gate) = worker_gate.lock() {
                                gate.last_backup_error = error;
                            }
                            next_attempt = Instant::now() + Duration::from_secs(30);
                        }
                    }
                    if shutdown {
                        break;
                    }
                }
                drop(connection);
            })
            .map_err(|e| e.to_string())?;
        // Join even if startup panics and disconnects the ready channel: failure
        // returns only after worker unwinding has closed SQLite and released ownership.
        match ready_rx
            .recv()
            .unwrap_or_else(|_| Err("Storage worker failed during startup".to_string()))
        {
            Ok(()) => {}
            Err(error) => {
                let _ = worker.join();
                return Err(error);
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                root,
                sender,
                gate,
                worker_id: worker.thread().id(),
                worker: Mutex::new(Some(worker)),
                validators,
            }),
        })
    }

    // Read the root directory owned by this store
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Register a pure domain invariant check before normal services start
    /// Validators must use only their snapshot connection, never runtime locks or
    /// this service. They run before any recovery snapshot is published.
    pub fn add_snapshot_validator<F>(&self, validator: F) -> Result<(), String>
    where
        F: Fn(&Connection) -> Result<(), String> + Send + Sync + 'static,
    {
        self.inner
            .validators
            .lock()
            .map_err(|_| "Snapshot validator registry failed")?
            .push(Arc::new(validator));
        Ok(())
    }
    // Return the current generation identifier
    pub fn generation(&self) -> u64 {
        self.inner
            .gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation
    }

    /// Run bounded native work. Captured input must be bounded by the caller
    /// Use `run_with_cost` when capturing serialized payloads to account for their bytes
    pub fn run<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        self.run_with_cost(0, operation)
    }

    // Run with cost
    pub fn run_with_cost<T, F>(&self, retained_bytes: usize, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        self.run_at_generation(self.generation(), retained_bytes, operation)
    }

    /// Fence asynchronous producers that started work before reset/restore
    pub fn run_at_generation<T, F>(
        &self,
        generation: u64,
        retained_bytes: usize,
        operation: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, String> + Send + 'static,
    {
        if thread::current().id() == self.inner.worker_id {
            return Err("Recursive storage access is not allowed".into());
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let gate = self.inner.gate.clone();
        let mut admission = gate.lock().map_err(|_| "Storage admission lock failed")?;
        if admission.mode != Mode::Open {
            return Err("Storage is unavailable during maintenance or recovery".into());
        }
        if admission.generation != generation {
            return Err("Storage operation belongs to an expired generation".into());
        }
        if admission.operations >= MAX_PENDING_OPERATIONS
            || retained_bytes > MAX_ADMITTED_BYTES.saturating_sub(admission.bytes)
        {
            return Err("Storage queue is busy; retry after pending operations finish".into());
        }
        admission.operations += 1;
        admission.bytes += retained_bytes;
        let completion_gate = gate.clone();
        let job = Box::new(move |connection: &mut Option<Connection>| {
            let permitted = completion_gate
                .lock()
                .is_ok_and(|gate| gate.mode == Mode::Open && gate.generation == generation);
            let result = if !permitted {
                Err("Storage operation invalidated by maintenance".into())
            } else if let Some(conn) = connection.as_mut() {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(conn)));
                let result = outcome.unwrap_or_else(|_| {
                    Err(
                        "Storage operation panicked; changes were rolled back where possible"
                            .into(),
                    )
                });
                if !conn.is_autocommit() {
                    let rolled_back = conn.execute_batch("ROLLBACK");
                    if rolled_back.is_err() {
                        if let Ok(mut gate) = completion_gate.lock() {
                            gate.mode = Mode::Failed;
                        }
                    }
                    Err("Storage operation left an open transaction; it was not committed".into())
                } else {
                    result
                }
            } else {
                Err("Storage connection is unavailable".into())
            };
            if let Ok(mut gate) = completion_gate.lock() {
                gate.operations -= 1;
                gate.bytes -= retained_bytes;
            }
            let _ = reply_tx.send(result);
        });
        if let Err(error) = self.inner.sender.try_send(Message::Job(job)) {
            admission.operations -= 1;
            admission.bytes -= retained_bytes;
            return Err(match error {
                TrySendError::Full(_) => "Storage queue is busy",
                TrySendError::Disconnected(_) => "Storage worker is unavailable",
            }
            .into());
        }
        drop(admission);
        reply_rx.recv().map_err(|_| {
            "Storage worker stopped before reporting the operation outcome".to_string()
        })?
    }

    // Run a database operation in a transaction
    pub fn transaction<T, F>(&self, operation: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T, String> + Send + 'static,
    {
        self.transaction_with_cost(0, operation)
    }

    // Run a database transaction with its declared maintenance cost
    pub fn transaction_with_cost<T, F>(
        &self,
        retained_bytes: usize,
        operation: F,
    ) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T, String> + Send + 'static,
    {
        self.run_with_cost(retained_bytes, move |conn| {
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| e.to_string())?;
            let result = operation(&tx)?;
            tx.execute("UPDATE database_metadata SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='commit_sequence' AND CAST(value AS INTEGER)<9223372036854775807",[]).map_err(|e|e.to_string())?;
            if tx.changes()!=1 {return Err("Storage commit sequence is unavailable or exhausted".into());}
            tx.commit().map_err(|e| e.to_string())?;
            Ok(result)
        })
    }

    // Read JSON
    pub fn read_json<T: DeserializeOwned>(
        &self,
        domain: Domain,
        key: &str,
        max_bytes: usize,
    ) -> Result<Option<T>, String> {
        let key = key.to_owned();
        let text = self.run(move |conn| records::read_text(conn, domain, &key, max_bytes))?;
        text.map(|text| {
            serde_json::from_str(&text).map_err(|_| "Stored record has invalid domain data".into())
        })
        .transpose()
    }

    // Write JSON
    pub fn write_json<T: Serialize + ?Sized>(
        &self,
        domain: Domain,
        key: &str,
        value: &T,
        max_bytes: usize,
    ) -> Result<(), String> {
        let payload = records::encode(value, max_bytes)?;
        let key = key.to_owned();
        self.transaction_with_cost(payload.len() + key.len(), move |tx| {
            records::write_text(tx, domain, &key, &payload)
        })
    }

    // Delete for the current operation
    pub fn delete(&self, domain: Domain, key: &str) -> Result<(), String> {
        let key = key.to_owned();
        self.transaction(move |tx| records::delete(tx, domain, &key))
    }

    // Read and decode the selected JSON records
    pub fn list_json<T: DeserializeOwned>(
        &self,
        domain: Domain,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<Vec<(String, T)>, String> {
        self.run(move |conn| records::list_text(conn, domain, max_rows, max_bytes))?
            .into_iter()
            .map(|(key, text)| {
                serde_json::from_str(&text)
                    .map(|value| (key, value))
                    .map_err(|_| "Stored record has invalid domain data".into())
            })
            .collect()
    }
}

/// Exclusive configuration ownership, released explicitly after connections close
/// On Unix File locking uses flock: fork/dup descriptors share the open-file
/// description, so closing only our descriptor need not release the lock while
/// an unrelated child is between fork and exec. Explicit unlock ends this guard's
/// ownership even if such an inherited descriptor still exists. A subsequent
/// owner opens a different file description; later closing the stale duplicate
/// cannot unlock that new owner's lock (covered by the duplicate regression).
pub(crate) struct RootOwner(File);
impl Drop for RootOwner {
    // Release resources held by this owner
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

// Acquire ownership of the configuration root
pub(crate) fn own_root(config_root: &Path) -> Result<(PathBuf, RootOwner), String> {
    files::ensure_directory(config_root)?;
    let root = config_root.canonicalize().map_err(|e| e.to_string())?;
    files::ensure_directory(&root.join("profiles"))?;
    let lock_path = root.join("profiles/writer.lock");
    if lock_path
        .symlink_metadata()
        .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err("Configuration ownership file is not a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let owner = options.open(lock_path).map_err(|e| e.to_string())?;
    owner.try_lock().map_err(|_| "Another Stream Dock Manager process owns this configuration. Quit it before using the standalone CLI.".to_string())?;
    Ok((root, RootOwner(owner)))
}

#[cfg(test)]
mod tests;
