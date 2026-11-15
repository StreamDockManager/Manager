use stream_dock_manager_plugin_runtimes::{Clock, RuntimeKind, Transport};

// Exercise selection through real discovery without network access or an install
// Pin the clock within Node 24's support window so this protocol test does not age out
pub struct ProtocolMetadata;
impl Clock for ProtocolMetadata {
    // Read the current time for this subsystem
    fn now(&self) -> i64 {
        1_793_491_200
    } // 2026-11-01 UTC
}
impl Transport for ProtocolMetadata {
    // Download for the current operation
    fn download(
        &self,
        _: RuntimeKind,
        spec: &stream_dock_manager_downloads::ArtifactSpec,
        cache: &stream_dock_manager_downloads::DownloadCache,
        control: &dyn Fn() -> stream_dock_manager_downloads::Control,
        progress: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<stream_dock_manager_downloads::Artifact, String> {
        let _ = (spec, cache, control, progress);
        Err("Metadata-only fixture does not download artifacts".into())
    }

    // Retrieve the requested value
    fn get(
        &self,
        kind: RuntimeKind,
        url: &str,
        artifact: bool,
        _: usize,
        _: &dyn Fn(u32, Option<u32>) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        assert_eq!(kind, RuntimeKind::Nodejs);
        assert!(
            !artifact,
            "protocol fixtures must never download executable code"
        );
        let value = match url {
            "https://raw.githubusercontent.com/nodejs/Release/main/schedule.json" => {
                serde_json::json!({"v24":{"lts":"2025-10-28","end":"2028-04-30"}})
            }
            "https://nodejs.org/dist/index.json" => {
                serde_json::json!([{"version":"v24.20.0","lts":"Krypton"}])
            }
            _ => return Err(format!("Unexpected protocol fixture URL: {url}")),
        };
        serde_json::to_vec(&value).map_err(|error| error.to_string())
    }
}
