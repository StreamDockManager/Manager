//! The public SQLCipher salt identifies the installation credential. A clean install gets
//! a fresh identity even when the OS still holds credentials for a deleted installation.
pub(crate) mod recovery;
mod reset;
#[cfg(windows)]
mod windows;
pub(crate) use reset::{
    check_available as check_reset_available, is_reset_marker, pending_operation,
    reset as reset_for_recovery, KeystoreInitialization,
};
use rusqlite::Connection;
use std::{fs, io::Read, path::Path};
use zeroize::Zeroizing;

pub(crate) const NAME: &str = "datastore.sqlite3";
const RECOVER: &str = "Manager cannot open its plugin settings keystore. Restore the keystore and its installation key, or unlock the OS credential store and restart. Your data has been preserved.";
pub const KEY_FILE: &str = "STREAM_DOCK_MANAGER_INSTALLATION_KEY_FILE";

const SCHEMA: &str = "CREATE TABLE settings_vault.revisions (
    domain TEXT NOT NULL, record_key TEXT NOT NULL, pointer TEXT NOT NULL,
    id TEXT PRIMARY KEY, value TEXT NOT NULL
) STRICT;
    CREATE TABLE settings_vault.plugin_storage_epoch(singleton INTEGER PRIMARY KEY CHECK(singleton=1),epoch TEXT NOT NULL CHECK(length(epoch)=36)) STRICT;
    CREATE TABLE settings_vault.plugin_storage_generations(plugin_id TEXT PRIMARY KEY NOT NULL, generation INTEGER NOT NULL CHECK(generation>0)) STRICT;
    CREATE TABLE settings_vault.plugin_storage_namespaces(plugin_id TEXT NOT NULL,namespace TEXT NOT NULL,generation TEXT NOT NULL,revision BLOB NOT NULL CHECK(length(revision)=8 AND revision != X'0000000000000000'),PRIMARY KEY(plugin_id,namespace)) STRICT;
        CREATE TABLE settings_vault.plugin_storage_records(plugin_id TEXT NOT NULL,namespace TEXT NOT NULL,key TEXT NOT NULL,value TEXT NOT NULL CHECK(json_valid(value)),encoded_bytes INTEGER NOT NULL CHECK(encoded_bytes>=0),PRIMARY KEY(plugin_id,namespace,key),FOREIGN KEY(plugin_id,namespace) REFERENCES plugin_storage_namespaces(plugin_id,namespace) ON DELETE CASCADE) STRICT;
        CREATE TABLE settings_vault.plugin_storage_receipts(plugin_id TEXT NOT NULL,namespace TEXT NOT NULL,operation_id TEXT NOT NULL,digest TEXT NOT NULL,generation TEXT NOT NULL,revision BLOB NOT NULL CHECK(length(revision)=8 AND revision != X'0000000000000000'),created_ms INTEGER NOT NULL,PRIMARY KEY(plugin_id,namespace,operation_id),FOREIGN KEY(plugin_id,namespace) REFERENCES plugin_storage_namespaces(plugin_id,namespace) ON DELETE CASCADE) STRICT;
        CREATE INDEX settings_vault.plugin_storage_receipts_created ON plugin_storage_receipts(plugin_id,created_ms);";

fn salt(path: &Path) -> Result<String, String> {
    let mut bytes = [0; 16];
    fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| RECOVER)?;
    Ok(hex::encode(bytes))
}

fn account(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new("app.streamdockmanager.plugin-settings", id).map_err(|_| RECOVER.into())
}

fn file_key(path: &Path, root: &Path) -> Result<Zeroizing<String>, String> {
    const ERROR: &str = "The installation key file must be an absolute, owner-only regular file outside the Manager data directory, containing one 256-bit hexadecimal key.";
    if !path.is_absolute()
        || !crate::vault::owned_regular(path)?
        || path.canonicalize().map_err(|_| ERROR)?.starts_with(root)
    {
        return Err(ERROR.into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|_| ERROR)?;
    let meta = file.metadata().map_err(|_| ERROR)?;
    if !meta.is_file() || meta.len() > 66 {
        return Err(ERROR.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 || meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() }
        {
            return Err(ERROR.into());
        }
    }
    #[cfg(windows)]
    if !windows::private(&file) {
        return Err(ERROR.into());
    }
    {
        let mut key = Zeroizing::new(String::new());
        file.take(67).read_to_string(&mut key).map_err(|_| ERROR)?;
        let len = key.trim_end_matches(['\r', '\n']).len();
        key.truncate(len);
        if len != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ERROR.into());
        }
        Ok(key)
    }
}

pub(crate) fn check_backup_directory(directory: &Path) -> Result<(), String> {
    if let Some(key) = std::env::var_os(KEY_FILE) {
        let key = Path::new(&key)
            .canonicalize()
            .map_err(|_| "The installation key file is unavailable")?;
        if key.starts_with(directory.canonicalize().map_err(|e| e.to_string())?) {
            return Err(
                "Choose a backup directory that does not contain your installation key file."
                    .into(),
            );
        }
    }
    Ok(())
}

fn attach(db: &Connection, path: &Path, key: &str) -> Result<(), String> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RECOVER.into());
    }
    let version: String = db
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .map_err(|_| RECOVER)?;
    if !version.starts_with("4.") {
        return Err(RECOVER.into());
    }
    let raw = Zeroizing::new(format!("x'{key}'"));
    db.execute(
        "ATTACH DATABASE ?1 AS settings_vault KEY ?2",
        (path.to_str().ok_or(RECOVER)?, raw.as_str()),
    )
    .map_err(|_| RECOVER)?;
    db.execute_batch("PRAGMA temp_store=MEMORY; PRAGMA settings_vault.synchronous=FULL; PRAGMA cipher_memory_security=ON;").map_err(|_| RECOVER)?;
    let mode: String = db
        .query_row("PRAGMA settings_vault.journal_mode", [], |r| r.get(0))
        .map_err(|_| RECOVER)?;
    if mode != "delete" {
        return Err(RECOVER.into());
    }
    Ok(())
}
fn installation(db: &Connection) -> Result<String, String> {
    db.query_row(
        "SELECT value FROM database_metadata WHERE key='settings_installation'",
        [],
        |r| r.get(0),
    )
    .map_err(|_| RECOVER.into())
}
fn existing_in(
    db: &Connection,
    directory: &Path,
    root: &Path,
    expected: &str,
) -> Result<String, String> {
    let path = directory.join(NAME);
    if !crate::vault::owned_regular(&path)? {
        return Err(RECOVER.into());
    }
    let id = salt(&path)?;
    if !expected.is_empty() && expected != id {
        return Err(RECOVER.into());
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        if crate::vault::owned_regular(&directory.join(format!("{NAME}{suffix}")))?
            && suffix != "-journal"
        {
            return Err(RECOVER.into());
        }
    }
    attach_existing_path(db, &path, root, &id)?;
    Ok(id)
}

fn attach_existing_path(db: &Connection, path: &Path, root: &Path, id: &str) -> Result<(), String> {
    let key = match std::env::var_os(KEY_FILE) {
        Some(file) => file_key(Path::new(&file), root)?,
        None => Zeroizing::new(account(id)?.get_password().map_err(|_| RECOVER)?),
    };
    attach(db, path, &key)?;
    let fmt: i64 = db
        .query_row("PRAGMA settings_vault.application_id", [], |r| r.get(0))
        .map_err(|_| RECOVER)?;
    if fmt != 0x53444b32 {
        return Err(RECOVER.into());
    }
    validate_schema(db)
}
pub(crate) fn attach_existing_if_needed(db: &Connection, root: &Path) -> Result<(), String> {
    let id = installation(db)?;
    if !id.is_empty() {
        existing_in(db, &crate::database_directory(root), root, &id)?;
    }
    Ok(())
}

/// An explicit SQLite file may have its own flat, sibling encrypted backup.
pub(crate) fn attach_archive_if_needed(db: &Connection, directory: &Path) -> Result<(), String> {
    let id = installation(db)?;
    if !id.is_empty() {
        existing_in(db, directory, directory, &id)?;
    }
    Ok(())
}
// Attach an encrypted temporary datastore for a portable restore. The original
// stays untouched until Recovery has validated and fenced the replacement pair.
pub(crate) fn stage_transfer(
    db: &Connection,
    root: &Path,
) -> Result<tempfile::NamedTempFile, String> {
    recovery::no_sidecars(root)?;
    let live = crate::database_path(root, NAME);
    let mut stage =
        tempfile::NamedTempFile::new_in(crate::database_directory(root)).map_err(|_| RECOVER)?;
    if crate::vault::owned_regular(&live)? {
        recovery::copy_bounded(&live, stage.as_file_mut(), recovery::MAX_BYTES)?;
        let id = salt(stage.path())?;
        attach_existing_path(db, stage.path(), root, &id)?;
    } else {
        let file_key_path = std::env::var_os(KEY_FILE);
        let key = match &file_key_path {
            Some(path) => file_key(Path::new(path), root)?,
            None => Zeroizing::new(hex::encode(crate::vault::data_key()?.as_slice())),
        };
        attach(db, stage.path(), &key)?;
        db.execute_batch("PRAGMA settings_vault.application_id=0x53444b32")
            .map_err(|_| RECOVER)?;
        initialize_schema(db)?;
        if file_key_path.is_none() {
            account(&salt(stage.path())?)?
                .set_password(&key)
                .map_err(|_| RECOVER)?;
        }
    }
    db.execute(
        "UPDATE database_metadata SET value=?1 WHERE key='settings_installation'",
        [salt(stage.path())?],
    )
    .map_err(|_| RECOVER)?;
    Ok(stage)
}

pub(crate) fn attach_live(db: &Connection, root: &Path) -> Result<(), String> {
    let expected = installation(db)?;
    let path = crate::database_path(root, NAME);
    if expected.is_empty() && !crate::vault::owned_regular(&path)? {
        for suffix in ["-wal", "-shm", "-journal"] {
            if crate::vault::owned_regular(&crate::database_path(root, &format!("{NAME}{suffix}")))?
            {
                return Err(RECOVER.into());
            }
        }
        let file_key_path = std::env::var_os(KEY_FILE);
        let key = match &file_key_path {
            Some(path) => file_key(Path::new(path), root)?,
            None => Zeroizing::new(hex::encode(crate::vault::data_key()?.as_slice())),
        };
        let stage = tempfile::NamedTempFile::new_in(crate::database_directory(root))
            .map_err(|_| RECOVER)?;
        let setup = Connection::open_in_memory().map_err(|_| RECOVER)?;
        attach(&setup, stage.path(), &key)?;
        setup
            .execute_batch("PRAGMA settings_vault.application_id=0x53444b32")
            .map_err(|_| RECOVER)?;
        initialize_schema(&setup)?;
        drop(setup);
        stage.as_file().sync_all().map_err(|_| RECOVER)?;
        if file_key_path.is_none() {
            account(&salt(stage.path())?)?
                .set_password(&key)
                .map_err(|_| RECOVER)?;
        }
        stage.persist_noclobber(&path).map_err(|_| RECOVER)?;
        crate::sync_directory(&crate::database_directory(root))?;
    }
    let id = existing_in(db, &crate::database_directory(root), root, &expected)?;
    if expected.is_empty() {
        db.execute(
            "UPDATE database_metadata SET value=?1 WHERE key='settings_installation'",
            [id],
        )
        .map_err(|_| RECOVER)?;
    }
    Ok(())
}
/// Check whether a validated Manager connection includes its installation datastore
pub(crate) fn attached(db: &Connection) -> bool {
    db.is_readonly("settings_vault").is_ok()
}

pub(crate) fn validate_schema(db: &Connection) -> Result<(), String> {
    fn rows(db: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
        let mut statement = db.prepare(
            "SELECT type,name,sql FROM settings_vault.sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name",
        )?;
        let records = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        records.collect()
    }
    let expected = Connection::open_in_memory().map_err(|_| RECOVER)?;
    attach_empty(&expected)?;
    let epoch: String = db
        .query_row(
            "SELECT epoch FROM settings_vault.plugin_storage_epoch WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(|_| RECOVER)?;
    uuid::Uuid::parse_str(&epoch).map_err(|_| RECOVER)?;
    if rows(db).map_err(|_| RECOVER)? != rows(&expected).map_err(|_| RECOVER)? {
        return Err(RECOVER.into());
    }
    Ok(())
}

pub(crate) fn initialize_schema(db: &Connection) -> Result<(), String> {
    db.execute_batch(SCHEMA).map_err(|_| RECOVER)?;
    db.execute(
        "INSERT INTO settings_vault.plugin_storage_epoch VALUES(1,?1)",
        [uuid::Uuid::new_v4().to_string()],
    )
    .map_err(|_| RECOVER)?;
    Ok(())
}
pub(crate) fn rotate_storage_epoch(db: &Connection) -> Result<(), String> {
    if db
        .execute(
            "UPDATE settings_vault.plugin_storage_epoch SET epoch=?1 WHERE singleton=1",
            [uuid::Uuid::new_v4().to_string()],
        )
        .map_err(|_| RECOVER)?
        != 1
    {
        return Err(RECOVER.into());
    }
    db.execute("DELETE FROM settings_vault.plugin_storage_receipts", [])
        .map_err(|_| RECOVER)?;
    Ok(())
}

pub(crate) fn attach_empty(copy: &Connection) -> Result<(), String> {
    copy.execute_batch("PRAGMA temp_store=MEMORY; ATTACH ':memory:' AS settings_vault KEY ''")
        .map_err(|e| e.to_string())?;
    initialize_schema(copy)?;
    Ok(())
}
pub(crate) fn attach_preview(copy: &Connection, source: &Connection) -> Result<(), String> {
    attach_empty(copy)?;
    if attached(source) {
        crate::settings_backup::copy_sdk_for_preview(copy, source)?;
        let mut q = source
            .prepare("SELECT domain,record_key,pointer,id,value FROM settings_vault.revisions")
            .map_err(|e| e.to_string())?;
        let mut rows = q.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let values = (0..5)
                .map(|i| row.get::<_, String>(i))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            copy.execute(
                "INSERT INTO settings_vault.revisions VALUES(?1,?2,?3,?4,?5)",
                rusqlite::params_from_iter(values),
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn explicit_key_rejects_shared_files_links_and_keys_inside_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("manager");
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let key = dir.path().join("owner-key");
        fs::write(&key, "a".repeat(64)).unwrap();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(file_key(&key, &root).is_ok());
        let link = dir.path().join("link");
        symlink(&key, &link).unwrap();
        assert!(file_key(&link, &root).is_err());
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(file_key(&key, &root).is_err());
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
        let inside = root.join("key");
        fs::rename(&key, &inside).unwrap();
        assert!(file_key(&inside, &root).is_err());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn windows_key_file_requires_a_private_acl() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("data");
        fs::create_dir(&root).unwrap();
        let key = fixture.path().join("test.key");
        fs::write(&key, "a".repeat(64)).unwrap();
        let user = Command::new("whoami")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()
            .unwrap();
        assert!(user.status.success());
        let output = String::from_utf8_lossy(&user.stdout);
        let sid = output.trim().rsplit(',').next().unwrap().trim_matches('"');
        assert!(sid.starts_with("S-1-"));
        let acl = |args: &[&str]| {
            let result = Command::new("icacls")
                .arg(&key)
                .args(args)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "icacls failed: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        acl(&["/setowner", &format!("*{sid}")]);
        acl(&["/inheritance:r", "/grant:r", &format!("*{sid}:F")]);
        let root = root.canonicalize().unwrap();
        assert!(file_key(&key, &root).is_ok());
        acl(&["/grant", "*S-1-1-0:R"]);
        assert!(file_key(&key, &root).is_err());
    }
}
