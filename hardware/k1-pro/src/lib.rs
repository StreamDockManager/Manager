use hardware_core::*;
use hardware_protocols::vendor_crt::{Codec, Session};
use std::sync::LazyLock;
mod keyboard_readback;

static MANIFEST: LazyLock<ModelManifest> = LazyLock::new(|| {
    ModelManifest::parse(include_str!("../device.toml")).expect("embedded model descriptor")
});
static CODEC: LazyLock<Codec> = LazyLock::new(|| {
    Codec::parse(include_str!("../codec.toml"), &MANIFEST).expect("embedded model codec")
});
pub struct Model;
impl HardwareModule for Model {
    // Return the manifest for this implementation
    fn manifest(&self) -> &ModelManifest {
        &MANIFEST
    }
    // Describe the evidence required to qualify this device adapter
    fn verification_plan(&self) -> &VerificationPlan {
        static PLAN: LazyLock<VerificationPlan> = LazyLock::new(|| {
            toml::from_str(include_str!("../verification/plan.toml")).expect("embedded model plan")
        });
        &PLAN
    }
    // Create a protocol session for the supplied device identity
    fn session_for(&self, contract: &ResolvedDeviceContract) -> Result<Box<dyn HardwareSession>> {
        contract.validate_for(&MANIFEST)?;
        Ok(Box::new(K1Session {
            session: Session::new_resolved(contract, &CODEC),
            keyboard: contract.manifest.capabilities.keyboard.clone(),
        }))
    }
    // Create a device protocol session
    fn session(&self) -> Box<dyn HardwareSession> {
        Box::new(K1Session {
            session: Session::new(&MANIFEST, &CODEC),
            keyboard: MANIFEST.capabilities.keyboard.clone(),
        })
    }
}

/// Model-specific keyboard commands over explicitly numbered HIDAPI reports
struct K1Session {
    session: Session,
    keyboard: Option<KeyboardCapabilities>,
}

/// The official keyboard web app's custom-color mode: flag, RGB, then HSV
/// Python's COLOR + three-zero payload leaves the custom-color flag disabled and
/// places RGB at different offsets. Keep this correction local to K1.
fn custom_color_payload([r, g, b]: [u8; 3], enabled: bool) -> [u8; 13] {
    // Use the same normalized RGB -> HSV operations as the web color picker
    let (red, green, blue) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;
    let sector = if delta == 0.0 {
        0.0
    } else if max == red {
        (green - blue) / delta + if green < blue { 6.0 } else { 0.0 }
    } else if max == green {
        (blue - red) / delta + 2.0
    } else {
        (red - green) / delta + 4.0
    };
    let hue = (60.0 * sector % 360.0) as u16;
    let saturation_percent = if max == 0.0 {
        0.0
    } else {
        100.0 * (delta / max)
    };
    let value_percent = 100.0 * max;
    let saturation = (saturation_percent / 100.0 * 255.0).floor() as u8;
    let value = (value_percent / 100.0 * 255.0).floor() as u8;
    [
        b'C',
        b'O',
        b'L',
        b'O',
        b'R',
        u8::from(enabled),
        r,
        g,
        b,
        (hue >> 8) as u8,
        hue as u8,
        saturation,
        value,
    ]
}

impl HardwareSession for K1Session {
    // Initialize for the current operation
    fn initialize(&mut self) -> Result<Vec<DeviceTransaction>> {
        Ok(vec![
            self.session.encode(DeviceCommand::Wake)?,
            self.encode(DeviceCommand::Clear)?,
            DeviceTransaction::single(CODEC.command(b"STP")?),
        ])
    }

    // Decode for the current operation
    fn decode(&mut self, report: &[u8]) -> Result<Vec<ControlEvent>> {
        // The pinned Python decoder treats 1 as press and every other state as
        // release; rotations are independent of the state. Normalize only the
        // state byte, retaining shared report-ID/ACK/OK/length validation.
        let mut normalized = report.to_vec();
        if let Some(state) = normalized.get_mut(11) {
            *state = if matches!(
                report.get(10),
                Some(0x50 | 0x51 | 0x60 | 0x61 | 0x90 | 0x91)
            ) {
                0
            } else {
                u8::from(*state == 1)
            };
        }
        self.session.decode(&normalized)
    }

    // Decode keyboard settings
    fn decode_keyboard_settings(&mut self, report: &[u8]) -> Result<Option<KeyboardSettings>> {
        keyboard_readback::decode(report)
    }

    // Encode for the current operation
    fn encode(&mut self, command: DeviceCommand) -> Result<DeviceTransaction> {
        if matches!(command, DeviceCommand::KeyboardSettingsQuery) {
            return CODEC.command(b"INFO\0").map(DeviceTransaction::single);
        }
        if matches!(command, DeviceCommand::KeyboardFactoryReset) {
            // All supported K1 Pro variants expose layered bindings in the vendor
            // registry. Restore default resets layers, then onboard records/images.
            return Ok(DeviceTransaction {
                reports: vec![CODEC.command(b"LYRST\xff")?, CODEC.command(b"REREC")?],
                coalesce_key: None,
            });
        }
        if matches!(command, DeviceCommand::Clear) {
            return CODEC.command(b"CLE\0\0\xff").map(DeviceTransaction::single);
        }
        let DeviceCommand::Keyboard(settings) = command else {
            return self.session.encode(command);
        };
        let capabilities = self
            .keyboard
            .as_ref()
            .ok_or_else(|| Error::Unsupported("keyboard settings".into()))?;
        settings.validate_for(capabilities)?;
        let mut reports = Vec::new();
        // Preserve the Python Static reset sequence, using the official web
        // app sendSpeed opcode (LSPD); Python LSPE disagrees with the web app and user-reported behavior.
        // An explicitly requested speed follows the effect, as a separate setter
        if let Some(effect) = settings.effect {
            if effect == 0 {
                reports.push(CODEC.command(b"LSPD\0\0")?);
            }
            reports.push(CODEC.command(&[b'L', b'M', b'O', b'D', 0, effect])?);
        }
        if let Some(speed) = settings.speed {
            reports.push(CODEC.command(&[b'L', b'S', b'P', b'D', 0, speed])?);
        }
        // Apply color after effect selection so its custom-color flag is not
        // superseded by changing the lighting mode in this same transaction.
        if settings.color.is_some() || settings.custom_color.is_some() {
            let enabled = settings.custom_color.unwrap_or(true);
            if enabled && settings.color.is_none() {
                return Err(Error::Protocol(
                    "Custom keyboard color requires RGB values".into(),
                ));
            }
            reports.push(CODEC.command(&custom_color_payload(
                settings.color.unwrap_or([0, 0, 0]),
                enabled,
            ))?);
        }
        if let Some(background) = settings.background_color {
            // The vendor's second Ripple (wire index 14) uses independent BASECO
            // hue/saturation, not a second COLOR payload or RGB/value bytes.
            let saturation = (f64::from(background.saturation) / 100.0 * 255.0).floor() as u8;
            reports.push(CODEC.command(&[
                b'B',
                b'A',
                b'S',
                b'E',
                b'C',
                b'O',
                (background.hue >> 8) as u8,
                background.hue as u8,
                saturation,
            ])?);
        }
        // Mode/color commands can reset the firmware's luminance. Establish the
        // selected level after all visual changes, including preview rollback.
        if let Some(brightness) = settings.brightness {
            reports.push(CODEC.command(&[b'L', b'L', b'U', b'M', 0, brightness])?);
        }
        // Official sendSleepTime uses unsigned big-endian seconds, with all ones
        // for Never. This is an idle timer, not an immediate sleep command.
        if let Some(seconds) = settings.sleep_timeout_seconds {
            let mut payload = b"SLPT\0".to_vec();
            payload.extend_from_slice(&seconds.to_be_bytes());
            reports.push(CODEC.command(&payload)?);
        }
        // Mode changes can affect enumeration. Always issue them last, never
        // write a mode on connect. Auto resolves the host only for this explicit request.
        if let Some(mode) = settings.os_mode {
            let value = match mode.resolved() {
                KeyboardOsMode::Windows => b'W',
                KeyboardOsMode::Macos => b'M',
                KeyboardOsMode::Auto => unreachable!("Auto resolves to a concrete mode"),
            };
            reports.push(CODEC.command(&[b'C', b'P', b'O', b'S', 0, value])?);
        }
        // Partial settings cannot safely coalesce with other partial updates
        Ok(DeviceTransaction {
            reports,
            coalesce_key: None,
        })
    }

    // Check support for frame encoding
    fn supports_frame_encoding(&self, encoding: FrameEncoding) -> bool {
        self.session.supports_frame_encoding(encoding)
    }

    // Encode frame with purpose
    fn encode_frame_with_purpose(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
        purpose: ImagePurpose,
    ) -> Result<Vec<FrameUpload>> {
        self.session
            .encode_frame_with_purpose(commands, encoding, purpose)
    }
    // Encode frame
    fn encode_frame(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
    ) -> Result<Vec<FrameUpload>> {
        self.session.encode_frame(commands, encoding)
    }
}
