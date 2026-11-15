//! One manifest interpretation for discovery, review, action listing, and launch
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct NormalizedManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: Option<String>,
    pub dialect: String,
    pub entry_point: String,
    pub runtime_kind: String,
    pub platforms: Vec<String>,
    pub action_count: usize,
    pub control_types: Vec<String>,
}

/// Vendor versions are deliberately independent of the official catalog SemVer schema
pub fn vendor_version(value: &str) -> Result<[u64; 4], String> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return Err("Vendor version needs one to four numeric components".into());
    }
    let mut out = [0; 4];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Invalid numeric vendor version".into());
        }
        out[i] = part
            .parse()
            .map_err(|_| "Vendor version component exceeds limit")?;
    }
    Ok(out)
}
// Compare versions using the supported vendor version rules
pub fn compare_vendor_versions(left: &str, right: &str) -> Result<std::cmp::Ordering, String> {
    Ok(vendor_version(left)?.cmp(&vendor_version(right)?))
}
/// Bundle suffixes are case-insensitive; the identity itself is not
pub(super) fn bundle_identity(name: &str) -> Option<&str> {
    let split = name.len().checked_sub(".sdPlugin".len())?;
    let suffix = name.get(split..)?;
    suffix
        .eq_ignore_ascii_case(".sdPlugin")
        .then(|| &name[..split])
}

// Select the manifest dialect
pub(super) fn dialect(manifest: &Value) -> &'static str {
    if let Some(value) = manifest["StreamDockManager"].get("ManifestDialect") {
        return value
            .as_str()
            .and_then(connection_contract::vendor)
            .map_or("unsupported", |vendor| vendor.dialect);
    }
    if manifest.get("SDKVersion").is_none() {
        return "openaction";
    }
    manifest["SDKVersion"]
        .as_u64()
        .and_then(connection_contract::vendor_for_sdk)
        .map_or("unsupported", |vendor| vendor.dialect)
}

/// Exact CPU/OS override first, then platform override, then portable entry
pub(super) fn entry_point(manifest: &Value) -> Option<&str> {
    entry_point_for(manifest, std::env::consts::ARCH, std::env::consts::OS)
}

// Resolve the executable entry point for the selected platform
fn entry_point_for<'a>(manifest: &'a Value, arch: &str, os: &str) -> Option<&'a str> {
    let dialect = dialect(manifest);
    let openaction = dialect == "openaction";
    let triple = match (arch, os) {
        ("x86_64", "windows") => "x86_64-pc-windows-msvc",
        ("aarch64", "windows") => "aarch64-pc-windows-msvc",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        _ => "",
    };
    // Native SDK/VSD unified bundles explicitly enumerate supported CPUs
    // A missing target must never fall back to a binary for another CPU
    if dialect == "vsd" && manifest.get("CodePaths").is_some() {
        return manifest["CodePaths"][triple].as_str();
    }
    let platform = match os {
        "macos" => "CodePathMac",
        "windows" => "CodePathWin",
        _ if openaction => "CodePathLin",
        _ => "CodePathLinux",
    };
    (if openaction {
        manifest["CodePaths"][triple].as_str()
    } else {
        None
    })
    .or_else(|| manifest[platform].as_str())
    .or_else(|| manifest["CodePath"].as_str())
}

// Normalize for the current operation
pub(super) fn normalize(path: &Path, manifest: &Value) -> Result<NormalizedManifest, String> {
    normalize_with_identity(path, manifest, None)
}
// Normalize with identity
pub(super) fn normalize_with_identity(
    path: &Path,
    manifest: &Value,
    installed_identity: Option<&str>,
) -> Result<NormalizedManifest, String> {
    // Package identity comes from a validated installation directory or the
    // vendor's .sdPlugin package name (including validated archive staging).
    // Namespace inference is confined to archive layout normalization
    let named_package = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(bundle_identity);
    let package_identity = named_package.or(installed_identity);
    let explicit = manifest
        .get("UUID")
        .map(|value| value.as_str().ok_or("Invalid manifest UUID"))
        .transpose()?;
    if explicit.zip(named_package).is_some_and(|(a, b)| a != b) {
        return Err("Manifest UUID does not match package identity".into());
    }
    let id = explicit
        .or(package_identity)
        .ok_or("Validated plugin package identity required")?;
    let dialect = dialect(manifest);
    let contract = connection_contract::vendor(dialect)
        .ok_or("Supported vendor SDKVersion or explicit manifest dialect required")?;
    if !(dialect == "openaction" && manifest.get("SDKVersion").is_none())
        && manifest["SDKVersion"]
            .as_u64()
            .and_then(connection_contract::vendor_for_sdk)
            != Some(contract)
    {
        return Err("Unsupported SDKVersion for declared manifest dialect".into());
    }
    if !safe_id(id) {
        return Err("Invalid plugin identity".into());
    }
    if let Some(monitors) = manifest.get("ApplicationsToMonitor") {
        let object = monitors
            .as_object()
            .ok_or("ApplicationsToMonitor must be an object")?;
        for (platform, apps) in object {
            if !matches!(platform.as_str(), "mac" | "windows")
                && !(dialect == "openaction" && platform == "linux")
            {
                return Err("Unsupported application monitor platform".into());
            }
            let apps = apps
                .as_array()
                .filter(|apps| apps.len() <= 128)
                .ok_or("Too many application monitors")?;
            if apps.iter().any(|app| {
                app.as_str().is_none_or(|app| {
                    app.is_empty() || app.len() > 256 || app.chars().any(char::is_control)
                })
            }) {
                return Err("Invalid monitored application identifier".into());
            }
        }
    }
    let name = manifest["Name"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .ok_or("Plugin Name missing")?;
    let version = manifest["Version"]
        .as_str()
        .ok_or("Plugin Version missing")?;
    if dialect == "openaction" {
        manifest_schema::validate_openaction(manifest, id)?;
    } else {
        vendor_version(version)?;
    }
    let entry = entry_point(manifest).ok_or("Standalone plugin entry point missing")?;
    for key in [
        "CodePath",
        "CodePathMac",
        "CodePathWin",
        "CodePathLinux",
        "CodePathLin",
        "PropertyInspectorPath",
    ] {
        if let Some(value) = manifest
            .get(key)
            .filter(|v| !(dialect == "openaction" && v.is_null()))
        {
            let relative = value.as_str().ok_or("Invalid manifest file reference")?;
            // Native packages can declare alternate-platform names without
            // shipping those binaries in this platform's archive. Every name
            // must be safe; selected code and inspectors must actually exist.
            let required = relative == entry || key == "PropertyInspectorPath";
            if !safe_relative(relative) {
                return Err(format!("Invalid {key}: {relative}"));
            }
            if (required || path.join(relative).exists())
                && !safe_file(path, relative)
                    .map_err(|error| format!("Could not read {key} file {relative}: {error}"))?
                    .is_file()
            {
                return Err(format!("Invalid {key}: {relative}"));
            }
        }
    }
    if dialect == "vsd" {
        if let Some(paths) = manifest.get("CodePaths") {
            let paths = paths
                .as_object()
                .filter(|p| !p.is_empty() && p.len() <= 6)
                .ok_or("CodePaths must be a nonempty target map")?;
            for (target, value) in paths {
                if !matches!(
                    target.as_str(),
                    "x86_64-pc-windows-msvc"
                        | "aarch64-pc-windows-msvc"
                        | "x86_64-apple-darwin"
                        | "aarch64-apple-darwin"
                        | "x86_64-unknown-linux-gnu"
                        | "aarch64-unknown-linux-gnu"
                ) {
                    return Err("Unsupported CodePaths target".into());
                }
                let relative = value.as_str().ok_or("Invalid CodePaths entry")?;
                if !safe_relative(relative) || !safe_file(path, relative)?.is_file() {
                    return Err("CodePaths entry must be a confined regular file".into());
                }
            }
        }
    }
    if let Some(paths) = manifest["CodePaths"].as_object() {
        for value in paths.values().filter(|v| !v.is_null()) {
            let relative = value.as_str().ok_or("Invalid CodePaths entry")?;
            if !safe_relative(relative) {
                return Err("Unsafe CodePaths entry".into());
            }
            if path.join(relative).exists() && !safe_file(path, relative)?.is_file() {
                return Err("CodePaths entry must be a regular file".into());
            }
        }
    }
    let entry_file = safe_file(path, entry)?;
    if !entry_file.is_file() {
        return Err("Entry point must be a regular file".into());
    }
    let requirement =
        stream_dock_manager_plugin_runtimes::manifest_requirement(manifest, &entry_file)?;
    let kind = requirement
        .as_ref()
        .map(|r| r.kind.name())
        .unwrap_or_else(|| match entry_file.extension().and_then(|s| s.to_str()) {
            Some("html" | "htm") => "browser",
            _ => "native",
        });
    let actions = manifest["Actions"]
        .as_array()
        .ok_or("Actions array missing")?;
    if actions.len() > 1024 {
        return Err("Too many action declarations".into());
    }
    let namespace = id;
    let mut seen = std::collections::HashSet::new();
    let mut controls = std::collections::BTreeSet::new();
    for action in actions {
        let action_id = action["UUID"].as_str().ok_or("Action UUID missing")?;
        if !safe_id(action_id)
            || ((dialect == "openaction"
                || manifest["StreamDockManager"].get("Capabilities").is_some())
                && !action_id.starts_with(&format!("{namespace}.")))
            || !seen.insert(action_id)
        {
            return Err("Duplicate action UUID or invalid action namespace".into());
        }
        if action["Name"].as_str().is_none_or(|s| s.trim().is_empty()) {
            return Err("Action Name missing".into());
        }
        if let Some(declarations) = action.get("Controllers") {
            let items = declarations
                .as_array()
                .ok_or("Invalid Controllers declaration")?;
            if items.is_empty()
                || items.iter().any(|c| {
                    c.as_str()
                        .is_none_or(|c| c.trim().is_empty() || c.len() > 64)
                })
            {
                return Err("Invalid Controllers declaration".into());
            }
        }
        for field in [
            "SupportedInMultiActions",
            "SupportedInKeyLogicActions",
            "VisibleInActionsList",
            "UserTitleEnabled",
            "DisableAutomaticStates",
        ] {
            if action.get(field).is_some_and(|value| !value.is_boolean()) {
                return Err(format!("Invalid {field}: expected a boolean"));
            }
        }
        controls.extend(action_control_types(action));
        for (field, value) in [
            (
                "PropertyInspectorPath",
                action["PropertyInspectorPath"].as_str(),
            ),
            ("layout", action["Encoder"]["layout"].as_str()),
        ] {
            if let Some(value) = value {
                if field == "layout" && value.starts_with('$') {
                    continue;
                }
                if !safe_relative(value) || !safe_file(path, value)?.is_file() {
                    return Err(format!("Invalid {field}: {value}"));
                }
            }
        }
        let mut assets = vec![
            action["Icon"].as_str(),
            action["Encoder"]["Icon"].as_str(),
            action["Encoder"]["background"].as_str(),
        ];
        if let Some(states) = action["States"].as_array() {
            assets.extend(
                states
                    .iter()
                    .flat_map(|s| [s["Image"].as_str(), s["MultiActionImage"].as_str()]),
            );
        }
        for asset in assets.into_iter().flatten() {
            if !safe_relative(asset) {
                return Err(format!("Unsafe image reference: {asset}"));
            }
        }
    }
    for key in ["Icon", "CategoryIcon"] {
        if let Some(value) = manifest[key].as_str() {
            if !safe_relative(value) {
                return Err(format!("Unsafe {key}"));
            }
        }
    }
    Ok(NormalizedManifest {
        id: id.into(),
        name: name.into(),
        version: version.into(),
        publisher: manifest["Author"].as_str().map(str::to_owned),
        dialect: dialect.into(),
        entry_point: entry.into(),
        runtime_kind: kind.into(),
        platforms: manifest["OS"]
            .as_array()
            .map(|p| {
                p.iter()
                    .filter_map(|p| p["Platform"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        action_count: actions.len(),
        control_types: controls.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const SYSTEM_INFORMATION: &str = include_str!(
        "../../../tests/fixtures/plugins/vendor/openaction/user-samples/system-information.json"
    );
    const REDLINE: &str =
        include_str!("../../../tests/fixtures/plugins/vendor/openaction/user-samples/redline.json");

    #[test]
    // Verify original openaction and opendeck import manifests normalize unchanged
    fn original_openaction_and_opendeck_import_manifests_normalize_unchanged() {
        for (source, id, expected_version, platforms) in [
            (SYSTEM_INFORMATION, "me.amankhanna.oasystem", "1.0.0", 3),
            (REDLINE, "com.kahikara.opendeck-redline", "1.0.9.1", 1),
        ] {
            let value: Value = serde_json::from_str(source).unwrap();
            let before = value.clone();
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join(format!("{id}.sdPlugin"));
            std::fs::create_dir(&root).unwrap();
            let mut files = vec![entry_point(&value).unwrap()];
            files.extend(
                value["Actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|a| a["PropertyInspectorPath"].as_str()),
            );
            for relative in files {
                let path = root.join(relative);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "fixture only; never executable").unwrap();
            }
            std::fs::write(root.join("manifest.json"), source).unwrap();
            let normalized = normalize(&root, &value).unwrap();
            let report = super::super::preflight::inspect(&root);
            assert!(
                !report
                    .checks
                    .iter()
                    .any(|check| matches!(check.code.as_str(), "manifest" | "vendor_contract")),
                "{:?}",
                report.checks
            );
            if id.ends_with("redline") && !cfg!(target_os = "linux") {
                assert!(report
                    .checks
                    .iter()
                    .any(|check| check.code == "declared_platform"));
                assert!(!report.can_enable);
            }
            if id == "me.amankhanna.oasystem" {
                for (action, expected) in value["Actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .zip([16.0, 14.0, 12.0, 12.0])
                {
                    assert_eq!(
                        super::super::visual::manifest_title_parameters(&action["States"][0])
                            ["fontSize"],
                        json!(expected)
                    );
                }
            }
            assert_eq!(normalized.id, id);
            assert_eq!(normalized.version, expected_version);
            assert_eq!(normalized.platforms.len(), platforms);
            assert_eq!(normalized.dialect, "openaction");
            if id.ends_with("redline") {
                assert_eq!(normalized.platforms, ["linux"]);
                assert_eq!(normalized.entry_point, "start.sh");
            }
            assert_eq!(value, before);
        }
    }

    #[test]
    // Verify opendeck adapter rejects malformed metadata and unsafe paths
    fn opendeck_adapter_rejects_malformed_metadata_and_unsafe_paths() {
        let original: Value = serde_json::from_str(REDLINE).unwrap();
        let id = "com.kahikara.opendeck-redline";
        for (pointer, invalid) in [
            ("/Version", json!("1.0.9.1.2")),
            ("/Version", json!("1.0.9.beta")),
            ("/Version", json!("1..2")),
            ("/Version", json!("1.2.")),
            ("/Version", json!("-1.2")),
            ("/Version", json!("18446744073709551616")),
            ("/Software", json!({"MinimumVersion":5})),
            ("/Software", json!({"MinimumVersion":"5.0","Unknown":true})),
            ("/Software", json!(null)),
            ("/OS/0/MinimumVersion", json!(false)),
        ] {
            let mut bad = original.clone();
            *bad.pointer_mut(pointer).unwrap() = invalid;
            assert!(
                manifest_schema::validate_openaction(&bad, id).is_err(),
                "{pointer}"
            );
        }
        let mut value: Value = serde_json::from_str(SYSTEM_INFORMATION).unwrap();
        for size in [
            json!(0),
            json!(-1),
            json!(12.5),
            json!(257),
            json!(true),
            json!(null),
            json!("large"),
        ] {
            value["Actions"][0]["States"][0]["FontSize"] = size;
            assert!(
                manifest_schema::validate_openaction(&value, "me.amankhanna.oasystem").is_err()
            );
        }
        let root = tempfile::tempdir().unwrap();
        let mut bad = original;
        bad["UUID"] = json!(id);
        bad["CodePath"] = json!("../escape.sh");
        assert!(normalize(root.path(), &bad)
            .unwrap_err()
            .contains("Invalid CodePath"));
    }

    #[test]
    // Verify genuine openaction starterpack manifest is accepted unchanged
    fn genuine_openaction_starterpack_manifest_is_accepted_unchanged() {
        let value: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/plugins/vendor/openaction/starterpack/manifest.json"
        ))
        .unwrap();
        let before = value.clone();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("com.amansprojects.starterpack.sdPlugin");
        std::fs::create_dir(&root).unwrap();
        let mut files = vec![entry_point(&value).unwrap()];
        files.extend(
            value["Actions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|a| a["PropertyInspectorPath"].as_str()),
        );
        for relative in files {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "fixture").unwrap();
        }
        let normalized = normalize(&root, &value).unwrap();
        assert_eq!(normalized.dialect, "openaction");
        assert_eq!(normalized.id, "com.amansprojects.starterpack");
        assert_eq!(value, before);
    }

    #[test]
    // Verify native unified codepaths select exact CPU without wrong target fallback
    fn native_unified_codepaths_select_exact_cpu_without_wrong_target_fallback() {
        let mut value = json!({"SDKVersion":1,"CodePath":"fallback","CodePathMac":"wrong-cpu","CodePaths":{
            "x86_64-pc-windows-msvc":"win-amd64", "aarch64-pc-windows-msvc":"win-arm64",
            "x86_64-apple-darwin":"mac-amd64", "aarch64-apple-darwin":"mac-arm64",
            "x86_64-unknown-linux-gnu":"linux-amd64", "aarch64-unknown-linux-gnu":"linux-arm64"}});
        for (os, prefix) in [("windows", "win"), ("macos", "mac"), ("linux", "linux")] {
            for (arch, suffix) in [("x86_64", "amd64"), ("aarch64", "arm64")] {
                assert_eq!(
                    entry_point_for(&value, arch, os),
                    Some(format!("{prefix}-{suffix}").as_str())
                );
            }
        }
        assert_eq!(entry_point_for(&value, "riscv64", "linux"), None);
        value["CodePaths"]
            .as_object_mut()
            .unwrap()
            .remove("aarch64-apple-darwin");
        assert_eq!(entry_point_for(&value, "aarch64", "macos"), None);
        value.as_object_mut().unwrap().remove("CodePaths");
        assert_eq!(
            entry_point_for(&value, "aarch64", "macos"),
            Some("wrong-cpu")
        );
    }

    #[test]
    // Verify native unified codepaths validate every shipped target
    fn native_unified_codepaths_validate_every_shipped_target() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("entry"), b"fixture").unwrap();
        let mut value = json!({"UUID":"org.vendor.unified","SDKVersion":1,"Name":"Unified","Version":"0.1.0","Actions":[],"CodePaths":{
            "x86_64-pc-windows-msvc":"entry", "aarch64-pc-windows-msvc":"entry",
            "x86_64-apple-darwin":"entry", "aarch64-apple-darwin":"entry",
            "x86_64-unknown-linux-gnu":"entry", "aarch64-unknown-linux-gnu":"entry"}});
        let normalized = normalize(root.path(), &value).unwrap();
        assert_eq!(normalized.dialect, "vsd");
        assert_eq!(normalized.entry_point, "entry");
        for invalid in [json!(null), json!(42), json!("../escape"), json!("missing")] {
            value["CodePaths"]["x86_64-pc-windows-msvc"] = invalid;
            assert!(normalize(root.path(), &value).is_err());
        }
        value["CodePaths"]["x86_64-pc-windows-msvc"] = json!("entry");
        value["CodePaths"]["unknown-target"] = json!("entry");
        assert!(normalize(root.path(), &value).is_err());
    }

    #[test]
    // Verify openaction manifest is bounded and selects CPU override
    fn openaction_manifest_is_bounded_and_selects_cpu_override() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("fallback.html"), "fixture").unwrap();
        std::fs::write(root.path().join("cpu.html"), "fixture").unwrap();
        let mut value = json!({"UUID":"org.openaction.counter", "Name":"Counter", "Author":"Fixture", "Version":"1.2.3-beta.1", "Icon":"icon", "CodePath":"fallback.html", "CodePathLin":null, "PropertyInspectorPath":null, "HasSettingsInterface":true, "OS":[{"Platform":"linux","Version":null},{"Platform":"mac"},{"Platform":"windows"}], "Actions":[{"UUID":"org.openaction.counter.count","Name":"Count","Icon":"icon","States":[{}, {"Image":"actionDefaultImage","FontSize":"16"}]}]});
        assert_eq!(
            normalize(root.path(), &value).unwrap().dialect,
            "openaction"
        );
        value["CodePaths"] = json!({"x86_64-pc-windows-msvc":"cpu.html","x86_64-apple-darwin":"cpu.html","aarch64-apple-darwin":"cpu.html","x86_64-unknown-linux-gnu":"cpu.html","aarch64-unknown-linux-gnu":"cpu.html"});
        assert_eq!(
            normalize(root.path(), &value).unwrap().entry_point,
            "cpu.html"
        );
        for (pointer, invalid) in [
            ("/Author", json!(null)),
            ("/Version", json!("1.2.3.4.5")),
            ("/Actions/0/States", json!([])),
            ("/Actions/0/UUID", json!("other.action")),
            ("/CodePaths/x86_64-pc-windows-msvc", json!("../escape")),
            ("/Actions/0/States/0/FontSize", json!(12.5)),
            ("/HasSettingsInterface", json!("yes")),
        ] {
            let mut bad = value.clone();
            if let Some(slot) = bad.pointer_mut(pointer) {
                *slot = invalid;
            } else {
                bad["Actions"][0]["States"][0]["FontSize"] = invalid;
            }
            assert!(normalize(root.path(), &bad).is_err(), "{pointer}");
        }
        let mut bad = value.clone();
        bad["SDKVersion"] = json!(99);
        assert!(normalize(root.path(), &bad).is_err());
        bad = value.clone();
        bad["StreamDockManager"] = json!({"ManifestDialect":"streamdeck"});
        assert!(normalize(root.path(), &bad).is_err());
    }

    #[test]
    // Verify unused platform code names are safe but need not be shipped
    fn unused_platform_code_names_are_safe_but_need_not_be_shipped() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "fixture").unwrap();
        let other_platform = if cfg!(windows) {
            "CodePathMac"
        } else {
            "CodePathWin"
        };
        let mut value = json!({"UUID":"org.vendor.platform","Name":"Platform fixture","Version":"1.0","CodePath":"index.html","SDKVersion":1,"StreamDockManager":{"ManifestDialect":"vsd"},"Actions":[]});
        value[other_platform] = json!("other-platform-binary");
        assert!(normalize(root.path(), &value).is_ok());
        value[other_platform] = json!("../escape");
        assert!(normalize(root.path(), &value).is_err());
    }
    #[test]
    // Verify vendor versions keep catalog semver separate
    fn vendor_versions_keep_catalog_semver_separate() {
        assert_eq!(
            compare_vendor_versions("0.1.0.0", "0.1.0").unwrap(),
            std::cmp::Ordering::Equal
        );
        assert_eq!(
            compare_vendor_versions("1.10", "1.9.99.99").unwrap(),
            std::cmp::Ordering::Greater
        );
        for invalid in ["", "1.2.3.4.5", "1.0-beta", "-1", "1..0"] {
            assert!(vendor_version(invalid).is_err());
        }
    }
    #[test]
    // Verify validated identity namespace and platform are required
    fn validated_identity_namespace_and_platform_are_required() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "browser").unwrap();
        let valid = json!({"UUID":"org.vendor.Counter", "Name":"Counter", "Version":"0.1.0", "SDKVersion":1, "StreamDockManager":{"ManifestDialect":"vsd"}, "CodePath":"index.html", "Actions":[{"UUID":"org.vendor.Counter.action","Name":"Counter","Controllers":["Keypad"]}]});
        assert_eq!(
            normalize(dir.path(), &valid).unwrap().id,
            "org.vendor.Counter"
        );
        for key in ["CodePath", "SDKVersion"] {
            let mut bad = valid.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(
                normalize_with_identity(dir.path(), &bad, Some("org.vendor.Counter")).is_err(),
                "{key}"
            );
        }
        let mut foreign = valid.clone();
        foreign.as_object_mut().unwrap().remove("CodePath");
        foreign[if cfg!(windows) {
            "CodePathMac"
        } else {
            "CodePathWin"
        }] = json!("index.html");
        assert!(normalize(dir.path(), &foreign).is_err());
        let mut bad = valid.clone();
        bad["Actions"][0]["UUID"] = json!("other.action");
        bad["StreamDockManager"]["Capabilities"] = json!({});
        assert!(normalize(dir.path(), &bad).is_err());
        bad = valid.clone();
        bad.as_object_mut().unwrap().remove("UUID");
        bad["uuid"] = json!("org.vendor.Counter");
        assert!(normalize(dir.path(), &bad).is_err());
    }
}
