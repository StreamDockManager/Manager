//! Validate platform packages with Manager without installing or executing any code
use serde_json::{json, Value};
use std::path::Path;
use stream_dock_manager_plugin_host::PluginHost;
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--paths") {
        let root = tempfile::tempdir()?;
        let mut results = vec![];
        for path in std::env::args().skip(2) {
            let stage = PluginHost::prepare_install(root.path(), Path::new(&path))?;
            let info = PluginHost::prepared_info(&stage)?;
            let mut zip = zip::ZipArchive::new(std::fs::File::open(&path)?)?;
            let manifest: Value = serde_json::from_reader(
                zip.by_name(&format!("{}.sdPlugin/manifest.json", info.id))?,
            )?;
            assert_eq!(manifest["SDKVersion"], 1);
            assert_eq!(info.dialect, "vsd");
            assert!(info.capabilities.is_some());
            results.push(json!({"id":info.id,"version":info.version,"sdk_version":manifest["SDKVersion"],"dialect":info.dialect,"path":path}));
        }
        if results.is_empty() {
            return Err("Pass explicit local package paths after --paths".into());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"validated":results.len(),"packages":results}))?
        );
        return Ok(());
    }
    Err("Usage: validate_packages --paths <package.zip> [package.zip ...] (packages must target this host platform)".into())
}
