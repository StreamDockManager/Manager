//! Scoped, revocation-only macOS session defenses. A positive console/login
//! snapshot or an availability notification is NEVER evidence of screen unlock
//! Lock detection is best effort: protected data lagged graphical lock by about
//! 10 seconds in a user-confirmed probe. Stop macros before locking; unknown or
//! restricted observations still deny, and detected revocation remains sticky
//!
//! Public API provenance: CoreGraphics/CGSession.h documents console/login keys
//! and NULL outside a Quartz session. NSWorkspace.h documents sleep, display and
//! session notifications; NSApplication.h documents protected-data notifications
//! (macOS 12+, below this app's macOS 14 minimum). The latter belong to the default
//! notification center; workspace notifications belong to NSWorkspace's center
//!
//! https://developer.apple.com/documentation/appkit/nsworkspace/sessiondidresignactivenotification
//! https://developer.apple.com/documentation/appkit/nsapplicationprotecteddatawillbecomeunavailablenotification
//! https://developer.apple.com/documentation/coregraphics/cgsessioncopycurrentdictionary()
//!
//! EndpointSecurity's LoginWindow events are not a substitute: ESMessage.h marks
//! these as userspace-produced events and says graphical IDs are NOT audit IDs
//! Its sequence counters detect loss on a later message, not a missing final
//! message or the current lock state at subscription. Its system-wide client also
//! requires an Apple entitlement, root and Full Disk Access. The macOS 27
//! descendants-only client is unprivileged, but cannot observe loginwindow (not
//! our descendant) or establish a current graphical-session snapshot. No privileged helper,
//! private CGSession lock key, Keychain probe or permission change is used here
//! https://developer.apple.com/documentation/endpointsecurity/client
//! https://developer.apple.com/documentation/endpointsecurity/es_message_t/seq_num
use super::super::{CaptureError, Signal};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSApplication, NSApplicationProtectedDataDidBecomeAvailableNotification,
    NSApplicationProtectedDataWillBecomeUnavailableNotification, NSWorkspace,
    NSWorkspaceDidWakeNotification, NSWorkspaceScreensDidSleepNotification,
    NSWorkspaceScreensDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification,
    NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{
    NSNotification, NSNotificationCenter, NSNotificationName, NSObjectProtocol,
};
use std::{
    ffi::c_void,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

type Cf = *const c_void;
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> Cf;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dictionary: Cf, key: Cf) -> Cf;
    fn CFGetTypeID(value: Cf) -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(value: Cf) -> bool;
    fn CFRelease(value: Cf);
}

struct OwnedCf(Cf);
impl Drop for OwnedCf {
    // Release this session owner when it leaves scope
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}
// Read a typed boolean from the native session dictionary
fn boolean(dictionary: Cf, key: &str) -> Option<bool> {
    let key = OwnedCf(unsafe { crate::feedback::cf_string(key) });
    if key.0.is_null() {
        return None;
    }
    let value = unsafe { CFDictionaryGetValue(dictionary, key.0) };
    if value.is_null() || unsafe { CFGetTypeID(value) != CFBooleanGetTypeID() } {
        None
    } else {
        Some(unsafe { CFBooleanGetValue(value) })
    }
}
// Validate console
fn validate_console(
    on_console: Option<bool>,
    login_done: Option<bool>,
) -> Result<(), CaptureError> {
    match (on_console, login_done) {
        (Some(true), Some(true)) => Ok(()),
        (Some(false), _) | (_, Some(false)) => Err(CaptureError::SessionLocked),
        _ => Err(CaptureError::SessionStateUnavailable),
    }
}
/// A recent observation of supported session signals, NOT proof of graphical unlock
/// Protected data can remain available while locked; callers accept best-effort detection
#[derive(Clone, Copy, Debug)]
pub(super) struct Snapshot {
    pub(super) on_console: Option<bool>,
    pub(super) login_done: Option<bool>,
    pub(super) protected_data_available: Option<bool>,
    pub(super) secure_input: Option<bool>,
    observed: Instant,
}
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(100);
impl Snapshot {
    // Check at
    fn check_at(&self, now: Instant) -> Result<(), CaptureError> {
        if now
            .checked_duration_since(self.observed)
            .is_none_or(|age| age > SNAPSHOT_TIMEOUT)
        {
            return Err(CaptureError::SessionStateUnavailable);
        }
        validate_console(self.on_console, self.login_done)?;
        match (self.protected_data_available, self.secure_input) {
            (Some(false), _) | (_, Some(true)) => Err(CaptureError::ProtectedInput),
            (Some(true), Some(false)) => Ok(()),
            _ => Err(CaptureError::SessionStateUnavailable),
        }
    }
    // Check the current session conditions
    pub(super) fn check(&self) -> Result<(), CaptureError> {
        self.check_at(Instant::now())
    }
    /// Callback-only guard: scalar validation, with no main-queue dispatch or wait
    pub(super) fn admit_callback(&self, signal: &Signal) -> bool {
        match self.check() {
            Ok(()) => !signal.stop.load(Ordering::Acquire),
            Err(error) => {
                signal.fail(error);
                false
            }
        }
    }
}
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    // Read only on the main thread; do not create an application during CLI preflight
    static NSApp: *mut NSApplication;
}
// Inspect native session and screen state on the macOS main thread
fn snapshot_on_main() -> Result<Snapshot, CaptureError> {
    let _main = objc2::MainThreadMarker::new().ok_or(CaptureError::SessionStateUnavailable)?;
    objc2::rc::autoreleasepool(|_| {
        let dictionary = OwnedCf(unsafe { CGSessionCopyCurrentDictionary() });
        let (on_console, login_done) = if dictionary.0.is_null() {
            (None, None)
        } else {
            (
                boolean(dictionary.0, "kCGSSessionOnConsoleKey"),
                boolean(dictionary.0, "kCGSessionLoginDoneKey"),
            )
        };
        Ok(Snapshot {
            on_console,
            login_done,
            protected_data_available: unsafe { NSApp.as_ref() }
                .map(|app| app.isProtectedDataAvailable()),
            secure_input: Some(unsafe { IsSecureEventInputEnabled() }),
            observed: Instant::now(),
        })
    })
}
// Receive snapshot
fn receive_snapshot(
    result: mpsc::Receiver<Result<Snapshot, CaptureError>>,
    timeout: Duration,
) -> Result<Snapshot, CaptureError> {
    result
        .recv_timeout(timeout)
        .map_err(|_| CaptureError::SessionStateUnavailable)?
}
// Check current
pub(crate) fn check_current() -> Result<(), CaptureError> {
    snapshot()?.check()
}
// Return a snapshot of session state
pub(super) fn snapshot() -> Result<Snapshot, CaptureError> {
    if objc2::MainThreadMarker::new().is_some() {
        return snapshot_on_main();
    }
    let (reply, result) = mpsc::sync_channel(1);
    dispatch2::DispatchQueue::main().exec_async(move || {
        // A late read cannot capture/inject or reauthorize a timed-out session
        let _ = reply.send(snapshot_on_main());
    });
    receive_snapshot(result, SNAPSHOT_TIMEOUT)
}

#[derive(Clone, Copy)]
enum Change {
    Sleep,
    Wake,
    DisplaySleep,
    DisplayWake,
    SessionLeft,
    SessionEntered,
    ProtectedDataLost,
    ProtectedDataReturned,
}
impl Change {
    // Construct the session error result
    fn error(self) -> CaptureError {
        match self {
            Self::ProtectedDataLost | Self::ProtectedDataReturned => CaptureError::ProtectedInput,
            _ => CaptureError::SessionLocked,
        }
    }
}

// A single atomic admits callbacks or closes admission. Tokens may be removed
// while another thread is already delivering a notification; those callbacks
// hold an Arc and must finish before a graceful terminal result can be published
const CLOSED: usize = 1 << (usize::BITS - 1);
struct Delivery {
    state: AtomicUsize,
    generation: AtomicU64,
    signal: Arc<Signal>,
}
struct InFlight<'a>(&'a Delivery);
impl Drop for InFlight<'_> {
    // Release this session owner when it leaves scope
    fn drop(&mut self) {
        self.0.state.fetch_sub(1, Ordering::Release);
    }
}
impl Delivery {
    // Initialize session state from the supplied inputs
    fn new(signal: Arc<Signal>) -> Self {
        Self {
            state: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
            signal,
        }
    }
    // Enter the protected session scope
    fn enter(&self) -> Option<InFlight<'_>> {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            if current & CLOSED != 0 {
                return None;
            }
            if current == CLOSED - 1 {
                self.signal.fail(CaptureError::WorkerFailed);
                return None;
            }
            match self.state.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(InFlight(self)),
                Err(value) => current = value,
            }
        }
    }
    // Notify consumers of the session change
    fn notify(&self, change: Change) {
        if let Some(_delivery) = self.enter() {
            // Notification delivery may occur on a different thread. No UI work,
            // allocation, raw keys or retained notification payloads here
            let _ = self
                .generation
                .try_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                    Some(generation.saturating_add(1))
                });
            self.signal.fail(change.error());
        }
    }
    // Close the owned session resource
    fn close(&self) {
        self.state.fetch_or(CLOSED, Ordering::AcqRel);
    }
    // Report the drained condition for session
    fn drained(&self) -> bool {
        self.state.load(Ordering::Acquire) == CLOSED
    }
}

struct Subscription {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}
impl Drop for Subscription {
    // Release this session owner when it leaves scope
    fn drop(&mut self) {
        unsafe { self.center.removeObserver((*self.token).as_ref()) };
    }
}
pub(super) struct SessionWatch {
    delivery: Arc<Delivery>,
    subscriptions: Vec<Subscription>,
}
impl SessionWatch {
    // Start the session operation
    pub(super) fn start(signal: Arc<Signal>) -> Self {
        let mut watch = Self {
            delivery: Arc::new(Delivery::new(signal)),
            subscriptions: Vec::with_capacity(8),
        };
        let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
        // Positive transitions revoke too: wake/unlock/switch-back must never
        // resume an old recording, even if its earlier notification was delayed
        unsafe {
            for (name, change) in [
                (NSWorkspaceWillSleepNotification, Change::Sleep),
                (NSWorkspaceDidWakeNotification, Change::Wake),
                (NSWorkspaceScreensDidSleepNotification, Change::DisplaySleep),
                (NSWorkspaceScreensDidWakeNotification, Change::DisplayWake),
                (
                    NSWorkspaceSessionDidResignActiveNotification,
                    Change::SessionLeft,
                ),
                (
                    NSWorkspaceSessionDidBecomeActiveNotification,
                    Change::SessionEntered,
                ),
            ] {
                watch.subscribe(&workspace, name, change);
            }
            let application = NSNotificationCenter::defaultCenter();
            watch.subscribe(
                &application,
                NSApplicationProtectedDataWillBecomeUnavailableNotification,
                Change::ProtectedDataLost,
            );
            watch.subscribe(
                &application,
                NSApplicationProtectedDataDidBecomeAvailableNotification,
                Change::ProtectedDataReturned,
            );
        }
        watch
    }
    // Subscribe to session updates
    fn subscribe(
        &mut self,
        center: &Retained<NSNotificationCenter>,
        name: &NSNotificationName,
        change: Change,
    ) {
        let delivery = self.delivery.clone();
        let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| delivery.notify(change));
        // No operation queue: run the small callback synchronously on the posting
        // thread, without an additional queue that could reorder revocation
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block)
        };
        self.subscriptions.push(Subscription {
            center: center.clone(),
            token,
        });
    }
    // Stop the session operation
    pub(super) fn stop(&mut self) {
        self.delivery.close();
        self.subscriptions.clear();
        let started = Instant::now();
        while !self.delivery.drained() {
            if started.elapsed() >= Duration::from_millis(100) {
                self.delivery.signal.fail(CaptureError::WorkerUnresponsive);
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for SessionWatch {
    // Release this session owner when it leaves scope
    fn drop(&mut self) {
        self.stop();
    }
}

/// Opt-in read-only GUI probe. No hooks, input, permissions, storage, or runtime startup
#[cfg(feature = "native-session-probe")]
// Run probe
pub(crate) fn run_probe(duration: Duration) -> Result<(), String> {
    use objc2_app_kit::NSApplicationActivationPolicy;
    let main = objc2::MainThreadMarker::new().ok_or("Run the probe on its main thread")?;
    if duration.is_zero() || duration > Duration::from_secs(600) {
        return Err("Probe duration must be between 1 and 600 seconds".into());
    }
    let app = NSApplication::sharedApplication(main);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    let signal = Arc::new(Signal::new());
    // Subscribe before finishLaunching to see the supported inactive-session
    // notification delivered during launch. This is not a screen-lock oracle
    let mut watch = SessionWatch::start(signal);
    app.finishLaunching();
    let started = Instant::now();
    let mut revoked = false;
    while started.elapsed() < duration {
        objc2::rc::autoreleasepool(|_| {
            let current = snapshot_on_main().ok();
            let generation = watch.delivery.generation.load(Ordering::Acquire);
            let healthy = current
                .as_ref()
                .is_some_and(|snapshot| snapshot.check().is_ok());
            revoked |= !healthy || generation != 0;
            println!(
                "{}",
                serde_json::json!({
                    "elapsed_ms": started.elapsed().as_millis(),
                    "unix_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|time| time.as_millis()),
                    "on_console": current.and_then(|s| s.on_console),
                    "login_done": current.and_then(|s| s.login_done),
                    "protected_data_available": current.and_then(|s| s.protected_data_available),
                    "secure_input": current.and_then(|s| s.secure_input),
                    "observations_healthy": healthy,
                    "revocation_generation": generation,
                    "revoked": revoked,
                })
            );
            unsafe {
                super::CFRunLoopRunInMode(super::kCFRunLoopDefaultMode, 0.1, false);
            }
        });
    }
    watch.stop();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Report whether the session observation is healthy
    fn healthy(now: Instant) -> Snapshot {
        Snapshot {
            on_console: Some(true),
            login_done: Some(true),
            protected_data_available: Some(true),
            secure_input: Some(false),
            observed: now,
        }
    }
    #[test]
    // Verify that supported observations reject unknown protected and stale states
    fn supported_observations_reject_unknown_protected_and_stale_states() {
        let now = Instant::now();
        let initial = healthy(now);
        assert_eq!(initial.check_at(now), Ok(())); // healthy signals, NOT unlocked proof
        assert_eq!(
            initial.check_at(now + SNAPSHOT_TIMEOUT + Duration::from_nanos(1)),
            Err(CaptureError::SessionStateUnavailable)
        );
        assert_eq!(
            initial.check_at(now - Duration::from_millis(1)),
            Err(CaptureError::SessionStateUnavailable)
        );
        for field in 0..4 {
            let mut missing = initial;
            match field {
                0 => missing.on_console = None,
                1 => missing.login_done = None,
                2 => missing.protected_data_available = None,
                _ => missing.secure_input = None,
            }
            assert_eq!(
                missing.check_at(now),
                Err(CaptureError::SessionStateUnavailable)
            );
        }
        let mut protected = initial;
        protected.secure_input = Some(true);
        assert_eq!(protected.check_at(now), Err(CaptureError::ProtectedInput));
        protected = initial;
        protected.protected_data_available = Some(false);
        assert_eq!(protected.check_at(now), Err(CaptureError::ProtectedInput));
    }
    #[test]
    // Verify that callback rejects stale or revoked snapshot without UI dispatch
    fn callback_rejects_stale_or_revoked_snapshot_without_ui_dispatch() {
        // Pure data on a worker with no pumping main queue: callback denial is immediate
        std::thread::spawn(|| {
            let signal = Signal::new();
            let stale = healthy(Instant::now() - SNAPSHOT_TIMEOUT - Duration::from_millis(1));
            assert!(!stale.admit_callback(&signal));
            assert_eq!(signal.error(), Some(CaptureError::SessionStateUnavailable));
            assert!(!healthy(Instant::now()).admit_callback(&signal));
            let signal = Signal::new();
            let mut protected = healthy(Instant::now());
            protected.secure_input = Some(true);
            assert!(!protected.admit_callback(&signal));
            assert_eq!(signal.error(), Some(CaptureError::ProtectedInput));
        })
        .join()
        .unwrap();
    }
    #[test]
    // Verify that main queue timeout and disconnect never supply a stale success
    fn main_queue_timeout_and_disconnect_never_supply_a_stale_success() {
        let (send, receive) = mpsc::sync_channel(1);
        assert_eq!(
            receive_snapshot(receive, Duration::ZERO).unwrap_err(),
            CaptureError::SessionStateUnavailable
        );
        assert!(send.send(Ok(healthy(Instant::now()))).is_err());
        let (send, receive) = mpsc::sync_channel(1);
        drop(send);
        assert_eq!(
            receive_snapshot(receive, Duration::ZERO).unwrap_err(),
            CaptureError::SessionStateUnavailable
        );
    }
    #[test]
    // Verify that background snapshot cannot call thread affine apis directly
    fn background_snapshot_cannot_call_thread_affine_apis_directly() {
        assert_eq!(
            std::thread::spawn(snapshot_on_main)
                .join()
                .unwrap()
                .unwrap_err(),
            CaptureError::SessionStateUnavailable
        );
    }
    #[test]
    // Verify that console membership cannot enable locked or unknown session
    fn console_membership_cannot_enable_locked_or_unknown_session() {
        assert_eq!(validate_console(Some(true), Some(true)), Ok(()));
        for (console, login) in [(None, None), (Some(true), None), (None, Some(true))] {
            assert_eq!(
                validate_console(console, login),
                Err(CaptureError::SessionStateUnavailable)
            );
        }
        assert_eq!(
            validate_console(Some(false), Some(true)),
            Err(CaptureError::SessionLocked)
        );
        assert_eq!(
            validate_console(Some(true), Some(false)),
            Err(CaptureError::SessionLocked)
        );
    }
    #[test]
    // Verify that every transition revokes without resuming on return
    fn every_transition_revokes_without_resuming_on_return() {
        for change in [
            Change::Sleep,
            Change::Wake,
            Change::DisplaySleep,
            Change::DisplayWake,
            Change::SessionLeft,
            Change::SessionEntered,
            Change::ProtectedDataLost,
            Change::ProtectedDataReturned,
        ] {
            let signal = Arc::new(Signal::new());
            let delivery = Delivery::new(signal.clone());
            delivery.notify(change);
            assert_eq!(signal.error(), Some(change.error()));
            delivery.notify(Change::ProtectedDataReturned);
            assert_eq!(signal.error(), Some(change.error()));
            assert!(signal.stop.load(Ordering::Acquire));
            assert_eq!(delivery.generation.load(Ordering::Acquire), 2);
        }
    }
    #[test]
    // Verify that notification revocation overrides graceful expiry
    fn notification_revocation_overrides_graceful_expiry() {
        let signal = Arc::new(Signal::new());
        signal.fail(CaptureError::SessionExpired);
        Delivery::new(signal.clone()).notify(Change::ProtectedDataLost);
        assert_eq!(signal.error(), Some(CaptureError::ProtectedInput));
    }
    #[test]
    // Verify that teardown waits for admitted delivery and rejects late callbacks
    fn teardown_waits_for_admitted_delivery_and_rejects_late_callbacks() {
        let signal = Arc::new(Signal::new());
        let delivery = Delivery::new(signal.clone());
        let admitted = delivery.enter().unwrap();
        delivery.close();
        assert!(!delivery.drained());
        assert!(delivery.enter().is_none());
        drop(admitted);
        assert!(delivery.drained());
        delivery.notify(Change::Sleep);
        assert_eq!(signal.error(), None);
    }
    #[test]
    // Verify that stalled notification cannot return a successful expired draft
    fn stalled_notification_cannot_return_a_successful_expired_draft() {
        let signal = Arc::new(Signal::new());
        signal.fail(CaptureError::SessionExpired);
        let delivery = Arc::new(Delivery::new(signal.clone()));
        let admitted = delivery.enter().unwrap();
        // No native subscriptions: this exercises only the teardown fence
        let mut watch = SessionWatch {
            delivery: delivery.clone(),
            subscriptions: Vec::new(),
        };
        watch.stop();
        assert_eq!(signal.error(), Some(CaptureError::WorkerUnresponsive));
        drop(admitted);
        watch.stop();
        assert!(delivery.drained());
    }
}
