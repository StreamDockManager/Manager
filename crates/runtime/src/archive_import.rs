use std::{fs::File, io::Read, path::Path};

pub const MAX_DROPPED_ARCHIVE: u64 = 288 * 1024 * 1024;
const MAX_WORKSPACE_DOCUMENT: u64 = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveImportKind {
    Plugin,
    Profiles,
    Macros,
}

/// Identify the review flow without extracting files or staging an import
pub fn identify(path: &Path) -> Result<ArchiveImportKind, String> {
    let metadata = path.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_DROPPED_ARCHIVE {
        return Err("Choose a ZIP archive up to 288 MiB.".into());
    }
    let mut archive = zip::ZipArchive::new(File::open(path).map_err(|e| e.to_string())?)
        .map_err(|_| "Choose a plugin, profile or macro ZIP archive.")?;
    if archive.len() > 4096 {
        return Err("The archive contains too many files.".into());
    }

    // Match the package installer's root or top-level .sdPlugin layouts
    let mut workspace = false;
    let mut plugin = false;
    for name in archive.file_names() {
        let name = name.map_err(|e| e.to_string())?;
        workspace |= name == "profiles.json";
        plugin |= name == "manifest.json"
            || name.split_once('/').is_some_and(|(folder, file)| {
                file == "manifest.json" && folder.to_ascii_lowercase().ends_with(".sdplugin")
            });
    }
    if plugin && workspace {
        return Err(
            "This ZIP has both plugin and workspace documents. Choose its import flow manually."
                .into(),
        );
    }
    if plugin {
        return Ok(ArchiveImportKind::Plugin);
    }
    if !workspace {
        return Err("This ZIP does not contain a plugin, profile or macro export.".into());
    }

    // Macro-only exports and profile exports both use profiles.json. A workspace
    // with profiles may also contain their referenced macros
    stream_dock_manager_plugin_runtimes::archives::validate_lzma_headers(&mut archive)?;
    let document = archive
        .by_name("profiles.json")
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    document
        .take(MAX_WORKSPACE_DOCUMENT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_WORKSPACE_DOCUMENT {
        return Err("The workspace document exceeds 16 MiB.".into());
    }
    let data: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid profiles.json: {e}"))?;
    let profiles = data["profiles"]
        .as_array()
        .ok_or("The workspace document has no profiles list.")?;
    let has_macros = data["saved_macros"]
        .as_array()
        .is_some_and(|macros| !macros.is_empty());
    let has_styles = data["styles"]
        .as_array()
        .is_some_and(|styles| !styles.is_empty());
    Ok(if profiles.is_empty() && has_macros && !has_styles {
        ArchiveImportKind::Macros
    } else {
        ArchiveImportKind::Profiles
    })
}
