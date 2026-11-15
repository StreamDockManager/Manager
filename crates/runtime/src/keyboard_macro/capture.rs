//! Scoped passive keyboard capture. Readiness never installs hooks or requests permission
//!
//! Native callbacks only normalize into a fixed-size queue. They do not call application
//! code, persist keys, emit events, or suppress input. The owner must retain a recording
//! transaction lease and stop this handle before releasing that lease
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
use super::MAX_DURATION_MS;
use super::{KeyboardMacroEventKind, PhysicalKey};
use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc::{Receiver, TryRecvError},
        Arc,
    },
    thread::JoinHandle,
    time::Instant,
};

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
use std::sync::mpsc::{self, SyncSender, TrySendError};

#[cfg(any(target_os = "windows", test))]
mod corroboration;
#[cfg(target_os = "linux")]
mod hyprland;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", target_os = "windows", test))]
mod mappings;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
mod x11;
#[cfg(any(target_os = "linux", test))]
mod x11_state;

/// Supported observations, not an unlock assertion or a qualification bypass
#[cfg(target_os = "macos")]
// Check macOS session observations
pub(crate) fn check_macos_session_observations() -> Result<(), CaptureError> {
    macos::session::check_current()
}

/// Read-only manual session qualification; never enables production input gates
#[cfg(all(target_os = "macos", feature = "native-session-probe"))]
// Run macOS session probe
pub fn run_macos_session_probe(duration: std::time::Duration) -> Result<(), String> {
    macos::session::run_probe(duration)
}

pub const SYNTHETIC_EVENT_MARKER: i64 = 0x5344_4b4d;
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
const QUEUE_CAPACITY: usize = 1024;
const KEY_SLOTS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureReadiness {
    pub available: bool,
    pub implemented: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CaptureError {
    EmergencyStop = 1,
    BufferOverflow,
    PermissionDenied,
    ProtectedInput,
    SessionLocked,
    SessionStateUnavailable,
    HookDisabled,
    UnsupportedKey,
    InitiallyHeldKeys,
    WorkerFailed,
    UnsupportedPlatform,
    SessionExpired,
    PhysicalInterference,
    RawInputUnavailable,
    EmergencyStopUnavailable,
    WorkerUnresponsive,
    InputMismatch,
    DeviceChanged,
    NativeQualificationRequired,
}
impl CaptureError {
    // Report whether graceful
    fn is_graceful(self) -> bool {
        matches!(self, Self::EmergencyStop | Self::SessionExpired)
    }
    // Construct from raw
    fn from_raw(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::EmergencyStop,
            2 => Self::BufferOverflow,
            3 => Self::PermissionDenied,
            4 => Self::ProtectedInput,
            5 => Self::SessionLocked,
            6 => Self::SessionStateUnavailable,
            7 => Self::HookDisabled,
            8 => Self::UnsupportedKey,
            9 => Self::InitiallyHeldKeys,
            10 => Self::WorkerFailed,
            11 => Self::UnsupportedPlatform,
            12 => Self::SessionExpired,
            13 => Self::PhysicalInterference,
            14 => Self::RawInputUnavailable,
            15 => Self::EmergencyStopUnavailable,
            16 => Self::WorkerUnresponsive,
            17 => Self::InputMismatch,
            18 => Self::DeviceChanged,
            19 => Self::NativeQualificationRequired,
            _ => return None,
        })
    }
}
impl fmt::Display for CaptureError {
    // Format capture for the requested formatter
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmergencyStop => "Keyboard session stopped with its emergency shortcut",
            Self::BufferOverflow => "Keyboard capture stopped because its event buffer filled",
            Self::PermissionDenied => {
                "Keyboard monitoring permission is unavailable or was revoked"
            }
            Self::ProtectedInput => "Keyboard session stopped because protected input is active",
            Self::SessionLocked => {
                "Keyboard session stopped because the desktop is locked or inactive"
            }
            Self::SessionStateUnavailable => "A supported desktop lock-state check is unavailable",
            Self::HookDisabled => {
                "Keyboard session stopped because its native observer was disabled"
            }
            Self::UnsupportedKey => "Keyboard capture encountered an unsupported physical key",
            Self::InitiallyHeldKeys => {
                "Release all physical keys before starting the keyboard session"
            }
            Self::WorkerFailed => "The native keyboard observer stopped unexpectedly",
            Self::UnsupportedPlatform => "Global keyboard capture is unavailable on this platform",
            Self::SessionExpired => "The keyboard session reached its time limit",
            Self::PhysicalInterference => {
                "Keyboard playback stopped because physical input was detected"
            }
            Self::RawInputUnavailable => "Exclusive passive keyboard observation is unavailable or its owner changed",
            Self::EmergencyStopUnavailable => "The global emergency shortcut is unavailable or already registered",
            Self::WorkerUnresponsive => "The native keyboard safety observer stopped responding",
            Self::InputMismatch => "Keyboard capture stopped because independent physical input observations did not agree",
            Self::DeviceChanged => "Keyboard capture stopped because the keyboard device or desktop session changed",
            Self::NativeQualificationRequired => "Global keyboard capture is implemented but disabled until its native safety qualification is complete",
        })
    }
}
impl std::error::Error for CaptureError {}

// Deliberately no Debug: a handle or event must not accidentally print raw keys
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CapturedEvent {
    pub key: PhysicalKey,
    pub kind: KeyboardMacroEventKind,
    pub elapsed_ms: u64,
}

struct Signal {
    stop: Arc<AtomicBool>,
    ready: AtomicBool,
    completed: AtomicBool,
    terminal: AtomicU8,
    playback_observer: bool,
    admission_error: AtomicU8,
    cleanup_error: AtomicU8,
    physical: [AtomicBool; KEY_SLOTS],
    strict_health: AtomicBool,
    verification_requested: AtomicU64,
    verified: AtomicU64,
    worker_heartbeat: AtomicU64,
    hotkey_heartbeat: AtomicU64,
    epoch: Instant,
}
impl Signal {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
    // Initialize capture state from the supplied inputs
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            ready: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            terminal: AtomicU8::new(0),
            playback_observer: false,
            admission_error: AtomicU8::new(0),
            cleanup_error: AtomicU8::new(0),
            physical: std::array::from_fn(|_| AtomicBool::new(false)),
            strict_health: AtomicBool::new(false),
            verification_requested: AtomicU64::new(0),
            verified: AtomicU64::new(0),
            worker_heartbeat: AtomicU64::new(0),
            hotkey_heartbeat: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }
    // Record the first cleanup failure and fail the capture signal
    fn fail_cleanup(&self, error: CaptureError) {
        let _ = self.cleanup_error.compare_exchange(
            0,
            error as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.fail(error);
    }
    // Mark the capture operation as failed
    fn fail(&self, error: CaptureError) {
        if self.playback_observer
            && matches!(
                error,
                CaptureError::PhysicalInterference | CaptureError::EmergencyStop
            )
        {
            let _ = self.admission_error.compare_exchange(
                0,
                error as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return; // Revoke new output, but keep observing physical releases for cleanup
        }
        // Neither a user stop nor a duration cap establishes success before
        // teardown. Preserve the first graceful reason, but let a real failure
        // override it; never overwrite an already-recorded real failure
        let mut current = self.terminal.load(Ordering::Acquire);
        while current == 0
            || (CaptureError::from_raw(current).is_some_and(CaptureError::is_graceful)
                && !error.is_graceful())
        {
            match self.terminal.compare_exchange(
                current,
                error as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(value) => current = value,
            }
        }
        self.stop.store(true, Ordering::Release);
    }
    // Return elapsed time in milliseconds
    fn elapsed_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
    }
    // Check liveness
    fn check_liveness(&self) {
        if self.strict_health.load(Ordering::Acquire)
            && self.ready.load(Ordering::Acquire)
            && !self.stop.load(Ordering::Acquire)
        {
            let now = self.elapsed_ms();
            if now.saturating_sub(self.worker_heartbeat.load(Ordering::Acquire)) > 250
                || now.saturating_sub(self.hotkey_heartbeat.load(Ordering::Acquire)) > 250
            {
                self.fail(CaptureError::WorkerUnresponsive);
            }
        }
    }

    // Construct the capture error result
    fn error(&self) -> Option<CaptureError> {
        CaptureError::from_raw(self.terminal.load(Ordering::Acquire))
    }
}

pub struct CaptureHandle {
    receiver: Receiver<CapturedEvent>,
    signal: Arc<Signal>,
    thread: Option<JoinHandle<()>>,
}
impl CaptureHandle {
    /// Queued events precede terminal status. EmergencyStop and SessionExpired
    /// preserve a review only after final validation/teardown; real failures interrupt it
    pub fn try_recv(&self) -> Result<Option<CapturedEvent>, CaptureError> {
        self.signal.check_liveness();
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                self.terminal_error().map_or(Ok(None), Err)
            }
        }
    }
    // Defer graceful completion errors until native teardown has finished
    pub fn terminal_error(&self) -> Option<CaptureError> {
        self.signal.check_liveness();
        let error = self.signal.error();
        if error.is_some_and(CaptureError::is_graceful)
            && self.thread.is_some()
            && !self.signal.completed.load(Ordering::Acquire)
        {
            // Final raw/hook agreement and teardown can still invalidate a draft
            None
        } else {
            error
        }
    }
    // Report the ready condition for capture
    pub fn ready(&self) -> bool {
        self.signal.check_liveness();
        self.signal.ready.load(Ordering::Acquire) && self.signal.error().is_none()
    }
    // Read the failure recorded during native capture cleanup
    pub fn cleanup_error(&self) -> Option<CaptureError> {
        CaptureError::from_raw(self.signal.cleanup_error.load(Ordering::Acquire))
    }
    /// Signal only: safe on UI/IPC paths and while holding the session store
    pub fn request_stop(&self) {
        self.signal.stop.store(true, Ordering::Release);
    }
    /// OS teardown must have returned, not merely published its terminal status
    pub fn worker_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
    /// Synchronously detaches the native hook before returning. Callers may then
    /// drain the bounded queue. Repeated calls cannot reattach a hook
    pub fn stop(&mut self) -> Result<(), CaptureError> {
        self.signal.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                self.signal.fail_cleanup(CaptureError::WorkerFailed);
            }
        }
        self.signal.error().map_or(Ok(()), Err)
    }
}
impl Drop for CaptureHandle {
    // Release this capture owner when it leaves scope
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Recording,
    PlaybackObserver,
}

/// A scoped interference observer stores no physical events while playback runs
/// Target identity/focus generation are independently checked by the playback owner
pub struct NativePlaybackObserver {
    handle: CaptureHandle,
}
impl NativePlaybackObserver {
    // Start the capture operation
    pub fn start() -> Result<Self, CaptureError> {
        let mut handle = start_native(Purpose::PlaybackObserver, Arc::new(AtomicBool::new(false)))?;
        let deadline = Instant::now();
        while !handle.signal.ready.load(Ordering::Acquire) {
            if let Some(error) = handle.signal.error() {
                let _ = handle.stop();
                return Err(error);
            }
            if deadline.elapsed() >= std::time::Duration::from_secs(5) {
                handle.signal.fail(CaptureError::WorkerFailed);
                let _ = handle.stop();
                return Err(CaptureError::WorkerFailed);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if let Some(error) = handle.signal.error() {
            let _ = handle.stop();
            return Err(error);
        }
        Ok(Self { handle })
    }
    // Stop the capture operation
    pub fn stop(&mut self) -> Result<(), CaptureError> {
        self.handle.stop()
    }
    // Report the physically down condition for capture
    pub fn physically_down(&self, key: PhysicalKey) -> bool {
        self.handle.signal.physical[key as usize].load(Ordering::Acquire)
    }
    // Check the current capture conditions
    pub fn check(&self) -> Result<(), CaptureError> {
        self.check_health(false)
    }
    // Check health
    fn check_health(&self, cleanup: bool) -> Result<(), CaptureError> {
        let signal = &self.handle.signal;
        let status = || {
            signal.check_liveness();
            if signal.stop.load(Ordering::Acquire) || self.handle.worker_finished() {
                return Err(CaptureError::WorkerFailed);
            }
            if let Some(error) = signal.error() {
                return Err(error);
            }
            if !cleanup {
                if let Some(error) =
                    CaptureError::from_raw(signal.admission_error.load(Ordering::Acquire))
                {
                    return Err(error);
                }
            }
            Ok(())
        };
        status()?;
        if !signal.ready.load(Ordering::Acquire) || !signal.strict_health.load(Ordering::Acquire) {
            return Err(CaptureError::WorkerUnresponsive);
        }
        let request = signal
            .verification_requested
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| CaptureError::WorkerFailed)?
            + 1;
        let started = Instant::now();
        while signal.verified.load(Ordering::Acquire) < request {
            status()?;
            if started.elapsed() > std::time::Duration::from_millis(100) {
                signal.fail(CaptureError::WorkerUnresponsive);
                return Err(CaptureError::WorkerUnresponsive);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        status()
    }
    /// Owned nonblocking guard for an asynchronous native-output queue. It holds
    /// no key data and never extends the native observer subscription lifetime
    pub fn admission_guard(&self) -> Arc<dyn Fn() -> Result<(), String> + Send + Sync> {
        let signal = self.handle.signal.clone();
        Arc::new(move || {
            signal.check_liveness();
            if signal.stop.load(Ordering::Acquire)
                || signal.completed.load(Ordering::Acquire)
                || signal.error().is_some()
                || signal.admission_error.load(Ordering::Acquire) != 0
                || !signal.ready.load(Ordering::Acquire)
                || !signal.strict_health.load(Ordering::Acquire)
            {
                Err("Keyboard input admission was revoked".into())
            } else {
                Ok(())
            }
        })
    }
    /// Queue-time cleanup veto observes current physical ownership independently
    /// of the press-admission cancellation bit
    pub fn release_guard(
        &self,
        key: PhysicalKey,
    ) -> Arc<dyn Fn() -> Result<(), String> + Send + Sync> {
        let signal = self.handle.signal.clone();
        Arc::new(move || {
            signal.check_liveness();
            if signal.stop.load(Ordering::Acquire)
                || signal.completed.load(Ordering::Acquire)
                || signal.error().is_some()
                || signal.physical[key as usize].load(Ordering::Acquire)
            {
                Err("Keyboard cleanup observation or ownership changed".into())
            } else {
                Ok(())
            }
        })
    }
    /// Last nonblocking press veto after target checks, inside ownership lock
    /// Does not replace the fresh worker barrier performed by check()
    pub fn admission_ready(&self) -> Result<(), String> {
        let signal = &self.handle.signal;
        signal.check_liveness();
        if signal.stop.load(Ordering::Acquire)
            || self.handle.worker_finished()
            || signal.error().is_some()
            || signal.admission_error.load(Ordering::Acquire) != 0
            || !signal.ready.load(Ordering::Acquire)
            || !signal.strict_health.load(Ordering::Acquire)
        {
            return Err("Keyboard input admission was revoked".into());
        }
        Ok(())
    }
    /// Nonblocking final veto, safe inside the input ownership mutex
    pub fn release_ready(&self, key: PhysicalKey) -> Result<(), String> {
        let signal = &self.handle.signal;
        signal.check_liveness();
        if signal.stop.load(Ordering::Acquire)
            || self.handle.worker_finished()
            || signal.error().is_some()
            || self.physically_down(key)
        {
            return Err(
                "Keyboard cleanup cannot safely release this key; ownership is retained".into(),
            );
        }
        Ok(())
    }
    /// Outside the native input mutex: wait only for this physical key, never
    /// discard synthetic ownership because a user currently holds the same key
    pub fn prepare_release(&self, key: PhysicalKey) -> Result<(), String> {
        wait_for_release(std::time::Duration::from_millis(500), || {
            self.check_health(true).map_err(|error| error.to_string())?;
            Ok(self.physically_down(key))
        })
    }
}

// Wait for for release
fn wait_for_release(
    timeout: std::time::Duration,
    mut sample: impl FnMut() -> Result<bool, String>,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if !sample()? {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err("Keyboard cleanup is incomplete because the physical key is still held; synthetic ownership is retained".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

// Return the platform-specific emergency stop shortcut
pub fn global_emergency_shortcut() -> &'static str {
    if cfg!(target_os = "windows") {
        "Control+Shift+F11"
    } else {
        "Control+Shift+F12"
    }
}

// Query native capture readiness for the selected platform backend
pub fn global_readiness() -> CaptureReadiness {
    #[cfg(target_os = "macos")]
    {
        macos::readiness()
    }
    #[cfg(target_os = "windows")]
    {
        windows::readiness()
    }
    #[cfg(target_os = "linux")]
    {
        if super::hyprland::selected() {
            hyprland::readiness()
        } else {
            x11::readiness()
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        CaptureReadiness { available: false, implemented: false, reason: Some("This platform has no native keyboard capture adapter. In-app recording and manual editing remain available.".into()) }
    }
}
// Query readiness of the native interference observer used during playback
pub fn playback_observer_readiness() -> CaptureReadiness {
    #[cfg(target_os = "linux")]
    {
        if super::hyprland::selected() {
            hyprland::readiness()
        } else {
            x11::observer_readiness()
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        global_readiness()
    }
}
// Start global
pub fn start_global(stop: Arc<AtomicBool>) -> Result<CaptureHandle, CaptureError> {
    start_native(Purpose::Recording, stop)
}
// Start native
fn start_native(purpose: Purpose, stop: Arc<AtomicBool>) -> Result<CaptureHandle, CaptureError> {
    if stop.load(Ordering::Acquire) {
        return Err(CaptureError::EmergencyStop);
    }
    #[cfg(target_os = "macos")]
    {
        macos::start(purpose, stop)
    }
    #[cfg(target_os = "windows")]
    {
        windows::start(purpose, stop)
    }
    #[cfg(target_os = "linux")]
    {
        if super::hyprland::selected() {
            hyprland::start(purpose, stop)
        } else {
            x11::start(purpose, stop)
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (purpose, stop);
        Err(CaptureError::UnsupportedPlatform)
    }
}

/// Hook-thread-owned state: no mutex, allocation, or unbounded work in callbacks
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
struct Sink {
    sender: SyncSender<CapturedEvent>,
    signal: Arc<Signal>,
    purpose: Purpose,
    started: Instant,
    held: [bool; KEY_SLOTS],
    pending: [Option<CapturedEvent>; 16],
    pending_len: usize,
    emergency_key: PhysicalKey,
}
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
impl Sink {
    #[cfg(test)]
    // Create a bounded capture channel with a new stop signal
    fn channel(purpose: Purpose) -> (Self, CaptureHandle) {
        Self::channel_with_stop(purpose, Arc::new(AtomicBool::new(false)))
    }
    // Create a bounded capture channel sharing the supplied stop signal
    fn channel_with_stop(purpose: Purpose, stop: Arc<AtomicBool>) -> (Self, CaptureHandle) {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let mut signal = Signal::new();
        signal.stop = stop;
        signal.playback_observer = purpose == Purpose::PlaybackObserver;
        let signal = Arc::new(signal);
        (
            Self {
                sender,
                signal: signal.clone(),
                purpose,
                started: Instant::now(),
                held: [false; KEY_SLOTS],
                pending: [None; 16],
                pending_len: 0,
                emergency_key: PhysicalKey::F12,
            },
            CaptureHandle {
                receiver,
                signal,
                thread: None,
            },
        )
    }
    // Report the stopped condition for capture
    fn stopped(&self) -> bool {
        self.signal.stop.load(Ordering::Acquire)
    }
    // Report the expired condition for capture
    fn expired(&self) -> bool {
        let limit_ms = if self.purpose == Purpose::Recording {
            MAX_DURATION_MS
        } else {
            (MAX_DURATION_MS as f64 / super::MIN_SPEED).ceil() as u64
        };
        if self.started.elapsed().as_millis() > u128::from(limit_ms) {
            self.signal.fail(CaptureError::SessionExpired);
        }
        self.stopped()
    }
    // Append a capture entry
    fn push(&mut self, key: Option<PhysicalKey>, down: bool, repeat: bool) {
        let elapsed_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        self.push_at(key, down, repeat, elapsed_ms, false);
    }
    // Push at
    fn push_at(
        &mut self,
        key: Option<PhysicalKey>,
        down: bool,
        repeat: bool,
        elapsed_ms: u64,
        draining: bool,
    ) {
        let stopped = if draining {
            self.stopped()
        } else {
            self.expired()
        };
        let terminal = self.signal.error();
        if terminal.is_some_and(|error| !error.is_graceful())
            || (stopped && !draining && terminal != Some(CaptureError::SessionExpired))
        {
            return;
        }
        // Corroboration can complete after the cap for an event observed before
        // it. Retain that original timestamp; never admit a post-cap event or
        // retime it to the cap. The recorder adds its own balanced releases
        if self.purpose == Purpose::Recording && elapsed_ms > MAX_DURATION_MS {
            self.signal.fail(CaptureError::SessionExpired);
            return;
        }
        if self.purpose == Purpose::PlaybackObserver {
            let Some(key) = key else {
                self.signal.fail(CaptureError::UnsupportedKey);
                return;
            };
            self.signal.physical[key as usize].store(down, Ordering::Release);
            self.signal.fail(CaptureError::PhysicalInterference);
            return;
        }
        let Some(key) = key else {
            self.signal.fail(CaptureError::UnsupportedKey);
            return;
        };
        let slot = key as usize;
        if slot >= KEY_SLOTS {
            self.signal.fail(CaptureError::UnsupportedKey);
            return;
        }
        // Release events predating admission and duplicate releases are not recordings
        if !down && !self.held[slot] {
            return;
        }
        let kind = if !down {
            KeyboardMacroEventKind::KeyUp
        } else if repeat || self.held[slot] {
            KeyboardMacroEventKind::Repeat
        } else {
            KeyboardMacroEventKind::KeyDown
        };
        if kind == KeyboardMacroEventKind::Repeat && !self.held[slot] {
            self.signal.fail(CaptureError::InitiallyHeldKeys);
            return;
        }
        self.held[slot] = down;
        let event = CapturedEvent {
            key,
            kind,
            elapsed_ms,
        };
        use PhysicalKey::*;
        let control = self.held[ControlLeft as usize] || self.held[ControlRight as usize];
        let shift = self.held[ShiftLeft as usize] || self.held[ShiftRight as usize];
        if key == self.emergency_key && down && control && shift {
            self.pending.fill(None);
            self.pending_len = 0;
            self.signal.fail(CaptureError::EmergencyStop);
            return;
        }
        // Delay only possible stop-chord prefixes. A non-chord key flushes exact
        // timestamps in their original order; modifier-only recordings survive Stop
        let candidate = matches!(key, ControlLeft | ControlRight | ShiftLeft | ShiftRight);
        if candidate && down {
            if self.pending_len == self.pending.len() {
                self.signal.fail(CaptureError::BufferOverflow);
                return;
            }
            self.pending[self.pending_len] = Some(event);
            self.pending_len += 1;
        } else {
            self.flush_pending();
            self.send(event);
        }
    }
    // Flush pending
    fn flush_pending(&mut self) {
        for index in 0..self.pending_len {
            if let Some(event) = self.pending[index].take() {
                self.send(event);
            }
        }
        self.pending_len = 0;
    }
    // Send the capture message
    fn send(&self, event: CapturedEvent) {
        match self.sender.try_send(event) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => self.signal.fail(CaptureError::BufferOverflow),
            Err(TrySendError::Disconnected(_)) => self.signal.fail(CaptureError::WorkerFailed),
        }
    }
    // Finalize the pending capture operation
    fn finish(&mut self) {
        if self
            .signal
            .error()
            .is_none_or(|error| error == CaptureError::SessionExpired)
        {
            // A duration cap keeps legitimate modifier-only prefixes. The
            // emergency chord's prefix is deliberately excluded instead
            self.flush_pending();
        }
    }
}

#[cfg(test)]
// Create a fixture whose completion signal precedes explicitly released teardown
pub(super) fn stalled_test_capture(
    fail_cleanup: bool,
    capture_failure: bool,
) -> (CaptureHandle, mpsc::Sender<()>) {
    let signal = Arc::new(Signal::new());
    let worker_signal = signal.clone();
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
    let (release, wait) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        // Publishing completion is not evidence that OS teardown returned
        worker_signal.completed.store(true, Ordering::Release);
        wait.recv().unwrap();
        if fail_cleanup {
            worker_signal.fail_cleanup(CaptureError::WorkerFailed);
        }
        if capture_failure {
            worker_signal.fail(CaptureError::BufferOverflow);
        }
        drop(sender);
    });
    (
        CaptureHandle {
            receiver,
            signal,
            thread: Some(thread),
        },
        release,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use PhysicalKey::*;
    #[test]
    // Verify that queued press veto does not cancel independent owned release
    fn queued_press_veto_does_not_cancel_independent_owned_release() {
        let (sink, handle) = Sink::channel(Purpose::PlaybackObserver);
        sink.signal.ready.store(true, Ordering::Release);
        sink.signal.strict_health.store(true, Ordering::Release);
        let observer = NativePlaybackObserver { handle };
        let press = observer.admission_guard();
        let release = observer.release_guard(PhysicalKey::KeyA);
        assert!(press().is_ok());
        sink.signal.fail(CaptureError::PhysicalInterference);
        assert!(press().is_err());
        assert!(release().is_ok());
        sink.signal.physical[PhysicalKey::KeyA as usize].store(true, Ordering::Release);
        assert!(release().is_err());
        sink.signal.completed.store(true, Ordering::Release);
    }

    #[test]
    // Verify that stop chord is not emitted and signal bypasses full queue
    fn stop_chord_is_not_emitted_and_signal_bypasses_full_queue() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.push(Some(ControlRight), true, false);
        sink.push(Some(ShiftLeft), true, false);
        sink.push(Some(F12), true, false);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::EmergencyStop)
        ));
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        for _ in 0..QUEUE_CAPACITY {
            sink.push(Some(KeyA), true, false);
        }
        sink.push(Some(KeyA), false, false);
        assert_eq!(handle.terminal_error(), Some(CaptureError::BufferOverflow));
        assert!(sink.stopped());
    }
    #[test]
    // Verify that modifier prefix keeps order and sides and repeat
    fn modifier_prefix_keeps_order_and_sides_and_repeat() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.push(Some(ControlRight), true, false);
        sink.push(Some(KeyA), true, false);
        sink.push(Some(KeyA), true, true);
        sink.push(Some(KeyA), false, false);
        sink.push(Some(ControlRight), false, false);
        let mut values = vec![];
        while let Some(event) = handle.try_recv().unwrap() {
            values.push((event.key, event.kind));
        }
        assert_eq!(
            values,
            vec![
                (ControlRight, KeyboardMacroEventKind::KeyDown),
                (KeyA, KeyboardMacroEventKind::KeyDown),
                (KeyA, KeyboardMacroEventKind::Repeat),
                (KeyA, KeyboardMacroEventKind::KeyUp),
                (ControlRight, KeyboardMacroEventKind::KeyUp)
            ]
        );
    }
    #[test]
    // Verify that observer keeps release tracking after admission revocation
    fn observer_keeps_release_tracking_after_admission_revocation() {
        let (mut sink, handle) = Sink::channel(Purpose::PlaybackObserver);
        sink.push(Some(KeyA), true, false);
        assert!(handle.signal.physical[KeyA as usize].load(Ordering::Acquire));
        assert!(!sink.stopped());
        sink.signal.fail(CaptureError::EmergencyStop);
        sink.push(Some(KeyA), false, false);
        assert!(!handle.signal.physical[KeyA as usize].load(Ordering::Acquire));
        assert!(handle.try_recv().unwrap().is_none());
        assert_eq!(
            CaptureError::from_raw(handle.signal.admission_error.load(Ordering::Acquire)),
            Some(CaptureError::PhysicalInterference)
        );
        sink.signal.fail(CaptureError::WorkerFailed);
        assert!(sink.stopped());
        assert!(matches!(handle.try_recv(), Err(CaptureError::WorkerFailed)));
    }
    #[test]
    // Verify that stop flushes modifier only and is idempotent
    fn stop_flushes_modifier_only_and_is_idempotent() {
        let (mut sink, mut handle) = Sink::channel(Purpose::Recording);
        sink.push(Some(ShiftRight), true, false);
        sink.finish();
        assert!(handle.stop().is_ok());
        assert!(handle.stop().is_ok());
        assert_eq!(handle.try_recv().unwrap().unwrap().key, ShiftRight);
        assert!(handle.try_recv().unwrap().is_none());
        sink.push(Some(KeyA), true, false);
        assert!(handle.try_recv().unwrap().is_none());
    }
    #[test]
    // Verify that unknown keys and unowned repeat fail closed stray up is ignored
    fn unknown_keys_and_unowned_repeat_fail_closed_stray_up_is_ignored() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.push(Some(KeyA), false, false);
        assert!(handle.try_recv().unwrap().is_none());
        sink.push(None, true, false);
        assert_eq!(handle.terminal_error(), Some(CaptureError::UnsupportedKey));
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.push(Some(KeyA), true, true);
        assert_eq!(
            handle.terminal_error(),
            Some(CaptureError::InitiallyHeldKeys)
        );
    }
    #[test]
    // Verify that dropping handle waits for worker exit even with a full queue
    fn dropping_handle_waits_for_worker_exit_even_with_a_full_queue() {
        let (mut sink, mut handle) = Sink::channel(Purpose::Recording);
        for _ in 0..QUEUE_CAPACITY {
            sink.push(Some(KeyA), true, false);
        }
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = exited.clone();
        handle.thread = Some(std::thread::spawn(move || {
            while !sink.stopped() {
                std::thread::yield_now();
            }
            sink.finish();
            worker_exited.store(true, Ordering::Release);
        }));
        drop(handle);
        assert!(exited.load(Ordering::Acquire));
    }
    #[test]
    // Verify that expiration and terminal status prevent later events
    fn expiration_and_terminal_status_prevent_later_events() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.started = Instant::now() - std::time::Duration::from_millis(MAX_DURATION_MS + 1);
        sink.push(Some(KeyA), true, false);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::SessionExpired)
        ));
        sink.push(Some(KeyB), true, false);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::SessionExpired)
        ));
    }
    #[test]
    // Verify that platform emergency key excludes its modifier prefix
    fn platform_emergency_key_excludes_its_modifier_prefix() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.emergency_key = F11;
        sink.push(Some(ControlLeft), true, false);
        sink.push(Some(ShiftRight), true, false);
        sink.push(Some(F11), true, false);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::EmergencyStop)
        ));
    }
    #[test]
    // Verify that a teardown failure overrides user stop without exposing keys
    fn a_teardown_failure_overrides_user_stop_without_exposing_keys() {
        let (sink, mut handle) = Sink::channel(Purpose::Recording);
        let (done_tx, done_rx) = mpsc::sync_channel::<()>(0);
        handle.thread = Some(std::thread::spawn(move || {
            let _ = done_rx.recv();
        }));
        sink.signal.fail(CaptureError::EmergencyStop);
        assert!(handle.try_recv().unwrap().is_none());
        assert_eq!(handle.terminal_error(), None);
        sink.signal.fail(CaptureError::InputMismatch);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::InputMismatch)
        ));
        done_tx.send(()).unwrap();
        assert_eq!(handle.stop(), Err(CaptureError::InputMismatch));
    }
    #[test]
    // Verify that corroborated stop drain preserves original timestamps
    fn corroborated_stop_drain_preserves_original_timestamps() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.push_at(Some(KeyA), true, false, 7, false);
        sink.signal.stop.store(true, Ordering::Release);
        sink.push_at(Some(KeyA), false, false, 42, true);
        assert_eq!(handle.try_recv().unwrap().unwrap().elapsed_ms, 7);
        assert_eq!(handle.try_recv().unwrap().unwrap().elapsed_ms, 42);
    }
    #[test]
    // Verify that stale native pumps fail closed and never claim ready
    fn stale_native_pumps_fail_closed_and_never_claim_ready() {
        let (sink, handle) = Sink::channel(Purpose::Recording);
        assert!(!handle.ready());
        sink.signal.ready.store(true, Ordering::Release);
        assert!(handle.ready());
        let mut stale = Signal::new();
        stale.epoch = Instant::now() - std::time::Duration::from_millis(260);
        stale.strict_health.store(true, Ordering::Release);
        stale.ready.store(true, Ordering::Release);
        stale.check_liveness();
        assert_eq!(stale.error(), Some(CaptureError::WorkerUnresponsive));
    }
    #[test]
    // Verify that playback barrier requires an independent worker acknowledgment
    fn playback_barrier_requires_an_independent_worker_acknowledgment() {
        let (sink, mut handle) = Sink::channel(Purpose::PlaybackObserver);
        sink.signal.strict_health.store(true, Ordering::Release);
        sink.signal.ready.store(true, Ordering::Release);
        let signal = sink.signal.clone();
        handle.thread = Some(std::thread::spawn(move || {
            while signal.verification_requested.load(Ordering::Acquire) == 0 {
                std::thread::yield_now();
            }
            signal.verified.store(
                signal.verification_requested.load(Ordering::Acquire),
                Ordering::Release,
            );
            while !signal.stop.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
        }));
        let observer = NativePlaybackObserver { handle };
        assert!(observer.check().is_ok());
        assert!(matches!(
            observer.check(),
            Err(CaptureError::WorkerUnresponsive)
        ));
    }
    #[test]
    // Verify that playback observer allows the slowest valid schedule
    fn playback_observer_allows_the_slowest_valid_schedule() {
        let (mut sink, _handle) = Sink::channel(Purpose::PlaybackObserver);
        sink.started = Instant::now() - std::time::Duration::from_millis(MAX_DURATION_MS + 1);
        assert!(!sink.expired());
        let longest = (MAX_DURATION_MS as f64 / super::super::MIN_SPEED).ceil() as u64;
        sink.started = Instant::now() - std::time::Duration::from_millis(longest + 1);
        assert!(sink.expired());
    }
    #[test]
    // Verify that first graceful reason survives but any real failure overrides it
    fn first_graceful_reason_survives_but_any_real_failure_overrides_it() {
        for first in [CaptureError::EmergencyStop, CaptureError::SessionExpired] {
            for second in [CaptureError::EmergencyStop, CaptureError::SessionExpired] {
                let signal = Signal::new();
                signal.fail(first);
                signal.fail(second);
                assert_eq!(signal.error(), Some(first));
                signal.fail(CaptureError::InputMismatch);
                assert_eq!(signal.error(), Some(CaptureError::InputMismatch));
                signal.fail(CaptureError::RawInputUnavailable);
                signal.fail(second);
                assert_eq!(signal.error(), Some(CaptureError::InputMismatch));
            }
        }
    }
    #[test]
    // Verify that expiry is not a success until worker completion
    fn expiry_is_not_a_success_until_worker_completion() {
        let (sink, mut handle) = Sink::channel(Purpose::Recording);
        let (finish_tx, finish_rx) = mpsc::sync_channel::<()>(0);
        let (done_tx, done_rx) = mpsc::sync_channel::<()>(0);
        let worker_signal = sink.signal.clone();
        handle.thread = Some(std::thread::spawn(move || {
            finish_rx.recv().unwrap();
            worker_signal.completed.store(true, Ordering::Release);
            done_tx.send(()).unwrap();
        }));
        sink.signal.fail(CaptureError::SessionExpired);
        assert_eq!(handle.terminal_error(), None);
        assert!(handle.try_recv().unwrap().is_none());
        finish_tx.send(()).unwrap();
        done_rx.recv().unwrap();
        assert_eq!(handle.terminal_error(), Some(CaptureError::SessionExpired));
        assert_eq!(handle.stop(), Err(CaptureError::SessionExpired));
    }
    #[test]
    // Verify that expiry followed by teardown error never returns a cap review
    fn expiry_followed_by_teardown_error_never_returns_a_cap_review() {
        let (sink, mut handle) = Sink::channel(Purpose::Recording);
        let (finish_tx, finish_rx) = mpsc::sync_channel::<()>(0);
        handle.thread = Some(std::thread::spawn(move || {
            finish_rx.recv().unwrap();
        }));
        sink.signal.fail(CaptureError::SessionExpired);
        assert!(handle.try_recv().unwrap().is_none());
        sink.signal.fail(CaptureError::RawInputUnavailable);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::RawInputUnavailable)
        ));
        finish_tx.send(()).unwrap();
        assert_eq!(handle.stop(), Err(CaptureError::RawInputUnavailable));
    }
    #[test]
    // Verify that cap drains only pre cap observations and flushes modifier prefixes
    fn cap_drains_only_pre_cap_observations_and_flushes_modifier_prefixes() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.started = Instant::now() - std::time::Duration::from_millis(MAX_DURATION_MS + 1);
        sink.push_at(Some(KeyA), true, false, MAX_DURATION_MS - 1, false);
        assert_eq!(sink.signal.error(), Some(CaptureError::SessionExpired));
        assert_eq!(
            handle.try_recv().unwrap().unwrap().elapsed_ms,
            MAX_DURATION_MS - 1
        );
        sink.push_at(Some(KeyA), false, false, MAX_DURATION_MS + 1, true);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::SessionExpired)
        ));
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        sink.push_at(Some(ControlLeft), true, false, MAX_DURATION_MS - 10, false);
        sink.signal.fail(CaptureError::SessionExpired);
        sink.finish();
        let event = handle.try_recv().unwrap().unwrap();
        assert_eq!(event.key, ControlLeft);
        assert_eq!(event.elapsed_ms, MAX_DURATION_MS - 10);
        assert!(matches!(
            handle.try_recv(),
            Err(CaptureError::SessionExpired)
        ));
    }
}

#[cfg(test)]
mod cleanup_wait_tests {
    use super::*;
    #[test]
    // Verify that release wait never claims success for held or unknown input
    fn release_wait_never_claims_success_for_held_or_unknown_input() {
        assert!(wait_for_release(std::time::Duration::ZERO, || Ok(true)).is_err());
        assert!(wait_for_release(std::time::Duration::ZERO, || Err("unknown".into())).is_err());
        let mut samples = [true, true, false].into_iter();
        assert!(
            wait_for_release(std::time::Duration::from_secs(1), || Ok(samples
                .next()
                .unwrap()))
            .is_ok()
        );
    }
    #[test]
    // Verify that cleanup uses fresh live health after admission revocation
    fn cleanup_uses_fresh_live_health_after_admission_revocation() {
        let (mut sink, mut handle) = Sink::channel(Purpose::PlaybackObserver);
        sink.signal.ready.store(true, Ordering::Release);
        sink.signal.strict_health.store(true, Ordering::Release);
        sink.push(Some(PhysicalKey::KeyA), true, false);
        let signal = sink.signal.clone();
        handle.thread = Some(std::thread::spawn(move || {
            while !signal.stop.load(Ordering::Acquire) {
                let request = signal.verification_requested.load(Ordering::Acquire);
                signal
                    .worker_heartbeat
                    .store(signal.elapsed_ms(), Ordering::Release);
                signal
                    .hotkey_heartbeat
                    .store(signal.elapsed_ms(), Ordering::Release);
                signal.verified.store(request, Ordering::Release);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }));
        let observer = NativePlaybackObserver { handle };
        assert!(matches!(
            observer.check(),
            Err(CaptureError::PhysicalInterference)
        ));
        assert!(observer.release_ready(PhysicalKey::KeyA).is_err());
        sink.push(Some(PhysicalKey::KeyA), false, false);
        assert!(observer.prepare_release(PhysicalKey::KeyA).is_ok());
        assert!(observer.release_ready(PhysicalKey::KeyA).is_ok());
        sink.signal.fail(CaptureError::ProtectedInput);
        assert!(observer.prepare_release(PhysicalKey::KeyA).is_err());
        assert!(observer.release_ready(PhysicalKey::KeyA).is_err());
    }
    #[test]
    // Verify that recording stop semantics remain terminal
    fn recording_stop_semantics_remain_terminal() {
        let (sink, _) = Sink::channel(Purpose::Recording);
        sink.signal.fail(CaptureError::EmergencyStop);
        assert!(sink.stopped());
        assert_eq!(sink.signal.error(), Some(CaptureError::EmergencyStop));
    }
}
