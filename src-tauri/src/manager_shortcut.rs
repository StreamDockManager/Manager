use crate::editor_lifecycle::{self, EditorWork};
use std::sync::{Arc, Mutex};
use stream_dock_manager_runtime::Runtime;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

#[cfg(target_os = "linux")]
mod wayland;

#[cfg(target_os = "macos")]
const ACCELERATOR: &str = "Super+Alt+Shift+S";
#[cfg(not(target_os = "macos"))]
const ACCELERATOR: &str = "Ctrl+Alt+Shift+S";
#[cfg(target_os = "macos")]
const LABEL: &str = "Cmd+Option+Shift+S";
#[cfg(not(target_os = "macos"))]
const LABEL: &str = ACCELERATOR;

#[derive(Default)]
pub struct ShortcutStateStore {
    status: Mutex<String>,
    native_ready: Mutex<bool>,
    #[cfg(target_os = "linux")]
    activation: Mutex<Option<(String, std::time::Instant)>>,
    #[cfg(target_os = "linux")]
    portal: Mutex<Option<tokio::sync::watch::Sender<bool>>>,
}

// Check whether the value is wayland
pub fn is_wayland() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
            || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()))
}

// Read the current operation status
fn status(app: &AppHandle, message: &str) {
    *app.state::<ShortcutStateStore>().status.lock().unwrap() = message.into();
    let _ = app.emit_to("main", "manager-shortcut-status", message);
}

// Initialize this desktop integration
pub fn init(app: &AppHandle) {
    if !is_wayland() {
        let plugin = tauri_plugin_global_shortcut::Builder::new()
            .with_handler(|app, _, event| {
                if event.state == ShortcutState::Pressed && enabled(app) {
                    editor_lifecycle::reveal_with_activation(app, None);
                }
            })
            .build();
        *app.state::<ShortcutStateStore>()
            .native_ready
            .lock()
            .unwrap() = app.plugin(plugin).is_ok();
    }
    apply(app, enabled(app));
}

// Report whether this feature is enabled
fn enabled(app: &AppHandle) -> bool {
    app.state::<Arc<Runtime>>()
        .desktop_settings()
        .show_manager_shortcut
        .unwrap_or(false)
}

// Apply for the current operation
fn apply(app: &AppHandle, enabled: bool) {
    let state = app.state::<ShortcutStateStore>();
    #[cfg(target_os = "linux")]
    if is_wayland() {
        let mut worker = state.portal.lock().unwrap();
        if enabled {
            if worker.is_none() {
                status(app, "Waiting for the desktop shortcut portal…");
                let (cancel, receiver) = tokio::sync::watch::channel(false);
                *worker = Some(cancel);
                tauri::async_runtime::spawn(wayland::run(app.clone(), receiver));
            }
        } else {
            if let Some(cancel) = worker.take() {
                let _ = cancel.send(true);
            }
            status(app, "");
        }
        return;
    }
    if !*state.native_ready.lock().unwrap() {
        status(
            app,
            if enabled {
                "Global shortcuts are unavailable on this desktop."
            } else {
                ""
            },
        );
        return;
    }
    let shortcuts = app.global_shortcut();
    if enabled {
        if shortcuts.is_registered(ACCELERATOR) || shortcuts.register(ACCELERATOR).is_ok() {
            status(app, LABEL);
        } else {
            crate::manager_log::warning("Could not register Show Manager shortcut");
            status(app, "The Show Manager shortcut is in use or unavailable.");
        }
    } else {
        if shortcuts.is_registered(ACCELERATOR) && shortcuts.unregister(ACCELERATOR).is_err() {
            crate::manager_log::warning("Could not unregister Show Manager shortcut");
            status(
                app,
                "Could not release the Show Manager shortcut. Quit Manager to release it.",
            );
            return;
        }
        status(app, "");
    }
}

#[cfg(target_os = "linux")]
// Record the current shortcut activation
pub fn remember_activation(app: &AppHandle, token: Option<String>) {
    if let Some(token) = token {
        *app.state::<ShortcutStateStore>().activation.lock().unwrap() =
            Some((token, std::time::Instant::now()));
    }
}

#[cfg(target_os = "linux")]
// Handle the global shortcut activation
pub fn activate(app: &AppHandle, window: &tauri::WebviewWindow) {
    use gtk::prelude::GtkWindowExt;
    let token = app
        .state::<ShortcutStateStore>()
        .activation
        .lock()
        .unwrap()
        .take();
    if let (Some((token, _)), Ok(gtk)) = (
        token.filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(5)),
        window.gtk_window(),
    ) {
        gtk.set_startup_id(&token);
    }
}

// Stop for the current operation
pub fn stop(app: &AppHandle) {
    if app.try_state::<ShortcutStateStore>().is_some() {
        apply(app, false);
    }
}
// Handle the manager shortcut status desktop command

#[tauri::command]
pub fn manager_shortcut_status(_editor_work: EditorWork, app: AppHandle) -> String {
    app.state::<ShortcutStateStore>()
        .status
        .lock()
        .unwrap()
        .clone()
}
// Handle the set manager shortcut desktop command

#[tauri::command]
pub async fn set_manager_shortcut(
    _editor_work: EditorWork,
    app: AppHandle,
    enabled: bool,
) -> Result<String, String> {
    editor_lifecycle::on_main_thread(&app, move |app| {
        app.state::<Arc<Runtime>>()
            .set_show_manager_shortcut(enabled)?;
        apply(&app, enabled);
        Ok(app
            .state::<ShortcutStateStore>()
            .status
            .lock()
            .unwrap()
            .clone())
    })
    .await
}
