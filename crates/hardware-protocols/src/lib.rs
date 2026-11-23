//! Reusable CRT report framing. Physical-model mappings belong to device crates
pub mod vendor_crt;
use hardware_core::{DeviceTransaction, Error, Result};
pub const PAYLOAD_SIZE: usize = 1024;
// Construct the command representation
pub fn command(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > PAYLOAD_SIZE - 5 {
        return Err(Error::Protocol("command too long".into()));
    }
    let mut report = vec![0; PAYLOAD_SIZE + 1];
    report[1..6].copy_from_slice(b"CRT\0\0");
    report[6..6 + bytes.len()].copy_from_slice(bytes);
    Ok(report)
}
// Transfer the prepared protocol bytes to the device
pub fn transfer(
    header: &[u8],
    data: &[u8],
    commit: bool,
    key: String,
) -> Result<DeviceTransaction> {
    if data.is_empty() || data.len() > 16 * 1024 * 1024 {
        return Err(Error::Protocol("invalid image size".into()));
    }
    let started = std::time::Instant::now();
    let report_count = data.len().div_ceil(PAYLOAD_SIZE) + 1 + usize::from(commit);
    let mut reports = Vec::with_capacity(report_count);
    reports.push(command(header)?);
    for chunk in data.chunks(PAYLOAD_SIZE) {
        let mut report = vec![0; PAYLOAD_SIZE + 1];
        report[1..1 + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }
    if commit {
        reports.push(command(b"STP")?);
    }
    hardware_core::AdapterWork::framed(started, data.len());
    Ok(DeviceTransaction {
        reports,
        coalesce_key: Some(key),
    })
}
// Normalize for the current operation
pub fn normalize(report: &[u8]) -> &[u8] {
    if report.first() == Some(&0) && report.get(1..4) == Some(b"ACK") {
        &report[1..]
    } else {
        report
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify report ID padding and split are correct
    fn report_id_padding_and_split_are_correct() {
        let tx = transfer(b"BAT", &vec![17; 1025], true, "key".into()).unwrap();
        assert_eq!(tx.reports.len(), 4);
        assert!(tx.reports.iter().all(|r| r.len() == 1025 && r[0] == 0));
        assert_eq!(tx.reports[2][1], 17);
        assert_eq!(tx.reports[2][2], 0);
        assert_eq!(&tx.reports[3][6..9], b"STP");
    }
}

#[cfg(test)]
mod framing_allocation_tests;

/// Declare a standard vendor CRT adapter. Models with contract overrides keep
/// explicit implementations rather than hiding their differences in this macro.
#[macro_export]
macro_rules! vendor_crt_model {
    () => {
        use hardware_core::*;
        use std::sync::LazyLock;
        use $crate::vendor_crt::{Codec, Session};
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
                    toml::from_str(include_str!("../verification/plan.toml"))
                        .expect("embedded model plan")
                });
                &PLAN
            }
            // Create a protocol session for the supplied device identity
            fn session_for(
                &self,
                contract: &ResolvedDeviceContract,
            ) -> Result<Box<dyn HardwareSession>> {
                contract.validate_for(&MANIFEST)?;
                Ok(Box::new(Session::new_resolved(contract, &CODEC)))
            }
            // Create a device protocol session
            fn session(&self) -> Box<dyn HardwareSession> {
                Box::new(Session::new(&MANIFEST, &CODEC))
            }
        }
    };
}
