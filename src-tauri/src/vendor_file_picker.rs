//! Explicit native file selection for IPC-free vendor inspectors on macOS
//! The delegate forwards all other WebKit UI behavior to Wry's original delegate
use block2::{Block, RcBlock};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    DefinedClass, MainThreadOnly,
};
use objc2_app_kit::{NSAlert, NSModalResponseOK, NSOpenPanel};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSError, NSNumber, NSObject, NSObjectProtocol, NSString, NSURL,
};
use objc2_web_kit::{WKContentWorld, WKFrameInfo, WKOpenPanelParameters, WKUIDelegate, WKWebView};
use std::ptr;
use stream_dock_manager_plugin_host::InspectorBrowser;

// Build the restricted file picker initialization script
pub fn initialization_script() -> &'static str {
    include_str!("vendor_file_input.js")
}

pub struct PickerDelegateHandle {
    _delegate: Retained<PickerDelegate>,
}
struct PickerIvars {
    original: Retained<ProtocolObject<dyn WKUIDelegate>>,
    session: InspectorBrowser,
    close: Box<dyn Fn()>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PickerIvars]
    struct PickerDelegate;
    unsafe impl NSObjectProtocol for PickerDelegate {
        #[unsafe(method(respondsToSelector:))]
        // Check whether the native object responds to the requested selector
        fn responds(&self, selector: Sel) -> bool {
            let own: bool = unsafe { msg_send![super(self), respondsToSelector: selector] };
            own || self.ivars().original.respondsToSelector(selector)
        }
    }
    impl PickerDelegate {
        #[unsafe(method(forwardingTargetForSelector:))]
        // Forward the permitted file picker event
        fn forward(&self, _selector: Sel) -> *mut AnyObject {
            Retained::as_ptr(&self.ivars().original).cast_mut().cast()
        }
    }
    unsafe impl WKUIDelegate for PickerDelegate {
        #[unsafe(method(webViewDidClose:))]
        // Clean up state after the file picker closes
        fn did_close(&self, _webview: &WKWebView) { (self.ivars().close)(); }
        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        // Select for the current operation
        fn select(&self, webview: &WKWebView, parameters: &WKOpenPanelParameters, frame: &WKFrameInfo, completion: &Block<dyn Fn(*const NSArray<NSURL>)>) {
            self.select_resources(webview, parameters, frame, completion);
        }
    }
);
impl PickerDelegate {
    // Select resources
    fn select_resources(
        &self,
        webview: &WKWebView,
        parameters: &WKOpenPanelParameters,
        frame: &WKFrameInfo,
        completion: &Block<dyn Fn(*const NSArray<NSURL>)>,
    ) {
        let session = &self.ivars().session;
        // The exact initiating frame, not the opener or current main window,
        // receives the selection. Remote frames and directories fail closed.
        let allowed = unsafe { frame.request().URL() }
            .and_then(|url| url.absoluteString())
            .is_some_and(|url| permitted_url(session, &url.to_string()));
        if !allowed || !session.is_active() || unsafe { parameters.allowsDirectories() } {
            completion.call((ptr::null(),));
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            completion.call((ptr::null(),));
            return;
        };
        let panel = NSOpenPanel::openPanel(mtm);
        // The opt-in standalone fixture starts directly in its generated files
        // Ordinary Manager sessions have no caller that enables this hook
        #[cfg(feature = "native-inspector-probe")]
        if let Some(directory) = crate::vendor_inspectors::probe::picker_directory(session.owner())
        {
            panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(
                &directory.to_string_lossy(),
            ))));
        }
        panel.setTitle(Some(&NSString::from_str("Choose plugin files")));
        panel.setMessage(Some(&NSString::from_str(&format!("Share selected files with {}. Up to 32 files per inspector, 16 MiB per file, and 64 MiB total. Directories are not shared.", session.owner()))));
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(unsafe { parameters.allowsMultipleSelection() });
        if panel.runModal() != NSModalResponseOK || !session.is_active() {
            completion.call((ptr::null(),));
            return;
        }
        let urls = panel.URLs();
        let paths: Option<Vec<_>> = urls
            .iter()
            .map(|url| {
                url.path()
                    .map(|path| std::path::PathBuf::from(path.to_string()))
            })
            .collect();
        let Some(paths) = paths else {
            completion.call((ptr::null(),));
            return;
        };
        let selected = match session.grant_selected_resources(&paths) {
            Ok(selected) => selected,
            Err(error) => {
                completion.call((ptr::null(),));
                if session.is_active() {
                    let alert = NSAlert::new(mtm);
                    alert.setMessageText(&NSString::from_str(
                        "The selected files could not be shared",
                    ));
                    alert.setInformativeText(&NSString::from_str(&error));
                    alert.runModal();
                }
                return;
            }
        };
        let prefix = format!(
            "{}/files/{}/",
            session.url().split_once("/files/").unwrap().0,
            session.token()
        );
        let script = format!(
            "location.href.startsWith({}) && window.__streamDockSelectedFile({})",
            serde_json::to_string(&prefix).unwrap(),
            serde_json::to_string(&selected).unwrap()
        );
        let completion = completion.copy();
        let session = session.clone();
        let handler = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
            let accepted = error.is_null()
                && !result.is_null()
                && session.is_active()
                && unsafe { result.as_ref() }
                    .and_then(|value| value.downcast_ref::<NSNumber>())
                    .is_some_and(NSNumber::boolValue);
            completion.call((if accepted {
                Retained::as_ptr(&urls)
            } else {
                ptr::null()
            },));
        });
        unsafe {
            webview.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(&script),
                Some(frame),
                &WKContentWorld::pageWorld(mtm),
                Some(&handler),
            );
        }
    }
}

// Check whether navigation remains within the permitted picker origin
fn permitted_url(session: &InspectorBrowser, target: &str) -> bool {
    let Some((origin, _)) = session.url().split_once("/files/") else {
        return false;
    };
    target.starts_with(&format!("{origin}/files/{}/", session.token()))
}
// Install for the current operation
pub fn install(
    webview: &WKWebView,
    session: InspectorBrowser,
    close: impl Fn() + 'static,
) -> Result<PickerDelegateHandle, String> {
    let mtm = MainThreadMarker::new().ok_or("Inspector file panel requires the UI thread")?;
    let original = unsafe { webview.UIDelegate() }.ok_or("Inspector UI delegate is unavailable")?;
    let allocated = mtm.alloc::<PickerDelegate>().set_ivars(PickerIvars {
        original,
        session,
        close: Box::new(close),
    });
    let delegate: Retained<PickerDelegate> = unsafe { msg_send![super(allocated), init] };
    unsafe {
        webview.setUIDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }
    Ok(PickerDelegateHandle {
        _delegate: delegate,
    })
}
