//! Official current Elgato manifest shape, separate from Manager extensions and VSD
//! The immutable schema is compiled once; plugin-supplied `$schema` never loads a URL
use super::*;
use std::sync::OnceLock;

const SCHEMA: &str =
    include_str!("../../../tests/fixtures/plugins/vendor/elgato/manifest-schema/manifest.json");

// Initialize and reuse the pinned manifest schema validator
fn validator() -> &'static jsonschema::Validator {
    static VALIDATOR: OnceLock<jsonschema::Validator> = OnceLock::new();
    VALIDATOR.get_or_init(|| {
        let schema: Value = serde_json::from_str(SCHEMA).expect("pinned Elgato schema JSON");
        jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .build(&schema)
            .expect("pinned Elgato manifest schema compiles")
    })
}

/// Called only after executable descriptor inspection. A claimed but invalid
/// descriptor must never turn off vendor validation or confer extension rights.
pub(super) fn validate(
    manifest: &Value,
    id: &str,
    dialect: &str,
    inspection: &capabilities::Inspection,
) -> Result<(), String> {
    if dialect != "streamdeck" || matches!(inspection, capabilities::Inspection::Manager(_)) {
        return Ok(());
    }
    let mut baseline = manifest.clone();
    let object = baseline
        .as_object_mut()
        .ok_or("Manifest must be an object")?;
    if let Some(extension) = object.remove("StreamDockManager") {
        validate_extension(&extension)?;
    }
    // Package identity has already passed namespace, package-name and spoofing
    // checks. This adapter never rewrites the package or fabricates another field.
    object.entry("UUID").or_insert_with(|| json!(id));
    let errors: Vec<_> = validator()
        .iter_errors(&baseline)
        .take(4)
        .map(|error| {
            let path = error.instance_path().to_string();
            let path = if path.is_empty() { "/" } else { &path };
            // Never echo manifest values; invalid input can contain private data
            format!(
                "{}: {}",
                bounded(path),
                bounded(&error.masked().to_string())
            )
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Elgato manifest does not match @elgato/schemas 0.6.0: {}",
            errors.join("; ")
        ))
    }
}

// Enforce the supported bounds of the supplied value
fn bounded(value: &str) -> String {
    value.chars().take(256).collect()
}

// Validate extension
fn validate_extension(extension: &Value) -> Result<(), String> {
    let extension = extension
        .as_object()
        .ok_or("StreamDockManager must be an object")?;
    for (field, value) in extension {
        let valid = match field.as_str() {
            "ManifestDialect" => value.as_str() == Some("streamdeck"),
            "MinimumVersion" => value
                .as_str()
                .is_some_and(|v| semver::Version::parse(v).is_ok()),
            "RequiredCapabilities" => value
                .as_array()
                .is_some_and(|values| values.iter().all(|value| value.as_str().is_some())),
            // Parsed with deny_unknown_fields by the shared runtime resolver
            "Runtime" => value.is_object(),
            // Descriptor inspection must return Manager before this branch
            _ => false,
        };
        if !valid {
            return Err(format!(
                "Unsupported or malformed StreamDockManager.{}",
                bounded(field)
            ));
        }
    }
    Ok(())
}

/// Shape validity does not imply implementation of the requested host behavior
/// Return field-specific limitations rather than silently dropping declarations
pub(super) fn unsupported(manifest: &Value) -> Vec<(String, &'static str, bool)> {
    let mut fields = Vec::new();
    if manifest.get("DefaultWindowSize").is_some() {
        fields.push((
            "DefaultWindowSize".into(),
            "Popup sizes have a bounded macOS isolated-window implementation under qualification; Windows/Linux inspector popups remain blocked",
            false,
        ));
    }
    if manifest["Profiles"]
        .as_array()
        .is_some_and(|profiles| !profiles.is_empty())
    {
        fields.push((
            "Profiles".into(),
            "Bundled profiles require separate user confirmation and matching hardware: only the source-backed single-page V2 keypad subset with confined Unix/Windows local-disk reads is supported; Readonly, V3, resources, folders and multipage archives remain unsupported",
            manifest["Profiles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|profile| profile["Readonly"] == true),
        ));
    }
    if let Some(actions) = manifest["Actions"].as_array() {
        for (index, action) in actions.iter().enumerate() {
            let prefix = format!("Actions[{index}]");
            if action["States"]
                .as_array()
                .is_some_and(|states| states.len() > 64)
            {
                fields.push((
                    format!("{prefix}.States"),
                    "Manager supports at most 64 states per action",
                    true,
                ));
            }
            if let Some(encoder) = action["Encoder"].as_object() {
                for field in encoder.keys() {
                    let limitation = match field.as_str() {
                        "background" if !encoder.contains_key("layout") => "Encoder.background needs an explicit Encoder.layout; Manager does not assume a vendor default layout",
                        _ => continue,
                    };
                    fields.push((format!("{prefix}.Encoder.{field}"), limitation, false));
                }
            }
            if action["SupportedInKeyLogicActions"] == true {
                fields.push((
                    format!("{prefix}.SupportedInKeyLogicActions"),
                    "Manager key logic has configurable timings and nested multi-action events; exact Stream Deck Key Logic event behavior remains unqualified",
                    false,
                ));
            }
        }
    }
    fields
}

/// OpenAction's documented shape is separate from the pinned Elgato schema
/// A bounded OpenDeck import adapter also accepts numeric font sizes, one-to-four-part
/// vendor versions and legacy Software metadata found in distributed plugins.
/// These are compatibility inputs, not additions to the OpenAction API contract
/// Unknown fields remain rejected
pub(super) fn validate_openaction(manifest: &Value, id: &str) -> Result<(), String> {
    // Require an object for this manifest field
    fn object(value: &Value, allowed: &[&str]) -> Result<(), String> {
        let fields = value.as_object().ok_or("OpenAction object required")?;
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err("Unsupported OpenAction manifest field".into());
        }
        Ok(())
    }
    // Resolve the text value for this operation
    fn text(value: &Value) -> bool {
        value.as_str().is_some_and(|s| {
            !s.trim().is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control)
        })
    }
    // Validate an optional manifest field
    fn optional(value: &Value, strings: &[&str], booleans: &[&str]) -> Result<(), String> {
        for key in strings {
            if value.get(*key).is_some_and(|v| {
                !v.is_string()
                    && !(v.is_null()
                        && matches!(
                            *key,
                            "CategoryIcon"
                                | "PropertyInspectorPath"
                                | "CodePath"
                                | "CodePathMac"
                                | "CodePathWin"
                                | "CodePathLin"
                        ))
            }) {
                return Err(format!("Invalid OpenAction {key}"));
            }
        }
        for key in booleans {
            if value.get(*key).is_some_and(|v| !v.is_boolean()) {
                return Err(format!("Invalid OpenAction {key}"));
            }
        }
        Ok(())
    }
    object(
        manifest,
        &[
            "UUID",
            "Name",
            "Author",
            "Version",
            "Icon",
            "Category",
            "CategoryIcon",
            "PropertyInspectorPath",
            "HasSettingsInterface",
            "ApplicationsToMonitor",
            "Actions",
            "OS",
            "CodePath",
            "CodePaths",
            "CodePathWin",
            "CodePathMac",
            "CodePathLin",
            "StreamDockManager",
            "Description",
            "URL",
            "$schema",
            "Nodejs",
            "Software",
        ],
    )?;
    for field in ["Name", "Author", "Version", "Icon"] {
        if !text(&manifest[field]) {
            return Err(format!("OpenAction {field} required"));
        }
    }
    let version = manifest["Version"].as_str().unwrap();
    if semver::Version::parse(version).is_err() && super::manifest::vendor_version(version).is_err()
    {
        return Err(
            "Plugin Version must be SemVer or one to four numeric vendor components".into(),
        );
    }
    // OpenDeck imports may retain Stream Deck Software metadata. Validate its
    // shape, but do not interpret that product version as an OpenAction API level.
    if let Some(software) = manifest.get("Software") {
        object(software, &["MinimumVersion"])?;
        super::manifest::vendor_version(
            software["MinimumVersion"]
                .as_str()
                .ok_or("Invalid OpenDeck Software.MinimumVersion")?,
        )?;
    }
    optional(
        manifest,
        &[
            "Category",
            "Description",
            "URL",
            "$schema",
            "CategoryIcon",
            "PropertyInspectorPath",
            "CodePath",
            "CodePathMac",
            "CodePathWin",
            "CodePathLin",
        ],
        &["HasSettingsInterface"],
    )?;
    if let Some(paths) = manifest.get("CodePaths") {
        object(
            paths,
            &[
                "x86_64-pc-windows-msvc",
                "x86_64-apple-darwin",
                "aarch64-apple-darwin",
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
            ],
        )?;
        if paths
            .as_object()
            .unwrap()
            .values()
            .any(|v| !v.is_null() && !text(v))
        {
            return Err("Invalid OpenAction CodePaths".into());
        }
    }
    let platforms = manifest["OS"]
        .as_array()
        .filter(|p| !p.is_empty() && p.len() <= 3)
        .ok_or("OpenAction OS required")?;
    let mut seen = std::collections::HashSet::new();
    for platform in platforms {
        object(platform, &["Platform", "Version", "MinimumVersion"])?;
        if platform.get("Version").is_some_and(|v| !v.is_null())
            && platform
                .get("MinimumVersion")
                .is_some_and(|v| !v.is_null() && v != &platform["Version"])
        {
            return Err("Conflicting OpenAction OS versions".into());
        }
        let name = platform["Platform"]
            .as_str()
            .ok_or("OpenAction Platform required")?;
        if !matches!(name, "mac" | "windows" | "linux") || !seen.insert(name) {
            return Err("Invalid OpenAction Platform".into());
        }
        if let Some(version) = platform
            .get("Version")
            .filter(|v| !v.is_null())
            .or_else(|| platform.get("MinimumVersion").filter(|v| !v.is_null()))
        {
            super::manifest::vendor_version(
                version.as_str().ok_or("Invalid OpenAction OS Version")?,
            )?;
        }
    }
    let actions = manifest["Actions"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 1024)
        .ok_or("OpenAction Actions required")?;
    for action in actions {
        object(
            action,
            &[
                "Name",
                "UUID",
                "Tooltip",
                "Icon",
                "DisableAutomaticStates",
                "VisibleInActionsList",
                "SupportedInMultiActions",
                "PropertyInspectorPath",
                "Controllers",
                "States",
            ],
        )?;
        for field in ["Name", "UUID", "Icon"] {
            if !text(&action[field]) {
                return Err(format!("OpenAction action {field} required"));
            }
        }
        if !action["UUID"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{id}."))
        {
            return Err("Invalid OpenAction action namespace".into());
        }
        optional(
            action,
            &["Tooltip", "PropertyInspectorPath"],
            &[
                "DisableAutomaticStates",
                "VisibleInActionsList",
                "SupportedInMultiActions",
            ],
        )?;
        if let Some(controllers) = action.get("Controllers") {
            let controllers = controllers
                .as_array()
                .filter(|v| !v.is_empty() && v.len() <= 2)
                .ok_or("Invalid OpenAction Controllers")?;
            let mut seen = std::collections::HashSet::new();
            if controllers.iter().any(|c| {
                c.as_str()
                    .is_none_or(|c| !matches!(c, "Keypad" | "Encoder") || !seen.insert(c))
            }) {
                return Err("Invalid OpenAction Controllers".into());
            }
        }
        let states = action["States"]
            .as_array()
            .filter(|s| !s.is_empty() && s.len() <= 64)
            .ok_or("OpenAction States required (1–64)")?;
        for state in states {
            object(
                state,
                &[
                    "Image",
                    "Name",
                    "Title",
                    "ShowTitle",
                    "TitleColor",
                    "TitleAlignment",
                    "FontStyle",
                    "FontSize",
                    "FontUnderline",
                ],
            )?;
            optional(
                state,
                &[
                    "Image",
                    "Name",
                    "Title",
                    "TitleColor",
                    "TitleAlignment",
                    "FontStyle",
                ],
                &["ShowTitle", "FontUnderline"],
            )?;
            if state.get("FontSize").is_some_and(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                    .is_none_or(|s| s == 0 || s > 256)
            }) {
                return Err("Invalid OpenAction FontSize".into());
            }
            if state
                .get("TitleAlignment")
                .is_some_and(|v| !matches!(v.as_str(), Some("top" | "middle" | "bottom")))
            {
                return Err("Invalid OpenAction TitleAlignment".into());
            }
            if state.get("FontStyle").is_some_and(|v| {
                !matches!(
                    v.as_str(),
                    Some("Regular" | "Bold" | "Italic" | "Bold Italic")
                )
            }) {
                return Err("Invalid OpenAction FontStyle".into());
            }
        }
    }
    Ok(())
}

/// Project-owned test manifests use the current vendor shape, never upstream edits
#[cfg(test)]
pub(super) fn fixture(id: &str, entry: &str) -> Value {
    json!({
        "UUID": id, "Name": "Vendor test fixture", "Version": "0.1.0.0",
        "Author": "Stream Dock Manager contributors",
        "Description": "Disposable vendor protocol test fixture",
        "Icon": "icon", "SDKVersion": 2, "CodePath": entry,
        "Software": {"MinimumVersion": "6.9"},
        "OS": [{"Platform":"mac", "MinimumVersion":"12"}, {"Platform":"windows", "MinimumVersion":"10"}],
        "Actions": [{"UUID":format!("{id}.action"), "Name":"Action", "Icon":"icon", "Controllers":["Keypad"], "States":[{"Image":"icon"}]}]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Read the bounded counter used by the manifest
    fn counter() -> Value {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/plugins/vendor/elgato/counter/manifest.json"
        ))
        .unwrap()
    }

    // Check for the current operation
    fn check(value: &Value) -> Result<(), String> {
        validate(
            value,
            "com.elgato.counter",
            "streamdeck",
            &capabilities::Inspection::VendorBaseline,
        )
    }

    #[test]
    // Verify untouched current counter matches official schema without rewriting
    fn untouched_current_counter_matches_official_schema_without_rewriting() {
        let value = counter();
        let before = value.clone();
        check(&value).unwrap();
        assert_eq!(value, before);
    }

    #[test]
    // Verify current schema enforces nested types required fields and unknown fields
    fn current_schema_enforces_nested_types_required_fields_and_unknown_fields() {
        for (field, value) in [
            ("Author", json!(null)),
            ("Version", json!("0.1.0")),
            ("DefaultWindowSize", json!([400])),
            ("Unexpected", json!(true)),
        ] {
            let mut invalid = counter();
            invalid[field] = value;
            assert!(check(&invalid).is_err(), "{field}");
        }
        for (pointer, value) in [
            ("/OS/0/Platform", json!("linux")),
            ("/OS/0/MinimumVersion", json!(12)),
            ("/Actions/0/States", json!([])),
            ("/Actions/0/Controllers", json!(["Keypad", "Keypad"])),
            ("/Nodejs/Version", json!("22")),
        ] {
            let mut invalid = counter();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(check(&invalid).is_err(), "{pointer}");
        }
        for field in [
            "Author",
            "Description",
            "CodePath",
            "Icon",
            "OS",
            "Software",
        ] {
            let mut invalid = counter();
            invalid.as_object_mut().unwrap().remove(field);
            assert!(check(&invalid).is_err(), "{field}");
        }
        for (field, value) in [
            ("FontSize", json!("12")),
            ("FontStyle", json!("Book")),
            ("Unexpected", json!(true)),
        ] {
            let mut invalid = counter();
            invalid["Actions"][0]["States"][0][field] = value;
            assert!(check(&invalid).is_err(), "{field}");
        }
    }

    #[test]
    // Verify official version branches gate node controllers and fields
    fn official_version_branches_gate_node_controllers_and_fields() {
        for version in [
            "6.4", "6.5", "6.6", "6.7", "6.8", "6.9", "7.0", "7.1", "7.2", "7.3", "7.4", "7.5",
            "7.6",
        ] {
            let mut value = counter();
            value["SDKVersion"] = json!(2);
            value["Software"]["MinimumVersion"] = json!(version);
            check(&value).unwrap_or_else(|error| panic!("{version}: {error}"));
        }
        let mut value = counter();
        value["Software"]["MinimumVersion"] = json!("6.9");
        value["Nodejs"]["Version"] = json!("24");
        assert!(check(&value).is_err());
        value["Software"]["MinimumVersion"] = json!("7.1");
        check(&value).unwrap();
        value["Nodejs"]["Version"] = json!("20");
        value["Software"]["MinimumVersion"] = json!("6.9");
        value["Actions"][0]["SupportedInKeyLogicActions"] = json!(false);
        assert!(check(&value).is_err());
        value["Software"]["MinimumVersion"] = json!("7.0");
        check(&value).unwrap();
        value["Actions"][0]["Controllers"] = json!(["Neo"]);
        assert!(check(&value).is_err());
        value["Software"]["MinimumVersion"] = json!("7.6");
        check(&value).unwrap();
        value["Software"]["MinimumVersion"] = json!("9.0");
        assert!(check(&value).is_err());
    }

    #[test]
    // Verify package identity adapter and manager metadata do not weaken vendor shape
    fn package_identity_adapter_and_manager_metadata_do_not_weaken_vendor_shape() {
        let mut value = counter();
        value.as_object_mut().unwrap().remove("UUID");
        let before = value.clone();
        check(&value).unwrap();
        assert_eq!(
            value, before,
            "identity adaptation must not modify the input"
        );
        value["StreamDockManager"] = json!({"ManifestDialect":"streamdeck"});
        check(&value).unwrap();
        value["Author"] = json!(null);
        assert!(
            check(&value).is_err(),
            "a dialect marker cannot bypass the schema"
        );
        value = counter();
        value["StreamDockManager"] = json!({"Unexpected":true});
        assert!(check(&value).is_err());
        value["StreamDockManager"] = json!(false);
        assert!(check(&value).is_err());
        value["StreamDockManager"] = json!({"Capabilities":{}});
        assert!(
            check(&value).is_err(),
            "unvalidated claims cannot select Manager shape"
        );
    }

    #[test]
    // Verify vendor schema is separate from VSD and validated manager extensions
    fn vendor_schema_is_separate_from_vsd_and_validated_manager_extensions() {
        let value = json!({"Version":"0.1.0","SDKVersion":1});
        validate(
            &value,
            "fixture",
            "vsd",
            &capabilities::Inspection::VendorBaseline,
        )
        .unwrap();
        validate(
            &value,
            "fixture",
            "streamdeck",
            &capabilities::Inspection::Manager(Box::default()),
        )
        .unwrap();
        assert!(check(&value).is_err());
    }

    #[test]
    // Verify schema errors are bounded and do not echo private values
    fn schema_errors_are_bounded_and_do_not_echo_private_values() {
        let mut value = counter();
        value["Author"] = json!({"private":"do-not-print-this-value"});
        let error = check(&value).unwrap_err();
        assert!(error.contains("Author"));
        assert!(!error.contains("do-not-print-this-value"));
        assert!(error.len() < 3000);
    }

    #[test]
    // Verify unsupported semantics report fields without rejecting supported options
    fn unsupported_semantics_report_fields_without_rejecting_supported_options() {
        let mut value = counter();
        value["Profiles"] = json!([]);
        value["Actions"][0]["DisableCaching"] = json!(true);
        value["Actions"][0]["SupportedInKeyLogicActions"] = json!(false);
        value["Actions"][0]["OS"] = json!(["windows"]);
        value["Actions"][0]["Encoder"] = json!({"layout":"$A1", "background":"background", "TriggerDescription":{"Push":"Mute"}});
        value["Actions"][0]["States"][0]["MultiActionImage"] = json!("multi-action");
        check(&value).unwrap();
        assert!(unsupported(&value).is_empty());

        value["DefaultWindowSize"] = json!([500, 650]);
        value["Profiles"] = json!([{"Name":"profile","DeviceType":0}]);
        value["Actions"][0]["SupportedInKeyLogicActions"] = json!(true);
        value["Actions"][0]["Encoder"]
            .as_object_mut()
            .unwrap()
            .remove("layout");
        value["Actions"][0]["Encoder"]["Icon"] = json!("dial-icon");
        value["Actions"][0]["Encoder"]["StackColor"] = json!("#ffffff");
        let limitations = unsupported(&value);
        let fields: Vec<_> = limitations
            .iter()
            .map(|(field, _, _)| field.as_str())
            .collect();
        for field in [
            "DefaultWindowSize",
            "Profiles",
            "Actions[0].Encoder.background",
            "Actions[0].SupportedInKeyLogicActions",
        ] {
            assert!(fields.contains(&field), "{field}");
        }
        assert!(limitations.iter().all(|(_, _, blocker)| !blocker));
    }

    #[test]
    // Verify preflight and package reads enforce shape without rewriting vendor input
    fn preflight_and_package_reads_enforce_shape_without_rewriting_vendor_input() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("org.vendor.schema.sdPlugin");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("plugin.html"), "<!doctype html>").unwrap();
        let mut value = fixture("org.vendor.schema", "plugin.html");
        value.as_object_mut().unwrap().remove("UUID");
        let write = |value: &Value| {
            let bytes = serde_json::to_vec(value).unwrap();
            std::fs::write(root.join("manifest.json"), &bytes).unwrap();
            bytes
        };
        let original = write(&value);
        assert!(preflight::inspect(&root).can_install);
        assert_eq!(read_plugin(&root).unwrap().info.id, "org.vendor.schema");
        assert_eq!(std::fs::read(root.join("manifest.json")).unwrap(), original);
        value["Actions"][0]["States"][0]["FontSize"] = json!("12");
        write(&value);
        assert!(read_plugin(&root).is_err());
        let report = preflight::inspect(&root);
        assert!(!report.can_install);
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "manifest_schema"));
        value = fixture("org.vendor.schema", "plugin.html");
        value["StreamDockManager"] = json!({"Capabilities":{}});
        write(&value);
        assert!(read_plugin(&root).is_err());
        let report = preflight::inspect(&root);
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "descriptor" && check.level == "blocker"));
        value = fixture("org.vendor.schema", "plugin.html");
        value["Software"]["MinimumVersion"] = json!("7.2");
        write(&value);
        let report = preflight::inspect(&root);
        assert!(
            report.can_install,
            "valid shape is separate from host capability"
        );
        assert!(!report.can_enable);
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "vendor_contract" && check.level == "blocker"));
    }

    #[test]
    // Verify required behavior blocks but optional presentation warns
    fn required_behavior_blocks_but_optional_presentation_warns() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("plugin.html"), "<!doctype html>").unwrap();
        let mut value = fixture("org.vendor.schema", "plugin.html");
        value["DefaultWindowSize"] = json!([500, 650]);
        value["Actions"][0]["Encoder"] = json!({"StackColor":"#ffffff"});
        value["Profiles"] = json!([{"Name":"profile", "DeviceType":0, "AutoInstall":false}]);
        let inspect = |value: &Value| {
            std::fs::write(
                temp.path().join("manifest.json"),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
            preflight::inspect(temp.path())
        };
        let report = inspect(&value);
        assert!(report.can_install);
        let optional: Vec<_> = report
            .checks
            .iter()
            .filter(|check| check.code == "unsupported_manifest")
            .collect();
        assert_eq!(optional.len(), 2);
        assert!(optional.iter().all(|check| check.level == "warning"));
        for field in ["DefaultWindowSize", "Profiles"] {
            assert!(optional.iter().any(|check| check.message.contains(field)));
        }
        assert!(optional
            .iter()
            .all(|check| !check.message.contains("StackColor")));
        value["Profiles"][0]["Readonly"] = json!(true);
        value["Actions"][0]["DisableCaching"] = json!(true);
        let report = inspect(&value);
        assert!(!report.can_enable);
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "unsupported_manifest"
                && check.level == "blocker"
                && check.message.contains("Profiles")));
    }

    #[test]
    // Verify bare dialect marker cannot hide an elgato manifest from validation
    fn bare_dialect_marker_cannot_hide_an_elgato_manifest_from_validation() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("plugin.html"), "<!doctype html>").unwrap();
        for (sdk, dialect) in [(2, "vsd"), (3, "vsd"), (1, "streamdeck")] {
            let mut value = fixture("org.vendor.schema", "plugin.html");
            value["SDKVersion"] = json!(sdk);
            value["StreamDockManager"] = json!({"ManifestDialect":dialect});
            // A malformed vendor field cannot hide behind the alternate label
            value["Author"] = json!(null);
            std::fs::write(
                temp.path().join("manifest.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
            assert!(manifest::normalize(temp.path(), &value)
                .unwrap_err()
                .contains("Unsupported SDKVersion for declared manifest dialect"));
            assert!(read_plugin(temp.path()).is_err());
            let report = preflight::inspect(temp.path());
            assert!(!report.can_install);
            assert!(report.checks.iter().any(|check| check
                .message
                .contains("Unsupported SDKVersion for declared manifest dialect")));
        }
    }
}
