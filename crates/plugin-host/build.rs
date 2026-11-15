mod connection_contract_build;
use connection_contract_build::{validate, validate_evidence, Registry};
use std::{fmt::Write, path::PathBuf};

// Run this executable entry point
fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    for path in [
        "connection-contract.v1.json",
        "connection-contract.schema.json",
        "connection-implementations.v1.json",
        "connection_contract_build.rs",
        "src",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let read = |name| std::fs::read(root.join(name)).expect("Read connection contract input");
    let config = serde_json::from_slice(&read("connection-contract.v1.json"))
        .expect("Parse connection contract");
    let schema = serde_json::from_slice(&read("connection-contract.schema.json"))
        .expect("Parse connection contract schema");
    let registry: Registry = serde_json::from_slice(&read("connection-implementations.v1.json"))
        .expect("Parse implementation registry");
    let config = validate(&config, &schema, &registry).expect("Validate connection contract");
    validate_evidence(&root, &registry).expect("Validate implementation registry");
    let mut source =
        String::from("// Generated at build time from connection-contract.v1.json. Do not edit.\n");
    writeln!(
        source,
        "pub const MANAGER_CONTRACT_VERSION: u32 = {};",
        config.manager_contract_version
    )
    .unwrap();
    writeln!(
        source,
        "pub const SURFACE_CONTRACT_VERSION: u32 = {};",
        config.surface_contract_version
    )
    .unwrap();
    writeln!(
        source,
        "pub const CAPABILITIES: &[&str] = &{:?};",
        config.capabilities
    )
    .unwrap();
    for (name, dialect, vendor) in [
        ("STREAMDECK", "streamdeck", config.vendors.streamdeck),
        ("VSD", "vsd", config.vendors.vsd),
        ("OPENACTION", "openaction", config.vendors.openaction),
    ] {
        writeln!(source, "pub const {name}: VendorContract = VendorContract {{ dialect: {dialect:?}, application_version: {:?}, compatibility_label: {:?}, sdk_versions: &{:?} }};", vendor.application_version, vendor.compatibility_label, vendor.sdk_versions).unwrap();
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out.join("connection_contract.rs"), source)
        .expect("Write embedded connection contract");
}
