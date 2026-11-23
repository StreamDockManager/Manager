//! Metadata-only firmware discovery, separate from all device I/O
pub mod records;
use crate::Runtime;
use records::{FirmwareCandidate, SourceId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use stream_dock_manager_storage::cache_index::{self, Kind, Limits, Record, Removal, Table};
const MAX_MANIFEST: u64 = 2 * 1024 * 1024;
const CACHE_AGE: u64 = 24 * 60 * 60;
const PARSER_VERSION: u32 = 1;
use crate::application_data::FIRMWARE_TEST_CATALOG_RECORD as PREF;
#[derive(Clone, Copy)]
struct Source {
    id: SourceId,
    name: &'static str,
    url: &'static str,
}
const SOURCES: [Source; 3] = [
    Source {
        id: SourceId::Mirabox,
        name: "Mirabox",
        url: "https://cdn1.key123.vip/StreamDock/firmware/firmware-version-mirabox.json",
    },
    Source {
        id: SourceId::VsdCraft,
        name: "VSD Craft",
        url: "https://cdn1.key123.vip/VSDCraft/firmware/firmware-version-check.json",
    },
    Source {
        id: SourceId::Test,
        name: "Extended test catalog",
        url: "https://cdn1.key123.vip/StreamDock/firmware/firmware-version-check-test.json",
    },
];
pub(crate) struct CatalogService {
    index: Table,
    refresh: Mutex<u64>,
    cache: Mutex<BTreeMap<String, Cached>>,
}
impl CatalogService {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            index: Table::new(root, Kind::Firmware),
            refresh: Default::default(),
            cache: Default::default(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Cached {
    parser_version: u32,
    raw: String,
    snapshot_id: String,
    fetched: u64,
    validated: u64,
    etag: Option<String>,
    modified: Option<String>,
    error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct FirmwareSourceStatus {
    pub id: String,
    pub name: String,
    pub role: String,
    pub url: String,
    pub state: String,
    #[ts(type = "number | null")]
    pub last_checked: Option<u64>,
    pub snapshot_id: Option<String>,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct FirmwareCatalogReport {
    pub device_id: String,
    pub connection_generation: String,
    pub installed: Option<String>,
    pub installed_stale: bool,
    pub include_test: bool,
    pub state: String,
    pub message: String,
    pub sources: Vec<FirmwareSourceStatus>,
    pub candidates: Vec<FirmwareCandidate>,
}
// Read wall-clock seconds without allowing pre-epoch time to wrap
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
// Keep manifest requests on the vendor's approved TLS origin and firmware trees
fn permitted(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("cdn1.key123.vip")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && ["/StreamDock/firmware/", "/VSDCraft/firmware/"]
            .iter()
            .any(|prefix| url.path().starts_with(prefix))
        && !url.path().to_ascii_lowercase().contains("%2e")
        && !url.path().to_ascii_lowercase().contains("%2f")
        && !url.path().to_ascii_lowercase().contains("%5c")
}
// Refresh one source with conditional validators and a strict decoded body bound
fn fetch(source: Source, previous: Option<&Cached>) -> Result<Cached, String> {
    fetch_at(source, previous, source.url)
}
// The URL is selected only by the fixed registry; tests substitute a disposable loopback server
fn fetch_at(source: Source, previous: Option<&Cached>, url: &str) -> Result<Cached, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5
                || !permitted(attempt.url())
                || attempt.url().as_str() != source.url
            {
                attempt.error("Firmware catalog redirect rejected")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| "Firmware HTTP client unavailable")?;
    for attempt in 0..2 {
        let mut request = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::ACCEPT_ENCODING, "identity");
        if let Some(old) = previous {
            if let Some(value) = &old.etag {
                request = request.header(reqwest::header::IF_NONE_MATCH, value);
            }
            if let Some(value) = &old.modified {
                request = request.header(reqwest::header::IF_MODIFIED_SINCE, value);
            }
        }
        let response = match request.send() {
            Ok(r) => r,
            Err(_) if attempt == 0 => {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            Err(_) => return Err("Firmware catalog request failed".into()),
        };
        if response.status().as_u16() == 304 {
            let mut old = previous
                .cloned()
                .ok_or("Firmware catalog returned 304 without a snapshot")?;
            if old.snapshot_id != hex::encode(Sha256::digest(old.raw.as_bytes())) {
                return Err("Firmware catalog 304 has no valid snapshot".into());
            }
            records::parse(source.id, source.url, &old.snapshot_id, old.raw.as_bytes())?;
            old.validated = now();
            old.error = None;
            old.parser_version = PARSER_VERSION;
            return Ok(old);
        }
        if !response.status().is_success() {
            // Never retry forbidden/missing sources or bypass a server Retry-After delay
            if attempt == 0
                && response.status().is_server_error()
                && !response
                    .headers()
                    .contains_key(reqwest::header::RETRY_AFTER)
            {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            return Err(format!(
                "Firmware catalog HTTP {}",
                response.status().as_u16()
            ));
        }
        if response.content_length().is_some_and(|n| n > MAX_MANIFEST) {
            return Err("Firmware catalog exceeds 2 MiB".into());
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let modified = response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut bytes = Vec::new();
        response
            .take(MAX_MANIFEST + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Firmware catalog body interrupted")?;
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err("Firmware catalog exceeds 2 MiB".into());
        }
        let bytes = decode_manifest(bytes)?;
        let snapshot_id = hex::encode(Sha256::digest(&bytes));
        records::parse(source.id, source.url, &snapshot_id, &bytes)?;
        let raw = String::from_utf8(bytes).map_err(|_| "Firmware catalog is not UTF-8")?;
        return Ok(Cached {
            parser_version: PARSER_VERSION,
            raw,
            snapshot_id,
            fetched: now(),
            validated: now(),
            etag,
            modified,
            error: None,
        });
    }
    Err("Firmware catalog request failed".into())
}
// Bound decoded JSON independently when an upstream ignores identity encoding
fn decode_manifest(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes);
    }
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| "Invalid compressed firmware catalog")?;
    if decoded.len() as u64 > MAX_MANIFEST {
        return Err("Decompressed firmware catalog exceeds 2 MiB".into());
    }
    Ok(decoded)
}
// Persist only accepted bounded snapshots outside configuration and recovery storage
fn save_cache(
    directory: &Path,
    source: Source,
    value: &Cached,
    index: &Table,
) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(directory)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Firmware cache directory cannot be a symlink".into());
    }
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    serde_json::to_writer(&mut file, value).map_err(|e| e.to_string())?;
    file.flush().map_err(|e| e.to_string())?;
    let name = format!("{}.json", source.id.id());
    index.record(&Record::file(
        name.clone(),
        file.as_file().metadata().map_err(|e| e.to_string())?.len(),
    ))?;
    file.persist(directory.join(name))
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn read_snapshot(path: &Path, source: Source) -> Option<Cached> {
    let meta = path.symlink_metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST * 3 {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_MANIFEST * 3 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let mut old: Cached = serde_json::from_slice(&bytes).ok()?;
    if old.raw.len() as u64 > MAX_MANIFEST
        || hex::encode(Sha256::digest(old.raw.as_bytes())) != old.snapshot_id
        || records::parse(source.id, source.url, &old.snapshot_id, old.raw.as_bytes()).is_err()
    {
        return None;
    }
    old.parser_version = PARSER_VERSION;
    Some(old)
}
// Classify only supported comparable standard rows, never treating missing coverage as current
fn classify(standard: &[&FirmwareCandidate], incomplete: bool) -> (&'static str, &'static str) {
    if incomplete {
        return (
            "incomplete",
            "Check incomplete; retained catalog results may be stale",
        );
    }
    if standard.is_empty() {
        return ("no_match","No matching firmware listed; this does not establish that installed firmware is latest");
    }
    if standard
        .iter()
        .any(|c| c.matching == records::MatchKind::Ambiguous)
    {
        return ("compatibility_unknown", "Compatibility needs confirmation");
    }
    if standard
        .iter()
        .any(|c| c.comparison == records::VersionComparison::Unavailable)
    {
        return (
            "version_unknown",
            "Version cannot be compared; inspect the source details",
        );
    }
    if standard
        .iter()
        .any(|c| c.comparison == records::VersionComparison::Newer)
    {
        return ("update_available","A matching standard catalog lists a newer version; a catalog listing does not establish compatibility");
    }
    if standard
        .iter()
        .all(|c| c.comparison == records::VersionComparison::Older)
    {
        return (
            "installed_newer",
            "Installed firmware is newer than the comparable catalog records",
        );
    }
    if [SourceId::Mirabox, SourceId::VsdCraft]
        .iter()
        .all(|source| standard.iter().any(|c| c.source == *source))
        && standard
            .iter()
            .all(|c| c.comparison == records::VersionComparison::Equal)
    {
        return (
            "matches",
            "Installed firmware matches the checked standard catalogs",
        );
    }
    (
        "version_unknown",
        "Catalog coverage is incomplete or records disagree; no latest-version claim is possible",
    )
}
impl Runtime {
    // Keep test-feed consent in the existing typed settings owner
    fn firmware_test_enabled(&self) -> Result<bool, String> {
        self.profiles
            .storage
            .run(|c| Ok(PREF.read_from(c)?.unwrap_or(false)))
    }
    // Changing consent never starts requests or transfers firmware
    pub fn set_firmware_catalog_test(&self, enabled: bool) -> Result<(), String> {
        PREF.write(&self.profiles.storage, &enabled)?;
        Ok(())
    }
    // Capture an actual connected identity without opening or querying another HID handle
    fn catalog_identity(&self, id: &str) -> Result<(crate::DeviceInfo, u64), String> {
        let epoch = self
            .connection_epochs
            .lock()
            .unwrap()
            .get(id)
            .copied()
            .unwrap_or(0);
        let device = self
            .snapshot()
            .devices
            .into_iter()
            .find(|d| d.id == id && d.connected)
            .ok_or("Device disconnected")?;
        Ok((device, epoch))
    }
    // Reject responses belonging to a removed or replaced connection
    fn ensure_catalog_identity(&self, id: &str, epoch: u64) -> Result<(), String> {
        let (_, current) = self.catalog_identity(id)?;
        if current != epoch {
            return Err("Device connection changed".into());
        }
        Ok(())
    }
    fn firmware_directory_safe(&self) -> bool {
        let dir = self.config_root.join("firmware-cache");
        match dir.symlink_metadata() {
            Ok(m) => m.is_dir() && !m.file_type().is_symlink(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }
    pub(crate) fn firmware_cache_status(
        &self,
        clear: Option<bool>,
    ) -> Result<(crate::cache::CacheFolderStatus, cache_index::Report), String> {
        let mut snapshots = self.firmware_catalog.cache.lock().unwrap();
        if !self.firmware_directory_safe() {
            return Err("Unsafe firmware cache directory".into());
        }
        let dir = self.config_root.join("firmware-cache");
        let index = &self.firmware_catalog.index;
        index.import_once(|| {
            let mut result = vec![];
            for source in SOURCES {
                let name = format!("{}.json", source.id.id());
                let path = dir.join(&name);
                let meta = match path.symlink_metadata() {
                    Ok(m) => m,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.to_string()),
                };
                let mut r = Record::file(name, meta.len());
                r.owned = read_snapshot(&path, source).is_some();
                r.used = cache_index::timestamp(meta.modified().map_err(|e| e.to_string())?);
                result.push(r);
            }
            Ok(result)
        })?;
        let max_bytes = MAX_MANIFEST * 3 * SOURCES.len() as u64;
        let report = if let Some(clear) = clear {
            index.purge(
                Limits {
                    bytes: max_bytes,
                    entries: Some(SOURCES.len()),
                    age: None,
                },
                clear,
                |key| {
                    let Some(source) = SOURCES
                        .into_iter()
                        .find(|s| Some(s.id.id()) == key.strip_suffix(".json"))
                    else {
                        return Ok(Removal::Protected);
                    };
                    let path = dir.join(key);
                    if path
                        .symlink_metadata()
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    {
                        snapshots.remove(source.id.id());
                        return Ok(Removal::Missing);
                    }
                    if read_snapshot(&path, source).is_none() {
                        return Ok(Removal::Protected);
                    }
                    std::fs::remove_file(path).map_err(|e| e.to_string())?;
                    snapshots.remove(source.id.id());
                    Ok(Removal::Removed)
                },
            )?
        } else {
            cache_index::Report {
                stats: index.stats()?,
                ..Default::default()
            }
        };
        Ok((
            crate::cache::CacheFolderStatus {
                id: "firmware-cache".into(),
                label: "Firmware catalogs".into(),
                bytes: report.stats.bytes,
                entries: report.stats.entries,
                max_bytes,
                max_entries: SOURCES.len(),
                max_age_seconds: 0,
                protected_entries: report.stats.protected,
            },
            report,
        ))
    }
    // Lazily recover bounded raw snapshots and derive records with the current parser
    fn load_firmware_cache(&self) {
        let mut cache = self.firmware_catalog.cache.lock().unwrap();
        for source in SOURCES {
            if cache.contains_key(source.id.id()) {
                continue;
            }
            let path = self
                .config_root
                .join("firmware-cache")
                .join(format!("{}.json", source.id.id()));
            if !self.firmware_directory_safe() {
                continue;
            }
            let Some(old) = read_snapshot(&path, source) else {
                continue;
            };
            let _ = self
                .firmware_catalog
                .index
                .touch(&format!("{}.json", source.id.id()), false);
            cache.insert(source.id.id().into(), old);
        }
    }
    // Return cached metadata only, never network or hardware work while opening device settings
    pub fn firmware_catalog_status(&self, id: &str) -> Result<FirmwareCatalogReport, String> {
        let (device, epoch) = self.catalog_identity(id)?;
        let include_test = self.firmware_test_enabled()?;
        self.load_firmware_cache();
        let cache = self.firmware_catalog.cache.lock().unwrap().clone();
        let mut sources = Vec::new();
        let mut candidates = Vec::new();
        for source in SOURCES {
            if source.id == SourceId::Test && !include_test {
                continue;
            }
            let old = cache.get(source.id.id());
            let fresh = old.is_some_and(|c| {
                c.error.is_none()
                    && c.validated <= now()
                    && now().saturating_sub(c.validated) < CACHE_AGE
            });
            sources.push(FirmwareSourceStatus {
                id: source.id.id().into(),
                name: source.name.into(),
                role: if source.id == SourceId::Test {
                    "test"
                } else {
                    "standard"
                }
                .into(),
                url: source.url.into(),
                state: if fresh {
                    "current"
                } else if old.is_some() {
                    "stale"
                } else {
                    "unchecked"
                }
                .into(),
                last_checked: old.filter(|c| c.validated > 0).map(|c| c.validated),
                snapshot_id: old
                    .filter(|c| !c.snapshot_id.is_empty())
                    .map(|c| c.snapshot_id.clone()),
                message: old.and_then(|c| c.error.clone()).unwrap_or_default(),
            });
            if let Some(old) = old {
                if let Ok(parsed) =
                    records::parse(source.id, source.url, &old.snapshot_id, old.raw.as_bytes())
                {
                    if !parsed.issues.is_empty() {
                        let status = sources.last_mut().expect("source status added");
                        status.message = [status.message.clone(), parsed.issues.join("; ")]
                            .into_iter()
                            .filter(|v| !v.is_empty())
                            .collect::<Vec<_>>()
                            .join("; ");
                        if status.state == "current" {
                            status.state = "partial".into();
                        }
                    }
                    candidates.extend(parsed.records);
                }
            }
        }
        let observation = self.firmware_status(id)?;
        let installed_stale = observation.stale;
        let installed = observation.installed;
        let candidates = records::evaluate(
            &device.model_id,
            &device.variant_id,
            installed.as_deref().filter(|_| !installed_stale),
            candidates,
        );
        let incomplete = sources
            .iter()
            .any(|s| s.role == "standard" && s.state != "current");
        // A missing record or unresolved comparison is never converted into a freshness assertion
        let standard: Vec<_> = candidates
            .iter()
            .filter(|c| c.source != SourceId::Test)
            .collect();
        let (state, message) = classify(&standard, incomplete);
        self.ensure_catalog_identity(id, epoch)?;
        Ok(FirmwareCatalogReport {
            device_id: id.into(),
            connection_generation: epoch.to_string(),
            installed,
            installed_stale,
            include_test,
            state: state.into(),
            message: message.into(),
            sources,
            candidates,
        })
    }
    // Coalesce overlapping explicit refreshes while fetching at most three enabled sources independently
    pub fn refresh_firmware_catalog(&self, id: &str) -> Result<FirmwareCatalogReport, String> {
        self.refresh_firmware_catalog_with(id, fetch)
    }
    // Inject only the transport boundary for deterministic cache and connection-lifecycle tests
    fn refresh_firmware_catalog_with(
        &self,
        id: &str,
        fetcher: impl Fn(Source, Option<&Cached>) -> Result<Cached, String> + Sync,
    ) -> Result<FirmwareCatalogReport, String> {
        let (_, epoch) = self.catalog_identity(id)?;
        let generation = match self.firmware_catalog.refresh.try_lock() {
            Ok(g) => *g,
            Err(std::sync::TryLockError::WouldBlock) => {
                let _guard = self.firmware_catalog.refresh.lock().unwrap();
                self.ensure_catalog_identity(id, epoch)?;
                return self.firmware_catalog_status(id);
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("Firmware catalog refresh unavailable".into())
            }
        };
        let mut refresh = self.firmware_catalog.refresh.lock().unwrap();
        if *refresh != generation {
            self.ensure_catalog_identity(id, epoch)?;
            return self.firmware_catalog_status(id);
        }
        self.load_firmware_cache();
        let include_test = self.firmware_test_enabled()?;
        let previous = self.firmware_catalog.cache.lock().unwrap().clone();
        let results = std::thread::scope(|scope| {
            let mut jobs = Vec::new();
            for source in SOURCES {
                if source.id == SourceId::Test && !include_test {
                    continue;
                }
                let old = previous.get(source.id.id()).cloned();
                let fetcher = &fetcher;
                jobs.push((source, scope.spawn(move || fetcher(source, old.as_ref()))));
            }
            jobs.into_iter()
                .map(|(source, job)| {
                    (
                        source,
                        job.join()
                            .unwrap_or_else(|_| Err("Firmware source worker failed".into())),
                    )
                })
                .collect::<Vec<_>>()
        });
        for (source, result) in results {
            match result {
                Ok(value) => {
                    let mut cache = self.firmware_catalog.cache.lock().unwrap();
                    let persistence = save_cache(
                        &self.config_root.join("firmware-cache"),
                        source,
                        &value,
                        &self.firmware_catalog.index,
                    );
                    let mut value = value;
                    if persistence.is_err() {
                        value.error = Some("Catalog cache could not be saved".into());
                    }
                    cache.insert(source.id.id().into(), value);
                }
                Err(error) => {
                    let mut cache = self.firmware_catalog.cache.lock().unwrap();
                    if let Some(old) = cache.get_mut(source.id.id()) {
                        old.error = Some(error);
                        let retained = old.clone();

                        let _ = save_cache(
                            &self.config_root.join("firmware-cache"),
                            source,
                            &retained,
                            &self.firmware_catalog.index,
                        );
                    } else {
                        cache.insert(
                            source.id.id().into(),
                            Cached {
                                parser_version: PARSER_VERSION,
                                raw: "{\"CurrentVersion\":{}}".into(),
                                snapshot_id: String::new(),
                                fetched: 0,
                                validated: 0,
                                etag: None,
                                modified: None,
                                error: Some(error),
                            },
                        );
                    }
                }
            }
        }
        *refresh = refresh.wrapping_add(1);
        self.ensure_catalog_identity(id, epoch)?;
        self.firmware_catalog_status(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    // Supply a single bounded HTTP response without contacting the vendor or touching hardware
    fn response(
        status: &str,
        headers: &str,
        body: Vec<u8>,
    ) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let status = status.to_string();
        let headers = headers.to_string();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 16384);
            }
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
            String::from_utf8(request).unwrap()
        });
        (url, thread)
    }
    // Keep conditional requests tied to accepted bytes and reject a cacheless 304
    #[test]
    fn conditional_fetch_revalidates_exact_snapshot_and_rejects_cacheless_304() {
        let raw=br#"{"CurrentVersion":{"VSD_N4Pro":{"Version":"V4.VSD N4 Pro.02.010","downloadURL":"https://cdn1.key123.vip/StreamDock/firmware/download/test.img"}}}"#.to_vec();
        let (url, server) = response("200 OK", "ETag: \"one\"\r\n", raw);
        let old = fetch_at(SOURCES[0], None, &url).unwrap();
        server.join().unwrap();
        let (url, server) = response("304 Not Modified", "", vec![]);
        let next = fetch_at(SOURCES[0], Some(&old), &url).unwrap();
        assert_eq!(next.snapshot_id, old.snapshot_id);
        assert_eq!(next.raw, old.raw);
        assert!(server
            .join()
            .unwrap()
            .to_lowercase()
            .contains("if-none-match: \"one\""));
        let (url, server) = response("304 Not Modified", "", vec![]);
        assert!(fetch_at(SOURCES[0], None, &url).is_err());
        server.join().unwrap();
    }
    // Never interpret HTML, oversized compressed content, or a test-feed redirect as a standard catalog
    #[test]
    fn transport_rejects_malformed_oversized_and_cross_source_redirects() {
        let (url, server) = response("200 OK", "", b"<html>unavailable</html>".to_vec());
        assert!(fetch_at(SOURCES[0], None, &url).is_err());
        server.join().unwrap();
        let (url, server) = response(
            "302 Found",
            &format!("Location: {}\r\n", SOURCES[2].url),
            vec![],
        );
        assert!(fetch_at(SOURCES[0], None, &url).is_err());
        server.join().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&vec![b' '; MAX_MANIFEST as usize + 1])
            .unwrap();
        assert!(decode_manifest(gzip.finish().unwrap()).is_err());
        let raw = b"{\"CurrentVersion\":{}}";
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(raw).unwrap();
        assert_eq!(decode_manifest(gzip.finish().unwrap()).unwrap(), raw);
    }
    // Never declare equal firmware current when required coverage is absent or compatibility is unresolved
    #[test]
    fn aggregate_classification_needs_complete_supported_standard_coverage() {
        let raw=br#"{"CurrentVersion":{"VSD_N4Pro":{"Version":"V4.VSD N4 Pro.02.010","downloadURL":"https://cdn1.key123.vip/StreamDock/firmware/download/test.img"}}}"#;
        let rows = records::parse(SourceId::Mirabox, SOURCES[0].url, "fixture", raw)
            .unwrap()
            .records;
        let rows = records::evaluate("n4-pro", "vsdinside", Some("V4.VSD N4 Pro.02.010"), rows);
        assert_eq!(rows.len(), 1);
        assert_ne!(classify(&[&rows[0]], false).0, "matches");
        assert_eq!(classify(&[&rows[0]], true).0, "incomplete");
        let mut other = rows[0].clone();
        other.source = SourceId::VsdCraft;
        assert_eq!(classify(&[&rows[0], &other], false).0, "matches");
        other.comparison = records::VersionComparison::Unavailable;
        assert_eq!(classify(&[&rows[0], &other], false).0, "version_unknown");
    }
    // Cache parser updates cannot change immutable snapshot bytes and invalid files are never loaded
    #[test]
    fn cache_roundtrip_is_bounded_and_preserves_previous_snapshot_on_failed_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let raw = "{\"CurrentVersion\":{}}".to_string();
        let old = Cached {
            parser_version: 0,
            snapshot_id: hex::encode(Sha256::digest(raw.as_bytes())),
            raw,
            fetched: 12,
            validated: 14,
            etag: Some("version-one".into()),
            modified: None,
            error: None,
        };
        save_cache(
            dir.path(),
            SOURCES[0],
            &old,
            &Table::new(&dir.path().canonicalize().unwrap(), Kind::Firmware),
        )
        .unwrap();
        let before = std::fs::read(dir.path().join("mirabox.json")).unwrap();
        let (url, server) = response("404 Not Found", "", vec![]);
        assert!(fetch_at(SOURCES[0], Some(&old), &url).is_err());
        server.join().unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("mirabox.json")).unwrap(),
            before
        );
    }
    #[test]
    fn firmware_cache_tracks_saved_snapshots_and_clears_valid_idle_files() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        // Complete the legacy import before publishing.
        assert_eq!(runtime.firmware_cache_status(None).unwrap().0.entries, 0);
        let dir = root.path().join("firmware-cache");
        let raw = r#"{"CurrentVersion":{}}"#.to_string();
        let value = Cached {
            parser_version: PARSER_VERSION,
            snapshot_id: hex::encode(Sha256::digest(&raw)),
            raw,
            fetched: 1,
            validated: 1,
            etag: None,
            modified: None,
            error: None,
        };
        save_cache(&dir, SOURCES[0], &value, &runtime.firmware_catalog.index).unwrap();
        runtime.load_firmware_cache();
        assert_eq!(runtime.firmware_catalog.cache.lock().unwrap().len(), 1);
        let name = format!("{}.json", SOURCES[0].id.id());
        runtime.firmware_catalog.index.set_used(&name, 1).unwrap();
        assert_eq!(
            runtime
                .firmware_cache_status(Some(false))
                .unwrap()
                .0
                .entries,
            1
        );
        assert_eq!(runtime.clear_cache().unwrap().removed_entries, 1);
        assert!(runtime.firmware_catalog.cache.lock().unwrap().is_empty());
        assert!(!dir.join(name).exists());
        runtime.shutdown();
    }
    // Test-feed consent remains off until an explicit persisted preference change
    #[test]
    fn test_preference_defaults_off_and_changes_explicitly() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        assert!(!runtime.firmware_test_enabled().unwrap());
        runtime.set_firmware_catalog_test(true).unwrap();
        assert!(runtime.firmware_test_enabled().unwrap());
        runtime.set_firmware_catalog_test(false).unwrap();
        assert!(!runtime.firmware_test_enabled().unwrap());
        runtime.shutdown();
    }
    // Disabled test sources make no request, failures retain metadata, and replaced connections reject results
    #[test]
    fn independent_sources_preserve_snapshot_and_disconnect_generation() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:catalog";
        runtime.simulate_device(true, id).unwrap();
        let calls = Mutex::new(Vec::new());
        let fixture = |source: Source, _old: Option<&Cached>| {
            calls.lock().unwrap().push(source.id);
            let raw = "{\"CurrentVersion\":{}}".to_string();
            Ok(Cached {
                parser_version: PARSER_VERSION,
                snapshot_id: hex::encode(Sha256::digest(raw.as_bytes())),
                raw,
                fetched: now(),
                validated: now(),
                etag: None,
                modified: None,
                error: None,
            })
        };
        let report = runtime.refresh_firmware_catalog_with(id, fixture).unwrap();
        assert_eq!(report.sources.len(), 2);
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert!(!calls.lock().unwrap().contains(&SourceId::Test));
        let old = runtime
            .firmware_catalog
            .cache
            .lock()
            .unwrap()
            .get("mirabox")
            .unwrap()
            .snapshot_id
            .clone();
        let failed = runtime
            .refresh_firmware_catalog_with(id, |source, previous| {
                if source.id == SourceId::Mirabox {
                    Err("controlled failure".into())
                } else {
                    Ok(previous.unwrap().clone())
                }
            })
            .unwrap();
        assert_eq!(failed.state, "incomplete");
        assert_eq!(
            runtime
                .firmware_catalog
                .cache
                .lock()
                .unwrap()
                .get("mirabox")
                .unwrap()
                .snapshot_id,
            old
        );
        runtime.set_firmware_catalog_test(true).unwrap();
        let report = runtime
            .refresh_firmware_catalog_with(id, |source, old| {
                if source.id == SourceId::Test {
                    runtime.set_firmware_catalog_test(false).unwrap();
                }
                fixture(source, old)
            })
            .unwrap();
        assert!(!report.include_test);
        assert_eq!(report.sources.len(), 2);
        let error = runtime
            .refresh_firmware_catalog_with(id, |source, old| {
                if source.id == SourceId::Mirabox {
                    *runtime
                        .connection_epochs
                        .lock()
                        .unwrap()
                        .get_mut(id)
                        .unwrap() += 1;
                }
                fixture(source, old)
            })
            .unwrap_err();
        assert!(error.contains("connection changed"));
        runtime.shutdown();
    }
    // The dedicated setter produces records accepted by both startup and validated snapshot reopen
    #[test]
    fn firmware_test_catalog_real_setter_survives_validated_reopen() {
        let root = tempfile::tempdir().unwrap();
        for value in [true, false] {
            let runtime = Runtime::build(root.path().into(), false).unwrap();
            runtime.set_firmware_catalog_test(value).unwrap();
            runtime
                .profiles
                .storage
                .run(|connection| crate::application_data::validate_snapshot(connection))
                .unwrap();
            runtime.shutdown();
            drop(runtime);
            crate::application_data::maintain(root.path().into(), "status", None).unwrap();
            let runtime = Runtime::build(root.path().into(), false).unwrap();
            assert_eq!(runtime.firmware_test_enabled().unwrap(), value);
            runtime.shutdown();
            drop(runtime);
        }
    }
    // Invalid persisted consent remains recoverable and its original database is never silently reset
    #[test]
    fn firmware_test_catalog_malformed_preserved_and_recovery_is_revision_guarded() {
        use stream_dock_manager_storage::Storage;
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime.set_firmware_catalog_test(true).unwrap();
        runtime.shutdown();
        drop(runtime);
        {
            let storage = Storage::open(root.path().into()).unwrap();
            storage
                .write_json(
                    PREF.domain,
                    PREF.key,
                    &serde_json::json!("invalid"),
                    PREF.max_bytes,
                )
                .unwrap();
        }
        let path = stream_dock_manager_storage::database_path(
            root.path(),
            stream_dock_manager_storage::DATABASE_NAME,
        );
        let before = std::fs::read(&path).unwrap();
        assert!(Runtime::build(root.path().into(), false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let recovery = crate::configuration_recovery::ConfigurationRecovery::open(
            root.path().into(),
            "test invalid consent".into(),
        )
        .unwrap();
        let doc = recovery
            .document(&format!("setting:{}:{}", PREF.domain.table(), PREF.key))
            .unwrap();
        assert!(!doc.issues.is_empty());
        assert_eq!(doc.json, "\"invalid\"");
        assert!(
            !recovery
                .validate(&doc.id, &doc.revision, "null")
                .unwrap()
                .valid
        );
        assert!(
            !recovery
                .validate(&doc.id, &doc.revision, "\"false\"")
                .unwrap()
                .valid
        );
        assert!(
            recovery
                .validate(&doc.id, &doc.revision, "false")
                .unwrap()
                .valid
        );
        assert_eq!(recovery.document(&doc.id).unwrap().json, doc.json);
        let report = recovery.save(&doc.id, &doc.revision, "false").unwrap();
        assert!(report.last_backup.is_some());
        assert!(recovery.save(&doc.id, &doc.revision, "true").is_err());
        let repaired = recovery.document(&doc.id).unwrap();
        assert_eq!(repaired.json, "false");
        recovery.delete(&repaired.id, &repaired.revision).unwrap();
        assert!(recovery.document(&doc.id).is_err());
        drop(recovery);
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        assert!(!runtime.firmware_test_enabled().unwrap());
        runtime.shutdown();
    }
}
