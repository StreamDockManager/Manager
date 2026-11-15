//! Linux evdev identities and XKB physical position names. X11 uses the server's
//! checked XKB names, never the common but nonportable `evdev + 8` assumption
use super::PhysicalKey;

// Map the physical key to its Linux evdev code
pub fn evdev_code(key: PhysicalKey) -> Option<u16> {
    use PhysicalKey::*;
    // The standard non-extended set-1 positions intentionally share Linux's first
    // 88 evdev identities; all extended/later keys are mapped explicitly below.
    Some(match key {
        IntlRo => 89,
        NumpadEnter => 96,
        ControlRight => 97,
        NumpadDivide => 98,
        PrintScreen => 99,
        AltRight => 100,
        Home => 102,
        ArrowUp => 103,
        PageUp => 104,
        ArrowLeft => 105,
        ArrowRight => 106,
        End => 107,
        ArrowDown => 108,
        PageDown => 109,
        Insert => 110,
        Delete => 111,
        NumpadEqual => 117,
        Pause => 119,
        NumpadComma => 121,
        IntlYen => 124,
        MetaLeft => 125,
        MetaRight => 126,
        F13 => 183,
        F14 => 184,
        F15 => 185,
        F16 => 186,
        F17 => 187,
        F18 => 188,
        F19 => 189,
        F20 => 190,
        F21 => 191,
        F22 => 192,
        F23 => 193,
        F24 => 194,
        _ => {
            let scan = super::windows_scan_code(key)?;
            if scan.extended || scan.code > 88 {
                return None;
            }
            scan.code
        }
    })
}
// Map a Linux evdev code to a supported physical key
pub fn physical_from_evdev(code: u16) -> Option<PhysicalKey> {
    PhysicalKey::ALL
        .iter()
        .copied()
        .find(|key| evdev_code(*key) == Some(code))
}
// Resolve the XKB key name for the physical key
pub fn xkb_name(key: PhysicalKey) -> Option<[u8; 4]> {
    use PhysicalKey::*;
    Some(*match key {
        KeyQ => b"AD01",
        KeyW => b"AD02",
        KeyE => b"AD03",
        KeyR => b"AD04",
        KeyT => b"AD05",
        KeyY => b"AD06",
        KeyU => b"AD07",
        KeyI => b"AD08",
        KeyO => b"AD09",
        KeyP => b"AD10",
        KeyA => b"AC01",
        KeyS => b"AC02",
        KeyD => b"AC03",
        KeyF => b"AC04",
        KeyG => b"AC05",
        KeyH => b"AC06",
        KeyJ => b"AC07",
        KeyK => b"AC08",
        KeyL => b"AC09",
        KeyZ => b"AB01",
        KeyX => b"AB02",
        KeyC => b"AB03",
        KeyV => b"AB04",
        KeyB => b"AB05",
        KeyN => b"AB06",
        KeyM => b"AB07",
        Digit1 => b"AE01",
        Digit2 => b"AE02",
        Digit3 => b"AE03",
        Digit4 => b"AE04",
        Digit5 => b"AE05",
        Digit6 => b"AE06",
        Digit7 => b"AE07",
        Digit8 => b"AE08",
        Digit9 => b"AE09",
        Digit0 => b"AE10",
        Minus => b"AE11",
        Equal => b"AE12",
        Backquote => b"TLDE",
        BracketLeft => b"AD11",
        BracketRight => b"AD12",
        Backslash => b"BKSL",
        Semicolon => b"AC10",
        Quote => b"AC11",
        Comma => b"AB08",
        Period => b"AB09",
        Slash => b"AB10",
        IntlBackslash => b"LSGT",
        IntlRo => b"AB11",
        IntlYen => b"AE13",
        Backspace => b"BKSP",
        Tab => b"TAB\0",
        Enter => b"RTRN",
        Space => b"SPCE",
        Escape => b"ESC\0",
        CapsLock => b"CAPS",
        ShiftLeft => b"LFSH",
        ShiftRight => b"RTSH",
        ControlLeft => b"LCTL",
        ControlRight => b"RCTL",
        AltLeft => b"LALT",
        AltRight => b"RALT",
        MetaLeft => b"LWIN",
        MetaRight => b"RWIN",
        Insert => b"INS\0",
        Delete => b"DELE",
        Home => b"HOME",
        End => b"END\0",
        PageUp => b"PGUP",
        PageDown => b"PGDN",
        ArrowLeft => b"LEFT",
        ArrowRight => b"RGHT",
        ArrowUp => b"UP\0\0",
        ArrowDown => b"DOWN",
        PrintScreen => b"PRSC",
        ScrollLock => b"SCLK",
        Pause => b"PAUS",
        NumLock => b"NMLK",
        F1 => b"FK01",
        F2 => b"FK02",
        F3 => b"FK03",
        F4 => b"FK04",
        F5 => b"FK05",
        F6 => b"FK06",
        F7 => b"FK07",
        F8 => b"FK08",
        F9 => b"FK09",
        F10 => b"FK10",
        F11 => b"FK11",
        F12 => b"FK12",
        F13 => b"FK13",
        F14 => b"FK14",
        F15 => b"FK15",
        F16 => b"FK16",
        F17 => b"FK17",
        F18 => b"FK18",
        F19 => b"FK19",
        F20 => b"FK20",
        F21 => b"FK21",
        F22 => b"FK22",
        F23 => b"FK23",
        F24 => b"FK24",
        Numpad0 => b"KP0\0",
        Numpad1 => b"KP1\0",
        Numpad2 => b"KP2\0",
        Numpad3 => b"KP3\0",
        Numpad4 => b"KP4\0",
        Numpad5 => b"KP5\0",
        Numpad6 => b"KP6\0",
        Numpad7 => b"KP7\0",
        Numpad8 => b"KP8\0",
        Numpad9 => b"KP9\0",
        NumpadAdd => b"KPAD",
        NumpadSubtract => b"KPSU",
        NumpadMultiply => b"KPMU",
        NumpadDivide => b"KPDV",
        NumpadDecimal => b"KPDL",
        NumpadEnter => b"KPEN",
        NumpadEqual => b"KPEQ",
        NumpadComma => return None,
    })
}
/// A required physical name must have exactly one server keycode. Duplicates or
/// missing names fail preflight; no keysym or layout-dependent guess is permitted
pub fn resolve_xkb(key: PhysicalKey, names: &[(u8, [u8; 4])]) -> Result<u8, String> {
    let name = xkb_name(key).ok_or("Unsupported XKB physical key")?;
    let mut matches = names.iter().filter(|(_, candidate)| *candidate == name);
    let code = matches
        .next()
        .map(|(code, _)| *code)
        .ok_or("Physical key is missing from the XKB map")?;
    if matches.next().is_some() {
        return Err("Physical key is ambiguous in the XKB map".into());
    }
    Ok(code)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that evdev map round trips without extended or function aliases
    fn evdev_map_round_trips_without_extended_or_function_aliases() {
        let mut seen = std::collections::HashSet::new();
        for key in PhysicalKey::ALL {
            let code = evdev_code(*key).unwrap();
            assert!(seen.insert(code));
            assert_eq!(physical_from_evdev(code), Some(*key));
        }
        assert_eq!(evdev_code(PhysicalKey::ControlRight), Some(97));
        assert_eq!(evdev_code(PhysicalKey::F24), Some(194));
    }
    #[test]
    // Verify that X11 resolution uses server names and rejects ambiguity
    fn x11_resolution_uses_server_names_and_rejects_ambiguity() {
        let names = [(209, *b"AC01"), (38, *b"AD01")];
        assert_eq!(resolve_xkb(PhysicalKey::KeyA, &names).unwrap(), 209);
        assert!(resolve_xkb(PhysicalKey::KeyB, &names).is_err());
        assert!(resolve_xkb(PhysicalKey::KeyA, &[(1, *b"AC01"), (2, *b"AC01")]).is_err());
    }
    #[test]
    // Verify that xkb names do not alias numpad or modifier sides
    fn xkb_names_do_not_alias_numpad_or_modifier_sides() {
        let mut seen = std::collections::HashSet::new();
        for key in PhysicalKey::ALL {
            if let Some(name) = xkb_name(*key) {
                assert!(seen.insert(name));
            }
        }
        assert_ne!(
            xkb_name(PhysicalKey::NumpadEnter),
            xkb_name(PhysicalKey::Enter)
        );
    }
}
