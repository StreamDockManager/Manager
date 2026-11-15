//! Connection-local protocol selection. Model/profile identities never change here
use crate::{Error, ModelManifest, Result};
use serde::{Deserialize, Serialize};

pub const CODEC_CONTRACT_REVISION: &str = "codec-fc124cb-v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WireImageEncoding {
    Jpeg,
    Png,
    BgrRows,
    BgrColumns,
    Rgb565Le,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageTransform {
    /// Clockwise rotation in degrees, applied before flips
    pub rotation: u16,
    pub flip_x: bool,
    pub flip_y: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImagePurpose {
    Static,
    Animated,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedDeviceContract {
    pub revision: String,
    pub variant_id: String,
    /// Actual HID observation, never the USB release number or a guessed default
    pub firmware: Option<String>,
    pub manifest: ModelManifest,
    pub key_transform: Option<ImageTransform>,
    pub boot_transform: Option<ImageTransform>,
    pub boot_encoding: Option<WireImageEncoding>,
    pub image_output_disabled_reason: Option<String>,
}
impl ResolvedDeviceContract {
    // Construct the value and initialize its state
    pub fn new(manifest: &ModelManifest, variant_id: &str, firmware: Option<&str>) -> Result<Self> {
        if !manifest.variants.iter().any(|v| v.id == variant_id) {
            return Err(Error::Unsupported(
                "unknown device variant; protocol selection refused".into(),
            ));
        }
        Ok(Self {
            revision: CODEC_CONTRACT_REVISION.into(),
            variant_id: variant_id.into(),
            firmware: firmware
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            manifest: manifest.clone(),
            key_transform: None,
            boot_transform: None,
            boot_encoding: None,
            image_output_disabled_reason: None,
        })
    }
    /// Includes the selected geometry, capabilities, transforms and the complete
    /// firmware observation. Use this identity at preparation AND dispatch.
    pub fn identity(&self) -> String {
        serde_json::to_string(self).expect("serializable device contract")
    }
    // Validate for
    pub fn validate_for(&self, manifest: &ModelManifest) -> Result<()> {
        if self.manifest.model_id != manifest.model_id
            || !manifest.variants.iter().any(|v| v.id == self.variant_id)
        {
            return Err(Error::Unsupported(
                "resolved contract does not belong to this module".into(),
            ));
        }
        Ok(())
    }
}
