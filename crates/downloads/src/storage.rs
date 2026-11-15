use super::{ArtifactSpec, CacheLimits, CacheStats, Control, PruneReport, MAX_ARTIFACT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
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
        file.write_all(&serde_json::to_vec(&self.meta).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
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
        erase(self.body.parent().ok_or("Invalid download cache")?)
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
    acquire_with_limits(root, spec, CacheLimits::default())
}
// Acquire with limits
pub(super) fn acquire_with_limits(
    root: &Path,
    spec: &ArtifactSpec,
    limits: CacheLimits,
) -> Result<Entry, String> {
    let _registry = registry(root)?;
    let entry = acquire_locked(root, spec)?;
    // Holding the new entry lease protects it while idle neighbors are pruned
    prune_locked(root, limits, false)?;
    Ok(entry)
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
fn acquire_locked(root: &Path, spec: &ArtifactSpec) -> Result<Entry, String> {
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
    let fresh = meta.is_none();
    let mut result = Entry {
        body: dir.join("body.part"),
        meta_path,
        meta: meta.unwrap_or_else(|| Metadata::new(spec)),
        _lease: lease,
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
pub(super) fn discard_identity(root: &Path, identity: &str) -> Result<(), String> {
    if identity.is_empty() || identity.len() > 4096 {
        return Err("Invalid artifact identity".into());
    }
    let _registry = registry(root)?;
    let scope = hex::encode(Sha256::digest(identity.as_bytes()));
    for (i, entry) in std::fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .enumerate()
    {
        if i >= 256 {
            return Err("Download cache inventory exceeds its bound".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_name().to_str().is_some_and(key_valid) {
            continue;
        }
        let path = entry.path();
        directory(&path)?;
        let Ok(meta) = metadata(&path.join("meta.json")) else {
            continue;
        };
        if !meta.valid()
            || meta.scope != scope
            || entry.file_name().to_str() != Some(meta.key.as_str())
        {
            continue;
        }
        let lease = private_file(&path.join("lease.lock"))?;
        if lease.try_lock().is_err() {
            continue;
        }
        erase(&path)?;
        drop(lease);
        std::fs::remove_file(path.join("lease.lock")).map_err(|e| e.to_string())?;
        std::fs::remove_dir(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

struct InventoryEntry {
    path: PathBuf,
    touched: u64,
    bytes: u64,
    empty: bool,
    // Keep every acquired lease until maintenance finishes, preventing mutation
    // of its metadata/body. Acquisition holds the same cross-process registry.
    lease: Option<File>,
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
// Collect the files and identity data for this inventory
fn inventory(root: &Path) -> Result<(Vec<InventoryEntry>, CacheStats), String> {
    let mut entries = Vec::new();
    let mut stats = CacheStats::default();
    for item in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        let name = item.file_name();
        if name == "registry.lock" {
            continue;
        }
        let path = item.path();
        let (bytes, incomplete) = measured_bytes(&path);
        stats.bytes = stats.bytes.saturating_add(bytes);
        stats.usage_incomplete |= incomplete;
        stats.entries += 1;
        if !name.to_str().is_some_and(key_valid)
            || !item.file_type().map_err(|e| e.to_string())?.is_dir()
        {
            stats.skipped_entries += 1;
            continue;
        }
        // Refuse unknown files, symlinks, and malformed ownership metadata
        // No recursive deletes and no paths from metadata are used
        let mut lock_error = None;
        let mut inspect = || -> Result<InventoryEntry, String> {
            owned_files(&path)?;
            let lease_path = path.join("lease.lock");
            regular(&lease_path)?;
            let lease = private_file(&lease_path)?;
            let lease = match lease.try_lock() {
                Ok(()) => Some(lease),
                Err(std::fs::TryLockError::WouldBlock) => None,
                Err(std::fs::TryLockError::Error(error)) => {
                    lock_error = Some(error.to_string());
                    return Err(error.to_string());
                }
            };
            // Explicit discard removes body and metadata but retains the lease
            // inode until its owner releases it. Recognize only that exact empty
            // shape, never orphan bodies, checkpoints, or malformed metadata.
            let mut files = std::fs::read_dir(&path).map_err(|e| e.to_string())?;
            let empty = files
                .next()
                .transpose()
                .map_err(|e| e.to_string())?
                .is_some_and(|file| file.file_name() == "lease.lock")
                && files.next().is_none()
                && lease_path.metadata().map_err(|e| e.to_string())?.len() == 0;
            if empty {
                return Ok(InventoryEntry {
                    path: path.clone(),
                    touched: 0,
                    bytes: 0,
                    empty: true,
                    lease,
                });
            }
            let meta = metadata(&path.join("meta.json"))?;
            if !meta.valid() || name.to_str() != Some(meta.key.as_str()) {
                return Err("Invalid cache ownership metadata".into());
            }
            let mut bytes = 0u64;
            for file in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
                let file = file.map_err(|e| e.to_string())?;
                let meta = file.path().symlink_metadata().map_err(|e| e.to_string())?;
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err("Unsafe cache file".into());
                }
                bytes = bytes.saturating_add(meta.len());
            }
            Ok(InventoryEntry {
                path: path.clone(),
                touched: meta.touched,
                bytes,
                empty: false,
                lease,
            })
        };
        match inspect() {
            Ok(entry) => {
                stats.active_entries += usize::from(entry.lease.is_none());
                entries.push(entry);
            }
            Err(_) => {
                if let Some(error) = lock_error {
                    return Err(format!("Unable to lock download cache entry: {error}"));
                }
                stats.skipped_entries += 1;
            }
        }
    }
    Ok((entries, stats))
}
// Read the current subsystem statistics
pub(super) fn stats(root: &Path) -> Result<CacheStats, String> {
    let _registry = registry(root)?;
    Ok(inventory(root)?.1)
}
// Prune for the current operation
pub(super) fn prune(root: &Path, limits: CacheLimits, clear: bool) -> Result<PruneReport, String> {
    let _registry = registry(root)?;
    prune_locked(root, limits, clear)
}
// Prune locked
fn prune_locked(root: &Path, limits: CacheLimits, clear: bool) -> Result<PruneReport, String> {
    let (mut entries, mut stats) = inventory(root)?;
    let mut report = PruneReport {
        active_entries: stats.active_entries,
        skipped_entries: stats.skipped_entries,
        ..PruneReport::default()
    };
    entries.sort_by(|a, b| {
        b.empty
            .cmp(&a.empty)
            .then_with(|| a.touched.cmp(&b.touched))
            .then_with(|| a.path.cmp(&b.path))
    });
    let current = now();
    for entry in entries {
        if entry.lease.is_none() {
            continue;
        }
        if !clear
            && !entry.empty
            && stats.entries <= limits.max_entries
            && stats.bytes <= limits.max_bytes
            && current.saturating_sub(entry.touched) < limits.max_age.as_secs()
        {
            continue;
        }
        let remove = || -> Result<(), String> {
            erase(&entry.path)?;
            // No acquirer can reopen this lease inode while the registry is held
            drop(entry.lease);
            std::fs::remove_file(entry.path.join("lease.lock")).map_err(|e| e.to_string())?;
            std::fs::remove_dir(&entry.path).map_err(|e| e.to_string())
        };
        match remove() {
            Ok(()) => {
                report.removed_entries += 1;
                report.removed_bytes = report.removed_bytes.saturating_add(entry.bytes);
                stats.entries = stats.entries.saturating_sub(1);
                stats.bytes = stats.bytes.saturating_sub(entry.bytes);
            }
            Err(error) => report
                .errors
                .push(format!("Unable to remove cache entry: {error}")),
        }
    }
    // Rescan after partial failures and concurrent writes by protected owners
    report.stats = inventory(root)?.1;
    Ok(report)
}
