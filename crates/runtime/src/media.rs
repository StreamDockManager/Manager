//! Bounded, offline media preparation. Playback never depends on an open webview
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Cursor, Read, Seek, Write},
    path::PathBuf,
    process::Stdio,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

pub(crate) const ASSET_EXTENSIONS: [&str; 3] = ["png", "svg", "sdmedia"];
/// Display-only bounds: preserve the hardware aspect ratio and never upscale
/// Device rendering and original assets do not use this helper
pub fn display_preview_size(size: [u16; 2], bounds: Option<[u16; 2]>) -> Result<[u16; 2], String> {
    let Some(bounds) = bounds else {
        return Ok(size);
    };
    if bounds.iter().any(|edge| !(1..=1280).contains(edge)) {
        return Err("Preview display bounds are outside their limits".into());
    }
    let scale = (f64::from(bounds[0]) / f64::from(size[0].max(1)))
        .min(f64::from(bounds[1]) / f64::from(size[1].max(1)))
        .min(1.0);
    Ok(size.map(|edge| (f64::from(edge) * scale).round().max(1.0) as u16))
}

#[test]
// Verify that display preview bounds preserve aspect and device sizes
fn display_preview_bounds_preserve_aspect_and_device_sizes() {
    assert_eq!(
        display_preview_size([1024, 600], Some([640, 180])).unwrap(),
        [307, 180]
    );
    assert_eq!(
        display_preview_size([1024, 600], Some([1280, 360])).unwrap(),
        [614, 360]
    );
    assert_eq!(
        display_preview_size([72, 72], Some([640, 180])).unwrap(),
        [72, 72]
    );
    assert_eq!(
        display_preview_size([1024, 600], None).unwrap(),
        [1024, 600]
    );
    assert!(display_preview_size([72, 72], Some([0, 180])).is_err());
    assert!(display_preview_size([72, 72], Some([1281, 180])).is_err());
}

pub const MAX_SOURCE: usize = 16 * 1024 * 1024;
pub const MAX_ASSET: usize = 96 * 1024 * 1024;
pub const MAX_FRAMES: usize = 8192;
pub const MAX_METADATA: usize = 1024 * 1024;
const MAX_DURATION_MS: u32 = 30_000;
const MAX_FRAME: usize = 2 * 1024 * 1024;
pub const MAX_CLIP: usize = 64 * 1024 * 1024;
const MAX_CACHE: usize = 64 * 1024 * 1024;
const DECODE_TIMEOUT: Duration = Duration::from_secs(120);

const MAX_SHARED_FRAMES: usize = 32768;
/// Keep the encoded payload outside the reference-counted allocation. Weak cache
/// entries retain only the small Box/control block after the last renderer drops
/// its strong reference, rather than retaining the entire encoded image
pub type SharedBytes = Arc<Box<[u8]>>;
// Transfer an owned byte vector into immutable shared storage
pub(crate) fn shared_bytes(bytes: Vec<u8>) -> SharedBytes {
    Arc::new(bytes.into_boxed_slice())
}
pub(crate) type WeakBytes = std::sync::Weak<Box<[u8]>>;
type FrameDigest = [u8; 32];
type FramePool = std::collections::HashMap<FrameDigest, WeakBytes>;
static SHARED_FRAMES: std::sync::OnceLock<std::sync::Mutex<FramePool>> = std::sync::OnceLock::new();

pub(crate) fn prune_shared_frames() {
    if let Some(pool) = SHARED_FRAMES.get() {
        let mut pool = pool.lock().unwrap();
        pool.retain(|_, weak| weak.strong_count() > 0);
        if pool.capacity() > 256 && pool.len() < pool.capacity() / 4 {
            pool.shrink_to_fit();
        }
    }
}

// Deduplicate frame allocations by content digest while bounding the weak-reference pool
fn share_frames(
    frames: Vec<Vec<u8>>,
    pool: &mut FramePool,
) -> (Vec<SharedBytes>, Vec<FrameDigest>) {
    use sha2::{Digest, Sha256};
    // Sweep at the admission boundary; the pool can never grow beyond it
    if pool.len() >= MAX_SHARED_FRAMES {
        pool.retain(|_, value| value.strong_count() > 0);
    }
    let mut digests = Vec::with_capacity(frames.len());
    let frames = frames
        .into_iter()
        .map(|bytes| {
            let hash: FrameDigest = Sha256::digest(&bytes).into();
            digests.push(hash);
            if let Some(frame) = pool.get(&hash).and_then(std::sync::Weak::upgrade) {
                return frame;
            }
            let frame = Arc::new(bytes.into_boxed_slice());
            if pool.len() < MAX_SHARED_FRAMES {
                pool.insert(hash, Arc::downgrade(&frame));
            }
            frame
        })
        .collect();
    (frames, digests)
}

// Intern frames through the process-wide shared frame pool
fn intern_frames(frames: Vec<Vec<u8>>) -> (Vec<SharedBytes>, Vec<FrameDigest>) {
    share_frames(
        frames,
        &mut SHARED_FRAMES.get_or_init(Default::default).lock().unwrap(),
    )
}

/// Advance the original clock phase beyond now, skipping overdue opportunities
pub fn advance_deadline(deadline: Instant, now: Instant, period: Duration) -> Instant {
    let steps = now.saturating_duration_since(deadline).as_nanos() / period.as_nanos().max(1) + 1;
    deadline + period.mul_f64(steps as f64)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MediaOptions {
    #[serde(skip_serializing_if = "auto_encoding")]
    #[ts(optional, as = "Option<String>")]
    pub encoding: String,
    pub fit: String,
    pub crop: [f64; 4],
    pub trim_start_ms: u32,
    pub trim_end_ms: Option<u32>,
    pub speed: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min_fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub jpeg_quality: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min_frame_edge: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_frame_edge: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub letterbox: Option<String>,
}
// Report the auto encoding condition for media
fn auto_encoding(value: &str) -> bool {
    value == "auto"
}
impl Default for MediaOptions {
    // Construct the default media value
    fn default() -> Self {
        Self {
            encoding: "auto".into(),
            fit: "fill".into(),
            crop: [0.0, 0.0, 1.0, 1.0],
            trim_start_ms: 0,
            trim_end_ms: None,
            speed: 1.0,
            min_fps: None,
            max_fps: None,
            jpeg_quality: None,
            min_frame_edge: None,
            max_frame_edge: None,
            letterbox: None,
        }
    }
}
impl MediaOptions {
    // Read the optional playback frame-rate cap
    pub fn maximum_fps(&self) -> Option<f64> {
        self.max_fps
    }
    // Combine source cadence, playback speed and the optional frame-rate cap
    pub fn requested_fps(&self, clip: &Clip) -> f64 {
        let source = clip.prepared_fps() * self.speed;
        self.maximum_fps()
            .map_or(source, |cap| source.min(cap))
            .max(0.001)
    }
    // Choose explicit JPEG quality or the video/static-image default
    pub fn quality(&self, video: bool) -> u8 {
        self.jpeg_quality.unwrap_or(if video { 75 } else { 90 })
    }
    // Resolve the minimum and maximum prepared frame edges from optional settings
    pub fn detail_bounds(&self) -> [u16; 2] {
        [
            self.min_frame_edge.unwrap_or(64),
            self.max_frame_edge.unwrap_or(512),
        ]
    }
    /// Only options affecting pixels belong in transformed-frame cache identities
    pub fn pixel_key(&self) -> String {
        serde_json::to_string(&(
            &self.fit,
            self.crop,
            self.min_frame_edge,
            self.max_frame_edge,
            &self.letterbox,
        ))
        .expect("finite validated crop")
    }
    // Validate the supplied media value
    pub fn validate(&self) -> Result<(), String> {
        let [x, y, w, h] = self.crop;
        if !["auto", "png", "jpeg"].contains(&self.encoding.as_str())
            || !["fit", "fill"].contains(&self.fit.as_str())
            || self
                .maximum_fps()
                .is_some_and(|fps| !fps.is_finite() || fps <= 0.0 || fps > 1_000_000.0)
            || self.min_fps.is_some_and(|fps| {
                !fps.is_finite() || fps <= 0.0 || self.maximum_fps().is_some_and(|max| fps > max)
            })
            || self.jpeg_quality.is_some_and(|q| !(1..=100).contains(&q))
            || self
                .detail_bounds()
                .iter()
                .any(|edge| !(64..=1024).contains(edge))
            || self.detail_bounds()[0] > self.detail_bounds()[1]
            || self
                .letterbox
                .as_deref()
                .is_some_and(|value| !["black", "transparent"].contains(&value))
            || !self.speed.is_finite()
            || !(0.25..=4.0).contains(&self.speed)
            || self.crop.iter().any(|v| !v.is_finite())
            || x < 0.0
            || y < 0.0
            || w <= 0.0
            || h <= 0.0
            || x + w > 1.000001
            || y + h > 1.000001
            || self
                .trim_end_ms
                .is_some_and(|end| end <= self.trim_start_ms || end > MAX_DURATION_MS)
            || self.trim_start_ms >= MAX_DURATION_MS
        {
            return Err("Invalid media crop, trim, fit or playback speed".into());
        }
        Ok(())
    }
    // Compute time until the next source-frame boundary at the selected playback speed
    pub fn next_frame_delay(&self, clip: &Clip, elapsed: Duration) -> Duration {
        let (tick, end) = self.source_position(clip, elapsed);
        let index = clip.ends_us.partition_point(|value| *value <= tick);
        let next = clip.ends_us.get(index).copied().unwrap_or(end).min(end);
        Duration::from_secs_f64(
            (f64::from(next.saturating_sub(tick)) / (1_000_000.0 * self.speed)).max(0.000_001),
        )
    }
    // Select the current frame after applying playback trimming and speed
    pub fn frame_index(&self, clip: &Clip, elapsed: Duration) -> usize {
        let (tick, _) = self.source_position(clip, elapsed);
        clip.ends_us
            .partition_point(|value| *value <= tick)
            .min(clip.frames.len() - 1)
    }
    /// Age of the selected source presentation in wall-clock time. The first
    /// visible frame in a trimmed loop begins at the trim point inside that frame.
    pub(crate) fn frame_age(&self, clip: &Clip, index: usize, elapsed: Duration) -> Duration {
        let (tick, _) = self.source_position(clip, elapsed);
        let start = index
            .checked_sub(1)
            .map_or(0, |i| clip.ends_us[i])
            .max(self.trim_start_ms.saturating_mul(1000));
        Duration::from_secs_f64(f64::from(tick.saturating_sub(start)) / (1_000_000. * self.speed))
    }
    // Map elapsed playback time into the trimmed source timeline
    fn source_position(&self, clip: &Clip, elapsed: Duration) -> (u32, u32) {
        let start = self
            .trim_start_ms
            .saturating_mul(1000)
            .min(clip.duration_us().saturating_sub(1));
        let end = self
            .trim_end_ms
            .map(|ms| ms.saturating_mul(1000))
            .unwrap_or(clip.duration_us())
            .min(clip.duration_us())
            .max(start + 1);
        let tick = start
            + ((elapsed.as_secs_f64() * 1_000_000.0 * self.speed).round() as u64
                % u64::from(end - start)) as u32;
        (tick, end)
    }
}

/// Fixed-size identity shared by the lookup and eviction indexes for a rendered output.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) struct RenderKey([u8; 32]);

impl RenderKey {
    pub(crate) fn from_inputs(inputs: &impl Serialize) -> Result<Self, String> {
        stream_dock_manager_storage::json_digest::sha256_bytes(inputs).map(Self)
    }

    pub(crate) fn from_name(name: &str) -> Self {
        use sha2::{Digest, Sha256};
        Self(Sha256::digest(name.as_bytes()).into())
    }
}

// Each entry stores a digest in both the lookup table and the eviction index.
const RENDER_KEY_BYTES: usize = 2 * size_of::<RenderKey>();

/// Encoded allocations are shared internally. Evicted buffers remain charged while a
/// renderer holds them; admission fails rather than allowing unbounded live references
#[derive(Default)]
pub struct RenderCache {
    // Latest-only surfaces use their stable surface key for eviction and verify the content
    // digest on lookup. Both indexes are retired together under the render-cache lock.
    latest: std::collections::HashMap<RenderKey, RenderKey>,
    entries: std::collections::HashMap<
        RenderKey,
        (Arc<crate::media_budget::Slot>, u64, usize, WeakBytes),
    >,
    budget: Arc<crate::media_budget::MediaBudget>,
    resident: std::collections::HashMap<usize, (WeakBytes, usize)>,
    order: std::collections::BTreeMap<u64, RenderKey>,
    retired: Vec<(WeakBytes, usize)>,
    clock: u64,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub copied_bytes: u64,
    pub rejected: u64,
}
impl RenderCache {
    // Construct using budget
    pub(crate) fn with_budget(budget: Arc<crate::media_budget::MediaBudget>) -> Self {
        Self {
            budget,
            ..Default::default()
        }
    }
    // Prune entries
    fn prune_entries(&mut self) {
        let expired: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, (slot, _, _, _))| !slot.live())
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.remove_key(key);
        }
    }
    // Collect memory
    pub(crate) fn collect_memory(&mut self, memory: &mut crate::media_memory::MediaMemory) {
        self.resident_bytes();
        for (slot, _, _, _) in self.entries.values() {
            if let Some(frame) = slot.peek_frame() {
                memory.cached_frame(&frame);
            }
        }
        for (frame, bytes) in self.resident.values() {
            memory.live_frame(frame, *bytes);
        }
    }
    // Return shared
    pub fn get_shared(&mut self, key: &str) -> Option<SharedBytes> {
        self.get_key(RenderKey::from_name(key), true)
    }
    // Return probe shared
    pub(crate) fn get_probe_shared(&mut self, key: &str) -> Option<SharedBytes> {
        self.get_key(RenderKey::from_name(key), false)
    }
    // Refresh recency on every hit. A repeated probe can skip the render counters.
    pub(crate) fn get_key(&mut self, key: RenderKey, count_render: bool) -> Option<SharedBytes> {
        // A budget eviction can expire this slot. Admission and memory reports
        // sweep the whole cache; a lookup only needs to check its own entry.
        if let Some((slot, stamp, _, _)) = self.entries.get_mut(&key) {
            let Some(bytes) = slot.frame() else {
                self.remove_key(key);
                if count_render {
                    self.misses += 1;
                }
                return None;
            };
            if count_render {
                self.hits += 1;
            }
            self.order.remove(stamp);
            self.clock += 1;
            *stamp = self.clock;
            self.order.insert(self.clock, key);
            Some(bytes)
        } else {
            if count_render {
                self.misses += 1;
            }
            None
        }
    }
    pub(crate) fn get_render(
        &mut self,
        content: RenderKey,
        slots: &[RenderKey],
    ) -> Option<SharedBytes> {
        if slots.is_empty() {
            return self.get_key(content, true);
        }
        // The preview and the hardware retain independent latest frames. Reuse
        // either one when the complete render inputs match.
        for slot in slots {
            if self.latest.get(slot) == Some(&content) {
                if let Some(bytes) = self.get_key(*slot, false) {
                    self.hits += 1;
                    return Some(bytes);
                }
            }
        }
        self.misses += 1;
        None
    }

    // Look up cached bytes
    fn cached_bytes(&self) -> usize {
        let mut allocations = std::collections::HashSet::new();
        self.latest.len() * RENDER_KEY_BYTES
            + self
                .entries
                .values()
                .map(|(_, _, size, weak)| {
                    RENDER_KEY_BYTES
                        + if allocations.insert(weak.as_ptr()) {
                            *size
                        } else {
                            0
                        }
                })
                .sum::<usize>()
    }
    // Prune expired references and count live render allocations plus retained key storage
    pub fn resident_bytes(&mut self) -> usize {
        self.prune_entries();
        self.retired.retain(|(bytes, _)| bytes.strong_count() > 0);
        self.resident
            .retain(|_, (bytes, _)| bytes.strong_count() > 0);
        (self.entries.len() + self.latest.len()) * RENDER_KEY_BYTES
            + self.resident.values().map(|(_, size)| size).sum::<usize>()
    }
    // Count resident bytes still held outside the cache’s current entry ownership
    pub fn in_flight_bytes(&mut self) -> usize {
        self.resident_bytes() - self.bytes
    }
    // Remove the selected media entry
    pub fn remove(&mut self, key: &str) {
        self.remove_key(RenderKey::from_name(key));
    }

    fn remove_key(&mut self, key: RenderKey) {
        if let Some((slot, stamp, size, weak)) = self.entries.remove(&key) {
            self.latest.remove(&key);
            self.bytes = self.cached_bytes();
            self.order.remove(&stamp);
            slot.evict();
            if weak.strong_count() > 0 {
                self.retired.push((weak, size));
            }
        }
    }
    // Clear retained media state
    pub fn clear(&mut self) {
        while let Some((_, &key)) = self.order.first_key_value() {
            self.remove_key(key);
        }
        self.resident_bytes();
    }
    // Insert an entry into media
    pub fn insert(&mut self, key: String, value: Vec<u8>) {
        let _ = self.insert_shared(key, value);
    }
    // Move owned bytes into shared storage before cache admission
    pub fn insert_shared(&mut self, key: String, value: Vec<u8>) -> Result<SharedBytes, String> {
        self.insert_frame(key, shared_bytes(value))
    }
    pub(crate) fn insert_frame(
        &mut self,
        key: String,
        value: SharedBytes,
    ) -> Result<SharedBytes, String> {
        self.insert_render(RenderKey::from_name(&key), None, value)
    }

    // Admit a shared frame within the memory and entry limits.
    pub(crate) fn insert_render(
        &mut self,
        content: RenderKey,
        surface: Option<RenderKey>,
        value: SharedBytes,
    ) -> Result<SharedBytes, String> {
        let key = surface.unwrap_or(content);
        let key_bytes = if surface.is_some() {
            2 * RENDER_KEY_BYTES
        } else {
            RENDER_KEY_BYTES
        };
        let size = key_bytes.saturating_add(value.len());
        if size > MAX_CACHE {
            self.rejected += 1;
            return Err("Encoded cache item exceeds budget".into());
        }
        self.remove_key(key);
        let additional = key_bytes
            + if self.resident.contains_key(&(Arc::as_ptr(&value) as usize)) {
                0
            } else {
                value.len()
            };
        while self.resident_bytes().saturating_add(additional) > MAX_CACHE
            || self.entries.len() + self.retired.len() >= 4096
        {
            let Some((_, &oldest)) = self.order.first_key_value() else {
                self.rejected += 1;
                return Err(
                    "Encoded cache budget is held by in-flight frames; retry next cycle".into(),
                );
            };
            self.remove_key(oldest);
        }
        let slot = self
            .budget
            .admit(crate::media_budget::Value::Frame(value.clone()))
            .inspect_err(|_| {
                self.rejected += 1;
            })?;
        self.prune_entries();
        let weak = Arc::downgrade(&value);
        self.resident
            .insert(Arc::as_ptr(&value) as usize, (weak.clone(), value.len()));
        self.clock += 1;
        self.order.insert(self.clock, key);
        self.entries
            .insert(key, (slot, self.clock, value.len(), weak));
        if let Some(surface) = surface {
            self.latest.insert(surface, content);
        }
        self.bytes = self.cached_bytes();
        Ok(value)
    }
}

pub struct Clip {
    pub source_format: String,
    pub frames: Vec<SharedBytes>,
    pub frame_digests: Vec<FrameDigest>,
    pub delays_ms: Vec<u32>,
    ends_us: Vec<u32>,
    pub delays_us: Vec<u32>,
    pub preparation_revision: u16,
    pub clip_info: Option<crate::video::ClipInfo>,
    pub duration_ms: u32,
}
impl Clip {
    // Charge allocated timeline, digest and provenance metadata capacities
    pub(crate) fn metadata_capacity_bytes(&self) -> usize {
        let provenance = self.clip_info.as_ref().map_or(0, |info| {
            info.clip_id.capacity()
                + info.name.capacity()
                + info.recording.id.capacity()
                + info.recording.name.capacity()
                + info.recording.sha256.capacity()
                + info.recording.source_format.capacity()
                + info.options.encoding.capacity()
                + info.options.fit.capacity()
                + info.options.letterbox.as_ref().map_or(0, String::capacity)
        });
        size_of::<Self>()
            + self.source_format.capacity()
            + self.frames.capacity() * size_of::<SharedBytes>()
            + self.frame_digests.capacity() * size_of::<FrameDigest>()
            + (self.delays_ms.capacity() + self.ends_us.capacity() + self.delays_us.capacity())
                * size_of::<u32>()
            + provenance
    }
    // Initialize media state from the supplied inputs
    pub(crate) fn new(
        format: &str,
        frames: Vec<Vec<u8>>,
        delays_ms: Vec<u32>,
    ) -> Result<Self, String> {
        Self::with_timing(
            format,
            frames,
            delays_ms
                .into_iter()
                .map(|ms| ms.saturating_mul(1000))
                .collect(),
            2,
            None,
        )
    }
    // Construct using timing
    pub(crate) fn with_timing(
        format: &str,
        frames: Vec<Vec<u8>>,
        delays_us: Vec<u32>,
        preparation_revision: u16,
        clip_info: Option<crate::video::ClipInfo>,
    ) -> Result<Self, String> {
        if frames.is_empty()
            || frames.len() > MAX_FRAMES
            || frames.iter().map(Vec::len).sum::<usize>() > MAX_CLIP
        {
            return Err("Animation exceeds frame or prepared-size limits".into());
        }
        let (frames, frame_digests) = intern_frames(frames);
        Self::with_shared_timing(
            format,
            frames,
            frame_digests,
            delays_us,
            preparation_revision,
            clip_info,
        )
    }
    // Construct using shared timing
    fn with_shared_timing(
        format: &str,
        frames: Vec<SharedBytes>,
        frame_digests: Vec<FrameDigest>,
        delays_us: Vec<u32>,
        preparation_revision: u16,
        clip_info: Option<crate::video::ClipInfo>,
    ) -> Result<Self, String> {
        let bytes: usize = frames.iter().map(|frame| frame.len()).sum();
        if !["gif", "mp4"].contains(&format)
            || frames.is_empty()
            || frames.len() > MAX_FRAMES
            || frames.len() != delays_us.len()
            || bytes > MAX_CLIP
        {
            return Err("Animation exceeds frame or prepared-size limits".into());
        }
        let mut ends_us = Vec::with_capacity(delays_us.len());
        let mut duration = 0u32;
        for delay in &delays_us {
            if *delay == 0 {
                return Err("Animation frame delay must be positive".into());
            }
            duration = duration
                .checked_add(*delay)
                .filter(|sum| *sum <= MAX_DURATION_MS * 1000)
                .ok_or("Animation exceeds 30 seconds")?;
            ends_us.push(duration);
        }
        let mut previous = 0;
        let delays_ms = ends_us
            .iter()
            .map(|end| {
                let rounded = (*end + 500) / 1000;
                let delay = rounded.saturating_sub(previous);
                previous = rounded;
                delay
            })
            .collect();
        let duration_ms = duration.div_ceil(1000);
        Ok(Self {
            source_format: format.into(),
            frames,
            frame_digests,
            delays_ms,
            ends_us,
            delays_us,
            preparation_revision,
            clip_info,
            duration_ms,
        })
    }
    // Select the frame at the wrapped position in the clip’s microsecond timeline
    pub fn frame_index(&self, elapsed: Duration) -> usize {
        let tick = (elapsed.as_micros() % u128::from(self.duration_us())) as u32;
        self.ends_us.partition_point(|end| *end <= tick)
    }
    // Express the measured duration in microseconds
    pub fn duration_us(&self) -> u32 {
        self.ends_us.last().copied().unwrap_or(1)
    }
    // Compute the clip’s fastest frame cadence from its shortest delay
    pub fn peak_fps(&self) -> f64 {
        1_000_000.0 / f64::from(self.delays_us.iter().copied().min().unwrap_or(1).max(1))
    }
    /// A shortened final hold does not create a higher source or preparation cadence
    pub fn prepared_fps(&self) -> f64 {
        self.clip_info.as_ref().map_or(self.peak_fps(), |info| {
            self.peak_fps()
                .min(info.recording.source_fps)
                .min(info.options.maximum_fps().unwrap_or(f64::INFINITY))
        })
    }
    // List frame boundaries beginning with the zero-time boundary
    pub fn boundaries_us(&self) -> Vec<u32> {
        std::iter::once(0)
            .chain(self.ends_us.iter().copied())
            .collect()
    }
    /// Select timestamp holds without decoding an animation again for every seek
    pub(crate) fn section(
        &self,
        start: u32,
        end: u32,
        info: crate::video::ClipInfo,
        control: &PreparationControl,
    ) -> Result<Self, String> {
        if start >= end || end > self.duration_us() {
            return Err("Section is outside the prepared source".into());
        }
        info.validate()?;
        let mut points: Vec<u32> = std::iter::once(start)
            .chain(
                self.boundaries_us()
                    .into_iter()
                    .filter(|time| *time > start && *time < end),
            )
            .collect();
        if let Some(cap) = info.options.maximum_fps() {
            let count = ((end - start) as f64 * cap / 1_000_000.).ceil() as usize;
            if count < points.len() {
                points = (0..count)
                    .map(|i| start + (i as f64 * 1_000_000. / cap).round() as u32)
                    .collect();
            }
        }
        let [minimum, maximum] = info.options.detail_bounds();
        let mut frames = vec![];
        let mut delays = vec![];
        let mut total = 0;
        for (position, left) in points.iter().copied().enumerate() {
            control.check()?;
            (control.progress)(position);
            let index = self.frame_index(Duration::from_micros(u64::from(left)));
            let source = &**self.frames[index];
            let (width, height) = image::ImageReader::new(Cursor::new(source))
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?;
            let frame = if width.max(height) < u32::from(minimum)
                || width.max(height) > u32::from(maximum)
            {
                let pixels = image::load_from_memory(source).map_err(|e| e.to_string())?;
                let edge = width.max(height).clamp(minimum.into(), maximum.into());
                let resized = pixels.resize(edge, edge, image::imageops::FilterType::Triangle);
                let alpha =
                    resized.color().has_alpha() && resized.to_rgba8().pixels().any(|p| p[3] < 255);
                hardware_core::render::encode_pixels_quality(
                    resized,
                    if alpha || info.options.encoding == "png" {
                        "png"
                    } else {
                        "jpeg"
                    },
                    92,
                )
                .map_err(|e| e.to_string())?
            } else {
                source.to_vec()
            };
            total += frame.len();
            if total > MAX_CLIP {
                return Err(
                    "Prepared clip exceeds 64 MiB; lower detail or shorten the range".into(),
                );
            }
            frames.push(frame.into_boxed_slice().into_vec());
            delays.push(points.get(position + 1).copied().unwrap_or(end) - left);
        }
        Self::with_timing(&self.source_format, frames, delays, 2, Some(info))
    }
    // Render the media representation
    pub fn render(
        &self,
        index: usize,
        width: u16,
        height: u16,
        rotation: u16,
        format: &str,
    ) -> Result<Vec<u8>, String> {
        hardware_core::render::render(&self.frames[index], width, height, rotation, format)
            .map_err(|e| e.to_string())
    }
    // Construct the media archive
    pub fn archive(&self, source: &[u8]) -> Result<Vec<u8>, String> {
        self.write_archive(
            Cursor::new(Vec::new()),
            source,
            &PreparationControl::default(),
        )
        .map(Cursor::into_inner)
    }
    // Write archive
    pub(crate) fn write_archive<W: Write + Seek>(
        &self,
        writer: W,
        source: &[u8],
        control: &PreparationControl,
    ) -> Result<W, String> {
        control.check()?;
        if source.len() > MAX_SOURCE {
            return Err("Media source exceeds 16 MiB".into());
        }
        let mut zip = zip::ZipWriter::new(writer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6));
        zip.start_file("animation.json", options)
            .map_err(|e| e.to_string())?;
        let metadata = Metadata {
            schema_version: 1,
            delays_us: self.delays_us.clone(),
            preparation_revision: self.preparation_revision,
            clip_info: self.clip_info.clone(),
            has_original: !source.is_empty(),
            source_format: self.source_format.clone(),
            delays_ms: self.delays_ms.clone(),
            frames: self
                .frames
                .iter()
                .map(|frame| {
                    use sha2::{Digest, Sha256};
                    hex::encode(Sha256::digest(&***frame))
                })
                .collect(),
        };
        serde_json::to_writer(&mut zip, &metadata).map_err(|e| e.to_string())?;
        if !source.is_empty() {
            zip.start_file(format!("original.{}", self.source_format), options)
                .map_err(|e| e.to_string())?;
            zip.write_all(source).map_err(|e| e.to_string())?;
        }
        let mut seen = std::collections::HashSet::new();
        for (index, frame) in self.frames.iter().enumerate() {
            control.check()?;
            let hash = &metadata.frames[index];
            if !seen.insert(hash) {
                continue;
            }
            zip.start_file(format!("frames/{hash}.image"), options)
                .map_err(|e| e.to_string())?;
            zip.write_all(frame).map_err(|e| e.to_string())?;
        }
        let mut writer = zip.finish().map_err(|e| e.to_string())?;
        if writer.stream_position().map_err(|e| e.to_string())? > MAX_ASSET as u64 {
            return Err("Prepared animation exceeds 96 MiB".into());
        }
        control.check()?;
        Ok(writer)
    }
    // Validate prepared
    pub(crate) fn validate_prepared(
        &self,
        source: &[u8],
        control: &PreparationControl,
    ) -> Result<(), String> {
        control.check()?;
        if source.len() > MAX_SOURCE
            || (!source.is_empty() && detect(source)? != self.source_format)
        {
            return Err("Animation source format mismatch or size limit".into());
        }
        let metadata = Metadata {
            schema_version: 1,
            delays_us: self.delays_us.clone(),
            preparation_revision: self.preparation_revision,
            clip_info: self.clip_info.clone(),
            has_original: !source.is_empty(),
            source_format: self.source_format.clone(),
            delays_ms: self.delays_ms.clone(),
            frames: self.frame_digests.iter().map(hex::encode).collect(),
        };
        let mut seen = std::collections::HashSet::new();
        for (index, frame) in self.frames.iter().enumerate() {
            control.check()?;
            use sha2::{Digest, Sha256};
            let digest: FrameDigest = Sha256::digest(&***frame).into();
            if self.frame_digests.get(index) != Some(&digest) {
                return Err("Prepared frame integrity failure".into());
            }
            if seen.insert(digest) {
                validate_prepared_frame(frame)?;
            }
        }
        metadata.validate(seen.len() + 1 + usize::from(!source.is_empty()))?;
        // Direct publication must retain the same derived timing as archive reload
        // The source delays are public, while the playback endpoints are private
        let mut end = 0u32;
        let mut previous_ms = 0;
        for (index, delay) in self.delays_us.iter().enumerate() {
            end = end.checked_add(*delay).ok_or("Invalid prepared timeline")?;
            let rounded = (end + 500) / 1000;
            if self.ends_us.get(index) != Some(&end)
                || self.delays_ms.get(index) != Some(&rounded.saturating_sub(previous_ms))
            {
                return Err("Inconsistent prepared timeline".into());
            }
            previous_ms = rounded;
        }
        if self.ends_us.len() != self.delays_us.len() || self.duration_ms != end.div_ceil(1000) {
            return Err("Inconsistent prepared timeline".into());
        }
        if self.frames.len() != self.delays_us.len()
            || self.frames.iter().map(|frame| frame.len()).sum::<usize>() > MAX_CLIP
        {
            return Err("Animation exceeds frame or prepared-size limits".into());
        }
        Ok(())
    }
    // Construct from archive controlled
    pub fn from_archive_controlled(
        bytes: &[u8],
        control: &PreparationControl,
    ) -> Result<Self, String> {
        Self::from_reader_controlled(Cursor::new(bytes), control)
    }
    // Construct from reader controlled
    pub(crate) fn from_reader_controlled<R: Read + Seek>(
        mut source: R,
        control: &PreparationControl,
    ) -> Result<Self, String> {
        control.check()?;
        if source
            .seek(std::io::SeekFrom::End(0))
            .map_err(|e| e.to_string())?
            > MAX_ASSET as u64
        {
            return Err("Prepared animation exceeds 96 MiB".into());
        }
        source.rewind().map_err(|e| e.to_string())?;
        let mut zip = current_archive(source)?;
        stream_dock_manager_plugin_runtimes::archives::validate_lzma_headers(&mut zip)?;
        let metadata: Metadata =
            serde_json::from_slice(&read_entry(&mut zip, "animation.json", MAX_METADATA)?)
                .map_err(|e| {
                    format!(
                "Unsupported prepared animation metadata; reimport from the original media: {e}"
            )
                })?;
        metadata.validate(zip.len())?;
        if metadata.has_original {
            let source = read_entry(
                &mut zip,
                &format!("original.{}", metadata.source_format),
                MAX_SOURCE,
            )?;
            if detect(&source)? != metadata.source_format {
                return Err("Animation source format mismatch".into());
            }
        }
        let mut timeline = Vec::with_capacity(metadata.delays_ms.len());
        let mut stored: Vec<Vec<u8>> = vec![];
        let mut unique: std::collections::HashMap<String, usize> = Default::default();
        let mut total = 0usize;
        for index in 0..metadata.delays_ms.len() {
            control.check()?;
            let path = metadata.frame_path(index);
            let stored_index = if let Some(validated) = unique.get(&path) {
                *validated
            } else {
                let bytes = read_entry(&mut zip, &path, MAX_FRAME)?;
                {
                    let hashes = &metadata.frames;
                    use sha2::{Digest, Sha256};
                    if hex::encode(Sha256::digest(&bytes)) != hashes[index] {
                        return Err("Prepared frame integrity failure".into());
                    }
                }
                validate_prepared_frame(&bytes)?;
                (control.progress)(unique.len() + 1);
                control.check()?;
                let stored_index = stored.len();
                stored.push(bytes);
                unique.insert(path, stored_index);
                stored_index
            };
            // Logical timeline bytes remain charged, even when storage is shared
            total = total
                .checked_add(stored[stored_index].len())
                .ok_or("Prepared frame size overflow")?;
            if total > MAX_CLIP {
                return Err("Prepared frames exceed 64 MiB".into());
            }
            timeline.push(stored_index);
        }
        // Intern one batch so a full shared pool is swept at most once per
        // archive, even when every stored frame is distinct and still live
        let (shared, hashes) = intern_frames(stored);
        let frames = timeline
            .iter()
            .map(|index| shared[*index].clone())
            .collect();
        let digests = timeline.iter().map(|index| hashes[*index]).collect();
        Self::with_shared_timing(
            &metadata.source_format,
            frames,
            digests,
            metadata.delays_us,
            metadata.preparation_revision,
            metadata.clip_info,
        )
    }
}
// Validate prepared frame
fn validate_prepared_frame(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FRAME {
        return Err("Animation entry exceeds limits".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(8 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|e| e.to_string())?;
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    delays_us: Vec<u32>,
    preparation_revision: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clip_info: Option<crate::video::ClipInfo>,
    has_original: bool,
    frames: Vec<String>,
    schema_version: u32,
    source_format: String,
    delays_ms: Vec<u32>,
}
impl Metadata {
    // Validate the supplied media value
    fn validate(&self, entries: usize) -> Result<u32, String> {
        if self.schema_version != 1
            || self.preparation_revision != 2
            || self.delays_us.is_empty()
            || self.delays_us.len() > MAX_FRAMES
            || self.delays_us.len() != self.delays_ms.len()
            || self.frames.len() != self.delays_us.len()
            || self.frames.iter().any(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            })
            || entries
                != self
                    .frames
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len()
                    + 1
                    + usize::from(self.has_original)
            || !["gif", "mp4"].contains(&self.source_format.as_str())
        {
            return Err(
                "Unsupported prepared animation metadata; reimport from the original media".into(),
            );
        }
        if !self.has_original && self.clip_info.is_none() {
            return Err(
                "Prepared clip provenance missing; reimport from the original media".into(),
            );
        }
        if let Some(info) = &self.clip_info {
            info.validate()?;
            if info.recording.source_format != self.source_format {
                return Err("Clip source format does not match its recording".into());
            }
        }
        let duration_us = self.delays_us.iter().try_fold(0u32, |sum, delay| {
            if *delay == 0 {
                return Err("Animation frame delay must be positive");
            }
            sum.checked_add(*delay)
                .filter(|total| *total <= MAX_DURATION_MS * 1000)
                .ok_or("Animation exceeds 30 seconds")
        })?;
        Ok(duration_us.div_ceil(1000))
    }
    // Resolve an archived frame’s content-addressed entry name
    fn frame_path(&self, index: usize) -> String {
        format!("frames/{}.image", self.frames[index])
    }
}
// Read current archive
fn current_archive<R: Read + Seek>(mut reader: R) -> Result<zip::ZipArchive<R>, String> {
    reader.rewind().map_err(|e| e.to_string())?;
    let mut signature = [0; 4];
    reader
        .read_exact(&mut signature)
        .map_err(|e| e.to_string())?;
    if signature != *b"PK\x03\x04" {
        return Err("Unsupported prefixed media archive; reimport from the original media".into());
    }
    reader.rewind().map_err(|e| e.to_string())?;
    zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        reader,
    )
    .map_err(|e| e.to_string())
}
/// Browsing only: validate the manifest and first frame without reading/decoding
/// the remaining prepared frames or retaining a playback clip in memory
#[derive(Debug)]
pub(crate) struct PreparedPreview {
    pub frame_rate: f64,
    pub format: String,
    pub duration_ms: u32,
    pub width: u32,
    pub height: u32,
    pub frame: Vec<u8>,
}
// Preview archive using the current media state
pub(crate) fn preview_archive(mut reader: impl Read + Seek) -> Result<PreparedPreview, String> {
    if reader
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| e.to_string())?
        > MAX_ASSET as u64
    {
        return Err("Prepared animation exceeds 96 MiB".into());
    }
    let mut zip = current_archive(reader)?;
    if zip.len() > MAX_FRAMES + 2 {
        return Err("Invalid prepared animation entry count".into());
    }
    stream_dock_manager_plugin_runtimes::archives::validate_lzma_headers(&mut zip)?;
    let metadata: Metadata = serde_json::from_slice(&read_entry(
        &mut zip,
        "animation.json",
        MAX_METADATA,
    )?)
    .map_err(|e| {
        format!("Unsupported prepared animation metadata; reimport from the original media: {e}")
    })?;
    let duration_ms = metadata.validate(zip.len())?;
    if metadata.has_original {
        let mut prefix = vec![];
        {
            let source = zip
                .by_name(&format!("original.{}", metadata.source_format))
                .map_err(|e| e.to_string())?;
            if source.size() > MAX_SOURCE as u64 {
                return Err("Media source exceeds 16 MiB".into());
            }
            source
                .take(16)
                .read_to_end(&mut prefix)
                .map_err(|e| e.to_string())?;
        }
        if detect(&prefix)? != metadata.source_format {
            return Err("Animation source format mismatch".into());
        }
    }
    let frame = read_entry(&mut zip, &metadata.frame_path(0), MAX_FRAME)?;
    {
        let hashes = &metadata.frames;
        use sha2::{Digest, Sha256};
        if hex::encode(Sha256::digest(&frame)) != hashes[0] {
            return Err("Prepared frame integrity failure".into());
        }
    }
    let (width, height) = image::ImageReader::new(Cursor::new(&frame))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err("Prepared frame dimensions exceed 1024 × 1024".into());
    }
    Ok(PreparedPreview {
        frame_rate: metadata.frames.len() as f64 * 1000.0 / f64::from(duration_ms.max(1)),
        format: metadata.source_format,
        duration_ms,
        width,
        height,
        frame,
    })
}
/// Catalog lookup only; playback validates every frame at selection time
pub(crate) fn clip_info_archive(
    reader: impl Read + Seek,
) -> Result<Option<crate::video::ClipInfo>, String> {
    #[cfg(test)]
    crate::media_library::count_library_work(2);
    let mut archive = current_archive(reader)?;
    let metadata: Metadata =
        serde_json::from_slice(&read_entry(&mut archive, "animation.json", MAX_METADATA)?)
            .map_err(|e| e.to_string())?;
    metadata.validate(archive.len())?;
    Ok(metadata.clip_info)
}
// Read entry
fn read_entry<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    zip.by_name(name)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Animation entry exceeds limits".into());
    }
    Ok(bytes.into_boxed_slice().into_vec())
}
// Detect available media support
pub fn detect(bytes: &[u8]) -> Result<&'static str, String> {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Ok("gif");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Ok("mp4");
    }
    if hardware_core::render::is_svg_media(bytes) {
        hardware_core::render::svg_media_dimensions(bytes).map_err(|error| error.to_string())?;
        return Ok("svg");
    }
    match image::guess_format(bytes).map_err(|_| "Choose PNG, JPEG, SVG, GIF or MP4 media")? {
        ImageFormat::Png => Ok("png"),
        ImageFormat::Jpeg => Ok("jpeg"),
        _ => Err("Choose PNG, JPEG, SVG, GIF or MP4 media".into()),
    }
}
#[derive(Clone)]
pub struct PreparationControl {
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub progress: Arc<dyn Fn(usize) + Send + Sync>,
}
impl Default for PreparationControl {
    // Construct the default media value
    fn default() -> Self {
        Self {
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: Arc::new(|_| {}),
        }
    }
}
impl PreparationControl {
    // Check the current media conditions
    pub fn check(&self) -> Result<(), String> {
        if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            Err("Media preparation cancelled".into())
        } else {
            Ok(())
        }
    }
}
// Prepare the media operation
pub fn prepare(bytes: &[u8]) -> Result<Clip, String> {
    prepare_controlled(bytes, &PreparationControl::default())
}
// Prepare controlled
pub fn prepare_controlled(bytes: &[u8], control: &PreparationControl) -> Result<Clip, String> {
    control.check()?;
    if bytes.len() > MAX_SOURCE {
        return Err("Media source exceeds 16 MiB".into());
    }
    match detect(bytes)? {
        "gif" => gif(bytes, control),
        "mp4" => mp4(bytes, control),
        _ => Err("Media is not animated".into()),
    }
}
// Prepare a GIF using the default 512-pixel detail limit
fn gif(bytes: &[u8], control: &PreparationControl) -> Result<Clip, String> {
    gif_with_detail(bytes, 512, control)
}
// Create a GIF decoder with explicit dimension and allocation limits
fn gif_decoder(bytes: &[u8]) -> Result<image::codecs::gif::GifDecoder<Cursor<&[u8]>>, String> {
    let mut decoder =
        image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(64 * 1024 * 1024);
    decoder.set_limits(limits).map_err(|e| e.to_string())?;
    Ok(decoder)
}

// Validate the first GIF frame and check whether a second frame exists
pub fn gif_has_animation(path: &std::path::Path) -> Result<bool, String> {
    let bytes = crate::media_library::read_regular(path, MAX_SOURCE)?;
    let mut frames = gif_decoder(&bytes)?.into_frames();
    frames
        .next()
        .ok_or("GIF has no frames")?
        .map_err(|e| e.to_string())?;
    Ok(frames
        .next()
        .transpose()
        .map_err(|e| e.to_string())?
        .is_some())
}

// Prepare GIF frames with the selected detail bound and standard cancellation limits
pub(crate) fn gif_with_detail(
    bytes: &[u8],
    edge: u16,
    control: &PreparationControl,
) -> Result<Clip, String> {
    if !(64..=1024).contains(&edge) {
        return Err("Invalid GIF detail".into());
    }
    gif_with_detail_checked(bytes, edge, control, &|| false, DECODE_TIMEOUT, MAX_FRAMES)
}
// Report the GIF with detail checked condition for media
pub(crate) fn gif_with_detail_checked(
    bytes: &[u8],
    edge: u16,
    control: &PreparationControl,
    cancelled: &dyn Fn() -> bool,
    maximum_time: Duration,
    maximum_frames: usize,
) -> Result<Clip, String> {
    if cancelled() {
        return Err("Vendor animation source changed during preparation".into());
    }
    if !(1..=1024).contains(&edge) || bytes.len() > MAX_SOURCE {
        return Err("Invalid GIF detail or source size".into());
    }
    let decoder = gif_decoder(bytes)?;
    let start = Instant::now();
    let mut frames = vec![];
    let mut delays = vec![];
    let mut total = 0;
    for frame in decoder.into_frames() {
        control.check()?;
        if cancelled() {
            return Err("Vendor animation source changed during preparation".into());
        }
        (control.progress)(frames.len());
        if frames.len() == maximum_frames || start.elapsed() > maximum_time {
            return Err("GIF exceeds frame or decoding-time limits".into());
        }
        let frame = frame.map_err(|e| e.to_string())?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let delay = numerator.div_ceil(denominator.max(1)).max(10);
        let pixels = DynamicImage::ImageRgba8(frame.into_buffer());
        let pixels = if pixels.width().max(pixels.height()) > u32::from(edge) {
            pixels.resize(
                edge.into(),
                edge.into(),
                image::imageops::FilterType::Triangle,
            )
        } else {
            pixels
        };
        let encoded =
            hardware_core::render::encode_pixels(pixels, "png").map_err(|e| e.to_string())?;
        total += encoded.len();
        if total > MAX_CLIP
            || delays.iter().copied().sum::<u32>().saturating_add(delay) > MAX_DURATION_MS
        {
            return Err("GIF exceeds 30 seconds or 64 MiB of prepared frames".into());
        }
        frames.push(encoded.into_boxed_slice().into_vec());
        delays.push(delay);
    }
    Clip::new("gif", frames, delays)
}
// Resolve an explicitly configured or available FFmpeg executable
pub fn ffmpeg_path() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("STREAM_DOCK_MANAGER_FFMPEG") {
        let path = PathBuf::from(path);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
        return Err("STREAM_DOCK_MANAGER_FFMPEG must name an absolute FFmpeg executable".into());
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join(name))
            .find(|p| p.is_file())
    }) {
        return Ok(path);
    }
    #[cfg(target_os = "macos")]
    for path in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
        if std::path::Path::new(path).is_file() {
            return Ok(path.into());
        }
    }
    Err("MP4 import requires FFmpeg. Install FFmpeg or set STREAM_DOCK_MANAGER_FFMPEG to its absolute executable path. Prepared profile archives play without FFmpeg.".into())
}
struct Decoder(std::process::Child);
impl Drop for Decoder {
    // Release this media owner when it leaves scope
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
// Stage MP4 bytes in a temporary file for the bounded FFmpeg decoding path
fn mp4(bytes: &[u8], control: &PreparationControl) -> Result<Clip, String> {
    let mut input = tempfile::Builder::new()
        .suffix(".mp4")
        .tempfile()
        .map_err(|e| e.to_string())?;
    input.write_all(bytes).map_err(|e| e.to_string())?;
    input.flush().map_err(|e| e.to_string())?;
    mp4_file(input.path(), None, control, 512, None)
}
// Decode the selected MP4 range into bounded prepared frames and timing
pub(crate) fn mp4_file(
    path: &std::path::Path,
    range: Option<(u64, u64)>,
    control: &PreparationControl,
    edge: u16,
    maximum: Option<f64>,
) -> Result<Clip, String> {
    let mut command = crate::native_process::command(ffmpeg_path()?);
    if let Some((start, _)) = range {
        command.args(["-ss", &(start as f64 / 1_000_000.0).to_string()]);
    }
    let mut child = Decoder(
        command
            .args([
                "-v",
                "info",
                "-nostdin",
                "-max_alloc",
                "67108864",
                "-protocol_whitelist",
                "file,pipe",
                "-max_pixels",
                "8847360",
                "-threads",
                "1",
                "-f",
                "mov",
                "-i",
            ])
            .arg(path)
            .args(if let Some((start, end)) = range {
                vec![
                    "-t".to_string(),
                    ((end - start) as f64 / 1_000_000.0).to_string(),
                ]
            } else {
                vec![]
            })
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-dn",
                "-vf",
                &format!(
                    "{}scale={edge}:{edge}:force_original_aspect_ratio=decrease,{}showinfo=checksum=0",
                    maximum.map(|rate| format!("fps={rate},")).unwrap_or_default(),
                    range.map(|(start,end)| format!("trim=duration={},",(end-start) as f64 / 1_000_000.)).unwrap_or_default()
                ),
                "-fps_mode",
                "passthrough",
                "-frames:v",
                "8193",
                "-threads",
                "1",
                "-filter_threads",
                "1",
                "-c:v",
                "png",
                "-f",
                "image2pipe",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Cannot start FFmpeg: {e}"))?,
    );
    let stdout = child.0.stdout.take().ok_or("FFmpeg stdout missing")?;
    let mut stderr = child.0.stderr.take().ok_or("FFmpeg stderr missing")?;
    // Drain stderr concurrently to avoid blocking FFmpeg while bounding retained diagnostics
    let errors = std::thread::spawn(move || {
        let mut kept = String::new();
        let mut times = Vec::new();
        let mut reader = BufReader::new(&mut stderr);
        loop {
            let mut line = String::new();
            match reader.by_ref().take(65536).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                _ => {}
            }
            if line.contains("showinfo") && line.contains("pts_time:") && times.len() <= MAX_FRAMES
            {
                let value = |key: &str| {
                    line.split(key)
                        .nth(1)
                        .and_then(|value| value.split_whitespace().next())
                        .and_then(|value| value.parse::<f64>().ok())
                };
                if let Some(pts) = value("pts_time:") {
                    times.push((pts, value("duration_time:")));
                }
            }
            if kept.len() < 8192 {
                kept.extend(line.chars().take(8192 - kept.len()));
            }
        }
        (kept, times)
    });
    let (sender, receiver) = mpsc::channel();
    let reader_control = control.clone();
    // Decode output on a worker while the caller enforces cancellation and the deadline
    let reader = std::thread::spawn(move || {
        let _ = sender.send(read_video_frames(stdout, &reader_control));
    });
    let deadline = Instant::now() + DECODE_TIMEOUT;
    let decoded = loop {
        if let Err(error) = control.check() {
            break Err(error);
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            _ => break Err("MP4 decoding exceeded two minutes".into()),
        }
    };
    let exit_deadline = Instant::now() + Duration::from_secs(1);
    let successful = if decoded.is_ok() {
        loop {
            match child.0.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < exit_deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => break false,
            }
        }
    } else {
        false
    };
    drop(child);
    let _ = reader.join();
    let (error, timestamps) = errors.join().unwrap_or_default();
    if !successful {
        return Err(format!(
            "MP4 import failed: {}. {}",
            decoded
                .err()
                .unwrap_or_else(|| "decoder rejected the clip".into()),
            error
        ));
    }
    let frames = decoded?;
    if timestamps.len() != frames.len() {
        return Err(format!(
            "FFmpeg supplied {} timestamps for {} prepared frames",
            timestamps.len(),
            frames.len()
        ));
    }
    let origin = timestamps.first().ok_or("No decoded video frames")?.0;
    let mut delays = Vec::with_capacity(frames.len());
    for (index, (pts, duration)) in timestamps.iter().enumerate() {
        let end = timestamps
            .get(index + 1)
            .map(|next| next.0)
            .unwrap_or_else(|| {
                pts + duration.filter(|d| *d > 0.0).unwrap_or_else(|| {
                    timestamps
                        .get(index.saturating_sub(1))
                        .map(|previous| pts - previous.0)
                        .filter(|d| *d > 0.0)
                        .unwrap_or(1.0 / 30.0)
                })
            });
        let start_us = ((pts - origin) * 1_000_000.0).round();
        let mut end_us = ((end - origin) * 1_000_000.0).round();
        if let Some((start, end)) = range {
            let limit = (end - start) as f64;
            end_us = if index + 1 == timestamps.len() {
                limit
            } else {
                end_us.min(limit)
            };
        }
        if !start_us.is_finite()
            || !end_us.is_finite()
            || end_us <= start_us
            || end_us > 30_000_000.0
        {
            return Err("Invalid source timeline or clip longer than 30 seconds".into());
        }
        delays.push((end_us - start_us) as u32);
    }
    Clip::with_timing("mp4", frames, delays, 2, None)
}
// Read video frames
fn read_video_frames(
    mut output: impl Read,
    control: &PreparationControl,
) -> Result<Vec<Vec<u8>>, String> {
    let mut frames = vec![];
    let mut total = 0;
    loop {
        control.check()?;
        (control.progress)(frames.len());
        let mut signature = [0; 8];
        if output
            .read(&mut signature[..1])
            .map_err(|e| e.to_string())?
            == 0
        {
            break;
        }
        output
            .read_exact(&mut signature[1..])
            .map_err(|e| e.to_string())?;
        if &signature != b"\x89PNG\r\n\x1a\n" {
            return Err("Invalid FFmpeg frame stream".into());
        }
        let mut bytes = signature.to_vec();
        loop {
            let mut header = [0; 8];
            output.read_exact(&mut header).map_err(|e| e.to_string())?;
            let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
            if bytes.len().saturating_add(length).saturating_add(12) > MAX_FRAME {
                return Err("Video frame exceeds limits".into());
            }
            bytes.extend_from_slice(&header);
            let offset = bytes.len();
            bytes.resize(offset + length + 4, 0);
            output
                .read_exact(&mut bytes[offset..])
                .map_err(|e| e.to_string())?;
            if &header[4..8] == b"IEND" {
                break;
            }
        }
        if frames.len() == MAX_FRAMES {
            return Err("MP4 exceeds 30 seconds".into());
        }
        let mut reader = image::ImageReader::new(Cursor::new(bytes));
        reader.set_format(ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(1024);
        limits.max_image_height = Some(1024);
        limits.max_alloc = Some(8 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().map_err(|e| e.to_string())?;
        let mut frame = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut frame, 92)
            .encode_image(&image.into_rgb8())
            .map_err(|e| e.to_string())?;
        total += frame.len();
        if total > MAX_CLIP {
            return Err("MP4 exceeds 64 MiB of prepared frames".into());
        }
        frames.push(frame.into_boxed_slice().into_vec());
    }
    Ok(frames)
}
#[derive(Default)]
pub(crate) struct Cache {
    entries: VecDeque<(String, Arc<crate::media_budget::Slot>)>,
    budget: Arc<crate::media_budget::MediaBudget>,
    // Track allocations, not Clip lifetime: a renderer can retain a frame after
    // Clip eviction, and multiple clips may share the same immutable frame
    resident: std::collections::HashMap<usize, (WeakBytes, usize)>,
}
impl Cache {
    // Construct using budget
    pub(crate) fn with_budget(budget: Arc<crate::media_budget::MediaBudget>) -> Self {
        Self {
            budget,
            ..Default::default()
        }
    }
    // Collect memory
    pub(crate) fn collect_memory(&mut self, memory: &mut crate::media_memory::MediaMemory) {
        self.resident_bytes();
        self.entries.retain(|(_, slot)| slot.live());
        for (_, slot) in &self.entries {
            if let Some(clip) = slot.peek_clip() {
                for frame in &clip.frames {
                    memory.cached_frame(frame);
                }
            }
        }
        for (frame, bytes) in self.resident.values() {
            memory.live_frame(frame, *bytes);
        }
    }
    // Discard expired frame references before summing resident allocation charges
    fn resident_bytes(&mut self) -> usize {
        self.resident
            .retain(|_, (frame, _)| frame.strong_count() > 0);
        self.resident.values().map(|(_, bytes)| bytes).sum()
    }
    // Remove the selected media entry
    pub fn remove(&mut self, id: &str) {
        if let Some(index) = self.entries.iter().position(|(key, _)| key == id) {
            if let Some((_, slot)) = self.entries.remove(index) {
                slot.evict();
            }
        }
        self.resident_bytes();
    }
    // Return the requested media value
    pub fn get(&mut self, id: &str) -> Option<Arc<Clip>> {
        let index = self.entries.iter().position(|(key, _)| key == id)?;
        let entry = self.entries.remove(index)?;
        let clip = entry.1.clip()?;
        self.entries.push_back(entry);
        Some(clip)
    }
    // Insert an entry into media
    pub fn insert(&mut self, id: String, clip: Arc<Clip>) -> Result<(), String> {
        self.remove(&id);
        self.entries.retain(|(_, slot)| slot.live());
        while self.entries.len() >= 4096 {
            if let Some((_, slot)) = self.entries.pop_front() {
                slot.evict();
            }
        }
        let slot = self
            .budget
            .admit(crate::media_budget::Value::Clip(clip.clone()))?;
        for frame in &clip.frames {
            self.resident.insert(
                Arc::as_ptr(frame) as usize,
                (Arc::downgrade(frame), frame.len()),
            );
        }
        self.resident_bytes();
        self.entries.push_back((id, slot));
        Ok(())
    }
}
#[cfg(test)]
#[path = "media_archive_tests.rs"]
mod archive_tests;

#[cfg(test)]
#[path = "media_allocation_tests.rs"]
pub(crate) mod allocation_tests;

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    // Verify that prepared preview reads a small part of a large clip
    fn prepared_preview_reads_a_small_part_of_a_large_clip() {
        use image::ImageEncoder;
        struct Counted {
            bytes: Cursor<Vec<u8>>,
            read: usize,
        }
        impl Read for Counted {
            // Read the requested media data
            fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
                let count = self.bytes.read(target)?;
                self.read += count;
                Ok(count)
            }
        }
        impl Seek for Counted {
            // Seek to the requested media position
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                self.bytes.seek(position)
            }
        }
        let frames = (0u8..128)
            .map(|i| {
                // Use deterministic noisy pixels so DEFLATE cannot collapse the large-clip fixture
                let mut noise = u32::from(i) + 1;
                let pixels = image::RgbImage::from_fn(64, 64, |_, _| {
                    image::Rgb(std::array::from_fn(|_| {
                        noise ^= noise << 13;
                        noise ^= noise >> 17;
                        noise ^= noise << 5;
                        noise as u8
                    }))
                });
                let mut exif = b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
                exif.resize(8192, 0);
                let mut bytes = vec![];
                let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
                encoder.set_exif_metadata(exif).unwrap();
                encoder
                    .write_image(&pixels, 64, 64, image::ExtendedColorType::Rgb8)
                    .unwrap();
                bytes
            })
            .collect();
        let clip = Clip::new("gif", frames, vec![50; 128]).unwrap();
        let bytes = clip.archive(&sample_gif()).unwrap();
        let total = bytes.len();
        assert!(
            total > 1024 * 1024,
            "fixture must remain a genuinely large clip"
        );
        let mut reader = Counted {
            bytes: Cursor::new(bytes),
            read: 0,
        };
        let preview = preview_archive(&mut reader).unwrap();
        assert_eq!(
            (preview.width, preview.height, preview.duration_ms),
            (64, 64, 6400)
        );
        assert_eq!(preview.frame.as_slice(), &**clip.frames[0]);
        assert!(
            reader.read < total / 8,
            "preview read {} of {total} bytes",
            reader.read
        );
        eprintln!(
            "MEDIA_BROWSE_IO preview_bytes={} archive_bytes={total} frames=128",
            reader.read
        );
    }
    #[test]
    // Verify that unsupported archive metadata is rejected without rewriting source
    fn unsupported_archive_metadata_is_rejected_without_rewriting_source() {
        let source = sample_gif();
        let bytes = prepare(&source).unwrap().archive(&source).unwrap();
        for field in [
            "delays_us",
            "preparation_revision",
            "has_original",
            "frames",
        ] {
            let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let mut writer = zip::ZipWriter::new(Cursor::new(vec![]));
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).unwrap();
                let name = entry.name().unwrap().to_string();
                let mut data = vec![];
                entry.read_to_end(&mut data).unwrap();
                if name == "animation.json" {
                    let mut metadata: serde_json::Value = serde_json::from_slice(&data).unwrap();
                    metadata.as_object_mut().unwrap().remove(field);
                    data = serde_json::to_vec(&metadata).unwrap();
                }
                writer
                    .start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(&data).unwrap();
            }
            let old = writer.finish().unwrap().into_inner();
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("unsupported.sdmedia");
            std::fs::write(&path, &old).unwrap();
            assert!(
                preview_archive(std::fs::File::open(&path).unwrap())
                    .unwrap_err()
                    .contains("reimport"),
                "{field}"
            );
            assert!(Clip::from_reader_controlled(
                std::fs::File::open(&path).unwrap(),
                &Default::default()
            )
            .is_err());
            assert_eq!(std::fs::read(&path).unwrap(), old);
        }
    }

    #[test]
    // Verify that prepared preview checks first frame integrity and limits
    fn prepared_preview_checks_first_frame_integrity_and_limits() {
        let source = sample_gif();
        let bytes = prepare(&source).unwrap().archive(&source).unwrap();
        let mut prefixed = b"legacy archive prefix".to_vec();
        prefixed.extend_from_slice(&bytes);
        assert!(preview_archive(Cursor::new(&prefixed))
            .unwrap_err()
            .contains("reimport"));
        assert!(Clip::from_archive_controlled(&prefixed, &Default::default()).is_err());
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let metadata: Metadata =
            serde_json::from_slice(&read_entry(&mut archive, "animation.json", 64 * 1024).unwrap())
                .unwrap();
        for index in [0, 1] {
            let mut writer = zip::ZipWriter::new(Cursor::new(vec![]));
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).unwrap();
                let mut data = vec![];
                entry.read_to_end(&mut data).unwrap();
                if entry.name().unwrap() == metadata.frame_path(index) {
                    data = b"corrupt frame".to_vec();
                }
                writer
                    .start_file(
                        entry.name().unwrap(),
                        zip::write::SimpleFileOptions::default(),
                    )
                    .unwrap();
                writer.write_all(&data).unwrap();
            }
            let changed = writer.finish().unwrap().into_inner();
            assert!(Clip::from_archive_controlled(&changed, &Default::default()).is_err());
            if index == 0 {
                assert!(preview_archive(Cursor::new(&changed)).is_err());
            } else {
                assert!(preview_archive(Cursor::new(&changed)).is_ok());
            }
        }
        let frame = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                1025,
                1,
                image::Rgb([0, 0, 0]),
            )),
            "png",
        )
        .unwrap();
        let oversized = Clip::new("gif", vec![frame], vec![100])
            .unwrap()
            .archive(&source)
            .unwrap();
        assert!(preview_archive(Cursor::new(oversized))
            .unwrap_err()
            .contains("1024"));
    }
    #[test]
    // Verify that source timeline and deadlines do not accumulate render time
    fn source_timeline_and_deadlines_do_not_accumulate_render_time() {
        let clip = Clip::new("gif", vec![vec![1], vec![2], vec![3]], vec![40, 80, 40]).unwrap();
        let mut options = MediaOptions::default();
        assert_eq!(options.frame_index(&clip, Duration::from_millis(40)), 1);
        assert_eq!(options.frame_index(&clip, Duration::from_millis(120)), 2);
        assert_eq!(options.frame_index(&clip, Duration::from_millis(160)), 0);
        assert_eq!(
            options.next_frame_delay(&clip, Duration::from_millis(40)),
            Duration::from_millis(80)
        );
        options.max_fps = Some(5.0);
        assert_eq!(
            options.frame_index(&clip, Duration::from_millis(120)),
            2,
            "rate caps must not quantize source time"
        );
        options.trim_start_ms = 40;
        options.trim_end_ms = Some(120);
        options.speed = 2.0;
        assert_eq!(
            options.next_frame_delay(&clip, Duration::from_millis(10)),
            Duration::from_millis(30)
        );
        options.trim_start_ms = 50;
        assert_eq!(
            options.frame_age(&clip, 1, Duration::from_millis(40)),
            Duration::from_millis(5)
        );
        let start = Instant::now();
        let period = Duration::from_millis(40);
        assert_eq!(
            advance_deadline(start, start + Duration::from_millis(13), period),
            start + period
        );
        assert_eq!(
            advance_deadline(start, start + Duration::from_millis(137), period),
            start + Duration::from_millis(160)
        );
    }

    #[test]
    // Verify that retired FPS is rejected even with current limit
    fn retired_fps_is_rejected_even_with_current_limit() {
        for value in [
            serde_json::json!({"fps":24}),
            serde_json::json!({"fps":24,"max_fps":30}),
        ] {
            let error = serde_json::from_value::<MediaOptions>(value)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("fps") && error.contains("max_fps"),
                "{error}"
            );
        }
        for value in [serde_json::json!({}), serde_json::json!({"max_fps":30})] {
            serde_json::from_value::<MediaOptions>(value)
                .unwrap()
                .validate()
                .unwrap();
        }
    }

    #[test]
    // Verify that assignment cadence is independent of pixels speed and clip duration
    fn assignment_cadence_is_independent_of_pixels_speed_and_clip_duration() {
        let clip = super::Clip::new(
            "mp4",
            (0..24).map(|i| vec![i]).collect(),
            (0..24)
                .map(|i| ((i + 1) * 1000 + 12) / 24 - (i * 1000 + 12) / 24)
                .collect(),
        )
        .unwrap();
        assert_eq!(clip.duration_ms, 1000);
        let mut options = super::MediaOptions::default();
        let pixels = options.pixel_key();
        for fps in [5, 10, 15, 20, 24] {
            options.max_fps = Some(f64::from(fps));
            options.validate().unwrap();
            assert_eq!(options.maximum_fps(), Some(f64::from(fps)));
            assert_eq!(options.pixel_key(), pixels);
            assert_eq!(
                options.frame_index(&clip, std::time::Duration::from_secs(1)),
                0
            );
            options.speed = 2.0;
            assert_eq!(
                options.frame_index(&clip, std::time::Duration::from_secs(1)),
                0
            );
            assert_eq!(options.pixel_key(), pixels);
        }
        options.max_fps = Some(23.0);
        options.validate().unwrap();
        options.max_fps = Some(f64::NAN);
        assert!(options.validate().is_err());
        assert_eq!(super::MediaOptions::default().maximum_fps(), None);
        let delays: Vec<_> = (0..720)
            .map(|i| ((i + 1) * 1000 + 12) / 24 - (i * 1000 + 12) / 24)
            .collect();
        assert_eq!(delays.iter().sum::<u32>(), 30_000);
        assert!(delays.iter().all(|d| [41, 42].contains(d)));
    }
    use super::*;
    #[test]
    // Verify that archive decodes unique frames once and preserves timeline and cancellation
    fn archive_decodes_unique_frames_once_and_preserves_timeline_and_cancellation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let frame = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                4,
                4,
                image::Rgb([10, 20, 30]),
            )),
            "png",
        )
        .unwrap();
        let clip = Clip::new("gif", vec![frame; 100], vec![50; 100]).unwrap();
        let archive = clip.archive(&sample_gif()).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let observed = count.clone();
        let control = PreparationControl {
            progress: Arc::new(move |_| {
                observed.fetch_add(1, Ordering::SeqCst);
            }),
            ..Default::default()
        };
        let restored = Clip::from_archive_controlled(&archive, &control).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(restored.delays_us, clip.delays_us);
        assert_eq!(restored.frames.len(), 100);
        assert!(Arc::ptr_eq(&restored.frames[0], &restored.frames[99]));
        assert_eq!(restored.frame_index(Duration::from_millis(4999)), 99);
        assert_eq!(restored.frame_index(Duration::from_millis(5000)), 0);
        let cancelled = control.cancelled.clone();
        let control = PreparationControl {
            progress: Arc::new(move |_| cancelled.store(true, Ordering::SeqCst)),
            ..control
        };
        assert!(Clip::from_archive_controlled(&archive, &control)
            .err()
            .unwrap()
            .contains("cancelled"));
    }
    #[test]
    // Verify that deduplicated archive still enforces logical frame bytes
    fn deduplicated_archive_still_enforces_logical_frame_bytes() {
        use image::ImageEncoder;
        let mut frame = vec![];
        let mut encoder = image::codecs::png::PngEncoder::new(&mut frame);
        let mut exif = b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
        exif.resize(10 * 1024, 0);
        encoder.set_exif_metadata(exif).unwrap();
        encoder
            .write_image(&[10, 20, 30], 1, 1, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert!(frame.len() * MAX_FRAMES > MAX_CLIP);
        let clip = Clip::new("gif", vec![frame], vec![1]).unwrap();
        let archive = clip.archive(&sample_gif()).unwrap();
        let mut source = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(vec![]));
        for index in 0..source.len() {
            let mut file = source.by_index(index).unwrap();
            let mut data = vec![];
            file.read_to_end(&mut data).unwrap();
            if file.name().unwrap() == "animation.json" {
                let mut metadata: Metadata = serde_json::from_slice(&data).unwrap();
                metadata.frames = vec![hex::encode(clip.frame_digests[0]); MAX_FRAMES];
                metadata.delays_ms = vec![1; MAX_FRAMES];
                metadata.delays_us = vec![1000; MAX_FRAMES];
                data = serde_json::to_vec(&metadata).unwrap();
            }
            writer
                .start_file(
                    file.name().unwrap(),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(&data).unwrap();
        }
        let archive = writer.finish().unwrap().into_inner();
        assert!(Clip::from_archive_controlled(&archive, &Default::default())
            .err()
            .unwrap()
            .contains("64 MiB"));
    }
    #[test]
    #[ignore = "Controlled release-mode performance probe; no HID or external services"]
    // Verify that patch archive profile
    fn patch_archive_profile() {
        let frame = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                256,
                256,
                image::Rgb([20, 40, 60]),
            )),
            "png",
        )
        .unwrap();
        let clip = Clip::new("gif", vec![frame; 100], vec![50; 100]).unwrap();
        let archive = clip.archive(&sample_gif()).unwrap();
        let mut runs = vec![];
        for _ in 0..5 {
            let cold = Instant::now();
            let decoded = Clip::from_archive_controlled(&archive, &Default::default()).unwrap();
            let cold_us = cold.elapsed().as_micros();
            let unique: std::collections::HashSet<_> = decoded
                .frames
                .iter()
                .map(|frame| Arc::as_ptr(frame) as *const u8 as usize)
                .collect();
            let warm = Instant::now();
            for _ in 0..10 {
                std::hint::black_box(
                    Clip::from_archive_controlled(&archive, &Default::default()).unwrap(),
                );
            }
            runs.push(serde_json::json!({"cold_us":cold_us,"warm_mean_us":warm.elapsed().as_secs_f64()*100_000.0,"logical_frames":decoded.frames.len(),"unique_frame_allocations":unique.len(),"stored_archive_bytes":archive.len(),"logical_frame_bytes":decoded.frames.iter().map(|frame|frame.len()).sum::<usize>()}));
        }
        eprintln!(
            "PATCH_ARCHIVE_PROFILE={}",
            serde_json::to_string(&runs).unwrap()
        );
    }
    #[test]
    // Verify that shared frame pool recovers at capacity without retiring live frames
    fn shared_frame_pool_recovers_at_capacity_without_retiring_live_frames() {
        use sha2::Digest;
        let mut pool = FramePool::default();
        let held = Arc::new(vec![91; 12].into_boxed_slice());
        pool.insert([255; 32], Arc::downgrade(&held));
        for index in 1..MAX_SHARED_FRAMES {
            pool.insert(
                sha2::Sha256::digest(index.to_le_bytes()).into(),
                Arc::downgrade(&Arc::new(vec![0].into_boxed_slice())),
            );
        }
        let (frames, _) = share_frames(vec![vec![7; 24], vec![7; 24]], &mut pool);
        assert!(
            Arc::ptr_eq(&frames[0], &frames[1]),
            "expired admissions must be reusable"
        );
        assert_eq!(pool.len(), 2);
        assert_eq!(&**pool[&[255; 32]].upgrade().unwrap(), &[91; 12]);
        assert_eq!(held.len(), 12);
        // A full live pool stays bounded and must not revoke external ownership
        let mut live = vec![];
        for index in pool.len()..MAX_SHARED_FRAMES {
            let value = Arc::new(vec![1].into_boxed_slice());
            pool.insert(
                sha2::Sha256::digest(index.to_le_bytes()).into(),
                Arc::downgrade(&value),
            );
            live.push(value);
        }
        share_frames(vec![vec![8]], &mut pool);
        assert_eq!(pool.len(), MAX_SHARED_FRAMES);
        assert_eq!(live.len(), MAX_SHARED_FRAMES - 2);
    }
    #[test]
    // Verify that clip cache counts shared frame allocations once
    fn clip_cache_counts_shared_frame_allocations_once() {
        let mut cache = Cache::default();
        for index in 0..8 {
            let clip = Arc::new(Clip::new("gif", vec![vec![71; 1024 * 1024]], vec![50]).unwrap());
            cache.insert(index.to_string(), clip).unwrap();
        }
        assert_eq!(cache.resident_bytes(), 1024 * 1024);
        assert_eq!(cache.resident.len(), 1);
    }
    #[test]
    // Verify that clip cache accounts for frames retained after clip eviction
    fn clip_cache_accounts_for_frames_retained_after_clip_eviction() {
        let mut cache = Cache::default();
        let mut held = vec![];
        for index in 0..6 {
            let clip =
                Arc::new(Clip::new("gif", vec![vec![index; MAX_CLIP - 1024]], vec![50]).unwrap());
            held.push(Arc::clone(&clip.frames[0]));
            cache.insert(index.to_string(), clip).unwrap();
        }
        let next = Arc::new(Clip::new("gif", vec![vec![9; MAX_CLIP]], vec![50]).unwrap());
        assert!(cache.insert("next".into(), Arc::clone(&next)).is_err());
        assert!(cache.resident_bytes() <= crate::media_budget::HARD_BYTES);
        assert_eq!(held[0][0], 0);
        drop(held);
        assert_eq!(cache.resident_bytes(), 0);
        cache.insert("next".into(), next).unwrap();
    }
    #[test]
    // Verify that shared cache hits do not copy and evicted live frames stay charged
    fn shared_cache_hits_do_not_copy_and_evicted_live_frames_stay_charged() {
        let mut cache = RenderCache::default();
        let first = cache
            .insert_shared("a".into(), vec![3; 20 * 1024 * 1024])
            .unwrap();
        let hit = cache.get_shared("a").unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
        assert_eq!(cache.copied_bytes, 0);
        let second = cache
            .insert_shared("b".into(), vec![4; 20 * 1024 * 1024])
            .unwrap();
        let third = cache
            .insert_shared("c".into(), vec![5; 20 * 1024 * 1024])
            .unwrap();
        assert!(cache
            .insert_shared("d".into(), vec![6; 20 * 1024 * 1024])
            .is_err());
        assert!(cache.resident_bytes() <= MAX_CACHE);
        assert_eq!(first[0], 3);
        cache.clear();
        assert_eq!(cache.bytes, 0);
        assert_eq!(cache.in_flight_bytes(), 60 * 1024 * 1024);
        drop((first, hit, second, third));
        assert_eq!(cache.resident_bytes(), 0);
        assert!(cache
            .insert_shared("d".into(), vec![6; 20 * 1024 * 1024])
            .is_ok());
    }
    #[test]
    // Verify that shared cache aliases charge allocation once
    fn shared_cache_aliases_charge_allocation_once() {
        let mut cache = RenderCache::default();
        let frame = cache.insert_shared("first".into(), vec![7; 100]).unwrap();
        cache.insert_frame("alias".into(), frame.clone()).unwrap();
        assert_eq!(cache.bytes, 100 + 2 * RENDER_KEY_BYTES);
        assert_eq!(cache.resident_bytes(), 100 + 2 * RENDER_KEY_BYTES);
        assert_eq!(cache.in_flight_bytes(), 0);
        cache.remove("first");
        assert_eq!(cache.bytes, 100 + RENDER_KEY_BYTES);
        assert_eq!(cache.in_flight_bytes(), 0);
        assert!(Arc::ptr_eq(&cache.get_shared("alias").unwrap(), &frame));
        cache.clear();
        assert_eq!(cache.in_flight_bytes(), 100);
        drop(frame);
        assert_eq!(cache.resident_bytes(), 0);
    }
    #[test]
    // Verify that shared cache replacement keeps old reference valid
    fn shared_cache_replacement_keeps_old_reference_valid() {
        let mut cache = RenderCache::default();
        let before = cache.insert_shared("key".into(), vec![1; 100]).unwrap();
        let after = cache.insert_shared("key".into(), vec![2; 100]).unwrap();
        assert_eq!(before[0], 1);
        assert_eq!(after[0], 2);
        assert_eq!(cache.in_flight_bytes(), 100);
        drop(before);
        assert_eq!(cache.in_flight_bytes(), 0);
        assert!(Arc::ptr_eq(&cache.get_shared("key").unwrap(), &after));
        assert_eq!(cache.copied_bytes, 0);
    }
    #[test]
    // Verify that GIF preparation preserves transparent pixels
    fn gif_preparation_preserves_transparent_pixels() {
        let mut image = image::RgbaImage::new(16, 16);
        for x in 4..12 {
            for y in 4..12 {
                image.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let mut source = vec![];
        {
            image::codecs::gif::GifEncoder::new(&mut source)
                .encode_frame(image::Frame::new(image))
                .unwrap();
        }
        let clip = prepare(&source).unwrap();
        let frame = image::load_from_memory(&clip.frames[0]).unwrap().to_rgba8();
        assert_eq!(frame.get_pixel(0, 0)[3], 0);
        assert_eq!(frame.get_pixel(8, 8).0, [255, 0, 0, 255]);
        let fitted = hardware_core::render::encode_pixels(
            hardware_core::render::edited_pixels(
                &clip.frames[0],
                32,
                16,
                0,
                [0.0, 0.0, 1.0, 1.0],
                true,
                None,
            )
            .unwrap(),
            "png",
        )
        .unwrap();
        assert_eq!(
            image::load_from_memory(&fitted)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            0
        );
    }
    /// Keep strict lifetime fixtures independent of the process-global frame
    /// pool used concurrently by other Runtime tests. The added canvas column
    /// changes every decoded frame's dimensions without changing frame timing
    pub fn isolated_gif(source: &[u8]) -> Vec<u8> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        let extra = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let decoder = image::codecs::gif::GifDecoder::new(Cursor::new(source)).unwrap();
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for frame in decoder.into_frames() {
                let frame = frame.unwrap();
                let delay = frame.delay();
                let pixels = frame.into_buffer();
                assert!(pixels.width() + extra <= 512);
                let mut marked = image::RgbaImage::from_pixel(
                    pixels.width() + extra,
                    pixels.height(),
                    image::Rgba([37, 89, 143, 255]),
                );
                image::imageops::replace(&mut marked, &pixels, 0, 0);
                encoder
                    .encode_frame(image::Frame::from_parts(marked, 0, 0, delay))
                    .unwrap();
            }
        }
        bytes
    }
    // Sample GIF
    pub fn sample_gif() -> Vec<u8> {
        let mut bytes = vec![];
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for color in [[240, 0, 0, 255], [0, 0, 240, 255]] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(4, 4, image::Rgba(color)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        bytes
    }
    #[test]
    // Verify that GIF input uses the photo editor only for a single frame
    fn gif_input_uses_the_photo_editor_only_for_a_single_frame() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.gif");
        let mut still = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut still)
            .encode_frame(image::Frame::new(image::RgbaImage::new(8, 4)))
            .unwrap();
        std::fs::write(&path, still).unwrap();
        assert!(!gif_has_animation(&path).unwrap());
        std::fs::write(&path, sample_gif()).unwrap();
        assert!(gif_has_animation(&path).unwrap());
        std::fs::write(&path, b"not a GIF").unwrap();
        assert!(gif_has_animation(&path).is_err());
    }
    #[test]
    // Verify that GIF frames keep timing loop and archive original source
    fn gif_frames_keep_timing_loop_and_archive_original_source() {
        let source = sample_gif();
        let clip = prepare(&source).unwrap();
        assert_eq!(clip.frames.len(), 2);
        assert_eq!(clip.duration_ms, 200);
        assert_eq!(clip.frame_index(Duration::from_millis(99)), 0);
        assert_eq!(clip.frame_index(Duration::from_millis(100)), 1);
        assert_eq!(clip.frame_index(Duration::from_millis(210)), 0);
        assert_eq!(clip.frame_index(Duration::from_millis(515)), 1);
        let archive = clip.archive(&source).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(archive.as_slice())).unwrap();
        assert_eq!(
            read_entry(&mut zip, "original.gif", MAX_SOURCE).unwrap(),
            source
        );
        let restored = Clip::from_archive_controlled(&archive, &Default::default()).unwrap();
        let red = restored.render(0, 112, 112, 180, "png").unwrap();
        let blue = restored.render(1, 176, 112, 0, "jpeg").unwrap();
        assert_ne!(red, blue);
        let image = image::load_from_memory(&blue).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (176, 112));
        assert!(image.get_pixel(20, 20)[2] > 200);
    }
    #[test]
    // Verify that repeated frames share storage and old archives still load
    fn repeated_frames_share_storage_and_old_archives_still_load() {
        let source = sample_gif();
        let original = prepare(&source).unwrap();
        let clip = Clip::new(
            "gif",
            vec![original.frames[0].to_vec(), original.frames[0].to_vec()],
            vec![100, 100],
        )
        .unwrap();
        assert!(Arc::ptr_eq(&clip.frames[0], &clip.frames[1]));
        let another = prepare(&source).unwrap();
        assert!(Arc::ptr_eq(&original.frames[0], &another.frames[0]));
        let bytes = clip.archive(&source).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(archive.len(), 3);
        for index in 0..archive.len() {
            assert_eq!(
                archive.by_index(index).unwrap().compression(),
                zip::CompressionMethod::Deflated
            );
        }
        let mut old = zip::ZipWriter::new(Cursor::new(vec![]));
        let options = zip::write::SimpleFileOptions::default();
        old.start_file("animation.json", options).unwrap();
        old.write_all(
            &serde_json::to_vec(&serde_json::json!({
                "schema_version": 1, "preparation_revision": 0,
                "has_original": true, "source_format": "gif", "delays_ms": [100,100]
            }))
            .unwrap(),
        )
        .unwrap();
        old.start_file("original.gif", options).unwrap();
        old.write_all(&source).unwrap();
        for i in 0..2 {
            old.start_file(format!("frames/{i:04}.image"), options)
                .unwrap();
            old.write_all(&original.frames[0]).unwrap();
        }
        let old = old.finish().unwrap().into_inner();
        assert!(Clip::from_archive_controlled(&old, &Default::default()).is_err());
        assert!(preview_archive(Cursor::new(&old)).is_err());
        assert_eq!(
            Clip::from_archive_controlled(&bytes, &Default::default())
                .unwrap()
                .frames
                .len(),
            2
        );
    }
    #[test]
    // Verify that two preparations run while waiting work is cancellable
    fn two_preparations_run_while_waiting_work_is_cancellable() {
        let gate = PreparationGate::default();
        let a = gate.lock().unwrap();
        let b = gate.lock().unwrap();
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        assert!(gate.lock_cancel(&cancelled).is_err());
        drop(a);
        drop(b);
        assert!(gate.lock_cancel(&cancelled).is_err());
        assert!(gate.lock().is_ok());
        let control = PreparationControl {
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            progress: Arc::new(|_| {}),
        };
        assert!(prepare_controlled(&sample_gif(), &control).is_err());
    }
    #[test]
    // Verify that malformed and oversized animation metadata are rejected
    fn malformed_and_oversized_animation_metadata_are_rejected() {
        assert!(prepare(b"GIF89anot-a-real-gif").is_err());
        assert!(detect(b"not an mp4 or image").is_err());
        assert!(Clip::from_archive_controlled(b"PKinvalid", &Default::default()).is_err());
        assert!(Clip::new("gif", vec![vec![1]], vec![30_001]).is_err());
        assert!(Clip::new("gif", vec![vec![1]], vec![0]).is_err());
        assert!(Clip::new(
            "gif",
            vec![vec![1]; MAX_FRAMES + 1],
            vec![1; MAX_FRAMES + 1]
        )
        .is_err());
    }
    #[test]
    #[ignore = "Requires an installed FFmpeg executable; CI installs and runs this separately"]
    // Verify that MP4 decodes to timed frames and round trips without redecoding
    fn mp4_decodes_to_timed_frames_and_round_trips_without_redecoding() {
        let source = include_bytes!("../../../tests/fixtures/media/red-blue.mp4");
        let clip = prepare(source).unwrap();
        assert_eq!(clip.source_format, "mp4");
        assert_eq!(clip.frames.len(), 20);
        let first = image::load_from_memory(&clip.frames[0]).unwrap().to_rgb8();
        let last = image::load_from_memory(&clip.frames[19]).unwrap().to_rgb8();
        assert!(first.get_pixel(0, 0)[0] > 200);
        assert!(last.get_pixel(0, 0)[2] > 200);
        let restored =
            Clip::from_archive_controlled(&clip.archive(source).unwrap(), &Default::default())
                .unwrap();
        assert_eq!(restored.frames, clip.frames);
        assert_eq!(restored.duration_ms, 1000);
    }
    #[test]
    #[ignore = "Requires an installed FFmpeg executable"]
    // Verify that MP4 rejects a forged container without hanging
    fn mp4_rejects_a_forged_container_without_hanging() {
        let invalid = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";
        assert!(prepare(invalid).is_err());
    }
}

/// Two preparations may decode concurrently; waiting clients do not hold store locks
#[derive(Default)]
struct PreparationState {
    active: usize,
    exclusive: bool,
    waiting_exclusive: usize,
}
#[derive(Default)]
pub struct PreparationGate {
    active: std::sync::Mutex<PreparationState>,
    ready: std::sync::Condvar,
}
pub struct PreparationPermit<'a>(&'a PreparationGate);
pub(crate) struct ExclusivePreparation<'a>(&'a PreparationGate);
impl PreparationGate {
    /// Device-actor preparation must never queue behind a long import/deletion
    pub(crate) fn try_lock(&self) -> Result<PreparationPermit<'_>, String> {
        let mut state = self
            .active
            .lock()
            .map_err(|_| "Preparation gate poisoned")?;
        if state.active >= 2 || state.exclusive || state.waiting_exclusive > 0 {
            return Err("Media preparation is busy; vendor animation will retry".into());
        }
        state.active += 1;
        Ok(PreparationPermit(self))
    }
    // Acquire the media lock
    pub fn lock(&self) -> Result<PreparationPermit<'_>, String> {
        self.lock_cancel(&std::sync::atomic::AtomicBool::new(false))
    }
    // Lock cancel
    pub fn lock_cancel(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<PreparationPermit<'_>, String> {
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Media preparation cancelled".into());
        }
        let mut state = self
            .active
            .lock()
            .map_err(|_| "Preparation gate poisoned")?;
        while state.active >= 2 || state.exclusive || state.waiting_exclusive > 0 {
            if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("Media preparation cancelled".into());
            }
            state = self
                .ready
                .wait_timeout(state, Duration::from_millis(50))
                .map_err(|_| "Preparation gate poisoned")?
                .0;
        }
        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Media preparation cancelled".into());
        }
        state.active += 1;
        Ok(PreparationPermit(self))
    }
    // Wait for active preparations to drain and hold exclusive preparation access
    pub(crate) fn exclusive(&self) -> Result<ExclusivePreparation<'_>, String> {
        let mut state = self
            .active
            .lock()
            .map_err(|_| "Preparation gate poisoned")?;
        state.waiting_exclusive += 1;
        while state.active > 0 || state.exclusive {
            state = self
                .ready
                .wait(state)
                .map_err(|_| "Preparation gate poisoned")?;
        }
        state.waiting_exclusive -= 1;
        state.exclusive = true;
        Ok(ExclusivePreparation(self))
    }
}
impl Drop for PreparationPermit<'_> {
    // Release this media owner when it leaves scope
    fn drop(&mut self) {
        self.0.active.lock().unwrap().active -= 1;
        self.0.ready.notify_all();
    }
}
impl Drop for ExclusivePreparation<'_> {
    // Release this media owner when it leaves scope
    fn drop(&mut self) {
        self.0.active.lock().unwrap().exclusive = false;
        self.0.ready.notify_all();
    }
}

#[cfg(test)]
mod exclusive_tests {
    use super::*;
    #[test]
    // Verify that exclusive media work waits for decoders and blocks new ones
    fn exclusive_media_work_waits_for_decoders_and_blocks_new_ones() {
        use std::sync::{atomic::AtomicBool, mpsc, Arc};
        let gate = Arc::new(PreparationGate::default());
        let first = gate.lock().unwrap();
        let second = gate.lock().unwrap();
        let (entered, receive) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let worker_gate = gate.clone();
        let worker = std::thread::spawn(move || {
            let _exclusive = worker_gate.exclusive().unwrap();
            entered.send(()).unwrap();
            released.recv().unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(25)).is_err());
        drop(first);
        assert!(receive.recv_timeout(Duration::from_millis(25)).is_err());
        drop(second);
        receive.recv_timeout(Duration::from_secs(2)).unwrap();
        let cancel = AtomicBool::new(true);
        assert!(gate.lock_cancel(&cancel).is_err());
        release.send(()).unwrap();
        worker.join().unwrap();
        let permit = gate.lock_cancel(&AtomicBool::new(false)).unwrap();
        drop(permit);
        assert!(!gate.active.lock().unwrap().exclusive);
        assert_eq!(gate.active.lock().unwrap().active, 0);
    }
}

#[cfg(test)]
mod source_rate_tests {
    use super::*;
    #[test]
    // Verify that source rates fractional timing speed trim and caps preserve phase
    fn source_rates_fractional_timing_speed_trim_and_caps_preserve_phase() {
        for rate in [23.976, 29.97, 30.0, 59.94, 60.0, 120.0] {
            let count = (rate * 2.0_f64).floor() as usize;
            let ends: Vec<u32> = (1..=count)
                .map(|index| (index as f64 * 1_000_000.0 / rate).round() as u32)
                .collect();
            let delays: Vec<u32> = ends
                .iter()
                .enumerate()
                .map(|(index, end)| *end - if index == 0 { 0 } else { ends[index - 1] })
                .collect();
            let clip = Clip::with_timing(
                "mp4",
                (0..count).map(|index| vec![index as u8]).collect(),
                delays,
                2,
                None,
            )
            .unwrap();
            let mut options = MediaOptions::default();
            assert!((options.requested_fps(&clip) - rate).abs() < 0.02);
            for index in 0..count {
                let position = if index == 0 { 0 } else { ends[index - 1] };
                assert_eq!(
                    options.frame_index(&clip, Duration::from_micros(u64::from(position))),
                    index
                );
            }
            options.max_fps = Some(15.0);
            assert_eq!(options.requested_fps(&clip), 15.0);
            assert_eq!(
                options.frame_index(&clip, Duration::from_micros(u64::from(ends[2]))),
                3
            );
            options.max_fps = None;
            options.speed = 2.0;
            assert!((options.requested_fps(&clip) - 2.0 * rate).abs() < 0.04);
            options.trim_start_ms = 100;
            options.trim_end_ms = Some(1000);
            assert_eq!(
                options.frame_index(&clip, Duration::ZERO),
                clip.frame_index(Duration::from_millis(100))
            );
        }
    }
    #[test]
    // Verify that variable delays hold frames and skip overdue source positions
    fn variable_delays_hold_frames_and_skip_overdue_source_positions() {
        let clip = Clip::with_timing(
            "mp4",
            vec![vec![1], vec![2], vec![3]],
            vec![8333, 16667, 1_000_000],
            2,
            None,
        )
        .unwrap();
        let options = MediaOptions::default();
        assert_eq!(options.frame_index(&clip, Duration::from_micros(8333)), 1);
        assert_eq!(options.frame_index(&clip, Duration::from_millis(900)), 2);
        assert_eq!(
            options.next_frame_delay(&clip, Duration::from_millis(900)),
            Duration::from_millis(125)
        );
        assert_eq!(options.frame_index(&clip, Duration::from_millis(1025)), 0);
    }
}
