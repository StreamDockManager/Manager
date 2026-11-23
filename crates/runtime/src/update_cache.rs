//! Retention for owned update metadata and published plugin archives
//! Unknown files and user-facing manager installers are never cache-clear targets
use super::*;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::ErrorKind,
};
use stream_dock_manager_storage::cache_index::{self, Limits, Record, Removal, Table};

const DOWNLOAD_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DOWNLOAD_MAX_ENTRIES: usize = 128;
const METADATA_MAX_ENTRIES: usize = 64;
const DOWNLOAD_MAX_AGE: u64 = 2 * 24 * 60 * 60;
const METADATA_MAX_BYTES: u64 = 512 * 1024 * 1024;
const METADATA_MAX_AGE: u64 = 24 * 60 * 60;
type Leases = Arc<Mutex<HashMap<PathBuf, usize>>>;

pub struct UpdateCacheLease {
    leases: Leases,
    indexed: Option<(Table, String)>,
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
                if let Some((table, key)) = &self.indexed {
                    let _ = table.touch(key, false);
                }
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
            Ok(m) if !plain_directory(&m) => {
                return Err(format!("Unsafe cache directory: {}", ancestor.display()))
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(format!("{}: {e}", ancestor.display())),
        }
    }
    Ok(())
}
// Do not remove directory links or Windows junctions even when their targets are empty
fn plain_directory(info: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    info.is_dir() && !info.file_type().is_symlink()
}

// Remove only empty digest folders while holding the update-cache lease registry
fn remove_empty_plugin_download_dirs(
    root: &Path,
    all: bool,
    mut candidates: HashSet<PathBuf>,
    leases: &HashMap<PathBuf, usize>,
) -> Result<Vec<String>, String> {
    let plugins = root.join("plugins");
    match plugins.symlink_metadata() {
        Ok(info) if plain_directory(&info) => {}
        Ok(_) => return Ok(vec![]),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(format!("{}: {error}", plugins.display())),
    }
    let mut errors = Vec::new();
    if all {
        for entry in fs::read_dir(&plugins).map_err(|e| format!("{}: {e}", plugins.display()))? {
            match entry {
                Ok(entry) if valid_hash(&entry.file_name().to_string_lossy()) => {
                    candidates.insert(entry.path());
                }
                Ok(_) => {}
                Err(error) => errors.push(format!("{}: {error}", plugins.display())),
            }
        }
    }
    let active_parents: HashSet<_> = leases.keys().filter_map(|path| path.parent()).collect();
    for directory in candidates {
        if active_parents.contains(directory.as_path()) {
            continue;
        }
        match directory.symlink_metadata() {
            Ok(info) if plain_directory(&info) => {}
            Ok(_) => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                errors.push(format!("{}: {error}", directory.display()));
                continue;
            }
        }
        // A nonrecursive removal preserves any archive, receipt, or unknown child
        match fs::remove_dir(&directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(error) => errors.push(format!("{}: {error}", directory.display())),
        }
    }
    Ok(errors)
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
        }
        let indexed = self.indexed_path(path);
        if let Some((table, key)) = &indexed {
            if !table.touch(key, true)? {
                // A released Manager may have written the file before the first import.
                // Preserve this access time even if its legacy mtime is old.
                if let Ok(info) = path.symlink_metadata() {
                    let metadata = path.starts_with(self.root.join("cache"));
                    if info.is_file() {
                        if let Some(entry) = inspect(
                            path.to_owned(),
                            info,
                            metadata,
                            key.split('/').count() - 1,
                            &mut Vec::new(),
                        ) {
                            let mut record = Record::file(key.clone(), entry.bytes);
                            record.active = true;
                            record.owned = !entry.protected;
                            table.record(&record)?;
                        }
                    }
                }
            }
        }
        *leases.entry(path.to_owned()).or_default() += 1;
        Ok(UpdateCacheLease {
            indexed,
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
    fn indexed_path(&self, path: &Path) -> Option<(Table, String)> {
        for (folder, table) in [
            ("cache", &self.metadata_index),
            ("downloads", &self.package_index),
        ] {
            if let Ok(rel) = path.strip_prefix(self.root.join(folder)) {
                let key = rel.to_str()?.replace(std::path::MAIN_SEPARATOR, "/");
                if cache_index::safe_relative(&key) {
                    return Some((table.clone(), key));
                }
            }
        }
        None
    }
    pub(super) fn publish_metadata(
        &self,
        path: &Path,
        value: &impl Serialize,
    ) -> Result<(), String> {
        let (table, key) = self.indexed_path(path).ok_or("Invalid metadata path")?;
        let leases = self.cache_leases.lock().unwrap();
        publish_json(path, value, |bytes| {
            let mut r = Record::file(key, bytes);
            r.active = leases.contains_key(path);
            table.record(&r)
        })
    }
    // Record cached download
    pub(super) fn record_cached_download(&self, path: &Path) -> Result<(), String> {
        let receipt = receipt_path(path);
        if receipt.symlink_metadata().is_ok() && !regular(&receipt) {
            return Err("Unsafe download cache receipt".into());
        }
        let (table, key) = self
            .indexed_path(path)
            .ok_or("Invalid download cache path")?;
        let leases = self.cache_leases.lock().unwrap();
        let size = path.metadata().map_err(|e| e.to_string())?.len();
        let receipt_data = Receipt {
            version: 1,
            name: path
                .file_name()
                .ok_or("Missing archive name")?
                .to_string_lossy()
                .into(),
            size,
        };
        publish_json(&receipt, &receipt_data, |receipt_bytes| {
            let mut r = Record::file(key, size + receipt_bytes);
            r.owned = r.path.starts_with("plugins/");
            r.active = leases.contains_key(path);
            table.record(&r)
        })
    }
    fn remove_cached_entry(
        &self,
        root: &Path,
        metadata: bool,
        key: &str,
        leases: &HashMap<PathBuf, usize>,
    ) -> Result<Removal, String> {
        let parts: Vec<_> = key.split('/').collect();
        if !(metadata && parts.len() == 1
            || !metadata
                && parts.len() == 3
                && parts[0] == "plugins"
                && valid_hash(parts[1])
                && safe_name(parts[2]))
        {
            return Ok(Removal::Protected);
        }
        let path = root.join(key);
        if leases.contains_key(&path) {
            return Ok(Removal::Busy);
        }
        safe_ancestors(path.parent().ok_or("Missing parent")?, &self.root)?;
        let info = match path.symlink_metadata() {
            Ok(m) if m.is_file() && !m.file_type().is_symlink() => m,
            Ok(_) => return Ok(Removal::Protected),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !metadata {
                    let sidecar = receipt_path(&path);
                    if read_receipt(&sidecar).is_some_and(|r| r.version == 1 && r.name == parts[2])
                    {
                        fs::remove_file(sidecar).map_err(|e| e.to_string())?;
                    }
                }
                return Ok(Removal::Missing);
            }
            Err(e) => return Err(e.to_string()),
        };
        let mut issues = vec![];
        let Some(entry) = inspect(path.clone(), info, metadata, parts.len() - 1, &mut issues)
        else {
            return Ok(Removal::Protected);
        };
        if !issues.is_empty() {
            return Err(issues.join("; "));
        }
        if entry.protected {
            return Ok(Removal::Protected);
        }
        if entry.receipt.is_some() {
            self.package_index.hint(key)?;
        }
        fs::remove_file(&path).map_err(|e| e.to_string())?;
        for (job, _) in self.jobs.lock().unwrap().values_mut() {
            if job.operation == "download"
                && job.state == "complete"
                && job
                    .result
                    .as_deref()
                    .is_some_and(|p| Path::new(p) == entry.path)
            {
                job.state = "failed".into();
                job.error = Some("Cached download expired or was cleared; download again".into());
                job.result = None;
                job.retry_available = Some(true);
            }
        }
        if let Some(receipt) = entry.receipt {
            fs::remove_file(&receipt).map_err(|e| e.to_string())?;
        }
        Ok(Removal::Removed)
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
            let table = if folder == "cache" {
                &self.metadata_index
            } else {
                &self.package_index
            };
            let result = (|| -> Result<cache_index::Report, String> {
                safe_ancestors(&root, &self.root)?;
                table.import_once(|| {
                    let mut entries = vec![];
                    scan(&root, folder == "cache", 0, &mut entries, &mut errors);
                    let mut records = vec![];
                    for entry in entries {
                        if let Some((_, key)) = self.indexed_path(&entry.path) {
                            let mut r = Record::file(key, entry.bytes);
                            r.used = (entry.used * 1000) as i64;
                            r.owned = !entry.protected;
                            r.active = leases.contains_key(&entry.path);
                            records.push(r);
                        }
                    }
                    Ok(records)
                })?;
                table.recover_activity(|key| Ok(leases.contains_key(&root.join(key))))?;
                let Some(clear) = clear else {
                    return Ok(cache_index::Report {
                        stats: table.stats()?,
                        ..Default::default()
                    });
                };
                let metadata = folder == "cache";
                let mut emptied = HashSet::new();
                let mut report = table.purge(
                    Limits {
                        bytes: max_bytes,
                        entries: Some(max_entries),
                        age: Some(Duration::from_secs(max_age)),
                    },
                    clear,
                    |key| {
                        let removal = self.remove_cached_entry(&root, metadata, key, &leases)?;
                        if !metadata
                            && !clear
                            && matches!(removal, Removal::Removed | Removal::Missing)
                        {
                            // The removal callback only owns validated plugins/<digest>/<name>
                            if let Some(parent) = root.join(key).parent() {
                                emptied.insert(parent.to_owned());
                            }
                        }
                        Ok(removal)
                    },
                )?;
                if !metadata && (clear || !emptied.is_empty()) {
                    // Explicit clear also picks up empty unindexed folders left by older builds
                    match remove_empty_plugin_download_dirs(&root, clear, emptied, &leases) {
                        Ok(issues) => report.errors.extend(issues),
                        Err(error) => report.errors.push(error),
                    }
                }
                Ok(report)
            })();
            let report = match result {
                Ok(r) => r,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };
            removed_bytes += report.removed_bytes;
            removed_entries += report.removed_entries;
            errors.extend(report.errors);
            statuses.push(crate::cache::CacheFolderStatus {
                id: format!("updates/{folder}"),
                label: label.into(),
                bytes: report.stats.bytes,
                entries: report.stats.entries,
                max_bytes,
                max_entries,
                max_age_seconds: max_age,
                protected_entries: report.stats.protected + report.stats.active,
            });
        }
        (statuses, removed_entries, removed_bytes, errors)
    }
}
struct Entry {
    path: PathBuf,
    receipt: Option<PathBuf>,
    bytes: u64,
    used: u64,
    protected: bool,
}
// Scan the available update cache entries
fn scan(
    root: &Path,
    metadata: bool,
    depth: usize,
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
            scan(&path, false, depth + 1, entries, errors);
            continue;
        }
        if let Some(entry) = inspect(path, info, metadata, depth, errors) {
            entries.push(entry);
        }
    }
}
fn inspect(
    path: PathBuf,
    info: fs::Metadata,
    metadata: bool,
    depth: usize,
    errors: &mut Vec<String>,
) -> Option<Entry> {
    let root = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
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
                if r.version == 1 && r.name == name && r.size == info.len() && safe_name(&name) {
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
                        return None;
                    }
                }
            }
        }
    }
    Some(Entry {
        path,
        receipt,
        bytes,
        used: info
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or_else(now),
        protected,
    })
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
        service
            .publish_metadata(&path, &serde_json::json!({"etag":"fixture","data":[]}))
            .unwrap();
        set_age(service, &path, age);
        path
    }
    fn set_age(service: &UpdateService, path: &Path, seconds: u64) {
        let (table, key) = service.indexed_path(path).unwrap();
        table
            .set_used(&key, (now().saturating_sub(seconds) * 1000) as i64)
            .unwrap();
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
            (536_870_912, 86_400, 64)
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
        set_age(&service, &plugin, 86_400);
        let (_, count, _, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 1);
        assert!(!expired.exists());
        assert!(plugin.exists());
        set_age(&service, &plugin, 172_800);
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
            .set_len(536_870_912)
            .unwrap();
        let (stats, count, _, errors) = service.cache_folders(Some(false));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 1);
        assert!(!owned.exists());
        assert!(unknown.exists());
        assert_eq!(stats[0].bytes, 536_870_912);
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
        assert!(!plugin.parent().unwrap().exists());
        assert!(manager.exists());
        assert_eq!(service.cache_folders(Some(true)).1, 0);
    }
    #[test]
    // Verify that explicit clear sweeps only empty idle digest folders
    fn clear_sweeps_old_empty_plugin_digest_folders() {
        let (_root, service) = service();
        let plugins = service.root.join("downloads/plugins");
        let empty = plugins.join("1".repeat(64));
        let active = plugins.join("2".repeat(64));
        let populated = plugins.join("3".repeat(64));
        let unknown = plugins.join("not-a-digest");
        let manager = service.root.join("downloads/manager").join("4".repeat(64));
        for dir in [&empty, &active, &populated, &unknown, &manager] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(populated.join("keep.txt"), b"unrecognized").unwrap();
        let lease = service.cache_lease(&active.join("incoming.zip")).unwrap();

        let (_, count, _, errors) = service.cache_folders(Some(true));
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(count, 0);
        assert!(!empty.exists());
        assert!(active.is_dir());
        assert!(populated.join("keep.txt").is_file());
        assert!(unknown.is_dir());
        assert!(manager.is_dir());

        drop(lease);
        assert!(service.cache_folders(Some(true)).3.is_empty());
        assert!(!active.exists());
    }

    #[cfg(unix)]
    #[test]
    // Verify that clear does not remove or follow links named like cache digest folders
    fn clear_preserves_linked_digest_folders() {
        let (root, service) = service();
        let outside = root.path().join("outside");
        let plugins = service.root.join("downloads/plugins");
        let link = plugins.join("d".repeat(64));
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&plugins).unwrap();
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        service.cache_folders(Some(true));
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(outside.is_dir());
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
        assert!(!plugin.parent().unwrap().exists());
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
