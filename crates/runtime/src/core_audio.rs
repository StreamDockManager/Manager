//! Shared Core Audio ABI. Subscription ownership stays in audio_observer
use std::ffi::c_void;
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}
// Encode four-character Core Audio selector and scope codes into a property address
pub(crate) fn address(selector: &[u8; 4], scope: &[u8; 4], element: u32) -> Address {
    Address {
        selector: u32::from_be_bytes(*selector),
        scope: u32::from_be_bytes(*scope),
        element,
    }
}
#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    pub(crate) fn AudioObjectHasProperty(object: u32, address: *const Address) -> u8;
    pub(crate) fn AudioObjectGetPropertyData(
        object: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}
