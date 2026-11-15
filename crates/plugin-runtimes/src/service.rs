mod removal;
pub use crate::installer::probe;
use crate::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
pub trait Clock: Send + Sync {
    // Read the current time for this subsystem
    fn now(&self) -> i64;
}
pub struct SystemClock;
impl Clock for SystemClock {
    // Read the current time for this subsystem
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}
pub trait Transport: Send + Sync {
    // Retrieve the requested value
    fn get(
        &self,
        kind: RuntimeKind,
        url: &str,
        artifact: bool,
        limit: usize,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<Vec<u8>, String>;
    /// Stream the artifact into the bounded download cache
    fn download(
        &self,
        kind: RuntimeKind,
        spec: &stream_dock_manager_downloads::ArtifactSpec,
        cache: &stream_dock_manager_downloads::DownloadCache,
        control: &dyn Fn() -> stream_dock_manager_downloads::Control,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<stream_dock_manager_downloads::Artifact, String>;
}
pub struct OfficialTransport;
impl Transport for OfficialTransport {
    // Download for the current operation
    fn download(
        &self,
        kind: RuntimeKind,
        spec: &stream_dock_manager_downloads::ArtifactSpec,
        cache: &stream_dock_manager_downloads::DownloadCache,
        control: &dyn Fn() -> stream_dock_manager_downloads::Control,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<stream_dock_manager_downloads::Artifact, String> {
        let validate = |url: &reqwest::Url, _redirect: bool| {
            if crate::providers::allowed(kind, url, true) {
                Ok(())
            } else {
                Err("Runtime origin is not permitted".into())
            }
        };
        cache.download_from_origin(
            spec,
            stream_dock_manager_downloads::NetworkPolicy {
                headers: None,
                validate: &validate,
                timeout: std::time::Duration::from_secs(300),
            },
            control,
            &|done, total| progress(done as u32, total.map(|n| n as u32)),
        )
    }
    // Retrieve the requested value
    fn get(
        &self,
        kind: RuntimeKind,
        url: &str,
        artifact: bool,
        limit: usize,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        let validate = |url: &reqwest::Url, _redirect: bool| {
            if crate::providers::allowed(kind, url, artifact) {
                Ok(())
            } else {
                Err("Runtime origin is not permitted".into())
            }
        };
        stream_dock_manager_downloads::metadata(
            url,
            stream_dock_manager_downloads::NetworkPolicy {
                headers: None,
                validate: &validate,
                timeout: std::time::Duration::from_secs(300),
            },
            limit,
            &|done, total| progress(done as u32, total.map(|n| n as u32)),
        )
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    lines: Vec<RuntimeLine>,
    releases: Vec<RuntimeRelease>,
    installations: Vec<RuntimeInstallation>,
    defaults: BTreeMap<String, RuntimeSelection>,
    plugins: BTreeMap<String, RuntimeSelection>,
    #[serde(default)]
    resolved_lines: BTreeMap<String, String>,
    automatic: bool,
    last_check: Option<i64>,
    next_check: i64,
    error: Option<String>,
}
// Future schedule dates alone do not establish that an LTS build has shipped
fn visible_lines(saved: &Saved) -> Vec<RuntimeLine> {
    saved
        .lines
        .iter()
        .filter(|l| {
            saved
                .releases
                .iter()
                .any(|r| r.kind == l.kind && r.channel == l.channel && r.line == l.line)
                || saved
                    .installations
                    .iter()
                    .any(|i| i.kind == l.kind && i.channel == l.channel && i.line == l.line)
        })
        .cloned()
        .collect()
}
struct State {
    saved: Saved,
    jobs: BTreeMap<String, (RuntimeJob, Arc<AtomicBool>)>,
    checking: bool,
}
#[derive(Clone)]
struct InstallRequest {
    release: RuntimeRelease,
}
type ProbeCache =
    BTreeMap<(RuntimeKind, PathBuf), (std::time::SystemTime, u64, RuntimeInstallation)>;
pub struct RuntimeService {
    pub(crate) root: PathBuf,
    storage: stream_dock_manager_storage::Storage,
    pub(crate) transport: Arc<dyn Transport>,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    probes: Mutex<ProbeCache>,
    pub(crate) downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
    user_cancelled: Mutex<std::collections::HashSet<String>>,
    install_requests: Mutex<BTreeMap<String, InstallRequest>>,
}
impl RuntimeService {
    // Forget plugin
    pub fn forget_plugin(&self, id: &str) -> Result<(), String> {
        self.forget_plugin_with(id, 0, |_| Ok(()))
    }
    /// Compose installation-local selection removal with the host's durable uninstall
    pub fn forget_plugin_with<T: Send + 'static>(
        &self,
        id: &str,
        cost: usize,
        commit: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<T, String>
            + Send
            + 'static,
    ) -> Result<T, String> {
        let mut state = self.state.lock().unwrap();
        let mut candidate = state.saved.clone();
        candidate.plugins.remove(id);
        let saved = candidate.clone();
        let bytes = serde_json::to_vec(&saved).map_err(|e| e.to_string())?.len();
        let result = self
            .storage
            .transaction_with_cost(cost + bytes, move |tx| {
                stream_dock_manager_storage::write_json(
                    tx,
                    stream_dock_manager_storage::Domain::RuntimeState,
                    "preferences",
                    &saved,
                    16 * 1024 * 1024,
                )?;
                commit(tx)
            })?;
        state.saved = candidate;
        Ok(result)
    }
    // Open for the current operation
    pub fn open(root: PathBuf) -> Result<Arc<Self>, String> {
        Self::with_dependencies(root, Arc::new(SystemClock), Arc::new(OfficialTransport))
    }
    // Open with downloads
    pub fn open_with_downloads(
        root: PathBuf,
        downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
    ) -> Result<Arc<Self>, String> {
        Self::with_downloads(
            root,
            Arc::new(SystemClock),
            Arc::new(OfficialTransport),
            downloads,
        )
    }
    // Open with storage
    pub fn open_with_storage(
        root: PathBuf,
        downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Arc<Self>, String> {
        Self::with_storage_dependencies(
            root,
            Arc::new(SystemClock),
            Arc::new(OfficialTransport),
            downloads,
            storage,
        )
    }
    // Open shared
    pub fn open_shared(
        root: PathBuf,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Arc<Self>, String> {
        let downloads = Arc::new(stream_dock_manager_downloads::DownloadCache::open(
            root.join("partials"),
        )?);
        Self::open_with_storage(root, downloads, storage)
    }
    // Access the storage used by this subsystem
    pub fn storage(&self) -> &stream_dock_manager_storage::Storage {
        &self.storage
    }
    // Construct the service with explicit dependencies
    pub fn with_dependencies(
        root: PathBuf,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
    ) -> Result<Arc<Self>, String> {
        let downloads = Arc::new(stream_dock_manager_downloads::DownloadCache::open(
            root.join("partials"),
        )?);
        Self::with_downloads(root, clock, transport, downloads)
    }
    // Construct the service with the supplied download manager
    fn with_downloads(
        root: PathBuf,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
        downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
    ) -> Result<Arc<Self>, String> {
        let configuration_root = if root
            .file_name()
            .is_some_and(|name| name == "runtimes" || name == ".runtimes")
        {
            root.parent()
                .ok_or("Runtime directory has no configuration root")?
                .to_path_buf()
        } else {
            root.clone()
        };
        let storage = stream_dock_manager_storage::Storage::open_validated(
            configuration_root,
            validate_storage_snapshot,
        )?;
        Self::with_storage_dependencies(root, clock, transport, downloads, storage)
    }
    // Construct the service with explicit storage and runtime dependencies
    fn with_storage_dependencies(
        root: PathBuf,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
        downloads: Arc<stream_dock_manager_downloads::DownloadCache>,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Arc<Self>, String> {
        storage.run(|connection| validate_storage_snapshot(connection))?;
        storage.add_snapshot_validator(validate_storage_snapshot)?;
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let mut saved: Saved = if let Some(saved) = storage.read_json::<serde_json::Value>(
            stream_dock_manager_storage::Domain::RuntimeState,
            "preferences",
            16 * 1024 * 1024,
        )? {
            serde_json::from_value(saved)
                .map_err(|e| format!("Invalid runtime preferences: {e}"))?
        } else {
            Saved {
                lines: vec![],
                releases: vec![],
                installations: vec![],
                defaults: BTreeMap::new(),
                plugins: BTreeMap::new(),
                resolved_lines: BTreeMap::new(),
                automatic: true,
                last_check: None,
                next_check: 0,
                error: None,
            }
        };
        if let Err(error) = removal::recover(&root, &saved.installations) {
            // Cleanup failure must not prevent unrelated plugins or the editor from
            // starting. Keep the owned files for another attempt and report it.
            saved.error = Some(format!("Runtime removal recovery needs attention: {error}"));
        }
        // Staging directories are never published and can be discarded after interruption
        {
            let kind = RuntimeKind::Nodejs;
            let dir = root.join(kind.name());
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().starts_with(".staging-") {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
        }
        // Recover a validated publication interrupted before the inventory commit
        {
            let kind = RuntimeKind::Nodejs;
            if let Ok(entries) = std::fs::read_dir(root.join(kind.name())) {
                for entry in entries.flatten() {
                    let directory = entry.path();
                    let record = directory.join("installation.json");
                    if !entry.file_type().is_ok_and(|t| t.is_dir())
                        || entry.file_name().to_string_lossy().starts_with('.')
                        || !std::fs::metadata(&record).is_ok_and(|m| m.len() <= 65536)
                    {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(record) else {
                        continue;
                    };
                    let Ok(i) = serde_json::from_slice::<RuntimeInstallation>(&bytes) else {
                        continue;
                    };
                    if saved.installations.iter().any(|old| old.id == i.id) {
                        continue;
                    }
                    let expected = directory.join(kind.executable());
                    if i.kind == kind
                        && i.source == "private"
                        && directory.file_name().and_then(|s| s.to_str()) == Some(i.id.as_str())
                        && expected.to_string_lossy() == i.executable
                        && expected.is_file()
                        && !expected.is_symlink()
                        && probe(kind, &expected)
                            .is_ok_and(|p| p.version == i.version && p.channel == i.channel)
                    {
                        saved.installations.push(i);
                    }
                }
            }
        }
        let service = Arc::new(Self {
            root,
            storage,
            transport,
            clock,
            probes: Mutex::new(BTreeMap::new()),
            downloads,
            user_cancelled: Default::default(),
            install_requests: Default::default(),
            state: Mutex::new(State {
                saved,
                jobs: BTreeMap::new(),
                checking: false,
            }),
        });
        service.persist(&service.state.lock().unwrap().saved)?;
        Ok(service)
    }
    // Read the current time for this subsystem
    pub(crate) fn now(&self) -> i64 {
        self.clock.now()
    }
    // Persist for the current operation
    fn persist(&self, saved: &Saved) -> Result<(), String> {
        self.storage.write_json(
            stream_dock_manager_storage::Domain::RuntimeState,
            "preferences",
            saved,
            16 * 1024 * 1024,
        )
    }
    // Save for the current operation
    fn save(&self, edit: impl FnOnce(&mut Saved)) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.saved.clone();
        edit(&mut next);
        self.persist(&next)?;
        state.saved = next;
        Ok(())
    }
    // Read the current operation status
    pub fn status(&self) -> RuntimeStatus {
        let state = self.state.lock().unwrap();
        let s = &state.saved;
        RuntimeStatus {
            lines: visible_lines(s),
            releases: s.releases.clone(),
            installations: s.installations.clone(),
            defaults: s.defaults.clone(),
            plugins: s.plugins.clone(),
            automatic: s.automatic,
            last_check: s.last_check,
            next_check: s.next_check,
            stale: s
                .last_check
                .is_none_or(|t| self.now().saturating_sub(t) > 86400),
            error: s.error.clone(),
            jobs: state.jobs.values().map(|(j, _)| j.clone()).collect(),
        }
    }
    // Check whether automatic runtime metadata updates are enabled
    pub fn automatic(&self, enabled: bool) -> Result<(), String> {
        self.save(|s| s.automatic = enabled)
    }
    // Check whether a runtime metadata refresh is due
    pub fn due(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.saved.automatic && !s.checking && self.now() >= s.saved.next_check
    }
    // Start daily
    pub fn start_daily(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || loop {
            let Some(service) = weak.upgrade() else { break };
            if service.due() {
                let _ = service.check();
            }
            drop(service);
            std::thread::sleep(std::time::Duration::from_secs(60));
        });
    }
    // Check for the current operation
    pub fn check(&self) -> Result<RuntimeStatus, String> {
        {
            let mut s = self.state.lock().unwrap();
            if s.checking {
                return Ok(self.status_unlocked(&s));
            }
            s.checking = true;
        }
        let discovery = crate::providers::discover(self.transport.as_ref(), self.now());
        let now = self.now();
        let error = discovery
            .as_ref()
            .err()
            .map(|error| format!("nodejs: {error}"));
        let result = self.save(|s| {
            if let Ok(discovery) = discovery {
                s.lines = discovery.lines;
                s.releases = discovery.releases;
            }
            s.error = error.clone();
            s.next_check = now + if error.is_some() { 3600 } else { 86400 };
            if error.is_none() {
                s.last_check = Some(now);
            }
        });
        self.state.lock().unwrap().checking = false;
        result?;
        Ok(self.status())
    }
    // Read runtime status while the caller owns the service lock
    fn status_unlocked(&self, s: &State) -> RuntimeStatus {
        let v = &s.saved;
        RuntimeStatus {
            lines: visible_lines(v),
            releases: v.releases.clone(),
            installations: v.installations.clone(),
            defaults: v.defaults.clone(),
            plugins: v.plugins.clone(),
            automatic: v.automatic,
            last_check: v.last_check,
            next_check: v.next_check,
            stale: v.last_check.is_none_or(|t| self.now() - t > 86400),
            error: v.error.clone(),
            jobs: s.jobs.values().map(|(j, _)| j.clone()).collect(),
        }
    }
    // Select the recommended runtime version
    pub fn recommended(&self, kind: RuntimeKind) -> Option<RuntimeSelection> {
        let status = self.status();
        if let Some(s) = status.defaults.get(kind.name()) {
            return Some(s.clone());
        }
        let mut lines: Vec<_> = status
            .lines
            .iter()
            .filter(|l| l.kind == kind && supported(l, self.now()))
            .collect();
        lines.sort_by_key(|l| {
            l.line
                .split('.')
                .map(|s| s.parse::<u64>().unwrap_or(0))
                .collect::<Vec<_>>()
        });
        lines.last().map(|l| RuntimeSelection {
            kind,
            channel: l.channel.clone(),
            line: l.line.clone(),
            installation_id: None,
            external: None,
            acknowledge_override: false,
        })
    }
    // Resolve for the current operation
    pub fn resolve(&self, plugin: &str, requirement: &RuntimeRequirement) -> RuntimeResolution {
        if !self.status().defaults.contains_key(requirement.kind.name()) {
            if let Some(choice) = self.recommended(requirement.kind) {
                let _ = self.save(|s| {
                    s.defaults
                        .entry(requirement.kind.name().into())
                        .or_insert(choice);
                });
            }
        }
        let result = self.resolve_inner(plugin, requirement, true);
        if requirement.version.is_some() && !self.status().plugins.contains_key(plugin) {
            if let Ok((selected, _)) = &result {
                let key = serde_json::to_string(&(
                    plugin,
                    requirement.kind,
                    &requirement.channel,
                    &requirement.version,
                ))
                .unwrap();
                let line = selected.line.clone();
                let _ = self.save(|s| {
                    s.resolved_lines.entry(key).or_insert(line);
                });
            }
        }
        self.resolution_result(requirement, result)
    }
    /// Read-only setup readiness. Never seeds choices, pins lines or executes an
    /// external interpreter. Existing private installations retain their compatibility
    /// checks; external executables need an explicit runtime management check.
    pub fn inspect_resolution(
        &self,
        plugin: &str,
        requirement: &RuntimeRequirement,
    ) -> RuntimeResolution {
        self.resolution_result(requirement, self.resolve_inner(plugin, requirement, false))
    }
    // Build the result of resolving a runtime requirement
    fn resolution_result(
        &self,
        requirement: &RuntimeRequirement,
        result: Result<(RuntimeInstallation, bool), (&'static str, String)>,
    ) -> RuntimeResolution {
        match result {
            Ok((selected, overridden)) => RuntimeResolution {
                recommended_line: Some(selected.line.clone()),
                requirement: requirement.clone(),
                selected: Some(selected),
                state: "ready".into(),
                message: String::new(),
                overridden,
            },
            Err((state, message)) => RuntimeResolution {
                recommended_line: self.recommended_for(requirement),
                requirement: requirement.clone(),
                selected: None,
                state: state.into(),
                message,
                overridden: false,
            },
        }
    }
    // Select the recommended version for this requirement
    fn recommended_for(&self, r: &RuntimeRequirement) -> Option<String> {
        let Some(value) = r.version.as_deref() else {
            return self.recommended(r.kind).map(|c| c.line);
        };
        let req = version_requirement(value).ok()?;
        let status = self.status();
        if let Some(release) = status
            .releases
            .iter()
            .filter(|v| v.kind == r.kind && v.channel == r.channel)
            .filter(|v| semver::Version::parse(&v.version).is_ok_and(|v| req.matches(&v)))
            .max_by_key(|v| semver::Version::parse(&v.version).ok())
        {
            return Some(release.line.clone());
        }
        let c = req.comparators.first()?;
        status
            .lines
            .iter()
            .filter(|l| l.kind == r.kind && l.channel == r.channel && supported(l, self.now()))
            .find(|l| l.line == c.major.to_string())
            .map(|l| l.line.clone())
            .or_else(|| self.recommended(r.kind).map(|c| c.line))
    }
    // Resolve inner
    fn resolve_inner(
        &self,
        plugin: &str,
        r: &RuntimeRequirement,
        external_probe: bool,
    ) -> Result<(RuntimeInstallation, bool), (&'static str, String)> {
        let status = self.status();
        let explicit = status.plugins.get(plugin);
        let selection = explicit.cloned().or_else(|| self.recommended(r.kind));
        let overridden = explicit.is_some_and(|s| s.acknowledge_override);
        if explicit.is_some_and(|s| s.kind != r.kind || s.channel != r.channel) {
            return Err((
                "conflict",
                "Runtime override cannot change the declared runtime family/channel".into(),
            ));
        }
        let key = serde_json::to_string(&(plugin, r.kind, &r.channel, &r.version)).unwrap();
        let pinned_line = self
            .state
            .lock()
            .unwrap()
            .saved
            .resolved_lines
            .get(&key)
            .cloned();
        let req = if overridden {
            None
        } else {
            r.version
                .as_deref()
                .map(version_requirement)
                .transpose()
                .map_err(|e| ("invalid", e))?
        };
        // Environment variables are explicit source selections; PATH candidates are never automatic
        let external = if let Some(explicit) = explicit {
            explicit.external.as_ref().map(PathBuf::from)
        } else {
            std::env::var_os(r.kind.env())
                .map(PathBuf::from)
                .or_else(|| {
                    selection
                        .as_ref()
                        .and_then(|s| s.external.as_ref().map(PathBuf::from))
                })
        };
        let selected = if let Some(path) = external {
            if !external_probe {
                return Err(("unverified", "An external interpreter is selected. Validate it explicitly in Manage runtime; passive setup checks never execute it.".into()));
            }
            self.probe_external(r.kind, &path)
                .map_err(|e| ("missing", e))?
        } else {
            let mut candidates: Vec<_> = status
                .installations
                .into_iter()
                .filter(|i| i.kind == r.kind && i.channel == r.channel)
                .filter(|i| {
                    if let Some(explicit) = explicit {
                        explicit
                            .installation_id
                            .as_ref()
                            .map_or(i.line == explicit.line, |id| i.id == *id)
                    } else if let Some(req) = &req {
                        pinned_line.as_ref().is_none_or(|line| i.line == *line)
                            && semver::Version::parse(&i.version).is_ok_and(|v| req.matches(&v))
                    } else {
                        selection.as_ref().is_some_and(|s| {
                            i.line == s.line
                                && s.installation_id.as_ref().is_none_or(|id| i.id == *id)
                        })
                    }
                })
                .collect();
            candidates.sort_by_key(|i| semver::Version::parse(&i.version).ok());
            candidates.pop().filter(|i|Path::new(&i.executable).is_file()).ok_or(("missing","A compatible runtime is required. Select a supported line and install privately or choose an existing executable.".into()))?
        };
        let v =
            semver::Version::parse(&selected.version).map_err(|e| ("invalid", e.to_string()))?;
        if selected.channel != r.channel {
            return Err((
                "channel_mismatch",
                format!(
                    "Selected {} build is {}, plugin requires {}",
                    r.kind.name(),
                    selected.channel,
                    r.channel
                ),
            ));
        }
        if !eligible(&status.lines, r.kind, &r.channel, &v, self.now()) {
            return Err(("unsupported","Runtime is EOL or its support is unknown; select a supported override and acknowledge compatibility.".into()));
        }
        if req.is_some_and(|req| !req.matches(&v)) {
            return Err(("version_mismatch","Selected runtime does not satisfy the manifest. A supported override requires acknowledgement.".into()));
        }
        Ok((selected, overridden))
    }
    // Select for the current operation
    pub fn select(
        &self,
        plugin: Option<&str>,
        choice: RuntimeSelection,
        requirement: Option<&RuntimeRequirement>,
    ) -> Result<(), String> {
        if choice.channel != choice.kind.channel() {
            return Err("Unsupported runtime channel".into());
        }
        if let Some(r) = requirement {
            if choice.kind != r.kind || choice.channel != r.channel {
                return Err("Cannot substitute a different runtime family/channel".into());
            }
        }
        let status = self.status();
        if !status.lines.iter().any(|l| {
            l.kind == choice.kind
                && l.channel == choice.channel
                && l.line == choice.line
                && supported(l, self.now())
        }) {
            return Err("Selected release line is not supported".into());
        }
        if choice.external.is_some() && choice.installation_id.is_some() {
            return Err("Choose one runtime source".into());
        }
        if let Some(path) = &choice.external {
            let p = probe(choice.kind, Path::new(path))?;
            if p.line != choice.line || p.channel != choice.channel {
                return Err("External executable does not match selected line/channel".into());
            }
        }
        if let Some(id) = &choice.installation_id {
            if !status.installations.iter().any(|i| {
                i.id == *id
                    && i.kind == choice.kind
                    && i.line == choice.line
                    && i.channel == choice.channel
            }) {
                return Err("Unknown runtime installation ID".into());
            }
        }
        if let Some(r) = requirement {
            if !choice.acknowledge_override {
                if let Some(req) = r.version.as_deref().map(version_requirement).transpose()? {
                    let candidate = if let Some(path) = &choice.external {
                        Some(probe(choice.kind, Path::new(path))?.version)
                    } else {
                        status
                            .installations
                            .iter()
                            .filter(|i| {
                                i.kind == choice.kind
                                    && i.channel == choice.channel
                                    && i.line == choice.line
                                    && choice.installation_id.as_ref().is_none_or(|id| i.id == *id)
                            })
                            .max_by_key(|i| semver::Version::parse(&i.version).ok())
                            .map(|i| i.version.clone())
                            .or_else(|| {
                                status
                                    .releases
                                    .iter()
                                    .find(|i| {
                                        i.kind == choice.kind
                                            && i.channel == choice.channel
                                            && i.line == choice.line
                                    })
                                    .map(|i| i.version.clone())
                            })
                    };
                    if candidate
                        .as_deref()
                        .and_then(|v| semver::Version::parse(v).ok())
                        .is_none_or(|v| !req.matches(&v))
                    {
                        return Err("Acknowledge the compatibility warning before overriding the manifest requirement".into());
                    }
                }
            }
        }
        self.save(|s| {
            if let Some(plugin) = plugin {
                s.plugins.insert(plugin.into(), choice);
            } else {
                s.defaults.insert(choice.kind.name().into(), choice);
            }
        })
    }
    // Inspect an externally installed runtime
    fn probe_external(
        &self,
        kind: RuntimeKind,
        path: &Path,
    ) -> Result<RuntimeInstallation, String> {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        let modified = meta.modified().map_err(|e| e.to_string())?;
        let key = (kind, path.to_path_buf());
        if let Some((stamp, len, cached)) = self.probes.lock().unwrap().get(&key) {
            if *stamp == modified && *len == meta.len() {
                return Ok(cached.clone());
            }
        }
        let installation = probe(kind, path)?;
        self.probes
            .lock()
            .unwrap()
            .insert(key, (modified, meta.len(), installation.clone()));
        Ok(installation)
    }
    // Collect runtime candidates satisfying the requirement
    pub fn candidates(&self, kind: RuntimeKind) -> Vec<RuntimeInstallation> {
        let status = self.status();
        let mut paths: Vec<PathBuf> = status
            .defaults
            .values()
            .chain(status.plugins.values())
            .filter(|s| s.kind == kind)
            .filter_map(|s| s.external.as_ref().map(PathBuf::from))
            .collect();
        if let Some(path) = std::env::var_os(kind.env()) {
            paths.push(PathBuf::from(path));
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let p = dir.join(kind.executable());
                if p.is_absolute() && p.is_file() {
                    paths.push(p);
                }
            }
        }
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .take(16)
            .filter_map(|p| probe(kind, &p).ok())
            .filter(|i| {
                semver::Version::parse(&i.version)
                    .is_ok_and(|v| eligible(&self.status().lines, kind, &i.channel, &v, self.now()))
            })
            .collect()
    }
    // Start install
    pub fn start_install(self: &Arc<Self>, id: &str) -> Result<String, String> {
        let release = self
            .status()
            .releases
            .into_iter()
            .find(|r| r.id == id)
            .ok_or("Unknown runtime release ID; check for updates")?;
        self.start_request(InstallRequest { release })
    }
    // Retry install
    pub fn retry_install(
        self: &Arc<Self>,
        job_id: &str,
        release_id: &str,
    ) -> Result<String, String> {
        {
            let state = self.state.lock().unwrap();
            let (job, _) = state
                .jobs
                .get(job_id)
                .ok_or("Runtime job no longer retained; choose the release again")?;
            if job.state != "failed" || job.release_id != release_id {
                return Err("Retry must identify the original failed runtime artifact".into());
            }
        }
        let request = self
            .install_requests
            .lock()
            .unwrap()
            .get(job_id)
            .cloned()
            .ok_or("Runtime request no longer retained")?;
        self.start_request(request)
    }
    // Start request
    fn start_request(self: &Arc<Self>, request: InstallRequest) -> Result<String, String> {
        let release = request.release.clone();
        let id = release.id.as_str();
        let cancel = Arc::new(AtomicBool::new(false));
        let job_id = uuid::Uuid::new_v4().to_string();
        {
            let mut s = self.state.lock().unwrap();
            if let Some((j, _)) = s
                .jobs
                .values()
                .find(|(j, _)| j.release_id == id && j.state == "installing")
            {
                return Ok(j.id.clone());
            }
            if s.saved.installations.iter().any(|i| i.id == id) {
                return Err("Runtime is already installed".into());
            }
            // Do not reuse an installation ID while its committed removal still
            // owns files; otherwise crash recovery could mistake it for a rollback.
            removal::prepare_install(&self.root, id)?;
            s.jobs.retain(|_, (j, _)| j.state == "installing");
            self.user_cancelled
                .lock()
                .unwrap()
                .retain(|id| s.jobs.contains_key(id));
            self.install_requests
                .lock()
                .unwrap()
                .retain(|id, _| s.jobs.contains_key(id));
            s.jobs.insert(
                job_id.clone(),
                (
                    RuntimeJob {
                        id: job_id.clone(),
                        release_id: id.into(),
                        state: "installing".into(),
                        downloaded: 0,
                        total: None,
                        error: None,
                        kind: Some(release.kind),
                        line: Some(release.line.clone()),
                        version: Some(release.version.clone()),
                    },
                    cancel.clone(),
                ),
            );
            self.install_requests
                .lock()
                .unwrap()
                .insert(job_id.clone(), request);
        }
        let service = self.clone();
        let job = job_id.clone();
        std::thread::spawn(move || {
            let control = || {
                if !cancel.load(Ordering::SeqCst) {
                    stream_dock_manager_downloads::Control::Continue
                } else if service.user_cancelled.lock().unwrap().contains(&job) {
                    stream_dock_manager_downloads::Control::Cancel
                } else {
                    stream_dock_manager_downloads::Control::Pause
                }
            };
            let progress = |downloaded, total| {
                if cancel.load(Ordering::SeqCst) {
                    return Err("Installation cancelled".into());
                }
                let mut s = service.state.lock().unwrap();
                if let Some((j, _)) = s.jobs.get_mut(&job) {
                    j.downloaded = downloaded;
                    j.total = total;
                }
                Ok(())
            };
            let mut result = crate::installer::install(&service, &release, &control, &progress)
                .and_then(|i| {
                    let target = service.root.join(release.kind.name()).join(&release.id);
                    let result = service.save(|s| {
                        s.installations.push(i);
                    });
                    if result.is_err() {
                        let _ = std::fs::remove_dir_all(target);
                    }
                    result
                });
            if result.is_err() && control() == stream_dock_manager_downloads::Control::Cancel {
                if let Err(error) = service
                    .downloads
                    .discard_identity(&crate::installer::download_identity(&release))
                {
                    result = Err(format!(
                        "Installation cancelled; partial cleanup failed: {error}"
                    ));
                }
            }
            let mut s = service.state.lock().unwrap();
            if let Some((j, _)) = s.jobs.get_mut(&job) {
                match result {
                    Ok(()) => j.state = "complete".into(),
                    Err(e) => {
                        j.state = if cancel.load(Ordering::SeqCst) {
                            "cancelled"
                        } else {
                            "failed"
                        }
                        .into();
                        j.error = Some(e);
                    }
                }
            }
        });
        Ok(job_id)
    }
    // Cancel for the current operation
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let s = self.state.lock().unwrap();
        let (_, cancel) = s.jobs.get(id).ok_or("Unknown runtime job ID")?;
        self.user_cancelled.lock().unwrap().insert(id.into());
        cancel.store(true, Ordering::SeqCst);
        Ok(())
    }
    // Pause pending runtime downloads
    pub fn pause_downloads(&self) {
        for (_, cancel) in self.state.lock().unwrap().jobs.values() {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}
/// Runtime flags are generated from validated declarations, never accepted from plugin IPC
pub fn launch_command(
    runtime: &RuntimeInstallation,
    r: &RuntimeRequirement,
    _package: &Path,
    entry: &Path,
    _port: u16,
) -> Result<std::process::Command, String> {
    if runtime.kind != r.kind || runtime.channel != "lts" || r.channel != "lts" {
        return Err("Only Node.js LTS runtimes are supported".into());
    }
    let mut command = std::process::Command::new(&runtime.executable);
    command.env_remove("NODE_OPTIONS").env_remove("NODE_PATH");
    // Node.js 22 does not resolve Windows verbatim drive paths
    command.arg(dunce::simplified(entry));
    Ok(command)
}

#[cfg(test)]
mod download_retry_tests {
    use super::*;
    struct Offline;
    impl Transport for Offline {
        // Download for the current operation
        fn download(
            &self,
            _: RuntimeKind,
            spec: &stream_dock_manager_downloads::ArtifactSpec,
            cache: &stream_dock_manager_downloads::DownloadCache,
            control: &dyn Fn() -> stream_dock_manager_downloads::Control,
            progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
        ) -> Result<stream_dock_manager_downloads::Artifact, String> {
            let _ = (spec, cache, control, progress);
            Err("Metadata-only fixture does not download artifacts".into())
        }

        // Retrieve the requested value
        fn get(
            &self,
            _: RuntimeKind,
            _: &str,
            _: bool,
            _: usize,
            _: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
        ) -> Result<Vec<u8>, String> {
            Err("offline fixture".into())
        }
    }
    #[test]
    // Verify runtime retry freezes the original release and URL
    fn runtime_retry_freezes_the_original_release_and_url() {
        let root = tempfile::tempdir().unwrap();
        let service = RuntimeService::with_dependencies(
            root.path().into(),
            Arc::new(SystemClock),
            Arc::new(Offline),
        )
        .unwrap();
        let original = RuntimeRelease {
            id: "original".into(),
            kind: RuntimeKind::Nodejs,
            channel: "lts".into(),
            line: "24".into(),
            version: "24.9.0".into(),
            platform: "fixture".into(),
            url: "https://nodejs.org/dist/v24.9.0/node-v24.9.0.tar.gz".into(),
            archive_name: "node-v24.9.0.tar.gz".into(),
            executable: RuntimeKind::Nodejs.executable().into(),
            verification: "official-https".into(),
            provenance: "fixture".into(),
        };
        service.install_requests.lock().unwrap().insert(
            "failed".into(),
            InstallRequest {
                release: original.clone(),
            },
        );
        service.state.lock().unwrap().jobs.insert(
            "failed".into(),
            (
                RuntimeJob {
                    id: "failed".into(),
                    release_id: original.id.clone(),
                    state: "failed".into(),
                    downloaded: 100,
                    total: None,
                    error: Some("interrupted".into()),
                    kind: Some(original.kind),
                    line: Some(original.line.clone()),
                    version: Some(original.version.clone()),
                },
                Arc::new(AtomicBool::new(false)),
            ),
        );
        let mut latest = original.clone();
        latest.id = "newest".into();
        latest.version = "24.10.0".into();
        service.state.lock().unwrap().saved.releases = vec![latest];
        assert!(service.retry_install("failed", "newest").is_err());
        let job = service.retry_install("failed", &original.id).unwrap();
        {
            let requests = service.install_requests.lock().unwrap();
            assert_eq!(requests[&job].release.version, "24.9.0");
            assert_eq!(requests[&job].release.url, original.url);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let state = service.state.lock().unwrap().jobs[&job].0.state.clone();
            if state != "installing" {
                assert_eq!(state, "failed");
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(service.status().installations.is_empty());
        assert!(service.retry_install("missing", &original.id).is_err());
    }
    #[test]
    // Verify byte transport cannot be used for official runtime archives
    fn byte_transport_cannot_be_used_for_official_runtime_archives() {
        assert!(OfficialTransport
            .get(
                RuntimeKind::Nodejs,
                "https://nodejs.org/dist/v24.9.0/archive.tar.gz",
                true,
                256 * 1024 * 1024,
                &|_, _| Ok(())
            )
            .unwrap_err()
            .contains("metadata bounds"));
    }
}

#[cfg(test)]
mod passive_resolution_tests {
    use super::*;
    #[test]
    // Verify passive resolution never executes external sources or saves choices
    fn passive_resolution_never_executes_external_sources_or_saves_choices() {
        let root = tempfile::tempdir().unwrap();
        let service = RuntimeService::open(root.path().into()).unwrap();
        let requirement = RuntimeRequirement {
            kind: RuntimeKind::Nodejs,
            channel: "lts".into(),
            version: Some("24".into()),
        };
        let before = serde_json::to_value(&service.state.lock().unwrap().saved).unwrap();
        let _ = service.inspect_resolution("org.test.absent", &requirement);
        assert_eq!(
            before,
            serde_json::to_value(&service.state.lock().unwrap().saved).unwrap()
        );
        service.state.lock().unwrap().saved.plugins.insert(
            "org.test.external".into(),
            RuntimeSelection {
                kind: RuntimeKind::Nodejs,
                channel: "lts".into(),
                line: "24".into(),
                installation_id: None,
                external: Some(root.path().join("never-execute").to_string_lossy().into()),
                acknowledge_override: false,
            },
        );
        let before = serde_json::to_value(&service.state.lock().unwrap().saved).unwrap();
        let result = service.inspect_resolution("org.test.external", &requirement);
        assert_eq!(result.state, "unverified");
        assert!(result.selected.is_none());
        assert_eq!(
            before,
            serde_json::to_value(&service.state.lock().unwrap().saved).unwrap()
        );
        assert!(service.probes.lock().unwrap().is_empty());
    }
}

/// Validate runtime preferences without launching probes, networking, or background workers
pub fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let Some(value) = stream_dock_manager_storage::read_json::<serde_json::Value>(
        connection,
        stream_dock_manager_storage::Domain::RuntimeState,
        "preferences",
        16 * 1024 * 1024,
    )?
    else {
        return Ok(());
    };
    if !value.is_object() {
        return Err("Invalid saved runtime state".into());
    }
    let saved: Saved = serde_json::from_value(value).map_err(|error| {
        format!("Unsupported or invalid saved runtime state; original preserved: {error}")
    })?;
    if saved.lines.len() > 4096
        || saved.releases.len() > 4096
        || saved.installations.len() > 4096
        || saved.plugins.len() > 4096
        || saved.resolved_lines.len() > 4096
        || saved.defaults.len() > 1
    {
        return Err("Saved runtime index exceeds bounds".into());
    }
    if saved.defaults.keys().any(|key| key != "nodejs") {
        return Err("Unsupported runtime default kind; original state preserved".into());
    }
    for key in saved.resolved_lines.keys() {
        if serde_json::from_str::<serde_json::Value>(key)
            .ok()
            .and_then(|value| value.get(1).cloned())
            .is_some_and(|kind| kind == "deno" || kind == "bun")
        {
            return Err("Unsupported resolved runtime kind; original state preserved".into());
        }
    }
    for (key, selection) in saved.defaults.iter().chain(saved.plugins.iter()) {
        if key.len() > 256
            || selection.channel.len() > 128
            || selection.line.len() > 128
            || selection.external.as_ref().is_some_and(|v| v.len() > 4096)
            || selection
                .installation_id
                .as_ref()
                .is_some_and(|v| v.len() > 512)
        {
            return Err("Invalid saved runtime selection".into());
        }
    }
    for installation in saved.installations {
        if installation.id.len() > 512
            || installation.executable.len() > 4096
            || installation.version.len() > 128
        {
            return Err("Invalid saved runtime installation".into());
        }
    }
    Ok(())
}
