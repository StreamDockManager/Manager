//! Bounded, operator-started benchmarks. Host completion is never display evidence
use crate::{DeviceInfo, Runtime};
use hardware_core::{
    DeviceCommand, FrameEncoding, OutputMode, OutputOutcome, OutputPolicy, OutputPriority,
    OutputQueueStats, OutputStatus, OutputWorker, SurfaceSubmission, Transport,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

static NEXT_RUN: AtomicU64 = AtomicU64::new(1);
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkConfig {
    /// manager, simulator, hardware, or plugins. Browser previews cannot benchmark
    pub target: String,
    #[serde(default = "default_pattern")]
    pub pattern: String,
    pub device_id: Option<String>,
    /// one_key, two_keys, all_keys, all_screens, background, key_background, all_background, mixed
    pub workload: String,
    /// individual_updates, frame_scheduling, grouped_key_updates; compare runs all three
    pub mode: String,
    pub target_fps: u8,
    pub warmup_seconds: u16,
    pub duration_seconds: u16,
    pub repetitions: u8,
    pub acknowledge_hardware: bool,
}
// Supply the default pattern
fn default_pattern() -> String {
    "numbered".into()
}
impl BenchmarkConfig {
    // Validate the supplied benchmark value
    fn validate(&self) -> Result<(), String> {
        if ![
            "individual_updates",
            "frame_scheduling",
            "grouped_key_updates",
            "compare",
        ]
        .contains(&self.mode.as_str())
        {
            return Err("Unsupported benchmark mode; use individual_updates, frame_scheduling, grouped_key_updates or compare".into());
        }
        if !["numbered", "high_detail"].contains(&self.pattern.as_str())
            || !["manager", "simulator", "hardware", "plugins"].contains(&self.target.as_str())
            || ![
                "one_key",
                "two_keys",
                "all_keys",
                "all_screens",
                "background",
                "key_background",
                "all_background",
                "mixed",
            ]
            .contains(&self.workload.as_str())
            || ![
                "individual_updates",
                "frame_scheduling",
                "grouped_key_updates",
                "compare",
            ]
            .contains(&self.mode.as_str())
            || !(1..=24).contains(&self.target_fps)
            || self.warmup_seconds > 10
            || !(1..=300).contains(&self.duration_seconds)
            || !(1..=3).contains(&self.repetitions)
        {
            return Err(
                "Invalid benchmark selection or bounds (1–24 FPS, 1–300 seconds, 1–3 repetitions)."
                    .into(),
            );
        }
        let modes = if self.mode == "compare" { 3 } else { 1 };
        if (u32::from(self.duration_seconds) + u32::from(self.warmup_seconds))
            * u32::from(self.repetitions)
            * modes
            > 1800
        {
            return Err("Benchmark matrix exceeds the 30 minute total limit.".into());
        }
        if self.target == "hardware" && !self.acknowledge_hardware {
            return Err(
                "Confirm temporary synthetic device images before starting a hardware benchmark."
                    .into(),
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BenchmarkPercentiles {
    pub count: usize,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
    pub maximum: Option<f64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BenchmarkSurface {
    pub surface_id: String,
    pub width: u16,
    pub height: u16,
    #[ts(type = "number")]
    pub generated: u64,
    #[ts(type = "number")]
    pub enqueued: u64,
    #[ts(type = "number")]
    pub transport_completed: u64,
    #[ts(type = "number")]
    pub dropped: u64,
    pub host_completed_fps: f64,
    pub displayed_fps: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BenchmarkRun {
    pub mode: String,
    pub repetition: u8,
    pub elapsed_ms: f64,
    pub requested_fps: u8,
    pub surfaces: Vec<BenchmarkSurface>,
    pub render_ms: BenchmarkPercentiles,
    pub queue_ms: BenchmarkPercentiles,
    pub transfer_ms: BenchmarkPercentiles,
    pub feedback_latency_ms: Option<BenchmarkPercentiles>,
    pub output: OutputQueueStats,
    pub generated_image_bytes: f64,
    pub policy: OutputPolicy,
    pub policy_sha256: String,
    pub asset_sha256: String,
    pub status: String,
    pub error: Option<String>,
    pub restoration: String,
    pub image_format: String,
    pub process_rss_peak_bytes: Option<f64>,
    pub process_cpu_percent: BenchmarkPercentiles,
    #[ts(type = "unknown")]
    pub cache_before: serde_json::Value,
    #[ts(type = "unknown")]
    pub cache_after: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BenchmarkReport {
    #[serde(default)]
    pub benchmark_harness_revision: String,
    pub restoration: String,
    pub schema_version: u32,
    pub manager_version: String,
    pub source_revision: String,
    pub source_dirty: String,
    pub build_profile: String,
    pub platform: String,
    pub started_utc: String,
    pub config: BenchmarkConfig,
    pub model_id: Option<String>,
    pub variant_id: Option<String>,
    pub firmware: Option<String>,
    pub usb_path: Option<String>,
    pub usb_speed: Option<String>,
    pub device_name: Option<String>,
    pub measurement_scope: String,
    pub runs: Vec<BenchmarkRun>,
    #[ts(type = "unknown | null")]
    pub plugin_fixture: Option<serde_json::Value>,
    pub limitations: Vec<String>,
    pub status: String,
}
/// Bounded snapshot of the current measured cell, never optical display evidence
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BenchmarkLiveMetrics {
    pub pending: usize,
    pub in_flight: usize,
    pub pending_bytes: usize,
    pub oldest_pending_ms: f64,
    #[ts(type = "number")]
    pub dropped: u64,
    #[ts(type = "number")]
    pub failed: u64,
    #[ts(type = "number")]
    pub outcomes_lost: u64,
    pub completed_updates_per_second: Option<f64>,
    pub queue_p95_ms: Option<f64>,
    pub transfer_p95_ms: Option<f64>,
}
impl BenchmarkLiveMetrics {
    // Compute queue, transfer and achieved-rate measurements relative to the benchmark baseline
    fn measured(
        stats: &OutputQueueStats,
        baseline: &OutputQueueStats,
        elapsed: Duration,
        queues: &[f64],
        transfers: &[f64],
    ) -> Self {
        Self {
            pending: stats.pending_count,
            in_flight: stats.in_flight_count,
            pending_bytes: stats.pending_bytes,
            oldest_pending_ms: stats.oldest_pending_ms,
            dropped: stats.total_dropped.saturating_sub(baseline.total_dropped),
            failed: stats.total_failed.saturating_sub(baseline.total_failed),
            outcomes_lost: stats.outcomes_lost.saturating_sub(baseline.outcomes_lost),
            completed_updates_per_second: (elapsed >= Duration::from_millis(250)).then(|| {
                stats
                    .total_completed
                    .saturating_sub(baseline.total_completed) as f64
                    / elapsed.as_secs_f64()
            }),
            queue_p95_ms: percentiles(queues).p95,
            transfer_p95_ms: percentiles(transfers).p95,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BenchmarkView {
    pub running: bool,
    pub cancelling: bool,
    pub run_id: String,
    pub progress: f64,
    pub phase: String,
    pub error: Option<String>,
    pub report: Option<BenchmarkReport>,
    #[serde(default)]
    #[ts(inline)]
    pub live_metrics: Option<BenchmarkLiveMetrics>,
}
/// Report-free window state. Revisions are monotonic within this Runtime instance
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BenchmarkProgress {
    pub run_id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub running: bool,
    pub cancelling: bool,
    pub phase: String,
    pub progress: f64,
    pub error_summary: Option<String>,
    #[ts(type = "number | null")]
    pub report_revision: Option<u64>,
    pub restoration_status: String,
    #[serde(default)]
    #[ts(inline)]
    pub live_metrics: Option<BenchmarkLiveMetrics>,
}
impl BenchmarkProgress {
    /// Shared projection used by the service and the reproducible serialization probe
    pub fn from_view(view: &BenchmarkView, revision: u64, report_revision: Option<u64>) -> Self {
        let error = view.error.as_deref().or_else(|| {
            view.report
                .as_ref()
                .filter(|report| report.status == "failed")
                .map(|report| {
                    report
                        .runs
                        .iter()
                        .find_map(|run| run.error.as_deref())
                        .unwrap_or("Benchmark failed; see report for details")
                })
        });
        Self {
            live_metrics: view.live_metrics.clone(),
            run_id: view.run_id.clone(),
            revision,
            running: view.running,
            cancelling: view.cancelling,
            phase: view.phase.clone(),
            progress: view.progress,
            error_summary: error.map(|message| message.chars().take(512).collect()),
            report_revision,
            restoration_status: view
                .report
                .as_ref()
                .map(|r| r.restoration.clone())
                .unwrap_or_else(|| {
                    if view.cancelling {
                        "cleanup_requested"
                    } else if view.running {
                        "not_finalized"
                    } else if view.error.is_some() {
                        "unknown"
                    } else {
                        "not_required"
                    }
                    .into()
                }),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BenchmarkResult {
    Ready {
        run_id: String,
        #[ts(type = "number")]
        report_revision: u64,
        report: Box<BenchmarkReport>,
    },
    NotReady {
        run_id: String,
    },
    Expired,
}
#[derive(Default)]
pub(crate) struct BenchmarkService {
    view: Mutex<BenchmarkView>,
    cancel: Mutex<Option<Arc<AtomicBool>>>,
    revision: AtomicU64,
    report_revision: AtomicU64,
    #[cfg(test)]
    legacy_status_calls: AtomicU64,
    #[cfg(test)]
    progress_calls: AtomicU64,
    #[cfg(test)]
    result_calls: AtomicU64,
    #[cfg(test)]
    reports_returned: AtomicU64,
}
impl BenchmarkService {
    // Report whether current work prevents releasing editor resources
    pub(crate) fn editor_retirement_blocked(&self) -> bool {
        crate::editor_retirement::unavailable_or(&self.view, |view| view.running)
    }
}
impl Runtime {
    /// Cancel while the device actor is still available for lease restoration
    /// Never hold service locks while waiting for the worker to finalize
    pub(crate) fn benchmark_shutdown(&self, timeout: Duration) -> bool {
        {
            // Same lock order as start/finalization prevents a late-start race
            let mut view = self.benchmark.view.lock().unwrap();
            if let Some(cancel) = self.benchmark.cancel.lock().unwrap().as_ref() {
                cancel.store(true, Ordering::SeqCst);
            }
            if view.running {
                view.cancelling = true;
                view.phase = "Application stopping; cancelling benchmark".into();
                self.benchmark.revision.fetch_add(1, Ordering::SeqCst);
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            if !self.benchmark.view.lock().unwrap().running {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // Read the active device benchmark status
    pub fn benchmark_status(&self) -> BenchmarkView {
        #[cfg(test)]
        self.benchmark
            .legacy_status_calls
            .fetch_add(1, Ordering::Relaxed);
        self.benchmark.view.lock().unwrap().clone()
    }
    // Read progress for the active device benchmark
    pub fn benchmark_progress(&self) -> BenchmarkProgress {
        #[cfg(test)]
        self.benchmark
            .progress_calls
            .fetch_add(1, Ordering::Relaxed);
        let view = self.benchmark.view.lock().unwrap();
        let revision = self.benchmark.revision.load(Ordering::SeqCst);
        let report_revision = self.benchmark.report_revision.load(Ordering::SeqCst);
        BenchmarkProgress::from_view(
            &view,
            revision,
            (report_revision != 0).then_some(report_revision),
        )
    }
    // Read the completed device benchmark result
    pub fn benchmark_result(&self, run_id: &str, report_revision: u64) -> BenchmarkResult {
        #[cfg(test)]
        self.benchmark.result_calls.fetch_add(1, Ordering::Relaxed);
        let view = self.benchmark.view.lock().unwrap();
        if view.run_id != run_id {
            return BenchmarkResult::Expired;
        }
        if view.running || view.report.is_none() {
            return BenchmarkResult::NotReady {
                run_id: run_id.into(),
            };
        }
        if report_revision == 0
            || self.benchmark.report_revision.load(Ordering::SeqCst) != report_revision
        {
            return BenchmarkResult::Expired;
        }
        #[cfg(test)]
        self.benchmark
            .reports_returned
            .fetch_add(1, Ordering::Relaxed);
        BenchmarkResult::Ready {
            run_id: run_id.into(),
            report_revision,
            report: Box::new(view.report.as_ref().unwrap().clone()),
        }
    }
    // Start a benchmark and return its initial progress
    pub fn benchmark_start_progress(
        self: &Arc<Self>,
        config: BenchmarkConfig,
    ) -> Result<BenchmarkProgress, String> {
        self.benchmark_start(config)?;
        Ok(self.benchmark_progress())
    }
    // Cancel a benchmark and return its current progress
    pub fn benchmark_cancel_progress(&self) -> BenchmarkProgress {
        let mut view = self.benchmark.view.lock().unwrap();
        self.cancel_benchmark_view(&mut view);
        let result_revision = self.benchmark.report_revision.load(Ordering::SeqCst);
        BenchmarkProgress::from_view(
            &view,
            self.benchmark.revision.load(Ordering::SeqCst),
            (result_revision != 0).then_some(result_revision),
        )
    }
    // Cancel benchmark view
    fn cancel_benchmark_view(&self, view: &mut BenchmarkView) {
        if let Some(cancel) = self.benchmark.cancel.lock().unwrap().as_ref() {
            cancel.store(true, Ordering::SeqCst);
        }
        if view.running {
            view.cancelling = true;
            view.phase = "Stopping at a safe output boundary and restoring device".into();
            self.benchmark.revision.fetch_add(1, Ordering::SeqCst);
        }
    }
    // Request cancellation of the active device benchmark
    pub fn benchmark_cancel(&self) -> BenchmarkView {
        // Serialize with start so cancellation cannot miss a newly installed token
        let mut view = self.benchmark.view.lock().unwrap();
        self.cancel_benchmark_view(&mut view);
        view.clone()
    }
    // Start a device benchmark with the supplied configuration
    pub fn benchmark_start(
        self: &Arc<Self>,
        mut config: BenchmarkConfig,
    ) -> Result<BenchmarkView, String> {
        config.validate()?;
        if config.target == "plugins" {
            if !["one_key", "two_keys", "all_keys", "mixed"].contains(&config.workload.as_str()) {
                return Err("Plugin fixture supports one key, two keys, 10 fixture keys or 14 mixed sections. Device LCD layouts require a device benchmark.".into());
            }
            config.pattern = "numbered".into();
            config.mode = "individual_updates".into();
            config.warmup_seconds = 0;
            config.repetitions = 1;
            config.device_id = None;
        }
        let device = if config.target == "plugins" {
            None
        } else {
            let snapshot = self.snapshot();
            let device = snapshot
                .devices
                .iter()
                .find(|d| Some(&d.id) == config.device_id.as_ref() && d.connected)
                .cloned()
                .ok_or("Select a connected device or simulator.")?;
            if config.target == "hardware" && device.simulated {
                return Err("Hardware benchmark requires a physical connected device.".into());
            }
            if config.target == "simulator" && !device.simulated {
                return Err("Simulator benchmark requires a simulated device.".into());
            }
            let capabilities = device.manifest.capabilities.frame_output.as_ref();
            if config.mode != "individual_updates"
                && !capabilities.is_some_and(|c| c.frame_scheduling)
            {
                return Err("Frame scheduling is unavailable for this model.".into());
            }
            if ["grouped_key_updates", "compare"].contains(&config.mode.as_str())
                && !capabilities.is_some_and(|c| c.grouped_key_updates)
            {
                return Err("Grouped key updates are unavailable for this model.".into());
            }
            selected_surfaces(&device.manifest, &config.workload)?;
            Some(device)
        };
        if config.target == "plugins" {
            fixture_command()?;
        }
        let mut view = self.benchmark.view.lock().unwrap();
        if self.stopping.load(Ordering::SeqCst) {
            return Err("Application is stopping; no new benchmark can start.".into());
        }
        if view.running {
            return Err("A benchmark is already running. Stop it before starting another.".into());
        }
        let run_id = NEXT_RUN.fetch_add(1, Ordering::SeqCst);
        let cancel = Arc::new(AtomicBool::new(false));
        *self.benchmark.cancel.lock().unwrap() = Some(cancel.clone());
        *view = BenchmarkView {
            running: true,
            run_id: run_id.to_string(),
            phase: "Preparing isolated workload".into(),
            ..Default::default()
        };
        self.benchmark.report_revision.store(0, Ordering::SeqCst);
        self.benchmark.revision.fetch_add(1, Ordering::SeqCst);
        let initial = view.clone();
        drop(view);
        let runtime = self.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute(&runtime, run_id, config, device, &cancel)
            }));
            let mut view = runtime.benchmark.view.lock().unwrap();
            view.running = false;
            view.cancelling = false;
            match result {
                Ok(Ok(report)) => {
                    view.phase = report.status.clone();
                    view.progress = if report.status == "completed" {
                        1.0
                    } else {
                        view.progress
                    };
                    view.report = Some(report);
                }
                Ok(Err(error)) => {
                    view.error = Some(error);
                    view.phase = "failed".into();
                }
                Err(_) => {
                    view.error =
                        Some("Benchmark worker failed; device lease cleanup was requested.".into());
                    view.phase = "failed".into();
                }
            }
            let revision = runtime.benchmark.revision.fetch_add(1, Ordering::SeqCst) + 1;
            runtime.benchmark.report_revision.store(
                if view.report.is_some() { revision } else { 0 },
                Ordering::SeqCst,
            );
            *runtime.benchmark.cancel.lock().unwrap() = None;
        });
        Ok(initial)
    }
    // Read the revision identifying the benchmark export
    pub fn benchmark_export_revision(
        &self,
        path: &str,
        format: &str,
        run_id: &str,
        report_revision: u64,
    ) -> Result<(), String> {
        match self.benchmark_result(run_id,report_revision){BenchmarkResult::Ready{report,..}=>write_report(&report,path,format),BenchmarkResult::NotReady{..}=>Err("Benchmark result is not ready.".into()),BenchmarkResult::Expired=>Err("Benchmark result expired after another run started. Review the current result before exporting.".into())}
    }
}
// Write report
fn write_report(report: &BenchmarkReport, path: &str, format: &str) -> Result<(), String> {
    let bytes = match format {
        "json" => serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?,
        "csv" => report_csv(report).into_bytes(),
        "md" => report_markdown(report).into_bytes(),
        _ => return Err("Export format must be JSON, CSV or Markdown.".into()),
    };
    let destination = std::path::Path::new(path);
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut temporary, &bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(destination).map_err(|e| e.to_string())?;
    Ok(())
}
// Publish a clamped benchmark progress value and advance its observation revision
fn progress(runtime: &Runtime, phase: String, value: f64) {
    let mut view = runtime.benchmark.view.lock().unwrap();
    view.phase = phase;
    view.progress = value.clamp(0.0, 1.0);
    runtime.benchmark.revision.fetch_add(1, Ordering::SeqCst);
}
// Compute latency percentiles from recorded benchmark samples
fn percentiles(values: &[f64]) -> BenchmarkPercentiles {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |p: f64| {
        sorted
            .get(((sorted.len().saturating_sub(1)) as f64 * p).round() as usize)
            .copied()
    };
    BenchmarkPercentiles {
        count: sorted.len(),
        p50: at(0.5),
        p95: at(0.95),
        p99: at(0.99),
        maximum: sorted.last().copied(),
    }
}
// Collect a benchmark sample
fn sample(values: &mut Vec<f64>, v: f64) {
    if values.len() < 20_000 && v.is_finite() {
        values.push(v);
    }
}
// Select output scheduling and image encoding for the requested benchmark mode
fn policy(mode: &str, fps: u8) -> (OutputPolicy, FrameEncoding) {
    (
        OutputPolicy {
            mode: if mode == "individual_updates" {
                OutputMode::IndividualUpdates
            } else {
                OutputMode::FrameScheduling
            },
            target_frame_hz: fps,
            ..Default::default()
        },
        if mode == "grouped_key_updates" {
            FrameEncoding::GroupedKeyUpdates
        } else {
            FrameEncoding::IndividualUpdates
        },
    )
}
#[derive(Clone)]
struct Surface {
    id: String,
    width: u16,
    height: u16,
    background: bool,
    rotation: u16,
}
// Choose drawable device surfaces for the requested benchmark workload
fn selected_surfaces(
    manifest: &hardware_core::ModelManifest,
    workload: &str,
) -> Result<Vec<Surface>, String> {
    let mut keys: Vec<_> = manifest
        .controls
        .iter()
        .filter(|c| c.image_slot.is_some() && (c.kind == "button" || workload == "all_screens"))
        .map(|c| Surface {
            id: c.id.clone(),
            width: c.image_width.unwrap_or(c.width),
            height: c.image_height.unwrap_or(c.height),
            background: false,
            rotation: manifest.layout.rotation,
        })
        .collect();
    if keys.is_empty() {
        keys = manifest
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some())
            .map(|c| Surface {
                id: c.id.clone(),
                width: c.image_width.unwrap_or(c.width),
                height: c.image_height.unwrap_or(c.height),
                background: false,
                rotation: manifest.layout.rotation,
            })
            .collect();
    }
    let n = match workload {
        "one_key" | "key_background" => 1,
        "two_keys" => 2,
        "background" => 0,
        _ => keys.len(),
    };
    if n > keys.len() {
        return Err("Model does not have enough image keys for this workload.".into());
    }
    keys.truncate(n);
    if workload == "mixed" {
        for c in manifest
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some() && c.kind != "button")
        {
            if !keys.iter().any(|s| s.id == c.id) {
                keys.push(Surface {
                    id: c.id.clone(),
                    width: c.image_width.unwrap_or(c.width),
                    height: c.image_height.unwrap_or(c.height),
                    background: false,
                    rotation: manifest.layout.rotation,
                });
            }
        }
    }
    if ["background", "key_background", "all_background", "mixed"].contains(&workload) {
        if let Some([width, height]) = manifest.capabilities.background_size {
            keys.push(Surface {
                id: "@background".into(),
                width,
                height,
                background: true,
                rotation: manifest.layout.rotation,
            });
        } else if workload != "mixed" {
            return Err("Model has no background surface.".into());
        }
    }
    if keys.is_empty() {
        return Err("Workload has no supported image surfaces.".into());
    }
    Ok(keys)
}
// Generate the requested benchmark data
fn generate(surface: &Surface, frame: u64, format: &str, pattern: &str) -> Result<Vec<u8>, String> {
    // Deterministic numbered/bar-coded frames, no external assets, fonts or IO
    let mut img = image::RgbImage::new(u32::from(surface.width), u32::from(surface.height));
    let surface_seed = surface.id.bytes().fold(2166136261u32, |seed, byte| {
        (seed ^ u32::from(byte)).wrapping_mul(16777619)
    });
    for (x, y, p) in img.enumerate_pixels_mut() {
        let bit = ((frame >> ((x / 8) % 16)) & 1) as u8;
        *p = if pattern == "high_detail" {
            let mut n = (x as u64).wrapping_mul(0x9e3779b97f4a7c15)
                ^ (y as u64).wrapping_mul(0xbf58476d1ce4e5b9)
                ^ frame.wrapping_mul(0x94d049bb133111eb)
                ^ 0x53444d
                ^ u64::from(surface_seed);
            n ^= n >> 30;
            n = n.wrapping_mul(0xbf58476d1ce4e5b9);
            n ^= n >> 27;
            image::Rgb([n as u8, (n >> 8) as u8, (n >> 16) as u8])
        } else {
            image::Rgb([
                ((x + frame as u32 * 3 + (surface_seed & 255)) % 256) as u8,
                ((y * 2 + frame as u32 * 7 + ((surface_seed >> 8) & 255)) % 256) as u8,
                bit * 200 + 20,
            ])
        };
    }
    let glyphs = [
        "111101101101111",
        "010110010010111",
        "111001111100111",
        "111001111001111",
        "101101111001001",
        "111100111001111",
        "111100111101111",
        "111001010010010",
        "111101111101111",
        "111101111001111",
    ];
    let text = (frame % 10000).to_string();
    let scale = (u32::from(surface.width) / (text.len() as u32 * 4 + 2)).clamp(1, 8);
    let left = (u32::from(surface.width).saturating_sub(text.len() as u32 * 4 * scale)) / 2;
    let top = (u32::from(surface.height).saturating_sub(5 * scale)) / 2;
    for (i, d) in text.bytes().enumerate() {
        for (j, b) in glyphs[(d - b'0') as usize].bytes().enumerate() {
            for dy in 0..scale {
                for dx in 0..scale {
                    let x = left + i as u32 * 4 * scale + (j as u32 % 3) * scale + dx;
                    let y = top + (j as u32 / 3) * scale + dy;
                    if x < img.width() && y < img.height() {
                        img.put_pixel(
                            x,
                            y,
                            image::Rgb(if b == b'1' {
                                [255, 255, 255]
                            } else {
                                [0, 0, 0]
                            }),
                        );
                    }
                }
            }
        }
    }
    let mut source = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut source, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    hardware_core::render::render(
        &source.into_inner(),
        surface.width,
        surface.height,
        surface.rotation,
        format,
    )
    .map_err(|e| e.to_string())
}
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn qualification_key_numbers(
    manifest: &hardware_core::ModelManifest,
) -> Result<Vec<DeviceCommand>, String> {
    selected_surfaces(manifest, "all_keys")?
        .iter()
        .enumerate()
        .map(|(index, surface)| {
            Ok(DeviceCommand::Image {
                control_id: surface.id.clone(),
                data: generate(surface, index as u64 + 1, "png", "numbered")?,
            })
        })
        .collect()
}

struct Sink;
impl Transport for Sink {
    // Write the supplied benchmark data
    fn write(&mut self, _: &[u8]) -> hardware_core::Result<()> {
        Ok(())
    }
}
struct DeviceLease<'a> {
    runtime: &'a Runtime,
    device: String,
    epoch: u64,
    run_id: u64,
    active: bool,
}
impl Drop for DeviceLease<'_> {
    // Release this benchmark owner when it leaves scope
    fn drop(&mut self) {
        if self.active {
            let _ = self
                .runtime
                .benchmark_device_stop(&self.device, self.epoch, self.run_id);
        }
    }
}
// Describe the configuration used for the benchmark report
fn report_config(config: &BenchmarkConfig, device: Option<&DeviceInfo>) -> BenchmarkConfig {
    let mut redacted = config.clone();
    if device.is_some_and(|d| !d.simulated) {
        redacted.device_id = Some("selected-physical-device".into());
    }
    redacted
}
// Execute the requested benchmark operation
fn execute(
    runtime: &Runtime,
    run_id: u64,
    config: BenchmarkConfig,
    device: Option<DeviceInfo>,
    cancel: &AtomicBool,
) -> Result<BenchmarkReport, String> {
    let mut report=BenchmarkReport{benchmark_harness_revision:option_env!("STREAM_DOCK_MANAGER_BENCHMARK_HARNESS_REVISION").unwrap_or("unknown").into(),restoration:"not_required".into(),schema_version:1,manager_version:env!("CARGO_PKG_VERSION").into(),source_revision:option_env!("STREAM_DOCK_MANAGER_SOURCE_REVISION").unwrap_or("unknown").into(),source_dirty:option_env!("STREAM_DOCK_MANAGER_SOURCE_DIRTY").unwrap_or("unknown").into(),build_profile:if cfg!(debug_assertions){"debug"}else{"release"}.into(),platform:format!("{} / {}",std::env::consts::OS,std::env::consts::ARCH),started_utc:chrono::Utc::now().to_rfc3339(),config:report_config(&config,device.as_ref()),model_id:device.as_ref().map(|d|d.model_id.clone()),variant_id:device.as_ref().map(|d|d.variant_id.clone()),firmware:device.as_ref().and_then(|d|if d.firmware.is_empty(){None}else{Some(d.firmware.clone())}),usb_path:None,usb_speed:None,device_name:device.as_ref().map(|d|d.name.clone()),measurement_scope:match config.target.as_str(){"hardware"=>"host_transport_completion","simulator"=>"simulated_transport_real_encoder","manager"=>"manager_render_only",_=>"plugin_engine_sdk_loopback"}.into(),runs:vec![],plugin_fixture:None,limitations:vec!["Host transport completion is not optically observed display FPS; displayed FPS and input-to-visible latency remain unmeasured.".into(),"USB topology, cable, power, speed and external optical observations must be recorded separately; serials and private paths are omitted.".into(),"Simulator timings do not measure USB, firmware or LCD capacity. Cross-platform physical qualification is separate.".into(),"Asset SHA256 hashes the deterministic reference frame-zero payloads, pattern, dimensions and encoding; it does not hash the full sequence, which skips late opportunities. Reference asset preparation is outside the measurement window. Render timing includes synthetic source generation and normal image conversion, not arbitrary scene composition. CPU/RSS include the whole Manager process, not an isolated benchmark process. Render cache samples are process-wide, not benchmark-only.".into(),"Latency samples are bounded to the first 20,000 observations per metric; full-run counters remain cumulative. Queue peak fields and throughput estimate are worker-session-wide (including warmup and prior activity), not per-run isolated peaks.".into()],status:"completed".into()};
    if config.target == "plugins" {
        match plugin_fixture(&config, cancel) {
            Ok(v) => report.plugin_fixture = Some(v),
            Err(e) => {
                report.status = if cancel.load(Ordering::SeqCst) {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                report.limitations.push(e);
            }
        }
        return Ok(report);
    }
    let device = device.ok_or("Device disappeared before benchmark preparation.")?;
    let modes: Vec<&str> = if config.mode == "compare" {
        vec![
            "individual_updates",
            "frame_scheduling",
            "grouped_key_updates",
        ]
    } else {
        vec![&config.mode]
    };
    let total = modes.len() * usize::from(config.repetitions);
    for mode in modes {
        for repetition in 1..=config.repetitions {
            if cancel.load(Ordering::SeqCst) {
                report.status = "cancelled".into();
                return Ok(report);
            }
            progress(
                runtime,
                format!("{} · repetition {} · preparing", mode, repetition),
                report.runs.len() as f64 / total as f64,
            );
            let result = run_cell(
                runtime,
                run_id,
                &config,
                &device,
                (mode, repetition),
                cancel,
                (report.runs.len(), total),
            );
            match result {
                Ok(cell) => {
                    let status = cell.status.clone();
                    report.restoration = cell.restoration.clone();
                    report.runs.push(cell);
                    if status != "completed" {
                        report.status = status;
                        return Ok(report);
                    }
                }
                Err(e) => {
                    report.status = if cancel.load(Ordering::SeqCst) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    if config.target == "hardware" {
                        report.restoration = "cleanup_requested_outcome_unknown".into();
                    }
                    report
                        .limitations
                        .push(format!("{mode} repetition {repetition}: {e}"));
                    return Ok(report);
                }
            }
        }
    }
    Ok(report)
}
// Run cell
fn run_cell(
    runtime: &Runtime,
    run_id: u64,
    config: &BenchmarkConfig,
    device: &DeviceInfo,
    (mode, repetition): (&str, u8),
    cancel: &AtomicBool,
    (cell, total): (usize, usize),
) -> Result<BenchmarkRun, String> {
    let (policy, encoding) = policy(mode, config.target_fps);
    let epoch = *runtime
        .connection_epochs
        .lock()
        .unwrap()
        .get(&device.id)
        .ok_or("Device disconnected.")?;
    let surfaces = selected_surfaces(&device.manifest, &config.workload)?;
    let module = hardware_registry::modules()
        .into_iter()
        .find(|m| m.manifest().model_id == device.model_id)
        .ok_or("Missing model encoder.")?;
    let contract = crate::device_contract::resolve(device)?;
    let mut session = module
        .session_for(&contract)
        .map_err(|error| error.to_string())?;
    let physical = config.target == "hardware";
    let format = device
        .manifest
        .capabilities
        .image_formats
        .first()
        .map(String::as_str)
        .unwrap_or("jpeg");
    // Hash actual deterministic reference payloads, not the varying skipped-frame sequence
    // Only one bounded surface buffer is retained, outside the measured interval
    let mut asset_hash = Sha256::new();
    asset_hash.update(b"stream-dock-manager-reference-frame-v1");
    asset_hash.update(config.pattern.as_bytes());
    asset_hash.update(format.as_bytes());
    for surface in &surfaces {
        if cancel.load(Ordering::SeqCst) {
            return Err("Benchmark cancelled during reference asset preparation.".into());
        }
        let bytes = generate(surface, 0, format, &config.pattern)?;
        asset_hash.update((surface.id.len() as u64).to_le_bytes());
        asset_hash.update(surface.id.as_bytes());
        asset_hash.update(surface.width.to_le_bytes());
        asset_hash.update(surface.height.to_le_bytes());
        asset_hash.update((bytes.len() as u64).to_le_bytes());
        asset_hash.update(bytes);
    }
    let asset_sha256 = hex::encode(asset_hash.finalize());
    let mut lease = DeviceLease {
        runtime,
        device: device.id.clone(),
        epoch,
        run_id,
        active: false,
    };
    let worker = if config.target == "simulator" {
        Some(
            OutputWorker::spawn_with_policy(Box::new(Sink), 24, policy)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    if physical {
        runtime.benchmark_device_start(&device.id, epoch, run_id, policy, encoding)?;
        lease.active = true;
    }
    runtime.benchmark.view.lock().unwrap().live_metrics = None;
    let get_stats = || -> Result<OutputQueueStats, String> {
        if physical {
            runtime.benchmark_device_stats(&device.id, epoch, run_id)
        } else {
            Ok(worker.as_ref().map(|w| w.queue_stats()).unwrap_or_default())
        }
    };
    let drain = || -> Result<Vec<OutputOutcome>, String> {
        if physical {
            runtime.benchmark_device_outcomes(&device.id, epoch, run_id)
        } else {
            Ok(worker
                .as_ref()
                .map(|w| w.drain_outcomes())
                .unwrap_or_default())
        }
    };
    let mut counters: BTreeMap<String, BenchmarkSurface> = surfaces
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                BenchmarkSurface {
                    surface_id: s.id.clone(),
                    width: s.width,
                    height: s.height,
                    ..Default::default()
                },
            )
        })
        .collect();
    let mut renders = vec![];
    let mut queues = vec![];
    let mut transfers = vec![];
    let mut image_bytes = 0u64;
    let mut frame = 0u64;
    let cache_before = runtime.cache_stats();
    let mut process = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let mut last_process_sample = Instant::now() - Duration::from_secs(1);
    let mut rss_peak: Option<f64> = None;
    let mut cpu = vec![];
    let mut measured = false;
    let mut measurement_started = Instant::now();
    let mut baseline = OutputQueueStats::default();
    let mut final_stats = OutputQueueStats::default();
    let mut last_live = Instant::now() - Duration::from_secs(1);
    let mut error = None;
    for warmup in [true, false] {
        let seconds = if warmup {
            config.warmup_seconds
        } else {
            config.duration_seconds
        };
        if !warmup {
            measured = true;
            baseline = get_stats()?;
            let _ = drain()?;
            measurement_started = Instant::now();
        }
        let start = Instant::now();
        let interval = Duration::from_secs_f64(1.0 / f64::from(config.target_fps));
        let mut next = start;
        while start.elapsed() < Duration::from_secs(u64::from(seconds)) {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            if runtime.stopping.load(Ordering::SeqCst) {
                error = Some("Application stopping.".into());
                break;
            }
            if runtime.connection_epochs.lock().unwrap().get(&device.id) != Some(&epoch) {
                error = Some("Device disconnected or reconnected during benchmark.".into());
                break;
            }
            let now = Instant::now();
            if now < next {
                std::thread::sleep((next - now).min(Duration::from_millis(10)));
                continue;
            }
            // Elapsed-time sampling skips missed opportunities rather than replaying a burst
            frame = frame.saturating_add(1);
            next = start
                + interval.mul_f64(
                    (start.elapsed().as_secs_f64() / interval.as_secs_f64()).floor() + 1.0,
                );
            if !warmup && last_process_sample.elapsed() >= Duration::from_millis(500) {
                if process.is_err() {
                    process = sysinfo::System::new();
                }
                if let Ok(process) = &mut process {
                    process.refresh_processes_specifics(
                        sysinfo::ProcessesToUpdate::Some(&[pid]),
                        true,
                        sysinfo::ProcessRefreshKind::nothing()
                            .with_cpu()
                            .with_memory(),
                    );
                    if let Some(p) = process.process(pid) {
                        rss_peak = Some(rss_peak.unwrap_or(0.0).max(p.memory() as f64));
                        sample(&mut cpu, f64::from(p.cpu_usage()));
                    }
                }
                last_process_sample = Instant::now();
            }
            let mut commands = Vec::with_capacity(surfaces.len());
            for surface in &surfaces {
                let at = Instant::now();
                let data = generate(surface, frame, format, &config.pattern)?;
                if !warmup {
                    sample(&mut renders, at.elapsed().as_secs_f64() * 1000.0);
                    image_bytes += data.len() as u64;
                    counters.get_mut(&surface.id).unwrap().generated += 1;
                }
                commands.push(if surface.background {
                    DeviceCommand::Background {
                        data,
                        x: 0,
                        y: 0,
                        width: surface.width,
                        height: surface.height,
                    }
                } else {
                    DeviceCommand::Image {
                        control_id: surface.id.clone(),
                        data,
                    }
                });
            }
            let submitted = if physical {
                runtime
                    .benchmark_device_frame(&device.id, epoch, run_id, commands)
                    .map(|_| ())
            } else if let Some(worker) = &worker {
                let uploads = session
                    .encode_frame(commands, encoding)
                    .map_err(|e| e.to_string())?;
                let submissions = uploads
                    .into_iter()
                    .zip(surfaces.iter())
                    .map(|(upload, s)| SurfaceSubmission {
                        upload,
                        surface_id: s.id.clone(),
                        content_revision: frame,
                        priority: OutputPriority::Animation,
                    })
                    .collect();
                worker
                    .submit_frame(worker.generation(), worker.scene_revision(), submissions)
                    .map_err(|e| e.to_string())
            } else {
                Ok(())
            };
            if let Err(e) = submitted {
                error = Some(e);
                break;
            }
            if !warmup && config.target != "manager" {
                for value in counters.values_mut() {
                    value.enqueued += 1;
                }
            }
            let outcomes = drain()?;
            if !warmup {
                collect_outcomes(outcomes, &mut counters, &mut queues, &mut transfers);
            }
            if last_live.elapsed() >= Duration::from_millis(500) {
                let live = if !warmup && config.target != "manager" {
                    Some(BenchmarkLiveMetrics::measured(
                        &get_stats()?,
                        &baseline,
                        measurement_started.elapsed(),
                        &queues,
                        &transfers,
                    ))
                } else {
                    None
                };
                runtime.benchmark.view.lock().unwrap().live_metrics = live;
                last_live = Instant::now();
            }
            let part = start.elapsed().as_secs_f64() / f64::from(seconds.max(1));
            progress(
                runtime,
                format!(
                    "{mode} · repetition {repetition} · {}",
                    if warmup { "warm-up" } else { "measuring" }
                ),
                (cell as f64 + if warmup { 0.0 } else { part.min(1.0) }) / total as f64,
            );
        }
        // Drain only a bounded tail. Completion window includes tail time, never inflates FPS
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            final_stats = get_stats()?;
            let outcomes = drain()?;
            if !warmup {
                collect_outcomes(outcomes, &mut counters, &mut queues, &mut transfers);
            }
            if final_stats.pending_count == 0 && final_stats.in_flight_count == 0
                || Instant::now() >= deadline
                || cancel.load(Ordering::SeqCst)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if error.is_some() || cancel.load(Ordering::SeqCst) {
            break;
        }
        if final_stats.pending_count > 0 || final_stats.in_flight_count > 0 {
            error=Some("Output did not drain within the bounded deadline; result is incomplete. Reduce workload.".into());
            break;
        }
    }
    let elapsed = if measured {
        measurement_started.elapsed().as_secs_f64().max(0.000001)
    } else {
        0.0
    };
    if !measured {
        final_stats = OutputQueueStats::default();
    }
    let mut restoration = "not_required".to_string();
    if physical {
        let result = runtime.benchmark_device_stop(&device.id, epoch, run_id);
        lease.active = false;
        match result {
            Ok(()) => restoration = "restore_requested_not_optically_verified".into(),
            Err(e) => {
                restoration = "pending_or_failed_reconnect_required".into();
                error = Some(format!("Device cleanup/restore: {e}"));
            }
        }
    }
    for value in counters.values_mut() {
        value.host_completed_fps = if elapsed > 0.0 {
            value.transport_completed as f64 / elapsed
        } else {
            0.0
        };
    }
    let mut output = final_stats;
    output.total_completed = output
        .total_completed
        .saturating_sub(baseline.total_completed);
    output.total_dropped = output.total_dropped.saturating_sub(baseline.total_dropped);
    output.total_report_bytes = output
        .total_report_bytes
        .saturating_sub(baseline.total_report_bytes);
    output.total_commits = output.total_commits.saturating_sub(baseline.total_commits);
    output.total_failed = output.total_failed.saturating_sub(baseline.total_failed);
    output.outcomes_lost = output.outcomes_lost.saturating_sub(baseline.outcomes_lost);
    Ok(BenchmarkRun {
        mode: mode.into(),
        repetition,
        elapsed_ms: elapsed * 1000.0,
        requested_fps: config.target_fps,
        surfaces: counters.into_values().collect(),
        render_ms: percentiles(&renders),
        queue_ms: percentiles(&queues),
        transfer_ms: percentiles(&transfers),
        feedback_latency_ms: None,
        output,
        generated_image_bytes: image_bytes as f64,
        policy_sha256: hex::encode(Sha256::digest(serde_json::to_vec(&policy).unwrap())),
        policy,
        asset_sha256,
        image_format: format.into(),
        process_rss_peak_bytes: rss_peak,
        process_cpu_percent: percentiles(&cpu),
        cache_before,
        cache_after: runtime.cache_stats(),
        restoration,
        status: if error.is_some() {
            "failed"
        } else if cancel.load(Ordering::SeqCst) {
            "cancelled"
        } else {
            "completed"
        }
        .into(),
        error,
    })
}
// Collect outcomes
fn collect_outcomes(
    outcomes: Vec<OutputOutcome>,
    counters: &mut BTreeMap<String, BenchmarkSurface>,
    queues: &mut Vec<f64>,
    transfers: &mut Vec<f64>,
) {
    for o in outcomes {
        if let Some(s) = counters.get_mut(&o.surface_id) {
            if o.status == OutputStatus::Completed {
                s.transport_completed += 1;
                sample(queues, o.queue_ms);
                sample(transfers, o.transfer_ms);
            } else {
                s.dropped += 1;
            }
        }
    }
}
// Copy the configured benchmark executable only after validating its size and digest
fn fixture_command() -> Result<tempfile::TempPath, String> {
    let path=std::env::var_os("STREAM_DOCK_MANAGER_BENCHMARK_FIXTURE").map(std::path::PathBuf::from).ok_or("Plugin fixture is unavailable. Configure a trusted benchmark fixture before launching Manager.")?;
    let expected = std::env::var("STREAM_DOCK_MANAGER_BENCHMARK_FIXTURE_SHA256")
        .map_err(|_| "Plugin fixture digest is unavailable.")?;
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid plugin fixture digest.".into());
    }
    let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
        return Err("Plugin fixture must be a bounded regular executable.".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|e| e.to_string())?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Plugin fixture grew beyond 64 MiB.".into());
    }
    if hex::encode(Sha256::digest(&bytes)) != expected.to_ascii_lowercase() {
        return Err("Plugin fixture digest mismatch. Rebuild the trusted fixture.".into());
    }
    let mut copy = tempfile::Builder::new()
        .prefix("stream-dock-manager-fixture-")
        .suffix(if cfg!(windows) { ".exe" } else { "" })
        .tempfile()
        .map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut copy, &bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        copy.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(copy.into_temp_path())
}
struct FixtureChild(Option<std::process::Child>);
impl std::ops::Deref for FixtureChild {
    type Target = std::process::Child;
    // Borrow the wrapped benchmark value
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for FixtureChild {
    // Borrow the wrapped benchmark value mutably
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}
impl FixtureChild {
    // Kill a still-running benchmark child and wait within the bounded termination deadline
    fn terminate(&mut self) -> Result<std::process::ExitStatus, String> {
        if let Some(status) = self.try_wait().map_err(|e| e.to_string())? {
            return Ok(status);
        }
        let _ = self.kill();
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            match self.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Err(e) => return Err(e.to_string()),
                Ok(None) => {}
            }
            if Instant::now() >= deadline {
                return Err("Fixture cleanup exceeded its bounded grace period; a background reaper owns the terminated process.".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for FixtureChild {
    // Release this benchmark owner when it leaves scope
    fn drop(&mut self) {
        if self.terminate().is_err() {
            if let Some(mut child) = self.0.take() {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }
}
// Construct the plugin fixture
fn plugin_fixture(
    config: &BenchmarkConfig,
    cancel: &AtomicBool,
) -> Result<serde_json::Value, String> {
    if cancel.load(Ordering::SeqCst) {
        return Err("Plugin fixture cancelled before launch.".into());
    }
    let path = fixture_command()?;
    if cancel.load(Ordering::SeqCst) {
        return Err("Plugin fixture cancelled before launch.".into());
    }
    let surfaces = match config.workload.as_str() {
        "one_key" | "key_background" => 1,
        "two_keys" => 2,
        "mixed" => 14,
        _ => 10,
    };
    let frames = (u32::from(config.duration_seconds) * u32::from(config.target_fps)).clamp(1, 1200);
    let process_started = Instant::now();
    let mut child = FixtureChild(Some(
        crate::native_process::command(&path)
            .args([
                "--frames",
                &frames.to_string(),
                "--surfaces",
                &surfaces.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?,
    ));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (out_tx, out_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = out_tx.send(result);
    });
    let (err_tx, err_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = err_tx.send(result);
    });
    let start = Instant::now();
    let mut stopped = false;
    let status = loop {
        if cancel.load(Ordering::SeqCst) || start.elapsed() > Duration::from_secs(65) {
            stopped = true;
            break child.terminate()?;
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.terminate();
                return Err(e.to_string());
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let bytes = out_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| "Fixture stdout did not close within the bounded deadline.")?
        .map_err(|e| e.to_string())?;
    let _errors = err_rx
        .recv_timeout(Duration::from_secs(1))
        .map_err(|_| "Fixture stderr did not close within the bounded deadline.")?
        .map_err(|e| e.to_string())?;
    if stopped {
        return Err("Plugin fixture cancelled or exceeded the 65 second deadline.".into());
    }
    if !status.success() {
        return Err("Plugin fixture failed; inspect the local fixture log independently. Raw stderr is omitted from exported evidence.".into());
    }
    if bytes.len() > 1024 * 1024 {
        return Err("Plugin fixture output exceeded 1 MiB.".into());
    }
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate_fixture_report(&value, frames, surfaces)?;
    value["manager_process_total_ms"] =
        serde_json::json!(process_started.elapsed().as_secs_f64() * 1000.0);
    Ok(value)
}
// Format benchmark measurements for a Markdown report
fn report_markdown(report: &BenchmarkReport) -> String {
    let mut out = format!(
        "# Stream Dock Manager benchmark\n\n- Status: {}\n- Scope: {}\n- Manager: {} ({}, dirty {})\n- Platform: {}\n- Model: {} / firmware {}\n- Displayed FPS and input-to-visible latency: **not measured**\n\n",
        report.status,
        report.measurement_scope,
        report.manager_version,
        report.source_revision,
        report.source_dirty,
        report.platform,
        report.model_id.as_deref().unwrap_or("not applicable"),
        report.firmware.as_deref().unwrap_or("unknown")
    );
    for run in &report.runs {
        out.push_str(&format!("## {} / repetition {}\n\nStatus: {}. Restoration: {}. Elapsed: {:.2} ms.\n\n| Surface | Requested FPS | Generated | Enqueued | Host completed | Host FPS |\n|---|---:|---:|---:|---:|---:|\n",run.mode,run.repetition,run.status,run.restoration,run.elapsed_ms));
        for s in &run.surfaces {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.2} |\n",
                s.surface_id.replace('|', "/"),
                run.requested_fps,
                s.generated,
                s.enqueued,
                s.transport_completed,
                s.host_completed_fps
            ));
        }
        out.push('\n');
    }
    out.push_str("## Limitations\n\n");
    for line in &report.limitations {
        out.push_str(&format!("- {}\n", line));
    }
    out.push_str("\n## Complete machine-readable evidence\n\n```json\n");
    out.push_str(&serde_json::to_string_pretty(report).unwrap_or_default());
    out.push_str("\n```\n");
    out
}
// Escape a value for a CSV report field
fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
// Encode benchmark measurements as CSV
fn report_csv(report: &BenchmarkReport) -> String {
    if let Some(plugin) = &report.plugin_fixture {
        return format!(
            "scope,fixture_version,source_revision,frames,surfaces,emitted,received,payload_bytes,elapsed_ms,throughput_per_second\n{},{},{},{},{},{},{},{},{},{}\n",
            csv_field(&report.measurement_scope),
            csv_field(plugin["fixture_version"].as_str().unwrap_or("unknown")),
            csv_field(plugin["source_revision"].as_str().unwrap_or("unknown")),
            plugin["frames"],
            plugin["surfaces"],
            plugin["emitted"],
            plugin["received"],
            plugin["payload_bytes"],
            plugin["elapsed_ms"],
            plugin["throughput_per_second"]
        );
    }
    let mut csv="scope,mode,repetition,surface,requested_fps,generated,enqueued,transport_completed,dropped,host_completed_fps,displayed_fps,elapsed_ms,render_p95_ms,queue_p95_ms,transfer_p95_ms,report_bytes,status\n".to_string();
    for run in &report.runs {
        for s in &run.surfaces {
            csv.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},,{},{},{},{},{},{}\n",
                csv_field(&report.measurement_scope),
                csv_field(&run.mode),
                run.repetition,
                csv_field(&s.surface_id),
                run.requested_fps,
                s.generated,
                s.enqueued,
                s.transport_completed,
                s.dropped,
                s.host_completed_fps,
                run.elapsed_ms,
                run.render_ms.p95.map(|v| v.to_string()).unwrap_or_default(),
                run.queue_ms.p95.map(|v| v.to_string()).unwrap_or_default(),
                run.transfer_ms
                    .p95
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                run.output.total_report_bytes,
                csv_field(&run.status)
            ));
        }
    }
    csv
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that live metrics use measured deltas and keep unknown latency unavailable
    fn live_metrics_use_measured_deltas_and_keep_unknown_latency_unavailable() {
        let baseline = OutputQueueStats {
            total_completed: 90,
            total_dropped: 10,
            ..Default::default()
        };
        let stats = OutputQueueStats {
            total_completed: 100,
            total_dropped: 12,
            pending_count: 3,
            in_flight_count: 1,
            ..Default::default()
        };
        let live = BenchmarkLiveMetrics::measured(
            &stats,
            &baseline,
            Duration::from_secs(2),
            &[1., 5.],
            &[3.],
        );
        assert_eq!(live.completed_updates_per_second, Some(5.));
        assert_eq!(live.dropped, 2);
        assert_eq!(live.pending, 3);
        assert_eq!(live.in_flight, 1);
        assert_eq!(live.queue_p95_ms, Some(5.));
        let warming = BenchmarkLiveMetrics::measured(&stats, &baseline, Duration::ZERO, &[], &[]);
        assert_eq!(warming.completed_updates_per_second, None);
        assert_eq!(warming.queue_p95_ms, None);
        assert_eq!(warming.transfer_p95_ms, None);
        let reset = BenchmarkLiveMetrics::measured(
            &OutputQueueStats::default(),
            &baseline,
            Duration::from_secs(1),
            &[],
            &[],
        );
        assert_eq!(reset.dropped, 0);
        assert_eq!(reset.completed_updates_per_second, Some(0.));
    }

    #[test]
    // Verify that editor retirement requires benchmark finalization after cancellation
    fn editor_retirement_requires_benchmark_finalization_after_cancellation() {
        let service = BenchmarkService::default();
        service.view.lock().unwrap().running = true;
        service.view.lock().unwrap().cancelling = true;
        assert!(service.editor_retirement_blocked());
        service.view.lock().unwrap().running = false;
        assert!(!service.editor_retirement_blocked());
    }

    // Build a short simulated-device benchmark configuration without hardware acknowledgement
    fn config() -> BenchmarkConfig {
        BenchmarkConfig {
            pattern: default_pattern(),
            target: "manager".into(),
            device_id: Some("simulator:n4-pro".into()),
            workload: "all_keys".into(),
            mode: "individual_updates".into(),
            target_fps: 10,
            warmup_seconds: 0,
            duration_seconds: 1,
            repetitions: 1,
            acknowledge_hardware: false,
        }
    }
    #[test]
    // Verify that only current modes and confirmation fields are accepted
    fn only_current_modes_and_confirmation_fields_are_accepted() {
        let mut c = config();
        for mode in [
            "individual_updates",
            "frame_scheduling",
            "grouped_key_updates",
            "compare",
        ] {
            c.mode = mode.into();
            c.validate().unwrap();
            let value = serde_json::to_value(&c).unwrap();
            assert!(value.get("acknowledge_unqualified_protocol").is_none());
            assert_eq!(
                serde_json::from_value::<BenchmarkConfig>(value)
                    .unwrap()
                    .mode,
                mode
            );
        }
        for mode in ["legacy", "device_frame", "grouped_keys", "unknown"] {
            c.mode = mode.into();
            assert!(c.validate().unwrap_err().contains("individual_updates"));
        }
        let mut value = serde_json::to_value(config()).unwrap();
        value["acknowledge_unqualified_protocol"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BenchmarkConfig>(value)
            .unwrap_err()
            .to_string()
            .contains("acknowledge_unqualified_protocol"));
    }
    #[test]
    // Verify that bounds and explicit hardware consent
    fn bounds_and_explicit_hardware_consent() {
        let mut c = config();
        assert!(c.validate().is_ok());
        c.target_fps = 25;
        assert!(c.validate().is_err());
        c.target_fps = 20;
        c.target = "hardware".into();
        assert!(c.validate().is_err());
        c.acknowledge_hardware = true;
        assert!(c.validate().is_ok());
        c.mode = "compare".into();
        c.duration_seconds = 300;
        c.repetitions = 3;
        assert!(c.validate().is_err());
    }
    #[test]
    // Verify that physical identifiers are removed from every export
    fn physical_identifiers_are_removed_from_every_export() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        r.simulate(true).unwrap();
        let mut d = r.snapshot().devices.remove(0);
        d.simulated = false;
        d.id = "n4-pro:PRIVATE-SERIAL-123".into();
        let mut c = config();
        c.device_id = Some(d.id.clone());
        let safe = report_config(&c, Some(&d));
        assert_eq!(safe.device_id.as_deref(), Some("selected-physical-device"));
        assert_eq!(c.device_id.as_deref(), Some(d.id.as_str()));
        r.benchmark_start(config()).unwrap();
        wait(&r);
        let mut report = r.benchmark_status().report.unwrap();
        report.config = safe;
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("PRIVATE-SERIAL"));
        assert!(!report_markdown(&report).contains("PRIVATE-SERIAL"));
        assert!(!report_csv(&report).contains("PRIVATE-SERIAL"));
    }
    #[cfg(target_os = "linux")]
    #[test]
    // Verify that fixture guard kills and reaps on unwind
    fn fixture_guard_kills_and_reaps_on_unwind() {
        let child = crate::native_process::command("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let id = child.id();
        let result = std::panic::catch_unwind(move || {
            let _guard = FixtureChild(Some(child));
            panic!("test worker panic");
        });
        assert!(result.is_err());
        assert!(!std::path::Path::new(&format!("/proc/{id}")).exists());
    }
    #[test]
    // Verify that patterns are deterministic and distinct
    fn patterns_are_deterministic_and_distinct() {
        let surface = Surface {
            id: "key-1".into(),
            width: 32,
            height: 32,
            background: false,
            rotation: 0,
        };
        let a = generate(&surface, 42, "png", "numbered").unwrap();
        assert_eq!(a, generate(&surface, 42, "png", "numbered").unwrap());
        let b = generate(&surface, 42, "png", "high_detail").unwrap();
        assert_eq!(b, generate(&surface, 42, "png", "high_detail").unwrap());
        assert_ne!(a, b);
        assert_ne!(b, generate(&surface, 43, "png", "high_detail").unwrap());
        let other = Surface {
            id: "key-2".into(),
            ..surface
        };
        assert_ne!(a, generate(&other, 42, "png", "numbered").unwrap());
        assert_ne!(b, generate(&other, 42, "png", "high_detail").unwrap());
    }
    #[test]
    // Verify that percentile samples are bounded and empty is unknown
    fn percentile_samples_are_bounded_and_empty_is_unknown() {
        assert!(percentiles(&[]).p95.is_none());
        assert_eq!(percentiles(&[3., 1., 2.]).p50, Some(2.));
        let mut v = vec![];
        for _ in 0..20_001 {
            sample(&mut v, 1.);
        }
        assert_eq!(v.len(), 20_000);
    }
    #[test]
    // Verify that all lcd sections follow every registered descriptor without a key cap
    fn all_lcd_sections_follow_every_registered_descriptor_without_a_key_cap() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        for manifest in hardware_registry::manifests() {
            let id = format!("simulator:{}", manifest.model_id);
            let snapshot = r.simulate_device(true, &id).unwrap();
            let device = snapshot
                .devices
                .iter()
                .find(|device| device.id == id)
                .unwrap();
            let surfaces = selected_surfaces(&device.manifest, "all_screens").unwrap();
            let expected: BTreeMap<_, _> = manifest
                .controls
                .iter()
                .filter(|control| control.image_slot.is_some())
                .map(|control| {
                    (
                        control.id.as_str(),
                        (
                            control.image_width.unwrap_or(control.width),
                            control.image_height.unwrap_or(control.height),
                        ),
                    )
                })
                .collect();
            let actual: BTreeMap<_, _> = surfaces
                .iter()
                .map(|surface| (surface.id.as_str(), (surface.width, surface.height)))
                .collect();
            assert_eq!(actual, expected, "{}", manifest.model_id);
            let mixed = selected_surfaces(&device.manifest, "mixed").unwrap();
            assert_eq!(
                mixed.len(),
                expected.len() + usize::from(manifest.capabilities.background_size.is_some())
            );
            if manifest.model_id.starts_with("n1") {
                assert_eq!(surfaces.len(), 18);
                for display in ["display-1", "display-2", "display-3"] {
                    assert!(actual.contains_key(display));
                }
                assert!(!actual.contains_key("key-16"));
                assert!(!actual.contains_key("key-17"));
                assert!(!actual.contains_key("dial-1"));
            }
            r.simulate_device(false, &id).unwrap();
        }
    }
    #[test]
    // Verify that both n1 variants benchmark all eighteen sections through each encoder mode
    fn both_n1_variants_benchmark_all_eighteen_sections_through_each_encoder_mode() {
        for model in ["n1", "n1-vsd"] {
            let root = tempfile::tempdir().unwrap();
            let r = crate::test_support::headless(root.path().into()).unwrap();
            let id = format!("simulator:{model}");
            r.simulate_device(true, &id).unwrap();
            let before = serde_json::to_vec(&r.snapshot().profiles).unwrap();
            let mut c = config();
            c.target = "simulator".into();
            c.device_id = Some(id);
            c.workload = "all_screens".into();
            c.mode = "compare".into();
            c.target_fps = 1;
            r.benchmark_start(c).unwrap();
            wait(&r);
            let report = r.benchmark_status().report.unwrap();
            assert_eq!(
                report.status, "completed",
                "{model}: {:?}",
                report.limitations
            );
            assert_eq!(report.runs.len(), 3);
            for run in report.runs {
                assert_eq!(run.surfaces.len(), 18, "{model}: {}", run.mode);
                assert!(run
                    .surfaces
                    .iter()
                    .all(|surface| surface.generated > 0 && surface.transport_completed > 0));
            }
            assert_eq!(serde_json::to_vec(&r.snapshot().profiles).unwrap(), before);
            r.shutdown();
        }
    }
    #[test]
    fn numbered_benchmark_keys_follow_panel_rotation() {
        for model in ["n4-pro", "n3-vsd", "k1-pro"] {
            let manifest = hardware_registry::manifests()
                .into_iter()
                .find(|m| m.model_id == model)
                .unwrap();
            for surface in selected_surfaces(&manifest, "all_keys").unwrap() {
                let logical = Surface {
                    rotation: 0,
                    ..surface.clone()
                };
                let pixels = |s: &Surface| {
                    image::load_from_memory(&generate(s, 7, "png", "numbered").unwrap())
                        .unwrap()
                        .into_rgb8()
                };
                let expected = if model == "n4-pro" {
                    image::imageops::rotate180(&pixels(&logical))
                } else {
                    pixels(&logical)
                };
                assert_eq!(pixels(&surface), expected, "{model} {}", surface.id);
            }
        }
    }

    #[test]
    // Verify that n4 workload maps all keys touch and background
    fn n4_workload_maps_all_keys_touch_and_background() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        r.simulate(true).unwrap();
        let d = r.snapshot().devices.remove(0);
        assert_eq!(
            selected_surfaces(&d.manifest, "all_keys").unwrap().len(),
            10
        );
        assert_eq!(selected_surfaces(&d.manifest, "mixed").unwrap().len(), 15);
        assert_eq!(
            selected_surfaces(&d.manifest, "background").unwrap().len(),
            1
        );
    }
    #[test]
    // Verify that manager benchmark is bounded and does not modify profiles
    fn manager_benchmark_is_bounded_and_does_not_modify_profiles() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        r.simulate(true).unwrap();
        let before = serde_json::to_vec(&r.snapshot().profiles).unwrap();
        r.benchmark_start(config()).unwrap();
        assert!(r.benchmark_start(config()).is_err());
        let deadline = Instant::now() + Duration::from_secs(5);
        while r.benchmark_status().running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let view = r.benchmark_status();
        assert!(!view.running);
        let report = view.report.unwrap();
        assert_eq!(report.status, "completed");
        assert_eq!(report.measurement_scope, "manager_render_only");
        assert!(report.runs[0]
            .surfaces
            .iter()
            .all(|s| s.displayed_fps.is_none() && s.transport_completed == 0));
        assert_eq!(before, serde_json::to_vec(&r.snapshot().profiles).unwrap());
    }
    #[test]
    // Verify that cancel and repeat are safe
    fn cancel_and_repeat_are_safe() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        r.simulate(true).unwrap();
        let mut c = config();
        c.duration_seconds = 30;
        r.benchmark_start(c).unwrap();
        r.benchmark_cancel();
        r.benchmark_cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        while r.benchmark_status().running && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!r.benchmark_status().running);
        assert_eq!(r.benchmark_status().report.unwrap().status, "cancelled");
    }
    #[test]
    // Verify that shutdown cancels active simulator and rejects restart
    fn shutdown_cancels_active_simulator_and_rejects_restart() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        r.simulate(true).unwrap();
        let mut c = config();
        c.target = "simulator".into();
        c.workload = "one_key".into();
        c.duration_seconds = 30;
        r.benchmark_start(c.clone()).unwrap();
        let ready = Instant::now() + Duration::from_secs(2);
        while !r.benchmark_status().phase.contains("measuring") && Instant::now() < ready {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(r.benchmark_status().running);
        let started = Instant::now();
        r.shutdown();
        assert!(started.elapsed() < Duration::from_secs(5));
        let status = r.benchmark_status();
        assert!(!status.running);
        assert_eq!(status.report.unwrap().status, "cancelled");
        assert!(r.benchmark_start(c).is_err());
        r.shutdown();
    }

    // Wait for the benchmark condition or result
    fn wait(r: &Runtime) {
        let until = Instant::now() + Duration::from_secs(10);
        while r.benchmark_status().running && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!r.benchmark_status().running);
    }
    #[test]
    // Verify that simulator warmup is excluded and exports are truthful
    fn simulator_warmup_is_excluded_and_exports_are_truthful() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        r.simulate(true).unwrap();
        let mut c = config();
        c.target = "simulator".into();
        c.workload = "one_key".into();
        c.warmup_seconds = 1;
        r.benchmark_start(c).unwrap();
        wait(&r);
        let report = r.benchmark_status().report.unwrap();
        assert_eq!(report.status, "completed");
        let run = &report.runs[0];
        assert!(run.elapsed_ms >= 1000. && run.elapsed_ms < 4000.);
        assert!(run.surfaces[0].transport_completed > 0);
        assert!(run.surfaces[0].transport_completed <= run.surfaces[0].enqueued);
        assert_eq!(
            run.output.total_completed,
            run.surfaces
                .iter()
                .map(|s| s.transport_completed)
                .sum::<u64>()
        );
        assert_eq!(report.restoration, "not_required");
        for f in ["json", "csv", "md"] {
            let path = root.path().join(format!("report.{f}"));
            r.benchmark_export_revision(
                path.to_str().unwrap(),
                f,
                &r.benchmark_status().run_id,
                r.benchmark.report_revision.load(Ordering::SeqCst),
            )
            .unwrap();
            let bytes = std::fs::read_to_string(path).unwrap();
            assert!(bytes.contains("simulated_transport_real_encoder"));
        }
        assert!(report_markdown(&report).contains("not measured"));
    }
    #[test]
    // Verify that cancelled warmup excludes transport counters
    fn cancelled_warmup_excludes_transport_counters() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        r.simulate(true).unwrap();
        let mut c = config();
        c.target = "simulator".into();
        c.workload = "one_key".into();
        c.warmup_seconds = 5;
        r.benchmark_start(c).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        r.benchmark_cancel();
        wait(&r);
        let report = r.benchmark_status().report.unwrap();
        assert_eq!(report.status, "cancelled");
        let run = &report.runs[0];
        assert_eq!(run.output.total_completed, 0);
        assert_eq!(run.elapsed_ms, 0.);
        assert!(run
            .surfaces
            .iter()
            .all(|s| s.transport_completed == 0 && s.enqueued == 0));
    }
}

// Validate the pinned v1 fixture contract, not arbitrary installed-plugin reports
fn validate_fixture_report(
    value: &serde_json::Value,
    frames: u32,
    surfaces: u32,
) -> Result<(), String> {
    let invalid = |field: &str| format!("Invalid plugin fixture report: {field}");
    if !(1..=1200).contains(&frames) || !(1..=14).contains(&surfaces) {
        return Err(invalid("requested workload bounds"));
    }
    for (key, expected) in [
        ("schema_version", 1),
        ("frames", u64::from(frames)),
        ("surfaces", u64::from(surfaces)),
    ] {
        if value.get(key).and_then(|v| v.as_u64()) != Some(expected) {
            return Err(invalid(key));
        }
    }
    for (key, expected) in [
        ("fixture_version", "1"),
        ("measurement_scope", "plugin_engine_sdk_loopback"),
    ] {
        if value.get(key).and_then(|v| v.as_str()) != Some(expected) {
            return Err(invalid(key));
        }
    }
    let events = u64::from(surfaces) * (5 + 2 * u64::from(frames - 1));
    let revisions = u64::from(frames) * u64::from(surfaces);
    for (key, expected) in [
        ("emitted", events),
        ("received", events),
        ("display_revisions", revisions),
        ("duplicate_revisions_suppressed", revisions),
    ] {
        if value.get(key).and_then(|v| v.as_u64()) != Some(expected) {
            return Err(invalid(key));
        }
    }
    // The fixture host bounds each display event to 64 KiB; bytes exclude barriers
    if !value
        .get("payload_bytes")
        .and_then(|v| v.as_u64())
        .is_some_and(|n| n > 0 && n <= events * 65536)
    {
        return Err(invalid("payload_bytes"));
    }
    let number = |key: &str| -> Result<f64, String> {
        value
            .get(key)
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| invalid(key))
    };
    let startup = number("startup_ms")?;
    if startup > 65000.0 {
        return Err(invalid("startup_ms deadline"));
    }
    let elapsed = number("elapsed_ms")?;
    if elapsed > 65000.0 {
        return Err(invalid("elapsed_ms deadline"));
    }
    let rate = number("throughput_per_second")?;
    let expected_rate = events as f64 * 1000.0 / elapsed;
    if (rate - expected_rate).abs() > expected_rate * 0.000001 {
        return Err(invalid(
            "throughput_per_second inconsistent with counts/time",
        ));
    }
    let mut previous = 0.0;
    for key in ["p50", "p95", "p99"] {
        let latency = value
            .get("latency_ms")
            .and_then(|v| v.get(key))
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n >= previous && *n <= elapsed)
            .ok_or_else(|| invalid("latency_ms"))?;
        previous = latency;
    }
    for key in ["source_revision", "os", "arch", "go_version"] {
        if !value
            .get(key)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        {
            return Err(invalid(key));
        }
    }
    if !matches!(
        value.get("source_dirty").and_then(|v| v.as_str()),
        Some("true" | "false" | "unknown")
    ) {
        return Err(invalid("source_dirty"));
    }
    if !value
        .get("limitations")
        .and_then(|v| v.as_array())
        .is_some_and(|items| {
            !items.is_empty()
                && items.len() <= 32
                && items
                    .iter()
                    .all(|v| v.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 4096))
        })
    {
        return Err(invalid("limitations"));
    }
    Ok(())
}

#[cfg(test)]
mod fixture_report_tests {
    use super::*;
    // Assemble the benchmark report
    fn report() -> serde_json::Value {
        serde_json::json!({"schema_version":1,"fixture_version":"1","measurement_scope":"plugin_engine_sdk_loopback",
            "frames":3,"surfaces":2,"emitted":18,"received":18,"display_revisions":6,"duplicate_revisions_suppressed":6,
            "payload_bytes":1800,"startup_ms":1.0,"elapsed_ms":100.0,"throughput_per_second":180.0,"latency_ms":{"p50":1.0,"p95":2.0,"p99":3.0},
            "source_revision":"unknown","source_dirty":"unknown","os":"linux","arch":"amd64","go_version":"go1.27.3","limitations":["Not hardware FPS"]})
    }
    #[test]
    // Verify that valid fixture contract
    fn valid_fixture_contract() {
        assert!(validate_fixture_report(&report(), 3, 2).is_ok());
    }
    #[test]
    // Verify that rejects malformed versions counts and timings
    fn rejects_malformed_versions_counts_and_timings() {
        for (key, value) in [
            ("schema_version", serde_json::json!(2)),
            ("fixture_version", serde_json::json!("2")),
            ("frames", serde_json::json!(4)),
            ("surfaces", serde_json::json!(1)),
            ("emitted", serde_json::json!(19)),
            ("received", serde_json::json!(17)),
            ("display_revisions", serde_json::json!(5)),
            ("duplicate_revisions_suppressed", serde_json::json!(0)),
            ("payload_bytes", serde_json::json!(u64::MAX)),
            ("elapsed_ms", serde_json::json!(0)),
            ("startup_ms", serde_json::json!(-1)),
            ("throughput_per_second", serde_json::json!(999.0)),
            ("source_dirty", serde_json::json!(true)),
            ("limitations", serde_json::json!([])),
        ] {
            let mut value_report = report();
            value_report[key] = value;
            assert!(
                validate_fixture_report(&value_report, 3, 2).is_err(),
                "accepted {key}"
            );
        }
        assert!(validate_fixture_report(&serde_json::json!({}), 3, 2).is_err());
        assert!(validate_fixture_report(&report(), 0, 2).is_err());
    }
    #[test]
    // Verify that rejects unordered and nonfinite latency
    fn rejects_unordered_and_nonfinite_latency() {
        for value in [
            serde_json::json!({"p50":3,"p95":2,"p99":4}),
            serde_json::json!({"p50":1,"p95":2,"p99":101}),
            serde_json::json!({"p50":null,"p95":2,"p99":3}),
        ] {
            let mut r = report();
            r["latency_ms"] = value;
            assert!(validate_fixture_report(&r, 3, 2).is_err());
        }
    }
}

#[cfg(test)]
mod progress_contract_tests {
    use super::*;
    #[test]
    // Verify that reports without optional harness provenance remain readable
    fn reports_without_optional_harness_provenance_remain_readable() {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/benchmark/completed-simulator-report.json"
        ))
        .unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("benchmark_harness_revision");
        let report: BenchmarkReport = serde_json::from_value(value).unwrap();
        assert!(report.benchmark_harness_revision.is_empty());
        assert_eq!(report.schema_version, 1);
        assert!(!report.runs.is_empty());
        assert_eq!(report.status, "completed");
    }
    // Construct an isolated benchmark fixture
    fn fixture(runtime: &Runtime) {
        let report: BenchmarkReport = serde_json::from_str(include_str!(
            "../../../tests/fixtures/benchmark/completed-simulator-report.json"
        ))
        .unwrap();
        *runtime.benchmark.view.lock().unwrap() = BenchmarkView {
            running: false,
            cancelling: false,
            run_id: "fixture".into(),
            progress: 1.,
            phase: "completed".into(),
            error: None,
            report: Some(report),
            live_metrics: None,
        };
        runtime.benchmark.revision.store(7, Ordering::SeqCst);
        runtime.benchmark.report_revision.store(7, Ordering::SeqCst);
    }
    #[test]
    // Verify that progress does not clone or transfer the report and legacy status remains compatible
    fn progress_does_not_clone_or_transfer_the_report_and_legacy_status_remains_compatible() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        fixture(&r);
        let full = serde_json::to_vec(&r.benchmark_status()).unwrap();
        for _ in 0..500 {
            let p = r.benchmark_progress();
            let bytes = serde_json::to_vec(&p).unwrap();
            assert!(bytes.len() * 10 < full.len());
            assert!(!String::from_utf8(bytes).unwrap().contains("surfaces"));
            assert_eq!(p.report_revision, Some(7));
            assert_eq!(p.restoration_status, "not_required");
        }
        assert_eq!(r.benchmark.progress_calls.load(Ordering::Relaxed), 500);
        assert_eq!(r.benchmark.result_calls.load(Ordering::Relaxed), 0);
        assert_eq!(r.benchmark.reports_returned.load(Ordering::Relaxed), 0);
        assert_eq!(r.benchmark.legacy_status_calls.load(Ordering::Relaxed), 1);
        match r.benchmark_result("fixture", 7) {
            BenchmarkResult::Ready { report, .. } => assert_eq!(report.runs.len(), 3),
            _ => panic!("ready report missing"),
        }
        assert_eq!(r.benchmark.reports_returned.load(Ordering::Relaxed), 1);
    }
    #[test]
    // Verify that result and export reject stale revisions without replacing authoritative content
    fn result_and_export_reject_stale_revisions_without_replacing_authoritative_content() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        fixture(&r);
        assert!(matches!(
            r.benchmark_result("fixture", 6),
            BenchmarkResult::Expired
        ));
        assert!(matches!(
            r.benchmark_result("other", 7),
            BenchmarkResult::Expired
        ));
        let stale = root.path().join("stale.json");
        assert!(r
            .benchmark_export_revision(stale.to_str().unwrap(), "json", "fixture", 6)
            .is_err());
        assert!(!stale.exists());
        for format in ["json", "csv", "md"] {
            let a = root.path().join(format!("old.{format}"));
            let b = root.path().join(format!("new.{format}"));
            write_report(
                &r.benchmark_status().report.unwrap(),
                a.to_str().unwrap(),
                format,
            )
            .unwrap();
            r.benchmark_export_revision(b.to_str().unwrap(), format, "fixture", 7)
                .unwrap();
            assert_eq!(std::fs::read(a).unwrap(), std::fs::read(b).unwrap());
        }
        {
            let mut v = r.benchmark.view.lock().unwrap();
            v.run_id = "next".into();
            v.running = true;
            v.report = None;
            r.benchmark.report_revision.store(0, Ordering::SeqCst);
            r.benchmark.revision.store(8, Ordering::SeqCst);
        }
        assert!(matches!(
            r.benchmark_result("fixture", 7),
            BenchmarkResult::Expired
        ));
        assert!(matches!(
            r.benchmark_result("next", 0),
            BenchmarkResult::NotReady { .. }
        ));
        let before = r.benchmark_progress();
        let cancelled = r.benchmark_cancel_progress();
        assert!(cancelled.revision > before.revision);
        assert!(cancelled.cancelling);
        assert_eq!(cancelled.restoration_status, "cleanup_requested");
    }
    #[test]
    // Verify that failed run summary and restoration remain available without result fetch
    fn failed_run_summary_and_restoration_remain_available_without_result_fetch() {
        let root = tempfile::tempdir().unwrap();
        let r = crate::test_support::headless(root.path().into()).unwrap();
        fixture(&r);
        {
            let mut view = r.benchmark.view.lock().unwrap();
            let report = view.report.as_mut().unwrap();
            report.status = "failed".into();
            report.restoration = "pending_or_failed_reconnect_required".into();
            report
                .limitations
                .push("device disconnected before finalization".into());
        }
        let p = r.benchmark_progress();
        assert_eq!(
            p.error_summary.as_deref(),
            Some("Benchmark failed; see report for details")
        );
        assert_eq!(p.restoration_status, "pending_or_failed_reconnect_required");
        assert_eq!(r.benchmark.result_calls.load(Ordering::Relaxed), 0);
        {
            let mut view = r.benchmark.view.lock().unwrap();
            view.report.as_mut().unwrap().runs[0].error = Some("x".repeat(1024));
        }
        assert_eq!(r.benchmark_progress().error_summary.unwrap().len(), 512);
    }
}
