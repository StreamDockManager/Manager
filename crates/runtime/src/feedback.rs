//! Transient, demand-driven live state. No window titles or command output are retained here
use crate::{actions::Request, Runtime};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Weak};

#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
pub struct AudioState {
    pub volume: Option<u8>,
    pub muted: Option<bool>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub struct ControlRun {
    queued: usize,
    running: usize,
    last: String,
}
#[derive(Debug, Clone, Default)]
pub struct FeedbackState {
    pub(crate) revision: u64,
    pub sampled_at: Option<std::time::Instant>,
    pub audio: AudioState,
    pub audio_observer: bool,
    pub runs: HashMap<String, ControlRun>,
    icons: HashMap<String, IconState>,
}
#[derive(Debug, Clone)]
struct IconState {
    signature: u64,
    connection_epoch: u64,
    toggled: bool,
}
// Hash an enabled toggle-icon binding to detect stale feedback ownership
pub fn icon_signature(binding: &crate::Binding) -> Option<u64> {
    if binding.disabled || binding.appearance.as_ref()?.toggle_icon.is_none() {
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(binding).ok()?.hash(&mut hash);
    Some(hash.finish())
}
// Request icon signature
pub fn request_icon_signature(runtime: &Runtime, r: &Request) -> Option<u64> {
    let snapshot = runtime.snapshot();
    let scope = if r.event_slot.is_some() {
        crate::event_bindings::strip_event_scope(&r.binding_scope).unwrap_or(&r.binding_scope)
    } else {
        &r.binding_scope
    };
    let (profile, page) = scope.split_once(':')?;
    let binding = snapshot
        .binding_overrides
        .get(&r.device_id)
        .and_then(|values| values.get(&r.control_id))
        .filter(|value| value.scope == scope)
        .map(|value| &value.binding)
        .or_else(|| {
            snapshot
                .profiles
                .iter()
                .find(|p| p.id == profile)?
                .pages
                .iter()
                .find(|p| p.id == page)?
                .bindings
                .get(&r.control_id)
        })?;
    icon_signature(binding)
}
// Report the icon toggled condition for feedback
pub fn icon_toggled(
    runtime: &Runtime,
    device: &str,
    scope: &str,
    control: &str,
    binding: &crate::Binding,
) -> bool {
    let Some(signature) = icon_signature(binding) else {
        return false;
    };
    let epoch = runtime
        .connection_epochs
        .lock()
        .unwrap()
        .get(device)
        .copied();
    runtime
        .feedback_state
        .lock()
        .unwrap()
        .icons
        .get(&format!("{device}:{scope}:{control}"))
        .is_some_and(|state| {
            Some(state.connection_epoch) == epoch && state.signature == signature && state.toggled
        })
}
// Toggle icon feedback only when execution identity and cancellation state still match
pub fn icon_succeeded(runtime: &Runtime, r: &Request) {
    let (Some(signature), Some(epoch)) = (&r.icon_signature, r.connection_epoch) else {
        return;
    };
    if r.cancellation_generation
        != runtime
            .cancellation
            .load(std::sync::atomic::Ordering::SeqCst)
        || runtime.connection_epochs.lock().unwrap().get(&r.device_id) != Some(&epoch)
        || request_icon_signature(runtime, r).as_ref() != Some(signature)
        || matches!(r.action, crate::actions::Action::None)
    {
        return;
    }
    let mut feedback = runtime.feedback_state.lock().unwrap();
    let scope = if r.event_slot.is_some() {
        crate::event_bindings::strip_event_scope(&r.binding_scope).unwrap_or(&r.binding_scope)
    } else {
        &r.binding_scope
    };
    let key = format!("{}:{scope}:{}", r.device_id, r.control_id);
    let state = feedback.icons.entry(key).or_insert_with(|| IconState {
        signature: *signature,
        connection_epoch: epoch,
        toggled: false,
    });
    if state.signature != *signature || state.connection_epoch != epoch {
        *state = IconState {
            signature: *signature,
            connection_epoch: epoch,
            toggled: false,
        };
    }
    state.toggled = !state.toggled;
    feedback.revision = feedback.revision.wrapping_add(1);
}
// Return the key identifying feedback
fn key(r: &Request) -> String {
    format!("{}:{}:{}", r.device_id, r.binding_scope, r.control_id)
}
// Mark the action’s feedback state as queued
pub fn queued(runtime: &Runtime, r: &Request) {
    change(runtime, r, "queued", false);
}
// Mark the action’s feedback state as running
pub fn running(runtime: &Runtime, r: &Request) {
    change(runtime, r, "running", false);
}
// Publish the action’s final feedback state and whether execution actually started
pub fn finished(runtime: &Runtime, r: &Request, state: &str, started: bool) {
    runtime.input_trace.record(r.trace.as_deref(), state, true);
    change(runtime, r, state, started);
}
// Apply the requested feedback change
fn change(runtime: &Runtime, r: &Request, state: &str, started: bool) {
    {
        let mut feedback = runtime.feedback_state.lock().unwrap();
        if feedback.runs.len() > 1000 {
            feedback
                .runs
                .retain(|_, run| run.running > 0 || run.queued > 0);
        }
        feedback.revision = feedback.revision.wrapping_add(1);
        let run = feedback.runs.entry(key(r)).or_default();
        match state {
            "queued" => run.queued += 1,
            "running" => {
                run.queued = run.queued.saturating_sub(1);
                run.running += 1;
            }
            _ => {
                if started {
                    run.running = run.running.saturating_sub(1);
                } else {
                    run.queued = run.queued.saturating_sub(1);
                }
                run.last = state.into();
            }
        }
    }
    wake(runtime);
}
// Wake the feedback worker
pub fn wake(runtime: &Runtime) {
    runtime.emit("live-feedback", None, None, None);
    if !runtime
        .device_wake_pending
        .swap(true, std::sync::atomic::Ordering::SeqCst)
    {
        let _ = runtime.device_commands.send(crate::devices::Request::Wake);
    }
}
// Resolve live application, audio or control feedback into display text
pub fn text(runtime: &Runtime, source: &str, device: &str, scope: &str, control: &str) -> String {
    if source == "application" {
        return runtime
            .automation_status
            .lock()
            .unwrap()
            .application
            .as_ref()
            .map(|application| {
                if application.contains(['/', '\\']) {
                    application
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(application)
                        .trim_end_matches(".exe")
                        .into()
                } else {
                    application.rsplit('.').next().unwrap_or(application).into()
                }
            })
            .unwrap_or_else(|| "Unavailable".into());
    }
    let feedback = runtime.feedback_state.lock().unwrap();
    match source {
        "volume" => feedback
            .audio
            .volume
            .map(|v| format!("{v}%"))
            .unwrap_or_else(|| "Unavailable".into()),
        "mute" => feedback
            .audio
            .muted
            .map(|v| if v { "Muted" } else { "Sound on" }.into())
            .unwrap_or_else(|| "Unavailable".into()),
        "macro" => feedback
            .runs
            .get(&format!("{device}:{scope}:{control}"))
            .map(|r| {
                if r.running > 0 {
                    "Running"
                } else if r.queued > 0 {
                    "Queued"
                } else if r.last.is_empty() {
                    "Idle"
                } else {
                    &r.last
                }
            })
            .unwrap_or("Idle")
            .into(),
        _ => String::new(),
    }
}
/// Immutable feedback sampled once per scene; no external native probes
pub(crate) struct RenderFeedback {
    state: FeedbackState,
    application: Option<String>,
    epoch: Option<u64>,
}
impl RenderFeedback {
    // Snapshot the device connection generation and current application for feedback capture
    pub(crate) fn metadata(runtime: &Runtime, device: &str) -> (Option<u64>, Option<String>) {
        let epoch = runtime
            .connection_epochs
            .lock()
            .unwrap()
            .get(device)
            .copied();
        let application = runtime
            .automation_status
            .lock()
            .unwrap()
            .application
            .clone();
        (epoch, application)
    }
    // Capture the current feedback observation
    pub(crate) fn capture(
        runtime: &Runtime,
        device: &str,
        metadata: (Option<u64>, Option<String>),
    ) -> Self {
        let (epoch, application) = metadata;
        // Called under plugin hub lock: feedback is a leaf lock; producers release
        // it before waking the device actor or looking up plugin/runtime state
        let state = {
            let live = runtime.feedback_state.lock().unwrap();
            let prefix = format!("{device}:");
            FeedbackState {
                revision: live.revision,
                sampled_at: live.sampled_at,
                audio: live.audio.clone(),
                audio_observer: live.audio_observer,
                runs: live
                    .runs
                    .iter()
                    .filter(|(key, _)| key.starts_with(&prefix))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
                icons: live
                    .icons
                    .iter()
                    .filter(|(key, _)| key.starts_with(&prefix))
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            }
        };
        Self {
            state,
            application,
            epoch,
        }
    }
    // Report the connected condition for feedback
    pub(crate) fn connected(&self, runtime: &Runtime, device: &str) -> bool {
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .get(device)
            .copied()
            == self.epoch
    }
    // Report the metadata current condition for feedback
    pub(crate) fn metadata_current(&self, runtime: &Runtime, device: &str) -> bool {
        Self::metadata(runtime, device) == (self.epoch, self.application.clone())
    }
    #[cfg(test)]
    // Return the current feedback value
    pub(crate) fn current(&self, runtime: &Runtime, device: &str) -> bool {
        let revision = runtime.feedback_state.lock().unwrap().revision;
        let epoch = runtime
            .connection_epochs
            .lock()
            .unwrap()
            .get(device)
            .copied();
        let application = runtime
            .automation_status
            .lock()
            .unwrap()
            .application
            .clone();
        revision == self.state.revision && epoch == self.epoch && application == self.application
    }
    // Render feedback text from the captured observation instead of rereading live state
    pub(crate) fn text(&self, source: &str, device: &str, scope: &str, control: &str) -> String {
        match source {
            "application" => self
                .application
                .as_ref()
                .map(|application| {
                    if application.contains(['/', '\\']) {
                        application
                            .rsplit(['/', '\\'])
                            .next()
                            .unwrap_or(application)
                            .trim_end_matches(".exe")
                            .into()
                    } else {
                        application.rsplit('.').next().unwrap_or(application).into()
                    }
                })
                .unwrap_or_else(|| "Unavailable".into()),
            "volume" => self
                .state
                .audio
                .volume
                .map(|v| format!("{v}%"))
                .unwrap_or_else(|| "Unavailable".into()),
            "mute" => self
                .state
                .audio
                .muted
                .map(|v| if v { "Muted" } else { "Sound on" }.into())
                .unwrap_or_else(|| "Unavailable".into()),
            "macro" => self
                .state
                .runs
                .get(&format!("{device}:{scope}:{control}"))
                .map(|r| {
                    if r.running > 0 {
                        "Running"
                    } else if r.queued > 0 {
                        "Queued"
                    } else if r.last.is_empty() {
                        "Idle"
                    } else {
                        &r.last
                    }
                })
                .unwrap_or("Idle")
                .into(),
            _ => String::new(),
        }
    }
    // Report the icon toggled condition for feedback
    pub(crate) fn icon_toggled(
        &self,
        device: &str,
        scope: &str,
        control: &str,
        binding: &crate::Binding,
    ) -> bool {
        let Some(signature) = icon_signature(binding) else {
            return false;
        };
        self.state
            .icons
            .get(&format!("{device}:{scope}:{control}"))
            .is_some_and(|state| {
                Some(state.connection_epoch) == self.epoch
                    && state.signature == signature
                    && state.toggled
            })
    }
}
// Start the feedback operation
pub fn start(runtime: Weak<Runtime>) {
    crate::audio_observer::start(runtime);
}
// Request refresh
pub fn request_refresh(runtime: &Runtime) {
    runtime.audio_signal.invalidate(false);
}
// Refresh the feedback observation
pub fn refresh(runtime: &Runtime) {
    publish_audio(runtime, read_audio(), false);
}
// Publish audio
pub(crate) fn publish_audio(runtime: &Runtime, audio: AudioState, observing: bool) {
    let changed = {
        let mut state = runtime.feedback_state.lock().unwrap();
        let changed = state.audio != audio;
        state.audio = audio;
        if changed {
            state.revision = state.revision.wrapping_add(1);
        }
        state.audio_observer = observing;
        state.sampled_at = Some(std::time::Instant::now());
        changed
    };
    runtime.sampling_signal.notify();
    if changed {
        wake(runtime);
    }
}
#[cfg(target_os = "linux")]
// Read audio
pub fn read_audio() -> AudioState {
    use stream_dock_manager_plugin_host::preflight::probe;
    let volume = probe("pactl", &["get-sink-volume", "@DEFAULT_SINK@"])
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .find_map(|v| v.strip_suffix('%').and_then(|v| v.parse::<u16>().ok()))
                .map(|v| v.min(100) as u8)
        });
    let muted = probe("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])
        .ok()
        .and_then(|s| match s.trim().rsplit_once(' ').map(|(_, v)| v) {
            Some("yes") => Some(true),
            Some("no") => Some(false),
            _ => None,
        });
    AudioState {
        volume,
        muted,
        error: if volume.is_none() || muted.is_none() {
            Some("Default audio output or pactl unavailable".into())
        } else {
            None
        },
    }
}
#[cfg(target_os = "macos")]
// Read audio
pub fn read_audio() -> AudioState {
    // Return the requested feedback value
    fn get<T: Default>(
        object: u32,
        selector: &[u8; 4],
        scope: &[u8; 4],
        element: u32,
    ) -> Option<T> {
        let address = crate::core_audio::address(selector, scope, element);
        let mut value = T::default();
        let mut size = std::mem::size_of::<T>() as u32;
        (unsafe {
            crate::core_audio::AudioObjectGetPropertyData(
                object,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                (&mut value as *mut T).cast(),
            )
        } == 0)
            .then_some(value)
    }
    let device = get::<u32>(1, b"dOut", b"glob", 0);
    let volume = device
        .and_then(|d| {
            get::<f32>(d, b"volm", b"outp", 0).or_else(|| {
                let l = get::<f32>(d, b"volm", b"outp", 1)?;
                let r = get::<f32>(d, b"volm", b"outp", 2).unwrap_or(l);
                Some((l + r) / 2.0)
            })
        })
        .filter(|v| v.is_finite())
        .map(|v| (v.clamp(0.0, 1.0) * 100.0).round() as u8);
    let muted = device
        .and_then(|d| get::<u32>(d, b"mute", b"outp", 0))
        .map(|v| v != 0);
    AudioState {
        volume,
        muted,
        error: if device.is_none() {
            Some("Default audio device unavailable".into())
        } else {
            None
        },
    }
}
#[cfg(target_os = "windows")]
// Read audio
pub fn read_audio() -> AudioState {
    use windows::Win32::{
        Media::Audio::{
            eMultimedia, eRender, Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator,
            MMDeviceEnumerator,
        },
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        },
    };
    let result = (|| -> windows::core::Result<(u8, bool)> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
            let result = (|| {
                let devices: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let device = devices.GetDefaultAudioEndpoint(eRender, eMultimedia)?;
                let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
                Ok((
                    (endpoint.GetMasterVolumeLevelScalar()?.clamp(0.0, 1.0) * 100.0).round() as u8,
                    endpoint.GetMute()?.as_bool(),
                ))
            })();
            CoUninitialize();
            result
        }
    })();
    match result {
        Ok((volume, muted)) => AudioState {
            volume: Some(volume),
            muted: Some(muted),
            error: None,
        },
        Err(_) => AudioState {
            error: Some("Default audio output unavailable".into()),
            ..Default::default()
        },
    }
}
#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub(crate) fn CFRelease(value: *const std::ffi::c_void);
    fn CFStringCreateWithBytes(
        allocator: *const std::ffi::c_void,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: bool,
    ) -> *const std::ffi::c_void;
    fn CFStringGetCString(
        value: *const std::ffi::c_void,
        buffer: *mut i8,
        size: isize,
        encoding: u32,
    ) -> bool;
    fn CFGetTypeID(value: *const std::ffi::c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFDictionaryCreate(
        allocator: *const std::ffi::c_void,
        keys: *const *const std::ffi::c_void,
        values: *const *const std::ffi::c_void,
        count: isize,
        key_callbacks: *const std::ffi::c_void,
        value_callbacks: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    static kCFBooleanTrue: *const std::ffi::c_void;
}
#[cfg(target_os = "macos")]
// Allocate a UTF-8 Core Foundation string for the native caller
pub(crate) unsafe fn cf_string(s: &str) -> *const std::ffi::c_void {
    CFStringCreateWithBytes(
        std::ptr::null(),
        s.as_ptr(),
        s.len() as isize,
        0x08000100,
        false,
    )
}
#[cfg(target_os = "macos")]
// Decode a bounded Core Foundation string after verifying its runtime type
pub(crate) unsafe fn cf_to_string(s: *const std::ffi::c_void) -> Option<String> {
    if CFGetTypeID(s) != CFStringGetTypeID() {
        return None;
    }
    let mut buffer = vec![0i8; 16385];
    if !CFStringGetCString(s, buffer.as_mut_ptr(), buffer.len() as isize, 0x08000100) {
        return None;
    }
    Some(
        std::ffi::CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .chars()
            .take(4096)
            .collect(),
    )
}
#[cfg(target_os = "macos")]
// Ask macOS to present its Accessibility permission prompt
pub(crate) unsafe fn accessibility_prompt() {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const std::ffi::c_void;
    }
    let options = CFDictionaryCreate(
        std::ptr::null(),
        &kAXTrustedCheckOptionPrompt,
        &kCFBooleanTrue,
        1,
        std::ptr::null(),
        std::ptr::null(),
    );
    AXIsProcessTrustedWithOptions(options);
    CFRelease(options);
}
