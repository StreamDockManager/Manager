use std::{fs::File, io::Write};
use stream_dock_manager_runtime::archive_import::{identify, ArchiveImportKind};

#[test]
// Verify that identifies renamed exports and leaves mixed workspaces in profile review
fn identifies_renamed_exports_and_leaves_mixed_workspaces_in_profile_review() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("renamed.bin");
    for (entries, expected) in [
        (vec![("manifest.json", "{}")], Ok(ArchiveImportKind::Plugin)),
        (vec![("org.example.SDPlugin/manifest.json", "{}")], Ok(ArchiveImportKind::Plugin)),
        (vec![("profiles.json", r#"{"profiles":[{}],"saved_macros":[{}]}"#)], Ok(ArchiveImportKind::Profiles)),
        (vec![("profiles.json", r#"{"profiles":[],"saved_macros":[{}]}"#)], Ok(ArchiveImportKind::Macros)),
        (
            vec![("profiles.json", r#"{"profiles":[],"saved_macros":[{}],"styles":[{}]}"#)],
            Ok(ArchiveImportKind::Profiles),
        ),
        (vec![("profiles.json", r#"{"profiles":[]}"#)], Ok(ArchiveImportKind::Profiles)),
        (vec![("unrelated/manifest.json", "{}")], Err("This ZIP does not contain a plugin, profile or macro export.")),
        (vec![("manifest.json", "{}"), ("profiles.json", "{}")], Err("This ZIP has both plugin and workspace documents. Choose its import flow manually.")),
    ] {
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        for (name, body) in entries {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        assert_eq!(identify(&path).as_ref().map_err(String::as_str), expected.as_ref().map_err(|error| *error));
    }
}
