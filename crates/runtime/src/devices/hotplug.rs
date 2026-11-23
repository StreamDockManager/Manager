//! macOS HID device arrival and removal notifications without opening device handles

use block2::{Block, RcBlock};
use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
use objc2_foundation::NSNotification;
use std::{
    ffi::c_void,
    ptr::{null, NonNull},
    sync::{mpsc, Arc},
    thread::{self, JoinHandle},
    time::Duration,
};

type Object = *const c_void;
type HidCallback = unsafe extern "C" fn(*mut c_void, i32, *mut c_void, *mut c_void);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDManagerCreate(allocator: Object, options: u32) -> Object;
    fn IOHIDManagerSetDeviceMatching(manager: Object, matching: Object);
    fn IOHIDManagerRegisterDeviceMatchingCallback(
        manager: Object,
        callback: Option<HidCallback>,
        context: *mut c_void,
    );
    fn IOHIDManagerRegisterDeviceRemovalCallback(
        manager: Object,
        callback: Option<HidCallback>,
        context: *mut c_void,
    );
    fn IOHIDManagerScheduleWithRunLoop(manager: Object, run_loop: Object, mode: Object);
    fn IOHIDManagerUnscheduleFromRunLoop(manager: Object, run_loop: Object, mode: Object);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: Object;
    fn CFRetain(object: Object) -> Object;
    fn CFRelease(object: Object);
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRun();
    fn CFRunLoopStop(run_loop: Object);
    fn CFRunLoopPerformBlock(run_loop: Object, mode: Object, block: &Block<dyn Fn()>);
    fn CFRunLoopWakeUp(run_loop: Object);
}

// Own a retained run loop so the observer can enqueue shutdown from another thread
struct RetainedRunLoop(NonNull<c_void>);

// Core Foundation run-loop scheduling, wakeup and reference counting are thread-safe
unsafe impl Send for RetainedRunLoop {}

impl Drop for RetainedRunLoop {
    // Release the retained run-loop reference
    fn drop(&mut self) {
        // SAFETY: start retained this run loop exactly once for this wrapper
        unsafe { CFRelease(self.0.as_ptr()) }
    }
}

pub(super) struct Observer {
    run_loop: RetainedRunLoop,
    worker: Option<JoinHandle<()>>,
}

impl Observer {
    // Start the observer and bound how long the coordinator waits for registration
    pub(super) fn start(callback: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let (ready, started) = mpsc::sync_channel(0);
        let worker = thread::Builder::new()
            .name("sdm-hid-hotplug".into())
            .spawn(move || observe(callback, ready))
            .ok()?;
        let run_loop = started.recv_timeout(Duration::from_secs(1)).ok()?;
        Some(Self {
            run_loop,
            worker: Some(worker),
        })
    }

    // Detect an unexpected observer exit so the coordinator can fall back to polling
    pub(super) fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }
}

impl Drop for Observer {
    // Stop on the run-loop thread and wait for it to unregister the callbacks
    fn drop(&mut self) {
        // Queue a stop rather than stopping directly so shutdown cannot race initial run entry
        let stop = RcBlock::new(|| {
            // SAFETY: this block executes on the observer's run loop
            unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) }
        });
        // SAFETY: the observer retains the run loop and Core Foundation copies the block
        unsafe {
            CFRunLoopPerformBlock(self.run_loop.0.as_ptr(), kCFRunLoopDefaultMode, &stop);
            CFRunLoopWakeUp(self.run_loop.0.as_ptr());
        }
        // The worker only references the callback while its own run loop is running
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Translate device notifications into a coalescible coordinator wake
unsafe extern "C" fn changed<F: Fn()>(
    context: *mut c_void,
    result: i32,
    _sender: *mut c_void,
    _device: *mut c_void,
) {
    if result == 0 {
        // SAFETY: observe registers a pointer to its boxed callback until it unschedules
        notify(unsafe { &**context.cast::<Arc<F>>() });
    }
}

// Never allow a panic to unwind across native callbacks
fn notify(callback: &impl Fn()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback));
}

// Own registration and callbacks on a single run-loop thread
fn observe<F: Fn() + Send + Sync + 'static>(callback: F, ready: mpsc::SyncSender<RetainedRunLoop>) {
    let mut callback = Box::new(Arc::new(callback));
    let context = (&mut *callback as *mut Arc<F>).cast::<c_void>();
    let wake_callback = Arc::clone(&callback);
    let wake = RcBlock::new(move |_: NonNull<NSNotification>| notify(&*wake_callback));
    // SAFETY: the manager, run loop and callback context live until teardown below
    unsafe {
        let manager = IOHIDManagerCreate(null(), 0);
        if manager.is_null() {
            return;
        }
        let run_loop = CFRunLoopGetCurrent();
        let mode = kCFRunLoopDefaultMode;
        IOHIDManagerSetDeviceMatching(manager, null());
        IOHIDManagerRegisterDeviceMatchingCallback(manager, Some(changed::<F>), context);
        IOHIDManagerRegisterDeviceRemovalCallback(manager, Some(changed::<F>), context);
        // Scheduling registers notifications without opening HID devices or reading reports
        IOHIDManagerScheduleWithRunLoop(manager, run_loop, mode);
        let retained = RetainedRunLoop(
            NonNull::new(CFRetain(run_loop).cast_mut()).expect("current CFRunLoop exists"),
        );
        // Subscribe independently of whether any plugin asks for wake events
        let (center, token) = objc2::rc::autoreleasepool(|_| {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let token = center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidWakeNotification),
                None,
                None,
                &wake,
            );
            (center, token)
        });
        // Rendezvous ensures a timed-out caller cannot leave a detached observer running
        if ready.send(retained).is_ok() {
            CFRunLoopRun();
        }
        center.removeObserver((*token).as_ref());
        IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, mode);
        CFRelease(manager);
    }
}
