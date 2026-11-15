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
        let legacy = self
            .manifest()
            .variants
            .iter()
            .find(|variant| variant.id == variant_id)
            .is_some_and(|variant| variant.identity.vendor_id == 0x6602);
        let size = if legacy { 60 } else { 64 };
        for control in &mut contract.manifest.controls {
            if control.image_slot.is_some() {
                control.image_width = Some(size);
                control.image_height = Some(size);
            }
        }
        contract.key_transform = Some(ImageTransform {
            rotation: if legacy { 0 } else { 90 },
            flip_x: false,
            flip_y: false,
        });
        contract.boot_transform = Some(ImageTransform {
            rotation: 90,
            flip_x: false,
            flip_y: false,
        });
        contract.boot_encoding = Some(if legacy {
            WireImageEncoding::Rgb565Le
        } else {
            WireImageEncoding::Jpeg
        });
        contract.revision.push_str(if legacy {
            ":n3-legacy-6602"
        } else {
            ":n3-modern"
        });
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
