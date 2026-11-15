//! Retention for owned update metadata and published plugin archives
//! Unknown files and user-facing manager installers are never cache-clear targets
use super::*;
use std::{collections::HashMap, fs, sync::MutexGuard};

const DOWNLOAD_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DOWNLOAD_MAX_ENTRIES: usize = 128;
const METADATA_MAX_ENTRIES: usize = 64;
const DOWNLOAD_MAX_AGE: u64 = 2 * 24 * 60 * 60;
const METADATA_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const METADATA_MAX_AGE: u64 = 24 * 60 * 60;
type Leases = Arc<Mutex<HashMap<PathBuf, usize>>>;

pub struct UpdateCacheLease {
    leases: Leases,
    path: PathBuf,
}
impl Drop for UpdateCacheLease {
    // Release this update cache owner when it leaves scope
    fn drop(&mut self) {
        let mut leases = self.leases.lock().unwrap();
        if let Some(count) = leases.get_mut(&self.path) {
            *count -= 1;
            if *count == 0 {
                leases.remove(&self.path);
            }
        }
    }
}

// Report the regular condition for update cache
fn regular(path: &Path) -> bool {
    path.symlink_metadata()
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
}
// Reject symlinks and nondirectory ancestors within the managed cache boundary
fn safe_ancestors(path: &Path, boundary: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        if ancestor == boundary.parent().unwrap_or(boundary) {
            break;
        }
        match ancestor.symlink_metadata() {
            Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
                return Err(format!("Unsafe cache directory: {}", ancestor.display()))
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(format!("{}: {e}", ancestor.display())),
        }
    }
    Ok(())
}
// Refresh the update cache entry’s activity marker
fn touch(path: &Path) -> Result<(), String> {
    if regular(path) {
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .and_then(|f| f.set_times(fs::FileTimes::new().set_modified(SystemTime::now())))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u32,
    name: String,
    size: u64,
}
// Derive the metadata receipt filename adjacent to a cached download
fn receipt_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        "{}.cache.json",
        path.file_name().unwrap().to_string_lossy()
    ))
}

impl UpdateService {
    /// Keep a source alive while it is being read/copied. No filesystem lock is held
    /// during network or plugin operations, so unrelated entries can still expire
    pub fn cache_lease(&self, path: &Path) -> Result<UpdateCacheLease, String> {
        let mut leases = self.cache_leases.lock().unwrap();
        // A manually selected file may use a filesystem alias of our cache. Map
        // that alias back to the scanned path, but do not change unrelated inputs
        // Validate the lexical owned path first: canonicalization must not hide a
        // symlink that replaced an owned directory or cache file
        if path.starts_with(&self.root) {
            safe_ancestors(path.parent().ok_or("Missing cache parent")?, &self.root)?;
            if path
                .symlink_metadata()
                .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
            {
                return Err("Cache source is not a regular file".into());
            }
        }
        let resolved = match path.canonicalize() {
            Ok(canonical) => self
                .root
                .canonicalize()
                .ok()
                .and_then(|root| {
                    canonical
                        .strip_prefix(root)
                        .ok()
                        .map(|relative| self.root.join(relative))
                })
                .unwrap_or(canonical),
            // Publication targets are not created yet and already come from our
            // validated product/hash/name layout
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_owned(),
            Err(error) => return Err(error.to_string()),
        };
        let path = resolved.as_path();
        if path.starts_with(&self.root) {
            safe_ancestors(path.parent().ok_or("Missing cache parent")?, &self.root)?;
            if path
                .symlink_metadata()
                .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
            {
                return Err("Cache source is not a regular file".into());
            }
            touch(path)?;
        }
        *leases.entry(path.to_owned()).or_default() += 1;
        Ok(UpdateCacheLease {
            leases: self.cache_leases.clone(),
            path: path.to_owned(),
        })
    }
    // Acquire a cleanup lease for a completed download and recheck that the file still exists
    pub fn leased_completed_download(
        &self,
        id: &str,
    ) -> Result<(PathBuf, UpdateCacheLease), String> {
        let path = self.completed_download(id)?;
        let lease = self.cache_lease(&path)?;
        if !regular(&path) {
            return Err("Cached download expired; download it again".into());
        }
        Ok((path, lease))
    }
    // Record cached download
    pub(super) fn record_cached_download(&self, path: &Path) -> Result<(), String> {
        let receipt = receipt_path(path);
        if receipt.symlink_metadata().is_ok() && !regular(&receipt) {
            return Err("Unsafe download cache receipt".into());
        }
        atomic_json(
            &receipt,
            &Receipt {
                version: 1,
                name: path
                    .file_name()
                    .ok_or("Missing archive name")?
                    .to_string_lossy()
                    .into(),
                size: path.metadata().map_err(|e| e.to_string())?.len(),
            },
        )
    }
    // Cache folders
    pub(crate) fn cache_folders(
        &self,
        clear: Option<bool>,
    ) -> (
        Vec<crate::cache::CacheFolderStatus>,
        usize,
        u64,
        Vec<String>,
    ) {
        let leases = self.cache_leases.lock().unwrap();
        let mut errors = Vec::new();
        let mut removed_entries = 0;
        let mut removed_bytes = 0;
        let mut statuses = Vec::new();
        for (folder, label, max_bytes, max_age, max_entries) in [
            (
                "cache",
                "Update metadata",
                METADATA_MAX_BYTES,
                METADATA_MAX_AGE,
                METADATA_MAX_ENTRIES,
            ),
            (
                "downloads",
                "Update downloads",
                DOWNLOAD_MAX_BYTES,
                DOWNLOAD_MAX_AGE,
                DOWNLOAD_MAX_ENTRIES,
            ),
        ] {
            let root = self.root.join(folder);
            let mut entries = Vec::new();
            if let Err(e) = safe_ancestors(&root, &self.root) {
                errors.push(e);
            } else {
                scan(
                    &root,
                    folder == "cache",
                    0,
                    &leases,
                    &mut entries,
                    &mut errors,
                );
            }
            entries.sort_by_key(|e| (e.used, e.path.clone()));
            let mut bytes: u64 = entries.iter().map(|e| e.bytes).sum();
            let mut count = entries.len();
            for entry in &entries {
                if !entry.protected
                    && clear.is_some()
                    && (clear == Some(true)
                        || now().saturating_sub(entry.used) >= max_age
                        || bytes > max_bytes
                        || count > max_entries)
                {
                    match fs::remove_file(&entry.path) {
                        Ok(()) => {
                            // Receipts are counted with their archive, not as separate entries
                            let mut deleted_bytes = entry.bytes;
                            if let Some(receipt) = &entry.receipt {
                                if let Err(e) = fs::remove_file(receipt) {
                                    deleted_bytes = entry.payload_bytes;
                                    errors.push(format!("{}: {e}", receipt.display()));
                                }
                            }
                            bytes = bytes.saturating_sub(deleted_bytes);
                            count -= 1;
                            for (job, _) in self.jobs.lock().unwrap().values_mut() {
                                if job.operation == "download"
                                    && job.state == "complete"
                                    && job
                                        .result
                                        .as_deref()
                                        .is_some_and(|p| Path::new(p) == entry.path)
                                {
                                    job.state = "failed".into();
                                    job.error = Some(
                                        "Cached download expired or was cleared; download again"
                                            .into(),
                                    );
                                    job.result = None;
                                    job.retry_available = Some(true);
                                }
                            }
                            removed_entries += 1;
                            removed_bytes += deleted_bytes;
                        }
                        Err(e) => errors.push(format!("{}: {e}", entry.path.display())),
                    }
                }
            }
            // Re-scan to report actual remaining usage, including failed deletions
            let mut remaining = Vec::new();
            if safe_ancestors(&root, &self.root).is_ok() {
                scan(
                    &root,
                    folder == "cache",
                    0,
                    &leases,
                    &mut remaining,
                    &mut errors,
                );
            }
            statuses.push(crate::cache::CacheFolderStatus {
                id: format!("updates/{folder}"),
                label: label.into(),
                bytes: remaining.iter().map(|e| e.bytes).sum(),
                entries: remaining.len(),
                max_bytes,
                max_entries,
                max_age_seconds: max_age,
                protected_entries: remaining.iter().filter(|e| e.protected).count(),
            });
        }
        (statuses, removed_entries, removed_bytes, errors)
    }
}
struct Entry {
    path: PathBuf,
    receipt: Option<PathBuf>,
    bytes: u64,
    payload_bytes: u64,
    used: u64,
    protected: bool,
}
// Scan the available update cache entries
fn scan(
    root: &Path,
    metadata: bool,
    depth: usize,
    leases: &MutexGuard<'_, HashMap<PathBuf, usize>>,
    entries: &mut Vec<Entry>,
    errors: &mut Vec<String>,
) {
    let children = match fs::read_dir(root) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            errors.push(format!("{}: {e}", root.display()));
            return;
        }
    };
    for child in children {
        let child = match child {
            Ok(c) => c,
            Err(e) => {
                errors.push(e.to_string());
                continue;
            }
        };
        let path = child.path();
        let info = match path.symlink_metadata() {
            Ok(m) => m,
            Err(e) => {
                errors.push(e.to_string());
                continue;
            }
        };
        let name = child.file_name().to_string_lossy().into_owned();
        if !metadata
            && info.is_dir()
            && !info.file_type().is_symlink()
            && ((depth == 0 && matches!(name.as_str(), "plugins" | "manager"))
                || (depth == 1 && valid_hash(&name)))
        {
            scan(&path, false, depth + 1, leases, entries, errors);
            continue;
        }
        if info.file_type().is_symlink() {
            errors.push(format!(
                "Symbolic link preserved; target usage is excluded: {}",
                path.display()
            ));
        }
        let mut protected = true;
        let mut receipt = None;
        let mut bytes = if info.is_dir() && !info.file_type().is_symlink() {
            protected_size(&path, errors)
        } else {
            info.len()
        };
        if info.is_file() && !info.file_type().is_symlink() {
            if metadata {
                let stem = name.strip_suffix(".json").unwrap_or("");
                let asset = stem.strip_prefix("asset-");
                if valid_hash(asset.unwrap_or(stem)) && info.len() <= 16 * 1024 * 1024 {
                    match fs::read(&path) {
                        Ok(b) => {
                            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) {
                                protected = !(v.as_object().is_some_and(|o| o.len() == 2)
                                    && v["etag"].is_string()
                                    && if asset.is_some() {
                                        serde_json::from_value::<MetadataAssetCache>(v).is_ok()
                                    } else {
                                        v.get("data").is_some()
                                    });
                            }
                        }
                        Err(e) => errors.push(format!("{}: {e}", path.display())),
                    }
                }
            } else if depth == 2 {
                // Only the exact archive named by a valid receipt is owned
                let receipt_file = receipt_path(&path);
                if let Some(r) = read_receipt(&receipt_file) {
                    if r.version == 1 && r.name == name && r.size == info.len() && safe_name(&name)
                    {
                        bytes += receipt_file
                            .symlink_metadata()
                            .map(|m| m.len())
                            .unwrap_or(0);
                        receipt = Some(receipt_file);
                        protected = root
                            .parent()
                            .and_then(Path::file_name)
                            .is_none_or(|p| p != "plugins");
                    }
                }
                if name.ends_with(".cache.json") {
                    if let Some(r) = read_receipt(&path) {
                        let archive = root.join(&r.name);
                        if r.version == 1
                            && safe_name(&r.name)
                            && receipt_path(&archive) == path
                            && regular(&archive)
                            && archive.metadata().is_ok_and(|m| m.len() == r.size)
                        {
                            continue;
                        }
                    }
                }
            }
        }
        protected |= leases.contains_key(&path);
        entries.push(Entry {
            path,
            receipt,
            bytes,
            payload_bytes: info.len(),
            used: info
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or_else(now),
            protected,
        });
    }
}
// Include unknown directory contents in usage without following links or deleting them
fn protected_size(path: &Path, errors: &mut Vec<String>) -> u64 {
    let mut pending = vec![path.to_owned()];
    let mut bytes = 0u64;
    while let Some(directory) = pending.pop() {
        let children = match fs::read_dir(&directory) {
            Ok(c) => c,
            Err(e) => {
                errors.push(format!("{}: {e}", directory.display()));
                continue;
            }
        };
        for child in children {
            let child = match child {
                Ok(c) => c,
                Err(e) => {
                    errors.push(e.to_string());
                    continue;
                }
            };
            match child.path().symlink_metadata() {
                Ok(m) if m.is_dir() && !m.file_type().is_symlink() => pending.push(child.path()),
                Ok(m) => {
                    if m.file_type().is_symlink() {
                        errors.push(format!(
                            "Symbolic link preserved; target usage is excluded: {}",
                            child.path().display()
                        ));
                    }
                    bytes = bytes.saturating_add(m.len());
                }
                Err(e) => errors.push(format!("{}: {e}", child.path().display())),
            }
        }
    }
    bytes
}
// Read receipt
fn read_receipt(path: &Path) -> Option<Receipt> {
    if !regular(path) || path.symlink_metadata().ok()?.len() > 4096 {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Create an update service inside an isolated temporary test directory
    fn service() -> (tempfile::TempDir, Arc<UpdateService>) {
        let root = tempfile::tempdir().unwrap();
        let service =
            UpdateService::open(root.path().canonicalize().unwrap().join("updates")).unwrap();
        (root, service)
    }
    // Write a cached metadata test entry with the requested modification age
    fn metadata(service: &UpdateService, n: usize, age: u64) -> PathBuf {
        let path = service.root.join("cache").join(format!("{n:064x}.json"));
        fs::write(&path, br#"{"etag":"fixture","data":[]}"#).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(now() - age)),
            )
            .unwrap();
        path
    }
    // Construct the update cache archive
    fn archive(service: &UpdateService, product: &str) -> PathBuf {
        let directory = service
            .root
            .join("downloads")
            .join(product)
            .join("a".repeat(64));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("fixture.zip");
        fs::write(&path, b"archive").unwrap();
        service.record_cached_download(&path).unwrap();
        path
    }
    #[test]
    // Verify that metadata limits are independent of download limits
    fn metadata_limits_are_independent_of_download_limits() {
        let (_root, service) = service();
        let (stats, count, bytes, errors) = service.cache_folders(None);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((count, bytes), (0, 0));
        assert_eq!(stats[0].id, "updates/cache");
        assert_eq!(
            (
                stats[0].max_bytes,
                stats[0].max_age_seconds,
                stats[0].max_entries
            ),
            (1_073_741_824, 86_400, 64)
        );
        assert_eq!(stats[1].id, "updates/downloads");
        assert_eq!(
            (
                stats[1].max_bytes,
                stats[1].max_age_seconds,
                stats[1].max_entries
            ),
            (4_294_967_296, 172_800, 128)
        );
    }
    #[test]
    // Verify that one day expires metadata but preserves update downloads
    fn one_day_expires_metadata_but_preserves_update_downloads() {
        let (_root, service) = service();
        let expired = metadata(&service, 1, 86_400);
        let plugin = archive(&service, "plugins");
        let file = fs::OpenOptions::new().write(true).open(&plugin).unwrap();
        file.set_len(1_073_741_825).unwrap();
        service.record_cached_download(&plugin).unwrap();
        file.set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(now() - 86_400)),
        )
        .unwrap();
        let (_, count, _, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 1);
        assert!(!expired.exists());
        assert!(plugin.exists());
        file.set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(now() - 172_800)),
        )
        .unwrap();
        assert_eq!(service.cache_folders(Some(false)).1, 1);
        assert!(!plugin.exists());
    }
    #[test]
    // Verify that metadata byte limit prunes owned entries and preserves unknown usage
    fn metadata_byte_limit_prunes_owned_entries_and_preserves_unknown_usage() {
        let (_root, service) = service();
        let owned = metadata(&service, 1, 0);
        let unknown = service.root.join("cache/unknown.bin");
        fs::File::create(&unknown)
            .unwrap()
            .set_len(1_073_741_824)
            .unwrap();
        let (stats, count, _, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 1);
        assert!(!owned.exists());
        assert!(unknown.exists());
        assert_eq!(stats[0].bytes, 1_073_741_824);
        assert_eq!(stats[0].protected_entries, 1);
    }
    #[test]
    // Verify that update download count retains 128 entries
    fn update_download_count_retains_128_entries() {
        let (_root, service) = service();
        let first = archive(&service, "plugins");
        for n in 1..128 {
            let path = first.with_file_name(format!("fixture-{n}.zip"));
            fs::write(&path, b"archive").unwrap();
            service.record_cached_download(&path).unwrap();
        }
        let (stats, count, _, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 0);
        assert_eq!(stats[1].entries, 128);
        let extra = first.with_file_name("extra.zip");
        fs::write(&extra, b"archive").unwrap();
        service.record_cached_download(&extra).unwrap();
        let (stats, count, _, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 1);
        assert_eq!(stats[1].entries, 128);
    }
    #[test]
    // Verify that age count and clear preserve unknown metadata
    fn age_count_and_clear_preserve_unknown_metadata() {
        let (_root, service) = service();
        let expired = metadata(&service, 1, METADATA_MAX_AGE);
        let fresh = metadata(&service, 2, METADATA_MAX_AGE - 10);
        let unknown = service.root.join("cache/notes.json");
        fs::write(&unknown, b"private").unwrap();
        let (_, count, _, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 1);
        assert!(!expired.exists());
        assert!(fresh.exists());
        for n in 3..70 {
            metadata(&service, n, 0);
        }
        let (stats, _, _, _) = service.cache_folders(Some(false));
        assert_eq!(stats[0].entries, METADATA_MAX_ENTRIES);
        let (stats, _, _, _) = service.cache_folders(Some(true));
        assert_eq!(stats[0].entries, 1);
        assert_eq!(stats[0].protected_entries, 1);
        assert!(unknown.exists());
    }
    #[test]
    // Verify that clear keeps installer and active archive then releases plugin
    fn clear_keeps_installer_and_active_archive_then_releases_plugin() {
        let (_root, service) = service();
        let manager = archive(&service, "manager");
        let plugin = archive(&service, "plugins");
        let lease = service.cache_lease(&plugin).unwrap();
        let other = service.clone();
        let (stats, count, _, errors) = std::thread::spawn(move || other.cache_folders(Some(true)))
            .join()
            .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 0);
        assert_eq!(stats[1].protected_entries, 2);
        drop(lease);
        let (_, count, _, _) = service.cache_folders(Some(true));
        assert_eq!(count, 1);
        assert!(!plugin.exists());
        assert!(manager.exists());
        assert_eq!(service.cache_folders(Some(true)).1, 0);
    }
    #[test]
    // Verify that legacy archives and malformed receipts remain protected
    fn legacy_archives_and_malformed_receipts_remain_protected() {
        let (_root, service) = service();
        let plugin = archive(&service, "plugins");
        fs::write(receipt_path(&plugin), b"{}").unwrap();
        let (_, count, _, _) = service.cache_folders(Some(true));
        assert_eq!(count, 0);
        assert!(plugin.exists());
    }
    #[test]
    // Verify that byte limit prunes oldest owned archive and invalidates completed job
    fn byte_limit_prunes_oldest_owned_archive_and_invalidates_completed_job() {
        let (_root, service) = service();
        let plugin = archive(&service, "plugins");
        fs::OpenOptions::new()
            .write(true)
            .open(&plugin)
            .unwrap()
            .set_len(DOWNLOAD_MAX_BYTES + 1)
            .unwrap();
        service.record_cached_download(&plugin).unwrap();
        let (id, _) = service.job("plugins", "download").unwrap();
        service.finish(&id, Ok(Some(plugin.to_string_lossy().into_owned())));
        let (_, count, bytes, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 1);
        assert!(bytes > DOWNLOAD_MAX_BYTES);
        let jobs = service.jobs.lock().unwrap();
        assert_eq!(jobs[&id].0.state, "failed");
        assert_eq!(jobs[&id].0.retry_available, Some(true));
    }
    #[test]
    // Verify that metadata lease is reference counted and touches last use
    fn metadata_lease_is_reference_counted_and_touches_last_use() {
        let (_root, service) = service();
        let path = metadata(&service, 1, METADATA_MAX_AGE + 1);
        let first = service.cache_lease(&path).unwrap();
        let second = service.cache_lease(&path).unwrap();
        drop(first);
        assert_eq!(service.cache_folders(Some(true)).1, 0);
        drop(second);
        assert_eq!(service.cache_folders(Some(false)).1, 0);
        assert_eq!(service.cache_folders(Some(true)).1, 1);
    }
    #[test]
    // Verify that parent component alias uses the scanned lease key
    fn parent_component_alias_uses_the_scanned_lease_key() {
        let (_root, service) = service();
        let plugin = archive(&service, "plugins");
        let parent = plugin.parent().unwrap();
        let alias = parent
            .join("..")
            .join(parent.file_name().unwrap())
            .join(plugin.file_name().unwrap());
        let lease = service.cache_lease(&alias).unwrap();
        assert_eq!(lease.path, plugin);
        assert_eq!(service.cache_folders(Some(true)).1, 0);
        assert!(plugin.exists());
        drop(lease);
        assert_eq!(service.cache_folders(Some(true)).1, 1);
    }
    #[cfg(unix)]
    #[test]
    // Verify that external symlink alias is leased but owned symlinks are rejected
    fn external_symlink_alias_is_leased_but_owned_symlinks_are_rejected() {
        let (_root, service) = service();
        let plugin = archive(&service, "plugins");
        let external = tempfile::tempdir().unwrap();
        let alias = external.path().join("package.zip");
        std::os::unix::fs::symlink(&plugin, &alias).unwrap();
        let lease = service.cache_lease(&alias).unwrap();
        assert_eq!(lease.path, plugin);
        assert_eq!(service.cache_folders(Some(true)).1, 0);
        let owned_alias = plugin.with_file_name("alias.zip");
        std::os::unix::fs::symlink(&plugin, &owned_alias).unwrap();
        assert!(service.cache_lease(&owned_alias).is_err());
        drop(lease);
        assert_eq!(service.cache_folders(Some(true)).1, 1);
        assert!(alias.symlink_metadata().is_ok());
    }
    #[cfg(unix)]
    #[test]
    // Verify that symlinks are never followed or removed
    fn symlinks_are_never_followed_or_removed() {
        let (_root, service) = service();
        let external = tempfile::tempdir().unwrap();
        let file = external.path().join("private");
        fs::write(&file, b"private").unwrap();
        std::os::unix::fs::symlink(
            &file,
            service
                .root
                .join("cache")
                .join(format!("{}.json", "b".repeat(64))),
        )
        .unwrap();
        service.cache_folders(Some(true));
        assert!(file.exists());
        fs::remove_dir(service.root.join("downloads")).unwrap();
        std::os::unix::fs::symlink(external.path(), service.root.join("downloads")).unwrap();
        assert!(!service.cache_folders(Some(true)).3.is_empty());
        assert!(file.exists());
    }
}
