//! AppKit's cooperative termination path bypasses Tauri's ExitRequested on Tao 0.37
//! Add the missing quit method and a fullscreen notification selector; preserve the
//! delegate object, class ivars and every Tao callback. Logout/restart uses AppKit.
use super::{EditorLifecycle, MAX_SEQUENCE};
use objc2::runtime::{AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel, MainThreadMarker};
use std::sync::{Arc, Mutex, OnceLock};
use stream_dock_manager_runtime::Runtime;
use tauri::Manager;

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static TERMINATION: Mutex<Termination> = Mutex::new(Termination {
    sequence: 0,
    pending: None,
});

#[derive(Clone, Copy, Debug)]
struct Pending {
    ticket: u64,
    action: Option<(u64, u64)>,
}
#[derive(Default, Debug)]
struct Termination {
    sequence: u64,
    pending: Option<Pending>,
}
impl Termination {
    // Begin for the current operation
    fn begin(&mut self) -> Option<(u64, bool)> {
        if let Some(pending) = self.pending {
            return Some((pending.ticket, false));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|n| *n <= MAX_SEQUENCE)?;
        self.pending = Some(Pending {
            ticket: self.sequence,
            action: None,
        });
        Some((self.sequence, true))
    }
    // Read the current value
    fn current(&self, ticket: u64) -> bool {
        self.pending.is_some_and(|pending| pending.ticket == ticket)
    }
    // Bind for the current operation
    fn bind(&mut self, generation: u64, action: u64) {
        if let Some(pending) = &mut self.pending {
            pending.action = Some((generation, action));
        }
    }
    // Take for the current operation
    fn take(&mut self) -> bool {
        self.pending.take().is_some()
    }
    // Check whether the native callback matches the pending action
    fn matches_action(&self, generation: u64, action: u64) -> bool {
        self.pending
            .is_some_and(|pending| pending.action == Some((generation, action)))
    }
}

// Install for the current operation
pub fn install(app: &tauri::AppHandle) -> Result<(), String> {
    let _main = MainThreadMarker::new().ok_or("Native quit setup requires the main thread")?;
    // SAFETY: NSApplication is the process singleton and its live delegate is owned
    // by Tao for the entire event loop. We neither replace nor release that object.
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![application, delegate];
        let delegate = delegate
            .as_ref()
            .ok_or("Native application delegate is missing")?;
        let class = delegate.class();
        let selector = sel!(applicationShouldTerminate:);
        if class.instance_method(selector).is_some() {
            return Err("The native application already owns a termination delegate; quit coordination requires review".into());
        }
        APP.set(app.clone())
            .map_err(|_| "Native quit bridge was already installed")?;
        // All supported macOS targets are 64-bit. NSUInteger is Q; the method's
        // remaining arguments are self, _cmd, NSApplication*. The static function
        // has that exact ABI and outlives the Objective-C class registration.
        let implementation: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize,
            Imp,
        >(should_terminate);
        if !objc2::ffi::class_addMethod(
            class as *const _ as *mut _,
            selector,
            implementation,
            c"Q@:@".as_ptr(),
        )
        .as_bool()
        {
            return Err("Could not install native quit coordination".into());
        }
        let exited = sel!(streamDockManagerWindowDidExitFullScreen:);
        let implementation: Imp = std::mem::transmute::<
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject),
            Imp,
        >(did_exit_fullscreen);
        if !objc2::ffi::class_addMethod(
            class as *const _ as *mut _,
            exited,
            implementation,
            c"v@:@".as_ptr(),
        )
        .as_bool()
        {
            return Err("Could not install native fullscreen observation".into());
        }
        let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let name: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"NSWindowDidExitFullScreenNotification".as_ptr()];
        let _: () = msg_send![center, addObserver: delegate, selector: exited, name: name, object: std::ptr::null::<AnyObject>()];
    }
    Ok(())
}

// Resume the pending transition after native fullscreen exits
unsafe extern "C-unwind" fn did_exit_fullscreen(
    _delegate: *mut AnyObject,
    _selector: Sel,
    notification: *mut AnyObject,
) {
    let Some(app) = APP.get() else { return };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    // SAFETY: this selector is registered only for NSWindow's documented exit
    // notification. Compare native object identity, not the reused main label.
    let origin: *mut AnyObject = unsafe { msg_send![notification, object] };
    let origin = origin as usize;
    if window.ns_window().ok().map(|pointer| pointer as usize) != Some(origin) {
        return;
    }
    let generation = app
        .state::<Arc<EditorLifecycle>>()
        .owner
        .lock()
        .unwrap()
        .generation;
    let app = app.clone();
    // Other observers (including Tao) still have exit work to finish. Dispatch
    // from the worker so the main-thread callback cannot run inline here.
    tauri::async_runtime::spawn(async move {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            super::restore_normal_after_fullscreen(&handle, generation, origin);
        });
    });
}

// Check whether this operation should terminate
unsafe extern "C-unwind" fn should_terminate(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut AnyObject,
) -> usize {
    let Some(app) = APP.get() else { return 1 }; // NSTerminateNow, outside setup ownership.
    let Some(runtime) = app.try_state::<Arc<Runtime>>() else {
        return 1;
    };
    if runtime.is_stopping() {
        return 1;
    }
    let Some((ticket, start)) = TERMINATION.lock().unwrap().begin() else {
        return 0;
    };
    if start {
        {
            let core = app.state::<Arc<EditorLifecycle>>();
            let mut owner = core.owner.lock().unwrap();
            owner.pending_quit = true;
        }
        let app = app.clone();
        // The callback must return NSTerminateLater before any YES/NO reply, including
        // the no-window case. A task on Tauri's worker runtime queues onto the UI loop.
        tauri::async_runtime::spawn(async move {
            let handle = app.clone();
            if let Err(error) = app.run_on_main_thread(move || {
                if TERMINATION.lock().unwrap().current(ticket) {
                    super::request_quit(&handle);
                }
            }) {
                eprintln!("Could not dispatch native quit coordination: {error}");
            }
        });
    }
    2 // NSTerminateLater. Repeated native requests share the same bounded ticket.
}

// Bind action
pub fn bind_action(generation: u64, action: u64) {
    TERMINATION.lock().unwrap().bind(generation, action);
}

// Cancel action
pub fn cancel_action(generation: u64, action: u64) {
    if TERMINATION
        .lock()
        .unwrap()
        .matches_action(generation, action)
    {
        reply(false);
    }
}

/// Only UI-thread callers may finish AppKit's pending request. Taking the ticket
/// before the message makes reentrant callbacks and duplicate acknowledgments safe.
pub fn reply(allow: bool) -> bool {
    assert!(
        MainThreadMarker::new().is_some(),
        "Native termination reply requires the main thread"
    );
    if !TERMINATION.lock().unwrap().take() {
        return false;
    }
    // SAFETY: same process singleton; AppKit's documented BOOL argument is encoded
    // by objc2. No delegate/window/runtime locks are held while AppKit calls back.
    unsafe {
        let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![application, replyToApplicationShouldTerminate: allow];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify native requests coalesce and complete once
    fn native_requests_coalesce_and_complete_once() {
        let mut state = Termination::default();
        let (ticket, start) = state.begin().unwrap();
        assert!(start);
        assert_eq!(state.begin(), Some((ticket, false)));
        state.bind(2, 8);
        assert!(state.matches_action(2, 8));
        assert!(!state.matches_action(1, 8));
        assert!(!state.matches_action(2, 7));
        assert!(state.take());
        assert!(!state.take());
        assert!(!state.current(ticket));
        let (next, start) = state.begin().unwrap();
        assert!(start && next > ticket);
        assert!(!state.matches_action(2, 8));
    }

    #[test]
    // Verify canceled native dispatch and stale failed flush do not affect new request
    fn canceled_native_dispatch_and_stale_failed_flush_do_not_affect_new_request() {
        let mut state = Termination::default();
        let old = state.begin().unwrap().0;
        state.bind(1, 1);
        assert!(state.take()); // Reveal/failed flush replies NO.
        let new = state.begin().unwrap().0;
        state.bind(1, 2);
        assert!(!state.current(old));
        assert!(state.current(new));
        assert!(!state.matches_action(1, 1));
        assert!(state.matches_action(1, 2));
        assert!(state.take()); // Exactly one later successful reply YES.
        state.sequence = MAX_SEQUENCE;
        assert!(state.begin().is_none());
    }
}
