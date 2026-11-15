//! Explicit SendInput scan-code output with live observer/session prerequisites
//! Native delivery, repeat receipt and emergency-stop behavior remain unverified
//! on an actual Windows scratch target; compilation is not qualification
use super::*;
use crate::keyboard_macro::capture::NativePlaybackObserver;
use std::{ffi::c_void, mem, ptr};
#[path = "windows_focus.rs"]
mod focus;

type Handle = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy)]
struct KeyboardInput {
    virtual_key: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct MouseInput {
    x: i32,
    y: i32,
    data: u32,
    flags: u32,
    time: u32,
    extra: usize,
}
#[repr(C)]
union InputValue {
    keyboard: KeyboardInput,
    mouse: MouseInput,
}
#[repr(C)]
struct Input {
    kind: u32,
    value: InputValue,
}
#[repr(C)]
struct SidAndAttributes {
    sid: *mut c_void,
    attributes: u32,
}
#[link(name = "user32")]
unsafe extern "system" {
    fn SendInput(count: u32, input: *const Input, size: i32) -> u32;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
    fn GetCurrentProcess() -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
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
    fn GetSidSubAuthorityCount(sid: *const c_void) -> *mut u8;
    fn GetSidSubAuthority(sid: *const c_void, index: u32) -> *mut u32;
}
struct OwnedHandle(Handle);
impl OwnedHandle {
    // Initialize Windows playback state from the supplied inputs
    fn new(handle: Handle) -> Result<Self, String> {
        if handle.is_null() {
            Err("Keyboard macro target permission cannot be verified".into())
        } else {
            Ok(Self(handle))
        }
    }
}
impl Drop for OwnedHandle {
    // Release this Windows playback owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
// Read the process token integrity level used for playback admission
fn integrity(process: Handle) -> Result<u32, String> {
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(process, 0x0008, &mut token) == 0 {
            return Err("Keyboard macro process integrity cannot be verified".into());
        }
        let token = OwnedHandle::new(token)?;
        let mut required = 0;
        GetTokenInformation(token.0, 25, ptr::null_mut(), 0, &mut required);
        if !(mem::size_of::<SidAndAttributes>() as u32..=65_536).contains(&required) {
            return Err("Keyboard macro process integrity is unavailable".into());
        }
        // usize storage retains the alignment required by TOKEN_MANDATORY_LABEL
        let mut buffer = vec![0usize; (required as usize).div_ceil(mem::size_of::<usize>())];
        if GetTokenInformation(
            token.0,
            25,
            buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        ) == 0
        {
            return Err("Keyboard macro process integrity cannot be verified".into());
        }
        let label = &*buffer.as_ptr().cast::<SidAndAttributes>();
        if label.sid.is_null() {
            return Err("Keyboard macro process integrity is unavailable".into());
        }
        let count = GetSidSubAuthorityCount(label.sid);
        if count.is_null() || *count == 0 {
            return Err("Keyboard macro process integrity is unavailable".into());
        }
        let level = GetSidSubAuthority(label.sid, u32::from(*count - 1));
        if level.is_null() {
            return Err("Keyboard macro process integrity is unavailable".into());
        }
        Ok(*level)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    window: isize,
    pid: u32,
    thread: u32,
}
impl Target {
    // Return the current Windows playback value
    fn current() -> Result<Self, String> {
        unsafe {
            use ::windows::Win32::UI::WindowsAndMessaging::{
                GetForegroundWindow, GetWindowThreadProcessId,
            };
            let handle = GetForegroundWindow();
            let window = handle.0 as isize;
            let mut pid = 0;
            let thread = GetWindowThreadProcessId(handle, Some(&mut pid));
            if window == 0 || pid == 0 || thread == 0 {
                return Err("Keyboard macro target cannot be verified".into());
            }
            Ok(Self {
                window,
                pid,
                thread,
            })
        }
    }
    // Check integrity
    fn check_integrity(self) -> Result<(), String> {
        unsafe {
            let target = OwnedHandle::new(OpenProcess(0x1000, 0, self.pid))?;
            if integrity(target.0)? > integrity(GetCurrentProcess())? {
                return Err("Keyboard macro cannot send input to a higher-integrity application; no elevation is attempted".into());
            }
        }
        Ok(())
    }
}
pub(crate) struct NativeBackend {
    owner: String,
    observer: Option<NativePlaybackObserver>,
    target_observer: Option<super::focus::Observer>,
}
impl NativeBackend {
    // Initialize Windows playback state from the supplied inputs
    pub(crate) fn new(owner: &str) -> Result<Self, String> {
        Ok(Self {
            owner: owner.into(),
            observer: None,
            target_observer: None,
        })
    }
}
impl Backend for NativeBackend {
    type Code = WindowsScanCode;
    // Check Windows playback prerequisites before proceeding
    fn preflight(
        &mut self,
        recording: &KeyboardMacroRecording,
        playback: &KeyboardMacroPlayback,
    ) -> Result<(), String> {
        repeat::require_exact(playback.repeat_mode)?;
        if let Some(reason) = playback_unavailable_reason() {
            return Err(reason);
        }
        repeat::preflight(repeat::Mechanism::RepeatedDown, recording)?;
        let readiness = crate::keyboard_macro::capture::playback_observer_readiness();
        if !readiness.available {
            return Err(readiness
                .reason
                .unwrap_or_else(|| "Keyboard macro observer is unavailable".into()));
        }
        Target::current()?.check_integrity()
    }
    // Resolve the supplied Windows playback request
    fn resolve(&mut self, key: PhysicalKey) -> Result<Self::Code, String> {
        if matches!(
            key,
            PhysicalKey::CapsLock
                | PhysicalKey::NumLock
                | PhysicalKey::ScrollLock
                | PhysicalKey::PrintScreen
                | PhysicalKey::Pause
        ) {
            return Err("Lock and multibyte keys are not qualified for keyboard playback".into());
        }
        windows_scan_code(key).ok_or_else(|| "Unsupported physical key mapping".into())
    }
    // Begin tracking the Windows playback operation
    fn begin(&mut self, policy: KeyboardMacroFocusPolicy) -> Result<(), String> {
        if let Some(reason) = playback_unavailable_reason() {
            return Err(reason);
        }
        self.observer = Some(NativePlaybackObserver::start().map_err(|error| error.to_string())?);
        if policy == KeyboardMacroFocusPolicy::StayOnTarget {
            self.target_observer = Some(focus::start()?);
        }
        self.check()
    }
    // Check the current Windows playback conditions
    fn check(&mut self) -> Result<(), String> {
        self.observer
            .as_ref()
            .ok_or("Keyboard macro observer is unavailable")?
            .check()
            .map_err(|error| error.to_string())?;
        let current = Target::current()?;
        current.check_integrity()?;
        if let Some(observer) = &self.target_observer {
            observer.check_fresh()?;
        }
        Ok(())
    }
    // Finalize the pending Windows playback operation
    fn finish(&mut self) -> Result<(), String> {
        let capture = self.observer.as_mut().map_or(Ok(()), |observer| {
            observer.stop().map_err(|error| error.to_string())
        });
        self.observer = None;
        let target = self
            .target_observer
            .as_mut()
            .map_or(Ok(()), |observer| observer.stop());
        self.target_observer = None;
        capture.and(target)
    }
    // Deliver the Windows playback event to its consumer
    fn emit(&mut self, code: &WindowsScanCode, kind: KeyboardMacroEventKind) -> Result<(), String> {
        let physical = physical_from_windows(code.code, code.extended);
        if kind == KeyboardMacroEventKind::KeyUp {
            let key = physical.ok_or("Keyboard cleanup mapping is unavailable")?;
            self.observer
                .as_ref()
                .ok_or("Keyboard cleanup observer is unavailable")?
                .prepare_release(key)?;
        }
        let owned_code = code.code | if code.extended { 0x100 } else { 0 };
        crate::builtins::native::macro_key_event(&self.owner, owned_code, kind, || {
            if kind == KeyboardMacroEventKind::KeyUp {
                self.observer
                    .as_ref()
                    .ok_or("Keyboard cleanup observer is unavailable")?
                    .release_ready(physical.ok_or("Keyboard cleanup mapping is unavailable")?)?;
            }
            let flags = 0x0008
                | if code.extended { 0x0001 } else { 0 }
                | if kind == KeyboardMacroEventKind::KeyUp {
                    0x0002
                } else {
                    0
                };
            let input = Input {
                kind: 1,
                value: InputValue {
                    keyboard: KeyboardInput {
                        virtual_key: 0,
                        scan: code.code,
                        flags,
                        time: 0,
                        extra: crate::keyboard_macro::capture::SYNTHETIC_EVENT_MARKER as usize,
                    },
                },
            };
            if unsafe { SendInput(1, &input, mem::size_of::<Input>() as i32) } != 1 {
                return Err("Keyboard macro input was rejected or partially delivered".into());
            }
            Ok(())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that input layout matches the win64 abi
    fn input_layout_matches_the_win64_abi() {
        if mem::size_of::<usize>() == 8 {
            assert_eq!(mem::size_of::<Input>(), 40);
            assert_eq!(mem::size_of::<KeyboardInput>(), 24);
        }
    }
}
