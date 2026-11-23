//! Trusted transfer, permission and result windows survive hiding the editor.
use crate::editor_lifecycle::{self, EditorWork};
use std::{
    sync::{mpsc, Arc, Weak},
    time::Duration,
};
use stream_dock_manager_plugin_host::{
    permission_rpc::{PluginNotification, PluginNotificationLevel, Services, Workflow},
    permissions::ActionScope,
};
use stream_dock_manager_runtime::{clipboard, permission_broker, Runtime};
use tauri::Manager;
use zeroize::Zeroizing;

enum Action {
    Open(String),
    Close(String),
}
struct Desktop(
    mpsc::Sender<Action>,
    tauri::AppHandle,
    Arc<stream_dock_manager_runtime::vault::Vault>,
);
impl Services for Desktop {
    fn changed(&self, plugin: &str) {
        use tauri::Emitter;
        let _ = self.1.emit_to("main", "plugin-permission-status", plugin);
    }
    fn references(
        &self,
    ) -> Option<&dyn stream_dock_manager_plugin_host::vault_references::References> {
        Some(&*self.2)
    }
    fn clipboard_available(&self) -> bool {
        clipboard::read_available()
    }
    fn notification_available(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            crate::macos_notification::authorized_without_prompt()
        }
        #[cfg(not(target_os = "macos"))]
        {
            use tauri_plugin_notification::NotificationExt;
            self.1
                .notification()
                .permission_state()
                .is_ok_and(|s| s == tauri::plugin::PermissionState::Granted)
        }
    }
    fn notify_completed(&self, plugin: &str) -> Result<(), &'static str> {
        crate::send_background_notification(
            &self.1,
            "Stream Dock Manager",
            &format!("{plugin} completed an action."),
        )
        .map_err(|_| "operation_unavailable")
    }
    fn plugin_notifications_available(&self) -> bool {
        true
    }
    fn create_plugin_notification(
        &self,
        notification: &PluginNotification,
    ) -> Result<(), &'static str> {
        use tauri::Emitter;
        let editor = self.1.get_webview_window("main").filter(|window| {
            editor_lifecycle::accepts_plugin_alerts(&self.1)
                && window.is_visible().unwrap_or(false)
                && !window.is_minimized().unwrap_or(true)
        });
        let deliver_to_editor = |window: &tauri::WebviewWindow| {
            let payload =
                serde_json::to_value(notification).map_err(|_| "operation_unavailable")?;
            self.1
                .emit_to(window.label(), "plugin-notification", payload)
                .map_err(|_| "operation_unavailable")
        };
        if let Some(window) = &editor {
            if window.is_focused().unwrap_or(false) {
                return deliver_to_editor(window);
            }
        }
        // On macOS the sender already checks authorization without prompting
        if cfg!(target_os = "macos") || self.notification_available() {
            let kind = match notification.level {
                PluginNotificationLevel::Info => "Information",
                PluginNotificationLevel::Success => "Success",
                PluginNotificationLevel::Warning => "Warning",
                PluginNotificationLevel::Error => "Error",
            };
            if crate::send_background_notification(
                &self.1,
                &format!("Stream Dock Manager · {kind}"),
                &format!("{}: {}", notification.plugin, notification.message),
            )
            .is_ok()
            {
                return Ok(());
            }
        }
        // Do not prompt for OS permissions or bring Manager to the foreground
        editor
            .as_ref()
            .ok_or("operation_unavailable")
            .and_then(deliver_to_editor)
    }
    fn copy_available(&self) -> bool {
        clipboard::copy_available()
    }
    fn capture(&self, cancelled: &dyn Fn() -> bool) -> Result<Zeroizing<String>, &'static str> {
        use clipboard::ClipboardError::*;
        clipboard::capture_text(cancelled).map_err(|e| match e {
            NoText => "no_text",
            UnsupportedFormat => "unsupported_format",
            UnsupportedBackend => "operation_unavailable",
            InvalidEncoding => "invalid_encoding",
            TooLarge => "too_large",
            Changed => "clipboard_changed",
            Access => "clipboard_access",
            Cancelled => "cancelled",
        })
    }
    fn copy(&self, text: &str) -> Result<(), &'static str> {
        clipboard::copy_result(text).map_err(|_| "operation_unavailable")
    }
    fn summarize(
        &self,
        text: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, &'static str> {
        permission_broker::summarize(text, cancelled)
    }
    fn credentialed_summary(
        &self,
        text: &str,
        key: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, &'static str> {
        permission_broker::summary(text, Some(key), cancelled)
    }
    fn webhook_available(&self) -> bool {
        true
    }
    fn webhook(
        &self,
        endpoint: &stream_dock_manager_plugin_host::permission_rpc::WebhookEndpoint,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), &'static str> {
        permission_broker::webhook(endpoint, cancelled)
    }
    fn open(&self, id: &str) -> Result<(), &'static str> {
        self.0
            .send(Action::Open(id.into()))
            .map_err(|_| "operation_unavailable")
    }
    fn close(&self, id: &str) {
        let _ = self.0.send(Action::Close(id.into()));
    }
}

pub fn attach(app: &tauri::AppHandle, runtime: &Arc<Runtime>) {
    let workflow = runtime.plugins.lock().unwrap().permission_workflow();
    let (tx, rx) = mpsc::channel();
    workflow.attach(Arc::new(Desktop(tx, app.clone(), runtime.vault.clone())));
    let weak: Weak<Workflow> = Arc::downgrade(&workflow);
    let app = app.clone();
    std::thread::spawn(move || loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Action::Open(id)) => {
                let Some(flow) = weak.upgrade() else { break };
                let label = format!("permission-popup-{id}");
                let url = format!("permission-popup.html?id={id}");
                match tauri::WebviewWindowBuilder::new(
                    &app,
                    label,
                    tauri::WebviewUrl::App(url.into()),
                )
                .title("Stream Dock Manager")
                .inner_size(520., 480.)
                .min_inner_size(320., 240.)
                .build()
                {
                    Ok(window) => {
                        let _ = window.set_focus();
                        window.on_window_event(move |e| {
                            if matches!(e, tauri::WindowEvent::Destroyed) {
                                flow.cancel(&id);
                            }
                        });
                    }
                    Err(_) => flow.cancel(&id),
                }
            }
            Ok(Action::Close(id)) => {
                if let Some(w) = app.get_webview_window(&format!("permission-popup-{id}")) {
                    let _ = w.close();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(flow) = weak.upgrade() {
            flow.expire()
        } else {
            break;
        }
    });
}

fn caller(window: &tauri::WebviewWindow, id: &str) -> Result<(), String> {
    if id.len() != 36 || window.label() != format!("permission-popup-{id}") {
        return Err("permission_denied".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn permission_window_view(
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<serde_json::Value, String> {
    caller(&window, &id)?;
    let rt = runtime.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let flow = rt.plugins.lock().unwrap().permission_workflow();
        flow.view(rt.runtimes.storage(), &id)
    })
    .await
    .map_err(|_| "operation_unavailable".to_string())?
}
#[tauri::command]
pub async fn permission_window_reply(
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    choice: String,
    text: Option<String>,
) -> Result<(), String> {
    caller(&window, &id)?;
    let rt = runtime.inner().clone();
    #[cfg(target_os = "windows")]
    let foreground_copy = choice == "copy";
    let respond = move || {
        let flow = rt.plugins.lock().unwrap().permission_workflow();
        flow.respond(
            rt.runtimes.storage(),
            &id,
            &choice,
            text.map(Zeroizing::new),
        )
    };
    #[cfg(target_os = "windows")]
    if foreground_copy {
        let (tx, rx) = tokio::sync::oneshot::channel();
        window
            .run_on_main_thread(move || {
                let _ = tx.send(respond());
            })
            .map_err(|_| "operation_unavailable")?;
        return rx.await.map_err(|_| "operation_unavailable")?;
    }
    tauri::async_runtime::spawn_blocking(respond)
        .await
        .map_err(|_| "operation_unavailable".to_string())?
}
#[tauri::command]
pub async fn installed_service_permissions(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
) -> Result<serde_json::Value, String> {
    work.ensure_editable()?;
    let rt = runtime.inner().clone();
    editor_lifecycle::spawn_blocking(&work, move || {
        rt.plugins.lock().unwrap().installed_permissions(&plugin_id)
    })
    .await
    .map_err(|_| "operation_unavailable".to_string())?
}
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn set_service_permission(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
    installation: String,
    inventory: String,
    revision: String,
    grants: Vec<ActionScope>,
    allow: bool,
) -> Result<(), String> {
    work.ensure_editable()?;
    let rt = runtime.inner().clone();
    editor_lifecycle::spawn_blocking(&work, move || {
        rt.plugins.lock().unwrap().change_permissions(
            &plugin_id,
            &installation,
            &inventory,
            &revision,
            &grants,
            allow,
        )
    })
    .await
    .map_err(|_| "operation_unavailable".to_string())?
}
