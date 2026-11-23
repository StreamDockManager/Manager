//! GitHub release catalogs and repository-scoped, OS-held authentication
use super::*;
use crate::credentials::CredentialState;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};

// Two channels only, process-local, never serialized. Keys contain a one-way credential
// context hash, not credentials or validators usable at another origin.
#[derive(Default, Clone)]
pub(super) struct CatalogCache {
    generation: u64,
    entries: BTreeMap<String, (String, PluginCatalog)>,
}
impl CatalogCache {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }
    fn resolve(
        &mut self,
        (repository, channel): (&str, &str),
        release: &serde_json::Value,
        asset: &serde_json::Value,
        token: Option<&str>,
        cancel: &AtomicBool,
        fetch: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Result<PluginCatalog, String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".into());
        }
        let digest = match asset["digest"].as_str() {
            None if asset["digest"].is_null() => None,
            None => return Err("Invalid GitHub catalog SHA-256 digest".into()),
            Some("") => None,
            Some(value) => Some(
                value
                    .strip_prefix("sha256:")
                    .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
                    .ok_or("Invalid GitHub catalog SHA-256 digest")?
                    .to_ascii_lowercase(),
            ),
        };
        let key = hex::encode(Sha256::digest(
            serde_json::to_vec(&(
                repository,
                channel,
                &release["id"],
                &release["tag_name"],
                &asset["id"],
                &asset["url"],
                &asset["size"],
                &digest,
                token.map(|t| hex::encode(Sha256::digest(t.as_bytes()))),
            ))
            .map_err(|e| e.to_string())?,
        ));
        if digest.is_some() {
            if let Some((_, value)) = self.entries.get(channel).filter(|(old, _)| old == &key) {
                return Ok(value.clone());
            }
        }
        let bytes = fetch()?;
        if bytes.len() > 4 * 1024 * 1024 || asset["size"].as_u64() != Some(bytes.len() as u64) {
            return Err("Plugin catalog size mismatch/limit".into());
        }
        if digest
            .as_ref()
            .is_some_and(|expected| *expected != hex::encode(Sha256::digest(&bytes)))
        {
            return Err("Plugin catalog SHA-256 mismatch".into());
        }
        let value: PluginCatalog = serde_json::from_slice(&bytes)
            .map_err(|_| "Invalid plugin catalog JSON (expected schema 2)")?;
        value.validate(repository, channel)?;
        if value
            .plugins
            .iter()
            .flat_map(|p| &p.downloads)
            .any(|a| !a.release_tag.starts_with(&format!("plugins-{channel}-")))
            || !release["tag_name"]
                .as_str()
                .unwrap_or("")
                .starts_with(&format!("plugins-{channel}-"))
        {
            return Err("Plugin catalog release channel mismatch".into());
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".into());
        }
        if digest.is_some() && ["stable", "development"].contains(&channel) {
            self.entries.insert(channel.into(), (key, value.clone()));
        }
        Ok(value)
    }
}

// Account for plugin github resource usage
fn account(repository: &str) -> Result<String, String> {
    if !repository_valid(repository) {
        return Err("Use a GitHub owner/repository identity".into());
    }
    Ok(format!(
        "plugin-github-{}",
        hex::encode(Sha256::digest(repository.as_bytes()))
    ))
}

// Report the asset API origin condition for plugin github
pub(super) fn asset_api_origin(value: &str, repository: &str) -> bool {
    let prefix = format!("https://api.github.com/repos/{repository}/releases/assets/");
    value.strip_prefix(&prefix).is_some_and(|id| {
        !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && id.parse::<u64>().is_ok_and(|id| id > 0)
    })
}

// Allow only the original repository asset API URL or approved HTTPS asset redirects
pub(super) fn asset_origin(
    url: &Url,
    repository: &str,
    initial: &str,
    redirect: bool,
) -> Result<(), String> {
    if !asset_api_origin(initial, repository) {
        return Err("Invalid GitHub release asset API URL".into());
    }
    if !redirect && url.as_str() == initial {
        return Ok(());
    }
    if redirect
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && matches!(
            url.host_str(),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com")
        )
    {
        return Ok(());
    }
    Err("Unexpected GitHub asset redirect".into())
}

// Attach the bearer credential only to the original asset API request
pub(super) fn asset_headers(
    url: &Url,
    initial: &str,
    token: Option<&str>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    if url.as_str() == initial {
        headers.insert(ACCEPT, HeaderValue::from_static("application/octet-stream"));
        if let Some(token) = token {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| "Invalid GitHub token")?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
    }
    Ok(headers)
}

// Select the newest eligible non-draft catalog release for each supported channel
fn candidates(releases: &[serde_json::Value]) -> BTreeMap<String, serde_json::Value> {
    let mut selected: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for release in releases {
        if release["draft"] != false {
            continue;
        }
        let tag = release["tag_name"].as_str().unwrap_or("");
        let channel = if tag.starts_with("plugins-stable-") && release["prerelease"] == false {
            "stable"
        } else if tag.starts_with("plugins-development-") && release["prerelease"] == true {
            "development"
        } else {
            continue;
        };
        if !safe_name(tag)
            || !release["assets"]
                .as_array()
                .is_some_and(|assets| assets.iter().any(|a| a["name"] == "catalog.json"))
        {
            continue;
        }
        let order = |r: &serde_json::Value| {
            (
                r["published_at"].as_str().unwrap_or("").to_string(),
                r["id"].as_u64().unwrap_or(0),
            )
        };
        if selected
            .get(channel)
            .is_none_or(|old| order(release) > order(old))
        {
            selected.insert(channel.into(), release.clone());
        }
    }
    selected
}

impl UpdateService {
    // Report repository credential availability without exposing its value
    pub fn plugin_github_credential_status(
        &self,
        repository: &str,
    ) -> Result<CredentialState, String> {
        let key = account(repository)?;
        if !self
            .store
            .lock()
            .unwrap()
            .github_credential_repositories
            .contains(repository)
        {
            return Ok(CredentialState::Missing);
        }
        Ok(match self.github_credentials.provider.read(&key) {
            Ok(_) => CredentialState::Configured,
            Err(state) => state,
        })
    }
    // Save plugin github token
    pub fn save_plugin_github_token(&self, repository: &str, token: &str) -> Result<(), String> {
        let key = account(repository)?;
        if token.is_empty() || token.len() > 4096 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("Enter a GitHub access token without whitespace".into());
        }
        let _change = self.github_credentials.changes.lock().unwrap();
        // Retire in-flight catalog work before the credential provider can change.
        // Readers share the change guard, but no store/cache lock spans provider I/O.
        {
            let mut store = self.store.lock().unwrap();
            store.catalog_generation = store.catalog_generation.wrapping_add(1);
        }
        self.github_credentials
            .provider
            .write(&key, token)
            .map_err(|_| "The OS credential store is locked or unavailable; token was not saved")?;
        self.github_credentials.remember(token);
        {
            let mut store = self.store.lock().unwrap();
            store
                .github_credential_repositories
                .insert(repository.into());
            if store.preferences.plugins_repository == repository
                && store.preferences.catalog_source() == "repository"
            {
                store.plugins.retry_after = None;
            }
        }
        self.persist()
    }
    // Clear plugin github token
    pub fn clear_plugin_github_token(&self, repository: &str) -> Result<(), String> {
        let key = account(repository)?;
        let _change = self.github_credentials.changes.lock().unwrap();
        // Retire in-flight catalog work before the credential provider can change.
        // Readers share the change guard, but no store/cache lock spans provider I/O.
        {
            let mut store = self.store.lock().unwrap();
            store.catalog_generation = store.catalog_generation.wrapping_add(1);
        }
        self.github_credentials
            .provider
            .delete(&key)
            .map_err(|_| "The OS credential store is locked or unavailable; token was retained")?;
        {
            let mut store = self.store.lock().unwrap();
            store.github_credential_repositories.remove(repository);
            if store.preferences.plugins_repository == repository
                && store.preferences.catalog_source() == "repository"
            {
                store.plugins.retry_after = None;
            }
        }
        self.persist()
    }
    // Read the explicitly configured repository token from the OS credential store
    pub(super) fn plugin_github_token(&self, repository: &str) -> Result<Option<String>, String> {
        let _change = self.github_credentials.changes.lock().unwrap();
        if !self
            .store
            .lock()
            .unwrap()
            .github_credential_repositories
            .contains(repository)
        {
            return Ok(None);
        }
        self.github_credentials.read(&account(repository)?).map(Some).map_err(|_| "GitHub token is missing or the OS credential store is locked; update the token in Catalog settings".into())
    }
    // Verify release identity, filename and size before returning its repository-scoped asset API URL
    pub(super) fn plugin_release_asset(
        &self,
        repository: &str,
        tag: &str,
        filename: &str,
        size: u64,
        cancel: &AtomicBool,
    ) -> Result<String, String> {
        if !repository_valid(repository) || !safe_name(tag) || !safe_name(filename) {
            return Err("Invalid plugin release identity".into());
        }
        let token = self.plugin_github_token(repository)?;
        let release = self.api_with_token(
            &client()?,
            &format!("https://api.github.com/repos/{repository}/releases/tags/{tag}"),
            cancel,
            token.as_deref(),
        )?;
        let expected =
            format!("https://github.com/{repository}/releases/download/{tag}/{filename}");
        let assets = release["assets"]
            .as_array()
            .ok_or("Missing GitHub release assets")?;
        let matches: Vec<_> = assets
            .iter()
            .filter(|a| {
                a["name"] == filename
                    && a["size"].as_u64() == Some(size)
                    && a["browser_download_url"] == expected
            })
            .collect();
        if release["draft"] != false || release["tag_name"] != tag || matches.len() != 1 {
            return Err("GitHub asset differs from the verified plugin catalog".into());
        }
        let url = matches[0]["url"]
            .as_str()
            .ok_or("Missing GitHub asset API URL")?;
        if !asset_api_origin(url, repository) {
            return Err("Invalid GitHub asset API origin".into());
        }
        Ok(url.into())
    }
    // Read plugin catalogs from the configured GitHub repositories
    pub(super) fn repository_plugin_catalogs(
        &self,
        repository: &str,
        generation: u64,
        cancel: &AtomicBool,
    ) -> Result<BTreeMap<String, PluginCatalog>, String> {
        account(repository)?;
        let token = self.plugin_github_token(repository)?;
        let client = client()?;
        let mut releases = vec![];
        for page in 1..=10 {
            let source = format!("https://api.github.com/repos/{repository}/releases");
            let url = format!("{source}?per_page=100&page={page}");
            let data = self.catalog_fetch(&client, super::catalog_transport::CatalogRequest {
                source: &source, repository, channel: "releases", url: &url, generation,
                token: token.as_deref(), kind: super::catalog_transport::CatalogKind::Listing,
            }, cancel).map_err(|e| format!("Cannot read GitHub plugin releases. Check the repository, connection or API rate limit. {e}"))?;
            let super::catalog_transport::ParsedCatalog::Listing(list) = &*data else {
                return Err("Catalog type mismatch".into());
            };
            releases.extend(list.iter().cloned());
            if list.len() < 100 {
                break;
            }
        }
        let mut cache = self.repository_catalog_cache.lock().unwrap().clone();
        if cache.generation != generation {
            cache.clear();
            cache.generation = generation;
        }
        let mut selected = candidates(&releases);
        let mut catalogs = BTreeMap::new();
        for channel in ["stable", "development"] {
            let catalog = if let Some(release) = selected.remove(channel) {
                let assets = release["assets"].as_array().unwrap();
                let matches: Vec<_> = assets
                    .iter()
                    .filter(|a| a["name"] == "catalog.json")
                    .collect();
                if matches.len() != 1
                    || !matches[0]["size"]
                        .as_u64()
                        .is_some_and(|size| (1..=4 * 1024 * 1024).contains(&size))
                {
                    return Err("Invalid GitHub catalog asset size/identity".into());
                }
                let initial = matches[0]["url"]
                    .as_str()
                    .ok_or("Missing catalog asset API URL")?;
                let policy = |url: &Url, redirect| asset_origin(url, repository, initial, redirect);
                let headers = |url: &Url| asset_headers(url, initial, token.as_deref());
                let progress = |_, _| {
                    if cancel.load(Ordering::SeqCst) {
                        Err("Cancelled".into())
                    } else {
                        Ok(())
                    }
                };
                cache.resolve(
                    (repository, channel),
                    &release,
                    matches[0],
                    token.as_deref(),
                    cancel,
                    || {
                        stream_dock_manager_downloads::metadata(
                            initial,
                            stream_dock_manager_downloads::NetworkPolicy {
                                headers: Some(&headers),
                                validate: &policy,
                                timeout: Duration::from_secs(20),
                            },
                            4 * 1024 * 1024,
                            &progress,
                        )
                    },
                )?
            } else {
                PluginCatalog {
                    schema_version: super::plugin_catalog::PLUGIN_CATALOG_SCHEMA,
                    product: "plugins".into(),
                    repository: repository.into(),
                    channel: channel.into(),
                    plugins: vec![],
                    preview: false,
                }
            };
            catalogs.insert(channel.into(), catalog);
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("Cancelled".into());
        }
        if self.plugin_github_token(repository)? != token {
            self.repository_catalog_cache.lock().unwrap().clear();
            return Err("GitHub credential changed during catalog check".into());
        }
        let store = self.store.lock().unwrap();
        if store.catalog_generation != generation {
            return Err("Plugin source or credential changed during check".into());
        }
        *self.repository_catalog_cache.lock().unwrap() = cache;
        Ok(catalogs)
    }
}

// Independently bind the reviewed archive to GitHub's numeric repository, owner
// and asset uploader identities. Old catalogs and unavailable proof require review.
fn release_publisher(
    repo: &serde_json::Value,
    release: &serde_json::Value,
    repository: &str,
    artifact: &PluginArtifact,
) -> Option<String> {
    let full = repo["full_name"].as_str()?;
    if !full.eq_ignore_ascii_case(repository)
        || repo["url"].as_str()? != format!("https://api.github.com/repos/{full}")
        || release["draft"] != false
        || release["tag_name"] != artifact.release_tag
    {
        return None;
    }
    let id = |v: &serde_json::Value| v.as_u64().filter(|v| *v != 0);
    let digest = format!("sha256:{}", artifact.sha256);
    let mut candidates = release["assets"]
        .as_array()?
        .iter()
        .filter(|a| a["name"] == artifact.filename);
    let asset = candidates.next()?;
    if candidates.next().is_some()
        || asset["size"].as_u64() != Some(artifact.size)
        || asset["digest"] != digest
        || asset["browser_download_url"] != artifact.url
        || !asset_api_origin(asset["url"].as_str()?, repository)
    {
        return None;
    }
    Some(format!(
        "github/{}/{}/{}/{}",
        id(&repo["id"])?,
        id(&repo["owner"]["id"])?,
        id(&release["author"]["id"])?,
        id(&asset["uploader"]["id"])?
    ))
}

impl UpdateService {
    pub(crate) fn scoped_publisher(&self, selection: &CatalogSelection) -> Option<String> {
        let repo = &selection.repository;
        let artifact = &selection.artifact;
        if !repository_valid(repo)
            || !safe_name(&artifact.release_tag)
            || !safe_name(&artifact.filename)
        {
            return None;
        }
        let cancel = AtomicBool::new(false);
        let client = client().ok()?;
        let token = self.plugin_github_token(repo).ok()?;
        let base = format!("https://api.github.com/repos/{repo}");
        let info = self
            .api_metadata(&client, &base, &cancel, token.as_deref(), false)
            .ok()?;
        let release = self
            .api_metadata(
                &client,
                &format!("{base}/releases/tags/{}", artifact.release_tag),
                &cancel,
                token.as_deref(),
                false,
            )
            .ok()?;
        release_publisher(&info, &release, repo, artifact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verified_catalog_reuse_is_bounded_scoped_and_failure_atomic() {
        use std::cell::Cell;
        let bytes = serde_json::to_vec(&serde_json::json!({"schema_version":2,"product":"plugins","repository":"owner/repo","channel":"stable","plugins":[],"preview":false})).unwrap();
        let release = serde_json::json!({"id":1,"tag_name":"plugins-stable-test"});
        let mut asset = serde_json::json!({"id":2,"url":"https://api.github.com/repos/owner/repo/releases/assets/2","size":bytes.len(),"digest":format!("sha256:{}",hex::encode(Sha256::digest(&bytes)))});
        let cancel = AtomicBool::new(false);
        let calls = Cell::new(0);
        let transferred = Cell::new(0);
        let fetch = || {
            calls.set(calls.get() + 1);
            transferred.set(transferred.get() + bytes.len());
            Ok(bytes.clone())
        };
        let mut cache = CatalogCache::default();
        for _ in 0..2 {
            cache
                .resolve(
                    ("owner/repo", "stable"),
                    &release,
                    &asset,
                    None,
                    &cancel,
                    fetch,
                )
                .unwrap();
        }
        assert_eq!(calls.get(), 1);
        assert_eq!(transferred.get(), bytes.len());
        cache
            .resolve(
                ("owner/repo", "stable"),
                &release,
                &asset,
                Some("fixture-token"),
                &cancel,
                fetch,
            )
            .unwrap();
        assert_eq!(calls.get(), 2);
        let retained = cache.entries["stable"].0.clone();
        asset["id"] = 3.into();
        assert!(cache
            .resolve(
                ("owner/repo", "stable"),
                &release,
                &asset,
                Some("fixture-token"),
                &cancel,
                || Ok(vec![0; bytes.len()])
            )
            .is_err());
        assert_eq!(cache.entries["stable"].0, retained);
        assert!(cache
            .resolve(
                ("other/repo", "stable"),
                &release,
                &asset,
                None,
                &cancel,
                fetch
            )
            .is_err());
        assert!(cache
            .resolve(
                ("owner/repo", "development"),
                &release,
                &asset,
                None,
                &cancel,
                fetch
            )
            .is_err());
        cancel.store(true, Ordering::SeqCst);
        assert!(cache
            .resolve(
                ("owner/repo", "stable"),
                &release,
                &asset,
                None,
                &cancel,
                || panic!("cancelled fetch")
            )
            .is_err());
        cancel.store(false, Ordering::SeqCst);
        asset["digest"] = serde_json::Value::Null;
        let before = calls.get();
        for _ in 0..2 {
            cache
                .resolve(
                    ("owner/repo", "stable"),
                    &release,
                    &asset,
                    None,
                    &cancel,
                    fetch,
                )
                .unwrap();
        }
        assert_eq!(calls.get(), before + 2);
        let mut invalid = asset.clone();
        invalid["size"] = 1.into();
        assert!(cache
            .resolve(
                ("owner/repo", "stable"),
                &release,
                &invalid,
                None,
                &cancel,
                || Ok(b"{".to_vec())
            )
            .unwrap_err()
            .contains("JSON"));
        invalid["size"] = (4 * 1024 * 1024 + 1).into();
        assert!(cache
            .resolve(
                ("owner/repo", "stable"),
                &release,
                &invalid,
                None,
                &cancel,
                || Ok(vec![0; 4 * 1024 * 1024 + 1])
            )
            .unwrap_err()
            .contains("limit"));
        assert!(cache
            .resolve(
                ("owner/repo", "stable"),
                &release,
                &asset,
                None,
                &cancel,
                || Err("429 retry later".into())
            )
            .unwrap_err()
            .contains("429"));
        assert_eq!(cache.entries.len(), 1);
        cache.clear();
        assert!(cache.entries.is_empty());
    }

    #[derive(Default)]
    struct MemoryCredentials(Mutex<BTreeMap<String, String>>);
    impl crate::credentials::CredentialProvider for MemoryCredentials {
        // Read the requested plugin github data
        fn read(&self, account: &str) -> Result<String, CredentialState> {
            self.0
                .lock()
                .unwrap()
                .get(account)
                .cloned()
                .ok_or(CredentialState::Missing)
        }
        // Write the supplied plugin github data
        fn write(&self, account: &str, value: &str) -> Result<(), CredentialState> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
        // Remove the selected plugin github entry
        fn delete(&self, account: &str) -> Result<(), CredentialState> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }
    #[test]
    // Verify that tokens are repository scoped and never enter settings or status
    fn tokens_are_repository_scoped_and_never_enter_settings_or_status() {
        let root = tempfile::tempdir().unwrap();
        let provider = Arc::new(MemoryCredentials::default());
        let downloads = Arc::new(
            stream_dock_manager_downloads::DownloadCache::open(root.path().join("partials"))
                .unwrap(),
        );
        let service = UpdateService::open_with_credential_provider(
            root.path().join("updates"),
            downloads,
            provider.clone(),
        )
        .unwrap();
        let generation = service.store.lock().unwrap().catalog_generation;
        assert_eq!(
            service
                .plugin_github_credential_status("example/private")
                .unwrap(),
            CredentialState::Missing
        );
        service
            .save_plugin_github_token("example/private", "fixture-private-token")
            .unwrap();
        assert_eq!(
            service
                .plugin_github_credential_status("example/private")
                .unwrap(),
            CredentialState::Configured
        );
        assert_eq!(
            service
                .plugin_github_token("example/private")
                .unwrap()
                .as_deref(),
            Some("fixture-private-token")
        );
        assert!(service
            .plugin_github_token("example/other")
            .unwrap()
            .is_none());
        assert!(!serde_json::to_string(&service.status())
            .unwrap()
            .contains("fixture-private-token"));
        assert!(!service
            .storage
            .read_json::<serde_json::Value>(
                stream_dock_manager_storage::Domain::LocalPreferences,
                "updates",
                16 * 1024 * 1024
            )
            .unwrap()
            .unwrap()
            .to_string()
            .contains("fixture-private-token"));
        service
            .clear_plugin_github_token("example/private")
            .unwrap();
        assert_eq!(
            service
                .plugin_github_credential_status("example/private")
                .unwrap(),
            CredentialState::Missing
        );
        assert!(provider.0.lock().unwrap().is_empty());
        assert_eq!(
            service.store.lock().unwrap().catalog_generation,
            generation + 2
        );
        let mut prefs = service.status().preferences;
        prefs.plugins_repository = "example/switched".into();
        service.preferences(prefs).unwrap();
        assert_eq!(
            service.store.lock().unwrap().catalog_generation,
            generation + 3
        );
    }
    #[test]
    // Verify that auth is only sent to the exact github asset endpoint
    fn auth_is_only_sent_to_the_exact_github_asset_endpoint() {
        let initial = "https://api.github.com/repos/example/plugins/releases/assets/123";
        assert!(asset_api_origin(initial, "example/plugins"));
        assert!(!asset_api_origin(
            "https://api.github.com/repos/other/plugins/releases/assets/123",
            "example/plugins"
        ));
        let headers = asset_headers(
            &Url::parse(initial).unwrap(),
            initial,
            Some("fixture-secret"),
        )
        .unwrap();
        assert!(headers[AUTHORIZATION].is_sensitive());
        let redirect =
            Url::parse("https://release-assets.githubusercontent.com/file?signature=temporary")
                .unwrap();
        assert!(asset_origin(&redirect, "example/plugins", initial, true).is_ok());
        assert!(!asset_headers(&redirect, initial, Some("fixture-secret"))
            .unwrap()
            .contains_key(AUTHORIZATION));
        assert!(asset_origin(
            &Url::parse("https://evil.example/file").unwrap(),
            "example/plugins",
            initial,
            true
        )
        .is_err());
    }
    #[test]
    fn publisher_identity_requires_an_independently_matching_release_asset() {
        let repository = "example/plugins";
        let url = "https://github.com/example/plugins/releases/download/v2/package.zip";
        let digest = "ab".repeat(32);
        let artifact: PluginArtifact = serde_json::from_value(serde_json::json!({"filename":"package.zip","release_tag":"v2","size":42,"url":url,"sha256":digest,"os":"linux","architectures":["x86_64"],"source_commit":"fixture","files":{}})).unwrap();
        let repo = serde_json::json!({"full_name":repository,"url":"https://api.github.com/repos/example/plugins","id":11,"owner":{"id":12}});
        let mut release = serde_json::json!({"tag_name":"v2","draft":false,"author":{"id":13},"assets":[{"name":"package.zip","size":42,"browser_download_url":url,"url":"https://api.github.com/repos/example/plugins/releases/assets/17","digest":format!("sha256:{digest}"),"uploader":{"id":14}}]});
        let proof = release_publisher(&repo, &release, repository, &artifact).unwrap();
        assert_eq!(proof, "github/11/12/13/14");
        release["assets"][0]["uploader"]["id"] = 15.into();
        assert_ne!(
            release_publisher(&repo, &release, repository, &artifact).unwrap(),
            proof
        );
        assert!(release_publisher(&repo, &release, "example/other", &artifact).is_none());
        release["assets"][0]["digest"] = "sha256:mismatch".into();
        assert!(release_publisher(&repo, &release, repository, &artifact).is_none());
    }
    #[test]
    // Verify that channels select latest non draft published catalog
    fn channels_select_latest_non_draft_published_catalog() {
        let release = |tag, draft, prerelease, date, id| serde_json::json!({"tag_name":tag,"draft":draft,"prerelease":prerelease,"published_at":date,"id":id,"assets":[{"name":"catalog.json"}]});
        let values = vec![
            release("plugins-stable-1", false, false, "2026-01-01", 1),
            release("plugins-stable-2", false, false, "2026-02-01", 2),
            release("plugins-stable-3", true, false, "2026-03-01", 3),
            release("plugins-development-4", false, true, "2026-03-01", 4),
        ];
        let result = candidates(&values);
        assert_eq!(result["stable"]["id"], 2);
        assert_eq!(result["development"]["id"], 4);
    }
}
