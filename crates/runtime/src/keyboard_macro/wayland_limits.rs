//! Limits of this application's portal-based Wayland backend, not a qualification gate
//!
//! RemoteDesktop output authorization does not grant passive keyboard observation or
//! the identity/lifetime of the focused destination. InputCapture v2 redirects input
//! only after compositor-controlled activation (currently pointer barriers), rather
//! than providing an immediately available passive observer. GlobalShortcuts v2 can
//! report configured shortcuts, not every physical press/release or initially held key
//!
//! ext-foreign-toplevel-list-v1 provides lifetime handles but deliberately leaves window
//! state, including focus, to extension protocols. xx-keyboard-filter-v1 is restricted
//! to an active input method with enabled text input; it is not a general keyboard
//! observer and does not control client-generated repeat. None of these interfaces
//! completes the strict macro backend by adding another portal permission alone
//!
//! A future compositor-specific backend must implement its own live session, target,
//! physical-input provenance and repeat contracts. Do not infer those contracts from
//! a desktop name, a listed toplevel, or a RemoteDesktop/InputCapture permission bit
//!
//! API references reviewed 2026-11-10:
//! - <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html>
//! - <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html>
//! - <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html>
//! - <https://wayland.app/protocols/ext-foreign-toplevel-list-v1>
//! - <https://wayland.app/protocols/xx-keyboard-filter-v1>

// Explain why native macro recording is unavailable for this Wayland session
pub(crate) const fn recording_unavailable_reason() -> &'static str {
    "Global recording is unavailable with this Wayland backend: the InputCapture portal redirects input after compositor activation, not immediate passive observation. RemoteDesktop permission does not add recording support. Use in-app recording or manual editing."
}

// Explain why native macro playback is unavailable for this Wayland session
pub(crate) const fn playback_unavailable_reason() -> &'static str {
    "Keyboard macro playback is unavailable with this Wayland backend: portal output permission does not provide physical-key interference checks or a verifiable focused target. Additional portal permission alone cannot enable it. Use simulation; native playback requires a supported compositor-specific backend."
}

// Explain why scratch-target verification is unavailable for this Wayland session
pub(crate) const fn scratch_target_unavailable_reason() -> &'static str {
    "Scratch-target testing is unavailable with this Wayland backend: the portals do not identify the focused destination, and the standard toplevel list does not report focus. Additional portal permission alone cannot enable it. Use simulation; XWayland fallback is not supported."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that each operation explains its missing contract and safe fallback
    fn each_operation_explains_its_missing_contract_and_safe_fallback() {
        let recording = recording_unavailable_reason();
        assert!(recording.contains("passive observation"));
        assert!(recording.contains("in-app recording or manual editing"));
        let playback = playback_unavailable_reason();
        assert!(playback.contains("physical-key interference"));
        assert!(playback.contains("focused target"));
        assert!(playback.contains("Use simulation"));
        let scratch = scratch_target_unavailable_reason();
        assert!(scratch.contains("does not report focus"));
        assert!(scratch.contains("Use simulation"));
        assert!(scratch.contains("XWayland fallback is not supported"));
    }

    #[test]
    // Verify that limits are missing backend contracts not unrun qualification
    fn limits_are_missing_backend_contracts_not_unrun_qualification() {
        for reason in [
            recording_unavailable_reason(),
            playback_unavailable_reason(),
            scratch_target_unavailable_reason(),
        ] {
            assert!(reason.contains("unavailable with this Wayland backend"));
            assert!(!reason.contains("qualification"));
            assert!(!reason.contains("authorize separately"));
            assert!(!reason.contains("grant permission"));
        }
    }
}
