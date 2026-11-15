//! Explicit, bounded, editor-owned recording sessions. No startup hooks or durable drafts
use super::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant};

const MAX_BATCH: usize = 256;
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardMacroCaptureMode {
    InApp,
    Global,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardMacroSessionState {
    WaitingForRelease,
    Countdown,
    Recording,
    Stopped,
    Canceled,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroCaptureEvent {
    pub kind: KeyboardMacroEventKind,
    pub key: PhysicalKey,
    pub elapsed_ms: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroCapabilities {
    pub platform: KeyboardMacroPlatform,
    pub native_warning: Option<String>,
    pub required_repeat_mode: Option<KeyboardMacroRepeatMode>,
    pub new_action_repeat_mode: KeyboardMacroRepeatMode,
    pub in_app_recording: bool,
    pub global_recording: bool,
    pub global_reason: Option<String>,
    pub native_playback: bool,
    pub playback_reason: Option<String>,
    pub test_playback: bool,
    pub test_reason: Option<String>,
    pub emergency_shortcut: String,
    pub global_emergency_shortcut: String,
    pub max_events: usize,
    #[ts(type = "number")]
    pub max_duration_ms: u64,
    pub supported_keys: Vec<PhysicalKey>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroSession {
    pub session_id: String,
    pub state: KeyboardMacroSessionState,
    pub event_count: usize,
    pub duration_ms: u32,
    pub balanced_releases: usize,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroRecordingResult {
    pub session: KeyboardMacroSession,
    pub recording: KeyboardMacroRecording,
}

// Identify the operating system recorded in macro metadata
pub fn platform() -> KeyboardMacroPlatform {
    if cfg!(target_os = "macos") {
        KeyboardMacroPlatform::Macos
    } else if cfg!(target_os = "windows") {
        KeyboardMacroPlatform::Windows
    } else if cfg!(target_os = "linux") {
        KeyboardMacroPlatform::Linux
    } else {
        KeyboardMacroPlatform::Unknown
    }
}

// Describe supported recorder capabilities
pub fn capabilities() -> KeyboardMacroCapabilities {
    let global = super::capture::global_readiness();
    let playback_reason = super::playback::unavailable_reason();
    let test_reason = super::test_session::unavailable_reason();
    KeyboardMacroCapabilities {
        platform: platform(),
        native_warning: super::platform::native_warning(platform()).map(str::to_owned),
        required_repeat_mode: super::platform::required_repeat_mode(),
        new_action_repeat_mode: super::platform::new_action_repeat_mode(),
        in_app_recording: true,
        global_recording: global.available,
        global_reason: global.reason,
        native_playback: playback_reason.is_none(),
        playback_reason,
        test_playback: test_reason.is_none(),
        test_reason,
        emergency_shortcut: "Ctrl+Shift+F12".into(),
        global_emergency_shortcut: super::capture::global_emergency_shortcut().into(),
        max_events: MAX_EVENTS,
        max_duration_ms: MAX_DURATION_MS,
        supported_keys: PhysicalKey::ALL.to_vec(),
    }
}

struct Session {
    owner: String,
    mode: KeyboardMacroCaptureMode,
    view: KeyboardMacroSession,
    recording: KeyboardMacroRecording,
    started: Instant,
    touched: Instant,
    last_input_ms: u32,
    last_input_at: Instant,
    last_recorded_ms: u32,
    baseline_ms: u32,
    initial: HashSet<PhysicalKey>,
    held: Vec<PhysicalKey>,
    raw_held: HashSet<PhysicalKey>,
    pending: Vec<KeyboardMacroCaptureEvent>,
    // Capture must retire before admission can be released, including unwinding
    capture: Option<super::capture::CaptureHandle>,
    lease: Option<super::playback::RecordingLease>,
    initializing: bool,
    startup_stop: Arc<AtomicBool>,
    retiring: bool,
    stop_requested: bool,
    cleanup_error: Option<String>,
}
impl Session {
    // Initialize recorder state from the supplied inputs
    fn new(
        owner: &str,
        mode: KeyboardMacroCaptureMode,
        initially_held: Vec<PhysicalKey>,
        now: Instant,
    ) -> Self {
        let waiting = !initially_held.is_empty();
        Self {
            owner: owner.into(),
            mode,
            view: KeyboardMacroSession {
                session_id: uuid::Uuid::new_v4().to_string(),
                state: if waiting {
                    KeyboardMacroSessionState::WaitingForRelease
                } else {
                    KeyboardMacroSessionState::Recording
                },
                event_count: 0,
                duration_ms: 0,
                balanced_releases: 0,
                error: None,
            },
            recording: KeyboardMacroRecording {
                platform: platform(),
                ..Default::default()
            },
            started: now,
            touched: now,
            last_input_ms: 0,
            last_input_at: now,
            last_recorded_ms: 0,
            baseline_ms: 0,
            initial: initially_held.into_iter().collect(),
            held: vec![],
            raw_held: HashSet::new(),
            pending: vec![],
            lease: None,
            capture: None,
            initializing: false,
            startup_stop: Arc::new(AtomicBool::new(false)),
            retiring: false,
            stop_requested: false,
            cleanup_error: None,
        }
    }
    // Refresh session liveness and fail in-app recording when editor focus is lost
    fn heartbeat(&mut self, focused: bool) {
        if self.active() && self.mode == KeyboardMacroCaptureMode::InApp && !focused {
            self.failure("recording_focus_lost");
            return;
        }
        self.touched = Instant::now();
    }
    // Refresh capture status
    fn refresh_capture_status(&mut self, ready: bool, now: Instant) {
        if self.mode != KeyboardMacroCaptureMode::Global {
            return;
        }
        if self.view.state == KeyboardMacroSessionState::Countdown && ready {
            self.view.state = KeyboardMacroSessionState::Recording;
            self.last_input_at = now;
        }
        if self.view.state == KeyboardMacroSessionState::Recording {
            // A silent native recording still needs a live elapsed indicator. This
            // is presentation only; recorded delays retain the native timestamps
            let elapsed = now
                .saturating_duration_since(self.last_input_at)
                .as_millis()
                .min(u128::from(u32::MAX)) as u32;
            self.view.duration_ms = self
                .last_input_ms
                .saturating_add(elapsed)
                .saturating_sub(self.baseline_ms)
                .min(MAX_DURATION_MS as u32);
        }
    }
    // Report the lease valid condition for recorder
    fn lease_valid(&self) -> bool {
        self.lease
            .as_ref()
            .is_none_or(|lease| lease.check().is_ok())
    }
    // Report the active recorder state
    fn active(&self) -> bool {
        matches!(
            self.view.state,
            KeyboardMacroSessionState::WaitingForRelease
                | KeyboardMacroSessionState::Countdown
                | KeyboardMacroSessionState::Recording
        )
    }
    // Fail the session, discard captured input and request native capture retirement
    fn failure(&mut self, code: &str) {
        self.view.state = KeyboardMacroSessionState::Failed;
        self.view.error = Some(code.into());
        self.recording.events.clear();
        self.pending.clear();
        self.held.clear();
        self.raw_held.clear();
        self.initial.clear();
        self.view.event_count = 0;
        self.request_retirement();
    }
    // Report the cleanup pending condition for recorder
    fn cleanup_pending(&self) -> bool {
        self.initializing || self.retiring || self.capture.is_some()
    }
    // Request retirement
    fn request_retirement(&mut self) {
        self.startup_stop.store(true, Ordering::Release);
        if let Some(capture) = &self.capture {
            capture.request_stop();
        }
        if !self.cleanup_pending() && self.cleanup_error.is_none() {
            self.lease.take();
        }
    }
    // Commit the staged recorder change
    fn commit(&mut self, event: KeyboardMacroCaptureEvent) -> Result<(), String> {
        let elapsed = event.elapsed_ms.saturating_sub(self.baseline_ms);
        let next_held = match event.kind {
            KeyboardMacroEventKind::KeyDown => self.held.len() + 1,
            KeyboardMacroEventKind::KeyUp => self.held.len().saturating_sub(1),
            KeyboardMacroEventKind::Repeat => self.held.len(),
        };
        // Reserve a release for every outstanding press so Stop never truncates a chord
        if self.recording.events.len() + 1 + next_held > MAX_EVENTS {
            return Err("recording_event_limit".into());
        }
        match event.kind {
            KeyboardMacroEventKind::KeyDown => {
                if self.held.contains(&event.key) {
                    return Err("duplicate_key_down".into());
                }
                self.held.push(event.key);
            }
            KeyboardMacroEventKind::KeyUp => self.held.retain(|key| *key != event.key),
            KeyboardMacroEventKind::Repeat => {
                if !self.held.contains(&event.key) {
                    return Err("repeat_without_press".into());
                }
            }
        }
        self.recording.events.push(KeyboardMacroEvent {
            kind: event.kind,
            key: event.key,
            delay_ms: elapsed.saturating_sub(self.last_recorded_ms),
        });
        self.last_recorded_ms = elapsed;
        self.view.duration_ms = elapsed;
        self.view.event_count = self.recording.events.len();
        Ok(())
    }
    // Flush pending recorder writes
    fn flush(&mut self) -> Result<(), String> {
        for event in std::mem::take(&mut self.pending) {
            self.commit(event)?;
        }
        Ok(())
    }
    // Accept the pending recorder request
    fn accept(&mut self, event: KeyboardMacroCaptureEvent) -> Result<(), String> {
        if !self.active() {
            return Err("recording_session_not_active".into());
        }
        if !self.lease_valid() {
            return Err("recording_input_canceled".into());
        }
        if u64::from(event.elapsed_ms) > MAX_DURATION_MS || event.elapsed_ms < self.last_input_ms {
            return Err("recording_timestamp_invalid".into());
        }
        self.last_input_ms = event.elapsed_ms;
        self.last_input_at = Instant::now();
        if self.view.state == KeyboardMacroSessionState::WaitingForRelease {
            match event.kind {
                KeyboardMacroEventKind::KeyDown => {
                    self.initial.insert(event.key);
                }
                KeyboardMacroEventKind::KeyUp => {
                    self.initial.remove(&event.key);
                }
                KeyboardMacroEventKind::Repeat => {}
            }
            if self.initial.is_empty() {
                self.baseline_ms = event.elapsed_ms;
                self.view.state = KeyboardMacroSessionState::Recording;
            }
            return Ok(());
        }
        match event.kind {
            KeyboardMacroEventKind::KeyDown => {
                if !self.raw_held.insert(event.key) {
                    return Err("duplicate_key_down".into());
                }
            }
            KeyboardMacroEventKind::KeyUp => {
                // A release from before focused capture began is not part of this recording
                if !self.raw_held.remove(&event.key) {
                    return Ok(());
                }
            }
            KeyboardMacroEventKind::Repeat => {
                if !self.raw_held.contains(&event.key) {
                    return Err("repeat_without_press".into());
                }
            }
        }
        let control = self.raw_held.contains(&PhysicalKey::ControlLeft)
            || self.raw_held.contains(&PhysicalKey::ControlRight);
        let shift = self.raw_held.contains(&PhysicalKey::ShiftLeft)
            || self.raw_held.contains(&PhysicalKey::ShiftRight);
        if self.mode == KeyboardMacroCaptureMode::InApp
            && event.kind == KeyboardMacroEventKind::KeyDown
            && event.key == PhysicalKey::F12
            && control
            && shift
        {
            // Delayed modifier prefix never enters the returned recording
            self.pending.clear();
            return self.finish(false);
        }
        if matches!(
            event.key,
            PhysicalKey::ControlLeft
                | PhysicalKey::ControlRight
                | PhysicalKey::ShiftLeft
                | PhysicalKey::ShiftRight
        ) {
            if self.pending.len() >= MAX_BATCH {
                self.flush()?;
            }
            let released = event.kind == KeyboardMacroEventKind::KeyUp;
            self.pending.push(event);
            // A completed modifier tap is recording content, even while another
            // key is held. Only the currently unreleased stop prefix may be omitted
            if released {
                self.flush()?;
            }
        } else {
            self.flush()?;
            self.commit(event)?;
        }
        Ok(())
    }
    // Finalize the pending recorder operation
    fn finish(&mut self, flush: bool) -> Result<(), String> {
        if self.view.state == KeyboardMacroSessionState::Stopped {
            return Ok(());
        }
        if !self.active() {
            return Err(self
                .view
                .error
                .clone()
                .unwrap_or_else(|| "recording_session_canceled".into()));
        }
        if flush {
            self.flush()?;
        }
        for key in self.held.clone().into_iter().rev() {
            self.commit(KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::KeyUp,
                key,
                elapsed_ms: self.last_input_ms,
            })?;
            self.view.balanced_releases += 1;
        }
        self.raw_held.clear();
        self.pending.clear();
        self.view.state = KeyboardMacroSessionState::Stopped;
        self.view.duration_ms = self.last_recorded_ms;
        self.request_retirement();
        if !self.recording.events.is_empty() {
            super::validation::validate(&self.recording, &KeyboardMacroPlayback::default())?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct Store {
    session: Option<Session>,
}
static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
// Store the recorder value
fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| Mutex::new(Store::default()))
}
// Require the active recording session to match both owner and session identifier
fn owned<'a>(store: &'a mut Store, owner: &str, id: &str) -> Result<&'a mut Session, String> {
    store
        .session
        .as_mut()
        .filter(|session| session.owner == owner && session.view.session_id == id)
        .ok_or_else(|| "recording_session_expired".into())
}

// Start the recorder operation
pub fn start(
    owner: &str,
    editor_id: &str,
    mode: KeyboardMacroCaptureMode,
    initially_held: Vec<PhysicalKey>,
) -> Result<KeyboardMacroSession, String> {
    if editor_id.is_empty()
        || editor_id.len() > 128
        || initially_held.len() > PhysicalKey::ALL.len()
    {
        return Err("recording_owner_invalid".into());
    }
    let mut guard = store()
        .lock()
        .map_err(|_| "recording_service_unavailable")?;
    if guard
        .session
        .as_ref()
        .is_some_and(|s| s.active() || s.cleanup_pending() || s.cleanup_error.is_some())
    {
        return Err("A keyboard recording is already active".into());
    }
    let mut session = Session::new(owner, mode, initially_held, Instant::now());
    session.lease = Some(super::playback::try_recording_lease()?);
    if mode == KeyboardMacroCaptureMode::Global {
        let readiness = super::capture::global_readiness();
        if !readiness.available {
            return Err(readiness
                .reason
                .unwrap_or_else(|| "Global recording unavailable".into()));
        }
        session.initializing = true;
        session.view.state = KeyboardMacroSessionState::Countdown;
    }
    let result = session.view.clone();
    let id = result.session_id.clone();
    guard.session = Some(session);
    drop(guard);
    let worker_id = id.clone();
    std::thread::Builder::new()
        .name("keyboard-recording-session".into())
        .spawn(move || watchdog(&worker_id))
        .map_err(|_| {
            if let Ok(mut guard) = store().lock() {
                if let Some(session) = guard.session.as_mut().filter(|s| s.view.session_id == id) {
                    session.initializing = false;
                    session.failure("recording_worker_unavailable");
                }
            }
            "recording_worker_unavailable".to_string()
        })?;
    Ok(result)
}

// Append an entry to recorder
pub fn append(
    owner: &str,
    id: &str,
    events: Vec<KeyboardMacroCaptureEvent>,
) -> Result<KeyboardMacroSession, String> {
    let mut guard = store()
        .lock()
        .map_err(|_| "recording_service_unavailable")?;
    let session = owned(&mut guard, owner, id)?;
    append_session(session, events)
}
// Append session
fn append_session(
    session: &mut Session,
    events: Vec<KeyboardMacroCaptureEvent>,
) -> Result<KeyboardMacroSession, String> {
    if session.mode != KeyboardMacroCaptureMode::InApp {
        return Err("global_recording_does_not_accept_browser_events".into());
    }
    // Automatic/emergency Stop can race a queued browser batch. Preserve the
    // completed draft and never restart capture or discard it because of late IPC
    if session.view.state == KeyboardMacroSessionState::Stopped {
        return Ok(session.view.clone());
    }
    if !session.active() {
        return Err(session
            .view
            .error
            .clone()
            .unwrap_or_else(|| "recording_session_not_active".into()));
    }
    if events.len() > MAX_BATCH {
        session.failure("recording_batch_limit");
        return Err("recording_batch_limit".into());
    }
    session.touched = Instant::now();
    for event in events {
        if u128::from(event.elapsed_ms) > session.started.elapsed().as_millis() + 2000 {
            session.failure("recording_timestamp_invalid");
            return Err("recording_timestamp_invalid".into());
        }
        if let Err(error) = session.accept(event) {
            if error == "recording_event_limit" {
                session.finish(false)?;
                return Ok(session.view.clone());
            }
            session.failure(&error);
            return Err(error);
        }
        if !session.active() {
            break;
        }
    }
    Ok(session.view.clone())
}
// Refresh session liveness and fail in-app recording when editor focus is lost
pub fn heartbeat(owner: &str, id: &str, focused: bool) -> Result<KeyboardMacroSession, String> {
    let mut guard = store()
        .lock()
        .map_err(|_| "recording_service_unavailable")?;
    let session = owned(&mut guard, owner, id)?;
    session.heartbeat(focused);
    Ok(session.view.clone())
}
// Stop the recorder operation
pub async fn stop(owner: &str, id: &str) -> Result<KeyboardMacroRecordingResult, String> {
    {
        let mut guard = store()
            .lock()
            .map_err(|_| "recording_service_unavailable")?;
        let session = owned(&mut guard, owner, id)?;
        session.stop_requested = true;
        session.request_retirement();
    }
    wait_retirement(owner, id).await?;
    let mut guard = store()
        .lock()
        .map_err(|_| "recording_service_unavailable")?;
    let session = owned(&mut guard, owner, id)?;
    complete_stop(session)
}
// Complete stop
fn complete_stop(session: &mut Session) -> Result<KeyboardMacroRecordingResult, String> {
    if let Err(error) = drain_capture(session) {
        session.failure(&error);
        return Err(error);
    }
    if session.active() {
        session.last_input_ms = session
            .last_input_ms
            .saturating_add(
                session
                    .last_input_at
                    .elapsed()
                    .as_millis()
                    .min(u128::from(u32::MAX)) as u32,
            )
            .min(MAX_DURATION_MS as u32);
    }
    if let Err(error) = session.finish(true) {
        session.failure(&error);
        return Err(error);
    }
    session.touched = Instant::now();
    Ok(KeyboardMacroRecordingResult {
        session: session.view.clone(),
        recording: session.recording.clone(),
    })
}
// Cancel the pending recorder operation
pub async fn cancel(owner: &str, id: &str) -> Result<(), String> {
    {
        let mut guard = store()
            .lock()
            .map_err(|_| "recording_service_unavailable")?;
        cancel_session(&mut guard, owner, id);
    }
    wait_retirement(owner, id).await
}
// Wait for retirement
async fn wait_retirement(owner: &str, id: &str) -> Result<(), String> {
    wait_retirement_for(owner, id, Duration::from_secs(5)).await
}
// Wait for retirement for
async fn wait_retirement_for(owner: &str, id: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        {
            let mut guard = store()
                .lock()
                .map_err(|_| "recording_service_unavailable")?;
            let Ok(session) = owned(&mut guard, owner, id) else {
                return Ok(());
            };
            if !session.cleanup_pending() {
                return session.cleanup_error.clone().map_or(Ok(()), Err);
            }
        }
        if Instant::now() >= deadline {
            return Err("Keyboard recording cleanup is still pending; retry after its native worker retires".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
// Report whether current work prevents releasing editor resources
pub fn editor_retirement_blocked() -> bool {
    match store().try_lock() {
        Ok(guard) => guard
            .session
            .as_ref()
            .is_some_and(|s| s.active() || s.cleanup_pending() || s.cleanup_error.is_some()),
        Err(_) => true,
    }
}

// Cancel session
fn cancel_session(store: &mut Store, owner: &str, id: &str) {
    // Native owner loss, expiry and repeated UI cancellation can retire the token
    // before this request arrives. A stale Cancel is successful but cannot affect
    // a newer recording or a different editor document
    if let Some(session) = store
        .session
        .as_mut()
        .filter(|session| session.owner == owner && session.view.session_id == id)
    {
        session.failure("recording_canceled");
        session.view.state = KeyboardMacroSessionState::Canceled;
    }
}
// Cancel owner
pub fn cancel_owner(owner: &str) {
    if let Ok(mut guard) = store().lock() {
        if guard
            .session
            .as_ref()
            .is_some_and(|s| s.owner == owner || owner == "main" && s.owner.starts_with("main:"))
        {
            if let Some(session) = guard.session.as_mut() {
                session.failure("recording_canceled");
                session.view.state = KeyboardMacroSessionState::Canceled;
            }
        }
    }
}
// Cancel all pending recorder operations
pub fn cancel_all() {
    if let Ok(mut guard) = store().lock() {
        if let Some(session) = guard.session.as_mut() {
            session.failure("recording_canceled");
            session.view.state = KeyboardMacroSessionState::Canceled;
        }
    }
}

// Drain capture
fn drain_capture(session: &mut Session) -> Result<(), String> {
    let ready = session
        .capture
        .as_ref()
        .is_some_and(|capture| capture.ready());
    session.refresh_capture_status(ready, Instant::now());
    loop {
        let Some(capture) = session.capture.as_mut() else {
            return Ok(());
        };
        match capture.try_recv() {
            Ok(Some(event)) => {
                if session.view.state == KeyboardMacroSessionState::Countdown {
                    session.view.state = KeyboardMacroSessionState::Recording;
                }
                if let Err(error) = session.accept(KeyboardMacroCaptureEvent {
                    kind: event.kind,
                    key: event.key,
                    elapsed_ms: u32::try_from(event.elapsed_ms)
                        .map_err(|_| "recording_timestamp_invalid")?,
                }) {
                    if error == "recording_event_limit" {
                        return session.finish(false);
                    }
                    return Err(error);
                }
            }
            Ok(None) => return Ok(()),
            Err(super::capture::CaptureError::EmergencyStop) => {
                session.pending.clear();
                return session.finish(false);
            }
            Err(super::capture::CaptureError::SessionExpired) => {
                session.last_input_ms = MAX_DURATION_MS as u32;
                return session.finish(true);
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}
// Finalize at duration limit
fn finish_at_duration_limit(session: &mut Session) -> Result<(), String> {
    if session.cleanup_pending() {
        session.stop_requested = true;
        session.request_retirement();
        return Ok(());
    }
    drain_capture(session)?;
    if session.active() {
        session.last_input_ms = MAX_DURATION_MS as u32;
        session.finish(true)?;
    }
    session.touched = Instant::now();
    Ok(())
}
// Start native capture outside the store lock and reconcile it with the live session
fn initialize_capture(
    id: &str,
    start: impl FnOnce(
        Arc<AtomicBool>,
    ) -> Result<super::capture::CaptureHandle, super::capture::CaptureError>,
) {
    let stop = {
        let mut guard = store().lock().unwrap();
        let Some(session) = guard
            .session
            .as_mut()
            .filter(|s| s.view.session_id == id && s.initializing)
        else {
            return;
        };
        if !session.active() || session.stop_requested {
            session.initializing = false;
            session.request_retirement();
            return;
        }
        session.startup_stop.clone()
    };
    let result = start(stop);
    let mut guard = store().lock().unwrap();
    let session = guard
        .session
        .as_mut()
        .filter(|s| s.view.session_id == id)
        .expect("initializing token retained");
    session.initializing = false;
    match result {
        Ok(capture) => session.capture = Some(capture),
        Err(error) => {
            if session.active() {
                session.failure(&error.to_string());
            }
        }
    }
    if !session.active() || session.stop_requested {
        session.request_retirement();
    }
}
// Monitor capture progress and retire sessions that stop or lose liveness
fn watchdog(id: &str) {
    // Startup may itself require an unbounded OS cleanup on error. Keep the
    // token and admission in STORE, but never run it on the UI or under STORE
    initialize_capture(id, super::capture::start_global);
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let Ok(mut guard) = store().lock() else {
            return;
        };
        let Some(session) = guard.session.as_mut().filter(|s| s.view.session_id == id) else {
            return;
        };
        if session.stop_requested || !session.active() {
            session.request_retirement();
        }
        if session
            .capture
            .as_ref()
            .is_some_and(|c| c.worker_finished())
        {
            let mut capture = session.capture.take().unwrap();
            session.retiring = true;
            drop(guard);
            // Only this worker joins; lease/token remain owned even if IPC is abandoned
            let result = capture.stop();
            let mut guard = store().lock().unwrap();
            let session = guard
                .session
                .as_mut()
                .filter(|s| s.view.session_id == id)
                .expect("retiring token retained");
            session.cleanup_error = capture.cleanup_error().map(|error| error.to_string());
            session.capture = Some(capture);
            if let Err(error) = result {
                if !matches!(
                    error,
                    super::capture::CaptureError::EmergencyStop
                        | super::capture::CaptureError::SessionExpired
                ) {
                    session.failure(&error.to_string());
                }
            }
            if session.active() {
                if let Err(error) = drain_capture(session) {
                    session.failure(&error);
                }
                if session.active() {
                    if let Err(error) = complete_stop(session) {
                        session.failure(&error);
                    }
                }
            }
            session.capture.take(); // already joined, cannot block
            session.retiring = false;
            if session.cleanup_error.is_none() {
                session.lease.take();
            }
            continue;
        }
        if !session.active() {
            if session.touched.elapsed() > REVIEW_TIMEOUT
                && !session.cleanup_pending()
                && session.cleanup_error.is_none()
            {
                guard.session.take();
                return;
            }
            continue;
        }
        if !session.lease_valid() {
            session.failure("recording_input_canceled");
            continue;
        }
        if session.touched.elapsed() > HEARTBEAT_TIMEOUT {
            session.failure("recording_editor_timeout");
            continue;
        }
        if session.started.elapsed().as_millis()
            > u128::from(MAX_DURATION_MS)
                + if session.mode == KeyboardMacroCaptureMode::Global {
                    3000
                } else {
                    0
                }
        {
            if let Err(error) = finish_at_duration_limit(session) {
                session.failure(&error);
            }
            continue;
        }
        if !session.stop_requested {
            if let Err(error) = drain_capture(session) {
                session.failure(&error);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct an in-app recording session for the test
    fn session(initial: Vec<PhysicalKey>) -> Session {
        Session::new(
            "main:1",
            KeyboardMacroCaptureMode::InApp,
            initial,
            Instant::now(),
        )
    }
    // Handle the incoming recorder event
    fn event(kind: KeyboardMacroEventKind, key: PhysicalKey, ms: u32) -> KeyboardMacroCaptureEvent {
        KeyboardMacroCaptureEvent {
            kind,
            key,
            elapsed_ms: ms,
        }
    }
    #[test]
    // Verify that stop balances in reverse order and is idempotent
    fn stop_balances_in_reverse_order_and_is_idempotent() {
        let mut s = session(vec![]);
        s.accept(event(
            KeyboardMacroEventKind::KeyDown,
            PhysicalKey::ControlLeft,
            0,
        ))
        .unwrap();
        s.accept(event(
            KeyboardMacroEventKind::KeyDown,
            PhysicalKey::KeyC,
            10,
        ))
        .unwrap();
        s.finish(true).unwrap();
        s.finish(true).unwrap();
        assert_eq!(s.view.balanced_releases, 2);
        assert_eq!(
            s.recording.events.iter().map(|e| e.key).collect::<Vec<_>>(),
            vec![
                PhysicalKey::ControlLeft,
                PhysicalKey::KeyC,
                PhysicalKey::KeyC,
                PhysicalKey::ControlLeft
            ]
        );
    }
    #[test]
    // Verify that initial keys and stray releases are not recorded
    fn initial_keys_and_stray_releases_are_not_recorded() {
        let mut s = session(vec![PhysicalKey::ShiftLeft]);
        s.accept(event(KeyboardMacroEventKind::KeyDown, PhysicalKey::KeyA, 1))
            .unwrap();
        s.accept(event(
            KeyboardMacroEventKind::KeyUp,
            PhysicalKey::ShiftLeft,
            2,
        ))
        .unwrap();
        assert_eq!(s.view.state, KeyboardMacroSessionState::WaitingForRelease);
        s.accept(event(KeyboardMacroEventKind::KeyUp, PhysicalKey::KeyA, 3))
            .unwrap();
        s.accept(event(KeyboardMacroEventKind::KeyUp, PhysicalKey::KeyB, 4))
            .unwrap();
        s.accept(event(
            KeyboardMacroEventKind::KeyDown,
            PhysicalKey::Escape,
            5,
        ))
        .unwrap();
        s.finish(true).unwrap();
        assert_eq!(s.recording.events.len(), 2);
        assert_eq!(s.recording.events[0].delay_ms, 2);
    }
    #[test]
    // Verify that emergency chord does not enter recording
    fn emergency_chord_does_not_enter_recording() {
        let mut s = session(vec![]);
        s.accept(event(KeyboardMacroEventKind::KeyDown, PhysicalKey::KeyA, 0))
            .unwrap();
        s.accept(event(KeyboardMacroEventKind::KeyUp, PhysicalKey::KeyA, 1))
            .unwrap();
        s.accept(event(
            KeyboardMacroEventKind::KeyDown,
            PhysicalKey::ControlRight,
            2,
        ))
        .unwrap();
        s.accept(event(
            KeyboardMacroEventKind::KeyDown,
            PhysicalKey::ShiftLeft,
            3,
        ))
        .unwrap();
        s.accept(event(KeyboardMacroEventKind::KeyDown, PhysicalKey::F12, 4))
            .unwrap();
        assert_eq!(s.view.state, KeyboardMacroSessionState::Stopped);
        assert_eq!(s.recording.events.len(), 2);
    }
    #[test]
    // Verify that invalid repeat timing and duplicate press are rejected
    fn invalid_repeat_timing_and_duplicate_press_are_rejected() {
        let mut s = session(vec![]);
        assert!(s
            .accept(event(KeyboardMacroEventKind::Repeat, PhysicalKey::KeyA, 0))
            .is_err());
        s.accept(event(KeyboardMacroEventKind::KeyDown, PhysicalKey::KeyA, 2))
            .unwrap();
        assert!(s
            .accept(event(KeyboardMacroEventKind::KeyDown, PhysicalKey::KeyA, 3))
            .is_err());
        assert!(s
            .accept(event(KeyboardMacroEventKind::KeyUp, PhysicalKey::KeyA, 1))
            .is_err());
    }
    #[test]
    // Verify that cancellation discards content and stale owner cannot access
    fn cancellation_discards_content_and_stale_owner_cannot_access() {
        let mut s = session(vec![]);
        s.accept(event(KeyboardMacroEventKind::KeyDown, PhysicalKey::KeyA, 1))
            .unwrap();
        let id = s.view.session_id.clone();
        let mut store = Store { session: Some(s) };
        assert!(owned(&mut store, "main:2", &id).is_err());
        assert!(owned(&mut store, "main:1", "stale").is_err());
        let s = owned(&mut store, "main:1", &id).unwrap();
        s.failure("recording_canceled");
        assert!(s.recording.events.is_empty());
        assert!(s.finish(true).is_err());
    }
}

#[cfg(test)]
mod limits_tests {
    use super::*;
    #[test]
    // Verify that event limit always reserves balancing releases
    fn event_limit_always_reserves_balancing_releases() {
        let mut session = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::InApp,
            vec![],
            Instant::now(),
        );
        for _ in 0..(MAX_EVENTS / 2 - 1) {
            session
                .accept(KeyboardMacroCaptureEvent {
                    kind: KeyboardMacroEventKind::KeyDown,
                    key: PhysicalKey::KeyA,
                    elapsed_ms: 0,
                })
                .unwrap();
            session
                .accept(KeyboardMacroCaptureEvent {
                    kind: KeyboardMacroEventKind::KeyUp,
                    key: PhysicalKey::KeyA,
                    elapsed_ms: 0,
                })
                .unwrap();
        }
        session
            .accept(KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::KeyDown,
                key: PhysicalKey::KeyB,
                elapsed_ms: 0,
            })
            .unwrap();
        assert!(session
            .accept(KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::KeyDown,
                key: PhysicalKey::KeyC,
                elapsed_ms: 0
            })
            .is_err());
        session.finish(true).unwrap();
        assert_eq!(session.recording.events.len(), MAX_EVENTS);
        assert_eq!(session.view.balanced_releases, 1);
    }
    #[test]
    // Verify that modifier only capture and repeat keep original order
    fn modifier_only_capture_and_repeat_keep_original_order() {
        let mut session = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::InApp,
            vec![],
            Instant::now(),
        );
        for (kind, key, elapsed_ms) in [
            (KeyboardMacroEventKind::KeyDown, PhysicalKey::ShiftRight, 0),
            (KeyboardMacroEventKind::KeyUp, PhysicalKey::ShiftRight, 10),
            (KeyboardMacroEventKind::KeyDown, PhysicalKey::KeyA, 10),
            (KeyboardMacroEventKind::Repeat, PhysicalKey::KeyA, 20),
            (KeyboardMacroEventKind::KeyUp, PhysicalKey::KeyA, 30),
        ] {
            session
                .accept(KeyboardMacroCaptureEvent {
                    kind,
                    key,
                    elapsed_ms,
                })
                .unwrap();
        }
        session.finish(true).unwrap();
        assert_eq!(
            session
                .recording
                .events
                .iter()
                .map(|e| e.delay_ms)
                .collect::<Vec<_>>(),
            vec![0, 10, 0, 10, 10]
        );
        assert_eq!(session.recording.events[0].key, PhysicalKey::ShiftRight);
        assert_eq!(
            session.recording.events[3].kind,
            KeyboardMacroEventKind::Repeat
        );
    }
    #[test]
    // Verify that event limit finishes review instead of discarding a long take
    fn event_limit_finishes_review_instead_of_discarding_a_long_take() {
        let mut session = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::InApp,
            vec![],
            Instant::now(),
        );
        session
            .accept(KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::KeyDown,
                key: PhysicalKey::KeyA,
                elapsed_ms: 0,
            })
            .unwrap();
        for _ in 0..MAX_EVENTS - 2 {
            session
                .accept(KeyboardMacroCaptureEvent {
                    kind: KeyboardMacroEventKind::Repeat,
                    key: PhysicalKey::KeyA,
                    elapsed_ms: 0,
                })
                .unwrap();
        }
        append_session(
            &mut session,
            vec![KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::Repeat,
                key: PhysicalKey::KeyA,
                elapsed_ms: 0,
            }],
        )
        .unwrap();
        assert_eq!(session.view.state, KeyboardMacroSessionState::Stopped);
        assert_eq!(session.recording.events.len(), MAX_EVENTS);
        assert_eq!(
            session.recording.events.last().unwrap().kind,
            KeyboardMacroEventKind::KeyUp
        );
        assert_eq!(session.view.balanced_releases, 1);
    }
    #[test]
    // Verify that duration limit stops balanced and late batches preserve the review
    fn duration_limit_stops_balanced_and_late_batches_preserve_the_review() {
        let mut session = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::InApp,
            vec![],
            Instant::now(),
        );
        session
            .accept(KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::KeyDown,
                key: PhysicalKey::KeyA,
                elapsed_ms: 0,
            })
            .unwrap();
        finish_at_duration_limit(&mut session).unwrap();
        assert_eq!(session.view.state, KeyboardMacroSessionState::Stopped);
        assert_eq!(session.view.duration_ms, MAX_DURATION_MS as u32);
        assert_eq!(session.view.balanced_releases, 1);
        let recording = session.recording.clone();
        append_session(
            &mut session,
            vec![KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::KeyUp,
                key: PhysicalKey::KeyA,
                elapsed_ms: MAX_DURATION_MS as u32,
            }],
        )
        .unwrap();
        session.finish(true).unwrap();
        assert_eq!(session.recording, recording);
        super::super::validation::validate(&session.recording, &KeyboardMacroPlayback::default())
            .unwrap();
        let mut empty = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::InApp,
            vec![PhysicalKey::ShiftLeft],
            Instant::now(),
        );
        finish_at_duration_limit(&mut empty).unwrap();
        assert_eq!(empty.view.state, KeyboardMacroSessionState::Stopped);
        assert!(empty.recording.events.is_empty());
    }
    #[test]
    // Verify that emergency stop preserves prior modifier taps and overlaps
    fn emergency_stop_preserves_prior_modifier_taps_and_overlaps() {
        for overlap in [false, true] {
            let mut session = Session::new(
                "main:1",
                KeyboardMacroCaptureMode::InApp,
                vec![],
                Instant::now(),
            );
            if overlap {
                session
                    .accept(KeyboardMacroCaptureEvent {
                        kind: KeyboardMacroEventKind::KeyDown,
                        key: PhysicalKey::KeyA,
                        elapsed_ms: 0,
                    })
                    .unwrap();
            }
            for (kind, key, elapsed_ms) in [
                (KeyboardMacroEventKind::KeyDown, PhysicalKey::ShiftRight, 5),
                (KeyboardMacroEventKind::KeyUp, PhysicalKey::ShiftRight, 10),
                (
                    KeyboardMacroEventKind::KeyDown,
                    PhysicalKey::ControlLeft,
                    20,
                ),
                (KeyboardMacroEventKind::KeyDown, PhysicalKey::ShiftLeft, 30),
                (KeyboardMacroEventKind::KeyDown, PhysicalKey::F12, 40),
            ] {
                session
                    .accept(KeyboardMacroCaptureEvent {
                        kind,
                        key,
                        elapsed_ms,
                    })
                    .unwrap();
            }
            assert_eq!(session.view.state, KeyboardMacroSessionState::Stopped);
            let keys = session
                .recording
                .events
                .iter()
                .map(|event| event.key)
                .collect::<Vec<_>>();
            assert_eq!(
                keys,
                if overlap {
                    vec![
                        PhysicalKey::KeyA,
                        PhysicalKey::ShiftRight,
                        PhysicalKey::ShiftRight,
                        PhysicalKey::KeyA,
                    ]
                } else {
                    vec![PhysicalKey::ShiftRight, PhysicalKey::ShiftRight]
                }
            );
            assert_eq!(session.view.balanced_releases, usize::from(overlap));
            super::super::validation::validate(
                &session.recording,
                &KeyboardMacroPlayback::default(),
            )
            .unwrap();
        }
    }
    #[test]
    // Verify that cancel is idempotent after native teardown and cannot cancel a new owner
    fn cancel_is_idempotent_after_native_teardown_and_cannot_cancel_a_new_owner() {
        let mut store = Store::default();
        cancel_session(&mut store, "main:1", "retired-token");
        let session = Session::new(
            "main:2",
            KeyboardMacroCaptureMode::InApp,
            vec![],
            Instant::now(),
        );
        let id = session.view.session_id.clone();
        store.session = Some(session);
        cancel_session(&mut store, "main:1", "retired-token");
        cancel_session(&mut store, "main:1", &id);
        assert!(store.session.as_ref().unwrap().active());
        cancel_session(&mut store, "main:2", &id);
        cancel_session(&mut store, "main:2", &id);
        assert_eq!(
            store.session.as_ref().unwrap().view.state,
            KeyboardMacroSessionState::Canceled
        );
        store.session = None;
        cancel_session(&mut store, "main:2", &id);
    }
    #[test]
    // Verify that native focus loss discards in app capture without a final browser event
    fn native_focus_loss_discards_in_app_capture_without_a_final_browser_event() {
        let mut session = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::InApp,
            vec![],
            Instant::now(),
        );
        session
            .accept(KeyboardMacroCaptureEvent {
                kind: KeyboardMacroEventKind::KeyDown,
                key: PhysicalKey::KeyA,
                elapsed_ms: 0,
            })
            .unwrap();
        session.heartbeat(false);
        assert_eq!(session.view.state, KeyboardMacroSessionState::Failed);
        assert_eq!(session.view.error.as_deref(), Some("recording_focus_lost"));
        assert!(session.recording.events.is_empty());
        assert!(session.finish(true).is_err());
        let mut global = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::Global,
            vec![],
            Instant::now(),
        );
        global.heartbeat(false);
        assert_eq!(global.view.state, KeyboardMacroSessionState::Recording);
    }
    #[test]
    // Verify that global countdown waits for native readiness and elapsed advances without keys
    fn global_countdown_waits_for_native_readiness_and_elapsed_advances_without_keys() {
        let now = Instant::now();
        let mut session = Session::new("main:1", KeyboardMacroCaptureMode::Global, vec![], now);
        session.view.state = KeyboardMacroSessionState::Countdown;
        session.refresh_capture_status(false, now + Duration::from_secs(3));
        assert_eq!(session.view.state, KeyboardMacroSessionState::Countdown);
        assert_eq!(session.view.duration_ms, 0);
        session.refresh_capture_status(true, now + Duration::from_secs(4));
        assert_eq!(session.view.state, KeyboardMacroSessionState::Recording);
        session.refresh_capture_status(true, now + Duration::from_millis(6500));
        assert_eq!(session.view.duration_ms, 2500);
        assert!(session.recording.events.is_empty());
        session.finish(true).unwrap();
        assert_eq!(session.view.duration_ms, 0);
        session.refresh_capture_status(true, now + Duration::from_secs(10));
        assert_eq!(session.view.state, KeyboardMacroSessionState::Stopped);
        assert_eq!(session.view.duration_ms, 0);
    }
    #[test]
    // Verify that global stop chord is owned by native adapter not in app f12 handler
    fn global_stop_chord_is_owned_by_native_adapter_not_in_app_f12_handler() {
        let mut session = Session::new(
            "main:1",
            KeyboardMacroCaptureMode::Global,
            vec![],
            Instant::now(),
        );
        for (index, key) in [
            PhysicalKey::ControlLeft,
            PhysicalKey::ShiftLeft,
            PhysicalKey::F12,
        ]
        .into_iter()
        .enumerate()
        {
            session
                .accept(KeyboardMacroCaptureEvent {
                    kind: KeyboardMacroEventKind::KeyDown,
                    key,
                    elapsed_ms: index as u32,
                })
                .unwrap();
        }
        assert_eq!(session.view.state, KeyboardMacroSessionState::Recording);
        session.finish(true).unwrap();
        assert_eq!(session.recording.events.len(), 6);
        super::super::validation::validate(&session.recording, &KeyboardMacroPlayback::default())
            .unwrap();
    }
    #[test]
    // Verify that raw capture dto is closed and timestamps are integers
    fn raw_capture_dto_is_closed_and_timestamps_are_integers() {
        for data in [
            r#"{"kind":"key_down","key":"KeyA","elapsed_ms":-1}"#,
            r#"{"kind":"key_down","key":"KeyA","elapsed_ms":1.5}"#,
            r#"{"kind":"key_down","key":"KeyA","elapsed_ms":0,"text":"private"}"#,
            r#"{"kind":"key_down","key":"Unknown","elapsed_ms":0}"#,
        ] {
            assert!(serde_json::from_str::<KeyboardMacroCaptureEvent>(data).is_err());
        }
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;

    #[test]
    // Verify that stalled native retirement retains lease and token without blocking store or UI
    fn stalled_native_retirement_retains_lease_and_token_without_blocking_store_or_ui() {
        // This regression deliberately exercises the process-global recorder AND
        // native keyboard admission. Other runtime tests legitimately cancel all
        // input/readers on shutdown. An isolated copy of this same test executable
        // preserves those real production boundaries without serializing the suite
        // or teaching production cancellation to ignore test owners
        const CHILD: &str = "SDM_RECORDER_RETIREMENT_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "keyboard_macro::recorder::retirement_tests::stalled_native_retirement_retains_lease_and_token_without_blocking_store_or_ui",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .expect("start isolated recorder retirement regression");
            assert!(
                output.status.success(),
                "isolated recorder regression failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(assert_native_retirement());
    }

    // Assert native retirement
    async fn assert_native_retirement() {
        for (fail_cleanup, capture_failure) in [(false, false), (true, false), (false, true)] {
            let (capture, release) =
                super::super::capture::stalled_test_capture(fail_cleanup, capture_failure);
            let mut session = Session::new(
                "retirement-test",
                KeyboardMacroCaptureMode::Global,
                vec![],
                Instant::now(),
            );
            session.capture = Some(capture);
            session.lease = Some(super::super::playback::try_recording_lease().unwrap());
            let id = session.view.session_id.clone();
            store().lock().unwrap().session = Some(session);
            let worker_id = id.clone();
            let worker = std::thread::spawn(move || watchdog(&worker_id));
            let mut stop = Box::pin(stop("retirement-test", &id));
            // Poll Stop so its cancellation signal is sent, then abandon the wait
            assert!(tokio::time::timeout(Duration::from_millis(80), &mut stop)
                .await
                .is_err());
            assert!(
                wait_retirement_for("retirement-test", &id, Duration::from_millis(20))
                    .await
                    .is_err()
            );
            assert!(store().try_lock().is_ok());
            assert!(editor_retirement_blocked());
            assert!(super::super::playback::try_recording_lease().is_err());
            assert!(start("other", "new", KeyboardMacroCaptureMode::InApp, vec![]).is_err());
            cancel("wrong-owner", &id).await.unwrap();
            cancel("retirement-test", "stale-token").await.unwrap();
            assert!(store().lock().unwrap().session.as_ref().unwrap().active());
            let mut cancel = Box::pin(cancel("retirement-test", &id));
            assert!(tokio::time::timeout(Duration::from_millis(80), &mut cancel)
                .await
                .is_err());
            assert!(store().try_lock().is_ok());
            assert!(super::super::playback::try_recording_lease().is_err());
            // Cancel supersedes Stop even with an outstanding Stop future
            assert_eq!(
                store().lock().unwrap().session.as_ref().unwrap().view.state,
                KeyboardMacroSessionState::Canceled
            );
            release.send(()).unwrap();
            let result = tokio::time::timeout(Duration::from_secs(2), cancel)
                .await
                .unwrap();
            assert_eq!(result.is_err(), fail_cleanup);
            assert!(stop.await.is_err());
            assert_eq!(editor_retirement_blocked(), fail_cleanup);
            assert_eq!(
                super::super::playback::try_recording_lease().is_ok(),
                !fail_cleanup
            );
            if fail_cleanup {
                // Repeated Cancel and owner cancellation must preserve the failure receipt
                assert!(super::cancel("retirement-test", &id).await.is_err());
                cancel_owner("retirement-test");
                assert!(editor_retirement_blocked());
                assert!(start("other", "new", KeyboardMacroCaptureMode::InApp, vec![]).is_err());
            }
            store().lock().unwrap().session.take();
            worker.join().unwrap();
        }
        for page_reload in [false, true] {
            let (capture, release) = super::super::capture::stalled_test_capture(false, false);
            let mut session = Session::new(
                "main:retirement-test",
                KeyboardMacroCaptureMode::Global,
                vec![],
                Instant::now(),
            );
            session.capture = Some(capture);
            session.lease = Some(super::super::playback::try_recording_lease().unwrap());
            session
                .accept(KeyboardMacroCaptureEvent {
                    kind: KeyboardMacroEventKind::KeyDown,
                    key: PhysicalKey::KeyA,
                    elapsed_ms: 0,
                })
                .unwrap();
            let id = session.view.session_id.clone();
            store().lock().unwrap().session = Some(session);
            let worker_id = id.clone();
            let worker = std::thread::spawn(move || watchdog(&worker_id));
            // Drop the IPC future entirely. Native ownership must not depend on it
            assert!(tokio::time::timeout(
                Duration::from_millis(30),
                stop("main:retirement-test", &id)
            )
            .await
            .is_err());
            if page_reload {
                cancel_owner("main");
            }
            assert!(editor_retirement_blocked());
            assert!(super::super::playback::try_recording_lease().is_err());
            release.send(()).unwrap();
            wait_retirement("main:retirement-test", &id).await.unwrap();
            let result = stop("main:retirement-test", &id).await;
            if page_reload {
                assert!(result.is_err());
            } else {
                let result = result.unwrap();
                assert_eq!(result.session.state, KeyboardMacroSessionState::Stopped);
                assert_eq!(result.session.balanced_releases, 1);
                assert_eq!(result.recording.events.len(), 2);
                assert!(stop("main:retirement-test", &id).await.is_ok());
            }
            assert!(!editor_retirement_blocked());
            store().lock().unwrap().session.take();
            worker.join().unwrap();
        }
        for cancel_before in [true, false] {
            let mut session = Session::new(
                "main:startup-test",
                KeyboardMacroCaptureMode::Global,
                vec![],
                Instant::now(),
            );
            session.initializing = true;
            session.lease = Some(super::super::playback::try_recording_lease().unwrap());
            let id = session.view.session_id.clone();
            store().lock().unwrap().session = Some(session);
            if cancel_before {
                cancel_owner("main");
                initialize_capture(&id, |_| {
                    panic!("canceled initializer must never start native capture")
                });
            } else {
                let (entered, observe) = std::sync::mpsc::channel();
                let (release, wait) = std::sync::mpsc::channel();
                let worker_id = id.clone();
                let worker = std::thread::spawn(move || {
                    initialize_capture(&worker_id, |stop| {
                        entered.send(stop.clone()).unwrap();
                        wait.recv().unwrap();
                        assert!(stop.load(Ordering::Acquire));
                        Err(super::super::capture::CaptureError::EmergencyStop)
                    })
                });
                let stop = observe.recv_timeout(Duration::from_secs(2)).unwrap();
                cancel_owner("main");
                assert!(stop.load(Ordering::Acquire));
                assert!(store().try_lock().is_ok());
                assert!(editor_retirement_blocked());
                assert!(super::super::playback::try_recording_lease().is_err());
                release.send(()).unwrap();
                worker.join().unwrap();
            }
            assert!(!editor_retirement_blocked());
            assert!(super::super::playback::try_recording_lease().is_ok());
            assert_eq!(
                store().lock().unwrap().session.as_ref().unwrap().view.state,
                KeyboardMacroSessionState::Canceled
            );
            store().lock().unwrap().session.take();
        }
    }
}
