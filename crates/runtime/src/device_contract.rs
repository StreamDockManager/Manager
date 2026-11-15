//! The same resolved policy is used for rendering, connected sessions and persistent previews
use crate::{DeviceInfo, Runtime};
use hardware_core::ResolvedDeviceContract;
use sha2::{Digest, Sha256};

// Resolve the supplied device contract request
pub(crate) fn resolve(device: &DeviceInfo) -> Result<ResolvedDeviceContract, String> {
    let module = hardware_registry::modules()
        .into_iter()
        .find(|module| module.manifest().model_id == device.model_id)
        .ok_or("Hardware module is unavailable")?;
    let contract = if device.simulated {
        module.representative_contract()
    } else {
        module.resolve_contract(
            &device.variant_id,
            (!device.firmware.is_empty()).then_some(device.firmware.as_str()),
        )
    };
    contract.map_err(|error| error.to_string())
}
impl Runtime {
    // Hash the resolved hardware contract together with its firmware observation
    pub(crate) fn device_contract_key(&self, device: &DeviceInfo) -> Result<String, String> {
        let contract = resolve(device)?;
        let mut hash = Sha256::new();
        hash.update(contract.identity());
        hash.update(self.firmware_observation_key(&device.id));
        Ok(hex::encode(hash.finalize()))
    }
    // Validate boot contract
    pub(crate) fn validate_boot_contract(
        &self,
        id: &str,
        epoch: u64,
        expected: &str,
    ) -> Result<(), String> {
        if self.connection_epochs.lock().unwrap().get(id) != Some(&epoch) {
            return Err("Startup-logo connection changed. Reopen the preview.".into());
        }
        let device = self
            .state
            .lock()
            .unwrap()
            .devices
            .iter()
            .find(|device| device.id == id && device.connected)
            .cloned()
            .ok_or("Device disconnected before startup-logo upload")?;
        if self.device_contract_key(&device)? != expected {
            return Err("Device variant, firmware observation or codec contract changed. Reopen the startup-logo preview.".into());
        }
        Ok(())
    }
}
