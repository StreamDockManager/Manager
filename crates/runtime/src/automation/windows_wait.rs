//! Sleep until queued WinEvents, configuration/shutdown, or a real sampling deadline.
use super::ObservationSignal;
use std::{ffi::c_void, sync::Arc, time::Duration};
type Handle = *mut c_void;
#[link(name = "kernel32")]
extern "system" {
    fn CreateEventW(
        attributes: *const c_void,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> Handle;
    fn SetEvent(event: Handle) -> i32;
    fn CloseHandle(event: Handle) -> i32;
}
#[link(name = "user32")]
extern "system" {
    fn MsgWaitForMultipleObjectsEx(
        count: u32,
        handles: *const Handle,
        milliseconds: u32,
        mask: u32,
        flags: u32,
    ) -> u32;
}
pub(super) struct NativeWait {
    event: Handle,
    signal: Arc<ObservationSignal>,
}
impl NativeWait {
    pub(super) fn new(signal: Arc<ObservationSignal>) -> Result<Self, String> {
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event.is_null() {
            return Err("Could not create foreground observer wake event".into());
        }
        let address = event as usize;
        signal.native(Some(Arc::new(move || unsafe {
            SetEvent(address as Handle);
        })));
        Ok(Self { event, signal })
    }
    pub(super) fn wait(&self, timeout: Duration) -> Result<(), String> {
        // Input-available avoids missing queued WinEvents after PeekMessage has inspected them.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(
                1,
                &self.event,
                timeout.as_millis().min(u32::MAX as u128 - 1) as u32,
                0x04ff,
                0x0004,
            )
        };
        if result == u32::MAX {
            Err("Foreground message wait failed".into())
        } else {
            Ok(())
        }
    }
}
impl Drop for NativeWait {
    fn drop(&mut self) {
        self.signal.native(None);
        unsafe {
            CloseHandle(self.event);
        }
    }
}
