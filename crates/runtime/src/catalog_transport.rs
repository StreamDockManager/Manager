//! Catalog-only conditional transport. Private metadata never enters the disk API cache.
use super::*;
use std::collections::VecDeque;

const BODY_LIMIT: usize = 4 * 1024 * 1024;
// Encoded-size admission budget; parsed heap and in-flight Arc owners are separate.
const CACHE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 12; // Two hosted channels or at most ten release-list pages.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CatalogKind {
    Hosted,
    Listing,
}
#[derive(Clone)]
pub(super) enum ParsedCatalog {
    Hosted(PluginCatalog),
    Listing(Vec<serde_json::Value>),
}
pub(super) struct CatalogRequest<'a> {
    pub source: &'a str,
    pub repository: &'a str,
    pub channel: &'a str,
    pub url: &'a str,
    pub generation: u64,
    pub token: Option<&'a str>,
    pub kind: CatalogKind,
}
impl CatalogRequest<'_> {
    fn key(&self) -> String {
        hex::encode(Sha256::digest(
            serde_json::to_vec(&(
                "catalog-conditional-v1",
                self.source,
                self.repository,
                self.channel,
                self.url,
                self.generation,
                self.kind == CatalogKind::Listing,
                self.token
                    .map(|t| hex::encode(Sha256::digest(t.as_bytes()))),
            ))
            .expect("scalar catalog identity"),
        ))
    }
    fn validate(&self) -> Result<(), String> {
        if !repository_valid(self.repository) {
            return Err("Invalid catalog repository".into());
        }
        let url = Url::parse(self.url).map_err(|_| "Invalid catalog URL")?;
        #[cfg(test)]
        if url.scheme() == "http" && url.host_str() == Some("127.0.0.1") {
            return Ok(());
        }
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("Invalid catalog transport origin".into());
        }
        match self.kind {
            CatalogKind::Hosted => {
                if self.token.is_some() || !["stable", "development"].contains(&self.channel) {
                    return Err("Invalid hosted catalog context".into());
                }
                let base = Url::parse(&resolve_catalog_url(Some(self.source), None)?)
                    .map_err(|_| "Invalid catalog source")?;
                let expected = if self.channel == "stable" {
                    base
                } else {
                    base.join("development/catalog.json")
                        .map_err(|_| "Invalid catalog source")?
                };
                if url != expected {
                    return Err("Catalog channel/source mismatch".into());
                }
            }
            CatalogKind::Listing => {
                let source = format!("https://api.github.com/repos/{}/releases", self.repository);
                let valid =
                    (1..=10).any(|page| self.url == format!("{source}?per_page=100&page={page}"));
                if self.source != source || self.channel != "releases" || !valid {
                    return Err("Invalid release-list source".into());
                }
            }
        }
        Ok(())
    }
    fn validate_value(&self, value: &ParsedCatalog) -> Result<(), String> {
        match (self.kind, value) {
            (CatalogKind::Hosted, ParsedCatalog::Hosted(value)) => {
                value.validate_source(self.repository, self.channel, Some(self.source))
            }
            (CatalogKind::Listing, ParsedCatalog::Listing(value)) => {
                if value.len() > 100
                    || value.iter().any(|release| {
                        !release.is_object()
                            || release["id"].as_u64().is_none_or(|id| id == 0)
                            || release["draft"].as_bool().is_none()
                            || release["prerelease"].as_bool().is_none()
                            || release["tag_name"].as_str().is_none()
                            || !release["assets"].is_array()
                            || release["url"].as_str().is_none_or(|url| {
                                !url.eq_ignore_ascii_case(&format!(
                                    "https://api.github.com/repos/{}/releases/{}",
                                    self.repository,
                                    release["id"].as_u64().unwrap_or(0)
                                ))
                            })
                    })
                {
                    return Err("Invalid GitHub release-list identity/shape".into());
                }
                Ok(())
            }
            _ => Err("Catalog validation contract mismatch".into()),
        }
    }
    fn parse(&self, bytes: &[u8]) -> Result<ParsedCatalog, String> {
        let value = match self.kind {
            CatalogKind::Hosted => ParsedCatalog::Hosted(
                serde_json::from_slice(bytes)
                    .map_err(|_| "Invalid plugin catalog JSON (expected schema 2)")?,
            ),
            CatalogKind::Listing => ParsedCatalog::Listing(
                serde_json::from_slice(bytes).map_err(|_| "Invalid GitHub releases response")?,
            ),
        };
        self.validate_value(&value)?;
        Ok(value)
    }
}
#[derive(Clone)]
struct ConditionalEntry {
    key: String,
    etag: String,
    value: Arc<ParsedCatalog>,
    bytes: usize,
}
#[derive(Default)]
pub(super) struct ConditionalCache {
    generation: u64,
    next: u64,
    pending: VecDeque<(String, u64)>,
    entries: VecDeque<ConditionalEntry>,
}
impl ConditionalCache {
    fn begin(&mut self, generation: u64, key: &str) -> (u64, Option<ConditionalEntry>) {
        if self.generation != generation {
            self.entries.clear();
            self.pending.clear();
            self.generation = generation;
        }
        self.next = self.next.wrapping_add(1);
        self.pending.retain(|(old, _)| old != key);
        self.pending.push_back((key.into(), self.next));
        while self.pending.len() > 24 {
            self.pending.pop_front();
        }
        (
            self.next,
            self.entries.iter().find(|entry| entry.key == key).cloned(),
        )
    }
    fn current(&self, key: &str, sequence: u64) -> bool {
        self.pending.iter().any(|(k, n)| k == key && *n == sequence)
    }
    fn insert(&mut self, entry: ConditionalEntry) {
        self.entries.retain(|old| old.key != entry.key);
        self.entries.push_back(entry);
        while self.entries.len() > CACHE_ENTRIES
            || self.entries.iter().map(|e| e.bytes).sum::<usize>() > CACHE_BYTES
        {
            self.entries.pop_front();
        }
    }
}
#[cfg(test)]
#[derive(Default)]
pub(super) struct CatalogStats {
    pub transferred: u64,
    pub parses: u64,
    pub hits: u64,
    pub retries: u64,
}
impl UpdateService {
    pub(super) fn catalog_fetch(
        &self,
        client: &Client,
        request: CatalogRequest<'_>,
        cancel: &AtomicBool,
    ) -> Result<Arc<ParsedCatalog>, String> {
        request.validate()?;
        let key = request.key();
        let (sequence, cached) = {
            let store = self.store.lock().unwrap();
            if store.catalog_generation != request.generation || cancel.load(Ordering::SeqCst) {
                return Err("Catalog context changed or cancelled".into());
            }
            self.catalog_http_cache
                .lock()
                .unwrap()
                .begin(request.generation, &key)
        };
        // Immutable entries are created only after validation. Reject a damaged/mismatched
        // entry defensively rather than attaching its validator to an unusable result.
        let cached = cached.filter(|entry| {
            request.validate_value(&entry.value).is_ok()
                && !entry.etag.is_empty()
                && entry.etag.len() <= 1024
        });
        for attempt in 0..2 {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".into());
            }
            let mut outgoing = client.get(request.url);
            if request.kind == CatalogKind::Listing {
                outgoing = outgoing
                    .header("Accept", "application/vnd.github+json")
                    .header("X-GitHub-Api-Version", "2022-11-28");
            }
            if let Some(token) = request.token {
                outgoing = outgoing.bearer_auth(token);
            }
            if attempt == 0 {
                if let Some(entry) = &cached {
                    outgoing = outgoing.header("If-None-Match", &entry.etag);
                }
            }
            // The existing client forbids automatic redirects. Do not forward validators/auth.
            let mut response = outgoing.send().map_err(|_| "Catalog request failed")?;
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".into());
            }
            let status = response.status().as_u16();
            if status == 304 {
                if attempt == 0 {
                    if let Some(entry) = &cached {
                        let store = self.store.lock().unwrap();
                        let cache = self.catalog_http_cache.lock().unwrap();
                        if store.catalog_generation != request.generation
                            || !cache.current(&key, sequence)
                            || cancel.load(Ordering::SeqCst)
                        {
                            return Err("Catalog context changed or cancelled".into());
                        }
                        #[cfg(test)]
                        {
                            self.catalog_http_stats.lock().unwrap().hits += 1;
                        }
                        return Ok(entry.value.clone());
                    }
                    #[cfg(test)]
                    {
                        self.catalog_http_stats.lock().unwrap().retries += 1;
                    }
                    continue;
                }
                return Err("Conditional catalog response has no matching validated body".into());
            }
            if status == 429 || (status == 403 && request.kind == CatalogKind::Listing) {
                let until = response
                    .headers()
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .or_else(|| {
                        response
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|delay| now().saturating_add(delay))
                    })
                    .unwrap_or_else(|| now().saturating_add(60));
                let mut store = self.store.lock().unwrap();
                if store.catalog_generation == request.generation
                    && !cancel.load(Ordering::SeqCst)
                    && self
                        .catalog_http_cache
                        .lock()
                        .unwrap()
                        .current(&key, sequence)
                {
                    store.plugins.retry_after = Some(until);
                    if request.kind == CatalogKind::Listing {
                        store.manager.retry_after = Some(until);
                    }
                }
                return Err(format!("Catalog rate limit; retry after {until}"));
            }
            if !response.status().is_success() {
                return Err(format!("Catalog metadata HTTP {status}"));
            }
            let etag = response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .filter(|v| !v.is_empty() && v.len() <= 1024)
                .map(str::to_owned);
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 16384];
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".into());
                }
                let count = response
                    .read(&mut chunk)
                    .map_err(|_| "Catalog body interrupted")?;
                if count == 0 {
                    break;
                }
                if bytes.len() + count > BODY_LIMIT {
                    return Err("Catalog metadata exceeds limit".into());
                }
                bytes.extend_from_slice(&chunk[..count]);
                #[cfg(test)]
                {
                    self.catalog_http_stats.lock().unwrap().transferred += count as u64;
                }
            }
            #[cfg(test)]
            {
                self.catalog_http_stats.lock().unwrap().parses += 1;
            }
            let value = Arc::new(request.parse(&bytes)?);
            let store = self.store.lock().unwrap();
            let mut cache = self.catalog_http_cache.lock().unwrap();
            if store.catalog_generation != request.generation
                || !cache.current(&key, sequence)
                || cancel.load(Ordering::SeqCst)
            {
                return Err("Catalog context changed or cancelled".into());
            }
            // Replace stale validators even when this valid response has no validator.
            cache.entries.retain(|entry| entry.key != key);
            if let Some(etag) = etag {
                cache.insert(ConditionalEntry {
                    key,
                    etag,
                    value: value.clone(),
                    bytes: bytes.len(),
                });
            }
            return Ok(value);
        }
        Err("Conditional catalog retry exhausted".into())
    }
}
#[cfg(test)]
#[path = "catalog_transport_tests.rs"]
mod tests;
