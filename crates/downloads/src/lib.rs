//! Bounded file-based artifact transfers. Callers retain origin, authenticity and
//! publication policy; this crate never executes or installs downloaded code.
mod storage;
use reqwest::{header, redirect::Policy, Url};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use storage::{Entry, Metadata};
// Run network
fn run_network<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let result = runtime.block_on(future);
    // DNS may use an OS blocking resolver. Do not let runtime teardown wait
    // indefinitely for an already dispatched resolver after a request timeout.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

const MAX_ARTIFACT: u64 = 2 * 1024 * 1024 * 1024;
const CHECKPOINT: u64 = 1024 * 1024;
#[derive(Clone)]
pub struct ArtifactSpec {
    /// Caller-owned immutable source identity (provider/release/target)
    pub identity: String,
    pub url: String,
    /// Expected publisher digest. Empty is accepted only by the explicit
    /// `*_from_origin` entry points, which delegate publisher trust to the caller.
    pub sha256: String,
    pub size: Option<u64>,
    pub limit: u64,
}
impl ArtifactSpec {
    // Validate for the current operation
    fn validate(&self) -> Result<(), String> {
        self.validate_digest(false)
    }
    // Validate digest
    fn validate_digest(&self, trust_origin: bool) -> Result<(), String> {
        let url = Url::parse(&self.url).map_err(|_| "Invalid artifact URL")?;
        if self.identity.is_empty()
            || self.identity.len() > 4096
            || self.url.len() > 8192
            || !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.username() != ""
            || url.password().is_some()
            || !(trust_origin && self.sha256.is_empty()
                || self.sha256.len() == 64 && self.sha256.bytes().all(|b| b.is_ascii_hexdigit()))
            || self.limit == 0
            || self.limit > MAX_ARTIFACT
            || self.size.is_some_and(|size| size == 0 || size > self.limit)
        {
            return Err("Invalid immutable artifact identity, digest or size bound".into());
        }
        Ok(())
    }
    // Derive the lookup key for this value
    fn key(&self) -> String {
        let value = serde_json::json!([
            self.identity,
            self.url,
            self.sha256.to_ascii_lowercase(),
            self.size,
            self.limit
        ]);
        hex::encode(Sha256::digest(value.to_string().as_bytes()))
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Continue,
    Pause,
    Cancel,
}
pub type RequestHeaders<'a> = dyn Fn(&Url) -> Result<header::HeaderMap, String> + 'a;
pub struct NetworkPolicy<'a> {
    /// Request headers are recomputed per hop so credentials never follow redirects
    pub headers: Option<&'a RequestHeaders<'a>>,

    /// Invoked before every request, including all redirects. `redirect` is false
    /// for the immutable initial URL and true for subsequent locations.
    pub validate: &'a dyn Fn(&Url, bool) -> Result<(), String>,
    pub timeout: Duration,
}
/// Small provider metadata remains bounded in memory. Archive callers must use
/// the file-backed artifact path instead. Every redirect is checked first.
pub fn metadata(
    url: &str,
    network: NetworkPolicy<'_>,
    limit: usize,
    progress: &dyn Fn(u64, Option<u64>) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    if limit == 0
        || limit > 8 * 1024 * 1024
        || network.timeout.is_zero()
        || network.timeout > Duration::from_secs(300)
    {
        return Err("Invalid metadata bounds".into());
    }
    run_network(async {
        progress(0, None)?;
        let client = network_client(network.timeout)?;
        let mut current = Url::parse(url).map_err(|_| "Invalid metadata URL")?;
        for hop in 0..6 {
            progress(0, None)?;
            (network.validate)(&current, hop > 0)?;
            let mut request = client.get(current.clone());
            if let Some(headers) = network.headers {
                request = request.headers(headers(&current)?);
            }
            let mut response = request
                .send()
                .await
                .map_err(|_| "Provider metadata request interrupted")?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .ok_or("Invalid metadata redirect")?;
                current = current
                    .join(location)
                    .map_err(|_| "Invalid metadata redirect")?;
                continue;
            }
            if !response.status().is_success() {
                return Err(format!(
                    "Provider metadata HTTP {}",
                    response.status().as_u16()
                ));
            }
            let total = response.content_length();
            if total.is_some_and(|total| total > limit as u64) {
                return Err("Provider metadata exceeds size limit".into());
            }
            let mut bytes = vec![];
            loop {
                progress(bytes.len() as u64, total)?;
                let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| "Provider metadata body interrupted")?
                else {
                    break;
                };
                if bytes.len().saturating_add(chunk.len()) > limit {
                    return Err("Provider metadata exceeds size limit".into());
                }
                bytes.extend_from_slice(&chunk);
            }
            return Ok(bytes);
        }
        Err("Too many metadata redirects".into())
    })
}
// Construct the HTTP client with the requested timeout
fn network_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("StreamDockManager/verified-downloads")
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(5))
        .redirect(Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}
/// Retention bounds apply to idle entries; active transfers and consumers are
/// always protected and can temporarily exceed these limits.
#[derive(Clone, Copy, Debug)]
pub struct CacheLimits {
    pub max_age: Duration,
    pub max_bytes: u64,
    pub max_entries: usize,
}
impl Default for CacheLimits {
    // Construct the default value
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(24 * 60 * 60),
            max_bytes: 2 * 1024 * 1024 * 1024,
            max_entries: 32,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct CacheStats {
    /// File lengths including metadata and safely measurable preserved files (not reservations)
    pub bytes: u64,
    pub entries: usize,
    pub active_entries: usize,
    /// Unrecognized or unsafe entries preserved and included in the entry count
    pub skipped_entries: usize,
    /// Some preserved contents could not be measured safely; bytes is a lower bound
    pub usage_incomplete: bool,
}
#[derive(Clone, Debug, Default)]
pub struct PruneReport {
    pub removed_entries: usize,
    pub removed_bytes: u64,
    pub active_entries: usize,
    pub skipped_entries: usize,
    pub stats: CacheStats,
    pub errors: Vec<String>,
}
pub struct DownloadCache {
    index: stream_dock_manager_storage::cache_index::Table,
    root: PathBuf,
    limits: CacheLimits,
}
pub struct Artifact {
    entry: Entry,
    size: u64,
}
impl Artifact {
    // Return the path of this resource
    pub fn path(&self) -> &Path {
        &self.entry.body
    }
    // Return the size of this resource
    pub fn size(&self) -> u64 {
        self.size
    }
}
impl DownloadCache {
    // Open for the current operation
    pub fn open(root: PathBuf) -> Result<Self, String> {
        Self::open_with_limits(root, CacheLimits::default())
    }
    // Open with limits
    pub fn open_with_limits(root: PathBuf, limits: CacheLimits) -> Result<Self, String> {
        storage::directory(&root)?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        Ok(Self {
            index: storage::table(&root),
            root,
            limits,
        })
    }
    // Read the current subsystem statistics
    pub fn stats(&self) -> Result<CacheStats, String> {
        storage::stats(&self.root, &self.index)
    }
    // Prune for the current operation
    pub fn prune(&self) -> Result<PruneReport, String> {
        storage::prune(&self.root, &self.index, self.limits, false)
    }
    /// Delete only recognized idle entries. Live Artifact handles remain usable
    pub fn clear(&self) -> Result<PruneReport, String> {
        storage::prune(&self.root, &self.index, self.limits, true)
    }
    // Discard
    pub fn discard(&self, spec: &ArtifactSpec) -> Result<(), String> {
        spec.validate()?;
        let entry = storage::acquire_indexed(&self.root, spec, &self.index)?;
        entry.discard()
    }
    /// Discard idle entries for one caller identity, even if freshly verified
    /// metadata has not yet resolved its expected checksum. Live owners remain
    /// protected and perform their own explicit cancellation cleanup.
    pub fn discard_identity(&self, identity: &str) -> Result<(), String> {
        storage::discard_identity(&self.root, &self.index, identity)
    }
    /// Store an artifact whose publisher is trusted through its origin, without
    /// an expected publisher digest. Local checkpoint hashes still detect damage.
    pub fn store_reader_from_origin(
        &self,
        spec: &ArtifactSpec,
        reader: &mut dyn Read,
        control: &dyn Fn() -> Control,
        progress: &dyn Fn(u64, Option<u64>) -> Result<(), String>,
    ) -> Result<Artifact, String> {
        spec.validate_digest(true)?;
        self.store_reader_inner(spec, reader, control, progress)
    }
    // Store streamed artifact bytes using the selected origin policy
    fn store_reader_inner(
        &self,
        spec: &ArtifactSpec,
        reader: &mut dyn Read,
        control: &dyn Fn() -> Control,
        progress: &dyn Fn(u64, Option<u64>) -> Result<(), String>,
    ) -> Result<Artifact, String> {
        let mut entry = storage::acquire_indexed(&self.root, spec, &self.index)?;
        entry.check_control(control())?;
        let mut file = storage::private_file(&entry.body)?;
        entry.reset(&mut file)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        let mut checkpoint = 0;
        let result = (|| -> Result<(), String> {
            loop {
                entry.check_control(control())?;
                progress(entry.meta.completed, spec.size)?;
                let count = reader
                    .read(&mut buffer)
                    .map_err(|_| "Injected artifact transfer interrupted")?;
                if count == 0 {
                    break;
                }
                let size = entry
                    .meta
                    .completed
                    .checked_add(count as u64)
                    .ok_or("Artifact size overflow")?;
                if size > spec.limit || spec.size.is_some_and(|total| size > total) {
                    entry.discard()?;
                    return Err("Artifact body exceeds declared size".into());
                }
                file.write_all(&buffer[..count])
                    .map_err(|e| e.to_string())?;
                hash.update(&buffer[..count]);
                entry.meta.completed = size;
                if size - checkpoint >= CHECKPOINT {
                    entry.checkpoint(&mut file, &hash)?;
                    checkpoint = size;
                }
            }
            if entry.meta.completed == 0
                || spec.size.is_some_and(|size| size != entry.meta.completed)
                || !spec.sha256.is_empty()
                    && hex::encode(hash.clone().finalize()) != spec.sha256.to_ascii_lowercase()
            {
                entry.discard()?;
                return Err("Artifact SHA-256/size mismatch; partial discarded".into());
            }
            entry.meta.total = Some(entry.meta.completed);
            Ok(())
        })();
        if control() == Control::Cancel {
            entry.discard()?;
            return Err("Download cancelled; partial discarded".into());
        }
        if entry.meta_path.exists() {
            entry.checkpoint(&mut file, &hash)?;
        }
        result?;
        entry.check_control(control())?;
        Ok(Artifact {
            size: entry.meta.completed,
            entry,
        })
    }
    // Download for the current operation
    pub fn download(
        &self,
        spec: &ArtifactSpec,
        network: NetworkPolicy<'_>,
        control: &dyn Fn() -> Control,
        progress: &dyn Fn(u64, Option<u64>) -> Result<(), String>,
    ) -> Result<Artifact, String> {
        spec.validate()?;
        run_network(self.download_inner(spec, network, control, progress))
    }
    /// Fetch from caller-authorized origins without requiring a publisher digest
    /// The caller must validate HTTPS origins on every hop. Resuming requires a
    /// matching strong ETag; otherwise the transfer restarts from byte zero.
    pub fn download_from_origin(
        &self,
        spec: &ArtifactSpec,
        network: NetworkPolicy<'_>,
        control: &dyn Fn() -> Control,
        progress: &dyn Fn(u64, Option<u64>) -> Result<(), String>,
    ) -> Result<Artifact, String> {
        spec.validate_digest(true)?;
        run_network(self.download_inner(spec, network, control, progress))
    }
    // Download inner
    async fn download_inner(
        &self,
        spec: &ArtifactSpec,
        network: NetworkPolicy<'_>,
        control: &dyn Fn() -> Control,
        progress: &dyn Fn(u64, Option<u64>) -> Result<(), String>,
    ) -> Result<Artifact, String> {
        spec.validate_digest(true)?;
        if network.timeout.is_zero() || network.timeout > Duration::from_secs(300) {
            return Err("Invalid download timeout".into());
        }
        let initial = Url::parse(&spec.url).map_err(|_| "Invalid artifact URL")?;
        (network.validate)(&initial, false)?;
        let mut entry = storage::acquire_indexed(&self.root, spec, &self.index)?;
        let mut file = storage::private_file(&entry.body)?;
        let mut hash = Sha256::new();
        // A crash may leave bytes beyond the last fsynced checkpoint. Retain only
        // the journaled prefix, and rehash it before sending a Range request.
        let length = file.metadata().map_err(|e| e.to_string())?.len();
        if length < entry.meta.completed || length > spec.limit {
            entry.reset(&mut file)?;
        }
        file.set_len(entry.meta.completed)
            .map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut buffer = [0u8; 65536];
        loop {
            entry.check_control(control())?;
            let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if hex::encode(hash.clone().finalize()) != entry.meta.prefix_sha256 {
            entry.reset(&mut file)?;
            hash = Sha256::new();
        }
        if let Err(error) = progress(entry.meta.completed, entry.meta.total) {
            entry.check_control(control())?;
            return Err(error);
        }
        entry.check_control(control())?;
        if entry.meta.total == Some(entry.meta.completed) && entry.meta.completed > 0 {
            if spec.sha256.is_empty()
                || hex::encode(hash.clone().finalize()) == spec.sha256.to_ascii_lowercase()
            {
                return Ok(Artifact {
                    size: entry.meta.completed,
                    entry,
                });
            }
            entry.reset(&mut file)?;
            return Err("Retained artifact checksum mismatch; discarded unsafe partial".into());
        }
        if spec.sha256.is_empty() && entry.meta.completed > 0 && entry.meta.etag.is_none() {
            entry.reset(&mut file)?;
            hash = Sha256::new();
        }
        let client = network_client(network.timeout)?;
        let offset = entry.meta.completed;
        let mut current = Url::parse(&spec.url).map_err(|_| "Invalid artifact URL")?;
        let mut response = None;
        for hop in 0..6 {
            entry.check_control(control())?;
            (network.validate)(&current, hop > 0)?;
            let mut request = client
                .get(current.clone())
                .header(header::ACCEPT_ENCODING, "identity");
            if let Some(headers) = network.headers {
                request = request.headers(headers(&current)?);
            }
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
                if let Some(validator) = entry.meta.etag.as_ref() {
                    request = request.header(header::IF_RANGE, validator);
                }
            }
            let result = match request.send().await {
                Ok(response) => response,
                Err(_) => {
                    entry.check_control(control())?;
                    return Err(
                        "Download request interrupted; Retry resumes the retained partial".into(),
                    );
                }
            };
            if result.status().is_redirection() {
                let location = result
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|h| h.to_str().ok())
                    .ok_or("Invalid artifact redirect")?;
                current = current
                    .join(location)
                    .map_err(|_| "Invalid artifact redirect")?;
                continue;
            }
            response = Some(result);
            break;
        }
        let mut response = response.ok_or("Too many artifact redirects")?;
        entry.check_control(control())?;
        let headers = response.headers();
        if headers
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            entry.discard()?;
            return Err("Encoded artifact response cannot be resumed".into());
        }
        let expected_response = match response.status().as_u16() {
            200 => {
                if offset > 0 {
                    entry.reset(&mut file)?;
                    hash = Sha256::new();
                }
                let total = response.content_length().or(spec.size);
                if total.is_some_and(|total| {
                    total > spec.limit || spec.size.is_some_and(|size| size != total)
                }) {
                    entry.discard()?;
                    return Err("Artifact response exceeds or changes its declared size".into());
                }
                entry.meta.total = total;
                response.content_length()
            }
            206 => {
                let range = headers
                    .get(header::CONTENT_RANGE)
                    .and_then(|h| h.to_str().ok())
                    .and_then(content_range);
                let valid = range.filter(|(start, end, total)| {
                    offset > 0
                        && *start == offset
                        && *end >= *start
                        && *end < *total
                        && *total <= spec.limit
                        && spec.size.is_none_or(|size| size == *total)
                        && entry.meta.total.is_none_or(|prior| prior == *total)
                        && response
                            .content_length()
                            .is_none_or(|length| length == end - start + 1)
                });
                let Some((start, end, total)) = valid else {
                    entry.discard()?;
                    return Err("Artifact Content-Range does not match the retained prefix".into());
                };
                if changed_validator(&entry.meta, headers)
                    || spec.sha256.is_empty()
                        && strong_etag(headers.get(header::ETAG).and_then(|v| v.to_str().ok()))
                            != entry.meta.etag
                {
                    entry.discard()?;
                    return Err("Artifact validator changed during resume".into());
                }
                entry.meta.total = Some(total);
                Some(end - start + 1)
            }
            416 => {
                entry.discard()?;
                return Err("Artifact range is no longer valid; partial discarded".into());
            }
            status => {
                return Err(format!(
                    "Artifact service HTTP {status}; Retry preserves the retained partial"
                ))
            }
        };
        entry.meta.etag = strong_etag(headers.get(header::ETAG).and_then(|v| v.to_str().ok()));
        entry.meta.modified = headers
            .get(header::LAST_MODIFIED)
            .and_then(|v| v.to_str().ok())
            .filter(|s| s.len() <= 128 && !s.contains(['\r', '\n']))
            .map(String::from);
        entry.checkpoint(&mut file, &hash)?;
        let initial = entry.meta.completed;
        let mut checkpoint = initial;
        let transfer = async {
            loop {
                entry.check_control(control())?;
                progress(entry.meta.completed, entry.meta.total)?;
                let Some(bytes) = response
                    .chunk()
                    .await
                    .map_err(|_| "Artifact body interrupted; Retry resumes the retained partial")?
                else {
                    break;
                };
                for data in bytes.chunks(buffer.len()) {
                    entry.check_control(control())?;
                    let count = data.len();
                    let size = entry
                        .meta
                        .completed
                        .checked_add(count as u64)
                        .ok_or("Artifact size overflow")?;
                    if size > spec.limit
                        || entry.meta.total.is_some_and(|total| size > total)
                        || expected_response.is_some_and(|length| size - initial > length)
                    {
                        entry.discard()?;
                        return Err("Artifact body exceeds declared size".into());
                    }
                    file.write_all(data).map_err(|e| e.to_string())?;
                    hash.update(data);
                    entry.meta.completed = size;
                    if size - checkpoint >= CHECKPOINT {
                        entry.checkpoint(&mut file, &hash)?;
                        checkpoint = size;
                    }
                }
            }
            if expected_response.is_some_and(|length| entry.meta.completed - initial != length) {
                return Err("Incomplete artifact body; Retry resumes the retained partial".into());
            }
            if entry.meta.total.is_none() {
                entry.meta.total = Some(entry.meta.completed);
            }
            if entry.meta.total != Some(entry.meta.completed) {
                return Err("Incomplete artifact range; Retry resumes the retained partial".into());
            }
            if entry.meta.completed == 0
                || !spec.sha256.is_empty()
                    && hex::encode(hash.clone().finalize()) != spec.sha256.to_ascii_lowercase()
            {
                entry.discard()?;
                return Err("Artifact SHA-256 checksum mismatch; partial discarded".into());
            }
            Ok::<(), String>(())
        }
        .await;
        if control() == Control::Cancel {
            entry.discard()?;
            return Err("Download cancelled; partial discarded".into());
        }
        // Persist complete accepted writes on pause/read failure, including bytes
        // below the periodic checkpoint. A failed write leaves an unjournaled
        // tail which the next owner truncates before rehashing.
        if entry.meta_path.exists() {
            entry.checkpoint(&mut file, &hash)?;
        }
        transfer?;
        entry.check_control(control())?;
        if let Err(error) = progress(entry.meta.completed, entry.meta.total) {
            entry.check_control(control())?;
            return Err(error);
        }
        entry.check_control(control())?;
        Ok(Artifact {
            size: entry.meta.completed,
            entry,
        })
    }
}
#[cfg(test)]
mod tests;
// Parse a response byte range and its total length
fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}
// Accept only a strong entity tag for resumable downloads
fn strong_etag(value: Option<&str>) -> Option<String> {
    value
        .filter(|s| {
            s.len() >= 2
                && s.len() <= 4096
                && s.starts_with('"')
                && s.ends_with('"')
                && s.as_bytes()[1..s.len() - 1]
                    .iter()
                    .all(|b| *b >= 0x21 && *b != b'"' && *b != 0x7f)
                && header::HeaderValue::from_str(s).is_ok()
        })
        .map(String::from)
}
// Detect a changed server validator before reusing partial bytes
fn changed_validator(meta: &Metadata, headers: &header::HeaderMap) -> bool {
    if let Some(etag) = &meta.etag {
        return headers
            .get(header::ETAG)
            .is_some_and(|value| value.as_bytes() != etag.as_bytes());
    }
    meta.modified.as_ref().is_some_and(|modified| {
        headers
            .get(header::LAST_MODIFIED)
            .is_some_and(|value| value.as_bytes() != modified.as_bytes())
    })
}
