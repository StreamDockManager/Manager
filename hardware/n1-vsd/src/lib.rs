use hardware_core::*;
use hardware_protocols::vendor_crt::{Codec, Session};
use std::{sync::LazyLock, time::Duration};
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
        Ok(Box::new(N1Session(Session::new_resolved(contract, &CODEC))))
    }
    // Create a device protocol session
    fn session(&self) -> Box<dyn HardwareSession> {
        Box::new(N1Session(Session::new(&MANIFEST, &CODEC)))
    }
}

struct N1Session(Session);

impl HardwareSession for N1Session {
    // Initialize for the current operation
    fn initialize(&mut self) -> Result<Vec<DeviceTransaction>> {
        self.0.initialize()
    }

    // Build the device initialization command sequence
    fn initialization_plan(&mut self) -> Result<Vec<InitializationStep>> {
        let mut transactions = self.initialize()?;
        let setup = transactions
            .first_mut()
            .ok_or_else(|| Error::Protocol("N1 initialization is missing mode selection".into()))?;
        if setup.reports.first().and_then(|report| report.get(6..9)) != Some(b"MOD".as_slice()) {
            return Err(Error::Protocol(
                "N1 initialization must begin with mode selection".into(),
            ));
        }
        let mode = DeviceTransaction::single(setup.reports.remove(0));
        // The vendor N1 connection handler selects software mode, then schedules
        // the first display refresh 200 ms later. USB completion alone is earlier.
        let mut plan = vec![
            InitializationStep::Write(mode),
            InitializationStep::Wait(Duration::from_millis(200)),
        ];
        plan.extend(transactions.into_iter().map(InitializationStep::Write));
        Ok(plan)
    }

    // Decode for the current operation
    fn decode(&mut self, report: &[u8]) -> Result<Vec<ControlEvent>> {
        self.0.decode(report)
    }

    // Encode for the current operation
    fn encode(&mut self, command: DeviceCommand) -> Result<DeviceTransaction> {
        self.0.encode(command)
    }

    // Check support for frame encoding
    fn supports_frame_encoding(&self, encoding: FrameEncoding) -> bool {
        self.0.supports_frame_encoding(encoding)
    }

    // Encode frame with purpose
    fn encode_frame_with_purpose(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
        purpose: ImagePurpose,
    ) -> Result<Vec<FrameUpload>> {
        self.0
            .encode_frame_with_purpose(commands, encoding, purpose)
    }
    // Encode frame
    fn encode_frame(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
    ) -> Result<Vec<FrameUpload>> {
        self.0.encode_frame(commands, encoding)
    }
}
