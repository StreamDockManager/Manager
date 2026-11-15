#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod archive_drag;
mod archive_import;
mod browser_inventory;
mod catalog_link;
mod commands;
mod configuration_recovery;
mod dev_restart;
mod diagnostic_export;
mod editor_lifecycle;
mod location;
mod manager_log;
mod manager_shortcut;
mod media_drop;
mod startup_args;
mod tray;
mod uninstall_data;
#[cfg(target_os = "macos")]
mod vendor_file_picker;
mod vendor_inspectors;
mod vendor_links;
use std::sync::Arc;
use stream_dock_manager_runtime::{Profile, Runtime, Snapshot};
use tauri::{Emitter, Manager};
// Handle the builtin capabilities desktop command
#[tauri::command]
fn builtin_capabilities(
    _editor_work: crate::editor_lifecycle::EditorWork,
) -> std::collections::BTreeMap<String, Option<String>> {
    [
        "keyboard",
        "pointer",
        "pointer_absolute",
        "clipboard",
        "selection",
        "insertion",
        "paste",
        "desktop",
    ]
    .into_iter()
    .map(|name| {
        (
            name.into(),
            stream_dock_manager_runtime::builtins::State::capability(name),
        )
    })
    .collect()
}
// Handle the resume bindings desktop command
#[tauri::command]
fn resume_bindings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
) {
    runtime.builtin_state.resume(&device_id);
}
// Handle the action UI request desktop command
#[tauri::command]
fn action_ui_request(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
) -> Result<serde_json::Value, String> {
    if window.label() != format!("action-popup-{id}") {
        return Err("Invalid popup owner".into());
    }
    runtime
        .builtin_state
        .pending_ui(&id)
        .ok_or("Prompt expired".into())
}
// Handle the action UI reply desktop command
#[tauri::command]
fn action_ui_reply(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
    value: Option<String>,
) -> Result<(), String> {
    if window.label() != format!("action-popup-{id}") {
        return Err("Invalid popup owner".into());
    }
    runtime.builtin_state.reply_ui(&id, value)
}
// Handle action UI
fn handle_action_ui(handle: &tauri::AppHandle, event: &stream_dock_manager_runtime::RuntimeEvent) {
    use tauri_plugin_dialog::DialogExt;
    if event.kind == "action-ui" {
        let Some(data) = event
            .message
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        else {
            return;
        };
        let Some(id) = data["id"].as_str().map(String::from) else {
            return;
        };
        let runtime = handle.state::<Arc<Runtime>>().inner().clone();
        if data["kind"] == "path" {
            let handle = handle.clone();
            std::thread::spawn(move || {
                let file = handle.dialog().file();
                let picked = if data["directory"].as_bool().unwrap_or(false) {
                    file.blocking_pick_folder()
                } else {
                    file.blocking_pick_file()
                };
                let value = picked
                    .and_then(|p| p.into_path().ok())
                    .map(|p| p.to_string_lossy().into());
                let _ = runtime.builtin_state.reply_ui(&id, value);
            });
        } else {
            let label = format!("action-popup-{id}");
            let url = format!("action-popup.html?id={id}");
            match tauri::WebviewWindowBuilder::new(
                handle,
                label,
                tauri::WebviewUrl::App(url.into()),
            )
            .theme(
                handle
                    .get_webview_window("main")
                    .and_then(|window| window.theme().ok()),
            )
            .title(
                data["title"]
                    .as_str()
                    .unwrap_or("Stream Dock Manager action"),
            )
            .inner_size(440.0, 340.0)
            .resizable(false)
            .always_on_top(true)
            .build()
            {
                Ok(window) => {
                    window.on_window_event(move |event| {
                        if matches!(event, tauri::WindowEvent::Destroyed) {
                            let _ = runtime.builtin_state.reply_ui(&id, None);
                        }
                    });
                }
                Err(error) => runtime
                    .builtin_state
                    .fail_ui(&id, format!("Could not open action popup: {error}")),
            }
        }
    } else if event.kind == "action-ui-close" {
        if let Some(id) = &event.message {
            if let Some(window) = handle.get_webview_window(&format!("action-popup-{id}")) {
                let _ = window.close();
            }
        }
    } else if event.kind == "action-show-manager" {
        editor_lifecycle::reveal(handle);
    } else if event.kind == "action-notification" {
        if let Some(data) = event
            .message
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        {
            use tauri_plugin_notification::NotificationExt;
            let _ = handle
                .notification()
                .builder()
                .title(data["title"].as_str().unwrap_or("Stream Dock Manager"))
                .body(data["text"].as_str().unwrap_or(""))
                .show();
        }
    }
}
// Handle the snapshot desktop command
#[tauri::command]
fn snapshot(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> Snapshot {
    runtime.snapshot()
}
// Handle the save profile desktop command
#[tauri::command]
fn save_profile(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    profile: Profile,
    expected_revision: u64,
) -> Result<Snapshot, String> {
    runtime.save_profile(profile, expected_revision)
}
// Handle the delete profile desktop command
#[tauri::command]
fn delete_profile(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
    expected_revision: u64,
) -> Result<Snapshot, String> {
    runtime.delete_profile(&id, expected_revision)
}
// Handle the delete page desktop command
#[tauri::command]
fn delete_page(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    profile_id: String,
    page_id: String,
    expected_revision: u64,
) -> Result<Snapshot, String> {
    runtime.delete_page(&profile_id, &page_id, expected_revision)
}
// Handle the activate profile desktop command
#[tauri::command]
fn activate_profile(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    id: String,
    device_id: Option<String>,
) -> Result<Snapshot, String> {
    if let Some(device_id) = device_id {
        runtime.select_device_profile(&device_id, &id, None, true)
    } else {
        runtime.activate_profile(&id)
    }
}
// Handle the simulate desktop command
#[tauri::command]
fn simulate(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    enabled: bool,
    device_id: Option<String>,
) -> Result<Snapshot, String> {
    runtime.simulate_device(enabled, device_id.as_deref().unwrap_or("simulator:n4-pro"))
}
// Handle the remove simulator desktop command
#[tauri::command]
fn remove_simulator(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
) -> Result<Snapshot, String> {
    runtime.remove_simulator(&device_id)
}
// Handle the simulate input desktop command
#[tauri::command]
fn simulate_input(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    event: hardware_core::ControlEvent,
) -> Result<(), String> {
    runtime.simulate_input(&device_id, event)
}
// Handle the brightness desktop command
#[tauri::command]
fn brightness(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    value: u8,
) -> Result<(), String> {
    runtime.brightness(&device_id, value)
}
// Handle the cancel macros desktop command
#[tauri::command]
fn cancel_macros(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) {
    runtime.cancel_macros();
}
// Handle the lighting desktop command
#[tauri::command]
fn lighting(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    device_id: String,
    brightness: u8,
    colors: Vec<[u8; 3]>,
) -> Result<(), String> {
    runtime.lighting(&device_id, brightness, colors)
}
// Handle the diagnostics desktop command
#[tauri::command]
async fn diagnostics(
    app: tauri::AppHandle,
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<Vec<stream_dock_manager_runtime::diagnostics::Diagnostic>, String> {
    let service = runtime.runtimes.clone();
    let browser_windows = crate::browser_inventory::collect(&app, &runtime);
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let mut rows = stream_dock_manager_runtime::diagnostics::collect();
        rows.extend(crate::diagnostic_export::loaded_rows());
        rows.extend(crate::browser_inventory::rows(&browser_windows));
        let status = service.status();
        for kind in stream_dock_manager_plugin_runtimes::RuntimeKind::ALL {
            rows.push(stream_dock_manager_runtime::diagnostics::Diagnostic {
                category: format!("{} plugin runtime", kind.name()),
                value: format!(
                    "{} private installations; selected line {}",
                    status
                        .installations
                        .iter()
                        .filter(|i| i.kind == kind)
                        .count(),
                    status
                        .defaults
                        .get(kind.name())
                        .map(|s| s.line.as_str())
                        .unwrap_or("not selected")
                ),
                status: "info".into(),
            });
        }
        rows
    })
    .await
    .map_err(|e| e.to_string())
}
// Handle the import asset desktop command
#[tauri::command]
async fn import_asset(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
) -> Result<String, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.import_asset(std::path::Path::new(&path))
    })
    .await
    .map_err(|e| e.to_string())?
}
// Open plugin view
fn open_plugin_view<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    label: &str,
    url: &str,
    visible: bool,
) -> Result<(), String> {
    let builder = tauri::WebviewWindowBuilder::new(
        app,
        label,
        tauri::WebviewUrl::External(url.parse().map_err(|_| "invalid plugin view URL")?),
    )
    .title("Stream Dock Manager plugin")
    .inner_size(540.0, 720.0)
    .visible(visible);
    #[cfg(target_os = "macos")]
    let builder =
        builder.background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled);
    builder.build().map_err(|e| e.to_string())?;
    Ok(())
}
// Handle the reload plugins desktop command
#[tauri::command]
async fn reload_plugins(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<stream_dock_manager_runtime::plugin_packages::PluginReloadResult, String> {
    let views: Vec<_> = {
        let host = runtime.plugins.lock().unwrap();
        host.list()
            .into_iter()
            .map(|plugin| {
                (
                    plugin.id.clone(),
                    host.view_labels(&plugin.id).unwrap_or_default(),
                )
            })
            .collect()
    };
    let owned = runtime.inner().clone();
    let result =
        crate::editor_lifecycle::spawn_blocking(&_editor_work, move || owned.reload_plugins())
            .await
            .map_err(|e| e.to_string())?;
    let snapshot = runtime.plugins_changed();
    for label in views
        .into_iter()
        .filter(|(id, _)| !snapshot.plugins.iter().any(|p| &p.id == id && p.enabled))
        .flat_map(|(_, labels)| labels)
    {
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.close();
        }
    }
    let result = result?;
    for label in result.closed_views {
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.close();
        }
    }
    Ok(
        stream_dock_manager_runtime::plugin_packages::PluginReloadResult {
            snapshot,
            added: result.added,
            removed: result.removed.len(),
        },
    )
}
// Handle the delete plugin desktop command
#[tauri::command]
async fn delete_plugin(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    keep_data: Option<bool>,
) -> Result<Snapshot, String> {
    let labels = runtime.plugins.lock().unwrap().view_labels(&id)?;
    let owned = runtime.inner().clone();
    let plugin_id = id.clone();
    let result = crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        let waiter = {
            let mut host = owned.plugins.lock().unwrap();
            host.enable(&plugin_id, false)?;
            host.stop_for_replacement(&plugin_id)?
        };
        if let Some(waiter) = waiter {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !waiter.load(std::sync::atomic::Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    return Err("Plugin process cleanup is still in progress".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        owned.delete_plugin(&plugin_id, keep_data.unwrap_or(false))
    })
    .await
    .map_err(|e| e.to_string())?;
    let snapshot = runtime.plugins_changed();
    // Close views after disable/removal, including committed deletion with pending file cleanup
    if !snapshot
        .plugins
        .iter()
        .any(|plugin| plugin.id == id && plugin.enabled)
    {
        for label in labels {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.close();
            }
        }
    }
    result?;
    Ok(snapshot)
}
// Handle the reset plugin styles desktop command

#[tauri::command]
async fn reset_plugin_styles(
    work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<Snapshot, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&work, move || runtime.reset_plugin_styles(&id))
        .await
        .map_err(|e| e.to_string())?
}
// Handle the open data folder desktop command
#[tauri::command]
async fn open_data_folder(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<(), String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || runtime.open_data_folder())
        .await
        .map_err(|e| e.to_string())?
}
// Reconcile browser views
fn reconcile_browser_views(app: &tauri::AppHandle) {
    let runtime = app.state::<Arc<Runtime>>();
    let views: Vec<_> = {
        let host = runtime.plugins.lock().unwrap();
        host.list()
            .iter()
            .filter_map(|p| {
                host.browser_view(&p.id)
                    .ok()
                    .flatten()
                    .map(|v| (p.id.clone(), v))
            })
            .collect()
    };
    for (label, view) in app.webview_windows() {
        if label.starts_with("plugin-") && !views.iter().any(|(_, desired)| desired.label == label)
        {
            let _ = view.close();
        }
    }
    for (id, view) in views {
        if app.get_webview_window(&view.label).is_none() {
            if let Err(error) = open_plugin_view(app, &view.label, &view.url, false) {
                runtime.plugins.lock().unwrap().startup_failure(&id, &error);
                runtime.plugins_changed();
            }
        }
    }
}
// Handle the set plugin sleep desktop command
#[tauri::command]
fn set_plugin_sleep(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    sleep_when_hidden: bool,
) -> Result<Snapshot, String> {
    runtime
        .plugins
        .lock()
        .unwrap()
        .set_sleep_when_hidden(&id, sleep_when_hidden)?;
    Ok(runtime.plugins_changed())
}
// Handle the enable plugin desktop command
#[tauri::command]
async fn enable_plugin<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle<R>,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    enabled: bool,
) -> Result<Snapshot, String> {
    let (old, next) = {
        let mut plugins = runtime.plugins.lock().unwrap();
        let paused = plugins.recovery_status().active;
        let old = if paused {
            None
        } else {
            plugins.browser_view(&id)?
        };
        plugins.enable(&id, enabled)?;
        (
            old,
            if paused {
                None
            } else {
                plugins.browser_view(&id)?
            },
        )
    };
    if let Some(old) = old {
        if let Some(window) = app.get_webview_window(&old.label) {
            let _ = window.close();
        }
    }
    if let Some(view) = next {
        if let Err(error) = open_plugin_view(&app, &view.label, &view.url, false) {
            runtime.plugins.lock().unwrap().startup_failure(&id, &error);
            runtime.plugins_changed();
            return Err(error);
        }
    }
    Ok(runtime.plugins_changed())
}
// Handle the appearance preferences desktop command
#[tauri::command]
fn appearance_preferences(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> stream_dock_manager_plugin_host::AppearancePreferences {
    runtime.plugins.lock().unwrap().appearance_preferences()
}
// Handle the set profile theme context desktop command
#[tauri::command]
fn set_profile_theme_context<R: tauri::Runtime>(
    _work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle<R>,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: String,
) -> Result<(), String> {
    runtime.set_profile_theme_context(&device_id)?;
    let _ = app.emit("ui-appearance-changed", ());
    Ok(())
}
// Handle the update theme palette desktop command

#[tauri::command]
async fn update_theme_palette<R: tauri::Runtime>(
    work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle<R>,
    runtime: tauri::State<'_, Arc<Runtime>>,
    palette: Option<stream_dock_manager_plugin_host::CustomPalette>,
    delete: Option<String>,
) -> Result<stream_dock_manager_plugin_host::AppearancePreferences, String> {
    let runtime = runtime.inner().clone();
    let preferences = crate::editor_lifecycle::spawn_blocking(&work, move || {
        runtime.update_theme_palette(palette, delete)
    })
    .await
    .map_err(|error| error.to_string())??;
    for label in app.webview_windows().keys() {
        if label.starts_with("log-reader-") || label.starts_with("action-popup-") {
            let _ = app.emit_to(label.as_str(), "ui-appearance-changed", ());
        }
    }
    Ok(preferences)
}
// Handle the set device theme preferences desktop command

#[tauri::command]
fn set_device_theme_preferences(
    _work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    preferences: stream_dock_manager_plugin_host::DeviceThemePreferences,
) -> Result<stream_dock_manager_plugin_host::DeviceThemePreferences, String> {
    runtime
        .plugins
        .lock()
        .unwrap()
        .set_device_theme_preferences(preferences)?;
    Ok(preferences)
}
// Handle the theme wallpaper frame desktop command

#[tauri::command]
async fn theme_wallpaper_frame(
    work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    asset: String,
    size: [u16; 2],
    options: stream_dock_manager_runtime::media::MediaOptions,
    still: bool,
    previous: Option<String>,
) -> Result<tauri::ipc::Response, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&work, move || {
        runtime
            .theme_wallpaper_frame(&asset, size, &options, still, previous.as_deref())
            .map(tauri::ipc::Response::new)
    })
    .await
    .map_err(|error| error.to_string())?
}
// Handle the set appearance desktop command

#[tauri::command]
fn set_appearance<R: tauri::Runtime>(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle<R>,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: Option<String>,
    preference: Option<String>,
    system_dark: Option<bool>,
    style: Option<stream_dock_manager_plugin_host::UiThemeStyle>,
) -> Result<stream_dock_manager_runtime::Snapshot, String> {
    let snapshot = match (preference, style) {
        (Some(preference), None) => {
            runtime.plugins.lock().unwrap().set_appearance(
                id.as_deref(),
                &preference,
                system_dark,
            )?;
            runtime.snapshot()
        }
        (preference, Some(style)) if id.is_none() && system_dark.is_none() => {
            runtime
                .plugins
                .lock()
                .unwrap()
                .set_ui_theme_style(style, preference.as_deref())?;
            runtime.snapshot()
        }
        _ => return Err("Invalid appearance update".into()),
    };
    if id.is_none() {
        for label in app.webview_windows().keys() {
            if label.starts_with("log-reader-") || label.starts_with("action-popup-") {
                let _ = app.emit_to(label.as_str(), "ui-appearance-changed", ());
            }
        }
    }
    Ok(snapshot)
}
// Handle the open settings panel desktop command
#[tauri::command]
async fn open_settings_panel(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    request: stream_dock_manager_plugin_host::SettingsPanelRequest,
) -> Result<stream_dock_manager_plugin_host::SettingsPanel, String> {
    let stream_dock_manager_plugin_host::SettingsPanelRequest {
        id,
        action,
        settings,
        setup,
        address,
        resources,
    } = request;
    let (mut panel, managed) = {
        let mut plugins = runtime.plugins.lock().unwrap();
        let managed = plugins.managed_settings(&id)?;
        if !managed && address.is_none() {
            return Err("Save this action before opening its vendor inspector".into());
        }
        let panel = if setup == Some(true) {
            if action.is_some() {
                return Err("Setup tools require a plugin-wide settings panel".into());
            }
            plugins.setup_settings_panel(&id)
        } else if settings.is_null() {
            plugins.default_settings_panel(
                &id,
                action.as_deref().ok_or("Choose an action for defaults")?,
            )
        } else {
            plugins.settings_panel(&id, action.as_deref(), &settings)
        }?;
        (panel, managed)
    };
    if let Some(resources) = resources.clone() {
        if let Err(error) = runtime.initialize_panel_resources(&panel.token, resources) {
            runtime
                .plugins
                .lock()
                .unwrap()
                .close_inspector(&panel.token);
            return Err(error);
        }
    }
    if !managed {
        if let Err(error) = runtime.bind_vendor_inspector(
            &panel.token,
            &id,
            action.as_deref().ok_or("Action required")?,
            &settings,
            &resources.unwrap_or_default(),
            address.unwrap(),
        ) {
            runtime
                .plugins
                .lock()
                .unwrap()
                .close_inspector(&panel.token);
            return Err(error);
        }
        panel.immediate = true;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let isolated = {
                let plugins = runtime.plugins.lock().unwrap();
                plugins.inspector_default_window_size(&id).and_then(|size| {
                    plugins
                        .isolate_inspector(&id, &panel)
                        .map(|lease| (lease, size))
                })
            };
            let result = match isolated {
                Ok((lease, size)) => {
                    panel.url = lease.url().to_owned();
                    vendor_inspectors::open(&app, lease, size).await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                runtime
                    .plugins
                    .lock()
                    .unwrap()
                    .close_inspector(&panel.token);
                runtime.plugin_lifecycle_changed();
                return Err(error);
            }
            panel.native_window = true;
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = &app; // Other targets retain the sandboxed, popup-disabled iframe.
    }
    runtime.plugin_lifecycle_changed();
    Ok(panel)
}
// Handle the plugin action defaults desktop command
#[tauri::command]
fn plugin_action_defaults(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    action: String,
) -> Result<serde_json::Value, String> {
    runtime
        .plugins
        .lock()
        .unwrap()
        .action_defaults(&id, &action)
}
// Handle the select settings path desktop command
#[tauri::command]
fn select_settings_path(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    token: String,
    key: String,
    value: String,
) -> Result<(), String> {
    runtime
        .plugins
        .lock()
        .unwrap()
        .select_settings_path(&token, &key, &value)
}
// Handle the settings panel value desktop command
#[tauri::command]
fn settings_panel_value(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    token: String,
    finish: Option<bool>,
) -> Result<serde_json::Value, String> {
    let result = if finish == Some(true) {
        runtime.plugins.lock().unwrap().finish_vendor_panel(&token)
    } else {
        runtime.plugins.lock().unwrap().panel_value(&token)
    };
    if finish == Some(true) {
        runtime.plugin_lifecycle_changed();
    }
    result
}
// Handle the close settings panel desktop command
#[tauri::command]
fn close_settings_panel(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    locations: tauri::State<location::Requests>,
    runtime: tauri::State<'_, Arc<Runtime>>,
    token: String,
    id: Option<String>,
    apply: Option<bool>,
) -> Result<(), String> {
    locations.cancel(&token);
    let mut plugins = runtime.plugins.lock().unwrap();
    if let Some(id) = id {
        plugins.commit_plugin_settings(&id, &token)?;
    } else if apply == Some(true) {
        plugins.commit_panel_globals(&token)?;
    }
    plugins.close_inspector(&token);
    drop(plugins);
    vendor_inspectors::close(&app, &token);
    runtime.plugin_lifecycle_changed();
    Ok(())
}
// Handle the authorize wayland desktop command
#[tauri::command]
async fn authorize_wayland(
    _editor_work: crate::editor_lifecycle::EditorWork,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        crate::editor_lifecycle::spawn_blocking(
            &_editor_work,
            stream_dock_manager_runtime::wayland::authorize,
        )
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("Wayland authorization is available on Linux desktop sessions.".into())
    }
}
// Handle the preview surfaces desktop command
#[tauri::command]
async fn preview_surfaces(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    device_id: Option<String>,
) -> Result<stream_dock_manager_runtime::SurfacePreview, String> {
    let runtime = runtime.inner().clone();
    crate::editor_lifecycle::spawn_blocking(&_editor_work, move || {
        runtime.preview_surfaces(device_id.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}
// Handle the desktop settings desktop command
#[tauri::command]
fn desktop_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
) -> stream_dock_manager_runtime::desktop::DesktopSettings {
    runtime.desktop_settings()
}
// Handle the save desktop settings desktop command
#[tauri::command]
fn save_desktop_settings(
    _editor_work: crate::editor_lifecycle::EditorWork,
    runtime: tauri::State<Arc<Runtime>>,
    settings: stream_dock_manager_runtime::desktop::DesktopSettings,
) -> Result<(), String> {
    runtime.save_desktop_settings(settings)
}
// Handle the autostart status desktop command
#[tauri::command]
fn autostart_status(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
) -> Result<stream_dock_manager_runtime::desktop::AutostartStatus, String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        let enabled = app
            .autolaunch()
            .is_enabled()
            .map_err(|error| error.to_string())?;
        Ok(stream_dock_manager_runtime::desktop::AutostartStatus {
            supported: true,
            enabled,
        })
    }
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        Ok(stream_dock_manager_runtime::desktop::AutostartStatus {
            supported: true,
            enabled: stream_dock_manager_runtime::desktop::autostart_status(),
        })
    }
}
// Handle the set autostart desktop command
#[tauri::command]
fn set_autostart(
    _editor_work: crate::editor_lifecycle::EditorWork,
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        let manager = app.autolaunch();
        if enabled {
            manager.enable()
        } else {
            manager.disable()
        }
        .map_err(|error| error.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        stream_dock_manager_runtime::desktop::set_autostart(enabled)
    }
}

// Build the Tauri desktop context
fn desktop_context<R: tauri::Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}
/// Main-window display facts only. Auxiliary/plugin windows never update native
/// registrations; their browser bootstrap resolves its own rendering ratio.
pub(crate) fn update_registration_display(window: &tauri::WebviewWindow) {
    if window.label() != "main" {
        return;
    }
    if let Some(runtime) = window.app_handle().try_state::<Arc<Runtime>>() {
        if let Ok(environment) = stream_dock_manager_plugin_host::RegistrationEnvironment::new(
            Some(stream_dock_manager_runtime::desktop::application_language()),
            window.scale_factor().ok(),
        ) {
            runtime
                .plugins
                .lock()
                .unwrap()
                .set_registration_environment(environment);
        }
    }
}

// Run this executable entry point
fn main() {
    // The installer runs this private mode in a temporary copy after normal app
    // shutdown. It must never initialize Tauri, plugins, or the data store.
    if let Some(code) = uninstall_data::dispatch(std::env::args_os().skip(1)) {
        std::process::exit(code);
    }
    let builder = tauri::Builder::default();
    // Manager does not consume Tao DeviceEvents. Leave Windows Raw Input
    // unregistered until an explicit scoped keyboard session owns that slot.
    // This is Tao's supported filter, not a replacement of another consumer
    #[cfg(target_os = "windows")]
    let builder = builder.device_event_filter(tauri::DeviceEventFilter::Always);
    builder
        .manage(archive_drag::Archives::default())
        .manage(archive_import::ArchiveDrops::default())
        .manage(media_drop::MediaDrops::default())
        .manage(catalog_link::PendingCatalogLink::default())
        .manage(manager_shortcut::ShortcutStateStore::default())
        .manage(location::Requests::default())
        .manage(commands::EditorReadiness::default())
        .manage(Arc::new(editor_lifecycle::EditorLifecycle::default()))
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            if app.try_state::<Arc<Runtime>>().is_none() {
                configuration_recovery::reveal(app);
                return;
            }
            if matches!(args.as_slice(), [_, url] if url.starts_with("stream-dock-manager:"))
                || args.iter().any(|arg| arg.starts_with("openaction://") || arg.starts_with("streamdeck://"))
                || stream_dock_manager_runtime::desktop::background_activation(&args)
            {
                return;
            }
            editor_lifecycle::reveal(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let config_root = startup_args::configuration_root(std::env::args_os().skip(1));
            if let Ok(root) = &config_root {
                manager_log::init(app.handle(), root);
            }
            let linked_launch = catalog_link::init(app.handle());
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            app.handle().plugin(tauri_plugin_autostart::init(
                tauri_plugin_autostart::MacosLauncher::LaunchAgent,
                Some(vec!["--autostart"]),
            ))?;
            // Before an editor exists, the actual primary display is the desktop
            // application's startup display. The main window replaces this context.
            let registration_environment = stream_dock_manager_plugin_host::RegistrationEnvironment::new(
                Some(stream_dock_manager_runtime::desktop::application_language()),
                app.primary_monitor().ok().flatten().map(|monitor| monitor.scale_factor()),
            );
            let recovery_root = config_root.as_ref().ok().cloned();
            let explicit_recovery = startup_args::recovery_mode(std::env::args_os().skip(1));
            let runtime = match config_root.and_then(|root| {
                if stream_dock_manager_runtime::configuration_recovery::startup_landing(&root, explicit_recovery)? {
                    configuration_recovery::open_requested(app.handle(), root)?;
                    return Ok(None);
                }
                Runtime::new_recovery_with_registration_environment(root, explicit_recovery, registration_environment?).map(Some)
            }) {
                Ok(Some(runtime)) => runtime,
                Ok(None) => return Ok(()),
                Err(error) => {
                    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                    manager_log::startup_failed(&error);
                    eprintln!("Stream Dock Manager could not start: {error}");
                    if let Some(root) = recovery_root {
                        match configuration_recovery::open(app.handle(), root, error.clone()) {
                            Ok(()) => return Ok(()),
                            Err(_recovery_error) => {
                                manager_log::failure("Configuration recovery unavailable");
                            }
                        }
                    }
                    let handle = app.handle().clone();
                    app.dialog()
                        .message(error)
                        .title("Stream Dock Manager could not start")
                        .kind(MessageDialogKind::Error)
                        .show(move |_| handle.exit(1));
                    // Returning an error from Tauri's setup hook panics on the
                    // native event loop. Keep it alive only for the error dialog.
                    return Ok(());
                }
            };
            manager_log::stage(if runtime.recovery_status().active { manager_log::Stage::Recovery } else { manager_log::Stage::RuntimeReady });
            runtime.builtin_state.attach_desktop_host();
            let permission_report = runtime.startup_permissions(false);
            if permission_report.ready { runtime.startup_permissions(true); }
            let permission_setup_needed = !permission_report.ready;
            let views: Vec<_> = {
                let plugins = runtime.plugins.lock().unwrap();
                plugins
                    .list()
                    .iter()
                    .filter(|plugin| plugin.enabled)
                    .filter_map(|plugin| {
                        plugins
                            .browser_view(&plugin.id)
                            .ok()
                            .flatten()
                            .map(|view| (plugin.id.clone(), view))
                    })
                    .collect()
            };
            for (id, view) in views {
                if let Err(error) = open_plugin_view(app.handle(), &view.label, &view.url, false) {
                    runtime.plugins.lock().unwrap().startup_failure(&id, &error);
                }
            }
            let receiver = runtime.subscribe();
            app.manage(runtime);
            let hidden_plugin_link = vendor_links::init(app.handle());
            editor_lifecycle::install_native_quit(app.handle()).inspect_err(|_| {
                manager_log::failure("Could not install the native quit handler");
            })?;
            let tray_result = tray::install(app.handle());
            let tray_available = tray_result.is_ok();
            let visible = permission_setup_needed || app.state::<Arc<Runtime>>().restarted() || app.state::<Arc<Runtime>>().recovery_status().active || linked_launch || (!hidden_plugin_link && !stream_dock_manager_runtime::desktop::start_hidden(
                &app.state::<Arc<Runtime>>().desktop_settings(),
                &std::env::args_os()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            ));
            editor_lifecycle::start(app.handle(), visible, tray_available);
            manager_shortcut::init(app.handle());
            if let Err(error) = tray_result {
                manager_log::failure("Could not create the system tray");
                use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
                app.dialog()
                    .message(format!("Could not create the system tray: {error}. The editor will remain available; use Quit to stop the application."))
                    .title("Stream Dock Manager")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {});
            }
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                for event in receiver {
                    if matches!(event.kind.as_str(), "profiles" | "devices" | "sessions" | "workspace-reset" | "display-theme") {
                        let _ = handle.emit("ui-appearance-changed", ());
                    }
                    if matches!(event.kind.as_str(), "plugin-runtime" | "plugin-lifecycle") {
                        if event.kind == "plugin-lifecycle" {
                            reconcile_browser_views(&handle);
                        }
                        let snapshots: Vec<_> = handle
                            .state::<Arc<Runtime>>()
                            .plugin_runtime_snapshot()
                            .into_iter()
                            .filter(|s| {
                                event.kind == "plugin-lifecycle"
                                    || matches!(
                                        s.runtime.state,
                                        stream_dock_manager_plugin_host::RuntimeState::Running
                                            | stream_dock_manager_plugin_host::RuntimeState::Starting
                                    )
                            })
                            .collect();
                        let _ = handle.emit_to("main", "plugin-runtime", snapshots);
                        continue;
                    }
                    handle_action_ui(&handle, &event);
                    if matches!(event.kind.as_str(), "plugins" | "plugin-health") {
                        if event.kind == "plugins" {
                            reconcile_browser_views(&handle);
                        }
                        let snapshots = handle.state::<Arc<Runtime>>().plugin_runtime_snapshot();
                        let _ = handle.emit_to("main", "plugin-runtime", snapshots);
                    }
                    if tray::needs_refresh(&event.kind) {
                        tray::refresh(&handle);
                    }
                    if matches!(event.kind.as_str(), "devices" | "updates")
                        && handle
                            .state::<Arc<Runtime>>()
                            .desktop_settings()
                            .notifications
                        && !handle
                            .get_webview_window("main")
                            .and_then(|w| w.is_visible().ok())
                            .unwrap_or(false)
                    {
                        use tauri_plugin_notification::NotificationExt;
                        let _ = handle
                            .notification()
                            .builder()
                            .title("Stream Dock Manager")
                            .body(
                                event
                                    .message
                                    .as_deref()
                                    .unwrap_or("Connected device list changed"),
                            )
                            .show();
                    }
                    let _ = handle.emit("runtime-event", event);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.app_handle().try_state::<Arc<Runtime>>().is_none() {
                configuration_recovery::close_requested(window, &event);
                return;
            }
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                archive_import::receive(window.app_handle(), paths);
            }
            if window.label() == "main"
                && matches!(event, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_)
                    | tauri::WindowEvent::ScaleFactorChanged { .. })
            {
                if let Some(view) = window.app_handle().get_webview_window("main") {
                    editor_lifecycle::track_placement(&view);
                    update_registration_display(&view);
                }
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    editor_lifecycle::request_close(window.app_handle());
                } else if let Some(token) = window.label().strip_prefix("pi-") {
                    window
                        .app_handle()
                        .state::<Arc<Runtime>>()
                        .plugins
                        .lock()
                        .unwrap()
                        .close_inspector(token);
                }
            }
        })
        .invoke_handler(include!(concat!(env!("OUT_DIR"), "/command_handler.rs")))
        .build(desktop_context())
        .expect("Stream Dock Manager failed to start")
        .run(|app, event| {
            let Some(runtime) = app.try_state::<Arc<Runtime>>() else {
                // Recovery has no devices/plugins or normal runtime to shut down
                configuration_recovery::exit_requested(app, &event);
                #[cfg(target_os = "macos")]
                if matches!(event, tauri::RunEvent::Reopen { .. }) {
                    configuration_recovery::reveal(app);
                }
                if matches!(event, tauri::RunEvent::Exit) { manager_log::finish(); }
                return;
            };
            match event {
                tauri::RunEvent::ExitRequested { api, code, .. }
                    if code != Some(tauri::RESTART_EXIT_CODE) && !runtime.is_stopping() =>
                {
                    api.prevent_exit();
                    // Closing the last WebView is intentional while Runtime continues in the tray
                    // Explicit application quit (a code) still follows the draft handshake
                    if code.is_some() {
                        editor_lifecycle::request_quit(app);
                    }
                }
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Reopen { .. } => editor_lifecycle::reveal(app),
                tauri::RunEvent::WindowEvent {
                    label,
                    event: tauri::WindowEvent::Destroyed,
                    ..
                } if label == "main" => {
                    // There is no longer an application window on the old monitor
                    // Future native launches use the actual current primary display
                    if let Ok(environment) = stream_dock_manager_plugin_host::RegistrationEnvironment::new(
                        Some(stream_dock_manager_runtime::desktop::application_language()),
                        app.primary_monitor().ok().flatten().map(|monitor| monitor.scale_factor()),
                    ) {
                        runtime.plugins.lock().unwrap().set_registration_environment(environment);
                    }
                    editor_lifecycle::destroyed(app);
                },
                tauri::RunEvent::Exit => {
                    vendor_inspectors::close_all(app);
                    runtime.shutdown();
                    manager_log::finish();
                },
                _ => {}
            }
        });
}
