use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
#[cfg(test)]
pub(super) mod testing {
    use std::cell::RefCell;
    #[derive(Default)]
    pub struct Fake {
        pub enabled: bool,
        pub clipboard: String,
        pub events: Vec<String>,
    }
    thread_local! {pub static STATE:RefCell<Fake>=RefCell::new(Fake::default());}
    // Reset and enable the thread-local fake input backend for tests
    pub fn enable() {
        STATE.with(|s| {
            *s.borrow_mut() = Fake {
                enabled: true,
                clipboard: "selected text".into(),
                events: vec![],
            }
        });
    }
    // Report the enabled condition for native
    pub fn enabled() -> bool {
        STATE.with(|s| s.borrow().enabled)
    }
    // Record the new native observation
    pub fn record(event: &str) {
        STATE.with(|s| s.borrow_mut().events.push(event.into()));
    }
    // Read the thread-local fake clipboard for input tests
    pub fn clipboard() -> String {
        STATE.with(|s| s.borrow().clipboard.clone())
    }
    // Set clipboard
    pub fn set_clipboard(text: &str) {
        STATE.with(|s| s.borrow_mut().clipboard = text.into());
    }
}
#[derive(Default)]
struct InputState {
    generation: usize,
    keyboard: Option<Enigo>,
    keys: HashMap<String, Vec<HeldKey>>,
    macro_keys: HashMap<String, Vec<u16>>,
    buttons: HashMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeldKey {
    name: String,
    resolved: Key,
}
static INPUT_GENERATION: AtomicUsize = AtomicUsize::new(0);
// Updated while INPUT owns each macro event; short admission never waits behind
// an ordinary text insertion merely to inspect residual macro ownership
static MACRO_KEYS_OWNED: AtomicBool = AtomicBool::new(false);
static INPUT: OnceLock<Mutex<InputState>> = OnceLock::new();
static CLIPBOARD: Mutex<()> = Mutex::new(());
static CHORD: Mutex<()> = Mutex::new(());

/// Admission is separate from INPUT: no mutex is held during a recording or a
/// timed playback wait. Cleanup never needs to enter the ordinary input lane
#[derive(Default)]
struct KeyboardAdmission {
    operations: usize,
    exclusive: Option<(String, Arc<AtomicBool>)>,
}
impl KeyboardAdmission {
    // Begin operation
    fn begin_operation(&mut self, residual_macro_keys: bool) -> Result<(), String> {
        if self.exclusive.is_some() {
            return Err("Keyboard input is busy recording or playing a macro".into());
        }
        if residual_macro_keys {
            return Err("Keyboard macro cleanup is incomplete; keyboard output is blocked".into());
        }
        self.operations = self
            .operations
            .checked_add(1)
            .ok_or("Keyboard input is busy")?;
        Ok(())
    }
    // Acquire the owned native resource
    fn acquire(&mut self, owner: &str, held: bool) -> Result<Arc<AtomicBool>, String> {
        if self.exclusive.is_some() || self.operations != 0 {
            return Err("Keyboard input is busy".into());
        }
        if held {
            return Err(
                "Release held or latched keys before recording or playing a keyboard macro".into(),
            );
        }
        let token = Arc::new(AtomicBool::new(false));
        self.exclusive = Some((owner.into(), token.clone()));
        Ok(token)
    }
    // Finalize the pending native operation
    fn finish(&mut self, owner: &str, token: &Arc<AtomicBool>) {
        if self
            .exclusive
            .as_ref()
            .is_some_and(|(active_owner, active_token)| {
                active_owner == owner && Arc::ptr_eq(active_token, token)
            })
        {
            self.exclusive = None;
        }
    }
    // Cancel the pending native operation
    fn cancel(&self, prefix: &str) {
        if let Some((owner, token)) = &self.exclusive {
            if owner.starts_with(prefix) {
                token.store(true, Ordering::SeqCst);
            }
        }
    }
}
static KEYBOARD_ADMISSION: Mutex<KeyboardAdmission> = Mutex::new(KeyboardAdmission {
    operations: 0,
    exclusive: None,
});
pub(crate) struct KeyboardLease {
    owner: String,
    cancelled: Arc<AtomicBool>,
    generation: usize,
}
impl KeyboardLease {
    /// Owned atomic-only press guard; queued output cannot extend lease validity
    pub(crate) fn press_guard(&self) -> crate::keyboard_macro::playback::PressGuard {
        let canceled = self.cancelled.clone();
        let generation = self.generation;
        Arc::new(move || {
            if canceled.load(Ordering::SeqCst)
                || generation != INPUT_GENERATION.load(Ordering::SeqCst)
            {
                Err("Cancelled".into())
            } else {
                Ok(())
            }
        })
    }

    // Check the current native conditions
    pub(crate) fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::SeqCst)
            || self.generation != INPUT_GENERATION.load(Ordering::SeqCst)
        {
            return Err("Cancelled".into());
        }
        Ok(())
    }
}
impl Drop for KeyboardLease {
    // Release this native owner when it leaves scope
    fn drop(&mut self) {
        let mut admission = KEYBOARD_ADMISSION.lock().unwrap_or_else(|e| e.into_inner());
        admission.finish(&self.owner, &self.cancelled);
    }
}
struct KeyboardOperation;
impl Drop for KeyboardOperation {
    // Release this native owner when it leaves scope
    fn drop(&mut self) {
        let mut admission = KEYBOARD_ADMISSION.lock().unwrap_or_else(|e| e.into_inner());
        admission.operations = admission.operations.saturating_sub(1);
    }
}
// Report the keyboard macro busy condition for native
pub(crate) fn keyboard_macro_busy() -> bool {
    let Ok(admission) = KEYBOARD_ADMISSION.lock() else {
        return true;
    };
    admission.exclusive.is_some() || MACRO_KEYS_OWNED.load(Ordering::SeqCst)
}
// Admit ordinary keyboard output only when no exclusive macro session owns the lane
fn keyboard_operation() -> Result<KeyboardOperation, String> {
    let mut admission = KEYBOARD_ADMISSION
        .lock()
        .map_err(|_| "Keyboard input is unavailable")?;
    // No INPUT wait here: existing text insertion can hold that lock. The
    // ownership snapshot is published before the exclusive lease is released
    if admission.exclusive.is_some() {
        return Err("Keyboard input is busy recording or playing a macro".into());
    }
    let residual_macro_keys = MACRO_KEYS_OWNED.load(Ordering::SeqCst);
    admission.begin_operation(residual_macro_keys)?;
    Ok(KeyboardOperation)
}
// Attempt keyboard lease
pub(crate) fn try_keyboard_lease(owner: &str) -> Result<KeyboardLease, String> {
    let mut admission = KEYBOARD_ADMISSION
        .lock()
        .map_err(|_| "Keyboard input is unavailable")?;
    // Do not wait behind INPUT when a text insertion already owns the lane
    if admission.exclusive.is_some() || admission.operations != 0 {
        return Err("Keyboard input is busy".into());
    }
    let state = lock_input();
    let cancelled = admission.acquire(
        owner,
        !state.keys.is_empty() || !state.macro_keys.is_empty(),
    )?;
    Ok(KeyboardLease {
        owner: owner.into(),
        cancelled,
        generation: state.generation,
    })
}
// Attempt recording lease
pub(crate) fn try_recording_lease() -> Result<KeyboardLease, String> {
    try_keyboard_lease(&format!("recording:{}", uuid::Uuid::new_v4()))
}
// Cancel keyboard lease
fn cancel_keyboard_lease(prefix: &str) {
    let admission = KEYBOARD_ADMISSION.lock().unwrap_or_else(|e| e.into_inner());
    admission.cancel(prefix);
}
/// Track resolved physical codes, not logical key names or a new layout lookup
/// The callback must not log event content or invoke an ordinary keyboard lane
pub(crate) fn macro_key_event(
    owner: &str,
    code: u16,
    kind: crate::keyboard_macro::KeyboardMacroEventKind,
    send: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    macro_key_event_tracked(
        &mut lock_input(),
        &MACRO_KEYS_OWNED,
        owner,
        code,
        kind,
        send,
    )
}
// Send a macro key event while tracking owned pressed keys
fn macro_key_event_tracked(
    state: &mut InputState,
    owned_snapshot: &AtomicBool,
    owner: &str,
    code: u16,
    kind: crate::keyboard_macro::KeyboardMacroEventKind,
    send: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let result = macro_key_event_resolved(state, owner, code, kind, send);
    owned_snapshot.store(!state.macro_keys.is_empty(), Ordering::SeqCst);
    result
}
// Send a macro key event using the resolved physical key
fn macro_key_event_resolved(
    state: &mut InputState,
    owner: &str,
    code: u16,
    kind: crate::keyboard_macro::KeyboardMacroEventKind,
    send: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    use crate::keyboard_macro::KeyboardMacroEventKind::*;
    let owned = state
        .macro_keys
        .get(owner)
        .is_some_and(|keys| keys.contains(&code));
    match kind {
        KeyDown => {
            if state.macro_keys.values().any(|keys| keys.contains(&code)) {
                return Err("Keyboard macro key is already owned".into());
            }
            // Retain ownership even on an uncertain native failure, until cleanup
            state.macro_keys.entry(owner.into()).or_default().push(code);
        }
        KeyUp if !owned => return Ok(()),
        Repeat if !owned => return Err("Keyboard macro repeat is not owned".into()),
        KeyUp | Repeat => (),
    }
    send()?;
    if kind == KeyUp {
        let held = state.macro_keys.get_mut(owner).unwrap();
        held.retain(|key| *key != code);
        if held.is_empty() {
            state.macro_keys.remove(owner);
        }
    }
    Ok(())
}
// Handle the incoming native input
fn input() -> &'static Mutex<InputState> {
    INPUT.get_or_init(Default::default)
}
// Synchronize input ownership with the current cancellation generation
fn sync_input_generation(state: &mut InputState, generation: usize) {
    if state.generation != generation {
        state.keys.clear();
        state.buttons.clear();
        state.generation = generation;
    }
}
// Lock input
fn lock_input() -> std::sync::MutexGuard<'static, InputState> {
    let mut state = input().lock().unwrap();
    sync_input_generation(&mut state, INPUT_GENERATION.load(Ordering::SeqCst));
    state
}
// Read the generation that invalidates stale native input ownership
pub(super) fn input_generation() -> usize {
    INPUT_GENERATION.load(Ordering::SeqCst)
}
#[cfg(target_os = "linux")]
// Invalidate input
pub(crate) fn invalidate_input() {
    INPUT_GENERATION.fetch_add(1, Ordering::SeqCst);
}
// Report the wayland condition for native
pub(crate) fn wayland() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland")
            || std::env::var_os("WAYLAND_DISPLAY").is_some())
}
// Initialize the native keyboard backend lazily and borrow it for the current operation
fn keyboard(state: &mut InputState) -> Result<&mut Enigo, String> {
    if state.keyboard.is_none() {
        state.keyboard = Some(Enigo::new(&Settings::default()).map_err(|e| e.to_string())?)
    }
    Ok(state.keyboard.as_mut().unwrap())
}
// Send key
fn send_key(state: &mut InputState, key: &HeldKey, pressed: bool) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if wayland() {
        let result = crate::wayland::key_state(&key.name, pressed);
        if state.generation != INPUT_GENERATION.load(Ordering::SeqCst) {
            if pressed {
                let _ = crate::wayland::key_state(&key.name, false);
            }
            return Err("Input authorization changed; retry the action".into());
        }
        return result;
    }
    keyboard(state)?
        .key(
            key.resolved,
            if pressed {
                Direction::Press
            } else {
                Direction::Release
            },
        )
        .map_err(|e| e.to_string())
}
// Retain the requested input hold until its owner releases it
pub fn hold(owner: &str, keys: &[String]) -> Result<(), String> {
    let _operation = keyboard_operation()?;
    #[cfg(test)]
    if testing::enabled() {
        testing::record("hold");
        return Ok(());
    }
    if lock_input().keys.contains_key(owner) {
        return Ok(());
    }
    let mut names = vec![];
    for key in keys {
        let name: String = match key.to_lowercase().as_str() {
            "control" => "ctrl".into(),
            "meta" | "super" => "cmd".into(),
            s => s.into(),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    // Complete every fallible layout lookup before acquiring INPUT or pressing
    // modifiers. The main thread may need INPUT during cancellation/shutdown
    let parsed = names
        .iter()
        .map(|name| crate::actions::parse_key(name))
        .collect::<Result<Vec<_>, _>>()?;
    let resolved = crate::keyboard_layout::resolve(parsed)?;
    let keys = names
        .into_iter()
        .zip(resolved)
        .map(|(name, resolved)| HeldKey { name, resolved })
        .collect();
    hold_resolved(&mut lock_input(), owner, keys, send_key)
}
// Hold the resolved physical key under the input owner
fn hold_resolved(
    state: &mut InputState,
    owner: &str,
    mut keys: Vec<HeldKey>,
    mut send: impl FnMut(&mut InputState, &HeldKey, bool) -> Result<(), String>,
) -> Result<(), String> {
    if state.keys.contains_key(owner) {
        return Ok(());
    }
    let mut sent = vec![];
    for key in &mut keys {
        if let Some(existing) = state.keys.values().flatten().find(|k| k.name == key.name) {
            // Another owner may have pressed this key before a layout switch
            // Share its original code so the last release matches the press
            key.resolved = existing.resolved;
        } else {
            if let Err(e) = send(state, key, true) {
                for key in sent.iter().rev() {
                    let _ = send(state, key, false);
                }
                return Err(e);
            }
            sent.push(key.clone());
        }
    }
    state.keys.insert(owner.into(), keys);
    Ok(())
}
// Release the owned native resource
pub fn release(owner: &str) {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("release");
        return;
    }
    release_resolved(&mut lock_input(), owner, send_key);
}
// Release resolved
fn release_resolved(
    state: &mut InputState,
    owner: &str,
    mut send: impl FnMut(&mut InputState, &HeldKey, bool) -> Result<(), String>,
) {
    if let Some(keys) = state.keys.remove(owner) {
        for key in keys.iter().rev() {
            if !state.keys.values().flatten().any(|k| k.name == key.name) {
                let _ = send(state, key, false);
            }
        }
    }
}
// Release prefix
pub fn release_prefix(prefix: &str) {
    cancel_keyboard_lease(prefix);
    #[cfg(test)]
    if testing::enabled() {
        testing::record("cleanup");
        return;
    }
    let owners: Vec<_> = input()
        .lock()
        .unwrap()
        .keys
        .keys()
        .filter(|k| k.starts_with(prefix) && (!prefix.is_empty() || !k.starts_with("plugin:")))
        .cloned()
        .collect();
    for owner in owners {
        release(&owner)
    }
    let buttons: Vec<_> = input()
        .lock()
        .unwrap()
        .buttons
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (o, b) in buttons {
        let _ = button(&o, &b, false);
    }
}
// Toggle ownership of the requested held-key latch
pub fn latch(owner: &str, keys: &[String]) -> Result<(), String> {
    let _operation = keyboard_operation()?;
    #[cfg(test)]
    if testing::enabled() {
        testing::record("latch");
        return Ok(());
    }
    let held = lock_input().keys.contains_key(owner);
    if held {
        release(owner);
        Ok(())
    } else {
        hold(owner, keys)
    }
}
// Hold a key on behalf of the requesting plugin
pub fn plugin_hold(owner: &str, keys: &[String]) -> Result<(), String> {
    hold(&format!("plugin:{owner}"), keys)
}
// Release a key held by the requesting plugin
pub fn plugin_release(owner: &str) {
    release(&format!("plugin:{owner}"));
}
// Send the requested key chord with owned press and release tracking
pub fn chord(keys: &[String]) -> Result<(), String> {
    let _operation = keyboard_operation()?;
    let _chord = CHORD.lock().unwrap();
    #[cfg(test)]
    if testing::enabled() {
        testing::record("chord");
        return Ok(());
    }
    let owner = format!("chord:{}", uuid::Uuid::new_v4());
    hold(&owner, keys)?;
    release(&owner);
    Ok(())
}
#[cfg(any(target_os = "macos", target_os = "windows", test))]
// Hold keyboard admission across the complete native output callback
fn admitted_keyboard_output<G>(
    admit: impl FnOnce() -> Result<G, String>,
    output: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let _operation = admit()?;
    output()
}
/// Consumer/media key actions use the same transaction lane and key ownership as
/// ordinary shortcuts. Non-key platform volume APIs intentionally do not use it
#[cfg(any(target_os = "macos", target_os = "windows"))]
// Send repeated system-key taps under keyboard admission and owned release tracking
pub(crate) fn tap_system_key(key: Key, count: u32) -> Result<(), String> {
    admitted_keyboard_output(keyboard_operation, || {
        #[cfg(test)]
        if testing::enabled() {
            testing::record("system-key");
            return Ok(());
        }
        let _chord = CHORD.lock().unwrap();
        let owner = format!("system-key:{}", uuid::Uuid::new_v4());
        let held = HeldKey {
            name: format!("system-key:{key:?}"),
            resolved: key,
        };
        for _ in 0..count {
            hold_resolved(&mut lock_input(), &owner, vec![held.clone()], send_key)?;
            release(&owner);
        }
        Ok(())
    })
}
// Translate the requested mouse button into its native representation
fn mouse_button(name: &str) -> Result<Button, String> {
    Ok(match name {
        "left" => Button::Left,
        "right" => Button::Right,
        "middle" => Button::Middle,
        "back" => Button::Back,
        "forward" => Button::Forward,
        _ => return Err("Unsupported mouse button".into()),
    })
}
// Apply a named mouse-button transition under the supplied input owner
pub fn button(owner: &str, name: &str, pressed: bool) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("button");
        return Ok(());
    }
    let mut state = lock_input();
    let other = state.buttons.iter().any(|(k, v)| k != owner && v == name);
    if !other {
        #[cfg(target_os = "linux")]
        if wayland() {
            crate::wayland::pointer_button(name, pressed)?;
        } else {
            keyboard(&mut state)?
                .button(
                    mouse_button(name)?,
                    if pressed {
                        Direction::Press
                    } else {
                        Direction::Release
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        #[cfg(not(target_os = "linux"))]
        keyboard(&mut state)?
            .button(
                mouse_button(name)?,
                if pressed {
                    Direction::Press
                } else {
                    Direction::Release
                },
            )
            .map_err(|e| e.to_string())?;
    }
    if state.generation != INPUT_GENERATION.load(Ordering::SeqCst) {
        #[cfg(target_os = "linux")]
        if wayland() && pressed {
            let _ = crate::wayland::pointer_button(name, false);
        }
        return Err("Input authorization changed; retry the action".into());
    }
    if pressed {
        state.buttons.insert(owner.into(), name.into());
    } else {
        state.buttons.remove(owner);
    }
    Ok(())
}
// Send the requested mouse click
pub fn click(name: &str, count: i32) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("click");
        return Ok(());
    }
    let owner = format!("click:{}", uuid::Uuid::new_v4());
    for _ in 0..count {
        button(&owner, name, true)?;
        button(&owner, name, false)?;
    }
    Ok(())
}
// Move the pointer to the requested destination
pub fn move_pointer(x: i32, y: i32, relative: bool) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("move");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if wayland() {
        return crate::wayland::pointer_move(x, y, relative);
    }
    keyboard(&mut lock_input())?
        .move_mouse(
            x,
            y,
            if relative {
                Coordinate::Rel
            } else {
                Coordinate::Abs
            },
        )
        .map_err(|e| e.to_string())
}
// Send the requested scroll operation
pub fn scroll(x: i32, y: i32) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("scroll");
        return Ok(());
    }
    #[cfg(target_os = "linux")]
    if wayland() {
        return crate::wayland::pointer_scroll(x, y);
    }
    let mut state = lock_input();
    let kb = keyboard(&mut state)?;
    if x != 0 {
        kb.scroll(x, Axis::Horizontal).map_err(|e| e.to_string())?
    }
    if y != 0 {
        kb.scroll(y, Axis::Vertical).map_err(|e| e.to_string())?
    }
    Ok(())
}
// Read the current insertion target or the fake target selected by tests
pub fn target() -> Option<String> {
    #[cfg(test)]
    if testing::enabled() {
        return Some("mock-target".into());
    }
    crate::desktop_input::target()
}
// Check target
fn check_target(expected: Option<&str>) -> Result<(), String> {
    if expected.is_none() || target().as_deref() != expected {
        return Err("Insertion target changed or cannot be verified; no text was inserted".into());
    }
    Ok(())
}
// Restore target
pub fn restore_target(expected: Option<&str>) {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("restore_target");
        return;
    }
    let Some(expected) = expected else { return };
    #[cfg(target_os = "macos")]
    if let Some((pid, _)) = expected.split_once(':') {
        if let Ok(pid) = pid.parse::<u32>() {
            let _=Command::new("osascript").args(["-e","on run argv\ntell application \"System Events\" to set frontmost of first application process whose unix id is (item 1 of argv as integer) to true\nend run","--",&pid.to_string()]).status();
        }
    }
    #[cfg(target_os = "windows")]
    if expected.parse::<u64>().is_ok() {
        let _=crate::native_process::command("powershell").args(["-NoProfile","-NonInteractive","-Command","Add-Type 'using System;using System.Runtime.InteropServices;public class W{[DllImport(\"user32.dll\")]public static extern bool SetForegroundWindow(IntPtr h);}';[W]::SetForegroundWindow([IntPtr]::new([long]$env:SD_WINDOW))"]).env("SD_WINDOW",expected).status();
    }
    #[cfg(target_os = "linux")]
    if !wayland() {
        let _ = Command::new("xdotool")
            .args(["windowactivate", "--sync", expected])
            .status();
    }
}
// Send the requested text through the selected native input path
pub fn type_text(text: &str, expected: Option<&str>) -> Result<(), String> {
    let _operation = keyboard_operation()?;
    #[cfg(test)]
    if testing::enabled() {
        testing::record(text);
        return Ok(());
    }
    check_target(expected)?;
    #[cfg(target_os = "linux")]
    if wayland() {
        return Err(
            "Direct text insertion cannot verify the target on Wayland; use Set clipboard".into(),
        );
    }
    keyboard(&mut lock_input())?
        .text(text)
        .map_err(|e| e.to_string())
}
// Run the command and return Unicode stdout, reporting stderr on unsuccessful exit
fn output(command: &mut Command) -> Result<String, String> {
    let out = command
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    String::from_utf8(out.stdout).map_err(|_| "Command returned non-Unicode text".into())
}
// Read clipboard inner
fn read_clipboard_inner() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    let text = output(Command::new("osascript").args(["-e", "the clipboard as «class utf8»"]))?
        .strip_suffix('\n')
        .unwrap_or("")
        .to_string();
    #[cfg(target_os="windows")]
    let text=output(crate::native_process::command("powershell").args(["-NoProfile","-NonInteractive","-Command","Add-Type -AssemblyName System.Windows.Forms;if(-not [Windows.Forms.Clipboard]::ContainsText()){exit 2};[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;[Console]::Write([Windows.Forms.Clipboard]::GetText())"]))?;
    #[cfg(target_os = "linux")]
    let text = if wayland() {
        output(Command::new("wl-paste").args(["--no-newline", "--type", "text"]))?
    } else {
        output(Command::new("xclip").args([
            "-selection",
            "clipboard",
            "-out",
            "-target",
            "UTF8_STRING",
        ]))?
    };
    if text.len() > 256 * 1024 {
        return Err("Clipboard text exceeds 256 KiB".into());
    }
    Ok(text)
}
// Read clipboard
pub fn read_clipboard() -> Result<String, String> {
    #[cfg(test)]
    if testing::enabled() {
        return Ok(testing::clipboard());
    }
    let _lock = CLIPBOARD.lock().unwrap();
    read_clipboard_inner()
}
// Set clipboard
pub fn set_clipboard(text: &str) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::set_clipboard(text);
        return Ok(());
    }
    let _lock = CLIPBOARD.lock().unwrap();
    crate::desktop_input::copy(text)
}
// Read a stamp identifying the current clipboard contents
fn clipboard_stamp() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    return output(Command::new("osascript").args([
        "-l",
        "JavaScript",
        "-e",
        "ObjC.import('AppKit'); String($.NSPasteboard.generalPasteboard.changeCount)",
    ]))
    .map(|s| s.trim().into());
    #[cfg(target_os="windows")]
    return output(crate::native_process::command("powershell").args(["-NoProfile","-NonInteractive","-Command","Add-Type 'using System.Runtime.InteropServices;public class C{[DllImport(\"user32.dll\")]public static extern uint GetClipboardSequenceNumber();}';[C]::GetClipboardSequenceNumber()"])).map(|s|s.trim().into());
    #[cfg(target_os = "linux")]
    {
        if wayland() {
            return Err("This clipboard backend cannot verify restoration ownership".into());
        }
        output(Command::new("xclip").args([
            "-selection",
            "clipboard",
            "-out",
            "-target",
            "TIMESTAMP",
        ]))
    }
}
// Capture the clipboard text and its change stamp
fn snapshot_text() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let formats=output(Command::new("osascript").args(["-l","JavaScript","-e","ObjC.import('AppKit');JSON.stringify(ObjC.deepUnwrap($.NSPasteboard.generalPasteboard.types))"]))?;
        let formats: Vec<String> =
            serde_json::from_str(&formats).map_err(|_| "Clipboard formats are unavailable")?;
        if formats.iter().any(|f| {
            ![
                "public.utf8-plain-text",
                "public.utf16-plain-text",
                "public.utf16-external-plain-text",
                "public.text",
                "NSStringPboardType",
                "com.apple.traditional-mac-plain-text",
            ]
            .contains(&f.as_str())
        }) {
            return Err(
                "Clipboard contains unsupported non-text formats; it was not changed".into(),
            );
        }
    }
    #[cfg(target_os = "windows")]
    {
        let formats=output(crate::native_process::command("powershell").args(["-NoProfile","-NonInteractive","-Command","Add-Type -AssemblyName System.Windows.Forms;[Windows.Forms.Clipboard]::GetDataObject().GetFormats($false) | ForEach-Object { [Console]::WriteLine($_) }"]))?;
        if formats.lines().any(|f| {
            !["Text", "UnicodeText", "OEMText", "System.String", "Locale"].contains(&f.trim())
        }) {
            return Err(
                "Clipboard contains unsupported non-text formats; it was not changed".into(),
            );
        }
    }
    #[cfg(target_os = "linux")]
    {
        if wayland() {
            return Err("Verified clipboard restoration is unavailable on Wayland".into());
        }
        let formats = output(Command::new("xclip").args([
            "-selection",
            "clipboard",
            "-out",
            "-target",
            "TARGETS",
        ]))?;
        if formats.lines().any(|f| {
            ![
                "TIMESTAMP",
                "TARGETS",
                "SAVE_TARGETS",
                "MULTIPLE",
                "UTF8_STRING",
                "STRING",
                "TEXT",
                "COMPOUND_TEXT",
                "text/plain",
                "text/plain;charset=utf-8",
            ]
            .contains(&f.trim())
        }) {
            return Err(
                "Clipboard contains unsupported non-text formats; it was not changed".into(),
            );
        }
    }
    read_clipboard_inner()
}
// Report the clipboard unchanged condition for native
fn clipboard_unchanged(text: &str, stamp: &str, current: &str, current_stamp: &str) -> bool {
    text == current && stamp == current_stamp
}
// Paste text using the clipboard-aware input path
pub fn paste(text: &str, restore: bool, expected: Option<&str>) -> Result<(), String> {
    let _operation = keyboard_operation()?;
    #[cfg(test)]
    if testing::enabled() {
        testing::record(text);
        if !restore {
            testing::set_clipboard(text);
        }
        return Ok(());
    }
    let _lock = CLIPBOARD.lock().unwrap();
    check_target(expected)?;
    let previous = if restore {
        Some(snapshot_text().map_err(|_|"Clipboard restoration requires an exclusively Unicode-text clipboard; it was not changed")?)
    } else {
        None
    };
    if restore {
        clipboard_stamp()?;
    }
    crate::desktop_input::copy(text)?;
    let inserted_stamp = if restore {
        Some(clipboard_stamp()?)
    } else {
        None
    };
    let result = check_target(expected).and_then(|_| {
        chord(&[
            if cfg!(target_os = "macos") {
                "cmd".into()
            } else {
                "ctrl".into()
            },
            "v".into(),
        ])
    });
    std::thread::sleep(Duration::from_millis(100));
    if let Some(previous) = previous {
        if let (Ok(current), Ok(stamp), Some(expected_stamp)) =
            (read_clipboard_inner(), clipboard_stamp(), inserted_stamp)
        {
            if clipboard_unchanged(text, &expected_stamp, &current, &stamp) {
                crate::desktop_input::copy(&previous)?;
            }
        }
    }
    result
}
// Capture selected text through Copy and restore the prior clipboard only if unchanged
pub fn selection(expected: Option<&str>) -> Result<String, String> {
    let _operation = keyboard_operation()?;
    #[cfg(test)]
    if testing::enabled() {
        return Ok(testing::clipboard());
    }
    let _lock = CLIPBOARD.lock().unwrap();
    check_target(expected)?;
    let previous = snapshot_text().map_err(|_| {
        "Selection capture requires an exclusively text clipboard so it can be restored"
    })?;
    clipboard_stamp()?;
    let marker = format!("sd-selection:{}", uuid::Uuid::new_v4());
    crate::desktop_input::copy(&marker)?;
    chord(&[
        if cfg!(target_os = "macos") {
            "cmd".into()
        } else {
            "ctrl".into()
        },
        "c".into(),
    ])?;
    let end = Instant::now() + Duration::from_millis(500);
    let result = loop {
        let current = read_clipboard_inner()?;
        if current != marker {
            break Ok(current);
        }
        if Instant::now() >= end {
            break Err("No selected text was copied".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let observed_stamp = clipboard_stamp()?;
    if let Ok(ref selected) = result {
        if let (Ok(current), Ok(stamp)) = (read_clipboard_inner(), clipboard_stamp()) {
            if clipboard_unchanged(selected, &observed_stamp, &current, &stamp) {
                crate::desktop_input::copy(&previous)?;
            }
        }
    } else if read_clipboard_inner().as_deref() == Ok(marker.as_str())
        && clipboard_stamp().as_deref() == Ok(observed_stamp.as_str())
    {
        crate::desktop_input::copy(&previous)?;
    }
    check_target(expected)?;
    result
}
// Explain why a requested native capability is unavailable on the current platform
pub fn capability(name: &str) -> Option<String> {
    if name == "keyboard_macro" {
        return crate::keyboard_macro::playback::unavailable_reason();
    }
    if wayland() && ["insertion", "paste", "selection"].contains(&name) {
        return Some(
            "This Wayland session cannot expose a verifiable insertion target; use Set clipboard"
                .into(),
        );
    }
    if wayland() && name == "pointer_absolute" {
        return Some("Absolute pointer movement requires an authorized screen mapping; use relative coordinates".into());
    }

    #[cfg(target_os = "macos")]
    if [
        "keyboard",
        "pointer",
        "pointer_absolute",
        "selection",
        "insertion",
        "paste",
    ]
    .contains(&name)
        && !crate::automation::title_permission(false)
    {
        return Some(
            "Allow Accessibility access for Stream Dock Manager in System Settings".into(),
        );
    }
    match name {
        "keyboard" | "pointer" if wayland() => {
            #[cfg(target_os = "linux")]
            {
                if !crate::wayland::authorized() {
                    return Some("Authorize Wayland desktop input in Settings".into());
                }
                if name == "pointer" && !crate::wayland::pointer_authorized() {
                    return Some("Authorize pointer access in Wayland Settings".into());
                }
            }
            None
        }
        "selection" if wayland() => {
            Some("Wayland does not expose the foreground insertion target".into())
        }
        "clipboard" | "paste" if cfg!(target_os = "linux") => {
            let helper = if wayland() { "wl-copy" } else { "xclip" };
            (!available(helper)).then(|| format!("Install {helper} for clipboard access"))
        }
        _ => None,
    }
}
// Report the available condition for native
fn available(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|p| p.join(program).is_file()))
}
// Read the current foreground application identity
pub fn foreground_app() -> Option<String> {
    #[cfg(target_os = "macos")]
    return objc2::rc::autoreleasepool(|_| {
        objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()?
            .bundleIdentifier()
            .map(|id| id.to_string())
    });
    #[cfg(target_os="windows")]
    return output(crate::native_process::command("powershell").args(["-NoProfile","-NonInteractive","-Command","Add-Type 'using System;using System.Runtime.InteropServices;public class W{[DllImport(\"user32.dll\")]public static extern IntPtr GetForegroundWindow();[DllImport(\"user32.dll\")]public static extern uint GetWindowThreadProcessId(IntPtr h,out uint p);}';$p=0;[W]::GetWindowThreadProcessId([W]::GetForegroundWindow(),[ref]$p)|Out-Null;(Get-Process -Id $p).Path"])).ok().map(|s|s.trim().into());
    #[cfg(target_os = "linux")]
    {
        if wayland() {
            None
        } else {
            output(Command::new("xdotool").args(["getactivewindow", "getwindowpid"]))
                .ok()
                .and_then(|s| std::fs::read_link(format!("/proc/{}/exe", s.trim())).ok())
                .map(|p| p.to_string_lossy().into())
        }
    }
}
// Collect names of currently observed processes
fn process_names() -> Result<String, String> {
    #[cfg(unix)]
    return output(Command::new("ps").args(["-A", "-o", "comm="]));
    #[cfg(windows)]
    return output(crate::native_process::command("powershell").args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "Get-Process | ForEach-Object { $_.Path; $_.ProcessName }",
    ]));
}
// Check whether the requested process is running
pub fn process_running(name: &str) -> Result<bool, String> {
    if process_names()?.lines().any(|line| {
        line.trim() == name
            || Path::new(line.trim()).file_name() == Some(std::ffi::OsStr::new(name))
    }) {
        return Ok(true);
    }
    #[cfg(target_os = "linux")]
    if Path::new(name).is_absolute() {
        return Ok(std::fs::read_dir("/proc")
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path().join("exe")).ok())
            .any(|path| path == Path::new(name)));
    }
    Ok(false)
}

// Collect the targets needed to evaluate an action condition
pub fn condition_targets(
    source: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let choices = match source {
        "process" => process_names()?
            .lines()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| (value.to_owned(), value.to_owned()))
            .collect(),
        "application" => running_application_targets(),
        _ => return Err("This condition source does not have a running-target list".into()),
    };
    Ok(choices.into_iter().take(1024).collect())
}

#[cfg(target_os = "macos")]
// Collect currently running application targets for rule selection
fn running_application_targets() -> std::collections::BTreeMap<String, String> {
    objc2::rc::autoreleasepool(|_| {
        let apps = objc2_app_kit::NSWorkspace::sharedWorkspace().runningApplications();
        (0..apps.count())
            .map(|index| apps.objectAtIndex(index))
            .filter_map(|app| {
                let id = app.bundleIdentifier()?.to_string();
                let label = app
                    .localizedName()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|| id.clone());
                Some((id, label))
            })
            .collect()
    })
}

#[cfg(not(target_os = "macos"))]
// Collect currently running application targets for rule selection
fn running_application_targets() -> std::collections::BTreeMap<String, String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let Ok(mut system) = System::new() else {
        return Default::default();
    };
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    system
        .processes()
        .values()
        .filter_map(|process| {
            let path = process.exe()?.to_str()?.to_owned();
            let label = process.name().to_string_lossy().into_owned();
            Some((path, label))
        })
        .collect()
}

// Resolve the application path used by an action condition
pub fn condition_target_path(source: &str, path: &str) -> Result<String, String> {
    let path = Path::new(path);
    if source == "application" {
        #[cfg(target_os = "macos")]
        return objc2::rc::autoreleasepool(|_| {
            use objc2_foundation::{NSBundle, NSString};
            NSBundle::bundleWithPath(&NSString::from_str(path.to_str().unwrap_or_default()))
                .and_then(|bundle| bundle.bundleIdentifier())
                .map(|id| id.to_string())
                .ok_or_else(|| "Choose a macOS application bundle (.app)".into())
        });
    } else if source != "process" {
        return Err("Unsupported target chooser".into());
    }
    if !path.is_file() {
        return Err("Choose an executable file".into());
    }
    #[cfg(target_os = "linux")]
    return path
        .canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string());
    #[cfg(target_os = "windows")]
    if source == "process" {
        return path
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| "Choose an executable file".into());
    }
    #[cfg(target_os = "macos")]
    if source == "process" {
        return path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| "Choose an executable file".into());
    }
    #[cfg(not(target_os = "linux"))]
    Ok(path.to_string_lossy().into_owned())
}
// Open link
pub fn open_link(url: &str) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("open_link");
        return Ok(());
    }
    let scheme = url
        .split_once(':')
        .map(|(s, _)| s)
        .ok_or("A URI scheme is required")?;
    if scheme.is_empty()
        || !scheme.chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_alphanumeric() || "+-.".contains(c)
            }
        })
        || ["javascript", "data", "vbscript", "file"]
            .contains(&scheme.to_ascii_lowercase().as_str())
    {
        return Err("Unsupported application URI scheme".into());
    }
    crate::actions::open(url)
}
// Reveal the requested path in the system file manager
pub fn reveal(path: &str) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("reveal");
        return Ok(());
    }
    let path = Path::new(path);
    if !path.exists() {
        return Err("File does not exist".into());
    }
    #[cfg(target_os = "macos")]
    {
        output(Command::new("open").arg("-R").arg("--").arg(path))?;
    }
    #[cfg(target_os = "windows")]
    {
        output(Command::new("explorer.exe").arg(format!("/select,{}", path.display())))?;
    }
    #[cfg(target_os = "linux")]
    {
        let uri = format!(
            "file://{}",
            path.canonicalize()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
        );
        let result = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.FileManager1",
                "--object-path",
                "/org/freedesktop/FileManager1",
                "--method",
                "org.freedesktop.FileManager1.ShowItems",
            ])
            .arg(format!(
                "['{}']",
                uri.replace('\\', "\\\\").replace('\'', "\\'")
            ))
            .arg("")
            .status()
            .map_err(|e| e.to_string())?;
        if !result.success() {
            return Err("The file manager does not support revealing files".into());
        }
    }
    Ok(())
}
// Open with
pub fn open_with(application: &str, path: &str) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("open_with");
        return Ok(());
    }
    if application.is_empty() {
        return Err("Choose an application".into());
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg("-a")
            .arg(application)
            .arg("--")
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        Command::new(application)
            .arg(path)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
// Open the requested command in a terminal
pub fn terminal(program: &str, path: &str) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record("terminal");
        return Ok(());
    }
    if !Path::new(path).is_dir() {
        return Err("Choose an existing working directory".into());
    }
    #[cfg(target_os = "macos")]
    if program.is_empty() {
        Command::new("open")
            .args(["-a", "Terminal", "--", path])
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let program = if program.is_empty() {
        if cfg!(windows) {
            "cmd.exe"
        } else {
            "x-terminal-emulator"
        }
    } else {
        program
    };
    Command::new(program)
        .current_dir(path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
#[cfg(windows)]
struct OwnedJob(windows::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl OwnedJob {
    // Initialize native state from the supplied inputs
    fn new() -> Result<Self, String> {
        use windows::Win32::{Foundation::CloseHandle, System::JobObjects::*};
        unsafe {
            let handle =
                CreateJobObjectW(None, windows::core::PCWSTR::null()).map_err(|e| e.to_string())?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Err(error) = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            ) {
                let _ = CloseHandle(handle);
                return Err(error.to_string());
            }
            Ok(Self(handle))
        }
    }
    // Assign the selected native value
    fn assign(&self, child: &std::process::Child) -> Result<(), String> {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            windows::Win32::System::JobObjects::AssignProcessToJobObject(
                self.0,
                windows::Win32::Foundation::HANDLE(child.as_raw_handle()),
            )
            .map_err(|e| e.to_string())
        }
    }
    // Report the active native state
    fn active(&self) -> bool {
        use windows::Win32::System::JobObjects::*;
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(self.0),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,
                std::mem::size_of_val(&info) as u32,
                None,
            )
            .is_err()
                || info.ActiveProcesses > 0
        }
    }
    // Terminate the Windows job and all processes it owns
    fn terminate(&self) {
        unsafe {
            let _ = windows::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}
#[cfg(windows)]
impl Drop for OwnedJob {
    // Release this native owner when it leaves scope
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
// Convert command output into the action’s result
pub fn command_result(
    program: &str,
    args: &[String],
    path: &str,
    timeout: i32,
    check: impl Fn() -> Result<(), String>,
) -> Result<Value, String> {
    if program.is_empty() {
        return Err("Choose an executable".into());
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !path.is_empty() {
        command.current_dir(path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    let job = OwnedJob::new()?;
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    if let Err(error) = job.assign(&child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let size = std::sync::Arc::new(AtomicUsize::new(0));
    // Drain child output on a worker while accounting for the shared byte limit
    fn reader(
        mut reader: impl Read + Send + 'static,
        size: std::sync::Arc<AtomicUsize>,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut result = vec![];
            let mut buffer = [0; 4096];
            while let Ok(n) = reader.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                if size.fetch_add(n, Ordering::SeqCst) + n <= 1024 * 1024 {
                    result.extend_from_slice(&buffer[..n]);
                }
            }
            let _ = tx.send(result);
        });
        rx
    }
    let stdout = reader(child.stdout.take().unwrap(), size.clone());
    let stderr = reader(child.stderr.take().unwrap(), size.clone());
    let start = Instant::now();
    let mut status = None;
    let mut out = None;
    let mut err = None;
    loop {
        let problem = check()
            .err()
            .or_else(|| {
                (start.elapsed().as_millis() >= timeout.max(1) as u128)
                    .then(|| "Command timed out".into())
            })
            .or_else(|| {
                (size.load(Ordering::SeqCst) > 1024 * 1024)
                    .then(|| "Command output exceeds 1 MiB".into())
            });
        if let Some(error) = problem {
            #[cfg(unix)]
            {
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .status();
            }
            #[cfg(windows)]
            job.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        if status.is_none() {
            status = child.try_wait().map_err(|e| e.to_string())?;
        }
        if out.is_none() {
            out = stdout.try_recv().ok();
        }
        if err.is_none() {
            err = stderr.try_recv().ok();
        }
        if let (Some(status), Some(out), Some(err)) = (&status, &out, &err) {
            return Ok(
                json!({"stdout":String::from_utf8_lossy(out),"stderr":String::from_utf8_lossy(err),"exit_status":status.code().unwrap_or(-1)}),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
// Write file
pub fn write_file(path: &str, text: &str, operation: &str, replace: bool) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    match operation {
        "create" => {
            options.create_new(true);
        }
        "replace" => {
            if !replace && Path::new(path).exists() {
                return Err("Destination exists; enable replacement".into());
            }
            options.create(true).truncate(true);
        }
        "append" => {
            options.create(true).append(true);
        }
        _ => return Err("Unsupported file operation".into()),
    };
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}
// Perform the requested file operation through the native action path
pub fn file_operation(
    operation: &str,
    source: &str,
    destination: &str,
    replace: bool,
) -> Result<(), String> {
    let source = Path::new(source);
    let destination = Path::new(destination);
    if !source.exists() {
        return Err("Source does not exist".into());
    }
    if operation == "trash" {
        #[cfg(target_os = "macos")]
        {
            output(Command::new("osascript").args(["-e","on run argv\ntell application \"Finder\" to delete POSIX file (item 1 of argv)\nend run","--"]).arg(source))?;
        }
        #[cfg(target_os = "windows")]
        {
            output(crate::native_process::command("powershell").args(["-NoProfile","-NonInteractive","-Command","Add-Type -AssemblyName Microsoft.VisualBasic;if(Test-Path -LiteralPath $env:SD_SOURCE -PathType Container){[Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($env:SD_SOURCE,'OnlyErrorDialogs','SendToRecycleBin')}else{[Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($env:SD_SOURCE,'OnlyErrorDialogs','SendToRecycleBin')}"]).env("SD_SOURCE",source))?;
        }
        #[cfg(target_os = "linux")]
        {
            output(Command::new("gio").arg("trash").arg("--").arg(source))?;
        }
        return Ok(());
    }
    if source.canonicalize().ok() == destination.canonicalize().ok() && destination.exists() {
        return Err("Source and destination are the same".into());
    }
    if destination.exists() && !replace {
        return Err("Destination exists; enable replacement".into());
    }
    match operation {
        "copy" => copy_path(source, destination),
        "move" | "rename" => {
            if source.is_dir() && destination.exists() {
                return Err("Replacing an existing directory is unsupported".into());
            }
            std::fs::rename(source, destination).map_err(|e| e.to_string())
        }
        _ => Err("Unsupported file operation".into()),
    }
}
// Copy path
fn copy_path(source: &Path, destination: &Path) -> Result<(), String> {
    if source
        .symlink_metadata()
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Copying symbolic links is unsupported".into());
    }
    if source.is_dir() {
        let absolute = source.canonicalize().map_err(|e| e.to_string())?;
        let parent = destination.parent().unwrap_or(Path::new("."));
        if parent
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&absolute)
        {
            return Err("Cannot copy a directory into itself".into());
        }
        std::fs::create_dir(destination).map_err(|e| e.to_string())?;
        for entry in std::fs::read_dir(source).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_path(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(source, destination).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// Open web
pub fn open_web(url: &str) -> Result<(), String> {
    #[cfg(test)]
    if testing::enabled() {
        testing::record(url);
        return Ok(());
    }
    crate::actions::open(url)
}

#[cfg(test)]
mod clipboard_tests {
    use super::*;
    #[test]
    // Verify that identical text from a new clipboard owner is not restored
    fn identical_text_from_a_new_clipboard_owner_is_not_restored() {
        assert!(clipboard_unchanged("snippet", "1", "snippet", "1"));
        assert!(!clipboard_unchanged("snippet", "1", "snippet", "2"));
        assert!(!clipboard_unchanged("snippet", "1", "other", "1"));
    }
}

#[cfg(test)]
mod condition_target_tests {
    use super::*;

    #[test]
    // Verify that selected executable identifies a running process
    fn selected_executable_identifies_a_running_process() {
        let exe = std::env::current_exe().unwrap();
        let target = condition_target_path("process", exe.to_str().unwrap()).unwrap();
        assert!(
            process_running(&target).unwrap(),
            "Could not match {target}"
        );
        assert!(!condition_targets("process").unwrap().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    // Verify that selected application bundle resolves to its bundle identifier
    fn selected_application_bundle_resolves_to_its_bundle_identifier() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("Example.app");
        let contents = app.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        std::fs::write(
            contents.join("Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>org.example.chooser</string></dict></plist>"#,
        )
        .unwrap();
        assert_eq!(
            condition_target_path("application", app.to_str().unwrap()).unwrap(),
            "org.example.chooser"
        );
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    #[test]
    // Verify that revocation clears owned keys and buttons before a new session
    fn revocation_clears_owned_keys_and_buttons_before_a_new_session() {
        let mut state = InputState::default();
        state.keys.insert(
            "owner".into(),
            vec![HeldKey {
                name: "ctrl".into(),
                resolved: Key::Control,
            }],
        );
        state.buttons.insert("drag".into(), "left".into());
        // Use an isolated generation: changing the process-wide value also cancels
        // runtimes owned by other tests running in parallel
        sync_input_generation(&mut state, 1);
        assert!(state.keys.is_empty());
        assert!(state.buttons.is_empty());
        assert_eq!(state.generation, 1);
    }
}

/// A single supervisor watches launched command groups without retaining an action worker
pub(super) fn spawn_supervised(
    program: String,
    args: Vec<String>,
    check: Box<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Result<(), String> {
    type Check = Box<dyn Fn() -> Result<(), String> + Send + Sync>;
    struct Request {
        program: String,
        args: Vec<String>,
        check: Check,
        reply: std::sync::mpsc::SyncSender<Result<(), String>>,
    }
    static SUPERVISOR: OnceLock<std::sync::mpsc::SyncSender<Request>> = OnceLock::new();
    let sender = SUPERVISOR.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Request>(128);
        std::thread::spawn(move || {
            struct Running {
                child: std::process::Child,
                check: Check,
                #[cfg(windows)]
                job: OwnedJob,
            }
            let mut running: Vec<Running> = vec![];
            loop {
                if let Ok(request) = receiver.recv_timeout(Duration::from_millis(20)) {
                    let result = (|| {
                        (request.check)()?;
                        if running.len() >= 128 {
                            return Err("Too many active command processes".into());
                        }
                        let mut command = Command::new(&request.program);
                        command.args(&request.args).stdin(Stdio::null());
                        #[cfg(unix)]
                        {
                            use std::os::unix::process::CommandExt;
                            command.process_group(0);
                        }
                        #[cfg(windows)]
                        let job = OwnedJob::new()?;
                        #[allow(unused_mut)]
                        let mut child = command.spawn().map_err(|e| e.to_string())?;
                        #[cfg(windows)]
                        if let Err(error) = job.assign(&child) {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(error);
                        }
                        Ok(Running {
                            child,
                            check: request.check,
                            #[cfg(windows)]
                            job,
                        })
                    })();
                    match result {
                        Ok(process) => {
                            running.push(process);
                            let _ = request.reply.send(Ok(()));
                        }
                        Err(error) => {
                            let _ = request.reply.send(Err(error));
                        }
                    }
                }
                let mut index = 0;
                while index < running.len() {
                    let cancelled = (running[index].check)().is_err();
                    let exited = running[index].child.try_wait().ok().flatten().is_some();
                    #[cfg(unix)]
                    let descendants = process_group_signal(running[index].child.id(), 0);
                    #[cfg(windows)]
                    let descendants = running[index].job.active();
                    #[cfg(not(any(unix, windows)))]
                    let descendants = false;
                    if cancelled {
                        #[cfg(unix)]
                        {
                            process_group_signal(running[index].child.id(), 9);
                        }
                        #[cfg(windows)]
                        running[index].job.terminate();
                        let _ = running[index].child.kill();
                        let _ = running[index].child.wait();
                    }
                    if cancelled || exited && !descendants {
                        running.swap_remove(index);
                    } else {
                        index += 1;
                    }
                }
            }
        });
        sender
    });
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    sender
        .try_send(Request {
            program,
            args,
            check,
            reply,
        })
        .map_err(|_| "Command supervisor queue is full")?;
    result.recv().map_err(|_| "Command supervisor stopped")?
}

#[cfg(unix)]
// Report the process group signal condition for native
fn process_group_signal(process: u32, signal: i32) -> bool {
    unsafe extern "C" {
        fn kill(process: i32, signal: i32) -> i32;
    }
    i32::try_from(process).is_ok_and(|process| unsafe { kill(-process, signal) == 0 })
}
#[cfg(test)]
mod input_tests {
    use super::*;

    // Return the key identifying native
    fn key(name: &str, resolved: Key) -> HeldKey {
        HeldKey {
            name: name.into(),
            resolved,
        }
    }

    #[test]
    // Verify that owned shortcuts release original codes after layout changes
    fn owned_shortcuts_release_original_codes_after_layout_changes() {
        let mut state = InputState::default();
        let ctrl = key("ctrl", Key::Control);
        let before = key("m", Key::Other(46));
        let after = key("m", Key::Other(20));
        let mut events = vec![];
        let mut send = |_: &mut InputState, key: &HeldKey, down: bool| {
            events.push((key.resolved, down));
            Ok(())
        };
        hold_resolved(
            &mut state,
            "discord-a",
            vec![ctrl.clone(), before.clone()],
            &mut send,
        )
        .unwrap();
        hold_resolved(
            &mut state,
            "discord-b",
            vec![ctrl.clone(), after],
            &mut send,
        )
        .unwrap();
        hold_resolved(&mut state, "discord-b", vec![before], &mut send).unwrap();
        release_resolved(&mut state, "discord-a", &mut send);
        release_resolved(&mut state, "discord-b", &mut send);
        release_resolved(&mut state, "discord-b", &mut send);
        assert_eq!(
            events,
            [
                (Key::Control, true),
                (Key::Other(46), true),
                (Key::Other(46), false),
                (Key::Control, false)
            ]
        );
        assert!(state.keys.is_empty());
        assert!(state.keyboard.is_none());
    }

    #[test]
    // Verify that failed shortcuts roll back modifiers without acquiring ownership
    fn failed_shortcuts_roll_back_modifiers_without_acquiring_ownership() {
        let mut state = InputState::default();
        let mut events = vec![];
        let error = hold_resolved(
            &mut state,
            "discord",
            vec![
                key("ctrl", Key::Control),
                key("shift", Key::Shift),
                key("m", Key::Other(46)),
            ],
            |_, key, down| {
                if down && key.name == "m" {
                    return Err("controlled failure".into());
                }
                events.push((key.resolved, down));
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error, "controlled failure");
        assert_eq!(
            events,
            [
                (Key::Control, true),
                (Key::Shift, true),
                (Key::Shift, false),
                (Key::Control, false)
            ]
        );
        assert!(state.keys.is_empty());
        assert!(state.keyboard.is_none());
    }
}
#[cfg(all(test, unix))]
mod supervisor_tests {
    use super::*;
    #[test]
    // Verify that cancellation kills descendants after the launcher exits
    fn cancellation_kills_descendants_after_the_launcher_exits() {
        let directory = tempfile::tempdir().unwrap();
        let ready = directory.path().join("ready");
        let marker = directory.path().join("marker");
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let check = cancelled.clone();
        spawn_supervised(
            "sh".into(),
            vec![
                "-c".into(),
                "(echo ready > \"$1\"; sleep 0.4; echo leaked > \"$2\") >/dev/null 2>&1 &".into(),
                "fixture".into(),
                ready.to_string_lossy().into(),
                marker.to_string_lossy().into(),
            ],
            Box::new(move || {
                if check.load(Ordering::SeqCst) {
                    Err("Cancelled".into())
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(60));
        cancelled.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(450));
        assert!(!marker.exists());
    }
}

#[cfg(test)]
mod keyboard_macro_input_tests {
    use super::*;
    use crate::keyboard_macro::KeyboardMacroEventKind::*;

    #[test]
    // Verify that recordings playback and ordinary input are mutually exclusive
    fn recordings_playback_and_ordinary_input_are_mutually_exclusive() {
        let mut admission = KeyboardAdmission::default();
        admission.begin_operation(false).unwrap();
        assert!(admission.acquire("macro", false).is_err());
        admission.operations -= 1;
        assert!(admission.acquire("macro", true).is_err());
        let token = admission.acquire("device:binding:run", false).unwrap();
        assert!(admission.begin_operation(false).is_err());
        for _ in 0..1000 {
            assert!(admission.acquire("other-device", false).is_err());
        }
        // Cleanup is a direct signal, never a queue admission request
        admission.cancel("other-device");
        assert!(!token.load(Ordering::SeqCst));
        admission.cancel("device:");
        assert!(token.load(Ordering::SeqCst));
        assert!(admission.acquire("recording", false).is_err());
        admission.finish("device:binding:run", &token);
        assert!(admission.acquire("recording", false).is_ok());
    }

    #[test]
    // Verify that system media and volume output never bypasses recording or playback admission
    fn system_media_and_volume_output_never_bypasses_recording_or_playback_admission() {
        for owner in ["recording", "device:binding:run"] {
            let mut admission = KeyboardAdmission::default();
            let token = admission.acquire(owner, false).unwrap();
            let mut emitted = false;
            assert!(admitted_keyboard_output(
                || admission.begin_operation(false),
                || {
                    emitted = true;
                    Ok(())
                }
            )
            .is_err());
            assert!(!emitted);
            admission.finish(owner, &token);
            admitted_keyboard_output(
                || admission.begin_operation(false),
                || {
                    emitted = true;
                    Ok(())
                },
            )
            .unwrap();
            assert!(emitted);
        }
    }

    #[test]
    // Verify that stale lease cannot finish a new transaction
    fn stale_lease_cannot_finish_a_new_transaction() {
        let mut admission = KeyboardAdmission::default();
        let first = admission.acquire("run", false).unwrap();
        admission.finish("run", &first);
        let second = admission.acquire("run", false).unwrap();
        admission.finish("run", &first);
        assert!(admission.begin_operation(false).is_err());
        admission.finish("run", &second);
        admission.begin_operation(false).unwrap();
    }

    #[test]
    // Verify that event ownership accumulates codes and releases only the owner
    fn event_ownership_accumulates_codes_and_releases_only_the_owner() {
        let mut state = InputState::default();
        let mut events = vec![];
        for (code, kind) in [
            (59, KeyDown),
            (0, KeyDown),
            (0, Repeat),
            (0, KeyUp),
            (59, KeyUp),
        ] {
            macro_key_event_resolved(&mut state, "run", code, kind, || {
                events.push((code, kind));
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(events.len(), 5);
        assert!(state.macro_keys.is_empty());
        macro_key_event_resolved(&mut state, "run", 59, KeyDown, || Ok(())).unwrap();
        macro_key_event_resolved(&mut state, "other", 59, KeyUp, || {
            panic!("another owner's key must not be released")
        })
        .unwrap();
        assert_eq!(state.macro_keys.len(), 1);
        assert!(
            macro_key_event_resolved(&mut state, "other", 59, KeyDown, || panic!(
                "owned key must not be pressed twice"
            ))
            .is_err()
        );
        macro_key_event_resolved(&mut state, "run", 59, KeyUp, || Ok(())).unwrap();
        macro_key_event_resolved(&mut state, "run", 59, KeyUp, || {
            panic!("cleanup must be idempotent")
        })
        .unwrap();
        assert!(state.macro_keys.is_empty());
    }

    #[test]
    // Verify that uncertain press and failed release keep ownership until successful cleanup
    fn uncertain_press_and_failed_release_keep_ownership_until_successful_cleanup() {
        let mut state = InputState::default();
        assert!(
            macro_key_event_resolved(&mut state, "run", 59, KeyDown, || Err("uncertain".into()))
                .is_err()
        );
        assert_eq!(state.macro_keys["run"], [59]);
        assert!(
            macro_key_event_resolved(
                &mut state,
                "run",
                59,
                KeyUp,
                || Err("failed release".into())
            )
            .is_err()
        );
        assert_eq!(state.macro_keys["run"], [59]);
        sync_input_generation(&mut state, 1);
        assert_eq!(state.macro_keys["run"], [59]);
        macro_key_event_resolved(&mut state, "run", 59, KeyUp, || Ok(())).unwrap();
        assert!(state.macro_keys.is_empty());
    }

    #[test]
    // Verify that failed macro cleanup blocks ordinary input until owned release succeeds
    fn failed_macro_cleanup_blocks_ordinary_input_until_owned_release_succeeds() {
        let mut state = InputState::default();
        let snapshot = AtomicBool::new(false);
        let mut admission = KeyboardAdmission::default();
        let lease = admission.acquire("run", false).unwrap();
        macro_key_event_tracked(&mut state, &snapshot, "run", 59, KeyDown, || Ok(())).unwrap();
        assert!(snapshot.load(Ordering::SeqCst));
        assert!(
            macro_key_event_tracked(&mut state, &snapshot, "run", 59, KeyUp, || Err(
                "release failed".into()
            ))
            .is_err()
        );
        admission.finish("run", &lease);
        assert!(admission
            .begin_operation(snapshot.load(Ordering::SeqCst))
            .is_err());
        assert_eq!(admission.operations, 0);
        assert!(admission
            .acquire("recording", !state.macro_keys.is_empty())
            .is_err());
        macro_key_event_tracked(&mut state, &snapshot, "run", 59, KeyUp, || Ok(())).unwrap();
        assert!(!snapshot.load(Ordering::SeqCst));
        admission
            .begin_operation(snapshot.load(Ordering::SeqCst))
            .unwrap();
        assert_eq!(admission.operations, 1);
    }

    #[test]
    // Verify that unowned repeat and release do not create phantom owners
    fn unowned_repeat_and_release_do_not_create_phantom_owners() {
        let mut state = InputState::default();
        assert!(
            macro_key_event_resolved(&mut state, "run", 59, Repeat, || panic!("unowned repeat"))
                .is_err()
        );
        macro_key_event_resolved(&mut state, "run", 59, KeyUp, || panic!("unowned release"))
            .unwrap();
        assert!(state.macro_keys.is_empty());
    }
}
