//! Connection-scoped device observations, distinct from saved preferences
use crate::{
    keyboard::{KeyboardApplyResult, KeyboardApplyStatus, TransferError},
    Runtime,
};
use hardware_core::{DeviceCommand, KeyboardSettings};
use std::{
    collections::HashMap,
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(crate) struct Observations(pub Mutex<HashMap<String, (u64, Option<KeyboardSettings>)>>);
#[derive(Clone, serde::Serialize, ts_rs::TS)]
pub struct KeyboardDeviceSettings {
    pub settings: Option<KeyboardSettings>,
    pub supported: bool,
    pub state: String,
    pub message: String,
}
impl Runtime {
    // Read the connected keyboard’s available settings
    pub fn keyboard_device_settings(&self, id: &str) -> KeyboardDeviceSettings {
        let snapshot = self.snapshot();
        let device = snapshot.devices.iter().find(|d| d.id == id && d.connected);
        let capable = device
            .and_then(|d| d.manifest.capabilities.keyboard.as_ref())
            .is_some_and(|k| k.settings_query);
        let epoch = self.connection_generation(id);
        let observation = self
            .keyboard_observations
            .0
            .lock()
            .unwrap()
            .get(id)
            .filter(|(generation, _)| Some(*generation) == epoch)
            .map(|(_, s)| s.clone());
        let simulated = device.is_some_and(|d| d.simulated);
        let supported = capable && (!simulated || observation.is_some());
        let settings = observation.clone().flatten();
        let state = if simulated && observation.is_some() {
            "simulated_reset"
        } else if !supported {
            "unsupported"
        } else if settings.is_some() {
            "observed"
        } else {
            "awaiting_readback"
        };
        KeyboardDeviceSettings { settings, supported, state: state.into(), message: match state {
            "simulated_reset" => "Simulator reset completed. Factory values are unknown; no physical keyboard was queried.",
            "observed" => "Read from the connected device. Unreported fields remain unknown.",
            "unsupported" => "Device settings readback is unavailable; values are saved app preferences, not device readings.",
            _ => "Waiting for device settings. Unreported fields remain unknown; saved preferences are not device readings.",
        }.into() }
    }
    // Observe keyboard settings
    pub(crate) fn observe_keyboard_settings(
        &self,
        id: &str,
        epoch: u64,
        settings: KeyboardSettings,
    ) {
        let epochs = self.connection_epochs.lock().unwrap();
        if epochs.get(id) != Some(&epoch) {
            return;
        }
        self.keyboard_observations
            .0
            .lock()
            .unwrap()
            .insert(id.into(), (epoch, Some(settings)));
        drop(epochs);
        self.emit("keyboard-device-settings", Some(id.into()), None, None);
    }
    // Forget the cached device observation without changing saved keyboard preferences
    pub(crate) fn forget_keyboard_settings(&self, id: &str) {
        self.keyboard_observations.0.lock().unwrap().remove(id);
    }
    // Refresh keyboard settings
    pub(crate) fn refresh_keyboard_settings(&self, id: &str) {
        if !self.keyboard_device_settings(id).supported {
            return;
        }
        self.forget_keyboard_settings(id);
        let _ = self.send_device_command(id, DeviceCommand::KeyboardSettingsQuery);
        self.emit("keyboard-device-settings", Some(id.into()), None, None);
    }
    // Reset the connected keyboard and refresh its reported settings
    pub fn factory_reset_keyboard(&self, id: &str) -> Result<KeyboardApplyResult, String> {
        let mut previews = self.keyboard_previews.lock().unwrap();
        previews.discard_stale(id, self.connection_generation(id));
        if previews.contains(id) {
            return Err("Cancel the keyboard preview before factory reset".into());
        }
        if self.benchmark_progress().running {
            return Err("Wait for the benchmark to finish".into());
        }
        let device = self
            .snapshot()
            .devices
            .into_iter()
            .find(|d| d.id == id && d.connected)
            .ok_or("Device disconnected")?;
        if !device
            .manifest
            .capabilities
            .keyboard
            .as_ref()
            .is_some_and(|k| k.factory_reset)
        {
            return Err("Factory reset is not supported for this device".into());
        }
        let epoch = self
            .connection_generation(id)
            .ok_or("Device disconnected")?;
        let key = self.device_contract_key(&device)?;
        self.validate_keyboard_contract(id, epoch, &key)?;
        let module = hardware_registry::modules()
            .into_iter()
            .find(|m| m.manifest().model_id == device.model_id)
            .ok_or("Hardware module unavailable")?;
        let contract = crate::device_contract::resolve(&device)?;
        let transaction = module
            .session_for(&contract)
            .map_err(|e| e.to_string())?
            .encode(DeviceCommand::KeyboardFactoryReset)
            .map_err(|e| e.to_string())?;
        self.forget_keyboard_settings(id);
        let status = if device.simulated {
            KeyboardApplyStatus::Simulated
        } else {
            let (reply, receive) = mpsc::channel();
            self.device_commands
                .send(crate::devices::Request::Keyboard {
                    device: id.into(),
                    epoch,
                    contract_key: key.clone(),
                    transaction,
                    deadline: Instant::now() + Duration::from_secs(30),
                    reply,
                })?;
            match receive.recv_timeout(Duration::from_secs(35)) {
                Ok(Ok(())) => KeyboardApplyStatus::Transmitted,
                Ok(Err(TransferError::NotSubmitted(error))) => return Err(error),
                Ok(Err(error)) => return Ok(KeyboardApplyResult { snapshot: self.snapshot(), status: KeyboardApplyStatus::Uncertain, saved: false, message: format!("Factory reset may be partial: {error}. Check the device; no automatic retry.") }),
                Err(_) => return Ok(KeyboardApplyResult { snapshot: self.snapshot(), status: KeyboardApplyStatus::Uncertain, saved: false, message: "Factory reset outcome unknown. Check the device; no automatic retry.".into() }),
            }
        };
        self.forget_keyboard_settings(id);
        // Clear stale requested keyboard values only, never profiles or bindings
        // Even a successful host transfer is not evidence of factory values
        let cleanup = (|| -> Result<(), String> {
            let epochs = self.connection_epochs.lock().unwrap();
            if epochs.get(id) != Some(&epoch) {
                return Err("device reconnected before saved preference cleanup".into());
            }
            let mut state = self.state.lock().unwrap();
            let mut session = state.session(id);
            session.output.keyboard = None;
            let mut sessions = state.sessions.clone();
            sessions.insert(device.persistence_key.clone(), session);
            self.profiles.write_preference(
                crate::profile_database::LocalPreference::Sessions,
                &sessions,
            )?;
            state.sessions = sessions;
            Ok(())
        })();
        if device.simulated {
            self.keyboard_observations
                .0
                .lock()
                .unwrap()
                .insert(id.into(), (epoch, None));
        }
        if !device.simulated && self.connection_generation(id) == Some(epoch) {
            self.refresh_keyboard_settings(id);
            let until = Instant::now() + Duration::from_secs(2);
            while Instant::now() < until
                && self.connection_generation(id) == Some(epoch)
                && self.keyboard_device_settings(id).settings.is_none()
            {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        drop(previews);
        self.emit("device-output", Some(id.into()), None, None);
        if let Err(error) = cleanup {
            return Ok(KeyboardApplyResult { snapshot: self.snapshot(), status, saved: false, message: format!("Factory reset was sent, but local preference cleanup failed: {error}. Reopen settings to read the device. Do not repeat the reset to fix this error.") });
        }
        Ok(KeyboardApplyResult { snapshot: self.snapshot(), status, saved: true, message: if device.simulated { "Simulator reset; factory hardware values are unknown. App profiles are unchanged." } else { "Vendor factory reset sent. App profiles are unchanged; only device-reported settings are shown. Check the device before retrying." }.into() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that observed settings are partial connection scoped and never saved preferences
    fn observed_settings_are_partial_connection_scoped_and_never_saved_preferences() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:k1-pro";
        runtime.simulate_device(true, id).unwrap();
        let before = runtime.snapshot().session(id).output.keyboard;
        let epoch = runtime.connection_generation(id).unwrap();
        runtime.observe_keyboard_settings(
            id,
            epoch,
            KeyboardSettings {
                brightness: Some(43),
                ..Default::default()
            },
        );
        let status = runtime.keyboard_device_settings(id);
        assert_eq!(status.settings.unwrap().brightness, Some(43));
        assert_eq!(runtime.snapshot().session(id).output.keyboard, before);
        runtime.observe_keyboard_settings(
            id,
            epoch + 1,
            KeyboardSettings {
                brightness: Some(88),
                ..Default::default()
            },
        );
        assert_eq!(
            runtime
                .keyboard_device_settings(id)
                .settings
                .unwrap()
                .brightness,
            Some(43)
        );
        runtime.simulate_device(false, id).unwrap();
        assert!(runtime.keyboard_device_settings(id).settings.is_none());
    }
    #[test]
    // Verify that transfer uses observed mode and brightness instead of stale preferences
    fn transfer_uses_observed_mode_and_brightness_instead_of_stale_preferences() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:k1-pro";
        runtime.simulate_device(true, id).unwrap();
        runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    custom_color: Some(false),
                    brightness: Some(10),
                    color: Some([0, 0, 0]),
                    ..Default::default()
                },
            )
            .unwrap();
        // Prepare only: synthetic physical metadata never submits a device command
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.simulated = false;
            device.variant_id = "usb-6603-1015".into();
        }
        runtime.observe_keyboard_settings(
            id,
            runtime.connection_generation(id).unwrap(),
            KeyboardSettings {
                custom_color: Some(true),
                brightness: Some(80),
                color: Some([1, 2, 3]),
                ..Default::default()
            },
        );
        let tx = runtime
            .prepare_keyboard_transfer(
                id,
                &KeyboardSettings {
                    color: Some([20, 40, 60]),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
            .transaction;
        let color = tx.reports.iter().find(|r| &r[6..11] == b"COLOR").unwrap();
        assert_eq!(&color[11..15], &[1, 20, 40, 60]);
        let brightness = tx.reports.iter().find(|r| &r[6..10] == b"LLUM").unwrap();
        assert_eq!(brightness[11], 80);
    }
    #[test]
    // Verify that simulated factory reset clears only keyboard preferences and invents no defaults
    fn simulated_factory_reset_clears_only_keyboard_preferences_and_invents_no_defaults() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:k1-pro";
        runtime.simulate_device(true, id).unwrap();
        runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    brightness: Some(50),
                    effect: Some(2),
                    ..Default::default()
                },
            )
            .unwrap();
        let before = runtime.snapshot();
        let result = runtime.factory_reset_keyboard(id).unwrap();
        assert_eq!(result.status, KeyboardApplyStatus::Simulated);
        assert!(result.snapshot.session(id).output.keyboard.is_none());
        assert_eq!(
            serde_json::to_value(&result.snapshot.profiles).unwrap(),
            serde_json::to_value(&before.profiles).unwrap()
        );
        assert_eq!(
            result.snapshot.session(id).output.brightness,
            before.session(id).output.brightness
        );
        let observed = runtime.keyboard_device_settings(id);
        assert_eq!(observed.state, "simulated_reset");
        assert!(observed.settings.is_none());
        runtime.simulate_device(true, "simulator:n4-pro").unwrap();
        assert!(runtime.factory_reset_keyboard("simulator:n4-pro").is_err());
    }
}
