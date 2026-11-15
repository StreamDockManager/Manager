//! Retained transfer data, metadata, and published package maintenance
//!
//! Limits apply independently to each folder. Active leases, unknown content,
//! and user-facing installers are preserved even when their usage prevents reaching a limit
use serde::Serialize;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use stream_dock_manager_downloads::DownloadCache;

pub const PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
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
    // Serialize manual clear, hourly prune, and status without blocking transfer
    // work. Per-entry storage leases arbitrate with transfers and their consumers
    operation: Mutex<()>,
    last_errors: Mutex<Vec<String>>,
}
impl Maintenance {
    // Initialize cache state from the supplied inputs
    pub(crate) fn new(downloads: Arc<DownloadCache>) -> Self {
        Self {
            downloads,
            operation: Mutex::new(()),
            last_errors: Mutex::new(Vec::new()),
        }
    }
}

impl crate::Runtime {
    // Cache status
    pub fn cache_status(&self) -> Result<CacheStatus, String> {
        let _operation = self
            .cache_maintenance
            .operation
            .lock()
            .map_err(|e| e.to_string())?;
        Ok(self.read_cache_status())
    }

    // Read cache status
    fn read_cache_status(&self) -> CacheStatus {
        let mut errors = self.cache_maintenance.last_errors.lock().unwrap().clone();
        let mut folders = vec![];
        match self.cache_maintenance.downloads.stats() {
            Ok(stats) => {
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
        let (update_folders, _, _, update_errors) = self.updates.cache_folders(None);
        folders.extend(update_folders);
        errors.extend(update_errors);
        errors.sort();
        errors.dedup();
        CacheStatus { folders, errors }
    }

    /// Explicitly clear recognized idle data. Consumers with leases, manually
    /// downloaded installers, unknown files, and symlinks remain untouched
    pub fn clear_cache(&self) -> Result<CacheCleanupResult, String> {
        self.maintain_cache(true)
    }

    // Prune cache
    pub(crate) fn prune_cache(&self) -> Result<CacheCleanupResult, String> {
        self.maintain_cache(false)
    }

    // Serialize cache maintenance and aggregate removed entries, byte counts and failures
    fn maintain_cache(&self, clear: bool) -> Result<CacheCleanupResult, String> {
        let _operation = self
            .cache_maintenance
            .operation
            .lock()
            .map_err(|e| e.to_string())?;
        let mut removed_entries = 0;
        let mut removed_bytes = 0u64;
        let mut errors = vec![];
        let result = if clear {
            self.cache_maintenance.downloads.clear()
        } else {
            self.cache_maintenance.downloads.prune()
        };
        match result {
            Ok(report) => {
                removed_entries += report.removed_entries;
                removed_bytes = removed_bytes.saturating_add(report.removed_bytes);
                errors.extend(
                    report
                        .errors
                        .into_iter()
                        .map(|e| format!("Download cache: {e}")),
                );
            }
            Err(error) => errors.push(format!("Download cache: {error}")),
        }
        let (_, entries, bytes, update_errors) = self.updates.cache_folders(Some(clear));
        removed_entries += entries;
        removed_bytes = removed_bytes.saturating_add(bytes);
        errors.extend(update_errors);
        *self.cache_maintenance.last_errors.lock().unwrap() = errors.clone();
        let status = self.read_cache_status();
        Ok(CacheCleanupResult {
            status,
            removed_entries,
            removed_bytes,
            errors,
        })
    }
}

/// First pass runs on startup; subsequent passes run hourly even when automatic
/// update discovery is disabled. The weak owner and short sleeps permit shutdown
/// without retaining an idle application for an hour
pub(crate) fn start(runtime: Weak<crate::Runtime>) {
    std::thread::spawn(move || {
        let mut next = Instant::now();
        loop {
            let Some(owner) = runtime.upgrade() else {
                break;
            };
            if owner.is_stopping() {
                break;
            }
            if Instant::now() >= next {
                match owner.prune_cache() {
                    Ok(report) if !report.errors.is_empty() => {
                        owner.emit(
                            "cache-maintenance",
                            None,
                            None,
                            Some(report.errors.join("; ")),
                        );
                    }
                    Err(error) => owner.emit("cache-maintenance", None, None, Some(error)),
                    _ => {}
                }
                next = Instant::now() + PRUNE_INTERVAL;
            }
            drop(owner);
            std::thread::sleep(Duration::from_secs(5));
        }
    });
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
        assert_eq!(status.folders.len(), 3);
        for folder in status.folders {
            let (bytes, entries, age) = match folder.id.as_str() {
                "download-cache" => (DOWNLOAD_BYTES, DOWNLOAD_ENTRIES, DOWNLOAD_AGE),
                "updates/cache" => (1024 * 1024 * 1024, 64, 24 * 60 * 60),
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
        start(Arc::downgrade(&runtime));
        let deadline = Instant::now() + Duration::from_secs(5);
        while path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !path.exists(),
            "startup maintenance never pruned an expired metadata entry"
        );
        assert_eq!(PRUNE_INTERVAL, Duration::from_secs(3600));
        runtime.shutdown();
    }
}
