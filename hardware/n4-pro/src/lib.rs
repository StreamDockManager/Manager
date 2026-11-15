use hardware_core::*;
use hardware_protocols::{command, normalize, transfer};
use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

static MANIFEST: LazyLock<ModelManifest> = LazyLock::new(|| {
    ModelManifest::parse(include_str!("../device.toml")).expect("embedded N4 Pro manifest")
});
pub struct N4Pro;
impl HardwareModule for N4Pro {
    // Describe the evidence required to qualify this device adapter
    fn verification_plan(&self) -> &VerificationPlan {
        static PLAN: LazyLock<VerificationPlan> = LazyLock::new(|| {
            toml::from_str(include_str!("../verification/plan.toml"))
                .expect("embedded verification plan")
        });
        &PLAN
    }
    // Return the manifest for this implementation
    fn manifest(&self) -> &ModelManifest {
        &MANIFEST
    }
    // Create a device protocol session
    fn session(&self) -> Box<dyn HardwareSession> {
        Box::new(Session::default())
    }
}
// Safety bound on gaps between coordinate reports, not a measured firmware
// gesture-duration limit. Continuous movement can last longer than this bound.
const TOUCH_TRAJECTORY_MAX_GAP: Duration = Duration::from_secs(2);
struct TouchTrajectory {
    start_x: u16,
    latest_x: u16,
    start_y: Option<u16>,
    latest_y: u16,
    updated: Instant,
}
#[derive(Default)]
pub struct Session {
    touch: Option<TouchTrajectory>,
}
impl Session {
    // Finish touch
    fn finish_touch(&mut self) -> Option<ControlEvent> {
        let touch = self.touch.take()?;
        let dx = i32::from(touch.latest_x) - i32::from(touch.start_x);
        let dy = touch
            .start_y
            .map(|y| i32::from(touch.latest_y) - i32::from(y))
            .unwrap_or(0);
        let direction = if dx.abs() >= 24 && dx.abs() * 2 >= dy.abs() * 3 {
            if dx > 0 {
                "right"
            } else {
                "left"
            }
        } else if dy.abs() >= 24 && dy.abs() * 2 >= dx.abs() * 3 {
            if dy > 0 {
                "down"
            } else {
                "up"
            }
        } else {
            return None;
        };
        Some(ControlEvent {
            control_id: "touch-strip".into(),
            input: Input::Swipe {
                direction: direction.into(),
            },
        })
    }
    // Finish vertical
    fn finish_vertical(&mut self, code: u8) -> Option<ControlEvent> {
        let touch = self.touch.take()?;
        let first_y = touch.start_y?;
        let dx = i32::from(touch.latest_x) - i32::from(touch.start_x);
        let dy = i32::from(touch.latest_y) - i32::from(first_y);
        // A quick, physically captured B1 swipe had only one ARX point
        // These verified firmware completions supply vertical intent when
        // samples are sparse, but cannot override clear contrary motion.
        if dx.abs() >= 24 && dx.abs() * 3 > dy.abs() * 2 {
            return None;
        }
        let up = code == 0xb1;
        if dy.abs() >= 24 && (dy < 0) != up {
            return None;
        }
        Some(ControlEvent {
            control_id: "touch-strip".into(),
            input: Input::Swipe {
                direction: if up { "up" } else { "down" }.into(),
            },
        })
    }
    // Validate touch
    fn validate_touch(&mut self, x: u16, y: u16) -> Result<()> {
        // Raw coordinates use the 800 × 480 device plane, not preview geometry
        if x >= 800 || y >= 480 {
            self.touch = None;
            return Err(Error::Protocol("touch coordinate outside 800×480".into()));
        }
        Ok(())
    }
    // Decode at
    fn decode_at(&mut self, report: &[u8], now: Instant) -> Result<Vec<ControlEvent>> {
        if self.touch.as_ref().is_some_and(|touch| {
            now.saturating_duration_since(touch.updated) > TOUCH_TRAJECTORY_MAX_GAP
        }) {
            self.touch = None;
        }
        let data = normalize(report);
        if data.get(..3) != Some(b"ACK") {
            self.touch = None;
            return Ok(vec![]);
        }
        let event = if data.get(4..7) == Some(b"ARX") {
            if data.len() < 14 {
                self.touch = None;
                return Err(Error::Protocol("truncated touch report".into()));
            }
            let x = u16::from_be_bytes([data[10], data[11]]);
            let y = u16::from_be_bytes([data[12], data[13]]);
            self.validate_touch(x, y)?;
            if let Some(touch) = self.touch.as_mut() {
                touch.latest_x = x;
                touch.start_y.get_or_insert(y);
                touch.latest_y = y;
                touch.updated = now;
            }
            Some(ControlEvent {
                control_id: "touch-strip".into(),
                input: Input::Touch {
                    phase: "move".into(),
                    x,
                    y,
                },
            })
        } else {
            if data.get(5..7) != Some(b"OK") {
                self.touch = None;
                return Ok(vec![]);
            }
            if data.len() < 11 {
                self.touch = None;
                return Err(Error::Protocol("truncated input report".into()));
            }
            let code = data[9];
            let state = data[10];
            let rotary = [(0xa0, 0xa1), (0x50, 0x51), (0x90, 0x91), (0x70, 0x71)];
            if (1..=10).contains(&code) {
                match state {
                    1 => Some(ControlEvent {
                        control_id: format!("key-{code}"),
                        input: Input::Button { pressed: true },
                    }),
                    0 | 2 => Some(ControlEvent {
                        control_id: format!("key-{code}"),
                        input: Input::Button { pressed: false },
                    }),
                    _ => None,
                }
            } else if let Some(index) = [0x37, 0x35, 0x33, 0x36].iter().position(|c| *c == code) {
                if state == 1 {
                    Some(ControlEvent {
                        control_id: format!("dial-{}", index + 1),
                        input: Input::Pulse,
                    })
                } else {
                    None
                }
            } else if let Some(index) = rotary
                .iter()
                .position(|(left, right)| code == *left || code == *right)
            {
                Some(ControlEvent {
                    control_id: format!("dial-{}", index + 1),
                    input: Input::Rotate {
                        delta: if rotary[index].0 == code { -1 } else { 1 },
                    },
                })
            } else if (0x40..=0x43).contains(&code) || code == 0 {
                if data.len() < 13 {
                    self.touch = None;
                    return Err(Error::Protocol("truncated touch coordinate".into()));
                }
                let x = u16::from_be_bytes([data[11], data[12]]);
                self.validate_touch(x, 0)?;
                self.touch = (code == 0).then_some(TouchTrajectory {
                    start_x: x,
                    latest_x: x,
                    start_y: None,
                    latest_y: 0,
                    updated: now,
                });
                Some(ControlEvent {
                    control_id: if code == 0 {
                        "touch-strip".into()
                    } else {
                        format!("touch-{}", code - 0x40 + 1)
                    },
                    input: Input::Touch {
                        phase: if code == 0 {
                            "start".into()
                        } else {
                            "end".into()
                        },
                        x,
                        y: 0,
                    },
                })
            } else if code == 0x38 || code == 0x39 {
                // Firmware emitted identical terminal bytes for opposite motions
                // The terminal confirms a gesture; only its active trajectory can
                // establish direction. Missing/expired/zero-motion input fails closed.
                self.finish_touch().filter(|event| matches!(&event.input, Input::Swipe { direction } if direction == "left" || direction == "right"))
            } else if (code == 0xb1 || code == 0xb2) && state == 0 {
                // Captured on the supplied VSDInside unit: B1 up, B2 down
                self.finish_vertical(code)
            } else {
                self.touch = None;
                None
            }
        };
        Ok(event.into_iter().collect())
    }
}
impl HardwareSession for Session {
    // Initialize for the current operation
    fn initialize(&mut self) -> Result<Vec<DeviceTransaction>> {
        self.touch = None;
        Ok(vec![self.encode(DeviceCommand::Wake)?])
    }
    // Decode for the current operation
    fn decode(&mut self, report: &[u8]) -> Result<Vec<ControlEvent>> {
        self.decode_at(report, Instant::now())
    }
    // Check support for frame encoding
    fn supports_frame_encoding(&self, _encoding: FrameEncoding) -> bool {
        true
    }
    // Encode frame
    fn encode_frame(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
    ) -> Result<Vec<FrameUpload>> {
        commands
            .into_iter()
            .map(|cmd| {
                if encoding == FrameEncoding::GroupedKeyUpdates {
                    if let DeviceCommand::Image { control_id, data } = &cmd {
                        let control = MANIFEST.control(control_id)?;
                        // Only the ten physical LCD keys participate. Touch-image slots
                        // retain their legacy STP; BGPIC has its own framing and no STP.
                        if control.kind == "button" {
                            let slot = control
                                .image_slot
                                .ok_or_else(|| Error::Unsupported("key image slot".into()))?;
                            let data = png_output::compact(data)?;
                            let mut header = b"BAT".to_vec();
                            header.extend((data.len() as u32).to_be_bytes());
                            header.push(slot);
                            return Ok(FrameUpload {
                                transaction: transfer(&header, &data, false, control_id.clone())?,
                                embedded_commit_count: 0,
                                commit: Some(FrameCommit {
                                    group: "n4-pro-key-stp-v1".into(),
                                    reports: vec![command(b"STP")?],
                                }),
                            });
                        }
                    }
                }
                let embedded_commit_count =
                    usize::from(matches!(&cmd, DeviceCommand::Image { .. }));
                self.encode(cmd).map(|transaction| FrameUpload {
                    transaction,
                    commit: None,
                    embedded_commit_count,
                })
            })
            .collect()
    }
    // Encode for the current operation
    fn encode(&mut self, cmd: DeviceCommand) -> Result<DeviceTransaction> {
        if matches!(&cmd, DeviceCommand::Sleep | DeviceCommand::Clear) {
            self.touch = None;
        }
        let single = |bytes: &[u8]| command(bytes).map(DeviceTransaction::single);
        match cmd {
            DeviceCommand::Wake => single(b"DIS"),
            DeviceCommand::Sleep => single(b"HAN"),
            DeviceCommand::Heartbeat => single(b"CONNECT"),
            DeviceCommand::Clear => single(b"CLE\0\0\0\xff"),
            DeviceCommand::BootLogo { data } => {
                use hardware_protocols::vendor_crt::{
                    encode_surface_for, EncodingPurpose, Surface, WireEncoding,
                };
                let data = encode_surface_for(
                    &data,
                    [800, 480],
                    &Surface {
                        control_id: "boot-logo".into(),
                        encoding: WireEncoding::Jpeg,
                        rotation: 180,
                        flip_x: false,
                        flip_y: false,
                        static_target: None,
                        animated_target: None,
                    },
                    EncodingPurpose::Boot,
                )?;
                let mut header = b"LOG".to_vec();
                header.extend((data.len() as u32).to_be_bytes());
                header.push(1);
                let mut transaction = transfer(&header, &data, false, "boot-logo".into())?;
                transaction.coalesce_key = None;
                Ok(transaction)
            }
            DeviceCommand::Brightness(value) => {
                if value > 100 {
                    return Err(Error::Protocol("brightness must be 0..100".into()));
                }
                single(&[b'L', b'I', b'G', 0, 0, value])
            }
            DeviceCommand::Keyboard(_)
            | DeviceCommand::KeyboardFactoryReset
            | DeviceCommand::KeyboardSettingsQuery => {
                Err(Error::Unsupported("keyboard settings".into()))
            }
            DeviceCommand::Vibration {
                enabled,
                configuration,
            } => {
                MANIFEST
                    .capabilities
                    .vibration
                    .as_ref()
                    .unwrap()
                    .validate_configuration(&configuration)?;
                // Pinned DeviceConfig.py / N4ProConfigEnumerate order; no field
                // has a preserve-current sentinel. Every value was reviewed.
                let values = [
                    configuration["led_follow_key_light"],
                    configuration["key_light_on_disconnect"],
                    configuration["check_usb_power"],
                    enabled,
                    configuration["reset_usb_report"],
                    configuration["enable_boot_video"],
                ];
                let mut payload = b"QUCMD".to_vec();
                payload.extend(values.map(|value| if value { 0x11 } else { 0xff }));
                Ok(DeviceTransaction {
                    reports: vec![command(&payload)?],
                    coalesce_key: Some("device-configuration".into()),
                })
            }
            DeviceCommand::Lighting { brightness, colors } => {
                if colors.len() != 4 {
                    return Err(Error::Unsupported(
                        "N4 Pro has four knob lighting channels".into(),
                    ));
                }
                if brightness > 100 {
                    return Err(Error::Protocol("lighting must be 0..100".into()));
                }
                let all_off = colors.iter().all(|c| *c == [0, 0, 0]);
                let mut reports = vec![command(&[
                    b'L',
                    b'B',
                    b'L',
                    b'I',
                    b'G',
                    if all_off { 0 } else { brightness },
                ])?];
                if !all_off {
                    let mut bytes = b"SETLB".to_vec();
                    for color in colors {
                        bytes.extend(color);
                    }
                    reports.push(command(&bytes)?);
                }
                Ok(DeviceTransaction {
                    reports,
                    coalesce_key: Some("lighting".into()),
                })
            }
            DeviceCommand::Image { control_id, data } => {
                let control = MANIFEST.control(&control_id)?;
                let slot = control.image_slot.ok_or_else(|| {
                    Error::Unsupported(format!("no image surface for {control_id}"))
                })?;
                let data = png_output::compact(&data)?;
                let mut header = b"BAT".to_vec();
                header.extend((data.len() as u32).to_be_bytes());
                header.push(slot);
                transfer(&header, &data, true, control_id)
            }
            DeviceCommand::Background {
                data,
                x,
                y,
                width,
                height,
            } => {
                if width == 0
                    || height == 0
                    || u32::from(x) + u32::from(width) > 800
                    || u32::from(y) + u32::from(height) > 480
                {
                    return Err(Error::Protocol(
                        "background rectangle outside 800×480".into(),
                    ));
                }
                let data = png_output::compact(&data)?;
                let mut header = b"BGPIC".to_vec();
                header.extend((data.len() as u32).to_be_bytes());
                for value in [800 - x - width, 480 - y - height, width, height] {
                    header.extend(value.to_be_bytes());
                }
                header.extend([0, 0]);
                transfer(&header, &data, false, "background".into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify startup logo is persistent JPEG without live frame or commit commands
    fn startup_logo_is_persistent_jpeg_without_live_frame_or_commit_commands() {
        let image = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                800,
                480,
                image::Rgb([37, 79, 143]),
            )),
            "png",
        )
        .unwrap();
        let tx = Session::default()
            .encode(DeviceCommand::BootLogo { data: image })
            .unwrap();
        assert_eq!(tx.coalesce_key, None);
        assert_eq!(&tx.reports[0][6..9], b"LOG");
        assert_eq!(tx.reports[0][13], 1);
        let length = u32::from_be_bytes(tx.reports[0][9..13].try_into().unwrap()) as usize;
        assert_eq!(tx.reports.len(), 1 + length.div_ceil(1024));
        let bytes: Vec<_> = tx.reports[1..]
            .iter()
            .flat_map(|report| report[1..].iter().copied())
            .take(length)
            .collect();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        let raster = image::load_from_memory(&bytes).unwrap();
        assert_eq!((raster.width(), raster.height()), (800, 480));
    }

    // Derive the lookup key for this value
    fn key(id: usize, data: Vec<u8>) -> DeviceCommand {
        DeviceCommand::Image {
            control_id: format!("key-{id}"),
            data,
        }
    }
    #[test]
    // Verify compressed touch and background keep pixels geometry and refresh rules
    fn compressed_touch_and_background_keep_pixels_geometry_and_refresh_rules() {
        let mut session = Session::default();
        for background in [false, true] {
            let (width, height) = if background { (800, 480) } else { (176, 112) };
            let pixels = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(
                width,
                height,
                |x, y| image::Rgba([(x % 251) as u8, (y % 241) as u8, 127, (x % 256) as u8]),
            ));
            let data = render::encode_pixels(pixels.clone(), "png").unwrap();
            let old_len = data.len();
            let command = if background {
                DeviceCommand::Background {
                    data,
                    x: 0,
                    y: 0,
                    width: width as u16,
                    height: height as u16,
                }
            } else {
                DeviceCommand::Image {
                    data,
                    control_id: "touch-1".into(),
                }
            };
            let tx = session.encode(command).unwrap();
            let offset = if background { 11 } else { 9 };
            let len =
                u32::from_be_bytes(tx.reports[0][offset..offset + 4].try_into().unwrap()) as usize;
            let payload: Vec<_> = tx.reports[1..]
                .iter()
                .flat_map(|r| r[1..].iter().copied())
                .take(len)
                .collect();
            assert_eq!(image::load_from_memory(&payload).unwrap(), pixels);
            assert!(len < old_len);
            assert_eq!(
                tx.reports.len(),
                1 + len.div_ceil(1024) + usize::from(!background)
            );
            if !background {
                assert_eq!(
                    tx.reports.last().unwrap(),
                    &hardware_protocols::command(b"STP").unwrap()
                );
            }
        }
    }

    #[test]
    // Verify experimental key uploads have explicit shared commit and legacy is unchanged
    fn experimental_key_uploads_have_explicit_shared_commit_and_legacy_is_unchanged() {
        for count in [1, 2, 10] {
            let mut session = Session::default();
            let commands = (1..=count)
                .map(|id| key(id, vec![id as u8; 1025]))
                .collect::<Vec<_>>();
            let legacy = session
                .encode_frame(commands.clone(), FrameEncoding::IndividualUpdates)
                .unwrap();
            let grouped = session
                .encode_frame(commands, FrameEncoding::GroupedKeyUpdates)
                .unwrap();
            for (index, (old, new)) in legacy.iter().zip(&grouped).enumerate() {
                assert_eq!(old.transaction.reports.len(), 4);
                assert_eq!(new.transaction.reports.len(), 3);
                assert_eq!(
                    &old.transaction.reports[..3],
                    new.transaction.reports.as_slice()
                );
                assert_eq!(
                    new.commit.as_ref().unwrap().reports,
                    vec![command(b"STP").unwrap()]
                );
                assert_eq!(
                    new.transaction.reports[0][13],
                    if index < 5 {
                        11 + index as u8
                    } else {
                        1 + index as u8
                    }
                );
                assert!(new
                    .transaction
                    .reports
                    .iter()
                    .all(|report| report.len() == 1025 && report[0] == 0));
                assert!(old.commit.is_none());
                assert_eq!(old.embedded_commit_count, 1);
                assert_eq!(new.embedded_commit_count, 0);
            }
            assert_eq!(
                grouped
                    .iter()
                    .map(|g| g.transaction.reports.len())
                    .sum::<usize>()
                    + 1,
                count * 3 + 1
            );
        }
    }
    #[test]
    // Verify grouped keys preserve touch background control bytes and rectangle transform
    fn grouped_keys_preserve_touch_background_control_bytes_and_rectangle_transform() {
        let mut session = Session::default();
        let mut commands = (1..=4)
            .map(|id| DeviceCommand::Image {
                control_id: format!("touch-{id}"),
                data: vec![7; 1025],
            })
            .collect::<Vec<_>>();
        commands.extend([
            DeviceCommand::Background {
                data: vec![9; 1025],
                x: 10,
                y: 20,
                width: 30,
                height: 40,
            },
            DeviceCommand::Brightness(80),
            DeviceCommand::Lighting {
                brightness: 50,
                colors: vec![[1, 2, 3]; 4],
            },
        ]);
        let old = session
            .encode_frame(commands.clone(), FrameEncoding::IndividualUpdates)
            .unwrap();
        let new = session
            .encode_frame(commands, FrameEncoding::GroupedKeyUpdates)
            .unwrap();
        for (a, b) in old.iter().zip(&new) {
            assert_eq!(a.transaction.reports, b.transaction.reports);
            assert!(b.commit.is_none());
        }
        for (index, upload) in new[..4].iter().enumerate() {
            assert_eq!(upload.transaction.reports[0][13], index as u8 + 1);
        }
        let background = &new[4].transaction;
        assert_eq!(&background.reports[0][6..11], b"BGPIC");
        assert_eq!(
            &background.reports[0][15..23],
            &[2, 248, 1, 164, 0, 30, 0, 40]
        );
        assert_eq!(background.reports.len(), 3);
        assert!(session
            .encode_frame(vec![key(1, vec![])], FrameEncoding::GroupedKeyUpdates)
            .is_err());
        assert!(session
            .encode_frame(vec![key(11, vec![1])], FrameEncoding::GroupedKeyUpdates)
            .is_err());
    }

    // Start for the current operation
    fn start(x: u16) -> Vec<u8> {
        let mut bytes = b"ACK\0\0OK\0\0\0\x01\0\0".to_vec();
        bytes[11..13].copy_from_slice(&x.to_be_bytes());
        bytes
    }
    // Decode the dial movement reported by the device
    fn movement(x: u16) -> Vec<u8> {
        let mut bytes = b"ACK\0ARX\0\0\0\0\0\0\x30".to_vec();
        bytes[10..12].copy_from_slice(&x.to_be_bytes());
        bytes
    }
    // Recognize a terminal device input report
    fn terminal() -> Vec<u8> {
        b"ACK\0\0OK\0\0\x39\0".to_vec()
    }

    #[test]
    // Verify vertical trajectories use real y and do not duplicate taps
    fn vertical_trajectories_use_real_y_and_do_not_duplicate_taps() {
        let now = Instant::now();
        let point = |x: u16, y: u16| {
            let mut report = movement(x);
            report[12..14].copy_from_slice(&y.to_be_bytes());
            report
        };
        for (first, last, expected) in [(100, 30, "up"), (30, 100, "down")] {
            let mut session = Session::default();
            session.decode_at(&start(400), now).unwrap();
            session.decode_at(&point(402, first), now).unwrap();
            session.decode_at(&point(404, last), now).unwrap();
            let mut end = terminal();
            end[9] = if expected == "up" { 0xb1 } else { 0xb2 };
            let result = session.decode_at(&end, now).unwrap();
            assert_eq!(
                result,
                vec![ControlEvent {
                    control_id: "touch-strip".into(),
                    input: Input::Swipe {
                        direction: expected.into()
                    }
                }]
            );
            assert!(session.decode_at(&terminal(), now).unwrap().is_empty());
        }
        for (dx, dy) in [(10, 10), (40, 40), (0, 0)] {
            let mut session = Session::default();
            session.decode_at(&start(400), now).unwrap();
            session.decode_at(&point(400, 50), now).unwrap();
            session.decode_at(&point(400 + dx, 50 + dy), now).unwrap();
            assert!(session.decode_at(&terminal(), now).unwrap().is_empty());
        }
        // One coordinate report cannot establish vertical displacement from the synthetic start
        let mut session = Session::default();
        session.decode_at(&start(400), now).unwrap();
        session.decode_at(&point(400, 100), now).unwrap();
        assert!(session.decode_at(&terminal(), now).unwrap().is_empty());
    }

    #[test]
    // Verify vertical terminals require fresh vertical motion and completion state
    fn vertical_terminals_require_fresh_vertical_motion_and_completion_state() {
        let now = Instant::now();
        let point = |x: u16, y: u16| {
            let mut report = movement(x);
            report[12..14].copy_from_slice(&y.to_be_bytes());
            report
        };
        let end = |state| {
            let mut report = terminal();
            report[9] = 0xb1;
            report[10] = state;
            report
        };
        let mut session = Session::default();
        assert!(session.decode_at(&end(0), now).unwrap().is_empty());
        session.decode_at(&start(400), now).unwrap();
        session.decode_at(&point(400, 110), now).unwrap();
        session.decode_at(&point(400, 30), now).unwrap();
        assert!(session.decode_at(&end(1), now).unwrap().is_empty());
        assert!(session.decode_at(&end(0), now).unwrap().is_empty());
        session.decode_at(&start(400), now).unwrap();
        session.decode_at(&point(400, 110), now).unwrap();
        session.decode_at(&point(400, 30), now).unwrap();
        assert!(session
            .decode_at(
                &end(0),
                now + TOUCH_TRAJECTORY_MAX_GAP + Duration::from_millis(1)
            )
            .unwrap()
            .is_empty());
        session.decode_at(&start(400), now).unwrap();
        session.decode_at(&point(400, 60), now).unwrap();
        session.decode_at(&point(100, 60), now).unwrap();
        assert!(session.decode_at(&end(0), now).unwrap().is_empty());
        session.decode_at(&start(400), now).unwrap();
        session.decode_at(&point(400, 110), now).unwrap();
        session.decode_at(&point(400, 30), now).unwrap();
        assert!(session.decode_at(&terminal(), now).unwrap().is_empty());
    }

    #[test]
    // Verify sparse vertical samples use verified codes but never contradict motion
    fn sparse_vertical_samples_use_verified_codes_but_never_contradict_motion() {
        let now = Instant::now();
        for (code, expected) in [(0xb1, "up"), (0xb2, "down")] {
            let mut session = Session::default();
            let mut end = terminal();
            end[9] = code;
            session.decode_at(&start(400), now).unwrap();
            assert!(
                session.decode_at(&end, now).unwrap().is_empty(),
                "A coordinate sample is required"
            );
            session.decode_at(&start(400), now).unwrap();
            session.decode_at(&movement(402), now).unwrap();
            assert_eq!(
                session.decode_at(&end, now).unwrap(),
                vec![ControlEvent {
                    control_id: "touch-strip".into(),
                    input: Input::Swipe {
                        direction: expected.into()
                    }
                }]
            );
            assert!(
                session.decode_at(&end, now).unwrap().is_empty(),
                "A completion is consumed once"
            );
            session.decode_at(&start(400), now).unwrap();
            let mut first = movement(400);
            first[12..14]
                .copy_from_slice(&(if code == 0xb1 { 30u16 } else { 100u16 }).to_be_bytes());
            let mut last = movement(400);
            last[12..14]
                .copy_from_slice(&(if code == 0xb1 { 100u16 } else { 30u16 }).to_be_bytes());
            session.decode_at(&first, now).unwrap();
            session.decode_at(&last, now).unwrap();
            assert!(
                session.decode_at(&end, now).unwrap().is_empty(),
                "Contradictory sampled motion fails closed"
            );
        }
    }

    #[test]
    // Verify stale touch trajectory cannot produce a swipe
    fn stale_touch_trajectory_cannot_produce_a_swipe() {
        let now = Instant::now();
        let mut session = Session::default();
        session.decode_at(&start(700), now).unwrap();
        session
            .decode_at(&movement(100), now + Duration::from_millis(100))
            .unwrap();
        assert!(session
            .decode_at(
                &terminal(),
                now + Duration::from_millis(101) + TOUCH_TRAJECTORY_MAX_GAP,
            )
            .unwrap()
            .is_empty());

        // An orphan move cannot revive an expired start
        session.decode_at(&start(700), now).unwrap();
        session
            .decode_at(&movement(100), now + TOUCH_TRAJECTORY_MAX_GAP * 2)
            .unwrap();
        assert!(session
            .decode_at(&terminal(), now + TOUCH_TRAJECTORY_MAX_GAP * 2)
            .unwrap()
            .is_empty());
    }

    #[test]
    // Verify continuous long touch uses latest direction and consumes trajectory
    fn continuous_long_touch_uses_latest_direction_and_consumes_trajectory() {
        let now = Instant::now();
        let mut session = Session::default();
        session.decode_at(&start(700), now).unwrap();
        for (second, x) in [(1, 600), (2, 400), (3, 100)] {
            session
                .decode_at(&movement(x), now + Duration::from_secs(second))
                .unwrap();
        }
        let at = now + Duration::from_secs(3) + TOUCH_TRAJECTORY_MAX_GAP;
        assert_eq!(
            session.decode_at(&terminal(), at).unwrap(),
            vec![ControlEvent {
                control_id: "touch-strip".into(),
                input: Input::Swipe {
                    direction: "left".into()
                }
            }]
        );
        assert!(session.decode_at(&terminal(), at).unwrap().is_empty());
    }
}
