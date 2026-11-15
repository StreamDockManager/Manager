//! AX lifetime pinning lives on the test-service worker. No unsafe Send for AX
//! objects, no global hook and no focus stealing. Live prerequisites remain
//! fail-closed; session observations use the disclosed best-effort lock signal
use super::TestTarget;
use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSDate;
use std::{cell::Cell, ffi::c_void, marker::PhantomData, ptr, rc::Rc};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, Uid, UpdateKind};

type Ref = *const c_void;
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> Ref;
    fn AXUIElementCopyAttributeValue(element: Ref, name: Ref, result: *mut Ref) -> i32;
    fn AXUIElementSetMessagingTimeout(element: Ref, seconds: f32) -> i32;
    fn AXUIElementGetPid(element: Ref, pid: *mut i32) -> i32;
    fn AXUIElementGetTypeID() -> usize;
    fn AXObserverCreate(
        pid: i32,
        callback: unsafe extern "C" fn(Ref, Ref, Ref, *mut c_void),
        result: *mut Ref,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: Ref,
        element: Ref,
        name: Ref,
        context: *mut c_void,
    ) -> i32;
    fn AXObserverRemoveNotification(observer: Ref, element: Ref, name: Ref) -> i32;
    fn AXObserverGetRunLoopSource(observer: Ref) -> Ref;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *mut c_void;
    fn CFRetain(value: Ref) -> Ref;
    fn CFRelease(value: Ref);
    fn CFEqual(first: Ref, second: Ref) -> bool;
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *mut c_void);
    fn CFRunLoopRemoveSource(run_loop: *mut c_void, source: *mut c_void, mode: *mut c_void);
    fn CFRunLoopSourceIsValid(source: Ref) -> bool;
    fn CFRunLoopRunInMode(mode: *mut c_void, seconds: f64, return_after_source: bool) -> i32;
}

// Construct the macOS test target error result
fn error() -> String {
    "The selected scratch window is no longer verifiable. Select it again.".into()
}
struct Owned(Ref);
impl Owned {
    // Initialize macOS test target state from the supplied inputs
    fn new(value: Ref) -> Result<Self, String> {
        if value.is_null() {
            Err(error())
        } else {
            Ok(Self(value))
        }
    }
    // Adopt and validate an owned accessibility element with a bounded messaging timeout
    fn element(value: Ref) -> Result<Self, String> {
        let result = Self::new(value)?;
        if unsafe { CFGetTypeID(result.0) != AXUIElementGetTypeID() } {
            return Err(error());
        }
        if unsafe { AXUIElementSetMessagingTimeout(result.0, 0.05) } != 0 {
            return Err(
                "The scratch application's accessibility response cannot be bounded.".into(),
            );
        }
        Ok(result)
    }
}
impl Drop for Owned {
    // Release this macOS test target owner when it leaves scope
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}
// Read an owned Accessibility attribute from the target element
fn attribute(element: Ref, name: &str) -> Result<Owned, String> {
    let name = Owned::new(unsafe { crate::feedback::cf_string(name) })?;
    let mut value = ptr::null();
    if unsafe { AXUIElementCopyAttributeValue(element, name.0, &mut value) } != 0 {
        return Err(error());
    }
    Owned::new(value)
}
// Require an Accessibility attribute to contain an element
fn element_attribute(element: Ref, name: &str) -> Result<Owned, String> {
    let value = attribute(element, name)?;
    // A second retained reference gives element() its own create-rule ownership
    Owned::element(unsafe { CFRetain(value.0) })
}
// Read a textual Accessibility attribute
fn text_attribute(element: Ref, name: &str) -> Result<String, String> {
    let value = attribute(element, name)?;
    unsafe { crate::feedback::cf_to_string(value.0) }.ok_or_else(error)
}
// Read the process identifier owning the accessibility element
fn pid(element: Ref) -> Result<i32, String> {
    let mut pid = 0;
    if unsafe { AXUIElementGetPid(element, &mut pid) } != 0 || pid <= 0 {
        return Err(error());
    }
    Ok(pid)
}
// Require existing Accessibility permission and safe native session observations
fn native_context() -> Result<(), String> {
    if !crate::automation::title_permission(false) {
        return Err("Scratch-target selection requires existing Accessibility permission.".into());
    }
    crate::keyboard_macro::capture::check_macos_session_observations().map_err(|error| {
        format!("Scratch-target session observations are unavailable or protected: {error}")
    })?;
    Ok(())
}

// Report the foreground PIDs match condition for macOS test target
fn foreground_pids_match(before: i32, application: i32, window: i32, after: i32) -> bool {
    before > 0 && application == before && window == before && after == before
}

struct Identity {
    application: Owned,
    window: Owned,
    pid: i32,
}
impl Identity {
    // Capture a consistent foreground application and window identity
    fn foreground() -> Result<Self, String> {
        native_context()?;
        // Setting an AX timeout on a system-wide element changes the timeout
        // globally for this process. Resolve the foreground application through
        // AppKit instead, then bound only our application/window AX objects
        let workspace = NSWorkspace::sharedWorkspace();
        let foreground = workspace.frontmostApplication().ok_or_else(error)?;
        let pid = foreground.processIdentifier();
        if pid <= 0 || foreground.isTerminated() {
            return Err(error());
        }
        if pid as u32 == std::process::id() {
            return Err("Select a scratch application outside Manager.".into());
        }
        let application = Owned::element(unsafe { AXUIElementCreateApplication(pid) })?;
        let window = element_attribute(application.0, "AXFocusedWindow")?;
        let application_pid = self::pid(application.0)?;
        let window_pid = self::pid(window.0)?;
        let role = text_attribute(window.0, "AXRole")?;
        // Do not accept a window read across a foreground-app change. Keeping
        // the NSRunningApplication identity also rejects a same-PID relaunch
        let current = workspace.frontmostApplication().ok_or_else(error)?;
        if foreground.isTerminated()
            || current != foreground
            || !foreground_pids_match(
                pid,
                application_pid,
                window_pid,
                current.processIdentifier(),
            )
            || role != "AXWindow"
        {
            return Err(error());
        }
        Ok(Self {
            application,
            window,
            pid,
        })
    }
    // Report the matches condition for macOS test target
    fn matches(&self, other: &Self) -> bool {
        self.pid == other.pid
            && unsafe {
                CFEqual(self.application.0, other.application.0)
                    && CFEqual(self.window.0, other.window.0)
            }
    }
}

// Record destruction of the pinned native target
unsafe extern "C" fn destroyed(_: Ref, _: Ref, _: Ref, context: *mut c_void) {
    if let Some(invalid) = context.cast::<Cell<bool>>().as_ref() {
        invalid.set(true);
    }
}
struct Lifetime {
    // Fields are released in declaration order after Drop removes the source
    observer: Option<Owned>,
    window: Owned,
    name: Owned,
    source: *mut c_void,
    run_loop: *mut c_void,
    invalid: Box<Cell<bool>>,
    _thread: PhantomData<Rc<()>>,
}
impl Lifetime {
    // Install the selected macOS test target content
    fn install(target: &Identity) -> Result<Self, String> {
        let mut observer = ptr::null();
        if unsafe { AXObserverCreate(target.pid, destroyed, &mut observer) } != 0 {
            return Err("The scratch window cannot provide lifetime notifications.".into());
        }
        let observer = Owned::new(observer)?;
        let window = Owned::new(unsafe { CFRetain(target.window.0) })?;
        let name = Owned::new(unsafe { crate::feedback::cf_string("AXUIElementDestroyed") })?;
        let invalid = Box::new(Cell::new(false));
        let source = unsafe { AXObserverGetRunLoopSource(observer.0) }.cast_mut();
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        if source.is_null() || run_loop.is_null() || !unsafe { CFRunLoopSourceIsValid(source) } {
            return Err(error());
        }
        if unsafe {
            AXObserverAddNotification(
                observer.0,
                window.0,
                name.0,
                (&*invalid as *const Cell<bool>).cast_mut().cast(),
            )
        } != 0
        {
            return Err(
                "The scratch window cannot provide reliable destruction notifications.".into(),
            );
        }
        unsafe { CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode) };
        Ok(Self {
            observer: Some(observer),
            window,
            name,
            source,
            run_loop,
            invalid,
            _thread: PhantomData,
        })
    }
    // Pump pending macOS test target events
    fn pump(&self) -> Result<(), String> {
        if self.observer.is_none() || unsafe { CFRunLoopGetCurrent() } != self.run_loop {
            self.invalid.set(true);
            return Err(error());
        }
        // Drain queued destruction events before checking retained AX identity
        // A busy/stopped/invalid observer fails closed rather than deferring events
        for _ in 0..256 {
            if !unsafe { CFRunLoopSourceIsValid(self.source) } || self.invalid.get() {
                self.invalid.set(true);
                return Err(error());
            }
            let result = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, true) };
            match result {
                3 => {
                    return if self.invalid.get() {
                        Err(error())
                    } else {
                        Ok(())
                    }
                }
                4 => continue,
                _ => {
                    self.invalid.set(true);
                    return Err(error());
                }
            }
        }
        self.invalid.set(true);
        Err("Scratch-window observer queue overflowed. Select the target again.".into())
    }
    // Finalize the pending macOS test target operation
    fn finish(&mut self) -> Result<(), String> {
        self.invalid.set(true);
        let Some(observer) = self.observer.take() else {
            return Ok(());
        };
        // Only this owning thread pumps callbacks. Detach before releasing the
        // observer/context. CFRelease also removes the source if AX removal fails
        unsafe { CFRunLoopRemoveSource(self.run_loop, self.source, kCFRunLoopDefaultMode) };
        let result =
            unsafe { AXObserverRemoveNotification(observer.0, self.window.0, self.name.0) };
        drop(observer);
        // A destroyed element or an already-removed subscription is fully retired
        if matches!(result, 0 | -25202 | -25210) {
            Ok(())
        } else {
            Err("Scratch-window lifetime observer could not confirm clean teardown.".into())
        }
    }
}
impl Drop for Lifetime {
    // Release this macOS test target owner when it leaves scope
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

// Report the credentials match condition for macOS test target
fn credentials_match(
    own_real: &Uid,
    own_effective: &Uid,
    target_real: &Uid,
    target_effective: &Uid,
) -> bool {
    let root = Uid::try_from(0usize).ok();
    root.as_ref() != Some(own_real)
        && own_real == own_effective
        && target_real == own_real
        && target_effective == own_real
}
// Reject targets that do not satisfy ordinary-process admission
fn ordinary_process(pid: i32) -> Result<(), String> {
    let own = Pid::from_u32(std::process::id());
    let target = Pid::from_u32(pid as u32);
    let mut system = System::new().map_err(|_| error())?;
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[own, target]),
        true,
        ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
    );
    let own = system.process(own).ok_or_else(error)?;
    let target = system.process(target).ok_or_else(error)?;
    if !credentials_match(
        own.user_id().ok_or_else(error)?,
        own.effective_user_id().ok_or_else(error)?,
        target.user_id().ok_or_else(error)?,
        target.effective_user_id().ok_or_else(error)?,
    ) {
        return Err("Select an ordinary, non-elevated application owned by this user.".into());
    }
    Ok(())
}
struct PinnedTarget {
    lifetime: Lifetime,
    target: Identity,
    application: Retained<NSRunningApplication>,
    launch: Retained<NSDate>,
    label: String,
}
impl TestTarget for PinnedTarget {
    // Return the display label for macOS test target
    fn label(&self) -> &str {
        &self.label
    }
    // Reject an input target whose original process is no longer alive
    fn check_alive(&mut self) -> Result<(), String> {
        self.lifetime.pump()?;
        let result = objc2::rc::autoreleasepool(|_| {
            native_context()?;
            ordinary_process(self.target.pid)?;
            let current =
                NSRunningApplication::runningApplicationWithProcessIdentifier(self.target.pid)
                    .ok_or_else(error)?;
            // AppKit explicitly says to compare running applications, not just PID
            // Keep launch identity as an additional fail-closed relaunch check
            if self.application.isTerminated()
                || current != self.application
                || current.processIdentifier() != self.target.pid
                || current
                    .launchDate()
                    .is_none_or(|launch| !launch.isEqualToDate(&self.launch))
                || pid(self.target.application.0)? != self.target.pid
                || pid(self.target.window.0)? != self.target.pid
                || text_attribute(self.target.window.0, "AXRole")? != "AXWindow"
            {
                return Err(error());
            }
            self.lifetime.pump()
        });
        if result.is_err() {
            self.lifetime.invalid.set(true);
        }
        result
    }
    // Reject an input target that no longer holds the required focus
    fn check_focused(&mut self) -> Result<(), String> {
        self.check_alive()?;
        if !self.target.matches(&Identity::foreground()?) {
            return Err("Focus the selected scratch window before test playback.".into());
        }
        self.lifetime.pump()
    }
    // Finalize the pending macOS test target operation
    fn finish(&mut self) -> Result<(), String> {
        self.lifetime.finish()
    }
}

// Explain why the current operation is unavailable
pub(super) fn unavailable_reason() -> Option<String> {
    native_context().err()
}

// Capture the current macOS test target observation
pub(super) fn capture() -> Result<Box<dyn TestTarget>, String> {
    // This gate precedes every native query, including permission/foreground APIs
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    objc2::rc::autoreleasepool(|_| {
        let target = Identity::foreground()?;
        ordinary_process(target.pid)?;
        let application = NSRunningApplication::runningApplicationWithProcessIdentifier(target.pid)
            .ok_or_else(error)?;
        let launch = application.launchDate().ok_or_else(error)?;
        let lifetime = Lifetime::install(&target)?;
        let name = application.localizedName().ok_or_else(error)?.to_string();
        let title = text_attribute(target.window.0, "AXTitle")?;
        if name.trim().is_empty() || title.trim().is_empty() {
            return Err(error());
        }
        let label = super::display_label(&name, &title);
        let mut pinned = PinnedTarget {
            lifetime,
            target,
            application,
            launch,
            label,
        };
        pinned.check_focused()?;
        Ok(Box::new(pinned) as Box<dyn TestTarget>)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that foreground capture rejects invalid mismatched and changed processes
    fn foreground_capture_rejects_invalid_mismatched_and_changed_processes() {
        assert!(foreground_pids_match(123, 123, 123, 123));
        for (before, application, window, after) in [
            (0, 0, 0, 0),
            (-1, -1, -1, -1),
            (123, 124, 123, 123),
            (123, 123, 124, 123),
            (123, 123, 123, 124),
            (123, 123, 123, 0),
        ] {
            assert!(!foreground_pids_match(before, application, window, after));
        }
    }
    #[test]
    // Verify that scratch target process policy rejects root setuid and other users
    fn scratch_target_process_policy_rejects_root_setuid_and_other_users() {
        let root = Uid::try_from(0usize).unwrap();
        let user = Uid::try_from(501usize).unwrap();
        let other = Uid::try_from(502usize).unwrap();
        assert!(credentials_match(&user, &user, &user, &user));
        assert!(!credentials_match(&root, &root, &root, &root));
        assert!(!credentials_match(&user, &root, &user, &user));
        assert!(!credentials_match(&user, &user, &user, &root));
        assert!(!credentials_match(&user, &user, &other, &other));
    }
    #[test]
    // Verify that protected session gate runs before any native query
    fn protected_session_gate_runs_before_any_native_query() {
        assert!(unavailable_reason().is_some());
        assert!(capture().is_err());
    }
    #[test]
    // Verify that destruction notification is sticky without querying native context
    fn destruction_notification_is_sticky_without_querying_native_context() {
        let invalid = Cell::new(false);
        let context = (&invalid as *const Cell<bool>).cast_mut().cast();
        unsafe {
            destroyed(ptr::null(), ptr::null(), ptr::null(), context);
        }
        assert!(invalid.get());
        // Refocusing or retaining the same numeric AX object cannot clear this bit
        unsafe {
            destroyed(ptr::null(), ptr::null(), ptr::null(), context);
        }
        assert!(invalid.get());
    }
}
