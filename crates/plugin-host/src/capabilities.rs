//! Optional Manager extension contract metadata. This module never establishes package trust
use super::*;
use base64::Engine;
const PREFIX: &[u8] = b"SDMCAP1:";
const SUFFIX: &[u8] = b":ENDSDMCAP1";
const LIMIT: usize = 64 * 1024;
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct ThemedAsset {
    pub light: String,
    pub dark: String,
}
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct ActionCapability {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refresh: Option<RefreshPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub parameter_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feedback_layout: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub control_types: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub additive_dial: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<String>,
    pub icon: ThemedAsset,
    pub states: Vec<ThemedAsset>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RefreshPolicy {
    pub default_ms: u32,
    pub minimum_ms: u32,
    pub maximum_ms: u32,
}
impl RefreshPolicy {
    // Report whether the value satisfies the supported constraints
    fn valid(&self) -> bool {
        self.minimum_ms > 0
            && self.maximum_ms >= self.minimum_ms
            && self.maximum_ms <= 86_400_000
            && (self.default_ms == 0
                || (self.minimum_ms..=self.maximum_ms).contains(&self.default_ms))
    }
}
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema, PartialEq,
)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permissions: Option<Value>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[ts(optional, as = "Option<String>")]
    pub minimum_manager: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub required_capabilities: Vec<String>,
    pub version: u32,
    pub id: String,
    pub plugin_version: String,
    pub theme_protocol: u32,
    pub settings_protocol: u32,
    pub input_protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_settings: Option<String>,
    pub icon: ThemedAsset,
    pub actions: std::collections::BTreeMap<String, ActionCapability>,
}
// Check whether the value is false
fn is_false(value: &bool) -> bool {
    !value
}
// Validate a package-relative resource path
fn relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && !path.contains(['\\', ':', '\0'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !Path::new(path).is_absolute()
}
// Require both themed asset paths to name safe package files
fn asset(root: &Path, value: &ThemedAsset) -> Result<(), String> {
    for path in [&value.light, &value.dark] {
        if !relative(path) {
            return Err("unsafe themed asset path".into());
        }
        if !safe_file(root, path)?.is_file() {
            return Err("themed asset is not a file".into());
        }
    }
    Ok(())
}
// Require any settings entry point to name a safe package file
fn entry(root: &Path, path: &Option<String>) -> Result<(), String> {
    if let Some(path) = path {
        if !relative(path) || !safe_file(root, path)?.is_file() {
            return Err("unsafe settings entrypoint".into());
        }
    }
    Ok(())
}
/// Descriptor absence is a supported vendor contract, not inspection failure
pub(super) enum Inspection {
    VendorBaseline,
    Manager(Box<Capabilities>),
}
impl Inspection {
    // Convert the declared capabilities to the Manager representation
    pub(super) fn into_manager(self) -> Option<Capabilities> {
        match self {
            Self::VendorBaseline => None,
            Self::Manager(value) => Some(*value),
        }
    }
}
// Inspect for the current operation
pub(super) fn inspect(
    root: &Path,
    executable: &Path,
    manifest: &Value,
) -> Result<Inspection, String> {
    if std::fs::metadata(executable)
        .map_err(|e| e.to_string())?
        .len()
        > 256 * 1024 * 1024
    {
        return Err("executable exceeds descriptor inspection limit".into());
    }
    let bytes = std::fs::read(executable).map_err(|e| e.to_string())?;
    let mut descriptor: Option<Capabilities> = None;
    for offset in bytes
        .windows(PREFIX.len())
        .enumerate()
        .filter_map(|(i, w)| (w == PREFIX).then_some(i + PREFIX.len()))
    {
        // Go also retains the prefix constant. Only a complete length prefix is
        // a descriptor candidate; complete but malformed frames are rejected.
        let Some(header) = bytes.get(offset..offset + 9) else {
            continue;
        };
        if header[8] != b':' || !header[..8].iter().all(u8::is_ascii_hexdigit) {
            continue;
        }
        let length = usize::from_str_radix(std::str::from_utf8(&header[..8]).unwrap(), 16)
            .map_err(|_| "invalid descriptor length")?;
        if length > LIMIT * 2 {
            return Err("capability descriptor exceeds limit".into());
        }
        let start = offset + 9;
        let end = start + length;
        if bytes.get(end..end + SUFFIX.len()) != Some(SUFFIX) {
            return Err("truncated capability descriptor".into());
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(&bytes[start..end])
            .map_err(|_| "invalid descriptor encoding")?;
        if data.len() > LIMIT {
            return Err("capability descriptor exceeds limit".into());
        }
        let value: Capabilities = serde_json::from_slice(&data)
            .map_err(|e| format!("invalid capability descriptor: {e}"))?;
        if descriptor.as_ref().is_some_and(|prior| prior != &value) {
            return Err("conflicting executable descriptors".into());
        }
        descriptor = Some(value);
    }
    let Some(value) = descriptor else {
        let extension = &manifest["StreamDockManager"];
        if ["Capabilities", "SetupSlots", "Permissions"]
            .iter()
            .any(|key| extension.get(*key).is_some())
        {
            return Err("Declared Manager capabilities require an executable descriptor".into());
        }
        // Standard vendor packages implement the vendor protocol, not the
        // Manager extension handshake. Absence never grants extension rights
        return Ok(Inspection::VendorBaseline);
    };
    if value.version != 1
        || value.theme_protocol > 1
        || value.settings_protocol > 1
        || value.input_protocol > 1
    {
        return Err("Supported capability descriptor required".into());
    }
    if (value.plugin_settings.is_some()
        || value
            .actions
            .values()
            .any(|action| action.settings.is_some()))
        && value.settings_protocol != 1
    {
        return Err("Settings entrypoints require managed settings protocol 1".into());
    }
    if value.permissions.as_ref() != manifest["StreamDockManager"].get("Permissions") {
        return Err("Package permission declarations differ from the compiled descriptor".into());
    }
    if value.id != manifest["UUID"]
        || value.plugin_version != manifest["Version"]
        || serde_json::to_value(&value).map_err(|e| e.to_string())?
            != manifest["StreamDockManager"]["Capabilities"]
    {
        return Err("manifest and executable capabilities disagree".into());
    }
    if !value.minimum_manager.is_empty()
        && (manifest["StreamDockManager"]["MinimumVersion"] != value.minimum_manager
            || manifest["StreamDockManager"]["RequiredCapabilities"]
                != serde_json::to_value(&value.required_capabilities).map_err(|e| e.to_string())?)
    {
        return Err("compiled Manager requirements disagree with manifest".into());
    }
    let actions = manifest["Actions"].as_array().ok_or("Actions missing")?;
    if value.actions.len() != actions.len() || actions.len() > 256 {
        return Err("descriptor action count disagrees".into());
    }
    asset(root, &value.icon)?;
    entry(root, &value.plugin_settings)?;
    for action in actions {
        let id = action["UUID"].as_str().ok_or("action ID missing")?;
        let declared = value.actions.get(id).ok_or("descriptor action missing")?;
        if declared
            .refresh
            .as_ref()
            .is_some_and(|policy| !policy.valid())
        {
            return Err("invalid refresh policy".into());
        }
        if declared.settings.as_deref() != action["PropertyInspectorPath"].as_str() {
            return Err("settings declaration disagrees".into());
        }
        if declared.states.is_empty() || declared.states.len() > 16 {
            return Err("invalid descriptor state count".into());
        }
        entry(root, &declared.feedback_layout)?;
        if declared
            .control_types
            .iter()
            .any(|kind| !["button", "encoder", "touch"].contains(&kind.as_str()))
        {
            return Err("invalid action capabilities".into());
        }
        asset(root, &declared.icon)?;
        entry(root, &declared.settings)?;
        for state in &declared.states {
            asset(root, state)?;
        }
    }
    Ok(Inspection::Manager(Box::new(value)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn vendor_baseline_cannot_declare_manager_permission_or_setup_authority() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("fixture");
        std::fs::write(&executable, b"no embedded descriptor").unwrap();
        for property in ["Permissions", "SetupSlots"] {
            let mut manifest = serde_json::json!({"StreamDockManager":{}});
            manifest["StreamDockManager"][property] = serde_json::json!({"version":1});
            assert!(inspect(root.path(), &executable, &manifest).is_err());
        }
    }

    #[test]
    // Verify refresh metadata checks inclusive bounds and off defaults
    fn refresh_metadata_checks_inclusive_bounds_and_off_defaults() {
        let (dir, mut manifest) = fixture();
        for (default, minimum, maximum, valid) in [
            (3_600_000, 1000, 86_400_000, true),
            (0, 1000, 1000, true),
            (500, 1000, 10000, false),
            (1000, 0, 10000, false),
            (1000, 1000, 86_400_001, false),
        ] {
            manifest["StreamDockManager"]["Capabilities"]["actions"]["fixture.action"]["refresh"] =
                json!({"default_ms":default,"minimum_ms":minimum,"maximum_ms":maximum});
            let executable = dir.path().join("plugin");
            std::fs::write(
                &executable,
                frame(&manifest["StreamDockManager"]["Capabilities"]),
            )
            .unwrap();
            assert_eq!(inspect(dir.path(), &executable, &manifest).is_ok(), valid);
        }
    }
    use super::*;
    // Construct the frame capability description
    fn frame(value: &Value) -> Vec<u8> {
        let data =
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(value).unwrap());
        format!("SDMCAP1:{:08x}:{}:ENDSDMCAP1", data.len(), data).into_bytes()
    }
    // Construct the isolated test fixture
    fn fixture() -> (tempfile::TempDir, Value) {
        let dir = tempfile::tempdir().unwrap();
        for path in ["light.svg", "dark.svg", "settings.html"] {
            std::fs::write(dir.path().join(path), "fixture").unwrap();
        }
        let assets = json!({"light":"light.svg","dark":"dark.svg"});
        let capability = json!({"version":1,"id":"fixture","plugin_version":"0.1.0","theme_protocol":1,"settings_protocol":1,"input_protocol":0,"icon":assets,"actions":{"fixture.action":{"settings":"settings.html","icon":assets,"states":[assets]}}});
        let manifest = json!({"UUID":"fixture","Version":"0.1.0","Actions":[{"UUID":"fixture.action","PropertyInspectorPath":"settings.html"}],"StreamDockManager":{"Capabilities":capability}});
        (dir, manifest)
    }
    #[test]
    // Verify frames are bounded and manifest agreement is required
    fn frames_are_bounded_and_manifest_agreement_is_required() {
        let (dir, mut manifest) = fixture();
        let executable = dir.path().join("plugin");
        std::fs::write(
            &executable,
            frame(&manifest["StreamDockManager"]["Capabilities"]),
        )
        .unwrap();
        assert!(inspect(dir.path(), &executable, &manifest)
            .unwrap()
            .into_manager()
            .is_some());
        manifest["Version"] = json!("9.0.0");
        assert!(inspect(dir.path(), &executable, &manifest).is_err());
        std::fs::write(&executable, b"SDMCAP1:ffffffff:").unwrap();
        assert!(inspect(dir.path(), &executable, &manifest).is_err());
        std::fs::write(&executable, b"legacy executable").unwrap();
        assert!(inspect(dir.path(), &executable, &manifest).is_err());
    }
    #[test]
    // Verify unsafe paths and conflicting architecture descriptors are rejected
    fn unsafe_paths_and_conflicting_architecture_descriptors_are_rejected() {
        let (dir, mut manifest) = fixture();
        let executable = dir.path().join("plugin");
        let valid = frame(&manifest["StreamDockManager"]["Capabilities"]);
        manifest["StreamDockManager"]["Capabilities"]["icon"]["light"] = json!("../escape.svg");
        let invalid = frame(&manifest["StreamDockManager"]["Capabilities"]);
        std::fs::write(&executable, &invalid).unwrap();
        assert!(inspect(dir.path(), &executable, &manifest).is_err());
        std::fs::write(&executable, [valid, invalid].concat()).unwrap();
        assert!(inspect(dir.path(), &executable, &manifest).is_err());
    }
    #[test]
    // Verify settings entrypoints require managed protocol
    fn settings_entrypoints_require_managed_protocol() {
        let (dir, mut manifest) = fixture();
        manifest["StreamDockManager"]["Capabilities"]["settings_protocol"] = json!(0);
        let executable = dir.path().join("plugin");
        std::fs::write(
            &executable,
            frame(&manifest["StreamDockManager"]["Capabilities"]),
        )
        .unwrap();
        assert!(inspect(dir.path(), &executable, &manifest).is_err());
    }
    #[test]
    // Verify unsupported protocol is rejected and identical universal frames are valid
    fn unsupported_protocol_is_rejected_and_identical_universal_frames_are_valid() {
        let (dir, mut manifest) = fixture();
        let executable = dir.path().join("plugin");
        let valid = frame(&manifest["StreamDockManager"]["Capabilities"]);
        std::fs::write(&executable, [valid.clone(), valid].concat()).unwrap();
        assert!(inspect(dir.path(), &executable, &manifest)
            .unwrap()
            .into_manager()
            .is_some());
        manifest["StreamDockManager"]["Capabilities"]["theme_protocol"] = json!(2);
        std::fs::write(
            &executable,
            frame(&manifest["StreamDockManager"]["Capabilities"]),
        )
        .unwrap();
        assert!(inspect(dir.path(), &executable, &manifest).is_err());
    }
}

#[cfg(test)]
mod script_descriptor_tests {
    use super::*;
    #[test]
    // Verify script metadata uses the same bound descriptor validation
    fn script_metadata_uses_the_same_bound_descriptor_validation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("icon.svg"), "<svg/>").unwrap();
        let asset = json!({"light":"icon.svg","dark":"icon.svg"});
        let capabilities = json!({"version":1,"id":"fixture","plugin_version":"0.1.0","theme_protocol":1,"settings_protocol":1,"input_protocol":0,"icon":asset,"actions":{"fixture.action":{"icon":asset,"states":[asset]}}});
        let mut manifest = json!({"UUID":"fixture","Name":"Fixture","Version":"0.1.0","SDKVersion":1,"CodePath":"main.mjs","Actions":[{"UUID":"fixture.action","Name":"Action"}],"StreamDockManager":{"ManifestDialect":"vsd","Capabilities":capabilities,"Runtime":{"Kind":"nodejs","Version":">=22, <27","Channel":"lts"}}});
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&capabilities).unwrap());
        let program = format!(
            "/* SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1 */\nconsole.log('fixture');",
            encoded.len()
        );
        std::fs::write(root.path().join("main.mjs"), program).unwrap();
        std::fs::write(root.path().join("manifest.json"), manifest.to_string()).unwrap();
        assert!(super::super::read_plugin(root.path())
            .unwrap()
            .info
            .capabilities
            .is_some());
        manifest["Version"] = json!("9.0.0");
        std::fs::write(root.path().join("manifest.json"), manifest.to_string()).unwrap();
        assert!(super::super::read_plugin(root.path()).is_err());
        std::fs::write(root.path().join("main.mjs"), "console.log('legacy');").unwrap();
        assert!(super::super::read_plugin(root.path()).is_err());
    }
}
