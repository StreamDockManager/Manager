use super::*;
use std::collections::{BTreeMap, HashSet};

pub struct LocalPackage {
    pub archive: PathBuf,
    pub id: String,
    pub version: String,
    pub files: BTreeMap<String, String>,
}

impl PluginHost {
    // Offline maintenance shares validation and persistence without creating a listener
    fn open_local(
        root: PathBuf,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Self, String> {
        if root.is_symlink() {
            return Err("Plugin directory is a symlink".into());
        }
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        storage.run(|connection| validate_storage_snapshot(connection))?;
        storage.add_snapshot_validator(validate_storage_snapshot)?;
        deletion::recover(&root, &storage)?;
        transactions::recover(&root, &storage)?;
        let logs = log_capture::LogCaptureService::with_storage(storage.clone())?;
        let mut host = Self {
            recovery: recovery::Recovery::open(storage.clone())?,
            transport_threads: vec![],
            logs,
            runtimes: stream_dock_manager_plugin_runtimes::RuntimeService::open_shared(
                root.parent().unwrap_or(&root).join("runtimes"),
                storage.clone(),
            )?,
            startup: Default::default(),
            visible_plugins: Default::default(),
            starting: Default::default(),
            next_start: std::time::Instant::now(),
            hub: Arc::new(Mutex::new(Hub::load(&storage)?)),
            root,
            plugins: HashMap::new(),
            stop: Arc::new(AtomicBool::new(true)),
            port: 0,
            theme_revision: 0,
            required: Default::default(),
            replacement_claims: Default::default(),
            last_health_flush: std::time::Instant::now(),
        };
        host.scan(true)?;
        Ok(host)
    }

    /// Remove explicit installed IDs, leaving absent packages and unrelated plugins alone
    /// The caller must hold the Manager configuration's exclusive writer lock
    pub fn uninstall_local_packages(
        root: PathBuf,
        ids: Vec<String>,
    ) -> Result<Vec<String>, String> {
        let storage = stream_dock_manager_storage::Storage::open_validated(
            root.parent()
                .ok_or("Plugin directory has no configuration root")?
                .to_path_buf(),
            super::validate_storage_snapshot,
        )?;
        Self::uninstall_local_packages_with_storage(root, ids, storage)
    }
    // Uninstall local packages with storage
    pub fn uninstall_local_packages_with_storage(
        root: PathBuf,
        ids: Vec<String>,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Vec<String>, String> {
        let mut seen = HashSet::new();
        if ids.is_empty()
            || ids.len() > 256
            || ids
                .iter()
                .any(|id| !safe_id(id) || !seen.insert(id.clone()))
        {
            return Err("Invalid or duplicate local plugin selection".into());
        }
        if root.is_symlink() {
            return Err("Plugin directory is a symlink".into());
        }
        if !root.try_exists().map_err(|e| e.to_string())? {
            return Ok(Vec::new());
        }
        let mut host = Self::open_local(root, storage)?;
        let selected: Vec<_> = ids
            .into_iter()
            .filter(|id| host.plugins.contains_key(id))
            .collect();
        // Preflight every selected path before removing any package or settings
        for id in &selected {
            host.deletion_path(id)?;
        }
        for id in &selected {
            host.delete(id)?;
        }
        Ok(selected)
    }

    // Install local archives with storage
    pub fn install_local_archives_with_storage(
        root: PathBuf,
        archives: Vec<PathBuf>,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Vec<PluginInfo>, String> {
        if archives.is_empty() || archives.len() > 256 || root.is_symlink() {
            return Err("Invalid local package selection or linked plugin directory".into());
        }
        let mut host = Self::open_local(root, storage)?;
        let mut ids = HashSet::new();
        let mut prepared = Vec::new();
        for archive in archives {
            let report = preflight::inspect_source(&archive)?;
            if !report.can_install {
                return Err("Local package is incompatible with this host".into());
            }
            let stage = Self::prepare_install(&host.root, &archive)?;
            let info = Self::prepared_info(&stage)?;
            if !ids.insert(info.id) {
                return Err("Duplicate local package".into());
            }
            prepared.push(stage);
        }
        prepared
            .into_iter()
            .map(|stage| host.replace_prepared_inner(stage, true))
            .collect()
    }
    /// Install explicit local builds without opening sockets or starting plugins
    /// The caller must hold the Manager configuration's exclusive writer lock
    pub fn install_local_packages(
        root: PathBuf,
        packages: Vec<LocalPackage>,
    ) -> Result<Vec<PluginInfo>, String> {
        let storage = stream_dock_manager_storage::Storage::open_validated(
            root.parent()
                .ok_or("Plugin directory has no configuration root")?
                .to_path_buf(),
            super::validate_storage_snapshot,
        )?;
        Self::install_local_packages_with_storage(root, packages, storage)
    }
    // Install local packages with storage
    pub fn install_local_packages_with_storage(
        root: PathBuf,
        packages: Vec<LocalPackage>,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Vec<PluginInfo>, String> {
        if packages.is_empty() || packages.len() > 256 || root.is_symlink() {
            return Err("Invalid local package selection or linked plugin directory".into());
        }
        let mut host = Self::open_local(root, storage)?;
        let mut seen = HashSet::new();
        let mut prepared = Vec::new();
        // Validate every selected archive before replacing any installed package
        for package in packages {
            if !safe_id(&package.id) || !seen.insert(package.id.clone()) {
                return Err("Invalid or duplicate local plugin identity".into());
            }
            let stage = Self::prepare_install(&host.root, &package.archive)?;
            Self::verify_prepared_files(&stage, &package.files)?;
            let info = Self::prepared_info(&stage)?;
            if info.id != package.id || info.version != package.version {
                return Err("Manifest identity/version differs from local inventory".into());
            }
            let target = host.root.join(&package.id);
            if target.is_symlink()
                || host
                    .plugins
                    .get(&package.id)
                    .is_some_and(|p| p.path != target)
            {
                return Err("Installation path differs from managed target".into());
            }
            prepared.push(stage);
        }
        let mut installed = Vec::new();
        for stage in prepared {
            installed.push(host.replace_prepared_inner(stage, true)?);
        }
        Ok(installed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Construct the package representation
    fn package(base: &Path, id: &str, version: &str) -> LocalPackage {
        let archive = base.join(id);
        std::fs::create_dir_all(&archive).unwrap();
        let mut manifest = json!({"UUID":id,"Name":id,"Version":version,"CodePath":"runtime.html","SDKVersion":1,"StreamDockManager":{"ManifestDialect":"vsd"},"Actions":[{"UUID":format!("{id}.action"),"Name":"Action"}]});
        crate::lifecycle_tests::current_descriptor(&archive, &mut manifest);
        let files = packages::inventory(&archive).unwrap();
        LocalPackage {
            archive,
            id: id.into(),
            version: version.into(),
            files,
        }
    }

    // Save fixture
    fn save_fixture(root: &Path, value: &Value) {
        let storage =
            stream_dock_manager_storage::Storage::open(root.parent().unwrap().into()).unwrap();
        let mut hub = Hub::load(&storage).unwrap();
        hub.globals = serde_json::from_value(value["globals"].clone()).unwrap_or_default();
        hub.action_defaults =
            serde_json::from_value(value["action_defaults"].clone()).unwrap_or_default();
        hub.enabled = serde_json::from_value(value["enabled"].clone()).unwrap_or_default();
        hub.provenance = serde_json::from_value(value["provenance"].clone()).unwrap_or_default();
        hub.persist().unwrap();
    }
    // Capture the fixture state for assertions
    fn fixture_snapshot(root: &Path) -> Value {
        let storage =
            stream_dock_manager_storage::Storage::open(root.parent().unwrap().into()).unwrap();
        let hub = Hub::load(&storage).unwrap();
        json!({"globals":hub.globals,"action_defaults":hub.action_defaults,"enabled":hub.enabled,"provenance":hub.provenance})
    }
    #[test]
    // Verify local install preserves settings and enable states and backs up old files
    fn local_install_preserves_settings_and_enable_states_and_backs_up_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins");
        package(&root, "fixture.enabled", "1.0.0");
        package(&root, "fixture.disabled", "1.0.0");
        package(&root, "fixture.other", "1.0.0");
        let saved = json!({"schema_version":1,"globals":{"fixture.enabled":{"token":"keep"}},"settings":{"fixture.enabled:context":{"gain":42}},"enabled":["fixture.enabled","fixture.other","fixture.new"],"provenance":{"fixture.enabled":{"repository":"original/release"},"fixture.other":{"repository":"keep/release"}}});
        save_fixture(&root, &saved);
        let builds = dir.path().join("builds");
        let packages = ["fixture.enabled", "fixture.disabled", "fixture.new"]
            .map(|id| package(&builds, id, "1.0.1"))
            .into();
        let installed = PluginHost::install_local_packages(root.clone(), packages).unwrap();
        assert!(installed[0].enabled);
        assert!(!installed[1].enabled);
        assert!(!installed[2].enabled);
        assert!(installed.iter().all(|p| p.version == "1.0.1"));
        let current = fixture_snapshot(&root);
        assert_eq!(current["globals"], saved["globals"]);
        assert!(current.get("settings").is_none());
        let enabled: HashSet<String> = serde_json::from_value(current["enabled"].clone()).unwrap();
        assert_eq!(
            enabled,
            HashSet::from(["fixture.enabled".into(), "fixture.other".into()])
        );
        assert!(current["provenance"].get("fixture.enabled").is_none());
        assert_eq!(
            current["provenance"]["fixture.other"],
            saved["provenance"]["fixture.other"]
        );
        assert!(
            std::fs::read_to_string(root.join("fixture.other/runtime.html"))
                .unwrap()
                .contains("1.0.0")
        );
        let backups: Vec<_> = std::fs::read_dir(root.join(".backups"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        for id in ["fixture.enabled", "fixture.disabled"] {
            assert!(backups.iter().any(|backup| std::fs::read_to_string(
                backup.join(id).join("runtime.html")
            )
            .is_ok_and(|text| text.contains("1.0.0"))));
        }
        assert_eq!(
            std::fs::read_dir(root.join(".transactions"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    // Verify every package is verified before any existing package is replaced
    fn every_package_is_verified_before_any_existing_package_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins");
        package(&root, "fixture.first", "1.0.0");
        let builds = dir.path().join("builds");
        let first = package(&builds, "fixture.first", "1.0.1");
        let broken = package(&builds, "fixture.second", "1.0.1");
        std::fs::write(broken.archive.join("icon.svg"), "changed after hashing").unwrap();
        assert!(
            PluginHost::install_local_packages(root.clone(), vec![first, broken])
                .unwrap_err()
                .contains("hash mismatch")
        );
        assert!(
            std::fs::read_to_string(root.join("fixture.first/runtime.html"))
                .unwrap()
                .contains("1.0.0")
        );
        assert!(!root.join("fixture.second").exists());
        assert!(!root.join(".backups").exists());
    }

    #[test]
    // Verify local uninstall removes selected packages and settings and preserves other data
    fn local_uninstall_removes_selected_packages_and_settings_and_preserves_other_data() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins");
        for id in [
            "fixture.enabled",
            "fixture.disabled",
            "fixture.enabled.extra",
        ] {
            package(&root, id, "1.0.0");
        }
        let saved = json!({"schema_version":1,
            "globals":{"fixture.enabled":{"token":"remove"},"fixture.enabled.extra":{"token":"keep"}},
            "settings":{"fixture.enabled:context":{"gain":42},"fixture.enabled.extra:context":{"gain":7}},
            "action_defaults":{"fixture.enabled":{"fixture.enabled.action":{"step":2}},"fixture.enabled.extra":{"fixture.enabled.extra.action":{"step":3}}},
            "enabled":["fixture.enabled","fixture.enabled.extra"],
            "provenance":{"fixture.enabled":{"repository":"remove/release"},"fixture.enabled.extra":{"repository":"keep/release"}}});
        save_fixture(&root, &saved);
        let backup = root.join(".backups/history");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("evidence"), "keep backup").unwrap();
        let selected = ["fixture.enabled", "fixture.disabled", "fixture.missing"]
            .map(String::from)
            .into();
        let removed = PluginHost::uninstall_local_packages(root.clone(), selected).unwrap();
        assert_eq!(removed, ["fixture.enabled", "fixture.disabled"]);
        assert!(!root.join("fixture.enabled").exists());
        assert!(!root.join("fixture.disabled").exists());
        assert!(root.join("fixture.enabled.extra/runtime.html").is_file());
        assert_eq!(
            std::fs::read_to_string(backup.join("evidence")).unwrap(),
            "keep backup"
        );
        let value = fixture_snapshot(&root);
        assert!(value["globals"].get("fixture.enabled").is_none());
        assert!(value["action_defaults"].get("fixture.enabled").is_none());
        assert!(value["provenance"].get("fixture.enabled").is_none());
        for field in ["globals", "action_defaults", "provenance"] {
            assert_eq!(
                value[field]["fixture.enabled.extra"],
                saved[field]["fixture.enabled.extra"]
            );
        }
        assert_eq!(value["enabled"], json!(["fixture.enabled.extra"]));
        assert!(
            PluginHost::uninstall_local_packages(root, vec!["fixture.enabled".into()])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    // Verify local uninstall rejects invalid selections before removing any package
    fn local_uninstall_rejects_invalid_selections_before_removing_any_package() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins");
        package(&root, "fixture.keep", "1.0.0");
        for ids in [
            vec![],
            vec!["fixture.keep", "../outside"],
            vec!["fixture.keep", "fixture.keep"],
        ] {
            assert!(PluginHost::uninstall_local_packages(
                root.clone(),
                ids.into_iter().map(String::from).collect()
            )
            .is_err());
            assert!(root.join("fixture.keep/manifest.json").is_file());
            assert!(!root.join("settings.json").exists());
        }
        let absent = dir.path().join("absent");
        assert!(
            PluginHost::uninstall_local_packages(absent.clone(), vec!["fixture.keep".into()])
                .unwrap()
                .is_empty()
        );
        assert!(!absent.exists());
    }

    #[cfg(unix)]
    #[test]
    // Verify local uninstall preflights linked packages and plugin roots
    fn local_uninstall_preflights_linked_packages_and_plugin_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plugins");
        package(&root, "fixture.keep", "1.0.0");
        let source = package(&dir.path().join("source"), "fixture.linked", "1.0.0");
        std::os::unix::fs::symlink(&source.archive, root.join("fixture.linked")).unwrap();
        assert!(PluginHost::uninstall_local_packages(
            root.clone(),
            vec!["fixture.keep".into(), "fixture.linked".into()]
        )
        .unwrap_err()
        .contains("symlink"));
        assert!(root.join("fixture.keep/manifest.json").is_file());
        assert!(source.archive.join("manifest.json").is_file());
        assert!(!root.join("settings.json").exists());
        let linked_root = dir.path().join("linked-plugins");
        std::os::unix::fs::symlink(&root, &linked_root).unwrap();
        assert!(
            PluginHost::uninstall_local_packages(linked_root, vec!["fixture.keep".into()])
                .unwrap_err()
                .contains("symlink")
        );
        assert!(root.join("fixture.keep/manifest.json").is_file());
    }
}
