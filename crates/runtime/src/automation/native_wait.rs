//! A private source keeps the observer loop alive; queued stop blocks also cover pre-wait races.
use super::ObservationSignal;
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
    signal: Arc<ObservationSignal>,
}
impl NativeWait {
    pub(super) fn new(signal: Arc<ObservationSignal>) -> Result<Self, String> {
        unsafe {
            let mut context: Context = std::mem::zeroed();
            let source =
                CFRunLoopSourceCreate(std::ptr::null(), 0, (&mut context as *mut Context).cast());
            if source.is_null() {
                return Err("Could not create foreground observer wake source".into());
            }
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(
                run_loop,
                source.cast_mut(),
                kCFRunLoopDefaultMode.cast_mut(),
            );
            let address = run_loop as usize;
            signal.native(Some(Arc::new(move || {
                let stop = RcBlock::new(|| CFRunLoopStop(CFRunLoopGetCurrent()));
                CFRunLoopPerformBlock(address as Ref, kCFRunLoopDefaultMode, &stop);
                CFRunLoopWakeUp(address as Ref);
            })));
            Ok(Self { source, signal })
        }
    }
    pub(super) fn wait(&self, revision: u64, timeout: Duration) {
        if self.signal.configuration() != revision {
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
    fn config_wake_before_native_wait_is_retained() {
        let signal = Arc::new(ObservationSignal::default());
        let wait = NativeWait::new(signal.clone()).unwrap();
        let revision = signal.configuration();
        signal.wake();
        let start = std::time::Instant::now();
        wait.wait(revision, Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn config_wake_unblocks_native_wait_after_entry() {
        let signal = Arc::new(ObservationSignal::default());
        let wait = NativeWait::new(signal.clone()).unwrap();
        let revision = signal.configuration();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            signal.wake();
        });
        let start = std::time::Instant::now();
        wait.wait(revision, Duration::from_secs(30));
        assert!(start.elapsed() < Duration::from_secs(2));
        worker.join().unwrap();
    }
}

#[cfg(test)]
mod idle_test {
    use super::*;
    #[test]
    fn native_source_parks_until_deadline_without_spin() {
        let signal = Arc::new(ObservationSignal::default());
        let wait = NativeWait::new(signal.clone()).unwrap();
        let start = std::time::Instant::now();
        wait.wait(signal.configuration(), Duration::from_millis(80));
        assert!(start.elapsed() >= Duration::from_millis(60));
    }
}
