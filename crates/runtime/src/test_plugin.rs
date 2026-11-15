//! Explicit current metadata for synthetic browser-plugin fixtures only
use base64::Engine;
use serde_json::{json, Value};
use std::path::Path;

// Convert the imported test plugin representation to the current contract
pub(crate) fn modernize(root: &Path) {
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    write_manifest(root, manifest);
}

// Write manifest
pub(crate) fn write_manifest(root: &Path, mut manifest: Value) {
    assert!(
        manifest["UUID"].is_string(),
        "fixture must declare its UUID"
    );
    manifest["SDKVersion"] = json!(1);
    manifest["StreamDockManager"]["ManifestDialect"] = json!("vsd");
    manifest["Icon"] = json!("fixture-icon.svg");
    std::fs::write(root.join("fixture-icon.svg"), "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"><path fill=\"red\" d=\"M0 0h1v1H0z\"/></svg>").unwrap();
    let asset = json!({"light":"fixture-icon.svg","dark":"fixture-icon.svg"});
    let actions: serde_json::Map<String, Value> = manifest["Actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| {
            let controls: Vec<&str> = action["Controllers"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .map(|value| match value.as_str().unwrap() {
                            "Keypad" => "button",
                            "Encoder" => "encoder",
                            "Touchscreen" => "touch",
                            other => panic!("Declare current fixture control: {other}"),
                        })
                        .collect()
                })
                .unwrap_or_else(|| vec!["button"]);
            let states: Vec<Value> = action["States"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .map(|state| {
                            let image = state["Image"].as_str().unwrap();
                            json!({"light":image,"dark":image})
                        })
                        .collect()
                })
                .unwrap_or_else(|| vec![asset.clone()]);
            let mut value = json!({"control_types":controls,"icon":asset,"states":states});
            if let Some(settings) = action["PropertyInspectorPath"].as_str() {
                value["settings"] = json!(settings);
            }
            (action["UUID"].as_str().unwrap().to_owned(), value)
        })
        .collect();
    let mut capabilities = json!({"version":1,"id":manifest["UUID"],"plugin_version":manifest["Version"],"theme_protocol":1,"settings_protocol":1,"input_protocol":0,"icon":asset,"actions":actions});
    if let Some(settings) = manifest["PropertyInspectorPath"].as_str() {
        capabilities["plugin_settings"] = json!(settings);
    }
    manifest["StreamDockManager"]["Capabilities"] = capabilities.clone();
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&capabilities).unwrap());
    let entry = root.join(manifest["CodePath"].as_str().unwrap());
    let existing = std::fs::read_to_string(&entry).unwrap_or_default();
    let existing = existing
        .split("\n/* CURRENT_TEST_DESCRIPTOR ")
        .next()
        .unwrap()
        .split("\n<!-- CURRENT_TEST_DESCRIPTOR ")
        .next()
        .unwrap();
    let frame = format!("SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1", encoded.len());
    let content = if entry.extension().is_some_and(|value| value == "html") {
        format!("{existing}\n<!-- CURRENT_TEST_DESCRIPTOR {frame} -->")
    } else {
        format!("{existing}\n/* CURRENT_TEST_DESCRIPTOR {frame} */")
    };
    std::fs::write(entry, content).unwrap();
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

// Build a test registration message from the fixture manifest and supplied session token
pub(crate) fn registration(root: &Path, token: &str) -> Value {
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    json!({"event":"registerPlugin","uuid":token,"payload":{"capabilities":manifest["StreamDockManager"]["Capabilities"]}})
}
