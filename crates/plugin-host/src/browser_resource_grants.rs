//! Exact, revocable file selections made by the native inspector's file panel
//! The browser cannot call this API. Selected bytes are snapshotted once, before
//! exposing a path, so later replacement of the source never expands authority.
use super::*;
use std::{fs::OpenOptions, time::Instant};

const MAX_FILES: usize = 32;
const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_TOTAL: u64 = 64 * 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(15 * 60);

pub(super) struct ResourceSelections {
    owner: String,
    context: String,
    selections: HashMap<PathBuf, (PathBuf, Instant)>,
    // Retain superseded snapshots until revocation: queued writes may use them
    pub(super) batches: Vec<Arc<tempfile::TempDir>>,
    alive: Arc<AtomicBool>,
    files: usize,
    bytes: u64,
}
impl ResourceSelections {
    // Resolve for the current operation
    pub(super) fn resolve(&self, owner: &str, context: &str, source: &Path) -> Option<&Path> {
        if !self.alive.load(Ordering::Acquire) || self.owner != owner || self.context != context {
            return None;
        }
        self.selections
            .get(source)
            .and_then(|(path, expires)| (Instant::now() < *expires).then_some(path.as_path()))
    }
}

impl InspectorBrowser {
    /// Only native code handling a completed, explicit user file selection may
    /// call this. There is deliberately no command, HTTP or WebSocket endpoint.
    pub fn grant_selected_resources(&self, paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
        if !self.is_active() || paths.is_empty() || paths.len() > MAX_FILES {
            return Err("Invalid or expired inspector file selection".into());
        }
        let context = {
            let hub = self.hub.lock().unwrap();
            hub.sessions
                .files
                .get(&self.token)
                .and_then(|grant| grant.context.clone())
                .ok_or("Inspector file selection expired")?
        };
        // Native panel completion is the authority, never a path from JavaScript
        // Do not hold the host lock while copying up to the bounded quota
        let batch = snapshot(paths)?;
        if !self.is_active() {
            return Err("Inspector file selection expired".into());
        }
        let mut hub = self.hub.lock().unwrap();
        if !self.alive.load(Ordering::Acquire)
            || hub.sessions.tokens.get(&self.token) != Some(&self.owner)
            || hub
                .sessions
                .files
                .get(&self.token)
                .and_then(|grant| grant.context.as_ref())
                != Some(&context)
            || hub.context_state.contexts.get(&context) != Some(&self.owner)
        {
            return Err("Inspector file selection expired".into());
        }
        let registry = hub
            .sessions
            .resource_selections
            .entry(self.token.clone())
            .or_insert_with(|| ResourceSelections {
                alive: self.alive.clone(),
                owner: self.owner.clone(),
                context: context.clone(),
                selections: HashMap::new(),
                batches: vec![],
                files: 0,
                bytes: 0,
            });
        if registry.owner != self.owner
            || registry.context != context
            || registry.files + batch.paths.len() > MAX_FILES
            || registry.bytes + batch.bytes > MAX_TOTAL
        {
            return Err(
                "Inspector selections exceed 32 files or 64 MiB; reopen the inspector".into(),
            );
        }
        let expires = Instant::now() + LIFETIME;
        let selected = batch
            .paths
            .iter()
            .map(|(source, _)| source.clone())
            .collect();
        registry.files += batch.paths.len();
        registry.bytes += batch.bytes;
        for (source, copy) in batch.paths {
            registry.selections.insert(source, (copy, expires));
        }
        registry.batches.push(Arc::new(batch.directory));
        Ok(selected)
    }
}

struct Snapshot {
    directory: tempfile::TempDir,
    paths: Vec<(PathBuf, PathBuf)>,
    bytes: u64,
}
// Copy reviewed regular files into a bounded temporary resource snapshot
fn snapshot(paths: &[PathBuf]) -> Result<Snapshot, String> {
    let mut batch = Snapshot {
        directory: tempfile::tempdir().map_err(|e| e.to_string())?,
        paths: vec![],
        bytes: 0,
    };
    for path in paths {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| "Selected file is unavailable")?;
        if !path.is_absolute()
            || !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_FILE
        {
            return Err("Select a regular file no larger than 16 MiB".into());
        }
        let source = path
            .canonicalize()
            .map_err(|_| "Selected file is unavailable")?;
        if source.to_str().is_none_or(|value| value.len() > 4096) {
            return Err("Selected file path cannot be represented by the plugin protocol".into());
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut input = options
            .open(&source)
            .map_err(|_| "Selected file is unavailable")?;
        let opened = input
            .metadata()
            .map_err(|_| "Selected file is unavailable")?;
        if !opened.is_file() || opened.len() > MAX_FILE {
            return Err("Select a regular file no larger than 16 MiB".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
                return Err("Selected file changed; select it again".into());
            }
        }
        let extension = source
            .extension()
            .and_then(|s| s.to_str())
            .filter(|s| {
                !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .unwrap_or("bin");
        let destination = batch
            .directory
            .path()
            .join(format!("{}.{extension}", batch.paths.len()));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|e| e.to_string())?;
        let copied = std::io::copy(
            &mut Read::by_ref(&mut input).take(MAX_FILE + 1),
            &mut output,
        )
        .map_err(|e| e.to_string())?;
        batch.bytes += copied;
        if copied > MAX_FILE || batch.bytes > MAX_TOTAL {
            return Err("Inspector selections exceed file size limits".into());
        }
        batch.paths.push((source, destination));
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct a test inspector with a scoped file grant
    fn inspector(root: &Path) -> InspectorBrowser {
        let mut hub = Hub::default();
        hub.sessions.tokens.insert("token".into(), "one".into());
        hub.sessions.files.insert(
            "token".into(),
            FileGrant {
                root: root.into(),
                runtime_entry: None,
                bootstrap: String::new(),
                context: Some("context".into()),
                setup_runtime: false,
            },
        );
        hub.context_state
            .contexts
            .insert("context".into(), "one".into());
        hub.configuration_actions
            .insert("context".into(), "action".into());
        InspectorBrowser {
            hub: Arc::new(Mutex::new(hub)),
            token: "token".into(),
            owner: "one".into(),
            url: "http://127.0.0.1:1/files/token/pi.html".into(),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }
    #[test]
    // Verify native selection stages exact snapshot and revokes without losing pending bytes
    fn native_selection_stages_exact_snapshot_and_revokes_without_losing_pending_bytes() {
        let package = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let source = external.path().join("chosen.txt");
        std::fs::write(&source, "selected bytes").unwrap();
        let lease = inspector(package.path());
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        lease.hub.lock().unwrap().settings_writer = Some(tx);
        let selected = lease
            .grant_selected_resources(std::slice::from_ref(&source))
            .unwrap();
        std::fs::remove_file(&source).unwrap();
        let payload = json!({"file":selected[0]});
        lease
            .hub
            .lock()
            .unwrap()
            .queue_resources("one", "token", "other", &payload)
            .unwrap_err();
        lease
            .hub
            .lock()
            .unwrap()
            .queue_resources("one", "token", "context", &payload)
            .unwrap();
        let queued = rx.try_recv().unwrap();
        let copy = PathBuf::from(queued.resources.as_ref().unwrap()["file"].as_str().unwrap());
        assert_ne!(copy, selected[0]);
        assert_eq!(std::fs::read(&copy).unwrap(), b"selected bytes");
        lease.revoke();
        assert!(lease
            .hub
            .lock()
            .unwrap()
            .sessions
            .resource_selections
            .is_empty());
        assert!(lease.grant_selected_resources(&[source]).is_err());
        assert!(lease
            .hub
            .lock()
            .unwrap()
            .queue_resources("one", "token", "context", &payload)
            .is_err());
        assert_eq!(std::fs::read(&copy).unwrap(), b"selected bytes");
        drop(queued);
        assert!(!copy.exists());
    }
    #[test]
    // Verify native selection enforces session quota and owner lifetime
    fn native_selection_enforces_session_quota_and_owner_lifetime() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("chosen");
        std::fs::write(&source, "x").unwrap();
        let lease = inspector(root.path());
        assert!(lease.grant_selected_resources(&[]).is_err());
        assert!(lease
            .grant_selected_resources(&vec![source.clone(); MAX_FILES + 1])
            .is_err());
        lease
            .grant_selected_resources(&vec![source.clone(); MAX_FILES])
            .unwrap();
        assert!(lease
            .grant_selected_resources(std::slice::from_ref(&source))
            .is_err());
        lease
            .hub
            .lock()
            .unwrap()
            .context_state
            .contexts
            .insert("context".into(), "other".into());
        assert!(lease.grant_selected_resources(&[source]).is_err());
    }
    #[test]
    // Verify selected snapshot cannot follow later source replacement
    fn selected_snapshot_cannot_follow_later_source_replacement() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("selected.txt");
        std::fs::write(&source, "selected").unwrap();
        let batch = snapshot(std::slice::from_ref(&source)).unwrap();
        std::fs::remove_file(&source).unwrap();
        std::fs::write(&source, "replaced").unwrap();
        assert_eq!(std::fs::read(&batch.paths[0].1).unwrap(), b"selected");
        assert_eq!(batch.paths[0].0, source.canonicalize().unwrap());
    }
    #[test]
    // Verify grant resolution is exact scoped and expiring
    fn grant_resolution_is_exact_scoped_and_expiring() {
        let grant = ResourceSelections {
            alive: Arc::new(AtomicBool::new(true)),
            owner: "one".into(),
            context: "context".into(),
            selections: HashMap::from([(
                PathBuf::from("/selected.txt"),
                (
                    PathBuf::from("/private/copy.txt"),
                    Instant::now() + LIFETIME,
                ),
            )]),
            batches: vec![],
            files: 1,
            bytes: 1,
        };
        assert_eq!(
            grant.resolve("one", "context", Path::new("/selected.txt")),
            Some(Path::new("/private/copy.txt"))
        );
        assert!(grant
            .resolve("two", "context", Path::new("/selected.txt"))
            .is_none());
        assert!(grant
            .resolve("one", "other", Path::new("/selected.txt"))
            .is_none());
        assert!(grant
            .resolve("one", "context", Path::new("/unselected.txt"))
            .is_none());
        let mut expired = grant;
        expired
            .selections
            .get_mut(Path::new("/selected.txt"))
            .unwrap()
            .1 = Instant::now();
        assert!(expired
            .resolve("one", "context", Path::new("/selected.txt"))
            .is_none());
    }
    #[test]
    // Verify snapshot rejects directories missing and oversized files
    fn snapshot_rejects_directories_missing_and_oversized_files() {
        let root = tempfile::tempdir().unwrap();
        assert!(snapshot(&[root.path().into()]).is_err());
        assert!(snapshot(&[root.path().join("missing")]).is_err());
        let source = root.path().join("large.txt");
        std::fs::File::create(&source)
            .unwrap()
            .set_len(MAX_FILE + 1)
            .unwrap();
        assert!(snapshot(&[source]).is_err());
    }
    #[cfg(unix)]
    #[test]
    // Verify snapshot rejects symlinks and special files
    fn snapshot_rejects_symlinks_and_special_files() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        std::fs::write(&source, "source").unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(snapshot(&[link]).is_err());
        assert!(snapshot(&[PathBuf::from("/dev/null")]).is_err());
    }
}
