//! K1's HAN command is not a reliable visible blackout on all firmware
//! Keep temporary overrides connection-local, never merge them into preferences
use super::*;

#[derive(Clone)]
struct Levels {
    display: u8,
    keyboard: Option<KeyboardSettings>,
}
struct Pending {
    receipt: mpsc::Receiver<OutputStatus>,
    deadline: Instant,
    action: &'static str,
}
#[derive(Default)]
pub(super) struct Blackout {
    restore: Option<Levels>,
    pending: Vec<Pending>,
}
impl Blackout {
    // Disconnect the owned device blackout resource
    pub(super) fn disconnect(&mut self, runtime: &Runtime, id: &str) {
        self.poll(runtime, id);
        if !self.pending.is_empty() || self.restore.is_some() {
            runtime.emit("device-error", Some(id.into()), None, Some("K1 disconnected during blackout/restoration. Brightness may remain changed; saved preferences are intact. Reapply keyboard brightness after reconnect if needed; no automatic keyboard replay.".into()));
        }
    }
    // Poll device blackout for pending work
    pub(super) fn poll(&mut self, runtime: &Runtime, id: &str) {
        self.pending.retain(|pending| {
            let message = match pending.receipt.try_recv() {
                Ok(OutputStatus::Completed) => return false,
                Ok(status) => format!("{} transfer {status:?}; device state may be partial. Check the device before retrying.", pending.action),
                Err(mpsc::TryRecvError::Disconnected) => format!("{} transfer interrupted; device state is unknown.", pending.action),
                Err(mpsc::TryRecvError::Empty) if Instant::now() < pending.deadline => return true,
                Err(mpsc::TryRecvError::Empty) => format!("{} transfer timed out; a host write may still finish. Device state is unknown; no automatic retry.", pending.action),
            };
            runtime.emit("device-error", Some(id.into()), None, Some(message));
            false
        });
    }
}

/// One FIFO, non-coalescing transaction keeps blackout/restoration ordered with
/// explicit keyboard changes. A wake queued behind sleep restores it even if
/// the sleep was still queued. Reconnect never replays a connection-local token
pub(super) fn send(c: &mut Connection, sleeping: bool) -> std::result::Result<(), String> {
    let runtime = c.runtime.upgrade().ok_or("Runtime stopped")?;
    c.blackout.poll(&runtime, &c.device_id);
    if sleeping && c.blackout.restore.is_some() {
        return Ok(()); // repeated restore/profile refresh is not another sleep
    }
    let levels = if sleeping {
        let state = runtime.state.lock().unwrap();
        let output = state.session(&c.device_id).output;
        Levels {
            display: output.brightness,
            keyboard: output.keyboard.map(|mut settings| {
                settings.os_mode = None; // Never switch OS mode during Wake
                settings.sleep_timeout_seconds = None; // Do not reset the firmware idle timer
                settings
            }),
        }
    } else if let Some(levels) = c.blackout.restore.clone() {
        levels
    } else {
        // Ordinary connect/wake must not replay keyboard preferences
        return send_plain_wake(c);
    };
    let mut commands = if sleeping {
        vec![DeviceCommand::Brightness(0)]
    } else {
        vec![
            DeviceCommand::Wake,
            DeviceCommand::Brightness(levels.display),
        ]
    };
    if sleeping {
        // Brightness zero and HAN do not blank every K1 firmware. Replace every
        // LCD surface with real black pixels, using the normal model encoder
        commands.extend(black_frames(c)?);
        if levels
            .keyboard
            .as_ref()
            .and_then(|v| v.brightness)
            .is_some()
        {
            commands.push(DeviceCommand::Keyboard(KeyboardSettings {
                brightness: Some(0),
                ..Default::default()
            }));
        }
        // Do not enter HAN's undocumented state after uploading black frames
        // Sleep is an app-managed visual blackout, not hardware suspend
    } else if let Some(settings) = levels.keyboard.clone().filter(|v| !v.is_empty()) {
        // DIS/brightness changes may reset firmware lighting state. Restore the
        // known visual settings in encoder order (effect, speed, custom color)
        commands.push(DeviceCommand::Keyboard(settings));
    }
    let mut transaction = DeviceTransaction {
        reports: vec![],
        coalesce_key: None,
    };
    for command in commands {
        transaction.reports.extend(
            c.session
                .lock()
                .unwrap()
                .encode(command)
                .map_err(|e| e.to_string())?
                .reports,
        );
    }
    let weak = Arc::downgrade(&runtime);
    let id = c.device_id.clone();
    let epoch = c.epoch;
    let identity = c.contract_identity.clone();
    let deadline = Instant::now() + Duration::from_secs(30);
    // Check only once before the group. Once a zero may have been written,
    // finish the ordered group rather than abandoning half of it on rapid Wake
    let receipt = c
        .worker
        .submit_tracked_guarded(
            c.worker.generation(),
            transaction,
            Box::new(move || {
                if Instant::now() >= deadline {
                    return false;
                }
                let Some(runtime) = weak.upgrade() else {
                    return false;
                };
                if runtime.connection_generation(&id) != Some(epoch) {
                    return false;
                }
                let device = runtime
                    .state
                    .lock()
                    .unwrap()
                    .devices
                    .iter()
                    .find(|d| d.id == id && d.connected)
                    .cloned();
                device.is_some_and(|device| {
                    crate::device_contract::resolve(&device)
                        .is_ok_and(|contract| contract.identity() == identity)
                })
            }),
        )
        .map_err(|e| format!("K1 blackout operation was not queued: {e}"))?;
    c.blackout.restore = sleeping.then_some(levels.clone());
    c.blackout.pending.push(Pending {
        receipt,
        deadline,
        action: if sleeping {
            "K1 blackout"
        } else {
            "K1 brightness restoration"
        },
    });
    if sleeping
        && levels
            .keyboard
            .as_ref()
            .and_then(|v| v.brightness)
            .is_none()
    {
        runtime.emit("device-error", Some(c.device_id.clone()), None, Some("LCD blackout requested. Keyboard brightness is unknown and was left unchanged; apply a keyboard brightness once to enable reversible keyboard blackout.".into()));
    }
    Ok(())
}
// Encode black images for every image-capable surface on the connected device
fn black_frames(c: &Connection) -> std::result::Result<Vec<DeviceCommand>, String> {
    c.manifest
        .controls
        .iter()
        .filter(|control| control.image_slot.is_some())
        .map(|control| {
            let width = control.image_width.ok_or("Missing LCD width")?;
            let height = control.image_height.ok_or("Missing LCD height")?;
            let mut data = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgb8(image::RgbImage::new(width.into(), height.into()))
                .write_to(&mut data, image::ImageFormat::Jpeg)
                .map_err(|error| error.to_string())?;
            Ok(DeviceCommand::Image {
                control_id: control.id.clone(),
                data: data.into_inner(),
            })
        })
        .collect()
}

// Send plain wake
fn send_plain_wake(c: &mut Connection) -> std::result::Result<(), String> {
    let transaction = c
        .session
        .lock()
        .unwrap()
        .encode(DeviceCommand::Wake)
        .map_err(|e| e.to_string())?;
    c.worker
        .submit(c.worker.generation(), transaction)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Extract drawable command prefixes while excluding setup and maintenance reports
    fn commands(reports: &[Vec<u8>]) -> Vec<Vec<u8>> {
        reports
            .iter()
            .filter(|r| r.get(1..6) == Some(b"CRT\0\0"))
            .map(|r| r[6..13].to_vec())
            .filter(|r| !r.starts_with(b"BAT") && !r.starts_with(b"LOG") && !r.starts_with(b"STP"))
            .collect()
    }
    #[test]
    // Verify that sleep uploads black pixels to every physical lcd without han
    fn sleep_uploads_black_pixels_to_every_physical_lcd_without_han() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) =
            super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        send(&mut c, true).unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        let reports = reports.lock().unwrap();
        let mut slots = Vec::new();
        for (index, report) in reports.iter().enumerate() {
            assert!(!report.get(6..9).is_some_and(|v| v == b"HAN"));
            if report.get(6..9) != Some(b"BAT") {
                continue;
            }
            slots.push(report[13]);
            let length = u32::from_be_bytes(report[9..13].try_into().unwrap()) as usize;
            let bytes: Vec<u8> = reports[index + 1..]
                .iter()
                .flat_map(|r| r[1..1024].iter().copied())
                .take(length)
                .collect();
            let pixels = image::load_from_memory(&bytes).unwrap().to_rgb8();
            assert_eq!(pixels.dimensions(), (64, 64));
            assert!(pixels.pixels().all(|pixel| pixel.0.iter().all(|v| *v == 0)));
        }
        assert_eq!(slots, [5, 3, 1, 6, 4, 2]);
        drop(reports);
        drop(c);
        runtime.shutdown();
    }
    #[test]
    // Verify that blackout and wake restore visual settings but never os mode
    fn blackout_and_wake_restore_visual_settings_but_never_os_mode() {
        for custom_color in [None, Some(false), Some(true)] {
            let root = tempfile::tempdir().unwrap();
            let runtime = Runtime::build(root.path().into(), false).unwrap();
            let (mut c, reports) =
                super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
            let settings = KeyboardSettings {
                custom_color,
                brightness: Some(80),
                color: Some([22, 33, 44]),
                effect: Some(2),
                speed: Some(3),
                os_mode: Some(KeyboardOsMode::Macos),
                sleep_timeout_seconds: Some(300),
                ..Default::default()
            };
            {
                let mut state = runtime.state.lock().unwrap();
                let output = &mut state
                    .sessions
                    .entry(c.device_id.clone())
                    .or_default()
                    .output;
                output.brightness = 72;
                output.keyboard = Some(settings.clone());
            }
            send(&mut c, true).unwrap();
            send(&mut c, true).unwrap(); // must not replace saved brightness with zero
            c.worker.invalidate_transient(); // profile changes preserve reversible FIFO work
            send(&mut c, false).unwrap(); // allowed before first transfer completion
            assert!(c.worker.wait_idle(Duration::from_secs(5)));
            let actual = commands(&reports.lock().unwrap());
            assert_eq!(actual.len(), 8);
            for (bytes, prefix) in actual.iter().zip([
                b"LIG\0\0\0".as_slice(),
                b"LLUM\0\0",
                b"DIS",
                b"LIG\0\0\x48",
                b"LMOD\0\x02",
                b"LSPD\0\x03",
                b"COLOR",
                b"LLUM\0\x50",
            ]) {
                assert!(bytes.starts_with(prefix), "{bytes:?} != {prefix:?}");
            }
            assert_eq!(actual[6][5], u8::from(custom_color.unwrap_or(true)));
            let color_report = reports
                .lock()
                .unwrap()
                .iter()
                .find(|report| report.get(6..11) == Some(b"COLOR"))
                .unwrap()
                .clone();
            assert_eq!(&color_report[12..15], &[22, 33, 44]);
            let saved = runtime.state.lock().unwrap().session(&c.device_id).output;
            assert_eq!(saved.brightness, 72);
            assert_eq!(saved.keyboard, Some(settings));
            c.blackout.poll(&runtime, &c.device_id);
            assert!(c.blackout.pending.is_empty());
            assert!(c.blackout.restore.is_none());
            let before = reports.lock().unwrap().len();
            send(&mut c, false).unwrap();
            assert!(c.worker.wait_idle(Duration::from_secs(5)));
            assert_eq!(reports.lock().unwrap().len(), before + 1); // ordinary Wake only
            drop(c);
            runtime.shutdown();
        }
    }
    #[test]
    // Verify that unknown keyboard level is not guessed and new connection has no restore token
    fn unknown_keyboard_level_is_not_guessed_and_new_connection_has_no_restore_token() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) =
            super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        send(&mut c, true).unwrap();
        send(&mut c, false).unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        let actual = commands(&reports.lock().unwrap());
        assert_eq!(actual.len(), 3);
        assert!(actual.iter().all(|r| !r.starts_with(b"LLUM")));
        drop(c);
        let (mut fresh, fresh_reports) =
            super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        send(&mut fresh, false).unwrap();
        assert!(fresh.worker.wait_idle(Duration::from_secs(5)));
        assert_eq!(fresh_reports.lock().unwrap().len(), 1);
        drop(fresh);
        runtime.shutdown();
    }
    #[test]
    // Verify that rejected queue does not create restore token
    fn rejected_queue_does_not_create_restore_token() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) =
            super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        c.worker.disconnect();
        assert!(send(&mut c, true).unwrap_err().contains("not queued"));
        assert!(c.blackout.restore.is_none());
        assert!(c.blackout.pending.is_empty());
        assert!(reports.lock().unwrap().is_empty());
        drop(c);
        runtime.shutdown();
    }
    #[test]
    // Verify that failed or partial transfer is reported without retry
    fn failed_or_partial_transfer_is_reported_without_retry() {
        struct FailAfterOne(usize);
        impl Transport for FailAfterOne {
            // Implement the fixture transport write operation
            fn write(&mut self, _: &[u8]) -> Result<()> {
                self.0 += 1;
                if self.0 > 1 {
                    Err(Error::Transport("fixture short write".into()))
                } else {
                    Ok(())
                }
            }
        }
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, _) = super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        c.worker = OutputWorker::spawn_with_policy(
            Box::new(FailAfterOne(0)),
            20,
            hardware_core::OutputPolicy::default(),
        )
        .unwrap();
        send(&mut c, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while c.worker.failure().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        c.blackout.poll(&runtime, &c.device_id);
        assert!(c.blackout.pending.is_empty());
        // A same-connection token is retained because the first write may have landed
        assert!(c.blackout.restore.is_some());
        send(&mut c, true).unwrap();
        assert!(c.blackout.pending.is_empty()); // no automatic retry
        drop(c);
        runtime.shutdown();
    }
    #[test]
    // Verify that zero levels restore as zero without substituting defaults
    fn zero_levels_restore_as_zero_without_substituting_defaults() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) =
            super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        {
            let mut state = runtime.state.lock().unwrap();
            let output = &mut state
                .sessions
                .entry(c.device_id.clone())
                .or_default()
                .output;
            output.brightness = 0;
            output.keyboard = Some(KeyboardSettings {
                brightness: Some(0),
                ..Default::default()
            });
        }
        send(&mut c, true).unwrap();
        send(&mut c, false).unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        let actual = commands(&reports.lock().unwrap());
        assert!(actual[3].starts_with(b"LIG\0\0\0"));
        assert!(actual[4].starts_with(b"LLUM\0\0"));
        drop(c);
        runtime.shutdown();
    }
    #[test]
    // Verify that stale epoch admission sends no blackout reports
    fn stale_epoch_admission_sends_no_blackout_reports() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) =
            super::super::frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(c.device_id.clone(), c.epoch + 1);
        send(&mut c, true).unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        assert!(reports.lock().unwrap().is_empty());
        c.blackout.poll(&runtime, &c.device_id);
        assert!(c.blackout.pending.is_empty());
        drop(c);
        runtime.shutdown();
    }
    #[test]
    // Verify that other models keep existing sleep protocol
    fn other_models_keep_existing_sleep_protocol() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) =
            super::super::frame_policy_tests::connection_for_model(&runtime, "n4-pro");
        super::super::send_resolved(&mut c, DeviceCommand::Sleep).unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        let actual = commands(&reports.lock().unwrap());
        assert_eq!(actual.len(), 1);
        assert!(actual[0].starts_with(b"HAN"));
        assert!(c.blackout.restore.is_none());
        drop(c);
        runtime.shutdown();
    }
}
