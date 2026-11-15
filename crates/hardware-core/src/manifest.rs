use crate::{Error, HidIdentity, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub schema_version: u32,
    pub model_id: String,
    pub name: String,
    /// Canonical suggested-profile family; native identities and codecs stay model-owned
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub profile_family: Option<String>,
    pub protocol: String,
    pub status: String,
    pub variants: Vec<Variant>,
    pub capabilities: DeviceCapabilities,
    pub layout: Layout,
    pub controls: Vec<Control>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Variant {
    pub id: String,
    pub name: String,
    pub identity: HidSelector,
    #[serde(default = "variant_available")]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unavailable_reason: Option<String>,
}
// Check whether this device variant is available
fn variant_available() -> bool {
    true
}
/// Discovery predicate, distinct from the actual identity reported by HIDAPI
/// Omitted interface means any interface; usage pages require an exact value or
/// an inclusive lower bound. Device/vendor/usage are always exact.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HidSelector {
    pub vendor_id: u16,
    pub product_id: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interface: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage_page: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage_page_min: Option<u16>,
    pub usage: u16,
}
impl HidSelector {
    // Report whether the candidate matches these criteria
    pub fn matches(&self, identity: &HidIdentity) -> bool {
        self.vendor_id == identity.vendor_id
            && self.product_id == identity.product_id
            && self.interface.is_none_or(|i| i == identity.interface)
            && self.usage_page.is_none_or(|p| p == identity.usage_page)
            && self.usage_page_min.is_none_or(|p| identity.usage_page >= p)
            && self.usage == identity.usage
    }
    // Check whether the supplied regions overlap
    pub fn overlaps(&self, other: &Self) -> bool {
        let range = |selector: &Self| match selector.usage_page {
            Some(page) => (page, page),
            None => (selector.usage_page_min.unwrap_or(0), u16::MAX),
        };
        let (low, high) = range(self);
        let (other_low, other_high) = range(other);
        self.vendor_id == other.vendor_id
            && self.product_id == other.product_id
            && self.usage == other.usage
            && (self.interface.is_none()
                || other.interface.is_none()
                || self.interface == other.interface)
            && low <= other_high
            && other_low <= high
    }
    // Report whether the value satisfies the supported constraints
    pub fn valid(&self) -> bool {
        self.interface.is_none_or(|i| (0..=255).contains(&i))
            && self.usage != 0
            && matches!(
                (self.usage_page, self.usage_page_min),
                (Some(1..), None) | (None, Some(1..))
            )
    }
}
impl From<HidIdentity> for HidSelector {
    // Convert the input to this type
    fn from(identity: HidIdentity) -> Self {
        Self {
            vendor_id: identity.vendor_id,
            product_id: identity.product_id,
            interface: Some(identity.interface),
            usage_page: Some(identity.usage_page),
            usage_page_min: None,
            usage: identity.usage,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub keyboard: Option<crate::KeyboardCapabilities>,
    /// A source-derived contract could not safely choose the image encoding
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_output_unavailable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub touch_panel: Option<TouchPanelCapabilities>,
    #[serde(default)]
    pub sleep: bool,
    pub brightness: bool,
    pub lighting: bool,
    pub lighting_channels: Vec<String>,
    pub background: bool,
    pub background_size: Option<[u16; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub boot_logo: Option<BootLogoCapabilities>,
    pub reliable_button_release: bool,
    pub reliable_encoder_release: bool,
    pub image_formats: Vec<String>,
    /// Accepted source assets; animated sources are decoded into image_formats by the runtime
    pub media_formats: Vec<String>,
    pub max_fps: u8,
    #[serde(default)]
    pub firmware_query: bool,
    /// Model-specific HID read contract. Omitted means SDK input report 0, 513 bytes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub firmware_report: Option<FirmwareReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vibration: Option<VibrationCapabilities>,
    /// Implemented experimental paths, not hardware qualification or FPS ratings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub frame_output: Option<FrameOutputCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background_minimum_manager: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub lighting_minimum_manager: Option<String>,
}
/// Read-only version transport; it never sends firmware images or update commands
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FirmwareReport {
    pub kind: FirmwareReportKind,
    pub report_id: u8,
    pub length: u16,
    /// Some independently sourced protocols are explicitly unimplemented on Windows
    pub windows_supported: bool,
}
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FirmwareReportKind {
    Input,
    Feature,
}
impl Default for FirmwareReport {
    // Construct the default value
    fn default() -> Self {
        Self {
            kind: FirmwareReportKind::Input,
            report_id: 0,
            length: 513,
            windows_supported: true,
        }
    }
}
/// Host routing capabilities, independent of physical firmware qualification
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TouchPanelCapabilities {
    pub gesture_control: String,
    pub button_controls: Vec<String>,
    pub swipe_directions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BootLogoCapabilities {
    pub size: [u16; 2],
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VibrationCapabilities {
    pub toggle: bool,
    pub strength: bool,
    pub writable: bool,
    pub unavailable_reason: String,
    pub firmware_requirements: std::collections::BTreeMap<String, String>,
    /// Other fields that the same hardware command must write explicitly
    #[serde(default)]
    pub configuration_fields: Vec<DeviceConfigurationField>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfigurationField {
    pub id: String,
    pub label: String,
    pub help: String,
}
impl VibrationCapabilities {
    // Validate configuration
    pub fn validate_configuration(
        &self,
        values: &std::collections::BTreeMap<String, bool>,
    ) -> Result<()> {
        if values.len() != self.configuration_fields.len()
            || self
                .configuration_fields
                .iter()
                .any(|field| !values.contains_key(&field.id))
        {
            return Err(Error::Protocol(
                "Review and choose every related device setting before applying vibration".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FrameOutputCapabilities {
    pub frame_scheduling: bool,
    pub grouped_key_updates: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub width: u16,
    pub height: u16,
    pub rotation: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub image_slot: Option<u8>,
    pub image_width: Option<u16>,
    pub image_height: Option<u16>,
    pub semantics: Vec<String>,
    #[serde(default)]
    pub preview_hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feedback_surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub directional_switch: Option<DirectionalSwitch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub minimum_manager: Option<String>,
}
/// A switch has independent left/center/right button edges. Each position owns
/// a distinct control ID, so simultaneous directions and their releases retain
/// normal per-control scheduling and held-input ownership.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DirectionalSwitch {
    pub group: String,
    pub position: String,
}
// Check that a stable identifier uses only lowercase ASCII, digits and hyphens
fn stable_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
impl ModelManifest {
    // Parse for the current operation
    pub fn parse(source: &str) -> Result<Self> {
        let value: Self = toml::from_str(source).map_err(|e| Error::Descriptor(e.to_string()))?;
        value.validate()?;
        Ok(value)
    }
    // Validate for the current operation
    pub fn validate(&self) -> Result<()> {
        if let Some(report) = &self.capabilities.firmware_report {
            if !self.capabilities.firmware_query || !(2..=1025).contains(&report.length) {
                return Err(Error::Descriptor("invalid firmware read contract".into()));
            }
        }
        let invalid =
            |message: &str| Err(Error::Descriptor(format!("{}: {message}", self.model_id)));
        if self.schema_version != 1 || !stable_id(&self.model_id) {
            return invalid("schema or model ID");
        }
        if self
            .profile_family
            .as_ref()
            .is_some_and(|id| !stable_id(id))
        {
            return invalid("profile family ID");
        }
        if self.status != "experimental" && self.status != "implemented" {
            return invalid("verification is derived from records, never descriptor status");
        }
        if let Some(keyboard) = &self.capabilities.keyboard {
            keyboard.validate()?;
        }
        if let Some(panel) = &self.capabilities.touch_panel {
            if !self
                .controls
                .iter()
                .any(|c| c.id == panel.gesture_control && c.semantics.iter().any(|s| s == "swipe"))
                || panel.button_controls.is_empty()
                || panel.button_controls.iter().collect::<HashSet<_>>().len()
                    != panel.button_controls.len()
                || panel.button_controls.iter().any(|id| {
                    !self
                        .controls
                        .iter()
                        .any(|c| &c.id == id && c.image_slot.is_some() && c.kind == "touch")
                        || self
                            .controls
                            .iter()
                            .filter(|c| {
                                c.kind == "encoder" && c.feedback_surface.as_ref() == Some(id)
                            })
                            .count()
                            != 1
                })
                || panel
                    .swipe_directions
                    .iter()
                    .any(|d| !["left", "right", "up", "down"].contains(&d.as_str()))
            {
                return invalid("invalid touch panel routing");
            }
        }
        if self.variants.is_empty() || self.controls.is_empty() {
            return invalid("variants and controls required");
        }
        if self.layout.width == 0
            || self.layout.height == 0
            || ![0, 90, 180, 270].contains(&self.layout.rotation)
        {
            return invalid("invalid layout");
        }
        if self.capabilities.max_fps == 0 || self.capabilities.max_fps > 24 {
            return invalid("invalid output rate");
        }
        if let Some(vibration) = &self.capabilities.vibration {
            let mut fields = HashSet::new();
            if vibration.configuration_fields.len() > 16
                || vibration.configuration_fields.iter().any(|field| {
                    field.id.is_empty()
                        || field.id.len() > 64
                        || !field.id.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                        })
                        || !fields.insert(&field.id)
                        || field.label.is_empty()
                        || field.label.len() > 128
                        || field.help.len() > 512
                })
            {
                return invalid("invalid device configuration fields");
            }
        }
        if self
            .capabilities
            .frame_output
            .as_ref()
            .is_some_and(|f| f.grouped_key_updates && !f.frame_scheduling)
        {
            return invalid("grouped keys require device-frame scheduling");
        }
        let mut lighting_ids = HashSet::new();
        if self.capabilities.background != self.capabilities.background_size.is_some()
            || self
                .capabilities
                .background_size
                .is_some_and(|size| size.iter().any(|n| *n == 0 || *n > 4096))
        {
            return invalid("invalid background surface dimensions");
        }
        if self
            .capabilities
            .boot_logo
            .as_ref()
            .is_some_and(|logo| logo.size.iter().any(|n| *n == 0 || *n > 4096))
        {
            return invalid("invalid startup logo dimensions");
        }
        if self.capabilities.lighting != !self.capabilities.lighting_channels.is_empty()
            || self.capabilities.lighting_channels.len() > 32
            || self
                .capabilities
                .lighting_channels
                .iter()
                .any(|id| !stable_id(id) || !lighting_ids.insert(id))
        {
            return invalid("invalid lighting channels");
        }
        if self.capabilities.image_formats.is_empty()
            || self
                .capabilities
                .image_formats
                .iter()
                .any(|f| !["png", "jpeg"].contains(&f.as_str()))
        {
            return invalid("invalid image formats");
        }
        let mut formats = HashSet::new();
        if self.capabilities.media_formats.is_empty()
            || self
                .capabilities
                .media_formats
                .iter()
                .any(|f| !["png", "jpeg", "gif", "mp4"].contains(&f.as_str()) || !formats.insert(f))
            || self
                .capabilities
                .image_formats
                .iter()
                .any(|f| !formats.contains(f))
        {
            return invalid("invalid media input formats");
        }
        let mut variants = HashSet::new();
        for v in &self.variants {
            if !stable_id(&v.id) || !variants.insert(&v.id) || !v.identity.valid() {
                return invalid("invalid or duplicate variant");
            }
        }
        let mut ids = HashSet::new();
        let mut slots = HashSet::new();
        let mut switches: std::collections::BTreeMap<&str, HashSet<&str>> = Default::default();
        for c in &self.controls {
            if !stable_id(&c.id) || !ids.insert(&c.id) {
                return invalid("invalid or duplicate control ID");
            }
            if !["button", "encoder", "touch", "display"].contains(&c.kind.as_str()) {
                return invalid("unknown control kind");
            }
            if c.width == 0
                || c.height == 0
                || u32::from(c.x) + u32::from(c.width) > u32::from(self.layout.width)
                || u32::from(c.y) + u32::from(c.height) > u32::from(self.layout.height)
            {
                return invalid("control outside layout");
            }
            if let Some(slot) = c.image_slot {
                if !slots.insert(slot)
                    || c.image_width.unwrap_or(0) == 0
                    || c.image_height.unwrap_or(0) == 0
                {
                    return invalid("image slot/dimensions");
                }
            }
            if c.semantics.is_empty()
                || c.semantics.iter().any(|s| {
                    ![
                        "press", "release", "pulse", "rotate", "touch", "swipe", "display",
                    ]
                    .contains(&s.as_str())
                })
            {
                return invalid("input semantics");
            }
            if (c.kind == "display" && (c.image_slot.is_none() || c.semantics != ["display"]))
                || (c.kind != "display" && c.semantics.iter().any(|s| s == "display"))
            {
                return invalid("display-only surface cannot claim input semantics");
            }
            if c.kind == "encoder"
                && !self.capabilities.reliable_encoder_release
                && c.semantics.iter().any(|s| s == "release")
            {
                return invalid("encoder cannot claim release");
            }
            if let Some(surface) = &c.feedback_surface {
                if !self
                    .controls
                    .iter()
                    .any(|target| target.id == *surface && target.image_slot.is_some())
                {
                    return invalid("feedback surface must reference a display control");
                }
            }
            if let Some(switch) = &c.directional_switch {
                if c.kind != "button"
                    || !stable_id(&switch.group)
                    || !["left", "center", "right"].contains(&switch.position.as_str())
                    || !c.semantics.iter().any(|s| s == "press")
                    || !c.semantics.iter().any(|s| s == "release")
                    || !switches
                        .entry(&switch.group)
                        .or_default()
                        .insert(&switch.position)
                {
                    return invalid("invalid directional switch position");
                }
            }
        }
        if switches.values().any(|positions| positions.len() != 3) {
            return invalid("directional switch requires three positions");
        }
        Ok(())
    }
    // Construct the control descriptor
    pub fn control(&self, id: &str) -> Result<&Control> {
        self.controls
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| Error::Unsupported(format!("control {id}")))
    }
}

#[cfg(test)]
mod selector_tests {
    use super::*;
    #[test]
    // Verify only current frame capability keys are accepted
    fn only_current_frame_capability_keys_are_accepted() {
        let current = serde_json::json!({"frame_scheduling":true,"grouped_key_updates":false});
        let parsed: FrameOutputCapabilities = serde_json::from_value(current.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), current);
        for value in [
            serde_json::json!({"device_frames":true,"grouped_keys":false}),
            serde_json::json!({"frame_scheduling":true,"grouped_key_updates":false,"device_frames":true}),
        ] {
            assert!(serde_json::from_value::<FrameOutputCapabilities>(value).is_err());
        }
    }

    // Read the actual value used by this assertion
    fn actual() -> HidIdentity {
        HidIdentity {
            vendor_id: 0x5548,
            product_id: 0x1030,
            interface: 0,
            usage_page: 0xffa0,
            usage: 1,
        }
    }
    #[test]
    // Verify exact selectors keep every identity constraint
    fn exact_selectors_keep_every_identity_constraint() {
        let original = actual();
        let selector = HidSelector::from(original.clone());
        assert!(selector.valid() && selector.matches(&original));
        for different in [
            HidIdentity {
                vendor_id: 0x5500,
                ..original.clone()
            },
            HidIdentity {
                product_id: 0x1033,
                ..original.clone()
            },
            HidIdentity {
                interface: 1,
                ..original.clone()
            },
            HidIdentity {
                usage_page: 1,
                ..original.clone()
            },
            HidIdentity {
                usage: 2,
                ..original.clone()
            },
        ] {
            assert!(!selector.matches(&different));
        }
    }
    #[test]
    // Verify vendor range predicate does not invent a concrete interface
    fn vendor_range_predicate_does_not_invent_a_concrete_interface() {
        let mut selector = HidSelector::from(actual());
        selector.interface = None;
        selector.usage_page = None;
        selector.usage_page_min = Some(1026);
        assert!(selector.valid());
        for interface in [-1, 0, 7] {
            assert!(selector.matches(&HidIdentity {
                interface,
                usage_page: 1026,
                ..actual()
            }));
            assert!(!selector.matches(&HidIdentity {
                interface,
                usage_page: 1025,
                ..actual()
            }));
        }
        assert!(!selector.matches(&HidIdentity {
            usage: 2,
            ..actual()
        }));
        selector.usage_page = Some(0xffa0);
        assert!(!selector.valid());
    }
    #[test]
    // Verify overlapping ranges and any interface cannot evade registry collisions
    fn overlapping_ranges_and_any_interface_cannot_evade_registry_collisions() {
        let exact = HidSelector::from(actual());
        let mut range = exact.clone();
        range.interface = None;
        range.usage_page = None;
        range.usage_page_min = Some(1026);
        assert!(range.overlaps(&exact) && exact.overlaps(&range));
        let different_interface = HidSelector {
            interface: Some(7),
            ..exact.clone()
        };
        assert!(!exact.overlaps(&different_interface));
        assert!(range.overlaps(&different_interface));
        assert!(!range.overlaps(&HidSelector {
            usage_page: Some(1025),
            ..exact.clone()
        }));
        assert!(!range.overlaps(&HidSelector {
            product_id: 0x1033,
            ..exact
        }));
    }
}
