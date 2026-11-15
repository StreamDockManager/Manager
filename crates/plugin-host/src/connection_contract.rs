//! Immutable build-validated compatibility metadata, not user configuration
//! Runtime identity, platform, permissions and presentation never come from it
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorContract {
    pub dialect: &'static str,
    pub application_version: &'static str,
    pub compatibility_label: &'static str,
    pub sdk_versions: &'static [u32],
}
include!(concat!(env!("OUT_DIR"), "/connection_contract.rs"));

// Describe the selected vendor connection contract
pub fn vendor(dialect: &str) -> Option<&'static VendorContract> {
    match dialect {
        "streamdeck" => Some(&STREAMDECK),
        "vsd" => Some(&VSD),
        "openaction" => Some(&OPENACTION),
        _ => None,
    }
}
// Select the vendor contract for the requested SDK version
pub fn vendor_for_sdk(sdk_version: u64) -> Option<&'static VendorContract> {
    u32::try_from(sdk_version).ok().and_then(|sdk| {
        [&STREAMDECK, &VSD]
            .into_iter()
            .find(|vendor| vendor.sdk_versions.contains(&sdk))
    })
}
// Check support for capability
pub fn supports_capability(capability: &str) -> bool {
    CAPABILITIES.contains(&capability)
}
// Construct the Manager registration contract
pub(super) fn manager_registration(run_id: &str, debug_network: bool) -> Value {
    json!({"version":env!("CARGO_PKG_VERSION"), "compatibility":STREAMDECK.compatibility_label,
        "vsd_compatibility":VSD.compatibility_label, "openaction_compatibility":OPENACTION.compatibility_label, "contract_version":MANAGER_CONTRACT_VERSION,
        "capabilities":CAPABILITIES,"theme_rpc":supports_capability("plugin-theme-v1"),
        "run_id":run_id,"debug_network":debug_network})
}
// Choose the default surface for this connection contract
pub(super) fn default_surface(control_id: &str) -> Value {
    json!({"contract_version":SURFACE_CONTRACT_VERSION,"control_id":control_id})
}

#[cfg(test)]
#[path = "connection_contract_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "../connection_contract_build.rs"]
mod validation;
