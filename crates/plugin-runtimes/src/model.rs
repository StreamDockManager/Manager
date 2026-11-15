use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
macro_rules! model { ($($item:item)*) => {$ (#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)] $item)*}; }
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Nodejs,
}
impl RuntimeKind {
    pub const ALL: [Self; 1] = [Self::Nodejs];
    // Return the name used by this implementation
    pub fn name(self) -> &'static str {
        "nodejs"
    }
    // Resolve the runtime release channel
    pub fn channel(self) -> &'static str {
        "lts"
    }
    // Resolve the runtime executable path
    pub fn executable(self) -> &'static str {
        if cfg!(windows) {
            "node.exe"
        } else {
            "node"
        }
    }
    // Build environment overrides for the runtime
    pub fn env(self) -> &'static str {
        "STREAM_DOCK_MANAGER_NODE"
    }
    // Construct the line representation
    pub fn line(self, v: &semver::Version) -> String {
        v.major.to_string()
    }
}
model! {
    pub struct RuntimeRequirement {pub kind: RuntimeKind,pub channel: String,pub version: Option<String>}
    pub struct RuntimeRelease {pub id:String,pub kind:RuntimeKind,pub channel:String,pub line:String,pub version:String,pub platform:String,pub url:String,pub archive_name:String,pub executable:String,pub verification:String,pub provenance:String}
    pub struct RuntimeInstallation {pub id:String,pub kind:RuntimeKind,pub channel:String,pub line:String,pub version:String,pub executable:String,pub source:String,pub verification:String,pub provenance:String}
    pub struct RuntimeSelection {pub kind:RuntimeKind,pub channel:String,pub line:String,pub installation_id:Option<String>,pub external:Option<String>,pub acknowledge_override:bool}
    pub struct RuntimeResolution {pub recommended_line:Option<String>,pub requirement:RuntimeRequirement,pub selected:Option<RuntimeInstallation>,pub state:String,pub message:String,pub overridden:bool}
    pub struct RuntimeJob {pub id:String,pub release_id:String,pub state:String,pub downloaded:u32,pub total:Option<u32>,pub error:Option<String>,
#[serde(default,skip_serializing_if="Option::is_none")] #[ts(optional)] pub kind:Option<RuntimeKind>,
#[serde(default,skip_serializing_if="Option::is_none")] #[ts(optional)] pub line:Option<String>,
#[serde(default,skip_serializing_if="Option::is_none")] #[ts(optional)] pub version:Option<String>}
    pub struct RuntimeLine {pub kind:RuntimeKind,pub channel:String,pub line:String,pub start:String,pub end:Option<String>,pub policy:String}
    pub struct RuntimeStatus {pub lines:Vec<RuntimeLine>,pub releases:Vec<RuntimeRelease>,pub installations:Vec<RuntimeInstallation>,pub defaults:BTreeMap<String,RuntimeSelection>,pub plugins:BTreeMap<String,RuntimeSelection>,pub automatic:bool,#[ts(type="number | null")] pub last_check:Option<i64>,#[ts(type="number")] pub next_check:i64,pub stale:bool,pub error:Option<String>,pub jobs:Vec<RuntimeJob>}
    pub struct RuntimeUsage {pub enabled_preference:bool,pub plugin_id:String,pub name:String,pub running:Option<RuntimeInstallation>,pub selected:RuntimeResolution,pub pending_restart:bool}
}
// Parse the requested runtime version constraint
pub fn version_requirement(value: &str) -> Result<semver::VersionReq, String> {
    // Current explicit declarations support major ranges and npm-style comparator sets
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(", ");
    semver::VersionReq::parse(value)
        .or_else(|_| semver::VersionReq::parse(&normalized))
        .map_err(|e| format!("Invalid runtime version requirement: {e}"))
}
// Resolve the runtime requirement declared by the plugin
pub fn manifest_requirement(
    manifest: &serde_json::Value,
    entry: &std::path::Path,
) -> Result<Option<RuntimeRequirement>, String> {
    let ext = entry.extension().and_then(|s| s.to_str()).unwrap_or("");
    if manifest.pointer("/stream-dock-manager/Runtime").is_some() {
        return Err("Unsupported runtime declaration; use StreamDockManager.Runtime".into());
    }
    let explicit = manifest.pointer("/StreamDockManager/Runtime");
    if let Some(node) = manifest.get("Nodejs") {
        if explicit.is_some() {
            return Err(
                "Declare either vendor Nodejs or StreamDockManager.Runtime, not both".into(),
            );
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase", deny_unknown_fields)]
        struct VendorNode {
            version: String,
            debug: Option<String>,
            generate_profiler_output: Option<bool>,
        }
        let node: VendorNode = serde_json::from_value(node.clone())
            .map_err(|e| format!("Invalid vendor Nodejs declaration: {e}"))?;
        if !matches!(node.version.as_str(), "20" | "24" | "26") {
            return Err("Unsupported vendor Nodejs.Version; expected 20, 24 or 26".into());
        }
        if !["js", "mjs", "cjs"].contains(&ext) {
            return Err("Vendor Nodejs requires a compiled JavaScript entry point".into());
        }
        // Debug arguments apply only to vendor debug mode, which Manager does not
        // enter during normal plugin execution. Never inject these into a process.
        if node
            .debug
            .as_ref()
            .is_some_and(|debug| debug.len() > 4096 || debug.chars().any(char::is_control))
        {
            return Err("Invalid vendor Nodejs.Debug".into());
        }
        if node.generate_profiler_output == Some(true) {
            return Err("Vendor Nodejs.GenerateProfilerOutput is not supported".into());
        }
        return Ok(Some(RuntimeRequirement {
            kind: RuntimeKind::Nodejs,
            channel: RuntimeKind::Nodejs.channel().into(),
            version: Some(node.version),
        }));
    }
    if ["ts", "mts", "cts", "tsx", "jsx"].contains(&ext) {
        return Err("TypeScript execution is unsupported; compile to JavaScript and declare StreamDockManager.Runtime".into());
    }
    if let Some(runtime) = explicit {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase", deny_unknown_fields)]
        struct Declaration {
            kind: RuntimeKind,
            version: String,
            channel: Option<String>,
        }
        let d: Declaration = serde_json::from_value(runtime.clone())
            .map_err(|e| format!("Invalid StreamDockManager.Runtime: {e}"))?;
        if !["js", "mjs", "cjs"].contains(&ext) {
            return Err("Runtime declarations require a compiled JavaScript entry point".into());
        }
        version_requirement(&d.version)?;
        let channel = d.channel.unwrap_or(d.kind.channel().into());
        if channel != d.kind.channel() {
            return Err(format!("Unsupported {} channel {channel}", d.kind.name()));
        }
        return Ok(Some(RuntimeRequirement {
            kind: d.kind,
            channel,
            version: Some(d.version),
        }));
    }
    if ["js", "mjs", "cjs"].contains(&ext) {
        Err("JavaScript plugins require an explicit StreamDockManager.Runtime declaration".into())
    } else {
        Ok(None)
    }
}
