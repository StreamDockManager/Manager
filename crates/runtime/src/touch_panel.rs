//! Reserved device-local page and profile navigation
use crate::Runtime;
use hardware_core::{ControlEvent, Input, ModelManifest};

// Report the active control condition for touch panel
pub(crate) fn active_control(manifest: &ModelManifest, id: &str) -> bool {
    !manifest
        .capabilities
        .touch_panel
        .as_ref()
        .is_some_and(|panel| panel.gesture_control == id)
}

impl Runtime {
    // Handle panel gesture
    pub(crate) fn handle_panel_gesture(&self, device_id: &str, event: &ControlEvent) -> bool {
        let Input::Swipe { direction } = &event.input else {
            return false;
        };
        let state = self.snapshot();
        let Some(device) = state.devices.iter().find(|d| d.id == device_id) else {
            return false;
        };
        if !device
            .manifest
            .control(&event.control_id)
            .is_ok_and(|c| c.semantics.iter().any(|s| s == "swipe"))
        {
            return false;
        }
        let session = state.session(device_id);
        if self
            .effective_device_output(device_id)
            .is_ok_and(|o| o.sleeping)
        {
            return true;
        }
        let result = match direction.as_str() {
            "left" | "right" => {
                let Some(profile) = state.profiles.iter().find(|p| p.id == session.profile_id)
                else {
                    return true;
                };
                let Some(current) = profile.pages.iter().position(|p| p.id == session.page_id)
                else {
                    return true;
                };
                if profile.pages.len() < 2 {
                    return true;
                }
                let next = (current
                    + if direction == "left" {
                        1
                    } else {
                        profile.pages.len() - 1
                    })
                    % profile.pages.len();
                self.select_device_profile(
                    device_id,
                    &profile.id,
                    Some(&profile.pages[next].id),
                    false,
                )
                .map(|_| ())
            }
            "up" | "down" => {
                // Only enable physical directions backed by this model's descriptor
                if !device
                    .manifest
                    .capabilities
                    .touch_panel
                    .as_ref()
                    .is_some_and(|p| {
                        p.gesture_control == event.control_id
                            && p.swipe_directions.contains(direction)
                    })
                {
                    return true;
                }
                let profiles: Vec<_> = state
                    .profiles
                    .iter()
                    .filter(|profile| profile.model_id == device.model_id)
                    .collect();
                let Some(current) = profiles
                    .iter()
                    .position(|profile| profile.id == session.profile_id)
                else {
                    return true;
                };
                if profiles.len() < 2 {
                    return true;
                }
                let next = if direction == "up" {
                    (current + 1) % profiles.len()
                } else {
                    (current + profiles.len() - 1) % profiles.len()
                };
                self.select_device_profile(device_id, &profiles[next].id, None, true)
                    .map(|_| ())
            }
            _ => return true,
        };
        if let Err(error) = result {
            self.emit("action-error", Some(device_id.into()), None, Some(error));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires local loopback sockets; never opens HID"]
    // Verify that unbound page and profile navigation are device local and persisted
    fn unbound_page_and_profile_navigation_are_device_local_and_persisted() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let a = "simulator:touch-a";
        let b = "simulator:touch-b";
        runtime.simulate_device(true, a).unwrap();
        runtime.simulate_device(true, b).unwrap();
        let state = runtime.snapshot();
        let mut profile = state.profiles[0].clone();
        profile.pages[0].bindings.remove("touch-strip");
        let mut extra = profile.pages[0].clone();
        extra.id = "extra".into();
        profile.pages.push(extra);
        let first_id = profile.id.clone();
        let mut second = profile.clone();
        second.id = "second-panel-profile".into();
        let mut incompatible = profile.clone();
        incompatible.id = "other-model".into();
        incompatible.model_id = "n4".into();
        incompatible
            .pages
            .iter_mut()
            .for_each(|page| page.bindings.clear());
        let saved = runtime
            .save_profile(profile, state.configuration_revision)
            .unwrap();
        let saved = runtime
            .save_profile(incompatible, saved.configuration_revision)
            .unwrap();
        runtime
            .save_profile(second, saved.configuration_revision)
            .unwrap();
        let revision = runtime.snapshot().configuration_revision;
        let swipe = |direction: &str| ControlEvent {
            control_id: "touch-strip".into(),
            input: Input::Swipe {
                direction: direction.into(),
            },
        };
        assert!(runtime.handle_panel_gesture(a, &swipe("left")));
        assert_eq!(runtime.snapshot().session(a).page_id, "extra");
        assert_eq!(runtime.snapshot().session(b).page_id, "main");
        runtime.handle_panel_gesture(a, &swipe("left"));
        assert_eq!(runtime.snapshot().session(a).page_id, "main");
        runtime.handle_panel_gesture(a, &swipe("right"));
        assert_eq!(runtime.snapshot().session(a).page_id, "extra");
        runtime.handle_panel_gesture(a, &swipe("up"));
        assert_eq!(
            runtime.snapshot().session(a).profile_id,
            "second-panel-profile"
        );
        assert_eq!(runtime.snapshot().session(b).profile_id, first_id);
        runtime.handle_panel_gesture(a, &swipe("up"));
        assert_eq!(runtime.snapshot().session(a).profile_id, first_id);
        assert_eq!(runtime.snapshot().session(a).page_id, "extra");
        runtime.handle_panel_gesture(a, &swipe("down"));
        let session = runtime.snapshot().session(a);
        assert_eq!(session.profile_id, "second-panel-profile");
        assert_eq!(session.manual_profile, "second-panel-profile");

        assert_eq!(runtime.snapshot().configuration_revision, revision);
        let stopped = std::sync::Arc::downgrade(&runtime);
        runtime.shutdown();
        drop(runtime);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while stopped.strong_count() != 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(stopped.strong_count(), 0);
        let restarted = Runtime::build(root.path().into(), false).unwrap();
        restarted.simulate_device(true, a).unwrap();
        assert_eq!(
            restarted.snapshot().session(a).profile_id,
            "second-panel-profile"
        );
        restarted.shutdown();
    }

    #[test]
    // Verify that former touch panel modes are rejected
    fn former_touch_panel_modes_are_rejected() {
        let error = serde_json::from_value::<crate::workspace::DeviceSession>(
            serde_json::json!({"profile_id": "default", "touch_panel_modes": {"default": "control_bar"}}),
        ).unwrap_err();
        assert!(error.to_string().contains("touch_panel_modes"));
    }
}
