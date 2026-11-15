//! Ephemeral, bounded setup checks. Readiness never dispatches actions, grants
//! permissions, starts plugins, downloads dependencies or changes runtime choices
use crate::{onboarding::SetupCheck, Runtime};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const QUALIFICATION: &str = "Readiness and observed input apply only to this check. They do not establish physical, firmware, native action or screen-reader qualification.";
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SetupCapabilityMode {
    Passive,
    DeviceInput,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupCapabilityRequest {
    pub mode: SetupCapabilityMode,
    pub device_id: Option<String>,
    pub plugin_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct SetupCapabilityJob {
    pub id: String,
    pub request: SetupCapabilityRequest,
    pub state: String,
    pub checks: Vec<SetupCheck>,
    pub qualification: String,
}
struct OwnedJob {
    report: SetupCapabilityJob,
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    epoch: Option<u64>,
    simulated: bool,
    input: Option<String>,
}
#[derive(Default)]
struct State {
    active: Option<String>,
    jobs: VecDeque<OwnedJob>,
}
#[derive(Default)]
pub(crate) struct SetupCapabilities {
    state: Mutex<State>,
}
// Truncate text at a UTF-8 boundary and append an ellipsis when it exceeds the limit
fn bounded(mut value: String, limit: usize) -> String {
    if value.len() > limit {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push('…');
    }
    value
}
// Check the current setup capabilities conditions
fn check(id: &str, title: &str, status: &str, detail: impl Into<String>) -> SetupCheck {
    SetupCheck {
        id: bounded(id.into(), 128),
        title: bounded(title.into(), 256),
        status: status.into(),
        detail: bounded(detail.into(), 2048),
        action: String::new(),
    }
}
// Settle still-waiting input checks when listening stops without an observed interaction
fn settle_waiting(job: &mut OwnedJob, status: &str) {
    for item in &mut job.report.checks {
        if item.status == "waiting" {
            item.status = status.into();
            item.detail = "Input listening stopped without an observed interaction.".into();
        }
    }
}
impl SetupCapabilities {
    // Report whether current work prevents releasing editor resources
    pub(crate) fn editor_retirement_blocked(&self) -> bool {
        crate::editor_retirement::unavailable_or(&self.state, |state| state.active.is_some())
    }

    // Reserve capacity for setup capabilities
    fn reserve(
        &self,
        request: SetupCapabilityRequest,
        epoch: Option<u64>,
        simulated: bool,
    ) -> Result<SetupCapabilityJob, String> {
        let mut state = self.state.lock().unwrap();
        if state.active.is_some() {
            return Err("The previous setup check is still running or stopping".into());
        }
        while state.jobs.len() >= 8 {
            state.jobs.pop_front();
        }
        let report = SetupCapabilityJob {
            id: uuid::Uuid::new_v4().to_string(),
            request,
            state: "running".into(),
            checks: vec![],
            qualification: QUALIFICATION.into(),
        };
        let duration = if report.request.mode == SetupCapabilityMode::DeviceInput {
            30
        } else {
            20
        };
        state.jobs.push_back(OwnedJob {
            report: report.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + Duration::from_secs(duration),
            epoch,
            simulated,
            input: None,
        });
        state.active = Some(report.id.clone());
        Ok(report)
    }
    // Cancel all pending setup capabilities operations
    pub(crate) fn cancel_all(&self) {
        let mut state = self.state.lock().unwrap();
        for job in &mut state.jobs {
            if job.report.state == "running" {
                job.cancel.store(true, Ordering::SeqCst);
                job.report.state = "cancelled".into();
                settle_waiting(job, "cancelled");
            }
        }
    }
    // Cancel the pending setup capabilities operation
    fn cancel(&self, id: &str) -> Result<(), String> {
        uuid::Uuid::parse_str(id).map_err(|_| "Invalid setup check ID")?;
        let mut state = self.state.lock().unwrap();
        let job = state
            .jobs
            .iter_mut()
            .find(|job| job.report.id == id)
            .ok_or("Setup check is no longer retained")?;
        if job.report.state == "running" {
            job.cancel.store(true, Ordering::SeqCst);
            job.report.state = "cancelled".into();
            settle_waiting(job, "cancelled");
        }
        Ok(())
    }
    // Assemble the setup capabilities report
    fn report(&self, id: &str) -> Result<SetupCapabilityJob, String> {
        uuid::Uuid::parse_str(id).map_err(|_| "Invalid setup check ID")?;
        self.state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|job| job.report.id == id)
            .map(|job| job.report.clone())
            .ok_or_else(|| "Setup check is no longer retained".into())
    }
    // Reject setup work whose retained job was cancelled or reached its deadline
    fn control(&self, id: &str) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        let job = state
            .jobs
            .iter()
            .find(|job| job.report.id == id)
            .ok_or("Setup check is no longer retained")?;
        if job.cancel.load(Ordering::SeqCst) {
            return Err("Setup check cancelled".into());
        }
        if Instant::now() >= job.deadline {
            return Err("Setup check deadline reached".into());
        }
        Ok(())
    }
    // Append an entry to setup capabilities
    fn append(&self, id: &str, value: SetupCheck) {
        let mut state = self.state.lock().unwrap();
        if let Some(job) = state
            .jobs
            .iter_mut()
            .find(|job| job.report.id == id && job.report.state == "running")
        {
            if let Some(existing) = job
                .report
                .checks
                .iter_mut()
                .find(|item| item.id == value.id)
            {
                *existing = value;
            } else if job.report.checks.len() < 79 {
                job.report.checks.push(value);
            }
        }
    }
    // Finalize the pending setup capabilities operation
    fn finish(&self, id: &str, result: Result<(), String>) {
        let mut state = self.state.lock().unwrap();
        if let Some(job) = state.jobs.iter_mut().find(|job| job.report.id == id) {
            if job.report.state == "running" {
                settle_waiting(job, "unavailable");
                job.report.state = if result.is_ok() { "complete" } else { "failed" }.into();
                if let Err(error) = result {
                    job.report.checks.push(check(
                        "incomplete",
                        "Check incomplete",
                        "unavailable",
                        error,
                    ));
                }
            }
        }
        if state.active.as_deref() == Some(id) {
            state.active = None;
        }
    }
    // Observe input
    pub(crate) fn observe_input(
        &self,
        device: &str,
        epoch: u64,
        simulated: bool,
        event: &hardware_core::ControlEvent,
    ) {
        use hardware_core::Input;
        if matches!(
            &event.input,
            Input::Button { pressed: false } | Input::Rotate { delta: 0 }
        ) || matches!(&event.input, Input::Touch { phase, .. } if phase == "up" || phase == "end" || phase == "cancel")
        {
            return;
        }
        let mut state = self.state.lock().unwrap();
        let Some(id) = state.active.clone() else {
            return;
        };
        if let Some(job) = state.jobs.iter_mut().find(|job| {
            job.report.id == id
                && job.report.state == "running"
                && job.report.request.mode == SetupCapabilityMode::DeviceInput
                && job.report.request.device_id.as_deref() == Some(device)
                && job.epoch == Some(epoch)
                && job.simulated == simulated
                && Instant::now() < job.deadline
        }) {
            job.input
                .get_or_insert_with(|| bounded(event.control_id.clone(), 128));
        }
    }
    // Handle the incoming setup capabilities input
    fn input(&self, id: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .jobs
            .iter()
            .find(|job| job.report.id == id)
            .and_then(|job| job.input.clone())
    }
}
impl Runtime {
    // Start setup capabilities
    pub fn start_setup_capabilities(
        self: &Arc<Self>,
        request: SetupCapabilityRequest,
    ) -> Result<SetupCapabilityJob, String> {
        if self.is_stopping() {
            return Err("Manager is stopping".into());
        }
        if request.plugin_ids.len() > 4
            || request
                .plugin_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 256)
            || request
                .plugin_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != request.plugin_ids.len()
        {
            return Err("Choose at most four distinct installed plugins".into());
        }
        if request.device_id.as_ref().is_some_and(|id| id.len() > 256) {
            return Err("Invalid device identity".into());
        }
        let device = {
            let state = self.state.lock().unwrap();
            if request
                .plugin_ids
                .iter()
                .any(|id| !state.plugins.iter().any(|plugin| plugin.id == *id))
            {
                return Err("A selected plugin is no longer installed".into());
            }
            request
                .device_id
                .as_ref()
                .map(|id| {
                    state
                        .devices
                        .iter()
                        .find(|device| device.id == *id && device.connected)
                        .cloned()
                        .ok_or("Selected device is no longer connected")
                })
                .transpose()?
        };
        if request.mode == SetupCapabilityMode::DeviceInput && device.is_none() {
            return Err("Choose a connected device for an input check".into());
        }
        let epoch = request
            .device_id
            .as_ref()
            .and_then(|id| self.connection_epochs.lock().unwrap().get(id).copied());
        if device.is_some() && epoch.is_none() {
            return Err("Selected device connection is changing; retry the check".into());
        }
        let report = self.setup_capabilities.reserve(
            request.clone(),
            epoch,
            device.as_ref().is_some_and(|device| device.simulated),
        )?;
        let runtime = self.clone();
        let id = report.id.clone();
        let worker = std::thread::Builder::new().name("setup-capabilities".into()).spawn(move || {
            let control = || {
                runtime.setup_capabilities.control(&id)?;
                if runtime.is_stopping() {
                    return Err("Manager is stopping".into());
                }
                if let Some(device) = &device {
                    let current_epoch = runtime.connection_epochs.lock().unwrap().get(&device.id).copied();
                    let connected = runtime
                            .state
                            .lock()
                            .unwrap()
                            .devices
                            .iter()
                            .any(|current| current.id == device.id && current.connected);
                    if current_epoch != epoch || !connected {
                        return Err(
                            "Selected device disconnected or reconnected; run a new check".into(),
                        );
                    }
                }
                Ok(())
            };
            let result = if request.mode == SetupCapabilityMode::DeviceInput {
                runtime.setup_capabilities.append(&id, check("input", "Selected device input", "waiting", "Listening for 30 seconds. A configured action may run when you press a control."));
                (|| loop {
                    control()?;
                    if let Some(input) = runtime.setup_capabilities.input(&id) {
                        runtime.setup_capabilities.append(&id, check("input", "Input observed", "observed", format!("{} input received for {input}. This does not test native action delivery or qualify the device.", if device.as_ref().is_some_and(|device| device.simulated) { "Simulator" } else { "Physical dock" })));
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                })()
            } else {
                runtime.run_setup_capabilities(&id, &request, device.as_ref(), &control)
            };
            runtime.setup_capabilities.finish(&id, result);
        });
        if let Err(error) = worker {
            self.setup_capabilities
                .finish(&report.id, Err("Setup worker could not start".into()));
            return Err(format!("Setup worker could not start: {error}"));
        }
        Ok(report)
    }
    // Read the retained setup capability report for the requested job
    pub fn setup_capability_status(&self, id: &str) -> Result<SetupCapabilityJob, String> {
        self.setup_capabilities.report(id)
    }
    // Cancel setup capabilities
    pub fn cancel_setup_capabilities(&self, id: &str) -> Result<(), String> {
        self.setup_capabilities.cancel(id)
    }
    // Run setup capabilities
    fn run_setup_capabilities(
        &self,
        id: &str,
        request: &SetupCapabilityRequest,
        device: Option<&crate::DeviceInfo>,
        control: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        control()?;
        if let Some(device) = device {
            self.setup_capabilities.append(
                id,
                check(
                    "device",
                    "Selected device",
                    "ok",
                    format!(
                        "{} · {} · {} controls. {}",
                        device.name,
                        device.model_id,
                        device.manifest.controls.len(),
                        if device.simulated {
                            "Simulator connection only."
                        } else {
                            "Connected; output and firmware behavior are not tested."
                        }
                    ),
                ),
            );
        }
        let automation = self.automation_status.lock().unwrap().clone();
        self.setup_capabilities.append(
            id,
            check(
                "foreground",
                "Foreground service",
                if automation.available {
                    "ok"
                } else {
                    "unavailable"
                },
                automation.reason,
            ),
        );
        for item in permission_checks() {
            self.setup_capabilities.append(id, item);
        }
        control()?;
        let helper = crate::media::ffmpeg_path()
            .and_then(|path| {
                stream_dock_manager_plugin_host::preflight::probe_controlled(
                    &path.to_string_lossy(),
                    &["-version"],
                    control,
                )
            })
            .and_then(|version| {
                if version.starts_with("ffmpeg version ") {
                    Ok(version)
                } else {
                    Err("Configured helper did not identify itself as FFmpeg".into())
                }
            });
        control()?;
        self.setup_capabilities.append(id, match helper { Ok(version) => check("ffmpeg", "Optional capture/media helper", "ok", version.lines().next().unwrap_or("FFmpeg responds")), Err(_) => check("ffmpeg", "Optional capture/media helper", "optional", "FFmpeg is unavailable. MP4 import and plugins using FFmpeg require a configured helper; no dependency was installed.") });
        for (plugin_index, plugin_id) in request.plugin_ids.iter().enumerate() {
            control()?;
            let (source, token) = loop {
                if let Ok(host) = self.plugins.try_lock() {
                    break (
                        host.preflight_source(plugin_id)?,
                        host.installation_token(plugin_id),
                    );
                }
                control()?;
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut report = stream_dock_manager_plugin_host::preflight::inspect_controlled(
                &source, plugin_id, control,
            )?;
            if let Some(runtime) = &report.runtime {
                let resolution = self
                    .runtimes
                    .inspect_resolution(plugin_id, &runtime.requirement);
                report
                    .checks
                    .retain(|check| check.code != "runtime_unavailable");
                report.checks.push(stream_dock_manager_plugin_host::preflight::CompatibilityCheck { code: "runtime".into(), level: if resolution.state == "ready" { "ok" } else { "unknown" }.into(), message: if resolution.state == "ready" { "A compatible private runtime is available; no interpreter was executed.".into() } else { resolution.message }, remedy: "Manage runtime versions and sources in Settings → Runtimes.".into() });
            }
            control()?;
            loop {
                if let Ok(host) = self.plugins.try_lock() {
                    if host.preflight_source(plugin_id)? != source
                        || host.installation_token(plugin_id) != token
                    {
                        return Err(
                            "Selected plugin installation changed; run the check again".into()
                        );
                    }
                    break;
                }
                control()?;
                std::thread::sleep(Duration::from_millis(20));
            }
            let blocked = report.checks.iter().any(|check| check.level == "blocker");
            let uncertain = token.is_none()
                || report
                    .checks
                    .iter()
                    .any(|check| check.level == "unknown" || check.level == "warning");
            self.setup_capabilities.append(id, check(&format!("plugin-{plugin_index}"), &report.name, if blocked { "unavailable" } else if uncertain { "unverified" } else { "ok" }, "Read-only dependency/manifest checks. Plugin code was not executed; startup behavior and permissions may still require explicit native testing."));
            for (index, item) in report.checks.iter().take(12).enumerate() {
                self.setup_capabilities.append(
                    id,
                    check(
                        &format!("plugin-{plugin_index}-{index}"),
                        &item.code,
                        &item.level,
                        format!("{} {}", item.message, item.remedy),
                    ),
                );
            }
            if report.checks.len() > 12 {
                self.setup_capabilities.append(
                    id,
                    check(
                        &format!("plugin-{plugin_index}-more"),
                        "Additional dependency details",
                        "optional",
                        "Open the plugin's compatibility review for the complete check list.",
                    ),
                );
            }
        }
        control()
    }
}
// Report platform permission readiness without prompting, injecting input or capturing pixels
fn permission_checks() -> Vec<SetupCheck> {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            fn CGPreflightScreenCaptureAccess() -> bool;
        }
        vec![check("input-permission", "Desktop input permission", if crate::automation::title_permission(false) { "ok" } else { "unavailable" }, "Accessibility permission readiness only. No keys were sent and no permission prompt was opened."), check("capture-permission", "Screen capture permission", if unsafe { CGPreflightScreenCaptureAccess() } { "ok" } else { "unavailable" }, "Screen capture permission readiness only. No pixels were captured and no permission prompt was opened.")]
    }
    #[cfg(target_os = "linux")]
    {
        vec![check("input-permission", "Desktop input service", if crate::builtins::on_wayland() && !crate::wayland::authorized() { "unavailable" } else { "unverified" }, "Wayland requires explicit desktop portal authorization; X11 requires an available desktop session. No input was injected."), check("capture-permission", "Screen capture service", "unverified", "Capture/portal consent is verified only by an explicitly invoked capture action in the current desktop session.")]
    }
    #[cfg(target_os = "windows")]
    {
        vec![check("input-permission", "Desktop input service", "unverified", "Input injection depends on the interactive desktop and target integrity level. No input was injected."), check("capture-permission", "Screen capture service", "unverified", "Capture permission depends on the target/session. Verify it through an explicitly invoked capture action; no pixels were captured.")]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        vec![check(
            "desktop-services",
            "Desktop input/capture services",
            "unavailable",
            "No supported readiness adapter for this operating system.",
        )]
    }
}

#[cfg(test)]
mod tests;
