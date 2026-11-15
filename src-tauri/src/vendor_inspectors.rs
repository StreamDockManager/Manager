//! Vendor HTML never enters a privileged Tauri webview on the native popup path
//! Each native root has its own revocable HTTP origin and nonpersistent storage
//! No Tauri initialization, app protocol, or editor opener is installed. macOS
//! is IPC-free and confirms popups; Linux exposes only a native chooser request
//! (never a JS-supplied path) and denies popups. Other platforms keep the iframe.
// Test builds compile the portable inspector on every platform
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use stream_dock_manager_plugin_host::InspectorBrowser;

#[cfg(any(target_os = "macos", test))]
#[derive(Default)]
struct WindowBudget {
    active: usize,
    created: usize,
    prompting: bool,
    denied: bool,
}
#[cfg(any(target_os = "macos", test))]
impl WindowBudget {
    // Reserve for the current operation
    fn reserve(&mut self, depth: usize) -> bool {
        if self.denied || self.prompting || depth > 2 || self.active >= 4 || self.created >= 8 {
            return false;
        }
        self.prompting = true;
        self.active += 1;
        self.created += 1;
        true
    }
    // Choose the next action from the current lifecycle state
    fn decide(&mut self, consent: bool, session_live: bool, opener_live: bool) -> bool {
        let accepted = consent && session_live && opener_live;
        self.prompting = false;
        if !accepted {
            self.denied = true;
            self.release();
        }
        accepted
    }
    // Release for the current operation
    fn release(&mut self) {
        self.active = self.active.saturating_sub(1);
    }
}
#[cfg(any(target_os = "macos", target_os = "linux", test))]
// Check whether the inspector is allowed to navigate to this URL
fn permits_url(base: &str, candidate: &str, child: bool) -> bool {
    // A blank child inherits only its isolated opener. All subsequent nonblank
    // navigations must remain inside this exact session's package grant.
    if child && candidate == "about:blank" {
        return true;
    }
    let (Ok(base), Ok(url)) = (tauri::Url::parse(base), tauri::Url::parse(candidate)) else {
        return false;
    };
    let Some((directory, _)) = base.path().rsplit_once('/') else {
        return false;
    };
    let token_prefix = directory.split('/').take(3).collect::<Vec<_>>().join("/") + "/";
    url.scheme() == "http"
        && url.origin() == base.origin()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().starts_with(&token_prefix)
        && !url.path().to_ascii_lowercase().contains("%2f")
        && !url.path().to_ascii_lowercase().contains("%5c")
        && !url.path().contains('\\')
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
// Clamp inspector dimensions to the supported bounds
fn bounded_size(size: [f64; 2]) -> (f64, f64) {
    let width = if size[0].is_finite() { size[0] } else { 500.0 };
    let height = if size[1].is_finite() { size[1] } else { 650.0 };
    (width.clamp(320.0, 1200.0), height.clamp(240.0, 1000.0))
}

#[cfg(any(target_os = "macos", test))]
// Collect labels belonging to the inspector window subtree
fn subtree_labels(
    root: &str,
    parents: &std::collections::HashMap<String, Option<String>>,
) -> Vec<String> {
    let mut pending = vec![root.to_owned()];
    let mut seen = std::collections::HashSet::new();
    let mut labels = Vec::new();
    while let Some(label) = pending.pop() {
        if !seen.insert(label.clone()) {
            continue;
        }
        pending.extend(
            parents
                .iter()
                .filter(|(_, parent)| parent.as_deref() == Some(&label))
                .map(|(id, _)| id.clone()),
        );
        labels.push(label);
    }
    labels
}

#[cfg(any(target_os = "macos", test))]
// Validate the requested inspector popup destination
fn popup_destination(base: &str, candidate: &str) -> String {
    let (Ok(base), Ok(url)) = (tauri::Url::parse(base), tauri::Url::parse(candidate)) else {
        return "a package page".into();
    };
    let prefix = base.path().split('/').take(3).collect::<Vec<_>>().join("/") + "/";
    // Queries, fragments, origins and bearer tokens never enter native prompts
    url.path()
        .strip_prefix(&prefix)
        .unwrap_or("a package page")
        .chars()
        .filter(|c| !c.is_control())
        .take(240)
        .collect()
}

#[cfg(target_os = "macos")]
pub use macos::{close, close_all, open};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
// Close for the current operation
pub fn close(_app: &tauri::AppHandle, _token: &str) {}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
// Close all
pub fn close_all(_app: &tauri::AppHandle) {}

#[cfg(any(target_os = "linux", test))]
#[path = "vendor_inspectors_portable.rs"]
mod portable;
#[cfg(target_os = "linux")]
pub use portable::{close, close_all, open};

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn};
    use objc2_foundation::NSString;
    use std::{
        cell::RefCell,
        collections::HashMap,
        rc::{Rc, Weak},
        time::Duration,
    };
    use tauri::{Emitter, Manager};
    use wry::{WebViewBuilderExtDarwin, WebViewBuilderExtMacos, WebViewExtMacOS};

    thread_local! { static TREES: RefCell<HashMap<String,Rc<Tree>>> = RefCell::new(HashMap::new()); }
    struct Window {
        native: tauri::Window,
        parent: Option<String>,
        _webview: wry::WebView,
        // Dropped only after the webview is no longer able to call its delegate
        _picker: crate::vendor_file_picker::PickerDelegateHandle,
    }
    struct NativeCleanup(Option<tauri::Window>);
    impl Drop for NativeCleanup {
        // Release resources held by this owner
        fn drop(&mut self) {
            if let Some(window) = self.0.take() {
                let _ = window.destroy();
            }
        }
    }
    struct Tree {
        app: tauri::AppHandle,
        lease: InspectorBrowser,
        windows: RefCell<HashMap<String, Window>>,
        budget: RefCell<WindowBudget>,
        popup_size: [f64; 2],
    }

    // Tauri runs main-thread tasks inline when called from a WK delegate
    // Queue destruction from a worker so its delegate/webview cannot be dropped
    // while the native callback is still borrowing it.
    fn defer(app: &tauri::AppHandle, action: impl FnOnce() + Send + 'static) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = app.run_on_main_thread(action);
        });
    }
    // Open for the current operation
    pub async fn open(
        app: &tauri::AppHandle,
        lease: InspectorBrowser,
        popup_size: [f64; 2],
    ) -> Result<(), String> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let handle = app.clone();
        let fail = lease.clone();
        if let Err(error) = app.run_on_main_thread(move || {
            let result = open_main(handle, lease, popup_size);
            let _ = sender.send(result);
        }) {
            fail.revoke();
            return Err(error.to_string());
        }
        let result = receiver
            .await
            .unwrap_or_else(|_| Err("Inspector window creation interrupted".into()));
        if result.is_err() {
            fail.revoke();
        }
        result
    }
    // Open main
    fn open_main(
        app: tauri::AppHandle,
        lease: InspectorBrowser,
        popup_size: [f64; 2],
    ) -> Result<(), String> {
        if !lease.is_active() {
            return Err("Inspector expired".into());
        }
        if TREES.with(|trees| !trees.borrow().is_empty()) {
            return Err("Close the existing vendor inspector first".into());
        }
        let tree = Rc::new(Tree {
            app: app.clone(),
            lease: lease.clone(),
            popup_size,
            windows: RefCell::new(HashMap::new()),
            budget: RefCell::new(WindowBudget {
                active: 1,
                created: 1,
                ..Default::default()
            }),
        });
        TREES.with(|trees| {
            trees
                .borrow_mut()
                .insert(lease.token().into(), tree.clone())
        });
        if let Err(error) = build_window(&tree, 0, None, None) {
            close_main(lease.token());
            return Err(error);
        }
        // Revocation is authoritative at every navigation/popup/resource use
        // This watcher also removes already loaded windows after token removal
        // or editor destruction, even if no more page requests occur.
        #[cfg(feature = "native-inspector-probe")]
        let native_probe_owner = probe::active(lease.owner());
        #[cfg(not(feature = "native-inspector-probe"))]
        let native_probe_owner = false;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let owner_missing = if native_probe_owner {
                    app.get_window("main").is_none()
                } else {
                    app.get_webview_window("main").is_none()
                };
                if !lease.is_active() || owner_missing {
                    break;
                }
            }
            let token = lease.token().to_owned();
            let notify = app.clone();
            let _ = app.run_on_main_thread(move || {
                close_main(&token);
                let _ = notify.emit_to("main", "vendor-inspector-closed", &token);
            });
        });
        Ok(())
    }
    // Build window
    fn build_window(
        tree: &Rc<Tree>,
        depth: usize,
        parent: Option<String>,
        features: Option<wry::NewWindowFeatures>,
    ) -> Result<objc2::rc::Retained<objc2_web_kit::WKWebView>, String> {
        let label = format!("vendor-inspector-{}", uuid::Uuid::new_v4());
        let size = features
            .as_ref()
            .and_then(|f| f.size)
            .map(|size| bounded_size([size.width, size.height]))
            .unwrap_or_else(|| {
                if depth == 0 {
                    (620.0, 720.0)
                } else {
                    bounded_size(tree.popup_size)
                }
            });
        let native = tauri::WindowBuilder::new(&tree.app, &label)
            .title(format!("Plugin inspector: {}", tree.lease.owner()))
            .inner_size(size.0, size.1)
            .min_inner_size(320.0, 240.0)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?;
        let mut cleanup = NativeCleanup(Some(native.clone()));
        let weak = Rc::downgrade(tree);
        let navigation = tree.lease.clone();
        let opener_label = label.clone();
        let mut builder = wry::WebViewBuilder::new()
            .with_incognito(true)
            .with_devtools(false)
            .with_clipboard(false)
            .with_back_forward_navigation_gestures(false)
            .with_allow_link_preview(false)
            .with_initialization_script(crate::vendor_file_picker::initialization_script())
            .with_navigation_handler(move |url| {
                navigation.is_active() && permits_url(navigation.url(), &url, depth > 0)
            })
            .with_new_window_req_handler(move |url, features| {
                popup(&weak, &opener_label, depth + 1, &url, features)
            })
            .with_download_started_handler(|_, _| false)
            .with_permission_handler(|_| wry::PermissionResponse::Deny);
        if let Some(features) = features {
            builder = builder.with_webview_configuration(features.opener.target_configuration);
        }
        let webview = match builder.build(&native) {
            Ok(view) => view,
            Err(error) => {
                let _ = native.destroy();
                return Err(error.to_string());
            }
        };
        let wk: objc2::rc::Retained<objc2_web_kit::WKWebView> = webview.webview().into_super();
        // Explicitly disable automatic script-created windows. The native
        // confirmation below is still required even for a genuine click.
        unsafe {
            let preferences = wk.configuration().preferences();
            preferences.setJavaScriptCanOpenWindowsAutomatically(false);
            preferences.setElementFullscreenEnabled(false);
        }
        let close_app = tree.app.clone();
        let close_token = tree.lease.token().to_owned();
        let close_label = label.clone();
        let picker = match crate::vendor_file_picker::install(&wk, tree.lease.clone(), move || {
            if depth == 0 {
                let _ = close_app.emit_to("main", "vendor-inspector-closed", &close_token);
            } else {
                let token = close_token.clone();
                let label = close_label.clone();
                defer(&close_app, move || close_subtree(&token, &label));
            }
        }) {
            Ok(picker) => picker,
            Err(error) => {
                drop(webview);
                let _ = native.destroy();
                return Err(error);
            }
        };
        if depth == 0 {
            webview
                .load_url(tree.lease.url())
                .map_err(|e| e.to_string())?;
        }
        let app = tree.app.clone();
        let token = tree.lease.token().to_owned();
        let closed_label = label.clone();
        native.on_window_event(move |event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } if depth == 0 => {
                // Let the editor finish already submitted immediate writes. It
                // then closes this entire tree through close_settings_panel.
                api.prevent_close();
                let _ = app.emit_to("main", "vendor-inspector-closed", &token);
            }
            tauri::WindowEvent::Destroyed => {
                let token = token.clone();
                let label = closed_label.clone();
                defer(&app, move || {
                    if depth == 0 {
                        close_main(&token);
                    } else {
                        close_subtree(&token, &label);
                    }
                });
            }
            _ => {}
        });
        native.show().map_err(|e| e.to_string())?;
        let _ = native.set_focus();
        tree.windows.borrow_mut().insert(
            label,
            Window {
                native: native.clone(),
                parent,
                _webview: webview,
                _picker: picker,
            },
        );
        cleanup.0 = None;
        Ok(wk)
    }
    // Handle the permitted inspector popup request
    fn popup(
        weak: &Weak<Tree>,
        opener_label: &str,
        depth: usize,
        url: &str,
        features: wry::NewWindowFeatures,
    ) -> wry::NewWindowResponse {
        let Some(tree) = weak.upgrade() else {
            return wry::NewWindowResponse::Deny;
        };
        if !tree.lease.is_active()
            || !tree.windows.borrow().contains_key(opener_label)
            || !permits_url(tree.lease.url(), url, true)
            || !tree.budget.borrow_mut().reserve(depth)
        {
            return wry::NewWindowResponse::Deny;
        }
        // NSAlert is a trusted, synchronous native decision. We never infer
        // activation from plugin JS, DOM events, timestamps or window features.
        // Reservations are made before the nested modal loop, preventing
        // re-entrant requests from bypassing count/depth/consent checks.
        let mtm = MainThreadMarker::new().expect("native inspector main thread");
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Allow a plugin window?"));
        let destination = if url == "about:blank" {
            "a blank window sharing this inspector".to_owned()
        } else {
            popup_destination(tree.lease.url(), url)
        };
        alert.setInformativeText(&NSString::from_str(&format!("Plugin {} wants to open {}. This window can access this plugin inspector's settings. It cannot access Manager. Approval applies only to this window, for this inspector session.",tree.lease.owner(),destination)));
        alert.addButtonWithTitle(&NSString::from_str("Allow window"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        #[cfg(feature = "native-inspector-probe")]
        let consent = if probe::injected_confirmation(tree.lease.owner()) {
            true
        } else {
            alert.runModal() == NSAlertFirstButtonReturn
        };
        #[cfg(not(feature = "native-inspector-probe"))]
        let consent = alert.runModal() == NSAlertFirstButtonReturn;
        let accepted = tree.budget.borrow_mut().decide(
            consent,
            tree.lease.is_active(),
            tree.windows.borrow().contains_key(opener_label),
        );
        if !accepted {
            return wry::NewWindowResponse::Deny;
        }
        match build_window(&tree, depth, Some(opener_label.into()), Some(features)) {
            Ok(webview) => wry::NewWindowResponse::Create { webview },
            Err(_) => {
                tree.budget.borrow_mut().release();
                wry::NewWindowResponse::Deny
            }
        }
    }
    // Close subtree
    fn close_subtree(token: &str, label: &str) {
        let tree = TREES.with(|trees| trees.borrow().get(token).cloned());
        let Some(tree) = tree else {
            return;
        };
        let removed = {
            let mut windows = tree.windows.borrow_mut();
            let parents = windows
                .iter()
                .map(|(id, window)| (id.clone(), window.parent.clone()))
                .collect();
            let mut removed = Vec::new();
            for label in subtree_labels(label, &parents) {
                if let Some(window) = windows.remove(&label) {
                    removed.push(window);
                    tree.budget.borrow_mut().release();
                }
            }
            removed
        };
        for window in removed {
            let _ = window.native.destroy();
        }
    }
    // Close main
    fn close_main(token: &str) {
        let tree = TREES
            .try_with(|trees| trees.borrow_mut().remove(token))
            .ok()
            .flatten();
        if let Some(tree) = tree {
            tree.lease.revoke();
            let windows = std::mem::take(&mut *tree.windows.borrow_mut());
            for (_, window) in windows {
                let _ = window.native.destroy();
            }
        }
    }
    #[cfg(feature = "native-inspector-probe")]
    pub mod probe {
        use super::*;
        use std::sync::{Arc, Mutex};
        thread_local! {
            static ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
            static INTERACTIVE_DIRECTORY: RefCell<Option<std::path::PathBuf>> = const { RefCell::new(None) };
        }
        // Check whether this inspector session is still active
        pub(super) fn active(owner: &str) -> bool {
            ACTIVE.with(|active| active.get()) && owner == "org.manager.disposable-probe"
        }
        // Check whether the active test inspector should use injected confirmation
        pub(super) fn injected_confirmation(owner: &str) -> bool {
            active(owner) && INTERACTIVE_DIRECTORY.with(|directory| directory.borrow().is_none())
        }
        // Resolve the initial directory for the file picker
        pub fn picker_directory(owner: &str) -> Option<std::path::PathBuf> {
            active(owner)
                .then(|| INTERACTIVE_DIRECTORY.with(|directory| directory.borrow().clone()))
                .flatten()
        }
        // Enable interactive
        pub async fn enable_interactive(
            app: &tauri::AppHandle,
            directory: std::path::PathBuf,
        ) -> Result<(), String> {
            let directory = directory.canonicalize().map_err(|e| e.to_string())?;
            if !directory.starts_with(
                std::env::temp_dir()
                    .canonicalize()
                    .map_err(|e| e.to_string())?,
            ) {
                return Err("Interactive fixture directory must be temporary".into());
            }
            let (send, receive) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move || {
                INTERACTIVE_DIRECTORY.with(|value| *value.borrow_mut() = Some(directory));
                ACTIVE.with(|active| active.set(true));
                let _ = send.send(());
            })
            .map_err(|e| e.to_string())?;
            receive.await.map_err(|e| e.to_string())
        }
        // Enable for the current operation
        pub async fn enable(app: &tauri::AppHandle) -> Result<(), String> {
            let (send, receive) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move || {
                INTERACTIVE_DIRECTORY.with(|value| *value.borrow_mut() = None);
                ACTIVE.with(|active| active.set(true));
                let _ = send.send(());
            })
            .map_err(|e| e.to_string())?;
            receive.await.map_err(|e| e.to_string())
        }
        // List the windows owned by the inspector
        pub async fn windows(app: &tauri::AppHandle) -> Result<Vec<serde_json::Value>, String> {
            let (send, receive) = tokio::sync::oneshot::channel();
            app.run_on_main_thread(move||{
                let value=TREES.with(|trees| trees.borrow().values().flat_map(|tree|tree.windows.borrow().iter().map(|(label,window)|{
                    let size=window.native.inner_size().unwrap().to_logical::<f64>(window.native.scale_factor().unwrap());
                    let wk=window._webview.webview();
                    let store=unsafe{wk.configuration().websiteDataStore()};
                    serde_json::json!({"label":label,"parent":window.parent,"width":size.width,"height":size.height,"persistent":unsafe{store.isPersistent()},"store":objc2::rc::Retained::as_ptr(&store) as usize,"automatic_popups":unsafe{wk.configuration().preferences().javaScriptCanOpenWindowsAutomatically()}})
                }).collect::<Vec<_>>()).collect());let _=send.send(value);
            }).map_err(|e|e.to_string())?;
            receive.await.map_err(|e| e.to_string())
        }
        // Evaluate the approved script in the inspector window
        pub async fn evaluate(
            app: &tauri::AppHandle,
            label: String,
            script: String,
        ) -> Result<serde_json::Value, String> {
            let (send, receive) = tokio::sync::oneshot::channel();
            let send = Arc::new(Mutex::new(Some(send)));
            app.run_on_main_thread(move || {
                let complete = send.clone();
                let result = TREES.with(|trees| {
                    for tree in trees.borrow().values() {
                        if !active(tree.lease.owner()) {
                            return Err("Probe session required".to_owned());
                        }
                        if let Some(window) = tree.windows.borrow().get(&label) {
                            // No real user activation or system permission is synthesized
                            // The dummy tree alone permits script requests so structural
                            // descendant creation can be exercised with injected consent.
                            if injected_confirmation(tree.lease.owner()) { unsafe {
                                window
                                    ._webview
                                    .webview()
                                    .configuration()
                                    .preferences()
                                    .setJavaScriptCanOpenWindowsAutomatically(true);
                            } }
                            let script=format!("JSON.stringify(eval({}))",serde_json::to_string(&script).unwrap());
                            let callback=block2::RcBlock::new(move|value:*mut objc2::runtime::AnyObject,error:*mut objc2_foundation::NSError| {
                                let result=if error.is_null(){unsafe{value.as_ref()}.and_then(|value|value.downcast_ref::<NSString>()).ok_or("Probe expression returned no JSON".to_string()).and_then(|text|serde_json::from_str(&text.to_string()).map_err(|e|e.to_string()))}else{Err("Native JavaScript evaluation failed".into())};
                                if let Some(send)=complete.lock().unwrap().take(){let _=send.send(result);}
                            });
                            unsafe{window._webview.webview().evaluateJavaScript_completionHandler(&NSString::from_str(&script),Some(&callback));}
                            return Ok(());
                        }
                    }
                    Err("Probe window missing".into())
                });
                if let Err(error) = result {
                    if let Some(send) = send.lock().unwrap().take() {
                        let _ = send.send(Err(error));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
            // Native modal interactions may legitimately hold a JavaScript callback
            tokio::time::timeout(Duration::from_secs(90), receive)
                .await
                .map_err(|_| "Probe JavaScript timeout".to_string())?
                .map_err(|e| e.to_string())?
        }
    }
    // Close all
    pub fn close_all(app: &tauri::AppHandle) {
        let _ = app.run_on_main_thread(|| {
            let tokens = TREES
                .try_with(|trees| trees.borrow().keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for token in tokens {
                close_main(&token);
            }
        });
    }
    // Close for the current operation
    pub fn close(app: &tauri::AppHandle, token: &str) {
        let token = token.to_owned();
        let _ = app.run_on_main_thread(move || close_main(&token));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify navigation stays inside one package origin
    fn navigation_stays_inside_one_package_origin() {
        let base = "http://127.0.0.1:4321/files/session/pi/index.html";
        for url in [
            base,
            "http://127.0.0.1:4321/files/session/config.html?option=1#tab",
            "http://127.0.0.1:4321/files/session/config%20page.html",
            "about:blank",
        ] {
            assert!(permits_url(base, url, true), "{url}");
        }
        for url in [
            "http://127.0.0.1:4322/files/session/pi.html",
            "http://127.0.0.1:4321/files/other/pi.html",
            "http://127.0.0.1:4321/files/session/../other/pi.html",
            "http://127.0.0.1:4321/files/session/%2f..%2fother/pi.html",
            "http://user@127.0.0.1:4321/files/session/pi.html",
            "https://example.com/",
            "tauri://localhost/",
            "ipc://localhost/",
            "javascript:alert(1)",
            "file:///tmp/pi.html",
            "data:text/html,hello",
            "blob:http://127.0.0.1:4321/value",
        ] {
            assert!(!permits_url(base, url, true), "{url}");
        }
        assert!(!permits_url(base, "about:blank", false));
        assert_eq!(
            popup_destination(
                base,
                "http://127.0.0.1:4321/files/session/config.html?secret=value"
            ),
            "config.html"
        );
    }
    #[test]
    // Verify popup size is finite and bounded for manifest and window features
    fn popup_size_is_finite_and_bounded_for_manifest_and_window_features() {
        assert_eq!(bounded_size([500.0, 650.0]), (500.0, 650.0));
        assert_eq!(bounded_size([-1.0, 1e99]), (320.0, 1000.0));
        assert_eq!(bounded_size([f64::NAN, f64::INFINITY]), (500.0, 650.0));
    }
    #[test]
    // Verify closing an opener retires its descendants not siblings
    fn closing_an_opener_retires_its_descendants_not_siblings() {
        let parents = std::collections::HashMap::from([
            ("root".into(), None),
            ("child".into(), Some("root".into())),
            ("grandchild".into(), Some("child".into())),
            ("sibling".into(), Some("root".into())),
        ]);
        let mut labels = subtree_labels("child", &parents);
        labels.sort();
        assert_eq!(labels, vec!["child", "grandchild"]);
        let mut labels = subtree_labels("root", &parents);
        labels.sort();
        assert_eq!(labels, vec!["child", "grandchild", "root", "sibling"]);
    }
    #[test]
    // Verify popup reservations bound reentrancy depth lifetime and denial
    fn popup_reservations_bound_reentrancy_depth_lifetime_and_denial() {
        let mut budget = WindowBudget {
            active: 1,
            created: 1,
            ..Default::default()
        };
        assert!(!budget.reserve(3));
        assert!(budget.reserve(1));
        assert!(!budget.reserve(1));
        budget.decide(true, true, true);
        assert!(budget.reserve(2));
        budget.decide(true, true, true);
        assert!(budget.reserve(2));
        budget.decide(true, true, true);
        assert!(!budget.reserve(1));
        for _ in 0..4 {
            budget.release();
            assert!(budget.reserve(1));
            budget.decide(true, true, true);
        }
        budget.release();
        assert!(!budget.reserve(1));
        for (session, opener) in [(false, true), (true, false), (false, false)] {
            let mut pending = WindowBudget::default();
            assert!(pending.reserve(1));
            assert!(!pending.decide(true, session, opener));
            assert_eq!(pending.active, 0);
        }
        let mut denied = WindowBudget::default();
        assert!(denied.reserve(1));
        denied.decide(false, true, true);
        assert_eq!(denied.active, 0);
        assert!(!denied.reserve(1));
    }
}

#[cfg(all(target_os = "macos", feature = "native-inspector-probe"))]
pub use macos::probe;
