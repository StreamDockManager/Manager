//! AX window notifications plus NSWorkspace app activation form a sticky target
//! generation. Both are scoped to one playback and removed before returning
use super::super::focus::{Observer, State};
use super::{ax_result, OwnedRef, Ref, Target};
use objc2_app_kit::{NSWorkspace, NSWorkspaceDidActivateApplicationNotification};
use objc2_foundation::NSNotification;
use std::{
    ffi::c_void,
    ptr,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXObserverCreate(
        pid: i32,
        callback: unsafe extern "C" fn(Ref, Ref, Ref, *mut c_void),
        result: *mut Ref,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: Ref,
        element: Ref,
        notification: Ref,
        context: *mut c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: Ref) -> Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *mut c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *mut c_void);
    fn CFRunLoopRemoveSource(run_loop: *mut c_void, source: *mut c_void, mode: *mut c_void);
    fn CFRunLoopRunInMode(mode: *mut c_void, seconds: f64, return_after_source: bool) -> i32;
}
struct OnDrop<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> Drop for OnDrop<F> {
    // Release this macOS focus owner when it leaves scope
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}
struct AxSource {
    _observer: OwnedRef,
    source: *mut c_void,
    run_loop: *mut c_void,
}
impl Drop for AxSource {
    // Release this macOS focus owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            CFRunLoopRemoveSource(self.run_loop, self.source, kCFRunLoopDefaultMode);
        }
    }
}
// Observe native focus changes and invalidate the captured target as needed
unsafe extern "C" fn changed(_: Ref, _: Ref, _: Ref, context: *mut c_void) {
    if let Some(signal) = context.cast::<State>().as_ref() {
        signal.changed();
    }
}
// Watch for macOS focus changes
fn watch(observer: Ref, element: Ref, name: &str, signal: &Arc<State>) -> Result<(), String> {
    unsafe {
        let notification = name;
        let name = OwnedRef::new(crate::feedback::cf_string(name))?;
        ax_result(
            &format!("subscribing to {notification}"),
            AXObserverAddNotification(
                observer,
                element,
                name.0,
                Arc::as_ptr(signal).cast_mut().cast(),
            ),
        )?;
    }
    Ok(())
}
// Start the macOS focus operation
pub(super) fn start() -> Result<Observer, String> {
    Observer::start(run)
}
// Drive the macOS focus operation
fn run(signal: Arc<State>, startup: mpsc::SyncSender<Result<(), String>>) -> Result<(), String> {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let activated = signal.clone();
    let callback = block2::RcBlock::new(move |_: ptr::NonNull<NSNotification>| activated.changed());
    let token = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidActivateApplicationNotification),
            None,
            None,
            &callback,
        )
    };
    let _notification = OnDrop(Some(|| unsafe {
        center.removeObserver((*token).as_ref());
    }));
    // Capture the target after the global application observer is installed
    let target = Target::current()?;
    let mut pid = 0;
    ax_result("verifying target process", unsafe {
        AXUIElementGetPid(target.application.0, &mut pid)
    })?;
    if pid <= 0 {
        return Err("Keyboard macro target process cannot be verified".into());
    }
    let mut observer = ptr::null();
    ax_result("creating target observer", unsafe {
        AXObserverCreate(pid, changed, &mut observer)
    })?;
    let observer = OwnedRef::new(observer)?;
    watch(
        observer.0,
        target.application.0,
        "AXFocusedWindowChanged",
        &signal,
    )?;
    watch(observer.0, target.window.0, "AXUIElementDestroyed", &signal)?;
    let source = unsafe { AXObserverGetRunLoopSource(observer.0) }.cast_mut();
    if source.is_null() {
        return Err("Keyboard macro target observer has no event source".into());
    }
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    unsafe {
        CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);
    }
    let _source = AxSource {
        _observer: observer,
        source,
        run_loop,
    };
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, false);
    }
    if !target.matches(&Target::current()?) {
        signal.changed();
    }
    signal.ready();
    startup
        .send(Ok(()))
        .map_err(|_| "Keyboard macro target observer was canceled")?;
    let mut verified = Instant::now();
    while !signal.stopped() {
        objc2::rc::autoreleasepool(|_| unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, true);
        });
        // Notifications latch even A -> B -> A between these health samples
        if signal.needs_check() || verified.elapsed() >= Duration::from_millis(100) {
            let request = signal.requested_check();
            if !target.matches(&Target::current()?) {
                signal.changed();
            }
            // Target::current just completed the bounded focused-window read;
            // a second identical IPC query adds latency but no new identity proof
            signal.complete_check(request);
            verified = Instant::now();
        }
        signal.pulse();
    }
    Ok(())
}
