//! Ephemeral, connection-scoped keyboard lighting previews. Never persisted
use crate::{
    keyboard::{KeyboardApplyResult, KeyboardApplyStatus, TransferError},
    Runtime,
};
use hardware_core::KeyboardSettings;
use std::{
    collections::HashMap,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

const LEASE: Duration = Duration::from_secs(300);
#[derive(Default)]
pub(crate) struct Previews {
    leases: HashMap<String, Lease>,
}
impl Previews {
    // Discard the selected stale state
    pub(crate) fn discard_stale(&mut self, id: &str, epoch: Option<u64>) {
        if self.leases.get(id).is_some_and(|l| Some(l.epoch) != epoch) {
            self.leases.remove(id);
        }
    }
    // Report the contains condition for keyboard preview
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.leases.contains_key(id)
    }
}
#[derive(Clone)]
struct Lease {
    token: String,
    epoch: u64,
    contract: String,
    baseline: KeyboardSettings,
    saved: Option<KeyboardSettings>,
    background_touched: bool,
    rollback_attempted: bool,
    until: Instant,
}
#[derive(serde::Serialize, ts_rs::TS)]
pub struct KeyboardPreviewResult {
    pub preview_id: String,
    pub status: KeyboardApplyStatus,
    pub message: String,
}
// Strip non-previewable keyboard settings and make custom-color rollback explicit
fn lighting(mut settings: KeyboardSettings) -> KeyboardSettings {
    settings.os_mode = None;
    settings.sleep_timeout_seconds = None;
    // Older saved RGB settings always used custom mode; make that rollback
    // explicit after previewing rainbow mode
    if settings.custom_color.is_none() && settings.color.is_some() {
        settings.custom_color = Some(true);
    }
    settings
}
// Check baseline
fn check_baseline(baseline: &KeyboardSettings, settings: &KeyboardSettings) -> Result<(), String> {
    if baseline.effect.is_none()
        || baseline.speed.is_none()
        || (baseline.color.is_none() && baseline.custom_color != Some(false))
    {
        return Err("Apply an effect, speed and color once before live preview so Manager can restore your saved lighting.".into());
    }
    if settings.brightness.is_some() && baseline.brightness.is_none() {
        return Err("Apply keyboard brightness once before previewing brightness changes.".into());
    }
    if settings.background_color.is_some() && baseline.background_color.is_none() {
        return Err(
            "Apply a Ripple background color once before previewing background changes.".into(),
        );
    }
    Ok(())
}
// Receive transfer
fn receive_transfer(
    receive: mpsc::Receiver<Result<(), TransferError>>,
    timeout: Duration,
) -> Result<KeyboardApplyStatus, TransferError> {
    receive.recv_timeout(timeout).map_err(|_| {
        TransferError::Uncertain(
            "Preview transfer outcome unknown; check the keyboard before retrying".into(),
        )
    })??;
    Ok(KeyboardApplyStatus::Transmitted)
}
// Complete restore
fn complete_restore(
    previews: &mut Previews,
    id: &str,
    result: Result<KeyboardApplyStatus, TransferError>,
) -> (KeyboardApplyStatus, String) {
    match result {
        Ok(status) => {
            previews.leases.remove(id);
            (
                status,
                "Restored saved lighting; no preferences changed. Device state is not read back."
                    .into(),
            )
        }
        Err(TransferError::NotSubmitted(error)) => (
            KeyboardApplyStatus::Failed,
            format!("Preview restoration was not submitted: {error}"),
        ),
        Err(error) => (
            KeyboardApplyStatus::Uncertain,
            format!(
                "Preview restoration may be partial: {error}. Check the keyboard before retrying."
            ),
        ),
    }
}
impl Runtime {
    // Transfer keyboard preview using the current keyboard preview state
    fn transfer_keyboard_preview(
        &self,
        id: &str,
        epoch: u64,
        key: &str,
        settings: KeyboardSettings,
    ) -> Result<KeyboardApplyStatus, TransferError> {
        let crate::keyboard::PreparedKeyboardTransfer {
            device,
            transaction,
            ..
        } = self
            .prepare_keyboard_transfer(id, &settings, Some((epoch, key)))
            .map_err(TransferError::NotSubmitted)?;
        if device.simulated {
            return Ok(KeyboardApplyStatus::Simulated);
        }
        let (reply, receive) = mpsc::channel();
        self.device_commands
            .send(crate::devices::Request::Keyboard {
                device: id.into(),
                epoch,
                contract_key: key.into(),
                transaction,
                deadline: Instant::now() + Duration::from_secs(30),
                reply,
            })
            .map_err(|e| TransferError::NotSubmitted(e.to_string()))?;
        receive_transfer(receive, Duration::from_secs(35))
    }
    // Apply keyboard settings for preview without saving the profile
    pub fn preview_keyboard_settings(
        self: &Arc<Self>,
        id: &str,
        token: Option<&str>,
        settings: KeyboardSettings,
    ) -> Result<KeyboardPreviewResult, String> {
        if settings.sleep_timeout_seconds.is_some() {
            return Err("Sleep timer is applied only with Apply, never previewed".into());
        }
        if settings.os_mode.is_some() {
            return Err("OS mode is applied only with Apply, never previewed".into());
        }
        if settings.is_empty() {
            return Err("Choose lighting settings to preview".into());
        }
        settings.validate_values().map_err(|e| e.to_string())?;
        if self.benchmark_progress().running {
            return Err("Wait for the benchmark to finish".into());
        }
        let mut previews = self.keyboard_previews.lock().unwrap();
        let epoch = self
            .connection_generation(id)
            .ok_or("Device disconnected")?;
        let device = self
            .snapshot()
            .devices
            .into_iter()
            .find(|d| d.id == id && d.connected)
            .ok_or("Device disconnected")?;
        let key = self.device_contract_key(&device)?;
        self.validate_keyboard_contract(id, epoch, &key)?;
        let saved = self.snapshot().session(id).output.keyboard;
        if previews
            .leases
            .get(id)
            .is_some_and(|l| l.epoch != epoch || l.contract != key)
        {
            previews.leases.remove(id);
        }
        let fresh = !previews.leases.contains_key(id);
        if fresh {
            if token.is_some() {
                return Err("Keyboard preview expired; reopen the settings".into());
            }
            let observed = self.keyboard_device_settings(id);
            let baseline = lighting(if observed.supported && !device.simulated {
                observed.settings.unwrap_or_default()
            } else {
                saved.clone().unwrap_or_default()
            });
            check_baseline(&baseline, &settings)?;
            previews.leases.insert(
                id.into(),
                Lease {
                    token: uuid::Uuid::new_v4().to_string(),
                    epoch,
                    contract: key.clone(),
                    baseline,
                    saved: saved.clone(),
                    background_touched: false,
                    rollback_attempted: false,
                    until: Instant::now() + LEASE,
                },
            );
        }
        let lease = previews.leases.get_mut(id).unwrap();
        if !fresh && token != Some(lease.token.as_str()) {
            return Err("Another keyboard preview is active".into());
        }
        if lease.epoch != epoch || lease.contract != key || lease.saved != saved {
            previews.leases.remove(id);
            return Err("Keyboard connection or saved settings changed; preview was discarded without writing to a different device".into());
        }
        if lease.rollback_attempted {
            return Err(
                "Restore or apply the previous preview before starting another preview".into(),
            );
        }
        check_baseline(&lease.baseline, &settings)?;
        lease.until = Instant::now() + LEASE;
        lease.background_touched |= settings.background_color.is_some();
        let token = lease.token.clone();
        let result = self.transfer_keyboard_preview(id, epoch, &key, settings);
        // Retain the lease even after uncertain/partial output so Cancel can restore it
        let (status, message) = match result {
            Ok(status) => (
                status,
                "Preview only. Cancel restores saved lighting; device state is not read back."
                    .into(),
            ),
            Err(TransferError::NotSubmitted(error)) => (KeyboardApplyStatus::Failed, error),
            Err(error) => (
                KeyboardApplyStatus::Uncertain,
                format!("{error}. Preview may be partial; use Cancel to restore saved lighting."),
            ),
        };
        drop(previews);
        if fresh {
            let weak = Arc::downgrade(self);
            let id = id.to_string();
            let expired = token.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(runtime) = weak.upgrade() else { break };
                let due = {
                    let previews = runtime.keyboard_previews.lock().unwrap();
                    let Some(lease) = previews.leases.get(&id).filter(|l| l.token == expired)
                    else {
                        break;
                    };
                    if lease.rollback_attempted {
                        break;
                    }
                    Instant::now() >= lease.until
                };
                if due {
                    let _ = runtime.finish_keyboard_preview(&id, &expired, false, None);
                    break;
                }
            });
        }
        Ok(KeyboardPreviewResult {
            preview_id: token,
            status,
            message,
        })
    }
    // Finalize keyboard preview
    pub fn finish_keyboard_preview(
        &self,
        id: &str,
        token: &str,
        apply: bool,
        settings: Option<KeyboardSettings>,
    ) -> Result<KeyboardApplyResult, String> {
        let mut previews = self.keyboard_previews.lock().unwrap();
        let Some(lease) = previews.leases.get(id).filter(|l| l.token == token) else {
            if apply {
                return Err("Keyboard preview expired; reopen settings before applying".into());
            }
            return Ok(KeyboardApplyResult {
                snapshot: self.snapshot(),
                status: KeyboardApplyStatus::Unchanged,
                saved: false,
                message: "Preview already ended. No device commands sent.".into(),
            });
        };
        if self.connection_generation(id) != Some(lease.epoch)
            || self.snapshot().session(id).output.keyboard != lease.saved
        {
            previews.leases.remove(id);
            return Err(
                "Keyboard connection or saved settings changed; no stale restoration was sent"
                    .into(),
            );
        }
        if apply {
            self.validate_keyboard_contract(id, lease.epoch, &lease.contract)?;
            let patch = settings.ok_or("Choose settings to apply")?;
            let mut complete = Some(lease.baseline.clone());
            crate::keyboard::merge(&mut complete, &patch);
            let result = self.apply_keyboard_settings_inner(id, complete.unwrap())?;
            previews.leases.remove(id);
            return Ok(result);
        }
        previews.leases.get_mut(id).unwrap().rollback_attempted = true;
        let lease = previews.leases.get(id).unwrap().clone();
        let mut restore = lease.baseline.clone();
        // BASECO belongs to Ripple 14. Restore its remembered color first, then
        // the original effect, never inventing an unknown background or OS mode
        let result = (|| {
            if lease.background_touched && restore.effect != Some(14) {
                self.transfer_keyboard_preview(
                    id,
                    lease.epoch,
                    &lease.contract,
                    KeyboardSettings {
                        background_color: restore.background_color,
                        ..Default::default()
                    },
                )?;
            }
            if restore.effect != Some(14) {
                restore.background_color = None;
            }
            self.transfer_keyboard_preview(id, lease.epoch, &lease.contract, restore)
        })();
        let (status, message) = complete_restore(&mut previews, id, result);
        self.emit(
            "device-output",
            Some(id.into()),
            None,
            Some(message.clone()),
        );
        Ok(KeyboardApplyResult {
            snapshot: self.snapshot(),
            status,
            saved: false,
            message,
        })
    }
    // Cancel all keyboard previews
    pub fn cancel_all_keyboard_previews(&self) {
        let leases: Vec<_> = self
            .keyboard_previews
            .lock()
            .unwrap()
            .leases
            .iter()
            .filter(|(_, l)| !l.rollback_attempted)
            .map(|(id, l)| (id.clone(), l.token.clone()))
            .collect();
        for (id, token) in leases {
            let _ = self.finish_keyboard_preview(&id, &token, false, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Build a known keyboard-settings fixture with lighting and sleep values
    fn known() -> KeyboardSettings {
        KeyboardSettings {
            brightness: Some(75),
            color: Some([12, 34, 56]),
            effect: Some(2),
            speed: Some(3),
            sleep_timeout_seconds: Some(300),
            ..Default::default()
        }
    }
    #[test]
    // Verify that custom mode rollback preserves old RGB and explicit rainbow
    fn custom_mode_rollback_preserves_old_rgb_and_explicit_rainbow() {
        let old = KeyboardSettings {
            effect: Some(2),
            speed: Some(3),
            color: Some([1, 2, 3]),
            ..Default::default()
        };
        let baseline = lighting(old);
        assert_eq!(baseline.custom_color, Some(true));
        let mut with_timer = known();
        with_timer.os_mode = Some(hardware_core::KeyboardOsMode::Macos);
        let lighting_only = lighting(with_timer);
        assert_eq!(lighting_only.sleep_timeout_seconds, None);
        assert_eq!(lighting_only.os_mode, None);
        let rainbow = KeyboardSettings {
            effect: Some(2),
            speed: Some(3),
            custom_color: Some(false),
            ..Default::default()
        };
        let baseline = lighting(rainbow);
        assert_eq!(baseline.custom_color, Some(false));
        assert!(baseline.color.is_none());
        assert!(check_baseline(
            &baseline,
            &KeyboardSettings {
                effect: Some(3),
                ..Default::default()
            }
        )
        .is_ok());
    }

    #[test]
    // Verify that unknown fields are never invented for rollback
    fn unknown_fields_are_never_invented_for_rollback() {
        let patch = KeyboardSettings {
            effect: Some(15),
            ..Default::default()
        };
        assert!(check_baseline(&KeyboardSettings::default(), &patch).is_err());
        assert!(check_baseline(&known(), &patch).is_ok());
        let mut baseline = known();
        baseline.brightness = None;
        assert!(check_baseline(&baseline, &patch).is_ok());
        assert!(check_baseline(
            &baseline,
            &KeyboardSettings {
                brightness: Some(1),
                ..Default::default()
            }
        )
        .is_err());
        assert!(check_baseline(
            &known(),
            &KeyboardSettings {
                background_color: Some(hardware_core::KeyboardBackgroundColor {
                    hue: 30,
                    saturation: 60
                }),
                ..Default::default()
            }
        )
        .is_err());
    }
    #[test]
    // Verify that failed and uncertain receipts keep rollback available until confirmed
    fn failed_and_uncertain_receipts_keep_rollback_available_until_confirmed() {
        let id = "test";
        let mut previews = Previews::default();
        previews.leases.insert(
            id.into(),
            Lease {
                token: "token".into(),
                epoch: 1,
                contract: "contract".into(),
                baseline: known(),
                saved: Some(known()),
                background_touched: false,
                rollback_attempted: false,
                until: Instant::now(),
            },
        );
        for error in [
            TransferError::NotSubmitted("busy".into()),
            TransferError::Uncertain("partial write".into()),
        ] {
            let (send, receive) = mpsc::channel();
            send.send(Err(error)).unwrap();
            let result = receive_transfer(receive, Duration::ZERO);
            let (status, _) = complete_restore(&mut previews, id, result);
            assert!(matches!(
                status,
                KeyboardApplyStatus::Failed | KeyboardApplyStatus::Uncertain
            ));
            assert!(previews.contains(id));
        }
        let (_send, receive) = mpsc::channel();
        let (status, _) =
            complete_restore(&mut previews, id, receive_transfer(receive, Duration::ZERO));
        assert_eq!(status, KeyboardApplyStatus::Uncertain);
        assert!(previews.contains(id));
        let (send, receive) = mpsc::channel();
        drop(send);
        let (status, _) =
            complete_restore(&mut previews, id, receive_transfer(receive, Duration::ZERO));
        assert_eq!(status, KeyboardApplyStatus::Uncertain);
        assert!(previews.contains(id));
        let (send, receive) = mpsc::channel();
        send.send(Ok(())).unwrap();
        let (status, _) =
            complete_restore(&mut previews, id, receive_transfer(receive, Duration::ZERO));
        assert_eq!(status, KeyboardApplyStatus::Transmitted);
        assert!(!previews.contains(id));
    }
    #[test]
    #[ignore = "Requires disposable local loopback plugin host; never opens HID"]
    // Verify that simulator preview cancel commit and stale guards
    fn simulator_preview_cancel_commit_and_stale_guards() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:k1-pro";
        runtime.simulate_device(true, id).unwrap();
        let patch = KeyboardSettings {
            effect: Some(15),
            ..Default::default()
        };
        assert!(runtime
            .preview_keyboard_settings(id, None, patch.clone())
            .is_err());
        runtime.apply_keyboard_settings(id, known()).unwrap();
        let original = runtime.snapshot().session(id).output.keyboard;
        let lease = runtime
            .preview_keyboard_settings(id, None, patch.clone())
            .unwrap();
        assert_eq!(lease.status, KeyboardApplyStatus::Simulated);
        assert_eq!(runtime.snapshot().session(id).output.keyboard, original);
        assert!(runtime
            .preview_keyboard_settings(id, None, patch.clone())
            .is_err());
        assert!(runtime.apply_keyboard_settings(id, patch.clone()).is_err());
        assert!(runtime
            .preview_keyboard_settings(
                id,
                Some(&lease.preview_id),
                KeyboardSettings {
                    os_mode: Some(hardware_core::KeyboardOsMode::Macos),
                    ..Default::default()
                }
            )
            .is_err());
        assert!(runtime
            .preview_keyboard_settings(
                id,
                Some(&lease.preview_id),
                KeyboardSettings {
                    sleep_timeout_seconds: Some(60),
                    ..Default::default()
                }
            )
            .err()
            .unwrap()
            .contains("never previewed"));
        let wrong = runtime
            .finish_keyboard_preview(id, "wrong", false, None)
            .unwrap();
        assert_eq!(wrong.status, KeyboardApplyStatus::Unchanged);
        assert!(runtime.keyboard_previews.lock().unwrap().contains(id));
        let canceled = runtime
            .finish_keyboard_preview(id, &lease.preview_id, false, None)
            .unwrap();
        assert!(!canceled.saved);
        assert_eq!(canceled.snapshot.session(id).output.keyboard, original);
        assert_eq!(
            runtime
                .finish_keyboard_preview(id, &lease.preview_id, false, None)
                .unwrap()
                .status,
            KeyboardApplyStatus::Unchanged
        );
        let lease = runtime
            .preview_keyboard_settings(id, None, patch.clone())
            .unwrap();
        let applied = runtime
            .finish_keyboard_preview(
                id,
                &lease.preview_id,
                true,
                Some(KeyboardSettings {
                    sleep_timeout_seconds: Some(900),
                    ..patch.clone()
                }),
            )
            .unwrap();
        assert!(applied.saved);
        assert_eq!(
            applied
                .snapshot
                .session(id)
                .output
                .keyboard
                .as_ref()
                .unwrap()
                .sleep_timeout_seconds,
            Some(900)
        );
        assert_eq!(
            applied.snapshot.session(id).output.keyboard.unwrap().effect,
            Some(15)
        );
        let lease = runtime
            .preview_keyboard_settings(
                id,
                None,
                KeyboardSettings {
                    effect: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        runtime.simulate_device(false, id).unwrap();
        runtime.simulate_device(true, id).unwrap();
        assert!(runtime
            .finish_keyboard_preview(id, &lease.preview_id, false, None)
            .is_err());
        assert!(!runtime.keyboard_previews.lock().unwrap().contains(id));
        let lease = runtime.preview_keyboard_settings(id, None, patch).unwrap();
        runtime.set_device_sleep(id, true).unwrap();
        let result = runtime
            .finish_keyboard_preview(id, &lease.preview_id, false, None)
            .unwrap();
        assert_eq!(result.status, KeyboardApplyStatus::Failed);
        assert!(!result.saved);
        runtime.shutdown();
    }
}
