use crate::records::Domain;
use rusqlite::{Connection, OpenFlags};
use std::{fs, path::Path, time::Duration};

pub const SCHEMA_VERSION: i64 = 1;
pub const APPLICATION_ID: i64 = 0x53444d31;
pub const DATABASE_NAME: &str = "manager.sqlite3";
pub(crate) const INITIALIZED_MARKER: &str = ".stream-dock-manager-sqlite-initialized";
type SchemaObject = (String, String, String, Option<String>);
pub(crate) const RECOVERY_PENDING: &str = ".stream-dock-manager-recovery-pending";

// Open for the current operation
pub(crate) fn open(root: &Path) -> Result<Connection, String> {
    open_validated(root, &|_| Ok(()))
}

// Open validated
pub(crate) fn open_validated(
    root: &Path,
    validator: &dyn Fn(&Connection) -> Result<(), String>,
) -> Result<Connection, String> {
    let path = root.join(DATABASE_NAME);
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
            create(root, &path)?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let expected = preflight(&path, validator)?;
    if fingerprint(&path)? != expected {
        return Err("Configuration changed during validation; retry after closing other writers. Originals preserved.".into());
    }
    let conn = connect(&path)?;
    validate(&conn)?;
    validator(&conn)?;
    configure(&conn)?;
    validate(&conn)?;
    mark_initialized(root)?;
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
        match root.join(name).symlink_metadata() {
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
        fs::rename(root.join(name), preserved.join(name)).map_err(|error| {
            format!(
                "Could not preserve orphan database evidence in {}: {error}",
                preserved.display()
            )
        })?;
        crate::files::sync_directory(&preserved)?;
        crate::files::sync_directory(root)?;
    }
    Ok(())
}

/// Recover a disposable copy including sidecars; never recover unsupported originals
fn preflight(
    path: &Path,
    validator: &dyn Fn(&Connection) -> Result<(), String>,
) -> Result<Fingerprint, String> {
    let (copy, fingerprint) = snapshot_copy_checked(path)?;
    validator(&copy)
        .map_err(|error| format!("Unsupported configuration; originals preserved: {error}"))?;
    Ok(fingerprint)
}

// Create a consistent SQLite snapshot
pub(crate) fn snapshot_copy(path: &Path) -> Result<Connection, String> {
    snapshot_copy_checked(path).map(|(connection, _)| connection)
}

// Create a snapshot and validate the resulting database
pub(crate) fn snapshot_copy_checked(path: &Path) -> Result<(Connection, Fingerprint), String> {
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
    // SQLite owns this temporary database and removes it when the connection closes
    // Unlike an in-memory copy this does not retain an entire large workspace in RAM
    let mut recovered = Connection::open("").map_err(|e| e.to_string())?;
    crate::maintenance::copy_database(&copy, &mut recovered)?;
    Ok((recovered, before))
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
                return Err("Database evidence is not a regular file; originals preserved".into())
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

// Mark initialized
pub(crate) fn mark_initialized(root: &Path) -> Result<(), String> {
    let marker = root.join(INITIALIZED_MARKER);
    if !marker.exists() {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(marker).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        crate::files::sync_directory(root)?;
    }
    Ok(())
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
        "INSERT INTO database_metadata VALUES('contract','current-v1')",
        [],
    )
    .map_err(|e| e.to_string())?;
    for domain in Domain::ALL {
        tx.execute_batch(&format!("CREATE TABLE {}(key TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(key AS BLOB)) BETWEEN 1 AND 1024),payload TEXT NOT NULL CHECK(json_valid(payload) AND length(CAST(payload AS BLOB)) <= {}),revision INTEGER NOT NULL CHECK(revision>0)) STRICT;",domain.table(),crate::records::MAX_RECORD_BYTES)).map_err(|e|e.to_string())?;
    }
    tx.execute_batch("CREATE TABLE plugin_storage_namespaces(plugin_id TEXT NOT NULL,namespace TEXT NOT NULL,generation TEXT NOT NULL,revision TEXT NOT NULL,PRIMARY KEY(plugin_id,namespace)) STRICT;
        CREATE TABLE plugin_storage_records(plugin_id TEXT NOT NULL,namespace TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL CHECK(json_valid(value)),encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes>=0),PRIMARY KEY(plugin_id,namespace,key),FOREIGN KEY(plugin_id,namespace) REFERENCES plugin_storage_namespaces(plugin_id,namespace) ON DELETE CASCADE) STRICT;
        CREATE TABLE plugin_storage_receipts(plugin_id TEXT NOT NULL,namespace TEXT NOT NULL,operation_id TEXT NOT NULL,digest TEXT NOT NULL,generation TEXT NOT NULL,revision TEXT NOT NULL,result TEXT NOT NULL CHECK(json_valid(result)),created_ms INTEGER NOT NULL,PRIMARY KEY(plugin_id,namespace,operation_id),FOREIGN KEY(plugin_id,namespace) REFERENCES plugin_storage_namespaces(plugin_id,namespace) ON DELETE CASCADE) STRICT;
        CREATE INDEX plugin_storage_receipts_created ON plugin_storage_receipts(plugin_id,created_ms);").map_err(|e|e.to_string())?;
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
        return Err(format!("Unsupported configuration database schema {version}; expected {SCHEMA_VERSION}. Data preserved."));
    }
    let mut expected = Connection::open_in_memory().map_err(|e| e.to_string())?;
    initialize(&mut expected)?;
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
    if total != 3 || contract.as_deref() != Some("current-v1") {
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
    for name in Domain::ALL.into_iter().map(Domain::table).chain([
        "database_metadata",
        "plugin_storage_namespaces",
        "plugin_storage_records",
        "plugin_storage_receipts",
    ]) {
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
    let generation: String = conn
        .query_row(
            "SELECT value FROM database_metadata WHERE key='storage_generation'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "Configuration database generation is missing".to_string())?;
    uuid::Uuid::parse_str(&generation)
        .map_err(|_| "Configuration database generation is invalid".to_string())?;
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
