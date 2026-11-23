use super::{ArtifactSpec, CacheLimits, CacheStats, Control, PruneReport, MAX_ARTIFACT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use stream_dock_manager_storage::cache_index::{self, Kind, Limits, Record, Removal, Table};
// Read the current time for this subsystem
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
// Validate or create the managed directory
pub(super) fn directory(path: &Path) -> Result<(), String> {
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir())
    {
        return Err("Download cache directory is not a regular directory".into());
    }
    std::fs::create_dir_all(path).map_err(|e| e.to_string())
}
// Require a regular file before accessing its contents
fn regular(path: &Path) -> Result<(), String> {
    let m = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err("Download cache file is not regular".into());
    }
    Ok(())
}
// Open a regular cache file, creating new Unix files with owner-only access
pub(super) fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            regular(path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|e| e.to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Metadata {
    schema: u32,
    key: String,
    scope: String,
    sha256: String,
    size: Option<u64>,
    limit: u64,
    pub total: Option<u64>,
    pub completed: u64,
    pub prefix_sha256: String,
    pub etag: Option<String>,
    pub modified: Option<String>,
    touched: u64,
}
impl Metadata {
    // Construct the value and initialize its state
    fn new(spec: &ArtifactSpec) -> Self {
        Self {
            schema: 1,
            key: spec.key(),
            scope: hex::encode(Sha256::digest(spec.identity.as_bytes())),
            sha256: spec.sha256.to_ascii_lowercase(),
            size: spec.size,
            limit: spec.limit,
            total: spec.size,
            completed: 0,
            prefix_sha256: hex::encode(Sha256::digest([])),
            etag: None,
            modified: None,
            touched: now(),
        }
    }
    // Report whether the value satisfies the supported constraints
    fn valid(&self) -> bool {
        self.schema == 1
            && key_valid(&self.key)
            && key_valid(&self.scope)
            && (self.sha256.is_empty() || key_valid(&self.sha256))
            && key_valid(&self.prefix_sha256)
            && self.limit > 0
            && self.limit <= MAX_ARTIFACT
            && self.completed <= self.limit
            && self
                .total
                .is_none_or(|total| total <= self.limit && self.completed <= total)
            && self
                .size
                .is_none_or(|size| size > 0 && size <= self.limit && self.total == Some(size))
            && self
                .etag
                .as_ref()
                .is_none_or(|s| super::strong_etag(Some(s)).is_some())
            && self
                .modified
                .as_ref()
                .is_none_or(|s| s.len() <= 128 && !s.contains(['\r', '\n']))
    }
    // Report whether the candidate matches these criteria
    fn matches(&self, spec: &ArtifactSpec) -> bool {
        self.valid()
            && self.key == spec.key()
            && self.scope == hex::encode(Sha256::digest(spec.identity.as_bytes()))
            && self.sha256 == spec.sha256.to_ascii_lowercase()
            && self.size == spec.size
            && self.limit == spec.limit
    }
}
// Check whether the cache key has the expected representation
fn key_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
// Read the metadata for this resource
fn metadata(path: &Path) -> Result<Metadata, String> {
    regular(path)?;
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 16384 {
        return Err("Download metadata exceeds limit".into());
    }
    serde_json::from_reader(file.take(16385)).map_err(|_| "Invalid download checkpoint".into())
}
// Enumerate files belonging to this managed cache
fn owned_files(dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for (i, entry) in entries.enumerate() {
        let entry = entry.map_err(|e| e.to_string())?;
        if i >= 8
            || !entry.file_name().to_str().is_some_and(|name| {
                matches!(name, "body.part" | "meta.json" | "lease.lock") || checkpoint_name(name)
            })
        {
            return Err("Unrecognized download cache contents; files preserved".into());
        }
        regular(&entry.path())?;
    }
    Ok(())
}
// Remove the selected managed cache entry
fn erase(dir: &Path) -> Result<(), String> {
    owned_files(dir)?;
    clear_checkpoints(dir)?;
    for name in ["body.part", "meta.json"] {
        match std::fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
// Recognize a managed checkpoint filename before cleanup
fn checkpoint_name(name: &str) -> bool {
    name.starts_with(".checkpoint-") && name.len() <= 64
}
// Clear checkpoints
fn clear_checkpoints(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_name().to_str().is_some_and(checkpoint_name) {
            regular(&entry.path())?;
            std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
pub(super) struct Entry {
    pub body: PathBuf,
    pub meta_path: PathBuf,
    pub meta: Metadata,
    _lease: File,
    index: Table,
}
impl Drop for Entry {
    fn drop(&mut self) {
        let _ = self.index.touch(&self.meta.key, false);
    }
}
impl Entry {
    // Write metadata
    fn write_metadata(&mut self) -> Result<(), String> {
        self.meta.touched = now();
        let parent = self.meta_path.parent().ok_or("Invalid download cache")?;
        let mut file = tempfile::Builder::new()
            .prefix(".checkpoint-")
            .tempfile_in(parent)
            .map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(&self.meta).map_err(|e| e.to_string())?;
        let mut record = Record::file(
            self.meta.key.clone(),
            self.body.metadata().map_err(|e| e.to_string())?.len() + bytes.len() as u64,
        );
        record.scope = self.meta.scope.clone();
        record.active = true;
        self.index.record(&record)?;
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(&self.meta_path).map_err(|e| e.to_string())?;
        Ok(())
    }
    // Flush downloaded bytes and persist the hash of the completed prefix
    pub fn checkpoint(&mut self, file: &mut File, hash: &Sha256) -> Result<(), String> {
        file.sync_all().map_err(|e| e.to_string())?;
        self.meta.prefix_sha256 = hex::encode(hash.clone().finalize());
        self.write_metadata()
    }
    // Reset for the current operation
    pub fn reset(&mut self, file: &mut File) -> Result<(), String> {
        file.set_len(0).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        self.meta.completed = 0;
        self.meta.total = self.meta.size;
        self.meta.etag = None;
        self.meta.modified = None;
        self.checkpoint(file, &Sha256::new())
    }
    // Discard for the current operation
    pub fn discard(&self) -> Result<(), String> {
        self.index.hint(&self.meta.key)?;
        erase(self.body.parent().ok_or("Invalid download cache")?)?;
        let mut record = Record::file(self.meta.key.clone(), 0);
        record.scope = self.meta.scope.clone();
        record.hint = true;
        record.active = true;
        self.index.record(&record)
    }
    // Check control
    pub fn check_control(&self, control: Control) -> Result<(), String> {
        match control {
            Control::Continue => Ok(()),
            Control::Pause => Err("Download paused; Retry resumes the retained partial".into()),
            Control::Cancel => {
                self.discard()?;
                Err("Download cancelled; partial discarded".into())
            }
        }
    }
}
#[cfg(test)]
// Acquire for the current operation
pub(super) fn acquire(root: &Path, spec: &ArtifactSpec) -> Result<Entry, String> {
    acquire_indexed(root, spec, &table(root))
}
pub(super) fn table(root: &Path) -> Table {
    let db_root = if root.file_name().is_some_and(|n| n == "download-cache") {
        root.parent().unwrap_or(root)
    } else {
        root
    };
    Table::new(db_root, Kind::Transfers)
}
pub(super) fn acquire_indexed(
    root: &Path,
    spec: &ArtifactSpec,
    index: &Table,
) -> Result<Entry, String> {
    let _registry = registry(root)?;
    acquire_locked(root, spec, index)
}
// Acquire the cache registry lock before inspecting or changing entries
fn registry(root: &Path) -> Result<File, String> {
    directory(root)?;
    if root.canonicalize().map_err(|e| e.to_string())? != root {
        return Err("Download cache directory or ancestor was replaced by a symbolic link".into());
    }
    let registry = private_file(&root.join("registry.lock"))?;
    let until = Instant::now() + Duration::from_secs(2);
    loop {
        match registry.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(error)) => return Err(error.to_string()),
        }
        if Instant::now() >= until {
            return Err("Download cache is busy; retry shortly".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(registry)
}
// Acquire locked
fn acquire_locked(root: &Path, spec: &ArtifactSpec, index: &Table) -> Result<Entry, String> {
    let key = spec.key();
    let dir = root.join(&key);
    directory(&dir)?;
    let lease = private_file(&dir.join("lease.lock"))?;
    lease
        .try_lock()
        .map_err(|_| "This artifact already has an active download owner")?;
    owned_files(&dir)?;
    clear_checkpoints(&dir)?;
    let meta_path = dir.join("meta.json");
    let meta = metadata(&meta_path).ok().filter(|m| m.matches(spec));
    let fresh = meta.is_none() || index.hinted(&key)?;
    let mut result = Entry {
        body: dir.join("body.part"),
        meta_path,
        meta: meta.unwrap_or_else(|| Metadata::new(spec)),
        _lease: lease,
        index: index.clone(),
    };
    let mut file = private_file(&result.body)?;
    if fresh {
        result.reset(&mut file)?;
    } else {
        result.write_metadata()?;
    }
    Ok(result)
}
// Discard identity
pub(super) fn discard_identity(root: &Path, index: &Table, identity: &str) -> Result<(), String> {
    if identity.is_empty() || identity.len() > 4096 {
        return Err("Invalid artifact identity".into());
    }
    let _registry = registry(root)?;
    import(root, index)?;
    for key in index.scope_keys(&hex::encode(Sha256::digest(identity.as_bytes())))? {
        if key_valid(&key) && !is_leased(&root.join(&key))? {
            index.hint(&key)?;
        }
    }
    Ok(())
}
// Measure only immediate regular files. Nested/linked/unreadable content stays
// untouched and marks the total as incomplete rather than pretending it is zero.
fn measured_bytes(path: &Path) -> (u64, bool) {
    let Ok(meta) = path.symlink_metadata() else {
        return (0, true);
    };
    if meta.is_file() {
        return (meta.len(), false);
    }
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return (0, true);
    }
    let Ok(files) = std::fs::read_dir(path) else {
        return (0, true);
    };
    let mut bytes = 0u64;
    let mut incomplete = false;
    for file in files {
        let Ok(file) = file else {
            incomplete = true;
            continue;
        };
        match file.path().symlink_metadata() {
            Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
                bytes = bytes.saturating_add(meta.len())
            }
            _ => incomplete = true,
        }
    }
    (bytes, incomplete)
}
// A missing lease file has no live owner. Never follow cached directory aliases.
fn is_leased(path: &Path) -> Result<bool, String> {
    match path.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => (),
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    }
    let lease = path.join("lease.lock");
    match lease.symlink_metadata() {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => (),
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lease)
        .map_err(|e| e.to_string())?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}
// Inspect a single legacy entry or an eviction candidate under the registry lock.
fn inspect(path: &Path, key: &str) -> Result<(Option<File>, Option<Metadata>), String> {
    if !key_valid(key)
        || !path
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    {
        return Err("Unrecognized transfer directory".into());
    }
    let lease_path = path.join("lease.lock");
    regular(&lease_path)?;
    let lease = private_file(&lease_path)?;
    let lease = match lease.try_lock() {
        Ok(()) => Some(lease),
        Err(std::fs::TryLockError::WouldBlock) => None,
        Err(e) => return Err(e.to_string()),
    };
    if lease.is_none() {
        return Ok((None, None));
    }
    owned_files(path)?;
    if lease_path.metadata().map_err(|e| e.to_string())?.len() == 0
        && std::fs::read_dir(path).map_err(|e| e.to_string())?.count() == 1
    {
        return Ok((lease, None));
    }
    let meta = metadata(&path.join("meta.json"))?;
    if !meta.valid() || meta.key != key {
        return Err("Invalid transfer ownership metadata".into());
    }
    Ok((lease, Some(meta)))
}
fn import(root: &Path, index: &Table) -> Result<(), String> {
    index.import_once(|| {
        let mut records = vec![];
        for item in std::fs::read_dir(root).map_err(|e| e.to_string())? {
            let path = item.map_err(|e| e.to_string())?.path();
            let Some(key) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if matches!(
                key,
                "registry.lock"
                    | "databases"
                    | "cache.sqlite3"
                    | "cache.sqlite3-wal"
                    | "cache.sqlite3-shm"
                    | "cache.sqlite3-journal"
            ) || !cache_index::safe_relative(key)
            {
                continue;
            }
            let (bytes, incomplete) = measured_bytes(&path);
            let mut record = Record::file(key.into(), bytes);
            record.incomplete = incomplete;
            match inspect(&path, key) {
                Ok((lease, meta)) => {
                    record.active = lease.is_none();
                    if let Some(meta) = meta {
                        record.used =
                            (meta.touched.saturating_mul(1000)).min(i64::MAX as u64) as i64;
                        record.scope = meta.scope;
                    } else if !record.active {
                        record.hint = true;
                    }
                }
                Err(_) => record.owned = false,
            }
            records.push(record);
        }
        Ok(records)
    })?;
    index.recover_activity(|key| {
        if key_valid(key) {
            is_leased(&root.join(key))
        } else {
            Ok(false)
        }
    })
}
fn cache_stats(stats: cache_index::Stats) -> CacheStats {
    CacheStats {
        bytes: stats.bytes,
        entries: stats.entries,
        active_entries: stats.active,
        skipped_entries: stats.protected,
        usage_incomplete: stats.incomplete,
    }
}
pub(super) fn stats(root: &Path, index: &Table) -> Result<CacheStats, String> {
    let _registry = registry(root)?;
    import(root, index)?;
    Ok(cache_stats(index.stats()?))
}
pub(super) fn prune(
    root: &Path,
    index: &Table,
    limits: CacheLimits,
    clear: bool,
) -> Result<PruneReport, String> {
    let _registry = registry(root)?;
    import(root, index)?;
    let report = index.purge(
        Limits {
            bytes: limits.max_bytes,
            entries: Some(limits.max_entries),
            age: Some(limits.max_age),
        },
        clear,
        |key| {
            if !key_valid(key) {
                return Ok(Removal::Protected);
            }
            let path = root.join(key);
            if path
                .symlink_metadata()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                return Ok(Removal::Missing);
            }
            let Ok((lease, _)) = inspect(&path, key) else {
                return Ok(Removal::Protected);
            };
            let Some(lease) = lease else {
                return Ok(Removal::Busy);
            };
            index.hint(key)?;
            erase(&path)?;
            drop(lease);
            std::fs::remove_file(path.join("lease.lock"))
                .and_then(|_| std::fs::remove_dir(&path))
                .map_err(|e| e.to_string())?;
            Ok(Removal::Removed)
        },
    )?;
    let stats = cache_stats(report.stats);
    Ok(PruneReport {
        active_entries: stats.active_entries,
        skipped_entries: stats.skipped_entries,
        stats,
        removed_entries: report.removed_entries,
        removed_bytes: report.removed_bytes,
        errors: report.errors,
    })
}
