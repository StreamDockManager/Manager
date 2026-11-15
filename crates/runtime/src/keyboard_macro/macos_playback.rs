//! CoreGraphics output uses a private event source and explicit marker/repeat flags
//! Live prerequisites fail closed. Lock observations are disclosed best effort;
//! exact repeat receipt still needs native scratch-target verification
use super::*;
use crate::keyboard_macro::capture::NativePlaybackObserver;
use crate::keyboard_macro::macos_modifiers::modifier_masks;
use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use std::{collections::HashSet, ffi::c_void, ptr};
#[path = "macos_focus.rs"]
mod focus;

type Ref = *const c_void;
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state: i32) -> Ref;
    fn CGEventCreateKeyboardEvent(source: Ref, key: u16, down: bool) -> Ref;
    fn CGEventSetIntegerValueField(event: Ref, field: u32, value: i64);
    fn CGEventSetFlags(event: Ref, flags: u64);
    fn CGEventGetFlags(event: *mut c_void) -> u64;
    fn CGEventSetType(event: Ref, event_type: u32);
    fn CGEventPost(tap: u32, event: Ref);
    fn CGPreflightPostEventAccess() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementCopyAttributeValue(element: Ref, attribute: Ref, result: *mut Ref) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFEqual(a: Ref, b: Ref) -> bool;
}
struct OwnedRef(Ref);
impl OwnedRef {
    // Initialize macOS playback state from the supplied inputs
    fn new(value: Ref) -> Result<Self, String> {
        if value.is_null() {
            Err("Keyboard macro native object is unavailable".into())
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for OwnedRef {
    // Release this macOS playback owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            CFRelease(self.0);
        }
    }
}
const TARGET_RESPONSE_TIMEOUT_MS: u64 = 100;
// A fresh request can arrive during an older AX read, which cannot acknowledge
// it. Allow both bounded reads plus run-loop dispatch, never an unbounded wait
const TARGET_VERIFICATION_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(2 * TARGET_RESPONSE_TIMEOUT_MS + 50);
// Convert an Accessibility API status into the playback result
fn ax_result(operation: &str, code: i32) -> Result<(), String> {
    if code == 0 {
        return Ok(());
    }
    let detail = match code {
        -25202 => "the selected accessibility element is no longer valid; select the target again",
        -25204 => "the target did not respond or accessibility messaging failed; let the app finish opening or responding, then retry",
        -25205 => "the selected app does not expose the required window attribute",
        -25207 => "the selected app does not support the required window-change notification",
        -25208 => "the selected app does not implement this accessibility operation",
        -25211 => "Accessibility access is disabled; check Manager's existing permission in System Settings",
        -25212 => "there is no focused application window; select an ordinary target window",
        _ => "macOS could not complete the accessibility operation",
    };
    Err(format!(
        "Keyboard macro {operation} failed (AX error {code}): {detail}"
    ))
}
// Validate the owned accessibility target and bound its messaging timeout
fn bound_target(element: &OwnedRef) -> Result<(), String> {
    if unsafe { CFGetTypeID(element.0) != AXUIElementGetTypeID() } {
        return Err("Keyboard macro target is not an accessibility element".into());
    }
    // Passing the system-wide element would change AX timeouts process-wide
    // Bound only the application/window objects owned by this transaction
    ax_result("setting target response timeout", unsafe {
        AXUIElementSetMessagingTimeout(element.0, TARGET_RESPONSE_TIMEOUT_MS as f32 / 1000.0)
    })
}
// Obtain the live frontmost application with a valid process identifier
fn frontmost_application() -> Result<Retained<NSRunningApplication>, String> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .filter(|application| application.processIdentifier() > 0 && !application.isTerminated())
        .ok_or_else(|| "Keyboard macro has no foreground application to verify".into())
}
// Verify foreground PID
fn verify_foreground_pid(selected: i32, current: i32) -> Result<(), String> {
    if selected > 0 && selected == current {
        Ok(())
    } else {
        Err("Keyboard macro target changed while its window was being selected".into())
    }
}
// Read an owned Accessibility attribute value
fn attribute(element: Ref, name: &str) -> Result<OwnedRef, String> {
    unsafe {
        let attr = OwnedRef::new(crate::feedback::cf_string(name))?;
        let mut value = ptr::null();
        // An unsuccessful copy does not transfer a value to the caller
        ax_result(
            &format!("reading {name}"),
            AXUIElementCopyAttributeValue(element, attr.0, &mut value),
        )?;
        OwnedRef::new(value)
    }
}
struct Target {
    application: OwnedRef,
    window: OwnedRef,
}
impl Target {
    // Return the current macOS playback value
    fn current() -> Result<Self, String> {
        if !crate::automation::title_permission(false) {
            return Err("Keyboard macro Accessibility permission is unavailable".into());
        }
        unsafe {
            let foreground = frontmost_application()?;
            let pid = foreground.processIdentifier();
            let application = OwnedRef::new(AXUIElementCreateApplication(pid))?;
            bound_target(&application)?;
            let window = attribute(application.0, "AXFocusedWindow")?;
            bound_target(&window)?;
            let current = frontmost_application()?;
            verify_foreground_pid(pid, current.processIdentifier())?;
            if foreground.isTerminated() || current != foreground {
                return Err("Keyboard macro target application changed during selection".into());
            }
            Ok(Self {
                application,
                window,
            })
        }
    }
    // Report the matches condition for macOS playback
    fn matches(&self, other: &Self) -> bool {
        unsafe {
            CFEqual(self.application.0, other.application.0)
                && CFEqual(self.window.0, other.window.0)
        }
    }
}
// Explain why the current native input path is or is not ready
fn readiness_reason(
    observer: crate::keyboard_macro::capture::CaptureReadiness,
    accessibility: bool,
    posting: bool,
) -> Option<String> {
    if !observer.available {
        return Some(
            observer
                .reason
                .unwrap_or_else(|| "Keyboard macro input observation is unavailable".into()),
        );
    }
    if !accessibility || !posting {
        return Some("Keyboard macro playback requires existing Accessibility and event-posting permission for this Manager app bundle. Permission is not requested automatically.".into());
    }
    None
}
// Explain why the current operation is unavailable
pub(super) fn unavailable_reason() -> Option<String> {
    readiness_reason(
        crate::keyboard_macro::capture::playback_observer_readiness(),
        crate::automation::title_permission(false),
        unsafe { CGPreflightPostEventAccess() },
    )
}

pub(crate) struct NativeBackend {
    owner: String,
    source: Option<OwnedRef>,
    observer: Option<NativePlaybackObserver>,
    target_observer: Option<super::focus::Observer>,
    held: HashSet<u16>,
}
impl NativeBackend {
    // Initialize macOS playback state from the supplied inputs
    pub(crate) fn new(owner: &str) -> Result<Self, String> {
        Ok(Self {
            owner: owner.into(),
            source: None,
            observer: None,
            target_observer: None,
            held: HashSet::new(),
        })
    }
}
// Combine native modifier flags for the held physical keys
fn modifier_flags(held: &HashSet<u16>) -> u64 {
    held.iter()
        .filter_map(|code| modifier_masks(*code))
        .fold(0, |flags, (family, side)| flags | family | side)
}
// Include device-side modifier identifiers in the native event flags
fn flags_with_key_identifiers(factory_flags: u64, owned_modifiers: u64) -> u64 {
    // Keep CoreGraphics' public keypad/Help/function key identifiers. Do not
    // carry ambient modifier, lock, reserved or device-side state from creation
    const KEY_IDENTIFIERS: u64 = (1 << 21) | (1 << 22) | (1 << 23);
    (factory_flags & KEY_IDENTIFIERS) | owned_modifiers
}
#[derive(Debug, PartialEq, Eq)]
struct KeyEvent {
    event_type: u32,
    down: bool,
    repeat: bool,
    flags: u64,
}
// Update the held-key set and construct the corresponding native key event
fn next_key_event(
    held: &HashSet<u16>,
    code: u16,
    kind: KeyboardMacroEventKind,
) -> (HashSet<u16>, KeyEvent) {
    let down = kind != KeyboardMacroEventKind::KeyUp;
    let mut next = held.clone();
    if down {
        next.insert(code);
    } else {
        next.remove(&code);
    }
    // A physical modifier edge is flagsChanged, not an ordinary keyDown/up
    // Include both the family and device-side flags on every event while held
    // Computing from the owned set keeps the other side held during cleanup
    let event = KeyEvent {
        event_type: if modifier_masks(code).is_some() {
            12
        } else if down {
            10
        } else {
            11
        },
        down,
        repeat: kind == KeyboardMacroEventKind::Repeat,
        flags: modifier_flags(&next),
    };
    (next, event)
}
impl Backend for NativeBackend {
    type Code = u16;
    // Check macOS playback prerequisites before proceeding
    fn preflight(
        &mut self,
        recording: &KeyboardMacroRecording,
        playback: &KeyboardMacroPlayback,
    ) -> Result<(), String> {
        repeat::require_exact(playback.repeat_mode)?;
        if let Some(reason) = playback_unavailable_reason() {
            return Err(reason);
        }
        repeat::preflight(repeat::Mechanism::ExplicitFlag, recording)?;
        let readiness = crate::keyboard_macro::capture::playback_observer_readiness();
        if !readiness.available {
            return Err(readiness
                .reason
                .unwrap_or_else(|| "Keyboard macro observer is unavailable".into()));
        }
        if !crate::automation::title_permission(false) || !unsafe { CGPreflightPostEventAccess() } {
            return Err("Keyboard macro Accessibility permission is unavailable".into());
        }
        Ok(())
    }
    // Resolve the supplied macOS playback request
    fn resolve(&mut self, key: PhysicalKey) -> Result<u16, String> {
        if matches!(
            key,
            PhysicalKey::CapsLock | PhysicalKey::NumLock | PhysicalKey::ScrollLock
        ) {
            return Err(
                "Lock-key state preservation is not qualified for keyboard playback".into(),
            );
        }
        macos_keycode(key).ok_or_else(|| "Unsupported physical key mapping".into())
    }
    // Begin tracking the macOS playback operation
    fn begin(&mut self, policy: KeyboardMacroFocusPolicy) -> Result<(), String> {
        if let Some(reason) = playback_unavailable_reason() {
            return Err(reason);
        }
        self.observer = Some(NativePlaybackObserver::start().map_err(|error| error.to_string())?);
        if policy == KeyboardMacroFocusPolicy::StayOnTarget {
            self.target_observer = Some(focus::start()?);
        }
        self.source = Some(OwnedRef::new(unsafe { CGEventSourceCreate(-1) })?);
        self.check()
    }
    // Check the current macOS playback conditions
    fn check(&mut self) -> Result<(), String> {
        self.observer
            .as_ref()
            .ok_or("Keyboard macro observer is unavailable")?
            .check()
            .map_err(|error| error.to_string())?;
        if !crate::automation::title_permission(false) || !unsafe { CGPreflightPostEventAccess() } {
            return Err("Keyboard macro permission was revoked".into());
        }
        if let Some(observer) = &self.target_observer {
            observer.check_fresh_with_timeout(TARGET_VERIFICATION_TIMEOUT)?;
        }
        Ok(())
    }
    // Finalize the pending macOS playback operation
    fn finish(&mut self) -> Result<(), String> {
        let capture = self.observer.as_mut().map_or(Ok(()), |observer| {
            observer.stop().map_err(|error| error.to_string())
        });
        self.observer = None;
        let target = self
            .target_observer
            .as_mut()
            .map_or(Ok(()), |observer| observer.stop());
        self.target_observer = None;
        capture.and(target)
    }
    // Deliver the macOS playback event to its consumer
    fn emit(&mut self, code: &u16, kind: KeyboardMacroEventKind) -> Result<(), String> {
        let (held, description) = next_key_event(&self.held, *code, kind);
        let source = self
            .source
            .as_ref()
            .ok_or("Keyboard macro event source is unavailable")?;
        if kind == KeyboardMacroEventKind::KeyUp {
            let key =
                physical_from_macos(*code).ok_or("Keyboard cleanup mapping is unavailable")?;
            self.observer
                .as_ref()
                .ok_or("Keyboard cleanup observer is unavailable")?
                .prepare_release(key)?;
            if !unsafe { CGPreflightPostEventAccess() } {
                return Err(
                    "Keyboard cleanup permission was revoked; ownership is retained".into(),
                );
            }
        }
        crate::builtins::native::macro_key_event(&self.owner, *code, kind, || {
            if kind == KeyboardMacroEventKind::KeyUp {
                self.observer
                    .as_ref()
                    .ok_or("Keyboard cleanup observer is unavailable")?
                    .release_ready(
                        physical_from_macos(*code)
                            .ok_or("Keyboard cleanup mapping is unavailable")?,
                    )?;
            }
            unsafe {
                let event = OwnedRef::new(CGEventCreateKeyboardEvent(
                    source.0,
                    *code,
                    description.down,
                ))?;
                CGEventSetType(event.0, description.event_type);
                CGEventSetIntegerValueField(
                    event.0,
                    42,
                    crate::keyboard_macro::capture::SYNTHETIC_EVENT_MARKER,
                );
                CGEventSetIntegerValueField(event.0, 8, i64::from(description.repeat));
                CGEventSetFlags(
                    event.0,
                    flags_with_key_identifiers(
                        CGEventGetFlags(event.0.cast_mut()),
                        description.flags,
                    ),
                );
                CGEventPost(0, event.0);
            }
            Ok(())
        })?;
        self.held = held;
        Ok(())
    }
}

/// Diagnostic only: no NativeBackend, CGEvent source/tap, posting, or readiness override
#[cfg(feature = "native-session-probe")]
// Run target probe
pub(super) fn run_target_probe(duration: std::time::Duration) -> Result<(), String> {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *mut c_void;
        fn CFRunLoopRunInMode(mode: *mut c_void, seconds: f64, return_after_source: bool) -> i32;
    }
    let main = objc2::MainThreadMarker::new().ok_or("Run the probe on its main thread")?;
    if duration.is_zero() || duration > Duration::from_secs(600) {
        return Err("Probe duration must be between 1 and 600 seconds".into());
    }
    // No request API: a shell-launched example may have a different TCC identity
    // from Manager. Existing Manager access need not grant this executable access
    if !crate::automation::title_permission(false) {
        return Err("The AX target probe lacks existing Accessibility permission for its launching executable/app. No permission was requested or changed.".into());
    }
    let app = NSApplication::sharedApplication(main);
    app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    app.finishLaunching();
    eprintln!("Select the intended ordinary target window within 3 seconds. The probe will never activate or retarget it; native readiness remains false.");
    let selection = Instant::now();
    let pump = || {
        objc2::rc::autoreleasepool(|_| unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.02, false);
        })
    };
    while selection.elapsed() < Duration::from_secs(3) {
        pump();
    }
    let worker = std::thread::Builder::new().name("ax-target-probe".into()).spawn(move || {
        objc2::rc::autoreleasepool(|_| {
            let mut observer = focus::start()?;
            let started = Instant::now();
            let mut revoked = false;
            while started.elapsed() < duration {
                let healthy = observer.check_fresh_with_timeout(TARGET_VERIFICATION_TIMEOUT).is_ok();
                revoked |= !healthy;
                println!("{}", serde_json::json!({
                    "elapsed_ms": started.elapsed().as_millis(),
                    "unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|time| time.as_millis()),
                    "target_pinned": true,
                    "target_observations_healthy": healthy,
                    "revocation_generation": observer.state.diagnostic_generation(),
                    "revoked": revoked,
                    "native_ready": false,
                }));
                std::thread::sleep(Duration::from_millis(100));
            }
            observer.stop()
        })
    }).map_err(|_| "AX target probe worker could not start")?;
    // Keep AppKit notifications dispatching. Never join a live worker from the UI
    while !worker.is_finished() {
        pump();
    }
    worker.join().map_err(|_| "AX target probe worker failed")?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that target errors preserve operation and native status
    fn target_errors_preserve_operation_and_native_status() {
        assert!(ax_result("reading AXFocusedWindow", 0).is_ok());
        let timeout = ax_result("reading AXFocusedWindow", -25204).unwrap_err();
        assert!(timeout.contains("AXFocusedWindow"));
        assert!(timeout.contains("-25204"));
        assert!(timeout.contains("retry"));
        let notification = ax_result("watching AXUIElementDestroyed", -25207).unwrap_err();
        assert!(notification.contains("AXUIElementDestroyed"));
        assert!(notification.contains("-25207"));
        assert!(!notification.contains("permission"));
    }
    #[test]
    // Verify that foreground switch during window lookup is rejected
    fn foreground_switch_during_window_lookup_is_rejected() {
        assert!(verify_foreground_pid(10, 10).is_ok());
        assert!(verify_foreground_pid(10, 11).is_err());
        assert!(verify_foreground_pid(0, 0).is_err());
    }
    #[test]
    // Verify that playback admission uses live prerequisites not a qualification switch
    fn playback_admission_uses_live_prerequisites_not_a_qualification_switch() {
        let ready = || crate::keyboard_macro::capture::CaptureReadiness {
            available: true,
            implemented: true,
            reason: None,
        };
        assert!(readiness_reason(ready(), true, true).is_none());
        assert!(readiness_reason(ready(), false, true).is_some());
        assert!(readiness_reason(ready(), true, false).is_some());
        let mut denied = ready();
        denied.available = false;
        denied.reason = Some("unknown or protected session".into());
        assert_eq!(
            readiness_reason(denied, true, true).as_deref(),
            Some("unknown or protected session")
        );
    }
    #[test]
    // Verify that releasing one modifier side preserves the other side
    fn releasing_one_modifier_side_preserves_the_other_side() {
        let mut held = HashSet::from([56, 60, 59, 62, 58, 61, 54, 55]);
        assert_eq!(
            modifier_flags(&held),
            (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | 0x207f
        );
        for code in [56, 59, 58, 54] {
            held.remove(&code);
        }
        assert_eq!(
            modifier_flags(&held),
            (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20) | 0x204c
        );
        held.clear();
        assert_eq!(modifier_flags(&held), 0);
    }
    #[test]
    // Verify that shift digit chord keeps modifier flags through repeat and key release
    fn shift_digit_chord_keeps_modifier_flags_through_repeat_and_key_release() {
        use KeyboardMacroEventKind::*;
        for (shift, flags) in [(56, 0x20002), (60, 0x20004)] {
            let (held, event) = next_key_event(&HashSet::new(), shift, KeyDown);
            assert_eq!(event.event_type, 12);
            assert_eq!(event.flags, flags);
            let (held, event) = next_key_event(&held, 18, KeyDown); // physical Digit1
            assert_eq!(event.event_type, 10);
            assert_eq!(event.flags, flags);
            assert!(!event.repeat);
            let (held, event) = next_key_event(&held, 18, Repeat);
            assert_eq!(event.flags, flags);
            assert_eq!(event.event_type, 10);
            assert!(event.repeat);
            let (held, event) = next_key_event(&held, 18, KeyUp);
            assert_eq!(event.event_type, 11);
            assert_eq!(event.flags, flags);
            let (held, event) = next_key_event(&held, shift, KeyUp);
            assert_eq!(event.event_type, 12);
            assert_eq!(event.flags, 0);
            assert!(held.is_empty());
        }
    }
    #[test]
    // Verify that key identifiers survive without inheriting unowned modifier or lock flags
    fn key_identifiers_survive_without_inheriting_unowned_modifier_or_lock_flags() {
        for identifiers in [0, 1 << 21, 1 << 22, 1 << 23, (1 << 21) | (1 << 23)] {
            let factory_flags = identifiers | 0x1f207f | (1 << 30);
            assert_eq!(flags_with_key_identifiers(factory_flags, 0), identifiers);
            assert_eq!(
                flags_with_key_identifiers(factory_flags, 0x20002),
                identifiers | 0x20002
            );
        }
    }
    #[test]
    // Verify that common control keys emit regular edges and keep held modifiers
    fn common_control_keys_emit_regular_edges_and_keep_held_modifiers() {
        use KeyboardMacroEventKind::*;
        use PhysicalKey::*;
        for key in [
            Enter,
            NumpadEnter,
            Tab,
            Escape,
            Space,
            Backspace,
            Delete,
            ArrowLeft,
            ArrowRight,
            ArrowDown,
            ArrowUp,
            Home,
            End,
            PageUp,
            PageDown,
            F1,
            F2,
            F3,
            F4,
            F5,
            F6,
            F7,
            F8,
            F9,
            F10,
            F11,
            F12,
            F13,
            F14,
            F15,
            F16,
            F17,
            F18,
            F19,
            F20,
        ] {
            let code = macos_keycode(key).unwrap();
            for held in [HashSet::new(), HashSet::from([56, 55])] {
                let flags = modifier_flags(&held);
                let (pressed, down) = next_key_event(&held, code, KeyDown);
                assert_eq!(down.event_type, 10);
                assert!(down.down && !down.repeat);
                assert_eq!(down.flags, flags);
                assert!(pressed.contains(&code));
                let (repeated, repeat) = next_key_event(&pressed, code, Repeat);
                assert_eq!(repeat.event_type, 10);
                assert!(repeat.down && repeat.repeat);
                assert_eq!(repeat.flags, flags);
                assert_eq!(repeated, pressed);
                let (released, up) = next_key_event(&pressed, code, KeyUp);
                assert_eq!(up.event_type, 11);
                assert!(!up.down && !up.repeat);
                assert_eq!(up.flags, flags);
                assert_eq!(released, held);
            }
        }
    }
    #[test]
    // Verify that every modifier combination survives nonmodifier edges and cleanup
    fn every_modifier_combination_survives_nonmodifier_edges_and_cleanup() {
        use KeyboardMacroEventKind::*;
        let modifiers = [
            (56, 0x20002),
            (60, 0x20004),
            (59, 0x40001),
            (62, 0x42000),
            (58, 0x80020),
            (61, 0x80040),
            (55, 0x100008),
            (54, 0x100010),
        ];
        for combination in 0..256 {
            let selected: Vec<_> = modifiers
                .iter()
                .enumerate()
                .filter(|(index, _)| combination & (1 << index) != 0)
                .map(|(_, value)| *value)
                .collect();
            let mut held = HashSet::new();
            let mut flags = 0;
            for (code, mask) in &selected {
                let (next, event) = next_key_event(&held, *code, KeyDown);
                flags |= mask;
                assert_eq!(event.flags, flags);
                assert_eq!(event.event_type, 12);
                held = next;
            }
            // Letter, digit, punctuation, keypad, function and navigation chords
            for code in [0, 18, 24, 44, 83, 122, 123] {
                let (next, event) = next_key_event(&held, code, KeyDown);
                assert_eq!(event.flags, flags);
                assert_eq!(event.event_type, 10);
                let (next, event) = next_key_event(&next, code, KeyUp);
                assert_eq!(event.flags, flags);
                assert_eq!(event.event_type, 11);
                assert_eq!(held, next);
            }
            for (index, (code, _)) in selected.iter().enumerate() {
                let (next, event) = next_key_event(&held, *code, KeyUp);
                let remaining = selected[index + 1..]
                    .iter()
                    .fold(0, |flags, (_, mask)| flags | mask);
                assert_eq!(event.flags, remaining);
                assert_eq!(event.event_type, 12);
                held = next;
            }
            assert!(held.is_empty());
        }
    }
}
