//! Individual package reviews share validated staging and recoverable replacement
use crate::updates::{compatible, stable_version, verify_file, CatalogSelection, PackageCandidate};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use stream_dock_manager_plugin_host::{PluginHost, PreparedPlugin};

#[derive(Serialize, ts_rs::TS)]
pub struct PluginReloadResult {
    pub snapshot: crate::Snapshot,
    pub added: usize,
    pub removed: usize,
}

#[derive(Serialize, ts_rs::TS)]
pub struct PluginSelectionFailure {
    pub source: String,
    pub status: String,
    pub message: String,
}

#[derive(Serialize, ts_rs::TS)]
pub struct PluginSelectionReview {
    pub reviews: Vec<PluginPackageReview>,
    pub results: Vec<PluginSelectionFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginPackageReview {
    #[serde(default)]
    #[ts(optional)]
    pub suggested_profiles: Option<crate::plugin_profiles::PluginProfileCatalog>,
    pub id: String,
    pub trusted: bool,
    pub package: PackageCandidate,
    pub installed_version: Option<String>,
    pub channel: String,
    pub channel_change: bool,
    pub compatible: bool,
    pub reason: Option<String>,
    pub manifest: stream_dock_manager_plugin_host::manifest::NormalizedManifest,
    pub source: stream_dock_manager_plugin_host::PackageSource,
    pub compatibility: stream_dock_manager_plugin_host::preflight::PluginPreflight,
    pub can_import: bool,
    pub can_run: bool,
    pub version_change: Option<String>,
}
pub struct StagedPackage {
    pub review: PluginPackageReview,
    prepared: Option<PreparedPlugin>,
    selection: Option<CatalogSelection>,
    created: std::time::Instant,
}
impl crate::review_store::ReviewSession for StagedPackage {
    const LIMIT: usize = 4;
    const FULL: &'static str = "Close an existing package review first";
    const LIFETIME: Option<std::time::Duration> = None;
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
impl crate::Runtime {
    // Prepare plugin package
    pub fn prepare_plugin_package(&self, path: &Path) -> Result<PluginPackageReview, String> {
        let _source_lease = self.updates.cache_lease(path)?;
        let root = self.plugins.lock().unwrap().installation_root();
        let prepared = PluginHost::prepare_install(&root, path)?;
        let info = PluginHost::prepared_info(&prepared)?;
        let descriptor = info.capabilities.as_ref();
        let source = PluginHost::prepared_source(&prepared);
        let package = PackageCandidate {
            files: PluginHost::prepared_inventory(&prepared),
            os: if cfg!(target_os = "macos") {
                "macos"
            } else {
                std::env::consts::OS
            }
            .into(),
            id: info.id,
            version: info.version,
            path: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            sha256: source.sha256.clone(),
            size: path.metadata().map_err(|e| e.to_string())?.len(),
            architectures: vec![if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "amd64"
            }
            .into()],
            minimum_manager: descriptor
                .map(|c| c.minimum_manager.clone())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "0.1.0".into()),
            required_capabilities: descriptor
                .map(|c| c.required_capabilities.clone())
                .unwrap_or_default(),
        };
        self.stage_package(prepared, package, None)
    }
    // Prepare catalog download
    pub fn prepare_catalog_download(&self, job: &str) -> Result<PluginPackageReview, String> {
        let selection = self
            .updates
            .downloaded_selection(job)
            .ok_or("Download has no immutable package selection")?;
        self.updates.selection_current(&selection)?;
        let package = selection.package();
        compatible(&package)?;
        let (path, _source_lease) = self.updates.leased_completed_download(job)?;
        verify_file(&path, &package.sha256, package.size)?;
        let root = self.plugins.lock().unwrap().installation_root();
        let prepared = PluginHost::prepare_install(&root, &path)?;
        PluginHost::verify_prepared_files(&prepared, &package.files)?;
        let info = PluginHost::prepared_info(&prepared)?;
        if info.id != package.id
            || info.version != package.version
            || info.capabilities.as_ref() != Some(&selection.plugin.capabilities)
        {
            return Err("Manifest identity/version/capabilities differ from catalog".into());
        }
        self.stage_package(prepared, package, Some(selection))
    }
    // Stage package
    fn stage_package(
        &self,
        prepared: PreparedPlugin,
        package: PackageCandidate,
        selection: Option<CatalogSelection>,
    ) -> Result<PluginPackageReview, String> {
        let host = self.plugins.lock().unwrap();
        let installed = host.list().into_iter().find(|p| p.id == package.id);
        let provenance = host.provenance(&package.id);
        let channel = selection
            .as_ref()
            .map(|s| s.channel.clone())
            .unwrap_or_else(|| "local".into());
        let channel_change =
            selection.is_some() && provenance.as_ref().is_some_and(|p| p["channel"] != channel);
        let compatibility = stream_dock_manager_plugin_host::preflight::resolve_runtime(
            PluginHost::prepared_preflight(&prepared),
            &self.runtimes,
        );
        let mut reason = if selection.is_some() {
            compatible(&package).err()
        } else if !compatibility.can_install {
            Some("Package cannot be imported".into())
        } else {
            None
        };
        if let (Some(old), Some(selection)) = (&installed, &selection) {
            if provenance
                .as_ref()
                .is_none_or(|p| p["repository"] != selection.repository)
            {
                reason = Some("Installed package is manually managed. Import a local ZIP to replace it explicitly.".into());
            } else if stable_version(&package.version)? < stable_version(&old.version)?
                || package.version == old.version && !channel_change
            {
                reason = Some("Already installed or downgrade blocked".into());
            }
        }
        drop(host);
        let version_change = installed
            .as_ref()
            .and_then(|old| {
                if old.dialect == "openaction" {
                    semver::Version::parse(&package.version)
                        .ok()
                        .zip(semver::Version::parse(&old.version).ok())
                        .map(|(new, old)| new.cmp(&old))
                } else {
                    stream_dock_manager_plugin_host::manifest::compare_vendor_versions(
                        &package.version,
                        &old.version,
                    )
                    .ok()
                }
            })
            .map(|order| {
                match order {
                    std::cmp::Ordering::Less => "downgrade",
                    std::cmp::Ordering::Equal => "same_version",
                    std::cmp::Ordering::Greater => "upgrade",
                }
                .into()
            });
        let review = PluginPackageReview {
            suggested_profiles: Some(crate::plugin_profiles::discover(
                PluginHost::prepared_directory(&prepared),
                &PluginHost::prepared_info(&prepared)?,
            )),
            manifest: PluginHost::prepared_manifest(&prepared),
            source: PluginHost::prepared_source(&prepared),
            can_import: reason.is_none(),
            can_run: compatibility.can_enable,
            compatibility,
            version_change,
            id: crate::review_store::new_id(),
            trusted: selection.is_some(),
            package,
            installed_version: installed.map(|p| p.version),
            channel,
            channel_change,
            compatible: reason.is_none(),
            reason,
        };
        let mut reviews = self.plugin_packages.lock().unwrap();
        reviews.check_capacity()?;
        reviews.insert(
            review.id.clone(),
            StagedPackage {
                review: review.clone(),
                prepared: Some(prepared),
                selection,
                created: std::time::Instant::now(),
            },
        )?;
        Ok(review)
    }
    // Cancel plugin package
    pub fn cancel_plugin_package(&self, id: &str) {
        self.plugin_packages.lock().unwrap().remove(id);
    }
    // Commit plugin package
    pub fn commit_plugin_package(
        &self,
        id: &str,
        allow_channel_change: bool,
        replace: bool,
    ) -> Result<crate::Snapshot, String> {
        let mut reviews = self.plugin_packages.lock().unwrap();
        let stage = reviews.get_mut(id).ok_or("Package review expired")?;
        let review = &stage.review;
        if !review.compatible {
            return Err(review
                .reason
                .clone()
                .unwrap_or_else(|| "Incompatible package".into()));
        }
        if stage.selection.is_some() {
            compatible(&review.package)?;
        }
        let mut host = self.plugins.lock().unwrap();
        let old = host.list().into_iter().find(|p| p.id == review.package.id);
        if let Some(selection) = &stage.selection {
            self.updates.selection_current(selection)?;
            if let Some(old) = &old {
                let provenance = host
                    .provenance(&old.id)
                    .ok_or("Installed package is manually managed")?;
                if provenance["repository"] != selection.repository {
                    return Err("Installed package origin differs".into());
                }
                if provenance["channel"] != selection.channel && !allow_channel_change {
                    return Err("Acknowledge the channel change".into());
                }
                if stable_version(&review.package.version)? < stable_version(&old.version)?
                    || review.package.version == old.version
                        && provenance["channel"] == selection.channel
                {
                    return Err("Already installed or downgrade blocked".into());
                }
            }
        } else if old.is_some() && !replace {
            return Err("Confirm replacing the installed package".into());
        }
        if let Some(prepared) = &stage.prepared {
            PluginHost::verify_prepared_files(prepared, &review.package.files)?;
        }
        let replacement = old
            .as_ref()
            .map(|old| host.reserve_replacement(&old.id))
            .transpose()?;
        // Waiting belongs outside the host mutex: input and other plugins keep working
        let waiter = if let Some(old) = &old {
            host.stop_for_replacement(&old.id)?
        } else {
            None
        };
        drop(host);
        if old.is_some() {
            self.emit("plugins", None, None, None);
        }
        if let Some(waiter) = waiter {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !waiter.load(std::sync::atomic::Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Err(
                        "Plugin process cleanup did not finish; review replacement again".into(),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        let mut host = self.plugins.lock().unwrap();
        let mut prepared = stage
            .prepared
            .take()
            .ok_or("Prepare the package again before retrying")?;
        if let Some(selection) = &stage.selection {
            self.updates.selection_current(selection)?;
            PluginHost::write_prepared_metadata(
                &mut prepared,
                ".stream-dock-manager-provenance.json",
                &serde_json::json!({"catalog_url":selection.catalog_url,"repository":selection.repository,"release_tag":selection.artifact.release_tag,"channel":selection.channel,"version":selection.plugin.version,"build":selection.artifact.release_tag,"package_hash":selection.artifact.sha256,"artifact":selection.artifact}),
            )?;
        }
        if stage.selection.is_none() {
            PluginHost::write_prepared_metadata(
                &mut prepared,
                ".stream-dock-manager-provenance.json",
                &serde_json::json!({"source":review.source,"dialect":review.manifest.dialect,"version":review.manifest.version,"trust":"unverified_local"}),
            )?;
        }
        let installed_id = review.package.id.clone();
        let result = host.replace_prepared(prepared);
        drop(host);
        reviews.remove(id);
        drop(reviews);
        drop(replacement);
        let snapshot = self.plugins_changed();
        if result.is_ok() && old.is_none() {
            self.clear_plugin_profile_acknowledgement(&installed_id)?;
        }
        result.map(|_| snapshot)
    }
}

/// Explicit local development installs preserve enablement without starting plugins
pub fn install_local_packages(
    config_root: &Path,
    paths: Vec<PathBuf>,
) -> Result<Vec<stream_dock_manager_plugin_host::PluginInfo>, String> {
    let _writer = crate::ProfileStore::open(config_root.join("profiles"))?;
    PluginHost::install_local_archives_with_storage(
        config_root.join("plugins"),
        paths,
        _writer.storage().clone(),
    )
}

/// Remove selected local packages without starting plugins or opening devices
pub fn uninstall_local_packages(
    config_root: &Path,
    ids: Vec<String>,
) -> Result<Vec<String>, String> {
    let _writer = crate::ProfileStore::open(config_root.join("profiles"))?;
    PluginHost::uninstall_local_packages_with_storage(
        config_root.join("plugins"),
        ids,
        _writer.storage().clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use stream_dock_manager_plugin_host::RuntimeState;
    #[test]
    // Verify that offline uninstall obeys writer lock and preserves profiles
    fn offline_uninstall_obeys_writer_lock_and_preserves_profiles() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("manager");
        let source = vendor(dir.path(), "0.1.0");
        install_local_packages(&config, vec![source.clone()]).unwrap();
        let store = crate::ProfileStore::open(config.join("profiles")).unwrap();
        store.load().unwrap();
        let profiles = serde_json::to_value(store.load().unwrap()).unwrap();
        let ids = vec!["org.vendor.fixture".into()];
        assert!(uninstall_local_packages(&config, ids.clone())
            .unwrap_err()
            .contains("Another Stream Dock Manager process"));
        assert!(config
            .join("plugins/org.vendor.fixture/manifest.json")
            .is_file());
        drop(store);
        assert_eq!(
            uninstall_local_packages(&config, ids).unwrap(),
            ["org.vendor.fixture"]
        );
        // The temporary uninstall host may still be retiring workers holding its
        // storage Arc. Wait only for that specific lock, without weakening the
        // assertion that an actively held writer prevents uninstall above
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let restored = loop {
            match crate::ProfileStore::open(config.join("profiles")) {
                Ok(store) => break store,
                Err(error)
                    if error.contains("Another Stream Dock Manager process")
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("Could not reopen after uninstall: {error}"),
            }
        };
        assert_eq!(
            serde_json::to_value(restored.load().unwrap()).unwrap(),
            profiles
        );
        assert!(source.join("manifest.json").is_file());
        assert!(!config.join("plugins/org.vendor.fixture").exists());
    }
    #[test]
    #[ignore = "requires explicitly downloaded pinned plugin samples"]
    // Verify that pinned vendor packages use review commit cancel without execution
    fn pinned_vendor_packages_use_review_commit_cancel_without_execution() {
        let manager = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let sources: serde_json::Value = serde_json::from_slice(
            &std::fs::read(manager.join("tests/fixtures/plugins/sources.json")).unwrap(),
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("manager"), false).unwrap();
        for source in sources["sources"].as_array().unwrap() {
            let path = manager
                .join("artifacts/plugin-samples")
                .join(source["id"].as_str().unwrap())
                .join(
                    source["archive"]
                        .as_str()
                        .or(source["package_path"].as_str())
                        .unwrap(),
                );
            let cancelled = runtime.prepare_plugin_package(&path).unwrap();
            assert!(cancelled.can_import && !cancelled.trusted);
            runtime.cancel_plugin_package(&cancelled.id);
            assert!(runtime
                .commit_plugin_package(&cancelled.id, false, false)
                .is_err());
            let review = runtime.prepare_plugin_package(&path).unwrap();
            assert_eq!(review.source.sha256, cancelled.source.sha256);
            let installed = runtime
                .commit_plugin_package(&review.id, false, false)
                .unwrap();
            let plugin = installed
                .plugins
                .iter()
                .find(|p| p.id == review.manifest.id)
                .unwrap();
            assert_eq!(plugin.version, review.manifest.version);
            assert!(!plugin.enabled);
            assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
            if source["id"] == "elgato-layouts" {
                assert_eq!(plugin.version, "0.1.0.0");
                assert!(!review.can_run);
                assert_eq!(
                    review
                        .compatibility
                        .runtime
                        .unwrap()
                        .requirement
                        .version
                        .as_deref(),
                    Some(">=22, <27")
                );
            }
        }
        runtime.shutdown();
    }
    // Write a vendor browser package fixture with the requested version
    fn vendor(root: &Path, version: &str) -> PathBuf {
        let source = root.join("org.vendor.fixture.sdPlugin");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("index.html"), "reviewed bytes").unwrap();
        crate::test_plugin::write_manifest(
            &source,
            json!({"UUID":"org.vendor.fixture","Name":"Vendor fixture","Author":"Vendor","Version":version,"SDKVersion":1,"CodePath":"index.html","UnknownOptional":{"keep":true},"Actions":[{"UUID":"org.vendor.fixture.action","Name":"Action","Controllers":["Keypad"]}]}),
        );
        source
    }
    #[test]
    // Verify that review commit cancel and replacement use exact staged bytes and preserve preferences
    fn review_commit_cancel_and_replacement_use_exact_staged_bytes_and_preserve_preferences() {
        let root = tempfile::tempdir().unwrap();
        let source = vendor(root.path(), "0.1.0.0");
        let manifest = std::fs::read(source.join("manifest.json")).unwrap();
        let original_entry = std::fs::read_to_string(source.join("index.html")).unwrap();
        let runtime = crate::Runtime::build(root.path().join("manager"), false).unwrap();
        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert!(review.can_import && review.can_run && !review.trusted);
        assert_eq!(review.manifest.version, "0.1.0.0");
        assert_eq!(review.manifest.publisher.as_deref(), Some("Vendor"));
        assert_eq!(review.package.files.len(), 3);
        assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
        std::fs::write(source.join("index.html"), "source changed after review").unwrap();
        let installed = runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        assert!(!installed.plugins[0].enabled);
        let target = runtime
            .plugins
            .lock()
            .unwrap()
            .installation_root()
            .join("org.vendor.fixture");
        assert_eq!(
            std::fs::read(target.join("manifest.json")).unwrap(),
            manifest
        );
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).unwrap(),
            original_entry
        );
        let origin = runtime
            .plugins
            .lock()
            .unwrap()
            .provenance("org.vendor.fixture")
            .unwrap();
        assert!(origin.get("repository").is_none());
        assert_eq!(origin["trust"], "unverified_local");
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable("org.vendor.fixture", true)
            .unwrap();
        assert_eq!(
            runtime.snapshot().plugins[0].runtime.state,
            RuntimeState::Idle
        );
        vendor(root.path(), "1.4.1");
        let replacement_entry = std::fs::read_to_string(source.join("index.html")).unwrap();
        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert_eq!(review.installed_version.as_deref(), Some("0.1.0.0"));
        assert_eq!(review.version_change.as_deref(), Some("upgrade"));
        assert!(runtime
            .commit_plugin_package(&review.id, false, false)
            .is_err());
        let snapshot = runtime
            .commit_plugin_package(&review.id, false, true)
            .unwrap();
        assert!(snapshot.plugins[0].enabled);
        assert_eq!(snapshot.plugins[0].runtime.state, RuntimeState::Idle);
        let review = runtime.prepare_plugin_package(&source).unwrap();
        runtime.cancel_plugin_package(&review.id);
        assert!(runtime
            .commit_plugin_package(&review.id, false, true)
            .is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).unwrap(),
            replacement_entry
        );
        assert!(
            crate::updates::stable_version("0.1.0.0").is_err(),
            "official catalog SemVer stays strict"
        );
        runtime.shutdown();
    }
    #[test]
    // Verify that changed staging fails without replacement and missing runtime is importable
    fn changed_staging_fails_without_replacement_and_missing_runtime_is_importable() {
        let root = tempfile::tempdir().unwrap();
        let source = vendor(root.path(), "1.0.0");
        let runtime = crate::Runtime::build(root.path().join("manager"), false).unwrap();
        let review = runtime.prepare_plugin_package(&source).unwrap();
        let plugin_root = runtime.plugins.lock().unwrap().installation_root();
        let staging = std::fs::read_dir(&plugin_root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".install-")
            })
            .unwrap();
        std::fs::write(
            staging.join("unpacked/org.vendor.fixture.sdPlugin/index.html"),
            "tampered",
        )
        .unwrap();
        assert!(runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap_err()
            .contains("hash mismatch"));
        assert!(runtime.snapshot().plugins.is_empty());
        runtime.cancel_plugin_package(&review.id);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(source.join("manifest.json")).unwrap()).unwrap();
        manifest["CodePath"] = json!("main.js");
        manifest["StreamDockManager"]["Runtime"] =
            json!({"Kind":"nodejs","Version":">=22, <27","Channel":"lts"});
        std::fs::write(source.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(
            source.join("main.js"),
            "throw Error('import must never execute')",
        )
        .unwrap();
        crate::test_plugin::write_manifest(&source, manifest);
        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert!(review.can_import && !review.can_run);
        assert_eq!(
            review
                .compatibility
                .runtime
                .unwrap()
                .requirement
                .version
                .as_deref(),
            Some(">=22, <27")
        );
        let snapshot = runtime
            .commit_plugin_package(&review.id, false, false)
            .unwrap();
        assert!(!snapshot.plugins[0].enabled);
        assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
        runtime.shutdown();
    }
}
