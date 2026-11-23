//! Local recovery captures only the encrypted SQLCipher database, never its OS key
//! or the separately managed user Vault. The serialized worker excludes writers.
use super::{attach_existing_path, installation, salt, NAME, RECOVER};
use crate::{files, schema};
use rusqlite::Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

const PREFIX: &str = "plugin-settings-";
const EXTENSION: &str = ".encrypted";
pub(super) const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FILES: usize = 1024;
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

#[derive(Debug, Serialize)]
pub struct PluginKeystoreBackup {
    pub id: String,
    pub bytes: u64,
    pub modified_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct PluginKeystoreReview {
    pub token: String,
    pub backup_id: Option<String>,
}

fn recovery_dir(root: &Path, create: bool) -> Result<Option<PathBuf>, String> {
    if create {
        crate::database_layout::ensure_recovery_directory(root).map(Some)
    } else {
        crate::database_layout::inspect_recovery_directory(root)
    }
}

fn valid_id(id: &str) -> bool {
    // Filename is both the ID and the lookup key. No caller-supplied path accepted.
    let Some(raw) = id
        .strip_prefix(PREFIX)
        .and_then(|s| s.strip_suffix(EXTENSION))
    else {
        return false;
    };
    let Some((time, nonce)) = raw.split_once('-') else {
        return false;
    };
    time.len() <= 20
        && time.parse::<u64>().is_ok_and(|t| t.to_string() == time)
        && uuid::Uuid::parse_str(nonce).is_ok_and(|value| value.hyphenated().to_string() == nonce)
}

pub(crate) fn backups(root: &Path) -> Result<Vec<PluginKeystoreBackup>, String> {
    let Some(dir) = recovery_dir(root, false)? else {
        return Ok(Vec::new());
    };
    let mut backups = Vec::new();
    for (index, item) in fs::read_dir(dir).map_err(|e| e.to_string())?.enumerate() {
        if index >= MAX_FILES {
            return Err("Recovery file inventory exceeds its limit".into());
        }
        let item = item.map_err(|e| e.to_string())?;
        let Some(id) = item
            .file_name()
            .into_string()
            .ok()
            .filter(|id| valid_id(id))
        else {
            continue;
        };
        if !crate::vault::owned_regular(&item.path())? {
            continue;
        }
        let meta = item.path().symlink_metadata().map_err(|e| e.to_string())?;
        if !(16..=MAX_BYTES).contains(&meta.len()) {
            continue;
        }
        backups.push(PluginKeystoreBackup {
            id,
            bytes: meta.len(),
            modified_ms: meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|time| u64::try_from(time.as_millis()).ok())
                .unwrap_or_default(),
        });
    }
    backups.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(backups)
}

// Copy a regular, single-linked file without following a later substituted link.
// Hash exactly the bytes sent to the private output; reject oversize before writing.
pub(super) fn copy_bounded(
    source: &Path,
    mut output: impl Write,
    max: u64,
) -> Result<[u8; 32], String> {
    if !crate::vault::owned_regular(source)? {
        return Err(RECOVER.into());
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
    let mut input = options.open(source).map_err(|_| RECOVER)?;
    let meta = input.metadata().map_err(|_| RECOVER)?;
    if !meta.is_file() || meta.len() > max {
        return Err(
            "Encrypted plugin settings recovery file exceeds its size limit or is not regular"
                .into(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
            return Err("Encrypted plugin settings recovery file is not owner-controlled".into());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err("Linked plugin settings recovery files are not supported".into());
        }
    }
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut buffer = [0; 65536];
    loop {
        let read = input.read(&mut buffer).map_err(|_| RECOVER)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max {
            return Err("Encrypted plugin settings recovery file exceeds its size limit".into());
        }
        hash.update(&buffer[..read]);
        output.write_all(&buffer[..read]).map_err(|_| RECOVER)?;
    }
    if total != meta.len()
        || input.metadata().map_err(|_| RECOVER)?.modified().ok() != meta.modified().ok()
    {
        return Err("Encrypted plugin settings file changed while copying".into());
    }
    Ok(hash.finalize().into())
}

pub(super) fn no_sidecars(root: &Path) -> Result<(), String> {
    for suffix in SIDECARS {
        if crate::vault::owned_regular(&crate::database_path(root, &format!("{NAME}{suffix}")))? {
            return Err("Plugin settings keystore has an outstanding SQLite sidecar; originals preserved. Close all writers before restoring.".into());
        }
    }
    Ok(())
}

/// Only an absent keystore with no leftover SQLite files is eligible for Create.
pub(crate) fn is_genuinely_missing(db: &Connection, root: &Path) -> Result<bool, String> {
    let id = installation(db)?;
    if !id.is_empty() && (id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(RECOVER.into());
    }
    if crate::vault::owned_regular(&crate::database_path(root, NAME))? {
        return Ok(false);
    }
    no_sidecars(root)?;
    Ok(true)
}

/// Capture after the redacted database snapshot on the same serialized worker.
pub(crate) fn capture(
    db: &Connection,
    root: &Path,
    time_ms: u128,
) -> Result<Option<PathBuf>, String> {
    let id = installation(db)?;
    if id.is_empty() {
        return Ok(None);
    }
    if !db.is_autocommit() || db.is_readonly("settings_vault").is_err() {
        return Err(
            "Live plugin settings keystore is not available for a consistent backup".into(),
        );
    }
    no_sidecars(root)?;
    let source = crate::database_path(root, NAME);
    let directory = recovery_dir(root, true)?.ok_or(RECOVER)?;
    super::check_backup_directory(&directory)?;
    let mut temp = tempfile::NamedTempFile::new_in(&directory).map_err(|_| RECOVER)?;
    copy_bounded(&source, temp.as_file_mut(), MAX_BYTES)?;
    temp.as_file().sync_all().map_err(|_| RECOVER)?;
    if salt(temp.path())? != id {
        return Err(
            "Live encrypted plugin settings installation changed; no backup written".into(),
        );
    }
    let destination = directory.join(format!(
        "{PREFIX}{time_ms}-{}{EXTENSION}",
        uuid::Uuid::new_v4()
    ));
    temp.persist_noclobber(&destination).map_err(|_| RECOVER)?;
    files::sync_directory(&directory)?;
    Ok(Some(destination))
}

// A recovery editor may inspect the intact main DB even when the keystore is
// unavailable; attaching a private copy never replays a journal into the original.
pub(crate) fn attach_inspection_copy(
    db: &schema::ScratchDatabase,
    root: &Path,
) -> Result<bool, String> {
    let expected = installation(db)?;
    let source = crate::database_path(root, NAME);
    if expected.is_empty() && !crate::vault::owned_regular(&source)? {
        no_sidecars(root)?;
        crate::plugin_settings::validate_recovery_refs(db)?;
        return Ok(false);
    }
    let path = db.directory.path().join(NAME);
    let mut temp = tempfile::NamedTempFile::new_in(db.directory.path()).map_err(|_| RECOVER)?;
    copy_bounded(&source, temp.as_file_mut(), MAX_BYTES)?;
    for suffix in SIDECARS {
        let source = crate::database_path(root, &format!("{NAME}{suffix}"));
        if crate::vault::owned_regular(&source)? {
            if suffix != "-journal" {
                return Err(RECOVER.into());
            }
            let mut sidecar =
                tempfile::NamedTempFile::new_in(db.directory.path()).map_err(|_| RECOVER)?;
            copy_bounded(&source, sidecar.as_file_mut(), MAX_BYTES)?;
            sidecar
                .persist_noclobber(db.directory.path().join(format!("{NAME}{suffix}")))
                .map_err(|_| RECOVER)?;
        }
    }
    temp.persist_noclobber(&path).map_err(|_| RECOVER)?;
    let id = salt(&path)?;
    if !expected.is_empty() && id != expected {
        return Err(RECOVER.into());
    }
    attach_existing_path(db, &path, root, &id)?;
    check_integrity(db)?;
    crate::plugin_settings::validate_recovery_refs(db)?;
    Ok(!expected.is_empty())
}

/// Prepared bytes live in a private owned-root file; keep only the digest in a review.
pub(crate) struct Candidate {
    pub(crate) sha256: [u8; 32],
    file: tempfile::NamedTempFile,
}

fn check_integrity(db: &Connection) -> Result<(), String> {
    let fmt: String = db
        .query_row("PRAGMA settings_vault.integrity_check", [], |r| r.get(0))
        .map_err(|_| RECOVER)?;
    let mut cipher = db
        .prepare("PRAGMA settings_vault.cipher_integrity_check")
        .map_err(|_| RECOVER)?;
    if fmt != "ok"
        || cipher
            .query([])
            .map_err(|_| RECOVER)?
            .next()
            .map_err(|_| RECOVER)?
            .is_some()
    {
        return Err("Encrypted plugin settings backup failed integrity validation".into());
    }
    Ok(())
}

pub(crate) fn prepare(root: &Path, backup_id: &str) -> Result<Candidate, String> {
    if !valid_id(backup_id) {
        return Err("Invalid encrypted plugin settings backup ID".into());
    }
    if root
        .join(schema::RECOVERY_PENDING)
        .symlink_metadata()
        .is_ok()
    {
        return Err("A database recovery operation is unfinished; resolve it before restoring plugin settings".into());
    }
    no_sidecars(root)?;
    let directory =
        recovery_dir(root, false)?.ok_or("Encrypted plugin settings backup is unavailable")?;
    let source = directory.join(backup_id);
    let mut file =
        tempfile::NamedTempFile::new_in(crate::database_directory(root)).map_err(|_| RECOVER)?;
    let sha256 = copy_bounded(&source, file.as_file_mut(), MAX_BYTES)?;
    file.as_file().sync_all().map_err(|_| RECOVER)?;
    let (db, _) =
        schema::snapshot_copy_unattached(&crate::database_path(root, schema::DATABASE_NAME))?;
    let id = installation(&db)?;
    if id.is_empty() || id != salt(file.path())? {
        return Err(
            "The backup does not belong to the current plugin settings installation".into(),
        );
    }
    attach_existing_path(&db, file.path(), root, &id)?;
    check_integrity(&db)?;
    crate::plugin_settings::validate_recovery_refs(&db)?;
    drop(db);
    // Validate did not rewrite the staged bytes or make a journal visible.
    if copy_bounded(file.path(), std::io::sink(), MAX_BYTES)? != sha256 {
        return Err("Encrypted plugin settings backup changed during validation".into());
    }
    Ok(Candidate { sha256, file })
}

pub(crate) fn publish_restore(
    root: &Path,
    candidate: Candidate,
    recheck: impl FnOnce() -> Result<(), String>,
) -> Result<PathBuf, String> {
    no_sidecars(root)?;
    // Explicitly preserved evidence is separate from automatic-prune inventory.
    let preserved = tempfile::Builder::new()
        .prefix("keystore-restore-preserved-")
        .tempdir_in(root)
        .map_err(|_| RECOVER)?;
    let live = crate::database_path(root, NAME);
    if crate::vault::owned_regular(&live)? {
        let mut file = tempfile::NamedTempFile::new_in(preserved.path()).map_err(|_| RECOVER)?;
        copy_bounded(&live, file.as_file_mut(), MAX_BYTES)?;
        file.as_file().sync_all().map_err(|_| RECOVER)?;
        file.persist_noclobber(preserved.path().join(NAME))
            .map_err(|_| RECOVER)?;
    }
    files::sync_directory(preserved.path())?;
    let preserved = preserved.keep();
    files::sync_directory(root)?;
    // Only one file changes, so no split commit with the unchanged main DB.
    no_sidecars(root)?;
    recheck()?;
    // An older datastore must not re-enable an SDK handle or receipt from its previous timeline.
    // The reviewed source bytes stay unchanged; only this private staged copy receives a new epoch.
    let check = Connection::open_in_memory().map_err(|_| RECOVER)?;
    let id = salt(candidate.file.path())?;
    attach_existing_path(&check, candidate.file.path(), root, &id)?;
    let transaction = check.unchecked_transaction().map_err(|_| RECOVER)?;
    super::rotate_storage_epoch(&transaction)?;
    transaction.commit().map_err(|_| RECOVER)?;
    check.close().map_err(|_| RECOVER)?;
    candidate.file.as_file().sync_all().map_err(|_| RECOVER)?;
    files::atomic_replace(candidate.file.path(), &live)?;
    Ok(preserved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Domain, RecoveryEditor, Storage};
    use serde_json::{json, Value};
    // Run fixture-only key tests via the process-scoped, owner-only key file.
    // Never trigger a test that writes a native installation credential.
    fn fixture() -> Option<(tempfile::TempDir, PathBuf, Storage)> {
        std::env::var_os(super::super::KEY_FILE)?;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("manager");
        let storage = Storage::open(root.clone()).unwrap();
        Some((temp, root, storage))
    }

    #[test]
    #[ignore = "Requires a disposable installation key file supplied to test process"]
    fn automatic_encrypted_keystore_archive_restores_only_matching_live_references() {
        let (_temp, root, storage) = fixture().expect("set a disposable process-scoped key file");
        let private = "fixture-private-0ef23000";
        storage
            .write_json(
                Domain::PluginGlobals,
                "owner",
                &json!({"secret":private}),
                1024,
            )
            .unwrap();
        let old_epoch: String = storage.run(|db| {
            db.execute_batch("INSERT INTO settings_vault.plugin_storage_generations VALUES('owner',7);
                INSERT INTO settings_vault.plugin_storage_namespaces VALUES('owner','state','17',X'0000000000000001');
                INSERT INTO settings_vault.plugin_storage_records VALUES('owner','state','a','true',5);
                INSERT INTO settings_vault.plugin_storage_receipts VALUES('owner','state','transaction','digest','17',X'0000000000000001',1);").map_err(|e| e.to_string())?;
            db.query_row("SELECT epoch FROM settings_vault.plugin_storage_epoch",[],|r|r.get(0)).map_err(|e|e.to_string())
        }).unwrap();
        let ordinary = storage.backup_recovery().unwrap();
        let first = backups(&root).unwrap();
        assert_eq!(first.len(), 1);
        let first_file = crate::recovery_directory(&root).join(&first[0].id);
        let encrypted = fs::read(&first_file).unwrap();
        assert_eq!(
            encrypted,
            fs::read(crate::database_path(&root, NAME)).unwrap()
        );
        assert!(!encrypted
            .windows(private.len())
            .any(|w| w == private.as_bytes()));
        let redacted = crate::maintenance::open_snapshot(&ordinary.path).unwrap();
        assert!(installation(&redacted).unwrap().is_empty());
        assert_eq!(
            crate::read_json::<Value>(&redacted, Domain::PluginGlobals, "owner", 1024).unwrap(),
            Some(json!({}))
        );

        let updated = json!({"secret":"current-only"});
        storage
            .write_json(Domain::PluginGlobals, "owner", &updated, 1024)
            .unwrap();
        storage.backup_recovery().unwrap();
        let matching = backups(&root)
            .unwrap()
            .into_iter()
            .find(|backup| backup.id != first[0].id)
            .unwrap();
        let matching_file = crate::recovery_directory(&root).join(&matching.id);
        let original = fs::read(&matching_file).unwrap();
        assert_ne!(original, encrypted);
        drop(storage);
        // A corrupt owned file is copied only on successful restore.
        let corrupt = b"deliberately corrupt keystore evidence";
        fs::write(crate::database_path(&root, NAME), corrupt).unwrap();
        let editor = RecoveryEditor::open_for_recovery(root.clone()).unwrap();
        let revision = editor.revision().unwrap();
        assert_eq!(editor.plugin_settings_status().unwrap().0, "unavailable");
        assert!(editor
            .preview_plugin_settings_restore(&revision, &first[0].id)
            .is_err());
        assert!(editor
            .preview_plugin_settings_restore(&revision, "../datastore.sqlite3")
            .is_err());
        let review = editor
            .preview_plugin_settings_restore(&revision, &matching.id)
            .unwrap();
        editor.retry_plugin_settings(&revision).unwrap();
        assert!(editor
            .restore_plugin_settings(&revision, &matching.id, &review.token)
            .is_err());
        let revision = editor.revision().unwrap();
        let review = editor
            .preview_plugin_settings_restore(&revision, &matching.id)
            .unwrap();
        let mut tampered = original.clone();
        *tampered.last_mut().unwrap() ^= 4;
        fs::write(&matching_file, &tampered).unwrap();
        assert!(editor
            .restore_plugin_settings(&revision, &matching.id, &review.token)
            .is_err());
        assert_eq!(
            fs::read(crate::database_path(&root, NAME)).unwrap(),
            corrupt
        );
        fs::write(&matching_file, &original).unwrap();
        let review = editor
            .preview_plugin_settings_restore(&revision, &matching.id)
            .unwrap();
        let preserved = editor
            .restore_plugin_settings(&revision, &matching.id, &review.token)
            .unwrap();
        assert_eq!(fs::read(preserved.join(NAME)).unwrap(), corrupt);
        assert_ne!(
            fs::read(crate::database_path(&root, NAME)).unwrap(),
            original
        );
        assert_eq!(fs::read(&matching_file).unwrap(), original);
        assert_eq!(editor.plugin_settings_status().unwrap().0, "ready");
        assert!(editor
            .preview_plugin_settings_restore(&editor.revision().unwrap(), &matching.id)
            .is_err());
        drop(editor);
        // The same reviewed restore can also recover a genuinely absent key.
        fs::remove_file(crate::database_path(&root, NAME)).unwrap();
        let editor = RecoveryEditor::open_for_recovery(root.clone()).unwrap();
        assert_eq!(editor.plugin_settings_status().unwrap().0, "missing");
        let revision = editor.revision().unwrap();
        let review = editor
            .preview_plugin_settings_restore(&revision, &matching.id)
            .unwrap();
        let evidence = editor
            .restore_plugin_settings(&revision, &matching.id, &review.token)
            .unwrap();
        assert!(!evidence.join(NAME).exists());
        assert_eq!(editor.plugin_settings_status().unwrap().0, "ready");
        drop(editor);
        let storage = Storage::open(root).unwrap();
        storage
            .run(move |db| {
                let (epoch, value, receipts): (String, String, i64) = db
                    .query_row(
                        "SELECT epoch,
                (SELECT value FROM settings_vault.plugin_storage_records),
                (SELECT count(*) FROM settings_vault.plugin_storage_receipts)
                FROM settings_vault.plugin_storage_epoch",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|e| e.to_string())?;
                assert_ne!(epoch, old_epoch);
                assert_eq!((value.as_str(), receipts), ("true", 0));
                Ok(())
            })
            .unwrap();
        assert_eq!(
            storage
                .read_json::<Value>(Domain::PluginGlobals, "owner", 1024)
                .unwrap(),
            Some(updated)
        );
    }

    fn pending_marker(kind: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "version": 1, "kind": kind, "id": uuid::Uuid::nil(),
            "old_primary": [null, null, null, null],
            "old_keystore": [null, null, null, null],
            "new_primary": [([1u8; 32]), null, null, null],
            "new_keystore": [([2u8; 32]), null, null, null],
        }))
        .unwrap()
    }

    #[test]
    fn interrupted_keystore_reset_is_unavailable_not_generic_restore() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("manager");
        drop(Storage::open_staging(root.clone()).unwrap());
        let path = root.join(schema::RECOVERY_PENDING);
        fs::write(&path, b"interrupted ordinary restore").unwrap();
        let editor = RecoveryEditor::open_for_recovery(root.clone()).unwrap();
        assert_eq!(editor.plugin_settings_status().unwrap().0, "unknown");
        drop(editor);
        fs::write(&path, pending_marker("installation-keystore-reset")).unwrap();
        let editor = RecoveryEditor::open_for_recovery(root).unwrap();
        assert_eq!(editor.plugin_settings_status().unwrap().0, "unavailable");
        let error = editor
            .preview_restore(
                &editor.revision().unwrap(),
                PathBuf::from("not-a-source"),
                |_| Ok(()),
                |_, _| Ok(Vec::new()),
            )
            .unwrap_err();
        assert!(error.contains("Complete the interrupted plugin settings reset"));
    }

    #[test]
    fn interrupted_keystore_create_has_distinct_resume_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("manager");
        drop(Storage::open_staging(root.clone()).unwrap());
        let editor = RecoveryEditor::open_for_recovery(root.clone()).unwrap();
        assert_eq!(editor.plugin_settings_status().unwrap().0, "missing");
        assert!(editor
            .preview_plugin_settings_reset(&editor.revision().unwrap())
            .unwrap_err()
            .contains("Choose Create"));
        drop(editor);
        fs::write(
            crate::database_path(&root, NAME),
            b"partially published synthetic file",
        )
        .unwrap();
        fs::write(
            root.join(schema::RECOVERY_PENDING),
            pending_marker("installation-keystore-create"),
        )
        .unwrap();
        let editor = RecoveryEditor::open_for_recovery(root).unwrap();
        let (status, reason) = editor.plugin_settings_status().unwrap();
        assert_eq!(status, "missing");
        assert!(reason.unwrap().contains("creation must be resumed"));
    }

    #[test]
    fn empty_installation_with_orphan_keystore_sidecar_is_not_ready() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("manager");
        drop(Storage::open_staging(root.clone()).unwrap());
        fs::write(
            crate::database_path(&root, "datastore.sqlite3-journal"),
            b"orphan",
        )
        .unwrap();
        let editor = RecoveryEditor::open_for_recovery(root.clone()).unwrap();
        assert_eq!(editor.plugin_settings_status().unwrap().0, "unavailable");
        assert_eq!(
            fs::read(crate::database_path(&root, "datastore.sqlite3-journal")).unwrap(),
            b"orphan"
        );
    }
}
