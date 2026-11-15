//! Isolated Linux vendor pages. The sole custom IPC operation opens a
//! native chooser; it accepts no file paths or arbitrary native commands. It is
//! deliberately not Tauri IPC. Popups, downloads and permission requests deny.
use super::{bounded_size, permits_url, InspectorBrowser};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::{Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

thread_local! { static WINDOWS: RefCell<HashMap<String, Window>> = RefCell::new(HashMap::new()); }
struct Window {
    native: tauri::Window,
    view: wry::WebView,
    lease: InspectorBrowser,
    pending: Option<String>,
    generation: Arc<AtomicU64>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    id: String,
    multiple: bool,
}
// Resolve the selected file picker result
fn selection(body: &str) -> Option<Selection> {
    if body.len() > 128 {
        return None;
    }
    let request: Selection = serde_json::from_str(body).ok()?;
    uuid::Uuid::parse_str(&request.id).ok()?;
    Some(request)
}
// Check whether the picker reply belongs to the active request
fn request_is_current(
    pending: Option<&str>,
    id: &str,
    current: u64,
    requested: u64,
    active: bool,
) -> bool {
    active && pending == Some(id) && current == requested
}
// Open for the current operation
pub async fn open(
    app: &tauri::AppHandle,
    lease: InspectorBrowser,
    size: [f64; 2],
) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = app.clone();
    let fail = lease.clone();
    app.run_on_main_thread(move || {
        let _ = tx.send(open_main(handle, lease, size));
    })
    .map_err(|e| e.to_string())?;
    let result = rx
        .await
        .unwrap_or_else(|_| Err("Inspector creation interrupted".into()));
    if result.is_err() {
        fail.revoke();
    }
    result
}
// Open main
fn open_main(
    app: tauri::AppHandle,
    lease: InspectorBrowser,
    _size: [f64; 2],
) -> Result<(), String> {
    if !lease.is_active() {
        return Err("Inspector expired".into());
    }
    if WINDOWS.with(|windows| !windows.borrow().is_empty()) {
        return Err("Close the existing vendor inspector first".into());
    }
    let (width, height) = bounded_size([620.0, 720.0]);
    let native =
        tauri::WindowBuilder::new(&app, format!("vendor-inspector-{}", uuid::Uuid::new_v4()))
            .title(format!("Plugin inspector: {}", lease.owner()))
            .inner_size(width, height)
            .min_inner_size(320.0, 240.0)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?;
    let generation = Arc::new(AtomicU64::new(0));
    let navigation_generation = generation.clone();
    let navigation = lease.clone();
    let ipc_lease = lease.clone();
    let ipc_app = app.clone();
    let builder = wry::WebViewBuilder::new()
        .with_incognito(true)
        .with_devtools(false)
        .with_clipboard(false)
        .with_initialization_script(include_str!("vendor_file_input_portable.js"))
        .with_navigation_handler(move |url| {
            navigation_generation.fetch_add(1, Ordering::SeqCst);
            navigation.is_active() && permits_url(navigation.url(), &url, false)
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_download_started_handler(|_, _| false)
        .with_permission_handler(|_| wry::PermissionResponse::Deny)
        .with_ipc_handler(move |request| {
            if !ipc_lease.is_active()
                || !permits_url(ipc_lease.url(), &request.uri().to_string(), false)
            {
                return;
            }
            let Some(request) = selection(request.body()) else {
                return;
            };
            choose(&ipc_app, &ipc_lease, request);
        });
    #[cfg(target_os = "linux")]
    let result = {
        use wry::WebViewBuilderExtUnix;
        native
            .default_vbox()
            .map_err(|e| e.to_string())
            .and_then(|container| builder.build_gtk(&container).map_err(|e| e.to_string()))
    };
    #[cfg(not(target_os = "linux"))]
    let result = builder.build(&native).map_err(|e| e.to_string());
    let view = match result {
        Ok(view) => view,
        Err(error) => {
            let _ = native.destroy();
            return Err(error);
        }
    };
    #[cfg(target_os = "linux")]
    {
        use webkit2gtk::{FileChooserRequestExt, WebViewExt};
        use wry::WebViewExtUnix;
        // Do not allow showPicker(), directory controls, subframes, or other
        // native chooser paths to bypass the explicit snapshot/grant bridge.
        view.webview().connect_run_file_chooser(|_, request| {
            request.cancel();
            true
        });
    }
    if let Err(error) = view.load_url(lease.url()) {
        drop(view);
        let _ = native.destroy();
        return Err(error.to_string());
    }
    let event_app = app.clone();
    let token = lease.token().to_owned();
    let event_lease = lease.clone();
    native.on_window_event(move |event| match event {
        tauri::WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            let _ = event_app.emit_to("main", "vendor-inspector-closed", &token);
        }
        tauri::WindowEvent::Destroyed => {
            event_lease.revoke();
            let app = event_app.clone();
            let token = token.clone();
            // Never drop the native view while its own callback is borrowed
            tauri::async_runtime::spawn(async move {
                close(&app, &token);
            });
        }
        _ => {}
    });
    if let Err(error) = native.show() {
        drop(view);
        let _ = native.destroy();
        return Err(error.to_string());
    }
    WINDOWS.with(|windows| {
        windows.borrow_mut().insert(
            lease.token().into(),
            Window {
                native,
                view,
                lease: lease.clone(),
                pending: None,
                generation,
            },
        )
    });
    // Session revocation and editor destruction close even an idle native page
    std::thread::spawn(move || {
        while lease.is_active() && app.get_webview_window("main").is_some() {
            std::thread::sleep(Duration::from_millis(100));
        }
        let token = lease.token().to_owned();
        close(&app, &token);
        let _ = app.emit_to("main", "vendor-inspector-closed", token);
    });
    Ok(())
}
// Apply the file selection for the current picker request
fn choose(app: &tauri::AppHandle, lease: &InspectorBrowser, request: Selection) {
    let accepted = WINDOWS.with(|windows| {
        let mut windows = windows.borrow_mut();
        let Some(window) = windows.get_mut(lease.token()) else {
            return None;
        };
        if window.pending.is_some() {
            return None;
        }
        window.pending = Some(request.id.clone());
        Some((
            window.generation.load(Ordering::SeqCst),
            window.native.clone(),
        ))
    });
    let Some((generation, parent)) = accepted else {
        return;
    };
    let token = lease.token().to_owned();
    let title = format!(
        "Share files with {} (16 MiB/file, 64 MiB total)",
        lease.owner()
    );
    let multiple = request.multiple;
    let lease = lease.clone();
    let handle = app.clone();
    let feedback = app.clone();
    let completion = move |files: Option<Vec<tauri_plugin_dialog::FilePath>>| {
        // All authority comes from this native selection, never the IPC message
        let paths = files.and_then(|files| {
            files
                .into_iter()
                .map(|f| f.into_path().ok())
                .collect::<Option<Vec<_>>>()
        });
        let _ = handle.run_on_main_thread(move || {
            let valid = WINDOWS.with(|windows| {
                let mut windows = windows.borrow_mut();
                let Some(window) = windows.get_mut(&token) else {
                    return false;
                };
                let valid = request_is_current(
                    window.pending.as_deref(),
                    &request.id,
                    window.generation.load(Ordering::SeqCst),
                    generation,
                    window.lease.is_active(),
                );
                if !valid && window.pending.as_deref() == Some(&request.id) {
                    window.pending = None;
                    let _ = window.view.evaluate_script(&format!(
                        "window.__streamDockFileResult({}, [])",
                        serde_json::to_string(&request.id).unwrap()
                    ));
                }
                valid
            });
            if !valid {
                return;
            }
            let result = paths
                .map(|paths| lease.grant_selected_resources(&paths))
                .transpose();
            WINDOWS.with(|windows| {
                let mut windows = windows.borrow_mut();
                let Some(window) = windows.get_mut(&token) else {
                    return;
                };
                if !request_is_current(
                    window.pending.as_deref(),
                    &request.id,
                    window.generation.load(Ordering::SeqCst),
                    generation,
                    window.lease.is_active(),
                ) {
                    return;
                }
                window.pending = None;
                let paths = match result {
                    Ok(paths) => paths.unwrap_or_default(),
                    Err(error) => {
                        feedback
                            .dialog()
                            .message(error)
                            .title("The selected files could not be shared")
                            .show(|_| {});
                        vec![]
                    }
                };
                let script = format!(
                    "window.__streamDockFileResult({}, {})",
                    serde_json::to_string(&request.id).unwrap(),
                    serde_json::to_string(&paths).unwrap()
                );
                let _ = window.view.evaluate_script(&script);
            });
        });
    };
    let dialog = app.dialog().file().set_parent(&parent).set_title(title);
    if multiple {
        dialog.pick_files(completion);
    } else {
        dialog.pick_file(move |file| completion(file.map(|file| vec![file])));
    }
}
// Close for the current operation
pub fn close(app: &tauri::AppHandle, token: &str) {
    let token = token.to_owned();
    let _ = app.run_on_main_thread(move || close_main(&token));
}
// Close main
fn close_main(token: &str) {
    if let Some(window) = WINDOWS.with(|windows| windows.borrow_mut().remove(token)) {
        window.lease.revoke();
        drop(window.view);
        let _ = window.native.destroy();
    }
}
// Close all
pub fn close_all(app: &tauri::AppHandle) {
    let _ = app.run_on_main_thread(|| {
        let tokens = WINDOWS.with(|windows| windows.borrow().keys().cloned().collect::<Vec<_>>());
        for token in tokens {
            close_main(&token);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify completion is bound to request document and live session
    fn completion_is_bound_to_request_document_and_live_session() {
        assert!(request_is_current(Some("id"), "id", 3, 3, true));
        assert!(!request_is_current(Some("id"), "id", 4, 3, true));
        assert!(!request_is_current(Some("other"), "id", 3, 3, true));
        assert!(!request_is_current(None, "id", 3, 3, true));
        assert!(!request_is_current(Some("id"), "id", 3, 3, false));
    }
    #[test]
    // Verify chooser protocol accepts only bounded requests not paths
    fn chooser_protocol_accepts_only_bounded_requests_not_paths() {
        let _ = (open, close, close_all); // Type-check the portable host on the test host too.
        let id = uuid::Uuid::new_v4();
        assert!(selection(&format!(r#"{{"id":"{id}","multiple":true}}"#)).is_some());
        for body in [
            format!(r#"{{"id":"{id}","multiple":true,"path":"/secret"}}"#),
            "{}".into(),
            r#"{"id":"bad","multiple":false}"#.into(),
            "x".repeat(129),
        ] {
            assert!(selection(&body).is_none());
        }
    }
}
