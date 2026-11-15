//! Read-only, optional session providers. No permission prompts or machine power actions
use super::SensorSample;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PowerPlatformCapabilities {
    pub idle: bool,
    pub lock: bool,
    pub idle_description: String,
    pub lock_description: String,
}
#[cfg(target_os = "macos")]
// Describe supported native capabilities
pub fn capabilities() -> PowerPlatformCapabilities {
    PowerPlatformCapabilities {
        idle: true,
        lock: false,
        idle_description:
            "Core Graphics input idle; native session accuracy requires qualification".into(),
        lock_description: "A supported macOS lock-state source is unavailable".into(),
    }
}
#[cfg(target_os = "macos")]
// Collect a native sample
pub fn sample(idle: bool, _lock: bool) -> SensorSample {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    let idle = idle
        .then(|| unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) })
        .and_then(|value| Duration::try_from_secs_f64(value).ok());
    SensorSample { idle, locked: None }
}
#[cfg(target_os = "windows")]
// Describe supported native capabilities
pub fn capabilities() -> PowerPlatformCapabilities {
    PowerPlatformCapabilities {
        idle: true,
        lock: true,
        idle_description: "Current Windows session input idle".into(),
        lock_description:
            "Current Windows session WTS lock flag; native session accuracy requires qualification"
                .into(),
    }
}
#[cfg(target_os = "windows")]
// Collect a native sample
pub fn sample(idle: bool, lock: bool) -> SensorSample {
    use windows::Win32::{
        System::{RemoteDesktop::*, SystemInformation::GetTickCount},
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };
    let idle = if idle {
        unsafe {
            let mut info = LASTINPUTINFO {
                cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
                ..Default::default()
            };
            if GetLastInputInfo(&mut info).as_bool() {
                let elapsed = GetTickCount().wrapping_sub(info.dwTime);
                // Future/nonmonotonic SendInput tick values must not fabricate a
                // multi-day idle interval. Existing suppression survives unknowns
                (elapsed <= i32::MAX as u32).then(|| Duration::from_millis(u64::from(elapsed)))
            } else {
                None
            }
        }
    } else {
        None
    };
    let locked = if lock {
        unsafe {
            let mut buffer = windows::core::PWSTR::null();
            let mut bytes = 0;
            if WTSQuerySessionInformationW(
                None,
                u32::MAX,
                WTSSessionInfoEx,
                &mut buffer,
                &mut bytes,
            )
            .is_ok()
                && !buffer.is_null()
            {
                let result = if bytes as usize >= std::mem::size_of::<WTSINFOEXW>() {
                    let value = (buffer.0 as *const WTSINFOEXW).read_unaligned();
                    if value.Level == 1 {
                        match value.Data.WTSInfoExLevel1.SessionFlags as u32 {
                            WTS_SESSIONSTATE_LOCK => Some(true),
                            WTS_SESSIONSTATE_UNLOCK => Some(false),
                            _ => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                WTSFreeMemory(buffer.0.cast());
                result
            } else {
                None
            }
        }
    } else {
        None
    };
    SensorSample { idle, locked }
}
#[cfg(target_os = "linux")]
// Report the command available condition for native
fn command_available(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(name).is_file())
    })
}
#[cfg(target_os = "linux")]
// Read and validate the session identifier supplied by the desktop environment
fn session_id() -> Option<String> {
    std::env::var("XDG_SESSION_ID").ok().filter(|value| {
        !value.is_empty()
            && value.len() <= 64
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    })
}
#[cfg(target_os = "linux")]
// Describe supported native capabilities
pub fn capabilities() -> PowerPlatformCapabilities {
    let x11 = std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "x11")
        || (std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_some());
    PowerPlatformCapabilities {
        idle: x11 && command_available("xprintidle"),
        lock: session_id().is_some() && command_available("loginctl"),
        idle_description: "X11 requires xprintidle; Wayland global input idle is unavailable"
            .into(),
        lock_description:
            "Requires loginctl, XDG_SESSION_ID and a desktop that maintains LockedHint".into(),
    }
}
#[cfg(target_os = "linux")]
// Collect a native sample
pub fn sample(idle: bool, lock: bool) -> SensorSample {
    use stream_dock_manager_plugin_host::preflight::probe;
    let support = capabilities();
    let idle = if idle && support.idle {
        probe("xprintidle", &[])
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .map(|millis| Duration::from_millis(u64::from(millis)))
    } else {
        None
    };
    let locked = if lock && support.lock {
        session_id()
            .and_then(|id| {
                probe(
                    "loginctl",
                    &[
                        "show-session",
                        &id,
                        "--property=LockedHint",
                        "--value",
                        "--no-pager",
                    ],
                )
                .ok()
            })
            .and_then(|text| match text.trim() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            })
    } else {
        None
    };
    SensorSample { idle, locked }
}
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
// Describe supported native capabilities
pub fn capabilities() -> PowerPlatformCapabilities {
    PowerPlatformCapabilities {
        idle: false,
        lock: false,
        idle_description: "System input idle unavailable".into(),
        lock_description: "Session lock state unavailable".into(),
    }
}
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
// Collect a native sample
pub fn sample(_: bool, _: bool) -> SensorSample {
    SensorSample::default()
}
