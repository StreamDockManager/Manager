//! Managed media inventory and complete reachability scans. Destructive cleanup uses the same gates
use crate::{
    asset_leases::AssetPins,
    profiles::{ProfileData, ProfileStore},
    Runtime,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MediaLibraryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub clip_info: Option<crate::video::ClipInfo>,
    pub id: String,
    pub format: String,
    #[ts(type = "number")]
    pub logical_bytes: u64,
    #[ts(type = "number")]
    pub estimated_reclaimable_bytes: u64,
    pub usages: Vec<String>,
    pub protection: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub frame_rate: Option<f64>,
    pub error: Option<String>,
    pub can_delete: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MediaLibraryPage {
    pub retention_checked: bool,
    pub generation: String,
    pub page: usize,
    pub page_size: usize,
    pub total: usize,
    #[ts(type = "number")]
    pub logical_bytes: u64,
    #[ts(type = "number")]
    pub estimated_reclaimable_bytes: u64,
    pub storage_estimate_available: bool,
    pub cleanup_available: bool,
    pub scan_errors: Vec<String>,
    pub entries: Vec<MediaLibraryEntry>,
}
// Present the media library result
fn present(path: &Path) -> Result<bool, String> {
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("Retention source cannot be inspected: {error}")),
    }
}
// Read regular
pub(crate) fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let metadata = path.symlink_metadata().map_err(|e| {
        format!(
            "{}: {e}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit as u64 {
        return Err("Retention source is not a bounded regular file".into());
    }
    let mut bytes = vec![];
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Retention source exceeds size limit".into());
    }
    Ok(bytes)
}
#[cfg(test)]
thread_local! {
    static LIBRARY_WORK: std::cell::Cell<[u64; 3]> = const { std::cell::Cell::new([0; 3]) };
}
#[cfg(test)]
// Count library work
pub(crate) fn count_library_work(index: usize) {
    LIBRARY_WORK.with(|work| {
        let mut counts = work.get();
        counts[index] += 1;
        work.set(counts);
    });
}
#[derive(Clone)]
pub(crate) struct MediaFile {
    pub id: String,
    pub extension: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: u128,
    pub identity: String,
    pub links: Option<u64>,
    pub allocated: u64,
}
#[derive(Clone)]
pub(crate) struct MediaScan {
    pub generation: String,
    pub files: BTreeMap<String, MediaFile>,
    pub uses: BTreeMap<String, BTreeSet<String>>,
    pub protection: BTreeMap<String, BTreeSet<String>>,
    pub errors: Vec<String>,
}
type MediaMetadata = (String, u32, u32, Option<u32>, Option<f64>);
type CachedMediaMetadata = (String, MediaMetadata);

#[derive(Default)]
pub(crate) struct BrowseCache {
    scan: Option<(String, std::sync::Arc<MediaScan>)>,
    clips: BTreeMap<String, (String, Option<crate::video::ClipInfo>)>,
    metadata: BTreeMap<String, CachedMediaMetadata>,
    scans: u64,
    clip_reads: u64,
    metadata_reads: u64,
}
// Build a source stamp with platform-specific evidence needed to detect replacement
fn browse_file_stamp_with_limit(path: &Path, _limit: usize) -> Result<String, String> {
    let stamp = crate::profiles::SourceStamp::read_bounded(path, _limit as u64)?;
    #[cfg(unix)]
    {
        Ok(format!("{stamp:?}"))
    }
    #[cfg(not(unix))]
    {
        // Without change time, same-size/mtime writes require content validation
        // Parsing and indexing are still reused; never trust timestamps alone
        let bytes = read_regular(path, _limit)?;
        Ok(format!("{stamp:?}:{}", hex::encode(Sha256::digest(bytes))))
    }
}
impl ProfileStore {
    // Enumerate bounded managed files without opening recovery databases or reading their contents
    fn browse_inventory(&self) -> Result<MediaScan, String> {
        if crate::media_cleanup::pending(&self.root)? {
            return Err("Media cleanup recovery is pending; restart Manager".into());
        }
        let deleted_assets = self.deleted_media.lock().unwrap().assets.clone();
        let mut files = BTreeMap::new();
        let dir = self.root.join("assets");
        let meta = dir.symlink_metadata().map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Media directory must be a regular managed directory".into());
        }
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if files.len() >= 8192 {
                return Err("Media inventory exceeds 8,192 assets".into());
            }
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("Invalid media filename")?;
            // Finder/Explorer metadata is not managed media. Leave it alone
            // instead of letting it invalidate the asset inventory
            if matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini") || name.starts_with("._") {
                continue;
            }
            let (id, extension) = name
                .rsplit_once('.')
                .ok_or("Unrecognized file in managed media")?;
            crate::profiles::validate_asset_id(id)?;
            if !crate::media::ASSET_EXTENSIONS.contains(&extension) {
                return Err("Unrecognized file in managed media".into());
            }
            if deleted_assets.contains(id) {
                continue;
            }
            let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || meta.len() > crate::media::MAX_ASSET as u64
            {
                return Err("Media inventory contains an unsafe or oversized file".into());
            }
            let modified = meta
                .modified()
                .map_err(|e| e.to_string())?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos();
            #[cfg(unix)]
            let (identity, links, allocated) = {
                use std::os::unix::fs::MetadataExt;
                (
                    format!("{}:{}", meta.dev(), meta.ino()),
                    Some(meta.nlink()),
                    meta.blocks().saturating_mul(512),
                )
            };
            #[cfg(not(unix))]
            let (identity, links, allocated) = (id.to_string(), None, meta.len());
            let file = MediaFile {
                id: id.into(),
                extension: extension.into(),
                path: path.clone(),
                bytes: meta.len(),
                modified,
                identity,
                links,
                allocated,
            };
            if files.insert(id.to_owned(), file).is_some() {
                return Err("Ambiguous media ID has multiple representations".into());
            }
        }

        Ok(MediaScan {
            generation: String::new(),
            files,
            uses: BTreeMap::new(),
            protection: BTreeMap::new(),
            errors: Vec::new(),
        })
    }

    // Fingerprint library records, source stamps and asset-pin state for inventory reuse
    fn browse_key(&self, pins: &AssetPins) -> Result<String, String> {
        let mut sources = BTreeMap::new();
        let change = self.storage.run(|connection| {
            connection
                .query_row("SELECT total_changes()", [], |row| row.get::<_, u64>(0))
                .map_err(|e| e.to_string())
        })?;
        sources.insert(
            "database".into(),
            format!("{}:{change}", self.storage.generation()),
        );
        for path in self.storage.backup_paths()? {
            sources.insert(
                format!("recovery:{}", path.display()),
                browse_file_stamp_with_limit(&path, 512 * 1024 * 1024)?,
            );
        }
        for name in ["assets", "media-cleanup"] {
            let directory = self.root.join(name);
            if !present(&directory)? {
                continue;
            }
            let metadata = directory.symlink_metadata().map_err(|e| e.to_string())?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("Unsafe media directory".into());
            }
            for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if sources.len() >= 16384 {
                    return Err("Media browse index exceeds limit".into());
                }
                let relative = format!("{name}/{}", entry.file_name().to_string_lossy());
                let limit = if name == "history" {
                    32 * 1024 * 1024
                } else {
                    crate::media::MAX_ASSET
                };
                sources.insert(
                    relative,
                    browse_file_stamp_with_limit(&entry.path(), limit)?,
                );
            }
        }
        let deleted = self.deleted_media.lock().unwrap();
        let mut owners: Vec<_> = pins.live.values().collect();
        owners.sort();
        Ok(hex::encode(Sha256::digest(
            serde_json::to_vec(&(sources, owners, deleted.revision, &deleted.assets))
                .map_err(|e| e.to_string())?,
        )))
    }
    // Reuse a matching library scan or refresh the cached inventory
    fn browse_scan(&self, pins: &AssetPins) -> std::sync::Arc<MediaScan> {
        let key = self.browse_key(pins).ok();
        if let Some(key) = &key {
            let cache = self.media_browse_cache.lock().unwrap();
            if let Some((saved, scan)) = &cache.scan {
                if saved == key {
                    return scan.clone();
                }
            }
        }
        let scan = std::sync::Arc::new(self.scan_media(pins));
        let stable = key.is_some() && self.browse_key(pins).ok() == key;
        let mut cache = self.media_browse_cache.lock().unwrap();
        cache.scans += 1;
        cache.scan = if stable && scan.errors.is_empty() {
            Some((key.unwrap(), scan.clone()))
        } else {
            None
        };
        cache.clips.retain(|id, _| scan.files.contains_key(id));
        cache.metadata.retain(|id, _| scan.files.contains_key(id));
        scan
    }
    // Cache prepared-clip provenance only while its source stamp remains unchanged
    fn browse_clip_info(&self, file: &MediaFile) -> Option<crate::video::ClipInfo> {
        if file.extension != "sdmedia" {
            return None;
        }
        let stamp = format!("{:?}", crate::profiles::SourceStamp::read(&file.path).ok()?);
        #[cfg(unix)]
        if let Some((saved, info)) = self.media_browse_cache.lock().unwrap().clips.get(&file.id) {
            if saved == &stamp {
                return info.clone();
            }
        }
        let info = crate::media::clip_info_archive(std::fs::File::open(&file.path).ok()?).ok()?;
        if crate::profiles::SourceStamp::read(&file.path)
            .ok()
            .map(|s| format!("{s:?}"))
            .as_ref()
            == Some(&stamp)
        {
            let mut cache = self.media_browse_cache.lock().unwrap();
            cache.clip_reads += 1;
            if cache.clips.len() >= 8192 {
                cache.clips.clear();
            }
            cache.clips.insert(file.id.clone(), (stamp, info.clone()));
        }
        info
    }
    // Reuse parsed media metadata only for an unchanged source stamp
    fn browse_metadata(&self, file: &MediaFile) -> Result<MediaMetadata, String> {
        let stamp = format!("{:?}", crate::profiles::SourceStamp::read(&file.path)?);
        #[cfg(unix)]
        if let Some((saved, metadata)) = self
            .media_browse_cache
            .lock()
            .unwrap()
            .metadata
            .get(&file.id)
        {
            if saved == &stamp {
                return Ok(metadata.clone());
            }
        }
        let metadata = self.media_metadata(file)?;
        if format!("{:?}", crate::profiles::SourceStamp::read(&file.path)?) != stamp {
            return Err("Media changed during inspection; refresh the library".into());
        }
        let mut cache = self.media_browse_cache.lock().unwrap();
        cache.metadata_reads += 1;
        if cache.metadata.len() >= 8192 {
            cache.metadata.clear();
        }
        cache
            .metadata
            .insert(file.id.clone(), (stamp, metadata.clone()));
        Ok(metadata)
    }
}
// Add an entry to media library
fn add(map: &mut BTreeMap<String, BTreeSet<String>>, id: &str, reason: String) {
    let reasons = map.entry(id.into()).or_default();
    if reasons.len() < 32 {
        reasons.insert(reason);
    } else {
        reasons.insert("Additional uses retained".into());
    }
}
// Collect asset references and their reasons from profiles, macros and styles
fn data_uses(
    data: &ProfileData,
    reason: &str,
    detailed: bool,
    uses: &mut BTreeMap<String, BTreeSet<String>>,
) {
    for profile in &data.profiles {
        if let Some(id) = &profile.background_asset {
            add(
                uses,
                id,
                if detailed {
                    format!("{} / background", profile.name)
                } else {
                    reason.into()
                },
            );
        }
        for page in &profile.pages {
            for (control, binding) in &page.bindings {
                if let Some(id) = &binding.asset {
                    add(
                        uses,
                        id,
                        if detailed {
                            crate::macro_debug::bounded(
                                format!("{} / {} / {}", profile.name, page.name, control),
                                320,
                            )
                        } else {
                            reason.into()
                        },
                    );
                }
            }
        }
    }
}
impl ProfileStore {
    /// Lock order: workspace transaction → exclusive media gate → asset pins
    /// No runtime state, draft, import, or device lock may be acquired during this scan
    pub(crate) fn scan_media(&self, pins: &AssetPins) -> MediaScan {
        #[cfg(test)]
        count_library_work(0);
        let (deleted_revision, deleted_assets) = {
            let deleted = self.deleted_media.lock().unwrap();
            (deleted.revision, deleted.assets.clone())
        };
        let mut uses = BTreeMap::new();
        let mut protection = BTreeMap::new();
        let mut errors = vec![];
        match crate::media_cleanup::pending(&self.root) {
            Ok(true) => errors
                .push("Media cleanup recovery is pending. Restart Manager before cleanup.".into()),
            Err(error) => errors.push(error),
            Ok(false) => {}
        }
        let mut hashes = vec![];
        match self
            .storage
            .run(|connection| stream_dock_manager_plugin_host::palette_media_references(connection))
        {
            Ok(references) => {
                hashes.push(format!(
                    "palette-wallpapers:{}",
                    serde_json::to_string(&references).unwrap_or_default()
                ));
                for (asset, name) in references {
                    add(&mut protection, &asset, format!("Palette: {name}"));
                }
            }
            Err(error) => errors.push(format!("Palette retention: {error}")),
        }
        let mut histories = BTreeSet::new();
        let current = (|| -> Result<ProfileData, String> {
            let data = self.load()?;
            let bytes = serde_json::to_vec(&data).map_err(|e| e.to_string())?;
            hashes.push(format!("current:{}", hex::encode(Sha256::digest(bytes))));
            Ok(data)
        })();
        match current {
            Ok(data) => {
                data_uses(&data, "Current workspace", true, &mut uses);
                for id in crate::profiles::referenced_assets(&data) {
                    add(&mut protection, &id, "Current workspace".into());
                }
                histories.extend(
                    data.history
                        .undo
                        .iter()
                        .chain(&data.history.redo)
                        .map(|h| h.id.clone()),
                );
            }
            Err(error) => errors.push(format!("Workspace retention: {error}")),
        }
        match self.storage.backup_paths().and_then(|paths| {
            for path in paths {
                let assets = self
                    .storage
                    .read_snapshot(&path, crate::workspace::snapshot_media)?;
                hashes.push(format!(
                    "recovery:{}:{}",
                    path.display(),
                    browse_file_stamp_with_limit(&path, 512 * 1024 * 1024)?
                ));
                for id in assets {
                    add(&mut protection, &id, "Database recovery snapshot".into());
                }
            }
            Ok(())
        }) {
            Ok(()) => {}
            Err(error) => errors.push(format!("Recovery retention: {error}")),
        }
        match self.retained_media(&histories) {
            Ok((ids, entries)) => {
                hashes.extend(entries);
                for id in ids {
                    add(&mut protection, &id, "Retained undo/redo".into());
                }
            }
            Err(error) => errors.push(format!("History retention: {error}")),
        }
        {
            match (|| -> Result<(), String> {
                let drafts: BTreeMap<String, crate::workspace::Draft> = self
                    .read_preference(crate::profile_database::LocalPreference::Drafts)?
                    .unwrap_or_default();
                let bytes = serde_json::to_vec(&drafts).map_err(|e| e.to_string())?;
                hashes.push(format!("drafts:{}", hex::encode(Sha256::digest(&bytes))));
                if drafts.len() > 1000 {
                    return Err("Draft retention exceeds limit".into());
                }
                for draft in drafts.values() {
                    if let Some(id) = draft.base.as_ref().and_then(|b| b.asset.as_ref()) {
                        crate::profiles::validate_asset_id(id)?;
                        add(&mut protection, id, "Saved draft base".into());
                    }
                    let value = draft
                        .value
                        .as_object()
                        .ok_or("Unrecognized saved draft representation")?;
                    if let Some(value) = value.get("asset").filter(|v| !v.is_null()) {
                        let id = value.as_str().ok_or("Invalid draft asset")?;
                        if !id.is_empty() {
                            crate::profiles::validate_asset_id(id)?;
                            add(&mut protection, id, "Saved inspector draft".into());
                        }
                    }
                }
                Ok(())
            })() {
                Ok(()) => {}
                Err(error) => errors.push(format!("Draft retention: {error}")),
            }
        }
        for (reason, ids) in pins.live.values() {
            for id in ids {
                add(&mut protection, id, reason.clone());
            }
        }
        let mut files = BTreeMap::new();
        let inventory_complete = match (|| -> Result<(), String> {
            let dir = self.root.join("assets");
            let meta = dir.symlink_metadata().map_err(|e| e.to_string())?;
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err("Media directory must be a regular managed directory".into());
            }
            for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if files.len() >= 8192 {
                    return Err("Media inventory exceeds 8,192 assets".into());
                }
                let path = entry.path();
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or("Invalid media filename")?;
                // Finder/Explorer metadata is not managed media. Leave it alone
                // instead of letting it invalidate the asset inventory
                if matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini")
                    || name.starts_with("._")
                {
                    continue;
                }
                let (id, extension) = name
                    .rsplit_once('.')
                    .ok_or("Unrecognized file in managed media")?;
                crate::profiles::validate_asset_id(id)?;
                if !crate::media::ASSET_EXTENSIONS.contains(&extension) {
                    return Err("Unrecognized file in managed media".into());
                }
                if deleted_assets.contains(id) {
                    continue;
                }
                let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
                if !meta.is_file()
                    || meta.file_type().is_symlink()
                    || meta.len() > crate::media::MAX_ASSET as u64
                {
                    return Err("Media inventory contains an unsafe or oversized file".into());
                }
                let modified = meta
                    .modified()
                    .map_err(|e| e.to_string())?
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_nanos();
                #[cfg(unix)]
                let (identity, links, allocated) = {
                    use std::os::unix::fs::MetadataExt;
                    (
                        format!("{}:{}", meta.dev(), meta.ino()),
                        Some(meta.nlink()),
                        meta.blocks().saturating_mul(512),
                    )
                };
                #[cfg(not(unix))]
                let (identity, links, allocated) = (id.to_string(), None, meta.len());
                let file = MediaFile {
                    id: id.into(),
                    extension: extension.into(),
                    path: path.clone(),
                    bytes: meta.len(),
                    modified,
                    identity,
                    links,
                    allocated,
                };
                if files.insert(id.to_owned(), file).is_some() {
                    return Err("Ambiguous media ID has multiple representations".into());
                }
            }
            Ok(())
        })() {
            Ok(()) => true,
            Err(error) => {
                errors.push(format!("Media inventory: {error}"));
                false
            }
        };
        // The durable deletion index takes precedence over retained history,
        // saved drafts and late leases. Their integrity is still checked above;
        // intentionally removed files must not block the next deletion
        uses.retain(|id, _| !deleted_assets.contains(id));
        protection.retain(|id, _| !deleted_assets.contains(id));
        hashes.push(format!(
            "deletions:{deleted_revision}:{}",
            hex::encode(Sha256::digest(serde_json::to_vec(&deleted_assets).unwrap()))
        ));
        // An interrupted inventory cannot establish that an unvisited asset is
        // missing. Its inspection error still blocks destructive operations
        if inventory_complete {
            for id in protection.keys() {
                if !files.contains_key(id) {
                    errors.push(format!("Referenced media {id} is missing"));
                }
            }
        }
        errors.sort();
        errors.dedup();
        if errors.len() > 64 {
            errors.truncate(64);
            errors.push("Further retention errors omitted; repair these and rescan.".into());
        }
        let inventory: Vec<_> = files
            .values()
            .map(|f| {
                (
                    &f.id,
                    &f.extension,
                    f.bytes,
                    f.modified,
                    &f.identity,
                    f.links,
                    f.allocated,
                )
            })
            .collect();
        let protected: Vec<_> = protection.keys().collect();
        let generation = hex::encode(Sha256::digest(
            serde_json::to_vec(&(hashes, inventory, protected, &errors)).unwrap(),
        ));
        MediaScan {
            generation,
            files,
            uses,
            protection,
            errors,
        }
    }
    // Inspect bounded media metadata while holding preparation admission and checking source identity
    pub(crate) fn media_metadata(&self, file: &MediaFile) -> Result<MediaMetadata, String> {
        #[cfg(test)]
        count_library_work(1);
        let _inspection = self.media_gate.lock()?;
        let stamp = crate::profiles::SourceStamp::read(&file.path)?;
        let metadata = if file.extension == "sdmedia" {
            let preview = crate::media::preview_archive(
                std::fs::File::open(&file.path).map_err(|e| e.to_string())?,
            )?;
            (
                preview.format,
                preview.width,
                preview.height,
                Some(preview.duration_ms),
                Some(preview.frame_rate),
            )
        } else if file.extension == "svg" {
            let bytes = read_regular(&file.path, 256 * 1024)?;
            let (width, height) = hardware_core::render::svg_media_dimensions(&bytes)
                .map_err(|error| error.to_string())?;
            ("svg".into(), width, height, None, None)
        } else {
            let reader = image::ImageReader::open(&file.path)
                .map_err(|e| e.to_string())?
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let format = reader
                .format()
                .map(|f| f.extensions_str()[0].to_string())
                .unwrap_or("unknown".into());
            let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
            (format, w, h, None, None)
        };
        if stamp != crate::profiles::SourceStamp::read(&file.path)? {
            return Err("Media changed during inspection; refresh the library".into());
        }
        Ok(metadata)
    }
}
impl MediaScan {
    // Estimate reclaimable bytes without double-counting shared file identities
    pub(crate) fn reclaimable(&self, selected: &BTreeSet<String>) -> (u64, bool) {
        let mut groups: BTreeMap<&str, Vec<&MediaFile>> = BTreeMap::new();
        for id in selected {
            if let Some(file) = self.files.get(id) {
                groups.entry(&file.identity).or_default().push(file);
            }
        }
        let mut total = 0u64;
        let mut known = true;
        for files in groups.values() {
            let first = files[0];
            match first.links {
                Some(links) if links == files.len() as u64 => {
                    total = total.saturating_add(first.allocated)
                }
                Some(_) => {}
                None => known = false,
            }
        }
        (total, known)
    }
}
impl Runtime {
    // Pin the selected asset and encode a bounded image thumbnail for the UI
    pub fn media_thumbnail(&self, asset: &str) -> Result<String, String> {
        use base64::Engine;
        let _lease = self
            .profiles
            .asset_leases
            .pin([asset.to_string()], "Media thumbnail")?;
        let extension = self.profiles.asset_extension(asset)?;
        let image = if extension == "sdmedia" {
            let path = self
                .profiles
                .root
                .join("assets")
                .join(format!("{asset}.sdmedia"));
            let stamp = crate::profiles::SourceStamp::read(&path)?;
            let render = || {
                let _inspection = self.profiles.media_gate.lock()?;
                let preview = crate::media::preview_archive(
                    std::fs::File::open(&path).map_err(|e| e.to_string())?,
                )?;
                if stamp != crate::profiles::SourceStamp::read(&path)? {
                    return Err("Media changed during thumbnail read; refresh the library".into());
                }
                self.cached_render(
                    crate::media::RenderKey::from_inputs(&(
                        "media-first-frame-v1",
                        hex::encode(Sha256::digest(&preview.frame)),
                    ))?,
                    || {
                        hardware_core::render::render(&preview.frame, 96, 96, 0, "png")
                            .map(crate::media::shared_bytes)
                            .map_err(|e| e.to_string())
                    },
                )
            };
            // Unix change time and inode detect in-place writes and replacement
            // Platforms without that detector re-read the bounded first frame;
            // its exact content hash still shares the rendered thumbnail
            #[cfg(unix)]
            {
                self.cached_render(
                    crate::media::RenderKey::from_inputs(&(
                        "media-thumbnail-v1",
                        asset,
                        format!("{stamp:?}"),
                    ))?,
                    render,
                )?
            }
            #[cfg(not(unix))]
            {
                render()?
            }
        } else {
            self.profiles.render_media_shared(
                asset,
                std::time::Duration::ZERO,
                96,
                96,
                0,
                "png",
                &Default::default(),
            )?
        };
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&**image)
        ))
    }
    // Retain media
    pub fn retain_media(&self, asset: String) -> Result<String, String> {
        if self.media_selections.lock().unwrap().len() >= 128 {
            return Err("Close an unfinished media selection first".into());
        }
        let lease = self
            .profiles
            .asset_leases
            .pin([asset.clone()], "Unfinished media selection")?;
        self.profiles.asset_extension(&asset)?;
        // Listing inspects only headers/first pixels. Fully validate the one
        // animation being selected at the existing owned-selection boundary
        self.profiles.clip(&asset)?;
        self.profiles.asset_extension(&asset)?;
        let mut selections = self.media_selections.lock().unwrap();
        if selections.len() >= 128 {
            return Err("Close an unfinished media selection first".into());
        }
        let id = lease.id().to_string();
        selections.insert(id.clone(), lease);
        Ok(id)
    }
    // Release media
    pub fn release_media(&self, id: &str) {
        self.media_selections.lock().unwrap().remove(id);
    }
    // Return ordinary inventory without a retention audit or destructive eligibility claim
    pub fn media_library(
        &self,
        page: usize,
        page_size: usize,
        query: &str,
    ) -> Result<MediaLibraryPage, String> {
        self.media_library_page(page, page_size, query, false)
    }
    // Explicitly audit retention before offering unused-media cleanup
    pub fn audit_media_library(
        &self,
        page: usize,
        page_size: usize,
        query: &str,
    ) -> Result<MediaLibraryPage, String> {
        self.media_library_page(page, page_size, query, true)
    }
    // Build only requested page metadata after selecting the inventory or authoritative audit
    fn media_library_page(
        &self,
        page: usize,
        page_size: usize,
        query: &str,
        audit: bool,
    ) -> Result<MediaLibraryPage, String> {
        if query.len() > 512 || ![12, 24, 48].contains(&page_size) {
            return Err("Invalid media library page".into());
        }
        let _transaction = audit.then(|| self.profiles.transaction.lock().unwrap());
        let scan = if audit {
            let pins = self.profiles.asset_leases.lock().clone();
            self.profiles.browse_scan(&pins)
        } else {
            std::sync::Arc::new(self.profiles.browse_inventory()?)
        };
        let cleanup_available = audit && scan.errors.is_empty();
        let logical_bytes = scan.files.values().map(|f| f.bytes).sum();
        let unused = scan
            .files
            .keys()
            .filter(|id| !scan.protection.contains_key(*id))
            .cloned()
            .collect();
        let (estimated_reclaimable_bytes, storage_estimate_available) = if audit {
            scan.reclaimable(&unused)
        } else {
            (0, false)
        };
        let query = query.trim().to_lowercase();
        let mut filtered: Vec<_> = scan
            .files
            .values()
            .filter(|f| {
                format!(
                    "{} {} {}",
                    f.id,
                    f.extension,
                    scan.uses
                        .get(&f.id)
                        .map(|v| v.iter().cloned().collect::<Vec<_>>().join(" "))
                        .unwrap_or_default()
                )
                .to_lowercase()
                .contains(&query)
                    || (!query.is_empty()
                        && f.extension == "sdmedia"
                        && self.profiles.browse_clip_info(f).is_some_and(|info| {
                            format!("{} {}", info.name, info.recording.name)
                                .to_lowercase()
                                .contains(&query)
                        }))
            })
            .collect();
        filtered.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.id.cmp(&b.id)));
        let total = filtered.len();
        let pages = total.div_ceil(page_size).max(1);
        let page = page.clamp(1, pages);
        let visible: Vec<_> = filtered
            .into_iter()
            .skip((page - 1) * page_size)
            .take(page_size)
            .collect();
        let _page_lease = self.profiles.asset_leases.pin(
            visible.iter().map(|file| file.id.clone()),
            "Media library inspection",
        )?;
        // Page assets are pinned; slow headers never hold the audit transaction
        // Ordinary browsing has no authoritative reachability or cleanup generation
        drop(_transaction);
        let mut entries = vec![];
        for file in visible {
            let protection = scan
                .protection
                .get(&file.id)
                .map(|v| v.iter().cloned().collect())
                .unwrap_or_default();
            let mut entry = MediaLibraryEntry {
                clip_info: None,
                id: file.id.clone(),
                format: file.extension.clone(),
                logical_bytes: file.bytes,
                estimated_reclaimable_bytes: if !audit || scan.protection.contains_key(&file.id) {
                    0
                } else {
                    scan.reclaimable(&BTreeSet::from([file.id.clone()])).0
                },
                usages: scan
                    .uses
                    .get(&file.id)
                    .map(|v| v.iter().cloned().collect())
                    .unwrap_or_default(),
                protection,
                width: None,
                height: None,
                duration_ms: None,
                frame_rate: None,
                error: None,
                can_delete: cleanup_available && !scan.protection.contains_key(&file.id),
            };
            match self.profiles.browse_metadata(file) {
                Ok((format, w, h, duration, frame_rate)) => {
                    entry.format = format;
                    entry.width = Some(w);
                    entry.height = Some(h);
                    entry.duration_ms = duration;
                    entry.frame_rate = frame_rate;
                }
                Err(error) => entry.error = Some(error),
            }
            if file.extension == "sdmedia" {
                entry.clip_info = self.profiles.browse_clip_info(file);
            }
            entries.push(entry);
        }
        let deleted = self.profiles.deleted_media.lock().unwrap();
        entries.retain(|entry| !deleted.assets.contains(&entry.id));
        drop(deleted);
        Ok(MediaLibraryPage {
            retention_checked: audit,
            generation: scan.generation.clone(),
            page,
            page_size,
            total,
            logical_bytes,
            estimated_reclaimable_bytes,
            storage_estimate_available,
            cleanup_available,
            scan_errors: scan.errors.clone(),
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{Draft, Edit};
    #[test]
    // Slow database inspection must not reserve render permits or live asset ownership
    fn slow_inventory_keeps_snapshot_and_render_admission_responsive() {
        use std::sync::mpsc;
        use std::time::Duration;
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("one.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 60, 255]))
            .save(&source)
            .unwrap();
        runtime.profiles.import_asset(&source).unwrap();
        let (entered, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let storage_runtime = runtime.clone();
        let storage = std::thread::spawn(move || {
            storage_runtime.profiles.storage.run(move |_| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            })
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let browse_runtime = runtime.clone();
        let (listed, listing_ready) = mpsc::channel();
        let browse = std::thread::spawn(move || {
            listed
                .send(browse_runtime.media_library(1, 12, ""))
                .unwrap()
        });
        let listed_without_storage = listing_ready.recv_timeout(Duration::from_secs(1));
        let admitted = runtime.profiles.media_gate.try_lock().is_ok();
        let probe_runtime = runtime.clone();
        let (sent, received) = mpsc::channel();
        let probe = std::thread::spawn(move || {
            let snapshot = probe_runtime.snapshot();
            let lease = probe_runtime
                .profiles
                .asset_leases
                .pin([], "Unrelated renderer")
                .unwrap();
            sent.send(snapshot.configuration_revision).unwrap();
            drop(lease);
        });
        let responsive = received.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        storage.join().unwrap().unwrap();
        browse.join().unwrap();
        assert_eq!(
            listed_without_storage
                .expect("Listing waited for storage")
                .unwrap()
                .entries
                .len(),
            1
        );
        probe.join().unwrap();
        assert!(admitted, "Read-only inventory blocked a device decoder");
        assert!(
            responsive.is_ok(),
            "Read-only inventory blocked a snapshot or asset pin"
        );
        runtime.shutdown();
    }

    #[test]
    #[ignore = "Controlled release-mode media browsing comparison; owned fixtures, never HID"]
    // Verify that patch browse profile
    fn patch_browse_profile() {
        let mut runs = vec![];
        for _ in 0..5 {
            let root = tempfile::tempdir().unwrap();
            let runtime = Runtime::build(root.path().join("app"), false).unwrap();
            let path = root.path().join("synthetic.gif");
            std::fs::write(&path, crate::media::tests::sample_gif()).unwrap();
            for _ in 0..200 {
                runtime.profiles.import_asset(&path).unwrap();
            }
            LIBRARY_WORK.with(|counts| counts.set([0; 3]));
            let started = std::time::Instant::now();
            let cold = runtime.media_library(1, 24, "absent-search-value").unwrap();
            let cold_us = started.elapsed().as_secs_f64() * 1e6;
            let cold_work = LIBRARY_WORK.with(|counts| counts.replace([0; 3]));
            assert_eq!(cold.total, 0);
            let started = std::time::Instant::now();
            for _ in 0..10 {
                let page = runtime.media_library(1, 24, "absent-search-value").unwrap();
                assert_eq!(page.generation, cold.generation);
                assert_eq!(page.total, 0);
                assert!(!page.retention_checked);
                assert!(!page.cleanup_available);
            }
            let warm_mean_us = started.elapsed().as_secs_f64() * 100_000.0;
            let warm_work = LIBRARY_WORK.with(|counts| counts.get());
            runs.push(serde_json::json!({"cold_us":cold_us,"warm_mean_us":warm_mean_us,"cold_work":cold_work,"warm_work":warm_work,"logical_bytes":cold.logical_bytes,"files":200}));
            runtime.shutdown();
        }
        eprintln!(
            "PATCH_BROWSE_PROFILE={}",
            serde_json::to_string(&runs).unwrap()
        );
    }
    #[test]
    // Corrupt recovery evidence blocks destructive review but never ordinary inventory
    fn one_item_browse_skips_corrupt_recovery_audit() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("one.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 60, 255]))
            .save(&source)
            .unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        runtime.profiles.storage.backup_recovery().unwrap();
        for path in runtime.profiles.storage.backup_paths().unwrap() {
            std::fs::write(path, b"deliberately corrupt owned fixture").unwrap();
        }
        LIBRARY_WORK.with(|counts| counts.set([0; 3]));
        let started = std::time::Instant::now();
        let page = runtime.media_library(1, 12, "png").unwrap();
        eprintln!("MEDIA_ONE_ITEM_WITH_CORRUPT_BACKUP={:?}", started.elapsed());
        assert_eq!(page.entries.len(), 1);
        assert!(!page.retention_checked);
        assert!(!page.cleanup_available);
        assert!(!page.entries[0].can_delete);
        assert_eq!(LIBRARY_WORK.with(|counts| counts.get()[0]), 0);
        assert!(runtime.prepare_media_deletion(&asset).is_err());
        let audit = runtime.audit_media_library(1, 12, "").unwrap();
        assert!(audit.retention_checked);
        assert!(!audit.cleanup_available);
        assert!(!audit.scan_errors.is_empty());
        runtime.shutdown();
    }

    #[test]
    // Bound first-page inspection independently of the total animated media inventory
    fn first_page_inspection_is_bounded_and_warm_reopen_reuses_metadata() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("sample.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        for _ in 0..120 {
            runtime.profiles.import_asset(&source).unwrap();
        }
        LIBRARY_WORK.with(|counts| counts.set([0; 3]));
        let started = std::time::Instant::now();
        let first = runtime.media_library(1, 12, "").unwrap();
        let cold = started.elapsed();
        let work = LIBRARY_WORK.with(|counts| counts.replace([0; 3]));
        assert_eq!(first.total, 120);
        assert_eq!(first.entries.len(), 12);
        assert_eq!(work[0], 0, "Browsing must not run retention scans");
        assert_eq!(work[1], 12);
        assert_eq!(
            runtime.profiles.source_work_stats()["clip_archive_reads"],
            0
        );
        let started = std::time::Instant::now();
        let again = runtime.media_library(1, 12, "").unwrap();
        eprintln!(
            "MEDIA_FIRST_PAGE cold={cold:?} warm={:?} work={work:?}",
            started.elapsed()
        );
        assert_eq!(first.generation, again.generation);
        assert_eq!(
            first.entries.iter().map(|e| &e.id).collect::<Vec<_>>(),
            again.entries.iter().map(|e| &e.id).collect::<Vec<_>>()
        );
        assert_eq!(LIBRARY_WORK.with(|counts| counts.get()), [0; 3]);
        runtime.shutdown();
    }
    #[test]
    // Verify that browse inventory reuses parsing and invalidates files drafts and leases
    fn browse_inventory_reuses_parsing_and_invalidates_files_drafts_and_leases() {
        let (_root, store, file) = setup();
        let id = store.import_asset(&file).unwrap();
        let initial = store.browse_scan(&store.asset_leases.lock());
        let repeated = store.browse_scan(&store.asset_leases.lock());
        assert!(std::sync::Arc::ptr_eq(&initial, &repeated));
        assert_eq!(store.media_browse_cache.lock().unwrap().scans, 1);
        let metadata = &initial.files[&id];
        store.browse_metadata(metadata).unwrap();
        store.browse_metadata(metadata).unwrap();
        assert_eq!(store.media_browse_cache.lock().unwrap().metadata_reads, 1);
        let lease = store
            .asset_leases
            .pin([id.clone()], "Fixture owner")
            .unwrap();
        let protected = store.browse_scan(&store.asset_leases.lock());
        assert!(protected.protection.contains_key(&id));
        drop(lease);
        assert!(!store
            .browse_scan(&store.asset_leases.lock())
            .protection
            .contains_key(&id));
        store
            .write_preference(
                crate::profile_database::LocalPreference::Drafts,
                &serde_json::json!(["invalid drafts"]),
            )
            .unwrap();
        assert!(!store
            .browse_scan(&store.asset_leases.lock())
            .errors
            .is_empty());
        store
            .storage
            .delete(
                stream_dock_manager_storage::Domain::LocalPreferences,
                "drafts",
            )
            .unwrap();
        let next = store.import_asset(&file).unwrap();
        assert!(store
            .browse_scan(&store.asset_leases.lock())
            .files
            .contains_key(&next));
        std::fs::write(&metadata.path, b"bad").unwrap();
        assert!(store.browse_metadata(metadata).is_err());
        // Destructive reviews always use a fresh authoritative scan
        assert!(!store
            .scan_media(&store.asset_leases.lock())
            .files
            .is_empty());
    }
    #[test]
    // Verify that cold browsing and thumbnails do not load playback clips
    fn cold_browsing_and_thumbnails_do_not_load_playback_clips() {
        use base64::Engine;
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("colors.gif");
        let original = crate::media::tests::sample_gif();
        std::fs::write(&source, &original).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        runtime.profiles.forget_asset(&asset);
        let page = runtime.media_library(1, 12, "").unwrap();
        let entry = page.entries.iter().find(|entry| entry.id == asset).unwrap();
        assert_eq!(
            (
                entry.format.as_str(),
                entry.width,
                entry.height,
                entry.duration_ms
            ),
            ("gif", Some(4), Some(4), Some(200))
        );
        assert!(entry.error.is_none());
        assert_eq!(entry.frame_rate, Some(10.0));
        let red = runtime.media_thumbnail(&asset).unwrap();
        assert_eq!(runtime.media_thumbnail(&asset).unwrap(), red);
        assert_eq!(
            runtime.profiles.source_work_stats()["clip_archive_reads"],
            0
        );
        let rgb = |image: &str| {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(image.split_once(',').unwrap().1)
                .unwrap();
            image::load_from_memory(&bytes)
                .unwrap()
                .to_rgb8()
                .get_pixel(48, 48)
                .0
        };
        assert_eq!(rgb(&red), [240, 0, 0]);
        let clip = crate::media::prepare(&original).unwrap();
        let changed = crate::media::Clip::new(
            "gif",
            vec![clip.frames[1].to_vec(), clip.frames[0].to_vec()],
            vec![100, 100],
        )
        .unwrap();
        let replacement = root.path().join("replacement.sdmedia");
        std::fs::write(&replacement, changed.archive(&original).unwrap()).unwrap();
        std::fs::rename(
            &replacement,
            runtime
                .profiles
                .root
                .join("assets")
                .join(format!("{asset}.sdmedia")),
        )
        .unwrap();
        let blue = runtime.media_thumbnail(&asset).unwrap();
        assert_eq!(rgb(&blue), [0, 0, 240]);
        assert_ne!(red, blue);
        assert_eq!(
            runtime.profiles.source_work_stats()["clip_archive_reads"],
            0
        );
        let lease = runtime.retain_media(asset.clone()).unwrap();
        assert_eq!(
            runtime.profiles.source_work_stats()["clip_archive_reads"],
            1
        );
        runtime.release_media(&lease);
        runtime.shutdown();
    }
    #[test]
    // Verify that selecting a listed animation still validates frames after its preview
    fn selecting_a_listed_animation_still_validates_frames_after_its_preview() {
        use std::io::{Cursor, Write};
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("colors.gif");
        let gif = crate::media::tests::sample_gif();
        std::fs::write(&source, &gif).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let clip = crate::media::prepare(&gif).unwrap();
        let bytes = clip.archive(&gif).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(vec![]));
        let second = format!("frames/{}.image", hex::encode(clip.frame_digests[1]));
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let mut data = vec![];
            entry.read_to_end(&mut data).unwrap();
            if entry.name().unwrap() == second {
                data = b"corrupt second frame".to_vec();
            }
            writer
                .start_file(
                    entry.name().unwrap(),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(&data).unwrap();
        }
        runtime.profiles.forget_asset(&asset);
        std::fs::write(
            runtime
                .profiles
                .root
                .join("assets")
                .join(format!("{asset}.sdmedia")),
            writer.finish().unwrap().into_inner(),
        )
        .unwrap();
        let page = runtime.media_library(1, 12, "").unwrap();
        assert!(page.entries[0].error.is_none());
        assert!(runtime.media_thumbnail(&asset).is_ok());
        assert!(runtime
            .retain_media(asset)
            .unwrap_err()
            .contains("integrity"));
        assert!(runtime.media_selections.lock().unwrap().is_empty());
        runtime.shutdown();
    }
    // Create a temporary profile store and a small sample PNG for library tests
    fn setup() -> (tempfile::TempDir, ProfileStore, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        store.load().unwrap();
        let file = root.path().join("sample.png");
        std::fs::write(
            &file,
            hardware_core::render::tile(4, 4, [10, 20, 30], "", 0, "png").unwrap(),
        )
        .unwrap();
        (root, store, file)
    }
    #[test]
    // Verify that desktop metadata does not hide assets or invalidate the inventory
    fn desktop_metadata_does_not_hide_assets_or_invalidate_the_inventory() {
        let (_root, store, file) = setup();
        let ids: Vec<_> = (0..4).map(|_| store.import_asset(&file).unwrap()).collect();
        let mut data = store.load().unwrap();
        data.profiles[0].background_asset = Some(ids[0].clone());
        for (index, id) in ids[1..].iter().enumerate() {
            data.profiles[0].pages[0]
                .bindings
                .get_mut(&format!("key-{}", index + 1))
                .unwrap()
                .asset = Some(id.clone());
        }
        store.save(&data).unwrap();
        let original = serde_json::to_vec(&store.load().unwrap()).unwrap();
        let before = store.scan_media(&store.asset_leases.lock());
        assert!(before.errors.is_empty(), "{:?}", before.errors);
        let metadata = [
            ".DS_Store".into(),
            "Thumbs.db".into(),
            "desktop.ini".into(),
            format!("._{}.png", ids[0]),
        ];
        for name in &metadata {
            std::fs::write(store.root.join("assets").join(name), b"desktop metadata").unwrap();
        }
        let scan = store.scan_media(&store.asset_leases.lock());
        assert!(scan.errors.is_empty(), "{:?}", scan.errors);
        assert_eq!(scan.files.len(), 4);
        assert_eq!(scan.generation, before.generation);
        for id in &ids {
            assert!(scan.files.contains_key(id));
            assert!(scan.uses.contains_key(id));
            assert!(scan.protection.contains_key(id));
        }
        assert_eq!(
            serde_json::to_vec(&store.load().unwrap()).unwrap(),
            original
        );
        for name in &metadata {
            assert_eq!(
                std::fs::read(store.root.join("assets").join(name)).unwrap(),
                b"desktop metadata"
            );
        }
    }
    #[test]
    // Verify that incomplete inventory does not misreport unscanned references as missing
    fn incomplete_inventory_does_not_misreport_unscanned_references_as_missing() {
        let (_root, store, file) = setup();
        let present = store.import_asset(&file).unwrap();
        let missing = uuid::Uuid::new_v4().to_string();
        let mut data = store.load().unwrap();
        data.profiles[0].background_asset = Some(missing.clone());
        store.save(&data).unwrap();
        let unexpected = store.root.join("assets/not-an-asset.png");
        std::fs::write(&unexpected, b"preserve unexpected data").unwrap();
        let scan = store.scan_media(&store.asset_leases.lock());
        assert!(scan
            .errors
            .iter()
            .any(|e| e.starts_with("Media inventory:")));
        assert!(!scan
            .errors
            .iter()
            .any(|e| e.starts_with("Referenced media")));
        assert_eq!(
            std::fs::read(&unexpected).unwrap(),
            b"preserve unexpected data"
        );
        assert!(store.asset_extension(&present).is_ok());
        std::fs::remove_file(&unexpected).unwrap();
        let scan = store.scan_media(&store.asset_leases.lock());
        assert!(scan
            .errors
            .iter()
            .any(|e| e == &format!("Referenced media {missing} is missing")));
        assert_eq!(
            store.load().unwrap().profiles[0].background_asset.as_ref(),
            Some(&missing)
        );
    }
    #[test]
    // Verify that scan protects workspace history recovery drafts and live owners
    fn scan_protects_workspace_history_recovery_drafts_and_live_owners() {
        let (_root, store, file) = setup();
        let ids: Vec<_> = (0..6).map(|_| store.import_asset(&file).unwrap()).collect();
        let mut data = store.load().unwrap();
        let mut profile = data.profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().asset = Some(ids[0].clone());
        data = store
            .edit(data.configuration_revision, "First", |data| {
                crate::workspace::apply_edits(
                    data,
                    vec![Edit::SaveProfile {
                        profile: profile.clone(),
                    }],
                )
            })
            .unwrap();
        profile.pages[0].bindings.get_mut("key-1").unwrap().asset = Some(ids[1].clone());
        data = store
            .edit(data.configuration_revision, "Second", |data| {
                crate::workspace::apply_edits(data, vec![Edit::SaveProfile { profile }])
            })
            .unwrap();
        let mut recovery = data.clone();
        // A current recovery snapshot keeps the history index for its retained
        // history rows; clearing only the index would create orphan records
        recovery.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .asset = Some(ids[2].clone());
        store.save(&recovery).unwrap();
        store.storage.backup_recovery().unwrap();
        store.save(&data).unwrap();
        store
            .write_preference(
                crate::profile_database::LocalPreference::Drafts,
                &BTreeMap::from([(
                    "draft",
                    Draft {
                        key: "draft".into(),
                        base: None,
                        value: serde_json::json!({"asset":ids[3]}),
                    },
                )]),
            )
            .unwrap();
        let lease = store
            .asset_leases
            .pin([ids[4].clone()], "Staged work")
            .unwrap();
        let scan = store.scan_media(&store.asset_leases.lock());
        assert!(scan.errors.is_empty(), "{:?}", scan.errors);
        for id in &ids[..5] {
            assert!(scan.protection.contains_key(id), "Unprotected {id}");
        }
        assert!(!scan.protection.contains_key(&ids[5]));
        assert!(scan.uses[&ids[1]]
            .iter()
            .any(|usage| usage.contains("key-1")));
        let before = scan.generation;
        let duplicate = store
            .asset_leases
            .pin([ids[4].clone()], "Concurrent renderer")
            .unwrap();
        assert_eq!(
            store.scan_media(&store.asset_leases.lock()).generation,
            before
        );
        drop(duplicate);
        assert_eq!(
            store.scan_media(&store.asset_leases.lock()).generation,
            before
        );
        drop(lease);
        assert_ne!(
            store.scan_media(&store.asset_leases.lock()).generation,
            before
        );
    }
    #[test]
    // Verify that incomplete or corrupt retention blocks cleanup instead of discarding it
    fn incomplete_or_corrupt_retention_blocks_cleanup_instead_of_discarding_it() {
        let (_root, store, file) = setup();
        let id = store.import_asset(&file).unwrap();
        let data = store.load().unwrap();
        let mut profile = data.profiles[0].clone();
        profile.name = "Renamed".into();
        let data = store
            .edit(data.configuration_revision, "Rename", |data| {
                crate::workspace::apply_edits(data, vec![Edit::SaveProfile { profile }])
            })
            .unwrap();
        let history = &data.history.undo[0].id;
        let before: serde_json::Value = store
            .storage
            .read_json(
                stream_dock_manager_storage::Domain::History,
                history,
                32 * 1024 * 1024,
            )
            .unwrap()
            .unwrap();
        store
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::History,
                history,
                &serde_json::json!(["invalid history"]),
                1024,
            )
            .unwrap();
        let scan = store.scan_media(&store.asset_leases.lock());
        assert!(scan
            .errors
            .iter()
            .any(|e| e.contains("retention") && e.contains("history")));
        assert!(scan.files.contains_key(&id));
        assert_eq!(
            store
                .storage
                .read_json::<serde_json::Value>(
                    stream_dock_manager_storage::Domain::History,
                    history,
                    1024
                )
                .unwrap(),
            Some(serde_json::json!(["invalid history"]))
        );
        store
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::History,
                history,
                &before,
                32 * 1024 * 1024,
            )
            .unwrap();
        store
            .write_preference(
                crate::profile_database::LocalPreference::Drafts,
                &serde_json::json!({"draft":{"key":"draft","base":null,"value":null}}),
            )
            .unwrap();
        assert!(store
            .scan_media(&store.asset_leases.lock())
            .errors
            .iter()
            .any(|e| e.contains("Draft retention")));
    }
    #[cfg(unix)]
    #[test]
    // Verify that storage estimates count shared physical storage only after its last link
    fn storage_estimates_count_shared_physical_storage_only_after_its_last_link() {
        let (_root, store, file) = setup();
        let first = store.import_asset(&file).unwrap();
        let second = store.import_asset(&file).unwrap();
        let scan = store.scan_media(&store.asset_leases.lock());
        assert!(scan.errors.is_empty());
        assert_eq!(scan.files[&first].identity, scan.files[&second].identity);
        assert_eq!(
            scan.reclaimable(&BTreeSet::from([first.clone()])),
            (0, true)
        );
        let all = BTreeSet::from([first.clone(), second]);
        let (reclaimable, known) = scan.reclaimable(&all);
        assert!(known);
        assert_eq!(reclaimable, scan.files[&first].allocated);
        assert!(scan.files.values().map(|file| file.bytes).sum::<u64>() > 0);
    }
}

#[cfg(test)]
mod concurrent_tests {
    use super::*;
    use std::sync::{mpsc, Arc, Barrier};
    #[test]
    #[ignore = "Requires disposable loopback host; controlled import/catalog concurrency, never HID"]
    // Verify that cold animation metadata and import commit follow one lock order
    fn cold_animation_metadata_and_import_commit_follow_one_lock_order() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let path = root.path().join("owned.gif");
        let mut encoder =
            image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
        encoder
            .encode_frame(image::Frame::from_parts(
                image::RgbaImage::from_pixel(2, 2, image::Rgba([100, 20, 30, 255])),
                0,
                0,
                image::Delay::from_numer_denom_ms(100, 1),
            ))
            .unwrap();
        drop(encoder);
        let asset = runtime.import_asset(&path).unwrap();
        runtime.release_media(&asset);
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().asset = Some(asset.clone());
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        let archive = root.path().join("profiles.zip");
        runtime.profiles.export(&archive).unwrap();
        let staged = runtime.prepare_profile_import(&archive).unwrap();
        assert!(staged.issues.is_empty(), "{:?}", staged.issues);
        runtime.profiles.forget_asset(&asset);
        let barrier = Arc::new(Barrier::new(4));
        let (sent, received) = mpsc::channel();
        let mut workers = vec![];
        for index in 0..3 {
            let runtime = runtime.clone();
            let barrier = barrier.clone();
            let sent = sent.clone();
            let id = staged.id.clone();
            let expected = staged.expected_revision;
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                let result = if index == 1 {
                    runtime.commit_profile_import(&id, expected).map(|_| ())
                } else {
                    runtime.media_library(1, 12, "").and_then(|page| {
                        if page
                            .entries
                            .iter()
                            .any(|entry| entry.format == "gif" && entry.width == Some(2))
                        {
                            Ok(())
                        } else {
                            Err("Cold animation metadata was unavailable".into())
                        }
                    })
                };
                sent.send(result).unwrap();
            }));
        }
        barrier.wait();
        for _ in 0..3 {
            received
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("Media/import lock order stalled")
                .unwrap();
        }
        for worker in workers {
            worker.join().unwrap();
        }
        runtime.shutdown();
    }
}
