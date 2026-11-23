//! Immutable staged imports and complete local inventories. Package code is never executed
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// Hash bytes streamed from the supplied reader
pub(super) fn hash_reader(mut reader: impl Read) -> Result<(u64, String), String> {
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let read = match reader.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            value => value.map_err(|error| error.to_string())?,
        };
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size = size
            .checked_add(read as u64)
            .ok_or("Package size overflow")?;
    }
    Ok((size, hex::encode(digest.finalize())))
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PackageSource {
    pub format: String,
    pub filename: String,
    pub sha256: String,
}

// Collect the files and identity data for this inventory
pub(super) fn inventory(root: &Path) -> Result<BTreeMap<String, String>, String> {
    // Walk the package tree for inspection
    fn walk(
        root: &Path,
        path: &Path,
        depth: usize,
        total: &mut u64,
        out: &mut BTreeMap<String, String>,
        names: &mut std::collections::HashSet<String>,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err("Package nesting exceeds limit".into());
        }
        let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err("Package symlinks or special files rejected".into());
        }
        if path != root {
            let name = path
                .strip_prefix(root)
                .map_err(|e| e.to_string())?
                .to_str()
                .ok_or("Non-UTF8 package path")?
                .replace('\\', "/");
            if !safe_relative(&name) || !names.insert(name.to_lowercase()) {
                return Err("Unsafe or case-colliding package path".into());
            }
            if names.len() > 4096 {
                return Err("Too many package entries".into());
            }
            if metadata.is_file() {
                if metadata.len() > 64 * 1024 * 1024 {
                    return Err("Package file exceeds 64 MiB".into());
                }
                let reader = std::fs::File::open(path)
                    .map_err(|e| e.to_string())?
                    .take(64 * 1024 * 1024 + 1);
                let (size, digest) = hash_reader(reader)?;
                *total = total.checked_add(size).ok_or("Package size overflow")?;
                if size != metadata.len() || *total > 256 * 1024 * 1024 {
                    return Err("Package changed or exceeds size limits".into());
                }
                out.insert(name, digest);
            }
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
                walk(
                    root,
                    &entry.map_err(|e| e.to_string())?.path(),
                    depth + 1,
                    total,
                    out,
                    names,
                )?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, 0, &mut 0, &mut out, &mut Default::default())?;
    Ok(out)
}
/// Recover the host executable permission in a private browser-drop staging tree
///
/// WebViews provide file bytes without Unix permissions. Use the same manifest
/// resolution as installation and never mark other files executable.
pub fn prepare_dropped_folder_executables(staging: &Path) -> Result<(), String> {
    let source = safe_file(staging, "manifest.json")?;
    const MAX_MANIFEST: u64 = 64 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::fs::File::open(source)
        .map_err(|_| "Could not read the plugin manifest")?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the plugin manifest")?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err("Plugin manifest exceeds 64 MiB".into());
    }
    let payload = serde_json::from_slice(&bytes).map_err(|_| "Invalid plugin manifest")?;
    let manifest = manifest::normalize(staging, &payload)?;
    #[cfg(unix)]
    if manifest.runtime_kind == "native" {
        use std::os::unix::fs::PermissionsExt;
        let entry = safe_file(staging, &manifest.entry_point)?;
        std::fs::set_permissions(entry, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not set executable permission for the plugin entry point")?;
    }
    #[cfg(not(unix))]
    let _ = manifest;
    Ok(())
}

// Recognize metadata that does not become executable package content
fn harmless_metadata(path: &Path) -> Result<u64, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err("Unexpected archive metadata payload".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 != 0 {
            return Err("Executable archive metadata rejected".into());
        }
    }
    let mut header = [0u8; 8];
    let size = std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .read(&mut header)
        .map_err(|e| e.to_string())?;
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let valid = if name == ".DS_Store" {
        size == 0 || header == *b"\0\0\0\x01Bud1"
    } else {
        name.starts_with("._") && size >= 4 && header[..4] == [0, 5, 0x16, 7]
    };
    if !valid {
        return Err("Unexpected payload outside plugin root".into());
    }
    Ok(metadata.len())
}
// Read the metadata needed to validate an archive entry
fn archive_metadata(path: &Path) -> Result<u64, String> {
    if path.is_dir() {
        let mut total = 0u64;
        for (relative, _) in inventory(path)? {
            total += harmless_metadata(&path.join(relative))?;
            if total > 2 * 1024 * 1024 {
                return Err("Archive metadata exceeds limit".into());
            }
        }
        Ok(total)
    } else {
        harmless_metadata(path)
    }
}
// Infer only a complete, shared namespace, never a raw character prefix. A
// single action or an organization-only prefix cannot establish package identity.
fn inferred_identity(manifest: &Value) -> Result<String, String> {
    let error = "Cannot determine plugin UUID; provide a manifest UUID or a <UUID>.sdPlugin folder";
    let actions = manifest["Actions"].as_array().ok_or(error)?;
    let mut unique = std::collections::HashSet::new();
    let mut common: Option<Vec<&str>> = None;
    for action in actions {
        let id = action["UUID"].as_str().ok_or(error)?;
        let mut parts: Vec<_> = id.split('.').collect();
        if !safe_id(id) || parts.iter().any(|part| part.is_empty()) || !unique.insert(id) {
            return Err(error.into());
        }
        parts.pop(); // The action itself is never the package namespace.
        if let Some(prefix) = &mut common {
            prefix.truncate(
                prefix
                    .iter()
                    .zip(&parts)
                    .take_while(|(a, b)| a == b)
                    .count(),
            );
        } else {
            common = Some(parts);
        }
    }
    let prefix = common.ok_or(error)?;
    if unique.len() < 2 || prefix.len() < 3 {
        return Err(error.into());
    }
    Ok(prefix.join("."))
}

// Select root
pub(super) fn select_root(staging: &Path) -> Result<PathBuf, String> {
    // Extraction already rejects traversal, duplicate/case-colliding paths and
    // special files. Validate the entire staged tree before moving any entries.
    inventory(staging)?;
    let mut payload = vec![];
    let mut metadata_size = 0;
    for item in std::fs::read_dir(staging).map_err(|e| e.to_string())? {
        let path = item.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name == "__MACOSX" && path.is_dir() || name == ".DS_Store" && path.is_file() {
            metadata_size += archive_metadata(&path)?;
        } else {
            payload.push(path);
        }
    }
    if metadata_size > 2 * 1024 * 1024 {
        return Err("Archive metadata exceeds limit".into());
    }
    let root = if staging.join("manifest.json").is_file() {
        staging.to_path_buf()
    } else if payload.len() == 1
        && payload[0].is_dir()
        && payload[0].join("manifest.json").is_file()
    {
        payload[0].clone()
    } else {
        return Err("Package must contain one plugin, with manifest.json at its root or in a single top-level folder".into());
    };
    // Do not flatten mixed or nested plugin bundles into one package
    if inventory(&root)?.keys().any(|name| {
        name.split('/')
            .any(|part| manifest::bundle_identity(part).is_some())
    }) {
        return Err("Multiple or nested plugin bundles are ambiguous".into());
    }
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let named = (root != staging)
        .then(|| {
            root.file_name()
                .and_then(|s| s.to_str())
                .and_then(manifest::bundle_identity)
        })
        .flatten();
    let explicit = manifest
        .get("UUID")
        .map(|value| value.as_str().ok_or("Invalid manifest UUID"))
        .transpose()?;
    if explicit.zip(named).is_some_and(|(a, b)| a != b) {
        return Err("Package root must match manifest UUID".into());
    }
    let id = match explicit.or(named) {
        Some(id) if safe_id(id) => id.to_string(),
        Some(_) => return Err("Invalid plugin identity".into()),
        None => inferred_identity(&manifest)?,
    };
    let target = staging.join(format!("{id}.sdPlugin"));
    if root == target {
        return Ok(root);
    }
    if std::fs::read_dir(staging)
        .map_err(|e| e.to_string())?
        .any(|entry| {
            entry.ok().is_some_and(|entry| {
                // A case-only suffix rename refers to this same validated root,
                // not a second payload. Other case aliases remain collisions.
                entry.path() != root
                    && entry
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&format!("{id}.sdPlugin"))
            })
        })
    {
        return Err("Normalized plugin folder collides with package contents".into());
    }
    if root == staging {
        std::fs::create_dir(&target).map_err(|e| e.to_string())?;
        for entry in payload {
            std::fs::rename(
                &entry,
                target.join(entry.file_name().ok_or("Invalid package path")?),
            )
            .map_err(|e| e.to_string())?;
        }
    } else {
        std::fs::rename(root, &target).map_err(|e| e.to_string())?;
    }
    Ok(target)
}

impl PluginHost {
    // Read the manifest from the prepared plugin
    pub fn prepared_manifest(prepared: &PreparedPlugin) -> manifest::NormalizedManifest {
        prepared.manifest.clone()
    }
    // Read the source identity of the prepared plugin
    pub fn prepared_source(prepared: &PreparedPlugin) -> PackageSource {
        prepared.origin.clone()
    }
    // Read the file inventory of the prepared plugin
    pub fn prepared_inventory(prepared: &PreparedPlugin) -> BTreeMap<String, String> {
        prepared.inventory.clone()
    }
    // Read the compatibility findings for the prepared plugin
    pub fn prepared_preflight(prepared: &PreparedPlugin) -> preflight::PluginPreflight {
        preflight::inspect(&prepared.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn browser_folder_drop_restores_only_the_selected_native_entry_permission() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("org.example.drop.sdPlugin");
        std::fs::create_dir(&root).unwrap();
        let manifest = root.join("manifest.json");
        let entry = root.join("plugin-runner");
        let asset = root.join("asset.txt");
        for file in [&entry, &asset] {
            std::fs::write(file, b"synthetic package file").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut metadata = json!({
            "Name": "Folder drop", "SDKVersion": 1, "Version": "1.0",
            "StreamDockManager": {"ManifestDialect":"vsd"},
            "CodePath": "plugin-runner",
            "Actions": [{"UUID": "org.example.drop.action", "Name":"Fixture"}]
        });
        std::fs::write(&manifest, metadata.to_string()).unwrap();
        prepare_dropped_folder_executables(&root).unwrap();
        let mode = |path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&entry), 0o700);
        assert_eq!(mode(&asset), 0o600);

        let browser = root.join("index.html");
        std::fs::write(&browser, b"<!doctype html>").unwrap();
        std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o600)).unwrap();
        metadata["CodePath"] = json!("index.html");
        std::fs::write(&manifest, metadata.to_string()).unwrap();
        prepare_dropped_folder_executables(&root).unwrap();
        assert_eq!(mode(&browser), 0o600);
        metadata["CodePath"] = json!("../asset.txt");
        std::fs::write(&manifest, metadata.to_string()).unwrap();
        assert!(prepare_dropped_folder_executables(&root).is_err());
    }

    #[test]
    // Verify flat openaction archive passes full review and keeps original manifest
    fn flat_openaction_archive_passes_full_review_and_keeps_original_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("openaction.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        let value = json!({"Name":"Counter", "Author":"Fixture", "Icon":"icon", "OS":[{"Platform":"mac"},{"Platform":"windows"},{"Platform":"linux"}], "Version":"1.0.0", "CodePath":"index.html", "Actions":[{"UUID":"org.vendor.counter.up","Name":"Up","Icon":"icon","States":[{}]},{"UUID":"org.vendor.counter.down","Name":"Down","Icon":"icon","States":[{}]}]});
        let bytes = serde_json::to_vec(&value).unwrap();
        writer.start_file("manifest.json", options).unwrap();
        writer.write_all(&bytes).unwrap();
        writer.start_file("index.html", options).unwrap();
        writer.write_all(b"<!doctype html>").unwrap();
        writer.start_file("icon.svg", options).unwrap();
        writer
            .write_all(br#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#)
            .unwrap();
        writer.finish().unwrap();
        let prepared = PluginHost::prepare_install(directory.path(), &archive).unwrap();
        assert_eq!(prepared.manifest.id, "org.vendor.counter");
        assert_eq!(prepared.manifest.dialect, "openaction");
        assert_eq!(
            std::fs::read(prepared.source.join("manifest.json")).unwrap(),
            bytes
        );
        PluginHost::verify_prepared_files(&prepared, &prepared.inventory).unwrap();
    }

    #[test]
    // Verify sdkversionless legacy numeric versions pass staged review
    fn sdkversionless_legacy_numeric_versions_pass_staged_review() {
        for version in ["1", "1.2", "1.2.3", "1.2.3.4", "1.2.3-beta.1"] {
            let directory = tempfile::tempdir().unwrap();
            let source = directory.path().join("org.vendor.cpu.sdplugin");
            std::fs::create_dir(&source).unwrap();
            let value = json!({"Name":"CPU", "Author":"Fixture", "Icon":"icon", "OS":[{"Platform":"mac"},{"Platform":"windows"},{"Platform":"linux"}], "Version":version, "CodePath":"index.html", "Actions":[{"UUID":"org.vendor.cpu.cpu","Name":"CPU","Icon":"icon","States":[{"Image":"icon","FontSize":"17"}],"SupportedInMultiActions":false}]});
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(source.join("manifest.json"), &bytes).unwrap();
            std::fs::write(source.join("index.html"), b"<!doctype html>").unwrap();
            std::fs::write(source.join("icon.svg"), b"<svg/>").unwrap();
            let prepared = PluginHost::prepare_install(directory.path(), &source).unwrap();
            assert_eq!(prepared.manifest.id, "org.vendor.cpu");
            assert_eq!(prepared.manifest.version, version);
            assert_eq!(
                std::fs::read(prepared.source.join("manifest.json")).unwrap(),
                bytes
            );
            PluginHost::verify_prepared_files(&prepared, &prepared.inventory).unwrap();
        }
    }

    // Opt-in, read-only source verification. Stages a copy in a disposable root;
    // never installs or executes the supplied package or edits the original.
    #[test]
    #[ignore = "requires PLUGIN_PACKAGE_PATH and PLUGIN_PACKAGE_ID pointing to a local sample"]
    fn local_package_staging_review_preserves_original() {
        let source = PathBuf::from(std::env::var("PLUGIN_PACKAGE_PATH").unwrap());
        let id = std::env::var("PLUGIN_PACKAGE_ID").unwrap();
        let before = std::fs::read(&source).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let staging = directory.path().join("unpacked");
        std::fs::create_dir(&staging).unwrap();
        extract_archive(&source, &staging).unwrap();
        let selected = select_root(&staging).unwrap();
        let value: Value =
            serde_json::from_slice(&std::fs::read(selected.join("manifest.json")).unwrap())
                .unwrap();
        let normalized = manifest::normalize(&selected, &value).unwrap();
        assert_eq!(normalized.id, id);
        let report = preflight::inspect(&selected);
        println!(
            "{} {}: {}",
            normalized.id,
            normalized.version,
            serde_json::to_string(&report).unwrap()
        );
        if let Ok(expected) = std::env::var("PLUGIN_PACKAGE_EXPECT_BLOCKER") {
            assert!(report
                .checks
                .iter()
                .any(|c| c.level == "blocker" && c.message.contains(&expected)));
        }
        let prepared = PluginHost::prepare_install(directory.path(), &source).unwrap();
        assert_eq!(prepared.manifest.id, id);
        assert_eq!(
            std::fs::read(prepared.source.join("manifest.json")).unwrap(),
            std::fs::read(selected.join("manifest.json")).unwrap()
        );
        PluginHost::verify_prepared_files(&prepared, &prepared.inventory).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }

    #[test]
    // Verify alternative layouts preserve manifest bytes and use validated identity
    fn alternative_layouts_preserve_manifest_bytes_and_use_validated_identity() {
        for (wrapper, explicit) in [
            ("", true),
            ("download", true),
            ("", false),
            ("download", false),
            ("org.vendor.counter.sdPlugin", false),
            ("org.vendor.counter.sdplugin", false),
            ("org.vendor.counter.SDPLUGIN", true),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join(wrapper);
            std::fs::create_dir_all(&root).unwrap();
            let mut value = json!({"Actions":[{"UUID":"org.vendor.counter.up"},{"UUID":"org.vendor.counter.down"}]});
            if explicit {
                value["UUID"] = json!("org.vendor.counter");
            }
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(root.join("manifest.json"), &bytes).unwrap();
            std::fs::write(root.join("entry.html"), b"entry").unwrap();
            let selected = select_root(directory.path()).unwrap();
            assert_eq!(selected.file_name().unwrap(), "org.vendor.counter.sdPlugin");
            assert_eq!(
                std::fs::read(selected.join("manifest.json")).unwrap(),
                bytes
            );
            assert!(selected.join("entry.html").is_file());
        }
    }

    #[test]
    // Verify ambiguous or invalid inferred namespaces are rejected
    fn ambiguous_or_invalid_inferred_namespaces_are_rejected() {
        for ids in [
            vec![],
            vec!["org.vendor.counter.one"],
            vec!["org.vendor.one", "org.vendor.two"],
            vec!["org.vendor.counter.one", "net.vendor.counter.two"],
            vec!["org.vendor.counter.one", "org.vendor.counter.one"],
            vec!["org.vendor.counter.one", "org.vendor.counterpart.two"],
            vec!["org..counter.one", "org..counter.two"],
            vec!["org.vendor.counter", "org.vendor.counter.two"],
        ] {
            let value =
                json!({"Actions":ids.iter().map(|id| json!({"UUID":id})).collect::<Vec<_>>()});
            assert!(inferred_identity(&value).is_err(), "accepted {ids:?}");
        }
    }

    #[test]
    // Verify explicit or named identity allows single action but conflicts fail
    fn explicit_or_named_identity_allows_single_action_but_conflicts_fail() {
        for (explicit, suffix) in [(true, "sdPlugin"), (false, "sdplugin"), (true, "SDPLUGIN")] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory
                .path()
                .join(format!("org.vendor.counter.{suffix}"));
            std::fs::create_dir(&root).unwrap();
            let mut value = json!({"Actions":[{"UUID":"org.vendor.counter.one"}]});
            if explicit {
                value["UUID"] = json!("org.vendor.counter");
            }
            std::fs::write(root.join("manifest.json"), value.to_string()).unwrap();
            let root = select_root(directory.path()).unwrap();
            assert_eq!(root.file_name().unwrap(), "org.vendor.counter.sdPlugin");
            value["UUID"] = json!("org.vendor.other");
            std::fs::write(root.join("manifest.json"), value.to_string()).unwrap();
            assert!(select_root(directory.path()).is_err());
        }
    }

    #[test]
    // Verify flat package normalization rejects case collisions and nested bundles
    fn flat_package_normalization_rejects_case_collisions_and_nested_bundles() {
        for collision in [
            "ORG.VENDOR.COUNTER.SDPLUGIN",
            "nested.sdPlugin",
            "nested.sdplugin",
        ] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(
                directory.path().join("manifest.json"),
                br#"{"UUID":"org.vendor.counter"}"#,
            )
            .unwrap();
            std::fs::create_dir(directory.path().join(collision)).unwrap();
            std::fs::write(directory.path().join(collision).join("payload"), b"data").unwrap();
            assert!(select_root(directory.path()).is_err());
        }
    }

    #[test]
    // Verify foreign archive explains platform before the missing host entry
    fn foreign_archive_explains_platform_before_the_missing_host_entry() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("foreign.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        let (platform, platform_name, foreign_key) = if cfg!(windows) {
            ("mac", "macOS", "CodePathMac")
        } else {
            ("windows", "Windows", "CodePathWin")
        };
        writer
            .start_file("test.foreign.sdPlugin/manifest.json", options)
            .unwrap();
        let manifest = serde_json::json!({
            "UUID": "test.foreign",
            "SDKVersion":1,"StreamDockManager":{"ManifestDialect":"vsd"},
            "Name": "Foreign plugin",
            "Version": "1.0",
            "CodePath": "missing-on-this-platform.html",
            (foreign_key): "index.html",
            "OS": [{"Platform": platform}],
            "Actions": [{"UUID": "test.foreign.action", "Name": "Action"}]
        });
        writer.write_all(manifest.to_string().as_bytes()).unwrap();
        writer
            .start_file("test.foreign.sdPlugin/index.html", options)
            .unwrap();
        writer.write_all(b"<!doctype html>").unwrap();
        writer.finish().unwrap();

        let error = PluginHost::prepare_install(directory.path(), &archive)
            .err()
            .expect("a host entry point is required");
        assert!(
            error.starts_with(&format!("This package targets {platform_name}.")),
            "{error}"
        );
        assert!(
            error.contains("Could not read CodePath file missing-on-this-platform.html"),
            "{error}"
        );
    }

    // Construct the ZIP package used by this operation
    fn zip(root: &Path, names: &[&str]) -> PathBuf {
        let path = root.join("fixture.ZIP");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for name in names {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"data").unwrap();
        }
        writer.finish().unwrap();
        path
    }
    #[test]
    // Verify stored deflate and lzma current installers share staging and preserve vendor bytes
    fn stored_deflate_and_lzma_current_installers_share_staging_and_preserve_vendor_bytes() {
        for bytes in [
            include_bytes!("../tests/fixtures/stored.streamDeckPlugin").as_slice(),
            include_bytes!("../tests/fixtures/deflate.streamDeckPlugin").as_slice(),
            include_bytes!("../tests/fixtures/lzma.streamDeckPlugin").as_slice(),
        ] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("LEGACY.STREAMDECKPLUGIN");
            std::fs::write(&archive, bytes).unwrap();
            let stage = PluginHost::prepare_install(root.path(), &archive).unwrap();
            assert_eq!(stage.manifest.id, "org.vendor.counter");
            assert_eq!(stage.manifest.version, "0.1.0.0");
            assert_eq!(stage.inventory.len(), 3);
            let original_manifest = std::fs::read(stage.source.join("manifest.json")).unwrap();
            assert!(serde_json::from_slice::<Value>(&original_manifest)
                .unwrap()
                .get("UUID")
                .is_some());
            // Changing the source after review has no effect on reviewed bytes
            std::fs::write(&archive, b"changed source").unwrap();
            PluginHost::verify_prepared_files(&stage, &stage.inventory).unwrap();
            std::fs::write(stage.source.join("index.html"), b"changed staging").unwrap();
            assert!(PluginHost::verify_prepared_files(&stage, &stage.inventory).is_err());
        }
    }
    #[test]
    // Verify unsafe paths case collisions file directory collisions and extra payloads are rejected
    fn unsafe_paths_case_collisions_file_directory_collisions_and_extra_payloads_are_rejected() {
        for names in [
            vec!["../escape"],
            vec!["/absolute"],
            vec!["a/./b"],
            vec!["a\\b"],
            vec!["C:stream"],
            vec!["Index.html", "index.html"],
            vec!["directory", "directory/file"],
        ] {
            let root = tempfile::tempdir().unwrap();
            let archive = zip(root.path(), &names);
            let target = root.path().join("target");
            std::fs::create_dir(&target).unwrap();
            assert!(
                extract_archive(&archive, &target).is_err(),
                "accepted {names:?}"
            );
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("org.vendor.counter.sdPlugin")).unwrap();
        std::fs::write(
            root.path()
                .join("org.vendor.counter.sdPlugin/manifest.json"),
            br#"{"UUID":"org.vendor.counter"}"#,
        )
        .unwrap();
        std::fs::write(root.path().join("unexpected.exe"), b"MZ").unwrap();
        assert!(select_root(root.path()).is_err());
        std::fs::remove_file(root.path().join("unexpected.exe")).unwrap();
        assert!(select_root(root.path()).is_ok());
        std::fs::write(root.path().join("manifest.json"), b"{}").unwrap();
        assert!(select_root(root.path()).is_err());
    }
    #[test]
    // Verify zip64 metadata and encryption are checked by the shared importer
    fn zip64_metadata_and_encryption_are_checked_by_the_shared_importer() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("fixture.streamDeckPlugin");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        let package = root.path().join("source");
        crate::lifecycle_tests::package(&package, "org.vendor.fixture");
        for name in packages::inventory(&package).unwrap().keys() {
            writer
                .start_file(format!("org.vendor.fixture.sdPlugin/{name}"), options)
                .unwrap();
            writer
                .write_all(&std::fs::read(package.join(name)).unwrap())
                .unwrap();
        }
        writer.start_file("__MACOSX/._fixture", options).unwrap();
        writer.write_all(b"\0\x05\x16\x07\0\0\0\0").unwrap();
        writer.finish().unwrap();
        let prepared = PluginHost::prepare_install(root.path(), &archive).unwrap();
        assert_eq!(prepared.inventory.len(), 4);
        assert!(!prepared.source.join("__MACOSX").exists());
        drop(prepared);
        let mut bytes = std::fs::read(&archive).unwrap();
        let central = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        bytes[6] |= 1;
        bytes[central + 8] |= 1;
        std::fs::write(&archive, bytes).unwrap();
        assert!(PluginHost::prepare_install(root.path(), &archive).is_err());

        let directory = root.path().join("metadata");
        std::fs::create_dir_all(directory.join("org.vendor.fixture.sdPlugin")).unwrap();
        std::fs::write(
            directory.join("org.vendor.fixture.sdPlugin/manifest.json"),
            "{}",
        )
        .unwrap();
        std::fs::create_dir(directory.join("__MACOSX")).unwrap();
        std::fs::write(
            directory.join("__MACOSX/._disguised"),
            b"MZunexpected executable",
        )
        .unwrap();
        assert!(select_root(&directory).is_err());
    }
    #[test]
    // Verify lzma dictionary properties truncation and CRC corruption fail before commit
    fn lzma_dictionary_properties_truncation_and_crc_corruption_fail_before_commit() {
        let fixture = include_bytes!("../tests/fixtures/lzma.streamDeckPlugin");
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(fixture)).unwrap();
        let entry = zip.by_index_raw(0).unwrap();
        let offset = entry.data_start().unwrap() as usize;
        let compressed_size = entry.compressed_size() as usize;
        drop(entry);
        for mutation in 0..4 {
            let mut bytes = fixture.to_vec();
            match mutation {
                0 => bytes[offset + 5..offset + 9]
                    .copy_from_slice(&(128u32 * 1024 * 1024).to_le_bytes()),
                1 => bytes[offset + 2] = 9,
                2 => bytes[offset + compressed_size - 6..offset + compressed_size].fill(0),
                _ => bytes[offset + 12] ^= 255,
            }
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("bad.zip");
            std::fs::write(&path, bytes).unwrap();
            let target = root.path().join("target");
            std::fs::create_dir(&target).unwrap();
            assert!(
                extract_archive(&path, &target).is_err(),
                "accepted corrupt LZMA mutation {mutation}"
            );
        }
    }
}
