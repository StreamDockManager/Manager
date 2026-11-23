//! Recovery-directory retention, run only by the root-owning storage worker after
//! publishing a validated snapshot. Never walk through symbolic links.
use crate::database_layout::is_redirect;
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub(crate) const FILE_LIMIT: usize = 32;
pub(crate) const BYTES_LIMIT: u64 = 1024 * 1024 * 1024;

// Check whether recovery files exceed the count or byte budget
pub(crate) fn exceeds(count: usize, bytes: u64) -> bool {
    count > FILE_LIMIT || bytes > BYTES_LIMIT
}
struct Entry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
    symlink: bool,
}
// Inventory nested recovery files without following symlinks or unbounded trees
fn scan(
    directory: &Path,
    entries: &mut Vec<Entry>,
    directories: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<(), String> {
    if depth > 64 || entries.len() + directories.len() > 100_000 {
        return Err(
            "Recovery retention scan exceeds its safety limit; files were preserved".into(),
        );
    }
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
        if metadata.is_dir() && !is_redirect(&metadata) {
            directories.push(path.clone());
            scan(&path, entries, directories, depth + 1)?;
        } else if (metadata.is_file() && !is_redirect(&metadata))
            || metadata.file_type().is_symlink()
        {
            entries.push(Entry {
                path,
                size: metadata.len(),
                modified: metadata.modified().map_err(|e| e.to_string())?,
                symlink: metadata.file_type().is_symlink(),
            });
        } else {
            return Err("Recovery retention found a special file; cleanup deferred".into());
        }
        if entries.len() + directories.len() > 100_000 {
            return Err(
                "Recovery retention scan exceeds its safety limit; files were preserved".into(),
            );
        }
    }
    Ok(())
}

// Verify directory chain
fn verify_directory_chain(root: &Path, mut path: Option<&Path>) -> Result<(), String> {
    while let Some(directory) = path {
        if !directory.starts_with(root) {
            break;
        }
        let metadata = directory.symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_dir() || is_redirect(&metadata) {
            return Err("Recovery directory changed during cleanup; retry later".into());
        }
        path = directory.parent();
    }
    Ok(())
}

// Evict oldest recovery files within the budget while preserving the newest snapshot
#[cfg(test)]
pub(crate) fn prune(root: &Path, newest: &Path) -> Result<Option<String>, String> {
    prune_protected(root, newest, None)
}

pub(crate) fn prune_protected(
    root: &Path,
    newest: &Path,
    keystore: Option<&Path>,
) -> Result<Option<String>, String> {
    // Restore owns this same root lock. This additional fence protects interrupted
    // publication if a future caller attempts cleanup before recovery finishes.
    match root
        .join(crate::schema::RECOVERY_PENDING)
        .symlink_metadata()
    {
        Ok(_) => {
            return Ok(Some(
                "Recovery cleanup deferred while database restore is unfinished".into(),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let directory = crate::database_layout::inspect_recovery_directory(root)?
        .ok_or("Recovery directory is unavailable; cleanup deferred")?;
    let database_directory = crate::database_directory(root);
    let mut entries = Vec::new();
    let mut directories = Vec::new();
    scan(&directory, &mut entries, &mut directories, 0)?;
    if !entries.iter().any(|e| e.path == newest && !e.symlink) {
        return Err("Newest recovery snapshot is unavailable; cleanup deferred".into());
    }
    let newest_bytes = entries.iter().find(|e| e.path == newest).unwrap().size;
    let available_keystore =
        keystore.and_then(|path| entries.iter().find(|e| e.path == path && !e.symlink));
    if keystore.is_some() && available_keystore.is_none() {
        return Err(
            "Newest encrypted plugin settings backup is unavailable; cleanup deferred".into(),
        );
    }
    let protected_keystore = available_keystore
        .filter(|entry| entry.size <= BYTES_LIMIT.saturating_sub(newest_bytes))
        .map(|entry| entry.path.clone());
    entries.sort_by(|a, b| {
        a.modified
            .cmp(&b.modified)
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut count = entries.len();
    // Reject accounting overflow rather than making an oversized recovery tree appear small
    let mut bytes = entries.iter().try_fold(0u64, |sum, e| {
        sum.checked_add(e.size).ok_or("Recovery size overflow")
    })?;
    for entry in entries {
        if !exceeds(count, bytes) {
            break;
        }
        if entry.path == newest || protected_keystore.as_ref() == Some(&entry.path) {
            continue;
        }
        // Cooperating writers are excluded by the owner lock; detect outside
        // changes rather than unlinking a replacement or traversing new links.
        verify_directory_chain(&database_directory, entry.path.parent())?;
        let metadata = entry.path.symlink_metadata().map_err(|e| e.to_string())?;
        if (!metadata.is_file() && !metadata.file_type().is_symlink())
            || metadata.len() != entry.size
            || metadata.modified().map_err(|e| e.to_string())? != entry.modified
            || is_redirect(&metadata) != entry.symlink
        {
            return Err("Recovery file changed during cleanup; retry later".into());
        }
        fs::remove_file(&entry.path).map_err(|e| e.to_string())?;
        crate::files::sync_directory(entry.path.parent().unwrap())?;
        count -= 1;
        bytes = bytes.saturating_sub(entry.size);
    }
    directories.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for candidate in directories {
        // Recheck the target and its ancestry too: a changed ancestor must not
        // redirect even an empty-directory removal outside the recovery tree.
        verify_directory_chain(&database_directory, Some(&candidate))?;
        // Only remove empty directories; never recursively delete unseen files
        match fs::remove_dir(&candidate) {
            Ok(()) => crate::files::sync_directory(candidate.parent().unwrap())?,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                ) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(if bytes > BYTES_LIMIT {
        Some("The newest recovery snapshot exceeds the 1 GiB budget; it was preserved".into())
    } else if keystore.is_some() && protected_keystore.is_none() {
        Some("The current database snapshot and encrypted plugin settings backup exceed the shared 1 GiB recovery budget".into())
    } else {
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Create the file used by the retention test
    fn file(path: &Path, size: u64, age: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::File::create(path).unwrap();
        file.set_len(size).unwrap();
        file.set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(age)),
        )
        .unwrap();
    }
    // Construct the isolated test fixture
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let newest = crate::recovery_directory(root.path()).join("newest.sqlite3");
        file(&newest, 1, 0); // Protected by identity, even when mtimes tie or skew.
        (root, newest)
    }
    #[test]
    // Verify nested unknown files and quarantine share the count budget
    fn nested_unknown_files_and_quarantine_share_the_count_budget() {
        let (root, newest) = fixture();
        for index in 0..33 {
            file(
                &crate::recovery_directory(root.path())
                    .join(format!("quarantine/old-{index:02}.evidence")),
                1,
                index + 1,
            );
        }
        assert!(prune(root.path(), &newest).unwrap().is_none());
        assert!(newest.exists());
        assert!(!crate::recovery_directory(root.path())
            .join("quarantine/old-00.evidence")
            .exists());
        assert!(!crate::recovery_directory(root.path())
            .join("quarantine/old-01.evidence")
            .exists());
        assert!(crate::recovery_directory(root.path())
            .join("quarantine/old-02.evidence")
            .exists());
        assert_eq!(
            fs::read_dir(crate::recovery_directory(root.path()).join("quarantine"))
                .unwrap()
                .count(),
            31
        );
    }
    #[test]
    fn newest_encrypted_keystore_shares_retention_without_age_eviction() {
        let (root, newest) = fixture();
        let encrypted =
            crate::recovery_directory(root.path()).join("plugin-settings-new.encrypted");
        file(&encrypted, 12, 0);
        for index in 0..40 {
            file(
                &crate::recovery_directory(root.path()).join(format!("older-{index:02}")),
                1,
                index + 1,
            );
        }
        prune_protected(root.path(), &newest, Some(&encrypted)).unwrap();
        assert!(newest.exists() && encrypted.exists());
        assert_eq!(
            fs::read_dir(crate::recovery_directory(root.path()))
                .unwrap()
                .count(),
            FILE_LIMIT
        );

        file(&newest, BYTES_LIMIT, 0);
        assert!(prune_protected(root.path(), &newest, Some(&encrypted))
            .unwrap()
            .unwrap()
            .contains("shared"));
        assert!(newest.exists());
        assert!(!encrypted.exists());
    }

    #[test]
    // Verify size budget uses all files and keeps oversized newest
    fn size_budget_uses_all_files_and_keeps_oversized_newest() {
        let (root, newest) = fixture();
        let old = crate::recovery_directory(root.path()).join("quarantine/old.bin");
        file(&old, BYTES_LIMIT, 1);
        assert!(prune(root.path(), &newest).unwrap().is_none());
        assert!(!old.exists());
        assert!(!old.parent().unwrap().exists());
        file(&newest, BYTES_LIMIT + 1, 2);
        assert!(prune(root.path(), &newest)
            .unwrap()
            .unwrap()
            .contains("preserved"));
        assert_eq!(fs::metadata(&newest).unwrap().len(), BYTES_LIMIT + 1);
    }
    #[test]
    // Verify unfinished restore and incomplete scan preserve everything
    fn unfinished_restore_and_incomplete_scan_preserve_everything() {
        let (root, newest) = fixture();
        let old = crate::recovery_directory(root.path()).join("quarantine/old.bin");
        file(&old, BYTES_LIMIT, 1);
        fs::write(
            root.path().join(crate::schema::RECOVERY_PENDING),
            b"malformed pending marker",
        )
        .unwrap();
        assert!(prune(root.path(), &newest)
            .unwrap()
            .unwrap()
            .contains("deferred"));
        assert!(old.exists());
        fs::remove_file(root.path().join(crate::schema::RECOVERY_PENDING)).unwrap();
        let mut deep = crate::recovery_directory(root.path());
        for _ in 0..66 {
            deep.push("nested");
        }
        fs::create_dir_all(deep).unwrap();
        assert!(prune(root.path(), &newest).is_err());
        assert!(old.exists());
    }
    #[test]
    // Verify interrupted restore defers the shared budget until completion
    fn interrupted_restore_defers_the_shared_budget_until_completion() {
        let (root, newest) = fixture();
        let quarantine = crate::recovery_directory(root.path()).join("quarantine-in-progress");
        for index in 0..40 {
            file(
                &quarantine.join(format!("evidence-{index:02}")),
                if index == 0 { BYTES_LIMIT } else { 1 },
                index + 1,
            );
        }
        let fence = root.path().join(crate::schema::RECOVERY_PENDING);
        fs::write(&fence, b"unfinished restore").unwrap();
        assert!(prune(root.path(), &newest)
            .unwrap()
            .unwrap()
            .contains("deferred"));
        assert_eq!(fs::read_dir(&quarantine).unwrap().count(), 40);
        assert_eq!(
            fs::metadata(quarantine.join("evidence-00")).unwrap().len(),
            BYTES_LIMIT
        );
        // Simulate the existing restore protocol clearing its fence on success
        fs::remove_file(fence).unwrap();
        assert!(prune(root.path(), &newest).unwrap().is_none());
        let mut entries = Vec::new();
        scan(
            &crate::recovery_directory(root.path()),
            &mut entries,
            &mut Vec::new(),
            0,
        )
        .unwrap();
        assert_eq!(entries.len(), FILE_LIMIT);
        assert!(entries.iter().map(|entry| entry.size).sum::<u64>() <= BYTES_LIMIT);
        assert!(newest.exists());
    }

    #[cfg(unix)]
    #[test]
    // Verify symlinks are unlinked without following external files or directories
    fn symlinks_are_unlinked_without_following_external_files_or_directories() {
        let (root, newest) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"untouched").unwrap();
        for index in 0..33 {
            let target = if index % 2 == 0 {
                outside.path().to_path_buf()
            } else {
                outside.path().join("keep")
            };
            std::os::unix::fs::symlink(
                target,
                crate::recovery_directory(root.path()).join(format!("link-{index:02}")),
            )
            .unwrap();
        }
        prune(root.path(), &newest).unwrap();
        assert_eq!(
            fs::read_dir(crate::recovery_directory(root.path()))
                .unwrap()
                .count(),
            32
        );
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"untouched");
        fs::create_dir(outside.path().join("empty")).unwrap();
        let redirected = crate::recovery_directory(root.path()).join("redirected");
        std::os::unix::fs::symlink(outside.path(), &redirected).unwrap();
        assert!(verify_directory_chain(
            &crate::recovery_directory(root.path()),
            Some(&redirected.join("empty"))
        )
        .is_err());
        assert!(outside.path().join("empty").is_dir());
        let alternate = tempfile::tempdir().unwrap();
        crate::database_layout::ensure_directory(alternate.path()).unwrap();
        std::os::unix::fs::symlink(outside.path(), crate::recovery_directory(alternate.path()))
            .unwrap();
        assert!(prune(alternate.path(), &outside.path().join("keep")).is_err());
    }
}
