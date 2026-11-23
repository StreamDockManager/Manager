//! Desktop notifications use the application's main run loop. Headless hosts
//! retain a sparse native-read fallback; no subprocess is used in either mode.
use super::ObserverSignal;
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock, Weak},
};
type Ref = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFPreferencesAnyApplication: Ref;
    fn CFStringCreateWithCString(
        allocator: Ref,
        value: *const std::ffi::c_char,
        encoding: u32,
    ) -> Ref;
    fn CFEqual(left: Ref, right: Ref) -> u8;
    fn CFRelease(value: Ref);
    fn CFPreferencesCopyAppValue(key: Ref, application: Ref) -> Ref;
    fn CFPreferencesAppSynchronize(application: Ref) -> u8;
    fn CFNotificationCenterGetDistributedCenter() -> Ref;
    fn CFNotificationCenterAddObserver(
        center: Ref,
        observer: Ref,
        callback: unsafe extern "C" fn(Ref, *mut c_void, Ref, Ref, Ref),
        name: Ref,
        object: Ref,
        behavior: isize,
    );
    fn CFNotificationCenterRemoveObserver(center: Ref, observer: Ref, name: Ref, object: Ref);
}
pub(super) fn appearance() -> Option<bool> {
    // Copy/Create objects are released within this scope; a missing global style
    // is the normal Light setting. Reads happen on events or the headless fallback.
    unsafe {
        let key = CFStringCreateWithCString(
            std::ptr::null(),
            c"AppleInterfaceStyle".as_ptr(),
            0x08000100,
        );
        let dark = CFStringCreateWithCString(std::ptr::null(), c"Dark".as_ptr(), 0x08000100);
        if key.is_null() || dark.is_null() {
            if !key.is_null() {
                CFRelease(key);
            }
            if !dark.is_null() {
                CFRelease(dark);
            }
            return None;
        }
        CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
        let value = CFPreferencesCopyAppValue(key, kCFPreferencesAnyApplication);
        let result = !value.is_null() && CFEqual(value, dark) != 0;
        if !value.is_null() {
            CFRelease(value);
        }
        CFRelease(key);
        CFRelease(dark);
        Some(result)
    }
}

fn observers() -> &'static Mutex<HashMap<usize, Weak<ObserverSignal>>> {
    static OBSERVERS: OnceLock<Mutex<HashMap<usize, Weak<ObserverSignal>>>> = OnceLock::new();
    OBSERVERS.get_or_init(Mutex::default)
}
unsafe extern "C" fn changed(_: Ref, observer: *mut c_void, _: Ref, _: Ref, _: Ref) {
    // Treat observer as an opaque non-reused identity, never a dereferenced
    // pointer. Removal races cannot leave a callback with freed host memory.
    let _ = std::panic::catch_unwind(|| {
        let signal = observers()
            .lock()
            .unwrap()
            .get(&(observer as usize))
            .and_then(Weak::upgrade);
        if let Some(signal) = signal {
            signal.wake();
        }
    });
}
pub(super) struct Observer {
    id: usize,
    center: Ref,
    name: Ref,
}
impl Observer {
    pub(super) fn start(signal: Arc<ObserverSignal>) -> Result<Self, String> {
        static NEXT_ID: Mutex<usize> = Mutex::new(1);
        let id = {
            let mut next = NEXT_ID.lock().unwrap();
            let id = *next;
            *next = next
                .checked_add(1)
                .ok_or("Appearance observer identity exhausted")?;
            id
        };
        unsafe {
            let center = CFNotificationCenterGetDistributedCenter();
            let name = CFStringCreateWithCString(
                std::ptr::null(),
                c"AppleInterfaceThemeChangedNotification".as_ptr(),
                0x08000100,
            );
            if center.is_null() || name.is_null() {
                if !name.is_null() {
                    CFRelease(name);
                }
                return Err("Unable to create native appearance observer".into());
            }
            observers()
                .lock()
                .unwrap()
                .insert(id, Arc::downgrade(&signal));
            // DeliverImmediately: also notify while the desktop is tray-hidden.
            // CF delivers this on the application's main run loop, not here.
            CFNotificationCenterAddObserver(center, id as Ref, changed, name, std::ptr::null(), 4);
            Ok(Self { id, center, name })
        }
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        observers().lock().unwrap().remove(&self.id);
        unsafe {
            CFNotificationCenterRemoveObserver(
                self.center,
                self.id as Ref,
                self.name,
                std::ptr::null(),
            );
            CFRelease(self.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_is_event_only_and_headless_or_failed_registration_is_sparse() {
        use std::sync::atomic::Ordering;
        let signal = ObserverSignal::default();
        assert_eq!(
            signal.poll_interval(true),
            Some(std::time::Duration::from_secs(60))
        );
        assert_eq!(signal.poll_interval(false), None);
        signal.main_run_loop.store(true, Ordering::SeqCst);
        assert_eq!(
            signal.poll_interval(true),
            Some(std::time::Duration::from_secs(60))
        );
        signal.native_registered.store(true, Ordering::SeqCst);
        assert_eq!(signal.poll_interval(true), None);
    }

    #[test]
    fn native_callback_wakes_demand_wait_without_a_polling_timer() {
        let signal = Arc::new(ObserverSignal::default());
        signal
            .main_run_loop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        signal
            .native_registered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (send, receive) = std::sync::mpsc::channel();
        let worker_signal = signal.clone();
        let worker = std::thread::spawn(move || {
            worker_signal.wait(0, true, &stop);
            send.send(()).unwrap();
        });
        assert!(receive
            .recv_timeout(std::time::Duration::from_millis(2200))
            .is_err());
        // Exercise the callback directly without posting a global notification
        // or modifying the user's system appearance/running applications.
        let observer = Observer::start(signal.clone()).unwrap();
        unsafe {
            changed(
                std::ptr::null(),
                observer.id as *mut c_void,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
        }
        receive
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn late_native_callback_after_unregister_does_not_wake_host() {
        let signal = Arc::new(ObserverSignal::default());
        let observer = Observer::start(signal.clone()).unwrap();
        let id = observer.id;
        drop(observer);
        let before = *signal.revision.lock().unwrap();
        unsafe {
            changed(
                std::ptr::null(),
                id as *mut c_void,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
        }
        assert_eq!(*signal.revision.lock().unwrap(), before);
    }

    #[test]
    fn native_appearance_read_and_observer_registration_cleanup() {
        assert!(appearance().is_some());
        for _ in 0..8 {
            let observer = Observer::start(Arc::new(ObserverSignal::default())).unwrap();
            drop(observer);
        }
    }
}
