//! Explicit physical mappings and native safety gates. No layout-based fallback,
//! clipboard fallback, permission prompt, or elevation is allowed during playback
use super::{playback::Backend, types::*};
#[cfg(any(target_os = "macos", target_os = "windows", test))]
#[path = "focus.rs"]
pub(crate) mod focus;
#[path = "linux_mapping.rs"]
pub mod linux_mapping;
#[cfg(any(target_os = "linux", test))]
#[path = "linux_session.rs"]
pub(crate) mod linux_session;
#[path = "repeat.rs"]
pub(crate) mod repeat;
#[cfg(any(target_os = "linux", test))]
#[path = "wayland_limits.rs"]
pub(crate) mod wayland_limits;
#[cfg(target_os = "linux")]
#[path = "x11_connection.rs"]
pub(crate) mod x11_connection;

/// Creation preference only. Deserialization and existing drafts retain Exact or
/// their explicitly saved choice; native capability admission is independent
pub fn new_action_repeat_mode() -> KeyboardMacroRepeatMode {
    #[cfg(target_os = "linux")]
    {
        let x11_display = std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty());
        repeat_mode_for_new_action(true, crate::builtins::native::wayland(), x11_display)
    }
    #[cfg(not(target_os = "linux"))]
    {
        KeyboardMacroRepeatMode::Exact
    }
}
#[cfg(any(target_os = "linux", test))]
// Choose the repeat mode for a newly added action
fn repeat_mode_for_new_action(
    linux: bool,
    wayland: bool,
    x11_display: bool,
) -> KeyboardMacroRepeatMode {
    if linux && !wayland && x11_display {
        KeyboardMacroRepeatMode::System
    } else {
        KeyboardMacroRepeatMode::Exact
    }
}

/// Wayland clients generate repeat independently of recorded physical edges
/// This requirement is explicit; never silently rewrite persisted Exact mode
pub fn required_repeat_mode() -> Option<KeyboardMacroRepeatMode> {
    #[cfg(target_os = "linux")]
    if crate::builtins::native::wayland()
        && std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
    {
        return Some(KeyboardMacroRepeatMode::System);
    }
    None
}

// Explain why native playback is unavailable on this platform
pub fn playback_unavailable_reason() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos::unavailable_reason()
    }
    #[cfg(target_os = "windows")]
    {
        let readiness = super::capture::playback_observer_readiness();
        if readiness.available {
            None
        } else {
            Some(
                readiness
                    .reason
                    .unwrap_or_else(|| "Windows keyboard input observation is unavailable".into()),
            )
        }
    }
    #[cfg(target_os = "linux")]
    {
        if crate::builtins::native::wayland() {
            if super::hyprland::selected() {
                return super::hyprland::readiness().err().or_else(|| {
                    (!crate::wayland::authorized())
                        .then(|| "Authorize Wayland keyboard output separately in Settings".into())
                });
            }
            return Some(wayland_limits::playback_unavailable_reason().into());
        }
        let readiness = super::capture::playback_observer_readiness();
        if readiness.available {
            None
        } else {
            Some(readiness.reason.unwrap_or_else(|| {
                "X11 keyboard input observation is unavailable. Use simulation.".into()
            }))
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        Some("Safe keyboard macro playback is not implemented on this platform. In-app recording, editing and simulation remain available.".into())
    }
}

/// Known platform limitation, separate from capability admission or permissions
pub fn native_warning(platform: KeyboardMacroPlatform) -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    if platform == KeyboardMacroPlatform::Linux && super::hyprland::selected() {
        return Some("Hyprland records physical edges and holds, not application-generated repeat counts. Select System repeat explicitly; the focused client controls repeat. Lock/focus callbacks and bounded live checks stop observed protection changes, but brief session, display or grab transitions can be missed and checks are not atomic with portal output. Stop macros before locking or switching sessions.");
    }
    match platform {
        KeyboardMacroPlatform::Macos => Some("macOS lock detection is best effort and can lag by about 10 seconds. Stop recording or playback before locking or switching users; do not trigger macros while locked. Secure-input and other checks are not a guarantee that a target is safe."),
        _ => None,
    }
}

/// Apple Carbon virtual-key positions, not characters or layout-derived values
/// None means the key has no supported physical counterpart on this backend
pub fn macos_keycode(key: PhysicalKey) -> Option<u16> {
    use PhysicalKey::*;
    Some(match key {
        KeyA => 0,
        KeyS => 1,
        KeyD => 2,
        KeyF => 3,
        KeyH => 4,
        KeyG => 5,
        KeyZ => 6,
        KeyX => 7,
        KeyC => 8,
        KeyV => 9,
        IntlBackslash => 10,
        KeyB => 11,
        KeyQ => 12,
        KeyW => 13,
        KeyE => 14,
        KeyR => 15,
        KeyY => 16,
        KeyT => 17,
        Digit1 => 18,
        Digit2 => 19,
        Digit3 => 20,
        Digit4 => 21,
        Digit6 => 22,
        Digit5 => 23,
        Equal => 24,
        Digit9 => 25,
        Digit7 => 26,
        Minus => 27,
        Digit8 => 28,
        Digit0 => 29,
        BracketRight => 30,
        KeyO => 31,
        KeyU => 32,
        BracketLeft => 33,
        KeyI => 34,
        KeyP => 35,
        Enter => 36,
        KeyL => 37,
        KeyJ => 38,
        Quote => 39,
        KeyK => 40,
        Semicolon => 41,
        Backslash => 42,
        Comma => 43,
        Slash => 44,
        KeyN => 45,
        KeyM => 46,
        Period => 47,
        Tab => 48,
        Space => 49,
        Backquote => 50,
        Backspace => 51,
        Escape => 53,
        MetaRight => 54,
        MetaLeft => 55,
        ShiftLeft => 56,
        CapsLock => 57,
        AltLeft => 58,
        ControlLeft => 59,
        ShiftRight => 60,
        AltRight => 61,
        ControlRight => 62,
        F17 => 64,
        NumpadDecimal => 65,
        NumpadMultiply => 67,
        NumpadAdd => 69,
        NumpadDivide => 75,
        NumpadEnter => 76,
        NumpadSubtract => 78,
        F18 => 79,
        F19 => 80,
        NumpadEqual => 81,
        Numpad0 => 82,
        Numpad1 => 83,
        Numpad2 => 84,
        Numpad3 => 85,
        Numpad4 => 86,
        Numpad5 => 87,
        Numpad6 => 88,
        Numpad7 => 89,
        F20 => 90,
        Numpad8 => 91,
        Numpad9 => 92,
        IntlYen => 93,
        IntlRo => 94,
        NumpadComma => 95,
        F5 => 96,
        F6 => 97,
        F7 => 98,
        F3 => 99,
        F8 => 100,
        F9 => 101,
        F11 => 103,
        F13 => 105,
        F16 => 106,
        F14 => 107,
        F10 => 109,
        F12 => 111,
        F15 => 113,
        Home => 115,
        PageUp => 116,
        Delete => 117,
        F4 => 118,
        End => 119,
        F2 => 120,
        PageDown => 121,
        F1 => 122,
        ArrowLeft => 123,
        ArrowRight => 124,
        ArrowDown => 125,
        ArrowUp => 126,
        Insert | PrintScreen | ScrollLock | Pause | NumLock | F21 | F22 | F23 | F24 => return None,
    })
}
// Map a macOS virtual key code to the supported physical key
pub fn physical_from_macos(code: u16) -> Option<PhysicalKey> {
    PhysicalKey::ALL
        .iter()
        .copied()
        .find(|key| macos_keycode(*key) == Some(code))
}

/// Windows scan-code set 1 plus the E0 prefix. E1/Pause is deliberately unsupported
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowsScanCode {
    pub code: u16,
    pub extended: bool,
}
// Map the physical key to its Windows scan code and extension flag
pub fn windows_scan_code(key: PhysicalKey) -> Option<WindowsScanCode> {
    use PhysicalKey::*;
    let (code, extended) = match key {
        KeyA => (0x1e, false),
        KeyB => (0x30, false),
        KeyC => (0x2e, false),
        KeyD => (0x20, false),
        KeyE => (0x12, false),
        KeyF => (0x21, false),
        KeyG => (0x22, false),
        KeyH => (0x23, false),
        KeyI => (0x17, false),
        KeyJ => (0x24, false),
        KeyK => (0x25, false),
        KeyL => (0x26, false),
        KeyM => (0x32, false),
        KeyN => (0x31, false),
        KeyO => (0x18, false),
        KeyP => (0x19, false),
        KeyQ => (0x10, false),
        KeyR => (0x13, false),
        KeyS => (0x1f, false),
        KeyT => (0x14, false),
        KeyU => (0x16, false),
        KeyV => (0x2f, false),
        KeyW => (0x11, false),
        KeyX => (0x2d, false),
        KeyY => (0x15, false),
        KeyZ => (0x2c, false),
        Digit1 => (0x02, false),
        Digit2 => (0x03, false),
        Digit3 => (0x04, false),
        Digit4 => (0x05, false),
        Digit5 => (0x06, false),
        Digit6 => (0x07, false),
        Digit7 => (0x08, false),
        Digit8 => (0x09, false),
        Digit9 => (0x0a, false),
        Digit0 => (0x0b, false),
        Escape => (0x01, false),
        Minus => (0x0c, false),
        Equal => (0x0d, false),
        Backspace => (0x0e, false),
        Tab => (0x0f, false),
        BracketLeft => (0x1a, false),
        BracketRight => (0x1b, false),
        Enter => (0x1c, false),
        ControlLeft => (0x1d, false),
        Semicolon => (0x27, false),
        Quote => (0x28, false),
        Backquote => (0x29, false),
        ShiftLeft => (0x2a, false),
        Backslash => (0x2b, false),
        Comma => (0x33, false),
        Period => (0x34, false),
        Slash => (0x35, false),
        ShiftRight => (0x36, false),
        AltLeft => (0x38, false),
        Space => (0x39, false),
        CapsLock => (0x3a, false),
        F1 => (0x3b, false),
        F2 => (0x3c, false),
        F3 => (0x3d, false),
        F4 => (0x3e, false),
        F5 => (0x3f, false),
        F6 => (0x40, false),
        F7 => (0x41, false),
        F8 => (0x42, false),
        F9 => (0x43, false),
        F10 => (0x44, false),
        F11 => (0x57, false),
        F12 => (0x58, false),
        F13 => (0x64, false),
        F14 => (0x65, false),
        F15 => (0x66, false),
        F16 => (0x67, false),
        F17 => (0x68, false),
        F18 => (0x69, false),
        F19 => (0x6a, false),
        F20 => (0x6b, false),
        F21 => (0x6c, false),
        F22 => (0x6d, false),
        F23 => (0x6e, false),
        F24 => (0x76, false),
        NumLock => (0x45, false),
        ScrollLock => (0x46, false),
        Numpad7 => (0x47, false),
        Numpad8 => (0x48, false),
        Numpad9 => (0x49, false),
        NumpadSubtract => (0x4a, false),
        Numpad4 => (0x4b, false),
        Numpad5 => (0x4c, false),
        Numpad6 => (0x4d, false),
        NumpadAdd => (0x4e, false),
        Numpad1 => (0x4f, false),
        Numpad2 => (0x50, false),
        Numpad3 => (0x51, false),
        Numpad0 => (0x52, false),
        NumpadDecimal => (0x53, false),
        NumpadMultiply => (0x37, false),
        NumpadEqual => (0x59, false),
        NumpadComma => (0x7e, false),
        IntlBackslash => (0x56, false),
        IntlRo => (0x73, false),
        IntlYen => (0x7d, false),
        NumpadEnter => (0x1c, true),
        ControlRight => (0x1d, true),
        NumpadDivide => (0x35, true),
        PrintScreen => (0x37, true),
        AltRight => (0x38, true),
        Home => (0x47, true),
        ArrowUp => (0x48, true),
        PageUp => (0x49, true),
        ArrowLeft => (0x4b, true),
        ArrowRight => (0x4d, true),
        End => (0x4f, true),
        ArrowDown => (0x50, true),
        PageDown => (0x51, true),
        Insert => (0x52, true),
        Delete => (0x53, true),
        MetaLeft => (0x5b, true),
        MetaRight => (0x5c, true),
        Pause => return None,
    };
    Some(WindowsScanCode { code, extended })
}
// Map a Windows scan code and extension flag to a physical key
pub fn physical_from_windows(code: u16, extended: bool) -> Option<PhysicalKey> {
    let expected = WindowsScanCode { code, extended };
    PhysicalKey::ALL
        .iter()
        .copied()
        .find(|key| windows_scan_code(*key) == Some(expected))
}

#[cfg(target_os = "macos")]
#[path = "macos_playback.rs"]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::NativeBackend;

#[cfg(target_os = "windows")]
#[path = "windows_playback.rs"]
mod windows;
#[cfg(target_os = "windows")]
pub(crate) use windows::NativeBackend;

#[cfg(target_os = "linux")]
#[path = "linux_playback.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::NativeBackend;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub(crate) struct NativeBackend;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl NativeBackend {
    // Initialize platform state from the supplied inputs
    pub(crate) fn new(_: &str) -> Result<Self, String> {
        Err(playback_unavailable_reason().unwrap())
    }
}
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl Backend for NativeBackend {
    type Code = u16;
    // Check platform prerequisites before proceeding
    fn preflight(
        &mut self,
        _: &KeyboardMacroRecording,
        _: &KeyboardMacroPlayback,
    ) -> Result<(), String> {
        Err(playback_unavailable_reason().unwrap())
    }
    // Resolve the supplied platform request
    fn resolve(&mut self, _: PhysicalKey) -> Result<u16, String> {
        Err("Unsupported physical key mapping".into())
    }
    // Begin tracking the platform operation
    fn begin(&mut self, _: KeyboardMacroFocusPolicy) -> Result<(), String> {
        Err(playback_unavailable_reason().unwrap())
    }
    // Check the current platform conditions
    fn check(&mut self) -> Result<(), String> {
        Err(playback_unavailable_reason().unwrap())
    }
    // Deliver the platform event to its consumer
    fn emit(&mut self, _: &u16, _: KeyboardMacroEventKind) -> Result<(), String> {
        Err(playback_unavailable_reason().unwrap())
    }
}

/// Explicit read-only candidate qualification; never starts keyboard observation
/// or output and cannot change native readiness
#[cfg(all(target_os = "linux", feature = "native-session-probe"))]
// Run Linux kde session probe
pub fn run_linux_kde_session_probe(
    profile: &str,
    duration: std::time::Duration,
) -> Result<(), String> {
    linux_session::kde::run_probe(profile, duration)
}

/// Read-only target-lifetime diagnostic. Does not enable native macro playback
#[cfg(all(target_os = "macos", feature = "native-session-probe"))]
// Run macOS target probe
pub fn run_macos_target_probe(duration: std::time::Duration) -> Result<(), String> {
    macos::run_target_probe(duration)
}

#[cfg(test)]
mod tests {
    #[test]
    // Verify that new action system repeat is only for Linux X11
    fn new_action_system_repeat_is_only_for_linux_x11() {
        use super::{repeat_mode_for_new_action, KeyboardMacroRepeatMode};
        assert_eq!(
            repeat_mode_for_new_action(true, false, true),
            KeyboardMacroRepeatMode::System
        );
        for (linux, wayland, display) in [
            (false, false, true),
            (true, true, true),
            (true, true, false),
            (true, false, false),
        ] {
            assert_eq!(
                repeat_mode_for_new_action(linux, wayland, display),
                KeyboardMacroRepeatMode::Exact
            );
        }
    }

    use super::*;
    #[test]
    // Verify that accepted macOS delay is disclosed without affecting other platforms
    fn accepted_macos_delay_is_disclosed_without_affecting_other_platforms() {
        let warning = native_warning(KeyboardMacroPlatform::Macos).unwrap();
        assert!(warning.contains("best effort"));
        assert!(warning.contains("10 seconds"));
        assert!(warning.contains("before locking"));
        assert!(native_warning(KeyboardMacroPlatform::Windows).is_none());
        assert!(native_warning(KeyboardMacroPlatform::Linux).is_none());
    }
    #[test]
    // Verify that macOS positions round trip without aliasing sides or numpad
    fn macos_positions_round_trip_without_aliasing_sides_or_numpad() {
        let mut seen = std::collections::HashSet::new();
        for key in PhysicalKey::ALL {
            if let Some(code) = macos_keycode(*key) {
                assert!(seen.insert(code), "duplicate native physical mapping");
                assert_eq!(physical_from_macos(code), Some(*key));
            }
        }
        assert_ne!(
            macos_keycode(PhysicalKey::ControlLeft),
            macos_keycode(PhysicalKey::ControlRight)
        );
        assert_ne!(
            macos_keycode(PhysicalKey::NumpadEnter),
            macos_keycode(PhysicalKey::Enter)
        );
    }
    #[test]
    // Verify that Windows scan codes preserve extended prefix and round trip
    fn windows_scan_codes_preserve_extended_prefix_and_round_trip() {
        let mut seen = std::collections::HashSet::new();
        for key in PhysicalKey::ALL {
            if let Some(scan) = windows_scan_code(*key) {
                assert!(seen.insert(scan), "duplicate Windows physical mapping");
                assert_eq!(physical_from_windows(scan.code, scan.extended), Some(*key));
            }
        }
        assert!(
            windows_scan_code(PhysicalKey::ControlRight)
                .unwrap()
                .extended
        );
        assert!(
            !windows_scan_code(PhysicalKey::ControlLeft)
                .unwrap()
                .extended
        );
        assert_eq!(
            physical_from_windows(0x1c, true),
            Some(PhysicalKey::NumpadEnter)
        );
        assert_eq!(physical_from_windows(0x1c, false), Some(PhysicalKey::Enter));
        assert!(windows_scan_code(PhysicalKey::Pause).is_none());
    }
    #[test]
    // Verify that unsupported keys are never reinterpreted as media or help
    fn unsupported_keys_are_never_reinterpreted_as_media_or_help() {
        for key in [
            PhysicalKey::Insert,
            PhysicalKey::PrintScreen,
            PhysicalKey::NumLock,
            PhysicalKey::F24,
        ] {
            assert_eq!(macos_keycode(key), None);
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    #[test]
    // Verify that unqualified native playback is fail closed without starting hooks
    fn unqualified_native_playback_is_fail_closed_without_starting_hooks() {
        assert!(playback_unavailable_reason().is_some());
    }
}
