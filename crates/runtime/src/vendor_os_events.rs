//! Vendor application lifecycle is independent of foreground focus and device power
//! Only declared identities reach plugins. Never collect command lines or titles
use crate::Runtime;
use std::sync::Weak;

// Start the vendor-only demand monitor without changing core power or automation observers
pub(crate) fn start(runtime: Weak<Runtime>) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    std::thread::spawn(move || {
        if let Err(error) = observe(runtime.clone()) {
            if let Some(runtime) = runtime.upgrade().filter(|runtime| !runtime.is_stopping()) {
                runtime.emit("plugin-os-events-unavailable", None, None, Some(error));
            }
        }
    });
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let _ = runtime;
}

/// Own one observer generation and do not retry a failed registration until demand is renewed
struct DemandSlot<T> {
    value: Option<T>,
    attempted: bool,
}
impl<T> DemandSlot<T> {
    // Register once on demand, retain an active observer, and synchronously retire it at zero demand
    fn update(
        &mut self,
        wanted: bool,
        start: impl FnOnce() -> Result<T, String>,
    ) -> Result<(), String> {
        if !wanted {
            self.value = None;
            self.attempted = false;
        } else if !self.attempted {
            self.attempted = true;
            self.value = Some(start()?);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::{rc::Retained, runtime::ProtocolObject};
    use objc2_app_kit::{
        NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey,
        NSWorkspaceDidLaunchApplicationNotification,
        NSWorkspaceDidTerminateApplicationNotification, NSWorkspaceDidWakeNotification,
    };
    use objc2_foundation::{
        NSDate, NSNotification, NSNotificationCenter, NSObjectProtocol, NSRunLoop,
    };
    use std::{
        ptr::NonNull,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    struct Subscription {
        center: Retained<NSNotificationCenter>,
        tokens: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
        active: Arc<AtomicBool>,
    }
    impl Subscription {
        // Register only the requested vendor notification group without synthesizing initial events
        fn new(runtime: Weak<Runtime>, applications: bool) -> Self {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let active = Arc::new(AtomicBool::new(true));
            let mut tokens = Vec::new();
            let notifications = if applications {
                vec![
                    (
                        unsafe { NSWorkspaceDidLaunchApplicationNotification },
                        "applicationDidLaunch",
                    ),
                    (
                        unsafe { NSWorkspaceDidTerminateApplicationNotification },
                        "applicationDidTerminate",
                    ),
                ]
            } else {
                vec![(unsafe { NSWorkspaceDidWakeNotification }, "systemDidWakeUp")]
            };
            for (name, event) in notifications {
                let observed = runtime.clone();
                let admitted = active.clone();
                let callback =
                    block2::RcBlock::new(move |notification: NonNull<NSNotification>| {
                        if !admitted.load(Ordering::Acquire) {
                            return;
                        }
                        let Some(runtime) =
                            observed.upgrade().filter(|runtime| !runtime.is_stopping())
                        else {
                            return;
                        };
                        if !applications {
                            let plugins = runtime.plugins.lock().unwrap();
                            if admitted.load(Ordering::Acquire) {
                                plugins.emit_system_wake();
                            }
                            return;
                        }
                        let notification = unsafe { notification.as_ref() };
                        let Some(info) = notification.userInfo() else {
                            return;
                        };
                        let Some(value) =
                            info.objectForKey(unsafe { NSWorkspaceApplicationKey }.as_ref())
                        else {
                            return;
                        };
                        let Some(application) = value.downcast_ref::<NSRunningApplication>() else {
                            return;
                        };
                        let Some(identity) = application.bundleIdentifier() else {
                            return;
                        };
                        let plugins = runtime.plugins.lock().unwrap();
                        if admitted.load(Ordering::Acquire) {
                            plugins.emit_application_event(event, &identity.to_string());
                        }
                    });
                tokens.push(unsafe {
                    center.addObserverForName_object_queue_usingBlock(
                        Some(name),
                        None,
                        None,
                        &callback,
                    )
                });
            }
            Self {
                center,
                tokens,
                active,
            }
        }
    }
    impl Drop for Subscription {
        // Invalidate callbacks before unregistering their observer tokens
        fn drop(&mut self) {
            self.active.store(false, Ordering::Release);
            for token in &self.tokens {
                unsafe {
                    self.center.removeObserver((**token).as_ref());
                }
            }
        }
    }

    // Maintain separate application and wake subscriptions only while live plugin recipients need them
    pub(super) fn observe(runtime: Weak<Runtime>) -> Result<(), String> {
        let mut applications = DemandSlot {
            value: None,
            attempted: false,
        };
        let mut wake = DemandSlot {
            value: None,
            attempted: false,
        };
        loop {
            let Some(owner) = runtime.upgrade().filter(|runtime| !runtime.is_stopping()) else {
                return Ok(());
            };
            let (names, wants_wake) = owner.plugins.lock().unwrap().vendor_os_event_demand();
            drop(owner);
            applications.update(!names.is_empty(), || {
                Ok(Subscription::new(runtime.clone(), true))
            })?;
            wake.update(wants_wake, || Ok(Subscription::new(runtime.clone(), false)))?;
            let start = Instant::now();
            if applications.value.is_some() || wake.value.is_some() {
                objc2::rc::autoreleasepool(|_| {
                    NSRunLoop::currentRunLoop()
                        .runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.5));
                });
            }
            std::thread::sleep(Duration::from_millis(500).saturating_sub(start.elapsed()));
        }
    }
}
#[cfg(target_os = "macos")]
use macos::observe;

/// A complete snapshot contains only explicitly requested executable basenames
/// New subscriptions are baselined, not misreported as newly launched processes
#[cfg(any(target_os = "windows", target_os = "linux", test))]
#[derive(Default)]
struct ProcessChanges {
    previous: std::collections::BTreeMap<String, std::collections::BTreeSet<(u32, u64)>>,
}
#[cfg(any(target_os = "windows", target_os = "linux", test))]
impl ProcessChanges {
    // Apply the new vendor os events state
    fn update(
        &mut self,
        snapshot: std::collections::BTreeMap<String, std::collections::BTreeSet<(u32, u64)>>,
    ) -> Vec<(&'static str, String)> {
        let mut events = Vec::new();
        for (name, current) in &snapshot {
            let Some(previous) = self.previous.get(name) else {
                continue;
            };
            for _ in previous.difference(current) {
                events.push(("applicationDidTerminate", name.clone()));
            }
            for _ in current.difference(previous) {
                events.push(("applicationDidLaunch", name.clone()));
            }
        }
        self.previous = snapshot;
        events
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
// Maintain vendor wake registration and lazily sampled application process state
fn observe(runtime: Weak<Runtime>) -> Result<(), String> {
    use std::{
        collections::{BTreeMap, BTreeSet},
        time::{Duration, Instant},
    };
    use sysinfo::{ProcessesToUpdate, System};
    let mut system: Option<System> = None;
    let mut wake = DemandSlot {
        value: None,
        attempted: false,
    };
    let mut changes = ProcessChanges::default();
    let mut observed_names = BTreeMap::new();
    let mut next_snapshot = Instant::now();
    loop {
        let Some(current) = runtime.upgrade().filter(|runtime| !runtime.is_stopping()) else {
            return Ok(());
        };
        let (requested, wants_wake) = current.plugins.lock().unwrap().vendor_os_event_demand();
        if let Err(error) = wake.update(wants_wake, || {
            let (sender, receiver) = std::sync::mpsc::sync_channel(32);
            #[cfg(target_os = "linux")]
            let subscription = linux_wake::Subscription::new(runtime.clone(), sender)?;
            #[cfg(target_os = "windows")]
            let subscription = windows_wake::Subscription::new(sender)?;
            Ok((subscription, receiver))
        }) {
            current.emit("plugin-os-events-unavailable", None, None, Some(error));
        }
        if let Some((_, receiver)) = &wake.value {
            while receiver.try_recv().is_ok() {
                current.plugins.lock().unwrap().emit_system_wake();
            }
        }
        if requested.is_empty() {
            // Release the entire OS process table and baseline when the last application subscriber leaves
            system = None;
            changes.previous.clear();
            observed_names.clear();
            next_snapshot = Instant::now();
        } else if Instant::now() >= next_snapshot {
            let mut snapshot: BTreeMap<String, BTreeSet<(u32, u64)>> = requested
                .into_iter()
                .map(|name| (process_identity(&name), BTreeSet::new()))
                .collect();
            if !snapshot.is_empty() {
                if system.is_none() {
                    system = Some(System::new().map_err(|error| {
                        format!("Application lifecycle observation unavailable: {error}")
                    })?);
                }
                let system = system.as_mut().unwrap();
                system.refresh_processes_specifics(
                    ProcessesToUpdate::All,
                    true,
                    process_refresh_kind(),
                );
                // Do not turn a failed/over-limit observation into false terminations
                if system.processes().is_empty() || system.processes().len() > 32768 {
                    changes.previous.clear();
                    next_snapshot = Instant::now() + Duration::from_secs(1);
                    drop(current);
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                for (pid, process) in system.processes() {
                    // Linux comm can be truncated to 15 bytes. Use the actual
                    // executable basename without retaining its full path
                    #[cfg(target_os = "linux")]
                    let observed_name = process
                        .exe()
                        .and_then(std::path::Path::file_name)
                        .unwrap_or_else(|| process.name())
                        .to_string_lossy();
                    #[cfg(target_os = "windows")]
                    let observed_name = process.name().to_string_lossy();
                    let name = process_identity(&observed_name);
                    if let Some(instances) = snapshot.get_mut(&name) {
                        observed_names.insert(name.clone(), observed_name.into_owned());
                        instances.insert((pid.as_u32(), process.start_time()));
                    }
                }
            }
            for (event, application) in changes.update(snapshot) {
                current.plugins.lock().unwrap().emit_application_event(
                    event,
                    observed_names
                        .get(&application)
                        .map(String::as_str)
                        .unwrap_or(&application),
                );
            }
            observed_names.retain(|name, _| changes.previous.contains_key(name));
            next_snapshot = Instant::now() + Duration::from_secs(1);
        }
        drop(current);
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
// Select process counters without enumerating duplicate Linux task entries
fn process_refresh_kind() -> sysinfo::ProcessRefreshKind {
    let refresh = sysinfo::ProcessRefreshKind::nothing().without_tasks();
    #[cfg(target_os = "linux")]
    let refresh = refresh.with_exe(sysinfo::UpdateKind::OnlyIfNotSet);
    refresh
}

// Linux executable names are case-sensitive; Windows names are not
#[cfg(any(target_os = "windows", target_os = "linux", test))]
// Normalize process-name matching case only on Windows
fn process_identity(name: &str) -> String {
    if cfg!(target_os = "windows") {
        name.to_ascii_lowercase()
    } else {
        name.to_owned()
    }
}

#[cfg(target_os = "linux")]
mod linux_wake {
    use super::*;
    use futures_util::StreamExt;
    use std::{sync::mpsc::SyncSender, time::Duration};

    pub(super) struct Subscription {
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        worker: Option<std::thread::JoinHandle<()>>,
    }
    impl Subscription {
        // Observe actual logind resume signals only for the lifetime of this subscription
        pub(super) fn new(runtime: Weak<Runtime>, sender: SyncSender<()>) -> Result<Self, String> {
            let (stop, stopped) = tokio::sync::oneshot::channel();
            let worker = std::thread::Builder::new().name("vendor-system-wake".into()).spawn(move || {
                let result = (|| -> Result<(), String> {
                    let executor = tokio::runtime::Builder::new_current_thread()
                        .enable_all().build().map_err(|error| error.to_string())?;
                    executor.block_on(async {
                        // Cancellation covers connection and subscription setup as well as signal waits
                        tokio::select! {
                            biased;
                            _ = stopped => Ok(()),
                            result = async {
                                let connection = tokio::time::timeout(Duration::from_secs(5), zbus::Connection::system())
                                    .await.map_err(|_| "System bus connection timed out".to_string())?
                                    .map_err(|error| error.to_string())?;
                                let proxy = zbus::Proxy::new(&connection, "org.freedesktop.login1",
                                    "/org/freedesktop/login1", "org.freedesktop.login1.Manager")
                                    .await.map_err(|error| error.to_string())?;
                                let mut signals = proxy.receive_signal("PrepareForSleep").await.map_err(|error| error.to_string())?;
                                while let Some(message) = signals.next().await {
                                    let (sleeping,): (bool,) = message.body().deserialize().map_err(|error| error.to_string())?;
                                    if !sleeping { let _ = sender.try_send(()); }
                                }
                                Err("System wake signal stream closed".to_string())
                            } => result,
                        }
                    })
                })();
                if let Err(error) = result {
                    if let Some(current) = runtime.upgrade().filter(|r| !r.is_stopping()) {
                        current.emit("plugin-os-events-unavailable", None, None,
                            Some(format!("Linux system wake observation unavailable: {error}")));
                    }
                }
            }).map_err(|error| error.to_string())?;
            Ok(Self {
                stop: Some(stop),
                worker: Some(worker),
            })
        }
    }
    impl Drop for Subscription {
        // Cancel setup or signal waiting and join before another observer generation can start
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_wake {
    use std::{ffi::c_void, sync::mpsc::SyncSender};
    use windows::Win32::{
        Foundation::HANDLE,
        System::Power::{
            PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY,
        },
        UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC},
    };
    pub(super) struct Subscription {
        handle: HPOWERNOTIFY,
        sender: *mut SyncSender<()>,
    }
    // Route the native operating-system notification to runtime handling
    unsafe extern "system" fn notify(context: *const c_void, event: u32, _: *const c_void) -> u32 {
        if event == PBT_APMRESUMEAUTOMATIC && !context.is_null() {
            // Context is owned until successful unregister waits for pending callbacks
            let sender = unsafe { &*context.cast::<SyncSender<()>>() };
            let _ = sender.try_send(());
        }
        0
    }
    impl Subscription {
        // Initialize vendor os events state from the supplied inputs
        pub(super) fn new(sender: SyncSender<()>) -> Result<Self, String> {
            let sender = Box::into_raw(Box::new(sender));
            let mut parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
                Callback: Some(notify),
                Context: sender.cast(),
            };
            let mut handle = std::ptr::null_mut();
            let result = unsafe {
                PowerRegisterSuspendResumeNotification(
                    DEVICE_NOTIFY_CALLBACK,
                    HANDLE((&mut parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast()),
                    &mut handle,
                )
            };
            if result.0 != 0 {
                unsafe {
                    drop(Box::from_raw(sender));
                }
                return Err(format!(
                    "System wake observation unavailable (Windows error {})",
                    result.0
                ));
            }
            Ok(Self {
                handle: HPOWERNOTIFY(handle as isize),
                sender,
            })
        }
    }
    impl Drop for Subscription {
        // Release this vendor os events owner when it leaves scope
        fn drop(&mut self) {
            if unsafe { PowerUnregisterSuspendResumeNotification(self.handle) }.0 == 0 {
                unsafe {
                    drop(Box::from_raw(self.sender));
                }
            }
            // Failed unregister: retain callback context rather than risk use-after-free
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessChanges;
    use std::collections::{BTreeMap, BTreeSet};
    #[test]
    // Verify that executable identity preserves case except on Windows
    fn executable_identity_preserves_case_except_on_windows() {
        let name = "Example-Long-Application.exe";
        assert_eq!(
            super::process_identity(name),
            if cfg!(target_os = "windows") {
                "example-long-application.exe"
            } else {
                name
            }
        );
    }

    // Return a snapshot of vendor os events state
    fn snapshot(values: &[(&str, &[(u32, u64)])]) -> BTreeMap<String, BTreeSet<(u32, u64)>> {
        values
            .iter()
            .map(|(name, ids)| (name.to_string(), ids.iter().copied().collect()))
            .collect()
    }
    #[test]
    // Verify that startup subscription and unsubscribe do not fabricate lifecycle
    fn startup_subscription_and_unsubscribe_do_not_fabricate_lifecycle() {
        let mut changes = ProcessChanges::default();
        assert!(changes
            .update(snapshot(&[("allowed.exe", &[(1, 10)])]))
            .is_empty());
        assert!(changes
            .update(snapshot(&[
                ("allowed.exe", &[(1, 10)]),
                ("new.exe", &[(2, 20)])
            ]))
            .is_empty());
        assert!(changes
            .update(snapshot(&[("new.exe", &[(2, 20)])]))
            .is_empty());
        assert!(changes.update(BTreeMap::new()).is_empty());
    }
    #[test]
    // Verify that process identity detects restart and multiple instances without focus events
    fn process_identity_detects_restart_and_multiple_instances_without_focus_events() {
        let mut changes = ProcessChanges::default();
        changes.update(snapshot(&[("allowed.exe", &[(1, 10)])]));
        assert!(changes
            .update(snapshot(&[("allowed.exe", &[(1, 10)])]))
            .is_empty());
        assert_eq!(
            changes.update(snapshot(&[("allowed.exe", &[(1, 11), (2, 12)])])),
            vec![
                ("applicationDidTerminate", "allowed.exe".into()),
                ("applicationDidLaunch", "allowed.exe".into()),
                ("applicationDidLaunch", "allowed.exe".into())
            ]
        );
        assert_eq!(
            changes.update(snapshot(&[("allowed.exe", &[])])),
            vec![
                ("applicationDidTerminate", "allowed.exe".into()),
                ("applicationDidTerminate", "allowed.exe".into())
            ]
        );
    }
    #[test]
    // Create one observer per demand interval and discard queued events before resubscription
    fn demand_owns_one_observer_and_discards_retired_notifications() {
        use std::{cell::Cell, rc::Rc, sync::mpsc};
        struct Observer(Rc<Cell<usize>>);
        impl Drop for Observer {
            // Record synchronous teardown of the test observer
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }
        let drops = Rc::new(Cell::new(0));
        let mut slot = super::DemandSlot {
            value: None,
            attempted: false,
        };
        slot.update(
            false,
            || -> Result<(Observer, mpsc::Receiver<()>), String> {
                panic!("idle must not register")
            },
        )
        .unwrap();
        let (old_sender, receiver) = mpsc::channel();
        slot.update(true, || Ok((Observer(drops.clone()), receiver)))
            .unwrap();
        old_sender.send(()).unwrap();
        slot.update(true, || {
            panic!("unchanged demand must not duplicate observers")
        })
        .unwrap();
        slot.update(false, || panic!("retirement must not register"))
            .unwrap();
        assert_eq!(drops.get(), 1);
        assert!(old_sender.send(()).is_err());
        let (new_sender, receiver) = mpsc::channel();
        slot.update(true, || Ok((Observer(drops.clone()), receiver)))
            .unwrap();
        let receiver = &slot.value.as_ref().unwrap().1;
        assert!(receiver.try_recv().is_err());
        new_sender.send(()).unwrap();
        assert_eq!(receiver.try_iter().count(), 1);
        drop(slot);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    // Report a registration failure once and allow a fresh attempt after demand is renewed
    fn failed_subscription_retries_only_after_demand_restarts() {
        let mut slot = super::DemandSlot::<()> {
            value: None,
            attempted: false,
        };
        assert!(slot.update(true, || Err("unavailable".into())).is_err());
        slot.update(true, || panic!("failed registration must not busy-loop"))
            .unwrap();
        slot.update(false, || panic!("no idle registration"))
            .unwrap();
        slot.update(true, || Ok(())).unwrap();
        assert!(slot.value.is_some());
    }

    #[test]
    // Baseline a renewed application subscription rather than replay transitions from its idle gap
    fn application_resubscription_starts_with_a_fresh_baseline() {
        let mut changes = ProcessChanges::default();
        assert!(changes.update(snapshot(&[("app", &[(1, 10)])])).is_empty());
        assert!(changes.update(BTreeMap::new()).is_empty());
        assert!(changes.update(snapshot(&[("app", &[(2, 20)])])).is_empty());
        assert_eq!(
            changes.update(snapshot(&[("app", &[])])),
            vec![("applicationDidTerminate", "app".into())]
        );
    }
}
