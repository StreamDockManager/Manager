pub mod appearance;
pub mod composition;
pub mod contract;
pub use contract::*;
pub mod feedback;
pub mod fonts;
pub mod frame;
#[cfg(target_os = "macos")]
mod native_text;
pub use frame::*;
pub mod icons;
pub mod keyboard;
pub mod manifest;
pub use keyboard::*;
pub mod output;
pub mod plugin_display;
pub mod png_output;
pub mod render;
mod vendor_title;
pub mod verification;

pub use manifest::*;
pub use output::*;
use serde::{Deserialize, Serialize};
pub use verification::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid descriptor: {0}")]
    Descriptor(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("connection is no longer current")]
    StaleConnection,
    #[error("transport: {0}")]
    Transport(String),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS, schemars::JsonSchema)]
pub struct HidIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    pub interface: i32,
    pub usage_page: u16,
    pub usage: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    Button { pressed: bool },
    Pulse,
    Rotate { delta: i32 },
    Touch { phase: String, x: u16, y: u16 },
    Swipe { direction: String },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS, schemars::JsonSchema)]
pub struct ControlEvent {
    pub control_id: String,
    pub input: Input,
}
#[derive(Debug, Clone)]
pub enum DeviceCommand {
    Wake,
    Sleep,
    Heartbeat,
    Clear,
    /// Explicit persistent startup-image upload. Data is a canonical PNG/JPEG source
    BootLogo {
        data: Vec<u8>,
    },
    Brightness(u8),
    Keyboard(KeyboardSettings),
    /// Explicit destructive on-device keyboard reset, never connection setup
    KeyboardFactoryReset,
    /// Read-only request for reported keyboard settings
    KeyboardSettingsQuery,
    Vibration {
        enabled: bool,
        configuration: std::collections::BTreeMap<String, bool>,
    },
    Lighting {
        brightness: u8,
        colors: Vec<[u8; 3]>,
    },
    Image {
        control_id: String,
        data: Vec<u8>,
    },
    Background {
        data: Vec<u8>,
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
}

pub trait HardwareModule: Send + Sync {
    // Return the manifest for this implementation
    fn manifest(&self) -> &ModelManifest;
    // Describe the evidence required to qualify this device adapter
    fn verification_plan(&self) -> &VerificationPlan;
    // Report whether the candidate matches these criteria
    fn matches(&self, identity: &HidIdentity) -> bool {
        self.manifest()
            .variants
            .iter()
            .any(|v| v.available && v.identity.matches(identity))
    }
    /// Explicit representative contract for schematic previews, never hardware evidence
    fn representative_contract(&self) -> Result<ResolvedDeviceContract> {
        let variant = self
            .manifest()
            .variants
            .first()
            .ok_or_else(|| Error::Descriptor("missing representative variant".into()))?;
        self.resolve_contract(&variant.id, None)
    }
    // Resolve contract
    fn resolve_contract(
        &self,
        variant_id: &str,
        firmware: Option<&str>,
    ) -> Result<ResolvedDeviceContract> {
        ResolvedDeviceContract::new(self.manifest(), variant_id, firmware)
    }
    // Create a protocol session for the supplied device identity
    fn session_for(&self, contract: &ResolvedDeviceContract) -> Result<Box<dyn HardwareSession>> {
        contract.validate_for(self.manifest())?;
        Ok(self.session())
    }
    // Create a device protocol session
    fn session(&self) -> Box<dyn HardwareSession>;
}

/// Ordered connection setup. A wait starts only after preceding writes complete
#[derive(Debug, Clone)]
pub enum InitializationStep {
    Write(DeviceTransaction),
    Wait(std::time::Duration),
}

pub trait HardwareSession: Send {
    // Initialize for the current operation
    fn initialize(&mut self) -> Result<Vec<DeviceTransaction>>;
    /// Model-owned connection timing; ordinary wake and frame uploads do not run this plan
    fn initialization_plan(&mut self) -> Result<Vec<InitializationStep>> {
        self.initialize().map(|transactions| {
            transactions
                .into_iter()
                .map(InitializationStep::Write)
                .collect()
        })
    }
    // Decode for the current operation
    fn decode(&mut self, report: &[u8]) -> Result<Vec<ControlEvent>>;
    // Encode for the current operation
    fn encode(&mut self, command: DeviceCommand) -> Result<DeviceTransaction>;
    /// Model-owned readback; absent fields remain unknown, never inferred defaults
    fn decode_keyboard_settings(&mut self, _report: &[u8]) -> Result<Option<KeyboardSettings>> {
        Ok(None)
    }
    /// Protocol capability only; never a physical performance qualification
    fn supports_frame_encoding(&self, encoding: FrameEncoding) -> bool {
        encoding == FrameEncoding::IndividualUpdates
    }
    /// A host-rendered key whose final lossless PNG has already been selected.
    /// Ordinary image commands, including imported/plugin bytes, use the validating path.
    fn encode_rendered_key(
        &mut self,
        control: String,
        data: Vec<u8>,
        encoding: FrameEncoding,
    ) -> Result<FrameUpload> {
        let mut uploads = self.encode_frame(
            vec![DeviceCommand::Image {
                control_id: control,
                data,
            }],
            encoding,
        )?;
        uploads
            .pop()
            .ok_or_else(|| Error::Protocol("Missing key upload".into()))
    }
    // Encode frame with purpose
    fn encode_frame_with_purpose(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
        _purpose: ImagePurpose,
    ) -> Result<Vec<FrameUpload>> {
        self.encode_frame(commands, encoding)
    }
    // Encode frame
    fn encode_frame(
        &mut self,
        commands: Vec<DeviceCommand>,
        encoding: FrameEncoding,
    ) -> Result<Vec<FrameUpload>> {
        if !self.supports_frame_encoding(encoding) {
            return Err(Error::Unsupported("grouped frame encoding".into()));
        }
        commands
            .into_iter()
            .map(|command| self.encode(command).map(FrameUpload::from))
            .collect()
    }
}
