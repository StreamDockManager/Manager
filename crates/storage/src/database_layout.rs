//! One-time, restartable layout change under the existing configuration lock.
use crate::files;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const DIR: &str = "databases";
const RECOVERY: &str = "recovery";
const JOURNAL: &str = ".stream-dock-manager-database-relocation-v1.json";
const MAX_ITEMS: usize = 256;
const MAX_JOURNAL: u64 = 64 * 1024;
const REJECT: &str = "Database layout could not be relocated. Original database files are preserved. Do not replace or delete either copy.";

pub fn database_directory(root: &Path) -> PathBuf {
    root.join(DIR)
}

pub fn database_path(root: &Path, name: &str) -> PathBuf {
    database_directory(root).join(name)
}

pub fn recovery_directory(root: &Path) -> PathBuf {
    database_directory(root).join(RECOVERY)
}

/// Observe legacy evidence only; use the exclusive preflight before opening databases
pub fn has_legacy_database_layout(root: &Path) -> Result<bool, String> {
    if metadata(&root.join(JOURNAL))?.is_some() || metadata(&root.join(RECOVERY))?.is_some() {
        return Ok(true);
    }
    Ok(!scan(root)?.is_empty())
}

/// Relocate owned files without opening SQLite or creating a primary database
pub fn prepare_database_layout(root: &Path) -> Result<(), String> {
    let (root, _owner) = crate::own_root(root)?;
    prepare_owned(&root)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Item {
    name: String,
    digest: [u8; 32],
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    items: Vec<Item>,
}

#[derive(PartialEq, Eq)]
enum Kind {
    Move,
    ClosedRequired,
    CacheSidecar,
}

fn basic_family(name: &str, base: &str) -> Option<Kind> {
    if name == base {
        return Some(Kind::Move);
    }
    let suffix = name.strip_prefix(base)?;
    match suffix {
        "-wal" | "-shm" if base == "cache.sqlite3" => Some(Kind::CacheSidecar),
        "-wal" | "-shm" | "-journal" => Some(Kind::ClosedRequired),
        tail if tail.starts_with("-mj") => Some(Kind::ClosedRequired),
        _ => None,
    }
}

fn classify(name: &str) -> Option<Kind> {
    for base in [
        crate::DATABASE_NAME,
        crate::installation_key::NAME,
        crate::vault::FILE_NAME,
        "cache.sqlite3",
    ] {
        if let Some(kind) = basic_family(name, base) {
            return Some(kind);
        }
    }
    if let Some(stem) = name
        .strip_prefix(".keystore-key-")
        .and_then(|s| s.strip_suffix(".json"))
    {
        return (stem.len() == 32 && stem.bytes().all(|c| c.is_ascii_hexdigit()))
            .then_some(Kind::Move);
    }
    for prefix in [".keystore-reset-", ".keystore-switch-", ".keystore-stage-"] {
        let Some(tail) = name.strip_prefix(prefix) else {
            continue;
        };
        let base = if prefix == ".keystore-stage-" {
            match tail.get(..6) {
                Some(s) if s.bytes().all(|c| c.is_ascii_alphanumeric()) => prefix.len() + 6,
                _ => continue,
            }
        } else {
            let Some(uuid) = tail.get(..36) else {
                continue;
            };
            if !tail[36..].starts_with(".sqlite3") || uuid::Uuid::parse_str(uuid).is_err() {
                continue;
            }
            prefix.len() + 36 + ".sqlite3".len()
        };
        if let Some(kind) = basic_family(name, &name[..base]) {
            return Some(kind);
        }
    }
    None
}

fn fail(why: impl std::fmt::Display) -> String {
    format!("{REJECT} {why}")
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, String> {
    match path.symlink_metadata() {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(fail(e)),
    }
}

pub(crate) fn is_redirect(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

fn nonregular(m: &fs::Metadata, is_dir: bool) -> bool {
    is_redirect(m) || if is_dir { !m.is_dir() } else { !m.is_file() }
}

fn scan(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let mut folded_names = BTreeSet::new();
    if metadata(root)?.is_none() {
        return Ok(names);
    }
    for entry in fs::read_dir(root).map_err(fail)? {
        let entry = entry.map_err(fail)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        if classify(&lower).is_none() {
            continue;
        }
        if classify(name).is_none() || !folded_names.insert(lower) || names.len() >= MAX_ITEMS {
            return Err(fail("Invalid or excessive database-family names."));
        }
        names.insert(name.to_string());
    }
    Ok(names)
}

fn digest(path: &Path) -> Result<Option<[u8; 32]>, String> {
    let Some(meta) = metadata(path)? else {
        return Ok(None);
    };
    if nonregular(&meta, false) {
        return Err(fail(format!(
            "Nonregular database evidence: {}.",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(fail(format!(
                "Linked database evidence: {}.",
                path.display()
            )));
        }
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
    let file = options.open(path).map_err(fail)?;
    let opened = file.metadata().map_err(fail)?;
    if nonregular(&opened, false) || !same_file_stamp(&opened, &meta) {
        return Err(fail("A database file changed while reading it."));
    }
    let mut file = file.take(meta.len().saturating_add(1));
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0;
    loop {
        let count = file.read(&mut buffer).map_err(fail)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        size += count as u64;
    }
    if size != meta.len() || metadata(path)?.is_none_or(|m| !same_file_stamp(&m, &opened)) {
        return Err(fail("A database file changed while reading it."));
    }
    Ok(Some(hash.finalize().into()))
}

fn same_file_stamp(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if left.dev() != right.dev() || left.ino() != right.ino() {
            return false;
        }
    }
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}

pub(crate) fn ensure_directory(root: &Path) -> Result<PathBuf, String> {
    let directory = database_directory(root);
    if metadata(&directory)?.is_none() {
        files::ensure_directory(&directory).map_err(fail)?;
        files::sync_directory(root).map_err(fail)?;
    }
    if metadata(&directory)?.is_none_or(|m| nonregular(&m, true)) {
        return Err(fail("The databases path is not a real directory."));
    }
    Ok(directory)
}

fn recovery_exists(path: &Path) -> Result<bool, String> {
    match metadata(path)? {
        Some(m) if nonregular(&m, true) => Err(fail("The recovery path is not a real directory.")),
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

pub(crate) fn inspect_recovery_directory(root: &Path) -> Result<Option<PathBuf>, String> {
    if metadata(&root.join(RECOVERY))?.is_some() {
        return Err(fail(
            "The legacy recovery directory must be relocated first.",
        ));
    }
    let parent = database_directory(root);
    match metadata(&parent)? {
        Some(m) if nonregular(&m, true) => {
            return Err(fail("The databases path is not a real directory."))
        }
        None => return Ok(None),
        Some(_) => {}
    }
    let path = recovery_directory(root);
    Ok(recovery_exists(&path)?.then_some(path))
}

pub(crate) fn ensure_recovery_directory(root: &Path) -> Result<PathBuf, String> {
    let parent = ensure_directory(root)?;
    let path = recovery_directory(root);
    if !recovery_exists(&path)? {
        // Never open/create an empty nested tree while the old tree still exists
        if metadata(&root.join(RECOVERY))?.is_some() {
            return Err(fail(
                "The legacy recovery directory must be relocated first.",
            ));
        }
        files::ensure_directory(&path).map_err(fail)?;
        files::sync_directory(&parent).map_err(fail)?;
    }
    if !recovery_exists(&path)? {
        return Err(fail("Recovery directory changed during creation."));
    }
    Ok(path)
}

fn recovery_to_move(root: &Path) -> Result<bool, String> {
    let source = recovery_exists(&root.join(RECOVERY))?;
    let target = recovery_exists(&recovery_directory(root))?;
    if source && target {
        return Err(fail(
            "Recovery directories exist in both locations; they will not be merged.",
        ));
    }
    Ok(source)
}

pub(crate) fn prepare_owned(root: &Path) -> Result<(), String> {
    let directory = ensure_directory(root)?;
    let recovery = recovery_to_move(root)?;
    prepare_database_files(root, &directory)?;
    // A single no-replace directory rename cannot split the recovery tree.
    // If interrupted before it, source remains; afterward only target remains.
    if recovery && recovery_to_move(root)? {
        move_noreplace(&root.join(RECOVERY), &recovery_directory(root)).map_err(fail)?;
        files::sync_directory(&directory).map_err(fail)?;
        files::sync_directory(root).map_err(fail)?;
    }
    Ok(())
}

fn prepare_database_files(root: &Path, directory: &Path) -> Result<(), String> {
    let marker = root.join(JOURNAL);
    let source = scan(root)?;
    if source.is_empty() && metadata(&marker)?.is_none() {
        return Ok(());
    }
    let target = scan(directory)?;
    let journal = if let Some(m) = metadata(&marker)? {
        if nonregular(&m, false) || m.len() > MAX_JOURNAL {
            return Err(fail("Invalid database relocation journal."));
        }
        let mut bytes = Vec::new();
        fs::File::open(&marker)
            .and_then(|f| f.take(MAX_JOURNAL + 1).read_to_end(&mut bytes))
            .map_err(fail)?;
        if bytes.len() as u64 > MAX_JOURNAL {
            return Err(fail("Invalid database relocation journal."));
        }
        serde_json::from_slice::<Journal>(&bytes).map_err(fail)?
    } else {
        if !target.is_empty() {
            return Err(fail("Database-family files exist in both locations without a reviewed relocation journal."));
        }
        if let Some(name) = source
            .iter()
            .find(|name| classify(name) == Some(Kind::ClosedRequired))
        {
            return Err(fail(format!(
                "Unsettled SQLite journal or sidecar {name}. Open the data with the previous Manager version, let it recover and quit cleanly before retrying."
            )));
        }
        if source
            .iter()
            .any(|name| classify(name) == Some(Kind::CacheSidecar))
            && !source.contains("cache.sqlite3")
        {
            return Err(fail("Legacy cache sidecars exist without their database."));
        }
        let items = source
            .iter()
            .map(|name| {
                Ok(Item {
                    name: name.clone(),
                    digest: digest(&root.join(name))?
                        .ok_or_else(|| fail("Database disappeared before relocation."))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let journal = Journal { version: 1, items };
        let mut pending = tempfile::NamedTempFile::new_in(root).map_err(fail)?;
        serde_json::to_writer(pending.as_file_mut(), &journal).map_err(fail)?;
        pending.as_file_mut().flush().map_err(fail)?;
        pending.as_file().sync_all().map_err(fail)?;
        pending.persist_noclobber(&marker).map_err(fail)?;
        files::sync_directory(root).map_err(fail)?;
        journal
    };

    let listed: BTreeSet<_> = journal.items.iter().map(|item| item.name.clone()).collect();
    if journal.version != 1
        || journal.items.is_empty()
        || journal.items.len() > MAX_ITEMS
        || listed.len() != journal.items.len()
        || !source.is_disjoint(&target)
        || listed != source.union(&target).cloned().collect()
        || listed
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>()
            .len()
            != listed.len()
        || listed
            .iter()
            .any(|name| !matches!(classify(name), Some(Kind::Move | Kind::CacheSidecar)))
        || (listed
            .iter()
            .any(|name| classify(name) == Some(Kind::CacheSidecar))
            && !listed.contains("cache.sqlite3"))
    {
        return Err(fail(
            "Database relocation journal or split layout does not match the owned inventory.",
        ));
    }
    // Verify every member before making the first change, including in a resumed run.
    for item in &journal.items {
        let origin = if source.contains(&item.name) {
            root
        } else {
            directory
        };
        if digest(&origin.join(&item.name))? != Some(item.digest) {
            return Err(fail(
                "A database file does not match the relocation journal.",
            ));
        }
    }
    for item in &journal.items {
        if source.contains(&item.name) {
            move_noreplace(&root.join(&item.name), &directory.join(&item.name)).map_err(fail)?;
            files::sync_directory(directory).map_err(fail)?;
            files::sync_directory(root).map_err(fail)?;
        }
    }
    fs::remove_file(&marker).map_err(fail)?;
    files::sync_directory(root).map_err(fail)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn move_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "macos")]
    let status = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn move_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn move_noreplace(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "No safe database relocation syscall",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Domain, RecoveryEditor, Storage, DATABASE_NAME};
    use serde_json::json;

    #[test]
    fn persistent_data_and_encrypted_settings_survive_clean_flat_relocation() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let storage = Storage::open(root.into()).unwrap();
        storage
            .write_json(
                Domain::ManagerPreferences,
                "fixture",
                &json!({"level":42}),
                4096,
            )
            .unwrap();
        storage
            .write_json(
                Domain::PluginGlobals,
                "fixture",
                &json!({"api_key":"synthetic relocation credential"}),
                4096,
            )
            .unwrap();
        drop(storage);
        let names = [DATABASE_NAME, crate::installation_key::NAME];
        let install_salt = fs::read(database_path(root, names[1])).unwrap()[..16].to_vec();
        for name in names {
            fs::rename(database_path(root, name), root.join(name)).unwrap();
        }
        assert!(has_legacy_database_layout(root).unwrap());
        let editor = RecoveryEditor::open(root.into()).unwrap();
        assert_eq!(editor.plugin_settings_status().unwrap().0, "ready");
        assert_eq!(
            editor
                .read(|connection, _| crate::read_json::<serde_json::Value>(
                    connection,
                    Domain::PluginGlobals,
                    "fixture",
                    4096
                ))
                .unwrap(),
            Some(json!({"api_key":"synthetic relocation credential"}))
        );
        drop(editor);
        assert!(!has_legacy_database_layout(root).unwrap());
        for name in names {
            assert!(database_path(root, name).is_file());
            assert!(!root.join(name).exists());
        }
        let reopened = Storage::open(root.into()).unwrap();
        assert_eq!(
            fs::read(database_path(root, names[1])).unwrap()[..16],
            install_salt
        );
        assert_eq!(
            reopened
                .read_json::<serde_json::Value>(Domain::ManagerPreferences, "fixture", 4096)
                .unwrap(),
            Some(json!({"level":42}))
        );
        assert_eq!(
            reopened
                .read_json::<serde_json::Value>(Domain::PluginGlobals, "fixture", 4096)
                .unwrap(),
            Some(json!({"api_key":"synthetic relocation credential"}))
        );
        assert!(root.join("profiles/writer.lock").is_file());
    }

    #[test]
    fn hot_super_journal_and_cross_location_conflict_leave_all_files_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let flat = root.join(DATABASE_NAME);
        fs::write(&flat, b"old primary").unwrap();
        let sidecar = root.join("manager.sqlite3-mj42");
        fs::write(&sidecar, b"synthetic super journal").unwrap();
        fs::write(root.join("cache.sqlite3"), b"old cache").unwrap();
        assert!(prepare_database_layout(root)
            .unwrap_err()
            .contains("Unsettled SQLite"));
        assert_eq!(fs::read(&sidecar).unwrap(), b"synthetic super journal");
        assert_eq!(fs::read(&flat).unwrap(), b"old primary");
        assert!(!database_path(root, DATABASE_NAME).exists());
        assert!(!root.join(JOURNAL).exists());
        fs::remove_file(&sidecar).unwrap();
        fs::write(database_path(root, DATABASE_NAME), b"new primary").unwrap();
        assert!(prepare_database_layout(root)
            .unwrap_err()
            .contains("both locations"));
        assert_eq!(fs::read(&flat).unwrap(), b"old primary");
        assert_eq!(
            fs::read(database_path(root, DATABASE_NAME)).unwrap(),
            b"new primary"
        );
        assert_eq!(fs::read(root.join("cache.sqlite3")).unwrap(), b"old cache");
    }

    #[test]
    fn resumes_only_the_exact_fingerprinted_split_and_includes_caseful_owned_stage_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        ensure_directory(root).unwrap();
        let files = [
            "cache.sqlite3",
            "cache.sqlite3-wal",
            "cache.sqlite3-shm",
            ".keystore-stage-aB3cD4",
        ];
        let mut items = Vec::new();
        for name in files {
            let source = root.join(name);
            fs::write(&source, name.as_bytes()).unwrap();
            items.push(Item {
                name: name.into(),
                digest: digest(&source).unwrap().unwrap(),
            });
        }
        let first = files[0];
        move_noreplace(&root.join(first), &database_path(root, first)).unwrap();
        fs::write(
            root.join(JOURNAL),
            serde_json::to_vec(&Journal { version: 1, items }).unwrap(),
        )
        .unwrap();
        fs::write(database_path(root, first), b"substituted cache").unwrap();
        assert!(prepare_database_layout(root)
            .unwrap_err()
            .contains("does not match"));
        assert_eq!(fs::read(root.join(files[1])).unwrap(), files[1].as_bytes());
        fs::write(database_path(root, first), first.as_bytes()).unwrap();
        prepare_database_layout(root).unwrap();
        for name in files {
            assert_eq!(
                fs::read(database_path(root, name)).unwrap(),
                name.as_bytes()
            );
            assert!(!root.join(name).exists());
        }
        assert!(!root.join(JOURNAL).exists());
    }

    #[test]
    fn moves_existing_recovery_tree_atomically_and_keeps_snapshots_readable() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let storage = Storage::open(root.into()).unwrap();
        storage
            .write_json(Domain::ManagerPreferences, "fixture", &true, 64)
            .unwrap();
        let snapshot = storage.backup_recovery().unwrap().path;
        let name = snapshot.file_name().unwrap().to_owned();
        drop(storage);
        let archive = recovery_directory(root);
        let original = fs::read(&snapshot).unwrap();
        let quarantine = archive.join("quarantine-kept");
        fs::create_dir(&quarantine).unwrap();
        fs::write(quarantine.join("raw-sidecar"), b"original evidence").unwrap();
        fs::rename(&archive, root.join(RECOVERY)).unwrap();
        assert!(has_legacy_database_layout(root).unwrap());
        assert!(inspect_recovery_directory(root).is_err());
        assert!(ensure_recovery_directory(root).is_err());
        prepare_database_layout(root).unwrap();
        // The atomic move can be retried after either side of the rename
        prepare_database_layout(root).unwrap();
        assert!(!has_legacy_database_layout(root).unwrap());
        assert!(!root.join(RECOVERY).exists());
        assert_eq!(fs::read(archive.join(&name)).unwrap(), original);
        assert_eq!(
            fs::read(quarantine.join("raw-sidecar")).unwrap(),
            b"original evidence"
        );
        let (values, warnings) = crate::inspect_recovery_backups(root, 32, |_, _, connection| {
            crate::read_json::<bool>(connection, Domain::ManagerPreferences, "fixture", 64)
        })
        .unwrap();
        assert!(warnings.is_empty());
        assert!(values.contains(&Some(true)));
    }

    #[test]
    fn recovery_directory_collision_is_not_merged_and_preflights_before_database_movement() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let old = root.join(RECOVERY);
        let new = recovery_directory(root);
        fs::create_dir(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        fs::write(old.join("same-name"), b"old").unwrap();
        fs::write(new.join("same-name"), b"new").unwrap();
        fs::write(root.join(DATABASE_NAME), b"old database").unwrap();
        assert!(prepare_database_layout(root)
            .unwrap_err()
            .contains("will not be merged"));
        assert_eq!(fs::read(old.join("same-name")).unwrap(), b"old");
        assert_eq!(fs::read(new.join("same-name")).unwrap(), b"new");
        assert_eq!(fs::read(root.join(DATABASE_NAME)).unwrap(), b"old database");
        assert!(!database_path(root, DATABASE_NAME).exists());
    }

    #[test]
    #[cfg(unix)]
    fn rejects_legacy_or_nested_recovery_directory_links_without_following_them() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"outside").unwrap();
        for nested in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            ensure_directory(root).unwrap();
            let link = if nested {
                recovery_directory(root)
            } else {
                root.join(RECOVERY)
            };
            std::os::unix::fs::symlink(outside.path(), &link).unwrap();
            fs::write(root.join(DATABASE_NAME), b"db evidence").unwrap();
            assert!(prepare_database_layout(root)
                .unwrap_err()
                .contains("not a real directory"));
            assert_eq!(fs::read_link(link).unwrap(), outside.path());
            assert!(!database_path(root, DATABASE_NAME).exists());
        }
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
    }

    #[test]
    #[cfg(unix)]
    fn symlink_destination_is_never_followed() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(DATABASE_NAME), b"legacy manager").unwrap();
        std::os::unix::fs::symlink(outside.path(), database_directory(directory.path())).unwrap();
        assert!(prepare_database_layout(directory.path())
            .unwrap_err()
            .contains("not a real directory"));
        assert!(!outside.path().join(DATABASE_NAME).exists());
        assert_eq!(
            fs::read(directory.path().join(DATABASE_NAME)).unwrap(),
            b"legacy manager"
        );
    }
}
