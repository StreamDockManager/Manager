use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMethod {
    Physical,
    Replay,
    Simulated,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ts_rs::TS, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    Skipped,
    Unavailable,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerificationPlan {
    pub revision: String,
    pub model_id: String,
    pub features: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecord {
    pub model_id: String,
    pub variant_id: String,
    pub usb_identity: String,
    pub firmware: String,
    pub driver_revision: String,
    pub plan_revision: String,
    pub platform: String,
    pub architecture: String,
    pub desktop_session: Option<String>,
    pub date: String,
    pub method: VerificationMethod,
    pub reviewed_by: Option<String>,
    pub features: BTreeMap<String, Outcome>,
    pub evidence: Vec<String>,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify verification records preserve physical and simulated provenance
    fn verification_records_preserve_physical_and_simulated_provenance() {
        let mut record = VerificationRecord {
            model_id: "n4-pro".into(),
            variant_id: "vsdinside".into(),
            usb_identity: "5548:1023".into(),
            firmware: "1".into(),
            driver_revision: "x".into(),
            plan_revision: "1".into(),
            platform: "linux".into(),
            architecture: "x64".into(),
            desktop_session: None,
            date: "2026-10-21".into(),
            method: VerificationMethod::Simulated,
            reviewed_by: Some("reviewer".into()),
            features: BTreeMap::from([("keys".into(), Outcome::Pass)]),
            evidence: vec!["test.log".into()],
        };
        let simulated = serde_json::to_value(&record).unwrap();
        assert_eq!(simulated["method"], "simulated");
        assert_eq!(simulated["features"]["keys"], "pass");
        record.method = VerificationMethod::Physical;
        let physical = serde_json::to_value(&record).unwrap();
        assert_eq!(physical["method"], "physical");
        assert_eq!(physical["features"]["keys"], "pass");
        let restored: VerificationRecord = serde_json::from_value(physical).unwrap();
        assert_eq!(restored.method, VerificationMethod::Physical);
    }
}
