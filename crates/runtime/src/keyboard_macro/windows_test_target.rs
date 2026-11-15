//! Ephemeral scratch-window identity. This module never focuses a window or sends input
//! The selecting worker owns the hook, process handle and message pump for their lifetime
use super::TestTarget;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    ffi::c_void,
    marker::PhantomData,
    mem,
    path::Path,
    ptr,
    rc::Rc,
};
use windows::Win32::{
    Foundation::HWND,
    System::RemoteDesktop::*,
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, EVENT_OBJECT_DESTROY, MSG, PM_REMOVE,
            WINEVENT_OUTOFCONTEXT, WM_QUIT,
        },
    },
};
type Handle = *mut c_void;
#[link(name = "user32")]
unsafe extern "system" {
    fn GetForegroundWindow() -> Handle;
    fn GetWindowThreadProcessId(window: Handle, pid: *mut u32) -> u32;
    fn IsWindow(window: Handle) -> i32;
    fn IsWindowVisible(window: Handle) -> i32;
    fn GetWindowTextW(window: Handle, text: *mut u16, count: i32) -> i32;
    fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> Handle;
    fn CloseDesktop(desktop: Handle) -> i32;
    fn GetUserObjectInformationW(
        object: Handle,
        index: i32,
        info: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn GetCurrentProcess() -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: u32,
        text: *mut u16,
        size: *mut u32,
    ) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(
        token: Handle,
        class: u32,
        info: *mut c_void,
        length: u32,
        required: *mut u32,
    ) -> i32;
    fn IsValidSid(sid: *const c_void) -> i32;
    fn GetSidSubAuthorityCount(sid: *const c_void) -> *mut u8;
    fn GetSidSubAuthority(sid: *const c_void, index: u32) -> *mut u32;
}
struct OwnedHandle(Handle);
impl Drop for OwnedHandle {
    // Release this Windows test target owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
// Construct the Windows test target error result
fn error() -> String {
    "The selected scratch window is no longer verifiable. Select it again.".into()
}

#[derive(Default)]
struct Lifetime {
    target: Cell<isize>,
    destroyed: RefCell<HashSet<isize>>,
    invalid: Cell<bool>,
    hook: Cell<isize>,
}
impl Lifetime {
    // Invalidate a destroyed target and remember destruction before target selection
    fn destroyed(&self, window: isize) {
        if window == self.target.get() && window != 0 {
            self.invalid.set(true);
        }
        if self.target.get() == 0 {
            let mut seen = self.destroyed.borrow_mut();
            if seen.len() >= 1024 {
                self.invalid.set(true);
            } else {
                seen.insert(window);
            }
        }
    }
    // Hold a lease on the selected Windows test target resource
    fn pin(&self, window: isize) -> Result<(), String> {
        if window == 0 || self.invalid.get() || self.destroyed.borrow().contains(&window) {
            return Err(error());
        }
        self.target.set(window);
        self.destroyed.borrow_mut().clear();
        Ok(())
    }
}
thread_local! { static CONTEXT: Cell<*const Lifetime> = const { Cell::new(ptr::null()) }; }
// Record destruction of the pinned native target
unsafe extern "system" fn destroyed(
    hook: HWINEVENTHOOK,
    event: u32,
    window: HWND,
    object: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    CONTEXT.with(|slot| {
        if let Some(state) = slot.get().as_ref() {
            if state.hook.get() == hook.0 as isize
                && event == EVENT_OBJECT_DESTROY
                && object == 0
                && child == 0
            {
                state.destroyed(window.0 as isize);
            }
        }
    });
}
struct WindowObserver {
    hook: Option<HWINEVENTHOOK>,
    state: Box<Lifetime>,
    _thread: PhantomData<Rc<()>>,
}
impl WindowObserver {
    // Install the selected Windows test target content
    fn install() -> Result<Self, String> {
        if CONTEXT.with(|slot| !slot.get().is_null()) {
            return Err("A scratch target is already selected on this worker.".into());
        }
        let state = Box::<Lifetime>::default();
        CONTEXT.with(|slot| slot.set(&*state));
        let hook = unsafe {
            SetWinEventHook(
                EVENT_OBJECT_DESTROY,
                EVENT_OBJECT_DESTROY,
                None,
                Some(destroyed),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            )
        };
        if hook.0.is_null() {
            CONTEXT.with(|slot| slot.set(ptr::null()));
            return Err("Scratch-window lifetime monitoring is unavailable.".into());
        }
        state.hook.set(hook.0 as isize);
        Ok(Self {
            hook: Some(hook),
            state,
            _thread: PhantomData,
        })
    }
    // Finalize the pending Windows test target operation
    fn finish(&mut self) -> Result<(), String> {
        self.state.invalid.set(true);
        release_hook(&mut self.hook, |hook| unsafe {
            UnhookWinEvent(hook).as_bool()
        })?;
        self.state.hook.set(0);
        self.clear_context();
        Ok(())
    }
    // Clear context
    fn clear_context(&self) {
        CONTEXT.with(|slot| {
            if std::ptr::eq(slot.get(), &*self.state) {
                slot.set(ptr::null());
            }
        });
    }
    // Pump pending Windows test target events
    fn pump(&self) -> Result<(), String> {
        if self.state.invalid.get() || self.hook.is_none() {
            return Err(error());
        }
        let mut message = MSG::default();
        for _ in 0..256 {
            if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
                return if self.state.invalid.get() {
                    Err(error())
                } else {
                    Ok(())
                };
            }
            if message.message == WM_QUIT {
                self.state.invalid.set(true);
                return Err(error());
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        self.state.invalid.set(true);
        Err("Scratch-window observer queue overflowed. Select the target again.".into())
    }
}
// Failed teardown retains ownership so the caller and Drop can retry it
fn release_hook<T: Copy>(
    hook: &mut Option<T>,
    unhook: impl FnOnce(T) -> bool,
) -> Result<(), String> {
    if let Some(value) = *hook {
        if !unhook(value) {
            return Err("Scratch-window lifetime observer could not be detached.".into());
        }
        *hook = None;
    }
    Ok(())
}
impl Drop for WindowObserver {
    // Release this Windows test target owner when it leaves scope
    fn drop(&mut self) {
        let _ = self.finish();
        // Even if final teardown fails, late callbacks cannot dereference freed
        // state. The service receives the explicit finish error before completion
        self.clear_context();
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    window: isize,
    pid: u32,
    thread: u32,
}
// Read and validate the target window, process and thread identities
fn identity(window: Handle) -> Result<Identity, String> {
    let mut pid = 0;
    let thread = unsafe { GetWindowThreadProcessId(window, &mut pid) };
    if window.is_null() || pid == 0 || thread == 0 || unsafe { IsWindow(window) } == 0 {
        return Err(error());
    }
    Ok(Identity {
        window: window as isize,
        pid,
        thread,
    })
}
// Report the acceptable process condition for Windows test target
fn acceptable_process(
    pid: u32,
    own_pid: u32,
    integrity: u32,
    own_integrity: u32,
    elevated: bool,
) -> bool {
    pid != 0 && pid != own_pid && !elevated && integrity <= 0x2000 && integrity <= own_integrity
}
#[repr(C)]
struct MandatoryLabel {
    sid: *mut c_void,
    attributes: u32,
}
// Read process integrity and elevation from its access token
fn token_properties(process: Handle) -> Result<(u32, bool), String> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(process, 0x0008, &mut token) == 0 || token.is_null() {
            return Err(error());
        }
        let token = OwnedHandle(token);
        let mut elevated = 0u32;
        let mut required = 0;
        if GetTokenInformation(
            token.0,
            20,
            (&mut elevated as *mut u32).cast(),
            mem::size_of_val(&elevated) as u32,
            &mut required,
        ) == 0
            || required as usize != mem::size_of_val(&elevated)
        {
            return Err(error());
        }
        GetTokenInformation(token.0, 25, ptr::null_mut(), 0, &mut required);
        if !(mem::size_of::<MandatoryLabel>() as u32..=65536).contains(&required) {
            return Err(error());
        }
        let mut buffer = vec![0usize; (required as usize).div_ceil(mem::size_of::<usize>())];
        if GetTokenInformation(
            token.0,
            25,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        ) == 0
        {
            return Err(error());
        }
        let label = &*buffer.as_ptr().cast::<MandatoryLabel>();
        if label.sid.is_null() || IsValidSid(label.sid) == 0 {
            return Err(error());
        }
        let count = GetSidSubAuthorityCount(label.sid);
        if count.is_null() || *count == 0 {
            return Err(error());
        }
        let level = GetSidSubAuthority(label.sid, u32::from(*count - 1));
        if level.is_null() {
            return Err(error());
        }
        Ok((*level, elevated != 0))
    }
}
// Require an active unlocked interactive Windows session
fn desktop_check() -> Result<(), String> {
    unsafe {
        let mut buffer = windows::core::PWSTR::null();
        let mut bytes = 0;
        if WTSQuerySessionInformationW(None, u32::MAX, WTSSessionInfoEx, &mut buffer, &mut bytes)
            .is_err()
            || buffer.is_null()
        {
            return Err(error());
        }
        let unlocked = if bytes as usize >= mem::size_of::<WTSINFOEXW>() {
            let info = (buffer.0 as *const WTSINFOEXW).read_unaligned();
            info.Level == 1
                && info.Data.WTSInfoExLevel1.SessionState == WTSActive
                && info.Data.WTSInfoExLevel1.SessionFlags as u32 == WTS_SESSIONSTATE_UNLOCK
        } else {
            false
        };
        WTSFreeMemory(buffer.0.cast());
        if !unlocked {
            return Err("Scratch playback requires an unlocked interactive desktop.".into());
        }
        let desktop = OpenInputDesktop(0, 0, 1);
        if desktop.is_null() {
            return Err(error());
        }
        let mut name = [0u16; 64];
        let mut needed = 0;
        let ok = GetUserObjectInformationW(
            desktop,
            2,
            name.as_mut_ptr().cast(),
            mem::size_of_val(&name) as u32,
            &mut needed,
        );
        let closed = CloseDesktop(desktop);
        if ok == 0
            || closed == 0
            || needed as usize > mem::size_of_val(&name)
            || name[..8] != [68, 101, 102, 97, 117, 108, 116, 0]
        {
            return Err("Protected desktops cannot be scratch targets.".into());
        }
        Ok(())
    }
}
struct PinnedTarget {
    observer: WindowObserver,
    identity: Identity,
    process: OwnedHandle,
    label: String,
}
impl TestTarget for PinnedTarget {
    // Finalize the pending Windows test target operation
    fn finish(&mut self) -> Result<(), String> {
        self.observer.finish()
    }
    // Return the display label for Windows test target
    fn label(&self) -> &str {
        &self.label
    }
    // Reject an input target whose original process is no longer alive
    fn check_alive(&mut self) -> Result<(), String> {
        self.observer.pump()?;
        let result = (|| {
            desktop_check()?;
            if unsafe { WaitForSingleObject(self.process.0, 0) } != 0x102
                || identity(self.identity.window as Handle)? != self.identity
            {
                return Err(error());
            }
            let (integrity, elevated) = token_properties(self.process.0)?;
            let (own_integrity, _) = token_properties(unsafe { GetCurrentProcess() })?;
            if !acceptable_process(
                self.identity.pid,
                std::process::id(),
                integrity,
                own_integrity,
                elevated,
            ) {
                return Err("Select an ordinary, non-elevated application outside Manager.".into());
            }
            self.observer.pump()
        })();
        if result.is_err() {
            self.observer.state.invalid.set(true);
        }
        result
    }
    // Reject an input target that no longer holds the required focus
    fn check_focused(&mut self) -> Result<(), String> {
        self.check_alive()?;
        if identity(unsafe { GetForegroundWindow() })? != self.identity {
            return Err("Focus the selected scratch window before test playback.".into());
        }
        self.observer.pump()
    }
}
// Explain why the current operation is unavailable
pub(super) fn unavailable_reason() -> Option<String> {
    desktop_check().err()
}
// Capture the current Windows test target observation
pub(super) fn capture() -> Result<Box<dyn TestTarget>, String> {
    desktop_check()?;
    // Install before reading identity so destruction and reuse during selection
    // are latched, including a same-process reuse of the numeric HWND
    let observer = WindowObserver::install()?;
    let identity = identity(unsafe { GetForegroundWindow() })?;
    observer.pump()?;
    observer.state.pin(identity.window)?;
    if identity.pid == std::process::id() {
        return Err("Select a scratch application outside Manager.".into());
    }
    if unsafe { IsWindowVisible(identity.window as Handle) } == 0 {
        return Err(error());
    }
    let process = unsafe { OpenProcess(0x0010_1000, 0, identity.pid) };
    if process.is_null() {
        return Err("The selected process is protected or cannot be verified.".into());
    }
    let process = OwnedHandle(process);
    let mut executable = vec![0u16; 32768];
    let mut length = executable.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process.0, 0, executable.as_mut_ptr(), &mut length) }
        == 0
        || length == 0
        || length as usize >= executable.len()
    {
        return Err(error());
    }
    let path = String::from_utf16_lossy(&executable[..length as usize]);
    let app = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(error)?;
    let mut title = [0u16; 513];
    let count = unsafe {
        GetWindowTextW(
            identity.window as Handle,
            title.as_mut_ptr(),
            title.len() as i32,
        )
    };
    if count <= 0 || count as usize >= title.len() {
        return Err("Select a scratch window with a readable title.".into());
    }
    let label = super::display_label(app, &String::from_utf16_lossy(&title[..count as usize]));
    let mut target = PinnedTarget {
        observer,
        identity,
        process,
        label,
    };
    target.check_focused()?;
    Ok(Box::new(target))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that teardown retains failed hook and is idempotent after success
    fn teardown_retains_failed_hook_and_is_idempotent_after_success() {
        let mut hook = Some(42);
        assert!(release_hook(&mut hook, |_| false).is_err());
        assert_eq!(hook, Some(42));
        release_hook(&mut hook, |value| value == 42).unwrap();
        assert_eq!(hook, None);
        release_hook(&mut hook, |_| panic!("must not unhook twice")).unwrap();
    }
    #[test]
    // Verify that destroyed handles stay invalid even if reused
    fn destroyed_handles_stay_invalid_even_if_reused() {
        let lifetime = Lifetime::default();
        lifetime.destroyed(10);
        assert!(lifetime.pin(10).is_err());
        let lifetime = Lifetime::default();
        lifetime.pin(20).unwrap();
        lifetime.destroyed(21);
        assert!(!lifetime.invalid.get());
        lifetime.destroyed(20);
        assert!(lifetime.invalid.get());
        assert!(lifetime.pin(20).is_err());
    }
    #[test]
    // Verify that bounded observer and process policy fail closed
    fn bounded_observer_and_process_policy_fail_closed() {
        let lifetime = Lifetime::default();
        for handle in 1..=1025 {
            lifetime.destroyed(handle);
        }
        assert!(lifetime.pin(2000).is_err());
        assert!(acceptable_process(2, 1, 0x2000, 0x2000, false));
        assert!(!acceptable_process(1, 1, 0x2000, 0x2000, false));
        assert!(!acceptable_process(2, 1, 0x3000, 0x4000, false));
        assert!(!acceptable_process(2, 1, 0x2000, 0x1000, false));
        assert!(!acceptable_process(2, 1, 0x2000, 0x2000, true));
    }
}
