//! Exact repeat is the default execution contract. Explicit System mode instead
//! preserves press/release timing and lets existing X11 controls generate repeats;
//! it never claims recorded repeat counts or rate. Never change the user's repeat
//! settings or convert a held repeat into fabricated tap pairs
//!
//! XKB DetectableAutorepeat changes only the requesting client's event delivery;
//! it does not suppress repeats for the target. RepeatKeys/PerKeyRepeat change
//! keyboard controls, not a private injection transaction. XTest has no repeat
//! event flag; RemoteDesktop and the EI keyboard protocol expose pressed/released
//! states only. ConnectToEIS therefore does not repair this contract by itself
//! Xorg can turn duplicate XTest downs into repeats, conditional on shared repeat
//! controls; that is not an explicit, transaction-local repeat guarantee
//! Verified Xorg path: Xext/xtest.c::ProcXTestFakeInput calls
//! GetKeyboardEvents then mieqProcessDeviceEvent, with no private repeat mode
//! xkb/xkbPrKeyEv.c::ProcessKeyboardEvent applies AccessX filters when
//! RepeatKeys is enabled; xkb/xkbAccessX.c::AccessXFilterPressEvent arms the
//! software timer and AccessXRepeatKeyExpire emits additional downs
//! xkb/ddxCtrls.c::XkbDDXUsesSoftRepeat returns 1. XTEST uses CoreKeyboardProc,
//! not a special repeat-exempt driver. Duplicate XTEST downs also depend on
//! shared controls in GetKeyboardEvents and may be ignored by AccessX while
//! that key is already repeating. No native replay qualification is claimed
//! https://github.com/XQuartz/xorg-server/blob/master/Xext/xtest.c
//! https://github.com/XQuartz/xorg-server/blob/master/xkb/xkbAccessX.c
//! https://github.com/XQuartz/xorg-server/blob/master/xkb/xkbPrKeyEv.c
//! https://github.com/XQuartz/xorg-server/blob/master/xkb/ddxCtrls.c
//! https://github.com/XQuartz/xorg-server/blob/master/dix/getevents.c
//! https://www.x.org/releases/current/doc/kbproto/xkbproto.html#Detectable_Autorepeat
//! https://www.x.org/docs/Xext/xtest.pdf
//! https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html
//! https://libinput.pages.freedesktop.org/libei/interfaces/ei_keyboard/index.html
use crate::keyboard_macro::types::{
    KeyboardMacroEventKind, KeyboardMacroRecording, KeyboardMacroRepeatMode, PhysicalKey,
};

/// Reject an opt-in mode on backends that do not implement that meaning. Import,
/// editing and simulation remain platform independent; native preflight is not
#[cfg(any(target_os = "macos", target_os = "windows", test))]
// Require exact
pub(crate) fn require_exact(mode: KeyboardMacroRepeatMode) -> Result<(), String> {
    match mode {
        KeyboardMacroRepeatMode::Exact => Ok(()),
        KeyboardMacroRepeatMode::System => Err("System repeat mode is supported only by Linux backends. Choose Exact repeat mode for this platform.".into()),
    }
}

/// A system-repeat event remains a scheduled checkpoint, not an output event
/// No fabricated key-up/down or ownership mutation is allowed for this skip
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
// Report the skip recorded repeat condition for repeat
pub(crate) fn skip_recorded_repeat(
    mode: KeyboardMacroRepeatMode,
    kind: KeyboardMacroEventKind,
) -> bool {
    mode == KeyboardMacroRepeatMode::System && kind == KeyboardMacroEventKind::Repeat
}

/// Read-only XKB repeat controls for one keyboard. This is policy input, not a
/// claim that the device or controls have been observed or qualified
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct X11RepeatControls {
    pub repeat_keys_enabled: bool,
    pub per_key_repeat: [u8; 32],
}
impl X11RepeatControls {
    // Report the repeats condition for repeat
    fn repeats(&self, keycode: u8) -> bool {
        self.repeat_keys_enabled
            && self.per_key_repeat[usize::from(keycode) / 8] & (1 << (keycode % 8)) != 0
    }
}

/// System mode changes only repeat semantics. All keys still resolve through the
/// caller's full action/mapping checks, and both live control snapshots remain
/// pinned/revocable. Existing user repeat preferences are never modified
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
// Reject recordings whose requested repeat behavior cannot be guaranteed by the X11 controls
pub(crate) fn preflight_x11(
    recording: &KeyboardMacroRecording,
    mode: KeyboardMacroRepeatMode,
    master: &X11RepeatControls,
    source: &X11RepeatControls,
    mut resolve: impl FnMut(PhysicalKey) -> Result<u8, String>,
) -> Result<(), String> {
    if mode == KeyboardMacroRepeatMode::Exact
        && recording
            .events
            .iter()
            .any(|event| event.kind == KeyboardMacroEventKind::Repeat)
    {
        return Err("Exact X11 playback does not support explicit repeat events. Choose System repeat mode to use current system repeat settings instead".into());
    }
    for event in &recording.events {
        let code = resolve(event.key)?;
        if code < 8 {
            return Err("X11 physical key resolution is invalid".into());
        }
        if mode == KeyboardMacroRepeatMode::Exact && (master.repeats(code) || source.repeats(code))
        {
            return Err("A recorded key can auto-repeat under the current XKB controls. Choose System repeat mode to use current system repeat settings instead".into());
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
#[cfg(any(target_os = "macos", target_os = "windows", test))]
pub(crate) enum Mechanism {
    /// CoreGraphics provides an explicit autorepeat field on each posted event
    #[cfg(any(target_os = "macos", test))]
    ExplicitFlag,
    /// SendInput provides repeated key-down, but no explicit repeat-count field
    #[cfg(any(target_os = "windows", test))]
    RepeatedDown,
}
#[cfg(any(target_os = "macos", target_os = "windows", test))]
// Check repeat prerequisites before proceeding
pub(crate) fn preflight(
    mechanism: Mechanism,
    _recording: &KeyboardMacroRecording,
) -> Result<(), String> {
    // CoreGraphics supplies the required per-event representation. Missing native
    // delivery-matrix evidence is not an unsupported-API condition. The caller
    // still validates structure, permissions, session/target and key ownership;
    // this success does not claim observed target receipt or native qualification
    // Ordinary downs carry 0 and explicit repeats carry 1 in field 8. Do not
    // synthesize timer-based repeats or change the user's repeat preferences
    // https://developer.apple.com/documentation/coregraphics/cgeventfield/keyboardeventautorepeat
    // SendInput inserts explicit down/up events; a validated Repeat is another
    // down while the recorded key remains held, never a fabricated up/down pair
    // Successful insertion is not proof of application receipt or native timing
    // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-sendinput
    match mechanism {
        #[cfg(any(target_os = "macos", test))]
        Mechanism::ExplicitFlag => Ok(()),
        #[cfg(any(target_os = "windows", test))]
        Mechanism::RepeatedDown => Ok(()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that system mode is explicit and does not mutate repeat controls
    fn system_mode_is_explicit_and_does_not_mutate_repeat_controls() {
        let master = controls(true, &[38]);
        let source = controls(true, &[38]);
        let before = (master.clone(), source.clone());
        let mut recording = held_key(400);
        recording.events.insert(
            1,
            crate::keyboard_macro::KeyboardMacroEvent {
                key: PhysicalKey::KeyA,
                kind: KeyboardMacroEventKind::Repeat,
                delay_ms: 200,
            },
        );
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::Exact,
            &master,
            &source,
            |_| Ok(38)
        )
        .is_err());
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::System,
            &master,
            &source,
            |_| Ok(38)
        )
        .is_ok());
        assert_eq!((master.clone(), source.clone()), before);
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::System,
            &master,
            &source,
            |_| Err("Unsupported action or mapping".into())
        )
        .is_err());
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::System,
            &master,
            &source,
            |_| Ok(7)
        )
        .is_err());
        assert!(require_exact(KeyboardMacroRepeatMode::Exact).is_ok());
        assert!(require_exact(KeyboardMacroRepeatMode::System).is_err());
        assert!(skip_recorded_repeat(
            KeyboardMacroRepeatMode::System,
            KeyboardMacroEventKind::Repeat
        ));
        for kind in [
            KeyboardMacroEventKind::KeyDown,
            KeyboardMacroEventKind::KeyUp,
        ] {
            assert!(!skip_recorded_repeat(KeyboardMacroRepeatMode::System, kind));
        }
        assert!(!skip_recorded_repeat(
            KeyboardMacroRepeatMode::Exact,
            KeyboardMacroEventKind::Repeat
        ));
    }
    #[test]
    // Verify that explicit flag represents ordinary holds and recorded repeats
    fn explicit_flag_represents_ordinary_holds_and_recorded_repeats() {
        let mut recording = held_key(10_000);
        assert!(preflight(Mechanism::ExplicitFlag, &recording).is_ok());
        recording.events.insert(
            1,
            crate::keyboard_macro::KeyboardMacroEvent {
                key: PhysicalKey::KeyA,
                kind: KeyboardMacroEventKind::Repeat,
                delay_ms: 100,
            },
        );
        assert!(preflight(Mechanism::ExplicitFlag, &recording).is_ok());
        assert!(preflight(Mechanism::RepeatedDown, &recording).is_ok());
    }

    #[test]
    // Verify that repeated down represents ordinary holds without timer synthesis
    fn repeated_down_represents_ordinary_holds_without_timer_synthesis() {
        for delay in [0, 10_000] {
            assert!(preflight(Mechanism::RepeatedDown, &held_key(delay)).is_ok());
        }
    }

    // Construct X11 repeat controls with the selected per-key bits
    fn controls(enabled: bool, repeating: &[u8]) -> X11RepeatControls {
        let mut per_key_repeat = [0; 32];
        for &code in repeating {
            per_key_repeat[usize::from(code) / 8] |= 1 << (code % 8);
        }
        X11RepeatControls {
            repeat_keys_enabled: enabled,
            per_key_repeat,
        }
    }

    // Construct a recording that holds one key for the requested duration
    fn held_key(delay_ms: u32) -> KeyboardMacroRecording {
        use crate::keyboard_macro::KeyboardMacroEvent;
        KeyboardMacroRecording {
            events: vec![
                KeyboardMacroEvent {
                    key: PhysicalKey::KeyA,
                    kind: KeyboardMacroEventKind::KeyDown,
                    delay_ms: 0,
                },
                KeyboardMacroEvent {
                    key: PhysicalKey::KeyA,
                    kind: KeyboardMacroEventKind::KeyUp,
                    delay_ms,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    // Verify that X11 candidate accepts only effectively nonrepeating keys without changing controls
    fn x11_candidate_accepts_only_effectively_nonrepeating_keys_without_changing_controls() {
        let master = controls(true, &[8, 254]);
        let source = controls(false, &[8, 255]);
        let before = (master.clone(), source.clone());
        for delay in [0, 10_000] {
            assert!(preflight_x11(
                &held_key(delay),
                KeyboardMacroRepeatMode::Exact,
                &master,
                &source,
                |_| Ok(255)
            )
            .is_ok());
        }
        assert_eq!((master, source), before);
    }

    #[test]
    // Verify that X11 candidate rejects repeat on either keyboard even for zero delay taps
    fn x11_candidate_rejects_repeat_on_either_keyboard_even_for_zero_delay_taps() {
        let quiet = controls(true, &[]);
        let repeats = controls(true, &[8]);
        for (master, source) in [(&quiet, &repeats), (&repeats, &quiet)] {
            assert!(preflight_x11(
                &held_key(0),
                KeyboardMacroRepeatMode::Exact,
                master,
                source,
                |_| Ok(8)
            )
            .is_err());
        }
    }

    #[test]
    // Verify that X11 candidate rejects explicit repeats unknown mapping and control changes
    fn x11_candidate_rejects_explicit_repeats_unknown_mapping_and_control_changes() {
        let quiet = controls(false, &[]);
        let mut recording = held_key(10_000);
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::Exact,
            &quiet,
            &quiet,
            |_| Ok(7)
        )
        .is_err());
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::Exact,
            &quiet,
            &quiet,
            |_| Err("unknown".into())
        )
        .is_err());
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::Exact,
            &quiet,
            &quiet,
            |_| Ok(38)
        )
        .is_ok());
        let changed = controls(true, &[38]);
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::Exact,
            &quiet,
            &changed,
            |_| Ok(38)
        )
        .is_err());
        recording.events[1].kind = KeyboardMacroEventKind::Repeat;
        assert!(preflight_x11(
            &recording,
            KeyboardMacroRepeatMode::Exact,
            &quiet,
            &quiet,
            |_| Ok(38)
        )
        .is_err());
    }
}
