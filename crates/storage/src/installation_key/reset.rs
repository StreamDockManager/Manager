//! Explicit offline installation keystore initialization (Create or Reset). The existing recovery fence makes the two-file
//! publication restartable. Originals and staged replacements are never renamed
//! out of evidence storage, even if directory synchronization fails.
use super::{account, attach, initialize_schema, installation, salt, KEY_FILE, NAME, RECOVER};
use crate::{files, maintenance, schema};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const RESET_KIND: &str = "installation-keystore-reset";
const CREATE_KIND: &str = "installation-keystore-create";
const CREATE_REQUIRES_MISSING: &str = "Create is only available when the installation keystore and all of its sidecars are missing. Existing evidence was preserved.";
const SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];
const FENCED: &str = "Installation keystore recovery has not completed. Original files are preserved; retry the reviewed operation before starting Manager.";
const KEY_ACCESS: &str = "The OS installation credential store is locked or unavailable. Unlock it and retry; no installation credentials were deleted.";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum KeystoreInitialization {
    #[serde(rename = "installation-keystore-reset")]
    Reset,
    #[serde(rename = "installation-keystore-create")]
    Create,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    kind: KeystoreInitialization,
    id: uuid::Uuid,
    old_primary: schema::Fingerprint,
    old_keystore: schema::Fingerprint,
    new_primary: schema::Fingerprint,
    new_keystore: schema::Fingerprint,
}

trait Credentials {
    fn read(&self, id: &str) -> Result<Option<Zeroizing<String>>, String>;
    fn create(&self, id: &str, key: &str) -> Result<(), String>;
}

struct OsCredentials;
impl Credentials for OsCredentials {
    fn read(&self, id: &str) -> Result<Option<Zeroizing<String>>, String> {
        match account(id)?.get_password() {
            Ok(key) => Ok(Some(Zeroizing::new(key))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(KEY_ACCESS.into()),
        }
    }

    fn create(&self, id: &str, key: &str) -> Result<(), String> {
        if self.read(id)?.is_some() {
            return Err(
                "New installation credential already exists; reset was not published.".into(),
            );
        }
        account(id)?
            .set_password(key)
            .map_err(|_| KEY_ACCESS.into())
    }
}

fn reject_development_key() -> Result<(), String> {
    if std::env::var_os(KEY_FILE).is_some() {
        return Err("Remove the explicit installation key file setting before resetting the OS installation keystore. The key file was not changed.".into());
    }
    Ok(())
}

/// Read-only review: SQLite sees only disposable copies; no credential is written.
pub(crate) fn check_available(root: &Path, mode: KeystoreInitialization) -> Result<(), String> {
    reject_development_key()?;
    check_using(root, &OsCredentials, mode)
}

fn check_using(
    root: &Path,
    credentials: &impl Credentials,
    mode: KeystoreInitialization,
) -> Result<(), String> {
    if let Some(intent) = load_intent(&root.join(schema::RECOVERY_PENDING))? {
        require_matching_mode(mode, intent.kind)?;
        let directory = evidence(root, intent.id);
        check_evidence(&directory, &intent)?;
        check_live(root, &intent)?;
        return check_staged(&directory.join("staged"), credentials, &intent);
    }
    let databases = crate::database_directory(root);
    ensure_regular_paths(&databases, schema::DATABASE_NAME)?;
    ensure_regular_paths(&databases, NAME)?;
    if mode == KeystoreInitialization::Create {
        for suffix in SUFFIXES {
            if crate::vault::owned_regular(&databases.join(format!("{NAME}{suffix}")))? {
                return Err(CREATE_REQUIRES_MISSING.into());
            }
        }
    }
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    copy_primary(&databases, scratch.path())?;
    let main = schema::connect(&scratch.path().join(schema::DATABASE_NAME))?;
    schema::validate(&main)?;
    probe_previous(&databases, &installation(&main)?, credentials)
}

/// The recovery editor must retain exclusive ownership of the canonical root.
pub(crate) fn reset(
    root: &Path,
    mode: KeystoreInitialization,
    recheck_review: impl Fn() -> Result<(), String>,
) -> Result<PathBuf, String> {
    reject_development_key()?;
    reset_using(root, &OsCredentials, mode, &recheck_review)
}

fn read_marker(marker: &Path) -> Result<Option<Vec<u8>>, String> {
    if !crate::vault::owned_regular(marker)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    fs::File::open(marker)
        .and_then(|file| file.take(16385).read_to_end(&mut bytes))
        .map_err(|_| FENCED)?;
    if bytes.len() > 16384 {
        return Err(FENCED.into());
    }
    Ok(Some(bytes))
}

fn marker_identifies_initialization(bytes: &[u8]) -> bool {
    [RESET_KIND, CREATE_KIND].iter().any(|kind| {
        bytes
            .windows(kind.len())
            .any(|window| window == kind.as_bytes())
    })
}

/// Ordinary restore cannot replace a two-file keystore initialization fence.
/// Also recognize a distinguishable but damaged or future-version marker.
pub(crate) fn is_reset_marker(root: &Path) -> Result<bool, String> {
    Ok(read_marker(&root.join(schema::RECOVERY_PENDING))?
        .is_some_and(|bytes| marker_identifies_initialization(&bytes)))
}

/// Return the reviewed operation that must be used to resume; other recovery
/// markers are not a keystore initialization.
pub(crate) fn pending_operation(root: &Path) -> Result<Option<KeystoreInitialization>, String> {
    let marker = root.join(schema::RECOVERY_PENDING);
    let Some(bytes) = read_marker(&marker)? else {
        return Ok(None);
    };
    if !marker_identifies_initialization(&bytes) {
        return Ok(None);
    }
    Ok(Some(decode_intent(&bytes)?.kind))
}

fn require_matching_mode(
    requested: KeystoreInitialization,
    pending: KeystoreInitialization,
) -> Result<(), String> {
    if requested != pending {
        return Err("A different installation keystore operation is unfinished. Resume that reviewed operation first.".into());
    }
    Ok(())
}

fn require_available(
    mode: KeystoreInitialization,
    previous: &schema::Fingerprint,
) -> Result<(), String> {
    if mode == KeystoreInitialization::Create && previous.iter().any(Option::is_some) {
        return Err(CREATE_REQUIRES_MISSING.into());
    }
    Ok(())
}

fn load_intent(marker: &Path) -> Result<Option<Intent>, String> {
    let Some(bytes) = read_marker(marker)? else {
        return Ok(None);
    };
    decode_intent(&bytes).map(Some)
}

fn decode_intent(bytes: &[u8]) -> Result<Intent, String> {
    let intent: Intent = serde_json::from_slice(bytes)
        .map_err(|_| "A different database recovery remains unfinished; resolve it before changing the installation keystore.")?;
    if intent.version != 1 {
        return Err(FENCED.into());
    }
    require_available(intent.kind, &intent.old_keystore)?;
    Ok(intent)
}

fn reset_using(
    root: &Path,
    credentials: &impl Credentials,
    mode: KeystoreInitialization,
    recheck_review: &impl Fn() -> Result<(), String>,
) -> Result<PathBuf, String> {
    let marker = root.join(schema::RECOVERY_PENDING);
    let existing = load_intent(&marker)?;
    let resuming = existing.is_some();
    let intent = match existing {
        Some(intent) => {
            require_matching_mode(mode, intent.kind)?;
            intent
        }
        None => prepare(root, credentials, &marker, mode, recheck_review)?,
    };
    let directory = evidence(root, intent.id);
    publish(root, credentials, &marker, &directory, &intent, || {
        // A new operation wrote its own marker after the final review check;
        // a resume must still match the marker the user reviewed.
        if resuming {
            recheck_review()
        } else {
            Ok(())
        }
    })?;
    Ok(directory.join("originals"))
}

fn copy_primary(source: &Path, target: &Path) -> Result<(), String> {
    for suffix in SUFFIXES {
        let filename = format!("{}{suffix}", schema::DATABASE_NAME);
        let file = source.join(&filename);
        if crate::vault::owned_regular(&file)? {
            copy_new(&file, &target.join(filename))?;
        }
    }
    Ok(())
}

fn probe_previous(
    root: &Path,
    previous: &str,
    credentials: &impl Credentials,
) -> Result<(), String> {
    let probe = if !previous.is_empty() {
        if previous.len() != 32 || !previous.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RECOVER.into());
        }
        previous.to_owned()
    } else if crate::vault::owned_regular(&root.join(NAME))? {
        salt(&root.join(NAME)).unwrap_or_else(|_| uuid::Uuid::new_v4().simple().to_string())
    } else {
        uuid::Uuid::new_v4().simple().to_string()
    };
    // A negative lookup is permitted only when the OS reported NoEntry.
    // A fresh credential must also be successfully persisted and reread before fencing.
    let _ = credentials.read(&probe)?;
    Ok(())
}

// Recovery backups are automatically pruned; this explicitly preserved evidence
// is a separate, owner-only directory and must not be subject to that pruning.
fn evidence(root: &Path, id: uuid::Uuid) -> PathBuf {
    root.join(format!("keystore-reset-preserved-{id}"))
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path).map_err(|e| e.to_string())
}

fn require_directory(path: &Path) -> Result<(), String> {
    let meta = path.symlink_metadata().map_err(|_| FENCED)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(FENCED.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
            return Err(FENCED.into());
        }
    }
    Ok(())
}

fn ensure_regular_paths(root: &Path, name: &str) -> Result<(), String> {
    for suffix in SUFFIXES {
        crate::vault::owned_regular(&root.join(format!("{name}{suffix}")))?;
    }
    Ok(())
}

fn prepare(
    root: &Path,
    credentials: &impl Credentials,
    marker: &Path,
    mode: KeystoreInitialization,
    recheck_review: &impl Fn() -> Result<(), String>,
) -> Result<Intent, String> {
    let primary = schema::DATABASE_NAME;
    let databases = crate::database_directory(root);
    ensure_regular_paths(&databases, primary)?;
    ensure_regular_paths(&databases, NAME)?;
    let old_primary = schema::fingerprint(&databases.join(primary))?;
    let old_keystore = schema::fingerprint(&databases.join(NAME))?;
    require_available(mode, &old_keystore)?;
    if old_primary[0].is_none() {
        return Err(
            "No regular primary configuration database is available; originals preserved.".into(),
        );
    }
    let id = uuid::Uuid::new_v4();
    let directory = evidence(root, id);
    let originals = directory.join("originals");
    let staged = directory.join("staged");
    create_private_directory(&directory)?;
    create_private_directory(&originals)?;
    create_private_directory(&staged)?;

    for name in [primary, NAME] {
        for suffix in SUFFIXES {
            let name = format!("{name}{suffix}");
            let source = databases.join(&name);
            if crate::vault::owned_regular(&source)? {
                copy_new(&source, &originals.join(name))?;
            }
        }
    }
    if schema::fingerprint(&originals.join(primary))? != old_primary
        || schema::fingerprint(&originals.join(NAME))? != old_keystore
    {
        return Err(
            "Configuration changed while preserving the originals; reset not published.".into(),
        );
    }
    files::sync_directory(&originals)?;
    files::sync_directory(&directory)?;
    files::sync_directory(root)?;

    // SQLite may recover an input journal; it only sees a separate scratch copy.
    let scratch = tempfile::tempdir_in(root).map_err(|e| e.to_string())?;
    copy_primary(&originals, scratch.path())?;
    let main = schema::connect(&scratch.path().join(primary))?;
    schema::validate(&main)?;
    let previous = installation(&main)?;
    probe_previous(&originals, &previous, credentials)?;
    let stage_primary = staged.join(primary);
    let stage_key = staged.join(NAME);
    create_new(&stage_primary)?;
    create_new(&stage_key)?;
    let mut next = schema::connect(&stage_primary)?;
    maintenance::copy_database(&main, &mut next)?;
    drop(main);
    schema::configure(&next)?;
    let key = Zeroizing::new(hex::encode(crate::vault::data_key()?.as_slice()));
    let setup = Connection::open_in_memory().map_err(|_| RECOVER)?;
    attach(&setup, &stage_key, &key)?;
    setup
        .execute_batch("PRAGMA settings_vault.application_id=0x53444b32;")
        .map_err(|_| RECOVER)?;
    initialize_schema(&setup)?;
    setup.close().map_err(|_| RECOVER)?;
    let next_id = salt(&stage_key)?;
    if next_id == previous
        || (old_keystore[0].is_some()
            && salt(&originals.join(NAME)).ok().as_deref() == Some(&next_id))
    {
        return Err(
            "New installation salt matches the previous salt; reset was not published.".into(),
        );
    }

    let tx = next.transaction().map_err(|e| e.to_string())?;
    crate::plugin_settings::clear_refs(&tx)?;
    if tx.execute("UPDATE database_metadata SET value=?1 WHERE key='settings_installation'", [&next_id]).map_err(|e| e.to_string())? != 1
        || tx.execute("UPDATE database_metadata SET value=?1 WHERE key='storage_generation'", [id.to_string()]).map_err(|e| e.to_string())? != 1
        || tx.execute("UPDATE database_metadata SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='commit_sequence' AND CAST(value AS INTEGER)<9223372036854775807", []).map_err(|e| e.to_string())? != 1
    {
        return Err("Installation metadata is unavailable or the commit sequence is exhausted.".into());
    }
    tx.commit().map_err(|e| e.to_string())?;
    schema::validate(&next)?;
    next.close().map_err(|(_, e)| e.to_string())?;
    sync_file(&stage_primary)?;
    sync_file(&stage_key)?;
    files::sync_directory(&staged)?;
    let intent = Intent {
        version: 1,
        kind: mode,
        id,
        old_primary,
        old_keystore,
        new_primary: schema::fingerprint(&stage_primary)?,
        new_keystore: schema::fingerprint(&stage_key)?,
    };
    check_live(root, &intent)?;
    recheck_review()?;
    credentials.create(&next_id, &key)?;
    check_staged(&staged, credentials, &intent)?;
    let mut pending = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    serde_json::to_writer(pending.as_file_mut(), &intent).map_err(|e| e.to_string())?;
    pending.as_file_mut().flush().map_err(|e| e.to_string())?;
    pending.as_file().sync_all().map_err(|e| e.to_string())?;
    recheck_review()?;
    pending.persist_noclobber(marker).map_err(|_| FENCED)?;
    files::sync_directory(root)?;
    Ok(intent)
}

fn valid_pair_shape(old: &schema::Fingerprint, new: &schema::Fingerprint) -> bool {
    old.len() == SUFFIXES.len()
        && new.len() == SUFFIXES.len()
        && new[0].is_some()
        && new[1..].iter().all(Option::is_none)
}

fn check_live(root: &Path, intent: &Intent) -> Result<(), String> {
    let databases = crate::database_directory(root);
    for (name, old, new) in [
        (
            schema::DATABASE_NAME,
            &intent.old_primary,
            &intent.new_primary,
        ),
        (NAME, &intent.old_keystore, &intent.new_keystore),
    ] {
        if !valid_pair_shape(old, new) {
            return Err(FENCED.into());
        }
        ensure_regular_paths(&databases, name)?;
        let current = schema::fingerprint(&databases.join(name))?;
        if (current[0] != old[0] && current[0] != new[0])
            || current[1..]
                .iter()
                .zip(&old[1..])
                .any(|(item, expected)| item.is_some() && item != expected)
        {
            return Err(FENCED.into());
        }
    }
    Ok(())
}

fn check_staged(
    staged: &Path,
    credentials: &impl Credentials,
    intent: &Intent,
) -> Result<(), String> {
    let primary = staged.join(schema::DATABASE_NAME);
    let keystore = staged.join(NAME);
    for name in [schema::DATABASE_NAME, NAME] {
        ensure_regular_paths(staged, name)?;
    }
    if schema::fingerprint(&primary)? != intent.new_primary
        || schema::fingerprint(&keystore)? != intent.new_keystore
        || !valid_pair_shape(&intent.old_primary, &intent.new_primary)
        || !valid_pair_shape(&intent.old_keystore, &intent.new_keystore)
    {
        return Err(FENCED.into());
    }
    let check = schema::connect(&primary)?;
    schema::validate(&check)?;
    let id = salt(&keystore)?;
    if installation(&check)? != id {
        return Err(FENCED.into());
    }
    let key = credentials.read(&id)?.ok_or(KEY_ACCESS)?;
    attach(&check, &keystore, &key)?;
    let app: i64 = check
        .query_row("PRAGMA settings_vault.application_id", [], |r| r.get(0))
        .map_err(|_| RECOVER)?;
    let rows: i64 = check
        .query_row("SELECT count(*) FROM settings_vault.revisions", [], |r| {
            r.get(0)
        })
        .map_err(|_| RECOVER)?;
    super::validate_schema(&check)?;
    let sdk_rows: i64 = check.query_row("SELECT (SELECT count(*) FROM settings_vault.plugin_storage_namespaces)+(SELECT count(*) FROM settings_vault.plugin_storage_records)+(SELECT count(*) FROM settings_vault.plugin_storage_receipts)+(SELECT count(*) FROM settings_vault.plugin_storage_generations)", [], |row| row.get(0)).map_err(|_| RECOVER)?;
    if app != 0x53444b32 || rows != 0 || sdk_rows != 0 {
        return Err(FENCED.into());
    }
    Ok(())
}

fn check_evidence(directory: &Path, intent: &Intent) -> Result<(), String> {
    let originals = directory.join("originals");
    let staged = directory.join("staged");
    for path in [directory, &originals, &staged] {
        require_directory(path)?;
    }
    for name in [schema::DATABASE_NAME, NAME] {
        ensure_regular_paths(&originals, name)?;
    }
    if schema::fingerprint(&originals.join(schema::DATABASE_NAME))? != intent.old_primary
        || schema::fingerprint(&originals.join(NAME))? != intent.old_keystore
    {
        return Err(FENCED.into());
    }
    Ok(())
}

fn publish(
    root: &Path,
    credentials: &impl Credentials,
    marker: &Path,
    directory: &Path,
    intent: &Intent,
    recheck_review: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    check_evidence(directory, intent)?;
    let staged = directory.join("staged");
    check_staged(&staged, credentials, intent)?;
    check_live(root, intent)?;
    recheck_review()?;
    // Retire live sidecars only after durable byte-for-byte copies and a startup fence.
    let databases = crate::database_directory(root);
    for name in [NAME, schema::DATABASE_NAME] {
        for suffix in SUFFIXES.into_iter().skip(1) {
            let path = databases.join(format!("{name}{suffix}"));
            if crate::vault::owned_regular(&path)? {
                fs::remove_file(path).map_err(|_| FENCED)?;
            }
        }
    }
    files::sync_directory(&databases)?;
    // Publish key first. The primary is never missing, and the fence is only
    // removed after both final fingerprints match the verified staged pair.
    for (name, fingerprint) in [
        (NAME, &intent.new_keystore),
        (schema::DATABASE_NAME, &intent.new_primary),
    ] {
        let target = databases.join(name);
        if schema::fingerprint(&target)? != *fingerprint {
            let mut copy = tempfile::NamedTempFile::new_in(&databases).map_err(|_| FENCED)?;
            std::io::copy(
                &mut fs::File::open(staged.join(name)).map_err(|_| FENCED)?,
                copy.as_file_mut(),
            )
            .map_err(|_| FENCED)?;
            copy.as_file().sync_all().map_err(|_| FENCED)?;
            files::atomic_replace(copy.path(), &target)?;
        }
        if schema::fingerprint(&target)? != *fingerprint {
            return Err(FENCED.into());
        }
    }
    fs::remove_file(marker).map_err(|_| FENCED)?;
    files::sync_directory(root)
}

fn create_new(path: &Path) -> Result<fs::File, String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| e.to_string())
}

fn copy_new(source: &Path, destination: &Path) -> Result<(), String> {
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
    let mut input = options.open(source).map_err(|e| e.to_string())?;
    if !input.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Recovery input is not a regular file.".into());
    }
    let mut output = create_new(destination)?;
    std::io::copy(&mut input, &mut output).map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())
}

fn sync_file(path: &Path) -> Result<(), String> {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
    };

    #[derive(Default)]
    struct Fake {
        keys: RefCell<BTreeMap<String, String>>,
        locked: Cell<bool>,
    }
    impl Credentials for Fake {
        fn read(&self, id: &str) -> Result<Option<Zeroizing<String>>, String> {
            if self.locked.get() {
                return Err(KEY_ACCESS.into());
            }
            Ok(self.keys.borrow().get(id).cloned().map(Zeroizing::new))
        }
        fn create(&self, id: &str, key: &str) -> Result<(), String> {
            assert!(!self.locked.get());
            assert!(self
                .keys
                .borrow_mut()
                .insert(id.into(), key.into())
                .is_none());
            Ok(())
        }
    }

    fn fixture(root: &Path, keys: &Fake) -> String {
        let primary = crate::database_path(root, schema::DATABASE_NAME);
        drop(crate::Storage::open_staging(root.to_owned()).unwrap());
        let db = schema::connect(&primary).unwrap();
        let path = crate::database_path(root, NAME);
        create_new(&path).unwrap();
        let key = "a".repeat(64);
        attach(&db, &path, &key).unwrap();
        db.execute_batch("PRAGMA settings_vault.application_id=0x53444b32")
            .unwrap();
        initialize_schema(&db).unwrap();
        let id = salt(&path).unwrap();
        keys.create(&id, &key).unwrap();
        db.execute(
            "UPDATE database_metadata SET value=?1 WHERE key='settings_installation'",
            [&id],
        )
        .unwrap();
        db.execute(
            "INSERT INTO plugin_globals(key,payload,revision) VALUES('example','{}',1)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO plugin_setting_refs(domain,record_key,pointer,revision_id,plugin_id) VALUES('plugin_globals','example','','old-secret-ref','example')", []).unwrap();
        db.execute("INSERT INTO settings_vault.revisions VALUES('plugin_globals','example','','old-secret-ref','\"test\"')", []).unwrap();
        db.close().unwrap();
        id
    }

    #[test]
    fn preserves_pair_and_resumes_interrupted_publish_without_rotating_again() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let keys = Fake::default();
        let old_id = fixture(root, &keys);
        let previous_generation = {
            let db = schema::connect(&crate::database_path(root, schema::DATABASE_NAME)).unwrap();
            crate::persisted_generation(&db).unwrap()
        };
        let old_main = fs::read(crate::database_path(root, schema::DATABASE_NAME)).unwrap();
        let old_key = fs::read(crate::database_path(root, NAME)).unwrap();
        fs::write(
            crate::database_path(root, "datastore.sqlite3-journal"),
            b"old sidecar evidence",
        )
        .unwrap();
        check_using(root, &keys, KeystoreInitialization::Reset).unwrap();
        assert_eq!(keys.keys.borrow().len(), 1);
        let marker = root.join(schema::RECOVERY_PENDING);
        let intent = prepare(root, &keys, &marker, KeystoreInitialization::Reset, &|| {
            Ok(())
        })
        .unwrap();
        let dir = evidence(root, intent.id);
        let staged = dir.join("staged");
        // Simulate power loss after clearing sidecars and replacing only the key.
        fs::remove_file(crate::database_path(root, "datastore.sqlite3-journal")).unwrap();
        fs::copy(staged.join(NAME), crate::database_path(root, NAME)).unwrap();
        assert!(schema::open(root).is_err());
        check_using(root, &keys, KeystoreInitialization::Reset).unwrap();
        let preserved =
            reset_using(root, &keys, KeystoreInitialization::Reset, &|| Ok(())).unwrap();
        assert!(!marker.exists());
        assert_eq!(keys.keys.borrow().len(), 2);
        assert!(keys.read(&old_id).unwrap().is_some());
        assert_eq!(
            fs::read(preserved.join(schema::DATABASE_NAME)).unwrap(),
            old_main
        );
        assert_eq!(fs::read(preserved.join(NAME)).unwrap(), old_key);
        assert_eq!(
            fs::read(preserved.join("datastore.sqlite3-journal")).unwrap(),
            b"old sidecar evidence"
        );
        let db = schema::connect(&crate::database_path(root, schema::DATABASE_NAME)).unwrap();
        let (payload, refs): (String, i64) = db
            .query_row(
                "SELECT payload,(SELECT count(*) FROM plugin_setting_refs) FROM plugin_globals WHERE key='example'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((payload.as_str(), refs), ("{}", 0));
        assert_ne!(installation(&db).unwrap(), old_id);
        assert_ne!(
            crate::persisted_generation(&db).unwrap(),
            previous_generation
        );
    }

    #[test]
    fn locked_store_and_unrelated_fence_leave_live_evidence_untouched() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let keys = Fake::default();
        fixture(root, &keys);
        let old_main = fs::read(crate::database_path(root, schema::DATABASE_NAME)).unwrap();
        let old_key = fs::read(crate::database_path(root, NAME)).unwrap();
        keys.locked.set(true);
        assert!(check_using(root, &keys, KeystoreInitialization::Reset).is_err());
        assert!(reset_using(root, &keys, KeystoreInitialization::Reset, &|| Ok(())).is_err());
        assert_eq!(keys.keys.borrow().len(), 1);
        let marker = root.join(schema::RECOVERY_PENDING);
        assert!(!marker.exists());
        fs::write(&marker, b"{\"version\":1,\"candidate\":\"old.sqlite3\"}").unwrap();
        keys.locked.set(false);
        assert!(check_using(root, &keys, KeystoreInitialization::Reset).is_err());
        assert!(reset_using(root, &keys, KeystoreInitialization::Reset, &|| Ok(())).is_err());
        assert_eq!(
            fs::read(crate::database_path(root, schema::DATABASE_NAME)).unwrap(),
            old_main
        );
        assert_eq!(fs::read(crate::database_path(root, NAME)).unwrap(), old_key);
        assert!(marker.exists());
        assert_eq!(keys.keys.borrow().len(), 1);
    }
    #[test]
    fn stale_review_stops_credential_creation_and_resumed_publication() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let keys = Fake::default();
        fixture(root, &keys);
        let marker = root.join(schema::RECOVERY_PENDING);
        let prior_main = fs::read(crate::database_path(root, schema::DATABASE_NAME)).unwrap();
        let prior_key = fs::read(crate::database_path(root, NAME)).unwrap();
        let stale = || Err("review became stale".into());
        assert_eq!(
            reset_using(root, &keys, KeystoreInitialization::Reset, &stale).unwrap_err(),
            "review became stale"
        );
        assert_eq!(keys.keys.borrow().len(), 1);
        assert!(!marker.exists());

        prepare(root, &keys, &marker, KeystoreInitialization::Reset, &|| {
            Ok(())
        })
        .unwrap();
        assert!(is_reset_marker(root).unwrap());
        assert_eq!(
            reset_using(root, &keys, KeystoreInitialization::Reset, &stale).unwrap_err(),
            "review became stale"
        );
        assert!(marker.exists());
        assert_eq!(keys.keys.borrow().len(), 2);
        assert_eq!(
            fs::read(crate::database_path(root, schema::DATABASE_NAME)).unwrap(),
            prior_main
        );
        assert_eq!(
            fs::read(crate::database_path(root, NAME)).unwrap(),
            prior_key
        );
    }
    #[test]
    fn create_requires_absent_evidence_and_only_resumes_as_create() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let keys = Fake::default();
        let old_id = fixture(root, &keys);
        let create = KeystoreInitialization::Create;
        let reset = KeystoreInitialization::Reset;
        let marker = root.join(schema::RECOVERY_PENDING);
        assert!(check_using(root, &keys, create).is_err());
        assert!(reset_using(root, &keys, create, &|| Ok(())).is_err());
        fs::remove_file(crate::database_path(root, NAME)).unwrap();
        let orphan = crate::database_path(root, "datastore.sqlite3-wal");
        fs::write(&orphan, b"orphan evidence").unwrap();
        assert!(check_using(root, &keys, create).is_err());
        assert!(reset_using(root, &keys, create, &|| Ok(())).is_err());
        assert_eq!(fs::read(&orphan).unwrap(), b"orphan evidence");
        assert_eq!(keys.keys.borrow().len(), 1);
        assert!(!marker.exists());
        fs::remove_file(orphan).unwrap();

        check_using(root, &keys, create).unwrap();
        let intent = prepare(root, &keys, &marker, create, &|| Ok(())).unwrap();
        let stage = evidence(root, intent.id).join("staged");
        fs::copy(stage.join(NAME), crate::database_path(root, NAME)).unwrap();
        assert!(is_reset_marker(root).unwrap());
        assert_eq!(pending_operation(root).unwrap(), Some(create));
        assert!(check_using(root, &keys, reset).is_err());
        assert!(reset_using(root, &keys, reset, &|| Ok(())).is_err());
        assert!(marker.exists());
        check_using(root, &keys, create).unwrap();
        let preserved = reset_using(root, &keys, create, &|| Ok(())).unwrap();
        assert!(preserved.join(schema::DATABASE_NAME).is_file());
        assert!(!preserved.join(NAME).exists());
        assert_eq!(pending_operation(root).unwrap(), None);
        assert_eq!(keys.keys.borrow().len(), 2);
        assert!(keys.read(&old_id).unwrap().is_some());
    }
}
