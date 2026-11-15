use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct Diagnostic {
    pub category: String,
    pub value: String,
    pub status: String,
}
// Collect the diagnostics values
pub fn collect() -> Vec<Diagnostic> {
    let mut rows = vec![];
    let mut add = |category: &str, value: String, status: &str| {
        rows.push(Diagnostic {
            category: category.into(),
            value,
            status: status.into(),
        })
    };
    add(
        "Platform",
        format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        "info",
    );
    if let Ok(bytes) = stream_dock_manager_plugin_runtimes::bounded_io::read(
        std::path::Path::new("/etc/os-release"),
        8192,
        "Distribution metadata exceeds 8 KiB",
    ) {
        add(
            "Distribution",
            String::from_utf8_lossy(&bytes).into_owned(),
            "info",
        );
    }
    for (name, var) in [
        ("Desktop", "XDG_CURRENT_DESKTOP"),
        ("Session", "XDG_SESSION_TYPE"),
        ("Display", "WAYLAND_DISPLAY"),
    ] {
        if let Ok(value) = std::env::var(var) {
            add(name, value, "info");
        }
    }
    match hidapi::HidApi::new() {
        Ok(api) => {
            let count = api
                .device_list()
                .filter(|d| hardware_registry::find(&crate::devices::identity(d)).is_some())
                .count();
            add(
                "Supported HID interfaces",
                count.to_string(),
                if count > 0 { "ok" } else { "info" },
            );
        }
        Err(e) => add("HID enumeration", e.to_string(), "error"),
    }
    #[cfg(target_os = "linux")]
    for (name, program, args) in [
        ("Kernel", "uname", vec!["-r"]),
        ("Audio", "pactl", vec!["info"]),
        ("Media / MPRIS", "playerctl", vec!["--list-all"]),
        (
            "Remote Desktop portal",
            "gdbus",
            vec![
                "introspect",
                "--session",
                "--dest",
                "org.freedesktop.portal.Desktop",
                "--object-path",
                "/org/freedesktop/portal/desktop",
            ],
        ),
        (
            "WebKitGTK development metadata (not loaded runtime)",
            "pkg-config",
            vec!["--modversion", "webkit2gtk-4.1"],
        ),
        (
            "GTK development metadata (not loaded runtime)",
            "pkg-config",
            vec!["--modversion", "gtk+-3.0"],
        ),
        ("Graphics", "lspci", vec![]),
    ] {
        match command_output(program, &args) {
            Ok((text, success)) => add(name, text, if success { "ok" } else { "unavailable" }),
            Err(error) => add(name, error, "unavailable"),
        }
    }
    #[cfg(target_os = "linux")]
    add(
        "Wayland keyboard authorization",
        crate::wayland::authorized().to_string(),
        "info",
    );
    add(
        "Hardware qualification",
        "No reviewed physical verification records are bundled.".into(),
        "experimental",
    );
    add(
        "Plugin runtimes",
        "Node.js LTS is optional and is not bundled. Install a private version or select a compatible executable in Settings → Runtimes.".into(),
        "info",
    );
    for kind in stream_dock_manager_plugin_runtimes::RuntimeKind::ALL {
        if let Some(path) = std::env::var_os(kind.env()) {
            add(
                kind.env(),
                format!("Configured external selection: {}", path.to_string_lossy()),
                "info",
            );
        }
    }
    match crate::media::ffmpeg_path() {
        Ok(path) => add("MP4 import / FFmpeg", path.display().to_string(), "ok"),
        Err(error) => add("MP4 import / FFmpeg", error, "unavailable"),
    }
    rows
}

#[cfg(any(target_os = "linux", all(test, unix)))]
// Run a diagnostic command with a five-second deadline and report stdout plus success
fn command_output(program: &str, args: &[&str]) -> Result<(String, bool), String> {
    let args = args
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let output = crate::builtins::bounded_command_result(program, &args, "", 5000, || Ok(()))?;
    Ok((
        output["stdout"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(8192)
            .collect(),
        output["exit_status"] == 0,
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    // Verify that diagnostic helpers keep status and bounded visible output
    fn diagnostic_helpers_keep_status_and_bounded_visible_output() {
        assert_eq!(
            command_output("/bin/sh", &["-c", "printf 'fixture'; exit 7"]).unwrap(),
            ("fixture".into(), false)
        );
        assert_eq!(
            command_output("/bin/sh", &["-c", "printf 'ready'"]).unwrap(),
            ("ready".into(), true)
        );
    }
    #[test]
    // Verify that oversized helper output fails before unbounded capture
    fn oversized_helper_output_fails_before_unbounded_capture() {
        let error =
            command_output("/bin/sh", &["-c", "yes fixture | head -c 2097152"]).unwrap_err();
        assert!(error.contains("exceeds 1 MiB"), "{error}");
    }
    #[test]
    // Verify that oversized helper output retires a live descendant
    fn oversized_helper_output_retires_a_live_descendant() {
        let directory = tempfile::tempdir().unwrap();
        let pid_path = directory.path().join("child.pid");
        let error = command_output(
            "/bin/sh",
            &[
                "-c",
                "sleep 30 & echo $! > \"$1\"; yes fixture | head -c 2097152; wait",
                "fixture",
                pid_path.to_str().unwrap(),
            ],
        )
        .unwrap_err();
        assert!(error.contains("exceeds 1 MiB"), "{error}");
        let pid = std::fs::read_to_string(pid_path).unwrap();
        let pid = pid.trim();
        let running = || {
            let output = std::process::Command::new("ps")
                .args(["-p", pid, "-o", "stat="])
                .output()
                .unwrap();
            let state = String::from_utf8_lossy(&output.stdout);
            let state = state.trim();
            !state.is_empty() && !state.starts_with('Z')
        };
        for _ in 0..50 {
            if !running() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // Retire only this fixture's child if cleanup regresses
        let _ = std::process::Command::new("kill")
            .args(["-KILL", pid])
            .status();
        panic!("bounded command capture left its live descendant running");
    }
}

/// Deliberately allowlisted support metadata, separate from the verbose local diagnostics
/// Never copy command output, host/display names, paths or unknown environment values
pub fn support_environment() -> serde_json::Value {
    let release = stream_dock_manager_plugin_runtimes::bounded_io::read(
        std::path::Path::new("/etc/os-release"),
        8192,
        "Distribution metadata exceeds limits",
    )
    .ok();
    let distro = release
        .as_ref()
        .map(|b| distribution(&String::from_utf8_lossy(b)))
        .unwrap_or_default();
    let session = match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("x11") => "x11",
        Ok("wayland") => "wayland",
        Ok("tty") => "tty",
        _ => "unknown",
    };
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let desktop = desktop
        .split(':')
        .find_map(|name| match name.to_ascii_lowercase().as_str() {
            "gnome" => Some("GNOME"),
            "kde" => Some("KDE"),
            "xfce" => Some("XFCE"),
            "cinnamon" => Some("Cinnamon"),
            "mate" => Some("MATE"),
            "sway" => Some("Sway"),
            "hyprland" => Some("Hyprland"),
            _ => None,
        });
    #[cfg(target_os = "linux")]
    let authorized = Some(crate::wayland::authorized());
    #[cfg(not(target_os = "linux"))]
    let authorized: Option<bool> = None;
    serde_json::json!({"os":std::env::consts::OS, "architecture":std::env::consts::ARCH,
        "distribution":distro, "session":session, "desktop":desktop,
        "os_version": sysinfo::System::os_version().ok().and_then(|value| numeric_version(&value)),
        "kernel_version": sysinfo::System::kernel_version().ok().and_then(|value| numeric_version(&value)),
        "backend":if cfg!(target_os="linux") { match session { "wayland"=>"Wayland portal", "x11"=>"X11", _=>"unknown" } } else if cfg!(target_os="macos") { "Cocoa" } else if cfg!(target_os="windows") { "Win32" } else { "unknown" },
        "wayland_authorized": authorized
    })
}
// Extract a bounded numeric version prefix without retaining descriptive system text
fn numeric_version(value: &str) -> Option<String> {
    let value: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .take(64)
        .collect();
    (!value.is_empty()).then_some(value)
}
// Extract only the distribution ID and version from os-release content
fn distribution(text: &str) -> std::collections::BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if !matches!(key, "ID" | "VERSION_ID") {
                return None;
            }
            let value = value.trim_matches('"');
            (!value.is_empty()
                && value.len() <= 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
            .then(|| (key.to_ascii_lowercase(), value.into()))
        })
        .collect()
}
#[cfg(test)]
mod support_tests {
    use super::*;
    #[test]
    // Verify that distribution metadata is allowlisted and bounded
    fn distribution_metadata_is_allowlisted_and_bounded() {
        let parsed=distribution("ID=ubuntu\nVERSION_ID=\"24.04\"\nHOME_URL=https://secret.example\nHOSTNAME=private\nVERSION_ID=/home/private\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed["id"], "ubuntu");
        assert_eq!(parsed["version_id"], "24.04");
        assert!(distribution(&format!("ID={}", "a".repeat(65))).is_empty());
        assert_eq!(
            numeric_version("6.14.2-private-host"),
            Some("6.14.2".into())
        );
        assert_eq!(numeric_version("/home/private"), None);
        assert_eq!(numeric_version(&"9".repeat(100)).unwrap().len(), 64);
    }
}
