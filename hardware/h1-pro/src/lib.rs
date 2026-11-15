use hardware_core::*;
use hardware_protocols::vendor_crt::{Codec, Session};
use std::sync::LazyLock;
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
    // Resolve contract
    fn resolve_contract(
        &self,
        variant_id: &str,
        firmware: Option<&str>,
    ) -> Result<ResolvedDeviceContract> {
        let mut contract = ResolvedDeviceContract::new(self.manifest(), variant_id, firmware)?;
        let modern = firmware.is_some_and(|version| {
            let mut parts = version.split('.');
            let family = parts.next().unwrap_or_default();
            let model = parts.next().unwrap_or_default().replace(' ', "");
            let revision = [parts.next(), parts.next()];
            matches!(family, "V25" | "V3")
                && matches!(model.as_str(), "H1" | "H1Pro")
                && revision.iter().all(|part| {
                    part.is_some_and(|part| {
                        !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
                })
                && parts.next().is_none()
        });
        if modern {
            for control in &mut contract.manifest.controls {
                if control.image_slot.is_some() {
                    control.image_width = Some(64);
                    control.image_height = Some(64);
                }
            }
            contract.key_transform = Some(ImageTransform {
                rotation: 270,
                flip_x: false,
                flip_y: false,
            });
            contract.revision.push_str(":h1-v25-v3");
        } else {
            // The SDK's non-V25/V3 fallback is not evidence that an arbitrary
            // missing or unrecognized firmware string identifies legacy hardware.
            contract.image_output_disabled_reason = Some("H1 Pro image output requires recognized V25/V3 firmware; the legacy image geometry is not positively identified.".into());
            contract
                .manifest
                .capabilities
                .image_output_unavailable_reason = contract.image_output_disabled_reason.clone();
            contract.revision.push_str(":h1-unknown");
        }
        Ok(contract)
    }
    // Create a protocol session for the supplied device identity
    fn session_for(&self, contract: &ResolvedDeviceContract) -> Result<Box<dyn HardwareSession>> {
        contract.validate_for(&MANIFEST)?;
        Ok(Box::new(Session::new_resolved(contract, &CODEC)))
    }
    // Create a device protocol session
    fn session(&self) -> Box<dyn HardwareSession> {
        Box::new(Session::new(&MANIFEST, &CODEC))
    }
}
