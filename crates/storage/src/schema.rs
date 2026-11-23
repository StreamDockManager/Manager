use crate::records::Domain;
use rusqlite::{Connection, OpenFlags};
use std::{fs, path::Path, time::Duration};

pub const SCHEMA_VERSION: i64 = 3;
pub const APPLICATION_ID: i64 = 0x53444d31;
pub const DATABASE_NAME: &str = "manager.sqlite3";
type SchemaObject = (String, String, String, Option<String>);
pub(crate) const RECOVERY_PENDING: &str = ".stream-dock-manager-recovery-pending";

// Open for the current operation
pub(crate) fn open(root: &Path) -> Result<Connection, String> {
    open_validated(root, &|_| Ok(()), false)
}

// Open validated
pub(crate) fn open_validated(
    root: &Path,
    validator: &dyn Fn(&Connection) -> Result<(), String>,
    staging: bool,
) -> Result<Connection, String> {
    let path = crate::database_path(root, DATABASE_NAME);
    match path.symlink_metadata() {
        Ok(metadata) => {
            if root.join(RECOVERY_PENDING).symlink_metadata().is_ok() {
                return Err("An explicit database recovery did not finish. Data and evidence are preserved; repeat the explicit restore before starting Manager.".into());
            }
            if !metadata.is_file() || metadata.len() == 0 {
                return Err("Existing configuration database is not a valid regular database file; it has been preserved".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Missing primary means a fresh database, not implicit backup restore or
            // legacy import. Never let SQLite consume orphan sidecars as new data.
            quarantine_orphans(root)?;
            create(&crate::database_directory(root), &path)?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let expected = preflight(root, validator)?;
    if fingerprint(&path)? != expected {
        return Err("Configuration changed during validation; retry after closing other writers. Originals preserved.".into());
    }
    let conn = connect(&path)?;
    validate(&conn)?;
    if staging {
        crate::installation_key::attach_empty(&conn)?;
    } else {
        crate::installation_key::attach_live(&conn, root)?;
    }
    validator(&conn)?;
    configure(&conn)?;
    validate(&conn)?;
    Ok(conn)
}

/// Preserve orphan files before publishing a fresh primary. A retained temporary
/// directory reserves a unique name atomically; no destination can be clobbered.
/// Keep it even on failure so interrupted moves are never deleted by RAII cleanup
fn quarantine_orphans(root: &Path) -> Result<(), String> {
    let names = [
        "manager.sqlite3-wal",
        "manager.sqlite3-shm",
        "manager.sqlite3-journal",
        RECOVERY_PENDING,
    ];
    let mut present = Vec::new();
    for name in names {
        match live_evidence_path(root, name).symlink_metadata() {
            Ok(_) => present.push(name),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    if present.is_empty() {
        return Ok(());
    }
    // Root is already canonicalized and exclusively owned. Keep quarantine here
    // rather than trusting a possibly symlinked or inaccessible recovery folder.
    let preserved = tempfile::Builder::new()
        .prefix("missing-primary-preserved-")
        .tempdir_in(root)
        .map_err(|error| error.to_string())?
        .keep();
    crate::files::sync_directory(root)?;
    for name in present {
        // Rename preserves bytes and symlinks without following their targets
        fs::rename(live_evidence_path(root, name), preserved.join(name)).map_err(|error| {
            format!(
                "Could not preserve orphan database evidence in {}: {error}",
                preserved.display()
            )
        })?;
        crate::files::sync_directory(&preserved)?;
        crate::files::sync_directory(&crate::database_directory(root))?;
        crate::files::sync_directory(root)?;
    }
    Ok(())
}

pub(crate) fn live_evidence_path(root: &Path, name: &str) -> std::path::PathBuf {
    if name == RECOVERY_PENDING {
        root.join(name)
    } else {
        crate::database_path(root, name)
    }
}

/// Recover a disposable copy including sidecars; never recover unsupported originals
fn preflight(
    root: &Path,
    validator: &dyn Fn(&Connection) -> Result<(), String>,
) -> Result<Fingerprint, String> {
    let (copy, fingerprint) = live_snapshot_copy_checked(root)?;
    validator(&copy)
        .map_err(|error| format!("Unsupported configuration; originals preserved: {error}"))?;
    Ok(fingerprint)
}

// Keep the private directory alive until SQLite releases its database and sidecars.
pub(crate) struct ScratchDatabase {
    pub connection: Connection,
    pub directory: tempfile::TempDir,
}
impl ScratchDatabase {
    pub fn new() -> Result<Self, String> {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let connection =
            Connection::open(directory.path().join(DATABASE_NAME)).map_err(|e| e.to_string())?;
        Ok(Self {
            connection,
            directory,
        })
    }
}
impl std::ops::Deref for ScratchDatabase {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}
impl std::ops::DerefMut for ScratchDatabase {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

// Create a consistent SQLite snapshot
pub(crate) fn snapshot_copy(path: &Path) -> Result<ScratchDatabase, String> {
    let (copy, _) = snapshot_copy_unattached(path)?;
    crate::installation_key::attach_archive_if_needed(
        &copy,
        path.parent().ok_or("Missing data directory")?,
    )?;
    Ok(copy)
}

pub(crate) fn live_snapshot_copy(root: &Path) -> Result<ScratchDatabase, String> {
    live_snapshot_copy_checked(root).map(|(copy, _)| copy)
}

fn live_snapshot_copy_checked(root: &Path) -> Result<(ScratchDatabase, Fingerprint), String> {
    let (copy, before) = snapshot_copy_unattached(&crate::database_path(root, DATABASE_NAME))?;
    crate::installation_key::attach_existing_if_needed(&copy, root)?;
    Ok((copy, before))
}

// Copy and validate the main database without attaching or opening live secrets.
pub(crate) fn snapshot_copy_unattached(
    path: &Path,
) -> Result<(ScratchDatabase, Fingerprint), String> {
    let before = fingerprint(path)?;
    let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Snapshot is not a regular file".into());
    }
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    fs::copy(path, scratch.path().join(DATABASE_NAME)).map_err(|e| e.to_string())?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let source = std::path::PathBuf::from(sidecar);
        match source.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => {
                fs::copy(
                    &source,
                    scratch.path().join(format!("{DATABASE_NAME}{suffix}")),
                )
                .map_err(|e| e.to_string())?;
            }
            Ok(_) => return Err("Unsupported database sidecar; originals preserved".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    if fingerprint(path)? != before {
        return Err("Configuration changed while copying recovery evidence; retry after closing other writers".into());
    }
    let copy = connect(&scratch.path().join(DATABASE_NAME))?;
    validate(&copy)?;
    Ok((
        ScratchDatabase {
            connection: copy,
            directory: scratch,
        },
        before,
    ))
}

pub(crate) type Fingerprint = Vec<Option<[u8; 32]>>;
/// The root lock excludes cooperating Manager writers. Fingerprints also detect
/// outside edits while copying/validating. They cannot exclude a hostile writer
/// racing the final check and SQLite open; callers must close external writers.
pub(crate) fn fingerprint(path: &Path) -> Result<Fingerprint, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut result = Vec::with_capacity(4);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let source = std::path::PathBuf::from(name);
        match source.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => {
                let mut file = fs::File::open(&source).map_err(|e| e.to_string())?;
                let mut digest = Sha256::new();
                let mut buffer = [0u8; 65536];
                loop {
                    let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
                    if size == 0 {
                        break;
                    }
                    digest.update(&buffer[..size]);
                }
                result.push(Some(digest.finalize().into()));
            }
            Ok(_) => {
                return Err("Database evidence is not a regular file; originals preserved".into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => result.push(None),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(result)
}

// Connect for the current operation
pub(crate) fn connect(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("Cannot open configuration database; data preserved: {e}"))?;
    conn.busy_timeout(Duration::from_millis(1500))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

// Create for the current operation
fn create(root: &Path, path: &Path) -> Result<(), String> {
    // Initialize away from the primary path so an interrupted first run can retry
    let temporary = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    let mut conn = connect(temporary.path())?;
    configure(&conn)?;
    initialize(&mut conn)?;
    validate(&conn)?;
    conn.close().map_err(|(_, error)| error.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist_noclobber(path)
        .map_err(|e| e.to_string())?;
    crate::files::sync_directory(root)
}

// Configure for the current operation
pub(crate) fn configure(conn: &Connection) -> Result<(), String> {
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| e.to_string())?;
    let journal: String = conn
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if journal != "delete" {
        return Err("Could not enable SQLite rollback journaling".into());
    }
    conn.pragma_update(None, "synchronous", "EXTRA")
        .map_err(|e| e.to_string())?;
    let fk: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let sync: i64 = conn
        .query_row("PRAGMA synchronous", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if fk != 1 || sync != 3 {
        return Err("SQLite connection policy was not applied".into());
    }
    conn.set_limit(
        rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH,
        (crate::MAX_ADMITTED_BYTES + 4096) as i32,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// Initialize for the current operation
fn initialize(conn: &mut Connection) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute_batch("CREATE TABLE database_metadata(key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL) STRICT;").map_err(|e|e.to_string())?;
    tx.execute(
        "INSERT INTO database_metadata(key,value) VALUES('storage_generation',?1)",
        [uuid::Uuid::new_v4().to_string()],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO database_metadata(key,value) VALUES('commit_sequence','0')",
        [],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO database_metadata VALUES('contract','settings-v3'),('settings_installation','')",
        [],
    )
    .map_err(|e| e.to_string())?;
    for domain in Domain::ALL {
        tx.execute_batch(&format!("CREATE TABLE {}(key TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(key AS BLOB)) BETWEEN 1 AND 1024),payload TEXT NOT NULL CHECK(json_valid(payload) AND length(CAST(payload AS BLOB)) <= {}),revision INTEGER NOT NULL CHECK(revision>0)) STRICT;",domain.table(),crate::records::MAX_RECORD_BYTES)).map_err(|e|e.to_string())?;
    }
    crate::plugin_settings::initialize(&tx)?;
    crate::plugin_health::create(&tx)?;
    crate::plugin_installations::initialize(&tx)?;
    tx.pragma_update(None, "application_id", APPLICATION_ID)
        .map_err(|e| e.to_string())?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

// Validate for the current operation
pub fn validate(conn: &Connection) -> Result<(), String> {
    let app: i64 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|e| format!("Configuration database is unreadable; data preserved: {e}"))?;
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if app != APPLICATION_ID {
        return Err("Configuration database belongs to a different application or has not completed initialization; data preserved".into());
    }
    if version != SCHEMA_VERSION {
        return Err(format!(
            "Unsupported configuration database schema {version}; expected {SCHEMA_VERSION}. Data preserved."
        ));
    }
    let mut expected = Connection::open_in_memory().map_err(|e| e.to_string())?;
    initialize(&mut expected)?;
    if crate::permissions::installed(conn)? {
        crate::permissions::migrate(&expected)?;
    }
    // Read the database schema entries
    fn schema_rows(connection: &Connection) -> Result<Vec<SchemaObject>, String> {
        let mut statement = connection
            .prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
    if schema_rows(conn)? != schema_rows(&expected)? {
        return Err("Unsupported configuration database schema; original preserved".into());
    }
    let total: i64 = conn
        .query_row("SELECT count(*) FROM database_metadata", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let contract: Option<String> = conn
        .query_row(
            "SELECT value FROM database_metadata WHERE key='contract'",
            [],
            |r| r.get(0),
        )
        .ok();
    if total != 4 || contract.as_deref() != Some("settings-v3") {
        return Err("Unsupported configuration database contract; original preserved".into());
    }
    let integrity: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if integrity != "ok" {
        return Err("Configuration database integrity validation failed; data preserved".into());
    }
    let mut fk = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(|e| e.to_string())?;
    if fk
        .query([])
        .map_err(|e| e.to_string())?
        .next()
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err(
            "Configuration database has invalid foreign-key references; data preserved".into(),
        );
    }
    crate::plugin_health::validate(conn)?;
    for name in Domain::ALL
        .into_iter()
        .map(Domain::table)
        .chain(["database_metadata"])
    {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [name],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !exists {
            return Err(format!(
                "Configuration database is missing required table {name}; data preserved"
            ));
        }
    }
    persisted_generation(conn)?;
    let revision: String = conn
        .query_row(
            "SELECT value FROM database_metadata WHERE key='commit_sequence'",
            [],
            |row| row.get(0),
        )
        .map_err(|_| "Configuration database commit sequence is missing".to_string())?;
    if revision.parse::<i64>().is_err() || revision.starts_with('-') {
        return Err("Configuration database commit sequence is invalid".into());
    }
    Ok(())
}

/// The durable restore/reset identity of this database, read on the caller's connection.
pub fn persisted_generation(conn: &Connection) -> Result<String, String> {
    let generation: String = conn
        .query_row(
            "SELECT value FROM database_metadata WHERE key='storage_generation'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "Configuration database generation is missing".to_string())?;
    uuid::Uuid::parse_str(&generation)
        .map_err(|_| "Configuration database generation is invalid".to_string())?;
    Ok(generation)
}
