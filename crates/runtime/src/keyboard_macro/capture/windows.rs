//! Scoped passive keyboard capture with independent Raw Input corroboration
//!
//! Microsoft documents that WH_KEYBOARD_LL can disappear silently; neither a live
//! message pump nor UnhookWindowsHookEx success establishes past hook continuity
//! Recordings commit only matching raw/hook observations, and a missing/reordered
//! observation discards the draft. Playback cancels on either physical source
//! A separate message-loop thread owns Control+Shift+F11 via RegisterHotKey (F12
//! is reserved by Windows). Hooks always forward input; only that explicitly
//! reserved emergency chord is registered. No injected probe or elevation
//!
//! Raw Input is process-wide. The desktop config explicitly disables unused Tao
//! DeviceEvents; this adapter nevertheless refuses an occupied keyboard slot and
//! verifies exact ownership throughout the session. It never steals/restores an
//! unrelated registration. All windows, hooks and registrations are scoped
//!
//! Primary API contracts:
//! https://learn.microsoft.com/windows/win32/winmsg/lowlevelkeyboardproc
//! https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-registerrawinputdevices
//! https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getregisteredrawinputdevices
//! https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-registerhotkey
//! https://learn.microsoft.com/windows/win32/api/wtsapi32/nf-wtsapi32-wtsregistersessionnotification
//! Runtime OS/session/hotkey checks are distinct from native qualification
use super::{
    corroboration::{Corroboration, Observation, AGREEMENT_MS},
    mappings::windows_key,
    CaptureError, CaptureHandle, CaptureReadiness, PhysicalKey, Purpose, Signal, Sink,
    SYNTHETIC_EVENT_MARKER,
};
use std::{
    cell::Cell,
    ffi::c_void,
    mem, ptr,
    sync::{atomic::Ordering, mpsc, Arc},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::{
    core::w,
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::RemoteDesktop::*,
        UI::{
            Input::{
                KeyboardAndMouse::{
                    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_CONTROL, MOD_NOREPEAT,
                    MOD_SHIFT,
                },
                *,
            },
            WindowsAndMessaging::*,
        },
    },
};

#[link(name = "user32")]
extern "system" {
    fn OpenInputDesktop(flags: u32, inherit: i32, access: u32) -> *mut c_void;
    fn CloseDesktop(desktop: *mut c_void) -> i32;
    fn GetUserObjectInformationW(
        object: *mut c_void,
        index: i32,
        info: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}
thread_local! { static CURRENT: Cell<*mut Context> = const { Cell::new(ptr::null_mut()) }; }
// Deliberately not configurable at runtime or through the environment. Only a
// reviewed change backed by the packaged-app native matrix may enable admission
// Windows packages are intentionally unsigned; signing is not an admission requirement
const HOTKEY_ID: i32 = 0x5344;
const CLASS_NAME: windows::core::PCWSTR = w!("StreamDockKeyboardCapture");

// Reject capture outside an active unlocked interactive desktop
fn desktop_check() -> Result<(), CaptureError> {
    unsafe {
        let mut buffer = windows::core::PWSTR::null();
        let mut bytes = 0;
        if WTSQuerySessionInformationW(None, u32::MAX, WTSSessionInfoEx, &mut buffer, &mut bytes)
            .is_err()
            || buffer.is_null()
        {
            return Err(CaptureError::SessionStateUnavailable);
        }
        let session = if bytes as usize >= std::mem::size_of::<WTSINFOEXW>() {
            let value = (buffer.0 as *const WTSINFOEXW).read_unaligned();
            if value.Level == 1 {
                let info = value.Data.WTSInfoExLevel1;
                if info.SessionState != WTSActive
                    || info.SessionFlags as u32 == WTS_SESSIONSTATE_LOCK
                {
                    Err(CaptureError::SessionLocked)
                } else if info.SessionFlags as u32 == WTS_SESSIONSTATE_UNLOCK {
                    Ok(())
                } else {
                    Err(CaptureError::SessionStateUnavailable)
                }
            } else {
                Err(CaptureError::SessionStateUnavailable)
            }
        } else {
            Err(CaptureError::SessionStateUnavailable)
        };
        WTSFreeMemory(buffer.0.cast());
        session?;
        // Opening for READOBJECTS never switches desktops, grants access, or
        // observes the secure/UAC desktop. Anything except Default fails closed
        let desktop = OpenInputDesktop(0, 0, 1);
        if desktop.is_null() {
            return Err(CaptureError::ProtectedInput);
        }
        let mut name = [0u16; 64];
        let mut needed = 0;
        let ok = GetUserObjectInformationW(
            desktop,
            2,
            name.as_mut_ptr().cast(),
            std::mem::size_of_val(&name) as u32,
            &mut needed,
        );
        let closed = CloseDesktop(desktop);
        let expected = [
            b'D' as u16,
            b'e' as u16,
            b'f' as u16,
            b'a' as u16,
            b'u' as u16,
            b'l' as u16,
            b't' as u16,
            0,
        ];
        if ok == 0
            || closed == 0
            || needed as usize > std::mem::size_of_val(&name)
            || name[..8] != expected
        {
            return Err(CaptureError::ProtectedInput);
        }
        Ok(())
    }
}
// Inspect the existing raw keyboard input registration
fn keyboard_registration() -> Result<Option<RAWINPUTDEVICE>, CaptureError> {
    let mut registrations = [RAWINPUTDEVICE::default(); 16];
    let mut count = registrations.len() as u32;
    let read = unsafe {
        GetRegisteredRawInputDevices(
            Some(registrations.as_mut_ptr()),
            &mut count,
            mem::size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    if read == u32::MAX
        || read as usize > registrations.len()
        || count as usize > registrations.len()
    {
        return Err(CaptureError::RawInputUnavailable);
    }
    let mut keyboard = None;
    for registration in &registrations[..read as usize] {
        if registration.usUsagePage == 1
            && (registration.usUsage == 6
                || (registration.usUsage == 0 && registration.dwFlags.0 & RIDEV_PAGEONLY.0 != 0))
            && keyboard.replace(*registration).is_some()
        {
            return Err(CaptureError::RawInputUnavailable);
        }
    }
    Ok(keyboard)
}
// Build the raw keyboard registration for this capture window
fn expected_registration(hwnd: HWND) -> RAWINPUTDEVICE {
    RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: 6,
        dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
        hwndTarget: hwnd,
    }
}
// Check ownership of registration
fn owns_registration(hwnd: HWND) -> Result<(), CaptureError> {
    if keyboard_registration()? == Some(expected_registration(hwnd)) {
        Ok(())
    } else {
        Err(CaptureError::RawInputUnavailable)
    }
}
// Report whether Windows is ready
pub(super) fn readiness() -> CaptureReadiness {
    let error = desktop_check()
        .and_then(|_| {
            if keyboard_registration()?.is_none() {
                Ok(())
            } else {
                Err(CaptureError::RawInputUnavailable)
            }
        })
        .err();
    readiness_result(error)
}
// Report readiness for result
fn readiness_result(error: Option<CaptureError>) -> CaptureReadiness {
    // This is a live prerequisite result, not evidence that the native device /
    // layout matrix has been exercised. Start still installs and verifies the
    // raw registration, emergency shortcut, hook and worker health before input
    CaptureReadiness {
        available: error.is_none(),
        implemented: true,
        reason: error.map(|error| error.to_string()),
    }
}
// Start the Windows operation
pub(super) fn start(
    purpose: Purpose,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<CaptureHandle, CaptureError> {
    desktop_check()?;
    if keyboard_registration()?.is_some() {
        return Err(CaptureError::RawInputUnavailable);
    }
    let (sink, mut handle) = Sink::channel_with_stop(purpose, stop);
    let signal = handle.signal.clone();
    handle.thread = Some(
        std::thread::Builder::new()
            .name("keyboard-capture".into())
            .spawn(move || {
                if sink.stopped() {
                    return;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(sink)));
                if result.is_err() {
                    signal.fail_cleanup(CaptureError::WorkerFailed);
                }
                signal
                    .completed
                    .store(true, std::sync::atomic::Ordering::Release);
            })
            .map_err(|_| CaptureError::WorkerFailed)?,
    );
    Ok(handle)
}

struct Hotkey {
    thread: Option<JoinHandle<()>>,
    signal: Arc<Signal>,
}
impl Hotkey {
    // Start the Windows operation
    fn start(signal: Arc<Signal>) -> Result<Self, CaptureError> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = signal.clone();
        let thread = std::thread::Builder::new()
            .name("keyboard-emergency-stop".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if unsafe {
                        RegisterHotKey(
                            None,
                            HOTKEY_ID,
                            MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT,
                            0x7a,
                        )
                    }
                    .is_err()
                    {
                        worker.fail(CaptureError::EmergencyStopUnavailable);
                        let _ = ready_tx.send(false);
                        return;
                    }
                    struct Registration(Arc<Signal>);
                    impl Drop for Registration {
                        // Release this Windows owner when it leaves scope
                        fn drop(&mut self) {
                            if unsafe { UnregisterHotKey(None, HOTKEY_ID) }.is_err() {
                                self.0.fail_cleanup(CaptureError::EmergencyStopUnavailable);
                            }
                        }
                    }
                    let _registration = Registration(worker.clone());
                    worker
                        .hotkey_heartbeat
                        .store(worker.elapsed_ms(), Ordering::Release);
                    let _ = ready_tx.send(true);
                    let mut message = MSG::default();
                    while !worker.stop.load(Ordering::Acquire) {
                        for _ in 0..64 {
                            if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }
                                .as_bool()
                            {
                                break;
                            }
                            if message.message == WM_HOTKEY
                                && message.wParam.0 == HOTKEY_ID as usize
                            {
                                worker.fail(CaptureError::EmergencyStop);
                                break;
                            }
                            if message.message == WM_QUIT {
                                worker.fail(CaptureError::EmergencyStopUnavailable);
                                break;
                            }
                        }
                        worker
                            .hotkey_heartbeat
                            .store(worker.elapsed_ms(), Ordering::Release);
                        std::thread::sleep(Duration::from_millis(2));
                    }
                }));
                if result.is_err() {
                    worker.fail_cleanup(CaptureError::WorkerFailed);
                }
            })
            .map_err(|_| CaptureError::WorkerFailed)?;
        let value = Self {
            thread: Some(thread),
            signal,
        };
        if ready_rx.recv_timeout(Duration::from_secs(2)) != Ok(true) {
            value.signal.fail(CaptureError::EmergencyStopUnavailable);
            return Err(CaptureError::EmergencyStopUnavailable);
        }
        Ok(value)
    }
}
impl Drop for Hotkey {
    // Release this Windows owner when it leaves scope
    fn drop(&mut self) {
        self.signal.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                self.signal.fail_cleanup(CaptureError::WorkerFailed);
            }
        }
    }
}

struct Resources {
    module: HINSTANCE,
    hwnd: HWND,
    hook: Option<HHOOK>,
    raw_owned: bool,
    notification: bool,
    signal: Arc<Signal>,
}
impl Resources {
    // Initialize Windows state from the supplied inputs
    fn new(signal: Arc<Signal>) -> Result<Self, CaptureError> {
        let module = HINSTANCE(unsafe { GetModuleHandleW(ptr::null()) });
        if module.is_invalid() {
            return Err(CaptureError::WorkerFailed);
        }
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_callback),
            hInstance: module,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(CaptureError::RawInputUnavailable);
        }
        let mut value = Self {
            module,
            hwnd: HWND::default(),
            hook: None,
            raw_owned: false,
            notification: false,
            signal,
        };
        value.hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!(""),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(module),
                None,
            )
        }
        .map_err(|_| CaptureError::RawInputUnavailable)?;
        if unsafe { WTSRegisterSessionNotification(value.hwnd, NOTIFY_FOR_THIS_SESSION) }.is_err() {
            return Err(CaptureError::SessionStateUnavailable);
        }
        value.notification = true;
        if keyboard_registration()?.is_some() {
            return Err(CaptureError::RawInputUnavailable);
        }
        unsafe {
            RegisterRawInputDevices(
                &[expected_registration(value.hwnd)],
                mem::size_of::<RAWINPUTDEVICE>() as u32,
            )
        }
        .map_err(|_| CaptureError::RawInputUnavailable)?;
        value.raw_owned = true;
        owns_registration(value.hwnd)?;
        value.hook = Some(
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_callback), Some(module), 0) }
                .map_err(|_| CaptureError::PermissionDenied)?,
        );
        Ok(value)
    }
    // Detach hook
    fn detach_hook(&mut self) {
        if let Some(hook) = self.hook.take() {
            // Failure invalidates the draft. Success is NOT treated as a proof of
            // historic continuity; independent raw/hook agreement supplies that
            if unsafe { UnhookWindowsHookEx(hook) }.is_err() {
                self.signal.fail_cleanup(CaptureError::HookDisabled);
            }
        }
    }
}
impl Drop for Resources {
    // Release this Windows owner when it leaves scope
    fn drop(&mut self) {
        self.detach_hook();
        if self.raw_owned {
            // Never remove another owner's registration, even during cleanup
            if owns_registration(self.hwnd).is_ok() {
                let remove = RAWINPUTDEVICE {
                    usUsagePage: 1,
                    usUsage: 6,
                    dwFlags: RIDEV_REMOVE,
                    hwndTarget: HWND::default(),
                };
                if unsafe {
                    RegisterRawInputDevices(&[remove], mem::size_of::<RAWINPUTDEVICE>() as u32)
                }
                .is_err()
                {
                    self.signal.fail_cleanup(CaptureError::RawInputUnavailable);
                }
            } else {
                self.signal.fail_cleanup(CaptureError::RawInputUnavailable);
            }
        }
        if self.notification && unsafe { WTSUnRegisterSessionNotification(self.hwnd) }.is_err() {
            self.signal
                .fail_cleanup(CaptureError::SessionStateUnavailable);
        }
        if !self.hwnd.is_invalid() && unsafe { DestroyWindow(self.hwnd) }.is_err() {
            self.signal.fail_cleanup(CaptureError::WorkerFailed);
        }
        if unsafe { UnregisterClassW(CLASS_NAME, Some(self.module)) }.is_err() {
            self.signal.fail_cleanup(CaptureError::WorkerFailed);
        }
    }
}

struct Context {
    sink: Sink,
    agreement: Corroboration,
    device: Option<usize>,
    draining: bool,
}
impl Context {
    // Return elapsed time in milliseconds
    fn elapsed_ms(&self) -> u64 {
        self.sink
            .started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }
    // Flush pending Windows writes
    fn flush(&mut self) {
        loop {
            match self.agreement.next(self.elapsed_ms()) {
                Ok(Some(event)) => self.sink.push_at(
                    windows_key(u32::from(event.scan), event.extended),
                    event.down,
                    false,
                    event.elapsed_ms,
                    self.draining,
                ),
                Ok(None) => break,
                Err(error) => {
                    self.sink.signal.fail(error);
                    break;
                }
            }
        }
    }
    // Record a low-level keyboard observation unless capture is stopping
    fn hook(&mut self, event: &KBDLLHOOKSTRUCT, down: bool) {
        if self.draining || self.sink.stopped() {
            return;
        }
        let key = if matches!(event.vkCode, 0x13 | 0x2c) {
            None
        } else {
            windows_key(event.scanCode, event.flags.0 & 1 != 0)
        };
        if self.sink.purpose == Purpose::PlaybackObserver {
            self.sink.push(key, down, false);
            return;
        }
        if key.is_none() {
            self.sink.signal.fail(CaptureError::UnsupportedKey);
            return;
        }
        let Ok(scan) = u16::try_from(event.scanCode) else {
            self.sink.signal.fail(CaptureError::UnsupportedKey);
            return;
        };
        if let Err(error) = self.agreement.hook(Observation {
            scan,
            extended: event.flags.0 & 1 != 0,
            down,
            elapsed_ms: self.elapsed_ms(),
        }) {
            self.sink.signal.fail(error);
            return;
        }
        self.flush();
    }
    // Read raw keyboard input and reconcile it with hook observations
    unsafe fn raw(&mut self, input: HRAWINPUT) {
        if self.sink.stopped() && !self.draining {
            return;
        }
        let mut data = RAWINPUT::default();
        let mut size = mem::size_of::<RAWINPUT>() as u32;
        let read = GetRawInputData(
            input,
            RID_INPUT,
            Some((&mut data as *mut RAWINPUT).cast()),
            &mut size,
            mem::size_of::<RAWINPUTHEADER>() as u32,
        );
        if read == u32::MAX
            || read < (mem::size_of::<RAWINPUTHEADER>() + mem::size_of::<RAWKEYBOARD>()) as u32
            || read as usize > mem::size_of::<RAWINPUT>()
            || data.header.dwSize != read
            || data.header.dwType != RIM_TYPEKEYBOARD.0
            || data.header.hDevice.is_invalid()
        {
            self.sink.signal.fail(CaptureError::RawInputUnavailable);
            return;
        }
        let device = data.header.hDevice.0 as usize;
        if let Some(previous) = self.device {
            if previous != device {
                self.sink.signal.fail(CaptureError::DeviceChanged);
                return;
            }
        } else {
            let mut info = RID_DEVICE_INFO {
                cbSize: mem::size_of::<RID_DEVICE_INFO>() as u32,
                ..Default::default()
            };
            let mut bytes = info.cbSize;
            if GetRawInputDeviceInfoW(
                Some(data.header.hDevice),
                RIDI_DEVICEINFO,
                Some((&mut info as *mut RID_DEVICE_INFO).cast()),
                &mut bytes,
            ) == u32::MAX
                || info.dwType != RIM_TYPEKEYBOARD
            {
                self.sink.signal.fail(CaptureError::RawInputUnavailable);
                return;
            }
            self.device = Some(device);
        }
        let keyboard = data.data.keyboard;
        // E1, overrun, fake/multibyte PrintScreen/Pause and unknown flags fail
        // rather than becoming invented portable keys or a lossy recording
        let key = if keyboard.Flags & !3 != 0
            || matches!(keyboard.MakeCode, 0 | 0xff)
            || matches!(keyboard.VKey, 0x13 | 0x2c | 0xff)
        {
            None
        } else {
            windows_key(u32::from(keyboard.MakeCode), keyboard.Flags & 2 != 0)
        };
        let down = keyboard.Flags & 1 == 0;
        if self.sink.purpose == Purpose::PlaybackObserver {
            // Independent physical-source cancellation even when LL vanished
            self.sink.push(key, down, false);
            return;
        }
        if key.is_none() {
            self.sink.signal.fail(CaptureError::UnsupportedKey);
            return;
        }
        if let Err(error) = self.agreement.raw(Observation {
            scan: keyboard.MakeCode,
            extended: keyboard.Flags & 2 != 0,
            down,
            elapsed_ms: self.elapsed_ms(),
        }) {
            self.sink.signal.fail(error);
            return;
        }
        self.flush();
    }
}
struct ContextRegistration;
impl Drop for ContextRegistration {
    // Release this Windows owner when it leaves scope
    fn drop(&mut self) {
        CURRENT.with(|slot| slot.set(ptr::null_mut()));
    }
}
// Report the pump condition for Windows
fn pump() -> bool {
    let mut message = MSG::default();
    for _ in 0..64 {
        if !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            return true;
        }
        if message.message == WM_QUIT {
            CURRENT.with(|slot| unsafe {
                if let Some(context) = slot.get().as_mut() {
                    context.sink.signal.fail(CaptureError::WorkerFailed);
                }
            });
            return false;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // Never acknowledge a safety barrier while messages remain unprocessed
    !unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) }.as_bool()
}
// Drive the Windows operation
fn run(mut sink: Sink) {
    if sink.purpose == Purpose::Recording {
        let countdown = Instant::now();
        while countdown.elapsed() < Duration::from_secs(3) {
            if sink.stopped() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if let Err(error) = desktop_check() {
        sink.signal.fail(error);
        return;
    }
    if (8..=254).any(|key| unsafe { GetAsyncKeyState(key) } < 0) {
        sink.signal.fail(CaptureError::InitiallyHeldKeys);
        return;
    }
    let hotkey = match Hotkey::start(sink.signal.clone()) {
        Ok(value) => value,
        Err(error) => {
            sink.signal.fail(error);
            return;
        }
    };
    sink.emergency_key = PhysicalKey::F11;
    sink.started = Instant::now();
    let mut context = Context {
        sink,
        agreement: Corroboration::default(),
        device: None,
        draining: false,
    };
    CURRENT.with(|slot| slot.set(&mut context));
    let _context_registration = ContextRegistration;
    let mut resources = match Resources::new(context.sink.signal.clone()) {
        Ok(value) => value,
        Err(error) => {
            context.sink.signal.fail(error);
            return;
        }
    };
    if (8..=254).any(|key| unsafe { GetAsyncKeyState(key) } < 0) {
        context.sink.signal.fail(CaptureError::InitiallyHeldKeys);
    }
    // Drain startup traffic before announcing readiness; every physical event
    // racing admission either appears in capture or prevents playback admission
    let _ = pump();
    context
        .sink
        .signal
        .worker_heartbeat
        .store(context.sink.signal.elapsed_ms(), Ordering::Release);
    context
        .sink
        .signal
        .strict_health
        .store(true, Ordering::Release);
    context.sink.signal.ready.store(true, Ordering::Release);
    while !context.sink.expired() {
        if let Err(error) = desktop_check().and_then(|_| owns_registration(resources.hwnd)) {
            context.sink.signal.fail(error);
            break;
        }
        context.sink.signal.check_liveness();
        let request = context
            .sink
            .signal
            .verification_requested
            .load(Ordering::Acquire);
        let drained = pump();
        context.flush();
        // Check after dispatch too: messages can signal lock/disconnect or alter
        // registration, and a request arriving during this pass waits another pass
        if let Err(error) = desktop_check().and_then(|_| owns_registration(resources.hwnd)) {
            context.sink.signal.fail(error);
            break;
        }
        context
            .sink
            .signal
            .worker_heartbeat
            .store(context.sink.signal.elapsed_ms(), Ordering::Release);
        if drained && context.agreement.is_empty() && context.sink.signal.error().is_none() {
            context
                .sink
                .signal
                .verified
                .store(request, Ordering::Release);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    context.draining = true;
    resources.detach_hook();
    // No further hook callbacks can add events on this dedicated thread. Match
    // already-observed events to queued raw messages; never fabricate a missing
    // counterpart or accept fresh raw-only events across the Stop boundary
    if !pump() {
        context.sink.signal.fail(CaptureError::BufferOverflow);
    }
    context.flush();
    let deadline = Instant::now();
    while !context.agreement.is_empty()
        && context
            .sink
            .signal
            .error()
            .is_none_or(CaptureError::is_graceful)
        && deadline.elapsed() <= Duration::from_millis(AGREEMENT_MS)
    {
        let _ = pump();
        context.flush();
        if !context.agreement.is_empty() {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    if let Err(error) = context.agreement.finish() {
        context.sink.signal.fail(error);
    }
    if let Err(error) = desktop_check().and_then(|_| owns_registration(resources.hwnd)) {
        context.sink.signal.fail(error);
    }
    drop(resources);
    drop(hotkey);
    context.sink.finish();
}
// Observe native window events for the active capture session
unsafe extern "system" fn window_callback(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    CURRENT.with(|slot| {
        if let Some(context) = slot.get().as_mut() {
            match message {
                WM_INPUT => context.raw(HRAWINPUT(lparam.0 as *mut c_void)),
                WM_INPUT_DEVICE_CHANGE => context.sink.signal.fail(CaptureError::DeviceChanged),
                WM_WTSSESSION_CHANGE => context.sink.signal.fail(CaptureError::SessionLocked),
                WM_POWERBROADCAST if wparam.0 == 4 => {
                    context.sink.signal.fail(CaptureError::SessionLocked)
                }
                _ => (),
            }
        }
    });
    // Required for foreground WM_INPUT cleanup; also forwards every ordinary
    // message. Neither this window nor the hook suppresses user input
    DefWindowProcW(hwnd, message, wparam, lparam)
}
// Process keyboard hook events for the active capture session
unsafe extern "system" fn hook_callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 && lparam.0 != 0 {
        let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if event.flags.0 & 0x12 == 0
            && event.dwExtraInfo != SYNTHETIC_EVENT_MARKER as usize
            && event.dwExtraInfo != enigo::EVENT_MARKER as usize
        {
            CURRENT.with(|slot| {
                if let Some(context) = slot.get().as_mut() {
                    let message = wparam.0 as u32;
                    if matches!(message, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP) {
                        context.hook(event, matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN));
                    }
                }
            });
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that readiness follows live prerequisites without installing input observers
    fn readiness_follows_live_prerequisites_without_installing_input_observers() {
        let ready = readiness_result(None);
        assert!(ready.available && ready.implemented);
        assert!(ready.reason.is_none());
        for error in [
            CaptureError::SessionLocked,
            CaptureError::ProtectedInput,
            CaptureError::SessionStateUnavailable,
            CaptureError::RawInputUnavailable,
            CaptureError::EmergencyStopUnavailable,
        ] {
            let status = readiness_result(Some(error));
            assert!(!status.available);
            assert!(status.implemented && status.reason.is_some());
        }
    }
}
