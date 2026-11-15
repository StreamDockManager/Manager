//! Registration is never performed at startup: native consent is required on every change
use crate::editor_lifecycle::EditorWork;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use stream_dock_manager_runtime::{desktop::PluginLinkStatus, Runtime};
use tauri::{AppHandle, Manager};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
static CHANGING: AtomicBool = AtomicBool::new(false);
struct ChangeGuard;
impl Drop for ChangeGuard {
    // Release resources held by this owner
    fn drop(&mut self) {
        CHANGING.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
struct Scheme(bool);
impl Scheme {
    // Return the name used by this implementation
    fn name(self) -> &'static str {
        if self.0 {
            "openaction"
        } else {
            "streamdeck"
        }
    }
    // Report whether this feature is enabled
    fn enabled(self, settings: &stream_dock_manager_runtime::desktop::DesktopSettings) -> bool {
        if self.0 {
            settings.openaction_links
        } else {
            settings.plugin_links
        }
    }
    // Read the previous vendor link handler
    fn previous(
        self,
        settings: &stream_dock_manager_runtime::desktop::DesktopSettings,
    ) -> Option<String> {
        if self.0 {
            settings.openaction_link_previous_handler.clone()
        } else {
            settings.plugin_link_previous_handler.clone()
        }
    }
    // Save for the current operation
    fn save(
        self,
        runtime: &Runtime,
        enabled: bool,
        previous: Option<String>,
    ) -> Result<(), String> {
        if self.0 {
            runtime.set_openaction_link_settings(enabled, previous)
        } else {
            runtime.set_plugin_link_settings(enabled, previous)
        }
    }
}
// Handle the plugin link status desktop command

#[tauri::command]
pub fn plugin_link_status(
    _editor_work: EditorWork,
    app: AppHandle,
    runtime: tauri::State<Arc<Runtime>>,
    openaction: Option<bool>,
) -> PluginLinkStatus {
    status(&app, &runtime, Scheme(openaction.unwrap_or(false)))
}
// Read the current operation status
fn status(_app: &AppHandle, runtime: &Runtime, scheme: Scheme) -> PluginLinkStatus {
    let enabled = scheme.enabled(&runtime.desktop_settings());
    let native = native::handlers(scheme.name());
    match native {
        Ok((ours, current)) => PluginLinkStatus {
            enabled,
            supported: true,
            registered: current.as_deref() == Some(ours.as_str()),
            reason: ownership_reason(enabled, current.as_deref() == Some(ours.as_str()))
                .replace("streamdeck", scheme.name()),
        },
        Err(reason) => PluginLinkStatus {
            enabled,
            supported: false,
            registered: false,
            reason,
        },
    }
}
// Explain why Manager should own this vendor link handler
fn ownership_reason(enabled: bool, registered: bool) -> &'static str {
    match (enabled,registered) {
        (true,true)=>"Manager handles these links. Only enabled, running plugins receive messages. Disabling restores the previous application while Manager is still selected.",
        (true,false)=>"The preference is enabled, but another application currently handles streamdeck:// links. Manager will not take ownership again automatically.",
        (false,true)=>"macOS currently points these links to Manager, but routing is off. Re-enable and then disable this setting to restore the recorded previous handler, or choose another default application in macOS.",
        (false,false)=>"Off by default. Enabling requests macOS approval and can replace the selected application as the streamdeck:// handler.",
    }
}
#[derive(Debug, PartialEq)]
enum HandlerPlan {
    DisableRouting,
    Change {
        previous: String,
        destination: String,
    },
}
// Build the requested vendor link handler changes
fn handler_plan(
    enabled: bool,
    ours: String,
    current: Option<String>,
    saved_previous: Option<String>,
) -> Result<HandlerPlan, String> {
    if !enabled && current.as_deref() != Some(ours.as_str()) {
        return Ok(HandlerPlan::DisableRouting);
    }
    let previous=if enabled {current.filter(|path|path!=&ours).or(saved_previous)}else{saved_previous}
        .ok_or("Choose another default plugin link application first, so it can be restored when this setting is disabled")?;
    let destination = if enabled { ours } else { previous.clone() };
    Ok(HandlerPlan::Change {
        previous,
        destination,
    })
}
// Handle the set plugin link handling desktop command
#[tauri::command]
pub async fn set_plugin_link_handling(
    _editor_work: EditorWork,
    app: AppHandle,
    runtime: tauri::State<'_, Arc<Runtime>>,
    enabled: bool,
    openaction: Option<bool>,
) -> Result<PluginLinkStatus, String> {
    let scheme = Scheme(openaction.unwrap_or(false));
    if CHANGING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("A link-handler change is already waiting for approval".into());
    }
    let _guard = ChangeGuard;
    let settings = runtime.desktop_settings();
    let (ours, current) = native::handlers(scheme.name())?;
    if enabled == scheme.enabled(&settings)
        && (current.as_deref() == Some(ours.as_str())) == enabled
    {
        return Ok(status(&app, &runtime, scheme));
    }
    if handler_plan(enabled, ours, current, scheme.previous(&settings))?
        == HandlerPlan::DisableRouting
    {
        scheme.save(&runtime, false, None)?;
        return Ok(status(&app, &runtime, scheme));
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let confirmation = if enabled {
        format!("Allow Manager to handle {}:// links? This can replace the currently selected application. macOS will ask you to approve the change. Only enabled, running plugins receive messages.", scheme.name())
    } else {
        format!("Restore the previously selected application for {}:// links? macOS will ask you to approve the change.", scheme.name())
    };
    app.dialog()
        .message(confirmation)
        .title("Plugin link handler")
        .kind(MessageDialogKind::Warning)
        .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancel)
        .show(move |accepted| {
            let _ = sender.send(accepted);
        });
    if !receiver
        .await
        .map_err(|_| "Link-handler confirmation closed")?
    {
        return Err("Cancelled".into());
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let native_runtime = runtime.inner().clone();
    app.run_on_main_thread(move || {
        // Ownership can change while either confirmation dialog is visible
        // Re-read immediately before requesting any native change
        let plan = (|| -> Result<Option<(String, String)>, String> {
            let (ours, current) = native::handlers(scheme.name())?;
            let settings = native_runtime.desktop_settings();
            let HandlerPlan::Change {
                previous,
                destination,
            } = handler_plan(enabled, ours, current, scheme.previous(&settings))?
            else {
                scheme.save(&native_runtime, false, None)?;
                return Ok(None);
            };
            if enabled {
                scheme.save(&native_runtime, false, Some(previous.clone()))?;
            }
            Ok(Some((previous, destination)))
        })();
        match plan {
            Err(error) => {
                let _ = sender.send(Err(error));
            }
            Ok(None) => {
                let _ = sender.send(Ok(None));
            }
            Ok(Some((previous, destination))) => {
                native::request(scheme.name(), destination.clone(), move |result| {
                    let _ = sender.send(result.map(|_| Some((previous, destination))));
                });
            }
        }
    })
    .map_err(|e| e.to_string())?;
    let Some((previous, destination)) = receiver
        .await
        .map_err(|_| "The operating-system handler request ended without a result")??
    else {
        return Ok(status(&app, &runtime, scheme));
    };
    let (_, actual) = native::handlers(scheme.name())?;
    if actual.as_ref() != Some(&destination) {
        return Err("The operating system did not confirm the requested handler; the preference was not changed".into());
    }
    scheme.save(&runtime,enabled,enabled.then_some(previous)).map_err(|error| format!("The operating-system handler changed, but the preference could not be saved: {error}. Retry the setting to recover."))?;
    Ok(status(&app, &runtime, scheme))
}
// Initialize this desktop integration
pub fn init(app: &AppHandle) -> bool {
    let mut hidden = false;
    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        if let Some(runtime) = handle.try_state::<Arc<Runtime>>() {
            for url in event.urls() {
                if matches!(url.scheme(), "streamdeck" | "openaction") {
                    dispatch(&handle, &runtime, url.as_str());
                }
            }
        }
    });
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        let runtime = app.state::<Arc<Runtime>>();
        for url in urls {
            if matches!(url.scheme(), "streamdeck" | "openaction") {
                hidden |= stream_dock_manager_runtime::desktop::plugin_link_hidden(url.as_str());
                dispatch(app, &runtime, url.as_str());
            }
        }
    }
    hidden
}
// Dispatch for the current operation
fn dispatch(app: &AppHandle, runtime: &Runtime, url: &str) {
    if runtime.dispatch_plugin_link(url).unwrap_or(false)
        && !stream_dock_manager_runtime::desktop::plugin_link_hidden(url)
    {
        crate::editor_lifecycle::reveal(app);
    }
}
#[cfg(target_os = "macos")]
mod native {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSBundle, NSError, NSString, NSURL};
    // Read the platform’s registered vendor link handlers
    pub fn handlers(scheme: &str) -> Result<(String, Option<String>), String> {
        let ours = NSBundle::mainBundle()
            .bundleURL()
            .path()
            .ok_or("Application bundle path unavailable")?
            .to_string();
        if !ours.ends_with(".app") {
            return Err("Link-handler changes require the packaged macOS application".into());
        }
        let probe = NSURL::URLWithString(&NSString::from_str(&format!(
            "{scheme}://plugins/message/probe/"
        )))
        .ok_or("Invalid link scheme")?;
        let current = NSWorkspace::sharedWorkspace()
            .URLForApplicationToOpenURL(&probe)
            .and_then(|url| url.path())
            .map(|path| path.to_string());
        Ok((ours, current))
    }
    // Process the request through this subsystem
    pub fn request(
        scheme: &'static str,
        path: String,
        complete: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        if !std::path::Path::new(&path).is_absolute()
            || !path.ends_with(".app")
            || !std::path::Path::new(&path).is_dir()
        {
            complete(Err("The selected application is unavailable".into()));
            return;
        }
        let callback = std::sync::Mutex::new(Some(complete));
        let block = block2::RcBlock::new(move |error: *mut NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                Err("macOS rejected or cancelled the default-handler change".into())
            };
            if let Some(callback) = callback.lock().unwrap().take() {
                callback(result);
            }
        });
        NSWorkspace::sharedWorkspace()
            .setDefaultApplicationAtURL_toOpenURLsWithScheme_completionHandler(
                &NSURL::fileURLWithPath(&NSString::from_str(&path)),
                &NSString::from_str(scheme),
                Some(&block),
            );
    }
}
#[cfg(not(target_os = "macos"))]
mod native {
    // Read the platform’s registered vendor link handlers
    pub fn handlers(_scheme: &str) -> Result<(String, Option<String>), String> {
        Err("Safe operating-system consent and previous-handler restoration are not yet available on this platform".into())
    }
    // Process the request through this subsystem
    pub fn request(
        _scheme: &'static str,
        _path: String,
        complete: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        complete(Err("Unsupported platform".into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify rechecked ownership preserves new handlers and restores exact previous app
    fn rechecked_ownership_preserves_new_handlers_and_restores_exact_previous_app() {
        assert_eq!(
            handler_plan(
                false,
                "ours".into(),
                Some("new-app".into()),
                Some("previous".into())
            )
            .unwrap(),
            HandlerPlan::DisableRouting
        );
        assert_eq!(
            handler_plan(
                true,
                "ours".into(),
                Some("new-app".into()),
                Some("stale-app".into())
            )
            .unwrap(),
            HandlerPlan::Change {
                previous: "new-app".into(),
                destination: "ours".into()
            }
        );
        assert_eq!(
            handler_plan(
                false,
                "ours".into(),
                Some("ours".into()),
                Some("previous".into())
            )
            .unwrap(),
            HandlerPlan::Change {
                previous: "previous".into(),
                destination: "previous".into()
            }
        );
        assert!(handler_plan(true, "ours".into(), None, None).is_err());
    }
    #[test]
    // Verify mismatched saved preferences never claim effective ownership
    fn mismatched_saved_preferences_never_claim_effective_ownership() {
        assert!(ownership_reason(true, false).contains("another application"));
        assert!(ownership_reason(false, true).contains("routing is off"));
        assert!(ownership_reason(false, false).contains("Off by default"));
    }
}
