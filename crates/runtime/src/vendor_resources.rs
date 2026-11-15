//! Content-addressed vendor action files. Only managed identifiers enter saved actions
use crate::{actions::Action, profiles::ProfileData};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub(crate) type Resources = BTreeMap<String, String>;
pub(crate) const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_TOTAL: u64 = 64 * 1024 * 1024;

// Validate the supplied vendor resources value
pub(crate) fn validate(resources: &Resources) -> Result<(), String> {
    if resources.len() > 32 {
        return Err("Action resources exceed 32 files".into());
    }
    for (key, id) in resources {
        if key.is_empty() || key.len() > 128 || key.chars().any(char::is_control) {
            return Err("Invalid action resource key".into());
        }
        validate_id(id)?;
    }
    Ok(())
}
// Validate ID
fn validate_id(id: &str) -> Result<(), String> {
    let Some((hash, extension)) = id.split_once('.') else {
        return Err("Invalid managed resource identifier".into());
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || extension.is_empty()
        || extension.len() > 16
        || !extension.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return Err("Invalid managed resource identifier".into());
    }
    Ok(())
}
// Create or validate the managed resource directory without accepting a symlink
fn directory(root: &Path) -> Result<PathBuf, String> {
    let dir = root.join("plugin-resources");
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
            return Err("Managed resources must be a regular directory".into())
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&dir).map_err(|e| e.to_string())?
        }
        Err(e) => return Err(e.to_string()),
    }
    Ok(dir)
}
// Resolve a validated resource identifier to a bounded regular nonsymlink file
pub(crate) fn path(root: &Path, id: &str) -> Result<PathBuf, String> {
    validate_id(id)?;
    let path = directory(root)?.join(id);
    let m = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_FILE {
        return Err("Resource is not a bounded regular file".into());
    }
    Ok(path)
}
// Read the requested vendor resources data
fn read(mut reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("Action resource exceeds 16 MiB".into());
    }
    Ok(bytes)
}
// Publish the current vendor resources result
fn publish(root: &Path, id: &str, bytes: &[u8]) -> Result<(), String> {
    publish_with_sync(root, id, bytes, stream_dock_manager_storage::sync_directory)
}
// Publish with sync
fn publish_with_sync(
    root: &Path,
    id: &str,
    bytes: &[u8],
    mut sync: impl FnMut(&Path) -> Result<(), String>,
) -> Result<(), String> {
    validate_id(id)?;
    if hex::encode(Sha256::digest(bytes)) != id.split_once('.').unwrap().0 {
        return Err("Action resource content digest mismatch".into());
    }
    let dir = directory(root)?;
    // Also flush on retries: an earlier failed sync may have left the directory
    // visible without making its parent entry durable
    sync(root)?;
    let destination = dir.join(id);
    if destination.symlink_metadata().is_ok() {
        let existing = read(std::fs::File::open(path(root, id)?).map_err(|e| e.to_string())?)?;
        if existing != bytes {
            return Err("Managed resource content changed".into());
        }
        return sync(&dir);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
    temporary.write_all(bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary
        .persist_noclobber(destination)
        .map_err(|e| e.to_string())?;
    sync(&dir)
}
// Stage the vendor resources change
pub(crate) fn stage(root: &Path, sources: &Resources) -> Result<Resources, String> {
    if sources.len() > 32 {
        return Err("Action resources exceed 32 files".into());
    }
    let mut staged = Vec::new();
    let mut total = 0;
    let mut result = Resources::new();
    for (key, source) in sources {
        let source = Path::new(source);
        let m = std::fs::symlink_metadata(source).map_err(|e| e.to_string())?;
        if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_FILE {
            return Err("Resource source is not a bounded regular file".into());
        }
        let bytes = read(std::fs::File::open(source).map_err(|e| e.to_string())?)?;
        total += bytes.len() as u64;
        if total > MAX_TOTAL {
            return Err("Action resources exceed 64 MiB".into());
        }
        let extension = source
            .extension()
            .and_then(|s| s.to_str())
            .filter(|s| {
                !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let id = format!("{}.{extension}", hex::encode(Sha256::digest(&bytes)));
        result.insert(key.clone(), id.clone());
        staged.push((id, bytes));
    }
    validate(&result)?;
    for (id, bytes) in staged {
        publish(root, &id, &bytes)?;
    }
    Ok(result)
}
// Validate resource references and resolve them to managed local file paths
pub(crate) fn paths(root: &Path, resources: &Resources) -> Result<Resources, String> {
    validate(resources)?;
    resources
        .iter()
        .map(|(key, id)| Ok((key.clone(), path(root, id)?.to_string_lossy().into_owned())))
        .collect()
}
// Collect resource identifiers referenced by plugin actions in profiles and saved macros
pub(crate) fn referenced(data: &ProfileData) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for action in data
        .profiles
        .iter()
        .flat_map(|p| &p.pages)
        .flat_map(|p| p.bindings.values())
        .map(|b| &b.action)
        .chain(data.saved_macros.iter().map(|m| &m.action))
    {
        action.visit(&mut |a| {
            if let Action::Plugin { resources, .. } = a {
                ids.extend(resources.values().cloned());
            }
        });
    }
    ids
}
// Export the selected vendor resources data
pub(crate) fn export(
    root: &Path,
    data: &ProfileData,
    zip: &mut zip::ZipWriter<&mut std::fs::File>,
    expanded: &mut u64,
    count: &mut usize,
) -> Result<(), String> {
    for id in referenced(data) {
        let bytes = read(std::fs::File::open(path(root, &id)?).map_err(|e| e.to_string())?)?;
        if hex::encode(Sha256::digest(&bytes)) != id.split_once('.').unwrap().0 {
            return Err("Managed resource content changed".into());
        }
        *expanded += bytes.len() as u64;
        *count += 1;
        if *expanded > 256 * 1024 * 1024 || *count > 256 {
            return Err("Profile resources exceed archive limits".into());
        }
        zip.start_file(
            format!("plugin-resources/{id}"),
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(6)),
        )
        .map_err(|e| e.to_string())?;
        zip.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    Ok(())
}
pub(crate) struct PreparedResourceImport {
    directory: tempfile::TempDir,
    ids: Vec<String>,
}
// Prepare import
pub(crate) fn prepare_import(
    data: &ProfileData,
    zip: &mut zip::ZipArchive<std::fs::File>,
) -> Result<PreparedResourceImport, String> {
    let mut staged = PreparedResourceImport {
        directory: tempfile::tempdir().map_err(|e| e.to_string())?,
        ids: Vec::new(),
    };
    let mut total = 0;
    for id in referenced(data) {
        validate_id(&id)?;
        let entry = zip
            .by_name(&format!("plugin-resources/{id}"))
            .map_err(|_| "Profile archive is missing an action resource")?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err("Resource archive links are not supported".into());
        }
        let bytes = read(entry)?;
        total += bytes.len() as u64;
        if total > 256 * 1024 * 1024 {
            return Err("Profile resources exceed archive limits".into());
        }
        if hex::encode(Sha256::digest(&bytes)) != id.split_once('.').unwrap().0 {
            return Err("Action resource content digest mismatch".into());
        }
        std::fs::write(staged.directory.path().join(&id), bytes).map_err(|e| e.to_string())?;
        staged.ids.push(id);
    }
    Ok(staged)
}
// Publish import
pub(crate) fn publish_import(root: &Path, staged: &PreparedResourceImport) -> Result<(), String> {
    for id in &staged.ids {
        let bytes = read(
            std::fs::File::open(staged.directory.path().join(id)).map_err(|e| e.to_string())?,
        )?;
        publish(root, id, &bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that managed content survives source removal and rejects escape
    fn managed_content_survives_source_removal_and_rejects_escape() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        std::fs::write(&source, "hello").unwrap();
        let resources = stage(
            root.path(),
            &BTreeMap::from([("file".into(), source.to_string_lossy().into_owned())]),
        )
        .unwrap();
        std::fs::remove_file(source).unwrap();
        assert_eq!(
            std::fs::read(paths(root.path(), &resources).unwrap()["file"].clone()).unwrap(),
            b"hello"
        );
        assert!(validate(&BTreeMap::from([("bad".into(), "../outside".into())])).is_err());
    }
    #[test]
    // Verify that invalid source preserves existing content
    fn invalid_source_preserves_existing_content() {
        let root = tempfile::tempdir().unwrap();
        assert!(stage(
            root.path(),
            &BTreeMap::from([("file".into(), root.path().to_string_lossy().into_owned())])
        )
        .is_err());
        assert!(!root.path().join("plugin-resources").exists());
    }
}

impl crate::Runtime {
    // Validate resource files and attach their identifiers and resolved paths to the authorized panel
    pub fn initialize_panel_resources(
        &self,
        token: &str,
        ids: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let paths = paths(&self.profiles.root, &ids)?;
        self.plugins.lock().unwrap().set_panel_resources(
            token,
            serde_json::to_value(ids).map_err(|e| e.to_string())?,
            serde_json::to_value(paths).map_err(|e| e.to_string())?,
        )
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;
    use crate::profiles::ProfileStore;
    #[test]
    // Verify that publication syncs parent then blob directory and propagates retry failures
    fn publication_syncs_parent_then_blob_directory_and_propagates_retry_failures() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"durable resource";
        let id = format!("{}.bin", hex::encode(Sha256::digest(bytes)));
        let dir = root.path().join("plugin-resources");
        let target = dir.join(&id);
        let mut calls = Vec::new();
        let error = publish_with_sync(root.path(), &id, bytes, |path| {
            calls.push(path.to_owned());
            if path == dir {
                assert!(target.exists());
                return Err("injected directory flush failure".into());
            }
            assert!(!target.exists());
            Ok(())
        })
        .unwrap_err();
        assert!(error.contains("injected"));
        assert_eq!(calls, vec![root.path().to_owned(), dir.clone()]);
        // Existing-content reuse must not bypass durability after a failed flush
        assert!(
            publish_with_sync(root.path(), &id, bytes, |_| Err("retry failure".into())).is_err()
        );
        calls.clear();
        publish_with_sync(root.path(), &id, bytes, |path| {
            calls.push(path.to_owned());
            stream_dock_manager_storage::sync_directory(path)
        })
        .unwrap();
        assert_eq!(calls, vec![root.path().to_owned(), dir]);
    }
    #[test]
    // Verify that parent flush failure prevents blob publication
    fn parent_flush_failure_prevents_blob_publication() {
        let root = tempfile::tempdir().unwrap();
        let bytes = b"uncommitted resource";
        let id = format!("{}.bin", hex::encode(Sha256::digest(bytes)));
        assert!(publish_with_sync(root.path(), &id, bytes, |_| Err(
            "parent flush failure".into()
        ))
        .is_err());
        assert!(!root.path().join("plugin-resources").join(id).exists());
    }
    #[test]
    // Verify that invalid media never publishes other valid resource files
    fn invalid_media_never_publishes_other_valid_resource_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(temp.path().join("target")).unwrap();
        let mut data = store.load().unwrap();
        let bytes = b"valid action resource";
        let id = format!("{}.txt", hex::encode(Sha256::digest(bytes)));
        let asset = uuid::Uuid::new_v4().to_string();
        let binding = data.profiles[0].pages[0].bindings.get_mut("key-1").unwrap();
        binding.action = Action::Plugin {
            user_desired_state: None,
            plugin_id: "example".into(),
            action_id: "example.run".into(),
            settings: serde_json::json!({}),
            resources: Resources::from([("file".into(), id.clone())]),
        };
        binding.asset = Some(asset.clone());
        let archive = temp.path().join("bad.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        for (name, contents) in [
            (
                "profiles.json".to_owned(),
                serde_json::to_vec(&data).unwrap(),
            ),
            (format!("plugin-resources/{id}"), bytes.to_vec()),
            (format!("assets/{asset}.png"), b"not a PNG".to_vec()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&contents).unwrap();
        }
        zip.finish().unwrap();
        assert!(store.import_for_repair(&archive).is_err());
        assert!(!store.root.join("plugin-resources").join(id).exists());
        assert!(!store
            .root
            .join("assets")
            .join(format!("{asset}.png"))
            .exists());
    }
    #[test]
    // Verify that action files round trip archives and are excluded without settings consent
    fn action_files_round_trip_archives_and_are_excluded_without_settings_consent() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input.txt");
        std::fs::write(&source, b"portable resource").unwrap();
        let store = ProfileStore::open(temp.path().join("source")).unwrap();
        let resources = stage(
            &store.root,
            &Resources::from([("document".into(), source.to_string_lossy().into_owned())]),
        )
        .unwrap();
        let mut data = store.load().unwrap();
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action = Action::Plugin {
            user_desired_state: None,
            plugin_id: "test.plugin".into(),
            action_id: "test.plugin.action".into(),
            settings: serde_json::json!({}),
            resources: resources.clone(),
        };
        let archive = temp.path().join("profile.zip");
        store.export_archive(&archive, data.clone(), true).unwrap();
        std::fs::remove_file(&source).unwrap();
        let other = ProfileStore::open(temp.path().join("target")).unwrap();
        let imported = other.import_for_repair(&archive).unwrap();
        assert_eq!(referenced(&imported), resources.values().cloned().collect());
        assert_eq!(
            std::fs::read(paths(&other.root, &resources).unwrap()["document"].clone()).unwrap(),
            b"portable resource"
        );
        let redacted = temp.path().join("redacted.zip");
        store.export_archive(&redacted, data, false).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(redacted).unwrap()).unwrap();
        assert!(!zip
            .file_names()
            .any(|name| name.unwrap().starts_with("plugin-resources/")));
        let mut document = String::new();
        zip.by_name("profiles.json")
            .unwrap()
            .read_to_string(&mut document)
            .unwrap();
        assert!(!document.contains(resources.values().next().unwrap()));
    }
    #[test]
    // Verify that corrupted managed content cannot be exported
    fn corrupted_managed_content_cannot_be_exported() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("input.txt");
        std::fs::write(&source, b"original").unwrap();
        let store = ProfileStore::open(temp.path().join("source")).unwrap();
        let resources = stage(
            &store.root,
            &Resources::from([("file".into(), source.to_string_lossy().into_owned())]),
        )
        .unwrap();
        let mut data = store.load().unwrap();
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action = Action::Plugin {
            user_desired_state: None,
            plugin_id: "test.plugin".into(),
            action_id: "test.plugin.action".into(),
            settings: serde_json::json!({}),
            resources: resources.clone(),
        };
        std::fs::write(
            path(&store.root, resources.values().next().unwrap()).unwrap(),
            "corrupted",
        )
        .unwrap();
        assert!(store
            .export_archive(&temp.path().join("export.zip"), data, true)
            .unwrap_err()
            .contains("content changed"));
        assert!(!temp.path().join("export.zip").exists());
    }
}
