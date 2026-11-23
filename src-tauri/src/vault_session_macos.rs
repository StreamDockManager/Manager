//! End the current Vault session on desktop security and sleep transitions.
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSApplicationProtectedDataWillBecomeUnavailableNotification, NSWorkspace,
    NSWorkspaceDidWakeNotification, NSWorkspaceScreensDidSleepNotification,
    NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};
use std::{cell::RefCell, ptr::NonNull, sync::Arc};
use stream_dock_manager_runtime::vault::Vault;

struct Observer {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Drop for Observer {
    fn drop(&mut self) {
        unsafe {
            self.center.removeObserver(self.token.as_ref());
        }
    }
}

thread_local! {
    static OBSERVERS: RefCell<Vec<Observer>> = const { RefCell::new(Vec::new()) };
}

// Tauri setup runs on the main application thread. Observer callbacks own only a weak Vault.
pub fn start(vault: &Arc<Vault>) {
    let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
    let application = NSNotificationCenter::defaultCenter();
    OBSERVERS.with_borrow_mut(|observers| {
        // Releasing the existing tokens also makes repeated desktop setup idempotent.
        observers.clear();
        unsafe {
            for (center, name) in [
                (&workspace, NSWorkspaceWillSleepNotification),
                (&workspace, NSWorkspaceDidWakeNotification),
                (&workspace, NSWorkspaceScreensDidSleepNotification),
                (&workspace, NSWorkspaceSessionDidResignActiveNotification),
                (
                    &application,
                    NSApplicationProtectedDataWillBecomeUnavailableNotification,
                ),
            ] {
                let weak = Arc::downgrade(vault);
                let callback = block2::RcBlock::new(move |_: NonNull<NSNotification>| {
                    if let Some(vault) = weak.upgrade() {
                        vault.end_session();
                    }
                });
                observers.push(Observer {
                    center: center.clone(),
                    token: center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        None,
                        &callback,
                    ),
                });
            }
        }
    });
}
