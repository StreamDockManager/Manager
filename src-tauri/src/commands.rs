use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use stream_dock_manager_runtime::{
    workspace::{Draft, Edit},
    Runtime, Snapshot,
};
use tauri::{Emitter, Manager};
#[cfg(test)]
mod ipc_tests;
#[derive(Default)]
pub struct EditorReadiness(pub AtomicBool);
// Handle the prepare boot logo desktop command
#[tauri::command]
pub async fn prepare_boot_logo(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
    path: String,
    fit: String,
    crop: Option<[f64; 4]>,
) -> Result<stream_dock_manager_runtime::boot_logo::BootLogoPreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_boot_logo(&device_id, std::path::Path::new(&path), &fit, crop)
    })
    .await
}
// Handle the cancel boot logo desktop command
#[tauri::command]
pub async fn cancel_boot_logo(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || runtime.cancel_boot_logo(&id))
        .await
}
// Handle the commit boot logo desktop command
#[tauri::command]
pub async fn commit_boot_logo(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::boot_logo::BootLogoResult, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || runtime.commit_boot_logo(&id))
        .await
}
// Handle the frontend ready desktop command
#[tauri::command]
pub fn frontend_ready(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    generation: u64,
) -> Result<(), String> {
    crate::editor_lifecycle::ready(&app, generation)?;
    crate::manager_log::stage(crate::manager_log::Stage::FrontendReady);
    Ok(())
}

// Logging remains available when editor bootstrap/admission failed. Capabilities,
// a closed DTO, and the sink's rate limit independently bound this route.
#[tauri::command]
pub fn report_frontend_error<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    report: stream_dock_manager_plugin_host::manager_diagnostics::FrontendDiagnostic,
) -> Result<(), String> {
    crate::manager_log::frontend(window.label(), report)
}
// Handle the simulator models desktop command
#[tauri::command]
pub fn simulator_models(
    _editor_work: crate::editor_lifecycle::EditorWork,
) -> Vec<hardware_core::ModelManifest> {
    hardware_registry::manifests()
}
// Arm a scoped gesture listener without dispatching selected live input
#[expect(
    clippy::too_many_arguments,
    reason = "Preserve the existing control event test IPC argument names"
)]
#[tauri::command]
pub fn arm_control_event_test(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    control_id: String,
    event_index: usize,
    assigned: Vec<usize>,
    multi_press_ms: u16,
    hold_ms: u16,
) -> Result<stream_dock_manager_runtime::control_event_test::ControlEventTest, String> {
    runtime.arm_control_event_test(
        &device_id,
        &control_id,
        event_index,
        &assigned,
        multi_press_ms,
        hold_ms,
    )
}
// Read the current gesture test result
#[tauri::command]
pub fn read_control_event_test(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    token: String,
) -> Result<stream_dock_manager_runtime::control_event_test::ControlEventTest, String> {
    runtime.read_control_event_test(&token)
}
// Cancel only the named test; a retired token cannot cancel its replacement
#[tauri::command]
pub fn cancel_control_event_test(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    token: String,
) -> Result<(), String> {
    runtime.cancel_control_event_test(&token);
    Ok(())
}
// Handle the trigger control desktop command
#[tauri::command]
pub fn trigger_control(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    event: hardware_core::ControlEvent,
) -> Result<(), String> {
    runtime.trigger_input(&device_id, event)
}
// Handle the set device output desktop command
#[tauri::command]
pub fn set_device_output(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    output: stream_dock_manager_runtime::workspace::DeviceOutput,
) -> Result<Snapshot, String> {
    runtime.set_device_output(&device_id, output)
}

// Recorder commands belong exclusively to the native main editor capability. A document
// generation is part of ownership, so a stale page cannot append to a later session.
fn keyboard_recording_owner(work: &crate::editor_lifecycle::EditorWork) -> Result<String, String> {
    Ok(format!("main:{}", work.generation()?))
}
// Handle the keyboard macro capabilities desktop command
#[tauri::command]
pub fn keyboard_macro_capabilities(
    _editor_work: crate::editor_lifecycle::EditorWork,
) -> Result<stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroCapabilities, String>
{
    _editor_work.ensure_editable()?;
    Ok(stream_dock_manager_runtime::keyboard_macro::recorder::capabilities())
}
// Handle the start keyboard macro recording desktop command
#[tauri::command]
pub fn start_keyboard_macro_recording<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow<R>,
    editor_id: String,
    mode: stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroCaptureMode,
    initially_held: Vec<stream_dock_manager_runtime::keyboard_macro::PhysicalKey>,
) -> Result<stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroSession, String> {
    _editor_work.ensure_editable()?;
    if window.label() != "main"
        || !window
            .is_focused()
            .map_err(|_| "Cannot verify editor focus")?
    {
        return Err("Focus the Manager recording area before recording".into());
    }
    stream_dock_manager_runtime::keyboard_macro::recorder::start(
        &keyboard_recording_owner(&_editor_work)?,
        &editor_id,
        mode,
        initially_held,
    )
}
// Handle the append keyboard macro events desktop command
#[tauri::command]
pub fn append_keyboard_macro_events<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow<R>,
    session_id: String,
    events: Vec<stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroCaptureEvent>,
) -> Result<stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroSession, String> {
    let owner = keyboard_recording_owner(&_editor_work)?;
    if window.label() != "main"
        || !window
            .is_focused()
            .map_err(|_| "Cannot verify editor focus")?
    {
        stream_dock_manager_runtime::keyboard_macro::recorder::cancel_owner(&owner);
        return Err("Recording stopped because the editor lost focus".into());
    }
    stream_dock_manager_runtime::keyboard_macro::recorder::append(&owner, &session_id, events)
}
// Handle the heartbeat keyboard macro recording desktop command
#[tauri::command]
pub fn heartbeat_keyboard_macro_recording<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow<R>,
    session_id: String,
) -> Result<stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroSession, String> {
    stream_dock_manager_runtime::keyboard_macro::recorder::heartbeat(
        &keyboard_recording_owner(&_editor_work)?,
        &session_id,
        window.label() == "main" && window.is_focused().unwrap_or(false),
    )
}
// Handle the stop keyboard macro recording desktop command
#[tauri::command]
pub async fn stop_keyboard_macro_recording(
    _editor_work: crate::editor_lifecycle::EditorWork,
    session_id: String,
) -> Result<
    stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroRecordingResult,
    String,
> {
    stream_dock_manager_runtime::keyboard_macro::recorder::stop(
        &keyboard_recording_owner(&_editor_work)?,
        &session_id,
    )
    .await
}
// Handle the cancel keyboard macro recording desktop command
#[tauri::command]
pub async fn cancel_keyboard_macro_recording(
    _editor_work: crate::editor_lifecycle::EditorWork,
    session_id: String,
) -> Result<(), String> {
    stream_dock_manager_runtime::keyboard_macro::recorder::cancel(
        &keyboard_recording_owner(&_editor_work)?,
        &session_id,
    )
    .await
}
// Handle the begin keyboard macro test desktop command

#[tauri::command]
pub fn begin_keyboard_macro_test<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow<R>,
    editor_id: String,
) -> Result<
    stream_dock_manager_runtime::keyboard_macro::test_session::KeyboardMacroTestSession,
    String,
> {
    _editor_work.ensure_editable()?;
    if window.label() != "main" || !window.is_focused().unwrap_or(false) {
        return Err("Focus Manager before choosing a scratch target".into());
    }
    stream_dock_manager_runtime::keyboard_macro::test_session::begin(
        &keyboard_recording_owner(&_editor_work)?,
        &editor_id,
    )
}
// Handle the start keyboard macro test desktop command
#[tauri::command]
pub fn start_keyboard_macro_test<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow<R>,
    session_id: String,
    recording: stream_dock_manager_runtime::keyboard_macro::KeyboardMacroRecording,
    playback: stream_dock_manager_runtime::keyboard_macro::KeyboardMacroPlayback,
) -> Result<
    stream_dock_manager_runtime::keyboard_macro::test_session::KeyboardMacroTestSession,
    String,
> {
    _editor_work.ensure_editable()?;
    if window.label() != "main" || !window.is_focused().unwrap_or(false) {
        return Err("Focus Manager to confirm the selected scratch target".into());
    }
    stream_dock_manager_runtime::keyboard_macro::test_session::start(
        &keyboard_recording_owner(&_editor_work)?,
        &session_id,
        recording,
        playback,
    )
}
// Handle the keyboard macro test status desktop command
#[tauri::command]
pub fn keyboard_macro_test_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    session_id: String,
) -> Result<
    stream_dock_manager_runtime::keyboard_macro::test_session::KeyboardMacroTestSession,
    String,
> {
    stream_dock_manager_runtime::keyboard_macro::test_session::status(
        &keyboard_recording_owner(&_editor_work)?,
        &session_id,
    )
}
// Handle the cancel keyboard macro test desktop command
#[tauri::command]
pub fn cancel_keyboard_macro_test(
    _editor_work: crate::editor_lifecycle::EditorWork,
    session_id: String,
) -> Result<
    Option<stream_dock_manager_runtime::keyboard_macro::test_session::KeyboardMacroTestSession>,
    String,
> {
    stream_dock_manager_runtime::keyboard_macro::test_session::cancel(
        &keyboard_recording_owner(&_editor_work)?,
        &session_id,
    )
}
// Handle the keyboard device settings desktop command

#[tauri::command]
pub fn keyboard_device_settings(
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> stream_dock_manager_runtime::keyboard_device::KeyboardDeviceSettings {
    runtime.keyboard_device_settings(&device_id)
}
// Handle the factory reset keyboard desktop command
#[tauri::command]
pub async fn factory_reset_keyboard(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::keyboard::KeyboardApplyResult, String> {
    _editor_work.ensure_editable()?;
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.factory_reset_keyboard(&device_id)
    })
    .await
}
// Handle the apply keyboard settings desktop command

#[tauri::command]
pub async fn apply_keyboard_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
    settings: hardware_core::KeyboardSettings,
) -> Result<stream_dock_manager_runtime::keyboard::KeyboardApplyResult, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.apply_keyboard_settings(&device_id, settings)
    })
    .await
}
// Handle the preview keyboard settings desktop command

#[tauri::command]
pub async fn preview_keyboard_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
    preview_id: Option<String>,
    settings: hardware_core::KeyboardSettings,
) -> Result<stream_dock_manager_runtime::keyboard_preview::KeyboardPreviewResult, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.preview_keyboard_settings(&device_id, preview_id.as_deref(), settings)
    })
    .await
}
// Handle the finish keyboard preview desktop command
#[tauri::command]
pub async fn finish_keyboard_preview(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
    preview_id: String,
    apply: bool,
    settings: Option<hardware_core::KeyboardSettings>,
) -> Result<stream_dock_manager_runtime::keyboard::KeyboardApplyResult, String> {
    if apply {
        _editor_work.ensure_editable()?;
    }
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.finish_keyboard_preview(&device_id, &preview_id, apply, settings)
    })
    .await
}
// Handle the set device sleep desktop command

#[tauri::command]
pub fn set_device_sleep(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    sleeping: bool,
) -> Result<Snapshot, String> {
    runtime.set_device_sleep(&device_id, sleeping)
}
// Handle the device power status desktop command
#[tauri::command]
pub fn device_power_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::power_policy::DevicePowerStatus, String> {
    runtime.device_power_status(&device_id)
}
// Handle the device power settings desktop command
#[tauri::command]
pub fn device_power_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::power_policy::DevicePowerSettings, String> {
    runtime.device_power_settings(&device_id)
}
// Handle the save device power policy desktop command
#[tauri::command]
pub fn save_device_power_policy(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    device_key: String,
    expected_revision: u64,
    policy: stream_dock_manager_runtime::power_policy::DevicePowerPolicy,
) -> Result<stream_dock_manager_runtime::power_policy::DevicePowerSettings, String> {
    runtime.save_device_power_policy(&device_id, &device_key, expected_revision, policy)
}
// Handle the finish window action desktop command

#[tauri::command]
pub async fn finish_window_action(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    quit: bool,
    generation: u64,
    action_id: u64,
) -> Result<(), String> {
    if !quit {
        return crate::editor_lifecycle::finish_close(&app, generation, action_id).await;
    }
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::on_main_thread(&app, move |app| {
        app.state::<Arc<crate::editor_lifecycle::EditorLifecycle>>()
            .finish_action(generation, action_id, quit)?;
        runtime.cancel_debug_sessions();
        crate::editor_lifecycle::complete_quit(&app);
        Ok(())
    })
    .await
}
// Handle the apply edits desktop command

#[tauri::command]
pub fn apply_edits(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    expected_revision: u64,
    description: String,
    edits: Vec<Edit>,
    device_id: Option<String>,
) -> Result<Snapshot, String> {
    runtime.apply_workspace_edits_for_device(
        expected_revision,
        &description,
        edits,
        device_id.as_deref(),
    )?;
    Ok(runtime.snapshot())
}
// Handle the apply workspace edits desktop command
#[tauri::command]
pub fn apply_workspace_edits(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    expected_revision: u64,
    description: String,
    edits: Vec<Edit>,
    device_id: Option<String>,
) -> Result<stream_dock_manager_runtime::workspace_stream::WorkspaceBatch, String> {
    runtime.apply_workspace_edits_for_device(
        expected_revision,
        &description,
        edits,
        device_id.as_deref(),
    )
}
// Handle the subscribe workspace desktop command
#[tauri::command]
pub fn subscribe_workspace(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    on_batch: tauri::ipc::Channel<stream_dock_manager_runtime::workspace_stream::WorkspaceBatch>,
) -> String {
    let runtime = runtime.inner().clone();
    let id = uuid_id();
    let cancel = Arc::new(AtomicBool::new(false));
    runtime
        .workspace_cancellations
        .lock()
        .unwrap()
        .insert(id.clone(), cancel.clone());
    let result = id.clone();
    let events = runtime.subscribe_workspace();
    std::thread::spawn(move || {
        let _editor_work = _editor_work;
        let mut last = None;
        while !cancel.load(Ordering::SeqCst) && !runtime.is_stopping() {
            match events.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(batch) => {
                    runtime.metrics.record(
                        "workspace_bytes",
                        serde_json::to_vec(batch.as_ref())
                            .map(|v| v.len())
                            .unwrap_or(0) as f64,
                    );
                    if on_batch.send(batch.as_ref().clone()).is_err() {
                        break;
                    }
                    last = Some((
                        batch.generation.clone(),
                        batch.sequence,
                        batch.revision,
                        batch.configuration_revision,
                    ));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some((generation, sequence, revision, configuration_revision)) = last {
                        let _ = on_batch.send(
                            stream_dock_manager_runtime::workspace_stream::WorkspaceBatch {
                                generation,
                                sequence,
                                revision,
                                configuration_revision,
                                reset: None,
                                reset_required: true,
                                changes: Default::default(),
                            },
                        );
                    }
                    break;
                }
            }
        }
        runtime.workspace_cancellations.lock().unwrap().remove(&id);
    });
    result
}
// Handle the unsubscribe workspace desktop command
#[tauri::command]
pub fn unsubscribe_workspace(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) {
    if let Some(cancel) = runtime.workspace_cancellations.lock().unwrap().remove(&id) {
        cancel.store(true, Ordering::SeqCst);
    }
}
// Handle the capture binding desktop command

#[tauri::command]
pub fn capture_binding(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    control_id: String,
) -> Result<stream_dock_manager_runtime::Binding, String> {
    runtime.capture_binding(&device_id, &control_id)
}
// Handle the history action desktop command
#[tauri::command]
pub fn history_action(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    expected_revision: u64,
    operation: String,
) -> Result<Snapshot, String> {
    runtime.history_action(&operation, expected_revision)
}
// Handle the select page desktop command
#[tauri::command]
pub fn select_page(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    page_id: String,
) -> Result<Snapshot, String> {
    let session = runtime.snapshot().session(&device_id);
    runtime.select_device_profile(&device_id, &session.profile_id, Some(&page_id), false)
}
// Handle the save automation configuration desktop command
#[tauri::command]
pub fn save_automation_configuration(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    request: stream_dock_manager_runtime::automation::AutomationConfigurationDraft,
) -> Result<Snapshot, String> {
    runtime.save_automation_configuration(request)
}
// Handle the set automation desktop command
#[tauri::command]
pub fn set_automation(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    enabled: bool,
    paused: bool,
) -> Result<Snapshot, String> {
    runtime.set_automation(&device_id, enabled, paused)
}
// Handle the resolve automation application desktop command
#[tauri::command]
pub fn resolve_automation_application(
    _editor_work: crate::editor_lifecycle::EditorWork,
    path: String,
) -> Result<String, String> {
    stream_dock_manager_runtime::automation_application::resolve(&path)
}
// Handle the automation status desktop command
#[tauri::command]
pub fn automation_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> stream_dock_manager_runtime::automation::AutomationStatus {
    runtime.automation_status.lock().unwrap().clone()
}
// Handle the drafts desktop command
#[tauri::command]
pub fn drafts(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> std::collections::BTreeMap<String, Draft> {
    runtime.drafts.lock().unwrap().clone()
}
// Handle the save draft desktop command
#[tauri::command]
pub fn save_draft(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    key: String,
    draft: Option<Draft>,
) -> Result<(), String> {
    runtime.save_draft(&key, draft)
}
// Handle the performance desktop command
#[tauri::command]
pub async fn performance(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::metrics::PerformanceReport, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || report(&runtime))
        .await
        .map_err(|error| error.to_string())?
}
// Handle the report desktop command
fn report(
    runtime: &Runtime,
) -> Result<stream_dock_manager_runtime::metrics::PerformanceReport, String> {
    let mut plugins = runtime.snapshot().plugins;
    // Reports contain structured status only, never raw plugin logs, arguments, or settings
    for plugin in &mut plugins {
        plugin.health.recent_failures.clear();
        plugin.health.missing_assets.clear();
        plugin.health.unsupported_apis.clear();
    }
    let evidence: serde_json::Value =
        serde_json::from_str(include_str!("../../public/plugin-compatibility.json"))
            .unwrap_or_default();
    let (manager, combined) = runtime.resource_usage();
    Ok(stream_dock_manager_runtime::metrics::PerformanceReport {
        manager,
        combined,
        window_seconds: 300,
        metrics: runtime.metrics.snapshot(),
        cache: serde_json::from_value(runtime.cache_stats()).map_err(|e| e.to_string())?,
        reference_compatibility: evidence,
        runtimes: runtime.runtimes.status().installations.iter().map(|i| serde_json::json!({"kind":i.kind,"version":i.version,"channel":i.channel,"source":i.source,"verification":i.verification})).collect(),
        plugins: plugins.iter().map(|p| serde_json::json!({"id":p.id,"version":p.version,"kind":p.kind,"enabled":p.enabled,"restart_attempts":p.health.restart_attempts})).collect(),
    })
}
// Handle the preview diagnostics desktop command
#[tauri::command]
pub async fn preview_diagnostics(
    app: tauri::AppHandle,
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    let browser_windows = crate::browser_inventory::collect(&app, &runtime);
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let mut environment = crate::diagnostic_export::environment();
        environment["browser_windows"] = browser_windows;
        crate::diagnostic_export::stage(
            environment,
            serde_json::to_value(report(&runtime)?).map_err(|e| e.to_string())?,
        )
    })
    .await
    .map_err(|e| e.to_string())?
}
// Handle the discard diagnostics desktop command
#[tauri::command]
pub fn discard_diagnostics(_editor_work: crate::editor_lifecycle::EditorWork, report_id: String) {
    crate::diagnostic_export::discard(&report_id);
}
// Handle the export diagnostics desktop command
#[tauri::command]
pub fn export_diagnostics(
    _editor_work: crate::editor_lifecycle::EditorWork,
    path: String,
    report_id: String,
) -> Result<(), String> {
    let document = crate::diagnostic_export::reviewed(&report_id)?;
    std::fs::write(path, document).map_err(|e| e.to_string())?;
    crate::diagnostic_export::discard(&report_id);
    Ok(())
}
// Handle the firmware status desktop command
#[tauri::command]
pub fn firmware_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::firmware::FirmwareStatus, String> {
    runtime.firmware_status(&device_id)
}
// Handle the check firmware desktop command
#[tauri::command]
pub async fn check_firmware(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::firmware::FirmwareStatus, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.check_firmware(&device_id)
    })
    .await
}
// Read cached firmware metadata away from the desktop event loop
#[tauri::command]
pub async fn firmware_catalog_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::firmware_catalog::FirmwareCatalogReport, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.firmware_catalog_status(&device_id)
    })
    .await
}
// Refresh bounded firmware metadata away from the desktop event loop
#[tauri::command]
pub async fn refresh_firmware_catalog(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::firmware_catalog::FirmwareCatalogReport, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.refresh_firmware_catalog(&device_id)
    })
    .await
}
// Persist the explicit test-catalog preference away from the desktop event loop
#[tauri::command]
pub async fn set_firmware_catalog_test(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    enabled: bool,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.set_firmware_catalog_test(enabled)
    })
    .await
}
// Handle the open firmware instructions desktop command
#[tauri::command]
pub async fn open_firmware_instructions(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.open_firmware_instructions(&device_id)
    })
    .await
}
// Handle the media preview desktop command
#[expect(
    clippy::too_many_arguments,
    reason = "Preserve the named desktop IPC arguments and editor lifecycle guard"
)]
#[tauri::command]
pub async fn media_preview(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
    device_id: String,
    control_id: Option<String>,
    options: stream_dock_manager_runtime::media::MediaOptions,
    source: Option<bool>,
    preview_bounds: Option<[u16; 2]>,
) -> Result<tauri::ipc::Response, String> {
    static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    struct Permit;
    impl Drop for Permit {
        // Handle the drop desktop command
        fn drop(&mut self) {
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
    let mut active = ACTIVE.load(Ordering::Acquire);
    loop {
        if active >= 2 {
            return Err("Preview renderer is busy; retry the latest frame".into());
        }
        match ACTIVE.compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => break,
            Err(current) => active = current,
        }
    }
    let permit = Permit;
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        let _permit = permit;
        if source.unwrap_or_default() {
            let bytes = runtime.render_artwork_source_preview(&asset, &options)?;
            let details = runtime.media_details(&asset)?;
            return media_preview_response(bytes, serde_json::json!({
                "mime": "image/png", "animated": details["animated"],
                "max_fps": 24, "duration_ms": details["duration_ms"], "requested_tps": 0
            }));
        }
        options.validate()?;
        let state = runtime.snapshot();
        let device = state
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or("Device missing")?;
        let size = if let Some(id) = &control_id {
            let control = device.manifest.control(id).map_err(|e| e.to_string())?;
            [
                control.image_width.ok_or("No image surface")?,
                control.image_height.ok_or("No image surface")?,
            ]
        } else {
            device
                .manifest
                .capabilities
                .background_size
                .ok_or("No background surface")?
        };
        let size = stream_dock_manager_runtime::media::display_preview_size(size, preview_bounds)?;
        let bytes = runtime.render_media_preview(
            &asset,
            size,
            &options,
            &device.manifest.capabilities.image_formats,
        )?;
        let details = runtime.media_details(&asset)?;
        let session = state.session(&device_id);
        let profile = state.profiles.iter().find(|p| p.id == session.profile_id);
        let mut requested = if details["animated"] == true {
            details["source_fps"].as_f64().unwrap_or(24.0).min(options.maximum_fps().unwrap_or(f64::INFINITY))
        } else {
            0.0
        };
        if let Some(profile) = profile {
            if let Some(page) = profile.pages.iter().find(|p| p.id == session.page_id) {
                for (id, binding) in &page.bindings {
                    if control_id.as_ref() == Some(id) {
                        continue;
                    }
                    if let Some(asset) = &binding.asset {
                        if runtime.media_details(asset)?["animated"] == true {
                            requested += runtime.media_details(asset)?["source_fps"].as_f64().unwrap_or(24.0).mul_add(binding.media.speed, 0.0).min(binding.media.maximum_fps().unwrap_or(f64::INFINITY));
                        }
                    }
                }
            }
            if control_id.is_some() {
                if let Some(asset) = &profile.background_asset {
                    if runtime.media_details(asset)?["animated"] == true {
                        requested += runtime.media_details(asset)?["source_fps"].as_f64().unwrap_or(24.0).mul_add(profile.background_media.speed, 0.0).min(profile.background_media.maximum_fps().unwrap_or(f64::INFINITY));
                    }
                }
            }
        }
        let metadata = serde_json::json!({
            "mime": if bytes.starts_with(&[0xff, 0xd8]) { "image/jpeg" } else { "image/png" },
            "max_fps": device.manifest.capabilities.max_fps, "animated": details["animated"],
            "duration_ms": details["duration_ms"], "requested_tps": requested, "format": details["format"], "source_fps": details["source_fps"]
        });
        media_preview_response(bytes, metadata)
    }).await
}
// Handle the media preview response desktop command
fn media_preview_response(
    bytes: Vec<u8>,
    metadata: serde_json::Value,
) -> Result<tauri::ipc::Response, String> {
    let metadata = serde_json::to_vec(&metadata).map_err(|e| e.to_string())?;
    let mut response = Vec::with_capacity(4 + metadata.len() + bytes.len());
    response.extend((metadata.len() as u32).to_le_bytes());
    response.extend(metadata);
    response.extend(bytes);
    Ok(tauri::ipc::Response::new(response))
}
// Handle the subscribe surfaces desktop command
#[tauri::command]
pub fn subscribe_surfaces(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    on_frame: tauri::ipc::Channel<tauri::ipc::Response>,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    let id = uuid_id();
    let subscription = runtime
        .preview_subscriptions
        .register_device(id.clone(), Some(device_id.clone()));
    let result = id.clone();
    std::thread::spawn(move || {
        let _editor_work = _editor_work;
        let worker = subscription;
        let mut preview =
            stream_dock_manager_runtime::preview::SurfacePreviewStream::for_browser(id.clone());
        let mut packet_sequence = 0u64;
        let mut next_frame = std::time::Instant::now();
        while worker.wait_for_frame(next_frame) && !runtime.is_stopping() {
            if let Some((delta, frames)) = preview.render_next(&runtime, &device_id) {
                packet_sequence += 1;
                let metadata = serde_json::to_vec(&serde_json::json!({"acknowledgement":{"generation":id,"sequence":packet_sequence},"delta":delta,"frames":frames.iter().map(|(id, bytes)| serde_json::json!({"id":id,"length":bytes.len(),"mime":if bytes.starts_with(&[0xff,0xd8]) {"image/jpeg"} else {"image/png"}})).collect::<Vec<_>>()})).unwrap();
                let packet_size =
                    4 + metadata.len() + frames.values().map(|bytes| bytes.len()).sum::<usize>();
                if metadata.len() > 65536 || packet_size > 32 * 1024 * 1024 {
                    runtime.metrics.record("preview_packet_limit", 1.0);
                    break;
                }
                let mut response = Vec::with_capacity(packet_size);
                response.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
                response.extend_from_slice(&metadata);
                for (_, bytes) in frames {
                    response.extend_from_slice(&bytes);
                }
                runtime
                    .metrics
                    .record("preview_bytes", response.len() as f64);
                worker.sent(&id, packet_sequence);
                if on_frame.send(tauri::ipc::Response::new(response)).is_err() {
                    break;
                }
            }
            worker.refresh(preview.needs_refresh());
            next_frame = stream_dock_manager_runtime::media::advance_deadline(
                next_frame,
                std::time::Instant::now(),
                preview.interval(),
            );
        }
    });
    Ok(result)
}
// Handle the acknowledge surface frame desktop command
#[tauri::command]
pub fn acknowledge_surface_frame(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
    generation: String,
    sequence: u64,
) {
    runtime
        .preview_subscriptions
        .acknowledge(&id, &generation, sequence);
}
// Handle the uuid ID desktop command
fn uuid_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("preview-{}", NEXT.fetch_add(1, Ordering::SeqCst))
}
// Handle the unsubscribe surfaces desktop command
#[tauri::command]
pub fn unsubscribe_surfaces(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) {
    runtime.preview_subscriptions.cancel(&id);
}
// Handle the setup status desktop command

#[tauri::command]
pub async fn setup_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::onboarding::SetupReport, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || runtime.setup_report())
        .await
        .map_err(|e| e.to_string())
}
// Handle the start setup capabilities desktop command
#[tauri::command]
pub fn start_setup_capabilities(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    request: stream_dock_manager_runtime::setup_capabilities::SetupCapabilityRequest,
) -> Result<stream_dock_manager_runtime::setup_capabilities::SetupCapabilityJob, String> {
    runtime.inner().start_setup_capabilities(request)
}
// Handle the setup capability status desktop command
#[tauri::command]
pub fn setup_capability_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::setup_capabilities::SetupCapabilityJob, String> {
    runtime.setup_capability_status(&id)
}
// Handle the cancel setup capabilities desktop command
#[tauri::command]
pub fn cancel_setup_capabilities(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    runtime.cancel_setup_capabilities(&id)
}
// Handle the save setup progress desktop command
#[tauri::command]
pub fn save_setup_progress(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    progress: stream_dock_manager_runtime::onboarding::SetupProgress,
) -> Result<(), String> {
    runtime.save_setup_progress(progress)
}
// Handle the authorize accessibility desktop command
#[tauri::command]
pub fn authorize_accessibility(_editor_work: crate::editor_lifecycle::EditorWork) -> bool {
    stream_dock_manager_runtime::automation::title_permission(true)
}
#[tauri::command]
pub fn arm_input_trace(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
) -> Result<stream_dock_manager_runtime::input_trace::InputTrace, String> {
    runtime.arm_input_trace(&device_id)
}
#[tauri::command]
pub fn read_input_trace(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    token: String,
) -> Result<stream_dock_manager_runtime::input_trace::InputTrace, String> {
    runtime.read_input_trace(&token)
}
#[tauri::command]
pub fn dismiss_input_trace(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    token: String,
) {
    runtime.dismiss_input_trace(&token)
}

// Handle the evaluate rules desktop command
#[tauri::command]
pub fn evaluate_rules(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
) -> Vec<stream_dock_manager_runtime::automation::RuleEvaluation> {
    stream_dock_manager_runtime::automation::preview(&runtime, &device_id)
}
#[tauri::command]
pub fn test_automation_rules(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    request: stream_dock_manager_runtime::automation::RuleTest,
) -> Result<stream_dock_manager_runtime::automation::RuleTestResult, String> {
    stream_dock_manager_runtime::automation::test_rules(&runtime.snapshot(), request)
}

// Handle the preview binding desktop command
#[tauri::command]
pub async fn preview_binding(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
    control_id: String,
    binding: stream_dock_manager_runtime::Binding,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime
            .preview_binding_details(&device_id, &control_id, &binding, None)
            .map(|preview| preview.image)
    })
    .await
}
// Handle the system fonts desktop command

#[tauri::command]
pub async fn system_fonts(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    refresh: Option<bool>,
) -> Result<Vec<String>, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.system_fonts(refresh.unwrap_or(false))
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the preview binding details desktop command
#[tauri::command]
pub async fn preview_binding_details(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
    control_id: String,
    binding: stream_dock_manager_runtime::Binding,
    preview_state: Option<u32>,
) -> Result<stream_dock_manager_runtime::preview::BindingPreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.preview_binding_details(&device_id, &control_id, &binding, preview_state)
    })
    .await
}
// Handle the preflight plugin desktop command
#[tauri::command]
pub async fn preflight_plugin(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: Option<String>,
    path: Option<String>,
) -> Result<stream_dock_manager_plugin_host::preflight::PluginPreflight, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        if let Some(path) = path {
            stream_dock_manager_plugin_host::preflight::inspect_source(std::path::Path::new(&path))
                .map(|report| {
                    stream_dock_manager_plugin_host::preflight::resolve_runtime(
                        report,
                        &runtime.runtimes,
                    )
                })
        } else {
            runtime
                .plugins
                .lock()
                .unwrap()
                .preflight(&id.ok_or("Choose a plugin")?)
        }
    })
    .await
}
// Handle the scan plugin sources desktop command
#[tauri::command]
pub async fn scan_plugin_sources(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    roots: Vec<String>,
    source: Option<String>,
) -> Result<Vec<stream_dock_manager_plugin_host::preflight::PluginPreflight>, String> {
    if roots.len() > 20 {
        return Err("Too many source directories".into());
    }
    let source = source.as_deref().unwrap_or("all");
    let defaults = stream_dock_manager_plugin_host::preflight::sources_for(source)?;
    let roots = if roots.is_empty() {
        defaults
    } else {
        roots.into_iter().map(std::path::PathBuf::from).collect()
    };
    if roots.is_empty() {
        return Ok(vec![]);
    }
    let installed = runtime.snapshot().plugins;
    let service = runtime.runtimes.clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        stream_dock_manager_plugin_host::preflight::discover(roots, &installed)
            .into_iter()
            .map(|report| {
                stream_dock_manager_plugin_host::preflight::resolve_runtime(report, &service)
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the import plugin selection desktop command
#[tauri::command]
pub async fn import_plugin_selection(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    paths: Vec<String>,
) -> Result<stream_dock_manager_runtime::plugin_packages::PluginSelectionReview, String> {
    if paths.is_empty() || paths.len() > 4 {
        return Err("Select one to four plugins per review".into());
    }
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let mut reviews = vec![];
        let mut results = vec![];
        for path in paths {
            match runtime.prepare_plugin_package(std::path::Path::new(&path)) {
                Ok(review) => reviews.push(review),
                Err(error) => results.push(
                    stream_dock_manager_runtime::plugin_packages::PluginSelectionFailure {
                        source: path,
                        status: "failed".into(),
                        message: error,
                    },
                ),
            }
        }
        stream_dock_manager_runtime::plugin_packages::PluginSelectionReview { reviews, results }
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the pending vendor profile reviews desktop command

#[tauri::command]
pub async fn pending_vendor_profile_reviews(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::vendor_profiles::VendorProfileReviews, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.pending_vendor_profile_reviews()
    })
    .await
}
// Handle the cancel vendor profile review desktop command
#[tauri::command]
pub async fn cancel_vendor_profile_review(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<Snapshot, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.resolve_vendor_profile_review(&id, false)
    })
    .await
}
// Handle the resolve vendor profile review desktop command
#[tauri::command]
pub async fn resolve_vendor_profile_review(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    accept: bool,
) -> Result<Snapshot, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.resolve_vendor_profile_review(&id, accept)
    })
    .await
}
// Handle the list plugin profiles desktop command

#[tauri::command]
pub async fn list_plugin_profiles(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::plugin_profiles::PluginProfileCatalog, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.list_plugin_profiles(&id)
    })
    .await
}
// Handle the pending plugin profile setup desktop command
#[tauri::command]
pub async fn pending_plugin_profile_setup(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    ids: Vec<String>,
) -> Result<Vec<stream_dock_manager_runtime::plugin_profiles::PluginProfileCatalog>, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.pending_plugin_profile_setup(ids)
    })
    .await
}
// Handle the acknowledge plugin profile setup desktop command
#[tauri::command]
pub async fn acknowledge_plugin_profile_setup(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    fingerprint: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.acknowledge_plugin_profile_setup(&id, &fingerprint)
    })
    .await
}
// Handle the prepare plugin profile import desktop command
#[tauri::command]
pub async fn prepare_plugin_profile_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    key: String,
    target_profile: Option<String>,
    target_page: Option<String>,
    target_device: Option<String>,
) -> Result<stream_dock_manager_runtime::importing::ImportPreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_plugin_profile_import_for_device(
            &id,
            &key,
            target_profile,
            target_page,
            target_device,
        )
    })
    .await
}
// Handle the prepare profile import desktop command
#[tauri::command]
pub async fn prepare_profile_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
    password: Option<String>,
) -> Result<stream_dock_manager_runtime::importing::ImportPreview, String> {
    let password = password.map(zeroize::Zeroizing::new);
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        stream_dock_manager_runtime::importing::read_transfer(
            std::path::Path::new(&path),
            password.as_ref().map(|s| s.as_str()),
            |p| runtime.prepare_profile_import(p),
        )
    })
    .await
}
// Handle the update profile import desktop command
#[tauri::command]
pub async fn update_profile_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    mode: String,
    repairs: Vec<stream_dock_manager_runtime::importing::ImportRepair>,
    placement: Option<stream_dock_manager_runtime::plugin_profiles::PluginProfilePlacement>,
    remember: Option<stream_dock_manager_runtime::import_recipes::RememberImportRepair>,
) -> Result<stream_dock_manager_runtime::importing::ImportPreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        if let Some(placement) = placement {
            return runtime.update_plugin_profile_import(&id, placement);
        }
        runtime.update_profile_import_remembered(&id, &mode, repairs, remember)
    })
    .await
}
// Handle the forget import recipe desktop command
#[tauri::command]
pub async fn forget_import_recipe(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.forget_import_recipe(&id)
    })
    .await
}
// Handle the cancel profile import desktop command
#[tauri::command]
pub fn cancel_profile_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) {
    runtime.cancel_profile_import(&id)
}
// Handle the commit profile import desktop command
#[tauri::command]
pub async fn commit_profile_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    expected_revision: u64,
) -> Result<Snapshot, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.commit_profile_import(&id, expected_revision)
    })
    .await
}
// Handle the update status desktop command

#[tauri::command]
pub fn update_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> stream_dock_manager_runtime::updates::UpdateStatus {
    runtime.update_status()
}
// Handle the save update preferences desktop command
#[tauri::command]
pub fn save_update_preferences(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    preferences: stream_dock_manager_runtime::updates::UpdatePreferences,
) -> Result<(), String> {
    runtime.updates.preferences(preferences)
}
// Handle the plugin github credential status desktop command
#[tauri::command]
pub fn plugin_github_credential_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    repository: String,
) -> Result<stream_dock_manager_runtime::credentials::CredentialState, String> {
    runtime.updates.plugin_github_credential_status(&repository)
}
// Handle the save plugin github token desktop command
#[tauri::command]
pub fn save_plugin_github_token(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    repository: String,
    token: String,
) -> Result<(), String> {
    runtime
        .updates
        .save_plugin_github_token(&repository, &token)
}
// Handle the clear plugin github token desktop command
#[tauri::command]
pub fn clear_plugin_github_token(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    repository: String,
) -> Result<(), String> {
    runtime.updates.clear_plugin_github_token(&repository)
}
// Handle the check updates desktop command

#[tauri::command]
pub fn check_updates(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    product: String,
) -> Result<String, String> {
    runtime.updates.check(&product)
}
// Handle the download release asset desktop command
#[tauri::command]
pub fn download_release_asset(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    product: String,
    name: String,
    channel: Option<String>,
    unified: Option<bool>,
) -> Result<String, String> {
    if product == "plugins" {
        return runtime.updates.download_plugin_variant(
            &name,
            channel.as_deref().unwrap_or("stable"),
            unified.unwrap_or(false),
        );
    }
    runtime
        .updates
        .download(&product, &name, channel.as_deref())
}
// Handle the cancel update job desktop command
#[tauri::command]
pub fn cancel_update_job(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    runtime.updates.cancel(&id)
}
// Handle the retry update job desktop command
#[tauri::command]
pub fn retry_update_job(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<String, String> {
    runtime.updates.retry_download(&id)
}
// Handle the show update download desktop command
#[tauri::command]
pub fn show_update_download(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    runtime.show_update_download(&id)
}
// Handle the open project link desktop command
#[tauri::command]
pub fn open_project_link(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    kind: stream_dock_manager_runtime::updates::ProjectLink,
) -> Result<(), String> {
    runtime.open_project_link(kind)
}
// Handle the open manager release link desktop command
#[tauri::command]
pub fn open_manager_release_link(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    kind: stream_dock_manager_runtime::updates::ManagerReleaseLink,
) -> Result<(), String> {
    runtime.open_manager_release_link(kind)
}
// Handle the open plugin documentation desktop command
#[tauri::command]
pub fn open_plugin_documentation(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    request: stream_dock_manager_runtime::updates::PluginDocumentationRequest,
) -> Result<(), String> {
    runtime.open_plugin_documentation(&request)
}
// Handle the prepare plugin package desktop command
#[tauri::command]
pub async fn prepare_plugin_package(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
) -> Result<stream_dock_manager_runtime::plugin_packages::PluginPackageReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_plugin_package(std::path::Path::new(&path))
    })
    .await
}
// Handle the prepare catalog download desktop command
#[tauri::command]
pub async fn prepare_catalog_download(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::plugin_packages::PluginPackageReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_catalog_download(&id)
    })
    .await
}
// Handle the commit plugin package desktop command
#[expect(
    clippy::too_many_arguments,
    reason = "Preserve the existing package review IPC argument names"
)]
#[tauri::command]
pub async fn commit_plugin_package(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    allow_channel_change: bool,
    replace: bool,
    force_reinstall: Option<bool>,
    approvals: Option<Vec<stream_dock_manager_plugin_host::permissions::ActionScope>>,
) -> Result<Snapshot, String> {
    let old_views: Vec<_> = {
        let host = runtime.plugins.lock().unwrap();
        host.list()
            .iter()
            .map(|p| (p.id.clone(), host.view_labels(&p.id).unwrap_or_default()))
            .collect()
    };
    let owned = runtime.inner().clone();
    let editor = _editor_work.generation()?;
    let result = crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        owned.commit_reviewed_plugin_package(
            &id,
            allow_channel_change,
            replace,
            Some(editor),
            approvals.as_deref().unwrap_or_default(),
            force_reinstall.unwrap_or(false),
        )
    })
    .await
    .map_err(|e| e.to_string())?;
    for (id, labels) in old_views {
        let (current, view) = {
            let host = runtime.plugins.lock().unwrap();
            (
                host.view_labels(&id).unwrap_or_default(),
                host.browser_view(&id).ok().flatten(),
            )
        };
        for label in &labels {
            if !current.contains(label) {
                if let Some(window) = app.get_webview_window(label) {
                    let _ = window.close();
                }
            }
        }
        if let Some(view) = view {
            if !labels.contains(&view.label) {
                if let Err(error) = crate::open_plugin_view(&app, &view.label, &view.url, false) {
                    runtime.plugins.lock().unwrap().startup_failure(&id, &error);
                    runtime.plugins_changed();
                    return Err(error);
                }
            }
        }
    }
    result
}
// Handle the cancel plugin package desktop command

#[tauri::command]
pub fn cancel_plugin_package(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) {
    runtime.cancel_plugin_package(&id)
}
// Handle the parameter overrides desktop command
#[tauri::command]
pub fn parameter_overrides(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::parameters::ParameterOverrides, String> {
    runtime.parameter_overrides()
}
// Handle the credential status desktop command
#[tauri::command]
pub fn credential_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    target: stream_dock_manager_runtime::parameter_credentials::CredentialTarget,
) -> Result<stream_dock_manager_runtime::credentials::CredentialState, String> {
    runtime.credential_status(&target)
}
// Handle the write credential desktop command
#[tauri::command]
pub fn write_credential(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    write: stream_dock_manager_runtime::parameter_credentials::CredentialWrite,
) -> Result<stream_dock_manager_runtime::credentials::CredentialState, String> {
    runtime.write_credential(write)
}
// Handle the clear credential desktop command
#[tauri::command]
pub fn clear_credential(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    target: stream_dock_manager_runtime::parameter_credentials::CredentialTarget,
    expected_revision: u64,
) -> Result<stream_dock_manager_runtime::credentials::CredentialState, String> {
    runtime.clear_credential(&target, expected_revision)
}
// Handle the save parameter overrides desktop command
#[tauri::command]
pub fn save_parameter_overrides(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    value: stream_dock_manager_runtime::parameters::ParameterOverrides,
) -> Result<(), String> {
    runtime.save_parameter_overrides(value)
}
// Handle the GIF has animation desktop command
#[tauri::command]
pub async fn gif_has_animation(
    _editor_work: crate::editor_lifecycle::EditorWork,
    path: String,
) -> Result<bool, String> {
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        stream_dock_manager_runtime::media::gif_has_animation(std::path::Path::new(&path))
    })
    .await
}
// Handle the prepare media desktop command
#[tauri::command]
pub async fn prepare_media(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
    key: String,
    generation: u64,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_media(path.into(), key, generation)
    })
    .await
}
// Handle the append browser media desktop command
#[tauri::command]
pub async fn append_browser_media(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let id = request
        .headers()
        .get("x-preparation-id")
        .and_then(|v| v.to_str().ok())
        .ok_or("Missing preparation ID")?
        .to_string();
    let sequence = request
        .headers()
        .get("x-preparation-sequence")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("Missing frame sequence")?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Video batches require binary transfers".into());
    };
    if bytes.len() > 1024 * 1024 {
        return Err("Video batch exceeds limits".into());
    }
    let precise = request
        .headers()
        .get("x-preparation-timing")
        .is_some_and(|value| value == "us");
    let bytes = bytes.clone();
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.append_browser_media_timed(&id, sequence, &bytes, precise)
    })
    .await
}
// Handle the publish browser media desktop command
#[tauri::command]
pub async fn publish_browser_media(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.publish_browser_media(&id)
    })
    .await
}
// Handle the media preparation desktop command
#[tauri::command]
pub fn media_preparation(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::preparation::MediaPreparation, String> {
    runtime.media_preparation(&id)
}
// Handle the cancel media preparation desktop command
#[tauri::command]
pub fn cancel_media_preparation(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    runtime.cancel_media_preparation(&id)
}
// Handle the icon search desktop command

#[tauri::command]
pub async fn icon_search(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    query: String,
    prefix: String,
    start: u32,
    offline: bool,
) -> Result<stream_dock_manager_runtime::icons::IconResults, String> {
    let store = runtime.icons.clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        store.search(&query, &prefix, start, offline)
    })
    .await
}
// Handle the icon load desktop command
#[tauri::command]
pub async fn icon_load(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    offline: bool,
) -> Result<hardware_core::appearance::IconAsset, String> {
    let store = runtime.icons.clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || store.load(&id, offline)).await
}
// Handle the icon save desktop command
#[tauri::command]
pub async fn icon_save(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: hardware_core::appearance::IconAsset,
) -> Result<(), String> {
    let store = runtime.icons.clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || store.save(&asset)).await
}
// Handle the prepare macro import desktop command

#[tauri::command]
pub async fn prepare_macro_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
    password: Option<String>,
) -> Result<stream_dock_manager_runtime::macro_sharing::MacroImportPreview, String> {
    let password = password.map(zeroize::Zeroizing::new);
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        stream_dock_manager_runtime::importing::read_transfer(
            std::path::Path::new(&path),
            password.as_ref().map(|s| s.as_str()),
            |p| runtime.prepare_macro_import(p),
        )
    })
    .await
}
// Handle the recheck macro import desktop command
#[tauri::command]
pub fn recheck_macro_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::macro_sharing::MacroImportPreview, String> {
    runtime.recheck_macro_import(&id)
}
// Handle the cancel macro import desktop command
#[tauri::command]
pub fn cancel_macro_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) {
    runtime.cancel_macro_import(&id);
}
// Handle the commit macro import desktop command
#[tauri::command]
pub async fn commit_macro_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    expected_revision: u64,
    include_plugin_settings: bool,
) -> Result<Snapshot, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.commit_macro_import(&id, expected_revision, include_plugin_settings)
    })
    .await
}

// Handle the runtime editor desktop command
fn runtime_editor<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("Runtime management is restricted to the first-party editor".into())
    }
}
// Handle the runtime status desktop command
#[tauri::command]
pub fn runtime_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
) -> Result<stream_dock_manager_plugin_runtimes::RuntimeStatus, String> {
    runtime_editor(&window)?;
    Ok(runtime.runtimes.status())
}
// Handle the runtime check desktop command
#[tauri::command]
pub async fn runtime_check(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_plugin_runtimes::RuntimeStatus, String> {
    runtime_editor(&window)?;
    let service = runtime.runtimes.clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || service.check()).await
}
// Handle the runtime install desktop command
#[tauri::command]
pub fn runtime_install(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    release_id: String,
    retry_job_id: Option<String>,
) -> Result<String, String> {
    runtime_editor(&window)?;
    if let Some(id) = retry_job_id {
        return runtime.runtimes.retry_install(&id, &release_id);
    }
    runtime.runtimes.start_install(&release_id)
}
// Handle the runtime remove desktop command
#[tauri::command]
pub async fn runtime_remove<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow<R>,
    runtime: tauri::State<'_, Arc<Runtime>>,
    installation_id: String,
) -> Result<(), String> {
    runtime_editor(&window)?;
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        let result = runtime
            .plugins
            .lock()
            .unwrap()
            .remove_private_runtime(&installation_id);
        runtime.plugins_changed();
        result
    })
    .await
}
// Handle the runtime cancel desktop command

#[tauri::command]
pub fn runtime_cancel(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    job_id: String,
) -> Result<(), String> {
    runtime_editor(&window)?;
    runtime.runtimes.cancel(&job_id)
}
// Handle the runtime automatic desktop command
#[tauri::command]
pub fn runtime_automatic(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    enabled: bool,
) -> Result<(), String> {
    runtime_editor(&window)?;
    runtime.runtimes.automatic(enabled)
}
// Handle the runtime candidates desktop command
#[tauri::command]
pub async fn runtime_candidates(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
    kind: stream_dock_manager_plugin_runtimes::RuntimeKind,
) -> Result<Vec<stream_dock_manager_plugin_runtimes::RuntimeInstallation>, String> {
    runtime_editor(&window)?;
    let service = runtime.runtimes.clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || service.candidates(kind))
        .await
        .map_err(|e| e.to_string())
}
// Handle the runtime validate external desktop command
#[tauri::command]
pub async fn runtime_validate_external(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    kind: stream_dock_manager_plugin_runtimes::RuntimeKind,
    path: String,
) -> Result<stream_dock_manager_plugin_runtimes::RuntimeInstallation, String> {
    runtime_editor(&window)?;
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        stream_dock_manager_plugin_runtimes::probe(kind, std::path::Path::new(&path))
    })
    .await
}
// Handle the runtime select desktop command
#[tauri::command]
pub fn runtime_select(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    plugin_id: Option<String>,
    choice: stream_dock_manager_plugin_runtimes::RuntimeSelection,
) -> Result<(), String> {
    runtime_editor(&window)?;
    let requirement = plugin_id
        .as_ref()
        .map(|id| {
            runtime
                .plugins
                .lock()
                .unwrap()
                .preflight(id)
                .map(|p| p.runtime.map(|r| r.requirement))
        })
        .transpose()?
        .flatten();
    if plugin_id.is_some() && requirement.is_none() {
        return Err("Plugin does not declare a managed runtime".into());
    }
    runtime
        .runtimes
        .select(plugin_id.as_deref(), choice, requirement.as_ref())
}
// Collect usage of the installed plugin runtimes
#[tauri::command]
pub fn runtime_usage(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
) -> Result<Vec<stream_dock_manager_plugin_runtimes::RuntimeUsage>, String> {
    runtime_editor(&window)?;
    Ok(runtime.plugins.lock().unwrap().runtime_usage())
}
// Handle the runtime apply desktop command
#[tauri::command]
pub fn runtime_apply(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    plugin_ids: Vec<String>,
) -> Result<Snapshot, String> {
    runtime_editor(&window)?;
    let result = runtime
        .plugins
        .lock()
        .unwrap()
        .apply_runtime_changes(&plugin_ids);
    let snapshot = runtime.plugins_changed();
    result?;
    Ok(snapshot)
}
// Handle the plugin runtime snapshot desktop command

#[tauri::command]
pub fn plugin_runtime_snapshot(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
) -> Result<Vec<stream_dock_manager_plugin_host::PluginRuntimeSnapshot>, String> {
    runtime_editor(&window)?;
    Ok(runtime.plugin_runtime_snapshot())
}
// Handle the plugin monitor visible desktop command
#[tauri::command]
pub fn plugin_monitor_visible(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    consumer: String,
    visible: bool,
) -> Result<(), String> {
    runtime_editor(&window)?;
    runtime.plugin_monitor_visibility(&consumer, visible)
}
// Handle the plugin retry desktop command
#[tauri::command]
pub fn plugin_retry(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<Snapshot, String> {
    runtime_editor(&window)?;
    runtime.plugins.lock().unwrap().retry(&id)?;
    Ok(runtime.plugins_changed())
}
// Handle the plugin restart desktop command
#[tauri::command]
pub fn plugin_restart(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<Snapshot, String> {
    runtime_editor(&window)?;
    runtime.plugins.lock().unwrap().restart(&id)?;
    Ok(runtime.plugins_changed())
}
// Handle the plugin show settings interface desktop command

#[tauri::command]
pub fn plugin_show_settings_interface(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    runtime_editor(&window)?;
    runtime.plugins.lock().unwrap().show_settings_interface(&id)
}
// Handle the condition targets desktop command

#[tauri::command]
pub async fn condition_targets(
    _editor_work: crate::editor_lifecycle::EditorWork,
    source: String,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        stream_dock_manager_runtime::builtins::condition_targets(&source)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the condition target path desktop command
#[tauri::command]
pub async fn condition_target_path(
    _editor_work: crate::editor_lifecycle::EditorWork,
    source: String,
    path: String,
) -> Result<String, String> {
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        stream_dock_manager_runtime::builtins::condition_target_path(&source, &path)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the run saved macro desktop command
#[tauri::command]
pub fn run_saved_macro(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: Option<String>,
    id: String,
    values: std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    runtime.run_saved_macro(device_id.as_deref(), &id, values)
}
// Handle the start macro debug desktop command
#[tauri::command]
pub fn start_macro_debug(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    request: stream_dock_manager_runtime::macro_debug::DebugStart,
) -> Result<stream_dock_manager_runtime::macro_debug::DebugView, String> {
    runtime.inner().start_macro_debug(request)
}
// Handle the macro debug view desktop command
#[tauri::command]
pub fn macro_debug_view(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
    after: u64,
) -> Result<stream_dock_manager_runtime::macro_debug::DebugView, String> {
    runtime.macro_debug_view(&id, after)
}
// Handle the macro debug command desktop command
#[tauri::command]
pub fn macro_debug_command(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
    command: stream_dock_manager_runtime::macro_debug::DebugCommand,
    path: Option<Vec<usize>>,
) -> Result<stream_dock_manager_runtime::macro_debug::DebugView, String> {
    runtime.macro_debug_command_at(&id, command, path)
}
// Handle the compare history desktop command

#[tauri::command]
pub async fn compare_history(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    expected_revision: u64,
    id: String,
) -> Result<stream_dock_manager_runtime::history_compare::HistoryComparison, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.compare_history(expected_revision, &id)
    })
    .await
}
// Handle the preview history control desktop command
#[tauri::command]
pub async fn preview_history_control(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    expected_revision: u64,
    id: String,
    address: stream_dock_manager_runtime::history_compare::HistoryControlAddress,
) -> Result<stream_dock_manager_runtime::history_compare::HistoryControlComparison, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.preview_history_control(expected_revision, &id, &address)
    })
    .await
}
// Handle the library organization desktop command

#[tauri::command]
pub async fn library_organization(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::library_organization::LibraryOrganization, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || runtime.library_organization())
        .await
}
// Handle the save library mark desktop command
#[tauri::command]
pub async fn save_library_mark(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    expected_revision: u64,
    mark: stream_dock_manager_runtime::library_organization::LibraryMark,
) -> Result<stream_dock_manager_runtime::library_organization::LibraryOrganization, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.save_library_mark(expected_revision, mark)
    })
    .await
}
// Handle the prepare profile conversion desktop command

#[tauri::command]
pub async fn prepare_profile_conversion(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    expected_revision: u64,
    profile_id: String,
    target_model: String,
) -> Result<stream_dock_manager_runtime::profile_conversion::ConversionPreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_profile_conversion(expected_revision, &profile_id, &target_model)
    })
    .await
}
// Handle the update profile conversion desktop command
#[tauri::command]
pub async fn update_profile_conversion(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    name: String,
    mappings: std::collections::BTreeMap<String, String>,
    keep_background: bool,
) -> Result<stream_dock_manager_runtime::profile_conversion::ConversionPreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.update_profile_conversion(&id, name, mappings, keep_background)
    })
    .await
}
// Handle the discard profile conversion desktop command
#[tauri::command]
pub async fn discard_profile_conversion(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.discard_profile_conversion(&id)
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the commit profile conversion desktop command
#[tauri::command]
pub async fn commit_profile_conversion(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<Snapshot, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.commit_profile_conversion(&id)
    })
    .await
}
// Handle the media library desktop command

#[tauri::command]
pub async fn media_library(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    page: usize,
    page_size: usize,
    query: String,
) -> Result<stream_dock_manager_runtime::media_library::MediaLibraryPage, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.media_library(page, page_size, &query)
    })
    .await
}
// Audit media ownership only when explicitly requested by the user
#[tauri::command]
pub async fn audit_media_library(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    page: usize,
    page_size: usize,
    query: String,
) -> Result<stream_dock_manager_runtime::media_library::MediaLibraryPage, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.audit_media_library(page, page_size, &query)
    })
    .await
}
// Handle the media thumbnail desktop command
#[tauri::command]
pub async fn media_thumbnail(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || runtime.media_thumbnail(&asset))
        .await
}
// Handle the retain media desktop command
#[tauri::command]
pub async fn retain_media(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || runtime.retain_media(asset))
        .await
}
// Handle the release media desktop command
#[tauri::command]
pub async fn release_media(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || runtime.release_media(&id))
        .await
        .map_err(|e| e.to_string())
}
// Handle the finish media preparation desktop command
#[tauri::command]
pub async fn finish_media_preparation(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.finish_media_preparation(&id)
    })
    .await
}
// Handle the prepare media cleanup desktop command
#[tauri::command]
pub async fn prepare_media_cleanup(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    generation: String,
    assets: Vec<String>,
) -> Result<stream_dock_manager_runtime::media_cleanup::MediaCleanupReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_media_cleanup(&generation, assets)
    })
    .await
}
// Handle the discard media cleanup desktop command
#[tauri::command]
pub async fn discard_media_cleanup(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.discard_media_cleanup(&id)
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the commit media cleanup desktop command
#[tauri::command]
pub async fn commit_media_cleanup(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::media_cleanup::MediaCleanupResult, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.commit_media_cleanup(&id)
    })
    .await
}
// Handle the prepare export review desktop command

#[tauri::command]
pub async fn prepare_export_review(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    request: stream_dock_manager_runtime::export_review::ExportReviewRequest,
) -> Result<stream_dock_manager_runtime::export_review::ExportReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_export_review(request)
    })
    .await
}
// Handle the prepare media deletion desktop command

#[tauri::command]
pub async fn prepare_media_deletion(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
) -> Result<stream_dock_manager_runtime::media_deletion::MediaDeletionReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_media_deletion(&asset)
    })
    .await
}
// Handle the discard media deletion desktop command
#[tauri::command]
pub async fn discard_media_deletion(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.discard_media_deletion(&id)
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the commit media deletion desktop command
#[tauri::command]
pub async fn commit_media_deletion(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::media_deletion::MediaDeletionResult, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.commit_media_deletion(&id)
    })
    .await
}
/// Publish the entire rendered set together. Dropping the private directory removes partial writes.
#[tauri::command]
pub async fn write_profile_reference(
    _editor_work: crate::editor_lifecycle::EditorWork,
    directory: String,
    names: Vec<String>,
    content: Vec<String>,
) -> Result<String, String> {
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        crate::profile_reference::write(std::path::Path::new(&directory), &names, &content)
    })
    .await
}

#[tauri::command]
pub async fn rehearse_export(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    expected_revision: u64,
    sha256: String,
    model: String,
) -> Result<Vec<stream_dock_manager_runtime::export_review::RehearsalFinding>, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.rehearse_export(&id, expected_revision, &sha256, &model)
    })
    .await
}

// Handle the update export review desktop command
#[tauri::command]
pub async fn update_export_review(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    expected_revision: u64,
    change: stream_dock_manager_runtime::export_review::ExportFieldEdit,
) -> Result<stream_dock_manager_runtime::export_review::ExportReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.update_export_review(&id, expected_revision, change)
    })
    .await
}
// Handle the discard export review desktop command
#[tauri::command]
pub async fn discard_export_review(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.discard_export_review(&id)
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the write export review desktop command
#[tauri::command]
pub async fn write_export_review(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    id: String,
    expected_revision: u64,
    sha256: String,
    path: String,
    password: Option<String>,
) -> Result<Option<String>, String> {
    let password = password.map(zeroize::Zeroizing::new);
    let runtime = app
        .try_state::<Arc<Runtime>>()
        .ok_or("Manager is unavailable")?
        .inner()
        .clone();
    let generation = _editor_work.generation()?;
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        let path = std::path::Path::new(&path);
        let sha256 = runtime.write_export_transfer(
            &id,
            expected_revision,
            &sha256,
            path,
            password.as_ref().map(|s| s.as_str()),
        )?;
        Ok(crate::archive_drag::reviewed_export(
            &app, generation, path, &sha256,
        ))
    })
    .await
}
// List the backups belonging to a plugin

#[tauri::command]
pub async fn plugin_backups(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    page: usize,
) -> Result<stream_dock_manager_plugin_host::backups::PluginBackups, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.plugin_backups(&id, page)
    })
    .await
}
// Handle the delete plugin backups desktop command
#[tauri::command]
pub async fn delete_plugin_backups(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    backup: Option<String>,
    all: bool,
) -> Result<usize, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.delete_plugin_backups(&id, backup.as_deref(), all)
    })
    .await
}
// Handle the prepare plugin rollback desktop command
#[tauri::command]
pub async fn prepare_plugin_rollback(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    backup: String,
) -> Result<stream_dock_manager_runtime::plugin_rollback::PluginRollbackReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_plugin_rollback(&id, &backup)
    })
    .await
}
// Handle the cancel plugin rollback desktop command
#[tauri::command]
pub async fn cancel_plugin_rollback(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.cancel_plugin_rollback(&id)
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the commit plugin rollback desktop command
#[tauri::command]
pub async fn commit_plugin_rollback(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    acknowledge_settings: bool,
    allow_unverified: bool,
) -> Result<Snapshot, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.commit_plugin_rollback(&id, acknowledge_settings, allow_unverified)
    })
    .await
}
// Handle the plugin update policy desktop command

#[tauri::command]
pub async fn plugin_update_policy(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::plugin_policy::PluginUpdatePolicy, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.plugin_update_policy(&id)
    })
    .await
}
// Handle the save plugin update policy desktop command
#[tauri::command]
pub async fn save_plugin_update_policy(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    value: stream_dock_manager_runtime::plugin_policy::PluginUpdatePolicy,
) -> Result<stream_dock_manager_runtime::plugin_policy::PluginUpdatePolicy, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.save_plugin_update_policy(value)
    })
    .await
}
// Handle the resource alert settings desktop command

#[tauri::command]
pub async fn resource_alert_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::resource_alerts::ResourceAlertSettings, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.resource_alert_settings(&id)
    })
    .await
}
// Handle the save resource alert settings desktop command
#[tauri::command]
pub async fn save_resource_alert_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    value: stream_dock_manager_runtime::resource_alerts::ResourceAlertSettings,
) -> Result<stream_dock_manager_runtime::resource_alerts::ResourceAlertSettings, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.save_resource_alert_settings(value)
    })
    .await
}
// Handle the benchmark start progress desktop command

#[tauri::command]
pub async fn benchmark_start_progress(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    config: stream_dock_manager_runtime::benchmark::BenchmarkConfig,
) -> Result<stream_dock_manager_runtime::benchmark::BenchmarkProgress, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.benchmark_start_progress(config)
    })
    .await
}
// Handle the benchmark cancel progress desktop command
#[tauri::command]
pub fn benchmark_cancel_progress(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> stream_dock_manager_runtime::benchmark::BenchmarkProgress {
    runtime.benchmark_cancel_progress()
}
// Handle the benchmark progress desktop command
#[tauri::command]
pub fn benchmark_progress(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> stream_dock_manager_runtime::benchmark::BenchmarkProgress {
    runtime.benchmark_progress()
}
// Handle the benchmark result desktop command
#[tauri::command]
pub fn benchmark_result(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    run_id: String,
    report_revision: u64,
) -> stream_dock_manager_runtime::benchmark::BenchmarkResult {
    runtime.benchmark_result(&run_id, report_revision)
}
// Handle the benchmark export desktop command
#[tauri::command]
pub async fn benchmark_export(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
    format: String,
    run_id: String,
    report_revision: u64,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.benchmark_export_revision(&path, &format, &run_id, report_revision)
    })
    .await
}
// Handle the start video import desktop command

#[tauri::command]
pub fn start_video_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    path: String,
    owner: String,
    generation: u32,
) -> Result<String, String> {
    runtime
        .inner()
        .start_video_import(path.into(), owner, generation)
}
// Handle the video import status desktop command
#[tauri::command]
pub fn video_import_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_runtime::video::VideoJob, String> {
    runtime.video_import_status(&id)
}
// Handle the cancel video import desktop command
#[tauri::command]
pub fn cancel_video_import(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    runtime.cancel_video_import(&id)
}
// Handle the read video recording desktop command
#[tauri::command]
pub async fn read_video_recording(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    recording: String,
    start: u64,
    length: u32,
) -> Result<tauri::ipc::Response, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime
            .read_recording_range(&recording, start, length)
            .map(tauri::ipc::Response::new)
    })
    .await
}
// Handle the save video metadata desktop command
#[tauri::command]
pub fn save_video_metadata(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    recording: stream_dock_manager_runtime::video::Recording,
) -> Result<stream_dock_manager_runtime::video::Recording, String> {
    runtime.save_recording_metadata(recording)
}
// Handle the video recording for asset desktop command
#[tauri::command]
pub async fn video_recording_for_asset(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
    owner: String,
    generation: u32,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.recording_for_asset(&asset, owner, generation)
    })
    .await
}
// Handle the prepare video clip desktop command
#[tauri::command]
pub async fn prepare_video_clip(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    draft: stream_dock_manager_runtime::video::ClipDraft,
    owner: String,
    generation: u64,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_recording_clip(draft, owner, generation)
    })
    .await
}
// Handle the video target desktop command
#[tauri::command]
pub fn video_target(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    control_id: Option<String>,
) -> Result<stream_dock_manager_runtime::video::VideoTarget, String> {
    runtime.video_target(&device_id, control_id.as_deref())
}
// Handle the video clip details desktop command
#[tauri::command]
pub async fn video_clip_details(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
) -> Result<stream_dock_manager_runtime::video::VideoClipDetails, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.video_clip_details(&asset)
    })
    .await
}
// Handle the video clips desktop command
#[tauri::command]
pub async fn video_clips(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    recording: Option<String>,
) -> Result<Vec<stream_dock_manager_runtime::video::VideoClipEntry>, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.video_clips(recording.as_deref())
    })
    .await
}
// Handle the video preview frame desktop command
#[tauri::command]
pub async fn video_preview_frame(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    request: tauri::ipc::Request<'_>,
) -> Result<tauri::ipc::Response, String> {
    let header = |key| {
        request
            .headers()
            .get(key)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let device = header("x-video-device").ok_or("Missing device")?;
    let control = header("x-video-control").filter(|value| !value.is_empty());
    let options: stream_dock_manager_runtime::media::MediaOptions =
        serde_json::from_str(&header("x-video-options").ok_or("Missing media options")?)
            .map_err(|e| e.to_string())?;
    options.validate()?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("Video previews require binary frames".into());
    };
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("Preview frame too large".into());
    }
    let bytes = bytes.clone();
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        let target = runtime.video_target(&device, control.as_deref())?;
        let pixels = hardware_core::render::edited_pixels(
            &bytes,
            target.width,
            target.height,
            0,
            options.crop,
            options.fit == "fit",
            Some(options.detail_bounds()),
        )
        .map_err(|e| e.to_string())?;
        let pixels = if options.letterbox.as_deref() == Some("black") {
            hardware_core::render::black_matte(pixels)
        } else {
            pixels
        };
        let transparent =
            pixels.color().has_alpha() && pixels.to_rgba8().pixels().any(|pixel| pixel[3] < 255);
        let format = if options.encoding == "png" || transparent {
            "png"
        } else {
            "jpeg"
        };
        let output =
            hardware_core::render::encode_pixels_quality(pixels, format, options.quality(true))
                .map_err(|e| e.to_string())?;
        Ok(tauri::ipc::Response::new(output))
    })
    .await
}
// Handle the probe video recording desktop command

#[tauri::command]
pub async fn probe_video_recording(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    recording: String,
) -> Result<stream_dock_manager_runtime::video::Recording, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.probe_recording(&recording)
    })
    .await
}
// Handle the video recording position desktop command
#[tauri::command]
pub async fn video_recording_position(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    recording: String,
    position_us: u64,
    direction: i8,
) -> Result<u64, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.recording_position(&recording, position_us, direction)
    })
    .await
}
// Handle the video recording frame desktop command
#[tauri::command]
pub async fn video_recording_frame(
    editor: crate::editor_lifecycle::EditorRuntime,
    recording: String,
    position_us: u64,
    edge: Option<u16>,
    device_id: Option<String>,
    control_id: Option<String>,
    options: Option<stream_dock_manager_runtime::media::MediaOptions>,
) -> Result<tauri::ipc::Response, String> {
    let crate::editor_lifecycle::EditorRuntime {
        work: _editor_work,
        runtime,
    } = editor;
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        let bytes = if let Some(device) = device_id {
            runtime.gif_recording_preview(
                &recording,
                position_us,
                edge.unwrap_or(512),
                &device,
                control_id.as_deref(),
                &options.ok_or("Missing media options")?,
            )
        } else {
            runtime.recording_frame(&recording, position_us, edge.unwrap_or(512))
        }?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
}
// Handle the prepare video clip native desktop command
#[tauri::command]
pub async fn prepare_video_clip_native(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    draft: stream_dock_manager_runtime::video::ClipDraft,
    owner: String,
    generation: u64,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_recording_clip_native(draft, owner, generation)
    })
    .await
}
// Handle the retain video recording desktop command

#[tauri::command]
pub fn retain_video_recording(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    recording: String,
    owner: String,
) -> Result<(), String> {
    runtime.retain_video_recording(&recording, owner)
}
// Handle the release video recording desktop command
#[tauri::command]
pub fn release_video_recording(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    owner: String,
) {
    runtime.release_video_recording(&owner);
}
// Handle the prepared video frame desktop command
#[tauri::command]
pub async fn prepared_video_frame(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
    position_us: u64,
    device_id: Option<String>,
    control_id: Option<String>,
    options: Option<stream_dock_manager_runtime::media::MediaOptions>,
) -> Result<tauri::ipc::Response, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        let bytes = if let Some(device) = device_id {
            runtime.prepared_video_preview(
                &asset,
                position_us,
                &device,
                control_id.as_deref(),
                &options.ok_or("Missing media options")?,
            )
        } else {
            runtime.prepared_video_frame(&asset, position_us)
        }?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
}
// Handle the prepare video clip from asset desktop command
#[tauri::command]
pub async fn prepare_video_clip_from_asset(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
    draft: stream_dock_manager_runtime::video::ClipDraft,
    owner: String,
    generation: u64,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.prepare_derived_video(asset, draft, owner, generation)
    })
    .await
}
// Handle the review unused recordings desktop command

#[tauri::command]
pub async fn review_unused_recordings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::video_recordings::RecordingCleanupReview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.review_unused_recordings()
    })
    .await
}
// Handle the discard recording review desktop command
#[tauri::command]
pub fn discard_recording_review(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) {
    runtime.discard_recording_review(&id);
}
// Handle the commit recording cleanup desktop command
#[tauri::command]
pub async fn commit_recording_cleanup(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<u64, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.commit_recording_cleanup(&id)
    })
    .await
}
// Handle the plugin log plugins desktop command

#[tauri::command]
pub fn plugin_log_plugins(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> Vec<stream_dock_manager_plugin_host::log_capture::LogPluginIdentity> {
    runtime
        .plugins
        .lock()
        .unwrap()
        .list()
        .into_iter()
        .map(
            |plugin| stream_dock_manager_plugin_host::log_capture::LogPluginIdentity {
                id: plugin.id,
                name: plugin.name,
            },
        )
        .collect()
}

// Only trusted application assets are loaded in these windows. Opening one reads
// an existing capture and never changes its lifecycle or starts a plugin.
#[tauri::command]
pub async fn open_plugin_log_window(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    stream: String,
) -> Result<(), String> {
    static OPEN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let capture = runtime
        .plugin_logs
        .list()
        .into_iter()
        .find(|capture| capture.id == id)
        .ok_or("Capture no longer exists")?;
    if stream != "stdout" && stream != "stderr" {
        return Err("Invalid log stream".into());
    }
    let _open = OPEN.lock().unwrap();
    if let Some(window) = app
        .webview_windows()
        .into_values()
        .find(|window| window.label().starts_with("log-reader-"))
    {
        window
            .emit(
                "plugin-log-open",
                serde_json::json!({"id":id,"stream":stream}),
            )
            .map_err(|e| e.to_string())?;
        window.show().map_err(|e| e.to_string())?;
        window.unminimize().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }
    let label = format!("log-reader-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let window = tauri::WebviewWindowBuilder::new(
        &app,
        &label,
        tauri::WebviewUrl::App(format!("plugin-logs.html?id={id}&stream={stream}").into()),
    )
    .title(format!("{} logs — Stream Dock Manager", capture.name))
    .inner_size(760.0, 620.0)
    .min_inner_size(360.0, 360.0)
    .resizable(true)
    .theme(
        app.get_webview_window("main")
            .and_then(|window| window.theme().ok()),
    )
    .build()
    .map_err(|e| e.to_string())?;
    let logs = runtime.plugin_logs.clone();
    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            logs.unsubscribe_owner(&label);
        } else if matches!(
            event,
            tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)
        ) {
            if let Some(observed) = handle.get_webview_window(&label) {
                let visible = observed.is_visible().unwrap_or(false)
                    && !observed.is_minimized().unwrap_or(false);
                let _ = observed.emit("plugin-log-visibility", visible);
            }
        }
    });
    Ok(())
}
// Handle the plugin log preferences desktop command

#[tauri::command]
pub fn plugin_log_preferences(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> stream_dock_manager_plugin_host::log_capture::LogPreferences {
    runtime.plugin_logs.preferences()
}
// Handle the save plugin log preferences desktop command
#[tauri::command]
pub async fn save_plugin_log_preferences(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    value: stream_dock_manager_plugin_host::log_capture::LogPreferences,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.plugin_logs.save_preferences(value)?;
        runtime.plugins.lock().unwrap().refresh_log_configuration();
        Ok(())
    })
    .await
}
// Handle the plugin log captures desktop command
#[tauri::command]
pub fn plugin_log_captures(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> Vec<stream_dock_manager_plugin_host::log_capture::LogCaptureStatus> {
    runtime.plugin_logs.list()
}
// Handle the start plugin log capture desktop command
#[tauri::command]
pub async fn start_plugin_log_capture(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
) -> Result<stream_dock_manager_plugin_host::log_capture::LogCaptureStatus, String> {
    let name = runtime
        .plugins
        .lock()
        .unwrap()
        .list()
        .into_iter()
        .find(|p| p.id == plugin_id)
        .map(|p| p.name)
        .ok_or("Plugin is not installed")?;
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        let result = runtime.plugin_logs.start(&plugin_id, &name)?;
        runtime.plugins.lock().unwrap().refresh_log_configuration();
        Ok(result)
    })
    .await
}
// Handle the stop plugin log capture desktop command
#[tauri::command]
pub async fn stop_plugin_log_capture(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.plugin_logs.stop(&id)?;
        runtime.plugins.lock().unwrap().refresh_log_configuration();
        Ok(())
    })
    .await
}
// Handle the resume plugin log capture desktop command
#[tauri::command]
pub async fn resume_plugin_log_capture(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.plugin_logs.resume(&id)?;
        runtime.plugins.lock().unwrap().refresh_log_configuration();
        Ok(())
    })
    .await
}
// Handle the clear plugin log capture desktop command
#[tauri::command]
pub async fn clear_plugin_log_capture(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.plugin_logs.clear(&id)?;
        runtime.plugins.lock().unwrap().refresh_log_configuration();
        Ok(())
    })
    .await
}
// Handle the subscribe plugin logs desktop command
#[tauri::command]
pub fn subscribe_plugin_logs(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_plugin_host::log_capture::LogReaderLease, String> {
    runtime.plugin_logs.subscribe_owned(&id, window.label())
}
// Handle the unsubscribe plugin logs desktop command
#[tauri::command]
pub fn unsubscribe_plugin_logs(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) {
    runtime.plugin_logs.unsubscribe(&id);
}
// Handle the read plugin logs desktop command
#[tauri::command]
pub async fn read_plugin_logs(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    lease: stream_dock_manager_plugin_host::log_capture::LogReaderLease,
    cursor: Option<stream_dock_manager_plugin_host::log_capture::LogCursor>,
    forward: bool,
) -> Result<stream_dock_manager_plugin_host::log_capture::LogPage, String> {
    let logs = runtime.plugin_logs.clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        logs.read(&lease, cursor, forward)
    })
    .await
}
// Handle the export plugin log capture desktop command
#[tauri::command]
pub async fn export_plugin_log_capture(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    path: String,
) -> Result<(), String> {
    let logs = runtime.plugin_logs.clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        logs.export(&id, std::path::Path::new(&path))
    })
    .await
}
// Handle the plugin log network detail desktop command

#[tauri::command]
pub async fn plugin_log_network_detail(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    lease: stream_dock_manager_plugin_host::log_capture::LogReaderLease,
    cursor: stream_dock_manager_plugin_host::log_capture::LogCursor,
) -> Result<stream_dock_manager_plugin_host::log_capture::LogNetworkDetail, String> {
    let logs = runtime.plugin_logs.clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        logs.network_detail(&lease, &cursor)
    })
    .await
}
// Handle the application data desktop command

#[tauri::command]
pub async fn application_data(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::application_data::ApplicationData, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || runtime.application_data())
        .await
        .map_err(|error| error.to_string())?
}
// Handle the create recovery snapshot desktop command
#[tauri::command]
pub async fn create_recovery_snapshot(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::application_data::ApplicationData, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.create_recovery_snapshot()
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the compact application data desktop command
#[tauri::command]
pub async fn compact_application_data(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::application_data::ApplicationData, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.compact_application_data()
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the purge plugin data desktop command
#[tauri::command]
pub async fn purge_plugin_data(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    review_token: String,
) -> Result<stream_dock_manager_runtime::application_data::ApplicationData, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.purge_plugin_data(&id, &review_token)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the client preferences desktop command

#[tauri::command]
pub async fn client_preferences(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::application_data::ClientPreferences, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || runtime.client_preferences())
        .await
        .map_err(|error| error.to_string())?
}
// Handle the save client preference desktop command
#[tauri::command]
pub async fn save_client_preference(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    name: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.save_client_preference(&name, value)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the restore plugin data desktop command

#[tauri::command]
pub async fn restore_plugin_data(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    review_token: String,
) -> Result<stream_dock_manager_runtime::application_data::ApplicationData, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.restore_plugin_data(&id, &review_token)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the purge plugin data batch desktop command

#[tauri::command]
pub async fn purge_plugin_data_batch(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    reviews: Vec<stream_dock_manager_runtime::application_data::PluginDataReview>,
) -> Result<stream_dock_manager_runtime::application_data::ApplicationData, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.purge_plugin_data_batch(reviews)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the arm locate desktop command

#[tauri::command]
pub fn arm_locate(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> Result<String, String> {
    runtime.inner().arm_locate(&device_id)
}
// Handle the cancel locate desktop command
#[tauri::command]
pub fn cancel_locate(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    token: Option<String>,
) {
    runtime.cancel_locate(token.as_deref());
}
// Handle the slice artwork desktop command

#[tauri::command]
pub fn slice_artwork(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    request: stream_dock_manager_runtime::artwork::ArtworkSlice,
    expected_revision: u64,
) -> Result<Snapshot, String> {
    runtime.slice_artwork(request, expected_revision)
}
// Temporarily expose a Settings palette to negotiated plugin theme consumers
#[tauri::command]
pub fn preview_plugin_palette(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    palette: stream_dock_manager_runtime::hardware_preview::PaletteDraft,
) -> Result<String, String> {
    runtime.inner().preview_plugin_palette(palette)
}
// Retire only the matching global plugin palette preview
#[tauri::command]
pub fn cancel_plugin_palette_preview(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    token: String,
) {
    runtime.cancel_plugin_palette_preview(Some(&token));
}

// Handle the preview on dock desktop command

#[tauri::command]
pub fn preview_on_dock(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
    expected_revision: u64,
    draft: stream_dock_manager_runtime::hardware_preview::HardwareDraft,
) -> Result<String, String> {
    runtime
        .inner()
        .preview_on_dock(&device_id, expected_revision, draft)
}
// Handle the cancel hardware preview desktop command
#[tauri::command]
pub fn cancel_hardware_preview(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    token: Option<String>,
) {
    runtime.cancel_hardware_preview(token.as_deref());
}
// Read the current plugin recovery status

#[tauri::command]
pub fn recovery_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> stream_dock_manager_plugin_host::recovery::RecoveryStatus {
    runtime.recovery_status()
}
// Handle the exit recovery mode desktop command
#[tauri::command]
pub async fn exit_recovery_mode(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
) -> Result<(), String> {
    crate::editor_lifecycle::on_main_thread(&app, |app| {
        if !app.state::<Arc<Runtime>>().recovery_status().active {
            return Err("Manager is not in recovery mode".into());
        }
        crate::editor_lifecycle::request_restart(&app, false);
        Ok(())
    })
    .await
}
// Handle the resolve interrupted startup desktop command
#[tauri::command]
pub async fn resolve_interrupted_startup(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    app: tauri::AppHandle,
    normal: bool,
) -> Result<stream_dock_manager_plugin_host::recovery::RecoveryStatus, String> {
    let status = runtime.resolve_interrupted_startup(normal)?;
    if !normal {
        // All explicit recovery entry points land in configuration recovery,
        // including the interrupted-plugin startup choice. Drain normally first.
        crate::editor_lifecycle::on_main_thread(&app, |app| {
            crate::editor_lifecycle::request_restart(&app, true);
            Ok(())
        })
        .await?;
    }
    Ok(status)
}
// Open only the selected Manager logs directory from the trusted editor, including failed startup
#[tauri::command]
pub async fn open_manager_logs<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    app: tauri::AppHandle<R>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Manager logs are restricted to the main window".into());
    }
    let path = crate::manager_log::selected_directory(&app)
        .ok_or("Manager logs folder is not available")?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::manager_log::open_folder(&path).map_err(|_| "Could not open Manager logs".into())
    })
    .await
    .map_err(|_| "Could not open Manager logs".to_owned())?
}
// Handle the cache status desktop command

#[tauri::command]
pub async fn cache_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::cache::CacheStatus, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || runtime.cache_status()).await
}
// Handle the clear cache desktop command

#[tauri::command]
pub async fn clear_cache(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::cache::CacheCleanupResult, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || runtime.clear_cache()).await
}
// Handle the startup permissions desktop command

#[tauri::command]
pub fn startup_permissions(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> stream_dock_manager_runtime::startup_permissions::StartupPermissionReport {
    crate::startup_permission_gate::report(&runtime)
}
// Handle the complete startup permissions desktop command
#[tauri::command]
pub fn complete_startup_permissions(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    skip_permission: Option<String>,
) -> Result<stream_dock_manager_runtime::startup_permissions::StartupPermissionReport, String> {
    crate::startup_permission_gate::complete(&runtime, skip_permission.as_deref())
}
// Handle the open startup permission settings desktop command
#[tauri::command]
pub fn open_startup_permission_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    id: String,
) -> Result<(), String> {
    let url = stream_dock_manager_runtime::startup_permissions::settings_url(&id)?;
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open")
            .arg(url)
            .status()
            .map_err(|error| format!("Could not open System Settings: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("Could not open System Settings. Open Privacy & Security manually.".into())
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        Err("This permission settings link is only available on macOS".into())
    }
}
// Handle the profile versions desktop command

#[tauri::command]
pub async fn profile_versions(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    profile_id: String,
    expected_revision: u64,
) -> Result<
    stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryVersions,
    String,
> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.profile_versions(&profile_id, expected_revision)
    })
    .await
}
// Handle the profile version desktop command
#[tauri::command]
pub async fn profile_version(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    profile_id: String,
    expected_revision: u64,
    version_id: String,
) -> Result<
    stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryDocument,
    String,
> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.profile_version(&profile_id, expected_revision, &version_id)
    })
    .await
}
// Handle the preview media optimization desktop command

#[tauri::command]
pub async fn preview_media_optimization(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
    max_edge: u16,
) -> Result<stream_dock_manager_runtime::media_optimization::MediaOptimizationPreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.preview_media_optimization(&asset, max_edge)
    })
    .await
}
// Handle the create optimized media copy desktop command
#[tauri::command]
pub async fn create_optimized_media_copy(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
    max_edge: u16,
    token: String,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::blocking_result(&_editor_work, move || {
        runtime.create_optimized_media_copy(&asset, max_edge, &token)
    })
    .await
}
