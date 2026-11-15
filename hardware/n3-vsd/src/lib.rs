//! VSDinside/TreasLin N3, a separate six-display model from the Mirabox N3
//! Protocol facts and exact source revisions are recorded in ../README.md
use hardware_core::*;
use hardware_protocols::vendor_crt::{Codec, Session};
use std::sync::LazyLock;

static MANIFEST: LazyLock<ModelManifest> = LazyLock::new(|| {
    ModelManifest::parse(include_str!("../device.toml")).expect("embedded N3 VSD descriptor")
});
static CODEC: LazyLock<Codec> = LazyLock::new(|| {
    Codec::parse(include_str!("../codec.toml"), &MANIFEST).expect("embedded N3 VSD codec")
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
            toml::from_str(include_str!("../verification/plan.toml")).expect("embedded N3 VSD plan")
        });
        &PLAN
    }
    // Create a protocol session for the supplied device identity
    fn session_for(&self, contract: &ResolvedDeviceContract) -> Result<Box<dyn HardwareSession>> {
        contract.validate_for(&MANIFEST)?;
        Ok(Box::new(N3Session(Session::new_resolved(contract, &CODEC))))
    }
    // Create a device protocol session
    fn session(&self) -> Box<dyn HardwareSession> {
        Box::new(N3Session(Session::new(&MANIFEST, &CODEC)))
    }
}

struct N3Session(Session);
impl HardwareSession for N3Session {
    // Initialize for the current operation
    fn initialize(&mut self) -> Result<Vec<DeviceTransaction>> {
        let mut transactions = self.0.initialize()?;
        transactions[0] = self.encode(DeviceCommand::Wake)?;
        Ok(transactions)
    }

    // Decode for the current operation
    fn decode(&mut self, report: &[u8]) -> Result<Vec<ControlEvent>> {
        // This variant's source establishes ACK and byte 9/10, without requiring
        // the additional OK marker used by the manufacturer SDK family decoder.
        let data = hardware_protocols::normalize(report);
        if data.get(..3) != Some(b"ACK") || data.len() < 11 {
            return Ok(vec![]);
        }
        let (code, state) = (data[9], data[10]);
        // Rotation direction is encoded entirely by the code; its state byte
        // is unused by the pinned reader. Button/encoder edges use 0 or 1.
        if state > 1 && !matches!(code, 0x90 | 0x91 | 0x60 | 0x61 | 0x50 | 0x51) {
            return Ok(vec![]);
        }
        let (control_id, input) = match code {
            1..=6 => (
                format!("key-{code}"),
                Input::Button {
                    pressed: state == 1,
                },
            ),
            0x25 | 0x30 | 0x31 => (
                format!(
                    "key-{}",
                    match code {
                        0x25 => 7,
                        0x30 => 8,
                        _ => 9,
                    }
                ),
                Input::Button {
                    pressed: state == 1,
                },
            ),
            0x33..=0x35 => (
                format!(
                    "dial-{}",
                    match code {
                        0x33 => 1,
                        0x34 => 2,
                        _ => 3,
                    }
                ),
                Input::Button {
                    pressed: state == 1,
                },
            ),
            0x90 | 0x91 | 0x60 | 0x61 | 0x50 | 0x51 => (
                format!(
                    "dial-{}",
                    match code {
                        0x90 | 0x91 => 1,
                        0x60 | 0x61 => 2,
                        _ => 3,
                    }
                ),
                Input::Rotate {
                    delta: if code & 1 == 0 { -1 } else { 1 },
                },
            ),
            _ => return Ok(vec![]),
        };
        Ok(vec![ControlEvent { control_id, input }])
    }

    // Encode for the current operation
    fn encode(&mut self, command: DeviceCommand) -> Result<DeviceTransaction> {
        let wake = matches!(command, DeviceCommand::Wake);
        let mut transaction = self.0.encode(command)?;
        if wake {
            // Both initial connection and wake require DIS followed by LIG
            // Runtime reapplies saved brightness after initialization/wake
            transaction
                .reports
                .extend(self.0.encode(DeviceCommand::Brightness(0))?.reports);
        }
        Ok(transaction)
    }

    // Check support for frame encoding
    fn supports_frame_encoding(&self, encoding: FrameEncoding) -> bool {
        self.0.supports_frame_encoding(encoding)
    }

    // Encode frame
    fn encode_frame(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
    ) -> Result<Vec<FrameUpload>> {
        self.encode_frame_with_purpose(commands, encoding, ImagePurpose::Static)
    }
    // Encode frame with purpose
    fn encode_frame_with_purpose(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
        purpose: ImagePurpose,
    ) -> Result<Vec<FrameUpload>> {
        let mut uploads = vec![];
        for command in commands {
            if matches!(command, DeviceCommand::Image { .. }) {
                uploads.extend(self.0.encode_frame_with_purpose(
                    vec![command],
                    encoding,
                    purpose,
                )?);
            } else {
                uploads.push(FrameUpload::from(self.encode(command)?));
            }
        }
        Ok(uploads)
    }
}
