//! Retained transfer data, metadata, and published package maintenance
//!
//! Limits apply independently to each folder. Active leases, unknown content,
//! and user-facing installers are preserved even when their usage prevents reaching a limit
use serde::Serialize;
use std::sync::{mpsc, Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use stream_dock_manager_downloads::{DownloadCache, PruneReport};
use stream_dock_manager_plugin_host::artwork_cache::{self, ArtworkCache};

pub const PRUNE_INTERVAL: Duration = Duration::from_secs(15 * 60);
const DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DOWNLOAD_ENTRIES: usize = 32;
const DOWNLOAD_AGE: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct CacheFolderStatus {
    pub id: String,
    pub label: String,
    #[ts(type = "number")]
    pub bytes: u64,
    pub entries: usize,
    #[ts(type = "number")]
    pub max_bytes: u64,
    pub max_entries: usize,
    #[ts(type = "number")]
    pub max_age_seconds: u64,
    pub protected_entries: usize,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct CacheStatus {
    pub folders: Vec<CacheFolderStatus>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
pub struct CacheCleanupResult {
    pub status: CacheStatus,
    pub removed_entries: usize,
    #[ts(type = "number")]
    pub removed_bytes: u64,
    pub errors: Vec<String>,
}

pub(crate) struct Maintenance {
    downloads: Arc<DownloadCache>,
    artwork: Arc<ArtworkCache>,
    worker: OnceLock<mpsc::Sender<Request>>,
    last_errors: Mutex<Vec<String>>,
}
impl Maintenance {
    // Initialize cache state from the supplied inputs
    pub(crate) fn new(downloads: Arc<DownloadCache>, artwork: Arc<ArtworkCache>) -> Self {
        Self {
            downloads,
            artwork,
            worker: OnceLock::new(),
            last_errors: Mutex::new(Vec::new()),
        }
    }
}

impl crate::Runtime {
    // Cache status
    pub fn cache_status(&self) -> Result<CacheStatus, String> {
        let (reply, result) = mpsc::channel();
        self.send_cache_request(Request::Status(reply))?;
        result.recv().map_err(|_| "Cache worker stopped".to_owned())
    }

    /// Explicitly clear recognized idle data. Consumers with leases, manually
    /// downloaded installers, unknown files, and symlinks remain untouched
    pub fn clear_cache(&self) -> Result<CacheCleanupResult, String> {
        let (reply, result) = mpsc::channel();
        self.send_cache_request(Request::Clear(reply))?;
        result.recv().map_err(|_| "Cache worker stopped".to_owned())
    }

    fn send_cache_request(&self, request: Request) -> Result<(), String> {
        self.cache_maintenance
            .worker
            .get()
            .ok_or("Cache worker unavailable")?
            .send(request)
            .map_err(|_| "Cache worker stopped".into())
    }

    // Run each owner once. Its report is the inventory after any deletions.
    fn maintain_cache(&self, clear: Option<bool>) -> CacheCleanupResult {
        let mut errors = Vec::new();
        let mut removed_entries = 0;
        let mut removed_bytes = 0;
        let downloads = &self.cache_maintenance.downloads;
        let report = match clear {
            Some(true) => downloads.clear(),
            Some(false) => downloads.prune(),
            None => downloads.stats().map(|stats| PruneReport {
                stats,
                ..Default::default()
            }),
        };
        let mut folders = vec![];
        match report {
            Ok(r) => {
                removed_entries += r.removed_entries;
                removed_bytes += r.removed_bytes;
                errors.extend(r.errors);
                let stats = r.stats;
                if stats.usage_incomplete {
                    errors.push(format!(
                        "Download cache: {} protected entries include contents that could not be measured safely; shown usage may be incomplete.",
                        stats.skipped_entries
                    ));
                }
                folders.push(CacheFolderStatus {
                    id: "download-cache".into(),
                    label: "Download cache".into(),
                    bytes: stats.bytes,
                    entries: stats.entries,
                    max_bytes: DOWNLOAD_BYTES,
                    max_entries: DOWNLOAD_ENTRIES,
                    max_age_seconds: DOWNLOAD_AGE,
                    protected_entries: stats.active_entries + stats.skipped_entries,
                });
            }
            Err(error) => errors.push(format!("Download cache: {error}")),
        }
        match self.cache_maintenance.artwork.maintain(clear) {
            Ok(r) => {
                removed_entries += r.removed_entries;
                removed_bytes += r.removed_bytes;
                errors.extend(r.errors);
                folders.push(CacheFolderStatus {
                    id: "cached-media".into(),
                    label: "Cached artwork".into(),
                    bytes: r.bytes,
                    entries: r.entries,
                    max_bytes: artwork_cache::MAX_BYTES,
                    max_entries: 0,
                    max_age_seconds: artwork_cache::MAX_AGE.as_secs(),
                    protected_entries: r.protected_entries,
                });
            }
            Err(e) => errors.push(format!("Cached artwork: {e}")),
        }
        let (update_folders, entries, bytes, update_errors) = self.updates.cache_folders(clear);
        removed_entries += entries;
        removed_bytes += bytes;
        folders.extend(update_folders);
        match self.firmware_cache_status(clear) {
            Ok((folder, report)) => {
                folders.push(folder);
                removed_entries += report.removed_entries;
                removed_bytes += report.removed_bytes;
                errors.extend(report.errors);
            }
            Err(e) => errors.push(format!("Firmware cache: {e}")),
        }
        errors.extend(update_errors);
        if clear.is_some() {
            *self.cache_maintenance.last_errors.lock().unwrap() = errors.clone();
        } else {
            errors.extend(
                self.cache_maintenance
                    .last_errors
                    .lock()
                    .unwrap()
                    .iter()
                    .cloned(),
            );
        }
        errors.sort();
        errors.dedup();
        CacheCleanupResult {
            status: CacheStatus {
                folders,
                errors: errors.clone(),
            },
            errors,
            removed_entries,
            removed_bytes,
        }
    }
}

enum Request {
    Status(mpsc::Sender<CacheStatus>),
    Clear(mpsc::Sender<CacheCleanupResult>),
}

pub(crate) fn start(runtime: &Arc<crate::Runtime>, periodic: bool) -> Result<(), String> {
    let (tx, rx) = mpsc::channel();
    let weak = Arc::downgrade(runtime);
    std::thread::Builder::new()
        .name("file-cache-maintenance".into())
        .spawn(move || run_worker(weak, rx, periodic.then_some(PRUNE_INTERVAL)))
        .map_err(|e| e.to_string())?;
    runtime
        .cache_maintenance
        .worker
        .set(tx)
        .map_err(|_| "Cache worker already running".into())
}
fn run_worker(
    runtime: Weak<crate::Runtime>,
    rx: mpsc::Receiver<Request>,
    interval: Option<Duration>,
) {
    let mut deadline = Instant::now();
    loop {
        if let Some(interval) = interval {
            if Instant::now() >= deadline {
                let Some(owner) = runtime.upgrade() else {
                    break;
                };
                if owner.is_stopping() {
                    break;
                }
                let report = owner.maintain_cache(Some(false));
                let message = (!report.errors.is_empty()).then(|| report.errors.join("; "));
                owner.emit("cache-maintenance", None, None, message);
                deadline = Instant::now() + interval;
            }
        }
        let request = if interval.is_some() {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            }
        } else {
            match rx.recv() {
                Ok(request) => request,
                Err(_) => break,
            }
        };
        let Some(owner) = runtime.upgrade() else {
            break;
        };
        match request {
            Request::Status(reply) => {
                let status = owner.maintain_cache(None).status;
                drop(owner);
                let _ = reply.send(status);
            }
            Request::Clear(reply) => {
                let result = owner.maintain_cache(Some(true));
                drop(owner);
                let _ = reply.send(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use stream_dock_manager_downloads::{ArtifactSpec, Control};

    #[test]
    // Verify that status exposes independent limits and clear preserves active artifacts
    fn status_exposes_independent_limits_and_clear_preserves_active_artifacts() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().to_path_buf(), false).unwrap();
        let status = runtime.cache_status().unwrap();
        assert!(status.errors.is_empty(), "{:?}", status.errors);
        assert_eq!(status.folders.len(), 5);
        for folder in status.folders {
            let (bytes, entries, age) = match folder.id.as_str() {
                "cached-media" => (
                    artwork_cache::MAX_BYTES,
                    0,
                    artwork_cache::MAX_AGE.as_secs(),
                ),
                "firmware-cache" => (18 * 1024 * 1024, 3, 0),
                "download-cache" => (DOWNLOAD_BYTES, DOWNLOAD_ENTRIES, DOWNLOAD_AGE),
                "updates/cache" => (512 * 1024 * 1024, 64, 24 * 60 * 60),
                "updates/downloads" => (4 * 1024 * 1024 * 1024, 128, 2 * 24 * 60 * 60),
                unexpected => panic!("Unexpected cache folder: {unexpected}"),
            };
            assert_eq!(folder.max_bytes, bytes);
            assert_eq!(folder.max_entries, entries);
            assert_eq!(folder.max_age_seconds, age);
        }
        let bytes = b"isolated cache fixture";
        let spec = ArtifactSpec {
            identity: "cache-fixture".into(),
            url: "https://example.invalid/fixture.zip".into(),
            sha256: hex::encode(Sha256::digest(bytes)),
            size: Some(bytes.len() as u64),
            limit: bytes.len() as u64,
        };
        let artifact = runtime
            .cache_maintenance
            .downloads
            .store_reader_from_origin(
                &spec,
                &mut std::io::Cursor::new(bytes),
                &|| Control::Continue,
                &|_, _| Ok(()),
            )
            .unwrap();
        let path = artifact.path().to_path_buf();
        let report = runtime.clear_cache().unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.removed_entries, 0);
        assert!(path.exists());
        assert_eq!(report.status.folders[0].protected_entries, 1);
        drop(artifact);
        let report = runtime.clear_cache().unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.removed_entries, 1);
        assert!(report.removed_bytes >= bytes.len() as u64);
        assert!(!path.exists());
        assert!(runtime.clear_cache().unwrap().removed_entries == 0);
        runtime.shutdown();
    }

    #[test]
    // Verify that worker prunes on startup even with update checks disabled
    fn worker_prunes_on_startup_even_with_update_checks_disabled() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().to_path_buf(), false).unwrap();
        let directory = root.path().join("updates/cache");
        let path = directory.join(format!("{}.json", "a".repeat(64)));
        std::fs::write(&path, r#"{"etag":"old","data":[]}"#).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        drop(file);
        let artwork = root
            .path()
            .join("cached-media")
            .join(format!("{}.art", "a".repeat(64)));
        std::fs::create_dir_all(artwork.parent().unwrap()).unwrap();
        std::fs::write(&artwork, b"old media").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&artwork)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        let (sender, rx) = mpsc::channel();
        let weak = Arc::downgrade(&runtime);
        let worker =
            std::thread::spawn(move || run_worker(weak, rx, Some(Duration::from_millis(25))));
        let deadline = Instant::now() + Duration::from_secs(5);
        while (path.exists() || artwork.exists()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !path.exists(),
            "startup maintenance never pruned an expired metadata entry"
        );
        assert!(!artwork.exists());
        // Reuse the migrated path after startup. Even though its mtime is fresh,
        // a durable deletion hint is picked up by the next scheduled pass.
        let name = artwork.file_name().unwrap().to_str().unwrap();
        std::fs::write(&artwork, b"new").unwrap();
        use stream_dock_manager_storage::cache_index::{Kind, Record, Table};
        let table = Table::new(&root.path().canonicalize().unwrap(), Kind::Artwork);
        table.record(&Record::file(name.into(), 3)).unwrap();
        table.hint(name).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while artwork.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !artwork.exists(),
            "the second scheduled pass did not reclaim the hinted file"
        );
        assert_eq!(PRUNE_INTERVAL, Duration::from_secs(900));
        drop(sender);
        worker.join().unwrap();
        runtime.shutdown();
    }
}
