//! Managed recording sources and immutable, portable clip provenance
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

pub const MAX_RECORDING: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_RECORDING_US: u64 = 4 * 60 * 60 * 1_000_000;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    pub id: String,
    pub name: String,
    #[ts(type = "number")]
    pub bytes: u64,
    pub sha256: String,
    #[serde(default = "mp4_format", skip_serializing_if = "is_mp4")]
    #[ts(optional, as = "Option<String>")]
    pub source_format: String,
    #[serde(default)]
    #[ts(type = "number")]
    pub duration_us: u64,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub source_fps: f64,
    #[serde(default)]
    pub variable_rate: bool,
}
// Supply MP4 as the default serialized recording format
fn mp4_format() -> String {
    "mp4".into()
}
// Report whether MP4
fn is_mp4(value: &str) -> bool {
    value == "mp4"
}
impl Recording {
    // Validate the supplied video value
    pub fn validate(&self) -> Result<(), String> {
        crate::profiles::validate_asset_id(&self.id)?;
        if self.source_format == "gif"
            && (self.bytes > crate::media::MAX_SOURCE as u64
                || self.duration_us > 30_000_000
                || self.width > 2048
                || self.height > 2048)
        {
            return Err("GIF exceeds the 16 MiB, 30-second or 2048-pixel limits".into());
        }
        if self.bytes == 0
            || !["mp4", "gif"].contains(&self.source_format.as_str())
            || self.bytes > MAX_RECORDING
            || self.duration_us == 0
            || self.duration_us > MAX_RECORDING_US
            || self.width == 0
            || self.height == 0
            || self.width > 4096
            || self.height > 4096
            || u64::from(self.width) * u64::from(self.height) > 4096 * 2160
            || !self.source_fps.is_finite()
            || self.source_fps <= 0.0
            || self.source_fps > 1_000_000.0
            || self.name.len() > 255
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(
                "Recording exceeds the 8 GiB, four-hour or 4K limits, or has invalid metadata"
                    .into(),
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipInfo {
    pub clip_id: String,
    pub revision: u32,
    pub name: String,
    pub recording: Recording,
    #[ts(type = "number")]
    pub start_us: u64,
    #[ts(type = "number")]
    pub end_us: u64,
    pub options: crate::media::MediaOptions,
}
impl ClipInfo {
    // Validate the supplied video value
    pub fn validate(&self) -> Result<(), String> {
        crate::profiles::validate_asset_id(&self.clip_id)?;
        self.recording.validate()?;
        self.options.validate()?;
        if self.revision == 0
            || self.name.trim().is_empty()
            || self.name.len() > 120
            || self.name.chars().any(char::is_control)
            || self.start_us >= self.end_us
            || self.end_us > self.recording.duration_us
            || self.end_us - self.start_us > 30_000_000
            || self.end_us - self.start_us
                < (1_000_000.0 / self.recording.source_fps).floor() as u64
        {
            return Err("Choose a clip name and a section of at most 30 seconds".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipDraft {
    pub recording_id: String,
    pub name: String,
    #[ts(type = "number")]
    pub start_us: u64,
    #[ts(type = "number")]
    pub end_us: u64,
    pub base_asset: Option<String>,
    pub options: crate::media::MediaOptions,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct VideoJob {
    pub id: String,
    pub owner: String,
    pub generation: u32,
    pub state: String,
    pub phase: String,
    #[ts(type = "number")]
    pub completed: u64,
    #[ts(type = "number | null")]
    pub total: Option<u64>,
    pub recording: Option<Recording>,
    pub error: Option<String>,
}
#[derive(Default)]
pub(crate) struct VideoService {
    jobs: Mutex<BTreeMap<String, (VideoJob, Arc<AtomicBool>)>>,
    pub(crate) writer: Mutex<()>,
    recovered: Mutex<bool>,
    pub(crate) recording_reviews: Mutex<
        BTreeMap<
            String,
            (
                std::time::Instant,
                crate::video_recordings::RecordingCleanupReview,
            ),
        >,
    >,
    pub(crate) recording_leases: crate::asset_leases::AssetLeases,
    recording_owners: Mutex<BTreeMap<String, RecordingOwner>>,
    recording_retired: Mutex<BTreeMap<String, std::time::Instant>>,
    budget: Arc<crate::media_budget::MediaBudget>,
    native_indices: Mutex<std::collections::VecDeque<(String, Arc<crate::media_budget::Slot>)>>,
    gif_sources: Mutex<std::collections::VecDeque<CachedGifSource>>,
    tracked_frames: Mutex<BTreeMap<usize, (crate::media::WeakBytes, usize)>>,
    tracked_indices: Mutex<BTreeMap<usize, TrackedIndex>>,
}
struct RecordingOwner {
    recording: String,
    epoch: Arc<()>,
    _lease: crate::asset_leases::AssetLease,
}
type GifSourceEntry = (
    String,
    u16,
    crate::profiles::SourceStamp,
    Arc<crate::media::Clip>,
);
type CachedGifSource = (
    String,
    u16,
    crate::profiles::SourceStamp,
    Arc<crate::media_budget::Slot>,
);
type TrackedIndex = (std::sync::Weak<Vec<u64>>, usize);
impl VideoService {
    // Report whether current work prevents releasing editor resources
    pub(crate) fn editor_retirement_blocked(&self) -> bool {
        use crate::editor_retirement::unavailable_or;
        unavailable_or(&self.jobs, |jobs| {
            jobs.values().any(|(job, _)| job.state == "running")
        }) || unavailable_or(&self.recording_owners, |owners| !owners.is_empty())
            || unavailable_or(&self.recording_reviews, |reviews| !reviews.is_empty())
            || self.recording_leases.has_live_or_busy()
    }

    // Construct using budget
    pub(crate) fn with_budget(budget: Arc<crate::media_budget::MediaBudget>) -> Self {
        Self {
            budget,
            ..Default::default()
        }
    }
    // Cache GIF
    fn cache_gif(
        &self,
        id: &str,
        epoch: &Option<Arc<()>>,
        entry: GifSourceEntry,
    ) -> Result<(), String> {
        let _writer = self.writer.lock().unwrap();
        self.track_clip(&entry.3);
        if !self.editor_epoch_current(id, epoch) {
            return Ok(());
        }
        let slot = self
            .budget
            .admit(crate::media_budget::Value::Clip(entry.3))?;
        let mut cache = self.gif_sources.lock().unwrap();
        cache.retain(|(key, _, _, _)| key != &entry.0);
        cache.retain(|(_, _, _, slot)| slot.live());
        cache.push_back((entry.0, entry.1, entry.2, slot));
        // Keep the existing two-entry locality limit under the combined budget
        while cache.len() > 2 {
            cache.pop_front();
        }
        Ok(())
    }
    // Cache index
    fn cache_index(
        &self,
        id: &str,
        epoch: &Option<Arc<()>>,
        index: Arc<Vec<u64>>,
    ) -> Result<(), String> {
        let _writer = self.writer.lock().unwrap();
        self.track_index(&index);
        if !self.editor_epoch_current(id, epoch) {
            return Ok(());
        }
        let slot = self
            .budget
            .admit(crate::media_budget::Value::Index(index))?;
        let mut indices = self.native_indices.lock().unwrap();
        indices.retain(|(key, _)| key != id);
        indices.retain(|(_, slot)| slot.live());
        indices.push_back((id.into(), slot));
        while indices.len() > 2 {
            indices.pop_front();
        }
        Ok(())
    }
    // Track clip
    fn track_clip(&self, clip: &crate::media::Clip) {
        let mut frames = self.tracked_frames.lock().unwrap();
        frames.retain(|_, (frame, _)| frame.strong_count() > 0);
        for frame in &clip.frames {
            frames.insert(
                Arc::as_ptr(frame) as usize,
                (Arc::downgrade(frame), frame.len()),
            );
        }
    }
    // Track index
    fn track_index(&self, index: &Arc<Vec<u64>>) {
        let mut indices = self.tracked_indices.lock().unwrap();
        indices.retain(|_, (index, _)| index.strong_count() > 0);
        indices.insert(
            Arc::as_ptr(index) as usize,
            (Arc::downgrade(index), index.capacity() * size_of::<u64>()),
        );
    }
    // Collect memory
    pub(crate) fn collect_memory(&self, memory: &mut crate::media_memory::MediaMemory) {
        for (_, _, _, slot) in self.gif_sources.lock().unwrap().iter() {
            if let Some(clip) = slot.peek_clip() {
                for frame in &clip.frames {
                    memory.cached_frame(frame);
                }
            }
        }
        for (_, slot) in self.native_indices.lock().unwrap().iter() {
            if let Some(index) = slot.peek_index() {
                memory.cached_index(&index);
            }
        }
        let mut frames = self.tracked_frames.lock().unwrap();
        frames.retain(|_, (frame, _)| frame.strong_count() > 0);
        for (frame, bytes) in frames.values() {
            memory.live_frame(frame, *bytes);
        }
        let mut indices = self.tracked_indices.lock().unwrap();
        indices.retain(|_, (index, _)| index.strong_count() > 0);
        for (index, bytes) in indices.values() {
            memory.live_index(index, *bytes);
        }
    }
    // Find the lifetime token of an editor currently owning the recording
    fn editor_epoch(&self, id: &str) -> Option<Arc<()>> {
        self.recording_owners
            .lock()
            .unwrap()
            .values()
            .find(|owner| owner.recording == id)
            .map(|owner| owner.epoch.clone())
    }
    // Call under writer: release/reopen and publication share this boundary
    fn editor_epoch_current(&self, id: &str, epoch: &Option<Arc<()>>) -> bool {
        epoch.as_ref().is_some_and(|epoch| {
            self.editor_epoch(id)
                .is_some_and(|current| Arc::ptr_eq(epoch, &current))
        })
    }
    // Release cached editor resources when no active work requires them
    fn retire_editor_cache(&self, id: &str) {
        if self.editor_epoch(id).is_none() {
            self.native_indices
                .lock()
                .unwrap()
                .retain(|(key, _)| key != id);
            let prefix = format!("{id}:");
            self.gif_sources
                .lock()
                .unwrap()
                .retain(|(key, _, _, _)| !key.starts_with(&prefix));
            self.tracked_frames
                .lock()
                .unwrap()
                .retain(|_, (frame, _)| frame.strong_count() > 0);
            self.tracked_indices
                .lock()
                .unwrap()
                .retain(|_, (index, _)| index.strong_count() > 0);
        }
    }
}
impl crate::Runtime {
    // Create and validate the recording directory and perform one-time recovery before use
    pub(crate) fn recording_directory(&self) -> Result<PathBuf, String> {
        let path = self.profiles.root.join("recordings");
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Invalid recording directory".into());
        }
        let mut recovered = self.video.recovered.lock().unwrap();
        if !*recovered {
            crate::video_recordings::recover(&path)?;
            *recovered = true;
        }
        Ok(path)
    }
    // Read bounded recording metadata and require its embedded identity to match the requested identifier
    pub fn video_recording(&self, id: &str) -> Result<Recording, String> {
        crate::profiles::validate_asset_id(id)?;
        let path = self.recording_directory()?.join(format!("{id}.json"));
        let bytes = crate::media_library::read_regular(&path, 8192)?;
        let recording: Recording = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if recording.id != id {
            return Err("Recording identity mismatch".into());
        }
        Ok(recording)
    }
    // Resolve an original recording only if its format, regular-file status and byte count still match
    pub fn recording_path(&self, id: &str) -> Result<PathBuf, String> {
        let recording = self.video_recording(id)?;
        if !["mp4", "gif"].contains(&recording.source_format.as_str()) {
            return Err("Invalid recording format".into());
        }
        let path = self
            .recording_directory()?
            .join(format!("{id}.{}", recording.source_format));
        let meta = path.symlink_metadata().map_err(|_| {
            "Original recording unavailable; import the original to edit this clip".to_string()
        })?;
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() != recording.bytes {
            return Err("Recording changed; import the original again".into());
        }
        Ok(path)
    }
    // Validate and pin an original recording against cleanup while holding the recording writer lock
    pub(crate) fn pin_recording(
        &self,
        id: &str,
        reason: &str,
    ) -> Result<crate::asset_leases::AssetLease, String> {
        let _writer = self.video.writer.lock().unwrap();
        self.recording_path(id)?;
        self.video.recording_leases.pin([id.to_string()], reason)
    }
    // Retain video recording
    pub fn retain_video_recording(&self, id: &str, owner: String) -> Result<(), String> {
        if owner.is_empty() || owner.len() > 300 {
            return Err("Invalid recording owner".into());
        }
        let _writer = self.video.writer.lock().unwrap();
        let mut retired = self.video.recording_retired.lock().unwrap();
        retired.retain(|_, when| when.elapsed() < std::time::Duration::from_secs(300));
        if retired.contains_key(&owner) {
            return Err("Video editor session ended".into());
        }
        self.recording_path(id)?;
        let lease = self
            .video
            .recording_leases
            .pin([id.to_owned()], "Active video editor")?;
        let mut owners = self.video.recording_owners.lock().unwrap();
        if owners.len() >= 128 && !owners.contains_key(&owner) {
            return Err("Too many active video editors".into());
        }
        let epoch = owners
            .values()
            .find(|owner| owner.recording == id)
            .map(|owner| owner.epoch.clone())
            .unwrap_or_else(|| Arc::new(()));
        let previous = owners.insert(
            owner,
            RecordingOwner {
                recording: id.into(),
                epoch,
                _lease: lease,
            },
        );
        drop(owners);
        if let Some(previous) = previous {
            self.video.retire_editor_cache(&previous.recording);
        }
        Ok(())
    }
    // Release video recording
    pub fn release_video_recording(&self, owner: &str) {
        let _writer = self.video.writer.lock().unwrap();
        let previous = self.video.recording_owners.lock().unwrap().remove(owner);
        if let Some(previous) = previous {
            self.video.retire_editor_cache(&previous.recording);
        }
        self.profiles.media_budget.trim_idle();
        let mut retired = self.video.recording_retired.lock().unwrap();
        retired.retain(|_, when| when.elapsed() < std::time::Duration::from_secs(300));
        if retired.len() < 4096 {
            retired.insert(owner.into(), std::time::Instant::now());
        }
    }
    // Read recording range
    pub fn read_recording_range(
        &self,
        id: &str,
        start: u64,
        length: u32,
    ) -> Result<Vec<u8>, String> {
        let path = self.recording_path(id)?;
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let before = file.metadata().map_err(|e| e.to_string())?;
        if length == 0
            || length > 1024 * 1024
            || start
                .checked_add(u64::from(length))
                .is_none_or(|end| end > before.len())
        {
            return Err("Recording reads must be within the file and at most 1 MiB".into());
        }
        file.seek(SeekFrom::Start(start))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; length as usize];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        let after = file.metadata().map_err(|e| e.to_string())?;
        if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
            return Err("Recording changed during read".into());
        }
        Ok(bytes)
    }
    // Save recording metadata
    pub fn save_recording_metadata(&self, recording: Recording) -> Result<Recording, String> {
        recording.validate()?;
        let _writer = self.video.writer.lock().unwrap();
        let previous = self.video_recording(&recording.id)?;
        if previous.bytes != recording.bytes
            || previous.sha256 != recording.sha256
            || previous.name != recording.name
            || previous.source_format != recording.source_format
        {
            return Err("Recording identity changed".into());
        }
        self.recording_path(&recording.id)?;
        self.write_recording(&recording)?;
        Ok(recording)
    }
    // Write recording
    fn write_recording(&self, recording: &Recording) -> Result<(), String> {
        let directory = self.recording_directory()?;
        let mut staged = tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
        staged
            .write_all(&serde_json::to_vec(recording).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        staged.as_file().sync_all().map_err(|e| e.to_string())?;
        staged
            .persist(directory.join(format!("{}.json", recording.id)))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    // Prune video jobs
    fn prune_video_jobs(&self) {
        let mut jobs = self.video.jobs.lock().unwrap();
        while jobs.len() >= 32 {
            let retired = jobs
                .iter()
                .find(|(_, (job, _))| job.state != "running")
                .map(|(id, _)| id.clone());
            if let Some(id) = retired {
                jobs.remove(&id);
            } else {
                break;
            }
        }
    }
    // Start video import
    pub fn start_video_import(
        self: &Arc<Self>,
        path: PathBuf,
        owner: String,
        generation: u32,
    ) -> Result<String, String> {
        if owner.is_empty() || owner.len() > 250 {
            return Err("Invalid editor owner".into());
        }
        self.prune_video_jobs();
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self.video.jobs.lock().unwrap();
            if jobs.values().filter(|(j, _)| j.state == "running").count() >= 2 {
                return Err("Two recording imports are already active".into());
            }
            jobs.insert(
                id.clone(),
                (
                    VideoJob {
                        id: id.clone(),
                        owner,
                        generation,
                        state: "running".into(),
                        phase: "Importing recording".into(),
                        completed: 0,
                        total: None,
                        recording: None,
                        error: None,
                    },
                    cancel.clone(),
                ),
            );
        }
        let runtime = self.clone();
        let output = id.clone();
        std::thread::spawn(move || {
            let result = runtime.copy_recording(&path, &id, &cancel);
            if let Some((job, _)) = runtime.video.jobs.lock().unwrap().get_mut(&id) {
                if cancel.load(Ordering::SeqCst) {
                    job.state = "cancelled".into();
                } else {
                    match result {
                        Ok(recording) => {
                            job.recording = Some(recording);
                            job.state = "complete".into();
                        }
                        Err(error) => {
                            job.error = Some(error);
                            job.state = "failed".into();
                        }
                    }
                }
            }
        });
        Ok(output)
    }
    // Copy recording
    fn copy_recording(
        &self,
        path: &Path,
        job: &str,
        cancel: &AtomicBool,
    ) -> Result<Recording, String> {
        let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
        if !meta.is_file()
            || meta.file_type().is_symlink()
            || meta.len() == 0
            || meta.len() > MAX_RECORDING
        {
            return Err("Choose an MP4 recording up to 8 GiB or a GIF up to 16 MiB".into());
        }
        let mut input = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let directory = self.recording_directory()?;
        if std::fs::read_dir(&directory)
            .map_err(|e| e.to_string())?
            .take(16385)
            .count()
            > 16384
        {
            return Err("Recording library exceeds 8,192 recordings".into());
        }
        let mut staged = tempfile::NamedTempFile::new_in(&directory).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        let mut copied = 0;
        let mut source_format = String::new();
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("Recording import cancelled".into());
            }
            let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if copied == 0 {
                source_format = crate::media::detect(&buffer[..count])?.into();
                if !["mp4", "gif"].contains(&source_format.as_str())
                    || source_format == "gif" && meta.len() > crate::media::MAX_SOURCE as u64
                {
                    return Err("Choose an MP4 recording or a GIF up to 16 MiB".into());
                }
            }
            copied += count as u64;
            if copied > meta.len() || copied > MAX_RECORDING {
                return Err("Recording changed during import".into());
            }
            hash.update(&buffer[..count]);
            staged
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
            if let Some((state, _)) = self.video.jobs.lock().unwrap().get_mut(job) {
                state.completed = copied;
                state.total = Some(meta.len());
            }
        }
        if copied != meta.len()
            || input.metadata().map_err(|e| e.to_string())?.modified().ok() != meta.modified().ok()
        {
            return Err("Recording changed during import".into());
        }
        let digest = hex::encode(hash.finalize());
        // Source copies are deduplicated independently of clip assets
        for entry in std::fs::read_dir(&directory)
            .map_err(|e| e.to_string())?
            .take(16384)
        {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().extension().is_some_and(|ext| ext == "json") {
                if let Ok(bytes) = crate::media_library::read_regular(&entry.path(), 8192) {
                    if let Ok(recording) = serde_json::from_slice::<Recording>(&bytes) {
                        if recording.sha256 == digest && self.recording_path(&recording.id).is_ok()
                        {
                            return Ok(recording);
                        }
                    }
                }
            }
        }
        let recording = Recording {
            id: uuid::Uuid::new_v4().to_string(),
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .chars()
                .take(255)
                .collect(),
            bytes: copied,
            sha256: digest,
            source_format,
            duration_us: 0,
            width: 0,
            height: 0,
            source_fps: 0.0,
            variable_rate: false,
        };
        if cancel.load(Ordering::SeqCst) {
            return Err("Recording import cancelled".into());
        }
        staged.as_file().sync_all().map_err(|e| e.to_string())?;
        let published = directory.join(format!("{}.{}", recording.id, recording.source_format));
        staged.persist(&published).map_err(|e| e.to_string())?;
        if let Err(error) = self.write_recording(&recording) {
            let _ = std::fs::remove_file(published);
            return Err(error);
        }
        Ok(recording)
    }
    // Return a snapshot of the requested video import job or reject an unknown identifier
    pub fn video_import_status(&self, id: &str) -> Result<VideoJob, String> {
        self.video
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .map(|(job, _)| job.clone())
            .ok_or("Unknown recording import".into())
    }
    // Cancel video import
    pub fn cancel_video_import(&self, id: &str) -> Result<(), String> {
        if let Some((_, cancel)) = self.video.jobs.lock().unwrap().get(id) {
            cancel.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
    // Read optional source-recording metadata from a prepared media clip
    pub fn clip_info(&self, asset: &str) -> Result<Option<ClipInfo>, String> {
        Ok(self
            .profiles
            .clip(asset)?
            .and_then(|clip| clip.clip_info.clone()))
    }
    // Find a usable local original by identity or matching content hash and byte count
    fn local_recording(&self, original: &Recording) -> Result<Recording, String> {
        if self.recording_path(&original.id).is_ok() {
            return self.video_recording(&original.id);
        }
        for (index, entry) in std::fs::read_dir(self.recording_directory()?)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if index >= 16384 {
                return Err("Recording library exceeds bounds".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let bytes = crate::media_library::read_regular(&path, 8192)?;
            let source: Recording = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            if source.sha256 == original.sha256
                && source.bytes == original.bytes
                && self.recording_path(&source.id).is_ok()
            {
                return Ok(source);
            }
        }
        Err("Original recording unavailable. Import the original recording to expand or rebuild this clip; prepared playback remains available.".into())
    }
    // Create an owner-scoped job that resolves or recovers the original recording for a prepared asset
    pub fn recording_for_asset(
        self: &Arc<Self>,
        asset: &str,
        owner: String,
        generation: u32,
    ) -> Result<String, String> {
        crate::profiles::validate_asset_id(asset)?;
        self.prune_video_jobs();
        if let Some(info) = self.clip_info(asset)? {
            if let Ok(recording) = self.local_recording(&info.recording) {
                let id = uuid::Uuid::new_v4().to_string();
                self.video.jobs.lock().unwrap().insert(
                    id.clone(),
                    (
                        VideoJob {
                            id: id.clone(),
                            owner,
                            generation,
                            state: "complete".into(),
                            phase: "Recording ready".into(),
                            completed: recording.bytes,
                            total: Some(recording.bytes),
                            recording: Some(recording),
                            error: None,
                        },
                        Arc::new(AtomicBool::new(false)),
                    ),
                );
                return Ok(id);
            }
            return Err("Original recording unavailable. Import the original recording to expand or rebuild this clip; prepared playback remains available.".into());
        }
        let path = self
            .profiles
            .root
            .join("assets")
            .join(format!("{asset}.sdmedia"));
        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let clip = self.profiles.clip(asset)?.ok_or("Clip unavailable")?;
        if clip.preparation_revision != 2 {
            return Err("Unsupported clip; reimport from the original recording".into());
        }
        let format = clip.source_format.clone();
        let mut source = archive
            .by_name(&format!("original.{format}"))
            .map_err(|_| "Original animation unavailable; reimport from the original media")?;
        if source.size() > crate::media::MAX_SOURCE as u64 {
            return Err("Original animation exceeds limits".into());
        }
        let mut staged = tempfile::Builder::new()
            .suffix(&format!(".{format}"))
            .tempfile()
            .map_err(|e| e.to_string())?;
        std::io::copy(&mut source, &mut staged).map_err(|e| e.to_string())?;
        staged.flush().map_err(|e| e.to_string())?;
        // Current native standalone animations retain their authored source
        // Copy before this temporary source retires; no obsolete archive is read
        let id = uuid::Uuid::new_v4().to_string();
        self.video.jobs.lock().unwrap().insert(
            id.clone(),
            (
                VideoJob {
                    id: id.clone(),
                    owner,
                    generation,
                    state: "running".into(),
                    phase: "Importing original".into(),
                    completed: 0,
                    total: None,
                    recording: None,
                    error: None,
                },
                Arc::new(AtomicBool::new(false)),
            ),
        );
        let result = self.copy_recording(staged.path(), &id, &AtomicBool::new(false));
        let mut jobs = self.video.jobs.lock().unwrap();
        let (job, _) = jobs.get_mut(&id).unwrap();
        match result {
            Ok(recording) => {
                job.recording = Some(recording);
                job.state = "complete".into();
            }
            Err(error) => {
                job.error = Some(error);
                job.state = "failed".into();
            }
        }
        Ok(id)
    }
    // Prepare recording clip
    pub fn prepare_recording_clip(
        &self,
        draft: ClipDraft,
        owner: String,
        generation: u64,
    ) -> Result<String, String> {
        let recording = self.video_recording(&draft.recording_id)?;
        recording.validate()?;
        self.recording_path(&recording.id)?;
        let previous = draft
            .base_asset
            .as_deref()
            .map(|asset| self.clip_info(asset))
            .transpose()?
            .flatten();
        if previous
            .as_ref()
            .is_some_and(|info| info.recording.sha256 != recording.sha256)
        {
            return Err("A clip revision must use its original recording; create a new clip for another recording".into());
        }
        let info = ClipInfo {
            clip_id: previous
                .as_ref()
                .map(|info| info.clip_id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            revision: previous
                .as_ref()
                .map_or(1, |info| info.revision.saturating_add(1)),
            name: draft.name.trim().into(),
            recording,
            start_us: draft.start_us,
            end_us: draft.end_us,
            options: draft.options,
        };
        info.validate()?;
        self.prepare_browser_clip(info, owner, generation)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct VideoTarget {
    pub name: String,
    pub width: u16,
    pub height: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct VideoClipEntry {
    pub asset: String,
    pub info: ClipInfo,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct VideoClipDetails {
    pub info: Option<ClipInfo>,
    pub frame_times_us: Vec<u32>,
    pub duration_us: u32,
    pub source_available: bool,
    pub prepared_width: u32,
    pub prepared_height: u32,
    pub prepared_fps: f64,
}
impl crate::Runtime {
    // Resolve target dimensions from a control's feedback surface or the device background display
    pub fn video_target(&self, device: &str, control: Option<&str>) -> Result<VideoTarget, String> {
        let state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|entry| entry.id == device)
            .ok_or("Device unavailable")?;
        if let Some(id) = control {
            let selected = device.manifest.control(id).map_err(|e| e.to_string())?;
            let surface = if let Some(feedback) = selected.feedback_surface.as_deref() {
                device
                    .manifest
                    .control(feedback)
                    .map_err(|e| e.to_string())?
            } else {
                selected
            };
            Ok(VideoTarget {
                name: selected.label.clone(),
                width: surface.image_width.ok_or("Control has no display")?,
                height: surface.image_height.ok_or("Control has no display")?,
            })
        } else {
            let [width, height] = device
                .manifest
                .capabilities
                .background_size
                .or_else(|| {
                    device
                        .manifest
                        .controls
                        .iter()
                        .find_map(|control| Some([control.image_width?, control.image_height?]))
                })
                .ok_or("Device has no display surface")?;
            Ok(VideoTarget {
                name: format!("{} background", device.name),
                width,
                height,
            })
        }
    }
    // Describe prepared clip dimensions, frame timing and availability of its original source
    pub fn video_clip_details(&self, asset: &str) -> Result<VideoClipDetails, String> {
        let clip = self.profiles.clip(asset)?.ok_or("Choose a video clip")?;
        let (prepared_width, prepared_height) =
            image::ImageReader::new(std::io::Cursor::new(&**clip.frames[0]))
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?;
        Ok(VideoClipDetails {
            prepared_width,
            prepared_height,
            prepared_fps: clip.prepared_fps(),
            info: clip.clip_info.clone(),
            frame_times_us: clip.boundaries_us(),
            duration_us: clip.duration_us(),
            source_available: clip
                .clip_info
                .as_ref()
                .is_some_and(|info| self.local_recording(&info.recording).is_ok())
                || clip.clip_info.is_none(),
        })
    }
    // Return the prepared frame at a valid source-relative playhead position
    pub fn prepared_video_frame(&self, asset: &str, position: u64) -> Result<Vec<u8>, String> {
        let clip = self
            .profiles
            .clip(asset)?
            .ok_or("Prepared clip unavailable")?;
        if position >= u64::from(clip.duration_us()) {
            return Err("Playhead outside prepared clip".into());
        }
        Ok(clip.frames[clip.frame_index(std::time::Duration::from_micros(position))].to_vec())
    }
    // Render a prepared clip for the target without applying the editor's speed and trim clock twice
    pub fn prepared_video_preview(
        &self,
        asset: &str,
        position: u64,
        device: &str,
        control: Option<&str>,
        options: &crate::media::MediaOptions,
    ) -> Result<Vec<u8>, String> {
        let clip = self
            .profiles
            .clip(asset)?
            .ok_or("Prepared clip unavailable")?;
        if position >= u64::from(clip.duration_us()) {
            return Err("Playhead outside prepared clip".into());
        }
        options.validate()?;
        let target = self.video_target(device, control)?;
        // The editor supplies source-relative positions, including its own speed
        // and trim clock. Reuse the output cache without applying them twice
        let mut options = options.clone();
        options.speed = 1.0;
        options.trim_start_ms = 0;
        options.trim_end_ms = None;
        self.render_media_frame(
            asset,
            std::time::Duration::from_micros(position),
            [target.width, target.height],
            0,
            &options,
            &["jpeg".into(), "png".into()],
        )
    }
    // Scan the bounded managed media library for clips optionally matching an original recording hash
    pub fn video_clips(&self, recording: Option<&str>) -> Result<Vec<VideoClipEntry>, String> {
        let source_hash = recording
            .map(|id| self.video_recording(id).map(|source| source.sha256))
            .transpose()?;
        let mut result = vec![];
        for (index, entry) in std::fs::read_dir(self.profiles.root.join("assets"))
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if index >= 8192 {
                return Err("Media library exceeds the 8,192 entry scan limit".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|ext| ext != "sdmedia") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            crate::profiles::validate_asset_id(id)?;
            let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err("Invalid managed clip file".into());
            }
            let info = crate::media::clip_info_archive(
                std::fs::File::open(&path).map_err(|e| e.to_string())?,
            )?;
            if let Some(info) = info {
                if recording.is_none_or(|id| id == info.recording.id)
                    || source_hash.as_ref() == Some(&info.recording.sha256)
                {
                    result.push(VideoClipEntry {
                        asset: id.into(),
                        info,
                    });
                }
            }
        }
        result.sort_by(|a, b| {
            a.info
                .name
                .to_lowercase()
                .cmp(&b.info.name.to_lowercase())
                .then(b.info.revision.cmp(&a.info.revision))
        });
        Ok(result)
    }
}

/// Helpers have finite pipes and deadlines. They never execute user shell text
fn helper_output(command: std::process::Command, limit: usize) -> Result<Vec<u8>, String> {
    helper_read(command, move |stdout| {
        let mut bytes = vec![];
        stdout
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > limit {
            return Err("Decoder output exceeds bounds".into());
        }
        Ok(bytes)
    })
}
// Read decoder output concurrently with bounded error capture and terminate a timed-out helper
fn helper_read<T: Send + 'static>(
    mut command: std::process::Command,
    read: impl FnOnce(std::process::ChildStdout) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("Missing decoder output")?;
    let stderr = child.stderr.take().ok_or("Missing decoder errors")?;
    let reader = std::thread::spawn(move || read(stdout));
    let errors = std::thread::spawn(move || {
        let mut bytes = vec![];
        let _ = stderr.take(8192).read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    let successful = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                break false;
            }
        }
    };
    let _ = child.wait();
    let bytes = reader.join().map_err(|_| "Decoder output stopped")??;
    let errors = errors.join().unwrap_or_default();
    if !successful {
        return Err(format!(
            "FFmpeg operation failed or timed out: {}",
            String::from_utf8_lossy(&errors)
        ));
    }
    Ok(bytes)
}

#[derive(Debug)]
struct RecordingIndex {
    times: Vec<u64>,
    minimum_delta: Option<u64>,
    maximum_delta: u64,
}
// An order-preserving encoding lets the original signed ticks become the final
// unsigned microseconds in the same allocation, including negative start_pts
fn ordered_tick(tick: i64) -> u64 {
    tick as u64 ^ (1 << 63)
}
// Capture bounded original-recording metadata for change detection
fn recording_source_stamp(path: &Path) -> Result<crate::profiles::SourceStamp, String> {
    crate::profiles::SourceStamp::read_bounded(path, MAX_RECORDING)
}
// Finalize recording index
fn finish_recording_index(
    mut ticks: Vec<u64>,
    origin: i64,
    time_base: f64,
    duration_us: u64,
) -> Result<RecordingIndex, String> {
    ticks.sort_unstable();
    ticks.dedup();
    if ticks.is_empty() {
        return Err("Recording has no presentation timestamps".into());
    }
    let (minimum_delta, maximum_delta) =
        ticks
            .windows(2)
            .fold((None, 0), |(minimum, maximum), pair| {
                let delta = (pair[1] - pair[0]).min(i64::MAX as u64);
                (
                    Some(minimum.map_or(delta, |value: u64| value.min(delta))),
                    maximum.max(delta),
                )
            });
    let origin = ordered_tick(origin);
    ticks.retain_mut(|tick| {
        *tick = ((tick.saturating_sub(origin).min(i64::MAX as u64)) as f64 * time_base * 1_000_000.)
            .round() as u64;
        *tick < duration_us
    });
    if ticks.is_empty() {
        return Err("Recording has no timestamps within its duration".into());
    }
    Ok(RecordingIndex {
        times: ticks,
        minimum_delta,
        maximum_delta,
    })
}
// Read recording index
fn read_recording_index(
    source: impl Read,
    origin: i64,
    time_base: f64,
    duration_us: u64,
) -> Result<RecordingIndex, String> {
    use std::io::BufRead;
    let mut source = std::io::BufReader::new(source);
    let mut line = Vec::with_capacity(256);
    let mut total = 0;
    let mut ticks = Vec::new();
    loop {
        line.clear();
        let count = (&mut source)
            .take(257)
            .read_until(b'\n', &mut line)
            .map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total += count;
        if count > 256 || total > 64 * 1024 * 1024 {
            return Err("Decoder output exceeds bounds".into());
        }
        let text = std::str::from_utf8(&line).map_err(|e| e.to_string())?;
        if let Some(tick) = text
            .trim_end_matches(['\r', '\n'])
            .split(',')
            .next()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|tick| *tick >= origin)
        {
            if ticks.len() >= 4_000_000 {
                return Err("Recording exceeds the four-million-frame indexing limit".into());
            }
            ticks.push(ordered_tick(tick));
        }
    }
    finish_recording_index(ticks, origin, time_base, duration_us)
}
impl crate::Runtime {
    // Decode a validated GIF recording with cancellation and owner-scoped source-cache reuse
    pub(crate) fn recording_gif(
        &self,
        id: &str,
        edge: u16,
        control: &crate::media::PreparationControl,
    ) -> Result<Arc<crate::media::Clip>, String> {
        control.check()?;
        let epoch = self.video.editor_epoch(id);
        if !(64..=1024).contains(&edge) {
            return Err("GIF detail must be between 64 and 1024 pixels".into());
        }
        let recording = self.video_recording(id)?;
        if recording.source_format != "gif" {
            return Err("Choose a GIF source".into());
        }
        let path = self.recording_path(id)?;
        let stamp = crate::profiles::SourceStamp::read(&path)?;
        let cache_key = format!("{id}:{}", recording.sha256);
        #[cfg(unix)]
        if epoch.is_some() {
            let mut cache = self.video.gif_sources.lock().unwrap();
            cache.retain(|(_, _, _, slot)| slot.live());
            if let Some(index) = cache.iter().position(|(key, saved_edge, saved, _)| {
                key == &cache_key && *saved_edge >= edge && saved == &stamp
            }) {
                let entry = cache.remove(index).unwrap();
                let clip = entry.3.clip();
                cache.push_back(entry);
                if let Some(clip) = clip {
                    return Ok(clip);
                }
            }
        }
        let _decode = self.profiles.media_gate.lock_cancel(&control.cancelled)?;
        let reservation = self
            .profiles
            .media_budget
            .reserve(crate::media_budget::PREPARATION_BYTES)?;
        control.check()?;
        let source = crate::media_library::read_regular(&path, crate::media::MAX_SOURCE)?;
        if hex::encode(Sha256::digest(&source)) != recording.sha256 {
            return Err("GIF source changed; import it again".into());
        }
        #[cfg(not(unix))]
        if let Some((_, _, _, clip)) =
            self.video
                .gif_sources
                .lock()
                .unwrap()
                .iter()
                .find(|(key, saved_edge, saved, _)| {
                    epoch.is_some() && key == &cache_key && *saved_edge >= edge && saved == &stamp
                })
        {
            if let Some(clip) = clip.clip() {
                return Ok(clip);
            }
        }
        let clip = Arc::new(crate::media::gif_with_detail(&source, edge, control)?);
        if crate::profiles::SourceStamp::read(&path)? != stamp {
            return Err("GIF changed during decoding".into());
        }
        reservation.track_clip(&clip, source.capacity())?;
        // Recording cleanup takes writer -> media_gate. Release decoding only
        // after the clip is complete, before checking the editor epoch
        drop(_decode);
        self.video
            .cache_gif(id, &epoch, (cache_key, edge, stamp, clip.clone()))?;
        Ok(clip)
    }
    // Probe recording using the current video state
    pub fn probe_recording(&self, id: &str) -> Result<Recording, String> {
        let epoch = self.video.editor_epoch(id);
        let mut recording = self.video_recording(id)?;
        if recording.source_format == "gif" {
            let clip = self.recording_gif(id, 512, &Default::default())?;
            let path = self.recording_path(id)?;
            let (width, height) = image::ImageReader::open(path)
                .map_err(|e| e.to_string())?
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?;
            recording.width = width;
            recording.height = height;
            recording.duration_us = clip.duration_us().into();
            recording.source_fps = clip.peak_fps();
            recording.variable_rate = clip.delays_us.windows(2).any(|pair| pair[0] != pair[1]);
            return self.save_recording_metadata(recording);
        }
        let path = self.recording_path(id)?;
        let source_stamp = recording_source_stamp(&path)?;
        let ffmpeg = crate::media::ffmpeg_path()?;
        let ffprobe = ffmpeg.with_file_name(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        if !ffprobe.is_file() {
            return Err("Install FFmpeg with ffprobe to use the native recording decoder".into());
        }
        let mut command = crate::native_process::command(ffprobe);
        command
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file,pipe",
                "-select_streams",
                "v:0",
                "-show_entries",
                "format=duration:stream=width,height,duration,time_base,start_pts,avg_frame_rate,r_frame_rate:stream_side_data=rotation",
                "-of",
                "json",
            ])
            .arg(&path);
        let output: serde_json::Value =
            serde_json::from_slice(&helper_output(command, 65536)?).map_err(|e| e.to_string())?;
        let stream = &output["streams"][0];
        let rate = |key: &str| -> Option<f64> {
            let (n, d) = stream[key].as_str()?.split_once('/')?;
            let value = n.parse::<f64>().ok()? / d.parse::<f64>().ok()?;
            (value.is_finite() && value > 0.0).then_some(value)
        };
        let mut recording = self.video_recording(id)?;
        recording.duration_us = (stream["duration"]
            .as_str()
            .or_else(|| output["format"]["duration"].as_str())
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or("Recording duration unavailable")?
            * 1_000_000.0)
            .round() as u64;
        recording.width = stream["width"].as_u64().unwrap_or(0) as u32;
        recording.height = stream["height"].as_u64().unwrap_or(0) as u32;
        if stream["side_data_list"].as_array().is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry["rotation"]
                    .as_i64()
                    .is_some_and(|rotation| rotation.rem_euclid(180) == 90)
            })
        }) {
            std::mem::swap(&mut recording.width, &mut recording.height);
        }
        recording.source_fps = rate("r_frame_rate")
            .or_else(|| rate("avg_frame_rate"))
            .ok_or("Recording FPS unavailable")?;
        recording.variable_rate =
            rate("avg_frame_rate").is_some_and(|avg| (avg - recording.source_fps).abs() > 0.01);
        recording.validate()?;
        let (numerator, denominator) = stream["time_base"]
            .as_str()
            .and_then(|value| value.split_once('/'))
            .ok_or("Recording time base unavailable")?;
        let time_base = numerator.parse::<f64>().map_err(|e| e.to_string())?
            / denominator.parse::<f64>().map_err(|e| e.to_string())?;
        if !time_base.is_finite() || time_base <= 0. {
            return Err("Invalid recording time base".into());
        }
        let origin = stream["start_pts"].as_i64().unwrap_or(0);
        let mut command = crate::native_process::command(
            crate::media::ffmpeg_path()?.with_file_name(if cfg!(windows) {
                "ffprobe.exe"
            } else {
                "ffprobe"
            }),
        );
        command
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file,pipe",
                "-select_streams",
                "v:0",
                "-show_entries",
                "packet=pts",
                "-of",
                "csv=p=0",
            ])
            .arg(&path);
        let duration_us = recording.duration_us;
        let reservation = self.profiles.media_budget.reserve(64 * 1024 * 1024)?;
        let index = helper_read(command, move |stdout| {
            read_recording_index(stdout, origin, time_base, duration_us)
        })?;
        if let Some(minimum) = index.minimum_delta {
            recording.source_fps = 1. / (minimum as f64 * time_base);
            recording.variable_rate = index.maximum_delta - minimum > 1;
        }
        if recording_source_stamp(&path)? != source_stamp {
            return Err("Recording changed during indexing; import the original again".into());
        }
        let times = Arc::new(index.times);
        reservation.track_index(&times)?;
        self.video.cache_index(id, &epoch, times)?;
        self.save_recording_metadata(recording)
    }
    // Snap or step a recording playhead to a valid frame boundary
    pub fn recording_position(
        &self,
        id: &str,
        position: u64,
        direction: i8,
    ) -> Result<u64, String> {
        let recording = self.video_recording(id)?;
        if position > recording.duration_us || !(-1..=1).contains(&direction) {
            return Err("Invalid video frame position".into());
        }
        if direction == 0 && position == recording.duration_us {
            return Ok(position);
        }
        if recording.source_format == "gif" {
            let clip = self.recording_gif(id, 512, &Default::default())?;
            let bounds = clip.boundaries_us();
            let limit = clip.frames.len() - 1;
            let selected = match direction {
                -1 => bounds
                    .partition_point(|time| u64::from(*time) < position)
                    .saturating_sub(1),
                1 => bounds
                    .partition_point(|time| u64::from(*time) <= position)
                    .min(limit),
                _ => bounds
                    .partition_point(|time| u64::from(*time) <= position)
                    .saturating_sub(1)
                    .min(limit),
            };
            return Ok(bounds[selected].into());
        }
        if let Some(times) = self
            .video
            .native_indices
            .lock()
            .unwrap()
            .iter()
            .find(|(key, _)| key == id)
            .and_then(|(_, slot)| slot.index())
        {
            let index = times.partition_point(|time| *time < position);
            let selected = if direction < 0 {
                index.saturating_sub(1)
            } else if direction > 0 {
                times
                    .partition_point(|time| *time <= position)
                    .min(times.len().saturating_sub(1))
            } else {
                times
                    .partition_point(|time| *time <= position)
                    .saturating_sub(1)
            };
            return times
                .get(selected)
                .copied()
                .ok_or("Frame index unavailable".into());
        }
        let ffprobe = crate::media::ffmpeg_path()?.with_file_name(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        let mut command = crate::native_process::command(ffprobe);
        let begin = position.saturating_sub(2_000_000) as f64 / 1_000_000.0;
        let end = (position + 2_000_000).min(recording.duration_us) as f64 / 1_000_000.0;
        command
            .args([
                "-v",
                "error",
                "-protocol_whitelist",
                "file,pipe",
                "-select_streams",
                "v:0",
                "-read_intervals",
                &format!("{begin}%{end}"),
                "-show_entries",
                "packet=pts_time:stream=start_time",
                "-of",
                "json",
            ])
            .arg(self.recording_path(id)?);
        let value: serde_json::Value =
            serde_json::from_slice(&helper_output(command, 4 * 1024 * 1024)?)
                .map_err(|e| e.to_string())?;
        let origin = value["streams"][0]["start_time"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let mut times: Vec<u64> = value["packets"]
            .as_array()
            .ok_or("No frame timestamps available")?
            .iter()
            .filter_map(|packet| packet["pts_time"].as_str()?.parse::<f64>().ok())
            .filter(|t| t.is_finite() && *t >= origin)
            .map(|t| ((t - origin) * 1_000_000.0).round() as u64)
            .filter(|t| *t < recording.duration_us)
            .collect();
        times.sort_unstable();
        times.dedup();
        let selected = match direction {
            -1 => times.iter().rev().find(|time| **time < position),
            1 => times.iter().find(|time| **time > position),
            _ => times.iter().rev().find(|time| **time <= position),
        };
        selected
            .copied()
            .or_else(|| times.last().copied().filter(|_| direction > 0))
            .or_else(|| (position == 0).then_some(0))
            .ok_or(
                "Frame timestamps unavailable near this position; choose another position".into(),
            )
    }
    // Decode a recording frame at a validated playhead and preview resolution
    pub fn recording_frame(&self, id: &str, position: u64, edge: u16) -> Result<Vec<u8>, String> {
        let recording = self.video_recording(id)?;
        if position >= recording.duration_us || !(64..=1024).contains(&edge) {
            return Err("Playhead or preview detail outside recording limits".into());
        }
        if recording.source_format == "gif" {
            let clip = self.recording_gif(id, edge, &Default::default())?;
            let index = clip.frame_index(std::time::Duration::from_micros(position));
            let (width, height) =
                image::ImageReader::new(std::io::Cursor::new(&**clip.frames[index]))
                    .with_guessed_format()
                    .map_err(|e| e.to_string())?
                    .into_dimensions()
                    .map_err(|e| e.to_string())?;
            if width.max(height) <= u32::from(edge) {
                return Ok(clip.frames[index].to_vec());
            }
            let scale = f64::from(edge) / f64::from(width.max(height));
            let size = [
                (f64::from(width) * scale).round().max(1.) as u16,
                (f64::from(height) * scale).round().max(1.) as u16,
            ];
            return Ok(self
                .render_animation_frame_shared(
                    &clip,
                    index,
                    size,
                    0,
                    &crate::media::MediaOptions::default(),
                    &["png".into()],
                    None,
                )?
                .to_vec());
        }
        let path = self.recording_path(id)?;
        let mut command = crate::native_process::command(crate::media::ffmpeg_path()?);
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-max_alloc",
                "67108864",
                "-max_pixels",
                "8847360",
                "-threads",
                "1",
                "-protocol_whitelist",
                "file,pipe",
                "-ss",
                &(position as f64 / 1_000_000.0).to_string(),
                "-i",
            ])
            .arg(path)
            .args([
                "-an",
                "-sn",
                "-dn",
                "-frames:v",
                "1",
                "-threads",
                "1",
                "-vf",
                &format!("scale={edge}:{edge}:force_original_aspect_ratio=decrease"),
                "-c:v",
                "mjpeg",
                "-q:v",
                "3",
                "-f",
                "image2pipe",
                "pipe:1",
            ]);
        helper_output(command, 2 * 1024 * 1024)
    }
    // Render a GIF recording frame for the selected device surface without reapplying trim offsets
    pub fn gif_recording_preview(
        &self,
        id: &str,
        position: u64,
        edge: u16,
        device: &str,
        control: Option<&str>,
        options: &crate::media::MediaOptions,
    ) -> Result<Vec<u8>, String> {
        let clip = self.recording_gif(id, edge, &Default::default())?;
        if position >= u64::from(clip.duration_us()) {
            return Err("Playhead outside GIF".into());
        }
        let target = self.video_target(device, control)?;
        let index = clip.frame_index(std::time::Duration::from_micros(position));
        let mut options = options.clone();
        options.trim_start_ms = 0;
        options.trim_end_ms = None;
        Ok(self
            .render_animation_frame_shared(
                &clip,
                index,
                [target.width, target.height],
                0,
                &options,
                &["jpeg".into(), "png".into()],
                None,
            )?
            .to_vec())
    }
    // Prepare recording clip native
    pub fn prepare_recording_clip_native(
        self: &Arc<Self>,
        draft: ClipDraft,
        owner: String,
        generation: u64,
    ) -> Result<String, String> {
        let id = self.prepare_recording_clip(draft, owner, generation)?;
        self.start_native_video_clip(&id)?;
        Ok(id)
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    #[test]
    // Verify that recording stamp supports large sources and retains file identity bounds
    fn recording_stamp_supports_large_sources_and_retains_file_identity_bounds() {
        let source = tempfile::NamedTempFile::new().unwrap();
        let length = crate::media::MAX_ASSET as u64 + 1;
        source.as_file().set_len(length).unwrap();
        assert!(crate::profiles::SourceStamp::read(source.path()).is_err());
        let before = recording_source_stamp(source.path()).unwrap();
        source.as_file().set_len(length + 1).unwrap();
        assert_ne!(before, recording_source_stamp(source.path()).unwrap());
        source.as_file().set_len(MAX_RECORDING + 1).unwrap();
        assert!(recording_source_stamp(source.path()).is_err());
        #[cfg(unix)]
        {
            source.as_file().set_len(length).unwrap();
            let before = recording_source_stamp(source.path()).unwrap();
            std::fs::remove_file(source.path()).unwrap();
            std::fs::File::create(source.path())
                .unwrap()
                .set_len(length)
                .unwrap();
            assert_ne!(before, recording_source_stamp(source.path()).unwrap());
            let directory = tempfile::tempdir().unwrap();
            let alias = directory.path().join("source.mp4");
            std::os::unix::fs::symlink(source.path(), &alias).unwrap();
            assert!(recording_source_stamp(&alias).is_err());
        }
    }
    #[test]
    // Verify that streaming index preserves signed vfr ticks order dedup and duration
    fn streaming_index_preserves_signed_vfr_ticks_order_dedup_and_duration() {
        let index = read_recording_index(
            &b"N/A\n-9\n0,extra\n-10\n-9\n50\n10\n"[..],
            -10,
            0.001,
            60_000,
        )
        .unwrap();
        assert_eq!(index.times, [0, 1000, 10000, 20000]);
        assert_eq!(index.minimum_delta, Some(1));
        assert_eq!(index.maximum_delta, 40);
        assert_eq!(index.times.partition_point(|t| *t < 10000), 2);
        assert_eq!(index.times.partition_point(|t| *t <= 10000), 3);
        let mut ticks = Vec::with_capacity(8192);
        ticks.extend([ordered_tick(-10), ordered_tick(-9), ordered_tick(20)]);
        let allocation = ticks.as_ptr();
        let index = finish_recording_index(ticks, -10, 0.001, 100_000).unwrap();
        assert_eq!(
            index.times.as_ptr(),
            allocation,
            "conversion allocated another full index"
        );
    }
    #[test]
    // Verify that streaming index rejects invalid and unbounded output
    fn streaming_index_rejects_invalid_and_unbounded_output() {
        assert!(read_recording_index(&b"N/A\n"[..], 0, 1., 10).is_err());
        assert!(read_recording_index(&b"1\n"[..], 0, 1., 10).is_err());
        assert!(read_recording_index(&[b'1'; 257][..], 0, 1., 10)
            .unwrap_err()
            .contains("bounds"));
        assert!(read_recording_index(&[255, b'\n'][..], 0, 1., 10).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that editor retirement waits for recording editor release not cached recording history
    fn editor_retirement_waits_for_recording_editor_release_not_cached_recording_history() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let recording = imported(&runtime, root.path());
        assert!(!runtime.video.editor_retirement_blocked());
        runtime
            .retain_video_recording(&recording.id, "editor-session".into())
            .unwrap();
        assert!(runtime.video.editor_retirement_blocked());
        runtime.release_video_recording("editor-session");
        assert!(!runtime.video.editor_retirement_blocked());
        assert!(runtime.recording_path(&recording.id).is_ok());
        runtime.shutdown();
    }

    use std::time::{Duration, Instant};
    // Import a synthetic recording fixture and wait for its test job to finish
    fn imported(runtime: &Arc<crate::Runtime>, root: &Path) -> Recording {
        let path = root.join("recording.mp4");
        let mut bytes = vec![0, 0, 0, 24, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm'];
        bytes.resize(4096, 7);
        std::fs::write(&path, bytes).unwrap();
        let id = runtime
            .start_video_import(path, "test-editor".into(), 1)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut recording = loop {
            let job = runtime.video_import_status(&id).unwrap();
            if let Some(recording) = job.recording {
                break recording;
            }
            assert_ne!(job.state, "failed", "{:?}", job.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        recording.width = 128;
        recording.height = 72;
        recording.duration_us = 10_000_000;
        recording.source_fps = 2.0;
        runtime.save_recording_metadata(recording).unwrap()
    }
    // Encode a small deterministic PNG frame for video tests
    fn frame() -> Vec<u8> {
        hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                128,
                72,
                image::Rgb([230, 30, 20]),
            )),
            "png",
        )
        .unwrap()
    }
    #[test]
    // Verify that cancelled browser clips release slots and recording leases each cycle
    fn cancelled_browser_clips_release_slots_and_recording_leases_each_cycle() {
        let mut observations = Vec::new();
        for repetition in 0..5 {
            let root = tempfile::tempdir().unwrap();
            let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
            let recording = imported(&runtime, root.path());
            let create = |generation| {
                runtime.prepare_recording_clip(
                    ClipDraft {
                        recording_id: recording.id.clone(),
                        name: "Unfinished clip".into(),
                        start_us: 0,
                        end_us: 1_000_000,
                        base_asset: None,
                        options: Default::default(),
                    },
                    "editor".into(),
                    generation,
                )
            };
            let payload = frame();
            for generation in 1..=2 {
                let job = create(generation).unwrap();
                runtime
                    .append_browser_media_timed(
                        &job,
                        0,
                        &batch(&[(500_000, payload.clone())]),
                        true,
                    )
                    .unwrap();
                runtime.cancel_media_preparation(&job).unwrap();
                assert!(runtime
                    .append_browser_media_timed(
                        &job,
                        1,
                        &batch(&[(500_000, payload.clone())]),
                        true
                    )
                    .is_err());
            }
            let retained_leases = runtime.video.recording_leases.lock().live.len();
            let storage = runtime.preparations.memory_stats();
            assert_eq!(storage["pending_buffer_capacity_bytes"], 0);
            assert_eq!(storage["cancelled_buffer_capacity_bytes"], 0);
            let third = create(3);
            observations.push(serde_json::json!({"repetition":repetition,"uploaded_encoded_bytes":payload.len()*2,"cancelled_recording_leases":retained_leases,"next_preparation_admitted":third.is_ok()}));
            if let Ok(job) = third {
                runtime.cancel_media_preparation(&job).unwrap();
            }
            runtime.shutdown();
        }
        eprintln!(
            "CANCELLED_BROWSER_CLIPS={}",
            serde_json::to_string(&observations).unwrap()
        );
        assert!(observations.iter().all(
            |o| o["cancelled_recording_leases"] == 0 && o["next_preparation_admitted"] == true
        ));
    }
    #[test]
    // Verify that closing GIF editors retires cache ownership without invalidating live frames
    fn closing_gif_editors_retires_cache_ownership_without_invalidating_live_frames() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let path = root.path().join("source.gif");
        std::fs::write(
            &path,
            crate::media::tests::isolated_gif(include_bytes!(
                "../../../tests/fixtures/media/source-25.gif"
            )),
        )
        .unwrap();
        let job = runtime
            .start_video_import(path, "fixture".into(), 1)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let recording = loop {
            let job = runtime.video_import_status(&job).unwrap();
            if let Some(recording) = job.recording {
                break recording;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        // Retain one control owner to reproduce the old cache-on-close policy
        // in this same optimized binary and on exactly the same media fixture
        runtime
            .retain_video_recording(&recording.id, "retained-cache-control".into())
            .unwrap();
        let mut control = Vec::new();
        for repetition in 0..5 {
            let started = Instant::now();
            let clip = runtime
                .recording_gif(&recording.id, 512, &Default::default())
                .unwrap();
            let reopen_us = started.elapsed().as_micros();
            let started = Instant::now();
            let warm = runtime
                .recording_gif(&recording.id, 512, &Default::default())
                .unwrap();
            let warm_us = started.elapsed().as_micros();
            assert!(Arc::ptr_eq(&clip, &warm));
            control.push(serde_json::json!({"repetition":repetition,"reopen_us":reopen_us,"warm_us":warm_us,"cached_unique_payload_bytes":runtime.cache_stats()["media_payload_storage"]["cached_payload_bytes"]}));
        }
        runtime.release_video_recording("retained-cache-control");
        assert_eq!(runtime.cache_stats()["media_budget"]["combined_bytes"], 0);
        eprintln!(
            "GIF_RETAINED_CACHE_CONTROL={}",
            serde_json::to_string(&control).unwrap()
        );
        let mut observations = Vec::new();
        for repetition in 0..5 {
            let owner = format!("editor-{repetition}");
            runtime
                .retain_video_recording(&recording.id, owner.clone())
                .unwrap();
            let started = Instant::now();
            let clip = runtime
                .recording_gif(&recording.id, 512, &Default::default())
                .unwrap();
            let reopen_us = started.elapsed().as_micros();
            let started = Instant::now();
            let warm = runtime
                .recording_gif(&recording.id, 512, &Default::default())
                .unwrap();
            let warm_us = started.elapsed().as_micros();
            assert!(Arc::ptr_eq(&clip, &warm));
            let payload: std::collections::BTreeMap<_, _> = clip
                .frames
                .iter()
                .map(|f| (Arc::as_ptr(f) as usize, f.len()))
                .collect();
            let held_frame = clip.frames[0].clone();
            let length = held_frame.len();
            drop((clip, warm));
            runtime.release_video_recording(&owner);
            let retained = runtime.video.gif_sources.lock().unwrap().len();
            assert_eq!(held_frame.len(), length);
            let after_close = runtime.cache_stats()["media_payload_storage"].clone();
            assert_eq!(after_close["cached_payload_bytes"], 0);
            assert_eq!(after_close["retired_live_payload_bytes"], length);
            drop(held_frame);
            let after_drop = runtime.cache_stats()["media_payload_storage"].clone();
            assert_eq!(after_drop["tracked_live_payload_bytes"], 0);
            observations.push(serde_json::json!({"repetition":repetition,"unique_frame_payload_bytes":payload.values().sum::<usize>(),"cache_entries_after_close":retained,"external_frame_bytes_after_close":length,"reopen_us":reopen_us,"warm_us":warm_us,"storage_after_close":after_close,"storage_after_frame_drop":after_drop}));
        }
        runtime.shutdown();
        eprintln!(
            "GIF_EDITOR_CACHE={}",
            serde_json::to_string(&observations).unwrap()
        );
        assert!(observations
            .iter()
            .all(|o| o["cache_entries_after_close"] == 0));
    }
    #[test]
    // Verify that shared editors keep cache until last close and old work cannot repopulate on reopen
    fn shared_editors_keep_cache_until_last_close_and_old_work_cannot_repopulate_on_reopen() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let recording = imported(&runtime, root.path());
        let id = &recording.id;
        runtime.retain_video_recording(id, "first".into()).unwrap();
        runtime.retain_video_recording(id, "second".into()).unwrap();
        let old_epoch = runtime.video.editor_epoch(id);
        let times = Arc::new(vec![0, 500_000, 1_000_000]);
        let clip = Arc::new(crate::media::Clip::new("gif", vec![frame()], vec![40]).unwrap());
        let stamp = recording_source_stamp(&runtime.recording_path(id).unwrap()).unwrap();
        let gif_entry = || {
            (
                format!("{id}:{}", recording.sha256),
                512,
                stamp.clone(),
                clip.clone(),
            )
        };
        runtime
            .video
            .cache_index(id, &old_epoch, times.clone())
            .unwrap();
        runtime
            .video
            .cache_gif(id, &old_epoch, gif_entry())
            .unwrap();
        runtime.release_video_recording("first");
        assert_eq!(runtime.video.native_indices.lock().unwrap().len(), 1);
        assert_eq!(runtime.video.gif_sources.lock().unwrap().len(), 1);
        runtime.release_video_recording("second");
        assert!(runtime.video.native_indices.lock().unwrap().is_empty());
        assert!(runtime.video.gif_sources.lock().unwrap().is_empty());
        runtime
            .retain_video_recording(id, "reopened".into())
            .unwrap();
        // These represent decodes/indexing already started by the old session
        runtime
            .video
            .cache_index(id, &old_epoch, times.clone())
            .unwrap();
        runtime
            .video
            .cache_gif(id, &old_epoch, gif_entry())
            .unwrap();
        runtime.video.cache_index(id, &None, times.clone()).unwrap();
        assert!(runtime.video.native_indices.lock().unwrap().is_empty());
        assert!(runtime.video.gif_sources.lock().unwrap().is_empty());
        assert_eq!(times[1], 500_000);
        assert!(!clip.frames[0].is_empty());
        let current = runtime.video.editor_epoch(id);
        runtime
            .video
            .cache_index(id, &current, times.clone())
            .unwrap();
        runtime.video.cache_gif(id, &current, gif_entry()).unwrap();
        assert_eq!(runtime.video.native_indices.lock().unwrap().len(), 1);
        assert_eq!(runtime.video.gif_sources.lock().unwrap().len(), 1);
        runtime.release_video_recording("reopened");
        runtime.shutdown();
    }
    // Encode test frames as a length-prefixed batch with per-frame delays
    fn batch(frames: &[(u32, Vec<u8>)]) -> Vec<u8> {
        let mut bytes = (frames.len() as u32).to_le_bytes().to_vec();
        for (delay, frame) in frames {
            bytes.extend(delay.to_le_bytes());
            bytes.extend((frame.len() as u32).to_le_bytes());
            bytes.extend(frame);
        }
        bytes
    }
    // Wait for a test media-preparation job to produce an asset or fail within its deadline
    fn completed(runtime: &crate::Runtime, id: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let job = runtime.media_preparation(id).unwrap();
            if let Some(asset) = job.asset {
                return asset;
            }
            assert_ne!(job.state, "failed", "{:?}", job.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    // Verify that recordings use bounded ranges and reviewed cleanup respects editor ownership
    fn recordings_use_bounded_ranges_and_reviewed_cleanup_respects_editor_ownership() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let recording = imported(&runtime, root.path());
        assert_eq!(
            runtime.read_recording_range(&recording.id, 4, 4).unwrap(),
            b"ftyp"
        );
        assert!(runtime
            .read_recording_range(&recording.id, 4090, 20)
            .is_err());
        assert!(runtime
            .read_recording_range(&recording.id, 0, 1024 * 1024 + 1)
            .is_err());
        assert!(runtime.read_recording_range("../outside", 0, 1).is_err());
        runtime
            .retain_video_recording(&recording.id, "active".into())
            .unwrap();
        assert!(runtime
            .review_unused_recordings()
            .unwrap()
            .recordings
            .is_empty());
        runtime.release_video_recording("active");
        assert!(runtime
            .retain_video_recording(&recording.id, "active".into())
            .is_err());
        let review = runtime.review_unused_recordings().unwrap();
        assert_eq!(review.recordings.len(), 1);
        runtime
            .retain_video_recording(&recording.id, "active-new".into())
            .unwrap();
        assert!(runtime.commit_recording_cleanup(&review.id).is_err());
        assert!(runtime.recording_path(&recording.id).unwrap().exists());
        runtime.release_video_recording("active-new");
        let review = runtime.review_unused_recordings().unwrap();
        assert_eq!(
            runtime.commit_recording_cleanup(&review.id).unwrap(),
            recording.bytes
        );
        assert!(runtime.video_recording(&recording.id).is_err());
        runtime.shutdown();
    }
    #[test]
    // Verify that portable clips retain source references and derive immutable revisions without originals
    fn portable_clips_retain_source_references_and_derive_immutable_revisions_without_originals() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let recording = imported(&runtime, root.path());
        let options = crate::media::MediaOptions {
            min_fps: Some(1.0),
            max_fps: Some(2.0),
            fit: "fit".into(),
            ..Default::default()
        };
        let draft = ClipDraft {
            recording_id: recording.id.clone(),
            name: "Sunset detail".into(),
            start_us: 0,
            end_us: 1_000_000,
            base_asset: None,
            options: options.clone(),
        };
        let job = runtime
            .prepare_recording_clip(draft, "clip-editor".into(), 1)
            .unwrap();
        runtime
            .append_browser_media_timed(
                &job,
                0,
                &batch(&[(500_000, frame()), (500_000, frame())]),
                true,
            )
            .unwrap();
        runtime.publish_browser_media(&job).unwrap();
        let asset = completed(&runtime, &job);
        runtime.finish_media_preparation(&job).unwrap();
        let bytes = runtime.profiles.asset(&asset).unwrap();
        let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
        assert!(!archive
            .file_names()
            .any(|name| name.is_ok_and(|name| name == "original.mp4")));
        let original =
            crate::media::Clip::from_archive_controlled(&bytes, &Default::default()).unwrap();
        assert_eq!(original.duration_us(), 1_000_000);
        assert_eq!(original.clip_info.as_ref().unwrap().revision, 1);
        assert!(runtime
            .review_unused_recordings()
            .unwrap()
            .recordings
            .is_empty());
        let library = runtime.media_library(1, 12, "Sunset").unwrap();
        assert_eq!(library.entries.len(), 1);
        assert_eq!(
            library.entries[0].clip_info.as_ref().unwrap().name,
            "Sunset detail"
        );
        std::fs::remove_file(runtime.recording_path(&recording.id).unwrap()).unwrap();
        let details = runtime.video_clip_details(&asset).unwrap();
        assert!(!details.source_available);
        assert_eq!(
            runtime.prepared_video_frame(&asset, 750_000).unwrap(),
            frame()
        );
        let device = "simulator:n4-pro:prepared-video";
        runtime.simulate_device(true, device).unwrap();
        let preview = runtime
            .prepared_video_preview(&asset, 750_000, device, None, &options)
            .unwrap();
        let decoded = image::load_from_memory(&preview).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (800, 480));
        let hits = runtime.profiles.render_cache.lock().unwrap().hits;
        assert_eq!(
            runtime
                .prepared_video_preview(&asset, 750_000, device, None, &options)
                .unwrap(),
            preview
        );
        assert!(runtime.profiles.render_cache.lock().unwrap().hits > hits);
        assert!(runtime
            .prepared_video_preview(&asset, 1_000_000, device, None, &options)
            .is_err());
        let derived = runtime
            .prepare_derived_video(
                asset.clone(),
                ClipDraft {
                    recording_id: recording.id,
                    name: "Shorter sunset".into(),
                    start_us: 200_000,
                    end_us: 800_000,
                    base_asset: Some(asset.clone()),
                    options,
                },
                "derived-editor".into(),
                2,
            )
            .unwrap();
        let replacement = completed(&runtime, &derived);
        assert_ne!(asset, replacement);
        let clip = runtime.profiles.clip(&replacement).unwrap().unwrap();
        assert_eq!(clip.duration_us(), 600_000);
        assert_eq!(clip.clip_info.as_ref().unwrap().revision, 2);
        assert_eq!(clip.prepared_fps(), 2.);
        let relinked = imported(&runtime, root.path());
        assert!(runtime.video_clip_details(&asset).unwrap().source_available);
        assert!(runtime
            .video_clips(Some(&relinked.id))
            .unwrap()
            .iter()
            .any(|entry| entry.asset == asset));
        assert!(runtime
            .review_unused_recordings()
            .unwrap()
            .recordings
            .is_empty());
        assert_eq!(
            runtime
                .profiles
                .clip(&asset)
                .unwrap()
                .unwrap()
                .clip_info
                .as_ref()
                .unwrap()
                .revision,
            1
        );
        runtime.finish_media_preparation(&derived).unwrap();
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires installed FFmpeg and loopback sockets, never HID"]
    // Verify that native recording fallback preserves source rate selected ranges and steps
    fn native_recording_fallback_preserves_source_rate_selected_ranges_and_steps() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let path = root.path().join("source.mp4");
        std::fs::write(
            &path,
            include_bytes!("../../../tests/fixtures/media/source-120-vp9.mp4"),
        )
        .unwrap();
        let import = runtime
            .start_video_import(path, "native-editor".into(), 1)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let recording = loop {
            let job = runtime.video_import_status(&import).unwrap();
            if let Some(source) = job.recording {
                break runtime.probe_recording(&source.id).unwrap();
            }
            assert_ne!(job.state, "failed", "{:?}", job.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!((recording.source_fps - 120.).abs() < 0.01);
        assert_eq!(
            runtime
                .recording_position(&recording.id, 250_000, 1)
                .unwrap(),
            258_333
        );
        assert_eq!(
            runtime
                .recording_position(&recording.id, 260_000, 0)
                .unwrap(),
            258_333
        );
        let job = runtime
            .prepare_recording_clip_native(
                ClipDraft {
                    recording_id: recording.id.clone(),
                    name: "Native trimmed".into(),
                    start_us: 200_000,
                    end_us: 700_000,
                    base_asset: None,
                    options: crate::media::MediaOptions {
                        max_fps: Some(30.),
                        max_frame_edge: Some(128),
                        ..Default::default()
                    },
                },
                "native-editor".into(),
                2,
            )
            .unwrap();
        let asset = completed(&runtime, &job);
        let clip = runtime.profiles.clip(&asset).unwrap().unwrap();
        assert_eq!(clip.frames.len(), 15);
        assert_eq!(clip.duration_us(), 500_000);
        runtime.finish_media_preparation(&job).unwrap();
        runtime.shutdown();
    }
    #[test]
    // Verify that current transparent gifs open without external decoders and keep variable holds
    fn current_transparent_gifs_open_without_external_decoders_and_keep_variable_holds() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let path = root.path().join("transparent.gif");
        let mut bytes = vec![];
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for delay in [40, 80, 120] {
                let pixels = image::RgbaImage::from_fn(4, 4, |x, y| {
                    if x == 0 || y == 0 {
                        image::Rgba([0, 0, 0, 0])
                    } else {
                        image::Rgba([240, 30, 20, 255])
                    }
                });
                encoder
                    .encode_frame(image::Frame::from_parts(
                        pixels,
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        std::fs::write(&path, bytes).unwrap();
        let asset = runtime.import_asset(&path).unwrap();
        assert_eq!(
            runtime
                .profiles
                .clip(&asset)
                .unwrap()
                .unwrap()
                .preparation_revision,
            2
        );
        let job = runtime
            .recording_for_asset(&asset, "gif-editor".into(), 1)
            .unwrap();
        let recording = runtime
            .video_import_status(&job)
            .unwrap()
            .recording
            .unwrap();
        let recording = runtime.probe_recording(&recording.id).unwrap();
        assert_eq!(recording.source_format, "gif");
        assert_eq!(recording.source_fps, 25.);
        assert!(recording.variable_rate);
        let source = runtime
            .recording_gif(&recording.id, 512, &Default::default())
            .unwrap();
        assert_eq!(source.delays_us, vec![40_000, 80_000, 120_000]);
        let mut options = crate::media::MediaOptions {
            max_frame_edge: Some(64),
            fit: "fit".into(),
            letterbox: Some("transparent".into()),
            ..Default::default()
        };
        let job = runtime
            .prepare_recording_clip_native(
                ClipDraft {
                    recording_id: recording.id.clone(),
                    name: "Transparent GIF".into(),
                    start_us: 40_000,
                    end_us: 240_000,
                    base_asset: None,
                    options: options.clone(),
                },
                "gif-editor".into(),
                1,
            )
            .unwrap();
        let asset = completed(&runtime, &job);
        let clip = runtime.profiles.clip(&asset).unwrap().unwrap();
        assert_eq!(clip.delays_us, vec![80_000, 120_000]);
        assert_eq!(
            image::load_from_memory(&clip.frames[0])
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            0
        );
        let formats = vec!["jpeg".into(), "png".into()];
        let state = runtime
            .simulate_device(true, "simulator:gif-color")
            .unwrap();
        let device = state
            .devices
            .iter()
            .find(|device| device.id == "simulator:gif-color")
            .unwrap();
        let control = device.manifest.control("key-1").unwrap();
        let mut binding = state.profiles[0].pages[0].bindings["key-1"].clone();
        binding.title.clear();
        binding.color = "#145096".into();
        binding.asset = Some(asset.clone());
        binding.media = options.clone();
        binding.media.encoding = "png".into();
        binding.appearance = None;
        binding.composition = None;
        let mut context = runtime.render_scene(&device.id).unwrap();
        context.media_elapsed = Duration::ZERO;
        let accelerated = runtime
            .render_bound_surface_in_scene(
                control,
                Some(&binding),
                0,
                &context.scope,
                "key-1",
                &context,
            )
            .unwrap();
        let composed = runtime
            .render_binding(control, Some(&binding), 0, "", None)
            .unwrap();
        assert_eq!(
            image::load_from_memory(&accelerated).unwrap().to_rgba8(),
            image::load_from_memory(&composed).unwrap().to_rgba8()
        );
        let colored = runtime
            .render_animation_frame_shared(
                &clip,
                0,
                [96, 96],
                0,
                &options,
                &["png".into()],
                Some([20, 80, 150, 255]),
            )
            .unwrap();
        assert_eq!(
            image::load_from_memory(&colored)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)
                .0,
            [20, 80, 150, 255]
        );
        let transparent = runtime
            .render_media_frame_shared(&asset, Duration::ZERO, [800, 480], 180, &options, &formats)
            .unwrap();
        assert!(transparent.starts_with(b"\x89PNG"));
        assert_eq!(
            image::load_from_memory(&transparent)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            0
        );
        options.letterbox = Some("black".into());
        let opaque = runtime
            .render_media_frame_shared(&asset, Duration::ZERO, [800, 480], 180, &options, &formats)
            .unwrap();
        assert!(opaque.starts_with(&[0xff, 0xd8]));
        runtime.finish_media_preparation(&job).unwrap();
        assert!(runtime
            .review_unused_recordings()
            .unwrap()
            .recordings
            .is_empty());
        let deletion = runtime.prepare_media_deletion(&asset).unwrap();
        runtime.commit_media_deletion(&deletion.id).unwrap();
        let review = runtime.review_unused_recordings().unwrap();
        assert_eq!(review.recordings.len(), 1);
        assert_eq!(review.recordings[0].source_format, "gif");
        let original = runtime.recording_path(&recording.id).unwrap();
        assert_eq!(
            runtime.commit_recording_cleanup(&review.id).unwrap(),
            recording.bytes
        );
        assert!(!original.exists());
        assert!(runtime.video_recording(&recording.id).is_err());
        runtime.shutdown();
    }
    #[test]
    // Verify that GIF sources share the clip editor timing cache revisions and portable playback
    fn gif_sources_share_the_clip_editor_timing_cache_revisions_and_portable_playback() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let path = root.path().join("source.gif");
        std::fs::write(
            &path,
            include_bytes!("../../../tests/fixtures/media/source-25.gif"),
        )
        .unwrap();
        let job = runtime
            .start_video_import(path, "gif-editor".into(), 1)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let recording = loop {
            let job = runtime.video_import_status(&job).unwrap();
            if let Some(recording) = job.recording {
                break recording;
            }
            assert_ne!(job.state, "failed", "{:?}", job.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(recording.source_format, "gif");
        runtime
            .retain_video_recording(&recording.id, "gif-cache-owner".into())
            .unwrap();
        let recording = runtime.probe_recording(&recording.id).unwrap();
        assert_eq!(
            (recording.width, recording.height, recording.duration_us),
            (320, 180, 1_000_000)
        );
        assert_eq!(recording.source_fps, 25.);
        let source = runtime
            .recording_gif(&recording.id, 512, &Default::default())
            .unwrap();
        assert!(Arc::ptr_eq(
            &source,
            &runtime
                .recording_gif(&recording.id, 512, &Default::default())
                .unwrap()
        ));
        // Timeline thumbnails reuse the same decode, then resize only the selected frame
        assert!(Arc::ptr_eq(
            &source,
            &runtime
                .recording_gif(&recording.id, 160, &Default::default())
                .unwrap()
        ));
        let thumbnail =
            image::load_from_memory(&runtime.recording_frame(&recording.id, 0, 160).unwrap())
                .unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (160, 90));
        assert_eq!(
            runtime
                .recording_position(&recording.id, 510_000, 0)
                .unwrap(),
            480_000
        );
        assert_eq!(
            runtime
                .recording_position(&recording.id, 480_000, 1)
                .unwrap(),
            520_000
        );
        assert_eq!(
            runtime
                .recording_position(&recording.id, 480_000, -1)
                .unwrap(),
            440_000
        );
        let options = crate::media::MediaOptions {
            min_fps: Some(5.),
            max_fps: Some(10.),
            max_frame_edge: Some(128),
            letterbox: Some("transparent".into()),
            fit: "fit".into(),
            ..Default::default()
        };
        let job = runtime
            .prepare_recording_clip_native(
                ClipDraft {
                    recording_id: recording.id.clone(),
                    name: "GIF section".into(),
                    start_us: 200_000,
                    end_us: 800_000,
                    base_asset: None,
                    options: options.clone(),
                },
                "gif-editor".into(),
                1,
            )
            .unwrap();
        let asset = completed(&runtime, &job);
        runtime.finish_media_preparation(&job).unwrap();
        let clip = runtime.profiles.clip(&asset).unwrap().unwrap();
        assert_eq!(clip.source_format, "gif");
        assert_eq!(clip.delays_us, vec![100_000; 6]);
        assert_eq!(
            clip.clip_info.as_ref().unwrap().recording.source_format,
            "gif"
        );
        let decoded = image::load_from_memory(&clip.frames[0]).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (128, 72));
        let next = runtime
            .prepare_derived_video(
                asset.clone(),
                ClipDraft {
                    recording_id: recording.id.clone(),
                    name: "Shorter GIF".into(),
                    start_us: 300_000,
                    end_us: 700_000,
                    base_asset: Some(asset.clone()),
                    options: options.clone(),
                },
                "gif-editor".into(),
                2,
            )
            .unwrap();
        let derived = completed(&runtime, &next);
        let revision = runtime.profiles.clip(&derived).unwrap().unwrap();
        assert_eq!(revision.source_format, "gif");
        assert_eq!(revision.duration_us(), 400_000);
        assert_eq!(revision.clip_info.as_ref().unwrap().revision, 2);
        runtime.finish_media_preparation(&next).unwrap();
        std::fs::remove_file(runtime.recording_path(&recording.id).unwrap()).unwrap();
        assert!(!runtime.video_clip_details(&asset).unwrap().source_available);
        assert!(runtime.prepared_video_frame(&asset, 500_000).is_ok());
        // Source-free exported GIF clips retain their provenance and source type
        let portable = crate::media::Clip::from_archive_controlled(
            &runtime.profiles.asset(&asset).unwrap(),
            &Default::default(),
        )
        .unwrap();
        assert_eq!(portable.source_format, "gif");
        runtime.shutdown();
    }
    #[test]
    // Verify that invalid clip bounds and metadata do not publish
    fn invalid_clip_bounds_and_metadata_do_not_publish() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let recording = imported(&runtime, root.path());
        let mut malformed = recording.clone();
        malformed.source_fps = f64::NAN;
        assert!(runtime.save_recording_metadata(malformed).is_err());
        let draft = ClipDraft {
            recording_id: recording.id,
            name: "Too long".into(),
            start_us: 0,
            end_us: 31_000_000,
            base_asset: None,
            options: Default::default(),
        };
        assert!(runtime
            .prepare_recording_clip(draft, "editor".into(), 1)
            .is_err());
        assert!(runtime.video_clips(None).unwrap().is_empty());
        runtime.shutdown();
    }
}
