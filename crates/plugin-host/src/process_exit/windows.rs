use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    process::Child,
};
use windows::Win32::{
    Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_OBJECT_0},
    System::Threading::{
        CreateEventW, GetCurrentProcess, SetEvent, WaitForMultipleObjects, INFINITE,
    },
};
// WaitForMultipleObjects permits 64 handles including the change/shutdown event.
// Excess plugins retain bounded polling rather than growing unbounded wait threads.
pub(super) const CAPACITY: usize = 63;
pub(super) struct Object(OwnedHandle);
pub(super) struct Signal(OwnedHandle);
impl Object {
    pub fn new(child: &Child) -> Option<Self> {
        let mut handle = HANDLE::default();
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                HANDLE(child.as_raw_handle()),
                GetCurrentProcess(),
                &mut handle,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
            .ok()?;
            Some(Self(OwnedHandle::from_raw_handle(handle.0)))
        }
    }
}
impl Signal {
    pub fn new() -> Option<Self> {
        // Auto-reset coalesces changes without losing a mutation between snapshot
        // creation and wait entry; no manual-reset clear race.
        let handle = unsafe { CreateEventW(None, false, false, None).ok()? };
        Some(Self(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
    }
    pub fn notify(&self) {
        let _ = unsafe { SetEvent(HANDLE(self.0.as_raw_handle())) };
    }
    pub fn wait(&self, objects: &[&Object]) -> io::Result<Vec<usize>> {
        let mut handles = Vec::with_capacity(objects.len() + 1);
        handles.push(HANDLE(self.0.as_raw_handle()));
        handles.extend(
            objects
                .iter()
                .map(|object| HANDLE(object.0.as_raw_handle())),
        );
        let result = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
        let index = result.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if index >= handles.len() {
            return Err(io::Error::last_os_error());
        }
        Ok(if index == 0 {
            Vec::new()
        } else {
            vec![index - 1]
        })
    }
}
