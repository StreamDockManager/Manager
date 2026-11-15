//! Explicit keyboard patches. Saved values are preferences, never automatic restoration
use crate::{Runtime, Snapshot};
use hardware_core::{DeviceCommand, KeyboardSettings, OutputStatus};
use std::{
    collections::BTreeMap,
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardApplyStatus {
    Unchanged,
    Simulated,
    Transmitted,
    Failed,
    Uncertain,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct KeyboardApplyResult {
    pub snapshot: Snapshot,
    pub status: KeyboardApplyStatus,
    pub saved: bool,
    pub message: String,
}
// Validate sessions
pub(crate) fn validate_sessions(
    sessions: &BTreeMap<String, crate::workspace::DeviceSession>,
) -> Result<(), String> {
    for session in sessions.values() {
        if let Some(settings) = &session.output.keyboard {
            settings.validate_values().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
// Merge the supplied keyboard values
pub(crate) fn merge(current: &mut Option<KeyboardSettings>, patch: &KeyboardSettings) {
    let current = current.get_or_insert_with(Default::default);
    if patch.brightness.is_some() {
        current.brightness = patch.brightness;
    }
    if patch.color.is_some() {
        current.color = patch.color;
    }
    if patch.custom_color.is_some() {
        current.custom_color = patch.custom_color;
    }
    if patch.background_color.is_some() {
        current.background_color = patch.background_color;
    }
    if patch.effect.is_some() {
        current.effect = patch.effect;
    }
    // Python's static-effect setter resets speed before selecting effect 0.
    // An explicitly supplied speed is a subsequent setter and takes precedence
    if patch.speed.is_some() {
        current.speed = patch.speed;
    } else if patch.effect == Some(0) {
        current.speed = Some(0);
    }
    if patch.sleep_timeout_seconds.is_some() {
        current.sleep_timeout_seconds = patch.sleep_timeout_seconds;
    }
    if patch.os_mode.is_some() {
        current.os_mode = patch.os_mode;
    }
}
// COLOR carries mode and RGB together. Resolve partial writes without changing
// saved preferences or silently re-enabling custom colors
fn color_transfer_settings(
    patch: &KeyboardSettings,
    saved: &KeyboardSettings,
) -> Result<KeyboardSettings, String> {
    let mut settings = patch.clone();
    if settings.color.is_some() || settings.custom_color.is_some() {
        let enabled = settings.custom_color.or(saved.custom_color).unwrap_or(true);
        settings.custom_color = Some(enabled);
        settings.color = settings.color.or(saved.color);
        // COLOR also carries HSV value; restore the requested brightness afterward
        settings.brightness = settings.brightness.or(saved.brightness);
        if settings.color.is_none() {
            if enabled {
                return Err("Choose a custom RGB color before enabling custom colors".into());
            }
            // Ignored in rainbow mode, never persisted as an invented RGB value
            settings.color = Some([255, 255, 255]);
        }
    }
    Ok(settings)
}

pub(crate) struct PreparedKeyboardTransfer {
    pub device: crate::DeviceInfo,
    pub epoch: u64,
    pub contract_key: String,
    pub transaction: hardware_core::DeviceTransaction,
}
impl Runtime {
    /// Preparation has no persistence or submission side effects. Preview's expected
    /// identity is checked first; Apply retains its capability-first validation order
    pub(crate) fn prepare_keyboard_transfer(
        &self,
        id: &str,
        settings: &KeyboardSettings,
        expected: Option<(u64, &str)>,
    ) -> Result<PreparedKeyboardTransfer, String> {
        if let Some((epoch, key)) = expected {
            self.validate_keyboard_contract(id, epoch, key)?;
        }
        let device = self
            .state
            .lock()
            .unwrap()
            .devices
            .iter()
            .find(|d| d.id == id && d.connected)
            .cloned()
            .ok_or(if expected.is_some() {
                "Device disconnected"
            } else {
                "Device is not connected"
            })?;
        let capability =
            device
                .manifest
                .capabilities
                .keyboard
                .as_ref()
                .ok_or(if expected.is_some() {
                    "Keyboard unavailable"
                } else {
                    "Keyboard settings are unavailable for this device"
                })?;
        settings
            .validate_for(capability)
            .map_err(|e| e.to_string())?;
        let observation = self.keyboard_device_settings(id);
        let baseline = if observation.supported && !device.simulated {
            observation.settings.unwrap_or_default()
        } else {
            self.snapshot()
                .session(id)
                .output
                .keyboard
                .unwrap_or_default()
        };
        let settings = color_transfer_settings(settings, &baseline)?;
        let (epoch, contract_key) = if let Some((epoch, key)) = expected {
            (epoch, key.to_owned())
        } else {
            let epoch = self
                .connection_generation(id)
                .ok_or("Device connection changed")?;
            let key = self.device_contract_key(&device)?;
            self.validate_keyboard_contract(id, epoch, &key)?;
            (epoch, key)
        };
        let module = hardware_registry::modules()
            .into_iter()
            .find(|m| m.manifest().model_id == device.model_id)
            .ok_or("Hardware module unavailable")?;
        let contract = crate::device_contract::resolve(&device)?;
        let transaction = module
            .session_for(&contract)
            .map_err(|e| e.to_string())?
            .encode(DeviceCommand::Keyboard(settings.clone()))
            .map_err(|e| e.to_string())?;
        Ok(PreparedKeyboardTransfer {
            device,
            epoch,
            contract_key,
            transaction,
        })
    }
    // Validate keyboard contract
    pub(crate) fn validate_keyboard_contract(
        &self,
        id: &str,
        epoch: u64,
        expected: &str,
    ) -> Result<(), String> {
        if self.connection_generation(id) != Some(epoch) {
            return Err("Keyboard connection changed".into());
        }
        let state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|d| d.id == id && d.connected)
            .cloned()
            .ok_or("Keyboard disconnected")?;
        let output = state.session(id).output;
        drop(state);
        if self.power.effective(id, epoch, &output).sleeping {
            return Err("Wake the device before applying keyboard settings".into());
        }
        if self.device_contract_key(&device)? != expected {
            return Err("Keyboard hardware contract changed".into());
        }
        Ok(())
    }
    // Apply keyboard settings
    pub fn apply_keyboard_settings(
        &self,
        device_id: &str,
        settings: KeyboardSettings,
    ) -> Result<KeyboardApplyResult, String> {
        let mut previews = self.keyboard_previews.lock().unwrap();
        previews.discard_stale(device_id, self.connection_generation(device_id));
        if previews.contains(device_id) {
            return Err("Finish or cancel the keyboard preview before applying settings".into());
        }
        self.apply_keyboard_settings_inner(device_id, settings)
    }
    // Apply keyboard settings inner
    pub(crate) fn apply_keyboard_settings_inner(
        &self,
        device_id: &str,
        settings: KeyboardSettings,
    ) -> Result<KeyboardApplyResult, String> {
        if settings.is_empty() {
            return Err("Choose at least one keyboard setting".into());
        }
        settings.validate_values().map_err(|e| e.to_string())?;
        if self.benchmark_progress().running {
            return Err(
                "Wait for the benchmark to finish before applying keyboard settings".into(),
            );
        }
        let PreparedKeyboardTransfer {
            device,
            epoch,
            contract_key,
            transaction,
        } = self.prepare_keyboard_transfer(device_id, &settings, None)?;
        // Epoch lock precedes state lock, matching simulator retirement/navigation
        {
            let epochs = self.connection_epochs.lock().unwrap();
            if epochs.get(device_id) != Some(&epoch) {
                return Err("Device connection changed".into());
            }
            let mut state = self.state.lock().unwrap();
            let current = state
                .devices
                .iter()
                .find(|d| d.id == device_id && d.connected)
                .ok_or("Device disconnected")?;
            if current.persistence_key != device.persistence_key
                || current.model_id != device.model_id
            {
                return Err("Device changed".into());
            }
            let mut session = state.session(device_id);
            if self
                .power
                .effective(device_id, epoch, &session.output)
                .sleeping
            {
                return Err("Wake the device before applying keyboard settings".into());
            }
            merge(&mut session.output.keyboard, &settings);
            let mut sessions = state.sessions.clone();
            sessions.insert(device.persistence_key.clone(), session);
            self.profiles.write_preference(
                crate::profile_database::LocalPreference::Sessions,
                &sessions,
            )?;
            state.sessions = sessions;
        }
        let (status, message) = if device.simulated {
            (
                KeyboardApplyStatus::Simulated,
                "Saved app preference and updated simulator. No hardware writes.".into(),
            )
        } else {
            let (reply, receive) = mpsc::channel();
            let deadline = Instant::now() + Duration::from_secs(30);
            match self.device_commands.send(crate::devices::Request::Keyboard { device: device_id.into(), epoch, contract_key, transaction, deadline, reply }) {
                Err(error) => (KeyboardApplyStatus::Failed, format!("App preference saved; transfer was not submitted: {error}")),
                Ok(()) => match receive.recv_timeout(Duration::from_secs(35)) {
                    Ok(Ok(())) => (KeyboardApplyStatus::Transmitted, "App preference saved; host transfer completed. Device state and firmware persistence are not verified.".into()),
                    Ok(Err(TransferError::NotSubmitted(error))) => (KeyboardApplyStatus::Failed, format!("App preference saved; transfer was not submitted: {error}")),
                    Ok(Err(error)) => (KeyboardApplyStatus::Uncertain, format!("App preference saved; {error}. Transfer may be partial. Check the device before retrying; no automatic retry.")),
                    Err(_) => (KeyboardApplyStatus::Uncertain, "App preference saved; keyboard transfer status unknown. Check the device before retrying; no automatic retry.".into()),
                }
            }
        };
        if !device.simulated {
            self.refresh_keyboard_settings(device_id);
        }
        self.emit("device-output", Some(device_id.into()), None, None);
        Ok(KeyboardApplyResult {
            snapshot: self.snapshot(),
            status,
            saved: true,
            message,
        })
    }
}
#[derive(Debug)]
pub(crate) enum TransferError {
    NotSubmitted(String),
    Uncertain(String),
}
impl From<String> for TransferError {
    // Convert the input into the keyboard representation
    fn from(message: String) -> Self {
        Self::Uncertain(message)
    }
}
impl From<&str> for TransferError {
    // Convert the input into the keyboard representation
    fn from(message: &str) -> Self {
        Self::Uncertain(message.into())
    }
}
impl std::fmt::Display for TransferError {
    // Format keyboard for the requested formatter
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSubmitted(message) | Self::Uncertain(message) => f.write_str(message),
        }
    }
}
pub(crate) struct PendingApply {
    pub receipt: mpsc::Receiver<OutputStatus>,
    pub reply: Option<mpsc::Sender<Result<(), TransferError>>>,
    pub deadline: Instant,
}
impl PendingApply {
    // Poll keyboard for pending work
    pub fn poll(&mut self, failure: Option<String>) -> bool {
        crate::transfer_receipt::poll(
            &self.receipt,
            &mut self.reply,
            self.deadline,
            |status| match status {
                OutputStatus::Completed => Ok(()),
                status => Err(format!(
                    "Keyboard transfer {status:?}: {}",
                    failure.unwrap_or_default()
                )
                .into()),
            },
            || "Keyboard connection changed during transfer".into(),
            || "Keyboard transfer timed out".into(),
        )
    }
}
impl Drop for PendingApply {
    // Release this keyboard owner when it leaves scope
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Err("Keyboard connection closed during transfer".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hardware_core::KeyboardOsMode;
    #[test]
    // Verify that custom color mode retains RGB and resolves partial writes
    fn custom_color_mode_retains_rgb_and_resolves_partial_writes() {
        let mut saved = Some(KeyboardSettings {
            color: Some([20, 40, 60]),
            brightness: Some(70),
            ..Default::default()
        });
        let off = KeyboardSettings {
            custom_color: Some(false),
            ..Default::default()
        };
        let transfer = color_transfer_settings(&off, saved.as_ref().unwrap()).unwrap();
        assert_eq!(transfer.color, Some([20, 40, 60]));
        assert_eq!(transfer.custom_color, Some(false));
        assert_eq!(transfer.brightness, Some(70));
        assert_eq!(
            color_transfer_settings(
                &KeyboardSettings {
                    brightness: Some(25),
                    ..off.clone()
                },
                saved.as_ref().unwrap()
            )
            .unwrap()
            .brightness,
            Some(25)
        );
        assert_eq!(
            color_transfer_settings(&off, &KeyboardSettings::default())
                .unwrap()
                .brightness,
            None
        );
        merge(&mut saved, &off);
        assert_eq!(saved.as_ref().unwrap().color, Some([20, 40, 60]));
        let color = KeyboardSettings {
            color: Some([30, 50, 70]),
            ..Default::default()
        };
        assert_eq!(
            color_transfer_settings(&color, saved.as_ref().unwrap())
                .unwrap()
                .custom_color,
            Some(false)
        );
        let on = KeyboardSettings {
            custom_color: Some(true),
            ..Default::default()
        };
        assert_eq!(
            color_transfer_settings(&on, saved.as_ref().unwrap())
                .unwrap()
                .color,
            Some([20, 40, 60])
        );
        assert!(color_transfer_settings(&on, &KeyboardSettings::default()).is_err());
        assert!(color_transfer_settings(&off, &KeyboardSettings::default()).is_ok());
        assert_eq!(
            serde_json::from_value::<KeyboardSettings>(
                serde_json::to_value(saved.unwrap()).unwrap()
            )
            .unwrap()
            .custom_color,
            Some(false)
        );
    }

    #[test]
    // Verify that omitted keyboard stays absent and invalid restore is rejected
    fn omitted_keyboard_stays_absent_and_invalid_restore_is_rejected() {
        assert!(serde_json::from_str::<crate::workspace::DeviceSession>("{}").is_err());
        let session: crate::workspace::DeviceSession = serde_json::from_value(
            serde_json::to_value(crate::workspace::DeviceSession::default()).unwrap(),
        )
        .unwrap();
        assert!(session.output.keyboard.is_none());
        assert!(serde_json::to_value(&session).unwrap()["output"]
            .get("keyboard")
            .is_none());
        let mut sessions = BTreeMap::from([("k1".into(), session)]);
        validate_sessions(&sessions).unwrap();
        sessions.get_mut("k1").unwrap().output.keyboard = Some(KeyboardSettings {
            brightness: Some(101),
            ..Default::default()
        });
        assert!(validate_sessions(&sessions).is_err());
        assert!(serde_json::from_str::<KeyboardSettings>(r#"{"unknown":1}"#).is_err());
        assert!(serde_json::from_str::<KeyboardSettings>(r#"{"os_mode":"linux"}"#).is_err());
    }
    #[test]
    // Verify that patch does not invent or reset omitted fields
    fn patch_does_not_invent_or_reset_omitted_fields() {
        let mut saved = None;
        merge(
            &mut saved,
            &KeyboardSettings {
                color: Some([1, 2, 3]),
                ..Default::default()
            },
        );
        assert_eq!(saved.as_ref().unwrap().brightness, None);
        assert_eq!(saved.as_ref().unwrap().os_mode, None);
        merge(
            &mut saved,
            &KeyboardSettings {
                brightness: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(saved.as_ref().unwrap().color, Some([1, 2, 3]));
        assert_eq!(saved.as_ref().unwrap().brightness, Some(0));
    }
    #[test]
    // Verify that effect speed patches merge independently and validate restored values
    fn effect_speed_patches_merge_independently_and_validate_restored_values() {
        let mut saved = Some(KeyboardSettings {
            effect: Some(4),
            speed: Some(7),
            color: Some([1, 2, 3]),
            ..Default::default()
        });
        merge(
            &mut saved,
            &KeyboardSettings {
                effect: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(saved.as_ref().unwrap().effect, Some(0));
        assert_eq!(saved.as_ref().unwrap().speed, Some(0));
        merge(
            &mut saved,
            &KeyboardSettings {
                speed: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(saved.as_ref().unwrap().speed, Some(0));
        assert_eq!(saved.as_ref().unwrap().color, Some([1, 2, 3]));
        merge(
            &mut saved,
            &KeyboardSettings {
                effect: Some(0),
                speed: Some(5),
                ..Default::default()
            },
        );
        assert_eq!(saved.as_ref().unwrap().speed, Some(5));
        let session = crate::workspace::DeviceSession::default();
        let mut sessions = BTreeMap::from([("k1".into(), session)]);
        for invalid in [
            KeyboardSettings {
                effect: Some(16),
                ..Default::default()
            },
            KeyboardSettings {
                speed: Some(8),
                ..Default::default()
            },
        ] {
            sessions.get_mut("k1").unwrap().output.keyboard = Some(invalid);
            assert!(validate_sessions(&sessions).is_err());
        }
    }
    #[test]
    // Verify that automatic preference survives partial updates
    fn automatic_preference_survives_partial_updates() {
        let mut saved = None;
        merge(
            &mut saved,
            &KeyboardSettings {
                os_mode: Some(KeyboardOsMode::Auto),
                ..Default::default()
            },
        );
        merge(
            &mut saved,
            &KeyboardSettings {
                brightness: Some(2),
                ..Default::default()
            },
        );
        assert_eq!(saved.as_ref().unwrap().os_mode, Some(KeyboardOsMode::Auto));
        assert_eq!(serde_json::to_value(saved).unwrap()["os_mode"], "auto");
    }

    #[test]
    // Verify that tracked completion failure timeout and disconnect are not readback
    fn tracked_completion_failure_timeout_and_disconnect_are_not_readback() {
        for status in [
            OutputStatus::Completed,
            OutputStatus::Failed,
            OutputStatus::Invalidated,
            OutputStatus::Replaced,
        ] {
            let (send, receipt) = mpsc::channel();
            let (reply, result) = mpsc::channel();
            let mut pending = PendingApply {
                receipt,
                reply: Some(reply),
                deadline: Instant::now() + Duration::from_secs(1),
            };
            send.send(status).unwrap();
            assert!(pending.poll(Some("short HID write".into())));
            assert_eq!(
                result.recv().unwrap().is_ok(),
                status == OutputStatus::Completed
            );
        }
        let (send, receipt) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        let mut pending = PendingApply {
            receipt,
            reply: Some(reply),
            deadline: Instant::now(),
        };
        assert!(
            !pending.poll(None),
            "timeout must retain the transfer fence"
        );
        assert!(result
            .recv()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("timed out"));
        send.send(OutputStatus::Completed).unwrap();
        assert!(pending.poll(None));
        let (_send, receipt) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        drop(PendingApply {
            receipt,
            reply: Some(reply),
            deadline: Instant::now(),
        });
        assert!(result
            .recv()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("closed"));
    }
    #[test]
    #[ignore = "Requires local loopback sockets for the plugin host; never opens HID"]
    // Verify that simulator patch isolation persistence validation and generic setter
    fn simulator_patch_isolation_persistence_validation_and_generic_setter() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:k1-pro";
        let other = "simulator:k1-pro:other";
        runtime.simulate_device(true, id).unwrap();
        runtime.simulate_device(true, other).unwrap();
        runtime.simulate_device(true, "simulator:n4-pro").unwrap();
        let mut stale = runtime.snapshot().session(id).output;
        stale.brightness = 42;
        let patch = KeyboardSettings {
            brightness: Some(75),
            color: Some([7, 8, 9]),
            os_mode: Some(KeyboardOsMode::Auto),
            effect: Some(2),
            speed: Some(3),
            sleep_timeout_seconds: Some(300),
            ..Default::default()
        };
        let before = runtime.snapshot().session(id).output;
        let prepared = runtime.prepare_keyboard_transfer(id, &patch, None).unwrap();
        assert!(prepared.device.simulated);
        assert_eq!(runtime.snapshot().session(id).output, before);
        assert!(runtime
            .prepare_keyboard_transfer(
                id,
                &patch,
                Some((prepared.epoch + 1, &prepared.contract_key))
            )
            .is_err());
        assert!(runtime
            .prepare_keyboard_transfer(id, &patch, Some((prepared.epoch, "stale")))
            .is_err());
        assert_eq!(runtime.snapshot().session(id).output, before);
        let applied = runtime.apply_keyboard_settings(id, patch.clone()).unwrap();
        assert_eq!(applied.status, KeyboardApplyStatus::Simulated);
        assert!(applied.saved);
        assert!(applied.snapshot.session(other).output.keyboard.is_none());
        runtime.set_device_output(id, stale).unwrap();
        let output = runtime.snapshot().session(id).output;
        assert_eq!(output.keyboard, Some(patch.clone()));
        assert_eq!(output.brightness, 42);
        for invalid in [0, 600, 7201] {
            assert!(runtime
                .apply_keyboard_settings(
                    id,
                    KeyboardSettings {
                        sleep_timeout_seconds: Some(invalid),
                        ..Default::default()
                    }
                )
                .is_err());
            assert_eq!(
                runtime.snapshot().session(id).output.keyboard,
                Some(patch.clone())
            );
        }
        let never = runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    sleep_timeout_seconds: Some(u32::MAX),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(never.saved);
        assert_eq!(
            never
                .snapshot
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .sleep_timeout_seconds,
            Some(u32::MAX)
        );
        assert_eq!(
            never
                .snapshot
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .brightness,
            patch.brightness
        );
        runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    sleep_timeout_seconds: Some(300),
                    ..Default::default()
                },
            )
            .unwrap();
        let updated = runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    brightness: Some(80),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            updated
                .snapshot
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .os_mode,
            patch.os_mode
        );
        assert_eq!(updated.snapshot.session(id).output.brightness, 42);
        assert!(runtime
            .apply_keyboard_settings("simulator:n4-pro", patch.clone())
            .is_err());
        assert!(runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    brightness: Some(101),
                    ..Default::default()
                }
            )
            .is_err());
        let epoch = runtime.connection_generation(id).unwrap();
        let device = runtime
            .snapshot()
            .devices
            .into_iter()
            .find(|d| d.id == id)
            .unwrap();
        let key = runtime.device_contract_key(&device).unwrap();
        assert!(runtime
            .validate_keyboard_contract(id, epoch + 1, &key)
            .is_err());
        assert!(runtime
            .validate_keyboard_contract(id, epoch, "stale")
            .is_err());
        runtime
            .profiles
            .storage
            .run(|c| {
                c.execute_batch("PRAGMA query_only=ON")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        let before_failed = runtime.snapshot().session(id).output.keyboard;
        assert!(runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    sleep_timeout_seconds: Some(7200),
                    ..Default::default()
                }
            )
            .is_err());
        assert_eq!(
            runtime.snapshot().session(id).output.keyboard,
            before_failed
        );
        assert!(runtime.apply_keyboard_settings(id, patch).is_err());
        assert_eq!(
            runtime
                .snapshot()
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .brightness,
            Some(80)
        );
        runtime
            .profiles
            .storage
            .run(|c| {
                c.execute_batch("PRAGMA query_only=OFF")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        runtime.simulate_device(false, id).unwrap();
        assert!(runtime
            .apply_keyboard_settings(
                id,
                KeyboardSettings {
                    brightness: Some(2),
                    ..Default::default()
                }
            )
            .is_err());
        runtime.simulate_device(true, id).unwrap();
        assert_eq!(
            runtime
                .snapshot()
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .brightness,
            Some(80)
        );
        runtime.shutdown();
        drop(runtime);
        let rebuilt = Runtime::build(root.path().into(), false).unwrap();
        rebuilt.simulate_device(true, id).unwrap();
        assert_eq!(
            rebuilt
                .snapshot()
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .brightness,
            Some(80)
        );
        assert_eq!(
            rebuilt
                .snapshot()
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .os_mode,
            Some(KeyboardOsMode::Auto)
        );
        let saved = rebuilt.snapshot().session(id).output.keyboard.unwrap();
        assert_eq!(saved.effect, Some(2));
        assert_eq!(saved.speed, Some(3));
        assert_eq!(saved.sleep_timeout_seconds, Some(300));
        rebuilt.shutdown();
    }
}
