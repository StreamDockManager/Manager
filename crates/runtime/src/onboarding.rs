use crate::Runtime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SetupProgress {
    pub step: u8,
    pub complete: bool,
    pub dismissed: bool,
    pub new_install: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct SetupCheck {
    pub id: String,
    pub title: String,
    pub status: String,
    pub detail: String,
    pub action: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct SetupReport {
    pub progress: SetupProgress,
    pub checks: Vec<SetupCheck>,
}
impl Runtime {
    // Assemble current device, runtime and permission checks for onboarding
    pub fn setup_report(&self) -> SetupReport {
        let mut checks = vec![];
        let mut add = |id: &str, title: &str, status: &str, detail: String, action: &str| {
            checks.push(SetupCheck {
                id: id.into(),
                title: title.into(),
                status: status.into(),
                detail,
                action: action.into(),
            })
        };
        let snapshot = self.snapshot();
        let devices = snapshot.devices.iter().filter(|d| !d.simulated).count();
        add(
            "device",
            "Stream Dock connection",
            if devices > 0 { "ok" } else { "optional" },
            device_connection_detail(devices),
            "simulator",
        );
        let automation = self.automation_status.lock().unwrap().clone();
        add(
            "foreground",
            "Automatic profiles",
            if automation.available {
                "ok"
            } else {
                "optional"
            },
            automation.reason,
            "recheck",
        );
        #[cfg(target_os="macos")]
        add("accessibility","Keyboard and window-title permission",if crate::automation::title_permission(false){"ok"}else{"optional"},"Allow Stream Dock Manager in System Settings → Privacy & Security → Accessibility when using keyboard actions or title rules.".into(),"accessibility");
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            add(
                "wayland",
                "Wayland keyboard authorization",
                if crate::wayland::authorized() {
                    "ok"
                } else {
                    "optional"
                },
                "Authorize the desktop portal to use keyboard actions in this session.".into(),
                "wayland",
            );
        }
        match crate::media::ffmpeg_path(){
            Ok(_)=>add("ffmpeg","Optional video fallback","optional","FFmpeg was found. Run the selected capability checks to verify the helper without importing media.".into(),"recheck"),
            Err(_)=>add("ffmpeg","Optional video fallback","optional","Install FFmpeg with your system package manager, or set STREAM_DOCK_MANAGER_FFMPEG to its absolute path before launch. Desktop MP4 imports first try the bundled browser decoder; FFmpeg is the fallback for unsupported codecs and headless CLI imports.".into(),"recheck"),
        }
        let status = self.runtimes.status();
        for kind in stream_dock_manager_plugin_runtimes::RuntimeKind::ALL {
            let installed = status
                .installations
                .iter()
                .filter(|i| i.kind == kind)
                .count();
            add(kind.name(), &format!("Optional {} plugins",kind.name()), if installed>0 {"ok"}else{"optional"}, format!("{installed} private installations. Manage supported versions and existing executables in Settings → Runtimes."), "recheck");
        }
        SetupReport {
            progress: self.desktop_settings().onboarding.unwrap_or_default(),
            checks,
        }
    }
    // Save setup progress
    pub fn save_setup_progress(&self, progress: SetupProgress) -> Result<(), String> {
        if progress.step > 4 {
            return Err("Invalid setup step".into());
        }
        self.update_desktop_settings(|settings| settings.onboarding = Some(progress))
    }
}

// Describe connected-device count or explain how to begin without hardware
fn device_connection_detail(devices: usize) -> String {
    match devices {
        0 => "Connect a supported device by USB, or continue with the simulator. On Linux, install the supplied udev rules and reconnect.".into(),
        1 => "1 device connected.".into(),
        count => format!("{count} devices connected."),
    }
}

#[cfg(test)]
mod tests {
    use super::device_connection_detail;

    #[test]
    // Verify that connection guidance is model neutral and preserves setup help
    fn connection_guidance_is_model_neutral_and_preserves_setup_help() {
        assert_eq!(device_connection_detail(1), "1 device connected.");
        assert_eq!(device_connection_detail(3), "3 devices connected.");
        let disconnected = device_connection_detail(0);
        assert!(disconnected.contains("supported device"));
        assert!(disconnected.contains("simulator"));
        assert!(disconnected.contains("udev rules"));
    }
}
