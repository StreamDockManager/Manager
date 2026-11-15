//! Process-session captures. Producers only enqueue; one worker owns all disk writes
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{mpsc, Arc, Mutex, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const BATCH: usize = 64 * 1024;
const QUEUE: usize = 1024 * 1024;
const FRAME: usize = 8 * 1024;
const PLUGIN_LIMIT: u64 = 256 * 1024 * 1024;
const SESSION_LIMIT: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default)]
pub struct LogPluginPreferences {
    pub enabled: bool,
    pub stdout: bool,
    pub stderr: bool,
    pub debug_network: bool,
}
impl Default for LogPluginPreferences {
    // Construct the default value
    fn default() -> Self {
        Self {
            enabled: true,
            stdout: true,
            stderr: true,
            debug_network: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default)]
pub struct LogPreferences {
    pub enabled: bool,
    pub plugins: BTreeMap<String, LogPluginPreferences>,
}
impl Default for LogPreferences {
    // Construct the default value
    fn default() -> Self {
        Self {
            enabled: true,
            plugins: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct LogCaptureStatus {
    pub id: String,
    pub plugin_id: String,
    pub name: String,
    pub capturing: bool,
    pub waiting: bool,
    pub incomplete: bool,
    pub error: Option<String>,
    #[ts(type = "number")]
    pub generation: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub sequence: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct LogPluginIdentity {
    pub id: String,
    pub name: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default)]
pub struct LogNetworkBody {
    pub text: String,
    pub encoding: String,
    pub content_type: String,
    #[ts(type = "number")]
    pub bytes: u64,
    pub complete: bool,
    pub truncated: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default)]
pub struct LogNetworkHeader {
    pub name: String,
    pub values: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default)]
pub struct LogNetworkHeaders {
    pub entries: Vec<LogNetworkHeader>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct LogNetworkDetail {
    pub request_id: String,
    pub request: LogNetworkBody,
    pub response: LogNetworkBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub request_headers: Option<LogNetworkHeaders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub response_headers: Option<LogNetworkHeaders>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct LogEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cursor: Option<LogCursor>,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub timestamp_ms: u64,
    pub stream: String,
    pub source: String,
    pub run_id: String,
    pub message: String,
    pub continuation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub network_request_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiskRecord {
    #[serde(flatten)]
    entry: LogEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    network: Option<LogNetworkDetail>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct LogCursor {
    pub capture_id: String,
    #[ts(type = "number")]
    pub generation: u64,
    #[ts(type = "number")]
    pub offset: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct LogPage {
    pub status: LogCaptureStatus,
    pub entries: Vec<LogEntry>,
    pub earlier: Option<LogCursor>,
    pub next: LogCursor,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct LogReaderLease {
    pub id: String,
    pub capture_id: String,
    #[ts(type = "number")]
    pub generation: u64,
}
struct Pending {
    status: LogCaptureStatus,
    entries: VecDeque<DiskRecord>,
    queued_bytes: usize,
}
#[derive(Default)]
struct Shared {
    captures: BTreeMap<String, Pending>,
    preferences: LogPreferences,
    secrets: Vec<String>,
    redaction_values: Vec<String>,
    closed: bool,
    readers: BTreeMap<String, LogReaderLease>,
    reader_owners: BTreeMap<String, String>,
    #[cfg(test)]
    writer_calls: u64,
    #[cfg(test)]
    reader_calls: u64,
}
struct Disk {
    file: File,
    index: Vec<(u64, u64)>,
    records: u64,
    staged: Vec<u8>,
    staged_index: Vec<(u64, u64)>,
}
impl Disk {
    // Construct the value and initialize its state
    fn new(file: File) -> Self {
        Self {
            file,
            index: vec![(0, 0)],
            records: 0,
            staged: Vec::with_capacity(BATCH + FRAME),
            staged_index: Vec::new(),
        }
    }
}

type Reply<T> = mpsc::SyncSender<Result<T, String>>;
enum Request {
    Wake,
    Start(String, String, Reply<LogCaptureStatus>),
    Stop(String, Reply<()>),
    Resume(String, Reply<()>),
    Clear(String, Reply<()>),
    Page(
        String,
        Option<LogCursor>,
        bool,
        Option<LogReaderLease>,
        Reply<LogPage>,
    ),
    Network(LogReaderLease, LogCursor, Reply<LogNetworkDetail>),
    Export(String, Reply<(File, u64)>),
    Shutdown(Reply<()>),
}
struct PipeBuffer {
    plugin: String,
    run: String,
    stream: String,
    pending: Vec<u8>,
}
pub struct LogCaptureService {
    pipes: Mutex<Vec<Weak<Mutex<PipeBuffer>>>>,
    preference_write: Mutex<()>,
    shared: Arc<Mutex<Shared>>,
    sender: mpsc::SyncSender<Request>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    storage: stream_dock_manager_storage::Storage,
}
impl LogCaptureService {
    // Open for the current operation
    pub fn open(preferences_path: std::path::PathBuf) -> Result<Arc<Self>, String> {
        let root = preferences_path
            .parent()
            .ok_or("Invalid preferences path")?;
        let storage = stream_dock_manager_storage::Storage::open(root.to_path_buf())?;
        Self::with_storage(storage)
    }
    // Construct the service with explicit storage
    pub fn with_storage(
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Arc<Self>, String> {
        let preferences = match storage.read_json::<serde_json::Value>(
            stream_dock_manager_storage::Domain::LoggingPreferences,
            "preferences",
            1024 * 1024,
        )? {
            None => LogPreferences::default(),
            Some(value) if value.is_object() => {
                serde_json::from_value(value).map_err(|_| "Invalid saved logging preferences")?
            }
            Some(_) => return Err("Invalid saved logging preferences".into()),
        };
        let shared = Arc::new(Mutex::new(Shared {
            preferences,
            ..Default::default()
        }));
        let (sender, receiver) = mpsc::sync_channel(32);
        let state = shared.clone();
        let log_directory = storage.root().join("logs");
        let worker = std::thread::Builder::new().name("plugin-log-writer".into()).spawn(move || {
            let mut directory: Option<tempfile::TempDir> = None;
            let mut files = BTreeMap::<String, Disk>::new();
            let mut deadline = std::time::Instant::now() + Duration::from_millis(250);
            loop {
                let active = state.lock().unwrap().captures.values().any(|p| p.status.capturing || !p.entries.is_empty()) || files.values().any(|disk| !disk.staged.is_empty());
                let request = if active { receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) } else { receiver.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected) };
                let force = !matches!(request, Ok(Request::Wake)) || std::time::Instant::now() >= deadline;
                flush(&state, &mut files, force);
                if force { deadline = std::time::Instant::now() + Duration::from_millis(250); }
                match request {
                    Ok(Request::Wake) | Err(mpsc::RecvTimeoutError::Timeout) => {},
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(Request::Start(plugin, name, reply)) => {
                        let result = (|| {
                            let mut s = state.lock().unwrap();
                            let prefs = s.preferences.plugins.get(&plugin).cloned().unwrap_or_default();
                            if !s.preferences.enabled || !prefs.enabled { return Err("Plugin logging is disabled in preferences.".into()); }
                            if let Some(p) = s.captures.get(&plugin) { return Ok(p.status.clone()); }
                            let id = uuid::Uuid::new_v4().to_string();
                            if directory.is_none() { directory = Some(private_directory(&log_directory)?); }
                            let path = directory.as_ref().unwrap().path().join(&id);
                            let file = OpenOptions::new().create_new(true).read(true).append(true).open(path).map_err(|e| e.to_string())?;
                            let status = LogCaptureStatus { id, plugin_id: plugin.clone(), name, capturing: true, waiting: true, incomplete: false, error: None, generation: 1, bytes: 0, sequence: 0 };
                            files.insert(plugin.clone(), Disk::new(file));
                            s.captures.insert(plugin, Pending { status: status.clone(), entries: VecDeque::new(), queued_bytes: 0 });
                            Ok(status)
                        })();
                        let _ = reply.send(result);
                    },
                    Ok(Request::Stop(id, reply)) => {
                        let result = find_mut(&mut state.lock().unwrap(), &id).map(|p| { p.status.capturing = false; });
                        // Freeze acceptance first, then flush every previously accepted entry
                        flush(&state, &mut files, true);
                        let _ = reply.send(result);
                    },
                    Ok(Request::Resume(id, reply)) => {
                        let mut s = state.lock().unwrap();
                        let result = (|| {
                            let plugin = s.captures.iter().find(|(_,p)| p.status.id == id).map(|(k,_)| k.clone()).ok_or("Capture no longer exists")?;
                            let prefs = s.preferences.plugins.get(&plugin).cloned().unwrap_or_default();
                            if !s.preferences.enabled || !prefs.enabled { return Err("Plugin logging is disabled in preferences.".into()); }
                            let p = s.captures.get_mut(&plugin).unwrap();
                            if p.status.incomplete { return Err("Download or clear this incomplete capture before starting another.".into()); }
                            if !p.status.capturing { p.status.capturing = true; enqueue(p, "stdout", "manager", "", "Capture resumed", false); }
                            Ok(())
                        })();
                        let _ = reply.send(result);
                    },
                    Ok(Request::Clear(id, reply)) => {
                        let result = (|| {
                            let mut s = state.lock().unwrap();
                            let plugin = s.captures.iter().find(|(_,p)| p.status.id == id).map(|(k,_)| k.clone()).ok_or("Capture no longer exists")?;
                            if s.captures[&plugin].status.capturing { return Err("Stop capture before clearing captured logs.".into()); }
                            let disk = files.remove(&plugin).ok_or("Capture file missing")?;
                            drop(disk);
                            std::fs::remove_file(directory.as_ref().ok_or("Capture directory missing")?.path().join(&id)).map_err(|e| e.to_string())?;
                            s.captures.remove(&plugin);
                            Ok(())
                        })();
                        let _ = reply.send(result);
                    },
                    Ok(Request::Page(id, cursor, forward, lease, reply)) => {
                        let valid = lease.is_none_or(|lease| state.lock().unwrap().readers.get(&lease.id).is_some_and(|current| current.capture_id == lease.capture_id && current.generation == lease.generation));
                        let result = if valid { read_page(&state, &mut files, &id, cursor, forward) } else { Err("Log reader is closed".into()) };
                        let _ = reply.send(result);
                    },
                    Ok(Request::Network(lease, cursor, reply)) => {
                        let _ = reply.send(read_network_detail(&state, &mut files, &lease, &cursor));
                    },
                    Ok(Request::Export(id, reply)) => {
                        let result = (|| {
                            let s = state.lock().unwrap();
                            let p = s.captures.values().find(|p| p.status.id == id).ok_or("Capture no longer exists")?;
                            let disk = files.get(&p.status.plugin_id).ok_or("Capture file missing")?;
                            // Independent file offset; later writes can proceed during export
                            let file = File::open(directory.as_ref().ok_or("Capture directory missing")?.path().join(&id)).map_err(|e| e.to_string())?;
                            Ok((file, disk.file.metadata().map_err(|e| e.to_string())?.len()))
                        })();
                        let _ = reply.send(result);
                    },
                    Ok(Request::Shutdown(reply)) => { let _ = reply.send(Ok(())); break; },
                }
            }
            drop(files);
            drop(directory);
        }).ok();
        Ok(Arc::new(Self {
            pipes: Mutex::new(Vec::new()),
            preference_write: Mutex::new(()),
            shared,
            sender,
            worker: Mutex::new(worker),
            storage,
        }))
    }
    // Process the request through this subsystem
    fn request<T>(&self, make: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender
            .send(make(tx))
            .map_err(|_| "Log writer stopped")?;
        rx.recv().map_err(|_| "Log writer stopped")?
    }
    /// Only readers owned by this window prevent that window's retirement
    /// Background capture and readers in other windows keep independent lifetimes
    pub fn has_readers_for_owner(&self, owner: &str) -> bool {
        self.shared
            .try_lock()
            .map(|state| state.reader_owners.values().any(|value| value == owner))
            .unwrap_or(true)
    }
    // List the entries tracked by this subsystem
    pub fn list(&self) -> Vec<LogCaptureStatus> {
        self.shared
            .lock()
            .unwrap()
            .captures
            .values()
            .map(|p| p.status.clone())
            .collect()
    }
    // Subscribe for the current operation
    pub fn subscribe(&self, id: &str) -> Result<LogReaderLease, String> {
        self.subscribe_owned(id, "")
    }
    // Subscribe owned
    pub fn subscribe_owned(&self, id: &str, owner: &str) -> Result<LogReaderLease, String> {
        let mut s = self.shared.lock().unwrap();
        if s.readers.len() >= 16 {
            return Err("Too many log readers".into());
        }
        let p = s
            .captures
            .values()
            .find(|p| p.status.id == id)
            .ok_or("Capture no longer exists")?;
        let lease = LogReaderLease {
            id: uuid::Uuid::new_v4().to_string(),
            capture_id: id.into(),
            generation: p.status.generation,
        };
        s.readers.insert(lease.id.clone(), lease.clone());
        s.reader_owners.insert(lease.id.clone(), owner.into());
        Ok(lease)
    }
    // Unsubscribe for the current operation
    pub fn unsubscribe(&self, id: &str) {
        let mut s = self.shared.lock().unwrap();
        s.readers.remove(id);
        s.reader_owners.remove(id);
    }
    // Unsubscribe owner
    pub fn unsubscribe_owner(&self, owner: &str) {
        let mut s = self.shared.lock().unwrap();
        let ids = s
            .reader_owners
            .iter()
            .filter(|(_, value)| *value == owner)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            s.readers.remove(&id);
            s.reader_owners.remove(&id);
        }
    }
    // Read for the current operation
    pub fn read(
        &self,
        lease: &LogReaderLease,
        cursor: Option<LogCursor>,
        forward: bool,
    ) -> Result<LogPage, String> {
        {
            let s = self.shared.lock().unwrap();
            if !s.readers.get(&lease.id).is_some_and(|l| {
                l.capture_id == lease.capture_id && l.generation == lease.generation
            }) {
                return Err("Log reader is closed".into());
            }
        }
        self.request(|tx| {
            Request::Page(
                lease.capture_id.clone(),
                cursor,
                forward,
                Some(lease.clone()),
                tx,
            )
        })
    }
    // Read the log capture preferences
    pub fn preferences(&self) -> LogPreferences {
        self.shared.lock().unwrap().preferences.clone()
    }
    // Check whether detailed network capture is enabled
    pub fn debug_network(&self, plugin: &str) -> bool {
        let s = self.shared.lock().unwrap();
        s.preferences.enabled
            && s.preferences
                .plugins
                .get(plugin)
                .is_some_and(|p| p.enabled && p.debug_network)
            && s.captures.get(plugin).is_some_and(|p| p.status.capturing)
    }
    // Save preferences
    pub fn save_preferences(&self, preferences: LogPreferences) -> Result<(), String> {
        let _write = self.preference_write.lock().unwrap();
        self.flush_pipes(None, false);
        self.storage.write_json(
            stream_dock_manager_storage::Domain::LoggingPreferences,
            "preferences",
            &preferences,
            1024 * 1024,
        )?;
        let ids = {
            let mut s = self.shared.lock().unwrap();
            s.preferences = preferences;
            let disabled = s
                .captures
                .iter()
                .filter(|(plugin, _)| {
                    !s.preferences.enabled
                        || !s
                            .preferences
                            .plugins
                            .get(*plugin)
                            .cloned()
                            .unwrap_or_default()
                            .enabled
                })
                .map(|(_, p)| p.status.id.clone())
                .collect::<Vec<_>>();
            for id in &disabled {
                find_mut(&mut s, id)?.status.capturing = false;
            }
            disabled
        };
        for id in ids {
            self.stop(&id)?;
        }
        Ok(())
    }
    // Start for the current operation
    pub fn start(&self, plugin: &str, name: &str) -> Result<LogCaptureStatus, String> {
        self.request(|tx| Request::Start(plugin.into(), name.into(), tx))
    }
    // Stop for the current operation
    pub fn stop(&self, id: &str) -> Result<(), String> {
        let plugin = self
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.plugin_id)
            .ok_or("Capture no longer exists")?;
        self.flush_pipes(Some(&plugin), true);
        self.request(|tx| Request::Stop(id.into(), tx))
    }
    // Resume for the current operation
    pub fn resume(&self, id: &str) -> Result<(), String> {
        self.request(|tx| Request::Resume(id.into(), tx))
    }
    // Clear for the current operation
    pub fn clear(&self, id: &str) -> Result<(), String> {
        self.request(|tx| Request::Clear(id.into(), tx))
    }
    // Read one page of captured log entries
    pub fn page(
        &self,
        id: &str,
        cursor: Option<LogCursor>,
        forward: bool,
    ) -> Result<LogPage, String> {
        self.request(|tx| Request::Page(id.into(), cursor, forward, None, tx))
    }
    // Read the detail for a captured network event
    pub fn network_detail(
        &self,
        lease: &LogReaderLease,
        cursor: &LogCursor,
    ) -> Result<LogNetworkDetail, String> {
        self.request(|tx| Request::Network(lease.clone(), cursor.clone(), tx))
    }
    // Export for the current operation
    pub fn export(&self, id: &str, path: &Path) -> Result<(), String> {
        let plugin = self
            .list()
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.plugin_id)
            .ok_or("Capture no longer exists")?;
        self.flush_pipes(Some(&plugin), false);
        let (file, length) = self.request(|tx| Request::Export(id.into(), tx))?;
        let mut output =
            tempfile::NamedTempFile::new_in(path.parent().ok_or("Invalid export destination")?)
                .map_err(|e| e.to_string())?;
        std::io::copy(&mut file.take(length), &mut output).map_err(|e| e.to_string())?;
        output.persist(path).map_err(|e| e.to_string())?;
        Ok(())
    }
    // Set secrets
    pub fn set_secrets(&self, secrets: Vec<String>) {
        let mut shared = self.shared.lock().unwrap();
        if shared.redaction_values != secrets {
            shared.secrets = crate::reference_settings::redaction_patterns(&secrets);
            shared.redaction_values = secrets;
        }
    }
    // Mark incomplete
    pub fn mark_incomplete(&self, plugin: &str, reason: &str) {
        if self
            .list()
            .iter()
            .any(|p| p.plugin_id == plugin && p.capturing)
        {
            self.flush_pipes(Some(plugin), true);
            if let Some(p) = self.shared.lock().unwrap().captures.get_mut(plugin) {
                fail(p, reason);
            }
            let _ = self.sender.try_send(Request::Wake);
        }
    }
    // Retire the selected state from active use
    pub fn retire(&self, plugin: &str) {
        let capturing = self
            .list()
            .iter()
            .any(|p| p.plugin_id == plugin && p.capturing);
        self.flush_pipes(Some(plugin), true);
        if let Some(p) = self.shared.lock().unwrap().captures.get_mut(plugin) {
            if capturing {
                enqueue(
                    p,
                    "stdout",
                    "manager",
                    "",
                    "Plugin removed; capture stopped",
                    false,
                );
            }
            p.status.capturing = false;
        }
        let _ = self.sender.try_send(Request::Wake);
    }
    // Forget plugin
    pub(super) fn forget_plugin(&self, plugin: &str) -> Result<(), String> {
        let _write = self.preference_write.lock().unwrap();
        let preferences = self
            .storage
            .read_json(
                stream_dock_manager_storage::Domain::LoggingPreferences,
                "preferences",
                1024 * 1024,
            )?
            .unwrap_or_default();
        self.shared.lock().unwrap().preferences = preferences;
        if let Some(capture) = self
            .list()
            .into_iter()
            .find(|capture| capture.plugin_id == plugin)
        {
            self.clear(&capture.id)?;
        }
        Ok(())
    }
    // Flush pipes
    fn flush_pipes(&self, plugin: Option<&str>, freeze: bool) {
        let pipes = {
            let mut pipes = self.pipes.lock().unwrap();
            pipes.retain(|p| p.strong_count() > 0);
            pipes.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        let mut guards = pipes.iter().map(|p| p.lock().unwrap()).collect::<Vec<_>>();
        for pipe in guards
            .iter_mut()
            .filter(|p| plugin.is_none_or(|id| p.plugin == id))
        {
            self.emit_pipe(pipe, true);
        }
        if freeze {
            let mut s = self.shared.lock().unwrap();
            for p in s
                .captures
                .values_mut()
                .filter(|p| plugin.is_none_or(|id| p.status.plugin_id == id))
            {
                p.status.capturing = false;
            }
        }
    }
    // Emit pipe
    fn emit_pipe(&self, pipe: &mut PipeBuffer, force: bool) {
        if pipe.pending.is_empty() {
            return;
        }
        let secrets = self.shared.lock().unwrap().secrets.clone();
        let mut end = match std::str::from_utf8(&pipe.pending) {
            Ok(_) => pipe.pending.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => pipe.pending.len(),
        };
        if force {
            end = pipe.pending.len();
        } else {
            for secret in secrets.iter().filter(|s| !s.is_empty()) {
                let pattern = secret.as_bytes();
                for length in (1..pattern.len().min(pipe.pending.len() + 1)).rev() {
                    if pipe.pending.ends_with(&pattern[..length]) {
                        end = end.min(pipe.pending.len() - length);
                        break;
                    }
                }
            }
            // Retain complete matches too if a shorter suffix overlaps them
            for secret in secrets.iter().filter(|s| !s.is_empty()) {
                for (start, _) in pipe
                    .pending
                    .windows(secret.len())
                    .enumerate()
                    .filter(|(_, w)| *w == secret.as_bytes())
                {
                    if start < end && start + secret.len() > end {
                        end = start;
                    }
                }
            }
        }
        if end > 0 {
            let mut text = String::from_utf8_lossy(&pipe.pending[..end]).into_owned();
            if force
                && secrets
                    .iter()
                    .any(|secret| !text.is_empty() && secret.starts_with(&text))
            {
                text = "[credential]".into();
            }
            self.record(&pipe.plugin, &pipe.run, &pipe.stream, "process", &text);
            pipe.pending.drain(..end);
        }
    }
    /// Capture bytes as they arrive, including output without a newline. The
    /// only retained tail is an incomplete UTF-8 character or credential prefix.
    pub fn drain_pipe(&self, plugin: &str, run: &str, stream: &str, mut pipe: impl Read) {
        let buffer = Arc::new(Mutex::new(PipeBuffer {
            plugin: plugin.into(),
            run: run.into(),
            stream: stream.into(),
            pending: Vec::new(),
        }));
        self.pipes.lock().unwrap().push(Arc::downgrade(&buffer));
        let mut bytes = [0; 4096];
        while let Ok(size) = pipe.read(&mut bytes) {
            if size == 0 {
                break;
            }
            let mut buffer = buffer.lock().unwrap();
            let active = {
                let s = self.shared.lock().unwrap();
                s.captures.get(plugin).is_some_and(|p| p.status.capturing)
            };
            if !active {
                buffer.pending.clear();
                continue;
            }
            buffer.pending.extend_from_slice(&bytes[..size]);
            self.emit_pipe(&mut buffer, false);
        }
        self.emit_pipe(&mut buffer.lock().unwrap(), true);
    }
    // Record for the current operation
    pub fn record(&self, plugin: &str, run: &str, stream: &str, source: &str, message: &str) {
        self.record_entry(plugin, run, stream, source, message, None);
    }
    // Record network
    pub fn record_network(
        &self,
        plugin: &str,
        run: &str,
        stream: &str,
        message: &str,
        detail: LogNetworkDetail,
    ) {
        self.record_entry(plugin, run, stream, "network", message, Some(detail));
    }
    // Record entry
    fn record_entry(
        &self,
        plugin: &str,
        run: &str,
        stream: &str,
        source: &str,
        message: &str,
        detail: Option<LogNetworkDetail>,
    ) {
        let mut s = self.shared.lock().unwrap();
        if s.closed || !s.preferences.enabled {
            return;
        }
        let preferences = s
            .preferences
            .plugins
            .get(plugin)
            .cloned()
            .unwrap_or_default();
        if !preferences.enabled
            || (source == "network" && !preferences.debug_network)
            || (stream == "stderr" && !preferences.stderr)
            || (stream != "stderr" && !preferences.stdout)
        {
            return;
        }
        if !s.captures.get(plugin).is_some_and(|p| p.status.capturing) {
            return;
        }
        let redacted = (s.secrets.len() > 1)
            .then(|| crate::reference_settings::redact_prepared(message, &s.secrets));
        let message = redacted.as_deref().unwrap_or(message);
        let detail = detail.map(|mut detail| {
            detail.request_id =
                crate::reference_settings::redact_prepared(&detail.request_id, &s.secrets);
            detail
                .request_id
                .retain(|c| c.is_ascii_alphanumeric() || c == '-');
            detail.request_id.truncate(80);
            sanitize_headers(&mut detail.request_headers, &s.secrets);
            sanitize_headers(&mut detail.response_headers, &s.secrets);
            sanitize_body(&mut detail.request, &s.secrets);
            sanitize_body(&mut detail.response, &s.secrets);
            detail
        });
        let p = s.captures.get_mut(plugin).unwrap();
        p.status.waiting = false;
        if let Some(detail) = detail {
            let size = detail.request.text.len()
                + detail.response.text.len()
                + header_bytes(&detail.request_headers)
                + header_bytes(&detail.response_headers)
                + 1024;
            if p.queued_bytes + size + message.len() > QUEUE {
                fail(p, "Log queue overloaded; capture is incomplete.");
                return;
            }
            let mut end = message.len().min(FRAME);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            enqueue(p, stream, source, run, &message[..end], false);
            let record = p.entries.back_mut().unwrap();
            record.entry.network_request_id = Some(detail.request_id.clone());
            record.network = Some(detail);
            p.queued_bytes += size;
            if p.queued_bytes >= BATCH {
                let _ = self.sender.try_send(Request::Wake);
            }
            return;
        }
        let mut rest = message;
        let mut continuation = false;
        while !rest.is_empty() {
            let mut end = rest.len().min(FRAME);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            if p.queued_bytes + end + 256 > QUEUE {
                fail(p, "Log queue overloaded; capture is incomplete.");
                break;
            }
            enqueue(p, stream, source, run, &rest[..end], continuation);
            rest = &rest[end..];
            continuation = true;
        }
        if p.queued_bytes >= BATCH {
            let _ = self.sender.try_send(Request::Wake);
        }
    }
    // Shutdown for the current operation
    pub fn shutdown(&self) {
        self.flush_pipes(None, true);
        let mut worker = self.worker.lock().unwrap();
        if let Some(handle) = worker.take() {
            {
                let mut s = self.shared.lock().unwrap();
                s.closed = true;
                for p in s.captures.values_mut() {
                    p.status.capturing = false;
                }
            }
            let _ = self.request(Request::Shutdown);
            let _ = handle.join();
        }
    }
}
impl Drop for LogCaptureService {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.shutdown();
    }
}
// Find the mutable log record for this owner
fn find_mut<'a>(s: &'a mut Shared, id: &str) -> Result<&'a mut Pending, String> {
    s.captures
        .values_mut()
        .find(|p| p.status.id == id)
        .ok_or_else(|| "Capture no longer exists".into())
}
// Queue a record for capture
fn enqueue(
    p: &mut Pending,
    stream: &str,
    source: &str,
    run: &str,
    message: &str,
    continuation: bool,
) {
    p.status.sequence += 1;
    p.queued_bytes += message.len() + 256;
    p.entries.push_back(DiskRecord {
        entry: LogEntry {
            cursor: None,
            sequence: p.status.sequence,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            stream: stream.into(),
            source: source.into(),
            run_id: run.into(),
            message: message.into(),
            continuation,
            network_request_id: None,
        },
        network: None,
    });
}
// Record the failure of the pending operation
fn fail(p: &mut Pending, message: &str) {
    p.status.capturing = false;
    p.status.incomplete = true;
    p.status.error = Some(message.into());
}
// Flush for the current operation
fn flush(shared: &Mutex<Shared>, files: &mut BTreeMap<String, Disk>, force: bool) {
    let mut batches = {
        let mut s = shared.lock().unwrap();
        s.captures
            .iter_mut()
            .map(|(id, p)| {
                p.queued_bytes = 0;
                (id.clone(), std::mem::take(&mut p.entries))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let mut total: u64 = shared
        .lock()
        .unwrap()
        .captures
        .values()
        .map(|p| p.status.bytes)
        .sum();
    for (plugin, disk) in files {
        let entries = batches.remove(plugin).unwrap_or_default();
        if entries.is_empty() && (!force || disk.staged.is_empty()) {
            continue;
        }
        let mut bytes = shared.lock().unwrap().captures[plugin].status.bytes;
        let mut error = None;
        let mut global = false;
        let mut writer_calls = 0_u64;
        let commit = |disk: &mut Disk,
                      bytes: &mut u64,
                      total: &mut u64,
                      calls: &mut u64|
         -> Result<(), ()> {
            if disk.staged.is_empty() {
                return Ok(());
            }
            *calls += 1;
            if disk.file.write_all(&disk.staged).is_err() {
                // A partial uncommitted record is not part of the readable boundary
                let _ = disk.file.set_len(*bytes);
                disk.staged.clear();
                disk.staged_index.clear();
                return Err(());
            }
            *bytes += disk.staged.len() as u64;
            *total += disk.staged.len() as u64;
            disk.index.append(&mut disk.staged_index);
            disk.staged.clear();
            Ok(())
        };
        if total + disk.staged.len() as u64 > SESSION_LIMIT {
            let available = SESSION_LIMIT.saturating_sub(total) as usize;
            let boundary = disk.staged[..available.min(disk.staged.len())]
                .iter()
                .rposition(|c| *c == b'\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            disk.staged.truncate(boundary);
            disk.staged_index
                .retain(|(_, offset)| *offset < bytes + boundary as u64);
            global = true;
            error = Some("Session captures reached their 1 GiB limit.");
        }
        for entry in entries {
            if error.is_some() {
                break;
            }
            let mut line = serde_json::to_vec(&entry).unwrap();
            line.push(b'\n');
            let next = disk.staged.len() as u64 + line.len() as u64;
            if bytes + next > PLUGIN_LIMIT {
                error = Some("Plugin capture reached its 256 MiB limit.");
                break;
            }
            if total + next > SESSION_LIMIT {
                global = true;
                error = Some("Session captures reached their 1 GiB limit.");
                break;
            }
            if disk.records % 128 == 0 {
                disk.staged_index
                    .push((disk.records, bytes + disk.staged.len() as u64));
            }
            disk.records += 1;
            disk.staged.extend(line);
            if disk.staged.len() >= BATCH
                && commit(disk, &mut bytes, &mut total, &mut writer_calls).is_err()
            {
                error = Some("Cannot write captured logs; check free disk space.");
                break;
            }
        }
        if (force || error.is_some())
            && !error.is_some_and(|e| e.starts_with("Cannot write"))
            && commit(disk, &mut bytes, &mut total, &mut writer_calls).is_err()
        {
            error = Some("Cannot write captured logs; check free disk space.");
        }
        let mut s = shared.lock().unwrap();
        #[cfg(test)]
        {
            s.writer_calls += writer_calls;
        }
        let p = s.captures.get_mut(plugin).unwrap();
        p.status.bytes = bytes;
        if let Some(error) = error {
            fail(p, error);
        }
        if global {
            for p in s.captures.values_mut().filter(|p| p.status.capturing) {
                fail(p, "Session captures reached their 1 GiB limit.");
            }
        }
    }
}

// Read page
fn read_page(
    shared: &Mutex<Shared>,
    files: &mut BTreeMap<String, Disk>,
    id: &str,
    cursor: Option<LogCursor>,
    forward: bool,
) -> Result<LogPage, String> {
    #[cfg(test)]
    {
        shared.lock().unwrap().reader_calls += 1;
    }
    let status = shared
        .lock()
        .unwrap()
        .captures
        .values()
        .find(|p| p.status.id == id)
        .map(|p| p.status.clone())
        .ok_or("Capture no longer exists")?;
    if cursor.as_ref().is_some_and(|c| {
        c.capture_id != id || c.generation != status.generation || c.offset > status.bytes
    }) {
        return Err("Stale log cursor".into());
    }
    let disk = files
        .get_mut(&status.plugin_id)
        .ok_or("Capture file missing")?;
    let boundary = cursor.as_ref().map(|c| c.offset).unwrap_or(status.bytes);
    let start = if forward {
        boundary
    } else {
        let i = disk.index.partition_point(|(_, offset)| *offset < boundary);
        disk.index[i.saturating_sub(3)].1
    };
    disk.file
        .seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(&disk.file);
    let mut entries = VecDeque::new();
    let mut offset = start;
    let mut line = String::new();
    let mut first = start;
    while offset < if forward { status.bytes } else { boundary } {
        line.clear();
        let length = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if length == 0 {
            break;
        }
        let entry: LogEntry = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        entries.push_back((offset, entry));
        offset += length as u64;
        if forward && entries.len() == 200 {
            break;
        }
        if !forward && entries.len() > 200 {
            entries.pop_front();
        }
    }
    if let Some((offset, _)) = entries.front() {
        first = *offset;
    }
    let generation = status.generation;
    let cursor = |offset| LogCursor {
        capture_id: id.into(),
        generation,
        offset,
    };
    Ok(LogPage {
        status,
        entries: entries
            .into_iter()
            .map(|(offset, mut entry)| {
                entry.cursor = Some(cursor(offset));
                entry
            })
            .collect(),
        earlier: (first > 0).then(|| cursor(first)),
        next: cursor(offset),
    })
}

#[cfg(test)]
mod tests;

// Recognize headers whose values must be redacted
fn sensitive_header(name: &str) -> bool {
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    name != "accesscontrolallowcredentials"
        && name != "authority"
        && ([
            "auth",
            "cookie",
            "token",
            "secret",
            "password",
            "credential",
            "signature",
            "apikey",
            "csrf",
            "xsrf",
            "hmac",
            "bearer",
            "sessionid",
        ]
        .iter()
        .any(|part| name.contains(part))
            || name == "key"
            || name.ends_with("key")
            || name.ends_with("session"))
}
// Calculate the byte charge of captured headers
fn header_bytes(headers: &Option<LogNetworkHeaders>) -> usize {
    headers.as_ref().map_or(0, |headers| {
        headers
            .entries
            .iter()
            .map(|field| {
                field.name.len()
                    + 32
                    + field
                        .values
                        .iter()
                        .map(|value| value.len() + 8)
                        .sum::<usize>()
            })
            .sum()
    })
}
// Sanitize headers
fn sanitize_headers(headers: &mut Option<LogNetworkHeaders>, secrets: &[String]) {
    let Some(headers) = headers else { return };
    if headers.entries.len() > 128 {
        headers.entries.truncate(128);
        headers.truncated = true;
    }
    let mut used = 0;
    let mut fields = Vec::new();
    for field in std::mem::take(&mut headers.entries) {
        if field.name.len() > 256 {
            headers.truncated = true;
            continue;
        }
        let sensitive = sensitive_header(&field.name);
        let name = crate::reference_settings::redact_prepared(&field.name, secrets);
        let mut cost = name.len() + 32;
        if used + cost > 8192 {
            headers.truncated = true;
            continue;
        }
        let mut values = Vec::new();
        for value in field.values {
            let value = if sensitive {
                "[redacted]".into()
            } else {
                crate::reference_settings::redact_prepared(&value, secrets)
            };
            if used + cost + value.len() + 8 > 8192 {
                headers.truncated = true;
                continue;
            }
            cost += value.len() + 8;
            values.push(value);
        }
        used += cost;
        fields.push(LogNetworkHeader { name, values });
    }
    fields.sort_by_cached_key(|field| field.name.to_ascii_lowercase());
    headers.entries = fields;
}

// Sanitize body
fn sanitize_body(body: &mut LogNetworkBody, secrets: &[String]) {
    use base64::Engine;
    let encoding = &base64::engine::general_purpose::STANDARD;
    let data = if body.encoding == "base64" {
        encoding.decode(&body.text).ok()
    } else if body.encoding == "utf8" {
        Some(body.text.as_bytes().to_vec())
    } else {
        None
    };
    body.content_type = crate::reference_settings::redact_prepared(&body.content_type, secrets);
    let mut end = body.content_type.len().min(256);
    while !body.content_type.is_char_boundary(end) {
        end -= 1;
    }
    body.content_type.truncate(end);
    let Some(data) = data.filter(|data| data.len() <= 64 * 1024) else {
        if body.encoding != "base64" && body.encoding != "utf8" {
            body.encoding = "utf8".into();
        }
        body.text.clear();
        body.truncated = true;
        return;
    };
    // Preserve binary bytes while matching the same prepared credential patterns
    let mut redacted = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if let Some(pattern) = secrets
            .iter()
            .find(|pattern| data[i..].starts_with(pattern.as_bytes()))
        {
            redacted.extend_from_slice(b"[credential]");
            i += pattern.len();
        } else {
            redacted.push(data[i]);
            i += 1;
        }
    }
    body.text = if body.encoding == "base64" {
        encoding.encode(redacted)
    } else {
        String::from_utf8_lossy(&redacted).into_owned()
    };
    if body.text.len() > 128 * 1024 {
        body.text.clear();
        body.truncated = true;
    }
}
// Read network detail
fn read_network_detail(
    shared: &Mutex<Shared>,
    files: &mut BTreeMap<String, Disk>,
    lease: &LogReaderLease,
    cursor: &LogCursor,
) -> Result<LogNetworkDetail, String> {
    let status = {
        let s = shared.lock().unwrap();
        if !s.readers.get(&lease.id).is_some_and(|current| {
            current.capture_id == lease.capture_id && current.generation == lease.generation
        }) {
            return Err("Log reader is closed".into());
        }
        s.captures
            .values()
            .find(|p| p.status.id == lease.capture_id)
            .map(|p| p.status.clone())
            .ok_or("Capture no longer exists")?
    };
    if cursor.capture_id != lease.capture_id
        || cursor.generation != status.generation
        || cursor.generation != lease.generation
        || cursor.offset >= status.bytes
    {
        return Err("Stale log cursor".into());
    }
    let disk = files
        .get_mut(&status.plugin_id)
        .ok_or("Capture file missing")?;
    if cursor.offset > 0 {
        disk.file
            .seek(SeekFrom::Start(cursor.offset - 1))
            .map_err(|e| e.to_string())?;
        let mut previous = [0];
        disk.file
            .read_exact(&mut previous)
            .map_err(|e| e.to_string())?;
        if previous != *b"\n" {
            return Err("Invalid log cursor".into());
        }
    }
    disk.file
        .seek(SeekFrom::Start(cursor.offset))
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new((&disk.file).take(2 * 1024 * 1024))
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let record: DiskRecord = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    record
        .network
        .ok_or_else(|| "This entry has no captured network bodies".into())
}

// Require a private directory for captured diagnostic data
fn private_directory(log_directory: &Path) -> Result<tempfile::TempDir, String> {
    std::fs::create_dir_all(log_directory).map_err(|e| e.to_string())?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("plugin-captures-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir_in(log_directory).map_err(|e| e.to_string())
}
