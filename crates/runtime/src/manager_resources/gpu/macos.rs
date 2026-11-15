//! Read-only IORegistry driver counters; no task ports or privileged helpers
use super::Counter;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{c_char, c_void},
    ptr,
};
type Cf = *const c_void;
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> Cf;
    fn IOServiceGetMatchingServices(port: u32, matching: Cf, iterator: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IORegistryEntryCreateIterator(
        entry: u32,
        plane: *const c_char,
        options: u32,
        iterator: *mut u32,
    ) -> i32;
    fn IORegistryEntryCreateCFProperties(
        entry: u32,
        properties: *mut Cf,
        allocator: Cf,
        options: u32,
    ) -> i32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Cf);
    fn CFGetTypeID(value: Cf) -> usize;
    fn CFStringCreateWithCString(allocator: Cf, text: *const c_char, encoding: u32) -> Cf;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetCString(value: Cf, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFDictionaryGetValue(dictionary: Cf, key: Cf) -> Cf;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: Cf) -> isize;
    fn CFArrayGetValueAtIndex(array: Cf, index: isize) -> Cf;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: Cf, kind: isize, value: *mut i64) -> bool;
}
struct Object(u32);
impl Drop for Object {
    // Release this macOS owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}
struct Value(Cf);
impl Drop for Value {
    // Release this macOS owner when it leaves scope
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CFRelease(self.0);
            }
        }
    }
}
// Borrow a named Core Foundation dictionary field after validating its container type
fn field(dictionary: Cf, key: &std::ffi::CStr) -> Cf {
    if dictionary.is_null() || unsafe { CFGetTypeID(dictionary) != CFDictionaryGetTypeID() } {
        return ptr::null();
    }
    let key = Value(unsafe { CFStringCreateWithCString(ptr::null(), key.as_ptr(), 0x08000100) });
    if key.0.is_null() {
        return ptr::null();
    }
    unsafe { CFDictionaryGetValue(dictionary, key.0) }
}
// Decode a nonnegative Core Foundation number into an unsigned counter
fn number(value: Cf) -> Option<u64> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFNumberGetTypeID() } {
        return None;
    }
    let mut n = 0i64;
    (unsafe { CFNumberGetValue(value, 4, &mut n) })
        .then(|| u64::try_from(n).ok())
        .flatten()
}
// Extract a process ID from the GPU entry’s creator string
fn creator(value: Cf) -> Option<u32> {
    if value.is_null() || unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    let mut buffer = [0u8; 512];
    if !unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            buffer.len() as isize,
            0x08000100,
        )
    } {
        return None;
    }
    let text = std::str::from_utf8(&buffer[..buffer.iter().position(|b| *b == 0)?]).ok()?;
    text.strip_prefix("pid ")?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}
// Collect accelerator counters attributed to the selected macOS processes
pub(super) fn counters(pids: &BTreeSet<u32>) -> BTreeMap<String, Counter> {
    let mut output = BTreeMap::new();
    // Every retained object has one release; dictionary values remain borrowed
    // only while their property dictionary is retained
    unsafe {
        let matching = IOServiceMatching(c"IOAccelerator".as_ptr());
        if matching.is_null() {
            return output;
        }
        let mut services = 0;
        if IOServiceGetMatchingServices(0, matching, &mut services) != 0 {
            return output;
        }
        let services = Object(services);
        for _ in 0..64 {
            let device = IOIteratorNext(services.0);
            if device == 0 {
                break;
            }
            let device = Object(device);
            let mut device_id = 0;
            if IORegistryEntryGetRegistryEntryID(device.0, &mut device_id) != 0 {
                continue;
            }
            let mut entries = 0;
            if IORegistryEntryCreateIterator(device.0, c"IOService".as_ptr(), 1, &mut entries) != 0
            {
                continue;
            }
            let entries = Object(entries);
            for _ in 0..4096 {
                let entry = IOIteratorNext(entries.0);
                if entry == 0 {
                    break;
                }
                let entry = Object(entry);
                let mut properties = ptr::null();
                if IORegistryEntryCreateCFProperties(entry.0, &mut properties, ptr::null(), 0) != 0
                {
                    continue;
                }
                let properties = Value(properties);
                let Some(pid) = creator(field(properties.0, c"IOUserClientCreator")) else {
                    continue;
                };
                if !pids.contains(&pid) {
                    continue;
                }
                let mut id = 0;
                if IORegistryEntryGetRegistryEntryID(entry.0, &mut id) != 0 {
                    continue;
                }
                let usage = field(properties.0, c"AppUsage");
                let mut nanos: Option<u64> = None;
                if !usage.is_null() && CFGetTypeID(usage) == CFArrayGetTypeID() {
                    for index in 0..CFArrayGetCount(usage).clamp(0, 256) {
                        if let Some(value) = number(field(
                            CFArrayGetValueAtIndex(usage, index),
                            c"accumulatedGPUTime",
                        )) {
                            nanos = Some(nanos.unwrap_or(0).saturating_add(value));
                        }
                    }
                }
                // Some drivers expose both representations of the same work
                // Prefer the per-API array and use the direct counter as fallback
                let nanos = nanos.or_else(|| number(field(properties.0, c"accumulatedGPUTime")));
                if let Some(nanos) = nanos {
                    output.insert(
                        format!("{device_id}:{id}:{pid}"),
                        Counter {
                            engine: device_id.to_string(),
                            nanos,
                            capacity: 1,
                        },
                    );
                }
            }
        }
    }
    output
}
