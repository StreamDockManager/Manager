//! Passive startup checks. Only explicit completion admits desktop HID discovery
use crate::Runtime;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct StartupPermission {
    pub id: String,
    pub title: String,
    pub description: String,
    pub instructions: String,
    pub status: String,
    pub required: bool,
    pub settings_available: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct StartupPermissionReport {
    pub platform: String,
    pub ready: bool,
    pub setup_ready: bool,
    pub development_mode: bool,
    pub skipped_permissions: Vec<String>,
    pub device_connections_enabled: bool,
    pub permissions: Vec<StartupPermission>,
}

pub(crate) struct DeviceAdmission(AtomicBool);
impl DeviceAdmission {
    // Initialize startup permissions state from the supplied inputs
    pub(crate) fn new(enabled: bool) -> Self {
        Self(AtomicBool::new(enabled))
    }
    // Report the enabled condition for startup permissions
    pub(crate) fn enabled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    // Apply the new startup permissions state
    pub(crate) fn update(&self, ready: bool, complete: bool) {
        if !ready || complete {
            self.0.store(ready, Ordering::Release);
        }
    }
}

// Assemble the startup permissions report
fn report(
    platform: &str,
    input: bool,
    accessibility: bool,
    screen: bool,
) -> StartupPermissionReport {
    let mut permissions = Vec::new();
    if platform == "macos" {
        for (id, title, description, required, granted, instructions) in [
            ("input_monitoring", "Input Monitoring", "Read connected Stream Dock buttons and dials, and record global keyboard macros.", true, input,
             "Open System Settings → Privacy & Security → Input Monitoring. Enable Stream Dock Manager, or add the installed copy from Applications with +. Fully quit and reopen Manager if macOS requests it. A development or Terminal grant may not apply to this app."),
            ("accessibility", "Accessibility", "Run keyboard actions and macros and identify the active window for automatic profiles.", true, accessibility,
             "Open System Settings → Privacy & Security → Accessibility. Enable Stream Dock Manager, or add the installed copy from Applications with +. Return here to check again. This is separate from Input Monitoring."),
            ("screen_recording", "Screen Recording", "Optional: capture screen content for features that use screenshots. Not needed to connect devices.", false, screen,
             "Only enable this if you use screen-capture features. Open System Settings → Privacy & Security → Screen Recording (called Screen & System Audio Recording on newer macOS versions), enable Stream Dock Manager, and reopen it if requested."),
        ] {
            permissions.push(StartupPermission { id: id.into(), title: title.into(), description: description.into(), instructions: instructions.into(), status: if granted { "granted" } else { "missing" }.into(), required, settings_available: true });
        }
    }
    let ready = permissions
        .iter()
        .all(|p| !p.required || p.status == "granted");
    StartupPermissionReport {
        platform: platform.into(),
        ready,
        setup_ready: ready,
        development_mode: false,
        skipped_permissions: Vec::new(),
        device_connections_enabled: false,
        permissions,
    }
}

/// Read permission state only. Never request a system prompt or capture input/pixels
pub fn check() -> StartupPermissionReport {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "ApplicationServices", kind = "framework")]
        unsafe extern "C" {
            fn CGPreflightListenEventAccess() -> bool;
            fn CGPreflightScreenCaptureAccess() -> bool;
        }
        report(
            "macos",
            unsafe { CGPreflightListenEventAccess() },
            crate::automation::title_permission(false),
            unsafe { CGPreflightScreenCaptureAccess() },
        )
    }
    #[cfg(not(target_os = "macos"))]
    report(std::env::consts::OS, true, true, true)
}

/// Fixed allowlist: renderer-supplied values never become arbitrary URLs or commands
pub fn settings_url(id: &str) -> Result<&'static str, String> {
    match id {
        "input_monitoring" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
        }
        "accessibility" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        "screen_recording" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        }
        _ => Err("Unknown startup permission".into()),
    }
}
// Only the explicitly acknowledged macOS input check may be omitted from HID admission
fn hid_eligible(report: &StartupPermissionReport, skip_input_monitoring: bool) -> bool {
    report.permissions.iter().all(|permission| {
        !permission.required
            || permission.status == "granted"
            || (report.platform == "macos"
                && skip_input_monitoring
                && permission.id == "input_monitoring")
    })
}

impl Runtime {
    // Refresh startup permission admission and wake device processing when connections become enabled
    pub fn startup_permissions(&self, complete: bool) -> StartupPermissionReport {
        self.startup_permissions_with_input_monitoring_skip(complete, false)
    }
    // Allow a trusted desktop development acknowledgement to attempt HID without granting OS access
    pub fn startup_permissions_with_input_monitoring_skip(
        &self,
        complete: bool,
        skip_input_monitoring: bool,
    ) -> StartupPermissionReport {
        let mut report = check();
        let eligible = hid_eligible(&report, skip_input_monitoring);
        let previously_enabled = self.device_admission.enabled();
        self.device_admission.update(eligible, complete);
        report.device_connections_enabled = self.device_admission.enabled();
        if report.device_connections_enabled != previously_enabled || (complete && eligible) {
            let _ = self.device_commands.send(crate::devices::Request::Wake);
        }
        report
    }
    // Report the device connections enabled condition for startup permissions
    pub fn device_connections_enabled(&self) -> bool {
        self.device_admission.enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that only required permissions block setup
    fn only_required_permissions_block_setup() {
        assert!(report("macos", true, true, false).ready);
        assert!(!report("macos", false, true, true).ready);
        assert!(!report("macos", true, false, true).ready);
        assert!(report("linux", false, false, false).ready);
    }
    #[test]
    // Verify that passive checks never first enable and revocation closes gate
    fn passive_checks_never_first_enable_and_revocation_closes_gate() {
        let gate = DeviceAdmission::new(false);
        gate.update(true, false);
        assert!(!gate.enabled());
        gate.update(false, true);
        assert!(!gate.enabled());
        gate.update(true, true);
        assert!(gate.enabled());
        gate.update(false, false);
        assert!(!gate.enabled());
        gate.update(true, false);
        assert!(!gate.enabled());
    }
    #[test]
    // A development input acknowledgement changes only HID eligibility and preserves every other blocker
    fn input_skip_is_narrow_and_does_not_grant_capabilities() {
        let missing_input = report("macos", false, true, false);
        assert!(!missing_input.ready);
        assert!(!hid_eligible(&missing_input, false));
        assert!(hid_eligible(&missing_input, true));
        assert_eq!(missing_input.permissions[0].status, "missing");
        assert!(!hid_eligible(&report("macos", false, false, true), true));
        let mut other_platform = missing_input.clone();
        other_platform.platform = "windows".into();
        assert!(!hid_eligible(&other_platform, true));
        let gate = DeviceAdmission::new(false);
        gate.update(hid_eligible(&missing_input, true), false);
        assert!(!gate.enabled());
        gate.update(hid_eligible(&missing_input, true), true);
        assert!(gate.enabled());
        gate.update(hid_eligible(&missing_input, true), false);
        assert!(gate.enabled());
        gate.update(
            hid_eligible(&report("macos", false, false, true), true),
            false,
        );
        assert!(!gate.enabled());
    }
    #[test]
    // Verify that settings targets are closed allowlist
    fn settings_targets_are_closed_allowlist() {
        assert!(settings_url("input_monitoring")
            .unwrap()
            .ends_with("Privacy_ListenEvent"));
        assert!(settings_url("accessibility")
            .unwrap()
            .ends_with("Privacy_Accessibility"));
        assert!(settings_url("screen_recording")
            .unwrap()
            .ends_with("Privacy_ScreenCapture"));
        assert!(settings_url("https://example.com").is_err());
        assert!(settings_url("; open /tmp").is_err());
    }
}
