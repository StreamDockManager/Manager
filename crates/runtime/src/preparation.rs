use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MediaPreparation {
    pub id: String,
    pub key: String,
    #[ts(type = "number")]
    pub generation: u64,
    pub state: String,
    pub frames: usize,
    pub asset: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparationState {
    Queued,
    Preparing,
    BrowserPending,
    BrowserPreparing,
    Publishing,
    Complete,
    Failed,
    Cancelled,
}
impl PreparationState {
    // Map preparation states to their frontend status strings
    fn wire(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Preparing => "preparing",
            Self::BrowserPending => "browser_pending",
            Self::BrowserPreparing | Self::Publishing => "browser_preparing",
            Self::Complete => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
    // Report the native condition for preparation
    fn native(self) -> bool {
        matches!(self, Self::Queued | Self::Preparing)
    }
    // Report the browser condition for preparation
    fn browser(self) -> bool {
        matches!(self, Self::BrowserPending | Self::BrowserPreparing)
    }
    // Report the active preparation state
    fn active(self) -> bool {
        self.native() || self.browser() || self == Self::Publishing
    }
}
struct PreparationJob {
    id: String,
    key: String,
    generation: u64,
    state: PreparationState,
    frames: usize,
    asset: Option<String>,
    error: Option<String>,
}
impl PreparationJob {
    // Return a snapshot of preparation state
    fn snapshot(&self) -> MediaPreparation {
        MediaPreparation {
            id: self.id.clone(),
            key: self.key.clone(),
            generation: self.generation,
            state: self.state.wire().into(),
            frames: self.frames,
            asset: self.asset.clone(),
            error: self.error.clone(),
        }
    }
    // Apply a preparation-state transition only when the current lifecycle permits it
    fn transition(&mut self, next: PreparationState) -> Result<(), String> {
        use PreparationState::*;
        let valid = match next {
            Queued => false,
            Preparing => self.state.native() || self.state.browser(),
            BrowserPending => self.state == Queued,
            BrowserPreparing => self.state.browser(),
            Publishing => self.state.browser(),
            Complete | Failed => self.state.active(),
            Cancelled => self.state.active() || self.state == Complete,
        };
        if !valid {
            return Err("Media preparation cancelled or already published".into());
        }
        self.state = next;
        Ok(())
    }
    // Native cancellation is a request until the worker reports completion
    fn cancel(&mut self) {
        if self.state.browser() || self.state == PreparationState::Complete {
            self.transition(PreparationState::Cancelled).unwrap();
            self.asset = None;
        }
    }
}
#[derive(Default)]
pub struct Preparations {
    jobs: Mutex<BTreeMap<String, (PreparationJob, Arc<AtomicBool>)>>,
    assets: Mutex<BTreeMap<String, crate::asset_leases::AssetLease>>,
    browser: Mutex<BTreeMap<String, BrowserPreparation>>,
}
struct BrowserPreparation {
    reservation: crate::media_budget::Reservation,
    frames: Vec<Vec<u8>>,
    delays: Vec<u32>,
    bytes: usize,
    sequence: u32,
    started: std::time::Instant,
    in_flight: Arc<AtomicBool>,
    precise: Option<bool>,
    clip_info: Option<crate::video::ClipInfo>,
    recording_lease: Option<crate::asset_leases::AssetLease>,
}
impl crate::Runtime {
    // Prepare browser clip
    pub(crate) fn prepare_browser_clip(
        &self,
        info: crate::video::ClipInfo,
        key: String,
        generation: u64,
    ) -> Result<String, String> {
        let mut browser = self.preparations.browser.lock().unwrap();
        self.preparations.prune_browser(&mut browser);
        if browser.len() >= 2 {
            return Err("Two video preparations are already active".into());
        }
        let reservation = self
            .profiles
            .media_budget
            .reserve(2 * crate::media::MAX_METADATA)?;
        let recording_lease = self.pin_recording(&info.recording.id, "Clip preparation")?;
        let (id, _) = self.preparations.enqueue(key, generation)?;
        self.preparations.prune_browser(&mut browser);
        browser.insert(
            id.clone(),
            BrowserPreparation {
                reservation,
                frames: vec![],
                delays: vec![],
                bytes: 0,
                sequence: 0,
                started: std::time::Instant::now(),
                in_flight: Arc::new(AtomicBool::new(false)),
                precise: Some(true),
                recording_lease: Some(recording_lease),
                clip_info: Some(info),
            },
        );
        self.preparations
            .jobs
            .lock()
            .unwrap()
            .get_mut(&id)
            .unwrap()
            .0
            .transition(PreparationState::BrowserPending)?;
        Ok(id)
    }
    // Require the browser preparation job to remain active and uncancelled
    fn browser_preparation_current(&self, id: &str) -> Result<(), String> {
        let jobs = self.preparations.jobs.lock().unwrap();
        let (job, cancel) = jobs.get(id).ok_or("Unknown media preparation")?;
        if cancel.load(Ordering::SeqCst) || !job.state.browser() {
            return Err("Media preparation cancelled or already published".into());
        }
        Ok(())
    }
    /// Each batch is acknowledged after validation and admission; at most eight
    /// encoded frames and 1 MiB may be in flight from a worker
    pub fn append_browser_media(
        &self,
        id: &str,
        sequence: u32,
        bytes: &[u8],
    ) -> Result<(), String> {
        self.append_browser_media_timed(id, sequence, bytes, false)
    }
    // Append browser media timed
    pub fn append_browser_media_timed(
        &self,
        id: &str,
        sequence: u32,
        bytes: &[u8],
        precise: bool,
    ) -> Result<(), String> {
        self.browser_preparation_current(id)?;
        let active = self
            .preparations
            .browser
            .lock()
            .unwrap()
            .get(id)
            .map(|job| job.in_flight.clone())
            .ok_or("Video preparation expired")?;
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Await the previous video batch acknowledgement")?;
        struct Batch(Arc<AtomicBool>);
        impl Drop for Batch {
            // Release this preparation owner when it leaves scope
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _batch = Batch(active);
        if bytes.len() > 1024 * 1024 || bytes.len() < 4 {
            return Err("Video batch exceeds limits".into());
        }
        let count = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        if !(1..=8).contains(&count) {
            return Err("Invalid video frame batch".into());
        }
        let validation = self
            .profiles
            .media_budget
            .reserve(bytes.len() + count * (size_of::<(u32, Vec<u8>)>() + size_of::<u32>()))?;
        let cancelled = self
            .preparations
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .map(|(_, cancelled)| cancelled.clone())
            .ok_or("Video preparation expired")?;
        // Repeated cancellation must not leave many old validation decoders
        // running beside the two current browser jobs. Waiting uses the same
        // cancellable native preparation gate, without any owner lock held
        let decode = self.profiles.media_gate.lock_cancel(&cancelled)?;
        let mut cursor = 4usize;
        let mut frames = Vec::with_capacity(count);
        let mut edges = Vec::with_capacity(count);
        for _ in 0..count {
            if cancelled.load(Ordering::SeqCst) {
                return Err("Media preparation cancelled".into());
            }
            let header = bytes
                .get(cursor..cursor + 8)
                .ok_or("Truncated video batch")?;
            let delay = u32::from_le_bytes(header[..4].try_into().unwrap());
            let length = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
            cursor += 8;
            let frame = bytes
                .get(cursor..cursor.checked_add(length).ok_or("Video batch overflow")?)
                .ok_or("Truncated video frame")?;
            cursor += length;
            if length == 0
                || length > 2 * 1024 * 1024
                || delay == 0
                || delay > if precise { 30_000_000 } else { 30_000 }
            {
                return Err("Invalid video frame size or timestamp".into());
            }
            let mut decoder = image::ImageReader::new(std::io::Cursor::new(frame))
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(1024);
            limits.max_image_height = Some(1024);
            limits.max_alloc = Some(8 * 1024 * 1024);
            decoder.limits(limits);
            let pixels = decoder.decode().map_err(|e| e.to_string())?;
            edges.push(pixels.width().max(pixels.height()));
            frames.push((delay, frame.to_vec()));
        }
        if cursor != bytes.len() {
            return Err("Trailing bytes in video batch".into());
        }
        drop(decode);
        self.commit_browser_batch(id, sequence, precise, frames, edges, Some(&validation))
    }
    // Commit browser batch
    fn commit_browser_batch(
        &self,
        id: &str,
        sequence: u32,
        precise: bool,
        frames: Vec<(u32, Vec<u8>)>,
        edges: Vec<u32>,
        validation: Option<&crate::media_budget::Reservation>,
    ) -> Result<(), String> {
        let mut browser = self.preparations.browser.lock().unwrap();
        let job = browser.get_mut(id).ok_or("Video preparation expired")?;
        // A batch may finish validation after cancellation. Check again while
        // holding both publication locks so it cannot revive a cancelled job
        let mut jobs = self.preparations.jobs.lock().unwrap();
        let entry = jobs.get_mut(id).ok_or("Video preparation expired")?;
        if entry.1.load(Ordering::SeqCst) || !entry.0.state.browser() {
            return Err("Media preparation cancelled or already published".into());
        }
        if job.precise.is_some_and(|timing| timing != precise) {
            return Err("Video timing units changed during preparation".into());
        }
        if let Some(info) = &job.clip_info {
            let [minimum, maximum] = info.options.detail_bounds();
            if edges
                .iter()
                .any(|edge| *edge < u32::from(minimum) || *edge > u32::from(maximum))
            {
                return Err("Prepared dimensions fall outside the selected detail bounds; adjust minimum or maximum detail".into());
            }
        }
        if job.sequence != sequence
            || job.started.elapsed() > std::time::Duration::from_secs(120)
            || job.frames.len() + frames.len() > crate::media::MAX_FRAMES
            || job.bytes + frames.iter().map(|(_, f)| f.len()).sum::<usize>()
                > crate::media::MAX_CLIP
            || job.delays.iter().map(|d| u64::from(*d)).sum::<u64>()
                + frames.iter().map(|(d, _)| u64::from(*d)).sum::<u64>()
                > if precise { 30_000_000 } else { 30_000 }
        {
            return Err(
                "Video preparation exceeds sequence, time, frame, duration or byte limits".into(),
            );
        }
        let count = job.frames.len() + frames.len();
        let capacity = job.frames.iter().map(Vec::capacity).sum::<usize>()
            + frames
                .iter()
                .map(|(_, frame)| frame.capacity())
                .sum::<usize>()
            + job.frames.capacity().max(count) * size_of::<Vec<u8>>()
            + job.delays.capacity().max(count) * size_of::<u32>()
            + 2 * crate::media::MAX_METADATA;
        // Admission happens before changing sequence, timing or owned vectors
        if let Some(validation) = validation {
            job.reservation.resize_from(validation, capacity)?;
        } else {
            job.reservation.resize(capacity)?;
        }
        job.frames.reserve_exact(count - job.frames.len());
        job.delays.reserve_exact(count - job.delays.len());
        job.precise = Some(precise);
        for (delay, frame) in frames {
            job.bytes += frame.len();
            job.frames.push(frame);
            job.delays.push(delay);
        }
        job.sequence += 1;
        let frame_count = job.frames.len();
        entry.0.transition(PreparationState::BrowserPreparing)?;
        entry.0.frames = frame_count;
        Ok(())
    }
    // Publish browser media
    pub fn publish_browser_media(&self, id: &str) -> Result<(), String> {
        self.browser_preparation_current(id)?;
        let job = {
            let mut browser = self.preparations.browser.lock().unwrap();
            let job = browser.get(id).ok_or("Video preparation expired")?;
            // Reserve publication workspace before consuming the recoverable
            // draft. Failed admission leaves its buffers and sequence intact
            let capacity = job.frames.iter().map(Vec::capacity).sum::<usize>()
                + job.frames.capacity() * size_of::<Vec<u8>>()
                + job.delays.capacity() * size_of::<u32>()
                + 2 * crate::media::MAX_METADATA
                + crate::media_budget::PUBLICATION_WORK_BYTES;
            job.reservation.resize(capacity)?;
            let mut jobs = self.preparations.jobs.lock().unwrap();
            let (entry, cancel) = jobs.get_mut(id).ok_or("Video preparation expired")?;
            if cancel.load(Ordering::SeqCst) {
                return Err("Media preparation cancelled".into());
            }
            entry.transition(PreparationState::Publishing)?;
            browser.remove(id).unwrap()
        };
        let result = (|| {
            let delays_us = if job.precise == Some(true) {
                job.delays
            } else {
                job.delays.into_iter().map(|ms| ms * 1000).collect()
            };
            if job.clip_info.as_ref().is_some_and(|info| {
                delays_us.iter().map(|d| u64::from(*d)).sum::<u64>() != info.end_us - info.start_us
            }) {
                return Err("Prepared timeline does not match the selected clip".into());
            }
            let clip = Arc::new(crate::media::Clip::with_timing(
                "mp4",
                job.frames,
                delays_us,
                2,
                job.clip_info,
            )?);
            job.reservation
                .track_clip(&clip, crate::media_budget::PUBLICATION_WORK_BYTES)?;
            let cancelled = self
                .preparations
                .jobs
                .lock()
                .unwrap()
                .get(id)
                .map(|(_, cancelled)| cancelled.clone())
                .ok_or("Video preparation expired")?;
            let control = crate::media::PreparationControl {
                cancelled,
                progress: Arc::new(|_| {}),
            };
            let (asset, lease) = self
                .profiles
                .publish_prepared_clip_shared(clip, &[], &control)?;
            control.check()?;
            Ok((asset, lease))
        })();
        let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
        let published = self.preparations.complete_asset(id, result);
        outcome?;
        if !published {
            return Err("Media preparation cancelled or already published".into());
        }
        Ok(())
    }
    // Prepare media
    pub fn prepare_media(
        self: &Arc<Self>,
        path: std::path::PathBuf,
        key: String,
        generation: u64,
    ) -> Result<String, String> {
        let (id, cancelled) = self.preparations.enqueue(key, generation)?;
        let runtime = self.clone();
        let output = id.clone();
        let progress_id = id.clone();
        let weak = Arc::downgrade(self);
        let progress = Arc::new(move |frames| {
            if let Some(runtime) = weak.upgrade() {
                if let Some((job, _)) = runtime
                    .preparations
                    .jobs
                    .lock()
                    .unwrap()
                    .get_mut(&progress_id)
                {
                    if job.state.native() {
                        job.transition(PreparationState::Preparing).unwrap();
                        job.frames = frames;
                    }
                }
            }
        });
        std::thread::spawn(move || {
            let result = runtime.profiles.import_asset_pinned(
                &path,
                &crate::media::PreparationControl {
                    cancelled: cancelled.clone(),
                    progress,
                },
                "Media preparation result",
            );
            runtime.preparations.complete_asset(&id, result);
        });
        Ok(output)
    }
    // Finalize media preparation
    pub fn finish_media_preparation(&self, id: &str) -> Result<(), String> {
        let jobs = self.preparations.jobs.lock().unwrap();
        let (job, _) = jobs.get(id).ok_or("Unknown media preparation")?;
        if job.state.active() {
            return Err("Media preparation is still active".into());
        }
        drop(jobs);
        self.preparations.assets.lock().unwrap().remove(id);
        self.preparations.browser.lock().unwrap().remove(id);
        self.profiles.media_budget.trim_idle();
        Ok(())
    }
    // Return a snapshot of the retained media preparation job
    pub fn media_preparation(&self, id: &str) -> Result<MediaPreparation, String> {
        self.preparations
            .jobs
            .lock()
            .unwrap()
            .get(id)
            .map(|(job, _)| job.snapshot())
            .ok_or("Unknown media preparation".into())
    }
    // Cancel media preparation
    pub fn cancel_media_preparation(&self, id: &str) -> Result<(), String> {
        {
            let mut jobs = self.preparations.jobs.lock().unwrap();
            let (job, cancel) = jobs.get_mut(id).ok_or("Unknown media preparation")?;
            cancel.store(true, Ordering::SeqCst);
            job.cancel();
        }
        // Admission takes browser -> jobs. Drop the job lock before releasing
        // pending buffers/leases, including a batch validated concurrently
        self.preparations.browser.lock().unwrap().remove(id);
        self.preparations.assets.lock().unwrap().remove(id);
        self.profiles.media_budget.trim_idle();
        Ok(())
    }
}

impl Preparations {
    // Report whether current work prevents releasing editor resources
    pub(crate) fn editor_retirement_blocked(&self) -> bool {
        use crate::editor_retirement::unavailable_or;
        unavailable_or(&self.browser, |jobs| !jobs.is_empty())
            || unavailable_or(&self.jobs, |jobs| {
                jobs.values().any(|(job, _)| job.state.active())
            })
            || unavailable_or(&self.assets, |assets| !assets.is_empty())
    }

    // Report retained browser buffer capacities and recording or asset lease counts
    pub(crate) fn memory_stats(&self) -> serde_json::Value {
        let browser = self.browser.lock().unwrap();
        let jobs = self.jobs.lock().unwrap();
        let capacity = |job: &BrowserPreparation| {
            job.frames.capacity() * size_of::<Vec<u8>>()
                + job.frames.iter().map(Vec::capacity).sum::<usize>()
                + job.delays.capacity() * size_of::<u32>()
        };
        serde_json::json!({
            "browser_jobs":browser.len(),
            "pending_buffer_capacity_bytes":browser.values().map(capacity).sum::<usize>(),
            "cancelled_buffer_capacity_bytes":browser.iter().filter(|(id, _)|
                jobs.get(*id).is_none_or(|(_, cancel)| cancel.load(Ordering::SeqCst)))
                .map(|(_, job)| capacity(job)).sum::<usize>(),
            "pending_recording_leases":browser.values().filter(|job| job.recording_lease.is_some()).count(),
            // Leases protect disk assets; their payload, if cached, is already
            // counted by the unique media allocation union
            "staged_asset_leases":self.assets.lock().unwrap().len(),
        })
    }
    // Prune browser
    fn prune_browser(&self, browser: &mut BTreeMap<String, BrowserPreparation>) {
        let mut jobs = self.jobs.lock().unwrap();
        browser.retain(|id, preparation| {
            let live = preparation.started.elapsed() <= std::time::Duration::from_secs(120)
                && jobs
                    .get(id)
                    .is_some_and(|(_, cancel)| !cancel.load(Ordering::SeqCst));
            if !live {
                if let Some((job, cancel)) = jobs.get_mut(id) {
                    cancel.store(true, Ordering::SeqCst);
                    job.cancel();
                }
            }
            live
        });
    }
    // Queue a preparation request
    fn enqueue(&self, key: String, generation: u64) -> Result<(String, Arc<AtomicBool>), String> {
        if key.len() > 300 || key.is_empty() {
            return Err("Invalid media preparation key".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.values().filter(|(j, _)| j.state.active()).count() >= 34 {
            return Err("Media preparation queue is full".into());
        }
        for (job, cancel) in jobs.values() {
            if job.key == key {
                if job.generation >= generation {
                    return Err("Stale media preparation generation".into());
                }
                cancel.store(true, Ordering::SeqCst);
            }
        }
        while jobs.len() >= 64 {
            let candidate = jobs
                .iter()
                .find(|(_, (j, _))| !j.state.active())
                .map(|(id, _)| id.clone())
                .ok_or("Media preparation queue is full")?;
            jobs.remove(&candidate);
            self.assets.lock().unwrap().remove(&candidate);
        }
        jobs.insert(
            id.clone(),
            (
                PreparationJob {
                    id: id.clone(),
                    key,
                    generation,
                    state: PreparationState::Queued,
                    frames: 0,
                    asset: None,
                    error: None,
                },
                cancelled.clone(),
            ),
        );
        Ok((id, cancelled))
    }

    // Regeneration keeps an existing asset identity and has no new result lease
    fn complete(&self, id: &str, result: Result<String, String>) {
        self.complete_result(id, result.map(|asset| (asset, None)));
    }
    // Complete asset
    fn complete_asset(
        &self,
        id: &str,
        result: Result<(String, crate::asset_leases::AssetLease), String>,
    ) -> bool {
        self.complete_result(id, result.map(|(asset, lease)| (asset, Some(lease))))
    }
    // Complete result
    fn complete_result(
        &self,
        id: &str,
        result: Result<(String, Option<crate::asset_leases::AssetLease>), String>,
    ) -> bool {
        let mut jobs = self.jobs.lock().unwrap();
        let Some((job, cancelled)) = jobs.get_mut(id) else {
            return false;
        };
        if !job.state.active() {
            return false;
        }
        if cancelled.load(Ordering::SeqCst) {
            job.transition(PreparationState::Cancelled).unwrap();
            return false;
        }
        match result {
            Ok((asset, lease)) => {
                job.transition(PreparationState::Complete).unwrap();
                job.asset = Some(asset);
                if let Some(lease) = lease {
                    self.assets.lock().unwrap().insert(id.into(), lease);
                }
                true
            }
            Err(error) => {
                job.transition(PreparationState::Failed).unwrap();
                job.error = Some(error);
                false
            }
        }
    }

    // Cancel all pending preparation operations
    pub fn cancel_all(&self) {
        {
            for (job, cancel) in self.jobs.lock().unwrap().values_mut() {
                cancel.store(true, Ordering::SeqCst);
                if job.state.browser() {
                    job.cancel();
                }
            }
        }
        self.browser.lock().unwrap().clear();
    }
}

impl crate::Runtime {
    // Start native video clip
    pub(crate) fn start_native_video_clip(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let (mut browser, cancel, path) = {
            let mut browser_jobs = self.preparations.browser.lock().unwrap();
            let browser = browser_jobs.get(id).ok_or("Unknown clip preparation")?;
            let info = browser
                .clip_info
                .as_ref()
                .ok_or("Clip provenance missing")?;
            let path = self.recording_path(&info.recording.id)?;
            let mut jobs = self.preparations.jobs.lock().unwrap();
            let (entry, cancel) = jobs.get_mut(id).ok_or("Unknown preparation")?;
            if cancel.load(Ordering::SeqCst) || !entry.state.browser() {
                return Err("Media preparation cancelled or already published".into());
            }
            entry.transition(PreparationState::Preparing)?;
            (browser_jobs.remove(id).unwrap(), cancel.clone(), path)
        };
        let info = browser.clip_info.unwrap();
        let recording_lease = browser.recording_lease;
        // Release browser buffers before the native worker takes ownership
        browser.frames = Vec::new();
        browser.delays = Vec::new();
        let reservation = browser.reservation;
        if let Err(error) = reservation.resize(2 * crate::media::MAX_METADATA) {
            self.preparations.complete(id, Err(error.clone()));
            return Err(error);
        }
        let id = id.to_string();
        let runtime = self.clone();
        std::thread::spawn(move || {
            let _recording_lease = recording_lease;
            let weak = Arc::downgrade(&runtime);
            let progress_id = id.clone();
            let control = crate::media::PreparationControl {
                cancelled: cancel.clone(),
                progress: Arc::new(move |frames| {
                    if let Some(runtime) = weak.upgrade() {
                        if let Some((job, _)) = runtime
                            .preparations
                            .jobs
                            .lock()
                            .unwrap()
                            .get_mut(&progress_id)
                        {
                            job.frames = frames;
                        }
                    }
                }),
            };
            let result = (|| {
                let clip = if info.recording.source_format == "gif" {
                    let source = runtime.recording_gif(
                        &info.recording.id,
                        info.options.detail_bounds()[1],
                        &control,
                    )?;
                    let _decode = runtime
                        .profiles
                        .media_gate
                        .lock_cancel(&control.cancelled)?;
                    reservation.resize(crate::media_budget::CLIP_WORK_BYTES)?;
                    source.section(info.start_us as u32, info.end_us as u32, info, &control)?
                } else {
                    let _decode = runtime
                        .profiles
                        .media_gate
                        .lock_cancel(&control.cancelled)?;
                    reservation.resize(crate::media_budget::CLIP_WORK_BYTES)?;
                    control.check()?;
                    let mut clip = crate::media::mp4_file(
                        &path,
                        Some((info.start_us, info.end_us)),
                        &control,
                        info.options.detail_bounds()[1],
                        info.options
                            .maximum_fps()
                            .filter(|rate| *rate < info.recording.source_fps),
                    )?;
                    clip.clip_info = Some(info);
                    clip
                };
                control.check()?;
                let clip = Arc::new(clip);
                reservation.track_clip(&clip, crate::media_budget::PUBLICATION_WORK_BYTES)?;
                let (asset, lease) =
                    runtime
                        .profiles
                        .publish_prepared_clip_shared(clip, &[], &control)?;
                control.check()?;
                Ok((asset, lease))
            })();
            runtime.preparations.complete_asset(&id, result);
        });
        Ok(())
    }
}

impl crate::Runtime {
    // Prepare derived video
    pub fn prepare_derived_video(
        self: &Arc<Self>,
        asset: String,
        draft: crate::video::ClipDraft,
        owner: String,
        generation: u64,
    ) -> Result<String, String> {
        let lease = self
            .profiles
            .asset_leases
            .pin([asset.clone()], "Derived video clip")?;
        let previous = self
            .profiles
            .clip(&asset)?
            .ok_or("Prepared clip unavailable")?;
        let mut info = previous
            .clip_info
            .clone()
            .ok_or("Unsupported clip provenance; reimport from the original recording")?;
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&**previous.frames[0]))
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .into_dimensions()
            .map_err(|e| e.to_string())?;
        if draft.start_us < info.start_us
            || draft.end_us > info.end_us
            || draft.end_us <= draft.start_us
            || (draft.options.detail_bounds()[1] > info.options.detail_bounds()[1]
                && draft.options.detail_bounds()[1] > width.max(height) as u16)
            || draft
                .options
                .maximum_fps()
                .unwrap_or(info.recording.source_fps)
                > previous.prepared_fps() + 0.02
        {
            return Err(
                "Import the original recording to expand the range, FPS or prepared detail".into(),
            );
        }
        let start = (draft.start_us - info.start_us) as u32;
        let end = (draft.end_us - info.start_us) as u32;
        info.start_us = draft.start_us;
        info.end_us = draft.end_us;
        info.name = draft.name;
        info.revision = info.revision.saturating_add(1);
        info.options = draft.options;
        info.validate()?;
        let (id, cancelled) = self.preparations.enqueue(owner, generation)?;
        let runtime = self.clone();
        let result_id = id.clone();
        std::thread::spawn(move || {
            let _lease = lease;
            let result = (|| {
                let control = crate::media::PreparationControl {
                    cancelled,
                    progress: Arc::new(|_| {}),
                };
                let (reservation, clip) = {
                    let _decode = runtime
                        .profiles
                        .media_gate
                        .lock_cancel(&control.cancelled)?;
                    let reservation = runtime
                        .profiles
                        .media_budget
                        .reserve(crate::media_budget::CLIP_WORK_BYTES)?;
                    let clip = Arc::new(previous.section(start, end, info, &control)?);
                    (reservation, clip)
                };
                reservation.track_clip(&clip, crate::media_budget::PUBLICATION_WORK_BYTES)?;
                let (asset, lease) =
                    runtime
                        .profiles
                        .publish_prepared_clip_shared(clip, &[], &control)?;
                Ok((asset, lease))
            })();
            runtime.preparations.complete_asset(&id, result);
        });
        Ok(result_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Create a browser-preparation fixture from a staged minimal video import
    fn browser_job(runtime: &Arc<crate::Runtime>, root: &std::path::Path, key: &str) -> String {
        let path = root.join(format!("{key}.mp4"));
        let mut bytes = vec![0, 0, 0, 24, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm'];
        bytes.resize(4096, 7);
        std::fs::write(&path, bytes).unwrap();
        let import = runtime
            .start_video_import(path, format!("import-{key}"), 1)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut recording = loop {
            let job = runtime.video_import_status(&import).unwrap();
            if let Some(recording) = job.recording {
                break recording;
            }
            assert_ne!(job.state, "failed", "{:?}", job.error);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        };
        recording.width = 128;
        recording.height = 72;
        recording.duration_us = 1_000_000;
        recording.source_fps = 24.0;
        let recording = runtime.save_recording_metadata(recording).unwrap();
        runtime
            .prepare_recording_clip(
                crate::video::ClipDraft {
                    recording_id: recording.id,
                    name: "Current clip".into(),
                    start_us: 0,
                    end_us: 42_000,
                    base_asset: None,
                    options: Default::default(),
                },
                key.into(),
                1,
            )
            .unwrap()
    }

    #[test]
    // Verify that terminal jobs reject late results and invalid transitions
    fn terminal_jobs_reject_late_results_and_invalid_transitions() {
        let jobs = Preparations::default();
        let (id, _) = jobs.enqueue("editor".into(), 1).unwrap();
        jobs.complete(&id, Ok("first".into()));
        jobs.complete(&id, Err("late error".into()));
        let leases = crate::asset_leases::AssetLeases::default();
        let asset = uuid::Uuid::new_v4().to_string();
        jobs.complete_asset(
            &id,
            Ok((asset.clone(), leases.pin([asset], "late").unwrap())),
        );
        assert!(leases.lock().live.is_empty());
        let mut entries = jobs.jobs.lock().unwrap();
        let job = &mut entries.get_mut(&id).unwrap().0;
        assert_eq!(job.asset.as_deref(), Some("first"));
        assert!(job.error.is_none());
        for next in [
            PreparationState::Preparing,
            PreparationState::BrowserPending,
            PreparationState::BrowserPreparing,
            PreparationState::Publishing,
        ] {
            assert!(job.transition(next).is_err());
        }
    }

    #[test]
    // Verify that cancellation during publication keeps owner until result retires
    fn cancellation_during_publication_keeps_owner_until_result_retires() {
        let jobs = Arc::new(Preparations::default());
        let (id, cancel) = jobs.enqueue("editor".into(), 1).unwrap();
        {
            let mut entries = jobs.jobs.lock().unwrap();
            let job = &mut entries.get_mut(&id).unwrap().0;
            job.transition(PreparationState::BrowserPending).unwrap();
            job.transition(PreparationState::Publishing).unwrap();
            assert_eq!(job.snapshot().state, "browser_preparing");
        }
        let worker = jobs.clone();
        let worker_id = id.clone();
        let leases = crate::asset_leases::AssetLeases::default();
        let asset = uuid::Uuid::new_v4().to_string();
        let lease = leases.pin([asset.clone()], "publishing").unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            wait.recv().unwrap();
            worker.complete_asset(&worker_id, Ok((asset, lease)));
        });
        cancel.store(true, Ordering::SeqCst);
        jobs.jobs.lock().unwrap().get_mut(&id).unwrap().0.cancel();
        assert!(jobs.editor_retirement_blocked());
        assert_eq!(leases.lock().live.len(), 1);
        release.send(()).unwrap();
        thread.join().unwrap();
        assert!(!jobs.editor_retirement_blocked());
        assert!(leases.lock().live.is_empty());
        assert_eq!(
            jobs.jobs.lock().unwrap()[&id].0.state,
            PreparationState::Cancelled
        );
    }

    #[test]
    // Verify that editor retirement waits for cancelled workers and unclaimed results
    fn editor_retirement_waits_for_cancelled_workers_and_unclaimed_results() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let (id, _) = runtime.preparations.enqueue("native".into(), 1).unwrap();
        assert!(runtime.preparations.editor_retirement_blocked());
        runtime.cancel_media_preparation(&id).unwrap();
        assert!(runtime.preparations.editor_retirement_blocked());
        runtime.preparations.complete(&id, Err("cancelled".into()));
        assert!(!runtime.preparations.editor_retirement_blocked());
        let (id, _) = runtime.preparations.enqueue("native".into(), 2).unwrap();
        let asset = uuid::Uuid::new_v4().to_string();
        let lease = runtime
            .profiles
            .asset_leases
            .pin([asset.clone()], "completed preparation")
            .unwrap();
        runtime.preparations.complete_asset(&id, Ok((asset, lease)));
        assert!(runtime.preparations.editor_retirement_blocked());
        runtime.finish_media_preparation(&id).unwrap();
        assert!(!runtime.preparations.editor_retirement_blocked());
        runtime.shutdown();
    }

    #[test]
    // Verify that native result leases follow publication cancellation and finish
    fn native_result_leases_follow_publication_cancellation_and_finish() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let (id, _) = runtime.preparations.enqueue("native".into(), 1).unwrap();
        let leases = crate::asset_leases::AssetLeases::default();
        let asset = uuid::Uuid::new_v4().to_string();
        let lease = leases.pin([asset.clone()], "completed native job").unwrap();
        runtime.cancel_media_preparation(&id).unwrap();
        runtime.preparations.complete_asset(&id, Ok((asset, lease)));
        assert_eq!(runtime.media_preparation(&id).unwrap().state, "cancelled");
        assert_eq!(
            runtime.preparations.memory_stats()["staged_asset_leases"],
            0
        );
        assert!(leases.lock().live.is_empty());
        let (next, _) = runtime.preparations.enqueue("native".into(), 2).unwrap();
        let asset = uuid::Uuid::new_v4().to_string();
        let lease = leases.pin([asset.clone()], "staged result").unwrap();
        runtime
            .preparations
            .complete_asset(&next, Ok((asset.clone(), lease)));
        assert_eq!(runtime.media_preparation(&next).unwrap().asset, Some(asset));
        assert_eq!(
            runtime.preparations.memory_stats()["staged_asset_leases"],
            1
        );
        assert_eq!(leases.lock().live.len(), 1);
        runtime.finish_media_preparation(&next).unwrap();
        assert_eq!(
            runtime.preparations.memory_stats()["staged_asset_leases"],
            0
        );
        assert!(leases.lock().live.is_empty());
        runtime.shutdown();
    }

    #[test]
    // Verify that validated browser batch cannot commit after cancellation and expired jobs release buffers
    fn validated_browser_batch_cannot_commit_after_cancellation_and_expired_jobs_release_buffers() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let id = browser_job(&runtime, root.path(), "first");
        // Model a batch that finished expensive validation before cancellation
        // acquired jobs, but reaches commit while browser cleanup is pending
        runtime
            .preparations
            .jobs
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .1
            .store(true, Ordering::SeqCst);
        let validation = runtime.profiles.media_budget.reserve(128).unwrap();
        assert!(runtime
            .commit_browser_batch(
                &id,
                0,
                false,
                vec![(40, vec![1; 128])],
                vec![128],
                Some(&validation)
            )
            .is_err());
        assert_eq!(runtime.profiles.media_budget.stats()["reservations"], 2);
        drop(validation);
        assert_eq!(runtime.profiles.media_budget.stats()["reservations"], 1);
        assert_eq!(runtime.preparations.browser.lock().unwrap()[&id].bytes, 0);
        let second = browser_job(&runtime, root.path(), "second");
        assert!(!runtime
            .preparations
            .browser
            .lock()
            .unwrap()
            .contains_key(&id));
        runtime
            .preparations
            .browser
            .lock()
            .unwrap()
            .get_mut(&second)
            .unwrap()
            .started = std::time::Instant::now() - std::time::Duration::from_secs(121);
        let third = browser_job(&runtime, root.path(), "third");
        assert!(!runtime
            .preparations
            .browser
            .lock()
            .unwrap()
            .contains_key(&second));
        assert_eq!(
            runtime.media_preparation(&second).unwrap().state,
            "cancelled"
        );
        runtime.cancel_media_preparation(&third).unwrap();
        assert_eq!(
            runtime.profiles.media_budget.stats()["reserved_preparation_bytes"],
            0
        );
        assert_eq!(runtime.profiles.media_budget.stats()["reservations"], 0);
        assert_eq!(
            runtime.preparations.memory_stats()["pending_buffer_capacity_bytes"],
            0
        );
        runtime.shutdown();
    }

    #[test]
    // Verify that retired rendering identifiers and acknowledgements are rejected
    fn retired_rendering_identifiers_and_acknowledgements_are_rejected() {
        for mode in ["legacy", "device_frame", "unknown"] {
            assert!(
                serde_json::from_value::<hardware_core::OutputMode>(serde_json::json!(mode))
                    .is_err()
            );
        }
        let mut value = serde_json::to_value(crate::workspace::DeviceFramePolicy {
            policy: hardware_core::OutputPolicy::default(),
            encoding: hardware_core::FrameEncoding::IndividualUpdates,
        })
        .unwrap();
        value["acknowledge_unqualified"] = serde_json::json!(true);
        assert!(serde_json::from_value::<crate::workspace::DeviceFramePolicy>(value).is_err());
    }

    #[test]
    #[ignore = "Requires isolated plugin-host loopback sockets; only project-owned media is decoded"]
    // Verify that browser batches validate before mutation and publication retains ownership
    fn browser_batches_validate_before_mutation_and_publication_retains_ownership() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("state"), false).unwrap();
        let create = |key: &str| browser_job(&runtime, root.path(), key);
        let first = create("first");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(128, 72)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let mut batch = 1u32.to_le_bytes().to_vec();
        batch.extend(42_000u32.to_le_bytes());
        batch.extend((png.len() as u32).to_le_bytes());
        batch.extend(png);
        let used = runtime.profiles.media_budget.stats()["combined_bytes"]
            .as_u64()
            .unwrap() as usize;
        let blocked = runtime
            .profiles
            .media_budget
            .reserve(
                runtime.profiles.media_budget.stats()["preparation_admission_limit_bytes"]
                    .as_u64()
                    .unwrap() as usize
                    - used,
            )
            .unwrap();
        assert!(runtime
            .append_browser_media_timed(&first, 0, &batch, true)
            .is_err());
        {
            let jobs = runtime.preparations.browser.lock().unwrap();
            let job = &jobs[&first];
            assert_eq!(job.sequence, 0);
            assert_eq!(job.precise, Some(true));
            assert_eq!(job.bytes, 0);
            assert!(job.frames.is_empty());
            assert!(!job.in_flight.load(Ordering::Acquire));
        }
        drop(blocked);
        assert!(runtime
            .append_browser_media_timed(&first, 0, &batch[..batch.len() - 1], true)
            .is_err());
        assert_eq!(runtime.media_preparation(&first).unwrap().frames, 0);
        assert!(runtime
            .append_browser_media_timed(&first, 1, &batch, true)
            .is_err());
        runtime
            .append_browser_media_timed(&first, 0, &batch, true)
            .unwrap();
        assert!(runtime
            .append_browser_media_timed(&first, 0, &batch, true)
            .is_err());
        assert_eq!(runtime.media_preparation(&first).unwrap().frames, 1);
        let used = runtime.profiles.media_budget.stats()["combined_bytes"]
            .as_u64()
            .unwrap() as usize;
        let blocked = runtime
            .profiles
            .media_budget
            .reserve(
                runtime.profiles.media_budget.stats()["preparation_admission_limit_bytes"]
                    .as_u64()
                    .unwrap() as usize
                    - used,
            )
            .unwrap();
        assert!(runtime.publish_browser_media(&first).is_err());
        assert_eq!(
            runtime.preparations.browser.lock().unwrap()[&first].sequence,
            1
        );
        assert_eq!(
            runtime.preparations.browser.lock().unwrap()[&first]
                .frames
                .len(),
            1
        );
        drop(blocked);
        runtime.publish_browser_media(&first).unwrap();
        assert_eq!(runtime.profiles.media_budget.stats()["reservations"], 0);
        let first_asset = runtime.media_preparation(&first).unwrap().asset.unwrap();
        assert!(runtime
            .append_browser_media_timed(&first, 1, &batch, true)
            .is_err());
        let second = create("second");
        runtime
            .append_browser_media_timed(&second, 0, &batch, true)
            .unwrap();
        runtime.publish_browser_media(&second).unwrap();
        let second_asset = runtime.media_preparation(&second).unwrap().asset.unwrap();
        assert_ne!(first_asset, second_asset);
        assert_eq!(
            runtime
                .profiles
                .clip(&first_asset)
                .unwrap()
                .unwrap()
                .frames
                .len(),
            1
        );
        let cancelled = create("cancelled");
        runtime.cancel_media_preparation(&cancelled).unwrap();
        assert!(runtime
            .append_browser_media_timed(&cancelled, 0, &batch, true)
            .is_err());
        assert!(runtime.publish_browser_media(&cancelled).is_err());
        assert!(runtime.profiles.clip(&first_asset).unwrap().is_some());
        runtime.shutdown();
    }

    #[test]
    // Verify that surfaces skip unchanged pixels across cadence and speed edits
    fn surfaces_skip_unchanged_pixels_across_cadence_and_speed_edits() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("state"), false).unwrap();
        let state = runtime.simulate_device(true, "simulator:cache").unwrap();
        let source = root.path().join("colors.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let asset = runtime.import_asset(&source).unwrap();
        let device = state
            .devices
            .iter()
            .find(|d| d.id == "simulator:cache")
            .unwrap();
        let control = device
            .manifest
            .controls
            .iter()
            .find(|c| c.id == "key-1")
            .unwrap();
        let mut binding = state.profiles[0].pages[0].bindings["key-1"].clone();
        binding.asset = Some(asset);
        let mut context = runtime.render_scene(&device.id).unwrap();
        context.media_elapsed = std::time::Duration::ZERO;
        let render = |binding: &crate::Binding| {
            runtime
                .render_bound_surface_in_scene(
                    control,
                    Some(binding),
                    0,
                    &context.scope,
                    "key-1",
                    &context,
                )
                .unwrap()
        };
        let first = render(&binding);
        let count = || {
            runtime
                .metrics
                .snapshot()
                .into_iter()
                .find(|m| m.name == "surface_render_invocations")
                .unwrap()
                .count
        };
        let initial = count();
        for fps in [5, 10, 15, 20, 24] {
            binding.media.max_fps = Some(f64::from(fps));
            binding.media.speed = 2.0;
            assert_eq!(render(&binding), first);
        }
        assert_eq!(
            count(),
            initial,
            "same source frame must not be composed again"
        );
        binding.title = "Changed title".into();
        assert_ne!(render(&binding), first);
        assert_eq!(count(), initial + 1);
        binding.media.trim_start_ms = u32::MAX;
        assert!(runtime
            .render_bound_surface_in_scene(
                control,
                Some(&binding),
                0,
                &context.scope,
                "key-1",
                &context
            )
            .is_err());
        runtime.shutdown();
    }

    #[test]
    // Verify that newer generations cancel prior results and reject stale requests
    fn newer_generations_cancel_prior_results_and_reject_stale_requests() {
        let preparations = Preparations::default();
        let (old, old_cancelled) = preparations.enqueue("editor".into(), 1).unwrap();
        let (current, current_cancelled) = preparations.enqueue("editor".into(), 2).unwrap();
        assert!(old_cancelled.load(Ordering::SeqCst));
        assert!(!current_cancelled.load(Ordering::SeqCst));
        for generation in [1, 2] {
            assert_eq!(
                preparations
                    .enqueue("editor".into(), generation)
                    .unwrap_err(),
                "Stale media preparation generation"
            );
        }
        assert!(!current_cancelled.load(Ordering::SeqCst));
        assert_eq!(preparations.jobs.lock().unwrap().len(), 2);

        // A late successful decoder result from the superseded request cannot become usable
        preparations.complete(&current, Ok("current-asset".into()));
        preparations.complete(&old, Ok("old-asset".into()));
        let jobs = preparations.jobs.lock().unwrap();
        assert_eq!(jobs[&old].0.state, PreparationState::Cancelled);
        assert_eq!(jobs[&old].0.asset, None);
        assert_eq!(jobs[&current].0.state, PreparationState::Complete);
        assert_eq!(jobs[&current].0.asset.as_deref(), Some("current-asset"));
    }

    #[test]
    // Verify that active queue stays bounded until cancelled work finishes
    fn active_queue_stays_bounded_until_cancelled_work_finishes() {
        let preparations = Preparations::default();
        let mut admitted = vec![];
        for index in 0..34 {
            admitted.push(preparations.enqueue(format!("editor-{index}"), 1).unwrap());
        }
        // Running and queued preparations share the same admission limit
        {
            let mut jobs = preparations.jobs.lock().unwrap();
            for (id, _) in &admitted[..2] {
                jobs.get_mut(id)
                    .unwrap()
                    .0
                    .transition(PreparationState::Preparing)
                    .unwrap();
            }
        }
        assert_eq!(
            preparations.enqueue("overflow".into(), 1).unwrap_err(),
            "Media preparation queue is full"
        );
        preparations.cancel_all();
        assert!(admitted
            .iter()
            .all(|(_, cancelled)| cancelled.load(Ordering::SeqCst)));
        assert_eq!(
            preparations.enqueue("overflow".into(), 2).unwrap_err(),
            "Media preparation queue is full"
        );

        preparations.complete(&admitted[0].0, Err("decoder cancelled".into()));
        let (replacement, cancelled) = preparations.enqueue("replacement".into(), 1).unwrap();
        assert!(!cancelled.load(Ordering::SeqCst));
        let jobs = preparations.jobs.lock().unwrap();
        assert_eq!(jobs[&admitted[0].0].0.state, PreparationState::Cancelled);
        assert_eq!(jobs[&admitted[0].0].0.error, None);
        assert_eq!(jobs[&replacement].0.state, PreparationState::Queued);
        assert_eq!(
            jobs.values().filter(|(job, _)| job.state.native()).count(),
            34
        );
    }

    #[test]
    // Verify that retained results are bounded without evicting active work
    fn retained_results_are_bounded_without_evicting_active_work() {
        let preparations = Preparations::default();
        let (running, running_cancelled) = preparations.enqueue("running".into(), 1).unwrap();
        let (queued, queued_cancelled) = preparations.enqueue("queued".into(), 1).unwrap();
        {
            let mut jobs = preparations.jobs.lock().unwrap();
            jobs.get_mut(&running)
                .unwrap()
                .0
                .transition(PreparationState::Preparing)
                .unwrap();
        }
        for index in 0..100 {
            let (id, _) = preparations
                .enqueue(format!("finished-{index}"), 1)
                .unwrap();
            preparations.complete(&id, Err("unsupported media".into()));
            let jobs = preparations.jobs.lock().unwrap();
            assert_eq!(jobs[&id].0.state, PreparationState::Failed);
            assert_eq!(jobs[&id].0.error.as_deref(), Some("unsupported media"));
            assert_eq!(jobs.len(), (index + 3).min(64));
            assert_eq!(jobs[&running].0.state, PreparationState::Preparing);
            assert_eq!(jobs[&queued].0.state, PreparationState::Queued);
        }
        assert!(!running_cancelled.load(Ordering::SeqCst));
        assert!(!queued_cancelled.load(Ordering::SeqCst));
    }
}
