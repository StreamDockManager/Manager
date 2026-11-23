//! Permanent media deletion. A durable tombstone is the commit point, so recovery,
//! old drafts and history cannot reassign a file after it has been removed
use crate::{
    media_library::read_regular, profiles::ProfileStore, workspace::Draft, ProfileData, Runtime,
    Snapshot,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path, time::Instant};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeletedMedia {
    pub schema_version: u32,
    pub revision: u64,
    pub assets: BTreeSet<String>,
}
impl DeletedMedia {
    // Read the requested media deletion data
    pub(crate) fn read(storage: &stream_dock_manager_storage::Storage) -> Result<Self, String> {
        let value: Self = crate::profile_database::DELETED_MEDIA_RECORD
            .read(storage)?
            .unwrap_or(Self {
                schema_version: 1,
                ..Default::default()
            });
        if value.schema_version != 1 || value.assets.len() > 65536 {
            return Err("Unsupported deleted media index; files preserved".into());
        }
        for id in &value.assets {
            crate::profiles::validate_asset_id(id)?;
        }
        Ok(value)
    }
    // Detach the owned media deletion resource
    pub(crate) fn detach(&self, data: &mut ProfileData) -> bool {
        let mut changed = data.configuration_revision < self.revision;
        data.configuration_revision = data.configuration_revision.max(self.revision);
        for profile in &mut data.profiles {
            if profile
                .background_asset
                .as_ref()
                .is_some_and(|id| self.assets.contains(id))
            {
                changed = true;
                profile.background_asset = None;
                profile.background_media = Default::default();
            }
            for binding in profile
                .pages
                .iter_mut()
                .flat_map(|p| p.bindings.values_mut())
            {
                if binding
                    .asset
                    .as_ref()
                    .is_some_and(|id| self.assets.contains(id))
                {
                    changed = true;
                    binding.asset = None;
                    binding.media = Default::default();
                }
            }
        }
        changed
    }
    // Detach draft
    pub(crate) fn detach_draft(&self, draft: &mut Draft) {
        if let Some(base) = &mut draft.base {
            if base
                .asset
                .as_ref()
                .is_some_and(|id| self.assets.contains(id))
            {
                base.asset = None;
                base.media = Default::default();
            }
        }
        if draft
            .value
            .get("asset")
            .and_then(|v| v.as_str())
            .is_some_and(|id| self.assets.contains(id))
        {
            if let Some(value) = draft.value.as_object_mut() {
                value.remove("asset");
                value.insert(
                    "media".into(),
                    serde_json::json!(crate::media::MediaOptions::default()),
                );
            }
        }
    }
    // Remove the selected media deletion data
    pub(crate) fn purge(&self, root: &Path) -> Result<(), String> {
        let directory = root.join("assets");
        let metadata = directory.symlink_metadata().map_err(|e| e.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Unsafe managed media directory".into());
        }
        for id in &self.assets {
            for extension in crate::media::ASSET_EXTENSIONS {
                let path = directory.join(format!("{id}.{extension}"));
                match path.symlink_metadata() {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.to_string()),
                    Ok(m)
                        if !m.is_file()
                            || m.file_type().is_symlink()
                            || m.len() > crate::media::MAX_ASSET as u64 =>
                    {
                        return Err("Unsafe deleted media file; preserved for inspection".into());
                    }
                    Ok(_) => std::fs::remove_file(path).map_err(|e| e.to_string())?,
                }
            }
        }
        Ok(())
    }
}
impl ProfileStore {
    // Detach deleted media
    pub(crate) fn detach_deleted_media(&self, data: &mut ProfileData) -> bool {
        self.deleted_media.lock().unwrap().detach(data)
    }
    // Detach deleted draft
    pub(crate) fn detach_deleted_draft(&self, draft: &mut Draft) {
        self.deleted_media.lock().unwrap().detach_draft(draft);
    }
    // Load media drafts
    pub(crate) fn load_media_drafts(
        &self,
    ) -> Result<std::collections::BTreeMap<String, Draft>, String> {
        let mut drafts: std::collections::BTreeMap<String, Draft> = self
            .read_preference(crate::profile_database::LocalPreference::Drafts)?
            .unwrap_or_default();
        for draft in drafts.values_mut() {
            self.detach_deleted_draft(draft);
        }
        Ok(drafts)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MediaDeletionReview {
    pub id: String,
    pub asset: String,
    pub usages: Vec<String>,
    pub retained_by: Vec<String>,
    pub active_devices: Vec<String>,
}
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MediaDeletionResult {
    pub snapshot: Snapshot,
    pub removed: String,
    pub warnings: Vec<String>,
}
pub(crate) struct StagedMediaDeletion {
    review: MediaDeletionReview,
    generation: String,
    sha256: String,
    created: Instant,
}
impl crate::review_store::ReviewSession for StagedMediaDeletion {
    const LIMIT: usize = 8;
    const FULL: &'static str = "Close another media deletion review first";
    const LIFETIME: Option<std::time::Duration> = Some(std::time::Duration::from_secs(300));
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
impl Runtime {
    // Prepare media deletion
    pub fn prepare_media_deletion(&self, asset: &str) -> Result<MediaDeletionReview, String> {
        crate::profiles::validate_asset_id(asset)?;
        let snapshot = self.snapshot();
        let mut stages = self.media_deletions.lock().unwrap();
        stages.check_capacity()?;
        let _transaction = self.profiles.transaction.lock().unwrap();
        // Review reads immutable assets only; decoder exclusion belongs to the confirmed commit
        let pins = self.profiles.asset_leases.lock().clone();
        let scan = self.profiles.scan_media(&pins);
        if !scan.errors.is_empty() {
            return Err(format!(
                "Media inspection is incomplete: {}",
                scan.errors.join("; ")
            ));
        }
        if scan
            .protection
            .get(asset)
            .is_some_and(|reasons| reasons.iter().any(|reason| reason.starts_with("Palette: ")))
        {
            return Err(
                "Remove this wallpaper from its palettes before deleting the media.".into(),
            );
        }
        let file = scan
            .files
            .get(asset)
            .ok_or("Media no longer exists. Refresh the library.")?;
        let active_devices = snapshot
            .devices
            .iter()
            .filter(|device| {
                let session = snapshot.session(&device.id);
                let profile = snapshot
                    .profiles
                    .iter()
                    .find(|p| p.id == session.profile_id);
                profile.is_some_and(|p| {
                    p.background_asset.as_deref() == Some(asset)
                        || p.pages
                            .iter()
                            .filter(|p| p.id == session.page_id)
                            .flat_map(|p| p.bindings.values())
                            .any(|b| b.asset.as_deref() == Some(asset))
                }) || snapshot
                    .binding_overrides
                    .get(&device.id)
                    .is_some_and(|bindings| {
                        bindings
                            .values()
                            .any(|b| b.binding.asset.as_deref() == Some(asset))
                    })
            })
            .map(|d| d.name.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let review = MediaDeletionReview {
            id: crate::review_store::new_id(),
            asset: asset.into(),
            usages: scan
                .uses
                .get(asset)
                .into_iter()
                .flatten()
                .cloned()
                .collect(),
            retained_by: scan
                .protection
                .get(asset)
                .into_iter()
                .flatten()
                .cloned()
                .collect(),
            active_devices,
        };
        stages.insert(
            review.id.clone(),
            StagedMediaDeletion {
                review: review.clone(),
                generation: scan.generation,
                sha256: hex::encode(Sha256::digest(read_regular(
                    &file.path,
                    crate::media::MAX_ASSET,
                )?)),
                created: Instant::now(),
            },
        )?;
        Ok(review)
    }
    // Discard the selected media deletion state
    pub fn discard_media_deletion(&self, id: &str) {
        self.media_deletions.lock().unwrap().remove(id);
    }
    // Publish durable deletion after all storage guards release without rolling back newer edits
    fn publish_media_deletion(&self, data: ProfileData) {
        let mut state = self.state.lock().unwrap();
        if state.configuration_revision < data.configuration_revision {
            Self::apply_data(&mut state, data);
        }
    }

    // Commit media deletion
    pub fn commit_media_deletion(&self, id: &str) -> Result<MediaDeletionResult, String> {
        let mut stages = self.media_deletions.lock().unwrap();
        let stage = stages.active(
            id,
            "Media deletion review expired",
            "Media deletion review expired. Review it again.",
        )?;
        // Disk validation and durable publication must not stop device snapshots or input
        // Publish to runtime state only after releasing workspace guards to preserve lock order
        let _transaction = self.profiles.transaction.lock().unwrap();
        let _media = self.profiles.media_gate.exclusive()?;
        let observed_pins = self.profiles.asset_leases.lock().clone();
        let scan = self.profiles.scan_media(&observed_pins);
        let mut pins = self.profiles.asset_leases.lock();
        // Readers may acquire pins during inspection; never commit against stale ownership
        if pins.generation != observed_pins.generation || pins.live != observed_pins.live {
            return Err("Media ownership changed. Cancel and review the deletion again.".into());
        }
        if !scan.errors.is_empty() || scan.generation != stage.generation {
            return Err("Media usage changed. Cancel and review the deletion again.".into());
        }
        let asset = stage.review.asset.clone();
        if scan
            .protection
            .get(&asset)
            .is_some_and(|reasons| reasons.iter().any(|reason| reason.starts_with("Palette: ")))
        {
            return Err(
                "Remove this wallpaper from its palettes before deleting the media.".into(),
            );
        }
        let file = scan.files.get(&asset).ok_or("Media no longer exists")?;
        if hex::encode(Sha256::digest(read_regular(
            &file.path,
            crate::media::MAX_ASSET,
        )?)) != stage.sha256
        {
            return Err("Media content changed. Cancel and review the deletion again.".into());
        }
        let mut data = self.profiles.load()?;
        let revision = data
            .configuration_revision
            .checked_add(1)
            .ok_or("Workspace revision limit reached")?;
        let mut deleted = self.profiles.deleted_media.lock().unwrap();
        if deleted.assets.len() >= 65536 {
            return Err("Deleted media index limit reached".into());
        }
        let mut candidate = DeletedMedia {
            schema_version: 1,
            revision,
            assets: deleted.assets.clone(),
        };
        candidate.assets.insert(asset.clone());
        candidate.detach(&mut data);
        let mut drafts = self.drafts.lock().unwrap();
        let mut next_drafts = drafts.clone();
        for draft in next_drafts.values_mut() {
            candidate.detach_draft(draft);
        }
        crate::profiles::validate_data(&data)?;
        let committed_data = data.clone();
        let committed_drafts = next_drafts.clone();
        let committed_deleted = candidate.clone();
        let expected = revision - 1;
        let cost = crate::profile_database::encoded_size(&committed_data, 16 * 1024 * 1024)?
            + crate::profile_database::encoded_size(&committed_drafts, 16 * 1024 * 1024)?
            + crate::profile_database::encoded_size(&committed_deleted, 4 * 1024 * 1024)?;
        self.profiles
            .storage
            .transaction_with_cost(cost, move |tx| {
                if crate::profile_database::load(tx)?
                    .ok_or("Workspace missing")?
                    .configuration_revision
                    != expected
                {
                    return Err("Workspace changed. Review media deletion again.".into());
                }
                crate::profile_database::DELETED_MEDIA_RECORD.write_to(tx, &committed_deleted)?;
                stream_dock_manager_storage::write_json(
                    tx,
                    stream_dock_manager_storage::Domain::LocalPreferences,
                    "drafts",
                    &committed_drafts,
                    16 * 1024 * 1024,
                )?;
                crate::profile_database::save(tx, &committed_data)
            })?;
        *deleted = candidate;
        *drafts = next_drafts;
        drop(deleted);
        drop(drafts);
        let mut warnings = vec![];
        for (_, assets) in pins.live.values_mut() {
            assets.remove(&asset);
        }
        pins.generation = pins.generation.wrapping_add(1);
        let selected = BTreeSet::from([asset.clone()]);
        // Restart recovery retries older tombstones; an interactive removal visits only its asset
        if let Err(e) = (DeletedMedia {
            schema_version: 1,
            revision,
            assets: selected.clone(),
        })
        .purge(&self.profiles.root)
        {
            warnings.push(format!(
                "Media unassigned; file removal will retry after restart: {e}"
            ));
        }
        if let Err(e) = self.profiles.finish_media_removal(&selected, &scan) {
            warnings.push(format!("Media deleted; cache index update failed: {e}"));
        }
        stages.remove(id);
        drop(pins);
        drop(_media);
        drop(_transaction);
        drop(stages);
        self.publish_media_deletion(data);
        self.emit("profiles", None, None, None);
        self.emit("media-deleted", None, None, Some(asset.clone()));
        let _ = self.device_commands.send(crate::devices::Request::Restore);
        Ok(MediaDeletionResult {
            snapshot: self.snapshot(),
            removed: asset,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{actions::Action, workspace::Edit};
    // Construct an isolated media deletion fixture
    fn fixture() -> (tempfile::TempDir, std::sync::Arc<Runtime>, String, String) {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        runtime
            .simulate_device(true, "simulator:n4-pro:delete-media")
            .unwrap();
        let path = root.path().join("media.gif");
        {
            let mut encoder =
                image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
            for color in [[24, 80, 150, 255], [120, 40, 60, 255]] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(16, 16, image::Rgba(color)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let asset = runtime.import_asset(&path).unwrap();
        let other_path = root.path().join("other.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([50, 80, 90, 255]))
            .save(&other_path)
            .unwrap();
        let other = runtime.import_asset(&other_path).unwrap();
        (root, runtime, asset, other)
    }
    #[test]
    // Delayed durable publication preserves newer configuration and unrelated session state
    fn delayed_deletion_publication_is_monotonic_and_preserves_sessions() {
        let (_root, runtime, _, _) = fixture();
        let mut data = runtime.profiles.load().unwrap();
        data.configuration_revision += 1;
        let device = runtime.snapshot().devices[0].clone();
        {
            let mut state = runtime.state.lock().unwrap();
            let mut session = state.session(&device.id);
            session.automation_paused = true;
            state
                .sessions
                .insert(device.persistence_key.clone(), session);
        }
        runtime.publish_media_deletion(data.clone());
        assert!(runtime.snapshot().session(&device.id).automation_paused);
        {
            let mut state = runtime.state.lock().unwrap();
            state.configuration_revision += 1;
            state.profiles[0].name = "Newer committed edit".into();
        }
        runtime.publish_media_deletion(data);
        assert_eq!(runtime.snapshot().profiles[0].name, "Newer committed edit");
        runtime.shutdown();
    }

    #[test]
    // Slow deletion inspection leaves snapshots and unrelated live state available
    fn slow_deletion_does_not_hold_runtime_state() {
        use std::sync::mpsc;
        use std::time::Duration;
        let (_root, runtime, asset, _) = fixture();
        let review = runtime.prepare_media_deletion(&asset).unwrap();
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
        let deleting_runtime = runtime.clone();
        let deleting =
            std::thread::spawn(move || deleting_runtime.commit_media_deletion(&review.id));
        let deadline = Instant::now() + Duration::from_secs(5);
        while runtime.profiles.transaction.try_lock().is_ok() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let reached_storage = runtime.profiles.transaction.try_lock().is_err();
        let probe_runtime = runtime.clone();
        let (sent, received) = mpsc::channel();
        let probe = std::thread::spawn(move || sent.send(probe_runtime.snapshot()).unwrap());
        let responsive = received.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        storage.join().unwrap().unwrap();
        assert!(
            reached_storage,
            "Worker did not reach the deliberately paused storage"
        );
        deleting.join().unwrap().unwrap();
        probe.join().unwrap();
        assert!(
            responsive.is_ok(),
            "Slow storage held the runtime state lock"
        );
        runtime.shutdown();
    }

    #[test]
    // Verify that deleting used media does not block later deletions or cleanup after restart
    fn deleting_used_media_does_not_block_later_deletions_or_cleanup_after_restart() {
        let (root, runtime, asset, other) = fixture();
        // Opening the managed folder in Finder must not break deletion or make
        // the remaining referenced media appear missing after the first removal
        std::fs::write(
            runtime.profiles.root.join("assets/.DS_Store"),
            b"Finder metadata",
        )
        .unwrap();
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile.background_asset = Some(asset.clone());
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        // Populate the history proof cache with the original reference
        assert!(
            runtime
                .audit_media_library(1, 12, "")
                .unwrap()
                .cleanup_available
        );
        let review = runtime.prepare_media_deletion(&asset).unwrap();
        runtime.commit_media_deletion(&review.id).unwrap();
        let page = runtime.audit_media_library(1, 12, "").unwrap();
        assert!(page.cleanup_available, "{:?}", page.scan_errors);
        let review = runtime.prepare_media_deletion(&other).unwrap();
        runtime.commit_media_deletion(&review.id).unwrap();
        let page = runtime.audit_media_library(1, 12, "").unwrap();
        assert!(page.cleanup_available, "{:?}", page.scan_errors);
        runtime.shutdown();
        let retired = std::sync::Arc::downgrade(&runtime);
        drop(runtime);
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while retired.strong_count() > 0 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            retired.strong_count(),
            0,
            "Runtime must release its configuration before reopening"
        );
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let page = runtime.audit_media_library(1, 12, "").unwrap();
        assert!(page.cleanup_available, "{:?}", page.scan_errors);
        // A genuinely missing file must still block destructive operations
        let missing = runtime
            .import_asset(&root.path().join("other.png"))
            .unwrap();
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile.background_asset = Some(missing.clone());
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        std::fs::remove_file(
            runtime
                .profiles
                .root
                .join("assets")
                .join(format!("{missing}.png")),
        )
        .unwrap();
        let page = runtime.audit_media_library(1, 12, "").unwrap();
        assert!(!page.cleanup_available);
        assert!(page
            .scan_errors
            .iter()
            .any(|error| error.contains(&missing)));
        runtime.shutdown();
    }
    #[test]
    // Verify that used media deletion detaches profiles drafts history and active output
    fn used_media_deletion_detaches_profiles_drafts_history_and_active_output() {
        let (root, runtime, asset, other) = fixture();
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile.background_asset = Some(asset.clone());
        let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
        binding.asset = Some(asset.clone());
        binding.title = "Keep title".into();
        binding.action = Action::None;
        let original = binding.clone();
        profile.pages[0]
            .bindings
            .insert("touch-1".into(), original.clone());
        let mut disabled = original.clone();
        disabled.disabled = true;
        profile.pages[0].bindings.insert("key-7".into(), disabled);
        runtime
            .save_profile(profile.clone(), runtime.snapshot().configuration_revision)
            .unwrap();
        profile.pages[0].bindings.get_mut("key-1").unwrap().title = "Changed title".into();
        runtime
            .save_profile(profile.clone(), runtime.snapshot().configuration_revision)
            .unwrap();
        let draft = Draft {
            key: "fixture".into(),
            base: Some(original.clone()),
            value: serde_json::json!({"asset":asset,"title":"Unsaved text","media":crate::media::MediaOptions::default(),"unknown":false}),
        };
        runtime.save_draft("fixture", Some(draft.clone())).unwrap();
        let _owner = runtime.retain_media(asset.clone()).unwrap();
        let id = "simulator:n4-pro:delete-media";
        let before = runtime.preview_surfaces(Some(id)).unwrap();
        assert!(before.animated);
        let review = runtime.prepare_media_deletion(&asset).unwrap();
        assert!(review.usages.iter().any(|s| s.contains("key-1")));
        assert!(!review.active_devices.is_empty());
        let history_len = runtime.snapshot().history.undo.len();
        let result = runtime.commit_media_deletion(&review.id).unwrap();
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert!(runtime.profiles.asset_extension(&asset).is_err());
        assert!(runtime.profiles.asset_extension(&other).is_ok());
        let imported_again = runtime
            .import_asset(&root.path().join("media.gif"))
            .unwrap();
        assert_ne!(
            imported_again, asset,
            "An intentional reimport gets a usable asset identity"
        );
        assert!(runtime.profiles.asset_extension(&imported_again).is_ok());
        assert_eq!(result.snapshot.history.undo.len(), history_len);
        for profile in &result.snapshot.profiles {
            assert!(profile.background_asset.is_none());
            for binding in profile.pages.iter().flat_map(|p| p.bindings.values()) {
                assert_ne!(binding.asset.as_deref(), Some(asset.as_str()));
            }
        }
        assert_eq!(
            result.snapshot.profiles[0].pages[0].bindings["key-1"].title,
            "Changed title"
        );
        assert_eq!(
            runtime.drafts.lock().unwrap()["fixture"].value["title"],
            "Unsaved text"
        );
        assert!(runtime.drafts.lock().unwrap()["fixture"]
            .value
            .get("asset")
            .is_none());
        assert!(runtime.drafts.lock().unwrap()["fixture"]
            .base
            .as_ref()
            .unwrap()
            .asset
            .is_none());
        // An already queued draft must not put a deleted asset back
        runtime.save_draft("late", Some(draft)).unwrap();
        assert!(runtime.drafts.lock().unwrap()["late"]
            .value
            .get("asset")
            .is_none());
        let after = runtime.preview_surfaces(Some(id)).unwrap();
        assert_eq!(
            after.configuration_revision,
            result.snapshot.configuration_revision
        );
        assert!(!after.animated);
        assert_ne!(after.surfaces["key-1"], before.surfaces["key-1"]);
        assert_ne!(after.surfaces["touch-1"], before.surfaces["touch-1"]);
        assert_ne!(after.background, before.background);
        let undo = runtime
            .history_action("undo", runtime.snapshot().configuration_revision)
            .unwrap();
        assert_eq!(
            undo.profiles[0].pages[0].bindings["key-1"].title,
            "Keep title"
        );
        assert!(undo.profiles[0].pages[0].bindings["key-1"].asset.is_none());
        let redo = runtime
            .history_action("redo", undo.configuration_revision)
            .unwrap();
        assert_eq!(
            redo.profiles[0].pages[0].bindings["key-1"].title,
            "Changed title"
        );
        assert!(redo.profiles[0].pages[0].bindings["key-1"].asset.is_none());
        let backup = runtime.profiles.storage.backup_recovery().unwrap();
        let retained = runtime
            .profiles
            .storage
            .read_snapshot(&backup.path, crate::workspace::snapshot_media)
            .unwrap();
        // Permanent tombstones are applied by the live owner even when retained
        // history in a recovery snapshot still contains an old assignment
        assert!(runtime
            .profiles
            .deleted_media
            .lock()
            .unwrap()
            .assets
            .contains(&asset));
        assert!(
            retained.contains(&asset),
            "Snapshot history retains the old reference; tombstones prevent its resurrection"
        );
        assert!(!runtime
            .audit_media_library(1, 12, "")
            .unwrap()
            .entries
            .iter()
            .any(|e| e.id == asset));
        runtime.shutdown();
    }
    #[test]
    // Verify that cancellation stale usage and changed content leave media intact
    fn cancellation_stale_usage_and_changed_content_leave_media_intact() {
        let (_root, runtime, asset, _other) = fixture();
        let review = runtime.prepare_media_deletion(&asset).unwrap();
        runtime.discard_media_deletion(&review.id);
        assert!(runtime.commit_media_deletion(&review.id).is_err());
        assert!(runtime.profiles.asset_extension(&asset).is_ok());
        let review = runtime.prepare_media_deletion(&asset).unwrap();
        runtime
            .profiles
            .storage
            .run(|connection| {
                connection
                    .execute_batch("PRAGMA query_only=ON")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        let revision = runtime.snapshot().configuration_revision;
        assert!(runtime.commit_media_deletion(&review.id).is_err());
        assert_eq!(runtime.snapshot().configuration_revision, revision);
        assert!(runtime.profiles.asset_extension(&asset).is_ok());
        runtime
            .profiles
            .storage
            .run(|connection| {
                connection
                    .execute_batch("PRAGMA query_only=OFF")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        let review = runtime.prepare_media_deletion(&asset).unwrap();
        let snapshot = runtime.snapshot();
        let mut binding = snapshot.profiles[0].pages[0].bindings["key-1"].clone();
        binding.asset = Some(asset.clone());
        runtime
            .apply_edits(
                snapshot.configuration_revision,
                "New usage",
                vec![Edit::Bindings {
                    profile_id: snapshot.profiles[0].id.clone(),
                    page_id: snapshot.profiles[0].pages[0].id.clone(),
                    bindings: [("key-1".into(), Some(binding))].into(),
                }],
            )
            .unwrap();
        assert!(runtime
            .commit_media_deletion(&review.id)
            .unwrap_err()
            .contains("usage changed"));
        let review = runtime.prepare_media_deletion(&asset).unwrap();
        std::fs::write(
            runtime
                .profiles
                .root
                .join("assets")
                .join(format!("{asset}.sdmedia")),
            b"changed",
        )
        .unwrap();
        assert!(runtime.commit_media_deletion(&review.id).is_err());
        assert!(!runtime
            .profiles
            .deleted_media
            .lock()
            .unwrap()
            .assets
            .contains(&asset));
        runtime.shutdown();
    }
    #[test]
    // Verify that durable deletion survives interrupted workspace and file removal
    fn durable_deletion_survives_interrupted_workspace_and_file_removal() {
        let root = tempfile::tempdir().unwrap();
        let profiles_root = root.path().join("profiles");
        let store = ProfileStore::open(profiles_root.clone()).unwrap();
        let source = root.path().join("image.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]))
            .save(&source)
            .unwrap();
        let asset = store.import_asset(&source).unwrap();
        let mut data = store.load().unwrap();
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .asset = Some(asset.clone());
        store.save(&data).unwrap();
        let revision = data.configuration_revision + 1;
        // Crash immediately after the commit marker, before clearing profiles/files
        store
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::MediaTombstones,
                "deleted",
                &DeletedMedia {
                    schema_version: 1,
                    revision,
                    assets: [asset.clone()].into(),
                },
                4 * 1024 * 1024,
            )
            .unwrap();
        drop(store);
        let reopened = ProfileStore::open(profiles_root.clone()).unwrap();
        let repaired = reopened.load().unwrap();
        assert_eq!(repaired.configuration_revision, revision);
        assert!(repaired.profiles[0].pages[0].bindings["key-1"]
            .asset
            .is_none());
        assert!(!profiles_root
            .join("assets")
            .join(format!("{asset}.png"))
            .exists());
        assert!(source.is_file(), "Original imports are never removed");
    }
}
