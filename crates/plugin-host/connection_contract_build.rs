//! Build/test-only validation. Runtime policy comes only from the embedded config
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub schema_version: u32,
    pub manager_contract_version: u32,
    pub surface_contract_version: u32,
    pub capabilities: Vec<String>,
    pub vendors: Vendors,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vendors {
    pub streamdeck: Vendor,
    pub vsd: Vendor,
    pub openaction: Vendor,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Vendor {
    pub application_version: String,
    pub compatibility_label: String,
    pub sdk_versions: Vec<u32>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema_version: u32,
    pub manager_contract_version: u32,
    pub surface_contract_version: u32,
    pub capabilities: Vec<CapabilityImplementation>,
    pub vendors: Vec<VendorImplementation>,
    pub contract_tests: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityImplementation {
    pub id: String,
    pub implementation: Vec<Implementation>,
    pub tests: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorImplementation {
    pub dialect: String,
    pub application_version: String,
    pub compatibility_label: String,
    pub sdk_versions: Vec<u32>,
    pub implementation: Vec<Implementation>,
    pub tests: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Implementation {
    pub path: String,
    pub symbol: String,
}

// Validate for the current operation
pub fn validate(config: &Value, schema: &Value, registry: &Registry) -> Result<Contract, String> {
    let validator = jsonschema::validator_for(schema).map_err(|e| e.to_string())?;
    let errors: Vec<_> = validator
        .iter_errors(config)
        .map(|e| e.to_string())
        .collect();
    if !errors.is_empty() {
        return Err(format!("Connection contract schema: {}", errors.join("; ")));
    }
    let config: Contract = serde_json::from_value(config.clone()).map_err(|e| e.to_string())?;
    if config.schema_version != 1
        || registry.schema_version != 1
        || config.manager_contract_version != registry.manager_contract_version
        || config.surface_contract_version != registry.surface_contract_version
    {
        return Err("Contract version has no implementation/conformance registration".into());
    }
    let registered: BTreeSet<_> = registry
        .capabilities
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    if registered.len() != registry.capabilities.len() || registry.contract_tests.is_empty() {
        return Err("Duplicate capability registration or missing contract conformance".into());
    }
    for capability in &config.capabilities {
        if !registered.contains(capability.as_str()) {
            return Err(format!(
                "Capability {capability} has no implementation/conformance registration"
            ));
        }
    }
    let mut sdk_versions = BTreeSet::new();
    for (dialect, vendor) in [
        ("streamdeck", &config.vendors.streamdeck),
        ("vsd", &config.vendors.vsd),
        ("openaction", &config.vendors.openaction),
    ] {
        let registered: Vec<_> = registry
            .vendors
            .iter()
            .filter(|entry| entry.dialect == dialect)
            .collect();
        if registered.len() != 1
            || registered[0].application_version != vendor.application_version
            || registered[0].compatibility_label != vendor.compatibility_label
            || registered[0].sdk_versions != vendor.sdk_versions
        {
            return Err(format!(
                "Vendor {dialect} claim has no matching implementation/conformance registration"
            ));
        }
        for sdk in &vendor.sdk_versions {
            if !sdk_versions.insert(sdk) {
                return Err("SDKVersion maps to multiple vendor dialects".into());
            }
        }
    }
    Ok(config)
}

// Validate evidence
pub fn validate_evidence(root: &Path, registry: &Registry) -> Result<(), String> {
    for (implementations, tests) in registry
        .capabilities
        .iter()
        .map(|entry| (&entry.implementation, &entry.tests))
        .chain(
            registry
                .vendors
                .iter()
                .map(|entry| (&entry.implementation, &entry.tests)),
        )
    {
        if implementations.is_empty()
            || tests.is_empty()
            || tests.iter().any(|name| name.is_empty())
        {
            return Err("Every advertised contract requires implementation and executable conformance tests".into());
        }
        for entry in implementations {
            let path = Path::new(&entry.path);
            if !entry.path.starts_with("src/")
                || path
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(format!("Invalid implementation path: {}", entry.path));
            }
            let text = std::fs::read_to_string(root.join(path)).map_err(|e| e.to_string())?;
            if entry.symbol.is_empty() || !text.contains(&entry.symbol) {
                return Err(format!(
                    "Missing implementation {} in {}",
                    entry.symbol, entry.path
                ));
            }
        }
    }
    // Exact test names are checked against the compiled test harness and executed
    // by scripts/check-connection-contract.py. Source substrings are not tests.
    Ok(())
}
