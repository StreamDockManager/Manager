//! GitHub release catalogs and repository-scoped, OS-held authentication
use super::*;
use crate::credentials::CredentialState;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};

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
        cancel: &AtomicBool,
    ) -> Result<BTreeMap<String, PluginCatalog>, String> {
        account(repository)?;
        let token = self.plugin_github_token(repository)?;
        let client = client()?;
        let mut releases = vec![];
        for page in 1..=10 {
            let data = self.api_with_token(&client, &format!("https://api.github.com/repos/{repository}/releases?per_page=100&page={page}"), cancel, token.as_deref())
                .map_err(|e| format!("Cannot read GitHub plugin releases. Check the repository, connection or API rate limit. Public repositories do not require sign-in; private repositories need configured access. {e}"))?;
            let list = data.as_array().ok_or("Invalid GitHub releases response")?;
            releases.extend(list.iter().cloned());
            if list.len() < 100 {
                break;
            }
        }
        let mut selected = candidates(&releases);
        let mut catalogs = BTreeMap::new();
        for channel in ["stable", "development"] {
            let catalog = if let Some(release) = selected.remove(channel) {
                let tag = release["tag_name"].as_str().unwrap();
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
                let bytes = stream_dock_manager_downloads::metadata(
                    initial,
                    stream_dock_manager_downloads::NetworkPolicy {
                        headers: Some(&headers),
                        validate: &policy,
                        timeout: Duration::from_secs(20),
                    },
                    4 * 1024 * 1024,
                    &progress,
                )?;
                let value: PluginCatalog =
                    serde_json::from_slice(&bytes).map_err(|_| "Invalid plugin catalog JSON")?;
                value.validate(repository, channel)?;
                if value
                    .plugins
                    .iter()
                    .flat_map(|p| &p.downloads)
                    .any(|a| !a.release_tag.starts_with(&format!("plugins-{channel}-")))
                    || !tag.starts_with(&format!("plugins-{channel}-"))
                {
                    return Err("Plugin catalog release channel mismatch".into());
                }
                value
            } else {
                PluginCatalog {
                    schema_version: 1,
                    product: "plugins".into(),
                    repository: repository.into(),
                    channel: channel.into(),
                    plugins: vec![],
                    preview: false,
                }
            };
            catalogs.insert(channel.into(), catalog);
        }
        Ok(catalogs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
