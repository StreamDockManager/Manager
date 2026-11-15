use super::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub struct InstalledPackageExport {
    pub id: String,
    installation: Option<String>,
    root: PathBuf,
    files: BTreeMap<String, String>,
}

impl PluginHost {
    // Prepare an export of the installed plugin package
    pub fn installed_package_export(&self, id: &str) -> Result<InstalledPackageExport, String> {
        let plugin = self
            .plugins
            .get(id)
            .ok_or("Plugin is no longer installed")?;
        let root = self.root.join(id);
        if plugin.path != root {
            return Err("Only managed plugin packages can be exported".into());
        }
        let mut files = packages::inventory(&root)?;
        files.retain(|name, _| {
            !name
                .split('/')
                .any(|part| part.starts_with(".stream-dock-manager-"))
        });
        if !files.contains_key("manifest.json") {
            return Err("The installed package has no manifest".into());
        }
        Ok(InstalledPackageExport {
            id: id.into(),
            installation: self.installation_token(id),
            root,
            files,
        })
    }

    // Check export installation
    pub fn check_export_installation(&self, export: &InstalledPackageExport) -> Result<(), String> {
        if self.plugins.contains_key(&export.id)
            && self.installation_token(&export.id) == export.installation
        {
            Ok(())
        } else {
            Err("The plugin was replaced or removed. Review its files again.".into())
        }
    }
}

impl InstalledPackageExport {
    // Enumerate files belonging to this operation
    pub fn files(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    // Write for the current operation
    pub fn write(&self, selected: &[String], destination: &Path) -> Result<(), String> {
        if selected.len() > self.files.len() {
            return Err("Invalid package file selection".into());
        }
        let names: BTreeSet<_> = selected
            .iter()
            .map(String::as_str)
            .chain(["manifest.json"])
            .collect();
        let current = packages::inventory(&self.root)?;
        let mut archive = ZipWriter::new(File::create(destination).map_err(|e| e.to_string())?);
        let expected_count = names.len();
        for name in names {
            let reviewed = self
                .files
                .get(name)
                .ok_or("File is outside the package review")?;
            if current.get(name) != Some(reviewed) {
                return Err("A selected file changed. Review the plugin files again.".into());
            }
            let path = self.root.join(name);
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
                return Err("A selected file is no longer a regular package file".into());
            }
            let mut source = File::open(path)
                .map_err(|e| e.to_string())?
                .take(MAX_FILE_BYTES + 1);
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(6));
            #[cfg(unix)]
            let options = {
                use std::os::unix::fs::PermissionsExt;
                options.unix_permissions(if metadata.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                })
            };
            archive
                .start_file(format!("{}.sdPlugin/{name}", self.id), options)
                .map_err(|e| e.to_string())?;
            let size_written = io::copy(&mut source, &mut archive).map_err(|e| e.to_string())?;
            if size_written != metadata.len() {
                return Err("A selected file changed while building the ZIP".into());
            }
        }
        let zip = archive.finish().map_err(|e| e.to_string())?;
        if zip.metadata().map_err(|e| e.to_string())?.len() > MAX_ARCHIVE_BYTES {
            return Err("The exported plugin ZIP exceeds the package size limit".into());
        }
        zip.sync_all().map_err(|e| e.to_string())?;
        let parent = self.root.parent().ok_or("Missing installation root")?;
        let prepared = PluginHost::prepare_install(parent, destination)?;
        let actual = PluginHost::prepared_inventory(&prepared);
        if PluginHost::prepared_info(&prepared)?.id != self.id
            || actual.len() != expected_count
            || actual.iter().any(|(name, digest)| {
                self.files.get(name) != Some(digest)
                    || (name != "manifest.json" && !selected.contains(name))
            })
        {
            return Err("The ZIP differs from the reviewed files".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify current zip contains only reviewed files and round trips
    fn current_zip_contains_only_reviewed_files_and_round_trips() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("org.test.legacy.sdPlugin");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("manifest.json"),
            br#"{"Name":"Legacy Fixture","Version":"1.0","CodePath":"index.html","Actions":[{"UUID":"org.test.legacy.action","Name":"Action"}]}"#,
        ).unwrap();
        fs::write(source.join("index.html"), "<title>Legacy</title>").unwrap();
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(source.join("manifest.json")).unwrap()).unwrap();
        manifest["UUID"] = json!("org.test.legacy");
        manifest["SDKVersion"] = json!(1);
        manifest["StreamDockManager"] = json!({"ManifestDialect":"vsd"});
        crate::lifecycle_tests::current_descriptor(&source, &mut manifest);
        let mut host = PluginHost::new(fixture.path().join("plugins")).unwrap();
        host.install(&source).unwrap();
        let installed = host.root.join("org.test.legacy");
        fs::write(installed.join("user-data.json"), "{\"token\":\"private\"}").unwrap();
        fs::write(installed.join(".stream-dock-manager-provenance.json"), "{}").unwrap();

        let review = host.installed_package_export("org.test.legacy").unwrap();
        assert_eq!(
            review.files(),
            ["icon.svg", "index.html", "manifest.json", "user-data.json"]
        );
        let zip = fixture.path().join("export.zip");
        review
            .write(&["icon.svg".into(), "index.html".into()], &zip)
            .unwrap();
        let mut written = zip::ZipArchive::new(File::open(&zip).unwrap()).unwrap();
        for index in 0..written.len() {
            assert_eq!(
                written.by_index(index).unwrap().compression(),
                CompressionMethod::Deflated
            );
        }
        let staged = PluginHost::prepare_install(&host.root, &zip).unwrap();
        assert_eq!(
            PluginHost::prepared_info(&staged).unwrap().id,
            "org.test.legacy"
        );
        assert_eq!(
            PluginHost::prepared_inventory(&staged)
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["icon.svg", "index.html", "manifest.json"]
        );
        assert!(review.write(&["../secret".into()], &zip).is_err());

        fs::write(installed.join("index.html"), "<title>Changed</title>").unwrap();
        assert!(review
            .write(&["icon.svg".into(), "index.html".into()], &zip)
            .is_err());
    }
}
