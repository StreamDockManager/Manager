//! Explicit, panel-scoped one-shot OS location access. Plugin webviews never
//! receive Tauri permissions and no coordinates are committed by this service.
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use stream_dock_manager_plugin_host::SettingsLocationFix as Fix;
use stream_dock_manager_runtime::Runtime;
use tauri::Manager;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Default)]
pub struct Requests(Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>);
impl Requests {
    // Report the pending work tracked by this owner
    pub fn pending(&self) -> bool {
        self.0
            .try_lock()
            .map_or(true, |requests| !requests.is_empty())
    }
    // Cancel for the current operation
    pub fn cancel(&self, token: &str) {
        if let Some(cancel) = self.0.lock().unwrap().remove(token) {
            cancel.store(true, Ordering::SeqCst);
        }
    }
}
// Validate fix
fn validate_fix(fix: Fix) -> Result<Fix, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    if !fix.latitude.is_finite()
        || !fix.longitude.is_finite()
        || fix.latitude.abs() > 90.0
        || fix.longitude.abs() > 180.0
        || !fix.accuracy_m.is_finite()
        || fix.accuracy_m < 0.0
        || !fix.timestamp.is_finite()
        || now - fix.timestamp > 300.0
        || fix.timestamp > now + 60.0
    {
        return Err(
            "No recent valid location fix is available. Try again or enter coordinates.".into(),
        );
    }
    Ok(fix)
}
// Check whether the location request has user authorization
fn trusted(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Location requests require the Manager settings window".into());
    }
    Ok(())
}
// Handle the cancel settings location desktop command
#[tauri::command]
pub fn cancel_settings_location(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    requests: tauri::State<Requests>,
    token: String,
) -> Result<(), String> {
    trusted(&window)?;
    requests.cancel(&token);
    Ok(())
}
// Handle the request settings location desktop command
#[tauri::command]
pub async fn request_settings_location(
    _editor_work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
    requests: tauri::State<'_, Requests>,
    token: String,
) -> Result<stream_dock_manager_plugin_host::SettingsLocationFix, String> {
    trusted(&window)?;
    runtime
        .plugins
        .lock()
        .unwrap()
        .settings_location_allowed(&token)?;
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut pending = requests.0.lock().unwrap();
        if pending.contains_key(&token) {
            return Err("A location request is already running".into());
        }
        if pending.len() >= 8 {
            return Err("Too many location requests".into());
        }
        pending.insert(token.clone(), cancel.clone());
    }
    let app = window.app_handle().clone();
    let runtime = runtime.inner().clone();
    let pending_requests = requests.0.clone();
    let permit = _editor_work.worker();
    // This task owns platform retirement even if the IPC future is abandoned. In particular,
    // macOS delegate cleanup and Linux portal closure must run after cancellation.
    tauri::async_runtime::spawn(async move {
        let _editor_work = permit;
        let result = platform_fix(app, cancel.clone(), &_editor_work).await;
        {
            let mut pending = pending_requests.lock().unwrap();
            if pending
                .get(&token)
                .is_some_and(|active| Arc::ptr_eq(active, &cancel))
            {
                pending.remove(&token);
            }
        }
        if cancel.load(Ordering::SeqCst) {
            return Err("Location request cancelled".into());
        }
        runtime
            .plugins
            .lock()
            .unwrap()
            .settings_location_allowed(&token)?;
        validate_fix(result?)
    })
    .await
    .map_err(|error| error.to_string())?
}

// Both first-party callers use the same one-shot platform request and native cleanup.
async fn platform_fix(
    app: tauri::AppHandle,
    cancel: Arc<AtomicBool>,
    editor: &crate::editor_lifecycle::EditorWork,
) -> Result<Fix, String> {
    #[cfg(target_os = "macos")]
    let result = macos::current(app, cancel, editor).await;
    #[cfg(target_os = "windows")]
    let result = windows::current(app, cancel, editor).await;
    #[cfg(target_os = "linux")]
    let result = {
        let _ = (app, editor);
        linux::current(cancel).await
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let result: Result<Fix, String> = {
        let _ = (app, editor);
        Err("Location services are unavailable on this platform".into())
    };
    result
}

#[tauri::command]
pub async fn vault_current_location(
    work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<stream_dock_manager_plugin_host::SettingsLocationFix, String> {
    trusted(&window)?;
    work.ensure_editable()?;
    if runtime.recovery_status().active {
        return Err("vault_unavailable".into());
    }
    let id = uuid::Uuid::parse_str(&id).map_err(|_| "invalid_request")?;
    let cancel = runtime.vault.begin_location(id, work.generation()?)?;
    let runtime = runtime.inner().clone();
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        let result = platform_fix(app, cancel.clone(), &work).await;
        runtime.vault.finish_location(&cancel)?;
        work.ensure_editable()?;
        validate_fix(result?)
    })
    .await
    .map_err(|_| "Location request unavailable".to_owned())?
}

#[tauri::command]
pub fn vault_cancel_location(
    work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    trusted(&window)?;
    let id = uuid::Uuid::parse_str(&id).map_err(|_| "invalid_request")?;
    runtime.vault.cancel_location(id, work.generation()?);
    Ok(())
}

/// Search only from an open, authorized location editor. Both the native request
/// and its result belong to the same Vault lease or settings draft.
#[expect(
    clippy::too_many_arguments,
    reason = "Preserve the existing location search IPC argument names"
)]
#[tauri::command]
pub async fn search_locations(
    work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<'_, Arc<Runtime>>,
    requests: tauri::State<'_, Requests>,
    vault_id: Option<String>,
    token: Option<String>,
    endpoint: String,
    query: String,
    country: String,
    postal: bool,
) -> Result<Vec<stream_dock_manager_runtime::location_search::LocationMatch>, String> {
    trusted(&window)?;
    work.ensure_editable()?;
    if runtime.recovery_status().active {
        return Err("Location search is unavailable in recovery.".into());
    }
    let runtime = runtime.inner().clone();
    // An empty saved preference selects the built-in provider, not an arbitrary destination
    if runtime.location_provider()? != endpoint {
        return Err("Save the search provider before searching.".into());
    }
    let pending = requests.0.clone();
    let cancel = match (&vault_id, &token) {
        (Some(id), None) => runtime.vault.begin_location(
            uuid::Uuid::parse_str(id).map_err(|_| "Invalid Vault request")?,
            work.generation()?,
        )?,
        (None, Some(token)) => {
            runtime
                .plugins
                .lock()
                .unwrap()
                .settings_location_allowed(token)?;
            let mut requests = pending.lock().unwrap();
            if requests.contains_key(token) || requests.len() >= 8 {
                return Err("Another location request is running.".into());
            }
            let cancel = Arc::new(AtomicBool::new(false));
            requests.insert(token.clone(), cancel.clone());
            cancel
        }
        _ => return Err("Choose a location in the active editor.".into()),
    };
    let permit = work.worker();
    crate::editor_lifecycle::blocking_result(&work, move || {
        let result = runtime
            .location_search
            .search(&endpoint, &query, &country, postal, || {
                cancel.load(Ordering::SeqCst)
            });
        if let Some(token) = token {
            let mut requests = pending.lock().unwrap();
            if requests
                .get(&token)
                .is_some_and(|c| Arc::ptr_eq(c, &cancel))
            {
                requests.remove(&token);
            }
            drop(requests);
            runtime
                .plugins
                .lock()
                .unwrap()
                .settings_location_allowed(&token)?;
        } else {
            runtime.vault.finish_location(&cancel)?;
        }
        permit.ensure_editable()?;
        if cancel.load(Ordering::SeqCst) {
            return Err("Location search cancelled.".into());
        }
        result
    })
    .await
}

#[tauri::command]
pub fn location_provider(
    work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
) -> Result<String, String> {
    trusted(&window)?;
    work.ensure_editable()?;
    runtime.location_provider()
}

#[tauri::command]
pub fn set_location_provider(
    work: crate::editor_lifecycle::EditorWork,
    window: tauri::WebviewWindow,
    runtime: tauri::State<Arc<Runtime>>,
    endpoint: String,
) -> Result<(), String> {
    trusted(&window)?;
    work.ensure_editable()?;
    runtime.set_location_provider(&endpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Read the current location fix
    fn fix() -> Fix {
        Fix {
            latitude: 0.0,
            longitude: 0.0,
            accuracy_m: 100.0,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
        }
    }
    #[test]
    // Verify actual zero coordinates are valid but missing or stale fixes are not
    fn actual_zero_coordinates_are_valid_but_missing_or_stale_fixes_are_not() {
        assert!(validate_fix(fix()).is_ok());
        for bad in [
            Fix {
                timestamp: 0.0,
                ..fix()
            },
            Fix {
                latitude: f64::NAN,
                ..fix()
            },
            Fix {
                accuracy_m: -1.0,
                ..fix()
            },
            Fix {
                longitude: 181.0,
                ..fix()
            },
        ] {
            assert!(validate_fix(bad).is_err());
        }
    }
    #[test]
    // Verify cancellation is scoped to the panel
    fn cancellation_is_scoped_to_the_panel() {
        let requests = Requests::default();
        let one = Arc::new(AtomicBool::new(false));
        let two = Arc::new(AtomicBool::new(false));
        requests
            .0
            .lock()
            .unwrap()
            .extend([("one".into(), one.clone()), ("two".into(), two.clone())]);
        requests.cancel("one");
        assert!(one.load(Ordering::SeqCst));
        assert!(!two.load(Ordering::SeqCst));
    }
}
