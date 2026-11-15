//! Independent, metadata-only discovery and cancellable verified downloads
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub use stream_dock_manager_plugin_host::CAPABILITIES;
#[path = "plugin_catalog.rs"]
mod plugin_catalog;
#[path = "plugin_github.rs"]
mod plugin_github;
pub use plugin_catalog::*;
#[path = "download_jobs.rs"]
mod download_jobs;
#[path = "update_cache.rs"]
mod update_cache;
use download_jobs::DownloadRequest;
pub use update_cache::UpdateCacheLease;
// Read the clock used for updates
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default)]
pub struct UpdatePreferences {
    #[serde(default)]
    #[ts(optional)]
    pub plugins_catalog_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugins_catalog_url: Option<String>,
    pub automatic_checks: bool,
    pub manager_repository: String,
    pub plugins_repository: String,
    pub plugin_channel: String,
}
impl UpdatePreferences {
    // Preserve explicit source choices and older custom CDN URLs while defaulting new catalogs to GitHub
    pub(crate) fn catalog_source(&self) -> &str {
        self.plugins_catalog_source
            .as_deref()
            .filter(|source| !source.is_empty())
            .unwrap_or_else(|| {
                if self
                    .plugins_catalog_url
                    .as_deref()
                    .is_some_and(|url| !url.trim().is_empty())
                {
                    "hosted"
                } else {
                    "repository"
                }
            })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub sha256: String,
    #[ts(type = "number")]
    pub size: u64,
    pub os: String,
    pub arch: String,
    pub signing: String,
}
impl ReleaseAsset {
    // Report the supported installer condition for updates
    fn supported_installer(&self) -> bool {
        self.os != "windows"
            || Path::new(&self.name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PackageCandidate {
    pub files: BTreeMap<String, String>,
    pub os: String,
    pub id: String,
    pub version: String,
    pub path: String,
    pub sha256: String,
    #[ts(type = "number")]
    pub size: u64,
    pub architectures: Vec<String>,
    pub minimum_manager: String,
    pub required_capabilities: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ReleaseCatalog {
    pub schema_version: u32,
    pub product: String,
    pub repository: String,
    pub version: String,
    pub channel: String,
    pub build: String,
    pub commit: String,
    pub published_at: String,
    pub release_notes: String,
    pub release_tag: String,
    pub assets: Vec<ReleaseAsset>,
    #[serde(default)]
    pub packages: Vec<PackageCandidate>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManagerReleaseLink {
    Release,
    Source,
    Build,
}
/// Fixed project destinations. Caller-supplied URLs are never accepted
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLink {
    Website,
    Source,
    Issues,
    License,
}
impl ProjectLink {
    // Resolve an allowlisted Manager information link to its canonical HTTPS destination
    fn url(self) -> &'static str {
        match self {
            Self::Website => "https://streamdockmanager.app",
            Self::Source => "https://github.com/StreamDockManager/Manager",
            Self::Issues => "https://github.com/StreamDockManager/Manager/issues",
            Self::License => "https://github.com/StreamDockManager/Manager/blob/main/LICENSE",
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct CheckState {
    pub state: String,
    pub error: Option<String>,
    #[ts(type = "number | null")]
    pub last_success: Option<u64>,
    #[ts(type = "number | null")]
    pub retry_after: Option<u64>,
    pub catalog: Option<ReleaseCatalog>,
    pub notified_versions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct UpdateJob {
    pub id: String,
    pub product: String,
    pub operation: String,
    pub state: String,
    #[ts(type = "number")]
    pub completed: u64,
    #[ts(type = "number")]
    pub total: u64,
    pub result: Option<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retry_available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub asset_name: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginUpdate {
    pub id: String,
    pub installed_version: String,
    pub available_version: String,
    pub channel: String,
    pub available_build: String,
    pub compatible: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct UpdateStatus {
    #[serde(default)]
    #[ts(optional)]
    pub effective_plugins_catalog_url: Option<String>,
    pub available_catalogs: Vec<PluginCatalog>,
    pub host_os: String,
    pub host_arch: String,
    pub plugin_updates: Vec<PluginUpdate>,
    #[serde(default)]
    #[ts(optional)]
    pub plugin_policy_error: Option<String>,
    pub installed_manager: String,
    pub preferences: UpdatePreferences,
    pub manager: CheckState,
    pub plugins: CheckState,
    pub jobs: Vec<UpdateJob>,
}
#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    catalog_source_url: Option<String>,
    #[serde(default)]
    github_credential_repositories: std::collections::BTreeSet<String>,
    #[serde(default)]
    catalogs: BTreeMap<String, PluginCatalog>,
    #[serde(default)]
    preferences: UpdatePreferences,
    #[serde(default)]
    manager: CheckState,
    #[serde(default)]
    plugins: CheckState,
}

// Validate storage snapshot
pub(crate) fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let Some(store): Option<Store> = crate::profile_database::LocalPreference::Updates
        .snapshot_record()
        .read_from(connection)?
    else {
        return Ok(());
    };
    let preferences = store.preferences;
    if let Some(url) = preferences
        .plugins_catalog_url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        resolve_catalog_url(Some(url), None)?;
    }
    if !["", "hosted", "repository"].contains(
        &preferences
            .plugins_catalog_source
            .as_deref()
            .unwrap_or("hosted"),
    ) || !["", "stable", "development"].contains(&preferences.plugin_channel.as_str())
        || [
            &preferences.manager_repository,
            &preferences.plugins_repository,
        ]
        .into_iter()
        .any(|repository| !repository.is_empty() && !repository_valid(repository))
    {
        return Err("Invalid update preferences in recovery snapshot".into());
    }
    Ok(())
}
pub struct UpdateService {
    storage: stream_dock_manager_storage::Storage,
    github_credentials: crate::credentials::Credentials,
    selections: Mutex<BTreeMap<String, Arc<CatalogSelection>>>,
    downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
    cache_leases: Arc<Mutex<std::collections::HashMap<PathBuf, usize>>>,
    download_requests: Mutex<BTreeMap<String, DownloadRequest>>,
    user_cancelled: Mutex<std::collections::HashSet<String>>,
    root: PathBuf,
    store: Mutex<Store>,
    jobs: Mutex<BTreeMap<String, (UpdateJob, Arc<AtomicBool>)>>,
}
// Serialize JSON to a synchronized temporary file and atomically publish it at the destination
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut f = tempfile::NamedTempFile::new_in(path.parent().ok_or("No parent")?)
        .map_err(|e| e.to_string())?;
    f.write_all(&serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    f.as_file().sync_all().map_err(|e| e.to_string())?;
    f.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
// Report the repository valid condition for updates
pub fn repository_valid(value: &str) -> bool {
    let p: Vec<_> = value.split('/').collect();
    p.len() == 2
        && p.iter().all(|s| {
            !s.is_empty()
                && *s != "."
                && *s != ".."
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        })
}
// Report the safe name condition for updates
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 200
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        && name != "."
        && name != ".."
}
// Reject packages whose operating system, architecture, Manager version or capabilities are incompatible
pub fn compatible(p: &PackageCandidate) -> Result<(), String> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        std::env::consts::OS
    };
    if p.os != os {
        return Err("Package is for a different operating system".into());
    }
    if semver::Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?
        < semver::Version::parse(&p.minimum_manager).map_err(|e| e.to_string())?
    {
        return Err(format!("Requires Manager {}", p.minimum_manager));
    };
    if let Some(cap) = p
        .required_capabilities
        .iter()
        .find(|s| !CAPABILITIES.contains(&s.as_str()))
    {
        return Err(format!("Requires host capability {cap}"));
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    if !p.architectures.iter().any(|a| a == arch) {
        return Err("Unsupported architecture".into());
    };
    Ok(())
}
// Parse a stable semantic version without prerelease or build metadata
pub(crate) fn stable_version(value: &str) -> Result<semver::Version, String> {
    let version = semver::Version::parse(value).map_err(|e| e.to_string())?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err("Version must be stable semver".into());
    }
    Ok(version)
}
// Validate catalog
pub fn validate_catalog(
    c: &ReleaseCatalog,
    product: &str,
    repo: &str,
    tag: &str,
) -> Result<(), String> {
    if c.schema_version != 1
        || c.product != product
        || c.repository != repo
        || c.release_tag != tag
        || !repository_valid(repo)
    {
        return Err("Catalogue product/repository/tag mismatch".into());
    }
    if !matches!(c.commit.len(), 40 | 64)
        || !c.commit.bytes().all(|b| b.is_ascii_hexdigit())
        || chrono::DateTime::parse_from_rfc3339(&c.published_at).is_err()
    {
        return Err("Invalid release commit/publication time".into());
    };
    stable_version(&c.version)?;
    if (c.channel == "stable" && tag != format!("v{}", c.version))
        || (c.channel == "development" && !tag.starts_with(&format!("dev-{}-", c.version)))
        || !matches!(c.channel.as_str(), "stable" | "development")
    {
        return Err("Invalid release channel/tag".into());
    }
    if product == "manager" && c.channel != "stable" {
        return Err("Manager supports stable releases only".into());
    }
    if c.assets.is_empty() || c.assets.len() > 32 || c.packages.len() > 256 || c.build.is_empty() {
        return Err("Incomplete catalogue".into());
    }
    let mut names = std::collections::HashSet::new();
    for a in &c.assets {
        if !names.insert(&a.name)
            || !safe_name(&a.name)
            || !valid_hash(&a.sha256)
            || a.size == 0
            || a.size > 2 * 1024 * 1024 * 1024
        {
            return Err("Invalid catalogue asset".into());
        };
        validate_origin(&a.url, repo, tag, false)?;
        let u = Url::parse(&a.url).map_err(|e| e.to_string())?;
        if u.path_segments().and_then(|mut s| s.next_back()) != Some(a.name.as_str()) {
            return Err("Asset name/URL mismatch".into());
        }
    }
    if product != "manager" || !c.packages.is_empty() {
        return Err("Application release catalog must contain only installers".into());
    }
    Ok(())
}
// Report the valid hash condition for updates
pub fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
// Validate origin
fn validate_origin(value: &str, repo: &str, tag: &str, redirect: bool) -> Result<(), String> {
    let u = Url::parse(value).map_err(|e| e.to_string())?;
    if u.scheme() != "https"
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err("Invalid release origin".into());
    };
    let host = u.host_str().unwrap_or("");
    if redirect
        && matches!(
            host,
            "release-assets.githubusercontent.com" | "objects.githubusercontent.com"
        )
    {
        return Ok(());
    };
    let prefix = format!("/{repo}/releases/download/{tag}/");
    if host == "github.com" && u.path().starts_with(&prefix) {
        return Ok(());
    };
    Err("Unexpected release asset origin".into())
}
// Create a bounded release-checking HTTP client that does not follow redirects automatically
fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent("StreamDockManager/0.1 release-checker")
        .timeout(Duration::from_secs(20))
        .redirect(Policy::none())
        .build()
        .map_err(|e| e.to_string())
}
// Fetch asset conditional
fn fetch_asset_conditional(
    client: &Client,
    url: &str,
    repo: &str,
    tag: &str,
    cancel: &AtomicBool,
    etag: Option<&str>,
) -> Result<reqwest::blocking::Response, String> {
    let mut current = url.to_owned();
    for _ in 0..6 {
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".into());
        };
        validate_origin(&current, repo, tag, true)?;
        let mut request = client.get(&current);
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag)
        };
        let r = request.send().map_err(|e| e.to_string())?;
        if r.status() == 304 {
            return Ok(r);
        };
        if r.status().is_redirection() {
            let location = r
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or("Missing asset redirect")?
                .to_str()
                .map_err(|e| e.to_string())?;
            current = Url::parse(&current)
                .map_err(|e| e.to_string())?
                .join(location)
                .map_err(|e| e.to_string())?
                .to_string();
            continue;
        };
        return r.error_for_status().map_err(|e| e.to_string());
    }
    Err("Too many asset redirects".into())
}
impl UpdateService {
    // Open the requested updates resource
    pub fn open(root: PathBuf) -> Result<Arc<Self>, String> {
        let downloads = Arc::new(stream_dock_manager_downloads::DownloadCache::open(
            root.join("partials"),
        )?);
        Self::open_with_downloads(root, downloads)
    }
    // Open with downloads
    pub fn open_with_downloads(
        root: PathBuf,
        downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
    ) -> Result<Arc<Self>, String> {
        Self::open_with_credential_provider(
            root,
            downloads,
            Arc::new(crate::credentials::OsCredentials),
        )
    }
    // Open with credential provider
    pub(crate) fn open_with_credential_provider(
        root: PathBuf,
        downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
        provider: Arc<dyn crate::credentials::CredentialProvider>,
    ) -> Result<Arc<Self>, String> {
        let config_root = if root.file_name().is_some_and(|name| name == "updates") {
            root.parent()
                .ok_or("Missing configuration root")?
                .to_path_buf()
        } else {
            root.clone()
        };
        let storage = stream_dock_manager_storage::Storage::open_validated(
            config_root,
            crate::application_data::validate_snapshot,
        )?;
        Self::open_with_storage(root, downloads, provider, storage)
    }
    // Open with storage
    pub(crate) fn open_with_storage(
        root: PathBuf,
        downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
        provider: Arc<dyn crate::credentials::CredentialProvider>,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Arc<Self>, String> {
        std::fs::create_dir_all(root.join("cache")).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(root.join("downloads")).map_err(|e| e.to_string())?;
        let mut store: Store = crate::profile_database::LocalPreference::Updates
            .record()
            .read(&storage)?
            .unwrap_or_default();
        if store.preferences.manager_repository.is_empty() {
            store.preferences.manager_repository =
                option_env!("STREAM_DOCK_MANAGER_RELEASE_REPOSITORY")
                    .unwrap_or("StreamDockManager/Manager")
                    .into()
        };
        if store.preferences.plugins_repository.is_empty() {
            store.preferences.plugins_repository =
                option_env!("STREAM_DOCK_MANAGER_PLUGINS_RELEASE_REPOSITORY")
                    .unwrap_or("StreamDockManager/Plugins")
                    .into()
        };
        if store
            .preferences
            .plugins_catalog_source
            .as_deref()
            .unwrap_or("")
            .is_empty()
        {
            store.preferences.plugins_catalog_source =
                Some(store.preferences.catalog_source().into());
        }
        if store.preferences.plugin_channel.is_empty() {
            store.preferences.plugin_channel = "stable".into()
        };
        for state in [&mut store.manager, &mut store.plugins] {
            if state.state.is_empty() || state.state == "checking" {
                state.state = "unavailable".into()
            }
        }
        store.plugins.catalog = None;
        let repository = store.preferences.plugins_repository.clone();
        let source = plugin_catalog::effective_catalog_url(&store.preferences).ok();
        if store.catalog_source_url != source || source.is_none() {
            store.catalogs.clear();
            store.plugins = CheckState {
                notified_versions: store.plugins.notified_versions.clone(),
                state: "unavailable".into(),
                ..Default::default()
            };
        }
        store.catalog_source_url = source.clone();
        store.catalogs.retain(|channel, catalog| {
            catalog
                .validate_source(&repository, channel, source.as_deref())
                .is_ok()
        });
        Ok(Arc::new(Self {
            storage,
            github_credentials: crate::credentials::Credentials::with_provider(provider),
            root,
            store: Mutex::new(store),
            jobs: Default::default(),
            selections: Default::default(),
            downloads,
            cache_leases: Default::default(),
            download_requests: Default::default(),
            user_cancelled: Default::default(),
        }))
    }
    // Persist the updates value
    fn persist(&self) -> Result<(), String> {
        crate::profile_database::LocalPreference::Updates
            .record::<Store>()
            .write(&self.storage, &*self.store.lock().unwrap())
    }
    // Return the current updates status
    pub fn status(&self) -> UpdateStatus {
        let s = self.store.lock().unwrap();
        let mut manager = s.manager.clone();
        if let Some(catalog) = &mut manager.catalog {
            catalog.assets.retain(ReleaseAsset::supported_installer);
        }
        UpdateStatus {
            effective_plugins_catalog_url: plugin_catalog::effective_catalog_url(&s.preferences)
                .ok(),
            available_catalogs: s.catalogs.values().cloned().collect(),
            host_os: if cfg!(target_os = "macos") {
                "macos"
            } else {
                std::env::consts::OS
            }
            .into(),
            host_arch: if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "amd64"
            }
            .into(),
            plugin_updates: vec![],
            plugin_policy_error: None,
            installed_manager: env!("CARGO_PKG_VERSION").into(),
            preferences: s.preferences.clone(),
            manager,
            plugins: s.plugins.clone(),
            jobs: self
                .jobs
                .lock()
                .unwrap()
                .values()
                .map(|(j, _)| j.clone())
                .collect(),
        }
    }
    // Validate update sources and repository settings before persisting the new preferences
    pub fn preferences(&self, p: UpdatePreferences) -> Result<(), String> {
        if let Some(url) = p
            .plugins_catalog_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
        {
            resolve_catalog_url(Some(url), None)?;
        }
        if !["", "hosted", "repository"].contains(&p.catalog_source()) {
            return Err("Choose hosted catalog or GitHub repository".into());
        }
        if p.catalog_source() == "repository" && !repository_valid(&p.plugins_repository) {
            return Err("Use a GitHub owner/repository identity".into());
        }
        for r in [&p.manager_repository, &p.plugins_repository] {
            if !r.is_empty() && !repository_valid(r) {
                return Err("Use a GitHub owner/repository identity".into());
            }
        }
        if !matches!(p.plugin_channel.as_str(), "stable" | "development") {
            return Err("Invalid plugin channel".into());
        };
        {
            let mut s = self.store.lock().unwrap();
            if p.manager_repository != s.preferences.manager_repository {
                s.manager = CheckState {
                    state: "unavailable".into(),
                    ..Default::default()
                }
            };
            if p.plugins_repository != s.preferences.plugins_repository
                || p.plugins_catalog_url != s.preferences.plugins_catalog_url
                || p.plugins_catalog_source != s.preferences.plugins_catalog_source
                || p.plugin_channel != s.preferences.plugin_channel
            {
                s.catalogs.clear();
                s.catalog_source_url = plugin_catalog::effective_catalog_url(&p).ok();
                s.plugins = CheckState {
                    notified_versions: s.plugins.notified_versions.clone(),
                    state: "unavailable".into(),
                    ..Default::default()
                }
            };
            s.preferences = p
        };
        self.persist()
    }
    // Register a bounded cancellable update job while rejecting duplicate running operations
    fn job(&self, product: &str, operation: &str) -> Result<(String, Arc<AtomicBool>), String> {
        if !matches!(product, "manager" | "plugins") {
            return Err("Invalid product".into());
        };
        let mut jobs = self.jobs.lock().unwrap();
        if jobs
            .values()
            .any(|(j, _)| j.product == product && j.operation == operation && j.state == "running")
        {
            return Err("Operation already running".into());
        };
        while jobs.len() >= 64 {
            let key = jobs
                .iter()
                .find(|(_, (j, _))| j.state != "running")
                .map(|(k, _)| k.clone())
                .ok_or("Too many active jobs")?;
            jobs.remove(&key);
            self.selections.lock().unwrap().remove(&key);
            self.download_requests.lock().unwrap().remove(&key);
            self.user_cancelled.lock().unwrap().remove(&key);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        jobs.insert(
            id.clone(),
            (
                UpdateJob {
                    id: id.clone(),
                    product: product.into(),
                    operation: operation.into(),
                    state: "running".into(),
                    completed: 0,
                    total: 0,
                    result: None,
                    error: None,
                    retry_available: None,
                    asset_name: None,
                },
                cancel.clone(),
            ),
        );
        Ok((id, cancel))
    }
    // Cancel the pending updates operation
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let jobs = self.jobs.lock().unwrap();
        let (_, c) = jobs.get(id).ok_or("Unknown job")?;
        self.user_cancelled.lock().unwrap().insert(id.into());
        c.store(true, Ordering::SeqCst);
        Ok(())
    }
    // Cancel all pending updates operations
    pub fn cancel_all(&self) {
        for (_, c) in self.jobs.lock().unwrap().values() {
            c.store(true, Ordering::SeqCst)
        }
    }
    // Finalize the pending updates operation
    fn finish(&self, id: &str, result: Result<Option<String>, String>) {
        if let Some((j, c)) = self.jobs.lock().unwrap().get_mut(id) {
            match result {
                Ok(path) => {
                    j.state = "complete".into();
                    j.result = path
                }
                Err(e) => {
                    j.state = if c.load(Ordering::SeqCst) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    j.error = Some(e)
                }
            }
        }
    }
    // Check the current updates conditions
    pub fn check(self: &Arc<Self>, product: &str) -> Result<String, String> {
        let (id, cancel) = self.job(product, "check")?;
        let service = self.clone();
        let product = product.to_owned();
        let output = id.clone();
        std::thread::spawn(move || {
            let result = service.check_inner(&product, &cancel);
            service.finish(&id, result.map(|_| None));
        });
        Ok(output)
    }
    // Check inner
    fn check_inner(&self, product: &str, cancel: &AtomicBool) -> Result<(), String> {
        if product == "plugins" {
            let result = self.check_plugin_catalogs(cancel);
            if let Err(error) = &result {
                let mut s = self.store.lock().unwrap();
                s.plugins.state = "failed".into();
                s.plugins.error = Some(error.clone());
            }
            self.persist()?;
            return result;
        }
        let repo = {
            let mut s = self.store.lock().unwrap();
            if s.manager.retry_after.is_some_and(|until| until > now()) {
                return Err("Release checks are rate limited; try later".into());
            }
            s.manager.state = "checking".into();
            s.manager.error = None;
            s.preferences.manager_repository.clone()
        };
        let result = if !repository_valid(&repo) {
            Err("Configure the application release repository first".into())
        } else {
            self.discover("manager", &repo, "stable", cancel)
        };
        {
            let mut s = self.store.lock().unwrap();
            if s.preferences.manager_repository != repo {
                return Err("Release source changed during check; check again".into());
            }
            let state = &mut s.manager;
            match &result {
                Ok(c) => {
                    state.last_success = Some(now());
                    state.retry_after = None;
                    state.catalog = c.clone();
                    state.state = match c {
                        None => "unavailable",
                        Some(c)
                            if semver::Version::parse(&c.version).is_ok_and(|v| {
                                v <= semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
                            }) =>
                        {
                            "current"
                        }
                        Some(_) => "updates_available",
                    }
                    .into();
                }
                Err(e) => {
                    state.state = "failed".into();
                    state.retry_after.get_or_insert(now() + 900);
                    state.error = Some(e.clone());
                }
            }
        }
        self.persist()?;
        result.map(|_| ())
    }
    // Fetch GitHub API metadata through the shared cache without an authorization token
    fn api(
        &self,
        client: &Client,
        url: &str,
        cancel: &AtomicBool,
    ) -> Result<serde_json::Value, String> {
        self.api_with_token(client, url, cancel, None)
    }
    // Fetch cancellable GitHub metadata with optional authentication and leased conditional cache reuse
    fn api_with_token(
        &self,
        client: &Client,
        url: &str,
        cancel: &AtomicBool,
        token: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".into());
        };
        let key = hex::encode(Sha256::digest(url.as_bytes()));
        let file = self.root.join("cache").join(format!("{key}.json"));
        let _lease = self.cache_lease(&file)?;
        let cache: serde_json::Value = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut req = client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        if let Some(tag) = cache["etag"].as_str() {
            req = req.header("If-None-Match", tag)
        };
        let mut r = req.send().map_err(|e| e.to_string())?;
        if r.status() == 304 {
            return cache
                .get("data")
                .cloned()
                .ok_or("Conditional cache missing".into());
        };
        if matches!(r.status().as_u16(), 403 | 429) {
            let until = r
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| {
                    r.headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|s| now() + s)
                })
                .unwrap_or(now() + 60);
            let mut store = self.store.lock().unwrap();
            store.manager.retry_after = Some(until);
            store.plugins.retry_after = Some(until);
            return Err(format!("GitHub rate limit; retry after {until}"));
        };
        if !r.status().is_success() {
            return Err(format!("GitHub metadata HTTP {}", r.status()));
        };
        let etag = r
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut bytes = Vec::new();
        r.by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 4 * 1024 * 1024 {
            return Err("Metadata exceeds limit".into());
        };
        let data: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        atomic_json(&file, &serde_json::json!({"etag":etag,"data":data}))?;
        Ok(data)
    }
    // Discover available updates candidates
    fn discover(
        &self,
        product: &str,
        repo: &str,
        channel: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<ReleaseCatalog>, String> {
        let client = client()?;
        let mut candidates = vec![];
        for page in 1..=10 {
            let releases = self.api(
                &client,
                &format!("https://api.github.com/repos/{repo}/releases?per_page=100&page={page}"),
                cancel,
            )?;
            let releases = releases.as_array().ok_or("Invalid releases response")?;
            for release in releases {
                if release["draft"] == true
                    || release["prerelease"].as_bool().unwrap_or(false)
                        != (channel == "development")
                {
                    continue;
                };
                let Some(tag) = release["tag_name"].as_str() else {
                    continue;
                };
                if channel == "stable" && !tag.starts_with('v')
                    || channel == "development" && !tag.starts_with("dev-")
                {
                    continue;
                };
                if !safe_name(tag) {
                    continue;
                };
                let Some(assets) = release["assets"].as_array() else {
                    continue;
                };
                let Some(catalog) = assets.iter().find(|a| a["name"] == "catalog.json") else {
                    continue;
                };
                let Some(sums) = assets.iter().find(|a| a["name"] == "SHA256SUMS") else {
                    continue;
                };
                let load = |asset: &serde_json::Value| -> Result<Vec<u8>, String> {
                    let url = asset["browser_download_url"]
                        .as_str()
                        .ok_or("Missing release URL")?;
                    validate_origin(url, repo, tag, false)?;
                    self.metadata_asset(&client, url, product, repo, tag, channel, cancel)
                };
                // Incomplete/unrelated catalogues are skipped, while network failures remain visible
                let bytes = load(catalog)?;
                let c: ReleaseCatalog = match serde_json::from_slice(&bytes) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if validate_catalog(&c, product, repo, tag).is_err() || c.channel != channel {
                    continue;
                };
                let sums = String::from_utf8(load(sums)?).map_err(|e| e.to_string())?;
                if c.assets.iter().any(|a| {
                    !assets
                        .iter()
                        .any(|r| r["name"] == a.name && r["size"].as_u64() == Some(a.size))
                        || !sums
                            .lines()
                            .any(|l| l == format!("{}  {}", a.sha256, a.name))
                }) {
                    continue;
                };
                candidates.push(c)
            }
            if releases.len() < 100 {
                break;
            }
        }
        candidates.sort_by(|a, b| {
            let va = semver::Version::parse(&a.version).unwrap();
            let vb = semver::Version::parse(&b.version).unwrap();
            vb.cmp(&va)
                .then_with(|| b.published_at.cmp(&a.published_at))
                .then_with(|| b.build.cmp(&a.build))
        });
        Ok(candidates.into_iter().next())
    }
    // Download the selected updates artifact
    pub fn download(
        self: &Arc<Self>,
        product: &str,
        name: &str,
        channel: Option<&str>,
    ) -> Result<String, String> {
        if product != "manager" || channel.is_some_and(|c| c != "stable") {
            return Err("Use individual plugin catalog downloads for plugins".into());
        }
        let catalog = {
            let s = self.store.lock().unwrap();
            let c = s
                .manager
                .catalog
                .clone()
                .ok_or("Check application releases first")?;
            if c.repository != s.preferences.manager_repository {
                return Err("Catalog source changed; check releases again".into());
            }
            c
        };
        let asset = catalog
            .assets
            .iter()
            .find(|a| a.name == name)
            .cloned()
            .ok_or("Asset not in verified catalogue")?;
        let os = if cfg!(target_os = "macos") {
            "macos"
        } else {
            std::env::consts::OS
        };
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "amd64"
        };
        if asset.os != os || !matches!(asset.arch.as_str(), "universal") && asset.arch != arch {
            return Err("Installer does not support this host".into());
        };
        self.launch_download(DownloadRequest::manager(&catalog, &asset)?)
    }
    // Resolve the regular cached file belonging to a completed download job
    pub fn completed_download(&self, id: &str) -> Result<PathBuf, String> {
        let jobs = self.jobs.lock().unwrap();
        let (j, _) = jobs.get(id).ok_or("Unknown download")?;
        if j.operation != "download" || j.state != "complete" {
            return Err("Download not complete".into());
        };
        let path = PathBuf::from(j.result.as_ref().ok_or("Missing download path")?);
        if !path.starts_with(self.root.join("downloads")) {
            return Err("Invalid download destination".into());
        };
        if !path.is_file() {
            return Err("Cached download expired or was cleared; download again".into());
        }
        Ok(path)
    }
    // Resolve the Manager release endpoint
    fn manager_release_url(&self, kind: ManagerReleaseLink) -> Result<String, String> {
        let store = self.store.lock().unwrap();
        let catalog = store
            .manager
            .catalog
            .as_ref()
            .ok_or("Check application releases first")?;
        // Revalidate persisted metadata at the action boundary. The caller can
        // select a known destination, but cannot supply a URL or filesystem path
        validate_catalog(
            catalog,
            "manager",
            &store.preferences.manager_repository,
            &catalog.release_tag,
        )?;
        if catalog
            .build
            .parse::<u64>()
            .ok()
            .filter(|id| *id > 0 && id.to_string() == catalog.build)
            .is_none()
        {
            return Err("Invalid application release build run".into());
        }
        let path = match kind {
            ManagerReleaseLink::Release => format!("releases/tag/{}", catalog.release_tag),
            ManagerReleaseLink::Source => format!("commit/{}", catalog.commit),
            ManagerReleaseLink::Build => format!("actions/runs/{}", catalog.build),
        };
        Ok(format!("https://github.com/{}/{path}", catalog.repository))
    }
    // Start daily
    pub fn start_daily(self: &Arc<Self>) {
        let service = Arc::downgrade(self);
        std::thread::spawn(move || {
            while let Some(service) = service.upgrade() {
                let status = service.status();
                if status.preferences.automatic_checks {
                    for (product, state) in
                        [("manager", status.manager), ("plugins", status.plugins)]
                    {
                        if state
                            .last_success
                            .is_none_or(|t| now().saturating_sub(t) >= 86400)
                            && state.retry_after.is_none_or(|t| t <= now())
                            && state.state != "checking"
                        {
                            let _ = service.check(product);
                        }
                    }
                };
                drop(service);
                std::thread::sleep(Duration::from_secs(30));
            }
        });
    }
}
// Verify file
pub fn verify_file(path: &Path, expected: &str, size: u64) -> Result<(), String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if f.metadata().map_err(|e| e.to_string())?.len() != size {
        return Err("File size mismatch".into());
    };
    let mut h = Sha256::new();
    let mut f = f;
    let mut b = [0u8; 65536];
    loop {
        let n = f.read(&mut b).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        };
        h.update(&b[..n])
    }
    if hex::encode(h.finalize()) != expected {
        return Err("File checksum mismatch".into());
    };
    Ok(())
}

impl crate::Runtime {
    // Update status
    pub fn update_status(&self) -> UpdateStatus {
        let mut status = self.updates.status();
        let policies = match self.stored_plugin_policies() {
            Ok(policies) => policies,
            Err(error) => {
                status.plugin_policy_error = Some(error);
                vec![]
            }
        };
        for catalog in &status.available_catalogs {
            let host = self.plugins.lock().unwrap();
            for installed in host.list() {
                let Some(provenance) = host.provenance(&installed.id) else {
                    continue;
                };
                if provenance["repository"] != catalog.repository
                    || provenance["channel"] != catalog.channel
                {
                    continue;
                }
                let Some(plugin) = catalog.plugins.iter().find(|p| p.id == installed.id) else {
                    continue;
                };
                if stable_version(&plugin.version).ok() <= stable_version(&installed.version).ok() {
                    continue;
                }
                let Some(artifact) = plugin.downloads.iter().find(|a| a.os == status.host_os)
                else {
                    continue;
                };
                let selection = CatalogSelection {
                    catalog_url: status.effective_plugins_catalog_url.clone(),
                    github_asset_url: None,
                    repository: catalog.repository.clone(),
                    channel: catalog.channel.clone(),
                    plugin: plugin.clone(),
                    artifact: artifact.clone(),
                };
                if let Some(token) = host.installation_token(&installed.id) {
                    if policies.iter().any(|policy| {
                        policy.suppresses(
                            &installed.id,
                            &catalog.repository,
                            &catalog.channel,
                            &token,
                            &plugin.version,
                        )
                    }) {
                        continue;
                    }
                }
                let reason = compatible(&selection.package()).err();
                status.plugin_updates.push(PluginUpdate {
                    id: installed.id,
                    installed_version: installed.version,
                    available_version: plugin.version.clone(),
                    channel: catalog.channel.clone(),
                    available_build: artifact.release_tag.clone(),
                    compatible: reason.is_none(),
                    reason,
                });
            }
        }
        if status.plugins.last_success.is_some() && !status.plugin_updates.is_empty() {
            status.plugins.state = "updates_available".into();
        }
        if status.plugins.state == "updates_available" && status.plugin_updates.is_empty() {
            status.plugins.state = "current".into()
        };
        status
    }
}

impl UpdateService {
    // Record previously unseen release versions and produce their notification messages
    pub fn notifications(&self, status: &UpdateStatus) -> Vec<String> {
        let mut messages = vec![];
        {
            let mut s = self.store.lock().unwrap();
            if status.manager.state == "updates_available" {
                if let Some(c) = &status.manager.catalog {
                    let key = format!("{}/{}", c.repository, c.version);
                    if !s.manager.notified_versions.contains(&key) {
                        s.manager.notified_versions.push(key);
                        messages.push(format!("Manager {} is available", c.version));
                    }
                }
            };
            for p in &status.plugin_updates {
                let version_key = format!(
                    "{}/{}/{}/{}",
                    status.preferences.plugins_repository, p.channel, p.id, p.available_version
                );
                let key = if p.channel == "stable" {
                    version_key.clone()
                } else {
                    format!("{version_key}/{}", p.available_build)
                };
                // Stable plugin versions can recur in later catalog publications. Also
                // honor keys written before notifications stopped including their build
                let already_notified = s.plugins.notified_versions.iter().any(|previous| {
                    previous == &key
                        || p.channel == "stable" && previous.starts_with(&format!("{version_key}/"))
                });
                if !already_notified {
                    s.plugins.notified_versions.push(key);
                    messages.push(format!("{} {} is available", p.id, p.available_version));
                }
            }
            if s.manager.notified_versions.len() > 1024 {
                s.manager.notified_versions.remove(0);
            }
            if s.plugins.notified_versions.len() > 1024 {
                s.plugins.notified_versions.remove(0);
            }
        };
        if !messages.is_empty() {
            let _ = self.persist();
        };
        messages
    }
}
impl crate::Runtime {
    // Open project link
    pub fn open_project_link(&self, kind: ProjectLink) -> Result<(), String> {
        crate::actions::open(kind.url())
    }
    // Open manager release link
    pub fn open_manager_release_link(&self, kind: ManagerReleaseLink) -> Result<(), String> {
        crate::actions::open(&self.updates.manager_release_url(kind)?)
    }
    // Open the containing directory of a validated completed update download
    pub fn show_update_download(&self, id: &str) -> Result<(), String> {
        let path = self.updates.completed_download(id)?;
        crate::actions::open(
            path.parent()
                .ok_or("Missing download directory")?
                .to_str()
                .ok_or("Invalid download path")?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that project links are fixed and reject caller URLs
    fn project_links_are_fixed_and_reject_caller_urls() {
        for (kind, expected) in [
            ("website", "https://streamdockmanager.app"),
            ("source", "https://github.com/StreamDockManager/Manager"),
            (
                "issues",
                "https://github.com/StreamDockManager/Manager/issues",
            ),
            (
                "license",
                "https://github.com/StreamDockManager/Manager/blob/main/LICENSE",
            ),
        ] {
            let link: ProjectLink = serde_json::from_value(serde_json::json!(kind)).unwrap();
            assert_eq!(link.url(), expected);
        }
        for value in [
            "",
            "Website",
            "release",
            "https://example.com",
            "file:///tmp/test",
            "javascript:alert(1)",
        ] {
            assert!(serde_json::from_value::<ProjectLink>(serde_json::json!(value)).is_err());
        }
        assert!(serde_json::from_value::<ProjectLink>(
            serde_json::json!({"source": "https://example.com"})
        )
        .is_err());
    }
    // Construct a deterministic Manager release catalog for update tests
    fn catalog() -> ReleaseCatalog {
        ReleaseCatalog {
            schema_version: 1,
            product: "manager".into(),
            repository: "example/manager".into(),
            version: "0.2.0".into(),
            channel: "stable".into(),
            build: "run-1".into(),
            commit: "a".repeat(40),
            published_at: "2026-10-22T00:00:00Z".into(),
            release_notes: "Release".into(),
            release_tag: "v0.2.0".into(),
            assets: vec![ReleaseAsset {
                name: "manager.dmg".into(),
                url: "https://github.com/example/manager/releases/download/v0.2.0/manager.dmg"
                    .into(),
                sha256: "a".repeat(64),
                size: 10,
                os: "macos".into(),
                arch: "arm64".into(),
                signing: "unsigned".into(),
            }],
            packages: vec![],
        }
    }
    #[test]
    // Verify that product repository tag and archive origins are independent
    fn product_repository_tag_and_archive_origins_are_independent() {
        let c = catalog();
        assert!(validate_catalog(&c, "manager", "example/manager", "v0.2.0").is_ok());
        assert!(validate_catalog(&c, "plugins", "example/manager", "v0.2.0").is_err());
        assert!(validate_catalog(&c, "manager", "other/manager", "v0.2.0").is_err());
        assert!(validate_catalog(&c, "manager", "example/manager", "v0.3.0").is_err());
        for url in [
            "http://github.com/example/manager/releases/download/v0.2.0/manager.dmg",
            "https://evil.invalid/example/manager/releases/download/v0.2.0/manager.dmg",
            "https://github.com/example/plugins/releases/download/v0.2.0/manager.dmg",
            "https://github.com/example/manager/releases/download/v0.2.0/../manager.dmg",
        ] {
            let mut bad = c.clone();
            bad.assets[0].url = url.into();
            assert!(
                validate_catalog(&bad, "manager", "example/manager", "v0.2.0").is_err(),
                "{url}"
            )
        }
        assert!(validate_origin(
            "https://release-assets.githubusercontent.com/github-production-release-asset/test",
            "example/manager",
            "v0.2.0",
            true
        )
        .is_ok());
        assert!(validate_origin(
            "https://release-assets.githubusercontent.com/test",
            "example/manager",
            "v0.2.0",
            false
        )
        .is_err());
    }
    #[test]
    // Verify that legacy Windows catalog exposes and downloads only exe installers
    fn legacy_windows_catalog_exposes_and_downloads_only_exe_installers() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        let mut current = catalog();
        current.assets.clear();
        for extension in ["exe", "msi"] {
            let mut asset = catalog().assets.remove(0);
            asset.os = "windows".into();
            asset.arch = "amd64".into();
            asset.name = format!("manager.{extension}");
            asset.url = format!(
                "https://github.com/example/manager/releases/download/v0.2.0/{}",
                asset.name
            );
            current.assets.push(asset);
        }
        service.store.lock().unwrap().manager.catalog = Some(current.clone());
        let exposed = service.status().manager.catalog.unwrap();
        assert_eq!(exposed.assets.len(), 1);
        assert_eq!(exposed.assets[0].name, "manager.exe");
        assert!(DownloadRequest::manager(&current, &current.assets[0]).is_ok());
        assert!(DownloadRequest::manager(&current, &current.assets[1]).is_err());
    }

    #[test]
    // Verify that manager release links use only the current validated catalog
    fn manager_release_links_use_only_the_current_validated_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        assert!(service
            .manager_release_url(ManagerReleaseLink::Release)
            .is_err());
        let mut current = catalog();
        current.build = "123456".into();
        {
            let mut store = service.store.lock().unwrap();
            store.preferences.manager_repository = current.repository.clone();
            store.manager.catalog = Some(current.clone());
        }
        for (kind, path) in [
            (ManagerReleaseLink::Release, "releases/tag/v0.2.0".into()),
            (
                ManagerReleaseLink::Source,
                format!("commit/{}", current.commit),
            ),
            (ManagerReleaseLink::Build, "actions/runs/123456".into()),
        ] {
            assert_eq!(
                service.manager_release_url(kind).unwrap(),
                format!("https://github.com/example/manager/{path}")
            );
        }
        service.store.lock().unwrap().preferences.manager_repository = "other/manager".into();
        assert!(service
            .manager_release_url(ManagerReleaseLink::Source)
            .is_err());
        for value in [
            "https://evil.invalid",
            "file:///tmp/test",
            "../source",
            "unknown",
        ] {
            assert!(
                serde_json::from_value::<ManagerReleaseLink>(serde_json::json!(value)).is_err()
            );
        }
    }
    #[test]
    // Verify that manager release links reject malformed persisted metadata
    fn manager_release_links_reject_malformed_persisted_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        let mut current = catalog();
        current.build = "123456".into();
        let original = serde_json::to_value(&current).unwrap();
        for (field, value) in [
            ("repository", "example/../evil"),
            ("repository", "example/manager?redirect=evil"),
            ("product", "plugins"),
            ("commit", "../../evil"),
            ("commit", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaag"),
            ("release_tag", "v0.2.0/../../evil"),
            ("build", "1?redirect=evil"),
            ("build", "https://evil.invalid"),
            ("build", ""),
            ("build", "0"),
            ("build", "01"),
            ("build", "18446744073709551616"),
        ] {
            let mut malformed = original.clone();
            malformed[field] = value.into();
            {
                let mut store = service.store.lock().unwrap();
                store.preferences.manager_repository = current.repository.clone();
                store.manager.catalog = Some(serde_json::from_value(malformed).unwrap());
            }
            service.persist().unwrap();
            let restored = UpdateService::open_with_storage(
                dir.path().into(),
                service.downloads.clone(),
                Arc::new(crate::credentials::OsCredentials),
                service.storage.clone(),
            )
            .unwrap();
            for kind in [
                ManagerReleaseLink::Release,
                ManagerReleaseLink::Source,
                ManagerReleaseLink::Build,
            ] {
                assert!(
                    restored.manager_release_url(kind).is_err(),
                    "{field}={value}"
                );
            }
        }
    }
    #[test]
    // Verify that notifications follow plugin versions instead of stable release builds
    fn notifications_follow_plugin_versions_instead_of_stable_release_builds() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        let mut status = service.status();
        status.plugin_updates.push(PluginUpdate {
            id: "org.test.counter".into(),
            installed_version: "1.2.2".into(),
            available_version: "1.2.3".into(),
            channel: "stable".into(),
            available_build: "release-9.0.0".into(),
            compatible: true,
            reason: None,
        });
        assert_eq!(service.notifications(&status).len(), 1);
        status.plugin_updates[0].available_build = "release-9.1.0".into();
        assert!(service.notifications(&status).is_empty());
        status.plugin_updates[0].available_version = "1.2.4".into();
        assert_eq!(service.notifications(&status).len(), 1);
        status.plugin_updates[0].channel = "development".into();
        assert_eq!(service.notifications(&status).len(), 1);
        status.plugin_updates[0].available_build = "another-development-build".into();
        assert_eq!(service.notifications(&status).len(), 1);
    }
    #[test]
    // Verify that checksums and host compatibility are separate from plugin version
    fn checksums_and_host_compatibility_are_separate_from_plugin_version() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("file");
        std::fs::write(&p, b"good").unwrap();
        let sha = hex::encode(Sha256::digest(b"good"));
        assert!(verify_file(&p, &sha, 4).is_ok());
        std::fs::write(&p, b"evil").unwrap();
        assert!(verify_file(&p, &sha, 4).is_err());
        let mut package = PackageCandidate {
            files: BTreeMap::new(),
            os: if cfg!(target_os = "macos") {
                "macos"
            } else {
                std::env::consts::OS
            }
            .into(),
            id: "org.test.plugin".into(),
            version: "9.0.0".into(),
            path: "packages/org.test.plugin.zip".into(),
            sha256: sha,
            size: 4,
            architectures: vec![if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "amd64"
            }
            .into()],
            minimum_manager: "0.1.0".into(),
            required_capabilities: vec!["plugin-protocol-v1".into()],
        };
        assert!(compatible(&package).is_ok());
        package
            .required_capabilities
            .push("future-capability".into());
        assert!(compatible(&package).is_err());
        package.required_capabilities.clear();
        package.minimum_manager = "99.0.0".into();
        assert!(compatible(&package).is_err());
    }
    #[test]
    // Verify that preferences default to manual and survive restart
    fn preferences_default_to_manual_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        assert!(!service.status().preferences.automatic_checks);
        assert_eq!(service.status().preferences.catalog_source(), "repository");
        let p = UpdatePreferences {
            plugins_catalog_source: Some("hosted".into()),
            plugins_catalog_url: None,
            automatic_checks: true,
            manager_repository: "example/manager".into(),
            plugins_repository: "example/plugins".into(),
            plugin_channel: "development".into(),
        };
        service.preferences(p).unwrap();
        drop(service);
        let service = UpdateService::open(dir.path().into()).unwrap();
        assert_eq!(service.status().preferences.plugin_channel, "development");
        assert_eq!(service.status().preferences.catalog_source(), "hosted");
        assert_eq!(
            service.status().preferences.manager_repository,
            "example/manager"
        );
        assert_eq!(
            service.status().preferences.plugins_repository,
            "example/plugins"
        );
        assert!(service.status().preferences.automatic_checks);
        assert_eq!(
            service.status().installed_manager,
            env!("CARGO_PKG_VERSION")
        );
    }
    #[test]
    // Preserve a pre-selector custom CDN URL when reopening saved update preferences
    fn older_custom_catalog_url_keeps_cdn_source() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        {
            let mut store = service.store.lock().unwrap();
            store.preferences.plugins_catalog_source = None;
            store.preferences.plugins_catalog_url = Some("https://cdn.example/catalog.json".into());
        }
        service.persist().unwrap();
        drop(service);
        let service = UpdateService::open(dir.path().into()).unwrap();
        assert_eq!(service.status().preferences.catalog_source(), "hosted");
        assert_eq!(
            service.status().effective_plugins_catalog_url.as_deref(),
            Some("https://cdn.example/catalog.json")
        );
    }
    #[test]
    // Verify that failed plugin check does not hide successful manager check
    fn failed_plugin_check_does_not_hide_successful_manager_check() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_repository
            .clear();
        service.store.lock().unwrap().manager = CheckState {
            state: "current".into(),
            last_success: Some(123),
            catalog: Some(catalog()),
            ..Default::default()
        };
        assert!(service
            .check_inner("plugins", &AtomicBool::new(false))
            .is_err());
        let status = service.status();
        assert_eq!(status.manager.state, "current");
        assert_eq!(status.manager.last_success, Some(123));
        assert_eq!(status.plugins.state, "failed");
        assert!(status.plugins.error.is_some());
    }
    #[test]
    // Verify that metadata cache honors etag and 304 without external network
    fn metadata_cache_honors_etag_and_304_without_external_network() {
        use std::net::TcpListener;
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(_) => return,
        };
        let url = format!("http://{}/releases", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for index in 0..2 {
                let (mut s, _) = listener.accept().unwrap();
                s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut b = [0u8; 4096];
                let n = s.read(&mut b).unwrap();
                let request = String::from_utf8_lossy(&b[..n]).to_lowercase();
                if index == 0 {
                    s.write_all(b"HTTP/1.1 200 OK\r\nETag: \"fixture\"\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]").unwrap()
                } else {
                    assert!(request.contains("if-none-match: \"fixture\""));
                    s.write_all(b"HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap()
                }
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        let c = client().unwrap();
        for _ in 0..2 {
            assert_eq!(
                service.api(&c, &url, &AtomicBool::new(false)).unwrap(),
                serde_json::json!([])
            )
        }
        server.join().unwrap();
    }
}

#[derive(Serialize, Deserialize)]
struct MetadataAssetCache {
    etag: String,
    bytes: Vec<u8>,
}
impl UpdateService {
    #[allow(clippy::too_many_arguments)]
    // Fetch a bounded release metadata asset with repository-scoped cache identity and authorization checks
    fn metadata_asset(
        &self,
        client: &Client,
        url: &str,
        product: &str,
        repo: &str,
        tag: &str,
        channel: &str,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, String> {
        let key = hex::encode(Sha256::digest(
            format!("{product}/{repo}/{channel}/{url}").as_bytes(),
        ));
        let path = self.root.join("cache").join(format!("asset-{key}.json"));
        let _lease = self.cache_lease(&path)?;
        let cache = std::fs::File::open(&path).ok().and_then(|f| {
            let mut b = vec![];
            f.take(12 * 1024 * 1024 + 1).read_to_end(&mut b).ok()?;
            if b.len() > 12 * 1024 * 1024 {
                return None;
            };
            serde_json::from_slice::<MetadataAssetCache>(&b).ok()
        });
        let response = fetch_asset_conditional(
            client,
            url,
            repo,
            tag,
            cancel,
            cache
                .as_ref()
                .filter(|c| !c.etag.is_empty())
                .map(|c| c.etag.as_str()),
        )?;
        if response.status() == 304 {
            return cache
                .map(|c| c.bytes)
                .ok_or("Conditional asset cache missing".into());
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut bytes = vec![];
        let mut response = response;
        let mut buffer = [0u8; 65536];
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".into());
            };
            let n = response.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            };
            bytes.extend_from_slice(&buffer[..n]);
            if bytes.len() > 2 * 1024 * 1024 {
                return Err("Catalogue exceeds limit".into());
            }
        }
        atomic_json(
            &path,
            &MetadataAssetCache {
                etag,
                bytes: bytes.clone(),
            },
        )?;
        Ok(bytes)
    }
}
