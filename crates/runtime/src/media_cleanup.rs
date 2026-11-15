//! Reviewed, generation-checked media cleanup with recoverable quarantine transactions
use crate::{
    media_library::{read_regular, MediaScan},
    Runtime,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path, time::Instant};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MediaCleanupReview {
    pub id: String,
    pub generation: String,
    pub assets: Vec<String>,
    #[ts(type = "number")]
    pub logical_bytes: u64,
    #[ts(type = "number")]
    pub estimated_reclaimable_bytes: u64,
    pub storage_estimate_available: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MediaCleanupResult {
    pub removed: Vec<String>,
    #[ts(type = "number")]
    pub estimated_reclaimable_bytes: u64,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemovedAsset {
    id: String,
    extension: String,
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    committed: bool,
    files: Vec<RemovedAsset>,
}
pub(crate) struct StagedMediaCleanup {
    review: MediaCleanupReview,
    files: Vec<RemovedAsset>,
    created: Instant,
}
impl crate::review_store::ReviewSession for StagedMediaCleanup {
    const LIMIT: usize = 8;
    const FULL: &'static str = "Close another cleanup review first";
    const LIFETIME: Option<std::time::Duration> = Some(std::time::Duration::from_secs(300));
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
// Compute a content digest for media cleanup
fn digest(path: &Path, expected: u64) -> Result<String, String> {
    let bytes = read_regular(path, crate::media::MAX_ASSET)?;
    if bytes.len() as u64 != expected {
        return Err("Media changed during cleanup review".into());
    }
    Ok(hex::encode(Sha256::digest(bytes)))
}
// Create the cleanup directory if absent and reject symlinks or non-directories
fn checked_directory(path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Cleanup quarantine must be a regular managed directory".into());
    }
    Ok(())
}
// Validate journal
fn validate_journal(journal: &Journal) -> Result<(), String> {
    if journal.schema_version != 1 || journal.files.is_empty() || journal.files.len() > 256 {
        return Err("Invalid media cleanup journal bounds".into());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for file in &journal.files {
        total = total
            .checked_add(file.bytes)
            .filter(|n| *n <= 512 * 1024 * 1024)
            .ok_or("Media journal exceeds size limit")?;
        crate::profiles::validate_asset_id(&file.id)?;
        if !seen.insert(&file.id)
            || !crate::media::ASSET_EXTENSIONS.contains(&file.extension.as_str())
            || file.bytes > crate::media::MAX_ASSET as u64
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid media cleanup journal inventory".into());
        }
    }
    Ok(())
}
// Finalize journal
fn finish_journal(root: &Path, directory: &Path, journal: &Journal) -> Result<(), String> {
    validate_journal(journal)?;
    let metadata = directory.symlink_metadata().map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Unsafe media quarantine directory".into());
    }
    // Check the complete owned inventory before changing a file. Interrupted atomic JSON writes
    // can leave bounded NamedTempFile payloads; preserve unrecognized files and block recovery
    let allowed: BTreeSet<_> = journal
        .files
        .iter()
        .map(|f| format!("{}.{}", f.id, f.extension))
        .chain(["journal.json".into()])
        .collect();
    let mut temporaries = vec![];
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().to_string();
        let metadata = entry.path().symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Unexpected object in media quarantine; files preserved".into());
        }
        if !allowed.contains(&name) {
            if name.starts_with(".tmp") && metadata.len() <= 256 * 1024 {
                temporaries.push(entry.path());
            } else {
                return Err("Unexpected file in media quarantine; files preserved".into());
            }
        }
    }
    for file in &journal.files {
        let name = format!("{}.{}", file.id, file.extension);
        let source = directory.join(&name);
        let destination = root.join("assets").join(&name);
        match source.symlink_metadata() {
            Ok(_) => {
                if digest(&source, file.bytes)? != file.sha256 {
                    return Err("Quarantined media integrity mismatch; files preserved".into());
                }
                if !journal.committed && destination.symlink_metadata().is_ok() {
                    return Err("Media recovery destination is occupied; files preserved".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !journal.committed && digest(&destination, file.bytes)? != file.sha256 {
                    return Err("Media recovery source is missing; files preserved".into());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    for file in &journal.files {
        let name = format!("{}.{}", file.id, file.extension);
        let source = directory.join(&name);
        if source.symlink_metadata().is_ok() {
            if journal.committed {
                std::fs::remove_file(&source).map_err(|e| e.to_string())?;
            } else {
                std::fs::rename(&source, root.join("assets").join(name))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    for file in temporaries {
        std::fs::remove_file(file).map_err(|e| e.to_string())?;
    }
    std::fs::remove_file(directory.join("journal.json")).map_err(|e| e.to_string())?;
    std::fs::remove_dir(directory).map_err(|e| e.to_string())?;
    Ok(())
}
// Recover the selected media cleanup state
pub(crate) fn recover(root: &Path) -> Result<(), String> {
    let quarantine = root.join("media-cleanup");
    let metadata = match quarantine.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Unsafe media cleanup recovery directory".into());
    }
    let assets = root
        .join("assets")
        .symlink_metadata()
        .map_err(|e| e.to_string())?;
    if !assets.is_dir() || assets.file_type().is_symlink() {
        return Err("Media recovery needs a regular managed assets directory".into());
    }
    let mut entries = vec![];
    for entry in std::fs::read_dir(&quarantine).map_err(|e| e.to_string())? {
        entries.push(entry.map_err(|e| e.to_string())?);
        if entries.len() > 16 {
            return Err("Too many pending media cleanup journals; files preserved".into());
        }
    }
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        crate::profiles::validate_asset_id(&name)?;
        let directory = entry.path();
        let metadata = directory.symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Unsafe cleanup recovery entry".into());
        }
        if matches!(directory.join("journal.json").symlink_metadata(),Err(error) if error.kind()==std::io::ErrorKind::NotFound)
        {
            let mut incomplete = vec![];
            for item in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
                let item = item.map_err(|e| e.to_string())?;
                let metadata = item.path().symlink_metadata().map_err(|e| e.to_string())?;
                if !item.file_name().to_string_lossy().starts_with(".tmp")
                    || !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() > 256 * 1024
                    || incomplete.len() >= 16
                {
                    return Err("Cleanup journal is missing; quarantine files preserved".into());
                }
                incomplete.push(item.path());
            }
            for file in incomplete {
                std::fs::remove_file(file).map_err(|e| e.to_string())?;
            }
            std::fs::remove_dir(&directory).map_err(|e| e.to_string())?;
            continue;
        }
        let bytes = read_regular(&directory.join("journal.json"), 256 * 1024)?;
        let journal: Journal = serde_json::from_slice(&bytes)
            .map_err(|_| "Corrupt media cleanup journal; files preserved")?;
        finish_journal(root, &directory, &journal)?;
    }
    Ok(())
}
// Check for pending cleanup evidence only inside a validated cleanup directory
pub(crate) fn pending(root: &Path) -> Result<bool, String> {
    let directory = root.join("media-cleanup");
    match directory.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("Unsafe media cleanup directory".into());
            }
            Ok(std::fs::read_dir(directory)
                .map_err(|e| e.to_string())?
                .next()
                .is_some())
        }
    }
}
// Validate selected deletion candidates against protection and aggregate file limits
fn review_files(
    scan: &MediaScan,
    selected: &BTreeSet<String>,
) -> Result<Vec<RemovedAsset>, String> {
    let mut files = vec![];
    let mut bytes = 0u64;
    for id in selected {
        if scan.protection.contains_key(id) {
            return Err("Selected media is now protected. Refresh the library.".into());
        }
        let file = scan
            .files
            .get(id)
            .ok_or("Selected media no longer exists")?;
        bytes += file.bytes;
        if bytes > 512 * 1024 * 1024 {
            return Err("Review at most 512 MiB of media per cleanup".into());
        }
        files.push(RemovedAsset {
            id: id.clone(),
            extension: file.extension.clone(),
            bytes: file.bytes,
            sha256: digest(&file.path, file.bytes)?,
        });
    }
    Ok(files)
}
impl Runtime {
    // Prepare media cleanup
    pub fn prepare_media_cleanup(
        &self,
        generation: &str,
        assets: Vec<String>,
    ) -> Result<MediaCleanupReview, String> {
        let mut stages = self.media_cleanups.lock().unwrap();
        stages.check_capacity()?;
        if assets.len() > 256 {
            return Err("Choose at most 256 assets".into());
        }
        let selected: BTreeSet<_> = assets.iter().cloned().collect();
        if selected.is_empty() || selected.len() != assets.len() || selected.len() > 256 {
            return Err("Choose 1–256 distinct unused assets".into());
        }
        for id in &selected {
            crate::profiles::validate_asset_id(id)?;
        }
        let _transaction = self.profiles.transaction.lock().unwrap();
        let _media = self.profiles.media_gate.exclusive()?;
        let pins = self.profiles.asset_leases.lock();
        if pending(&self.profiles.root)? {
            return Err(
                "A media cleanup recovery is pending. Restart Manager before another cleanup."
                    .into(),
            );
        }
        let scan = self.profiles.scan_media(&pins);
        if !scan.errors.is_empty() {
            return Err(format!(
                "Media retention scan is incomplete: {}",
                scan.errors.join("; ")
            ));
        }
        if scan.generation != generation {
            return Err(
                "Media ownership changed. Refresh the library before reviewing cleanup.".into(),
            );
        }
        let files = review_files(&scan, &selected)?;
        let (estimated_reclaimable_bytes, storage_estimate_available) = scan.reclaimable(&selected);
        let review = MediaCleanupReview {
            id: crate::review_store::new_id(),
            generation: generation.into(),
            assets: assets.clone(),
            logical_bytes: files.iter().map(|file| file.bytes).sum(),
            estimated_reclaimable_bytes,
            storage_estimate_available,
        };
        stages.insert(
            review.id.clone(),
            StagedMediaCleanup {
                review: review.clone(),
                files,
                created: Instant::now(),
            },
        )?;
        Ok(review)
    }
    // Discard the selected media cleanup state
    pub fn discard_media_cleanup(&self, id: &str) {
        self.media_cleanups.lock().unwrap().remove(id);
    }
    // Commit media cleanup
    pub fn commit_media_cleanup(&self, id: &str) -> Result<MediaCleanupResult, String> {
        let mut stages = self.media_cleanups.lock().unwrap();
        let stage = stages.active(
            id,
            "Cleanup review expired",
            "Cleanup review expired. Review the selection again.",
        )?;
        let _transaction = self.profiles.transaction.lock().unwrap();
        let _media = self.profiles.media_gate.exclusive()?;
        let pins = self.profiles.asset_leases.lock();
        if pending(&self.profiles.root)? {
            return Err("Media cleanup recovery is pending; restart Manager".into());
        }
        let scan = self.profiles.scan_media(&pins);
        if !scan.errors.is_empty() || scan.generation != stage.review.generation {
            return Err(
                "Media ownership changed or retention is incomplete. Refresh and review again."
                    .into(),
            );
        }
        let selected = stage.review.assets.iter().cloned().collect();
        let verified = review_files(&scan, &selected)?;
        if verified.iter().zip(&stage.files).any(|(a, b)| {
            a.id != b.id || a.extension != b.extension || a.sha256 != b.sha256 || a.bytes != b.bytes
        }) {
            return Err("Media content changed. Refresh and review again.".into());
        }
        let quarantine = self.profiles.root.join("media-cleanup");
        checked_directory(&quarantine)?;
        let directory = quarantine.join(id);
        std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
        let mut journal = Journal {
            schema_version: 1,
            committed: false,
            files: stage.files.clone(),
        };
        if let Err(error) = crate::workspace::write_json(&directory.join("journal.json"), &journal)
        {
            let _ = std::fs::remove_dir_all(&directory);
            return Err(error);
        }
        let staged = (|| -> Result<(), String> {
            for file in &journal.files {
                let name = format!("{}.{}", file.id, file.extension);
                std::fs::rename(
                    self.profiles.root.join("assets").join(&name),
                    directory.join(name),
                )
                .map_err(|e| e.to_string())?;
            }
            journal.committed = true;
            crate::workspace::write_json(&directory.join("journal.json"), &journal)
        })();
        if let Err(error) = staged {
            journal.committed = false;
            let rollback = finish_journal(&self.profiles.root, &directory, &journal);
            return Err(match rollback {
                Ok(()) => format!("Cleanup was rolled back: {error}"),
                Err(recovery) => format!(
                    "Cleanup interrupted: {error}. Restart to recover; files preserved: {recovery}"
                ),
            });
        }
        let mut result = MediaCleanupResult {
            removed: stage.review.assets.clone(),
            estimated_reclaimable_bytes: stage.review.estimated_reclaimable_bytes,
            warnings: vec![],
        };
        if let Err(error) = self.profiles.finish_media_removal(&selected, &scan) {
            result
                .warnings
                .push(format!("Media removed; cache index update failed: {error}"));
        }
        if let Err(error) = finish_journal(&self.profiles.root, &directory, &journal) {
            result.estimated_reclaimable_bytes = 0;
            result.warnings.push(format!("Media removed from the library; quarantine cleanup will resume after restart: {error}"));
        }
        stages.remove(id);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ProfileStore;
    // Construct an isolated media cleanup fixture
    fn fixture() -> (tempfile::TempDir, ProfileStore, std::path::PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        store.load().unwrap();
        let file = root.path().join("image.png");
        std::fs::write(
            &file,
            hardware_core::render::tile(8, 8, [10, 20, 30], "", 0, "png").unwrap(),
        )
        .unwrap();
        let id = store.import_asset(&file).unwrap();
        (root, store, file, id)
    }
    // Move the unusable media cleanup artifact aside for inspection
    fn quarantine(store: &ProfileStore, id: &str, committed: bool) -> std::path::PathBuf {
        let source = store.root.join("assets").join(format!("{id}.png"));
        let bytes = source.metadata().unwrap().len();
        let file = RemovedAsset {
            id: id.into(),
            extension: "png".into(),
            bytes,
            sha256: digest(&source, bytes).unwrap(),
        };
        let parent = store.root.join("media-cleanup");
        checked_directory(&parent).unwrap();
        let directory = parent.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&directory).unwrap();
        crate::workspace::write_json(
            &directory.join("journal.json"),
            &Journal {
                schema_version: 1,
                committed,
                files: vec![file],
            },
        )
        .unwrap();
        std::fs::rename(source, directory.join(format!("{id}.png"))).unwrap();
        directory
    }
    #[test]
    // Verify that interrupted cleanup restores pending files and finishes committed removals
    fn interrupted_cleanup_restores_pending_files_and_finishes_committed_removals() {
        for committed in [false, true] {
            let (_root, store, _file, id) = fixture();
            let root = store.root.clone();
            let directory = quarantine(&store, &id, committed);
            drop(store);
            let recovered = ProfileStore::open(root.clone()).unwrap();
            assert_eq!(
                root.join("assets").join(format!("{id}.png")).is_file(),
                !committed
            );
            assert!(!directory.exists());
            assert!(!pending(&root).unwrap());
            drop(recovered);
        }
    }
    #[test]
    // Verify that corrupt quarantine and unsafe inventory are preserved
    fn corrupt_quarantine_and_unsafe_inventory_are_preserved() {
        let (_root, store, _file, id) = fixture();
        let root = store.root.clone();
        let directory = quarantine(&store, &id, false);
        let path = directory.join(format!("{id}.png"));
        std::fs::write(&path, b"altered").unwrap();
        drop(store);
        assert!(ProfileStore::open(root)
            .err()
            .unwrap()
            .contains("preserved"));
        assert_eq!(std::fs::read(path).unwrap(), b"altered");
        assert!(directory.join("journal.json").is_file());
    }
    #[test]
    // Verify that a crash before the initial journal does not block startup or move assets
    fn a_crash_before_the_initial_journal_does_not_block_startup_or_move_assets() {
        let (_root, store, _file, id) = fixture();
        let root = store.root.clone();
        let parent = root.join("media-cleanup");
        checked_directory(&parent).unwrap();
        let directory = parent.join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join(".tmpInterrupted"), b"partial journal").unwrap();
        drop(store);
        let recovered = ProfileStore::open(root.clone()).unwrap();
        assert!(recovered.asset(&id).is_ok());
        assert!(!directory.exists());
    }
    #[test]
    #[ignore = "Requires disposable native loopback host; deletes only owned test assets"]
    // Verify that cleanup rechecks leases and new drafts without changing workspace history
    fn cleanup_rechecks_leases_and_new_drafts_without_changing_workspace_history() {
        let (root, store, file, _id) = fixture();
        drop(store);
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let used = runtime.import_asset(&file).unwrap();
        let unused = runtime.import_asset(&file).unwrap();
        runtime.release_media(&used);
        runtime.release_media(&unused);
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().asset = Some(used.clone());
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        let before = serde_json::to_vec(&runtime.profiles.load().unwrap()).unwrap();
        let library = runtime.media_library(1, 12, "").unwrap();
        assert!(library.cleanup_available, "{:?}", library.scan_errors);
        let review = runtime
            .prepare_media_cleanup(&library.generation, vec![unused.clone()])
            .unwrap();
        let lease = runtime.retain_media(unused.clone()).unwrap();
        assert!(runtime
            .commit_media_cleanup(&review.id)
            .unwrap_err()
            .contains("ownership"));
        assert!(runtime.profiles.asset(&unused).is_ok());
        runtime.release_media(&lease);
        runtime.discard_media_cleanup(&review.id);
        let library = runtime.media_library(1, 12, "").unwrap();
        let review = runtime
            .prepare_media_cleanup(&library.generation, vec![unused.clone()])
            .unwrap();
        runtime
            .save_draft(
                "draft",
                Some(crate::workspace::Draft {
                    key: "draft".into(),
                    base: None,
                    value: serde_json::json!({"asset":unused}),
                }),
            )
            .unwrap();
        assert!(runtime.commit_media_cleanup(&review.id).is_err());
        runtime.save_draft("draft", None).unwrap();
        runtime.discard_media_cleanup(&review.id);
        let library = runtime.media_library(1, 12, "").unwrap();
        let review = runtime
            .prepare_media_cleanup(&library.generation, vec![unused.clone()])
            .unwrap();
        let result = runtime.commit_media_cleanup(&review.id).unwrap();
        assert_eq!(result.removed.as_slice(), std::slice::from_ref(&unused));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert!(runtime.profiles.asset(&unused).is_err());
        assert!(runtime.profiles.asset(&used).is_ok());
        assert_eq!(
            serde_json::to_vec(&runtime.profiles.load().unwrap()).unwrap(),
            before
        );
        let imported = runtime.import_asset(&file).unwrap();
        let scan = runtime
            .profiles
            .scan_media(&runtime.profiles.asset_leases.lock());
        assert!(scan.errors.is_empty(), "{:?}", scan.errors);
        assert_eq!(
            runtime.profiles.asset(&used).unwrap(),
            runtime.profiles.asset(&imported).unwrap()
        );
        // The inventory only claims physical hard-link identity where the platform
        // exposes it. Windows deliberately uses distinct asset IDs and unknown links
        #[cfg(unix)]
        assert_eq!(scan.files[&used].identity, scan.files[&imported].identity);
        #[cfg(not(unix))]
        {
            assert_eq!(scan.files[&used].identity, used);
            assert_eq!(scan.files[&imported].identity, imported);
            assert_eq!(scan.files[&used].links, None);
            assert_eq!(scan.files[&imported].links, None);
        }
        let index: serde_json::Value = serde_json::from_slice(
            &std::fs::read(runtime.profiles.root.join("media-content.json")).unwrap(),
        )
        .unwrap();
        assert!(index["sources"]
            .as_object()
            .unwrap()
            .values()
            .any(|value| value == &format!("{imported}.png")));
        assert_eq!(
            serde_json::to_vec(&runtime.profiles.load().unwrap()).unwrap(),
            before
        );
        runtime.shutdown();
    }
}
