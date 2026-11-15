//! Private runtime removal is an inventory transaction, never a caller-provided path
use super::*;

const REMOVING: &str = ".removing-";

// Check whether the runtime installation identifier is valid
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 512
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
// Check whether the runtime record refers to a linked installation
fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true; // Includes directory junctions and other reparse points.
        }
    }
    metadata.file_type().is_symlink()
}
// Validate or create the managed directory
fn directory(path: &Path) -> Result<(), String> {
    // The service root was canonicalized at open. Reject subsequently substituted
    // parents too, rather than allowing a symlink to redirect a destructive operation.
    for ancestor in path.ancestors() {
        let metadata = ancestor.symlink_metadata().map_err(|e| e.to_string())?;
        if linked(&metadata) || !metadata.is_dir() {
            return Err("Runtime removal requires regular private directories, not links".into());
        }
    }
    Ok(())
}
// Record for the current operation
fn record(path: &Path) -> Result<RuntimeInstallation, String> {
    let path = path.join("installation.json");
    let metadata = path.symlink_metadata().map_err(|e| e.to_string())?;
    if linked(&metadata) || !metadata.is_file() || metadata.len() > 65536 {
        return Err("Private runtime ownership record is not a bounded regular file".into());
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if !opened.is_file() || linked(&opened) || opened.len() > 65536 {
        return Err("Private runtime ownership record is not a bounded regular file".into());
    }
    let bytes =
        crate::bounded_io::read_from(file, 65536, "Private runtime ownership record is too large")?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("Invalid private runtime ownership record: {e}"))
}
// Resolve the installation path owned by Manager
fn owned_path(root: &Path, installation: &RuntimeInstallation) -> Result<PathBuf, String> {
    if installation.source != "private" || !valid_id(&installation.id) {
        return Err("Only application-managed private runtime installations can be removed".into());
    }
    let path = root.join(installation.kind.name()).join(&installation.id);
    if Path::new(&installation.executable) != path.join(installation.kind.executable()) {
        return Err("Runtime executable is outside its private installation directory".into());
    }
    Ok(path)
}
// Check whether the runtime record is unchanged
fn same_record(a: &RuntimeInstallation, b: &RuntimeInstallation) -> bool {
    serde_json::to_value(a).ok() == serde_json::to_value(b).ok()
}
// Enumerate files belonging to this operation
fn files(path: &Path, kind: RuntimeKind) -> Result<Vec<PathBuf>, String> {
    directory(path)?;
    let mut files = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or("Unexpected file in private runtime directory")?;
        let metadata = entry.path().symlink_metadata().map_err(|e| e.to_string())?;
        if linked(&metadata)
            || !metadata.is_file()
            || !(name == kind.executable()
                || name == "installation.json"
                || name.starts_with("notice-"))
        {
            return Err(
                "Unexpected file or link in private runtime directory; files preserved".into(),
            );
        }
        files.push(entry.path());
        if files.len() > 10002 {
            return Err("Private runtime directory exceeds removal limits".into());
        }
    }
    Ok(files)
}
// Clean up resources owned by this lifecycle
fn cleanup(path: &Path, kind: RuntimeKind) -> Result<(), String> {
    let files = files(path, kind)?;
    // Keep ownership evidence until all payloads are gone so interrupted cleanup
    // can be verified and retried. Never recurse into directories or follow links.
    for file in files.iter().filter(|file| {
        file.file_name()
            .is_none_or(|name| name != "installation.json")
    }) {
        std::fs::remove_file(file).map_err(|e| e.to_string())?;
    }
    let marker = path.join("installation.json");
    if marker.symlink_metadata().is_ok() {
        std::fs::remove_file(marker).map_err(|e| e.to_string())?;
    }
    std::fs::remove_dir(path).map_err(|e| e.to_string())
}

/// The caller holds the inventory lock and has established that this ID is not
/// installed. Finish any committed removal before allowing the ID to be reused.
pub(super) fn prepare_install(root: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("Invalid private runtime installation ID".into());
    }
    let pending = root
        .join(RuntimeKind::Nodejs.name())
        .join(format!("{REMOVING}{id}"));
    match pending.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let contents = files(&pending, RuntimeKind::Nodejs)?;
    if !contents.is_empty() {
        let marker = record(&pending)?;
        owned_path(root, &marker)?;
        if marker.id != id {
            return Err("Pending runtime removal ownership mismatch".into());
        }
    }
    cleanup(&pending, RuntimeKind::Nodejs).map_err(|error| {
        format!("Finish removing the previous private runtime before reinstalling: {error}")
    })
}

// Recover for the current operation
pub(super) fn recover(root: &Path, installations: &[RuntimeInstallation]) -> Result<(), String> {
    let parent = root.join(RuntimeKind::Nodejs.name());
    if !parent.try_exists().map_err(|e| e.to_string())? {
        return Ok(());
    }
    directory(&parent)?;
    for entry in std::fs::read_dir(&parent).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|name| name.strip_prefix(REMOVING)) else {
            continue;
        };
        if !valid_id(id) {
            return Err("Invalid pending runtime removal directory".into());
        }
        let path = entry.path();
        let contents = files(&path, RuntimeKind::Nodejs)?;
        let saved = installations
            .iter()
            .find(|installation| installation.id == id);
        if contents.is_empty() && saved.is_none() {
            std::fs::remove_dir(path).map_err(|e| e.to_string())?;
            continue;
        }
        let marker = record(&path)?;
        let original = owned_path(root, &marker)?;
        if marker.id != id {
            return Err("Pending runtime removal ownership mismatch".into());
        }
        if let Some(saved) = saved {
            if !same_record(saved, &marker) || original.symlink_metadata().is_ok() {
                return Err("Cannot restore interrupted runtime removal; files preserved".into());
            }
            std::fs::rename(path, original).map_err(|e| e.to_string())?;
        } else {
            cleanup(&path, marker.kind)?;
        }
    }
    Ok(())
}

impl RuntimeService {
    /// Call while holding the plugin host lock, passing all live runtime processes
    /// Exact selections remain pinned and become missing, never silently replaced
    pub fn remove_private(&self, id: &str, running: &[RuntimeInstallation]) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let installation = state
            .saved
            .installations
            .iter()
            .find(|i| i.id == id)
            .ok_or("Unknown private runtime installation ID")?;
        let target = owned_path(&self.root, installation)?;
        directory(&target)?;
        let marker = record(&target)?;
        if !same_record(installation, &marker) {
            return Err(
                "Private runtime ownership record does not match the installed inventory".into(),
            );
        }
        files(&target, installation.kind)?;
        let executable = PathBuf::from(&installation.executable);
        if running.iter().any(|runtime| {
            runtime.id == id
                || Path::new(&runtime.executable) == executable
                || Path::new(&runtime.executable).canonicalize().ok().as_ref() == Some(&executable)
        }) {
            return Err("Runtime is in use. Stop the plugins using it or select another runtime and restart them before deleting it.".into());
        }
        if state
            .jobs
            .values()
            .any(|(job, _)| job.release_id == id && job.state == "installing")
        {
            return Err("Wait for the runtime installation to finish before deleting it".into());
        }
        let kind = installation.kind;
        let pending = target.with_file_name(format!("{REMOVING}{id}"));
        if pending.symlink_metadata().is_ok() {
            return Err(
                "A previous runtime removal needs recovery; restart the application".into(),
            );
        }
        std::fs::rename(&target, &pending).map_err(|e| e.to_string())?;
        let mut next = state.saved.clone();
        next.installations
            .retain(|installation| installation.id != id);
        if let Err(error) = self.persist(&next) {
            return match std::fs::rename(&pending, &target) {
                Ok(()) => Err(error),
                Err(restore) => Err(format!("Runtime removal was not saved: {error}. Restoring its files failed: {restore}; restart to recover.")),
            };
        }
        state.saved = next;
        self.probes
            .lock()
            .unwrap()
            .retain(|_, (_, _, cached)| cached.executable != executable.to_string_lossy());
        cleanup(&pending, kind).map_err(|error| format!("Runtime removed, but temporary file cleanup failed; it will retry on restart: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Construct the isolated test fixture
    fn fixture() -> (tempfile::TempDir, Arc<RuntimeService>, RuntimeInstallation) {
        let temporary = tempfile::tempdir().unwrap();
        let service = RuntimeService::open(temporary.path().join("runtimes")).unwrap();
        let installation = install(&service, "nodejs-lts-24.16.0-fixture");
        (temporary, service, installation)
    }
    // Install for the current operation
    fn install(service: &RuntimeService, id: &str) -> RuntimeInstallation {
        let target = service.root.join("nodejs").join(id);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            target.join(RuntimeKind::Nodejs.executable()),
            b"test fixture; never executed",
        )
        .unwrap();
        std::fs::write(target.join("notice-1-LICENSE"), "fixture license").unwrap();
        let installation = RuntimeInstallation {
            id: id.into(),
            kind: RuntimeKind::Nodejs,
            channel: "lts".into(),
            line: "24".into(),
            version: "24.16.0".into(),
            executable: target
                .join(RuntimeKind::Nodejs.executable())
                .to_string_lossy()
                .into(),
            source: "private".into(),
            verification: "official-https".into(),
            provenance: "test fixture".into(),
        };
        std::fs::write(
            target.join("installation.json"),
            serde_json::to_vec(&installation).unwrap(),
        )
        .unwrap();
        service
            .save(|saved| saved.installations.push(installation.clone()))
            .unwrap();
        installation
    }
    // Report the pending work tracked by this owner
    fn pending(service: &RuntimeService, installation: &RuntimeInstallation) -> PathBuf {
        service
            .root
            .join("nodejs")
            .join(format!("{REMOVING}{}", installation.id))
    }
    // Preserve an installation that cannot safely be removed
    fn preserve(service: &RuntimeService, installation: &RuntimeInstallation) {
        assert!(Path::new(&installation.executable).is_file());
        assert!(service
            .status()
            .installations
            .iter()
            .any(|i| i.id == installation.id));
    }
    #[test]
    // Verify removes only target and preserves exact selections after reopen
    fn removes_only_target_and_preserves_exact_selections_after_reopen() {
        let (_temporary, service, installation) = fixture();
        let other = install(&service, "nodejs-lts-24.15.0-fixture");
        service
            .save(|saved| {
                let choice = RuntimeSelection {
                    kind: installation.kind,
                    channel: installation.channel.clone(),
                    line: installation.line.clone(),
                    installation_id: Some(installation.id.clone()),
                    external: None,
                    acknowledge_override: false,
                };
                saved.defaults.insert("nodejs".into(), choice.clone());
                saved.plugins.insert("plugin".into(), choice);
            })
            .unwrap();
        service.remove_private(&installation.id, &[]).unwrap();
        assert!(!Path::new(&installation.executable).exists());
        assert!(!pending(&service, &installation).exists());
        preserve(&service, &other);
        assert_eq!(
            service.status().defaults["nodejs"]
                .installation_id
                .as_deref(),
            Some(installation.id.as_str())
        );
        assert!(service
            .inspect_resolution(
                "plugin",
                &RuntimeRequirement {
                    kind: installation.kind,
                    channel: "lts".into(),
                    version: Some("24".into())
                }
            )
            .selected
            .is_none());
        assert!(service
            .remove_private(&installation.id, &[])
            .unwrap_err()
            .contains("Unknown"));
        let root = service.root.clone();
        drop(service);
        let reopened = RuntimeService::open(root).unwrap();
        assert_eq!(reopened.status().installations.len(), 1);
        assert_eq!(
            reopened.status().plugins["plugin"]
                .installation_id
                .as_deref(),
            Some(installation.id.as_str())
        );
    }
    #[test]
    // Verify rejects unknown external and outside inventory paths
    fn rejects_unknown_external_and_outside_inventory_paths() {
        let (temporary, service, installation) = fixture();
        for id in ["../nodejs", "..", "/tmp", "external-node"] {
            assert!(service.remove_private(id, &[]).is_err());
        }
        let outside = temporary.path().join("external-node");
        std::fs::write(&outside, "keep").unwrap();
        for (source, executable, id) in [
            (
                "external",
                installation.executable.clone(),
                installation.id.clone(),
            ),
            (
                "private",
                outside.to_string_lossy().into(),
                installation.id.clone(),
            ),
            (
                "private",
                installation.executable.clone(),
                "../escape".into(),
            ),
        ] {
            service
                .save(|saved| {
                    let i = &mut saved.installations[0];
                    i.source = source.into();
                    i.executable = executable;
                    i.id = id.clone();
                })
                .unwrap();
            assert!(service.remove_private(&id, &[]).is_err());
            assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
            assert!(Path::new(&installation.executable).exists());
        }
    }
    #[test]
    // Verify ownership mismatch unknown contents and missing marker are preserved
    fn ownership_mismatch_unknown_contents_and_missing_marker_are_preserved() {
        let (_temporary, service, installation) = fixture();
        let target = Path::new(&installation.executable).parent().unwrap();
        let marker = target.join("installation.json");
        let original = std::fs::read(&marker).unwrap();
        let mut changed = installation.clone();
        changed.version = "22.1.0".into();
        std::fs::write(&marker, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(service
            .remove_private(&installation.id, &[])
            .unwrap_err()
            .contains("ownership"));
        preserve(&service, &installation);
        std::fs::write(&marker, &original).unwrap();
        std::fs::write(target.join("user-file"), "keep").unwrap();
        assert!(service
            .remove_private(&installation.id, &[])
            .unwrap_err()
            .contains("Unexpected"));
        preserve(&service, &installation);
        std::fs::remove_file(target.join("user-file")).unwrap();
        std::fs::remove_file(marker).unwrap();
        assert!(service.remove_private(&installation.id, &[]).is_err());
        preserve(&service, &installation);
    }
    #[test]
    // Verify in use IDs paths and install jobs block removal
    fn in_use_ids_paths_and_install_jobs_block_removal() {
        let (_temporary, service, installation) = fixture();
        assert!(service
            .remove_private(&installation.id, std::slice::from_ref(&installation))
            .unwrap_err()
            .contains("in use"));
        let mut external = installation.clone();
        external.id = "external-alias".into();
        external.source = "external".into();
        assert!(service
            .remove_private(&installation.id, &[external])
            .unwrap_err()
            .contains("in use"));
        service.state.lock().unwrap().jobs.insert(
            "job".into(),
            (
                RuntimeJob {
                    id: "job".into(),
                    release_id: installation.id.clone(),
                    state: "installing".into(),
                    downloaded: 0,
                    total: None,
                    error: None,
                    kind: None,
                    line: None,
                    version: None,
                },
                Arc::new(AtomicBool::new(false)),
            ),
        );
        assert!(service
            .remove_private(&installation.id, &[])
            .unwrap_err()
            .contains("finish"));
        preserve(&service, &installation);
    }
    #[test]
    // Verify failed persistence restores files and inventory
    fn failed_persistence_restores_files_and_inventory() {
        let (_temporary, service, installation) = fixture();
        service.storage.run(|connection| connection.execute_batch("CREATE TEMP TRIGGER fail_removal BEFORE INSERT ON runtime_state BEGIN SELECT RAISE(ABORT, 'injected removal failure'); END;").map_err(|e| e.to_string())).unwrap();
        assert!(service
            .remove_private(&installation.id, &[])
            .unwrap_err()
            .contains("injected removal failure"));
        preserve(&service, &installation);
        assert!(!pending(&service, &installation).exists());
    }
    #[test]
    // Verify recovery restores uncommitted and finishes committed removal
    fn recovery_restores_uncommitted_and_finishes_committed_removal() {
        for committed in [false, true] {
            let (_temporary, service, installation) = fixture();
            let target = Path::new(&installation.executable).parent().unwrap();
            std::fs::rename(target, pending(&service, &installation)).unwrap();
            if committed {
                service.save(|saved| saved.installations.clear()).unwrap();
            }
            let root = service.root.clone();
            drop(service);
            let service = RuntimeService::open(root).unwrap();
            assert!(!pending(&service, &installation).exists());
            assert_eq!(Path::new(&installation.executable).is_file(), !committed);
            assert_eq!(service.status().installations.is_empty(), committed);
        }
    }
    #[test]
    // Verify recovery finishes partial cleanup without executable or marker
    fn recovery_finishes_partial_cleanup_without_executable_or_marker() {
        for keep_marker in [false, true] {
            let (_temporary, service, installation) = fixture();
            let target = Path::new(&installation.executable).parent().unwrap();
            let pending = pending(&service, &installation);
            std::fs::rename(target, &pending).unwrap();
            std::fs::remove_file(pending.join(RuntimeKind::Nodejs.executable())).unwrap();
            std::fs::remove_file(pending.join("notice-1-LICENSE")).unwrap();
            if !keep_marker {
                std::fs::remove_file(pending.join("installation.json")).unwrap();
            }
            service.save(|saved| saved.installations.clear()).unwrap();
            let root = service.root.clone();
            drop(service);
            RuntimeService::open(root).unwrap();
            assert!(!pending.exists());
        }
    }
    #[test]
    // Verify reinstall finishes pending removal before reusing inventory ID
    fn reinstall_finishes_pending_removal_before_reusing_inventory_id() {
        let (_temporary, service, installation) = fixture();
        let target = Path::new(&installation.executable).parent().unwrap();
        let pending = pending(&service, &installation);
        std::fs::rename(target, &pending).unwrap();
        service.save(|saved| saved.installations.clear()).unwrap();
        std::fs::write(pending.join("unexpected-user-file"), "keep").unwrap();
        assert!(prepare_install(&service.root, &installation.id).is_err());
        assert!(pending.join("installation.json").is_file());
        assert!(service.status().installations.is_empty());
        std::fs::remove_file(pending.join("unexpected-user-file")).unwrap();
        prepare_install(&service.root, &installation.id).unwrap();
        assert!(!pending.exists());
        install(&service, &installation.id);
        let root = service.root.clone();
        drop(service);
        let service = RuntimeService::open(root).unwrap();
        service.remove_private(&installation.id, &[]).unwrap();
        assert!(service.status().installations.is_empty());
    }
    #[test]
    // Verify failed recovery preserves files without blocking application startup
    fn failed_recovery_preserves_files_without_blocking_application_startup() {
        let (_temporary, service, installation) = fixture();
        let target = Path::new(&installation.executable).parent().unwrap();
        let pending = pending(&service, &installation);
        std::fs::rename(target, &pending).unwrap();
        std::fs::write(pending.join("unexpected-user-file"), "keep").unwrap();
        service.save(|saved| saved.installations.clear()).unwrap();
        let root = service.root.clone();
        drop(service);
        let service = RuntimeService::open(root).unwrap();
        assert!(service
            .status()
            .error
            .unwrap()
            .contains("removal recovery needs attention"));
        assert_eq!(
            std::fs::read_to_string(pending.join("unexpected-user-file")).unwrap(),
            "keep"
        );
        assert!(pending.join(RuntimeKind::Nodejs.executable()).is_file());
    }
    #[cfg(unix)]
    #[test]
    // Verify rejects symlinked parents installation marker and payload without touching targets
    fn rejects_symlinked_parents_installation_marker_and_payload_without_touching_targets() {
        use std::os::unix::fs::symlink;
        for component in [
            "root",
            "kind",
            "installation",
            "marker",
            "executable",
            "notice",
        ] {
            let (temporary, service, installation) = fixture();
            let target = Path::new(&installation.executable).parent().unwrap();
            let path = match component {
                "root" => service.root.clone(),
                "kind" => service.root.join("nodejs"),
                "installation" => target.to_path_buf(),
                "marker" => target.join("installation.json"),
                "executable" => PathBuf::from(&installation.executable),
                _ => target.join("notice-1-LICENSE"),
            };
            let preserved = temporary.path().join("preserved");
            std::fs::rename(&path, &preserved).unwrap();
            symlink(&preserved, &path).unwrap();
            assert!(
                service.remove_private(&installation.id, &[]).is_err(),
                "{component}"
            );
            assert!(preserved.exists(), "{component}");
            assert!(service
                .status()
                .installations
                .iter()
                .any(|i| i.id == installation.id));
        }
    }
    #[cfg(unix)]
    #[test]
    // Verify running external symlink alias is in use
    fn running_external_symlink_alias_is_in_use() {
        let (temporary, service, installation) = fixture();
        let alias = temporary.path().join("node-alias");
        std::os::unix::fs::symlink(&installation.executable, &alias).unwrap();
        let mut external = installation.clone();
        external.id = "external-alias".into();
        external.executable = alias.to_string_lossy().into();
        assert!(service
            .remove_private(&installation.id, &[external])
            .unwrap_err()
            .contains("in use"));
        preserve(&service, &installation);
    }
}
