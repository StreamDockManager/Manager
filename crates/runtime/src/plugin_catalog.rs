//! Plugin schema 1, independent of the application release catalog
use super::*;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginCatalog {
    pub schema_version: u32,
    pub product: String,
    pub repository: String,
    pub channel: String,
    pub plugins: Vec<CatalogPlugin>,
    #[serde(default)]
    pub preview: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct CatalogPlugin {
    pub icon: String,
    pub id: String,
    pub slug: String,
    pub name: String,
    pub version: String,
    pub minimum_manager: String,
    pub required_capabilities: Vec<String>,
    pub actions: Vec<String>,
    pub description: String,
    pub author: String,
    pub license: String,
    pub documentation: String,
    pub tags: Vec<String>,
    pub dependencies: Vec<PluginDependency>,
    pub downloads: Vec<PluginArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_download: Option<UnifiedPluginArtifact>,
    pub capabilities: stream_dock_manager_plugin_host::Capabilities,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginDependency {
    pub name: String,
    pub required: bool,
    pub platforms: Vec<String>,
    pub actions: Vec<String>,
    pub documentation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginDocumentationRequest {
    pub repository: String,
    pub channel: String,
    pub plugin_id: String,
    pub plugin_version: String,
    pub dependency: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct QualificationEvidence {
    pub plugin_version: String,
    pub os: String,
    pub sha256: String,
    pub url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct VsdCompatibility {
    pub status: String,
    pub reason: String,
    #[serde(default)]
    pub evidence: Option<QualificationEvidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginArtifact {
    pub filename: String,
    pub os: String,
    pub architectures: Vec<String>,
    pub release_tag: String,
    pub source_commit: String,
    pub url: String,
    #[ts(type = "number")]
    pub size: u64,
    pub sha256: String,
    pub files: BTreeMap<String, String>,
    pub vsd_craft_compatible: bool,
    pub compatibility: VsdCompatibility,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnifiedPluginTarget {
    pub os: String,
    pub architectures: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnifiedPluginArtifact {
    pub filename: String,
    pub release_tag: String,
    pub source_commit: String,
    pub url: String,
    #[ts(type = "number")]
    pub size: u64,
    pub sha256: String,
    pub files: BTreeMap<String, String>,
    pub targets: Vec<UnifiedPluginTarget>,
}
impl UnifiedPluginArtifact {
    // Describe the unified artifact for a target without claiming native VSD qualification
    fn artifact(&self, os: &str, architectures: Vec<String>) -> PluginArtifact {
        PluginArtifact {
            filename: self.filename.clone(),
            release_tag: self.release_tag.clone(),
            source_commit: self.source_commit.clone(),
            url: self.url.clone(),
            size: self.size,
            sha256: self.sha256.clone(),
            files: self.files.clone(),
            os: os.into(),
            architectures,
            vsd_craft_compatible: false,
            compatibility: VsdCompatibility {
                status: "unverified".into(),
                reason: "Unified package requires separate native qualification".into(),
                evidence: None,
            },
        }
    }
    // Select for host
    fn for_host(&self, os: &str, arch: &str) -> Option<PluginArtifact> {
        let target = self
            .targets
            .iter()
            .find(|target| target.os == os && target.architectures.iter().any(|a| a == arch))?;
        Some(self.artifact(os, target.architectures.clone()))
    }
    // Validate targets
    fn validate_targets(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        if self.targets.len() != 3
            || self.targets.iter().any(|target| {
                !seen.insert(target.os.as_str())
                    || artifact_architecture(&target.os, &target.architectures).is_none()
            })
            || !["macos", "windows", "linux"]
                .iter()
                .all(|os| seen.contains(os))
        {
            return Err(
                "Unified package must declare distinct macOS, Windows and Linux targets".into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct CatalogSelection {
    pub repository: String,
    pub catalog_url: Option<String>,
    pub github_asset_url: Option<String>,
    pub channel: String,
    pub plugin: CatalogPlugin,
    pub artifact: PluginArtifact,
}
impl CatalogSelection {
    // Build the install candidate from the selected catalog artifact and plugin requirements
    pub fn package(&self) -> PackageCandidate {
        PackageCandidate {
            files: self.artifact.files.clone(),
            os: self.artifact.os.clone(),
            id: self.plugin.id.clone(),
            version: self.plugin.version.clone(),
            path: self.artifact.filename.clone(),
            sha256: self.artifact.sha256.clone(),
            size: self.artifact.size,
            architectures: self.artifact.architectures.clone(),
            minimum_manager: self.plugin.minimum_manager.clone(),
            required_capabilities: self.plugin.required_capabilities.clone(),
        }
    }
}
// Validate the repository identity and resolve the configured catalog URL
pub fn catalog_url(repository: &str) -> Result<String, String> {
    if !repository_valid(repository) {
        return Err("Invalid plugin repository".into());
    }
    resolve_catalog_url(None, option_env!("STREAM_DOCK_MANAGER_PLUGINS_CATALOG_URL"))
}
// Resolve catalog URL
pub fn resolve_catalog_url(saved: Option<&str>, default: Option<&str>) -> Result<String, String> {
    let value = saved
        .filter(|v| !v.trim().is_empty())
        .or(default.filter(|v| !v.trim().is_empty()))
        .ok_or("Plugin catalog source is not configured; set its HTTPS URL in Catalog settings")?
        .trim();
    let url = public_catalog_url(value)?;
    Ok(url.to_string())
}
// Resolve the public plugin catalog endpoint
fn public_catalog_url(value: &str) -> Result<Url, String> {
    if !https(value) || value.contains(['\\', '%']) || value.chars().any(char::is_whitespace) {
        return Err("Plugin catalog URL requires an absolute HTTPS URL without credentials or encoded paths".into());
    }
    let url = Url::parse(value).map_err(|e| e.to_string())?;
    if url.query().is_some() || url.path().ends_with('/') {
        return Err("Use the complete public catalog URL without a query or fragment".into());
    }
    Ok(url)
}
// Resolve effective catalog URL
pub(super) fn effective_catalog_url(preferences: &UpdatePreferences) -> Result<String, String> {
    if !matches!(
        preferences.plugins_catalog_source.as_deref(),
        None | Some("" | "hosted" | "repository")
    ) {
        return Err("Choose hosted catalog or GitHub repository in Catalog settings".into());
    }
    if preferences.catalog_source() == "repository" {
        if !repository_valid(&preferences.plugins_repository) {
            return Err("Use a GitHub owner/repository identity".into());
        }
        return Ok(format!(
            "https://api.github.com/repos/{}/releases",
            preferences.plugins_repository
        ));
    }
    resolve_catalog_url(
        preferences.plugins_catalog_url.as_deref(),
        option_env!("STREAM_DOCK_MANAGER_PLUGINS_CATALOG_URL"),
    )
}
// Require the artifact URL to remain within the catalog origin and directory prefix
fn bucket_origin(value: &str, source: &str) -> Result<(), String> {
    let url = public_catalog_url(value)?;
    let base = public_catalog_url(source)?
        .join(".")
        .map_err(|e| e.to_string())?;
    if url.origin() != base.origin() || !url.path().starts_with(base.path()) {
        return Err("Plugin artifact is outside the configured catalog origin/prefix".into());
    }
    Ok(())
}
// Validate plugin origin
pub(super) fn validate_plugin_origin(
    value: &str,
    _repository: &str,
    _artifact: &PluginArtifact,
    source: Option<&str>,
    _redirect: bool,
) -> Result<(), String> {
    bucket_origin(
        value,
        source.ok_or("Plugin catalog source is not configured")?,
    )
}
// Report the HTTPS condition for plugin catalog
fn https(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
    })
}
// Architecture variants extend schema v1's existing architecture list
fn artifact_architecture(os: &str, architectures: &[String]) -> Option<&'static str> {
    match (
        os,
        architectures
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
    ) {
        ("macos", ["amd64", "arm64"]) => Some("universal"),
        ("macos" | "windows" | "linux", ["amd64"]) => Some("amd64"),
        ("macos", ["arm64"]) => Some("arm64"),
        _ => None,
    }
}
// Choose a matching host artifact with the narrowest architecture set
fn artifact_for_host<'a>(
    plugin: &'a CatalogPlugin,
    os: &str,
    arch: &str,
) -> Option<&'a PluginArtifact> {
    plugin
        .downloads
        .iter()
        .filter(|a| a.os == os && a.architectures.iter().any(|value| value == arch))
        .min_by_key(|a| a.architectures.len())
}

// Select artifact
fn select_artifact(
    plugin: &CatalogPlugin,
    os: &str,
    arch: &str,
    unified: bool,
) -> Result<PluginArtifact, String> {
    if unified {
        plugin
            .unified_download
            .as_ref()
            .and_then(|a| a.for_host(os, arch))
            .ok_or_else(|| "Unified plugin package unavailable for this platform".into())
    } else {
        artifact_for_host(plugin, os, arch)
            .cloned()
            .ok_or_else(|| "Plugin unavailable for this platform".into())
    }
}

impl PluginCatalog {
    // Validate the supplied plugin catalog value
    pub fn validate(&self, repository: &str, channel: &str) -> Result<(), String> {
        self.validate_source(repository, channel, None)
    }
    // Validate source
    pub fn validate_source(
        &self,
        repository: &str,
        channel: &str,
        source: Option<&str>,
    ) -> Result<(), String> {
        let repository_source = format!("https://api.github.com/repos/{repository}/releases");
        let source = source.filter(|url| *url != repository_source);
        if self.schema_version != 1
            || self.product != "plugins"
            || self.repository != repository
            || !repository_valid(repository)
            || self.channel != channel
            || !["stable", "development"].contains(&channel)
            || self.preview
            || self.plugins.len() > 256
        {
            return Err("Invalid plugin catalog schema/channel/origin".into());
        }
        let mut ids = std::collections::HashSet::new();
        for p in &self.plugins {
            stable_version(&p.version)?;
            stable_version(&p.minimum_manager)?;
            if !ids.insert(&p.id)
                || !safe_name(&p.id)
                || p.id.bytes().any(|b| b.is_ascii_uppercase())
                || !p.icon.starts_with("data:image/svg+xml;base64,")
                || p.icon.len() > 256 * 1024
                || p.name.is_empty()
                || p.description.is_empty()
                || p.author.is_empty()
                || p.license.is_empty()
                || !https(&p.documentation)
                || p.downloads.is_empty()
                || p.downloads.len() > 5
                || p.capabilities.id != p.id
                || p.capabilities.plugin_version != p.version
                || p.capabilities.minimum_manager != p.minimum_manager
                || p.capabilities.required_capabilities != p.required_capabilities
                || p.capabilities.actions.len() != p.actions.len()
                || p.capabilities
                    .actions
                    .keys()
                    .any(|id| !p.actions.iter().any(|a| *id == format!("{}.{}", p.id, a)))
            {
                return Err("Invalid plugin metadata/descriptor".into());
            }
            for d in &p.dependencies {
                if d.name.is_empty()
                    || !https(&d.documentation)
                    || d.platforms
                        .iter()
                        .any(|os| !["macos", "windows", "linux"].contains(&os.as_str()))
                    || d.actions.iter().any(|a| !p.actions.contains(a))
                {
                    return Err("Invalid plugin dependency".into());
                }
            }
            let mut platforms = std::collections::HashSet::new();
            if let Some(unified) = &p.unified_download {
                unified.validate_targets()?;
            }
            let unified = p
                .unified_download
                .as_ref()
                .map(|a| a.artifact("unified", vec![]));
            for (a, is_unified) in p
                .downloads
                .iter()
                .map(|a| (a, false))
                .chain(unified.iter().map(|a| (a, true)))
            {
                let architecture = if is_unified {
                    "all"
                } else {
                    artifact_architecture(&a.os, &a.architectures)
                        .ok_or("Invalid artifact architecture")?
                };
                let filename = if is_unified {
                    format!("{}-{}-unified.zip", p.id, p.version)
                } else {
                    format!("{}-{}-{}-{}.zip", p.id, p.version, a.os, architecture)
                };
                // Repository catalogs use exact immutable browser asset URLs. The
                // downloader independently resolves that tag/name/size through
                // the repository-scoped API and retains its redirect allowlist
                let url_valid = source.map_or_else(
                    || {
                        a.url
                            == format!(
                                "https://github.com/{repository}/releases/download/{}/{}",
                                a.release_tag, a.filename
                            )
                    },
                    |source| {
                        let suffix = format!(
                            "packages/{channel}/{}/{}/{}/{}",
                            p.id, p.version, a.sha256, a.filename
                        );
                        public_catalog_url(source)
                            .and_then(|base| base.join(&suffix).map_err(|e| e.to_string()))
                            .is_ok_and(|expected| {
                                public_catalog_url(&a.url).is_ok_and(|url| url == expected)
                                    && bucket_origin(&a.url, source).is_ok()
                            })
                    },
                );
                if !platforms.insert((&a.os, architecture))
                    || (!is_unified && !["macos", "windows", "linux"].contains(&a.os.as_str()))
                    || a.filename != filename
                    || !url_valid
                    || !a.release_tag.starts_with(&format!("plugins-{channel}-"))
                    || !safe_name(&a.release_tag)
                    || !valid_hash(&a.sha256)
                    || !(1..=256 * 1024 * 1024).contains(&a.size)
                    || !(40..=64).contains(&a.source_commit.len())
                    || !a
                        .source_commit
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    || a.files.is_empty()
                    || a.files.len() > 4096
                    || !a.files.contains_key("manifest.json")
                    || a.files.iter().any(|(n, h)| {
                        n.contains(['\\', ':', '\0'])
                            || n.split('/').any(|p| ["", ".", ".."].contains(&p))
                            || !valid_hash(h)
                    })
                {
                    return Err("Invalid immutable plugin artifact/inventory".into());
                }
                let c = &a.compatibility;
                if !["verified", "incompatible", "unverified"].contains(&c.status.as_str())
                    || c.reason.is_empty()
                    || a.vsd_craft_compatible != (c.status == "verified")
                {
                    return Err("Invalid VSD compatibility status".into());
                }
                if (c.status == "verified" || c.evidence.is_some())
                    && c.evidence.as_ref().is_none_or(|e| {
                        e.plugin_version != p.version
                            || e.os != a.os
                            || e.sha256 != a.sha256
                            || !https(&e.url)
                    })
                {
                    return Err("VSD evidence does not match artifact".into());
                }
            }
        }
        Ok(())
    }
}
impl UpdateService {
    // Resolve the selected plugin’s documentation URL
    fn plugin_documentation_url(
        &self,
        request: &PluginDocumentationRequest,
    ) -> Result<String, String> {
        let store = self.store.lock().unwrap();
        if request.repository != store.preferences.plugins_repository {
            return Err("Plugin catalog source changed; refresh the catalog".into());
        }
        let catalog = store
            .catalogs
            .get(&request.channel)
            .ok_or("Check plugin catalog first")?;
        catalog.validate_source(
            &store.preferences.plugins_repository,
            &request.channel,
            effective_catalog_url(&store.preferences).ok().as_deref(),
        )?;
        let plugin = catalog
            .plugins
            .iter()
            .find(|plugin| {
                plugin.id == request.plugin_id && plugin.version == request.plugin_version
            })
            .ok_or("Plugin catalog entry changed; refresh the catalog")?;
        if let Some(name) = &request.dependency {
            let mut matches = plugin
                .dependencies
                .iter()
                .filter(|dependency| &dependency.name == name);
            let dependency = matches
                .next()
                .ok_or("Dependency absent from catalog entry")?;
            if matches.next().is_some() {
                return Err("Dependency identity is ambiguous in catalog entry".into());
            }
            Ok(dependency.documentation.clone())
        } else {
            Ok(plugin.documentation.clone())
        }
    }
    // Check plugin catalogs
    pub(super) fn check_plugin_catalogs(&self, cancel: &AtomicBool) -> Result<(), String> {
        let initial = self.status();
        let prefs = initial.preferences;
        if initial
            .plugins
            .retry_after
            .is_some_and(|until| until > now())
        {
            return Err("Plugin catalog check is rate limited".into());
        }
        let base = effective_catalog_url(&prefs)?;
        {
            let mut s = self.store.lock().unwrap();
            s.plugins.state = "checking".into();
            s.plugins.error = None;
        }
        let result = (|| {
            let mut catalogs = if prefs.catalog_source() == "repository" {
                self.repository_plugin_catalogs(&prefs.plugins_repository, cancel)?
            } else {
                BTreeMap::new()
            };
            for channel in ["stable", "development"] {
                if prefs.catalog_source() == "repository" {
                    continue;
                }
                if cancel.load(Ordering::SeqCst) {
                    return Err("Cancelled".into());
                }
                let url = if channel == "stable" {
                    base.clone()
                } else {
                    Url::parse(&base)
                        .map_err(|e| e.to_string())?
                        .join("development/catalog.json")
                        .map_err(|e| e.to_string())?
                        .to_string()
                };
                let response = client()?.get(url).send().map_err(|e| e.to_string())?;
                if response.status().as_u16() == 429 {
                    let until = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|delay| now() + delay)
                        .unwrap_or(now() + 60);
                    self.store.lock().unwrap().plugins.retry_after = Some(until);
                }
                let response = response.error_for_status().map_err(|e| e.to_string())?;
                let mut bytes = vec![];
                response
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() > 4 * 1024 * 1024 {
                    return Err("Plugin catalog exceeds limit".into());
                }
                let c: PluginCatalog = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                c.validate_source(&prefs.plugins_repository, channel, Some(&base))?;
                catalogs.insert(channel.into(), c);
            }
            let mut s = self.store.lock().unwrap();
            if s.preferences.plugins_repository != prefs.plugins_repository
                || s.preferences.plugins_catalog_url != prefs.plugins_catalog_url
                || s.preferences.plugin_channel != prefs.plugin_channel
                || s.preferences.plugins_catalog_source != prefs.plugins_catalog_source
            {
                return Err("Plugin source changed during check".into());
            }
            s.catalog_source_url = Some(base.clone());
            s.catalogs = catalogs;
            Ok(())
        })();
        {
            let mut s = self.store.lock().unwrap();
            s.plugins.state = if result.is_ok() { "current" } else { "error" }.into();
            s.plugins.error = result.as_ref().err().cloned();
            if result.is_ok() {
                s.plugins.last_success = Some(now());
                s.plugins.retry_after = None;
            }
        }
        self.persist()?;
        result
    }
    // Download plugin
    pub fn download_plugin(
        self: &Arc<Self>,
        plugin_id: &str,
        channel: &str,
    ) -> Result<String, String> {
        self.download_plugin_variant(plugin_id, channel, false)
    }
    // Download plugin variant
    pub fn download_plugin_variant(
        self: &Arc<Self>,
        plugin_id: &str,
        channel: &str,
        unified: bool,
    ) -> Result<String, String> {
        let status = self.status();
        let c = status
            .available_catalogs
            .iter()
            .find(|c| c.channel == channel)
            .ok_or("Check plugin catalog first")?;
        let source = effective_catalog_url(&status.preferences).ok();
        c.validate_source(
            &status.preferences.plugins_repository,
            channel,
            source.as_deref(),
        )?;
        let plugin = c
            .plugins
            .iter()
            .find(|p| p.id == plugin_id)
            .cloned()
            .ok_or("Plugin absent from catalog")?;
        let artifact = select_artifact(&plugin, &status.host_os, &status.host_arch, unified)?;
        let selection = CatalogSelection {
            github_asset_url: None,
            catalog_url: source,
            repository: c.repository.clone(),
            channel: channel.into(),
            plugin,
            artifact,
        };
        compatible(&selection.package())?;
        self.launch_download(DownloadRequest::plugin(Arc::new(selection))?)
    }
    // Reject a reviewed selection if the configured repository or catalog URL changed
    pub(crate) fn selection_current(&self, selection: &CatalogSelection) -> Result<(), String> {
        let store = self.store.lock().unwrap();
        if store.preferences.plugins_repository != selection.repository
            || effective_catalog_url(&store.preferences).ok() != selection.catalog_url
        {
            return Err("Plugin catalog source changed; review the current catalog again".into());
        }
        Ok(())
    }
    // Copy the retained catalog selection for a downloaded plugin
    pub fn downloaded_selection(&self, id: &str) -> Option<CatalogSelection> {
        self.selections
            .lock()
            .unwrap()
            .get(id)
            .map(|selection| (**selection).clone())
    }
}
impl crate::Runtime {
    // Open plugin documentation
    pub fn open_plugin_documentation(
        &self,
        request: &PluginDocumentationRequest,
    ) -> Result<(), String> {
        crate::actions::open(&self.updates.plugin_documentation_url(request)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use serde_json::json;
    // Construct an isolated plugin catalog fixture
    fn fixture(version: &str) -> (CatalogPlugin, Vec<u8>) {
        let id = "org.test.catalog";
        let assets = stream_dock_manager_plugin_host::ThemedAsset {
            light: "icon.svg".into(),
            dark: "icon.svg".into(),
        };
        let capabilities = stream_dock_manager_plugin_host::Capabilities {
            version: 1,
            id: id.into(),
            plugin_version: version.into(),
            minimum_manager: "0.1.0".into(),
            required_capabilities: vec!["manager-controls-v1".into()],
            icon: assets.clone(),
            actions: BTreeMap::from([(
                format!("{id}.action"),
                stream_dock_manager_plugin_host::ActionCapability {
                    icon: assets.clone(),
                    states: vec![assets],
                    control_types: vec!["button".into()],
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let manifest = json!({"UUID":id,"Name":"Catalog fixture","Version":version,"SDKVersion":1,"CodePath":"plugin","StreamDockManager":{"ManifestDialect":"vsd","MinimumVersion":"0.1.0","RequiredCapabilities":capabilities.required_capabilities,"Capabilities":capabilities},"Actions":[{"UUID":format!("{id}.action"),"Name":"Fixture","Controllers":["Keypad"],"Icon":"icon.svg","States":[{"Image":"icon.svg"}]}]});
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&capabilities).unwrap());
        let frame = format!("SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1", encoded.len());
        let files = BTreeMap::from([
            (
                "manifest.json".to_string(),
                serde_json::to_vec(&manifest).unwrap(),
            ),
            ("plugin".into(), frame.into_bytes()),
            (
                "icon.svg".into(),
                b"<svg xmlns='http://www.w3.org/2000/svg' width='128' height='128'/>".to_vec(),
            ),
        ]);
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
        for (name, bytes) in &files {
            archive
                .start_file(
                    format!("{id}.sdPlugin/{name}"),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            archive.write_all(bytes).unwrap();
        }
        let bytes = archive.finish().unwrap().into_inner();
        let os = if cfg!(target_os = "macos") {
            "macos"
        } else {
            std::env::consts::OS
        };
        let filename = format!(
            "{id}-{version}-{os}-{}.zip",
            if os == "macos" { "universal" } else { "amd64" }
        );
        let tag = format!("plugins-stable-{}-aaaaaaaaaaaa", version.replace('.', "-"));
        let artifact = PluginArtifact {
            filename: filename.clone(),
            os: os.into(),
            architectures: if os == "macos" {
                vec!["amd64".into(), "arm64".into()]
            } else {
                vec!["amd64".into()]
            },
            release_tag: tag.clone(),
            source_commit: "a".repeat(40),
            url: format!(
                "https://catalog.example/packages/stable/{id}/{version}/{}/{filename}",
                hex::encode(Sha256::digest(&bytes))
            ),
            size: bytes.len() as u64,
            sha256: hex::encode(Sha256::digest(&bytes)),
            files: files
                .into_iter()
                .map(|(name, bytes)| (name, hex::encode(Sha256::digest(bytes))))
                .collect(),
            vsd_craft_compatible: false,
            compatibility: VsdCompatibility {
                status: "unverified".into(),
                reason: "Pending native qualification".into(),
                evidence: None,
            },
        };
        (
            CatalogPlugin {
                icon: "data:image/svg+xml;base64,PHN2Zy8+".into(),
                id: id.into(),
                slug: "fixture".into(),
                name: "Fixture".into(),
                version: version.into(),
                minimum_manager: "0.1.0".into(),
                required_capabilities: vec!["manager-controls-v1".into()],
                actions: vec!["action".into()],
                description: "Fixture".into(),
                author: "Test".into(),
                license: "GPL-3.0-or-later".into(),
                documentation: "https://example.com/setup".into(),
                tags: vec![],
                dependencies: vec![],
                downloads: vec![artifact],
                unified_download: None,
                capabilities,
            },
            bytes,
        )
    }
    // Construct a stable-channel catalog containing the test plugin
    fn catalog(plugin: CatalogPlugin) -> PluginCatalog {
        PluginCatalog {
            schema_version: 1,
            product: "plugins".into(),
            repository: "StreamDockManager/Plugins".into(),
            channel: "stable".into(),
            plugins: vec![plugin],
            preview: false,
        }
    }
    // Construct using unified
    fn with_unified(mut plugin: CatalogPlugin) -> CatalogPlugin {
        let base = &plugin.downloads[0];
        let filename = format!("{}-{}-unified.zip", plugin.id, plugin.version);
        plugin.unified_download = Some(UnifiedPluginArtifact {
            filename: filename.clone(),
            release_tag: base.release_tag.clone(),
            source_commit: base.source_commit.clone(),
            url: format!(
                "https://catalog.example/packages/stable/{}/{}/{}/{}",
                plugin.id, plugin.version, base.sha256, filename
            ),
            size: base.size,
            sha256: base.sha256.clone(),
            files: base.files.clone(),
            targets: vec![
                UnifiedPluginTarget {
                    os: "macos".into(),
                    architectures: vec!["amd64".into(), "arm64".into()],
                },
                UnifiedPluginTarget {
                    os: "windows".into(),
                    architectures: vec!["amd64".into()],
                },
                UnifiedPluginTarget {
                    os: "linux".into(),
                    architectures: vec!["amd64".into()],
                },
            ],
        });
        plugin
    }

    #[test]
    // Verify that unified catalog is opt in and checks target origin and inventory
    fn unified_catalog_is_opt_in_and_checks_target_origin_and_inventory() {
        let (plugin, _) = fixture("1.0.0");
        let plugin = with_unified(plugin);
        let host = &plugin.downloads[0];
        let chosen = select_artifact(&plugin, &host.os, &host.architectures[0], false).unwrap();
        assert_eq!(chosen.filename, host.filename);
        let chosen = select_artifact(&plugin, &host.os, &host.architectures[0], true).unwrap();
        assert!(chosen.filename.ends_with("-unified.zip"));
        assert_eq!(chosen.os, host.os);
        assert!(!chosen.vsd_craft_compatible);
        assert!(select_artifact(&plugin, "linux", "arm64", true).is_err());
        let mut no_default = plugin.clone();
        no_default.downloads.clear();
        assert!(select_artifact(&no_default, "macos", "arm64", false).is_err());
        let valid = catalog(plugin);
        valid
            .validate_source(
                &valid.repository,
                "stable",
                Some("https://catalog.example/catalog.json"),
            )
            .unwrap();
        for mutation in 0..8 {
            let mut bad = valid.clone();
            let a = bad.plugins[0].unified_download.as_mut().unwrap();
            match mutation {
                0 => {
                    a.targets.pop();
                }
                1 => a.targets[1].os = "macos".into(),
                2 => a.targets[1].architectures = vec!["arm64".into()],
                3 => a.url = a.url.replace("catalog.example", "untrusted.example"),
                4 => a.filename = "wrong.zip".into(),
                5 => {
                    a.files.insert("../escape".into(), "a".repeat(64));
                }
                6 => a.size = 0,
                _ => {
                    a.files.remove("manifest.json");
                }
            }
            assert!(
                bad.validate_source(
                    &bad.repository,
                    "stable",
                    Some("https://catalog.example/catalog.json")
                )
                .is_err(),
                "mutation {mutation}"
            );
        }
        let mut unknown = serde_json::to_value(valid).unwrap();
        unknown["plugins"][0]["unified_download"]["vsd_craft_compatible"] = json!(true);
        assert!(serde_json::from_value::<PluginCatalog>(unknown).is_err());
    }

    #[test]
    // Verify that mac variants validate and prefer the host architecture
    fn mac_variants_validate_and_prefer_the_host_architecture() {
        let (mut plugin, _) = fixture("1.0.0");
        let base = plugin.downloads[0].clone();
        plugin.downloads.clear();
        for archs in [vec!["amd64", "arm64"], vec!["arm64"], vec!["amd64"]] {
            let mut artifact = base.clone();
            artifact.os = "macos".into();
            artifact.architectures = archs.into_iter().map(str::to_owned).collect();
            let variant = artifact_architecture(&artifact.os, &artifact.architectures).unwrap();
            artifact.filename = format!("{}-{}-macos-{variant}.zip", plugin.id, plugin.version);
            artifact.url = format!(
                "https://catalog.example/packages/stable/{}/{}/{}/{}",
                plugin.id, plugin.version, artifact.sha256, artifact.filename
            );
            plugin.downloads.push(artifact);
        }
        catalog(plugin.clone())
            .validate_source(
                "StreamDockManager/Plugins",
                "stable",
                Some("https://catalog.example/catalog.json"),
            )
            .unwrap();
        for arch in ["amd64", "arm64"] {
            assert_eq!(
                artifact_for_host(&plugin, "macos", arch)
                    .unwrap()
                    .architectures,
                vec![arch]
            );
        }
        assert!(artifact_for_host(&plugin, "macos", "riscv64").is_none());
        plugin.downloads.reverse();
        assert_eq!(
            artifact_for_host(&plugin, "macos", "arm64")
                .unwrap()
                .architectures,
            vec!["arm64"]
        );
        plugin.downloads.retain(|a| a.architectures.len() == 2);
        assert_eq!(
            artifact_for_host(&plugin, "macos", "arm64")
                .unwrap()
                .architectures
                .len(),
            2
        );
        plugin.downloads.push(plugin.downloads[0].clone());
        assert!(catalog(plugin)
            .validate_source(
                "StreamDockManager/Plugins",
                "stable",
                Some("https://catalog.example/catalog.json")
            )
            .is_err());
        for archs in [
            vec!["arm64", "amd64"],
            vec!["amd64", "amd64"],
            vec!["riscv64"],
        ] {
            assert!(artifact_architecture(
                "macos",
                &archs.into_iter().map(str::to_owned).collect::<Vec<_>>()
            )
            .is_none());
        }
        assert!(artifact_architecture("windows", &["arm64".into()]).is_none());
    }
    // Construct the documentation fixture
    fn documentation_fixture() -> (PluginCatalog, PluginDocumentationRequest) {
        let (mut plugin, _) = fixture("1.0.0");
        plugin.dependencies = vec![PluginDependency {
            name: "FFmpeg".into(),
            required: false,
            platforms: vec!["macos".into(), "windows".into(), "linux".into()],
            actions: vec!["action".into()],
            documentation: "https://example.com/ffmpeg".into(),
        }];
        let catalog = catalog(plugin);
        let request = PluginDocumentationRequest {
            repository: catalog.repository.clone(),
            channel: catalog.channel.clone(),
            plugin_id: catalog.plugins[0].id.clone(),
            plugin_version: catalog.plugins[0].version.clone(),
            dependency: None,
        };
        (catalog, request)
    }
    #[test]
    // Verify that documentation links resolve only cached plugin and named dependency
    fn documentation_links_resolve_only_cached_plugin_and_named_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_source = Some("hosted".into());
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_url = Some("https://catalog.example/catalog.json".into());
        let (mut catalog, mut request) = documentation_fixture();
        assert!(service.plugin_documentation_url(&request).is_err());
        service
            .store
            .lock()
            .unwrap()
            .catalogs
            .insert("stable".into(), catalog.clone());
        assert_eq!(
            service.plugin_documentation_url(&request).unwrap(),
            "https://example.com/setup"
        );
        request.dependency = Some("FFmpeg".into());
        assert_eq!(
            service.plugin_documentation_url(&request).unwrap(),
            "https://example.com/ffmpeg"
        );
        let mut other = catalog.plugins[0].dependencies[0].clone();
        other.name = "Other".into();
        other.documentation = "https://example.com/other".into();
        catalog.plugins[0].dependencies.insert(0, other);
        service
            .store
            .lock()
            .unwrap()
            .catalogs
            .insert("stable".into(), catalog);
        assert_eq!(
            service.plugin_documentation_url(&request).unwrap(),
            "https://example.com/ffmpeg"
        );
    }
    #[test]
    // Verify that documentation links reject stale or unknown identity and caller URLs
    fn documentation_links_reject_stale_or_unknown_identity_and_caller_urls() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_source = Some("hosted".into());
        let (catalog, request) = documentation_fixture();
        service
            .store
            .lock()
            .unwrap()
            .catalogs
            .insert("stable".into(), catalog);
        let original = serde_json::to_value(&request).unwrap();
        for (field, value) in [
            ("repository", "other/plugins"),
            ("channel", "development"),
            ("plugin_id", "other.plugin"),
            ("plugin_version", "1.1.0"),
            ("dependency", "missing"),
        ] {
            let mut invalid = original.clone();
            invalid[field] = value.into();
            let invalid = serde_json::from_value(invalid).unwrap();
            assert!(
                service.plugin_documentation_url(&invalid).is_err(),
                "{field}"
            );
        }
        let mut injected = original;
        injected["url"] = "https://arbitrary.example".into();
        assert!(serde_json::from_value::<PluginDocumentationRequest>(injected).is_err());
        service.store.lock().unwrap().preferences.plugins_repository = "other/plugins".into();
        assert!(service.plugin_documentation_url(&request).is_err());
    }
    #[test]
    // Verify that documentation links revalidate persisted metadata and reject ambiguous dependencies
    fn documentation_links_revalidate_persisted_metadata_and_reject_ambiguous_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let service = UpdateService::open(dir.path().into()).unwrap();
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_source = Some("hosted".into());
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_url = Some("https://catalog.example/catalog.json".into());
        let (catalog, mut request) = documentation_fixture();
        for url in [
            "file:///tmp/test",
            "http://example.com",
            "javascript:alert(1)",
            "https://user:pass@example.com",
        ] {
            for dependency in [false, true] {
                let mut invalid = catalog.clone();
                if dependency {
                    invalid.plugins[0].dependencies[0].documentation = url.into();
                } else {
                    invalid.plugins[0].documentation = url.into();
                }
                service
                    .store
                    .lock()
                    .unwrap()
                    .catalogs
                    .insert("stable".into(), invalid);
                service.persist().unwrap();
                let restored = UpdateService::open_with_storage(
                    dir.path().into(),
                    service.downloads.clone(),
                    Arc::new(crate::credentials::OsCredentials),
                    service.storage.clone(),
                )
                .unwrap();
                assert!(
                    restored.plugin_documentation_url(&request).is_err(),
                    "{url}"
                );
            }
        }
        let mut ambiguous = catalog;
        let duplicate = ambiguous.plugins[0].dependencies[0].clone();
        ambiguous.plugins[0].dependencies.push(duplicate);
        service
            .store
            .lock()
            .unwrap()
            .catalogs
            .insert("stable".into(), ambiguous);
        request.dependency = Some("FFmpeg".into());
        assert!(service
            .plugin_documentation_url(&request)
            .unwrap_err()
            .contains("ambiguous"));
    }
    #[test]
    // Verify that catalog source precedence and bucket origin are explicit
    fn catalog_source_precedence_and_bucket_origin_are_explicit() {
        let source = "https://bucket.example/nested/plugins/catalog.json";
        assert_eq!(
            resolve_catalog_url(Some(source), Some("https://default.example/catalog.json"))
                .unwrap(),
            source
        );
        assert_eq!(resolve_catalog_url(Some(""), Some(source)).unwrap(), source);
        assert!(resolve_catalog_url(None, None)
            .unwrap_err()
            .contains("not configured"));
        for invalid in [
            "http://bucket.example/catalog.json",
            "https://user:token@bucket.example/catalog.json",
            "https://bucket.example/catalog.json#fragment",
            "https://bucket.example/catalog.json?token=secret",
        ] {
            assert!(resolve_catalog_url(Some(invalid), None).is_err());
        }
        let (mut plugin, _) = fixture("1.0.0");
        let artifact = &mut plugin.downloads[0];
        artifact.release_tag = format!("plugins-stable-local-{}", "a".repeat(20));
        artifact.url = format!(
            "https://bucket.example/nested/plugins/packages/stable/{}/{}/{}/{}",
            plugin.id, plugin.version, artifact.sha256, artifact.filename
        );
        let valid = catalog(plugin);
        valid
            .validate_source(&valid.repository, "stable", Some(source))
            .unwrap();
        let mut default_port = valid.clone();
        default_port.plugins[0].downloads[0].url = default_port.plugins[0].downloads[0]
            .url
            .replace("bucket.example", "bucket.example:443");
        default_port
            .validate_source(&valid.repository, "stable", Some(source))
            .unwrap();
        assert!(valid.validate(&valid.repository, "stable").is_err());
        for changed in [
            "https://other.example/nested/plugins/catalog.json",
            "https://bucket.example/other/catalog.json",
        ] {
            assert!(valid
                .validate_source(&valid.repository, "stable", Some(changed))
                .is_err());
        }
        let artifact = &valid.plugins[0].downloads[0];
        assert!(validate_plugin_origin(
            &artifact.url,
            &valid.repository,
            artifact,
            Some(source),
            false
        )
        .is_ok());
        for redirect in [
            "https://evil.example/file.zip",
            "https://bucket.example/nested/plugins-other/file.zip",
            "https://bucket.example/nested/plugins/%2e%2e/file.zip",
            "http://bucket.example/nested/plugins/file.zip",
        ] {
            assert!(validate_plugin_origin(
                redirect,
                &valid.repository,
                artifact,
                Some(source),
                true
            )
            .is_err());
        }
    }
    #[test]
    // Default to the public feed without falling back to a saved CDN after explicit GitHub selection
    fn implicit_source_defaults_and_explicit_choices_are_distinct() {
        let mut prefs = UpdatePreferences {
            plugins_repository: "StreamDockManager/Plugins".into(),
            ..Default::default()
        };
        assert_eq!(
            effective_catalog_url(&prefs).unwrap(),
            "https://api.github.com/repos/StreamDockManager/Plugins/releases"
        );
        prefs.plugins_catalog_url = Some("https://cdn.example/catalog.json".into());
        assert_eq!(prefs.catalog_source(), "hosted");
        prefs.plugins_catalog_source = Some("repository".into());
        assert_eq!(
            effective_catalog_url(&prefs).unwrap(),
            "https://api.github.com/repos/StreamDockManager/Plugins/releases"
        );
        prefs.plugins_repository.clear();
        assert!(effective_catalog_url(&prefs).is_err());
        prefs.plugins_catalog_source = Some("hosted".into());
        assert_eq!(
            effective_catalog_url(&prefs).unwrap(),
            "https://cdn.example/catalog.json"
        );
    }
    #[test]
    // Verify that repository mode uses release API and retains custom hosted URL
    fn repository_mode_uses_release_api_and_retains_custom_hosted_url() {
        let mut prefs = UpdatePreferences {
            plugins_catalog_source: Some("repository".into()),
            plugins_repository: "example/fork".into(),
            plugins_catalog_url: Some("https://bucket.example/catalog.json".into()),
            ..Default::default()
        };
        assert_eq!(
            effective_catalog_url(&prefs).unwrap(),
            "https://api.github.com/repos/example/fork/releases"
        );
        prefs.plugins_catalog_source = Some("hosted".into());
        assert_eq!(
            effective_catalog_url(&prefs).unwrap(),
            "https://bucket.example/catalog.json"
        );
    }
    #[test]
    // Verify that pending selections and persisted cache are bound to catalog source
    fn pending_selections_and_persisted_cache_are_bound_to_catalog_source() {
        let root = tempfile::tempdir().unwrap();
        let service = UpdateService::open(root.path().into()).unwrap();
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_source = Some("hosted".into());
        let source = "https://bucket.example/catalog.json";
        let (plugin, _) = fixture("1.0.0");
        let selection = CatalogSelection {
            repository: "StreamDockManager/Plugins".into(),
            catalog_url: Some(source.into()),
            github_asset_url: None,
            channel: "stable".into(),
            artifact: plugin.downloads[0].clone(),
            plugin: plugin.clone(),
        };
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_url = Some(source.into());
        service.selection_current(&selection).unwrap();
        {
            let mut store = service.store.lock().unwrap();
            store.catalogs.insert("stable".into(), catalog(plugin));
            store.catalog_source_url = Some("https://old.example/catalog.json".into());
        }
        service.persist().unwrap();
        let reopened = UpdateService::open_with_storage(
            root.path().into(),
            service.downloads.clone(),
            Arc::new(crate::credentials::OsCredentials),
            service.storage.clone(),
        )
        .unwrap();
        assert!(reopened.status().available_catalogs.is_empty());
        assert_eq!(
            reopened.status().preferences.plugins_catalog_url.as_deref(),
            Some(source)
        );
        service
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_url = Some("https://other.example/catalog.json".into());
        assert!(service.selection_current(&selection).is_err());
    }
    #[test]
    // Verify that repository catalog accepts only exact release assets and ignores opendeck
    fn repository_catalog_accepts_only_exact_release_assets_and_ignores_opendeck() {
        let (mut plugin, _) = fixture("1.0.0");
        let repository = catalog(plugin.clone()).repository;
        let artifact = &mut plugin.downloads[0];
        artifact.url = format!(
            "https://github.com/{repository}/releases/download/{}/{}",
            artifact.release_tag, artifact.filename
        );
        let mut plugin = with_unified(plugin);
        let unified = plugin.unified_download.as_mut().unwrap();
        unified.url = format!(
            "https://github.com/{repository}/releases/download/{}/{}",
            unified.release_tag, unified.filename
        );
        let valid = catalog(plugin);
        valid.validate(&repository, "stable").unwrap();
        valid
            .validate_source(
                &repository,
                "stable",
                Some(&format!(
                    "https://api.github.com/repos/{repository}/releases"
                )),
            )
            .unwrap();
        assert!(valid
            .validate_source(
                &repository,
                "stable",
                Some("https://catalog.example/catalog.json")
            )
            .is_err());
        let original = valid.plugins[0].downloads[0].url.clone();
        for bad_url in [
            original.replace(&repository, "attacker/plugins"),
            original.replace("plugins-stable-", "plugins-development-"),
            original.replace("github.com", "github.com.evil.example"),
            original.replace("github.com", "github.com:443"),
            original.replace("github.com", "user@github.com"),
            original.replace("/releases/", "/x/../releases/"),
            original.replace("/releases/", "/%72eleases/"),
            format!("{original}?download=1"),
            format!("{original}#fragment"),
            "https://release-assets.githubusercontent.com/file.zip".into(),
        ] {
            let mut bad = valid.clone();
            bad.plugins[0].downloads[0].url = bad_url;
            assert!(bad.validate(&repository, "stable").is_err());
        }
        assert!(valid
            .validate_source(
                &repository,
                "stable",
                Some("https://api.github.com/repos/attacker/plugins/releases")
            )
            .is_err());
        let mut json = serde_json::to_value(&valid).unwrap();
        json["plugins"][0]["opendeck_download"] =
            serde_json::json!({"url": "https://example.com/oa.zip"});
        let parsed: PluginCatalog = serde_json::from_value(json).unwrap();
        parsed.validate(&repository, "stable").unwrap();
        assert_eq!(parsed.plugins[0].downloads.len(), 1);
        assert!(!parsed.plugins[0].downloads[0].vsd_craft_compatible);
        // Catalogs from third-party GitHub repositories use the same contract;
        // accepting one repository must never authorize a different repository
        let mut third_party = valid.clone();
        third_party.repository = "third-party/private-plugins".into();
        for plugin in &mut third_party.plugins {
            for artifact in &mut plugin.downloads {
                artifact.url = artifact.url.replace(&repository, &third_party.repository);
            }
            if let Some(artifact) = &mut plugin.unified_download {
                artifact.url = artifact.url.replace(&repository, &third_party.repository);
            }
        }
        third_party
            .validate(&third_party.repository, "stable")
            .unwrap();
        assert!(third_party.validate(&repository, "stable").is_err());
    }

    #[test]
    // Verify that initial contract rejects evidence origin and inventory mismatches
    fn initial_contract_rejects_evidence_origin_and_inventory_mismatches() {
        let (plugin, _) = fixture("1.0.0");
        let valid = catalog(plugin);
        valid
            .validate_source(
                &valid.repository,
                "stable",
                Some("https://catalog.example/catalog.json"),
            )
            .unwrap();
        let mut old = valid.clone();
        let artifact = &mut old.plugins[0].downloads[0];
        artifact.url = format!(
            "https://github.com/{}/releases/download/{}/{}",
            old.repository, artifact.release_tag, artifact.filename
        );
        assert!(old
            .validate_source(
                &valid.repository,
                "stable",
                Some("https://catalog.example/catalog.json")
            )
            .is_err());
        let mut wrong = valid.clone();
        wrong.plugins[0].downloads[0].url = "https://evil.example/download".into();
        assert!(wrong
            .validate_source(
                &valid.repository,
                "stable",
                Some("https://catalog.example/catalog.json")
            )
            .is_err());
        let mut wrong = valid.clone();
        wrong.plugins[0].downloads[0].compatibility.status = "verified".into();
        wrong.plugins[0].downloads[0].vsd_craft_compatible = true;
        assert!(wrong
            .validate_source(
                &valid.repository,
                "stable",
                Some("https://catalog.example/catalog.json")
            )
            .is_err());
        let mut wrong = valid.clone();
        wrong.plugins[0].downloads[0]
            .files
            .insert("../escape".into(), "a".repeat(64));
        assert!(wrong
            .validate_source(
                &valid.repository,
                "stable",
                Some("https://catalog.example/catalog.json")
            )
            .is_err());
    }
    #[test]
    // Verify that supplied qualification evidence is bound to every status
    fn supplied_qualification_evidence_is_bound_to_every_status() {
        let (plugin, _) = fixture("1.0.0");
        let original = catalog(plugin);
        for status in ["verified", "incompatible", "unverified"] {
            let mut valid = original.clone();
            let plugin = &mut valid.plugins[0];
            let artifact = &mut plugin.downloads[0];
            artifact.compatibility.status = status.into();
            artifact.vsd_craft_compatible = status == "verified";
            artifact.compatibility.evidence = Some(QualificationEvidence {
                plugin_version: plugin.version.clone(),
                os: artifact.os.clone(),
                sha256: artifact.sha256.clone(),
                url: "https://example.com/qualification".into(),
            });
            valid
                .validate_source(
                    &valid.repository,
                    "stable",
                    Some("https://catalog.example/catalog.json"),
                )
                .unwrap();
            for field in ["version", "platform", "hash", "url"] {
                let mut wrong = valid.clone();
                let evidence = wrong.plugins[0].downloads[0]
                    .compatibility
                    .evidence
                    .as_mut()
                    .unwrap();
                match field {
                    "version" => evidence.plugin_version = "2.0.0".into(),
                    "platform" => evidence.os = "another-platform".into(),
                    "hash" => evidence.sha256 = "0".repeat(64),
                    "url" => evidence.url = "https://".into(),
                    _ => unreachable!(),
                }
                assert!(
                    wrong
                        .validate_source(
                            &wrong.repository,
                            "stable",
                            Some("https://catalog.example/catalog.json")
                        )
                        .is_err(),
                    "accepted mismatched {field} evidence for {status}"
                );
            }
        }
    }
    #[test]
    #[ignore = "Requires loopback for the isolated plugin host; never executes a plugin or opens HID"]
    // Verify that individual reviews install preserve provenance and ignore unrelated batches
    fn individual_reviews_install_preserve_provenance_and_ignore_unrelated_batches() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(temporary.path().join("app"), false).unwrap();
        runtime
            .updates
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_source = Some("hosted".into());
        runtime
            .updates
            .store
            .lock()
            .unwrap()
            .preferences
            .plugins_catalog_url = Some("https://catalog.example/catalog.json".into());
        for version in ["1.0.0", "1.1.0"] {
            let (mut plugin, bytes) = fixture(version);
            let source = if version == "1.1.0" {
                let url = "https://bucket.example/plugins/catalog.json";
                runtime
                    .updates
                    .store
                    .lock()
                    .unwrap()
                    .preferences
                    .plugins_catalog_url = Some(url.into());
                let artifact = &mut plugin.downloads[0];
                artifact.url = format!(
                    "https://bucket.example/plugins/packages/stable/{}/{}/{}/{}",
                    plugin.id, plugin.version, artifact.sha256, artifact.filename
                );
                let mut store = runtime.updates.store.lock().unwrap();
                store
                    .catalogs
                    .insert("stable".into(), catalog(plugin.clone()));
                drop(store);
                assert_eq!(runtime.update_status().plugin_updates.len(), 1);
                Some(url.into())
            } else {
                effective_catalog_url(&runtime.updates.status().preferences).ok()
            };
            let selected = CatalogSelection {
                catalog_url: source,
                github_asset_url: None,
                repository: "StreamDockManager/Plugins".into(),
                channel: "stable".into(),
                artifact: plugin.downloads[0].clone(),
                plugin: plugin.clone(),
            };
            let (id, cancel) = runtime.updates.job("plugins", "download").unwrap();
            let dir = runtime
                .updates
                .root
                .join("downloads/plugins")
                .join(&selected.artifact.sha256);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(&selected.artifact.filename);
            std::fs::write(&path, bytes).unwrap();
            runtime
                .updates
                .selections
                .lock()
                .unwrap()
                .insert(id.clone(), Arc::new(selected.clone()));
            runtime
                .updates
                .finish(&id, Ok(Some(path.to_string_lossy().into())));
            let _ = cancel;
            let review = runtime.prepare_catalog_download(&id).unwrap();
            assert!(review.trusted);
            assert!(review.compatible);
            runtime
                .commit_plugin_package(&review.id, false, false)
                .unwrap();
            let host = runtime.plugins.lock().unwrap();
            let installed = host.list().into_iter().find(|p| p.id == plugin.id).unwrap();
            assert_eq!(installed.version, version);
            assert!(!installed.enabled);
            assert_eq!(
                host.provenance(&plugin.id).unwrap()["release_tag"],
                selected.artifact.release_tag
            );
            assert_eq!(
                host.provenance(&plugin.id).unwrap()["artifact"]["source_commit"],
                selected.artifact.source_commit
            );
            assert_eq!(
                host.provenance(&plugin.id).unwrap()["artifact"]["url"],
                selected.artifact.url
            );
            drop(host);
            runtime
                .updates
                .store
                .lock()
                .unwrap()
                .catalogs
                .insert("stable".into(), catalog(plugin));
            assert!(runtime.update_status().plugin_updates.is_empty());
        }
        runtime.shutdown();
    }
    // Cache fixture
    fn cache_fixture(runtime: &crate::Runtime, version: &str) -> (CatalogPlugin, String) {
        let (plugin, bytes) = fixture(version);
        let selection = CatalogSelection {
            catalog_url: effective_catalog_url(&runtime.updates.status().preferences).ok(),
            github_asset_url: None,
            repository: "StreamDockManager/Plugins".into(),
            channel: "stable".into(),
            artifact: plugin.downloads[0].clone(),
            plugin: plugin.clone(),
        };
        let (id, _) = runtime.updates.job("plugins", "download").unwrap();
        let directory = runtime
            .updates
            .root
            .join("downloads/plugins")
            .join(&selection.artifact.sha256);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(&selection.artifact.filename);
        std::fs::write(&path, bytes).unwrap();
        runtime
            .updates
            .selections
            .lock()
            .unwrap()
            .insert(id.clone(), Arc::new(selection));
        runtime
            .updates
            .finish(&id, Ok(Some(path.to_string_lossy().into())));
        (plugin, id)
    }
    #[test]
    #[ignore = "Requires a disposable loopback host; metadata and local policy only"]
    // Verify that source bound holds and skips suppress suggestions but leave explicit install available
    fn source_bound_holds_and_skips_suppress_suggestions_but_leave_explicit_install_available() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let (plugin, job) = cache_fixture(&runtime, "1.0.0");
        let review = runtime.prepare_catalog_download(&job).unwrap();
        runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        let (next, job) = cache_fixture(&runtime, "1.1.0");
        runtime
            .updates
            .store
            .lock()
            .unwrap()
            .catalogs
            .insert("stable".into(), catalog(next));
        assert_eq!(runtime.update_status().plugin_updates.len(), 1);
        let mut policy = runtime.plugin_update_policy(&plugin.id).unwrap();
        assert!(policy.managed);
        policy.held = true;
        runtime.save_plugin_update_policy(policy.clone()).unwrap();
        assert!(runtime.update_status().plugin_updates.is_empty());
        let review = runtime.prepare_catalog_download(&job).unwrap();
        assert!(review.can_import); // Explicit catalog installation remains available
        policy.held = false;
        policy.skipped_versions = vec!["1.1.0".into()];
        runtime.save_plugin_update_policy(policy.clone()).unwrap();
        assert!(runtime.update_status().plugin_updates.is_empty());
        policy.skipped_versions = vec!["1.2.0".into()];
        runtime.save_plugin_update_policy(policy.clone()).unwrap();
        assert_eq!(runtime.update_status().plugin_updates.len(), 1);
        runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        let current = runtime.plugin_update_policy(&plugin.id).unwrap();
        assert_ne!(current.installation, policy.installation);
        assert!(!current.held && current.skipped_versions.is_empty());
        let before =
            std::fs::read(runtime.profiles.root.join("plugin-update-policies.json")).unwrap();
        assert!(runtime.save_plugin_update_policy(policy).is_err());
        assert_eq!(
            std::fs::read(runtime.profiles.root.join("plugin-update-policies.json")).unwrap(),
            before
        );
        let (next, _) = cache_fixture(&runtime, "1.2.0");
        runtime
            .updates
            .store
            .lock()
            .unwrap()
            .catalogs
            .insert("stable".into(), catalog(next));
        assert_eq!(runtime.update_status().plugin_updates.len(), 1);
        std::fs::write(
            runtime.profiles.root.join("plugin-update-policies.json"),
            b"corrupt original",
        )
        .unwrap();
        assert!(runtime.update_status().plugin_policy_error.is_some());
        assert!(runtime.save_plugin_update_policy(current).is_err());
        assert_eq!(
            std::fs::read(runtime.profiles.root.join("plugin-update-policies.json")).unwrap(),
            b"corrupt original"
        );
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires a disposable loopback host; trusted inventory fixtures are not published-package qualification"]
    // Verify that recorded catalog backup restores exact origin and remains disabled
    fn recorded_catalog_backup_restores_exact_origin_and_remains_disabled() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let (plugin, job) = cache_fixture(&runtime, "1.0.0");
        let review = runtime.prepare_catalog_download(&job).unwrap();
        runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        let (_, job) = cache_fixture(&runtime, "1.1.0");
        let review = runtime.prepare_catalog_download(&job).unwrap();
        runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        let backups = runtime.plugin_backups(&plugin.id, 1).unwrap();
        assert_eq!(backups.total, 1);
        assert!(backups.entries[0].trusted_origin);
        let review = runtime
            .prepare_plugin_rollback(&plugin.id, &backups.entries[0].id)
            .unwrap();
        assert!(review.can_restore);
        assert!(runtime
            .commit_plugin_rollback(&review.id, false, false)
            .is_err());
        let snapshot = runtime
            .commit_plugin_rollback(&review.id, true, false)
            .unwrap();
        let restored = snapshot.plugins.iter().find(|p| p.id == plugin.id).unwrap();
        assert_eq!(restored.version, "1.0.0");
        assert!(!restored.enabled);
        assert_eq!(
            runtime
                .plugins
                .lock()
                .unwrap()
                .provenance(&plugin.id)
                .unwrap()["release_tag"],
            plugin.downloads[0].release_tag
        );
        assert!(runtime.plugin_update_policy(&plugin.id).unwrap().managed);
        assert_eq!(runtime.plugin_backups(&plugin.id, 1).unwrap().total, 2);
        runtime.shutdown();
    }
}
