use super::*;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_core_location::{CLLocation, CLLocationManager, CLLocationManagerDelegate};
use objc2_foundation::{MainThreadMarker, NSArray, NSError, NSObject, NSObjectProtocol};
use std::{cell::RefCell, sync::mpsc};

define_class!(
    // NSObject has no subclassing requirements. All delegate access is on the
    // application's main run loop, including creation and destruction.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = mpsc::Sender<Result<Fix,String>>]
    struct LocationDelegate;
    unsafe impl NSObjectProtocol for LocationDelegate {}
    unsafe impl CLLocationManagerDelegate for LocationDelegate {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        // Handle a new native location observation
        fn updated(&self, manager: &CLLocationManager, locations: &NSArray<CLLocation>) {
            if let Some(location) = locations.lastObject() {
                let result = unsafe {
                    let coordinate=location.coordinate();
                    validate_fix(Fix { latitude:coordinate.latitude,longitude:coordinate.longitude,accuracy_m:location.horizontalAccuracy(),timestamp:location.timestamp().timeIntervalSince1970() })
                };
                // Cached stale fixes may arrive before the fresh position
                if result.is_ok() { let _=self.ivars().send(result); unsafe { manager.stopUpdatingLocation(); } }
            }
        }
        #[unsafe(method(locationManager:didFailWithError:))]
        // Handle the native operation failure
        fn failed(&self, manager: &CLLocationManager, error: &NSError) {
            // kCLErrorLocationUnknown is transient; wait for a fix or timeout.
            if error.code() == 0 { return; }
            let message = if error.code()==1 { "Location access denied. Enable Stream Dock Manager in System Settings → Privacy & Security → Location Services." } else { "Location services could not determine a position. Try again or enter coordinates." };
            let _=self.ivars().send(Err(message.into()));
            unsafe { manager.stopUpdatingLocation(); }
        }
    }
);
type ActiveLocation = (Retained<CLLocationManager>, Retained<LocationDelegate>);
thread_local! { static ACTIVE: RefCell<HashMap<usize, ActiveLocation>> = RefCell::new(HashMap::new()); }
// Read the current value
pub async fn current(
    app: tauri::AppHandle,
    cancel: Arc<AtomicBool>,
    permit: &crate::editor_lifecycle::EditorWork,
) -> Result<Fix, String> {
    let (tx, rx) = mpsc::channel();
    let id = Arc::as_ptr(&cancel) as usize;
    let flag = cancel.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let before_start = finished.clone();
    let setup_permit = permit.worker();
    app.run_on_main_thread(move || {
        let _editor_work = setup_permit;
        if flag.load(Ordering::SeqCst) || before_start.load(Ordering::SeqCst) {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            let _ = tx.send(Err("Location requires the main application thread".into()));
            return;
        };
        unsafe {
            if !CLLocationManager::locationServicesEnabled_class() {
                let _ = tx.send(Err(
                    "Location services are disabled in System Settings".into()
                ));
                return;
            }
            let delegate: Retained<LocationDelegate> =
                msg_send![super(LocationDelegate::alloc(mtm).set_ivars(tx)), init];
            let manager = CLLocationManager::new();
            manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            manager.setDesiredAccuracy(1000.0);
            manager.requestWhenInUseAuthorization();
            manager.startUpdatingLocation();
            ACTIVE.with(|active| {
                active.borrow_mut().insert(id, (manager, delegate));
            });
        }
    })
    .map_err(|e| e.to_string())?;
    let result = crate::editor_lifecycle::spawn_blocking(permit, move || {
        let end = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("Location request cancelled".into());
            }
            if std::time::Instant::now() >= end {
                return Err("Location request timed out. Try again or enter coordinates.".into());
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("Location service unavailable".into()),
            }
        }
    })
    .await
    .map_err(|e| e.to_string());
    finished.store(true, Ordering::SeqCst);
    // The delegate is retained until updates stop, including cancellation
    let cleanup_permit = permit.worker();
    let _ = app.run_on_main_thread(move || {
        let _editor_work = cleanup_permit;
        ACTIVE.with(|active| {
            if let Some((manager, _delegate)) = active.borrow_mut().remove(&id) {
                unsafe {
                    manager.stopUpdatingLocation();
                    manager.setDelegate(None);
                }
            }
        })
    });
    result?
}
