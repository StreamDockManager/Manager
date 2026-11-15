//! Physical mappings, never characters or current-layout virtual-key translation
//! Apple HIToolbox Events.h virtual codes and Windows set-1 scan codes. Unknown,
//! consumer, Fn, IME, and ambiguous multibyte sequences fail closed
use super::PhysicalKey;
#[cfg(any(target_os = "macos", test))]
// Map a macOS key code to a supported physical key
pub(super) fn mac_key(code: u16) -> Option<PhysicalKey> {
    super::super::platform::physical_from_macos(code).filter(|key| *key != PhysicalKey::CapsLock)
}

#[cfg(any(target_os = "windows", test))]
// Map a Windows scan code and extension bit to a physical key
pub(super) fn windows_key(scan: u32, extended: bool) -> Option<PhysicalKey> {
    use PhysicalKey::*;
    super::super::platform::physical_from_windows(u16::try_from(scan).ok()?, extended).filter(
        |key| {
            !matches!(
                key,
                // Preserve the capture adapter's reviewed subset. These identities
                // exist for persistence but their multibyte/international capture
                // behavior requires native qualification before admission
                PrintScreen | Pause | IntlRo | IntlYen | NumpadEqual | NumpadComma
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that mappings preserve all modifier sides
    fn mappings_preserve_all_modifier_sides() {
        use PhysicalKey::*;
        for (code, key) in [
            (0x36, MetaRight),
            (0x37, MetaLeft),
            (0x38, ShiftLeft),
            (0x3c, ShiftRight),
            (0x3a, AltLeft),
            (0x3d, AltRight),
            (0x3b, ControlLeft),
            (0x3e, ControlRight),
        ] {
            assert_eq!(mac_key(code), Some(key));
        }
        assert_eq!(windows_key(0x1d, false), Some(ControlLeft));
        assert_eq!(windows_key(0x1d, true), Some(ControlRight));
        assert_eq!(windows_key(0x38, false), Some(AltLeft));
        assert_eq!(windows_key(0x38, true), Some(AltRight));
        assert_eq!(windows_key(0x2a, false), Some(ShiftLeft));
        assert_eq!(windows_key(0x36, false), Some(ShiftRight));
    }
    #[test]
    // Verify that extended navigation is distinct from numpad
    fn extended_navigation_is_distinct_from_numpad() {
        assert_eq!(windows_key(0x1c, false), Some(PhysicalKey::Enter));
        assert_eq!(windows_key(0x1c, true), Some(PhysicalKey::NumpadEnter));
        assert_eq!(windows_key(0x47, false), Some(PhysicalKey::Numpad7));
        assert_eq!(windows_key(0x47, true), Some(PhysicalKey::Home));
        assert_eq!(windows_key(0x37, true), None);
        assert_eq!(windows_key(u32::MAX, false), None);
        for code in [0x59, 0x73, 0x7d, 0x7e] {
            assert_eq!(windows_key(code, false), None);
        }
        assert_eq!(mac_key(0x39), None); // CapsLock is a toggle flagsChanged event
        assert_eq!(mac_key(0x3f), None); // Fn is not portable
    }
    #[test]
    // Verify that macOS common control keys match the SDK and round trip
    fn macos_common_control_keys_match_the_sdk_and_round_trip() {
        use PhysicalKey::*;
        // HIToolbox/Events.h: Return and keypad Enter are distinct physical keys
        for (code, key) in [
            (36, Enter),
            (76, NumpadEnter),
            (48, Tab),
            (53, Escape),
            (49, Space),
            (51, Backspace),
            (117, Delete),
            (123, ArrowLeft),
            (124, ArrowRight),
            (125, ArrowDown),
            (126, ArrowUp),
            (115, Home),
            (119, End),
            (116, PageUp),
            (121, PageDown),
            (122, F1),
            (120, F2),
            (99, F3),
            (118, F4),
            (96, F5),
            (97, F6),
            (98, F7),
            (100, F8),
            (101, F9),
            (109, F10),
            (103, F11),
            (111, F12),
            (105, F13),
            (107, F14),
            (113, F15),
            (106, F16),
            (64, F17),
            (79, F18),
            (80, F19),
            (90, F20),
        ] {
            assert_eq!(mac_key(code), Some(key));
            assert_eq!(
                super::super::super::platform::macos_keycode(key),
                Some(code)
            );
        }
    }
    #[test]
    // Verify that each native mapping has one canonical identity
    fn each_native_mapping_has_one_canonical_identity() {
        let mut seen = std::collections::HashSet::new();
        for code in 0..=255 {
            if let Some(key) = mac_key(code) {
                assert!(seen.insert(key));
            }
        }
        seen.clear();
        for extended in [false, true] {
            for code in 0..=255 {
                if let Some(key) = windows_key(code, extended) {
                    assert!(seen.insert(key));
                }
            }
        }
    }
}
