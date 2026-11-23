//! A private source keeps the observer loop alive; queued stop blocks also cover pre-wait races.
use super::Signal;
use block2::{Block, RcBlock};
use std::{ffi::c_void, sync::Arc, time::Duration};
type Ref = *const c_void;
#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(Ref) -> Ref>,
    release: Option<unsafe extern "C" fn(Ref)>,
    description: Option<unsafe extern "C" fn(Ref) -> Ref>,
    equal: Option<unsafe extern "C" fn(Ref, Ref) -> u8>,
    hash: Option<unsafe extern "C" fn(Ref) -> usize>,
    schedule: Option<unsafe extern "C" fn(*mut c_void, Ref, Ref)>,
    cancel: Option<unsafe extern "C" fn(*mut c_void, Ref, Ref)>,
    perform: Option<unsafe extern "C" fn(*mut c_void)>,
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: Ref;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRunInMode(mode: *mut c_void, seconds: f64, return_after_source: bool) -> i32;
    fn CFRunLoopStop(run_loop: Ref);
    fn CFRunLoopWakeUp(run_loop: Ref);
    fn CFRunLoopPerformBlock(run_loop: Ref, mode: Ref, block: &Block<dyn Fn()>);
    fn CFRunLoopSourceCreate(allocator: Ref, order: isize, context: *mut c_void) -> Ref;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *mut c_void);
    fn CFRunLoopSourceInvalidate(source: Ref);
    fn CFRelease(value: Ref);
}
pub(super) struct NativeWait {
    source: Ref,
    signal: Arc<Signal>,
}
impl NativeWait {
    pub(super) fn new(signal: Arc<Signal>) -> Result<Self, String> {
        unsafe {
            let mut context: Context = std::mem::zeroed();
            let source =
                CFRunLoopSourceCreate(std::ptr::null(), 0, (&mut context as *mut Context).cast());
            if source.is_null() {
                return Err("Could not create vendor observer wake source".into());
            }
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(
                run_loop,
                source.cast_mut(),
                kCFRunLoopDefaultMode.cast_mut(),
            );
            let address = run_loop as usize;
            let pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
            signal.native(Some(Arc::new(move || {
                // Collapse demand bursts to one run-loop block without retaining runtime state
                if pending.swap(true, std::sync::atomic::Ordering::AcqRel) {
                    return;
                }
                let pending = pending.clone();
                let stop = RcBlock::new(move || {
                    pending.store(false, std::sync::atomic::Ordering::Release);
                    CFRunLoopStop(CFRunLoopGetCurrent());
                });
                CFRunLoopPerformBlock(address as Ref, kCFRunLoopDefaultMode, &stop);
                CFRunLoopWakeUp(address as Ref);
            })));
            Ok(Self { source, signal })
        }
    }
    pub(super) fn wait(&self, revision: u64, timeout: Duration) {
        if self.signal.stopped() || self.signal.revision() != revision {
            return;
        }
        // Queued blocks survive a wake between the revision check and run-loop entry.
        unsafe {
            CFRunLoopRunInMode(
                kCFRunLoopDefaultMode.cast_mut(),
                timeout.as_secs_f64(),
                true,
            );
        }
    }
}
impl Drop for NativeWait {
    fn drop(&mut self) {
        // Serialize unregistration with wake before invalidating the worker-owned source.
        self.signal.native(None);
        unsafe {
            CFRunLoopSourceInvalidate(self.source);
            CFRelease(self.source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Stop remains authoritative even after its wake was consumed before Runtime stopping is published
    fn stop_before_wait_is_sticky_at_current_generation() {
        let signal = Arc::new(Signal::default());
        let wait = NativeWait::new(signal.clone()).unwrap();
        signal.stop();
        let revision = signal.revision();
        assert!(signal.stopped());
        let start = std::time::Instant::now();
        wait.wait(revision, Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    // A native wait receives demand or shutdown without a periodic timer or real OS notification
    fn demand_and_shutdown_interrupt_native_wait() {
        for stop in [false, true] {
            let signal = Arc::new(Signal::default());
            let worker_signal = signal.clone();
            let (send, receive) = std::sync::mpsc::channel();
            let (done, completed) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let wait = NativeWait::new(worker_signal.clone()).unwrap();
                let revision = worker_signal.revision();
                send.send(()).unwrap();
                wait.wait(revision, Duration::from_secs(30));
                done.send(()).unwrap();
            });
            receive.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(completed.recv_timeout(Duration::from_millis(100)).is_err());
            if stop {
                signal.stop();
            } else {
                signal.notify();
            }
            completed.recv_timeout(Duration::from_secs(1)).unwrap();
            worker.join().unwrap();
        }
    }
    #[test]
    // Queued native notifications coalesce and stale generations skip entering the run loop
    fn pre_wait_notification_is_not_lost() {
        let signal = Arc::new(Signal::default());
        let wait = NativeWait::new(signal.clone()).unwrap();
        let revision = signal.revision();
        for _ in 0..100 {
            signal.notify();
        }
        wait.wait(revision, Duration::from_secs(30));
        assert_eq!(signal.revision(), revision + 100);
    }
}
