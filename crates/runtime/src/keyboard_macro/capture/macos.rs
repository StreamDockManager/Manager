//! Passive listen-only CGEvent taps. No Accessibility/Input Monitoring request API
//! is called. Public CoreGraphics APIs do not expose an authoritative screen-lock
//! state. NSWorkspace session/screen-sleep notifications do not close that gap
//! Lock handling is best effort: a local probe observed protected-data revocation
//! about 10 seconds after graphical lock. Users must stop macros before locking;
//! healthy observations do not establish unlock. Native device testing is separate
//!
//! ABI provenance: the installed Apple SDK's CoreGraphics/CGEvent.h (listen-only
//! taps and permission preflight), CGEventTypes.h (field/event values),
//! CGEventSource.h (HID-system state), CoreGraphics/CGSession.h (no lock key), and
//! HIToolbox/CarbonEventsCore.h (IsSecureEventInputEnabled). No private API or
//! additional dependency is used. Compilation is not native qualification
//!
//! Apple DTS explicitly confirmed in October 2026 that commonly cited lock/
//! unlock notifications are implementation details, and protected-data availability
//! is not guaranteed to follow every graphical screen lock. Neither NSWorkspace
//! session state nor a Keychain probe is proof of graphical unlock:
//! https://developer.apple.com/forums/thread/850327
pub(super) mod session;

use super::{
    mappings::mac_key, CaptureError, CaptureHandle, CaptureReadiness, Purpose, Sink,
    SYNTHETIC_EVENT_MARKER,
};
use std::{
    ffi::c_void,
    ptr,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

type Ref = *mut c_void;
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGEventTapCreate(
        location: u32,
        placement: u32,
        options: u32,
        mask: u64,
        callback: unsafe extern "C" fn(Ref, u32, Ref, Ref) -> Ref,
        context: Ref,
    ) -> Ref;
    fn CGEventTapEnable(tap: Ref, enable: bool);
    fn CGEventTapIsEnabled(tap: Ref) -> bool;
    fn CGEventGetIntegerValueField(event: Ref, field: u32) -> i64;
    fn CGEventGetFlags(event: Ref) -> u64;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: Ref;
    fn CFMachPortCreateRunLoopSource(allocator: Ref, port: Ref, order: isize) -> Ref;
    fn CFMachPortInvalidate(port: Ref);
    fn CFRunLoopGetCurrent() -> Ref;
    fn CFRunLoopAddSource(run_loop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRemoveSource(run_loop: Ref, source: Ref, mode: Ref);
    fn CFRunLoopRunInMode(mode: Ref, seconds: f64, return_after_source: bool) -> i32;
    fn CFRelease(value: *const c_void);
}

// Recheck native capture prerequisites while recording is active
fn runtime_check() -> Result<session::Snapshot, CaptureError> {
    if !unsafe { CGPreflightListenEventAccess() } {
        return Err(CaptureError::PermissionDenied);
    }
    let snapshot = session::snapshot()?;
    snapshot.check()?;
    Ok(snapshot)
}
// Reject capture when the current macOS session is no longer admissible
fn session_check() -> Result<(), CaptureError> {
    runtime_check().map(|_| ())
}
// Report readiness for from check
fn readiness_from_check(check: Result<(), CaptureError>) -> CaptureReadiness {
    let reason = match check {
        Ok(()) => None,
        Err(CaptureError::PermissionDenied) => Some("Global recording requires existing Input Monitoring permission for this Manager app bundle. Permission is not requested automatically."),
        Err(CaptureError::ProtectedInput) => Some("Secure Event Input or protected-data restrictions are active. Global capture is unavailable; do not record protected fields."),
        Err(CaptureError::SessionLocked) => Some("The login session is inactive or restricted. Global capture is unavailable."),
        Err(_) => Some("Supported macOS session observations are unavailable or stale. Global capture is unavailable until they can be checked."),
    };
    CaptureReadiness {
        available: reason.is_none(),
        implemented: true,
        reason: reason.map(str::to_owned),
    }
}
// Report whether macOS is ready
pub(super) fn readiness() -> CaptureReadiness {
    // User-accepted best-effort session protection, not a claim of immediate lock detection
    readiness_from_check(session_check())
}

// Start the macOS operation
pub(super) fn start(
    purpose: Purpose,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<CaptureHandle, CaptureError> {
    session_check()?;
    let (sink, mut handle) = Sink::channel_with_stop(purpose, stop);
    let signal = handle.signal.clone();
    handle.thread = Some(
        std::thread::Builder::new()
            .name("keyboard-capture".into())
            .spawn(move || {
                if sink.stopped() {
                    return;
                }
                // Never unwind through a native callback; callbacks below contain only
                // bounded, non-panicking operations. Guard all worker-side setup/teardown
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    objc2::rc::autoreleasepool(|_| run(sink));
                }));
                if result.is_err() {
                    signal.fail_cleanup(CaptureError::WorkerFailed);
                }
                signal
                    .completed
                    .store(true, std::sync::atomic::Ordering::Release);
            })
            .map_err(|_| CaptureError::WorkerFailed)?,
    );
    Ok(handle)
}

struct Tap {
    tap: Ref,
    source: Ref,
    run_loop: Ref,
}
impl Drop for Tap {
    // Release this macOS owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            CGEventTapEnable(self.tap, false);
            CFMachPortInvalidate(self.tap);
            CFRunLoopRemoveSource(self.run_loop, self.source, kCFRunLoopDefaultMode);
            CFRelease(self.source);
            CFRelease(self.tap);
        }
    }
}
// Runloop results: finished=1, stopped=2, timed out=3, handled source=4.
// Never acknowledge while an unbounded stream of callbacks is still pending
fn drain_pending(mut pump: impl FnMut() -> i32) -> Result<(), CaptureError> {
    for _ in 0..32 {
        match pump() {
            3 => return Ok(()),
            4 => {}
            _ => return Err(CaptureError::HookDisabled),
        }
    }
    Err(CaptureError::WorkerUnresponsive)
}
// Deliver a macOS wake signal
fn pulse(signal: &super::Signal, request: u64) {
    let now = signal.elapsed_ms();
    signal.worker_heartbeat.store(now, Ordering::Release);
    // This backend's emergency chord is recognized by the same session tap
    signal.hotkey_heartbeat.store(now, Ordering::Release);
    signal.verified.store(request, Ordering::Release);
}
struct Context {
    sink: Sink,
    snapshot: session::Snapshot,
}
// Drive the macOS operation
fn run(sink: Sink) {
    let snapshot = match runtime_check() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            sink.signal.fail(error);
            return;
        }
    };
    let mut context = Context { sink, snapshot };
    // Scoped to this transaction, including countdown; no observers are installed
    // by readiness or at app startup. All notifications revoke, never grant
    let mut session = session::SessionWatch::start(context.sink.signal.clone());
    if context.sink.purpose == Purpose::Recording {
        let countdown = Instant::now();
        while countdown.elapsed() < Duration::from_secs(3) {
            if context.sink.stopped() {
                return;
            }
            match runtime_check() {
                Ok(snapshot) => context.snapshot = snapshot,
                Err(error) => {
                    context.sink.signal.fail(error);
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    match runtime_check() {
        Ok(snapshot) => context.snapshot = snapshot,
        Err(error) => {
            context.sink.signal.fail(error);
            return;
        }
    }
    // HID-system state excludes our private injected source. Admission from a
    // clean state prevents recording the tail of an earlier physical chord
    if (0..=127).any(|key| unsafe { CGEventSourceKeyState(1, key) }) {
        context.sink.signal.fail(CaptureError::InitiallyHeldKeys);
        return;
    }
    context.sink.started = Instant::now();
    let tap = unsafe {
        // kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionListenOnly
        CGEventTapCreate(
            1,
            0,
            1,
            (1 << 10) | (1 << 11) | (1 << 12),
            callback,
            (&mut context as *mut Context).cast(),
        )
    };
    if tap.is_null() {
        context.sink.signal.fail(CaptureError::PermissionDenied);
        return;
    }
    let source = unsafe { CFMachPortCreateRunLoopSource(ptr::null_mut(), tap, 0) };
    if source.is_null() {
        unsafe {
            CFMachPortInvalidate(tap);
            CFRelease(tap);
        }
        context.sink.signal.fail(CaptureError::WorkerFailed);
        return;
    }
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    let guard = Tap {
        tap,
        source,
        run_loop,
    };
    unsafe {
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
        CGEventTapEnable(tap, true);
    }
    // Recheck after hook installation to close the initial-state admission race
    if (0..=127).any(|key| unsafe { CGEventSourceKeyState(1, key) }) {
        context.sink.signal.fail(CaptureError::InitiallyHeldKeys);
    }
    match runtime_check() {
        Ok(snapshot) => context.snapshot = snapshot,
        Err(error) => context.sink.signal.fail(error),
    }
    if !context.sink.stopped() {
        pulse(&context.sink.signal, 0);
        context
            .sink
            .signal
            .strict_health
            .store(true, Ordering::Release);
        context
            .sink
            .signal
            .ready
            .store(true, std::sync::atomic::Ordering::Release);
    }
    while !context.sink.expired() {
        let request = context
            .sink
            .signal
            .verification_requested
            .load(Ordering::Acquire);
        match runtime_check() {
            Ok(snapshot) => context.snapshot = snapshot,
            Err(error) => {
                context.sink.signal.fail(error);
                break;
            }
        }
        if !unsafe { CGEventTapIsEnabled(tap) } {
            context.sink.signal.fail(CaptureError::HookDisabled);
            break;
        }
        let drained = objc2::rc::autoreleasepool(|_| {
            drain_pending(|| unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, true) })
        });
        if let Err(error) = drained {
            context.sink.signal.fail(error);
            break;
        }
        match runtime_check() {
            Ok(snapshot) => context.snapshot = snapshot,
            Err(error) => {
                context.sink.signal.fail(error);
                break;
            }
        }
        if context.sink.stopped() {
            break;
        }
        if !unsafe { CGEventTapIsEnabled(tap) } {
            context.sink.signal.fail(CaptureError::HookDisabled);
            break;
        }
        pulse(&context.sink.signal, request);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Callbacks run only on this thread's run loop. Detach before releasing context.sink
    drop(guard);
    session.stop();
    context.sink.finish();
}
// Route the native platform event to its registered runtime owner
unsafe extern "C" fn callback(_: Ref, event_type: u32, event: Ref, context: Ref) -> Ref {
    if context.is_null() {
        return event;
    }
    let context = &mut *context.cast::<Context>();
    let sink = &mut context.sink;
    if matches!(event_type, 0xffff_fffe | 0xffff_ffff) {
        sink.signal.fail(CaptureError::HookDisabled);
        return event; // Never silently re-enable a disabled security/timeout tap
    }
    if event.is_null() || sink.stopped() {
        return event;
    }
    // Callback stays bounded: never query Carbon/AppKit or wait for the UI here
    if !context.snapshot.admit_callback(&sink.signal) {
        return event;
    }
    let marker = CGEventGetIntegerValueField(event, 42);
    let pid = CGEventGetIntegerValueField(event, 41);
    if marker == SYNTHETIC_EVENT_MARKER
        || marker == i64::from(enigo::EVENT_MARKER)
        || pid == i64::from(std::process::id())
    {
        return event;
    }
    let raw = CGEventGetIntegerValueField(event, 9);
    let Ok(code) = u16::try_from(raw) else {
        sink.signal.fail(CaptureError::UnsupportedKey);
        return event;
    };
    let key = mac_key(code);
    match event_type {
        10 => sink.push(key, true, CGEventGetIntegerValueField(event, 8) != 0),
        11 => sink.push(key, false, false),
        // Decode the queued event's side-specific flags. Sampling live HID state
        // here can see a later release and discard this earlier modifier press
        12 => push_modifier(sink, code, CGEventGetFlags(event)),
        _ => (),
    }
    event // Listen-only: never swallow a key, including the emergency chord
}

// Push modifier
fn push_modifier(sink: &mut Sink, code: u16, flags: u64) {
    let Some(key) = mac_key(code).filter(|key| key.is_modifier()) else {
        sink.signal.fail(CaptureError::UnsupportedKey);
        return;
    };
    match crate::keyboard_macro::macos_modifiers::event_down(code, flags) {
        Some(down) => sink.push(Some(key), down, false),
        None => sink.signal.fail(CaptureError::InputMismatch),
    }
}

#[cfg(test)]
mod health_tests {
    use super::*;
    use crate::keyboard_macro::{KeyboardMacroEventKind::*, PhysicalKey::*};

    #[test]
    // Verify that queued shift digit chord retains modifier edges without live HID queries
    fn queued_shift_digit_chord_retains_modifier_edges_without_live_hid_queries() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        // These queued events may all be delivered after the physical keys have
        // been released. Their stored flags still describe the original chord
        push_modifier(&mut sink, 56, (1 << 17) | 0x0002);
        sink.push(Some(Digit1), true, false);
        sink.push(Some(Digit1), false, false);
        push_modifier(&mut sink, 56, 0);
        let values: Vec<_> = std::iter::from_fn(|| handle.try_recv().unwrap())
            .map(|event| (event.key, event.kind))
            .collect();
        assert_eq!(
            values,
            [
                (ShiftLeft, KeyDown),
                (Digit1, KeyDown),
                (Digit1, KeyUp),
                (ShiftLeft, KeyUp)
            ]
        );
    }

    #[test]
    // Verify that queued dual sided modifiers keep each press and release
    fn queued_dual_sided_modifiers_keep_each_press_and_release() {
        for (left, right) in [(56, 60), (59, 62), (58, 61), (55, 54)] {
            let (mut sink, handle) = Sink::channel(Purpose::Recording);
            let (aggregate, left_mask) =
                crate::keyboard_macro::macos_modifiers::modifier_masks(left).unwrap();
            let (_, right_mask) =
                crate::keyboard_macro::macos_modifiers::modifier_masks(right).unwrap();
            push_modifier(&mut sink, left, aggregate | left_mask);
            push_modifier(&mut sink, right, aggregate | left_mask | right_mask);
            push_modifier(&mut sink, left, aggregate | right_mask);
            push_modifier(&mut sink, right, 0);
            let values: Vec<_> = std::iter::from_fn(|| handle.try_recv().unwrap())
                .map(|event| (event.key, event.kind))
                .collect();
            assert_eq!(
                values,
                [
                    (mac_key(left).unwrap(), KeyDown),
                    (mac_key(right).unwrap(), KeyDown),
                    (mac_key(left).unwrap(), KeyUp),
                    (mac_key(right).unwrap(), KeyUp)
                ]
            );
        }
    }

    #[test]
    // Verify that ambiguous modifier flags fail instead of silently losing the chord
    fn ambiguous_modifier_flags_fail_instead_of_silently_losing_the_chord() {
        let (mut sink, handle) = Sink::channel(Purpose::Recording);
        push_modifier(&mut sink, 56, 1 << 17);
        assert_eq!(handle.terminal_error(), Some(CaptureError::InputMismatch));
    }

    #[test]
    // Verify that capture readiness uses live checks not a qualification gate
    fn capture_readiness_uses_live_checks_not_a_qualification_gate() {
        let ready = readiness_from_check(Ok(()));
        assert!(ready.available && ready.implemented);
        assert!(ready.reason.is_none());
        for error in [
            CaptureError::PermissionDenied,
            CaptureError::ProtectedInput,
            CaptureError::SessionLocked,
            CaptureError::SessionStateUnavailable,
            CaptureError::WorkerUnresponsive,
        ] {
            let denied = readiness_from_check(Err(error));
            assert!(!denied.available && denied.implemented);
            assert!(denied.reason.is_some());
        }
    }

    #[test]
    // Verify that callback barrier requires a drained live runloop
    fn callback_barrier_requires_a_drained_live_runloop() {
        let mut outcomes = [4, 4, 3].into_iter();
        assert_eq!(drain_pending(|| outcomes.next().unwrap()), Ok(()));
        assert_eq!(drain_pending(|| 4), Err(CaptureError::WorkerUnresponsive));
        for result in [0, 1, 2] {
            assert_eq!(drain_pending(|| result), Err(CaptureError::HookDisabled));
        }
    }
    #[test]
    // Verify that verification ack does not consume a newer request
    fn verification_ack_does_not_consume_a_newer_request() {
        let signal = super::super::Signal::new();
        signal.verification_requested.store(2, Ordering::Release);
        pulse(&signal, 1);
        assert_eq!(signal.verified.load(Ordering::Acquire), 1);
        assert_eq!(signal.verification_requested.load(Ordering::Acquire), 2);
    }
}
