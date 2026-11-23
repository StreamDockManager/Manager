//! Submit built-in notifications as the actual Manager application bundle on macOS

use block2::{DynBlock, RcBlock};
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationSetting,
    UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use std::cell::OnceCell;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

const REQUEST_PREFIX: &str = "app.streamdockmanager.manager.notification.";
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(15);
const UNAVAILABLE: &str = "macOS Notification Center is unavailable";
const DENIED: &str =
    "Notifications are disabled for this Manager app. Check System Settings → Notifications";
const BUNDLE_REQUIRED: &str = "Notifications require the Stream Dock Manager app bundle on macOS";
static APPROVAL_PENDING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, PartialEq, Eq)]
enum Posting {
    Ask,
    Allowed,
    Disabled,
    Unavailable,
}

// Treat a quiet Notification Center entry as available without promising a banner
fn posting(status: UNAuthorizationStatus, alert: bool, center: bool) -> Posting {
    match status {
        UNAuthorizationStatus::NotDetermined => Posting::Ask,
        UNAuthorizationStatus::Denied => Posting::Disabled,
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => {
            if alert || center {
                Posting::Allowed
            } else {
                Posting::Disabled
            }
        }
        _ => Posting::Unavailable,
    }
}

define_class!(
    // This delegate has no mutable state and accepts calls on the OS-selected callback thread
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[name = "SDMNativeNotificationDelegate"]
    struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}
    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        // Ask macOS to present only notification requests submitted by Manager
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            notification: &UNNotification,
            completion: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            let options =
                native(|| foreground_options(&notification.request().identifier().to_string()))
                    .unwrap_or_else(|_| UNNotificationPresentationOptions::empty());
            completion.call((options,));
        }
    }
);

thread_local! {
    // The native delegate property is weak; retain our instance on the application thread
    static FOREGROUND_DELEGATE: OnceCell<Retained<NotificationDelegate>> = const { OnceCell::new() };
}

// Restrict the foreground presentation policy to requests from this backend
fn foreground_options(identifier: &str) -> UNNotificationPresentationOptions {
    if identifier
        .strip_prefix(REQUEST_PREFIX)
        .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
    {
        UNNotificationPresentationOptions::List | UNNotificationPresentationOptions::Banner
    } else {
        UNNotificationPresentationOptions::empty()
    }
}

// Set the delegate during app setup, without requesting permission or posting anything
pub(crate) fn install_foreground_delegate() -> Result<(), String> {
    if MainThreadMarker::new().is_none() {
        return Err("The macOS notification delegate must be installed during app setup".into());
    }
    native(check_bundle)??;
    native(|| {
        FOREGROUND_DELEGATE.with(|delegate| {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            if center.delegate().is_none() {
                let delegate = delegate.get_or_init(|| {
                    // NSObject initialization has no additional arguments or requirements
                    unsafe { msg_send![super(NotificationDelegate::alloc().set_ivars(())), init] }
                });
                center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
            }
        })
    })
}

// Admit only the shipped or sanctioned development app, not a raw Cargo/CLI binary
fn manager_bundle(identifier: &str, root: &Path, executable: &Path) -> bool {
    matches!(
        identifier,
        "app.streamdockmanager.manager" | "app.streamdockmanager.manager.localdev"
    ) && root.extension().is_some_and(|extension| extension == "app")
        && executable
            .parent()
            .is_some_and(|parent| parent == root.join("Contents/MacOS"))
}

// Resolve the real process bundle without changing a process-wide sender override
fn check_bundle() -> Result<(), String> {
    let bundle = NSBundle::mainBundle();
    let id = bundle.bundleIdentifier().ok_or(BUNDLE_REQUIRED)?;
    let root = bundle.bundleURL().to_file_path().ok_or(BUNDLE_REQUIRED)?;
    let exe = bundle
        .executableURL()
        .and_then(|url| url.to_file_path())
        .ok_or(BUNDLE_REQUIRED)?;
    if !manager_bundle(&id.to_string(), &root, &exe) {
        return Err(BUNDLE_REQUIRED.into());
    }
    Ok(())
}

// Keep Objective-C failures private; exceptions and userInfo can include caller data
fn native<T>(call: impl FnOnce() -> T) -> Result<T, String> {
    objc2::exception::catch(AssertUnwindSafe(call)).map_err(|_| UNAVAILABLE.into())
}

// Inspect permission for this exact bundle; callback extracts only a small, safe state
fn settings(center: &UNUserNotificationCenter) -> Result<Posting, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |value: NonNull<UNNotificationSettings>| {
        // The completion owns a live settings object for the duration of this callback
        let value = unsafe { value.as_ref() };
        let state = posting(
            value.authorizationStatus(),
            value.alertSetting() == UNNotificationSetting::Enabled,
            value.notificationCenterSetting() == UNNotificationSetting::Enabled,
        );
        let _ = tx.try_send(state);
    });
    native(|| center.getNotificationSettingsWithCompletionHandler(&callback))?;
    drop(callback);
    rx.recv_timeout(RESPONSE_TIMEOUT)
        .map_err(|_| "Could not check macOS notification permissions in time".into())
}

// Request a normal alert only for a real notification action and share the pending prompt
fn request_authorization(center: &UNUserNotificationCenter) -> Result<(), String> {
    if APPROVAL_PENDING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("A macOS notification permission request is already pending".into());
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        APPROVAL_PENDING.store(false, Ordering::Release);
        let result = if !error.is_null() {
            Err("macOS could not request notification permission")
        } else if !granted.as_bool() {
            Err(DENIED)
        } else {
            Ok(())
        };
        let _ = tx.try_send(result);
    });
    if let Err(error) = native(|| {
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &callback,
        )
    }) {
        APPROVAL_PENDING.store(false, Ordering::Release);
        return Err(error);
    }
    drop(callback);
    // A slow prompt may outlive this action. Never schedule the stale action on late approval
    rx.recv_timeout(APPROVAL_TIMEOUT)
        .map_err(|_| "Respond to the macOS notification permission prompt, then try again")?
        .map_err(str::to_owned)
}

// Wait for acceptance from the OS, not delivery or presentation to the user
fn schedule(center: &UNUserNotificationCenter, title: &str, text: &str) -> Result<(), String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |error: *mut NSError| {
        let result = if error.is_null() {
            Ok(())
        } else {
            Err("macOS did not accept the notification. Check System Settings → Notifications")
        };
        let _ = tx.try_send(result);
    });
    native(|| {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(text));
        let id = NSString::from_str(&format!("{REQUEST_PREFIX}{}", uuid::Uuid::new_v4()));
        let request =
            UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
        center.addNotificationRequest_withCompletionHandler(&request, Some(&callback));
    })?;
    drop(callback);
    // A timeout does not mean the native request was rejected; avoid duplicate retries
    rx.recv_timeout(RESPONSE_TIMEOUT)
        .map_err(|_| "Could not confirm that macOS accepted the notification")?
        .map_err(str::to_owned)
}

// Built-in actions run off the main thread; leave the app event loop free to show permission UI
pub(crate) fn submit(title: &str, text: &str) -> Result<(), String> {
    submit_inner(title, text, true)
}

// Service alerts must not initiate a new OS permission prompt in the background
pub(crate) fn submit_without_prompt(title: &str, text: &str) -> Result<(), String> {
    submit_inner(title, text, false)
}

// Preflight optional service alerts without prompting or inspecting notification contents
pub(crate) fn authorized_without_prompt() -> bool {
    with_center(|center| settings(center).map(|state| state == Posting::Allowed)).unwrap_or(false)
}

// Submit after checking the current settings; only an actual action may request access
fn submit_inner(title: &str, text: &str, may_prompt: bool) -> Result<(), String> {
    with_center(|center| {
        let mut state = settings(center)?;
        if state == Posting::Ask && may_prompt {
            request_authorization(center)?;
            state = settings(center)?;
        }
        match state {
            Posting::Allowed => schedule(center, title, text),
            Posting::Disabled => Err(DENIED.into()),
            Posting::Ask => Err("macOS has not granted notification permission".into()),
            Posting::Unavailable => Err(UNAVAILABLE.into()),
        }
    })
}

// Leave the main event loop free and scope autoreleased native objects to the worker
fn with_center<T>(
    operation: impl FnOnce(&UNUserNotificationCenter) -> Result<T, String>,
) -> Result<T, String> {
    if MainThreadMarker::new().is_some() {
        return Err("Native notification submission requires a background action".into());
    }
    autoreleasepool(|_| {
        native(check_bundle)??;
        let center = native(UNUserNotificationCenter::currentNotificationCenter)?;
        operation(&center)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_notifications_use_only_a_real_manager_app_bundle() {
        let root = Path::new("/tmp/Stream Dock Manager Local Dev.app");
        let executable = root.join("Contents/MacOS/stream-dock-manager");
        for id in [
            "app.streamdockmanager.manager",
            "app.streamdockmanager.manager.localdev",
        ] {
            assert!(manager_bundle(id, root, &executable));
        }
        assert!(!manager_bundle("com.apple.Terminal", root, &executable));
        assert!(!manager_bundle(
            "app.streamdockmanager.manager.localdev",
            Path::new("/tmp/target/debug"),
            Path::new("/tmp/target/debug/stream-dock-manager"),
        ));
        assert!(!manager_bundle(
            "app.streamdockmanager.manager",
            root,
            Path::new("/tmp/target/release/stream-dock-manager"),
        ));
    }

    #[test]
    fn macos_notifications_present_only_their_own_requests_in_the_foreground() {
        let id = uuid::Uuid::new_v4();
        let policy = foreground_options(&format!("{REQUEST_PREFIX}{id}"));
        assert!(policy.contains(UNNotificationPresentationOptions::List));
        assert!(policy.contains(UNNotificationPresentationOptions::Banner));
        assert!(!policy.contains(UNNotificationPresentationOptions::Sound));
        assert_eq!(
            foreground_options(&id.to_string()),
            UNNotificationPresentationOptions::empty()
        );
        assert_eq!(
            foreground_options(&format!("{REQUEST_PREFIX}foreign")),
            UNNotificationPresentationOptions::empty()
        );
    }

    #[test]
    fn macos_notifications_distinguish_approval_from_presentation() {
        assert_eq!(
            posting(UNAuthorizationStatus::NotDetermined, false, false),
            Posting::Ask
        );
        assert_eq!(
            posting(UNAuthorizationStatus::Denied, true, true),
            Posting::Disabled
        );
        assert_eq!(
            posting(UNAuthorizationStatus::Authorized, false, false),
            Posting::Disabled
        );
        assert_eq!(
            posting(UNAuthorizationStatus::Authorized, true, false),
            Posting::Allowed
        );
        assert_eq!(
            posting(UNAuthorizationStatus::Provisional, false, true),
            Posting::Allowed
        );
        assert_eq!(
            posting(UNAuthorizationStatus(99), true, true),
            Posting::Unavailable
        );
    }
}
