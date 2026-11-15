//! Public Core Audio property notifications. SDK AudioHardware.h documents that
//! registrations copy the block and retain its dispatch queue until removal
use super::{Signal, Watch};
use block2::{Block, RcBlock};
use std::{ffi::c_void, sync::Arc};

use crate::core_audio::{address, Address, AudioObjectGetPropertyData, AudioObjectHasProperty};
type ListenerBlock = dyn Fn(u32, *const c_void);
#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectAddPropertyListenerBlock(
        object: u32,
        address: *const Address,
        queue: *mut c_void,
        listener: *const Block<ListenerBlock>,
    ) -> i32;
    fn AudioObjectRemovePropertyListenerBlock(
        object: u32,
        address: *const Address,
        queue: *mut c_void,
        listener: *const Block<ListenerBlock>,
    ) -> i32;
}
#[link(name = "System")]
extern "C" {
    fn dispatch_queue_create(label: *const std::ffi::c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_sync_f(queue: *mut c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_release(object: *mut c_void);
}
struct Listener {
    object: u32,
    address: Address,
    block: RcBlock<ListenerBlock>,
}
pub(super) struct Subscription {
    queue: *mut c_void,
    listeners: Vec<Listener>,
}
impl Subscription {
    // Initialize macOS state from the supplied inputs
    pub fn new(signal: Arc<Signal>) -> Result<Self, String> {
        let queue = unsafe {
            dispatch_queue_create(
                c"app.streamdockmanager.audio-observer".as_ptr(),
                std::ptr::null(),
            )
        };
        if queue.is_null() {
            return Err("Audio notification queue unavailable".into());
        }
        let mut value = Self {
            queue,
            listeners: Vec::with_capacity(5),
        };
        value.add(1, address(b"dOut", b"glob", 0), signal.clone(), true)?;
        let mut device = 0u32;
        let mut size = 4;
        let result = unsafe {
            AudioObjectGetPropertyData(
                1,
                &address(b"dOut", b"glob", 0),
                0,
                std::ptr::null(),
                &mut size,
                (&mut device as *mut u32).cast(),
            )
        };
        if result != 0 || size != 4 || device == 0 {
            return Err("Default audio output unavailable".into());
        }
        for element in 0..3 {
            let property = address(b"volm", b"outp", element);
            if unsafe { AudioObjectHasProperty(device, &property) } != 0 {
                value.add(device, property, signal.clone(), false)?;
            }
        }
        let property = address(b"mute", b"outp", 0);
        if unsafe { AudioObjectHasProperty(device, &property) } != 0 {
            value.add(device, property, signal, false)?;
        }
        if value.listeners.len() <= 1 {
            return Err("Output volume/mute notifications unavailable".into());
        }
        Ok(value)
    }
    // Add an entry to macOS
    fn add(
        &mut self,
        object: u32,
        address: Address,
        signal: Arc<Signal>,
        rebind: bool,
    ) -> Result<(), String> {
        let block = RcBlock::new(move |_: u32, _: *const c_void| signal.invalidate(rebind));
        if unsafe {
            AudioObjectAddPropertyListenerBlock(
                object,
                &address,
                self.queue,
                RcBlock::as_ptr(&block),
            )
        } != 0
        {
            return Err("Audio property notification registration failed".into());
        }
        self.listeners.push(Listener {
            object,
            address,
            block,
        });
        Ok(())
    }
}
impl Watch for Subscription {
    // Report whether the macOS observation is healthy
    fn healthy(&mut self) -> bool {
        true
    }
}
impl Drop for Subscription {
    // Release this macOS owner when it leaves scope
    fn drop(&mut self) {
        for listener in &self.listeners {
            let _ = unsafe {
                AudioObjectRemovePropertyListenerBlock(
                    listener.object,
                    &listener.address,
                    self.queue,
                    RcBlock::as_ptr(&listener.block),
                )
            };
        }
        // Complete the native queue drain barrier
        extern "C" fn drained(_: *mut c_void) {}
        unsafe {
            // Callbacks only invalidate; this barrier never waits on a runtime
            // lock held by this worker. Blocks retain their own signal lifetime
            dispatch_sync_f(self.queue, std::ptr::null_mut(), drained);
            dispatch_release(self.queue);
        }
    }
}
