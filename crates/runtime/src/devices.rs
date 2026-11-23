use crate::{DeviceInfo, Runtime};
use hardware_core::*;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{mpsc, Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{FeedbackKind, HostEffect};
#[path = "device_blackout.rs"]
mod blackout;
#[path = "devices/discovery.rs"]
mod discovery;
#[cfg(target_os = "macos")]
#[path = "devices/hotplug.rs"]
mod hotplug;
#[path = "devices/input_readiness.rs"]
mod input_readiness;
#[cfg(test)]
#[path = "devices/n4_capture_tests.rs"]
mod n4_capture_tests;
#[cfg(all(test, target_os = "macos"))]
#[path = "devices/palette_physical_support.rs"]
mod palette_physical_support;
#[cfg(all(test, target_os = "macos"))]
#[path = "devices/palette_profile.rs"]
pub(crate) mod palette_profile;
#[cfg(all(test, target_os = "macos"))]
#[path = "devices/render_qualification.rs"]
mod render_qualification;
#[path = "device_transitions.rs"]
mod transitions;

pub(crate) enum Request {
    Wake,
    Command(String, u64, DeviceCommand),
    Keyboard {
        device: String,
        epoch: u64,
        contract_key: String,
        transaction: DeviceTransaction,
        deadline: Instant,
        reply: mpsc::Sender<std::result::Result<(), crate::keyboard::TransferError>>,
    },
    BootLogo {
        device: String,
        epoch: u64,
        contract_key: String,
        transaction: DeviceTransaction,
        reply: mpsc::Sender<std::result::Result<(), String>>,
    },
    Restore,
    RestoreDevice(String, u64),
    RefreshPluginBindings {
        device: String,
        epoch: u64,
        sources: Vec<String>,
    },
    Shutdown(mpsc::Sender<()>),
    Lost(String, u64),
    BenchmarkStart {
        device: String,
        epoch: u64,
        run_id: u64,
        policy: OutputPolicy,
        encoding: FrameEncoding,
        reply: mpsc::Sender<std::result::Result<(), String>>,
    },
    BenchmarkFrame {
        device: String,
        epoch: u64,
        run_id: u64,
        commands: Vec<DeviceCommand>,
        reply: mpsc::Sender<std::result::Result<OutputQueueStats, String>>,
    },
    BenchmarkStats {
        device: String,
        epoch: u64,
        run_id: u64,
        reply: mpsc::Sender<std::result::Result<OutputQueueStats, String>>,
    },
    BenchmarkOutcomes {
        device: String,
        epoch: u64,
        run_id: u64,
        reply: mpsc::Sender<std::result::Result<Vec<OutputOutcome>, String>>,
    },
    BenchmarkStop {
        device: String,
        epoch: u64,
        run_id: u64,
        reply: mpsc::Sender<std::result::Result<(), String>>,
    },
    Playback {
        device: String,
        control: Option<String>,
        asset: String,
        lease: crate::asset_leases::AssetLease,
        format: String,
        epoch: u64,
    },
}
// Extract the HID identifiers used to match a hardware interface
pub fn identity(info: &hidapi::DeviceInfo) -> HidIdentity {
    HidIdentity {
        vendor_id: info.vendor_id(),
        product_id: info.product_id(),
        interface: info.interface_number(),
        usage_page: info.usage_page(),
        usage: info.usage(),
    }
}
struct HidTransport {
    device: Arc<Mutex<hidapi::HidDevice>>,
    runtime: Weak<Runtime>,
}
impl Transport for HidTransport {
    // Write the supplied devices data
    fn write(&mut self, report: &[u8]) -> Result<()> {
        let waiting = Instant::now();
        let (result, lock_wait, write_time) = {
            let device = self.device.lock().unwrap();
            let lock_wait = waiting.elapsed();
            let writing = Instant::now();
            let result = device.write(report);
            (result, lock_wait, writing.elapsed())
        };
        // Keep diagnostic sampling outside the shared input/output lock. These
        // bounded timings distinguish host lock contention from driver latency
        if let Some(runtime) = self.runtime.upgrade() {
            runtime
                .metrics
                .record("hid_output_lock_wait_ms", lock_wait.as_secs_f64() * 1000.0);
            runtime
                .metrics
                .record("hid_output_write_ms", write_time.as_secs_f64() * 1000.0);
        }
        let written = result.map_err(|e| Error::Transport(e.to_string()))?;
        if written != report.len() {
            return Err(Error::Transport("short HID write".into()));
        }
        Ok(())
    }
}
struct Connection {
    runtime: Weak<Runtime>,
    _mailbox_registration: Option<crate::device_mailbox::Registration>,
    device_id: String,
    sleeping: bool,
    navigation_fallback: crate::keyboard_navigation::BlankPageTimer,
    restore_retry: Option<Instant>,
    boot_logo_restore: Option<Instant>,
    transition: Option<transitions::Transition>,
    worker: OutputWorker,
    boot_logo: Option<crate::boot_logo::PendingUpload>,
    keyboard: Option<crate::keyboard::PendingApply>,
    blackout: blackout::Blackout,
    session: Arc<Mutex<Box<dyn HardwareSession>>>,
    input_stop: Arc<std::sync::atomic::AtomicBool>,
    input_wake: Arc<input_readiness::Signal>,
    benchmark_input: Arc<Mutex<bool>>,
    input_thread: Option<std::thread::JoinHandle<()>>,
    epoch: u64,
    heartbeat: Instant,
    manifest: ModelManifest,
    contract_identity: String,
    playbacks: std::collections::BTreeMap<String, Playback>,
    playback_cursor: usize,
    next_frame: Instant,
    animation_rate: AnimationRate,
    last_queue_ms: f64,
    policy: OutputPolicy,
    encoding: FrameEncoding,
    scene_revision: u64,
    draft_preview: Option<(String, Vec<String>, bool)>,
    draft_sample: Instant,
    static_palette: StaticPaletteGate,
    content_revision: u64,
    delivery: HashMap<String, Delivery>,
    benchmark: Option<(u64, Instant)>,
    lost_outcomes: u64,
    force_individual_updates: bool,
    applied_vibration: Option<(bool, BTreeMap<String, bool>)>,
}
// Keep static scene reuse proof local to its physical connection
#[derive(Default)]
struct StaticPaletteGate {
    scene: Option<crate::render_scene::RenderSceneContext>,
    output: Option<(u64, u64, usize)>,
    visits: u64,
    captures: u64,
    skipped: u64,
}
// Classify only built-in static palette scenes and preserve timed refresh for unknown content
fn static_palette_scene(
    scene: &crate::render_scene::RenderSceneContext,
    profile: Option<&crate::Profile>,
) -> bool {
    scene.draft.as_ref().is_some_and(|draft| {
        draft.palette.is_some() && draft.bindings.is_empty() && draft.background.is_none()
    }) && scene.theme_wallpaper.is_none()
        && profile.is_none_or(|profile| profile.background_asset.is_none())
        && scene.overrides.is_empty()
        && scene.visuals.is_empty()
        && scene.fallbacks.is_empty()
        && scene.page.as_ref().is_some_and(|page| {
            page.bindings.values().all(|binding| {
                matches!(binding.action, crate::actions::Action::None)
                    && binding.asset.is_none()
                    && binding.appearance.is_none()
                    && binding.plugin_appearance.is_none()
                    && binding.composition.is_none()
                    && binding
                        .feedback
                        .as_deref()
                        .is_none_or(|feedback| feedback == "off")
                    && !binding.title.contains(['{', '}'])
            })
        })
}
struct Delivery {
    presentation: Option<Instant>,
    origin: Option<crate::input_trace::InputOrigin>,
    digest: [u8; 32],
    revision: u64,
    pending: bool,
    completed: bool,
}
struct AnimationRate {
    healthy_since: Option<Instant>,
    divisor: u32,
    overloaded_since: Option<Instant>,
    last_adjustment: Option<Instant>,
}
impl Default for AnimationRate {
    // Construct the default devices value
    fn default() -> Self {
        Self {
            healthy_since: None,
            divisor: 1,
            overloaded_since: None,
            last_adjustment: None,
        }
    }
}
impl AnimationRate {
    // Update devices from the new observation
    fn observe(&mut self, depth: usize, queue_ms: f64, now: Instant) {
        if depth > 2 || queue_ms > 150.0 {
            if self.last_adjustment.is_none_or(|last| {
                now.saturating_duration_since(last) >= Duration::from_millis(250)
            }) {
                self.divisor = (self.divisor * 2).min(1024);
                self.last_adjustment = Some(now);
            }
            self.overloaded_since.get_or_insert(now);
            self.healthy_since = None;
        } else if depth <= 1 && queue_ms <= 75.0 {
            self.overloaded_since = None;
            let since = self.healthy_since.get_or_insert(now);
            if now.saturating_duration_since(*since) >= Duration::from_secs(3) {
                self.divisor = (self.divisor / 2).max(1);
                self.healthy_since = Some(now);
            }
        } else {
            self.healthy_since = None;
        }
    }
    // Reduce animation cadence under sustained overload without exceeding the source rate
    fn rate(&self, source: f64, preferred: f64, now: Instant) -> f64 {
        let reduced = source / f64::from(self.divisor);
        if self
            .overloaded_since
            .is_some_and(|since| now.saturating_duration_since(since) >= Duration::from_secs(1))
        {
            reduced.max(0.25).min(source)
        } else {
            reduced.max(preferred.min(source)).min(source)
        }
    }
}
struct Playback {
    vendor_started: Option<Duration>,
    asset: String,
    // Persist across render cycles: disposable-cache eviction must not force
    // active media to reload or release its original encoded allocations
    clip: Option<crate::media_budget::ProtectedClip>,
    _assets: crate::asset_leases::AssetLease,
    control: Option<String>,
    format: String,
    last_frame: Option<usize>,
    next_sample: Duration,
    pending_frame: Option<(usize, u64)>,
    media: crate::media::MediaOptions,
}
impl Playback {
    // Measure playback time relative to vendor animation activation when present
    fn elapsed(&self, now: Duration) -> Duration {
        self.vendor_started
            .map_or(now, |started| now.saturating_sub(started))
    }
    // Reuse the pinned vendor clip or resolve media through the current render cycle
    fn source_clip(
        &self,
        profiles: &crate::profiles::ProfileStore,
        cycle: &crate::profiles::ClipCycle,
    ) -> std::result::Result<Option<Arc<crate::media::Clip>>, String> {
        if self.vendor_started.is_some() {
            return Ok(self.clip.as_ref().map(|clip| clip.clip.clone()));
        }
        cycle.get(profiles, &self.asset)
    }
}
impl Connection {
    // Report the image admission condition for devices
    fn image_admission(&self) -> Option<Arc<dyn Fn() -> bool + Send + Sync>> {
        let weak = self.runtime.clone();
        let id = self.device_id.clone();
        let epoch = self.epoch;
        let expected = self.contract_identity.clone();
        Some(Arc::new(move || {
            weak.upgrade().is_some_and(|runtime| {
                if runtime.connection_epochs.lock().unwrap().get(&id) != Some(&epoch) {
                    return false;
                }
                let device = runtime
                    .state
                    .lock()
                    .unwrap()
                    .devices
                    .iter()
                    .find(|device| device.id == id)
                    .cloned();
                device
                    .as_ref()
                    .and_then(|device| crate::device_contract::resolve(device).ok())
                    .is_some_and(|contract| {
                        contract.identity() == expected
                            && contract.image_output_disabled_reason.is_none()
                    })
            })
        }))
    }
    // Start playback
    fn start_playback(&mut self, key: String, playback: Playback) {
        if self.playbacks.is_empty() {
            // Idle time has no animation opportunities to miss. Keep the current
            // deadline when adding/replacing a surface during active playback
            self.next_frame = Instant::now();
        }
        self.playbacks.insert(key, playback);
    }
}
impl Drop for Connection {
    // Release this devices owner when it leaves scope
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            self.blackout.disconnect(&runtime, &self.device_id);
        }
        self.stop_benchmark();
        self.input_stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.input_wake.stop();
        if let Some(thread) = self.input_thread.take() {
            let _ = thread.join();
        }
    }
}
/// Hold the connection-local lease gate through dispatch. Benchmark acquisition
/// therefore cannot race a decoded event into the user's existing action bindings
/// Returns true when an event was observed but its normal side effects suppressed
fn guarded_input(gate: &Mutex<bool>, dispatch: impl FnOnce()) -> bool {
    let owned = gate.lock().unwrap();
    if *owned {
        true
    } else {
        dispatch();
        false
    }
}

/// Shared handles must not hold the output lock while waiting for input. Keep
/// readiness callbacks park outside either handle lock and drain reports without delay
/// Unavailable readiness preserves the existing shared or separate handle polling cadence
fn read_hid_input<T, E>(
    hid: &Mutex<T>,
    shared_handle: bool,
    readiness: bool,
    read: impl FnOnce(&T, i32) -> std::result::Result<usize, E>,
    idle: impl FnOnce(Duration),
) -> std::result::Result<usize, E> {
    let policy = input_readiness::policy(shared_handle || readiness);
    debug_assert!(!policy.reason.is_empty());
    let result = {
        let device = hid.lock().unwrap();
        read(&device, policy.driver_timeout_ms)
    };
    if let Some(delay) = policy.idle_delay.filter(|_| matches!(result, Ok(0))) {
        idle(delay);
    }
    result
}

// Query firmware using the current devices state
fn query_firmware(
    hid: &Mutex<hidapi::HidDevice>,
    report: &hardware_core::FirmwareReport,
) -> std::result::Result<Option<String>, String> {
    crate::firmware::read_firmware_report(report, cfg!(target_os = "windows"), |kind, bytes| {
        let device = hid.lock().unwrap();
        match kind {
            hardware_core::FirmwareReportKind::Input => device.get_input_report(bytes),
            hardware_core::FirmwareReportKind::Feature => device.get_feature_report(bytes),
        }
        .map_err(|e| e.to_string())
    })
}

// Start input
fn start_input(
    runtime: Weak<Runtime>,
    (id, epoch): (String, u64),
    hid: Arc<Mutex<hidapi::HidDevice>>,
    session: Arc<Mutex<Box<dyn HardwareSession>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    input_wake: Arc<input_readiness::Signal>,
    benchmark_input: Arc<Mutex<bool>>,
) -> std::thread::JoinHandle<()> {
    let (query, requests) = mpsc::sync_channel::<crate::firmware::QueryReply>(1);
    let query = crate::firmware::QuerySender::new(query, {
        let wake = input_wake.clone();
        Arc::new(move || wake.notify())
    });
    let firmware_report = runtime.upgrade().and_then(|r| {
        r.snapshot()
            .devices
            .iter()
            .find(|d| d.id == id && d.manifest.capabilities.firmware_query)
            .map(|d| {
                d.manifest
                    .capabilities
                    .firmware_report
                    .clone()
                    .unwrap_or_default()
            })
    });
    let firmware_report =
        firmware_report.filter(|r| !cfg!(target_os = "windows") || r.windows_supported);
    let supported = firmware_report.is_some();
    if supported {
        if let Some(runtime) = runtime.upgrade() {
            runtime
                .firmware
                .queries
                .lock()
                .unwrap()
                .insert(id.clone(), (epoch, query));
        }
    }
    std::thread::spawn(move || {
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        let registration =
            match input_readiness::Registration::install(hid.clone(), input_wake.clone()) {
                Ok(registration) => Some(registration),
                Err(error) => {
                    eprintln!("HID readiness unavailable, retaining polling: {error}");
                    None
                }
            };
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        let readiness = registration.is_some();
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        let readiness = false;
        let mut initial_query = supported;
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            let observed = if readiness {
                input_wake.generation()
            } else {
                0
            };
            let replies: Vec<_> = requests.try_iter().take(16).collect();
            if initial_query || !replies.is_empty() {
                let result =
                    query_firmware(&hid, firmware_report.as_ref().expect("supported query"));
                if let Some(runtime) = runtime.upgrade() {
                    runtime.observe_firmware(&id, epoch, result.clone());
                }
                for reply in replies {
                    let _ = reply.send(result.clone());
                }
                initial_query = false;
            }
            let mut report = [0; 1025];
            let wait_for_hid = Instant::now();
            let mut hid_lock_wait = Duration::ZERO;
            let read = read_hid_input(
                &hid,
                !cfg!(target_os = "windows"),
                readiness,
                |device, timeout| {
                    hid_lock_wait = wait_for_hid.elapsed();
                    device.read_timeout(&mut report, timeout)
                },
                |delay| {
                    if readiness {
                        input_wake.wait(observed);
                    } else {
                        std::thread::sleep(delay);
                    }
                },
            );
            let received = Instant::now();
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            if runtime.connection_epochs.lock().unwrap().get(&id) != Some(&epoch) {
                break;
            }
            match read {
                Ok(0) => {}
                Ok(size) => {
                    let waiting = Instant::now();
                    let mut decoder = session.lock().unwrap();
                    let decoding = Instant::now();
                    let settings = decoder.decode_keyboard_settings(&report[..size]);
                    let decoded = if matches!(&settings, Ok(None)) {
                        decoder.decode(&report[..size])
                    } else {
                        Ok(Vec::new())
                    };
                    let decoded_at = Instant::now();
                    drop(decoder);
                    runtime.metrics.record(
                        "input_hid_lock_wait_ms",
                        hid_lock_wait.as_secs_f64() * 1000.,
                    );
                    runtime.metrics.record(
                        "input_session_lock_wait_ms",
                        (decoding - waiting).as_secs_f64() * 1000.,
                    );
                    match settings {
                        Ok(Some(settings)) => {
                            runtime.observe_keyboard_settings(&id, epoch, settings);
                            continue;
                        }
                        Err(error) => {
                            runtime.emit(
                                "protocol-error",
                                Some(id.clone()),
                                None,
                                Some(error.to_string()),
                            );
                            continue;
                        }
                        Ok(None) => {}
                    }
                    runtime.metrics.record(
                        "input_receipt_to_decoded_ms",
                        (decoded_at - received).as_secs_f64() * 1000.,
                    );
                    match decoded {
                        Ok(events) => {
                            for event in events {
                                runtime
                                    .setup_capabilities
                                    .observe_input(&id, epoch, false, &event);
                                let trace =
                                    runtime.input_trace.ingress_at(&id, epoch, &event, received);
                                if guarded_input(&benchmark_input, || {
                                    if !runtime.power_input(&id, epoch, &event) {
                                        runtime.dispatch_input(
                                            &id,
                                            &event,
                                            trace.clone(),
                                            Some((epoch, &stop)),
                                        );
                                    } else {
                                        runtime.input_trace.record(
                                            trace.as_deref(),
                                            "Power gate consumed input",
                                            true,
                                        );
                                    }
                                }) {
                                    runtime.input_trace.record(
                                        trace.as_deref(),
                                        "Benchmark consumed input",
                                        true,
                                    );
                                    runtime.metrics.record("benchmark_input_events", 1.0);
                                }
                                runtime.emit("input", Some(id.clone()), Some(event), None);
                            }
                        }
                        Err(error) => runtime.emit(
                            "protocol-error",
                            Some(id.clone()),
                            None,
                            Some(error.to_string()),
                        ),
                    }
                }
                Err(_) => {
                    let _ = runtime
                        .device_commands
                        .send(Request::Lost(id.clone(), epoch));
                    break;
                }
            }
        }
    })
}
// Prepare command
fn prepare_command(
    c: &Connection,
    cmd: DeviceCommand,
) -> std::result::Result<DeviceCommand, String> {
    let cmd = if let Ok(format) = std::env::var("STREAM_DOCK_MANAGER_IMAGE_FORMAT") {
        if !c.manifest.capabilities.image_formats.contains(&format) {
            return Err("diagnostic image format is unsupported by this module".into());
        }
        match cmd {
            DeviceCommand::Image { control_id, data } => {
                let control = c.manifest.control(&control_id).map_err(|e| e.to_string())?;
                DeviceCommand::Image {
                    control_id,
                    data: hardware_core::render::render(
                        &data,
                        control.image_width.ok_or("control has no image surface")?,
                        control.image_height.ok_or("control has no image surface")?,
                        0,
                        &format,
                    )
                    .map_err(|e| e.to_string())?,
                }
            }
            DeviceCommand::Background {
                data,
                x,
                y,
                width,
                height,
            } => DeviceCommand::Background {
                data: hardware_core::render::render(&data, width, height, 0, &format)
                    .map_err(|e| e.to_string())?,
                x,
                y,
                width,
                height,
            },
            other => other,
        }
    } else {
        cmd
    };
    Ok(cmd)
}
// Identify the image or background surface targeted by an output command
fn command_surface(command: &DeviceCommand) -> Option<String> {
    match command {
        DeviceCommand::Image { control_id, .. } => Some(control_id.clone()),
        DeviceCommand::Background { .. } => Some("@background".into()),
        _ => None,
    }
}
// Hash the surface identity and encoded payload used to suppress duplicate output
fn command_digest(command: &DeviceCommand) -> [u8; 32] {
    let mut hash = Sha256::new();
    match command {
        DeviceCommand::Image { control_id, data } => {
            hash.update(control_id.as_bytes());
            hash.update(data);
        }
        DeviceCommand::Background {
            data,
            x,
            y,
            width,
            height,
        } => {
            for value in [x, y, width, height] {
                hash.update(value.to_le_bytes());
            }
            hash.update(data);
        }
        _ => {}
    }
    hash.finalize().into()
}
impl Connection {
    // Stop benchmark
    fn stop_benchmark(&mut self) {
        self.benchmark = None;
        *self.benchmark_input.lock().unwrap() = false;
    }
    // Invalidate scene
    fn invalidate_scene(&mut self) {
        self.transition = None;
        self.scene_revision = self.scene_revision.wrapping_add(1).max(1);
        self.worker.set_scene_revision(self.scene_revision);
        self.delivery.clear();
        for playback in self.playbacks.values_mut() {
            playback.last_frame = None;
            playback.pending_frame = None;
        }
    }
    // Collect outcomes
    fn collect_outcomes(&mut self, runtime: &Runtime) {
        let lost = self.worker.queue_stats().outcomes_lost;
        if lost != self.lost_outcomes {
            self.lost_outcomes = lost;
            self.worker.drain_outcomes();
            restore(runtime, self);
            runtime.emit("device-output-recovery", Some(self.device_id.clone()), None,
                Some("Completion history overflowed; speculative state discarded and current scene redrawn".into()));
            return;
        }
        for outcome in self.worker.drain_outcomes() {
            #[cfg(test)]
            playback_benchmark::record_completion(&outcome);
            runtime
                .metrics
                .record("output_completed_report_bytes", outcome.report_bytes as f64);
            runtime
                .metrics
                .record("output_refresh_count", outcome.commit_count as f64);
            if outcome.scene_revision != self.scene_revision {
                continue;
            }
            let Some(delivery) = self
                .delivery
                .get_mut(&outcome.surface_id)
                .filter(|delivery| delivery.revision == outcome.content_revision)
            else {
                continue;
            };
            delivery.pending = false;
            delivery.completed = outcome.status == OutputStatus::Completed;
            if let (Some(presentation), Some((_, finished))) =
                (delivery.presentation.take(), outcome.write_interval)
            {
                runtime.metrics.record(
                    "animation_presentation_to_final_write_ms",
                    finished
                        .saturating_duration_since(presentation)
                        .as_secs_f64()
                        * 1000.,
                );
            }
            if let (Some(origin), Some((first, finished))) =
                (delivery.origin.take(), outcome.write_interval)
            {
                let elapsed = (finished - origin.received).as_secs_f64() * 1000.;
                runtime.metrics.record(
                    "input_to_first_write_ms",
                    (first - origin.received).as_secs_f64() * 1000.,
                );
                runtime.metrics.record("input_to_final_write_ms", elapsed);
                runtime
                    .input_trace
                    .wrote(&origin, self.epoch, &outcome, elapsed);
            }
            if let Some(playback) = self.playbacks.get_mut(&outcome.surface_id) {
                if let Some((index, revision)) = playback.pending_frame {
                    if revision == outcome.content_revision {
                        if delivery.completed {
                            playback.last_frame = Some(index);
                        }
                        playback.pending_frame = None;
                    }
                }
            }
            runtime.metrics.record(
                if delivery.completed {
                    "output_surface_completed"
                } else {
                    "output_surface_dropped"
                },
                1.0,
            );
        }
    }
}
/// Queue current surface content. Duplicate pending/completed revisions do not generate traffic
/// Completion, not successful enqueue, marks a playback frame delivered to the transport
fn send_surfaces(
    c: &mut Connection,
    commands: Vec<DeviceCommand>,
    priority: OutputPriority,
) -> std::result::Result<Vec<(String, u64)>, String> {
    if let Some(runtime) = c.runtime.upgrade() {
        refresh_contract(&runtime, c)?;
    }
    send_surfaces_resolved(c, commands, priority, false)
}
/// Measure image preparation on the producer thread, including frozen scenes and
/// legacy individual uploads. Release the protocol session before publishing timings.
fn encode_output<T>(
    c: &Connection,
    encode: impl FnOnce(&mut dyn HardwareSession) -> hardware_core::Result<T>,
) -> std::result::Result<T, String> {
    let waiting = Instant::now();
    let mut session = c.session.lock().unwrap();
    let encoding_started = Instant::now();
    let before = AdapterWork::current();
    let result = encode(session.as_mut());
    let elapsed = encoding_started.elapsed();
    let after = AdapterWork::current();
    drop(session);
    if let Some(runtime) = c.runtime.upgrade() {
        runtime.metrics.record(
            "output_session_lock_wait_ms",
            (encoding_started - waiting).as_secs_f64() * 1000.,
        );
        let framing = after.framing - before.framing;
        runtime.metrics.record(
            "output_report_construction_ms",
            framing.as_secs_f64() * 1000.,
        );
        runtime.metrics.record(
            "output_model_encode_ms",
            elapsed.saturating_sub(framing).as_secs_f64() * 1000.,
        );
        runtime.metrics.record(
            "output_adapter_payload_bytes",
            after.payload_bytes.wrapping_sub(before.payload_bytes) as f64,
        );
        runtime.metrics.record(
            "output_jpeg_target_attempts",
            after.jpeg_attempts.wrapping_sub(before.jpeg_attempts) as f64,
        );
    }
    result.map_err(|e| e.to_string())
}
// Send surfaces resolved
fn send_surfaces_resolved(
    c: &mut Connection,
    commands: Vec<DeviceCommand>,
    priority: OutputPriority,
    rendered_key: bool,
) -> std::result::Result<Vec<(String, u64)>, String> {
    let mut selected = Vec::new();
    let mut metadata = Vec::new();
    let mut source_bytes = 0usize;
    for command in commands {
        let command = prepare_command(c, command)?;
        let surface =
            command_surface(&command).ok_or("Only image surfaces can be scheduled as a frame")?;
        let bytes = match &command {
            DeviceCommand::Image { data, .. } | DeviceCommand::Background { data, .. } => {
                data.len()
            }
            _ => 0,
        };
        let digest = command_digest(&command);
        if c.delivery
            .get(&surface)
            .is_some_and(|value| value.digest == digest && (value.pending || value.completed))
        {
            continue;
        }
        source_bytes = source_bytes.saturating_add(bytes);
        if bytes > c.policy.max_surface_bytes || source_bytes > c.policy.max_pending_bytes {
            return Err("Rendered frame exceeds the bounded device output memory budget".into());
        }
        c.content_revision = c.content_revision.wrapping_add(1).max(1);
        metadata.push((surface, digest, c.content_revision));
        selected.push(command);
    }
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let uploads = encode_output(c, |session| {
        if rendered_key && std::env::var_os("STREAM_DOCK_MANAGER_IMAGE_FORMAT").is_none() {
            let Some(DeviceCommand::Image { control_id, data }) = selected.pop() else {
                return Err(Error::Protocol("Expected one rendered key".into()));
            };
            if !selected.is_empty() {
                return Err(Error::Protocol("Expected one rendered key".into()));
            }
            session
                .encode_rendered_key(control_id, data, c.encoding)
                .map(|one| vec![one])
        } else {
            session.encode_frame_with_purpose(
                selected,
                c.encoding,
                if priority == OutputPriority::Animation {
                    ImagePurpose::Animated
                } else {
                    ImagePurpose::Static
                },
            )
        }
    })?;
    if uploads.len() != metadata.len() {
        return Err("Encoder did not preserve surface correspondence".into());
    }
    let submissions = uploads
        .into_iter()
        .zip(&metadata)
        .map(|(upload, (surface, _, revision))| SurfaceSubmission {
            upload,
            surface_id: surface.clone(),
            content_revision: *revision,
            priority,
        })
        .collect();
    c.worker
        .submit_frame_guarded(
            c.worker.generation(),
            c.scene_revision,
            submissions,
            c.image_admission(),
        )
        .map_err(|e| e.to_string())?;
    let mut accepted = Vec::new();
    for (surface, digest, revision) in metadata {
        c.delivery.insert(
            surface.clone(),
            Delivery {
                presentation: None,
                origin: None,
                digest,
                revision,
                pending: true,
                completed: false,
            },
        );
        accepted.push((surface, revision));
    }
    Ok(accepted)
}
// Send the devices message
fn send(c: &mut Connection, cmd: DeviceCommand) -> std::result::Result<(), String> {
    if let Some(runtime) = c.runtime.upgrade() {
        refresh_contract(&runtime, c)?;
    }
    send_resolved(c, cmd)
}
// Send resolved
fn send_resolved(c: &mut Connection, cmd: DeviceCommand) -> std::result::Result<(), String> {
    send_resolved_output(c, cmd, false)
}
fn send_resolved_output(
    c: &mut Connection,
    cmd: DeviceCommand,
    ready: bool,
) -> std::result::Result<(), String> {
    if c.benchmark.is_some() {
        return Ok(());
    }
    if command_surface(&cmd).is_some() && c.transition.is_some() {
        return Ok(());
    }
    if command_surface(&cmd).is_some() && c.policy.mode != OutputMode::IndividualUpdates {
        return send_surfaces_resolved(c, vec![cmd], OutputPriority::Interaction, ready)
            .map(|_| ());
    }
    if c.manifest.model_id == "k1-pro" && matches!(cmd, DeviceCommand::Sleep | DeviceCommand::Wake)
    {
        let result = blackout::send(c, matches!(cmd, DeviceCommand::Sleep));
        if let Err(error) = &result {
            if let Some(runtime) = c.runtime.upgrade() {
                runtime.emit(
                    "device-error",
                    Some(c.device_id.clone()),
                    None,
                    Some(error.clone()),
                );
            }
        }
        return result;
    }
    let is_image = command_surface(&cmd).is_some();
    if let Some(surface) = command_surface(&cmd) {
        // Legacy individual writes bypass the tracked frame queue. Retire its previous
        // signature so a later live label can restore the same pixels after this write.
        c.delivery.remove(&surface);
    }
    if is_image {
        c.static_palette.scene = None;
    }
    let cmd = prepare_command(c, cmd)?;
    let tx = if is_image {
        encode_output(c, |session| session.encode(cmd))?
    } else {
        c.session
            .lock()
            .unwrap()
            .encode(cmd)
            .map_err(|error| error.to_string())?
    };
    if let Some(admission) = is_image.then(|| c.image_admission()).flatten() {
        c.worker
            .submit_tracked_guarded(c.worker.generation(), tx, Box::new(move || admission()))
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        c.worker
            .submit(c.worker.generation(), tx)
            .map_err(|e| e.to_string())
    }
}
// Navigation closes input admission before publishing a mapping. Ordinary
// producers honor that same boundary even before the actor consumes RestoreDevice
fn transition_pending(runtime: &Runtime, c: &Connection) -> bool {
    c.transition.is_some()
        || runtime
            .transition_inputs
            .lock()
            .unwrap()
            .pending(&c.device_id, c.epoch)
            .is_some()
}
// Send current
fn send_current(
    runtime: &Runtime,
    c: &mut Connection,
    command: DeviceCommand,
) -> std::result::Result<(), String> {
    send_current_output(runtime, c, command, false)
}
fn send_current_output(
    runtime: &Runtime,
    c: &mut Connection,
    command: DeviceCommand,
    ready: bool,
) -> std::result::Result<(), String> {
    refresh_contract(runtime, c)?;
    let gate = runtime.transition_inputs.lock().unwrap();
    if command_surface(&command).is_some() && gate.pending(&c.device_id, c.epoch).is_some() {
        return Ok(());
    }
    send_resolved_output(c, command, ready)
}
// Send current surfaces
fn send_current_surfaces(
    runtime: &Runtime,
    c: &mut Connection,
    commands: Vec<DeviceCommand>,
    priority: OutputPriority,
) -> std::result::Result<Vec<(String, u64)>, String> {
    refresh_contract(runtime, c)?;
    let gate = runtime.transition_inputs.lock().unwrap();
    if gate.pending(&c.device_id, c.epoch).is_some() {
        return Ok(Vec::new());
    }
    send_surfaces_resolved(c, commands, priority, false)
}

// Apply vibration
fn apply_vibration(
    runtime: &Runtime,
    c: &mut Connection,
    output: &crate::workspace::DeviceOutput,
) -> std::result::Result<(), String> {
    if output.sleeping || c.sleeping || c.benchmark.is_some() {
        return Ok(());
    }
    let Some(enabled) = output.vibration_enabled else {
        return Ok(());
    };
    let Some(capability) = c
        .manifest
        .capabilities
        .vibration
        .as_ref()
        .filter(|v| v.toggle && v.writable)
    else {
        return Ok(());
    };
    capability
        .validate_configuration(&output.device_configuration)
        .map_err(|e| e.to_string())?;
    if let Some(device) = runtime
        .snapshot()
        .devices
        .iter()
        .find(|d| d.id == c.device_id)
    {
        if let Some(error) =
            runtime.vibration_firmware_error(&c.device_id, capability, &device.variant_id)
        {
            return Err(error);
        }
    }
    let desired = (enabled, output.device_configuration.clone());
    if c.applied_vibration.as_ref() == Some(&desired) {
        return Ok(());
    }
    send(
        c,
        DeviceCommand::Vibration {
            enabled,
            configuration: desired.1.clone(),
        },
    )?;
    c.applied_vibration = Some(desired);
    Ok(())
}

// Initialize hardware output using real waits between required firmware steps
fn initialize_output(session: &mut dyn HardwareSession, worker: &OutputWorker) -> Result<()> {
    initialize_output_with_wait(session, worker, std::thread::sleep)
}

// Complete initialization writes and firmware waits within one total deadline
fn initialize_output_with_wait(
    session: &mut dyn HardwareSession,
    worker: &OutputWorker,
    mut wait: impl FnMut(Duration),
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let timeout = || {
        Error::Transport(
            worker
                .failure()
                .unwrap_or_else(|| "Device initialization did not finish within 2 seconds".into()),
        )
    };
    let drain = || {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() && worker.wait_idle(remaining) {
            Ok(())
        } else {
            Err(timeout())
        }
    };
    let generation = worker.generation();
    for step in session.initialization_plan()? {
        match step {
            InitializationStep::Write(transaction) => {
                if Instant::now() >= deadline {
                    return Err(timeout());
                }
                if let Err(error) = worker.submit(generation, transaction) {
                    return Err(worker.failure().map_or(error, Error::Transport));
                }
            }
            InitializationStep::Wait(duration) => {
                // Firmware delays start after the preceding reports finish,
                // not after they merely enter the asynchronous output queue
                drain()?;
                if duration > deadline.saturating_duration_since(Instant::now()) {
                    return Err(timeout());
                }
                wait(duration);
            }
        }
    }
    // Restoring a saved output policy can invalidate queued transactions. Finish
    // connection setup before publishing the device or applying that policy
    drain()
}

// Keep the native cause for diagnostics, but make macOS permission denial actionable
// Do not turn every HID failure (disconnect/busy/etc.) into a permission diagnosis
fn hid_open_error(message: String, platform: &str) -> Error {
    let normalized = message.to_ascii_lowercase();
    if platform == "macos"
        && normalized.contains("failed to open iohiddevice")
        && normalized.contains("0xe00002e2")
    {
        Error::Transport(format!(
            "macOS denied access to the HID device. Check System Settings > Privacy & Security > Input Monitoring for the installed Stream Dock Manager app in Applications, then fully quit Manager from its menu-bar menu and reopen it. Accessibility is a separate permission and does not replace Input Monitoring. A development/Terminal grant may not apply to the installed app; a replaced ad-hoc build may need approval again. Native error: {message}"
        ))
    } else {
        Error::Transport(message)
    }
}

// Open connection
fn open_connection(
    runtime: &Arc<Runtime>,
    info: &hidapi::DeviceInfo,
    api: &hidapi::HidApi,
    module: &dyn HardwareModule,
    epoch: u64,
    force_individual_updates: bool,
) -> Result<Connection> {
    if !runtime.device_connections_enabled() {
        return Err(Error::Transport(
            "Complete permission setup before connecting devices".into(),
        ));
    }
    let id = info.path().to_string_lossy().to_string();
    let registration = runtime.device_commands.register(&id, epoch);
    let output_hid =
        Arc::new(Mutex::new(info.open_device(api).map_err(|e| {
            hid_open_error(e.to_string(), std::env::consts::OS)
        })?));
    // Windows permits shared opens of this exact HID collection. A separate
    // input handle keeps blocking reads from inserting gaps between image
    // reports, without sharing one HidDevice concurrently across threads
    #[cfg(target_os = "windows")]
    let input_hid =
        Arc::new(Mutex::new(info.open_device(api).map_err(|e| {
            Error::Transport(format!("Could not open HID input: {e}"))
        })?));
    #[cfg(not(target_os = "windows"))]
    let input_hid = output_hid.clone();
    let worker = OutputWorker::spawn_with_policy(
        Box::new(HidTransport {
            device: output_hid,
            runtime: Arc::downgrade(runtime),
        }),
        module.manifest().capabilities.max_fps,
        hardware_core::OutputPolicy::default(),
    )?;
    let weak = Arc::downgrade(runtime);
    let lost_id = id.clone();
    worker.set_failure_waker(Arc::new(move || {
        if let Some(runtime) = weak.upgrade() {
            let _ = runtime
                .device_commands
                .send(Request::Lost(lost_id.clone(), epoch));
        }
    }));
    let variant = module
        .manifest()
        .variants
        .iter()
        .find(|v| v.available && v.identity.matches(&identity(info)))
        .ok_or_else(|| Error::Descriptor("matching variant missing".into()))?;
    // Select exact geometry before initialization or restoration. Firmware-sensitive
    // models remain output-disabled when this read fails; USB release is unrelated
    let firmware = if module.manifest().capabilities.firmware_query {
        query_firmware(
            &input_hid,
            &module
                .manifest()
                .capabilities
                .firmware_report
                .clone()
                .unwrap_or_default(),
        )
        .ok()
        .flatten()
    } else {
        None
    };
    let contract = module.resolve_contract(&variant.id, firmware.as_deref())?;
    let mut session = module.session_for(&contract)?;
    initialize_output(session.as_mut(), &worker)?;
    let manifest = contract.manifest.clone();
    runtime
        .connection_epochs
        .lock()
        .unwrap()
        .insert(id.clone(), epoch);
    runtime.state.lock().unwrap().devices.push(DeviceInfo {
        id: id.clone(),
        persistence_key: info
            .serial_number()
            .filter(|s| !s.is_empty())
            .map(|serial| format!("{}:{serial}", manifest.model_id))
            .unwrap_or_else(|| id.clone()),
        identity_port_dependent: info.serial_number().is_none_or(str::is_empty),
        model_id: manifest.model_id.clone(),
        name: variant.name.clone(),
        variant_id: variant.id.clone(),
        firmware: firmware.unwrap_or_default(),
        usb_release: Some(format!("{:04x}", info.release_number())),
        connected: true,
        simulated: false,
        manifest: manifest.clone(),
    });
    let session = Arc::new(Mutex::new(session));
    let input_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let input_wake = Arc::new(input_readiness::Signal::default());
    let benchmark_input = Arc::new(Mutex::new(false));
    runtime.transition_inputs.lock().unwrap().begin(&id, epoch);
    let input_thread = Some(start_input(
        Arc::downgrade(runtime),
        (id.clone(), epoch),
        input_hid,
        session.clone(),
        input_stop.clone(),
        input_wake.clone(),
        benchmark_input.clone(),
    ));
    let mut connection = Connection {
        runtime: Arc::downgrade(runtime),
        _mailbox_registration: Some(registration),
        sleeping: false,
        navigation_fallback: Default::default(),
        restore_retry: None,
        boot_logo_restore: None,
        transition: None,
        device_id: id.clone(),
        worker,
        session,
        input_stop,
        input_wake,
        benchmark_input,
        input_thread,
        epoch,
        heartbeat: Instant::now(),
        manifest,
        contract_identity: contract.identity(),
        playbacks: std::collections::BTreeMap::new(),
        playback_cursor: 0,
        next_frame: Instant::now(),
        animation_rate: AnimationRate::default(),
        last_queue_ms: 0.0,
        policy: OutputPolicy::default(),
        encoding: FrameEncoding::IndividualUpdates,
        scene_revision: 1,
        draft_preview: None,
        draft_sample: Instant::now(),
        static_palette: StaticPaletteGate::default(),
        content_revision: 0,
        delivery: HashMap::new(),
        benchmark: None,
        lost_outcomes: 0,
        force_individual_updates: false,
        applied_vibration: None,
        boot_logo: None,
        keyboard: None,
        blackout: Default::default(),
    };
    connection.force_individual_updates = force_individual_updates;
    restore(runtime, &mut connection);
    if connection
        .manifest
        .capabilities
        .keyboard
        .as_ref()
        .is_some_and(|k| k.settings_query)
    {
        let _ = send(&mut connection, DeviceCommand::KeyboardSettingsQuery);
    }
    Ok(connection)
}
// Benchmark connection using the current devices state
fn benchmark_connection<'a>(
    connections: &'a mut HashMap<String, Connection>,
    device: &str,
    epoch: u64,
    run_id: u64,
) -> std::result::Result<&'a mut Connection, String> {
    let c = connections
        .get_mut(device)
        .filter(|c| c.epoch == epoch)
        .ok_or("Benchmark connection changed or disconnected")?;
    if c.benchmark.as_ref().map(|(owner, _)| *owner) != Some(run_id) {
        return Err("Benchmark no longer owns this device".into());
    }
    c.benchmark = Some((run_id, Instant::now()));
    Ok(c)
}
// Benchmark frame using the current devices state
fn benchmark_frame(
    connections: &mut HashMap<String, Connection>,
    device: &str,
    epoch: u64,
    run_id: u64,
    commands: Vec<DeviceCommand>,
) -> std::result::Result<OutputQueueStats, String> {
    let c = benchmark_connection(connections, device, epoch, run_id)?;
    let surface_count = c
        .manifest
        .controls
        .iter()
        .filter(|control| control.image_slot.is_some())
        .count()
        + usize::from(c.manifest.capabilities.background_size.is_some());
    if commands.is_empty() || commands.len() > surface_count {
        return Err(format!(
            "Benchmark frame must contain 1..{surface_count} image surfaces for this device"
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for command in &commands {
        let surface = match command {
            DeviceCommand::Image { control_id, .. } => {
                if !c.manifest.controls.iter().any(|control| control.id == *control_id && control.image_slot.is_some()) {
                    return Err("Benchmark image target is not an LCD section on this device".into());
                }
                control_id.as_str()
            }
            DeviceCommand::Background { .. } if c.manifest.capabilities.background_size.is_some() => "@background",
            _ => return Err("Benchmark accepts supported image surfaces only; host and control actions are forbidden".into()),
        };
        if !seen.insert(surface) {
            return Err("Benchmark frame contains a duplicate image surface".into());
        }
    }
    send_surfaces(c, commands, OutputPriority::Animation)?;
    Ok(c.worker.queue_stats())
}
// Wake for benchmark
fn wake_for_benchmark(runtime: &Runtime, c: &mut Connection) -> std::result::Result<(), String> {
    c.sleeping = false;
    send(c, DeviceCommand::Wake)?;
    if c.manifest.capabilities.brightness {
        let output = runtime.state.lock().unwrap().session(&c.device_id).output;
        let output = runtime.power.effective(&c.device_id, c.epoch, &output);
        send(c, DeviceCommand::Brightness(output.brightness))?;
    }
    Ok(())
}
// Start deliberately retains its own timeout cleanup; ordinary requests share this envelope
fn receive_benchmark_reply<T>(
    response: mpsc::Receiver<std::result::Result<T, String>>,
    operation: &str,
    timeout: Duration,
) -> std::result::Result<T, String> {
    response
        .recv_timeout(timeout)
        .map_err(|error| format!("Benchmark {operation} timed out: {error}"))?
}

impl Runtime {
    // Benchmark device start using the current devices state
    pub(crate) fn benchmark_device_start(
        &self,
        device: &str,
        epoch: u64,
        run_id: u64,
        policy: OutputPolicy,
        encoding: FrameEncoding,
    ) -> std::result::Result<(), String> {
        let (reply, response) = mpsc::channel();
        self.device_commands
            .send(Request::BenchmarkStart {
                device: device.into(),
                epoch,
                run_id,
                policy,
                encoding,
                reply,
            })
            .map_err(|e| e.to_string())?;
        match response.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result,
            Err(error) => {
                // Ordered actor cleanup even if start was queued behind a slow render
                let (reply, _) = mpsc::channel();
                let _ = self.device_commands.send(Request::BenchmarkStop {
                    device: device.into(),
                    epoch,
                    run_id,
                    reply,
                });
                Err(format!("Benchmark start timed out: {error}"))
            }
        }
    }
    // Benchmark request using the current devices state
    fn benchmark_request<T>(
        &self,
        operation: &str,
        request: impl FnOnce(mpsc::Sender<std::result::Result<T, String>>) -> Request,
    ) -> std::result::Result<T, String> {
        let (reply, response) = mpsc::channel();
        self.device_commands
            .send(request(reply))
            .map_err(|e| e.to_string())?;
        receive_benchmark_reply(response, operation, Duration::from_secs(5))
    }
    // Benchmark device frame using the current devices state
    pub(crate) fn benchmark_device_frame(
        &self,
        device: &str,
        epoch: u64,
        run_id: u64,
        commands: Vec<DeviceCommand>,
    ) -> std::result::Result<OutputQueueStats, String> {
        self.benchmark_request("frame", |reply| Request::BenchmarkFrame {
            device: device.into(),
            epoch,
            run_id,
            commands,
            reply,
        })
    }
    // Benchmark device stats using the current devices state
    pub(crate) fn benchmark_device_stats(
        &self,
        device: &str,
        epoch: u64,
        run_id: u64,
    ) -> std::result::Result<OutputQueueStats, String> {
        self.benchmark_request("stats", |reply| Request::BenchmarkStats {
            device: device.into(),
            epoch,
            run_id,
            reply,
        })
    }
    // Benchmark device outcomes using the current devices state
    pub(crate) fn benchmark_device_outcomes(
        &self,
        device: &str,
        epoch: u64,
        run_id: u64,
    ) -> std::result::Result<Vec<OutputOutcome>, String> {
        self.benchmark_request("outcomes", |reply| Request::BenchmarkOutcomes {
            device: device.into(),
            epoch,
            run_id,
            reply,
        })
    }
    // Benchmark device stop using the current devices state
    pub(crate) fn benchmark_device_stop(
        &self,
        device: &str,
        epoch: u64,
        run_id: u64,
    ) -> std::result::Result<(), String> {
        self.benchmark_request("stop", |reply| Request::BenchmarkStop {
            device: device.into(),
            epoch,
            run_id,
            reply,
        })
    }
}
impl Runtime {
    // Send device command
    pub(crate) fn send_device_command(
        &self,
        device: &str,
        command: DeviceCommand,
    ) -> std::result::Result<(), String> {
        let epoch = self
            .connection_epochs
            .lock()
            .unwrap()
            .get(device)
            .copied()
            .ok_or("device disconnected")?;
        self.device_commands
            .send(Request::Command(device.into(), epoch, command))
    }
}

/// Enumeration cadence, connection generations and per-device compatibility memory
/// belong to the existing coordinator, not a second worker or queue
struct DiscoveryState {
    individual_update_fallbacks: std::collections::HashSet<String>,
    scan: Instant,
    schedule: discovery::Schedule,
    changed: Arc<std::sync::atomic::AtomicBool>,
    next_epoch: u64,
}
impl Default for DiscoveryState {
    // Construct the default devices value
    fn default() -> Self {
        Self {
            individual_update_fallbacks: Default::default(),
            scan: Instant::now() - Duration::from_secs(2),
            schedule: discovery::Schedule::new(Instant::now()),
            changed: Default::default(),
            next_epoch: 0,
        }
    }
}

/// Owns one coordinator thread's state. Methods are synchronous phases of the
/// existing loop, not independent workers or additional queues
struct Coordinator {
    connections: HashMap<String, Connection>,
    discovery: DiscoveryState,
    scene_revision: u64,
    display_theme_revision: u64,
    vault_surface_revision: u64,
    plugins_dirty: bool,
    dirty: DirtyVisuals,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retirement {
    InputLost,
    PermissionRequired,
    Absent,
    OutputFailed,
}
impl Coordinator {
    // Initialize devices state from the supplied inputs
    fn new() -> Self {
        Self {
            connections: HashMap::new(),
            discovery: DiscoveryState::default(),
            scene_revision: 0,
            display_theme_revision: 0,
            vault_surface_revision: 0,
            plugins_dirty: false,
            dirty: DirtyVisuals::default(),
        }
    }
    // Retire the inactive devices resource
    fn retire(&mut self, runtime: &Runtime, id: &str, reason: Retirement) {
        runtime.vendor_animations.lock().unwrap().clear(id, false);
        runtime.vendor_animations.lock().unwrap().clear(id, true);
        if reason != Retirement::OutputFailed {
            runtime.builtin_state.cleanup(Some(id));
        }
        runtime.connection_epochs.lock().unwrap().remove(id);
        runtime.event_bindings.cancel(Some(id));
        runtime.control_event_test.retire_device(id);
        runtime.forget_firmware_connection(id);
        runtime.forget_keyboard_settings(id);
        self.connections.remove(id);
        if matches!(reason, Retirement::InputLost | Retirement::OutputFailed) {
            self.discovery.schedule.retry_connection(Instant::now());
        }
        runtime.transition_inputs.lock().unwrap().forget(id);
        runtime.state.lock().unwrap().devices.retain(|d| d.id != id);
        runtime.emit(
            "devices",
            Some(id.into()),
            None,
            Some(
                match reason {
                    Retirement::InputLost => "Input interface disconnected",
                    Retirement::PermissionRequired => {
                        "Disconnected; complete permission setup to reconnect"
                    }
                    Retirement::Absent => "Disconnected; pending output discarded",
                    Retirement::OutputFailed => {
                        "Connection failed; reconnect will restore current surfaces"
                    }
                }
                .into(),
            ),
        );
    }
    // Dispatch the devices request
    fn dispatch(&mut self, runtime: &Arc<Runtime>, request: Request) -> Option<mpsc::Sender<()>> {
        match request {
            Request::Keyboard {
                device,
                epoch,
                contract_key,
                transaction,
                deadline,
                reply,
            } => {
                let result = (|| -> std::result::Result<(), String> {
                    let c = self
                        .connections
                        .get_mut(&device)
                        .filter(|c| c.epoch == epoch)
                        .ok_or("Physical connection changed or is unavailable")?;
                    runtime.validate_keyboard_contract(&device, epoch, &contract_key)?;
                    if c.sleeping || c.transition.is_some() {
                        return Err("Wait until the device is awake and idle".into());
                    }
                    if c.keyboard.is_some() || c.boot_logo.is_some() || c.benchmark.is_some() {
                        return Err("Device already has an active transfer or benchmark".into());
                    }
                    if c.manifest.capabilities.keyboard.is_none() {
                        return Err("Keyboard settings unavailable".into());
                    }
                    if Instant::now() >= deadline {
                        return Err("Keyboard request expired before transmission".into());
                    }
                    let weak = Arc::downgrade(runtime);
                    let receipt = c
                        .worker
                        .submit_tracked_per_report_guarded(
                            c.worker.generation(),
                            transaction,
                            Box::new(move || {
                                weak.upgrade().is_some_and(|r| {
                                    r.validate_keyboard_contract(&device, epoch, &contract_key)
                                        .is_ok()
                                }) && Instant::now() < deadline
                            }),
                        )
                        .map_err(|e| e.to_string())?;
                    c.keyboard = Some(crate::keyboard::PendingApply {
                        receipt,
                        reply: Some(reply.clone()),
                        deadline,
                    });
                    Ok(())
                })();
                if let Err(error) = result {
                    let _ = reply.send(Err(crate::keyboard::TransferError::NotSubmitted(error)));
                }
            }
            Request::BootLogo {
                device,
                epoch,
                contract_key,
                transaction,
                reply,
            } => {
                let result = (|| -> std::result::Result<_, String> {
                    let connection = self
                        .connections
                        .get_mut(&device)
                        .filter(|c| c.epoch == epoch)
                        .ok_or("Physical connection changed or is unavailable")?;
                    runtime.validate_boot_contract(&device, epoch, &contract_key)?;
                    if connection.boot_logo.is_some()
                        || connection.keyboard.is_some()
                        || connection.benchmark.is_some()
                    {
                        return Err("Device already has an active upload or benchmark".into());
                    }
                    if connection.manifest.capabilities.boot_logo.is_none() {
                        return Err("Device does not support a startup logo".into());
                    }
                    let weak = Arc::downgrade(runtime);
                    let guarded_device = device.clone();
                    let receipt = connection
                        .worker
                        .submit_tracked_guarded(
                            connection.worker.generation(),
                            transaction,
                            Box::new(move || {
                                weak.upgrade().is_some_and(|runtime| {
                                    runtime
                                        .validate_boot_contract(
                                            &guarded_device,
                                            epoch,
                                            &contract_key,
                                        )
                                        .is_ok()
                                })
                            }),
                        )
                        .map_err(|e| e.to_string())?;
                    connection.boot_logo_restore = None;
                    connection.boot_logo = Some(crate::boot_logo::PendingUpload {
                        completed: false,
                        receipt,
                        reply: Some(reply.clone()),
                        deadline: Instant::now() + Duration::from_secs(30),
                    });
                    Ok(())
                })();
                if let Err(error) = result {
                    let _ = reply.send(Err(error));
                }
            }
            Request::BenchmarkStart {
                device,
                epoch,
                run_id,
                policy,
                encoding,
                reply,
            } => {
                let result = (|| -> std::result::Result<(), String> {
                    let c = self
                        .connections
                        .get_mut(&device)
                        .filter(|c| c.epoch == epoch)
                        .ok_or("Physical connection changed or is unavailable")?;
                    if c.benchmark.is_some() {
                        return Err("Device already has a benchmark owner".into());
                    }
                    if c.boot_logo.is_some() || c.keyboard.is_some() {
                        return Err("Wait for the startup logo upload to finish".into());
                    }
                    if c.force_individual_updates && policy.mode != OutputMode::IndividualUpdates {
                        return Err("Rendering failed; select Individual updates in device settings before retrying".into());
                    }
                    crate::workspace::DeviceFramePolicy { policy, encoding }
                        .validate(&c.manifest.capabilities)?;
                    if !c.session.lock().unwrap().supports_frame_encoding(encoding) {
                        return Err("Session does not support the requested encoding".into());
                    }
                    c.invalidate_scene();
                    c.playbacks.clear();
                    c.worker
                        .set_policy(policy)
                        .map_err(|error| error.to_string())?;
                    c.scene_revision = c.worker.scene_revision();
                    c.policy = policy;
                    c.encoding = encoding;
                    if let Err(error) = wake_for_benchmark(runtime, c) {
                        restore(runtime, c);
                        return Err(error);
                    }
                    if !c.worker.wait_idle(Duration::from_secs(2)) {
                        restore(runtime, c);
                        return Err("Existing output did not settle before benchmark start".into());
                    }
                    c.worker.drain_outcomes();
                    c.lost_outcomes = c.worker.queue_stats().outcomes_lost;
                    *c.benchmark_input.lock().unwrap() = true;
                    runtime.builtin_state.cancel_device(&device);
                    c.benchmark = Some((run_id, Instant::now()));
                    Ok(())
                })();
                let _ = reply.send(result);
            }
            Request::BenchmarkFrame {
                device,
                epoch,
                run_id,
                commands,
                reply,
            } => {
                let result =
                    benchmark_frame(&mut self.connections, &device, epoch, run_id, commands);
                let _ = reply.send(result);
            }
            Request::BenchmarkStats {
                device,
                epoch,
                run_id,
                reply,
            } => {
                let result = benchmark_connection(&mut self.connections, &device, epoch, run_id)
                    .map(|c| c.worker.queue_stats());
                let _ = reply.send(result);
            }
            Request::BenchmarkOutcomes {
                device,
                epoch,
                run_id,
                reply,
            } => {
                let result = benchmark_connection(&mut self.connections, &device, epoch, run_id)
                .and_then(|c| {
                    if c.worker.queue_stats().outcomes_lost != c.lost_outcomes {
                        c.stop_benchmark();
                        restore(runtime, c);
                        return Err("Benchmark completion history overflowed; run invalidated and current scene restored".into());
                    }
                    let outcomes = c.worker.drain_outcomes();
                    for outcome in &outcomes {
                        if let Some(value) = c
                            .delivery
                            .get_mut(&outcome.surface_id)
                            .filter(|value| value.revision == outcome.content_revision)
                        {
                            value.pending = false;
                            value.completed = outcome.status == OutputStatus::Completed;
                        }
                    }
                    Ok(outcomes)
                });
                let _ = reply.send(result);
            }
            Request::BenchmarkStop {
                device,
                epoch,
                run_id,
                reply,
            } => {
                let result = benchmark_connection(&mut self.connections, &device, epoch, run_id)
                    .map(|c| {
                        c.stop_benchmark();
                        restore(runtime, c);
                    });
                let _ = reply.send(result);
            }
            Request::Wake => {
                // Allow changes arriving during a repaint to schedule another one
                runtime
                    .device_wake_pending
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                for connection in self.connections.values_mut() {
                    refresh_live(runtime, connection);
                }
            }
            Request::Shutdown(reply) => {
                runtime.connection_epochs.lock().unwrap().clear();
                return Some(reply);
            }
            Request::Lost(id, epoch) => {
                if self.connections.get(&id).is_some_and(|c| c.epoch == epoch) {
                    if let Some(c) = self
                        .connections
                        .get_mut(&id)
                        .filter(|c| c.worker.failure().is_some())
                    {
                        if let Some(transition) = c.transition.as_mut() {
                            transition.fail(runtime, &id, c.worker.failure().unwrap());
                        }
                        if c.policy.mode != OutputMode::IndividualUpdates {
                            self.discovery
                                .individual_update_fallbacks
                                .insert(id.clone());
                            runtime.emit("output-fallback", Some(id.clone()), None, Some("Output failed. Reconnect will use Individual updates for this session; saved settings were not changed.".into()));
                        }
                    }
                    self.retire(runtime, &id, Retirement::InputLost);
                }
            }
            Request::Restore => {
                self.discovery.schedule.request(Instant::now());
                self.plugins_dirty = true;
                for connection in self.connections.values_mut() {
                    connection.stop_benchmark();
                    restore(runtime, connection);
                }
            }
            Request::RestoreDevice(id, epoch) => {
                self.plugins_dirty = true;
                if let Some(connection) = self
                    .connections
                    .get_mut(&id)
                    .filter(|connection| connection.epoch == epoch)
                {
                    connection.stop_benchmark();
                    restore(runtime, connection);
                }
            }
            Request::RefreshPluginBindings {
                device,
                epoch,
                sources,
            } => {
                self.plugins_dirty = true;
                if self
                    .connections
                    .get(&device)
                    .is_some_and(|c| c.epoch == epoch)
                {
                    for source in sources {
                        // Redraw current binding output without invalidating playback or transport
                        self.dirty.refresh(&device, &source);
                    }
                }
            }
            Request::Command(id, epoch, cmd) => {
                if let Err(error) =
                    dispatch_command(runtime, &mut self.connections, &id, epoch, cmd)
                {
                    runtime.emit("device-error", Some(id), None, Some(error));
                }
            }
            Request::Playback {
                device,
                control,
                asset,
                lease,
                format,
                epoch,
            } => {
                if let Some(connection) = self.connections.get_mut(&device) {
                    if connection.benchmark.is_some() {
                        return None;
                    }
                    if connection.epoch != epoch {
                        runtime.emit(
                            "media-error",
                            Some(device),
                            None,
                            Some(
                                "Connection changed while preparing media; playback discarded"
                                    .into(),
                            ),
                        );
                        return None;
                    }
                    let clip = match runtime.profiles.clip(&asset).and_then(|clip| {
                        runtime
                            .profiles
                            .media_budget
                            .protect(clip.ok_or("Animation disappeared")?)
                    }) {
                        Ok(clip) => clip,
                        Err(error) => {
                            runtime.emit("media-error", Some(device), None, Some(error));
                            return None;
                        }
                    };
                    let key = control.clone().unwrap_or_else(|| "@background".into());
                    connection.start_playback(
                        key,
                        Playback {
                            vendor_started: None,
                            asset,
                            clip: Some(clip),
                            _assets: lease,
                            control,
                            format,
                            last_frame: None,
                            next_sample: Duration::ZERO,
                            pending_frame: None,
                            media: Default::default(),
                        },
                    );
                }
            }
        }
        None
    }
    // Retire connected devices when device-connection permission is unavailable
    fn enforce_permission_gate(&mut self, runtime: &Runtime) {
        if !runtime.device_connections_enabled() {
            let ids: Vec<_> = self.connections.keys().cloned().collect();
            for id in ids {
                self.retire(runtime, &id, Retirement::PermissionRequired);
            }
        }
    }
    // Discover available devices candidates
    fn discover(&mut self, runtime: &Arc<Runtime>) {
        let now = Instant::now();
        let changed = self
            .discovery
            .changed
            .swap(false, std::sync::atomic::Ordering::AcqRel);
        if self
            .discovery
            .schedule
            .update(now, runtime.device_connections_enabled(), changed)
        {
            self.discovery.scan = now;
            let mut failed = false;
            match hidapi::HidApi::new() {
                Ok(api) => {
                    let mut present = std::collections::HashSet::new();
                    for info in api.device_list() {
                        let Some(module) = hardware_registry::find(&identity(info)) else {
                            continue;
                        };
                        let id = info.path().to_string_lossy().to_string();
                        present.insert(id.clone());
                        if self.connections.contains_key(&id) {
                            continue;
                        }
                        self.discovery.next_epoch += 1;
                        match open_connection(
                            runtime,
                            info,
                            &api,
                            module.as_ref(),
                            self.discovery.next_epoch,
                            self.discovery.individual_update_fallbacks.contains(&id),
                        ) {
                            Ok(connection) => {
                                let name = connection.manifest.name.clone();
                                self.connections.insert(id.clone(), connection);
                                runtime.emit(
                                    "devices",
                                    Some(id),
                                    None,
                                    Some(format!("{name} connected")),
                                );
                            }
                            Err(error) => {
                                failed = true;
                                runtime.emit(
                                    "hid-access-error",
                                    Some(id),
                                    None,
                                    Some(error.to_string()),
                                );
                            }
                        }
                    }
                    let removed: Vec<String> = self
                        .connections
                        .keys()
                        .filter(|id| !present.contains(*id))
                        .cloned()
                        .collect();
                    for id in removed {
                        self.retire(runtime, &id, Retirement::Absent);
                    }
                }
                Err(e) => {
                    failed = true;
                    runtime.emit("hid-enumeration-error", None, None, Some(e.to_string()));
                }
            }
            self.discovery.schedule.complete(Instant::now(), failed);
            runtime.discovery_readiness.finish();
        }
    }
    // Poll plugins
    fn poll_plugins(&mut self, runtime: &Arc<Runtime>) {
        let revision = runtime.state.lock().unwrap().revision;
        if self.plugins_dirty || self.scene_revision != revision {
            self.plugins_dirty = false;
            sync_plugins(runtime);
            self.scene_revision = revision;
        }
        let (effects, plugins_changed) = {
            let mut plugins = runtime.plugins.lock().unwrap();
            let changed = plugins.poll();
            (plugins.drain_effects(), changed)
        };
        let theme_revision = runtime.plugins.lock().unwrap().display_theme_revision();
        let vault_revision = runtime
            .vault_surface_revision
            .load(std::sync::atomic::Ordering::Acquire);
        let theme_changed = theme_revision != self.display_theme_revision;
        if theme_changed || vault_revision != self.vault_surface_revision {
            let mut current = true;
            for connection in self.connections.values_mut() {
                current &= refresh_display_theme(runtime, connection, theme_changed);
            }
            if current {
                if theme_changed {
                    runtime.emit("display-theme", None, None, None);
                }
                self.display_theme_revision = theme_revision;
                self.vault_surface_revision = vault_revision;
            }
        }
        if plugins_changed {
            sync_plugins(runtime);
            runtime.plugin_lifecycle_changed();
        }
        // PluginHost already validated and applied each message in order
        // Its drain is capped at64, so an indefinitely busy plugin cannot
        // postpone this cycle's flush or device input processing
        let plugin_deadline = Instant::now() + Duration::from_millis(16);
        for effect in effects {
            runtime.metrics.record("plugin_effects_received", 1.0);
            if !effect.persistent_visual() {
                flush_plugin_visuals_until(
                    runtime,
                    &mut self.connections,
                    &mut self.dirty,
                    plugin_deadline,
                );
            }
            let feedback = match effect {
                HostEffect::SwitchProfile(effect) => {
                    runtime.queue_vendor_profile_switch(effect);
                    continue;
                }
                HostEffect::DesktopInput(effect) => {
                    crate::desktop_input::submit(runtime, &effect);
                    continue;
                }
                HostEffect::OpenUrl { url, .. } => {
                    let result = crate::actions::Action::OpenUrl { url: url.clone() }
                        .validate(0)
                        .and_then(|_| crate::actions::open(&url));
                    if let Err(error) = result {
                        runtime.emit("plugin-error", None, None, Some(error));
                    }
                    continue;
                }
                HostEffect::Feedback(effect) => effect,
            };
            if let (Some(visual), Some(target)) = (&feedback.visual, &feedback.target) {
                if !target.device.is_empty() && !target.control.is_empty() {
                    runtime.metrics.record("plugin_visual_effects", 1.0);
                    self.dirty.insert(
                        &target.device,
                        &target.control,
                        if feedback.kind.transient() {
                            Some(TransientVisual {
                                context: feedback.context.clone().unwrap_or_default(),
                                generation: feedback.generation,
                                badge: visual["badge"]
                                    .as_str()
                                    .map(|badge| badge.chars().take(16).collect()),
                            })
                        } else {
                            None
                        },
                    );
                    if !feedback.kind.persistent() {
                        flush_plugin_visuals_until(
                            runtime,
                            &mut self.connections,
                            &mut self.dirty,
                            plugin_deadline,
                        );
                    }
                }
            }
            runtime.emit(
                if feedback.kind == FeedbackKind::Unsupported {
                    "plugin-error"
                } else {
                    "plugin-feedback"
                },
                feedback
                    .target
                    .map(|target| target.device)
                    .filter(|device| !device.is_empty()),
                None,
                feedback.error,
            );
        }
        flush_plugin_visuals_until(
            runtime,
            &mut self.connections,
            &mut self.dirty,
            plugin_deadline,
        );
    }
    // Maintain live connections and retire those whose output workers have failed
    fn maintain_connections(&mut self, runtime: &Arc<Runtime>) {
        runtime.vendor_animations.lock().unwrap().prune();
        let mut failed = vec![];
        for (id, c) in self.connections.iter_mut() {
            retry_restore(runtime, c, Instant::now());
            c.blackout.poll(runtime, id);
            if let Err(error) = refresh_contract(runtime, c) {
                runtime.emit("protocol-error", Some(id.clone()), None, Some(error));
                continue;
            }
            if c.benchmark
                .is_some_and(|(_, touched)| touched.elapsed() > Duration::from_secs(30))
            {
                c.stop_benchmark();
                restore(runtime, c);
                runtime.emit(
                    "benchmark-error",
                    Some(id.clone()),
                    None,
                    Some("Inactive benchmark lease expired; current scene restored".into()),
                );
            }
            if c.benchmark.is_none() {
                c.collect_outcomes(runtime);
            }
            let stats = c.worker.queue_stats();
            runtime
                .metrics
                .record("output_pending_bytes", stats.pending_bytes as f64);
            runtime
                .metrics
                .record("output_oldest_pending_ms", stats.oldest_pending_ms);
            // A completed transfer's old wait is not ongoing congestion
            // Current pending age remains available from the worker queue
            c.last_queue_ms = 0.0;
            for (queue_ms, transfer_ms) in c.worker.drain_measurements() {
                c.last_queue_ms = queue_ms;
                runtime.metrics.record("output_queue_wait_ms", queue_ms);
                runtime.metrics.record("output_transfer_ms", transfer_ms);
            }
            transitions::advance(runtime, c);
            refresh_navigation_fallback(runtime, c, Instant::now());
            refresh_draft_preview(runtime, c);
            synchronize_vendor_playback(runtime, id, c);
            animate(runtime, id, c);
            if !c.force_individual_updates && c.policy.mode == OutputMode::IndividualUpdates {
                self.discovery.individual_update_fallbacks.remove(id);
            }
            if c.worker.failure().is_some() {
                if c.policy.mode != OutputMode::IndividualUpdates {
                    self.discovery
                        .individual_update_fallbacks
                        .insert(id.clone());
                    runtime.emit("output-fallback", Some(id.clone()), None,
                Some("Rendering failed. Reconnect will use Individual updates for this session; saved settings were not changed. Select Individual updates before retrying.".into()));
                }
                failed.push(id.clone());
                continue;
            }
            if c.heartbeat.elapsed() > Duration::from_secs(10) {
                let _ = send(c, DeviceCommand::Heartbeat);
                c.heartbeat = Instant::now();
            }
        }
        for id in failed {
            self.retire(runtime, &id, Retirement::OutputFailed);
        }
    }
    // Choose the next device-loop deadline from discovery, heartbeat and playback work
    fn timeout(&self, runtime: &Runtime) -> Duration {
        let now = Instant::now();
        let mut timeout = self
            .discovery
            .schedule
            .remaining(now, runtime.device_connections_enabled());
        for c in self.connections.values() {
            if let Some(deadline) = c
                .restore_retry
                .into_iter()
                .chain(c.boot_logo_restore)
                .chain(c.navigation_fallback.deadline())
                .min()
            {
                timeout = timeout.min(deadline.saturating_duration_since(now));
            }
            // Receipts have no completion waker, so poll only while a persistent upload is active
            if c.boot_logo.is_some() {
                timeout = timeout.min(Duration::from_millis(50));
            }
            timeout = timeout.min(Duration::from_secs(10).saturating_sub(c.heartbeat.elapsed()));
            if c.transition.as_ref().is_some_and(|t| !t.failed) {
                timeout = timeout.min(Duration::from_millis(2));
            }
            if c.transition.is_none() && !c.playbacks.is_empty() {
                timeout = timeout.min(c.next_frame.saturating_duration_since(now));
            }
            runtime
                .metrics
                .record("output_queue_depth", c.worker.queue_depth() as f64);
        }
        if !self.dirty.surfaces.is_empty() {
            timeout = timeout.min(Duration::from_millis(16));
        }
        if let Some(deadline) = runtime.plugins.lock().unwrap().next_deadline() {
            timeout = timeout.min(deadline.saturating_duration_since(now));
        }

        timeout
    }
    // Finalize the pending devices operation
    fn finish(&mut self) {
        for c in self.connections.values_mut() {
            c.stop_benchmark();
            if let Ok(transaction) = c.session.lock().unwrap().encode(DeviceCommand::Sleep) {
                c.worker.finish(transaction);
            }
        }
    }
}

// Start the devices operation
pub(crate) fn start(runtime: Weak<Runtime>, receiver: crate::device_mailbox::Receiver) {
    std::thread::spawn(move || {
        let mut coordinator = Coordinator::new();
        #[cfg(target_os = "macos")]
        let _hotplug = {
            let changed = coordinator.discovery.changed.clone();
            let owner = runtime.clone();
            let observer = hotplug::Observer::start(move || {
                if !changed.swap(true, std::sync::atomic::Ordering::AcqRel) {
                    if let Some(runtime) = owner.upgrade() {
                        let _ = runtime.device_commands.send(Request::Wake);
                    }
                }
            });
            coordinator.discovery.schedule.observe(observer.is_some());
            observer
        };
        let mut media_maintenance = crate::media_budget::Maintenance::default();
        let mut initialize_maintenance = true;
        if let Some(owner) = runtime.upgrade() {
            owner
                .profiles
                .media_budget
                .set_maintenance_waker(receiver.media_waker());
        }
        let mut memory = None;
        let mut shutdown_reply = None;
        let mut pending = None;
        loop {
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            let media_changed =
                receiver.take_media_attention() | std::mem::take(&mut initialize_maintenance);
            let budget = &runtime.profiles.media_budget;
            media_maintenance.service(budget, Instant::now(), media_changed, || {
                if memory.is_none() {
                    memory = sysinfo::System::new().ok();
                }
                if let Some(memory) = &mut memory {
                    memory.refresh_memory();
                    budget
                        .observe_available_memory(memory.available_memory(), memory.total_memory());
                    runtime.metrics.record("media_pressure_samples", 1.0);
                }
            });

            #[cfg(target_os = "macos")]
            coordinator
                .discovery
                .schedule
                .observe(_hotplug.as_ref().is_some_and(hotplug::Observer::is_running));
            coordinator.enforce_permission_gate(&runtime);
            for connection in coordinator.connections.values_mut() {
                if connection
                    .keyboard
                    .as_mut()
                    .is_some_and(|pending| pending.poll(connection.worker.failure()))
                {
                    connection.keyboard = None;
                }
                poll_boot_logo(connection, Instant::now());
            }
            // Shutdown has a dedicated slot and supersedes previously selected work
            if let Some(shutdown) = receiver.take_shutdown() {
                pending = Some(shutdown);
            }
            // Select lazily so stop/shutdown can preempt the next request
            let requests = pending
                .take()
                .into_iter()
                .chain(receiver.try_iter().take(63));
            for mut received in requests {
                // Hold the admission permit until this request finishes dispatch
                let request = received.request.take().expect("unhandled device request");
                if let Some(reply) = coordinator.dispatch(&runtime, request) {
                    shutdown_reply = Some(reply);
                    break;
                }
            }
            if shutdown_reply.is_some() {
                break;
            }
            coordinator.discover(&runtime);
            coordinator.poll_plugins(&runtime);
            coordinator.maintain_connections(&runtime);
            runtime.metrics.record("management_wakeups", 1.0);
            // Empty media budgets add no timer to the device coordinator
            let mut timeout = coordinator.timeout(&runtime);
            if let Some(deadline) = media_maintenance.deadline() {
                timeout = timeout.min(deadline.saturating_duration_since(Instant::now()));
            }
            match receiver.recv_timeout(timeout) {
                Ok(request) => pending = Some(request),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(_) => {}
            }
        }
        if let Some(runtime) = runtime.upgrade() {
            runtime.discovery_readiness.stop();
        }
        coordinator.finish();
        if let Some(reply) = shutdown_reply {
            let _ = reply.send(());
        }
    });
}

// Dispatch command
fn dispatch_command(
    runtime: &Runtime,
    connections: &mut HashMap<String, Connection>,
    id: &str,
    epoch: u64,
    command: DeviceCommand,
) -> std::result::Result<(), String> {
    if matches!(
        command,
        DeviceCommand::Keyboard(_) | DeviceCommand::KeyboardFactoryReset
    ) {
        return Err("Keyboard settings require explicit Apply".into());
    }
    let connection = connections
        .get_mut(id)
        .filter(|connection| connection.epoch == epoch)
        .ok_or("Device connection changed or is unavailable")?;
    if connection.benchmark.is_some() {
        connection.stop_benchmark();
        restore(runtime, connection);
    }
    match &command {
        DeviceCommand::Sleep | DeviceCommand::Clear => {
            runtime.transition_inputs.lock().unwrap().begin(id, epoch);
            runtime.release_transition_inputs(id);
            connection.invalidate_scene();
            connection.sleeping = matches!(&command, DeviceCommand::Sleep);
            if connection.sleeping {
                connection.applied_vibration = None;
            }
            connection.playbacks.clear();
            runtime.vendor_animations.lock().unwrap().clear(id, false);
        }
        DeviceCommand::Wake if connection.manifest.model_id == "k1-pro" => {
            let saved = runtime.snapshot().session(id).output;
            if runtime.power.effective(id, epoch, &saved).sleeping {
                return Err(
                    "Device is still requested asleep; use Wake to update its sleep state".into(),
                );
            }
            // Sleep removes playback and closes scene admission. Sending DIS alone
            // cannot restart media or reopen that gate: rebuild the current scene
            restore(runtime, connection);
            return Ok(());
        }
        DeviceCommand::Wake => {
            connection.sleeping = false;
            // Recreate cleared transient timelines at frame zero on wake
            let _ = runtime.render_scene(id);
        }
        DeviceCommand::Image { control_id, .. } => {
            connection.playbacks.remove(control_id);
        }
        DeviceCommand::Background { .. } => {
            connection.playbacks.remove("@background");
        }
        _ => {}
    }
    send_current(runtime, connection, command)
}

// Refresh contract
fn refresh_contract(runtime: &Runtime, c: &mut Connection) -> std::result::Result<(), String> {
    let Some(device) = runtime
        .state
        .lock()
        .unwrap()
        .devices
        .iter()
        .find(|device| device.id == c.device_id)
        .cloned()
    else {
        return Ok(());
    };
    let contract = crate::device_contract::resolve(&device)?;
    let identity = contract.identity();
    if identity == c.contract_identity {
        return Ok(());
    }
    let module = hardware_registry::modules()
        .into_iter()
        .find(|module| module.manifest().model_id == device.model_id)
        .ok_or("Hardware module unavailable")?;
    let session = module
        .session_for(&contract)
        .map_err(|error| error.to_string())?;
    // A firmware policy change invalidates cached deliveries and pending scenes
    // An already admitted persistent transaction completes its legal group
    c.invalidate_scene();
    c.worker.invalidate_transient();
    c.playbacks.clear();
    *c.session.lock().unwrap() = session;
    c.manifest = contract.manifest;
    c.contract_identity = identity;
    Ok(())
}
// Repaint only after confirmed upload completion without repeating the persistent logo write
fn poll_boot_logo(connection: &mut Connection, now: Instant) {
    if connection
        .boot_logo
        .as_mut()
        .is_some_and(|upload| upload.poll(connection.worker.failure()))
    {
        if connection
            .boot_logo
            .as_ref()
            .is_some_and(|upload| upload.completed)
        {
            connection.boot_logo_restore = Some(now + Duration::from_secs(2));
        }
        connection.boot_logo = None;
    }
}
// Return an uninterrupted blank keyboard page to Navigation through the normal transition gate
fn refresh_navigation_fallback(runtime: &Runtime, c: &mut Connection, now: Instant) {
    if !crate::keyboard_navigation::supported(&c.manifest.model_id) {
        return;
    }
    let suspended = c.sleeping
        || c.transition.is_some()
        || c.restore_retry.is_some()
        || transition_pending(runtime, c)
        || c.benchmark.is_some()
        || c.boot_logo.is_some()
        || c.boot_logo_restore.is_some()
        || runtime.hardware_preview(&c.device_id).is_some();
    let (target, revision) = {
        let state = runtime.state.lock().unwrap();
        let layered = !runtime
            .builtin_state
            .device_overrides(&state, &c.device_id)
            .1
            .is_empty();
        (
            if suspended || layered {
                None
            } else {
                crate::keyboard_navigation::blank_target(&state, &c.device_id)
            },
            state.revision,
        )
    };
    if c.navigation_fallback
        .update(target.as_ref().map(|(p, g)| (p.as_str(), g.as_str())), now)
    {
        let (profile, page) = target.unwrap();
        // Recheck the exact page and connection under the normal navigation lock before committing
        let result = runtime.select_device_profile_checked(
            &c.device_id,
            &profile,
            Some(crate::keyboard_navigation::PAGE_ID),
            false,
            Some(c.epoch),
            None,
            None,
            Some((revision, &profile, &page)),
            None,
        );
        c.navigation_fallback.update(None, now);
        if let Err(error) = result {
            runtime.emit(
                "navigation-error",
                Some(c.device_id.clone()),
                None,
                Some(error),
            );
        }
    }
}
// Retry deferred output restoration without blocking the device actor
fn retry_restore(runtime: &Runtime, c: &mut Connection, now: Instant) {
    if c.restore_retry.is_some_and(|deadline| now >= deadline)
        || c.boot_logo_restore.is_some_and(|deadline| now >= deadline)
    {
        // Consume the one-shot deadline before workspace admission can defer restoration
        c.boot_logo_restore = None;
        restore(runtime, c);
    }
}
// Restore the reviewed devices state
fn restore(runtime: &Runtime, c: &mut Connection) {
    // Admit a consistent snapshot and its media pins before changing any output state.
    // Never park the device actor behind a slow workspace operation while holding state.
    let (snapshot, assets) = {
        let state = match runtime.state.try_lock() {
            Ok(state) => state,
            Err(std::sync::TryLockError::WouldBlock) => {
                c.restore_retry = Some(Instant::now() + Duration::from_millis(25));
                return;
            }
            Err(std::sync::TryLockError::Poisoned(error)) => panic!("{error}"),
        };
        let _transaction = match runtime.profiles.transaction.try_lock() {
            Ok(transaction) => transaction,
            Err(std::sync::TryLockError::WouldBlock) => {
                c.restore_retry = Some(Instant::now() + Duration::from_millis(25));
                return;
            }
            Err(std::sync::TryLockError::Poisoned(error)) => panic!("{error}"),
        };
        let snapshot = state.clone();
        let session = snapshot.session(&c.device_id);
        let ids = snapshot
            .profiles
            .iter()
            .filter(|p| p.id == session.profile_id)
            .flat_map(|p| {
                p.pages
                    .iter()
                    .flat_map(|page| page.bindings.values())
                    .filter_map(|b| b.asset.clone())
                    .chain(p.background_asset.clone())
            });
        let assets = runtime
            .profiles
            .asset_leases
            .pin(ids, "Active device playback");
        drop(_transaction);
        drop(state);
        (snapshot, assets)
    };
    c.restore_retry = None;
    c.boot_logo_restore = None;
    if let Err(error) = refresh_contract(runtime, c) {
        runtime.emit(
            "protocol-error",
            Some(c.device_id.clone()),
            None,
            Some(error),
        );
        return;
    }
    if c.benchmark.is_some() {
        return;
    }
    let previous = c.transition.as_ref().map(|t| (t.generation, t.started));
    if c.transition.is_some() {
        runtime.metrics.record("transition_supersessions", 1.0);
    }
    let generation = {
        let mut inputs = runtime.transition_inputs.lock().unwrap();
        inputs
            .pending(&c.device_id, c.epoch)
            .unwrap_or_else(|| inputs.begin(&c.device_id, c.epoch))
    };
    runtime.release_transition_inputs(&c.device_id);
    c.invalidate_scene();
    c.sleeping = false;
    c.playbacks.clear();
    let assets = match assets {
        Ok(assets) => assets,
        Err(error) => {
            transitions::reject(runtime, c, generation, error);
            return;
        }
    };
    sync_plugins(runtime);
    let mut session = snapshot.session(&c.device_id);
    if session
        .output
        .frame_policy
        .as_ref()
        .is_some_and(|settings| settings.policy.mode == OutputMode::IndividualUpdates)
    {
        c.force_individual_updates = false;
    }
    let settings = session.output.frame_policy.as_ref().filter(|settings| {
        !c.force_individual_updates && settings.validate(&c.manifest.capabilities).is_ok()
    });
    let policy = settings
        .map(|settings| settings.policy)
        .unwrap_or_else(|| OutputPolicy {
            mode: if c.force_individual_updates {
                OutputMode::IndividualUpdates
            } else {
                OutputMode::FrameScheduling
            },
            ..Default::default()
        });
    let encoding = settings
        .map(|settings| settings.encoding)
        .unwrap_or_else(|| {
            if policy.mode == OutputMode::FrameScheduling
                && c.manifest
                    .capabilities
                    .frame_output
                    .as_ref()
                    .is_some_and(|capability| capability.grouped_key_updates)
                && c.session
                    .lock()
                    .unwrap()
                    .supports_frame_encoding(FrameEncoding::GroupedKeyUpdates)
            {
                FrameEncoding::GroupedKeyUpdates
            } else {
                FrameEncoding::IndividualUpdates
            }
        });
    if let Err(error) = c.worker.set_policy(policy) {
        transitions::reject(runtime, c, generation, error.to_string());
        return;
    }
    c.scene_revision = c.worker.scene_revision();
    c.policy = policy;
    c.encoding = encoding;
    session.output = runtime
        .power
        .effective(&c.device_id, c.epoch, &session.output);
    if session.output.sleeping && c.manifest.capabilities.sleep {
        runtime
            .vendor_animations
            .lock()
            .unwrap()
            .clear(&c.device_id, false);
        c.sleeping = true;
        c.applied_vibration = None;
        let _ = send(c, DeviceCommand::Sleep);
        return;
    }
    let profile = snapshot
        .profiles
        .iter()
        .find(|p| p.id == session.profile_id && p.model_id == c.manifest.model_id);
    let _ = send(c, DeviceCommand::Wake);
    if let Err(error) = apply_vibration(runtime, c, &session.output) {
        runtime.emit("device-error", Some(c.device_id.clone()), None, Some(error));
    }
    if c.manifest.capabilities.brightness {
        let _ = send(c, DeviceCommand::Brightness(session.output.brightness));
    }
    if c.manifest.capabilities.lighting {
        let colors = session
            .output
            .colors(c.manifest.capabilities.lighting_channels.len());
        let _ = send(
            c,
            DeviceCommand::Lighting {
                brightness: session.output.lighting_brightness,
                colors,
            },
        );
    }
    if let Some(reason) = &c.manifest.capabilities.image_output_unavailable_reason {
        runtime.emit(
            "protocol-error",
            Some(c.device_id.clone()),
            None,
            Some(reason.clone()),
        );
        runtime
            .transition_inputs
            .lock()
            .unwrap()
            .complete(&c.device_id, c.epoch, generation);
        return;
    }
    let (_, overrides) = runtime
        .builtin_state
        .device_overrides(&snapshot, &c.device_id);
    c.transition = Some(transitions::Transition::new(
        generation,
        profile.cloned(),
        assets,
        &session,
        &overrides,
        &c.manifest,
    ));
    if let Some((old_generation, started)) = previous.filter(|(old, _)| *old == generation) {
        debug_assert_eq!(old_generation, generation);
        c.transition.as_mut().unwrap().started = started;
    }
}

// Publish scene wallpaper
fn publish_scene_wallpaper(
    runtime: &Runtime,
    c: &mut Connection,
    context: &crate::render_scene::RenderSceneContext,
    profile: Option<&crate::Profile>,
) -> bool {
    let Some([width, height]) = c.manifest.capabilities.background_size else {
        return true;
    };
    let active = context.background_media(profile);
    if c.playbacks.get("@background").is_some_and(|current| {
        active.is_some_and(|(asset, options)| asset == current.asset && options == &current.media)
    }) {
        return context.current(runtime);
    }
    if let Ok(data) = runtime.render_scene_wallpaper(
        context,
        profile,
        [width, height],
        c.manifest.layout.rotation,
        &c.manifest.capabilities.image_formats,
    ) {
        if !context.current(runtime) {
            return false;
        }
        let _ = send_current(
            runtime,
            c,
            DeviceCommand::Background {
                data: runtime.owned_render_bytes(data),
                x: 0,
                y: 0,
                width,
                height,
            },
        );
    }
    c.playbacks.remove("@background");
    if let Some((asset, options)) = active {
        let clip = runtime
            .profiles
            .clip(asset)
            .ok()
            .flatten()
            .filter(|clip| clip.frames.len() > 1)
            .and_then(|clip| runtime.profiles.media_budget.protect(clip).ok());
        if let (Some(clip), Ok(assets)) = (
            clip,
            runtime
                .profiles
                .asset_leases
                .pin([asset.to_owned()], "Active device wallpaper"),
        ) {
            c.start_playback(
                "@background".into(),
                Playback {
                    vendor_started: None,
                    asset: asset.into(),
                    clip: Some(clip),
                    _assets: assets,
                    control: None,
                    format: "png".into(),
                    last_frame: None,
                    next_sample: Duration::ZERO,
                    pending_frame: None,
                    media: options.clone(),
                },
            );
        }
    }
    true
}

// Palette changes don't reset the worker, playback clock, or plugin contexts
fn refresh_display_theme(runtime: &Runtime, c: &mut Connection, theme_changed: bool) -> bool {
    if transition_pending(runtime, c) {
        return false;
    }
    if c.sleeping || c.benchmark.is_some() {
        return true;
    }
    let Ok(context) = runtime.render_scene(&c.device_id) else {
        return false;
    };
    let state = runtime.snapshot();
    if !context.matches_snapshot(&state) {
        return false;
    }
    let session = state.session(&c.device_id);
    let profile = state
        .profiles
        .iter()
        .find(|profile| profile.id == session.profile_id);
    if theme_changed && !publish_scene_wallpaper(runtime, c, &context, profile) {
        return false;
    }
    for control in c.manifest.controls.clone() {
        let Some(page) = context.page.as_ref() else {
            break;
        };
        if control.image_slot.is_none() {
            continue;
        }
        let source = context.surface_source(&c.manifest, &control.id);
        let binding = page.bindings.get(source);
        if !theme_changed
            && !context
                .draft
                .as_ref()
                .and_then(|draft| draft.bindings.get(source))
                .or_else(|| context.overrides.get(source).map(|value| &value.binding))
                .or(binding)
                .is_some_and(crate::live_label::has_vault_text)
        {
            continue;
        }
        if let Ok(data) = runtime.render_bound_surface_in_scene(
            &control,
            binding,
            c.manifest.layout.rotation,
            &context.scope,
            source,
            &context,
        ) {
            if !context.current(runtime) {
                return false;
            }
            let _ = send_current(
                runtime,
                c,
                DeviceCommand::Image {
                    control_id: control.id,
                    data: runtime.owned_render_bytes(data),
                },
            );
        }
    }
    true
}

// Refresh draft preview
fn refresh_draft_preview(runtime: &Runtime, c: &mut Connection) {
    c.static_palette.visits = c.static_palette.visits.saturating_add(1);
    if c.sleeping || c.benchmark.is_some() || c.transition.is_some() {
        c.static_palette.scene = None;
    }
    if transition_pending(runtime, c) {
        return;
    }
    let preview = runtime.hardware_preview(&c.device_id);
    let token = preview.as_ref().map(|value| &value.0);
    let changed = c.draft_preview.as_ref().map(|value| &value.0) != token;
    if !changed && (preview.is_none() || c.draft_sample.elapsed() < Duration::from_millis(50)) {
        return;
    }
    let output = (c.scene_revision, c.content_revision, c.delivery.len());
    if !changed
        && c.static_palette.output == Some(output)
        && c.delivery
            .values()
            .all(|delivery| delivery.pending || delivery.completed)
        && c.static_palette
            .scene
            .as_ref()
            .is_some_and(|scene| scene.current(runtime))
    {
        c.static_palette.skipped = c.static_palette.skipped.saturating_add(1);
        return;
    }
    c.static_palette.scene = None;
    let mut controls = std::collections::BTreeSet::new();
    let mut background = false;
    if let Some((_, ids, bg)) = &c.draft_preview {
        controls.extend(ids.iter().cloned());
        background |= *bg;
    }
    if let Some((_, draft)) = &preview {
        controls.extend(draft.bindings.keys().cloned());
        background |= draft.background.is_some()
            || (draft.palette.is_some() || draft.display_test.is_some())
                && c.manifest.capabilities.background;
        if draft.palette.is_some() || draft.display_test.is_some() {
            controls.extend(
                c.manifest
                    .controls
                    .iter()
                    .filter(|control| control.image_slot.is_some())
                    .map(|control| control.id.clone()),
            );
        }
    }
    if changed {
        let mut surfaces: Vec<_> = controls
            .iter()
            .filter_map(|id| {
                c.manifest.control(id).ok().map(|control| {
                    control
                        .feedback_surface
                        .clone()
                        .unwrap_or_else(|| id.clone())
                })
            })
            .collect();
        if background {
            surfaces.push("@background".into());
        }
        c.worker.invalidate_surfaces(&surfaces);
        for surface in surfaces {
            c.delivery.remove(&surface);
        }
    }
    c.draft_preview = preview.as_ref().map(|(id, draft)| {
        (
            id.clone(),
            draft
                .bindings
                .keys()
                .cloned()
                .chain(
                    c.manifest
                        .controls
                        .iter()
                        .filter(|control| {
                            (draft.palette.is_some() || draft.display_test.is_some())
                                && control.image_slot.is_some()
                        })
                        .map(|control| control.id.clone()),
                )
                .collect(),
            draft.background.is_some()
                || (draft.palette.is_some() || draft.display_test.is_some())
                    && c.manifest.capabilities.background,
        )
    });
    c.draft_sample = Instant::now();
    if c.sleeping || c.benchmark.is_some() || transition_pending(runtime, c) {
        return;
    }
    c.static_palette.captures = c.static_palette.captures.saturating_add(1);
    let Ok(context) = runtime.render_scene(&c.device_id) else {
        return;
    };
    let snapshot = runtime.snapshot();
    if !context.matches_snapshot(&snapshot) {
        return;
    }
    let session = snapshot.session(&c.device_id);
    let profile = snapshot
        .profiles
        .iter()
        .find(|p| p.id == session.profile_id);
    let eligible = static_palette_scene(&context, profile);
    let mut published = true;
    if background {
        if let Some([width, height]) = c.manifest.capabilities.background_size {
            if let Ok(data) = runtime.render_scene_wallpaper(
                &context,
                profile,
                [width, height],
                c.manifest.layout.rotation,
                &c.manifest.capabilities.image_formats,
            ) {
                if context.current(runtime) {
                    published &= send_current_surfaces(
                        runtime,
                        c,
                        vec![DeviceCommand::Background {
                            data: runtime.owned_render_bytes(data),
                            x: 0,
                            y: 0,
                            width,
                            height,
                        }],
                        OutputPriority::Interaction,
                    )
                    .is_ok();
                } else {
                    published = false;
                }
            } else {
                published = false;
            }
        }
    }
    let surfaces: std::collections::BTreeSet<_> = controls
        .iter()
        .filter_map(|source| {
            c.manifest.control(source).ok().map(|control| {
                control
                    .feedback_surface
                    .clone()
                    .unwrap_or_else(|| source.clone())
            })
        })
        .collect();
    for surface in surfaces {
        let Ok(control) = c.manifest.control(&surface).cloned() else {
            continue;
        };
        let source = context.surface_source(&c.manifest, &surface).to_owned();
        let binding = context
            .page
            .as_ref()
            .and_then(|page| page.bindings.get(&source));
        if let Ok(data) = runtime.render_bound_surface_in_scene(
            &control,
            binding,
            c.manifest.layout.rotation,
            &context.scope,
            &source,
            &context,
        ) {
            if !context.current(runtime) {
                return;
            }
            published &= send_current_surfaces(
                runtime,
                c,
                vec![DeviceCommand::Image {
                    control_id: control.id,
                    data: runtime.owned_render_bytes(data),
                }],
                OutputPriority::Interaction,
            )
            .is_ok();
        } else {
            published = false;
        }
    }
    if eligible && published && context.current(runtime) {
        c.static_palette.output = Some((c.scene_revision, c.content_revision, c.delivery.len()));
        c.static_palette.scene = Some(context);
    }
}

// Refresh live
fn refresh_live(runtime: &Runtime, c: &mut Connection) {
    if c.sleeping || c.benchmark.is_some() || transition_pending(runtime, c) {
        return;
    }
    let Ok(context) = runtime.render_scene(&c.device_id) else {
        return;
    };
    let Some(page) = context.page.as_ref() else {
        return;
    };
    let controls = c.manifest.controls.clone();
    for control in controls {
        if control.image_slot.is_none() {
            continue;
        }
        let source = context.surface_source(&c.manifest, &control.id);
        let binding = page.bindings.get(source);
        let effective = context
            .draft
            .as_ref()
            .and_then(|draft| draft.bindings.get(source))
            .or_else(|| context.overrides.get(source).map(|value| &value.binding))
            .or(binding);
        if !effective.is_some_and(|b| {
            b.live_label.iter().any(|s| {
                matches!(
                    s,
                    crate::live_label::LabelSegment::MacroState { .. }
                        | crate::live_label::LabelSegment::Parameter { .. }
                )
            }) || b.feedback.as_deref().is_some_and(|f| f != "off")
                || b.appearance
                    .as_ref()
                    .is_some_and(|a| a.toggle_icon.is_some())
        }) {
            continue;
        }
        if let Ok(data) = runtime.render_bound_surface_in_scene(
            &control,
            binding,
            c.manifest.layout.rotation,
            &context.scope,
            source,
            &context,
        ) {
            if !context.current(runtime) {
                break;
            }
            let command = DeviceCommand::Image {
                control_id: control.id,
                data: runtime.owned_render_bytes(data),
            };
            if effective.is_some_and(|b| !b.live_label.is_empty()) {
                // Reuse delivery's pending/completed digest check in individual mode too.
                let _ =
                    send_current_surfaces(runtime, c, vec![command], OutputPriority::Interaction);
            } else {
                let _ = send_current(runtime, c, command);
            }
        }
    }
}
// Read current badge matches
fn current_badge_matches(visual: Option<&serde_json::Value>, badge: Option<&str>) -> bool {
    visual.and_then(|visual| visual["badge"].as_str()) == badge
}
struct TransientVisual {
    context: String,
    generation: Option<u64>,
    badge: Option<String>,
}
#[derive(Default)]
struct DirtyVisuals {
    // The incoming drain is64; retries are capped at512 and fairly rotated
    // Only identities survive hard-generation races, never old pixels
    surfaces: BTreeMap<(String, String), Option<TransientVisual>>,
    order: VecDeque<(String, String)>,
    restore_required: bool,
}
impl DirtyVisuals {
    // A settings refresh cannot overwrite a pending transient or escalate into a scene reset
    fn refresh(&mut self, device: &str, source: &str) {
        let key = (device.to_owned(), source.to_owned());
        if self.surfaces.len() < 512 && !self.surfaces.contains_key(&key) {
            self.order.push_back(key.clone());
            self.surfaces.insert(key, None);
        }
    }

    // Insert an entry into devices
    fn insert(&mut self, device: &str, source: &str, transient: Option<TransientVisual>) {
        let key = (device.into(), source.into());
        if let Some(existing) = self.surfaces.get_mut(&key) {
            *existing = transient;
            return;
        }
        if self.surfaces.len() >= 512 {
            self.restore_required = true;
            return;
        }
        self.order.push_back(key.clone());
        self.surfaces.insert(key, transient);
    }
}
#[cfg(test)]
// Flush plugin visuals
fn flush_plugin_visuals(
    runtime: &Runtime,
    connections: &mut HashMap<String, Connection>,
    dirty: &mut DirtyVisuals,
) {
    flush_plugin_visuals_until(
        runtime,
        connections,
        dirty,
        Instant::now() + Duration::from_millis(16),
    );
}
// Flush plugin visuals until
fn flush_plugin_visuals_until(
    runtime: &Runtime,
    connections: &mut HashMap<String, Connection>,
    dirty: &mut DirtyVisuals,
    deadline: Instant,
) {
    if std::mem::take(&mut dirty.restore_required) {
        runtime
            .metrics
            .record("plugin_dirty_overflow_restores", 1.0);
        let _ = runtime.device_commands.send(Request::Restore);
    }
    let mut surfaces = Vec::new();
    for _ in 0..64 {
        let Some(key) = dirty.order.pop_front() else {
            break;
        };
        if let Some(value) = dirty.surfaces.remove(&key) {
            surfaces.push((key, value));
        }
    }
    let mut contexts = HashMap::new();
    let mut surfaces = surfaces.into_iter();
    while let Some(((device, source), transient)) = surfaces.next() {
        if Instant::now() >= deadline {
            dirty.insert(&device, &source, transient);
            for ((device, source), transient) in surfaces {
                dirty.insert(&device, &source, transient);
            }
            break;
        }
        let Some(connection) = connections.get_mut(&device) else {
            continue;
        };
        if transition_pending(runtime, connection) {
            dirty.insert(&device, &source, transient);
            continue;
        }
        if connection.sleeping || connection.benchmark.is_some() {
            continue;
        }
        let Ok(control) = connection.manifest.control(&source) else {
            continue;
        };
        let output = control
            .feedback_surface
            .as_deref()
            .unwrap_or(&source)
            .to_string();
        let Ok(control) = connection.manifest.control(&output).cloned() else {
            continue;
        };
        let Ok(context) = contexts
            .entry(device.clone())
            .or_insert_with(|| runtime.render_scene(&device))
        else {
            dirty.insert(&device, &source, None);
            continue;
        };
        // Explicit touch assignments, including cleared ones, outrank dial feedback
        let Some(page) = context.page.as_ref() else {
            continue;
        };
        if !page.bindings.contains_key(&source)
            || context.surface_source(&connection.manifest, &output) != source
        {
            continue;
        }
        if let Some(value) = transient.as_ref() {
            let valid = value.generation.is_some_and(|generation| {
                context.permits_transient(&source, &value.context, generation)
            }) && current_badge_matches(
                context.visuals.get(&source).map(AsRef::as_ref),
                value.badge.as_deref(),
            );
            if !valid {
                runtime.metrics.record("obsolete_plugin_transients", 1.0);
            }
            // Reject the old badge, not the redraw. An unrelated lifecycle edit
            // may change a conservative generation while the authoritative current
            // badge is still visible. Never resurrect an expired/superseded badge
        }
        let binding = page.bindings.get(&source);
        // A transient is a flush barrier, not last-wins persistent state. Its
        // host badge is retained for the existing1500ms host lifetime; ordinary
        // subsequent state messages do not clear it. Transport is still async,
        // so this is not an optical presentation guarantee
        let data = runtime.render_bound_surface_output(
            &control,
            binding,
            connection.manifest.layout.rotation,
            &context.scope,
            &source,
            context,
        );
        if !context.current(runtime) {
            runtime.metrics.record("obsolete_render_batches", 1.0);
            dirty.insert(&device, &source, None);
            continue;
        }
        if let Ok((data, ready)) = data {
            runtime.metrics.record("plugin_dirty_surfaces", 1.0);
            let command = DeviceCommand::Image {
                control_id: output,
                data: runtime.owned_render_bytes(data),
            };
            let _ = send_current_output(runtime, connection, command, ready);
        }
    }
}
// Estimate queued animation opportunities using batching and pending-byte pressure
fn animation_queue_depth(policy: OutputPolicy, stats: &OutputQueueStats) -> usize {
    let mut depth = if policy.mode == OutputMode::FrameScheduling {
        // A healthy multi-key frame is one opportunity, not 15 legacy frames
        stats.pending_count.div_ceil(policy.max_batch_surfaces)
    } else {
        stats.pending_count
    };
    if stats.pending_bytes > policy.max_pending_bytes / 2 {
        depth = depth.max(3);
    }
    depth
}
// Synchronize vendor playback using the current devices state
fn synchronize_vendor_playback(runtime: &Runtime, device: &str, c: &mut Connection) {
    if c.sleeping || c.benchmark.is_some() || transition_pending(runtime, c) {
        return;
    }
    let retry_due = runtime
        .vendor_animations
        .lock()
        .unwrap()
        .retry_due(device, false);
    if retry_due {
        let _ = runtime.render_scene(device);
    }
    let active = runtime
        .vendor_animations
        .lock()
        .unwrap()
        .active(device, false);
    let mut wanted = HashMap::new();
    for (source, animation) in active {
        let Ok(control) = c.manifest.control(&source) else {
            continue;
        };
        let output = control
            .feedback_surface
            .as_deref()
            .unwrap_or(&source)
            .to_owned();
        wanted.insert(output, animation);
    }
    c.playbacks.retain(|key, playback| {
        playback.vendor_started.is_none()
            || wanted.get(key).is_some_and(|animation| {
                animation.asset == playback.asset
                    && Some(animation.started) == playback.vendor_started
            })
    });
    for (output, animation) in wanted {
        if c.playbacks.get(&output).is_some_and(|p| {
            p.vendor_started == Some(animation.started) && p.asset == animation.asset
        }) {
            continue;
        }
        let Ok(clip) = runtime.profiles.media_budget.protect(animation.clip) else {
            continue;
        };
        let Ok(lease) = runtime
            .profiles
            .asset_leases
            .pin([], "Transient vendor animation")
        else {
            continue;
        };
        c.start_playback(
            output.clone(),
            Playback {
                vendor_started: Some(animation.started),
                asset: animation.asset,
                clip: Some(clip),
                _assets: lease,
                control: Some(output),
                format: "png".into(),
                last_frame: None,
                pending_frame: None,
                next_sample: Duration::ZERO,
                media: Default::default(),
            },
        );
    }
}
// Advance the devices animation
fn animate(runtime: &Runtime, device: &str, c: &mut Connection) {
    if c.benchmark.is_some()
        || transition_pending(runtime, c)
        || c.sleeping
        || c.playbacks.is_empty()
        || Instant::now() < c.next_frame
    {
        return;
    }
    c.collect_outcomes(runtime);
    let stats = c.worker.queue_stats();
    let depth = animation_queue_depth(c.policy, &stats);
    c.animation_rate.observe(
        depth,
        c.last_queue_ms.max(stats.oldest_pending_ms),
        Instant::now(),
    );
    let clips = crate::profiles::ClipCycle::default();
    for playback in c.playbacks.values_mut() {
        if let Ok(Some(clip)) = playback.source_clip(&runtime.profiles, &clips) {
            if playback
                .clip
                .as_ref()
                .is_none_or(|held| !Arc::ptr_eq(&held.clip, &clip))
            {
                if let Ok(clip) = runtime.profiles.media_budget.protect(clip) {
                    playback.clip = Some(clip);
                    // A changed validated source has a new frame identity even
                    // when its timeline happens to select the same frame number
                    playback.last_frame = None;
                    playback.pending_frame = None;
                }
            }
        }
    }
    let active_fps = c
        .playbacks
        .values()
        .filter_map(|p| {
            let clip = p.source_clip(&runtime.profiles, &clips).ok().flatten()?;
            Some(c.animation_rate.rate(
                p.media.requested_fps(&clip),
                p.media.min_fps.unwrap_or(5.0),
                Instant::now(),
            ))
        })
        .fold(0.001, f64::max);
    let frame_period = Duration::from_secs_f64(1.0 / active_fps);
    let missed = Instant::now()
        .saturating_duration_since(c.next_frame)
        .as_secs_f64()
        / frame_period.as_secs_f64();
    if missed >= 1.0 {
        runtime
            .metrics
            .record("skipped_animation_opportunities", missed.floor());
    }
    // Source timestamps and each playback's cap/backoff deadline already define
    // the next opportunity. A second independently phased frame clock can postpone
    // a source frame that is ready, especially after a slow cold render
    c.next_frame = Instant::now() + frame_period;
    let elapsed = runtime.media_elapsed();
    if let Some(delay) = c
        .playbacks
        .values()
        .filter_map(|p| {
            let clip = p.source_clip(&runtime.profiles, &clips).ok().flatten()?;
            Some(
                p.media
                    .next_frame_delay(&clip, p.elapsed(elapsed))
                    .max(p.next_sample.saturating_sub(elapsed)),
            )
        })
        .min()
    {
        c.next_frame = Instant::now() + delay;
    }
    if !c.playbacks.values().any(|playback| {
        // Inspect only the cached clip timeline before capturing bindings,
        // plugin visuals, feedback and fonts. Pending/delivered identical frames
        // have no new pixels. Errors still enter the normal reporting path
        playback
            .source_clip(&runtime.profiles, &clips)
            .map_or(true, |clip| {
                clip.is_none_or(|clip| {
                    if elapsed < playback.next_sample {
                        return false;
                    }
                    let index = playback.media.frame_index(&clip, playback.elapsed(elapsed));
                    playback.last_frame != Some(index)
                        && !playback
                            .pending_frame
                            .is_some_and(|(pending, _)| pending == index)
                })
            })
    }) {
        runtime
            .metrics
            .record("unchanged_animation_opportunities", 1.0);
        return;
    }
    // Rendering is synchronous but workspace edits can arrive from another thread
    // Recheck the scene token before enqueue instead of publishing obsolete CPU work
    let Ok(mut context) = runtime.render_scene(device) else {
        return;
    };
    context.clips = clips;
    // A palette supplies only the fallback wallpaper and must not freeze custom backgrounds
    let palette_owns_background = context
        .draft
        .as_ref()
        .is_some_and(|draft| draft.palette.is_some())
        && {
            let snapshot = runtime.snapshot();
            let session = snapshot.session(device);
            !snapshot
                .profiles
                .iter()
                .find(|profile| profile.id == session.profile_id)
                .is_some_and(|profile| profile.background_asset.is_some())
        };
    let keys: Vec<_> = c.playbacks.keys().cloned().collect();
    let start_cursor = c.playback_cursor;
    let mut due = Vec::new();
    let mut indices = HashMap::new();
    let render_deadline =
        Instant::now() + Duration::from_millis(u64::from(c.policy.max_batch_estimated_ms));
    for offset in 0..keys.len() {
        let position = (start_cursor + offset) % keys.len();
        let key = &keys[position];
        let playback = c.playbacks.get(key).unwrap();
        let result: std::result::Result<_, String> = (|| {
            if context.media_elapsed < playback.next_sample {
                return Ok(None);
            }
            let clip = playback
                .source_clip(&runtime.profiles, &context.clips)?
                .ok_or("Animation disappeared")?;
            let index = playback
                .media
                .frame_index(&clip, playback.elapsed(context.media_elapsed));
            if playback.last_frame == Some(index)
                || playback
                    .pending_frame
                    .is_some_and(|(pending, _)| pending == index)
            {
                return Ok(None);
            }
            let (size, source) = if let Some(control) = &playback.control {
                let surface = c.manifest.control(control).map_err(|e| e.to_string())?;
                (
                    [
                        surface.image_width.ok_or("no image surface")?,
                        surface.image_height.ok_or("no image surface")?,
                    ],
                    Some(control.as_str()),
                )
            } else {
                (
                    c.manifest
                        .capabilities
                        .background_size
                        .ok_or("no background surface")?,
                    None,
                )
            };
            if source.is_some_and(|surface| context.draft_owns_surface(&c.manifest, surface))
                || source.is_none()
                    && context.draft.as_ref().is_some_and(|draft| {
                        draft.background.is_some()
                            || draft.display_test.is_some()
                            || palette_owns_background
                    })
            {
                return Ok(None);
            }
            if let Some(source) = source {
                let source = context
                    .page
                    .as_ref()
                    .map(|p| p.surface_source(&c.manifest, source))
                    .unwrap_or(source);
                if context.visuals.contains_key(source) && playback.vendor_started.is_none() {
                    return Ok(None);
                }
            }
            if let Some(started) = playback.vendor_started {
                let active = runtime
                    .vendor_animations
                    .lock()
                    .unwrap()
                    .active(device, false);
                let control = playback.control.as_deref().unwrap_or("");
                let source = context
                    .page
                    .as_ref()
                    .map(|page| page.surface_source(&c.manifest, control))
                    .unwrap_or(control);
                if !active
                    .get(source)
                    .is_some_and(|a| a.asset == playback.asset && a.started == started)
                {
                    return Ok(None);
                }
            }
            let render_start = Instant::now();
            let data = if let Some(id) = &playback.control {
                let page = context.page.as_ref();
                let source = page
                    .map(|p| p.surface_source(&c.manifest, id))
                    .unwrap_or(id);
                let binding = page.and_then(|p| p.bindings.get(source));
                if playback.vendor_started.is_some()
                    || binding.is_some_and(|b| b.asset.as_deref() == Some(&playback.asset))
                {
                    runtime.render_bound_surface_in_scene(
                        c.manifest.control(id).map_err(|e| e.to_string())?,
                        binding,
                        c.manifest.layout.rotation,
                        &context.scope,
                        source,
                        &context,
                    )?
                } else {
                    runtime.profiles.render_media_shared(
                        &playback.asset,
                        context.media_elapsed,
                        size[0],
                        size[1],
                        c.manifest.layout.rotation,
                        &playback.format,
                        &playback.media,
                    )?
                }
            } else if playback.format == "png" {
                runtime.render_animation_frame_in_cycle(
                    &clip,
                    index,
                    size,
                    c.manifest.layout.rotation,
                    &playback.media,
                    &c.manifest.capabilities.image_formats,
                    None,
                    context.wallpaper_target,
                    &context.clips,
                )?
            } else {
                // Standalone uploads can request an explicit format. Profile
                // backgrounds use shared PNG rendering and select their final
                // encoding from media settings
                let bytes = runtime.profiles.render_media_shared(
                    &playback.asset,
                    context.media_elapsed,
                    size[0],
                    size[1],
                    c.manifest.layout.rotation,
                    &playback.format,
                    &playback.media,
                )?;
                runtime.encode_media_shared(
                    bytes,
                    &playback.asset,
                    &playback.media,
                    &c.manifest.capabilities.image_formats,
                )?
            };
            runtime
                .metrics
                .record("render_ms", render_start.elapsed().as_secs_f64() * 1000.0);
            let command = if let Some(control) = &playback.control {
                DeviceCommand::Image {
                    control_id: control.clone(),
                    data: runtime.owned_render_bytes(data),
                }
            } else {
                DeviceCommand::Background {
                    data: runtime.owned_render_bytes(data),
                    x: 0,
                    y: 0,
                    width: size[0],
                    height: size[1],
                }
            };
            let age =
                playback
                    .media
                    .frame_age(&clip, index, playback.elapsed(context.media_elapsed));
            let presentation = runtime.media_start + context.media_elapsed.saturating_sub(age);
            Ok(Some((index, presentation, command)))
        })();
        if let Some(playback) = c.playbacks.get_mut(key) {
            let source = playback
                .source_clip(&runtime.profiles, &context.clips)
                .ok()
                .flatten()
                .map_or(24.0, |clip| playback.media.requested_fps(&clip));
            let rate = c.animation_rate.rate(
                source,
                playback.media.min_fps.unwrap_or(5.0),
                Instant::now(),
            );
            let period = 1.0 / rate;
            playback.next_sample = Duration::from_secs_f64(
                ((context.media_elapsed.as_secs_f64() / period).floor() + 1.0) * period,
            );
        }
        match result {
            Ok(Some((index, presentation, command))) => {
                c.playback_cursor = (position + 1) % keys.len();
                indices.insert(key.clone(), (index, presentation));
                due.push(command);
                if due.len() >= c.policy.max_batch_surfaces || Instant::now() >= render_deadline {
                    break;
                }
            }
            Ok(None) => {}
            Err(error) => {
                c.playbacks.remove(key);
                runtime.emit("media-error", Some(device.into()), None, Some(error));
            }
        }
    }
    if !due.is_empty() {
        if !context.current(runtime) {
            c.invalidate_scene();
            runtime.metrics.record("obsolete_render_batches", 1.0);
            return;
        }
        match send_current_surfaces(runtime, c, due, OutputPriority::Animation) {
            Ok(accepted) => {
                for (surface, revision) in accepted {
                    if let Some((_, presentation)) = indices.get(&surface) {
                        if let Some(delivery) = c
                            .delivery
                            .get_mut(&surface)
                            .filter(|d| d.revision == revision)
                        {
                            delivery.presentation = Some(*presentation);
                        }
                    }
                }
                // A new source index may encode to pixels already pending or
                // completed (including the initial restore). It still represents
                // this animation frame: don't repeatedly render it waiting for a
                // submission/outcome that duplicate suppression won't create
                for (surface, (index, _)) in indices {
                    if let (Some(playback), Some(delivery)) =
                        (c.playbacks.get_mut(&surface), c.delivery.get(&surface))
                    {
                        if delivery.completed {
                            playback.last_frame = Some(index);
                            playback.pending_frame = None;
                        } else if delivery.pending {
                            playback.pending_frame = Some((index, delivery.revision));
                        }
                    }
                }
            }
            Err(error) => runtime.emit("media-error", Some(device.into()), None, Some(error)),
        }
    }
}

// Collect plugin action nodes, including referenced saved macros, with their action paths
fn plugin_nodes<'a>(
    action: &'a crate::actions::Action,
    macros: &'a [crate::builtins::SavedMacro],
) -> Vec<(Vec<usize>, &'a crate::actions::Action)> {
    // Visit the current devices value
    fn visit<'a>(
        action: &'a crate::actions::Action,
        path: Vec<usize>,
        nodes: &mut Vec<(Vec<usize>, &'a crate::actions::Action)>,
        macros: &'a [crate::builtins::SavedMacro],
    ) {
        use crate::actions::Action;
        if path.len() > 8 {
            return;
        }
        match action {
            Action::Builtin { action }
                if matches!(&**action, crate::builtins::BuiltinAction::SavedMacro { .. }) =>
            {
                if let crate::builtins::BuiltinAction::SavedMacro { macro_id, .. } = &**action {
                    if let Some(m) = macros.iter().find(|m| m.id == *macro_id) {
                        let mut p = path;
                        p.push(0);
                        visit(&m.action, p, nodes, macros);
                    }
                }
            }
            Action::Plugin { .. } => nodes.push((path, action)),
            Action::Builtin { action } => {
                for (index, child) in action.children().iter().enumerate() {
                    let mut p = path.clone();
                    p.push(index);
                    visit(child, p, nodes, macros);
                }
            }
            Action::Macro { actions }
            | Action::Dial { actions, .. }
            | Action::Events { actions, .. } => {
                for (index, action) in actions.iter().enumerate() {
                    let mut child = path.clone();
                    child.push(index);
                    visit(action, child, nodes, macros);
                }
            }
            Action::Toggle { on, off } => {
                let mut left = path.clone();
                left.push(0);
                visit(on, left, nodes, macros);
                let mut right = path;
                right.push(1);
                visit(off, right, nodes, macros);
            }
            _ => {}
        }
    }
    let mut nodes = vec![];
    visit(action, vec![], &mut nodes, macros);
    nodes
}
// Collect plugin visibility and input demand from connected devices’ effective bindings
pub(crate) fn plugin_demand(
    snapshot: &crate::Snapshot,
) -> std::collections::HashMap<String, Vec<stream_dock_manager_plugin_host::PluginDemand>> {
    let mut required = std::collections::HashMap::<
        String,
        Vec<stream_dock_manager_plugin_host::PluginDemand>,
    >::new();
    for device in snapshot.devices.iter().filter(|d| d.connected) {
        let session = snapshot.session(&device.id);
        let Some(profile) = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id && p.model_id == device.model_id)
        else {
            continue;
        };
        for page in &profile.pages {
            let mut effective = page.bindings.clone();
            if let Some(overrides) = snapshot.binding_overrides.get(&device.id) {
                for (id, value) in overrides {
                    effective.insert(id.clone(), value.binding.clone());
                }
            }
            for (id, binding) in effective.iter().filter(|(_, b)| !b.disabled) {
                let Ok(control) = device.manifest.control(id) else {
                    continue;
                };
                if control.kind == "display" {
                    continue;
                }
                let qualified = match &binding.action {
                    crate::actions::Action::Events { actions, .. } => Some(
                        crate::event_bindings::active_slots(snapshot, &device.id, id, actions),
                    ),
                    _ => None,
                };
                for (path, node) in plugin_nodes(&binding.action, &snapshot.saved_macros) {
                    if qualified
                        .is_some_and(|mask| path.first().is_none_or(|slot| mask & (1 << slot) == 0))
                    {
                        continue;
                    }
                    let crate::actions::Action::Plugin {
                        plugin_id,
                        action_id,
                        ..
                    } = node
                    else {
                        continue;
                    };
                    if !snapshot.plugins.iter().any(|p| {
                        p.id == *plugin_id
                            && p.actions.iter().any(|a| {
                                a.id == *action_id && a.control_types.contains(&control.kind)
                            })
                    }) {
                        continue;
                    }
                    let reference = stream_dock_manager_plugin_host::PluginDemand {
                        device_id: device.id.clone(),
                        profile_id: profile.id.clone(),
                    };
                    let values = required.entry(plugin_id.clone()).or_default();
                    if !values.contains(&reference) {
                        values.push(reference);
                    }
                }
            }
        }
    }
    required
}
// Describe actual feedback-surface geometry while preserving the input control identity
fn plugin_surface(manifest: &ModelManifest, control: &Control) -> serde_json::Value {
    // An encoder may render onto a separate touch display. Describe the actual
    // output geometry while retaining the input control's identity and kind
    let output = control
        .feedback_surface
        .as_deref()
        .and_then(|id| manifest.control(id).ok())
        .unwrap_or(control);
    serde_json::json!({
        "contract_version": 1,
        "control_id": control.id,
        "width": output.image_width,
        "height": output.image_height,
        "feedback_target": output.id,
        "kind": control.kind,
    })
}

// Translate authored font and alignment settings into host title-rendering parameters
fn authored_title_parameters(
    appearance: Option<&hardware_core::appearance::Appearance>,
    height: Option<u16>,
) -> Option<serde_json::Value> {
    let appearance = appearance?;
    let height = height.filter(|height| *height > 0)?;
    let fonts = hardware_core::fonts::database_for(&appearance.font);
    Some(serde_json::json!({
        "fontFamily": hardware_core::fonts::family(&appearance.font, &fonts),
        "fontSize": (f64::from(height) * f64::from(appearance.size) / 100.0).max(6.0),
        // The actual title SVG renderer uses regular, undecorated text
        "fontStyle": "Regular",
        "fontUnderline": false,
        "showTitle": appearance.show_title,
        "titleAlignment": match appearance.position.as_str() { "top" => "top", "bottom" => "bottom", _ => "middle" },
        "titleColor": appearance.foreground,
    }))
}

// Read only committed profiles, including their nonvisible pages and nested actions.
pub(crate) fn configured_plugin_actions(
    profiles: &[crate::profiles::Profile],
    macros: &[crate::builtins::SavedMacro],
) -> std::collections::BTreeSet<String> {
    use stream_dock_manager_plugin_host::CONFIGURED_ACTIONS_OWNER;
    profiles
        .iter()
        .flat_map(|profile| &profile.pages)
        .flat_map(|page| page.bindings.values())
        .filter(|binding| !binding.disabled)
        .flat_map(|binding| plugin_nodes(&binding.action, macros))
        .filter_map(|(_, action)| match action {
            crate::actions::Action::Plugin {
                plugin_id,
                action_id,
                ..
            } if plugin_id == CONFIGURED_ACTIONS_OWNER => Some(action_id.clone()),
            _ => None,
        })
        .collect()
}

// Reconcile plugin lifecycle, input settings and surface descriptions with connected devices
pub(crate) fn sync_plugins(runtime: &Runtime) {
    let snapshot = runtime.snapshot();
    let required = plugin_demand(&snapshot);
    let needs_configured = runtime
        .plugins
        .lock()
        .unwrap()
        .needs_configured_actions(snapshot.configuration_revision);
    let configured = needs_configured
        .then(|| configured_plugin_actions(&snapshot.profiles, &snapshot.saved_macros));
    let mut actions = vec![];
    let mut addresses = vec![];
    let mut execution_settings = vec![];
    let mut title_updates = vec![];
    let mut devices = vec![];
    for device in snapshot.devices.iter().filter(|d| d.connected) {
        let session = snapshot.session(&device.id);
        let Some(profile) = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
        else {
            continue;
        };
        let Some(page) = profile.pages.iter().find(|p| p.id == session.page_id) else {
            continue;
        };
        let keys: Vec<_> = device
            .manifest
            .controls
            .iter()
            .filter(|c| c.kind == "button")
            .collect();
        let columns: std::collections::BTreeSet<_> = keys.iter().map(|c| c.x).collect();
        let rows: std::collections::BTreeSet<_> = keys.iter().map(|c| c.y).collect();
        let sdk_type = crate::vendor_profiles::device_type(device).unwrap_or(
            if device.manifest.controls.iter().any(|c| c.kind == "encoder") {
                7
            } else {
                0
            },
        );
        devices.push(serde_json::json!({"id":device.id,"name":device.name,"type":sdk_type,"size":{"columns":columns.len(),"rows":rows.len()},"capabilities":device.manifest.capabilities}));
        if device.model_id != profile.model_id
            || runtime
                .effective_device_output(&device.id)
                .is_ok_and(|output| output.sleeping)
        {
            // Sleep hides action contexts and releases their owned plugin input,
            // while profile demand keeps background timers/runtime state alive
            continue;
        }
        let mut effective = page.bindings.clone();
        if let Some(overrides) = snapshot.binding_overrides.get(&device.id) {
            for (id, value) in overrides {
                effective.insert(id.clone(), value.binding.clone());
            }
        }
        for (id, binding) in &effective {
            let qualified = match &binding.action {
                crate::actions::Action::Events { actions, .. } => Some(
                    crate::event_bindings::active_slots(&snapshot, &device.id, id, actions),
                ),
                _ => None,
            };
            if qualified.is_none() && !crate::touch_panel::active_control(&device.manifest, id) {
                continue;
            }
            if binding.disabled {
                continue;
            }
            for (path, action) in plugin_nodes(&binding.action, &snapshot.saved_macros) {
                if qualified
                    .is_some_and(|mask| path.first().is_none_or(|slot| mask & (1 << slot) == 0))
                {
                    continue;
                }
                let crate::actions::Action::Plugin {
                    plugin_id,
                    action_id,
                    settings,
                    ..
                } = action
                else {
                    continue;
                };
                let Ok(control) = device.manifest.control(id) else {
                    continue;
                };
                if control.kind == "display" {
                    continue;
                }
                if !snapshot.plugins.iter().any(|p| {
                    p.id == *plugin_id
                        && p.actions
                            .iter()
                            .any(|a| a.id == *action_id && a.control_types.contains(&control.kind))
                }) {
                    continue;
                }
                let group: Vec<_> = device
                    .manifest
                    .controls
                    .iter()
                    .filter(|c| c.kind == control.kind)
                    .collect();
                let xs: std::collections::BTreeSet<_> = group.iter().map(|c| c.x).collect();
                let ys: std::collections::BTreeSet<_> = group.iter().map(|c| c.y).collect();
                let base_scope = format!("{}:{}", profile.id, page.id);
                let scope = snapshot
                    .binding_overrides
                    .get(&device.id)
                    .and_then(|m| m.get(id))
                    .map(|value| value.scope.as_str())
                    .unwrap_or(&base_scope);
                let scope = if qualified.is_some() {
                    let Some((&index, children)) = path.split_first() else {
                        continue;
                    };
                    let Some(slot) = crate::actions::EventSlot::from_index(index) else {
                        continue;
                    };
                    crate::actions::node_scope(
                        &crate::event_bindings::event_scope(scope, slot),
                        children,
                    )
                } else {
                    crate::actions::node_scope(scope, &path)
                };
                let override_ = snapshot
                    .binding_overrides
                    .get(&device.id)
                    .and_then(|m| m.get(id));
                let address = match crate::builtins::resolve_reference_path(
                    &binding.action,
                    &path,
                    &snapshot.saved_macros,
                ) {
                    Ok((Some(macro_id), local)) => {
                        serde_json::json!({"macro_id":macro_id,"path":local})
                    }
                    _ => {
                        serde_json::json!({"profile_id":override_.map(|v|v.profile_id.as_str()).unwrap_or(&profile.id),"page_id":override_.map(|v|v.page_id.as_str()).unwrap_or(&page.id),"control_id":id,"path":path})
                    }
                };
                addresses.push((
                    plugin_id.clone(),
                    scope.clone(),
                    device.id.clone(),
                    id.clone(),
                    address,
                ));
                let resolved = match runtime.resolve_parameters(action, &device.id, &scope) {
                    Ok(crate::actions::Action::Plugin { settings, .. }) => settings,
                    _ => continue,
                };
                let resources = match action {
                    crate::actions::Action::Plugin { resources, .. } => {
                        match crate::vendor_resources::paths(&runtime.profiles.root, resources) {
                            Ok(paths) => paths,
                            Err(error) => {
                                runtime.emit(
                                    "plugin-error",
                                    Some(device.id.clone()),
                                    None,
                                    Some(format!("{id}: {error}")),
                                );
                                continue;
                            }
                        }
                    }
                    _ => Default::default(),
                };
                execution_settings.push((
                    plugin_id.clone(),
                    scope.clone(),
                    device.id.clone(),
                    id.clone(),
                    settings.clone(),
                    resolved,
                    resources,
                ));
                let output = control
                    .feedback_surface
                    .as_deref()
                    .and_then(|id| device.manifest.control(id).ok())
                    .unwrap_or(control);
                let parameters =
                    authored_title_parameters(binding.appearance.as_deref(), output.image_height);
                title_updates.push((
                    plugin_id.clone(),
                    scope.clone(),
                    device.id.clone(),
                    id.clone(),
                    binding.title.clone(),
                    parameters,
                    action_id.clone(),
                    binding
                        .plugin_appearance
                        .as_ref()
                        .filter(|p| p.plugin_id == *plugin_id && p.action_id == *action_id)
                        .and_then(|p| p.vendor_profile.clone()),
                ));
                actions.push(stream_dock_manager_plugin_host::ActiveAction {
                    plugin_id: plugin_id.clone(),
                    action_id: action_id.clone(),
                    device_id: device.id.clone(),
                    control_id: id.clone(),
                    scope,
                    settings: settings.clone(),
                    surface: Some(plugin_surface(&device.manifest, control)),
                    column: xs.iter().position(|x| *x == control.x).unwrap_or(0),
                    row: ys.iter().position(|y| *y == control.y).unwrap_or(0),
                    encoder: matches!(control.kind.as_str(), "encoder" | "touch"),
                });
            }
        }
    }
    // Publish demand and visible-page priority together, so polling cannot
    // launch a queued off-page plugin before the current page is synchronized
    let mut plugins = runtime.plugins.lock().unwrap();
    if let Some(configured) = configured {
        plugins.set_configured_actions(snapshot.configuration_revision, configured);
    }
    plugins.reconcile_visible_demand(required, &actions);
    for (plugin, scope, device, control, durable, resolved, resources) in execution_settings {
        plugins.set_action_resources(
            &plugin,
            &scope,
            &device,
            &control,
            serde_json::json!(resources),
        );
        plugins.set_execution_settings(
            &plugin,
            &scope,
            &device,
            &control,
            &durable,
            &resolved,
            runtime.credentials.redactions(),
        );
    }
    for (plugin, scope, device, control, address) in addresses {
        plugins.set_action_address(&plugin, &scope, &device, &control, address);
    }
    for (plugin, scope, device, control, _, _, action, appearance) in &title_updates {
        if let Err(error) = plugins.configure_profile_appearance(
            plugin,
            action,
            scope,
            device,
            control,
            appearance.clone(),
        ) {
            runtime.emit("plugin-error", Some(device.clone()), None, Some(error));
        }
    }
    plugins.synchronize(actions, devices);
    for (plugin, scope, device, control, title, parameters, _, appearance) in title_updates {
        if appearance.is_some() {
            continue;
        }
        let result = match parameters {
            Some(parameters) => plugins
                .update_title_parameters(&plugin, &scope, &device, &control, &title, parameters),
            None => plugins.hide_title_parameters(&plugin, &scope, &device, &control, &title),
        };
        if let Err(error) = result {
            runtime.emit("plugin-error", Some(device), None, Some(error));
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn configured_actions_follow_saved_enabled_bindings_not_appearance() {
        let owner = stream_dock_manager_plugin_host::CONFIGURED_ACTIONS_OWNER;
        let counter = format!("{owner}.dice-roll-counter");
        let total = format!("{owner}.dice-total-counter");
        let binding = |owner: &str, action: &str, disabled| {
            serde_json::json!({
                "title": "", "color": "", "disabled": disabled,
                "action": {"kind": "plugin", "plugin_id": owner, "action_id": action, "settings": {}}
            })
        };
        let mut profile: crate::profiles::Profile = serde_json::from_value(serde_json::json!({
            "id": "profile", "name": "Profile", "model_id": "n4-pro", "active_page": "first",
            "pages": [
                {"id": "first", "name": "First", "bindings": {
                    "duplicate": binding(owner, &counter, false),
                    "disabled": binding(owner, &total, true),
                    "foreign": binding("other.owner", "other.owner.test", false)
                }},
                {"id": "second", "name": "Second", "bindings": {
                    "counter": binding(owner, &counter, false)
                }}
            ]
        }))
        .unwrap();
        let inventory = |profile: &crate::profiles::Profile| {
            super::configured_plugin_actions(std::slice::from_ref(profile), &[])
        };
        assert_eq!(inventory(&profile), [counter.clone()].into());
        profile.active_page = "second".into();
        assert_eq!(inventory(&profile), [counter.clone()].into());
        profile.pages[0].bindings.remove("duplicate");
        assert_eq!(inventory(&profile), [counter].into());
        profile.pages[1].bindings.remove("counter");
        assert!(inventory(&profile).is_empty());
    }
    #[test]
    // Verify that permission gate blocks discovery without spinning or marking ready
    fn permission_gate_blocks_discovery_without_spinning_or_marking_ready() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        runtime.device_admission.update(false, false);
        let mut coordinator = Coordinator::new();
        let previous_scan = coordinator.discovery.scan;
        coordinator.discover(&runtime);
        assert_eq!(coordinator.discovery.scan, previous_scan);
        assert!(coordinator.connections.is_empty());
        assert_eq!(coordinator.timeout(&runtime), Duration::from_secs(1));
        assert!(runtime.discovery_readiness.wait(Duration::ZERO).is_err());
        runtime.shutdown();
    }
    #[test]
    // Verify that macOS HID permission error is actionable and preserves native cause
    fn macos_hid_permission_error_is_actionable_and_preserves_native_cause() {
        let cause = "hidapi error: hid_open_path: failed to open IOHIDDevice from mach entry: (0xE00002E2) (iokit/common) not permitted";
        let error = super::hid_open_error(cause.into(), "macos").to_string();
        assert!(error.contains("Input Monitoring"));
        assert!(error.contains("fully quit"));
        assert!(error.contains(cause));
    }

    #[test]
    // Verify that unrelated HID errors are not relabelled as macOS permissions
    fn unrelated_hid_errors_are_not_relabelled_as_macos_permissions() {
        for (platform, cause) in [
            (
                "linux",
                "hid_open_path: failed to open IOHIDDevice (0xE00002E2)",
            ),
            (
                "macos",
                "hid_open_path: failed to open IOHIDDevice (0xE00002C5) exclusive access",
            ),
            (
                "macos",
                "hid_open_path: device mach entry not found with the given path",
            ),
        ] {
            let error = super::hid_open_error(cause.into(), platform).to_string();
            assert_eq!(error, format!("transport: {cause}"));
        }
    }

    use super::*;
    #[test]
    // Verify that vendor title parameters follow authored renderer values only
    fn vendor_title_parameters_follow_authored_renderer_values_only() {
        let appearance = hardware_core::appearance::Appearance {
            size: 20,
            position: "center".into(),
            foreground: "#123456".into(),
            show_title: false,
            ..Default::default()
        };
        let value = authored_title_parameters(Some(&appearance), Some(100)).unwrap();
        assert_eq!(value["fontSize"], 20.0);
        assert_eq!(value["titleAlignment"], "middle");
        assert_eq!(value["titleColor"], "#123456");
        assert_eq!(value["showTitle"], false);
        assert_eq!(value["fontStyle"], "Regular");
        assert!(!value["fontFamily"].as_str().unwrap().is_empty());
        assert!(authored_title_parameters(None, Some(100)).is_none());
        assert!(authored_title_parameters(Some(&appearance), None).is_none());
    }
    #[test]
    // Verify that shared input idle releases output and drains edges before reporting disconnect
    fn shared_input_idle_releases_output_and_drains_edges_before_reporting_disconnect() {
        use std::collections::VecDeque;
        struct Input {
            reports: std::cell::RefCell<VecDeque<std::result::Result<Vec<u8>, &'static str>>>,
            output_writes: usize,
        }
        let hid = Mutex::new(Input {
            reports: std::cell::RefCell::new(VecDeque::from([
                Ok(vec![]),
                Ok(vec![1, 1]),
                Ok(vec![1, 0]),
                Err("disconnected"),
            ])),
            output_writes: 0,
        });
        let mut received = Vec::new();
        for _ in 0..4 {
            let mut report = [0; 8];
            let result = read_hid_input(
                &hid,
                true,
                false,
                |input, timeout| {
                    assert_eq!(timeout, 0, "Never wait with the shared output lock");
                    let bytes = input.reports.borrow_mut().pop_front().unwrap()?;
                    report[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                },
                |delay| {
                    assert_eq!(delay, Duration::from_millis(5));
                    // An output writer must be able to run during the idle wait
                    hid.try_lock()
                        .expect("Input idle blocked output")
                        .output_writes += 1;
                },
            );
            received.push(result.map(|size| report[..size].to_vec()));
        }
        assert_eq!(
            received,
            [
                Ok(vec![]),
                Ok(vec![1, 1]),
                Ok(vec![1, 0]),
                Err("disconnected")
            ]
        );
        assert_eq!(hid.lock().unwrap().output_writes, 1);
    }

    #[test]
    // Verify that separate input handle keeps driver wait without extra poll delay
    fn separate_input_handle_keeps_driver_wait_without_extra_poll_delay() {
        let hid = Mutex::new(());
        let result: std::result::Result<usize, ()> = read_hid_input(
            &hid,
            false,
            false,
            |_, timeout| {
                assert_eq!(timeout, 5);
                Ok(0)
            },
            |_| panic!("Separate input handle already waited in the driver"),
        );
        assert_eq!(result, Ok(0));
    }

    #[test]
    // Native readiness never blocks under either handle lock or waits after a report
    fn readiness_parks_outside_shared_and_separate_handles() {
        for shared in [false, true] {
            let hid = Mutex::new(());
            let mut waits = 0;
            for size in [0, 3] {
                let result: std::result::Result<usize, ()> = read_hid_input(
                    &hid,
                    shared,
                    true,
                    |_, timeout| {
                        assert_eq!(timeout, 0);
                        Ok(size)
                    },
                    |_| {
                        assert!(hid.try_lock().is_ok());
                        waits += 1;
                    },
                );
                assert_eq!(result, Ok(size));
            }
            assert_eq!(waits, 1);
        }
    }

    #[test]
    // A terminal backend readiness error reaches connection retirement without parking again
    fn failed_readiness_backend_does_not_reenter_wait() {
        let hid = Mutex::new(());
        let wake = input_readiness::Signal::default();
        let observed = wake.generation();
        wake.notify();
        wake.wait(observed);
        let result = read_hid_input(
            &hid,
            false,
            true,
            |_, timeout| {
                assert_eq!(timeout, 0);
                Err("readiness observer failed")
            },
            |_| panic!("Terminal backend failure must not park the runtime"),
        );
        assert_eq!(result, Err("readiness observer failed"));
    }

    #[test]
    // Verify that plugin surface metadata uses routed output dimensions
    fn plugin_surface_metadata_uses_routed_output_dimensions() {
        let mut routed = 0;
        for module in hardware_registry::modules() {
            let manifest = module.manifest();
            for control in &manifest.controls {
                let surface = plugin_surface(manifest, control);
                assert_eq!(surface["control_id"], control.id);
                assert_eq!(surface["kind"], control.kind);
                let output = if let Some(id) = &control.feedback_surface {
                    routed += 1;
                    manifest.control(id).unwrap()
                } else {
                    control
                };
                assert_eq!(surface["feedback_target"], output.id);
                assert_eq!(surface["width"], serde_json::json!(output.image_width));
                assert_eq!(surface["height"], serde_json::json!(output.image_height));
            }
            if manifest.model_id == "n4-pro" {
                let dial = plugin_surface(manifest, manifest.control("dial-4").unwrap());
                let touch = plugin_surface(manifest, manifest.control("touch-4").unwrap());
                assert_eq!(dial["feedback_target"], "touch-4");
                assert_eq!(dial["width"], 176);
                assert_eq!(dial["height"], 112);
                assert_eq!(dial["width"], touch["width"]);
                assert_eq!(dial["height"], touch["height"]);
            }
        }
        assert!(routed > 0);
    }

    #[test]
    // Verify that animation recovers only after three continuously healthy seconds
    fn animation_recovers_only_after_three_continuously_healthy_seconds() {
        let now = Instant::now();
        let mut rate = AnimationRate::default();
        assert_eq!(rate.rate(120.0, 5.0, now), 120.0);
        for index in 0..10 {
            rate.observe(3, 151.0, now + Duration::from_millis(index * 250));
        }
        assert_eq!(rate.divisor, 1024);
        assert!(rate.rate(120.0, 5.0, now + Duration::from_secs(3)) < 5.0);
        let healthy = now + Duration::from_secs(4);
        rate.observe(1, 75.0, healthy);
        assert_eq!(rate.rate(120.0, 5.0, healthy), 5.0);
        rate.observe(1, 75.0, healthy + Duration::from_millis(2999));
        assert_eq!(rate.divisor, 1024);
        rate.observe(1, 75.0, healthy + Duration::from_secs(3));
        assert_eq!(rate.divisor, 512);
        for index in 2..=10 {
            rate.observe(1, 75.0, healthy + Duration::from_secs(index * 3));
        }
        assert_eq!(rate.divisor, 1);

        // The intermediate band neither increases backoff nor counts towards
        // recovery: both queue depth and latency must stay healthy for 3 s
        for (depth, latency) in [(2, 75.0), (1, 75.1)] {
            let mut rate = AnimationRate::default();
            rate.observe(3, 0.0, now);
            rate.observe(0, 0.0, now + Duration::from_secs(1));
            rate.observe(depth, latency, now + Duration::from_secs(3));
            assert_eq!(rate.divisor, 2);
            rate.observe(0, 0.0, now + Duration::from_secs(4));
            rate.observe(0, 0.0, now + Duration::from_millis(6999));
            assert_eq!(rate.divisor, 2);
            rate.observe(0, 0.0, now + Duration::from_secs(7));
            assert_eq!(rate.divisor, 1);
        }
    }
    // Finalize transition
    pub(super) fn finish_transition(runtime: &Runtime, c: &mut Connection) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while c.transition.is_some() || c.restore_retry.is_some() {
            assert!(
                Instant::now() < deadline,
                "restore timed out: retry={:?}, transition={}, sleeping={}, playbacks={}",
                c.restore_retry,
                c.transition.is_some(),
                c.sleeping,
                c.playbacks.len()
            );
            if let Some(retry) = c.restore_retry {
                // Drive the actor's recorded deadline rather than assuming admission succeeded
                retry_restore(runtime, c, retry);
            }
            if let Some(transition) = &c.transition {
                assert!(!transition.failed, "transition failed");
                transitions::advance(runtime, c);
            }
            std::thread::yield_now();
        }
    }
    struct Capture(Arc<Mutex<Vec<Vec<u8>>>>);
    impl Transport for Capture {
        // Write the supplied devices data
        fn write(&mut self, report: &[u8]) -> Result<()> {
            self.0.lock().unwrap().push(report.to_vec());
            Ok(())
        }
    }
    #[test]
    // Verify that animation deadline excludes idle time but preserves active lateness
    fn animation_deadline_excludes_idle_time_but_preserves_active_lateness() {
        let module = hardware_registry::modules().remove(0);
        let mut connection = Connection {
            runtime: Weak::new(),
            _mailbox_registration: None,
            sleeping: false,
            navigation_fallback: Default::default(),
            restore_retry: None,
            boot_logo_restore: None,
            transition: None,
            device_id: "simulated".into(),
            worker: OutputWorker::spawn_with_policy(
                Box::new(Capture(Arc::new(Mutex::new(vec![])))),
                20,
                hardware_core::OutputPolicy::default(),
            )
            .unwrap(),
            session: Arc::new(Mutex::new(module.session())),
            input_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            input_wake: Arc::new(input_readiness::Signal::default()),
            benchmark_input: Arc::new(Mutex::new(false)),
            input_thread: None,
            epoch: 1,
            heartbeat: Instant::now(),
            manifest: module.manifest().clone(),
            contract_identity: module.representative_contract().unwrap().identity(),
            playbacks: std::collections::BTreeMap::new(),
            playback_cursor: 0,
            next_frame: Instant::now() - Duration::from_secs(600),
            animation_rate: AnimationRate::default(),
            last_queue_ms: 0.0,
            policy: OutputPolicy::default(),
            encoding: FrameEncoding::IndividualUpdates,
            scene_revision: 1,
            draft_preview: None,
            draft_sample: Instant::now(),
            static_palette: StaticPaletteGate::default(),
            content_revision: 0,
            delivery: HashMap::new(),
            benchmark: None,
            lost_outcomes: 0,
            force_individual_updates: false,
            applied_vibration: None,
            boot_logo: None,
            keyboard: None,
            blackout: Default::default(),
        };
        let playback = || Playback {
            vendor_started: None,
            asset: "fixture".into(),
            clip: None,
            _assets: crate::asset_leases::AssetLeases::default()
                .pin([], "Playback fixture")
                .unwrap(),
            control: Some("key-1".into()),
            format: "png".into(),
            last_frame: None,
            next_sample: Duration::ZERO,
            pending_frame: None,
            media: Default::default(),
        };
        let started = Instant::now();
        connection.start_playback("key-1".into(), playback());
        assert!(connection.next_frame >= started);
        assert!(connection.next_frame <= Instant::now());

        // Adding or replacing active surfaces must not conceal a real missed
        // deadline, including while another surface remains after removal
        let overdue = Instant::now() - Duration::from_secs(2);
        connection.next_frame = overdue;
        connection.start_playback("key-1".into(), playback());
        connection.start_playback("key-2".into(), playback());
        connection.playbacks.remove("key-1");
        connection.start_playback("key-1".into(), playback());
        assert_eq!(connection.next_frame, overdue);

        connection.playbacks.clear();
        let resumed = Instant::now();
        connection.start_playback("key-1".into(), playback());
        assert!(connection.next_frame >= resumed);
        assert!(connection.next_frame <= Instant::now());
    }
    #[test]
    #[ignore = "Requires local loopback sockets for the plugin host; uses a simulated transport and never opens HID"]
    // Verify that animated surfaces run without an editor and stop on scene change
    fn animated_surfaces_run_without_an_editor_and_stop_on_scene_change() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        let source = root.path().join("colors.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let asset = runtime.import_asset(&source).unwrap();
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().asset = Some(asset.clone());
        profile.background_asset = Some(asset);
        runtime.save_profile(profile, snapshot.revision).unwrap();
        let modules = hardware_registry::modules();
        let module = &modules[0];
        let mut manifest = module.manifest().clone();
        // A reduced synthetic surface set keeps transport assertions small
        manifest.controls.retain(|c| c.id == "key-1");
        manifest.capabilities.background_size = Some([32, 32]);
        let reports = Arc::new(Mutex::new(vec![]));
        let mut connection = Connection {
            runtime: Arc::downgrade(&runtime),
            _mailbox_registration: None,
            sleeping: false,
            navigation_fallback: Default::default(),
            restore_retry: None,
            boot_logo_restore: None,
            transition: None,
            device_id: "loopback".into(),
            worker: OutputWorker::spawn_with_policy(
                Box::new(Capture(reports.clone())),
                20,
                hardware_core::OutputPolicy::default(),
            )
            .unwrap(),
            session: Arc::new(Mutex::new(module.session())),
            input_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            input_wake: Arc::new(input_readiness::Signal::default()),
            benchmark_input: Arc::new(Mutex::new(false)),
            input_thread: None,
            epoch: 1,
            heartbeat: Instant::now(),
            manifest,
            contract_identity: module.representative_contract().unwrap().identity(),
            playbacks: std::collections::BTreeMap::new(),
            playback_cursor: 0,
            next_frame: Instant::now() - Duration::from_secs(600),
            animation_rate: AnimationRate::default(),
            last_queue_ms: 0.0,
            policy: OutputPolicy::default(),
            encoding: FrameEncoding::IndividualUpdates,
            scene_revision: 1,
            draft_preview: None,
            draft_sample: Instant::now(),
            static_palette: StaticPaletteGate::default(),
            content_revision: 0,
            delivery: HashMap::new(),
            benchmark: None,
            lost_outcomes: 0,
            force_individual_updates: false,
            applied_vibration: None,
            boot_logo: None,
            keyboard: None,
            blackout: Default::default(),
        };
        runtime.state.lock().unwrap().devices.push(DeviceInfo {
            id: "loopback".into(),
            persistence_key: "loopback".into(),
            identity_port_dependent: false,
            model_id: module.manifest().model_id.clone(),
            name: "Captured fixture".into(),
            variant_id: module.manifest().variants[0].id.clone(),
            firmware: String::new(),
            usb_release: None,
            connected: true,
            simulated: false,
            manifest: module.manifest().clone(),
        });
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert("loopback".into(), connection.epoch);
        runtime.state.lock().unwrap().sessions.insert(
            "loopback".into(),
            crate::workspace::DeviceSession {
                output: crate::workspace::DeviceOutput {
                    keyboard: None,
                    sleeping: false,
                    brightness: 42,
                    lighting_brightness: 70,
                    lighting_colors: vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 0, 0]],
                    vibration_enabled: None,
                    device_configuration: BTreeMap::new(),
                    frame_policy: None,
                },
                ..Default::default()
            },
        );
        // Busy workspace admission must not clear playback/transition/output state or block
        // snapshots. The actor's retry path must still deliver the requested restore.
        let gate = runtime.profiles.transaction.lock().unwrap();
        let scene_revision = connection.scene_revision;
        connection.sleeping = true;
        restore(&runtime, &mut connection);
        assert!(connection.restore_retry.is_some());
        assert_eq!(connection.scene_revision, scene_revision);
        assert!(connection.sleeping);
        assert!(connection.transition.is_none());
        assert!(runtime.state.try_lock().is_ok());
        drop(gate);
        let restored_at = Instant::now();
        let retry_at = connection.restore_retry.unwrap();
        retry_restore(&runtime, &mut connection, retry_at);
        assert!(connection.restore_retry.is_none());
        finish_transition(&runtime, &mut connection);
        assert_eq!(connection.playbacks.len(), 2);
        assert!(connection.next_frame >= restored_at);
        let preview = runtime.preview_surfaces(None).unwrap();
        assert!(preview.animated);
        assert!(preview.background.is_some());
        for _ in 0..80 {
            animate(&runtime, "test-device", &mut connection);
            std::thread::sleep(Duration::from_millis(10));
        }
        let captured = reports.lock().unwrap();
        assert!(captured
            .iter()
            .any(|report| report.get(6..12) == Some(b"LIG\0\0\x2a")));
        assert!(captured
            .iter()
            .any(|report| report.get(6..12) == Some(&[b'L', b'B', b'L', b'I', b'G', 70])));
        assert!(captured
            .iter()
            .any(|report| report.get(6..11) == Some(b"SETLB")
                && report.get(11..23) == Some(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0])));
        assert!(
            captured
                .iter()
                .filter(|r| r.get(6..9) == Some(b"BAT"))
                .count()
                >= 2
        );
        assert!(
            captured
                .iter()
                .filter(|r| r.get(6..11) == Some(b"BGPIC"))
                .count()
                >= 2
        );
        let colors: std::collections::HashSet<_> = captured
            .iter()
            .filter_map(|r| image::load_from_memory(&r[1..]).ok())
            .map(|im| im.to_rgb8().get_pixel(0, 0).0)
            .collect();
        assert!(colors.iter().any(|rgb| rgb[0] > 200));
        assert!(colors.iter().any(|rgb| rgb[2] > 200));
        drop(captured);
        // Scene restoration must preserve an explicit sleep request and rebuild
        // current playback after wake without losing brightness or lighting
        runtime
            .state
            .lock()
            .unwrap()
            .sessions
            .get_mut("loopback")
            .unwrap()
            .output
            .sleeping = true;
        restore(&runtime, &mut connection);
        finish_transition(&runtime, &mut connection);
        assert!(connection.sleeping);
        assert!(connection.playbacks.is_empty());
        runtime
            .state
            .lock()
            .unwrap()
            .sessions
            .get_mut("loopback")
            .unwrap()
            .output
            .sleeping = false;
        restore(&runtime, &mut connection);
        finish_transition(&runtime, &mut connection);
        assert!(!connection.sleeping);
        assert_eq!(connection.playbacks.len(), 2);
        assert_eq!(runtime.snapshot().session("loopback").output.brightness, 42);
        // Use the editor's clear transaction, then undo it: both playback and
        // rendered preview content must follow the binding rather than the asset file
        let snapshot = runtime.snapshot();
        let profile = &snapshot.profiles[0];
        let cleared = runtime
            .apply_edits(
                snapshot.configuration_revision,
                "Clear Key 1",
                vec![crate::workspace::Edit::Bindings {
                    profile_id: profile.id.clone(),
                    page_id: profile.pages[0].id.clone(),
                    bindings: std::collections::BTreeMap::from([("key-1".into(), None)]),
                }],
            )
            .unwrap();
        restore(&runtime, &mut connection);
        finish_transition(&runtime, &mut connection);
        assert!(!connection.playbacks.contains_key("key-1"));
        assert!(connection.playbacks.contains_key("@background"));
        let cleared_preview = runtime.preview_surfaces(None).unwrap();
        assert_ne!(cleared_preview.surfaces["key-1"], preview.surfaces["key-1"]);
        let restored = runtime
            .history_action("undo", cleared.configuration_revision)
            .unwrap();
        restore(&runtime, &mut connection);
        finish_transition(&runtime, &mut connection);
        assert_eq!(connection.playbacks.len(), 2);
        assert!(restored.profiles[0].pages[0].bindings["key-1"]
            .asset
            .is_some());
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.background_asset = None;
        profile.pages[0].bindings.remove("key-1");
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        restore(&runtime, &mut connection);
        finish_transition(&runtime, &mut connection);
        assert!(connection.playbacks.is_empty());
        std::thread::sleep(Duration::from_millis(250));
        let before = reports.lock().unwrap().len();
        for _ in 0..10 {
            animate(&runtime, "test-device", &mut connection);
        }
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(reports.lock().unwrap().len(), before);
        drop(connection);
        runtime.shutdown();
    }
}

#[cfg(test)]
mod demand_tests {
    use super::*;
    use crate::{
        actions::Action,
        builtins::{BuiltinAction, SavedMacro},
        profiles::Page,
    };
    use serde_json::json;
    #[test]
    // Verify that active sessions all pages macros overrides controls and devices determine demand
    fn active_sessions_all_pages_macros_overrides_controls_and_devices_determine_demand() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/fixture.demand");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("runtime.html"), "browser").unwrap();
        std::fs::write(package.join("manifest.json"), json!({"UUID":"fixture.demand","Name":"Demand","Version":"1.0.0","CodePath":"runtime.html","Actions":[{"UUID":"fixture.demand.action","Name":"Demand","Controllers":["Keypad"]}]}).to_string()).unwrap();
        crate::test_plugin::modernize(&package);
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime.simulate_device(true, "simulator:demand").unwrap();
        let mut snapshot = runtime.snapshot();
        let plugin = Action::Plugin {
            user_desired_state: None,
            plugin_id: "fixture.demand".into(),
            action_id: "fixture.demand.action".into(),
            settings: json!({}),
            resources: Default::default(),
        };
        let mut binding = snapshot.profiles[0].pages[0].bindings["key-1"].clone();
        binding.action = plugin.clone();
        for page in &mut snapshot.profiles[0].pages {
            page.bindings.clear();
        }
        snapshot.profiles[0].pages.push(Page {
            id: "offscreen".into(),
            name: "Offscreen".into(),
            bindings: std::collections::BTreeMap::from([("key-1".into(), binding.clone())]),
        });
        assert_eq!(
            plugin_demand(&snapshot)["fixture.demand"].len(),
            1,
            "offscreen page creates demand"
        );
        snapshot.active_profile = "editor-only-focus".into();
        assert_eq!(
            plugin_demand(&snapshot)["fixture.demand"].len(),
            1,
            "editor focus does not select a runtime profile"
        );
        let baseline = snapshot.clone();
        let mut override_binding = binding.clone();
        override_binding.action = Action::None;
        snapshot.binding_overrides.insert(
            "simulator:demand".into(),
            std::collections::BTreeMap::from([(
                "key-1".into(),
                crate::builtins::EffectiveBinding {
                    profile_id: "temporary-layer".into(),
                    page_id: "layer-page".into(),
                    scope: "temporary-layer:layer-page".into(),
                    binding: override_binding,
                },
            )]),
        );
        assert!(
            plugin_demand(&snapshot).is_empty(),
            "effective overrides suppress covered profile bindings"
        );
        snapshot
            .binding_overrides
            .get_mut("simulator:demand")
            .unwrap()
            .get_mut("key-1")
            .unwrap()
            .binding
            .action = plugin.clone();
        assert_eq!(plugin_demand(&snapshot)["fixture.demand"].len(), 1);
        snapshot = baseline.clone();
        let mut inactive = snapshot.profiles[0].clone();
        inactive.id = "inactive".into();
        snapshot.profiles[0].pages[1].bindings.clear();
        snapshot.profiles.push(inactive);
        assert!(
            plugin_demand(&snapshot).is_empty(),
            "inactive profiles never create demand"
        );
        snapshot = baseline.clone();
        snapshot.profiles[0].pages[1]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .disabled = true;
        assert!(plugin_demand(&snapshot).is_empty());
        snapshot = baseline.clone();
        let mut invalid = binding.clone();
        invalid.action = Action::None;
        snapshot.profiles[0].pages[1]
            .bindings
            .insert("key-1".into(), invalid.clone());
        snapshot.saved_macros.push(SavedMacro {
            inputs: Default::default(),
            id: "saved".into(),
            name: "Saved".into(),
            action: plugin.clone(),
        });
        assert!(
            plugin_demand(&snapshot).is_empty(),
            "unreferenced saved macro does not create demand"
        );
        invalid.action = Action::Macro {
            actions: vec![Action::Toggle {
                on: Box::new(Action::None),
                off: Box::new(Action::Builtin {
                    action: Box::new(BuiltinAction::SavedMacro {
                        arguments: Default::default(),
                        macro_id: "saved".into(),
                    }),
                }),
            }],
        };
        snapshot.profiles[0].pages[1]
            .bindings
            .insert("key-1".into(), invalid);
        assert_eq!(plugin_demand(&snapshot)["fixture.demand"].len(), 1);
        snapshot = baseline.clone();
        let mut device = snapshot.devices[0].clone();
        device.id = "second".into();
        device.persistence_key = "second".into();
        let session = snapshot.session("simulator:demand");
        snapshot.sessions.insert("second".into(), session);
        snapshot.devices.push(device);
        assert_eq!(plugin_demand(&snapshot)["fixture.demand"].len(), 2);
        snapshot.devices[0].connected = false;
        assert_eq!(plugin_demand(&snapshot)["fixture.demand"].len(), 1);
        snapshot.devices[1].connected = false;
        assert!(plugin_demand(&snapshot).is_empty());
        snapshot = baseline;
        snapshot.profiles[0].model_id = "wrong-model".into();
        assert!(plugin_demand(&snapshot).is_empty());
        runtime.shutdown();
    }
    #[test]
    // Verify that visibility and monitoring events never change profile or render revisions
    fn visibility_and_monitoring_events_never_change_profile_or_render_revisions() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let before = runtime.snapshot();
        let events = runtime.subscribe();
        let sequence = runtime
            .event_sequence
            .load(std::sync::atomic::Ordering::Relaxed);
        runtime.plugin_monitor_visible(true);
        runtime.emit_plugin_runtime();
        assert_eq!(
            runtime
                .event_sequence
                .load(std::sync::atomic::Ordering::Relaxed),
            sequence
        );
        let event = events
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(event.kind, "plugin-runtime");
        assert_eq!(event.revision, before.revision);
        runtime.emit("plugin-health", None, None, None);
        let health = events
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(health.kind, "plugin-health");
        assert_eq!(health.sequence, sequence + 1);
        assert_eq!(health.revision, before.revision);
        assert!(health.timestamp_ms.is_some_and(|timestamp| timestamp > 0));
        let after = runtime.snapshot();
        assert_eq!(before.revision, after.revision);
        assert_eq!(before.configuration_revision, after.configuration_revision);
        assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
        runtime.shutdown();
    }
}

#[cfg(test)]
mod frame_policy_tests {
    include!("n1_output_tests.rs");
    use super::*;
    #[test]
    // Keep no-profile keyboard output functional without manufacturing a persisted profile
    fn keyboard_navigation_without_profile_renders_and_resolves_keys() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) = connection_for_model(&runtime, "k1-pro");
        runtime.state.lock().unwrap().profiles.clear();
        let snapshot = runtime.snapshot();
        assert!(snapshot.session(&c.device_id).profile_id.is_empty());
        assert_eq!(
            snapshot.session(&c.device_id).page_id,
            crate::keyboard_navigation::PAGE_ID
        );
        let scene = runtime.render_scene(&c.device_id).unwrap();
        assert!(scene.keyboard_navigation);
        assert_eq!(scene.page.as_ref().unwrap().bindings.len(), 6);
        assert!(
            runtime
                .render_scene_for_target(&c.device_id, true)
                .unwrap()
                .keyboard_navigation
        );
        restore(&runtime, &mut c);
        super::tests::finish_transition(&runtime, &mut c);
        assert!(!reports.lock().unwrap().is_empty());
        let input = crate::keyboard_navigation::fallback_binding(
            &runtime.snapshot(),
            &c.device_id,
            "key-1",
        )
        .unwrap();
        assert!(
            matches!(input.binding.action, crate::actions::Action::Hotkey { keys } if keys == vec!["insert"])
        );
        assert!(crate::keyboard_navigation::fallback_binding(
            &runtime.snapshot(),
            &c.device_id,
            "unknown"
        )
        .is_none());
        assert!(runtime.snapshot().profiles.is_empty());
        drop(c);
        runtime.shutdown();
    }

    #[test]
    // Keep populated pages, custom media, other models and stale navigation decisions unchanged
    fn keyboard_navigation_blank_classification_and_stale_guard() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (c, _) = connection_for_model(&runtime, "k1-pro");
        {
            let mut state = runtime.state.lock().unwrap();
            let profile = &mut state.profiles[0];
            profile.model_id = "k1-pro".into();
            profile.pages[0].bindings.clear();
            crate::keyboard_navigation::normalize_profile(profile).unwrap();
        }
        let mut snapshot = runtime.snapshot();
        assert!(crate::keyboard_navigation::blank_target(&snapshot, &c.device_id).is_some());
        snapshot.profiles[0].background_asset = Some("custom.png".into());
        assert!(crate::keyboard_navigation::blank_target(&snapshot, &c.device_id).is_none());
        snapshot.profiles[0].background_asset = None;
        let binding = snapshot.profiles[0].pages[0].bindings["key-1"].clone();
        snapshot.profiles[0].pages[1]
            .bindings
            .insert("key-1".into(), binding);
        assert!(crate::keyboard_navigation::blank_target(&snapshot, &c.device_id).is_none());
        snapshot.profiles[0].pages[1].bindings.clear();
        snapshot.devices[0].model_id = "n4-pro".into();
        assert!(crate::keyboard_navigation::blank_target(&snapshot, &c.device_id).is_none());
        snapshot.profiles.clear();
        assert!(crate::keyboard_navigation::fallback_page(&snapshot, &c.device_id).is_none());
        let before = runtime.snapshot();
        runtime
            .select_device_profile_checked(
                &c.device_id,
                &before.profiles[0].id,
                Some(crate::keyboard_navigation::PAGE_ID),
                false,
                Some(c.epoch),
                None,
                None,
                Some((before.revision + 1, &before.profiles[0].id, "main")),
                None,
            )
            .unwrap();
        assert_eq!(runtime.snapshot().session(&c.device_id).page_id, "main");
        assert!(runtime
            .select_device_profile_checked(
                &c.device_id,
                &before.profiles[0].id,
                Some(crate::keyboard_navigation::PAGE_ID),
                false,
                Some(c.epoch + 1),
                None,
                None,
                Some((before.revision, &before.profiles[0].id, "main")),
                None
            )
            .is_err());
        assert_eq!(runtime.snapshot().session(&c.device_id).page_id, "main");
        drop(c);
        runtime.shutdown();
    }

    #[test]
    // Switch only a continuously blank keyboard page and preserve customized Navigation content
    fn keyboard_navigation_blank_page_deadline_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, _) = connection_for_model(&runtime, "k1-pro");
        {
            let mut state = runtime.state.lock().unwrap();
            let profile = &mut state.profiles[0];
            profile.model_id = "k1-pro".into();
            profile.pages[0].bindings.clear();
            crate::keyboard_navigation::normalize_profile(profile).unwrap();
            profile.pages[0].bindings.get_mut("key-1").unwrap().title = "My insert".into();
        }
        let now = Instant::now();
        refresh_navigation_fallback(&runtime, &mut c, now);
        assert_eq!(
            c.navigation_fallback.deadline(),
            Some(now + Duration::from_secs(30))
        );
        refresh_navigation_fallback(&runtime, &mut c, now + Duration::from_millis(29999));
        assert_eq!(runtime.snapshot().session(&c.device_id).page_id, "main");
        c.sleeping = true;
        refresh_navigation_fallback(&runtime, &mut c, now + Duration::from_secs(30));
        assert!(c.navigation_fallback.deadline().is_none());
        c.sleeping = false;
        refresh_navigation_fallback(&runtime, &mut c, now + Duration::from_secs(31));
        refresh_navigation_fallback(&runtime, &mut c, now + Duration::from_secs(60));
        assert_eq!(runtime.snapshot().session(&c.device_id).page_id, "main");
        refresh_navigation_fallback(&runtime, &mut c, now + Duration::from_secs(61));
        let snapshot = runtime.snapshot();
        assert_eq!(
            snapshot.session(&c.device_id).page_id,
            crate::keyboard_navigation::PAGE_ID
        );
        assert_eq!(
            snapshot.profiles[0].pages[0].bindings["key-1"].title,
            "My insert"
        );
        assert!(c.navigation_fallback.deadline().is_none());
        drop(c);
        runtime.shutdown();
    }

    #[test]
    // Verify that firmware observation change rejects already queued images before transport
    fn firmware_observation_change_rejects_already_queued_images_before_transport() {
        struct Barrier {
            captured: Arc<Mutex<Vec<Vec<u8>>>>,
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl Transport for Barrier {
            // Write the supplied devices data
            fn write(&mut self, report: &[u8]) -> Result<()> {
                self.captured.lock().unwrap().push(report.to_vec());
                if report == [42] {
                    self.entered.send(()).unwrap();
                    self.release.recv().unwrap();
                }
                Ok(())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:h1-pro:queued";
        runtime.simulate_device(true, id).unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
            device.simulated = false;
            device.variant_id = device.manifest.variants[0].id.clone();
        }
        let epoch = runtime.connection_epochs.lock().unwrap()[id];
        runtime.observe_firmware(id, epoch, Ok(Some("V25.H1 Pro.01.000".into())));
        let (mut c, reports) = connection_for_model(&runtime, "h1-pro");
        c.device_id = id.into();
        c.epoch = epoch;
        c.runtime = Arc::downgrade(&runtime);
        let (entered, observed) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        c.worker = OutputWorker::spawn_with_policy(
            Box::new(Barrier {
                captured: reports.clone(),
                entered,
                release: waiting,
            }),
            30,
            hardware_core::OutputPolicy::default(),
        )
        .unwrap();
        c.worker
            .submit(1, DeviceTransaction::single(vec![42]))
            .unwrap();
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let data =
            hardware_core::render::tile_rgba(64, 64, [255, 0, 0, 255], "", 0, "png").unwrap();
        send_surfaces(
            &mut c,
            vec![DeviceCommand::Image {
                control_id: "key-1".into(),
                data: data.clone(),
            }],
            OutputPriority::Normal,
        )
        .unwrap();
        runtime.observe_firmware(id, epoch, Ok(None));
        release.send(()).unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(*reports.lock().unwrap(), vec![vec![42]]);
        assert!(send_surfaces(
            &mut c,
            vec![DeviceCommand::Image {
                control_id: "key-1".into(),
                data
            }],
            OutputPriority::Normal
        )
        .is_err());
        runtime.shutdown();
    }
    #[test]
    // Verify that reviewed vibration is deferred asleep and deduplicated across scene updates
    fn reviewed_vibration_is_deferred_asleep_and_deduplicated_across_scene_updates() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut connection, reports) = connection(&runtime);
        let mut output = crate::workspace::DeviceOutput::default();
        apply_vibration(&runtime, &mut connection, &output).unwrap();
        assert_eq!(connection.worker.queue_depth(), 0);
        output.vibration_enabled = Some(true);
        assert!(apply_vibration(&runtime, &mut connection, &output).is_err());
        output.device_configuration = connection
            .manifest
            .capabilities
            .vibration
            .as_ref()
            .unwrap()
            .configuration_fields
            .iter()
            .enumerate()
            .map(|(i, field)| (field.id.clone(), i % 2 == 0))
            .collect();
        output.sleeping = true;
        apply_vibration(&runtime, &mut connection, &output).unwrap();
        assert!(connection.applied_vibration.is_none());
        output.sleeping = false;
        apply_vibration(&runtime, &mut connection, &output).unwrap();
        connection.invalidate_scene();
        apply_vibration(&runtime, &mut connection, &output).unwrap();
        assert!(connection.worker.wait_idle(Duration::from_secs(3)));
        assert_eq!(reports.lock().unwrap().len(), 1);
        assert_eq!(reports.lock().unwrap()[0][14], 0x11);
        output.vibration_enabled = Some(false);
        apply_vibration(&runtime, &mut connection, &output).unwrap();
        assert!(connection.worker.wait_idle(Duration::from_secs(3)));
        assert_eq!(reports.lock().unwrap().len(), 2);
        assert_eq!(reports.lock().unwrap()[1][14], 0xff);
        // A new connection has no applied state and restores the reviewed choices
        let (mut reconnected, next_reports) = self::connection(&runtime);
        apply_vibration(&runtime, &mut reconnected, &output).unwrap();
        assert!(reconnected.worker.wait_idle(Duration::from_secs(3)));
        assert_eq!(next_reports.lock().unwrap().len(), 1);
        runtime.shutdown();
    }
    #[test]
    // Verify that coordinator retirement ignores stale epochs and removes only current connection
    fn coordinator_retirement_ignores_stale_epochs_and_removes_only_current_connection() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (connection, _) = connection(&runtime);
        let id = connection.device_id.clone();
        let epoch = connection.epoch;
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(id.clone(), epoch);
        let mut coordinator = Coordinator::new();
        coordinator.connections.insert(id.clone(), connection);
        assert!(coordinator
            .dispatch(&runtime, Request::Lost(id.clone(), epoch + 1))
            .is_none());
        assert!(coordinator.connections.contains_key(&id));
        assert_eq!(runtime.connection_generation(&id), Some(epoch));
        coordinator.dispatch(&runtime, Request::Lost(id.clone(), epoch));
        assert!(!coordinator.connections.contains_key(&id));
        assert!(runtime.connection_generation(&id).is_none());
        runtime.shutdown();
    }
    #[test]
    // Verify that coordinator shutdown revokes epochs before finishing connections
    fn coordinator_shutdown_revokes_epochs_before_finishing_connections() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut connection, reports) = connection(&runtime);
        let id = connection.device_id.clone();
        *connection.benchmark_input.lock().unwrap() = true;
        connection.benchmark = Some((1, Instant::now()));
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(id.clone(), connection.epoch);
        let mut coordinator = Coordinator::new();
        coordinator.connections.insert(id.clone(), connection);
        let (reply, response) = mpsc::channel();
        let reply = coordinator
            .dispatch(&runtime, Request::Shutdown(reply))
            .unwrap();
        assert!(runtime.connection_epochs.lock().unwrap().is_empty());
        assert_eq!(coordinator.connections.len(), 1);
        assert!(response.try_recv().is_err());
        coordinator.finish();
        assert!(coordinator.connections[&id].benchmark.is_none());
        assert!(!*coordinator.connections[&id].benchmark_input.lock().unwrap());
        assert!(!reports.lock().unwrap().is_empty());
        reply.send(()).unwrap();
        assert!(response.try_recv().is_ok());
        runtime.shutdown();
    }
    struct Capture(Arc<Mutex<Vec<Vec<u8>>>>);
    impl Transport for Capture {
        // Write the supplied devices data
        fn write(&mut self, report: &[u8]) -> Result<()> {
            self.0.lock().unwrap().push(report.to_vec());
            Ok(())
        }
    }
    #[test]
    // Verify that vendor GIF uses actor delivery tracking and releases on retirement
    fn vendor_gif_uses_actor_delivery_tracking_and_releases_on_retirement() {
        use base64::Engine;
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/org.vendor.animation");
        std::fs::create_dir_all(&package).unwrap();
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            for color in [[240, 0, 0, 255], [0, 0, 240, 255]] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(4, 4, image::Rgba(color)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(10_000, 1),
                    ))
                    .unwrap();
            }
        }
        std::fs::write(package.join("state.gif"), &gif).unwrap();
        std::fs::write(
            package.join("runtime.html"),
            "<!doctype html><title>Harmless fixture</title>",
        )
        .unwrap();
        let mut manifest = serde_json::json!({"UUID":"org.vendor.animation","SDKVersion":2,"Name":"Animation","Version":"1.0.0.0","Author":"Stream Dock Manager contributors","Description":"Disposable native animation fixture","Icon":"state","Software":{"MinimumVersion":"6.9"},"OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],"CodePath":"runtime.html","Actions":[{"UUID":"org.vendor.animation.action","Name":"Animation","Icon":"state","Controllers":["Keypad"],"States":[{"Image":"state"}]}]});
        crate::test_plugin::use_portable_vendor_on_linux(&mut manifest);
        std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut c, reports) = connection(&runtime);
        {
            // Sleep/wake reloads the validated saved workspace. Persist this
            // fixture binding rather than losing an in-memory-only edit on wake
            let snapshot = runtime.snapshot();
            let mut profile = snapshot.profiles[0].clone();
            profile.model_id = "n4-pro".into();
            let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
            binding.action = crate::actions::Action::Plugin {
                user_desired_state: None,
                plugin_id: "org.vendor.animation".into(),
                action_id: "org.vendor.animation.action".into(),
                settings: serde_json::json!({}),
                resources: Default::default(),
            };
            binding.asset = None;
            runtime
                .save_profile(profile, snapshot.configuration_revision)
                .unwrap();
        }
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable("org.vendor.animation", true)
            .unwrap();
        sync_plugins(&runtime);
        runtime.plugins.lock().unwrap().poll();
        sync_plugins(&runtime);
        let first = runtime.profiles.media_gate.lock().unwrap();
        let second = runtime.profiles.media_gate.lock().unwrap();
        let blocked = runtime.render_scene("frame-fixture").unwrap();
        assert!(blocked.visuals["key-1"]["vendorAnimationError"].is_string());
        assert!(runtime
            .vendor_animations
            .lock()
            .unwrap()
            .active("frame-fixture", false)
            .is_empty());
        drop((first, second));
        std::thread::sleep(Duration::from_millis(1050));
        synchronize_vendor_playback(&runtime, "frame-fixture", &mut c);
        assert!(
            c.playbacks.contains_key("key-1"),
            "actor retries even when no animation playback was admitted"
        );
        let scene = runtime.render_scene("frame-fixture").unwrap();
        assert!(scene.visuals["key-1"]["image"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        synchronize_vendor_playback(&runtime, "frame-fixture", &mut c);
        assert!(c.playbacks["key-1"].vendor_started.is_some());
        assert!(c.playbacks["key-1"].last_frame.is_none());
        animate(&runtime, "frame-fixture", &mut c);
        assert!(c.worker.wait_idle(Duration::from_secs(2)));
        c.collect_outcomes(&runtime);
        assert_eq!(c.playbacks["key-1"].last_frame, Some(0));
        let count = reports.lock().unwrap().len();
        runtime.profiles.media_budget.trim_pressure();
        c.next_frame = Instant::now();
        c.playbacks.get_mut("key-1").unwrap().next_sample = Duration::ZERO;
        animate(&runtime, "frame-fixture", &mut c);
        assert!(c.worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(
            reports.lock().unwrap().len(),
            count,
            "unchanged frame must not enqueue twice"
        );
        let image = scene.visuals["key-1"]["image"]
            .as_str()
            .unwrap()
            .strip_prefix("data:image/png;base64,")
            .unwrap();
        let png = base64::engine::general_purpose::STANDARD
            .decode(image)
            .unwrap();
        assert!(
            image::load_from_memory(&png)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[0]
                > 200
        );
        runtime
            .state
            .lock()
            .unwrap()
            .devices
            .iter_mut()
            .find(|device| device.id == "frame-fixture")
            .unwrap()
            .simulated = true;
        let started = runtime
            .vendor_animations
            .lock()
            .unwrap()
            .active("frame-fixture", false)["key-1"]
            .started;
        runtime.set_device_sleep("frame-fixture", true).unwrap();
        restore(&runtime, &mut c);
        assert!(c.sleeping);
        assert!(c.playbacks.is_empty());
        assert!(runtime
            .vendor_animations
            .lock()
            .unwrap()
            .active("frame-fixture", false)
            .is_empty());
        runtime.set_device_sleep("frame-fixture", false).unwrap();
        restore(&runtime, &mut c);
        super::tests::finish_transition(&runtime, &mut c);
        assert!(!c.sleeping);
        // The real runtime monitor pumps startup and re-publishes visible
        // contexts after wake; this actor-only fixture must drive that work
        let deadline = Instant::now() + Duration::from_secs(3);
        let scene = loop {
            runtime.plugins.lock().unwrap().poll();
            sync_plugins(&runtime);
            let scene = runtime.render_scene("frame-fixture").unwrap();
            if runtime
                .vendor_animations
                .lock()
                .unwrap()
                .active("frame-fixture", false)
                .contains_key("key-1")
            {
                break scene;
            }
            assert!(
                Instant::now() < deadline,
                "Vendor animation did not restart after wake"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let restarted = runtime
            .vendor_animations
            .lock()
            .unwrap()
            .active("frame-fixture", false)["key-1"]
            .started;
        assert!(restarted > started);
        assert_eq!(
            scene.visuals["key-1"]["image"]
                .as_str()
                .unwrap()
                .strip_prefix("data:image/png;base64,")
                .unwrap(),
            image
        );
        runtime
            .vendor_animations
            .lock()
            .unwrap()
            .clear("frame-fixture", false);
        synchronize_vendor_playback(&runtime, "frame-fixture", &mut c);
        assert!(!c.playbacks.contains_key("key-1"));
        runtime.shutdown();
    }
    // Create an N4 Pro connection fixture that captures outgoing reports
    fn connection(runtime: &Arc<Runtime>) -> (Connection, Arc<Mutex<Vec<Vec<u8>>>>) {
        connection_for_model(runtime, "n4-pro")
    }
    // Create the selected hardware session with a report-capturing transport
    pub(super) fn connection_for_model(
        runtime: &Arc<Runtime>,
        model: &str,
    ) -> (Connection, Arc<Mutex<Vec<Vec<u8>>>>) {
        let module = hardware_registry::modules()
            .into_iter()
            .find(|m| m.manifest().model_id == model)
            .unwrap();
        let contract = if model == "h1-pro" {
            // Synthetic recognized-firmware metadata; never a physical qualification
            module
                .resolve_contract(&module.manifest().variants[0].id, Some("V3.H1.1.0"))
                .unwrap()
        } else {
            module.representative_contract().unwrap()
        };
        let device_id = "frame-fixture".to_string();
        {
            let mut state = runtime.state.lock().unwrap();
            state.devices.retain(|device| device.id != device_id);
            state.devices.push(DeviceInfo {
                id: device_id.clone(),
                persistence_key: device_id.clone(),
                identity_port_dependent: false,
                model_id: model.into(),
                name: "Captured transport fixture".into(),
                variant_id: contract.variant_id.clone(),
                firmware: contract.firmware.clone().unwrap_or_default(),
                usb_release: None,
                connected: true,
                simulated: false,
                manifest: contract.manifest.clone(),
            });
        }
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(device_id, 1);
        let reports = Arc::new(Mutex::new(Vec::new()));
        let policy = OutputPolicy {
            mode: OutputMode::FrameScheduling,
            max_batch_estimated_ms: 100,
            ..Default::default()
        };
        let c = Connection {
            runtime: Arc::downgrade(runtime),
            _mailbox_registration: None,
            device_id: "frame-fixture".into(),
            sleeping: false,
            navigation_fallback: Default::default(),
            restore_retry: None,
            boot_logo_restore: None,
            transition: None,
            worker: OutputWorker::spawn_with_policy(Box::new(Capture(reports.clone())), 20, policy)
                .unwrap(),
            session: Arc::new(Mutex::new(module.session_for(&contract).unwrap())),
            input_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            input_wake: Arc::new(input_readiness::Signal::default()),
            benchmark_input: Arc::new(Mutex::new(false)),
            input_thread: None,
            epoch: 1,
            heartbeat: Instant::now(),
            manifest: contract.manifest.clone(),
            contract_identity: contract.identity(),
            playbacks: Default::default(),
            playback_cursor: 0,
            next_frame: Instant::now(),
            animation_rate: Default::default(),
            last_queue_ms: 0.0,
            policy,
            encoding: FrameEncoding::GroupedKeyUpdates,
            scene_revision: 1,
            draft_preview: None,
            draft_sample: Instant::now(),
            static_palette: StaticPaletteGate::default(),
            content_revision: 0,
            delivery: Default::default(),
            benchmark: None,
            lost_outcomes: 0,
            force_individual_updates: false,
            applied_vibration: None,
            boot_logo: None,
            keyboard: None,
            blackout: Default::default(),
        };
        (c, reports)
    }
    #[test]
    // Verify that k1 sleep wake rebuilds GIF playback and direct wake respects saved sleep
    fn k1_sleep_wake_rebuilds_gif_playback_and_direct_wake_respects_saved_sleep() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        let source = root.path().join("colors.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let asset = runtime.import_asset(&source).unwrap();
        let (mut c, reports) = connection_for_model(&runtime, "k1-pro");
        let id = c.device_id.clone();
        let epoch = c.epoch;
        {
            let mut state = runtime.state.lock().unwrap();
            // Drive the public persistence API without opening the HID actor;
            // the captured connection below executes its restore work explicitly
            state
                .devices
                .iter_mut()
                .find(|device| device.id == id)
                .unwrap()
                .simulated = true;
            let profile = &mut state.profiles[0];
            profile.model_id = "k1-pro".into();
            profile.pages[0].bindings.get_mut("key-1").unwrap().asset = Some(asset);
            let session = crate::workspace::DeviceSession {
                profile_id: profile.id.clone(),
                page_id: profile.pages[0].id.clone(),
                ..Default::default()
            };
            state.sessions.insert(id.clone(), session);
        }
        restore(&runtime, &mut c);
        super::tests::finish_transition(&runtime, &mut c);
        assert!(c.playbacks.contains_key("key-1"));
        runtime.set_device_sleep(&id, true).unwrap();
        restore(&runtime, &mut c);
        assert!(c.sleeping);
        assert!(c.playbacks.is_empty());
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        let mut connections = HashMap::from([(id.clone(), c)]);
        assert!(
            dispatch_command(&runtime, &mut connections, &id, epoch, DeviceCommand::Wake).is_err()
        );
        assert!(connections[&id].sleeping);
        runtime.set_device_sleep(&id, false).unwrap();
        let workspace_busy = runtime.profiles.transaction.lock().unwrap();
        dispatch_command(&runtime, &mut connections, &id, epoch, DeviceCommand::Wake).unwrap();
        let c = connections.get_mut(&id).unwrap();
        let retry = c
            .restore_retry
            .expect("Wake must defer while workspace is busy");
        assert!(c.playbacks.is_empty());
        assert!(c.transition.is_none());
        assert!(runtime.state.try_lock().is_ok());
        // A visit before the deadline must not consume it; repeated due admission stays pending
        retry_restore(&runtime, c, retry - Duration::from_millis(1));
        assert_eq!(c.restore_retry, Some(retry));
        retry_restore(&runtime, c, retry);
        assert!(c.restore_retry.is_some());
        assert!(c.playbacks.is_empty());
        assert!(runtime.state.try_lock().is_ok());
        drop(workspace_busy);
        super::tests::finish_transition(&runtime, c);
        assert!(c.restore_retry.is_none());
        assert!(!c.sleeping);
        assert!(c.playbacks.contains_key("key-1"));
        assert!(!transition_pending(&runtime, c));
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        let before = reports.lock().unwrap().len();
        for _ in 0..40 {
            animate(&runtime, &id, c);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        assert!(
            reports.lock().unwrap().len() > before,
            "GIF frames must resume after Wake"
        );
        // Direct Sleep/Wake without persisted sleep also reconstructs playback
        dispatch_command(&runtime, &mut connections, &id, epoch, DeviceCommand::Sleep).unwrap();
        dispatch_command(&runtime, &mut connections, &id, epoch, DeviceCommand::Wake).unwrap();
        let c = connections.get_mut(&id).unwrap();
        super::tests::finish_transition(&runtime, c);
        assert!(c.playbacks.contains_key("key-1"));
        assert!(!transition_pending(&runtime, c));
        drop(connections);
        runtime.shutdown();
    }
    #[test]
    // Verify that temporary preview schedules only affected surfaces and restores each model
    fn temporary_preview_schedules_only_affected_surfaces_and_restores_each_model() {
        for module in hardware_registry::modules() {
            let model = module.manifest().model_id.clone();
            let Some(key) = module
                .manifest()
                .controls
                .iter()
                .find(|c| c.kind == "button" && c.image_slot.is_some())
                .map(|c| c.id.clone())
            else {
                continue;
            };
            let root = tempfile::tempdir().unwrap();
            let runtime = Runtime::build(root.path().into(), false).unwrap();
            let id = format!("simulator:{model}:preview");
            runtime.simulate_device(true, &id).unwrap();
            {
                let mut state = runtime.state.lock().unwrap();
                let device = &mut state.devices[0];
                device.simulated = false;
                device.variant_id = module.manifest().variants[0].id.clone();
                device.firmware = if model == "h1-pro" {
                    "V3.H1.1.0".into()
                } else {
                    String::new()
                };
            }
            let (mut c, _reports) = connection_for_model(&runtime, &model);
            c.device_id = id.clone();
            c.epoch = runtime.connection_generation(&id).unwrap();
            c.encoding = FrameEncoding::IndividualUpdates;
            let binding: crate::Binding=serde_json::from_value(serde_json::json!({"title":"DRAFT","color":"#663322","action":{"kind":"none"},"media":crate::media::MediaOptions::default()})).unwrap();
            for mode in [OutputMode::IndividualUpdates, OutputMode::FrameScheduling] {
                c.policy.mode = mode;
                c.worker.set_policy(c.policy).unwrap();
                c.scene_revision = c.worker.scene_revision();
                let token = runtime
                    .preview_on_dock(
                        &id,
                        runtime.snapshot().configuration_revision,
                        crate::hardware_preview::HardwareDraft {
                            display_test: None,
                            bindings: std::collections::BTreeMap::from([(
                                key.clone(),
                                binding.clone(),
                            )]),
                            background: None,
                            scope: None,
                            preview_states: Default::default(),
                            palette: None,
                        },
                    )
                    .unwrap();
                refresh_draft_preview(&runtime, &mut c);
                assert!(c.worker.wait_idle(Duration::from_secs(2)), "{model}");
                let outcomes = c.worker.drain_outcomes();
                assert_eq!(outcomes.len(), 1, "{model} {mode:?}: {outcomes:?}");
                assert_eq!(outcomes[0].surface_id, key);
                assert_eq!(outcomes[0].status, OutputStatus::Completed);
                runtime.cancel_hardware_preview(Some(&token));
                refresh_draft_preview(&runtime, &mut c);
                assert!(c.worker.wait_idle(Duration::from_secs(2)));
                let restored = c.worker.drain_outcomes();
                assert_eq!(restored.len(), 1, "{model} {mode:?}");
                assert_eq!(restored[0].surface_id, key);
                assert_eq!(restored[0].status, OutputStatus::Completed);
                assert!(c.draft_preview.is_none());
            }
            runtime.shutdown();
        }
    }
    #[test]
    // Restore current default or newly selected custom media only after the upload receipt and preview interval
    fn startup_logo_completion_restores_latest_scene() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("runtime"), false).unwrap();
        let (mut connection, reports) = connection_for_model(&runtime, "n4-pro");
        restore(&runtime, &mut connection);
        super::tests::finish_transition(&runtime, &mut connection);
        assert!(connection.worker.wait_idle(Duration::from_secs(5)));
        reports.lock().unwrap().clear();
        let now = Instant::now();
        let (send, receipt) = std::sync::mpsc::channel();
        connection.boot_logo = Some(crate::boot_logo::PendingUpload {
            receipt,
            reply: None,
            deadline: now + Duration::from_secs(30),
            completed: false,
        });
        poll_boot_logo(&mut connection, now);
        assert!(connection.boot_logo_restore.is_none());
        send.send(OutputStatus::Completed).unwrap();
        poll_boot_logo(&mut connection, now);
        let deadline = connection.boot_logo_restore.unwrap();
        assert_eq!(deadline, now + Duration::from_secs(2));
        retry_restore(
            &runtime,
            &mut connection,
            deadline - Duration::from_millis(1),
        );
        assert!(reports.lock().unwrap().is_empty());
        let workspace_busy = runtime.profiles.transaction.lock().unwrap();
        retry_restore(&runtime, &mut connection, deadline);
        assert!(connection.boot_logo_restore.is_none());
        assert!(connection.restore_retry.is_some());
        assert!(runtime.state.try_lock().is_ok());
        drop(workspace_busy);
        let retry = connection.restore_retry.unwrap();
        retry_restore(&runtime, &mut connection, retry);
        super::tests::finish_transition(&runtime, &mut connection);
        assert!(connection.worker.wait_idle(Duration::from_secs(5)));
        assert!(connection.boot_logo_restore.is_none());
        let default_reports = std::mem::take(&mut *reports.lock().unwrap());
        assert!(!default_reports.is_empty());

        // A later accepted upload replaces the deadline and restoration resolves new media
        let (send, receipt) = std::sync::mpsc::channel();
        connection.boot_logo = Some(crate::boot_logo::PendingUpload {
            receipt,
            reply: None,
            deadline: now + Duration::from_secs(30),
            completed: false,
        });
        send.send(OutputStatus::Completed).unwrap();
        poll_boot_logo(&mut connection, now + Duration::from_secs(1));
        assert_eq!(
            connection.boot_logo_restore,
            Some(now + Duration::from_secs(3))
        );
        let path = root.path().join("custom.png");
        image::RgbaImage::from_pixel(32, 32, image::Rgba([240u8, 12, 99, 255]))
            .save(&path)
            .unwrap();
        let asset = runtime.profiles.import_asset(&path).unwrap();
        let snapshot = runtime.snapshot();
        let profile_id = snapshot.session(&connection.device_id).profile_id;
        let mut profile = snapshot
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .unwrap()
            .clone();
        profile.background_asset = Some(asset);
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        retry_restore(&runtime, &mut connection, now + Duration::from_secs(2));
        assert!(reports.lock().unwrap().is_empty());
        retry_restore(&runtime, &mut connection, now + Duration::from_secs(3));
        super::tests::finish_transition(&runtime, &mut connection);
        assert!(connection.worker.wait_idle(Duration::from_secs(5)));
        let custom_reports = reports.lock().unwrap();
        assert!(!custom_reports.is_empty());
        assert_ne!(&default_reports, &*custom_reports);
        // Restoration produces ordinary output only, never another persistent LOG command
        assert!(!custom_reports
            .iter()
            .any(|report| report.windows(3).any(|part| part == b"LOG")));
        drop(custom_reports);
        runtime.shutdown();
    }
    #[test]
    // Exercise palette wallpaper delivery and restoration through encoded hardware reports
    fn palette_preview_delivers_background_to_transport() {
        for model in ["n4-pro", "k1-pro"] {
            let root = tempfile::tempdir().unwrap();
            let runtime = Runtime::build(root.path().into(), false).unwrap();
            let (mut c, reports) = connection_for_model(&runtime, model);
            c.encoding = FrameEncoding::IndividualUpdates;
            let target = if c.manifest.capabilities.background {
                "@background"
            } else {
                "key-1"
            };
            let original = runtime.render_scene(&c.device_id).unwrap();
            let mut draft = crate::hardware_preview::HardwareDraft {
                palette: Some(crate::hardware_preview::PaletteDraft {
                    mode: "light".into(),
                    id: "default".into(),
                    custom: None,
                }),
                ..Default::default()
            };
            if runtime
                .resolve_preview_palette(draft.palette.as_ref().unwrap())
                .unwrap()
                .0["wallpaper"]
                .as_str()
                == Some(original.wallpaper.as_str())
            {
                draft.palette.as_mut().unwrap().mode = "dark".into();
            }
            let token = runtime
                .preview_on_dock(
                    &c.device_id,
                    runtime.snapshot().configuration_revision,
                    draft,
                )
                .unwrap();
            let candidate = runtime.render_scene(&c.device_id).unwrap();
            refresh_draft_preview(&runtime, &mut c);
            assert!(c.worker.wait_idle(Duration::from_secs(5)), "{model}");
            let outcomes = c.worker.drain_outcomes();
            assert!(
                outcomes
                    .iter()
                    .any(|o| o.surface_id == target && o.status == OutputStatus::Completed),
                "{model}: {outcomes:?}"
            );
            assert!(!reports.lock().unwrap().is_empty(), "{model}");
            let candidate_reports = std::mem::take(&mut *reports.lock().unwrap());
            runtime.cancel_hardware_preview(Some(&token));
            refresh_draft_preview(&runtime, &mut c);
            assert!(c.worker.wait_idle(Duration::from_secs(5)), "{model}");
            let restored = c.worker.drain_outcomes();
            assert!(
                restored
                    .iter()
                    .any(|o| o.surface_id == target && o.status == OutputStatus::Completed),
                "{model}: {restored:?}"
            );
            assert_ne!(original.wallpaper, candidate.wallpaper, "{model}");
            assert!(
                candidate_reports != *reports.lock().unwrap(),
                "{model} wallpaper bytes must change, including empty per-key LCDs"
            );
            if !c.manifest.capabilities.background {
                assert!(!outcomes
                    .iter()
                    .any(|outcome| outcome.surface_id == "@background"));
            }
            runtime.shutdown();
        }
    }
    #[test]
    // One palette owner updates selected and unselected docks and restores their own saved scenes
    fn global_palette_reaches_multiple_docks_without_renewal_rewrites() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut first, first_reports) = connection_for_model(&runtime, "n4-pro");
        let previous_id = first.device_id.clone();
        first.device_id = "unselected-n4".into();
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state
                .devices
                .iter_mut()
                .find(|d| d.id == previous_id)
                .unwrap();
            device.id = first.device_id.clone();
            device.persistence_key = first.device_id.clone();
        }
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(first.device_id.clone(), first.epoch);
        let (mut second, second_reports) = connection_for_model(&runtime, "k1-pro");
        {
            let mut state = runtime.state.lock().unwrap();
            let mut profile = state.profiles[0].clone();
            profile.id = "other-active-profile".into();
            profile.model_id = "k1-pro".into();
            let mut session = state.session(&second.device_id);
            session.profile_id = profile.id.clone();
            session.page_id = profile.active_page.clone();
            state.profiles.push(profile);
            state.sessions.insert(second.device_id.clone(), session);
        }
        {
            let mut state = runtime.state.lock().unwrap();
            for profile in &mut state.profiles {
                profile.background_asset = None;
                for page in &mut profile.pages {
                    page.bindings.clear();
                }
            }
        }
        assert_eq!(runtime.snapshot().devices.len(), 2);
        assert_ne!(first.device_id, second.device_id);
        let assignments = runtime.snapshot().sessions;
        first.encoding = FrameEncoding::IndividualUpdates;
        second.encoding = FrameEncoding::IndividualUpdates;
        let mut candidate = crate::hardware_preview::PaletteDraft {
            mode: "light".into(),
            id: "default".into(),
            custom: None,
        };
        if runtime.resolve_preview_palette(&candidate).unwrap().0["wallpaper"].as_str()
            == Some(
                runtime
                    .render_scene(&first.device_id)
                    .unwrap()
                    .wallpaper
                    .as_str(),
            )
        {
            candidate.mode = "dark".into();
        }
        second.sleeping = true;
        let token = runtime.preview_plugin_palette(candidate.clone()).unwrap();
        for c in [&mut first, &mut second] {
            refresh_draft_preview(&runtime, c);
            assert!(c.worker.wait_idle(Duration::from_secs(5)));
        }
        assert!(
            second.sleeping,
            "Palette browsing must not wake the sleeping dock"
        );
        assert!(second_reports.lock().unwrap().is_empty());
        second.sleeping = false;
        second.draft_sample = Instant::now() - Duration::from_millis(100);
        refresh_draft_preview(&runtime, &mut second);
        assert!(second.worker.wait_idle(Duration::from_secs(5)));
        let first_candidate = std::mem::take(&mut *first_reports.lock().unwrap());
        let second_candidate = std::mem::take(&mut *second_reports.lock().unwrap());
        assert!(!first_candidate.is_empty() && !second_candidate.is_empty());
        // Media and local inspector drafts deliberately retain their existing timed refresh path
        let mut unknown = runtime.render_scene(&first.device_id).unwrap();
        assert!(static_palette_scene(&unknown, None));
        unknown.theme_wallpaper = Some(("synthetic-animated-asset".into(), Default::default()));
        assert!(!static_palette_scene(&unknown, None));
        unknown.theme_wallpaper = None;
        let mut custom_profile = runtime.snapshot().profiles[0].clone();
        custom_profile.background_asset = Some("custom-wallpaper".into());
        assert!(!static_palette_scene(&unknown, Some(&custom_profile)));
        unknown
            .visuals
            .insert("plugin".into(), std::sync::Arc::new(serde_json::json!({})));
        assert!(!static_palette_scene(&unknown, None));
        unknown.visuals.clear();
        unknown
            .fallbacks
            .insert("plugin".into(), serde_json::json!({}));
        assert!(!static_palette_scene(&unknown, None));
        unknown.fallbacks.clear();
        unknown.draft.as_mut().unwrap().background =
            Some(crate::hardware_preview::BackgroundDraft {
                asset: None,
                media: Default::default(),
            });
        assert!(!static_palette_scene(&unknown, None));
        // Dynamic label content keeps timed recapture even though static reuse is now the default
        let dynamic: crate::Binding = serde_json::from_value(serde_json::json!({
            "title": "{time}", "color": "#223344", "action": {"kind": "none"}
        }))
        .unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            let session = state.session(&first.device_id);
            let profile = state
                .profiles
                .iter_mut()
                .find(|p| p.id == session.profile_id)
                .unwrap();
            profile
                .pages
                .iter_mut()
                .find(|p| p.id == session.page_id)
                .unwrap()
                .bindings
                .insert("key-1".into(), dynamic);
            state.revision += 1;
        }
        let before = first.static_palette.captures;
        for _ in 0..5 {
            first.draft_sample = Instant::now() - Duration::from_millis(100);
            refresh_draft_preview(&runtime, &mut first);
            assert!(first.worker.wait_idle(Duration::from_secs(5)));
            assert!(first.static_palette.scene.is_none());
        }
        assert_eq!(first.static_palette.captures - before, 5);
        {
            let mut state = runtime.state.lock().unwrap();
            let session = state.session(&first.device_id);
            let profile = state
                .profiles
                .iter_mut()
                .find(|p| p.id == session.profile_id)
                .unwrap();
            profile
                .pages
                .iter_mut()
                .find(|p| p.id == session.page_id)
                .unwrap()
                .bindings
                .clear();
            state.revision += 1;
        }
        for c in [&mut first, &mut second] {
            c.draft_sample = Instant::now() - Duration::from_millis(100);
            refresh_draft_preview(&runtime, c);
            assert!(c.worker.wait_idle(Duration::from_secs(5)));
        }
        first_reports.lock().unwrap().clear();
        second_reports.lock().unwrap().clear();
        let captures = (
            first.static_palette.captures,
            second.static_palette.captures,
        );
        assert!(first.static_palette.scene.is_some() && second.static_palette.scene.is_some());
        // Exercise sixty seconds of eligible 50ms scheduler visits without waiting on wall time
        for tick in 0..1200 {
            if tick % 40 == 0 {
                runtime.preview_plugin_palette(candidate.clone()).unwrap();
            }
            for c in [&mut first, &mut second] {
                c.draft_sample = Instant::now() - Duration::from_millis(100);
                refresh_draft_preview(&runtime, c);
            }
        }
        assert_eq!(
            captures,
            (
                first.static_palette.captures,
                second.static_palette.captures
            )
        );
        assert_eq!(first.static_palette.skipped, 1200);
        assert_eq!(second.static_palette.skipped, 1200);
        assert!(first_reports.lock().unwrap().is_empty());
        assert!(second_reports.lock().unwrap().is_empty());
        // A genuine scene revision and an output invalidation each recapture immediately
        runtime.state.lock().unwrap().revision += 1;
        for c in [&mut first, &mut second] {
            c.draft_sample = Instant::now() - Duration::from_millis(100);
            refresh_draft_preview(&runtime, c);
        }
        assert_eq!(first.static_palette.captures, captures.0 + 1);
        first.delivery.clear();
        first.draft_sample = Instant::now() - Duration::from_millis(100);
        refresh_draft_preview(&runtime, &mut first);
        assert_eq!(first.static_palette.captures, captures.0 + 2);
        // A failed delivery cannot be hidden by an otherwise current static scene
        if let Some(delivery) = first.delivery.values_mut().next() {
            delivery.pending = false;
            delivery.completed = false;
        } else {
            panic!("The fixture must have a delivery to invalidate");
        }
        first.draft_sample = Instant::now() - Duration::from_millis(100);
        refresh_draft_preview(&runtime, &mut first);
        assert_eq!(first.static_palette.captures, captures.0 + 3);
        for c in [&mut first, &mut second] {
            assert!(c.worker.wait_idle(Duration::from_secs(5)));
        }
        first_reports.lock().unwrap().clear();
        second_reports.lock().unwrap().clear();
        let visual = runtime.hardware_preview_token(&first.device_id);
        let renewed = runtime.preview_plugin_palette(candidate).unwrap();
        assert_eq!(visual, runtime.hardware_preview_token(&first.device_id));
        runtime.cancel_plugin_palette_preview(Some(&token));
        assert_eq!(visual, runtime.hardware_preview_token(&first.device_id));
        for c in [&mut first, &mut second] {
            refresh_draft_preview(&runtime, c);
            assert!(c.worker.wait_idle(Duration::from_secs(5)));
        }
        assert!(first_reports.lock().unwrap().is_empty());
        assert!(second_reports.lock().unwrap().is_empty());
        runtime.cancel_plugin_palette_preview(Some(&renewed));
        for c in [&mut first, &mut second] {
            refresh_draft_preview(&runtime, c);
            assert!(c.worker.wait_idle(Duration::from_secs(5)));
        }
        assert_ne!(first_candidate, *first_reports.lock().unwrap());
        assert_ne!(second_candidate, *second_reports.lock().unwrap());
        assert!(runtime.hardware_preview_token(&first.device_id).is_none());
        assert!(runtime.hardware_preview_token(&second.device_id).is_none());
        assert_eq!(
            serde_json::to_value(assignments).unwrap(),
            serde_json::to_value(runtime.snapshot().sessions).unwrap()
        );
        runtime.shutdown();
    }

    #[test]
    // Verify that queued command cannot mutate a reconnected device or its benchmark
    fn queued_command_cannot_mutate_a_reconnected_device_or_its_benchmark() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let (mut connection, reports) = connection(&runtime);
        connection.epoch = 2;
        connection.benchmark = Some((42, Instant::now()));
        let mut connections = HashMap::from([("fixture".into(), connection)]);
        assert!(dispatch_command(
            &runtime,
            &mut connections,
            "fixture",
            1,
            DeviceCommand::Sleep
        )
        .is_err());
        let connection = &connections["fixture"];
        assert!(!connection.sleeping);
        assert_eq!(connection.benchmark.as_ref().map(|(run, _)| *run), Some(42));
        assert!(connection.worker.wait_idle(Duration::from_secs(1)));
        assert!(reports.lock().unwrap().is_empty());
        // A current-epoch command remains functional after the stale one
        connections.get_mut("fixture").unwrap().benchmark = None;
        dispatch_command(
            &runtime,
            &mut connections,
            "fixture",
            2,
            DeviceCommand::Sleep,
        )
        .unwrap();
        assert!(connections["fixture"].sleeping);
        assert!(connections["fixture"]
            .worker
            .wait_idle(Duration::from_secs(1)));
        assert!(!reports.lock().unwrap().is_empty());
        runtime.shutdown();
    }
    // Read the summed test metric or zero when it has not been recorded
    fn metric(runtime: &Runtime, name: &str) -> f64 {
        runtime
            .metrics
            .snapshot()
            .iter()
            .find(|m| m.name == name)
            .map_or(0.0, |m| m.sum)
    }
    #[test]
    // Verify that protected playback survives pressure and delivers replaced source at same frame index
    fn protected_playback_survives_pressure_and_delivers_replaced_source_at_same_frame_index() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = crate::media::tests::sample_gif();
        let path = root.path().join("source.gif");
        std::fs::write(&path, &source).unwrap();
        let asset = runtime.profiles.import_asset(&path).unwrap();
        let original = runtime.profiles.clip(&asset).unwrap().unwrap();
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile.background_asset = Some(asset.clone());
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        let (mut c, reports) = connection(&runtime);
        c.manifest.capabilities.background_size = Some([32, 16]);
        c.start_playback(
            "@background".into(),
            Playback {
                vendor_started: None,
                clip: Some(
                    runtime
                        .profiles
                        .media_budget
                        .protect(original.clone())
                        .unwrap(),
                ),
                asset: asset.clone(),
                _assets: runtime
                    .profiles
                    .asset_leases
                    .pin([asset.clone()], "Protected playback fixture")
                    .unwrap(),
                control: None,
                format: "png".into(),
                last_frame: None,
                next_sample: Duration::ZERO,
                pending_frame: None,
                media: crate::media::MediaOptions {
                    trim_end_ms: Some(1),
                    ..Default::default()
                },
            },
        );
        animate(&runtime, "frame-fixture", &mut c);
        assert!(c.worker.wait_idle(Duration::from_secs(2)));
        c.collect_outcomes(&runtime);
        assert_eq!(c.playbacks["@background"].last_frame, Some(0));
        let old_reports = reports.lock().unwrap().len();
        runtime.profiles.media_budget.trim_pressure();
        assert!(Arc::ptr_eq(
            &c.playbacks["@background"].clip.as_ref().unwrap().clip,
            &original
        ));
        c.next_frame = Instant::now();
        c.playbacks.get_mut("@background").unwrap().next_sample = Duration::ZERO;
        animate(&runtime, "frame-fixture", &mut c);
        assert!(c.worker.wait_idle(Duration::from_secs(2)));
        assert_eq!(reports.lock().unwrap().len(), old_reports);
        let green = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                32,
                16,
                image::Rgb([0, 255, 0]),
            )),
            "png",
        )
        .unwrap();
        let changed = crate::media::Clip::new("gif", vec![green; 2], vec![50; 2]).unwrap();
        let destination = runtime
            .profiles
            .root
            .join("assets")
            .join(format!("{asset}.sdmedia"));
        let staged = destination.with_extension("replacement");
        std::fs::write(&staged, changed.archive(&source).unwrap()).unwrap();
        crate::profiles::atomic_replace(&staged, &destination).unwrap();
        c.next_frame = Instant::now();
        c.playbacks.get_mut("@background").unwrap().next_sample = Duration::ZERO;
        animate(&runtime, "frame-fixture", &mut c);
        assert!(!Arc::ptr_eq(
            &c.playbacks["@background"].clip.as_ref().unwrap().clip,
            &original
        ));
        assert!(c.worker.wait_idle(Duration::from_secs(2)));
        c.collect_outcomes(&runtime);
        assert_eq!(c.playbacks["@background"].last_frame, Some(0));
        assert!(reports.lock().unwrap().len() > old_reports);
        c.playbacks.clear();
        assert_eq!(
            runtime.profiles.media_budget.stats()["protected_playback_bytes"],
            0
        );
        runtime.shutdown();
    }
    #[test]
    // Verify that background animation skips unchanged scenes and tracks duplicate delivery
    fn background_animation_skips_unchanged_scenes_and_tracks_duplicate_delivery() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("colors.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let (mut c, reports) = connection(&runtime);
        c.manifest.capabilities.background_size = Some([32, 16]);
        c.start_playback(
            "@background".into(),
            Playback {
                vendor_started: None,
                clip: Some(
                    runtime
                        .profiles
                        .media_budget
                        .protect(runtime.profiles.clip(&asset).unwrap().unwrap())
                        .unwrap(),
                ),
                asset: asset.clone(),
                _assets: runtime
                    .profiles
                    .asset_leases
                    .pin([asset], "Background fixture")
                    .unwrap(),
                control: None,
                format: "png".into(),
                last_frame: Some(0),
                next_sample: Duration::ZERO,
                pending_frame: None,
                // A deterministic one-frame trim avoids wall-clock boundary races
                media: crate::media::MediaOptions {
                    trim_end_ms: Some(1),
                    ..Default::default()
                },
            },
        );
        for _ in 0..20 {
            c.next_frame = Instant::now();
            animate(&runtime, "frame-fixture", &mut c);
        }
        assert_eq!(metric(&runtime, "scene_context_builds"), 0.0);
        assert_eq!(metric(&runtime, "unchanged_animation_opportunities"), 20.0);
        assert!(reports.lock().unwrap().is_empty());
        c.playbacks.get_mut("@background").unwrap().last_frame = None;
        c.playbacks.get_mut("@background").unwrap().next_sample = Duration::ZERO;
        c.next_frame = Instant::now();
        animate(&runtime, "frame-fixture", &mut c);
        assert_eq!(metric(&runtime, "scene_context_builds"), 1.0);
        assert!(c.playbacks["@background"].pending_frame.is_some());
        assert!(c.worker.wait_idle(Duration::from_secs(3)));
        c.collect_outcomes(&runtime);
        assert_eq!(c.playbacks["@background"].last_frame, Some(0));
        let transferred = reports.lock().unwrap().len();
        assert!(transferred > 0);

        // Initial restore or a different source index can already have sent the
        // same pixels. Duplicate suppression must still mark that frame done
        c.playbacks.get_mut("@background").unwrap().last_frame = None;
        c.playbacks.get_mut("@background").unwrap().next_sample = Duration::ZERO;
        c.next_frame = Instant::now();
        animate(&runtime, "frame-fixture", &mut c);
        assert_eq!(c.playbacks["@background"].last_frame, Some(0));
        assert!(c.playbacks["@background"].pending_frame.is_none());
        for _ in 0..20 {
            c.next_frame = Instant::now();
            animate(&runtime, "frame-fixture", &mut c);
        }
        assert_eq!(metric(&runtime, "scene_context_builds"), 2.0);
        assert_eq!(reports.lock().unwrap().len(), transferred);

        // The same case while an identical transfer is in flight must attach to
        // its existing revision, then skip CPU work until an outcome arrives
        let revision = c.delivery["@background"].revision;
        let delivery = c.delivery.get_mut("@background").unwrap();
        delivery.completed = false;
        delivery.pending = true;
        c.playbacks.get_mut("@background").unwrap().last_frame = None;
        c.playbacks.get_mut("@background").unwrap().next_sample = Duration::ZERO;
        c.next_frame = Instant::now();
        animate(&runtime, "frame-fixture", &mut c);
        assert_eq!(
            c.playbacks["@background"].pending_frame,
            Some((0, revision))
        );
        c.next_frame = Instant::now();
        animate(&runtime, "frame-fixture", &mut c);
        assert_eq!(metric(&runtime, "scene_context_builds"), 3.0);
        assert_eq!(reports.lock().unwrap().len(), transferred);
        runtime.shutdown();
    }
    #[test]
    // Verify that single frame backgrounds render once without starting an animation loop
    fn single_frame_backgrounds_render_once_without_starting_an_animation_loop() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("still.gif");
        let mut gif = vec![];
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    4,
                    4,
                    image::Rgba([15, 45, 90, 255]),
                )))
                .unwrap();
        }
        std::fs::write(&source, gif).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.background_asset = Some(asset);
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        let (mut c, reports) = connection(&runtime);
        c.manifest.controls.retain(|control| control.id == "key-1");
        c.manifest.capabilities.background_size = Some([32, 16]);
        restore(&runtime, &mut c);
        super::tests::finish_transition(&runtime, &mut c);
        assert!(c.playbacks.is_empty());
        assert!(c.worker.wait_idle(Duration::from_secs(3)));
        let transferred = reports.lock().unwrap().len();
        assert!(transferred > 0);
        let scenes = metric(&runtime, "scene_context_builds");
        for _ in 0..20 {
            c.next_frame = Instant::now();
            animate(&runtime, "frame-fixture", &mut c);
        }
        assert_eq!(metric(&runtime, "scene_context_builds"), scenes);
        assert_eq!(reports.lock().unwrap().len(), transferred);
        let preview = runtime.preview_surfaces(None).unwrap();
        assert!(!preview.animated);
        assert!(preview.background.is_some());
        runtime.shutdown();
    }
    #[test]
    // Verify that persistent plugin burst renders once per surface and honors touch override
    fn persistent_plugin_burst_renders_once_per_surface_and_honors_touch_override() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        crate::render_scene::install_burst_fixture(&runtime);
        let (connection, _) = connection(&runtime);
        let device = connection.device_id.clone();
        let mut connections = HashMap::from([(device.clone(), connection)]);
        let mut dirty = DirtyVisuals::default();
        for field in [
            FeedbackKind::Layout,
            FeedbackKind::State,
            FeedbackKind::Image,
            FeedbackKind::Title,
            FeedbackKind::Feedback,
        ] {
            assert!(field.persistent());
            for key in 1..=10 {
                dirty.insert(&device, &format!("key-{key}"), None);
            }
        }
        assert_eq!(dirty.surfaces.len(), 10);
        let mut flushes = 0;
        while !dirty.surfaces.is_empty() && flushes < 20 {
            flush_plugin_visuals(&runtime, &mut connections, &mut dirty);
            flushes += 1;
        }
        assert!(
            dirty.surfaces.is_empty(),
            "Bounded flushes must make progress"
        );
        let rendered = runtime
            .metrics
            .snapshot()
            .into_iter()
            .find(|m| m.name == "surface_render_invocations")
            .unwrap();
        assert_eq!(rendered.sum, 10.0);
        let contexts = runtime
            .metrics
            .snapshot()
            .into_iter()
            .find(|m| m.name == "scene_context_builds")
            .unwrap();
        assert_eq!(contexts.sum, f64::from(flushes));
        let manifest = &connections[&device].manifest;
        let dial = manifest
            .controls
            .iter()
            .find(|c| c.feedback_surface.is_some())
            .unwrap();
        let source = dial.id.clone();
        let output = dial.feedback_surface.clone().unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            let page = &mut state.profiles[0].pages[0];
            let mut binding = page.bindings["key-1"].clone();
            binding.action = crate::actions::Action::None;
            binding.title.clear();
            page.bindings.insert(source.clone(), binding.clone());
            page.bindings.insert(output, binding);
        }
        dirty.insert(&device, &source, None);
        flush_plugin_visuals(&runtime, &mut connections, &mut dirty);
        let rendered = runtime
            .metrics
            .snapshot()
            .into_iter()
            .find(|m| m.name == "surface_render_invocations")
            .unwrap();
        assert_eq!(
            rendered.sum, 10.0,
            "Explicit cleared touch binding suppresses dial feedback"
        );
        runtime.shutdown();
    }
    #[test]
    // Verify that transient barriers share one expired cycle budget and resume next cycle
    fn transient_barriers_share_one_expired_cycle_budget_and_resume_next_cycle() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        crate::render_scene::install_burst_fixture(&runtime);
        let (connection, _) = connection(&runtime);
        let device = connection.device_id.clone();
        let mut connections = HashMap::from([(device.clone(), connection)]);
        let mut dirty = DirtyVisuals::default();
        let expired = Instant::now() - Duration::from_secs(1);
        for _ in 0..64 {
            dirty.insert(
                &device,
                "key-1",
                Some(TransientVisual {
                    context: "fixture".into(),
                    generation: Some(0),
                    badge: Some("!".into()),
                }),
            );
            flush_plugin_visuals_until(&runtime, &mut connections, &mut dirty, expired);
        }
        assert_eq!(dirty.surfaces.len(), 1);
        assert!(
            runtime
                .metrics
                .snapshot()
                .iter()
                .all(|metric| metric.name != "surface_render_invocations"),
            "No barrier resets the exhausted actor-cycle budget"
        );
        flush_plugin_visuals_until(
            &runtime,
            &mut connections,
            &mut dirty,
            Instant::now() + Duration::from_secs(60),
        );
        assert!(dirty.surfaces.is_empty());
        assert_eq!(
            runtime
                .metrics
                .snapshot()
                .iter()
                .find(|metric| metric.name == "surface_render_invocations")
                .unwrap()
                .sum,
            1.0
        );
        runtime.shutdown();
    }
    #[test]
    // Verify that transient expiry and new badge order never resurrect or clear the wrong badge
    fn transient_expiry_and_new_badge_order_never_resurrect_or_clear_the_wrong_badge() {
        let alert = serde_json::json!({"badge":"!","title":"normal title update"});
        let expired = serde_json::json!({"badge":null});
        let newer_ok = serde_json::json!({"badge":"OK"});
        assert!(
            current_badge_matches(Some(&alert), Some("!")),
            "Ordinary state preserves the live alert"
        );
        assert!(
            !current_badge_matches(Some(&expired), Some("!")),
            "Delayed alert cannot resurrect after expiry"
        );
        assert!(
            !current_badge_matches(Some(&alert), None),
            "Delayed expiry cannot clear a newer alert"
        );
        assert!(
            !current_badge_matches(Some(&newer_ok), Some("!")),
            "Older alert cannot replace newer OK badge"
        );
        assert!(current_badge_matches(Some(&expired), None));
    }
    #[test]
    // Verify that stale transient redraws current state without replaying old badge
    fn stale_transient_redraws_current_state_without_replaying_old_badge() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        crate::render_scene::install_burst_fixture(&runtime);
        let (connection, _) = connection(&runtime);
        let device = connection.device_id.clone();
        let context = runtime.render_scene(&device).unwrap();
        let control = connection.manifest.control("key-1").unwrap();
        let data = runtime
            .render_bound_surface_in_scene(
                control,
                context.page.as_ref().unwrap().bindings.get("key-1"),
                connection.manifest.layout.rotation,
                &context.scope,
                "key-1",
                &context,
            )
            .unwrap();
        let expected = command_digest(&DeviceCommand::Image {
            control_id: "key-1".into(),
            data: runtime.owned_render_bytes(data),
        });
        let mut connections = HashMap::from([(device.clone(), connection)]);
        let mut dirty = DirtyVisuals::default();
        dirty.insert(
            &device,
            "key-1",
            Some(TransientVisual {
                context: "old-plugin-context".into(),
                generation: Some(123),
                badge: Some("!".into()),
            }),
        );
        for _ in 0..20 {
            if dirty.surfaces.is_empty() {
                break;
            }
            flush_plugin_visuals(&runtime, &mut connections, &mut dirty);
        }
        assert!(dirty.surfaces.is_empty());
        assert_eq!(connections[&device].delivery["key-1"].digest, expected);
        assert_eq!(
            runtime
                .metrics
                .snapshot()
                .iter()
                .find(|metric| metric.name == "obsolete_plugin_transients")
                .unwrap()
                .sum,
            1.0
        );
        runtime.shutdown();
    }
    #[test]
    // Settings refreshes stay within the connection epoch and retain pending plugin badges
    fn settings_refresh_never_restarts_scenes_or_replaces_pending_badges() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (n4, n4_reports) = connection(&runtime);
        let (mut k1, k1_reports) = connection_for_model(&runtime, "k1-pro");
        k1.device_id = "keyboard".into();
        let n4_id = n4.device_id.clone();
        let n4_epoch = n4.epoch;
        let n4_scene = n4.scene_revision;
        let k1_scene = k1.scene_revision;
        let mut coordinator = Coordinator::new();
        coordinator.connections.insert(n4_id.clone(), n4);
        coordinator.connections.insert("keyboard".into(), k1);
        coordinator.dirty.insert(
            &n4_id,
            "key-1",
            Some(TransientVisual {
                context: "fixture".into(),
                generation: Some(1),
                badge: Some("ok".into()),
            }),
        );
        coordinator.dispatch(
            &runtime,
            Request::RefreshPluginBindings {
                device: n4_id.clone(),
                epoch: n4_epoch + 1,
                sources: vec!["key-2".into()],
            },
        );
        assert_eq!(coordinator.dirty.surfaces.len(), 1);
        coordinator.dispatch(
            &runtime,
            Request::RefreshPluginBindings {
                device: n4_id.clone(),
                epoch: n4_epoch,
                sources: vec!["key-1".into(), "key-2".into()],
            },
        );
        assert_eq!(coordinator.dirty.surfaces.len(), 2);
        assert!(coordinator.dirty.surfaces[&(n4_id.clone(), "key-1".into())].is_some());
        assert!(coordinator
            .dirty
            .surfaces
            .keys()
            .all(|(id, _)| id == &n4_id));
        assert_eq!(coordinator.connections[&n4_id].scene_revision, n4_scene);
        assert_eq!(coordinator.connections["keyboard"].scene_revision, k1_scene);
        assert!(n4_reports.lock().unwrap().is_empty());
        assert!(k1_reports.lock().unwrap().is_empty());
        runtime.shutdown();
    }

    #[test]
    // Verify that dirty retry storage is bounded and duplicate updates keep fair order
    fn dirty_retry_storage_is_bounded_and_duplicate_updates_keep_fair_order() {
        let mut dirty = DirtyVisuals::default();
        for key in 0..600 {
            dirty.insert("fixture", &format!("key-{key}"), None);
        }
        assert_eq!(dirty.surfaces.len(), 512);
        assert_eq!(dirty.order.len(), 512);
        assert!(dirty.restore_required);
        let first = dirty.order.front().cloned();
        for _ in 0..1000 {
            dirty.insert("fixture", "key-0", None);
        }
        assert_eq!(dirty.order.front().cloned(), first);
        assert_eq!(dirty.order.len(), 512);
    }
    #[test]
    // Verify that transient and ordered events are flush barriers not persistent state
    fn transient_and_ordered_events_are_flush_barriers_not_persistent_state() {
        for kind in [
            FeedbackKind::Alert,
            FeedbackKind::Ok,
            FeedbackKind::BadgeExpired,
            FeedbackKind::Unsupported,
            FeedbackKind::Other,
        ] {
            assert!(!kind.persistent(), "{kind:?} must not be collapsed");
        }
        assert!(FeedbackKind::Alert.transient());
        assert!(FeedbackKind::BadgeExpired.transient());
        let mut dirty = DirtyVisuals::default();
        dirty.insert("one", "key-1", None);
        dirty.insert("two", "key-1", None);
        dirty.insert("one", "key-2", None);
        dirty.insert("one", "key-1", None);
        assert_eq!(dirty.surfaces.len(), 3, "Device and control isolation");
    }

    #[test]
    // Verify that benchmark input is observation only and stop restores dispatch
    fn benchmark_input_is_observation_only_and_stop_restores_dispatch() {
        let fixture_root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
        let (mut c, _) = connection(&runtime);
        let calls = std::cell::Cell::new(0);
        *c.benchmark_input.lock().unwrap() = true;
        c.benchmark = Some((7, Instant::now()));
        assert!(guarded_input(&c.benchmark_input, || calls.set(calls.get() + 1)));
        assert_eq!(
            calls.get(),
            0,
            "No power, shell, or plugin dispatch in a benchmark"
        );
        c.stop_benchmark();
        assert!(!guarded_input(&c.benchmark_input, || calls.set(calls.get() + 1)));
        assert_eq!(calls.get(), 1);
        assert!(c.benchmark.is_none());
        // Each connection owns a different gate; old leases cannot suppress a reconnect
        let (next, _) = connection(&runtime);
        *c.benchmark_input.lock().unwrap() = true;
        assert!(!guarded_input(&next.benchmark_input, || calls.set(calls.get() + 1)));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    // Verify that healthy multisurface frame does not trigger legacy queue backoff
    fn healthy_multisurface_frame_does_not_trigger_legacy_queue_backoff() {
        let mut policy = OutputPolicy {
            mode: OutputMode::FrameScheduling,
            ..Default::default()
        };
        let mut stats = OutputQueueStats {
            pending_count: 15,
            pending_bytes: 100_000,
            ..Default::default()
        };
        let mut rate = AnimationRate::default();
        rate.observe(animation_queue_depth(policy, &stats), 20.0, Instant::now());
        assert_eq!(rate.divisor, 1);
        policy.mode = OutputMode::IndividualUpdates;
        rate.observe(animation_queue_depth(policy, &stats), 20.0, Instant::now());
        assert_eq!(rate.divisor, 2);
        policy.mode = OutputMode::FrameScheduling;
        stats.pending_bytes = policy.max_pending_bytes;
        assert!(animation_queue_depth(policy, &stats) > 2);
    }

    // Build ten independently addressed image commands for output-queue tests
    fn commands() -> Vec<DeviceCommand> {
        (1..=10)
            .map(|index| DeviceCommand::Image {
                control_id: format!("key-{index}"),
                data: hardware_core::render::encode_pixels(
                    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                        16,
                        16,
                        image::Rgb([index, 0, 0]),
                    )),
                    "png",
                )
                .unwrap(),
            })
            .collect()
    }
    #[test]
    // Verify that benchmark wake restores selected brightness after model initialization
    fn benchmark_wake_restores_selected_brightness_after_model_initialization() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        for model in ["n1-vsd", "n1", "n4-pro"] {
            for brightness in [42, 65, 0] {
                let (mut c, reports) = connection_for_model(&runtime, model);
                let output = crate::workspace::DeviceOutput {
                    brightness,
                    ..Default::default()
                };
                runtime.state.lock().unwrap().sessions.insert(
                    c.device_id.clone(),
                    crate::workspace::DeviceSession {
                        output,
                        ..Default::default()
                    },
                );
                let before = serde_json::to_value(&runtime.state.lock().unwrap().sessions).unwrap();
                let expected_wake = c
                    .session
                    .lock()
                    .unwrap()
                    .encode(DeviceCommand::Wake)
                    .unwrap();
                wake_for_benchmark(&runtime, &mut c).unwrap();
                assert!(c.worker.wait_idle(Duration::from_secs(2)));
                let captured = reports.lock().unwrap();
                assert_eq!(
                    &captured[..expected_wake.reports.len()],
                    expected_wake.reports.as_slice()
                );
                assert_eq!(captured.len(), expected_wake.reports.len() + 1);
                assert_eq!(
                    &captured.last().unwrap()[6..12],
                    &[b'L', b'I', b'G', 0, 0, brightness],
                    "{model}"
                );
                assert_eq!(
                    serde_json::to_value(&runtime.state.lock().unwrap().sessions).unwrap(),
                    before
                );
            }
        }
        runtime.shutdown();
    }
    #[test]
    // Verify that physical benchmark path transfers every lcd section above fifteen
    fn physical_benchmark_path_transfers_every_lcd_section_above_fifteen() {
        let fixture_root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
        // The physical actor's frame handler, real encoders and worker use a
        // capture transport here; this is not a USB/firmware qualification
        for (model, count) in [("n1", 18), ("n1-vsd", 18), ("xl", 33), ("n4-pro", 15)] {
            for (mode, encoding) in [
                (
                    OutputMode::IndividualUpdates,
                    FrameEncoding::IndividualUpdates,
                ),
                (
                    OutputMode::FrameScheduling,
                    FrameEncoding::IndividualUpdates,
                ),
                (
                    OutputMode::FrameScheduling,
                    FrameEncoding::GroupedKeyUpdates,
                ),
            ] {
                let (mut c, reports) = connection_for_model(&runtime, model);
                c.policy.mode = mode;
                c.worker.set_policy(c.policy).unwrap();
                c.scene_revision = c.worker.scene_revision();
                c.encoding = encoding;
                c.benchmark = Some((7, Instant::now()));
                let mut commands: Vec<_> = c
                    .manifest
                    .controls
                    .iter()
                    .filter(|control| control.image_slot.is_some())
                    .map(|control| DeviceCommand::Image {
                        control_id: control.id.clone(),
                        data: hardware_core::render::encode_pixels(
                            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                                (control.image_width.unwrap_or(control.width)) as u32,
                                (control.image_height.unwrap_or(control.height)) as u32,
                                image::Rgb([42, 64, 128]),
                            )),
                            "png",
                        )
                        .unwrap(),
                    })
                    .collect();
                if let Some([width, height]) = c.manifest.capabilities.background_size {
                    commands.push(DeviceCommand::Background {
                        data: hardware_core::render::encode_pixels(
                            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                                u32::from(width),
                                u32::from(height),
                                image::Rgb([42, 64, 128]),
                            )),
                            "png",
                        )
                        .unwrap(),
                        x: 0,
                        y: 0,
                        width,
                        height,
                    });
                }
                assert_eq!(commands.len(), count);
                let mut connections = HashMap::from([("fixture".into(), c)]);
                benchmark_frame(&mut connections, "fixture", 1, 7, commands)
                    .unwrap_or_else(|error| panic!("{model} {mode:?} {encoding:?}: {error}"));
                let c = &connections["fixture"];
                assert!(c.worker.wait_idle(Duration::from_secs(3)));
                let outcomes = c.worker.drain_outcomes();
                assert_eq!(outcomes.len(), count, "{model} {mode:?} {encoding:?}");
                assert!(outcomes
                    .iter()
                    .all(|outcome| outcome.status == OutputStatus::Completed
                        && outcome.report_bytes > 0));
                assert_eq!(c.delivery.len(), count);
                assert!(!reports.lock().unwrap().is_empty());
            }
        }
    }
    #[test]
    // Verify that physical benchmark frames remain bounded to owned supported unique surfaces
    fn physical_benchmark_frames_remain_bounded_to_owned_supported_unique_surfaces() {
        let fixture_root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
        let (mut c, reports) = connection_for_model(&runtime, "n1");
        c.benchmark = Some((7, Instant::now()));
        let mut connections = HashMap::from([("fixture".into(), c)]);
        let image = |id: &str| DeviceCommand::Image {
            control_id: id.into(),
            data: hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    96,
                    96,
                    image::Rgb([42, 64, 128]),
                )),
                "png",
            )
            .unwrap(),
        };
        for commands in [
            vec![],
            vec![DeviceCommand::Sleep],
            vec![image("key-16")],
            vec![image("key-999")],
            vec![image("key-1"), image("key-1")],
            (0..19).map(|_| image("key-1")).collect(),
            vec![DeviceCommand::Background {
                data: vec![1],
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }],
        ] {
            assert!(benchmark_frame(&mut connections, "fixture", 1, 7, commands).is_err());
        }
        assert!(benchmark_frame(&mut connections, "fixture", 2, 7, vec![image("key-1")]).is_err());
        assert!(benchmark_frame(&mut connections, "fixture", 1, 8, vec![image("key-1")]).is_err());
        let c = &connections["fixture"];
        assert!(c.worker.wait_idle(Duration::from_secs(1)));
        assert_eq!(c.worker.queue_stats().total_report_bytes, 0);
        assert!(c.delivery.is_empty());
        assert!(reports.lock().unwrap().is_empty());
    }
    #[test]
    // Verify that ten changed keys are one frame submission and pending duplicates are suppressed
    fn ten_changed_keys_are_one_frame_submission_and_pending_duplicates_are_suppressed() {
        let fixture_root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
        let (mut c, reports) = connection(&runtime);
        let accepted = send_surfaces(&mut c, commands(), OutputPriority::Animation).unwrap();
        assert_eq!(accepted.len(), 10);
        assert!(c
            .delivery
            .values()
            .all(|value| value.pending && !value.completed));
        assert!(send_surfaces(&mut c, commands(), OutputPriority::Animation)
            .unwrap()
            .is_empty());
        assert!(c.worker.wait_idle(Duration::from_secs(3)));
        let outcomes = c.worker.drain_outcomes();
        assert_eq!(outcomes.len(), 10);
        assert!(outcomes
            .iter()
            .all(|outcome| outcome.status == OutputStatus::Completed));
        let reports = reports.lock().unwrap();
        assert_eq!(
            reports
                .iter()
                .filter(|report| report.get(6..9) == Some(b"BAT"))
                .count(),
            10
        );
        assert_eq!(
            reports
                .iter()
                .filter(|report| report.get(6..9) == Some(b"STP"))
                .count(),
            1
        );
    }
    #[test]
    // Verify that identical images become eligible again after scene invalidation
    fn identical_images_become_eligible_again_after_scene_invalidation() {
        let fixture_root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
        let (mut c, _) = connection(&runtime);
        assert_eq!(
            send_surfaces(&mut c, commands(), OutputPriority::Animation)
                .unwrap()
                .len(),
            10
        );
        assert!(c.worker.wait_idle(Duration::from_secs(3)));
        c.invalidate_scene();
        assert!(c.delivery.is_empty());
        assert_eq!(
            send_surfaces(&mut c, commands(), OutputPriority::Animation)
                .unwrap()
                .len(),
            10
        );
        assert!(c.worker.wait_idle(Duration::from_secs(3)));
        assert_eq!(c.worker.scene_revision(), c.scene_revision);
    }
    #[test]
    // Verify that frame policy requires capability and has bounded limits without acknowledgement
    fn frame_policy_requires_capability_and_has_bounded_limits_without_acknowledgement() {
        let fixture_root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
        let (c, _) = connection(&runtime);
        let mut settings = crate::workspace::DeviceFramePolicy {
            policy: c.policy,
            encoding: FrameEncoding::GroupedKeyUpdates,
        };
        assert!(settings.validate(&c.manifest.capabilities).is_ok());
        assert!(settings.validate(&c.manifest.capabilities).is_ok());
        let mut unsupported = c.manifest.capabilities.clone();
        unsupported.frame_output = None;
        assert!(settings.validate(&unsupported).is_err());
        settings.policy.target_frame_hz = 60;
        assert!(settings.validate(&c.manifest.capabilities).is_err());
    }
    #[test]
    // Verify that default output groups supported keys and preserves explicit rendering
    fn default_output_groups_supported_keys_and_preserves_explicit_rendering() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        for module in hardware_registry::modules() {
            let (mut c, _) = connection_for_model(&runtime, &module.manifest().model_id);
            runtime.state.lock().unwrap().sessions.insert(
                c.device_id.clone(),
                crate::workspace::DeviceSession::default(),
            );
            let supported = c
                .manifest
                .capabilities
                .frame_output
                .as_ref()
                .is_some_and(|capability| capability.grouped_key_updates)
                && c.session
                    .lock()
                    .unwrap()
                    .supports_frame_encoding(FrameEncoding::GroupedKeyUpdates);
            restore(&runtime, &mut c);
            assert_eq!(c.policy.mode, OutputMode::FrameScheduling);
            assert_eq!(
                c.encoding,
                if supported {
                    FrameEncoding::GroupedKeyUpdates
                } else {
                    FrameEncoding::IndividualUpdates
                },
                "{} default",
                c.manifest.model_id
            );

            runtime
                .state
                .lock()
                .unwrap()
                .sessions
                .get_mut(&c.device_id)
                .unwrap()
                .output
                .frame_policy = Some(crate::workspace::DeviceFramePolicy {
                policy: OutputPolicy::default(),
                encoding: FrameEncoding::IndividualUpdates,
            });
            restore(&runtime, &mut c);
            assert_eq!(
                c.encoding,
                FrameEncoding::IndividualUpdates,
                "{} saved choice",
                c.manifest.model_id
            );

            runtime
                .state
                .lock()
                .unwrap()
                .sessions
                .get_mut(&c.device_id)
                .unwrap()
                .output
                .frame_policy = None;
            c.force_individual_updates = true;
            restore(&runtime, &mut c);
            assert_eq!(c.policy.mode, OutputMode::IndividualUpdates);
            assert_eq!(c.encoding, FrameEncoding::IndividualUpdates);
        }
        runtime.shutdown();
    }
    #[test]
    // Verify that missing required lighting fields are rejected and fresh defaults remain fifteen percent
    fn missing_required_lighting_fields_are_rejected_and_fresh_defaults_remain_fifteen_percent() {
        assert!(serde_json::from_str::<crate::workspace::DeviceOutput>("{}").is_err());
        let defaults = crate::workspace::DeviceOutput::default();
        assert_eq!(defaults.lighting_brightness, 15);
        assert_eq!(defaults.brightness, 50);
        let mut value = serde_json::to_value(defaults).unwrap();
        value["lighting_brightness"] = serde_json::json!(65);
        let saved: crate::workspace::DeviceOutput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(saved.lighting_brightness, 65);
        value.as_object_mut().unwrap().remove("lighting_brightness");
        assert!(serde_json::from_value::<crate::workspace::DeviceOutput>(value).is_err());
    }
    #[test]
    // Verify that current device output allows omitted optional frame policy
    fn current_device_output_allows_omitted_optional_frame_policy() {
        let settings: crate::workspace::DeviceOutput = serde_json::from_str(
            r#"{"sleeping":false,"brightness":65,"lighting_brightness":65,"lighting_colors":[]}"#,
        )
        .unwrap();
        assert!(settings.frame_policy.is_none());
        assert_eq!(
            hardware_core::OutputPolicy::default().mode,
            hardware_core::OutputMode::FrameScheduling
        );
        assert!(serde_json::to_value(settings)
            .unwrap()
            .get("frame_policy")
            .is_none());
    }
    #[test]
    #[ignore = "Release-only throughput probe with local loopback runtime; simulated transport never opens HID"]
    // Verify that cold 25 FPS background keeps pace with an active device preview
    fn cold_25_fps_background_keeps_pace_with_an_active_device_preview() {
        cold_background("mp4");
    }
    #[test]
    #[ignore = "Release-only throughput probe with local loopback runtime; simulated transport never opens HID"]
    // Verify that cold 25 FPS GIF background keeps pace with an active device preview
    fn cold_25_fps_gif_background_keeps_pace_with_an_active_device_preview() {
        cold_background("gif");
    }
    // Exercise cold animated backgrounds through the requested encoder and simulated device
    fn cold_background(format: &str) {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let device = "simulator:n4-pro:cold-video";
        runtime.simulate_device(true, device).unwrap();
        let mut frames = vec![];
        for frame in 0..25 {
            let pixels = image::RgbImage::from_fn(512, 288, |x, y| {
                image::Rgb([
                    ((x * 19 + y * 7 + frame * 11) % 256) as u8,
                    ((x * 5 + y * 31 + frame * 17) % 256) as u8,
                    ((x * 13 + y * 11 + frame * 23) % 256) as u8,
                ])
            });
            frames.push(
                hardware_core::render::encode_pixels_quality(
                    image::DynamicImage::ImageRgb8(pixels),
                    if format == "gif" { "png" } else { "jpeg" },
                    92,
                )
                .unwrap(),
            );
        }
        let clip =
            crate::media::Clip::with_timing(format, frames, vec![40_000; 25], 2, None).unwrap();
        let (asset, lease) = runtime
            .profiles
            .publish_prepared_clip_shared(
                Arc::new(clip),
                if format == "gif" {
                    include_bytes!("../../../tests/fixtures/media/source-25.gif")
                } else {
                    include_bytes!("../../../tests/fixtures/media/source-25-vp9.mp4")
                },
                &Default::default(),
            )
            .unwrap();
        let media = crate::media::MediaOptions {
            fit: "fit".into(),
            letterbox: Some("black".into()),
            ..Default::default()
        };
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile.background_asset = Some(asset.clone());
        profile.background_media = media.clone();
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        let (mut c, _reports) = connection(&runtime);
        c.device_id = device.into();
        c.start_playback(
            "@background".into(),
            Playback {
                vendor_started: None,
                clip: None,
                asset,
                _assets: lease,
                control: None,
                format: "png".into(),
                last_frame: None,
                next_sample: Duration::ZERO,
                pending_frame: None,
                media,
            },
        );
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let preview_stop = stop.clone();
        let preview_runtime = runtime.clone();
        let preview = std::thread::spawn(move || {
            while !preview_stop.load(std::sync::atomic::Ordering::Relaxed) {
                match preview_runtime.preview_surfaces_binary(device) {
                    Ok(_) => {}
                    Err(error) if error.starts_with("Render scene changed during ") => {}
                    Err(error) => panic!("Preview failed: {error}"),
                }
                std::thread::sleep(Duration::from_millis(8));
            }
        });
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(1) {
            animate(&runtime, device, &mut c);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(c.worker.wait_idle(Duration::from_secs(3)));
        c.collect_outcomes(&runtime);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        preview.join().unwrap();
        let completed = runtime
            .metrics
            .snapshot()
            .iter()
            .find(|m| m.name == "output_surface_completed")
            .map_or(0., |m| m.sum);
        eprintln!(
            "COLD_ANIMATION format={format} first_second_completed={completed}, cache={}",
            runtime.cache_stats()
        );
        assert!(
            completed >= 18.,
            "Cold playback completed only {completed} of 25 opportunities with preview running"
        );
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires local loopback sockets for isolated runtime; simulated transport never opens HID"]
    // Verify that scheduler selects multiple due animations and updates frames only after completion
    fn scheduler_selects_multiple_due_animations_and_updates_frames_only_after_completion() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        let source = root.path().join("colors.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let asset = runtime.import_asset(&source).unwrap();
        let (mut c, _) = connection(&runtime);
        let lease = runtime
            .profiles
            .asset_leases
            .pin([asset.clone()], "Frame fixture")
            .unwrap();
        for control in c
            .manifest
            .controls
            .iter_mut()
            .filter(|control| control.id.starts_with("key-"))
        {
            control.image_width = Some(16);
            control.image_height = Some(16);
        }
        for key in ["key-1", "key-2"] {
            c.start_playback(
                key.into(),
                Playback {
                    vendor_started: None,
                    clip: None,
                    asset: asset.clone(),
                    _assets: lease.clone(),
                    control: Some(key.into()),
                    format: "png".into(),
                    last_frame: None,
                    next_sample: Duration::ZERO,
                    pending_frame: None,
                    media: Default::default(),
                },
            );
        }
        animate(&runtime, "frame-fixture", &mut c);
        assert_eq!(
            c.delivery.len(),
            2,
            "A cycle must gather multiple due surfaces"
        );
        assert!(c
            .playbacks
            .values()
            .all(|p| p.last_frame.is_none() && p.pending_frame.is_some()));
        assert!(c.worker.wait_idle(Duration::from_secs(3)));
        c.collect_outcomes(&runtime);
        assert!(c
            .playbacks
            .values()
            .all(|p| p.last_frame.is_some() && p.pending_frame.is_none()));
        runtime.shutdown();
    }
}

#[cfg(test)]
#[path = "playback_benchmark.rs"]
mod playback_benchmark;

#[cfg(test)]
#[path = "n4_refresh_probe.rs"]
mod n4_refresh_probe;

#[cfg(test)]
mod reply_tests {
    use super::*;
    #[test]
    // Verify that benchmark reply preserves success and operation errors
    fn benchmark_reply_preserves_success_and_operation_errors() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(42)).unwrap();
        assert_eq!(receive_benchmark_reply(rx, "stats", Duration::ZERO), Ok(42));
        let (tx, rx) = mpsc::channel::<std::result::Result<(), String>>();
        tx.send(Err("device changed".into())).unwrap();
        assert_eq!(
            receive_benchmark_reply(rx, "stop", Duration::ZERO),
            Err("device changed".into())
        );
    }
    #[test]
    // Verify that benchmark reply labels timeout and disconnect
    fn benchmark_reply_labels_timeout_and_disconnect() {
        let (tx, rx) = mpsc::channel::<std::result::Result<(), String>>();
        assert!(receive_benchmark_reply(rx, "frame", Duration::ZERO)
            .unwrap_err()
            .starts_with("Benchmark frame timed out:"));
        let (tx2, rx) = mpsc::channel::<std::result::Result<(), String>>();
        drop(tx2);
        assert!(receive_benchmark_reply(rx, "outcomes", Duration::ZERO)
            .unwrap_err()
            .starts_with("Benchmark outcomes timed out:"));
        drop(tx);
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    // Submit the devices request
    fn request(
        runtime: &Runtime,
        c: &Connection,
        deadline: Instant,
    ) -> (
        Request,
        mpsc::Receiver<std::result::Result<(), crate::keyboard::TransferError>>,
    ) {
        let device = runtime
            .snapshot()
            .devices
            .into_iter()
            .find(|d| d.id == c.device_id)
            .unwrap();
        let contract_key = runtime.device_contract_key(&device).unwrap();
        let transaction = c
            .session
            .lock()
            .unwrap()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                brightness: Some(3),
                color: Some([11, 22, 33]),
                os_mode: Some(KeyboardOsMode::Windows),
                ..Default::default()
            }))
            .unwrap();
        let (reply, receive) = mpsc::channel();
        (
            Request::Keyboard {
                device: c.device_id.clone(),
                epoch: c.epoch,
                contract_key,
                transaction,
                deadline,
                reply,
            },
            receive,
        )
    }
    #[test]
    // Verify that keyboard coordinator admission guards and tracked mock completion
    fn keyboard_coordinator_admission_guards_and_tracked_mock_completion() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (c, reports) = frame_policy_tests::connection_for_model(&runtime, "k1-pro");
        let id = c.device_id.clone();
        let mut coordinator = Coordinator::new();
        coordinator.connections.insert(id.clone(), c);
        // Refuse expired work before submitting any packet
        let (expired, receive) = request(&runtime, &coordinator.connections[&id], Instant::now());
        coordinator.dispatch(&runtime, expired);
        assert!(receive.recv().unwrap().is_err());
        assert!(reports.lock().unwrap().is_empty());
        // Neither sleep nor benchmark ownership may be overridden by Apply
        coordinator.connections.get_mut(&id).unwrap().sleeping = true;
        let (asleep, receive) = request(
            &runtime,
            &coordinator.connections[&id],
            Instant::now() + Duration::from_secs(5),
        );
        coordinator.dispatch(&runtime, asleep);
        assert!(receive.recv().unwrap().is_err());
        coordinator.connections.get_mut(&id).unwrap().sleeping = false;
        coordinator.connections.get_mut(&id).unwrap().benchmark = Some((1, Instant::now()));
        let (busy, receive) = request(
            &runtime,
            &coordinator.connections[&id],
            Instant::now() + Duration::from_secs(5),
        );
        coordinator.dispatch(&runtime, busy);
        assert!(receive.recv().unwrap().is_err());
        coordinator.connections.get_mut(&id).unwrap().benchmark = None;
        assert!(reports.lock().unwrap().is_empty());
        // A stale connection cannot target a replacement device
        let (mut stale, receive) = request(
            &runtime,
            &coordinator.connections[&id],
            Instant::now() + Duration::from_secs(5),
        );
        if let Request::Keyboard { epoch, .. } = &mut stale {
            *epoch += 1;
        }
        coordinator.dispatch(&runtime, stale);
        assert!(receive.recv().unwrap().is_err());
        assert!(reports.lock().unwrap().is_empty());
        let (apply, receive) = request(
            &runtime,
            &coordinator.connections[&id],
            Instant::now() + Duration::from_secs(5),
        );
        coordinator.dispatch(&runtime, apply);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let c = coordinator.connections.get_mut(&id).unwrap();
            if c.keyboard.as_mut().unwrap().poll(c.worker.failure()) {
                c.keyboard = None;
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        receive.recv().unwrap().unwrap();
        assert_eq!(reports.lock().unwrap().len(), 3);
        assert!(dispatch_command(
            &runtime,
            &mut coordinator.connections,
            &id,
            1,
            DeviceCommand::Keyboard(KeyboardSettings {
                brightness: Some(1),
                ..Default::default()
            })
        )
        .is_err());
        // Saved keyboard values never become part of surface/profile restoration
        let settings = KeyboardSettings {
            brightness: Some(5),
            color: Some([1, 2, 3]),
            os_mode: Some(KeyboardOsMode::Macos),
            effect: Some(2),
            speed: Some(3),
            ..Default::default()
        };
        runtime
            .state
            .lock()
            .unwrap()
            .sessions
            .entry(id.clone())
            .or_default()
            .output
            .keyboard = Some(settings);
        let before = reports.lock().unwrap().len();
        let c = coordinator.connections.get_mut(&id).unwrap();
        restore(&runtime, c);
        tests::finish_transition(&runtime, c);
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        let reports = reports.lock().unwrap();
        for report in &reports[before..] {
            let command = report.get(6..).unwrap_or_default();
            assert!(
                !command.starts_with(b"LLUM")
                    && !command.starts_with(b"COLOR")
                    && !command.starts_with(b"CPOS")
                    && !command.starts_with(b"LMOD")
                    && !command.starts_with(b"LSPD")
            );
        }
        drop(reports);
        coordinator.finish();
        runtime.shutdown();
    }
}
