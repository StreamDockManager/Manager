use hardware_core::Input;
use serde::{Deserialize, Serialize};
use std::{
    process::Command,
    sync::{atomic::Ordering, mpsc, Weak},
};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(transparent)]
pub struct RunId(pub String);
impl std::fmt::Display for RunId {
    // Format actions for the requested formatter
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::ops::Deref for RunId {
    type Target = str;
    // Borrow the wrapped actions value
    fn deref(&self) -> &str {
        &self.0
    }
}
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPolicy {
    #[default]
    Queue,
    IgnoreBusy,
    ReplacePending,
}
impl ActivationPolicy {
    // Report whether queue
    pub fn is_queue(&self) -> bool {
        *self == Self::Queue
    }
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Completed,
    Failed,
    Cancelled,
    Ignored,
    Superseded,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NavigationTarget {
    Page,
    Profile,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NavigationDirection {
    Previous,
    Next,
}

// Resolve relative page or profile navigation against the current workspace
pub(crate) fn navigation_destination(
    profiles: &[crate::profiles::Profile],
    profile_id: &str,
    page_id: &str,
    target: NavigationTarget,
    direction: NavigationDirection,
) -> Result<(String, String), String> {
    let profile = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .ok_or("Current profile missing")?;
    let next = |index: usize, len: usize| match direction {
        NavigationDirection::Previous => (index + len - 1) % len,
        NavigationDirection::Next => (index + 1) % len,
    };
    match target {
        NavigationTarget::Page => {
            let index = profile
                .pages
                .iter()
                .position(|p| p.id == page_id)
                .ok_or("Current page missing")?;
            Ok((
                profile.id.clone(),
                profile.pages[next(index, profile.pages.len())].id.clone(),
            ))
        }
        NavigationTarget::Profile => {
            let compatible: Vec<_> = profiles
                .iter()
                .filter(|p| p.model_id == profile.model_id)
                .collect();
            let index = compatible
                .iter()
                .position(|p| p.id == profile.id)
                .ok_or("Current profile missing")?;
            let selected = compatible[next(index, compatible.len())];
            Ok((selected.id.clone(), selected.active_page.clone()))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    None,
    Builtin {
        action: Box<crate::builtins::BuiltinAction>,
    },
    Hotkey {
        keys: Vec<String>,
    },
    OpenUrl {
        url: String,
    },
    OpenPath {
        path: String,
    },
    Application {
        path: String,
    },
    Media {
        command: String,
    },
    Volume {
        delta: i32,
    },
    Mute,
    Command {
        program: String,
        args: Vec<String>,
    },
    Profile {
        profile_id: String,
    },
    Page {
        page_id: String,
    },
    Navigate {
        target: NavigationTarget,
        direction: NavigationDirection,
    },
    SwipeNavigation {
        target: String,
        wrap: bool,
        reverse: bool,
    },
    Delay {
        #[ts(type = "number")]
        milliseconds: u64,
    },
    Macro {
        actions: Vec<Action>,
    },
    /// Independent dial actions: turn left, turn right, and press
    Dial {
        actions: Vec<Action>,
        /// Keep turn-lane editing synchronized without changing independent runtime contexts
        #[serde(default)]
        #[ts(optional, as = "Option<bool>")]
        linked_turns: bool,
    },
    Toggle {
        on: Box<Action>,
        off: Box<Action>,
    },
    Brightness {
        value: u8,
    },
    Lighting {
        brightness: u8,
        colors: Vec<[u8; 3]>,
    },
    Sleep,
    Plugin {
        plugin_id: String,
        action_id: String,
        settings: serde_json::Value,
        /// Explicit Elgato multi-action state selection; plugin settings stay opaque
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        user_desired_state: Option<u8>,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        resources: std::collections::BTreeMap<String, String>,
    },
}
impl Action {
    // Validate the supplied actions value
    pub fn validate(&self, depth: usize) -> Result<(), String> {
        self.validate_with_draft(depth, false)
    }
    /// Only local editor drafts and read-only previews accept an empty Macro placeholder
    pub(crate) fn validate_draft(&self, depth: usize) -> Result<(), String> {
        self.validate_with_draft(depth, true)
    }
    // Validate with draft
    fn validate_with_draft(&self, depth: usize, draft: bool) -> Result<(), String> {
        if depth == 0 {
            crate::keyboard_macro::validation::action_event_count(self)?;
        }
        if depth > 8 {
            return Err("macro nesting exceeds 8".into());
        }
        match self {
            Self::Builtin { action } => {
                if draft {
                    crate::builtins::validate_draft(action, depth)?;
                } else {
                    crate::builtins::validate(action, depth)?;
                }
            }
            Self::SwipeNavigation { target, .. }
                if !["page", "profile"].contains(&target.as_str()) =>
            {
                return Err("Swipe navigation target must be page or profile".into());
            }
            Self::OpenUrl { url } => {
                if !url.starts_with("https://")
                    && !url.starts_with("http://")
                    && crate::parameters::reference(url).is_none()
                    && crate::macro_inputs::reference(url).is_none()
                {
                    return Err("only HTTP(S) URLs allowed".into());
                }
            }
            Self::Hotkey { keys } => {
                if keys.is_empty() || keys.len() > 12 {
                    return Err("hotkey needs 1–12 keys".into());
                }
                for k in keys {
                    parse_key(k)?;
                }
            }
            Self::Media { command } => {
                if !["play-pause", "next", "previous", "stop"].contains(&command.as_str()) {
                    return Err("invalid media command".into());
                }
            }
            Self::Volume { delta } => {
                if !(-100..=100).contains(delta) {
                    return Err("volume delta outside range".into());
                }
            }
            Self::Brightness { value } => {
                if *value > 100 {
                    return Err("brightness outside range".into());
                }
            }
            Self::Lighting { brightness, colors } => {
                if *brightness > 100 || colors.is_empty() || colors.len() > 32 {
                    return Err("invalid lighting brightness or channel count".into());
                }
            }
            Self::Delay { milliseconds } => {
                if *milliseconds > 60_000 {
                    return Err("delay exceeds 60 seconds".into());
                }
            }
            Self::Dial { actions, .. } => {
                if actions.len() != 3 {
                    return Err("dial needs exactly left, right, and press actions".into());
                }
                for action in actions {
                    action.validate_with_draft(depth + 1, draft)?;
                }
            }
            Self::Macro { actions } => {
                if actions.len() > 100 {
                    return Err("macro exceeds 100 actions".into());
                }
                for a in actions {
                    a.validate_with_draft(depth + 1, draft)?;
                }
            }
            Self::Toggle { on, off } => {
                on.validate_with_draft(depth + 1, draft)?;
                off.validate_with_draft(depth + 1, draft)?;
            }
            Self::Command { program, args } if program.is_empty() || args.len() > 128 => {
                return Err("invalid command".into());
            }
            Self::Plugin {
                plugin_id,
                action_id,
                ..
            } if plugin_id.is_empty() || action_id.is_empty() => {
                return Err("choose a plugin and its action before saving".into());
            }
            Self::Plugin {
                user_desired_state,
                resources,
                ..
            } => {
                if user_desired_state.is_some_and(|state| state > 1) {
                    return Err("Plugin desired state must be 0 or 1".into());
                }
                crate::vendor_resources::validate(resources)?;
            }
            _ => {}
        }
        Ok(())
    }
}
impl Action {
    /// Sequence-backed children only; use the visitors for complete structural traversal
    pub fn children(&self) -> &[Action] {
        match self {
            Self::Macro { actions } | Self::Dial { actions, .. } => actions,
            Self::Builtin { action } => action.children(),
            _ => &[],
        }
    }
    // Borrow child action nodes for in-place editing
    pub fn children_mut(&mut self) -> &mut [Action] {
        match self {
            Self::Macro { actions } | Self::Dial { actions, .. } => actions,
            Self::Builtin { action } => action.children_mut(),
            _ => &mut [],
        }
    }
    // Look up the selected child action node
    pub fn child(&self, index: usize) -> Option<&Action> {
        match self {
            Self::Toggle { on, off } => match index {
                0 => Some(on),
                1 => Some(off),
                _ => None,
            },
            _ => self.children().get(index),
        }
    }
    // Borrow the selected child action node mutably
    pub fn child_mut(&mut self, index: usize) -> Option<&mut Action> {
        match self {
            Self::Toggle { on, off } => match index {
                0 => Some(on),
                1 => Some(off),
                _ => None,
            },
            _ => self.children_mut().get_mut(index),
        }
    }
    /// Visit immediate structural children in stable path order (Toggle on, then off)
    /// Unlike `children`, this includes non-contiguous boxed Toggle branches
    pub fn try_for_each_child<E>(
        &self,
        mut f: impl FnMut(&Action) -> Result<(), E>,
    ) -> Result<(), E> {
        if let Self::Toggle { on, off } = self {
            f(on)?;
            f(off)?;
        } else {
            for child in self.children() {
                f(child)?;
            }
        }
        Ok(())
    }
    // Attempt for each child mut
    pub fn try_for_each_child_mut<E>(
        &mut self,
        mut f: impl FnMut(&mut Action) -> Result<(), E>,
    ) -> Result<(), E> {
        if let Self::Toggle { on, off } = self {
            f(on)?;
            f(off)?;
        } else {
            for child in self.children_mut() {
                f(child)?;
            }
        }
        Ok(())
    }
    // Select for each child
    pub fn for_each_child(&self, mut f: impl FnMut(&Action)) {
        let _: Result<(), std::convert::Infallible> = self.try_for_each_child(|child| {
            f(child);
            Ok(())
        });
    }
    // Select for each child mut
    pub fn for_each_child_mut(&mut self, mut f: impl FnMut(&mut Action)) {
        let _: Result<(), std::convert::Infallible> = self.try_for_each_child_mut(|child| {
            f(child);
            Ok(())
        });
    }
    // Visit the current actions value
    pub fn visit(&self, f: &mut impl FnMut(&Action)) {
        f(self);
        self.for_each_child(|child| child.visit(f));
    }
    // Visit mut
    pub fn visit_mut(&mut self, f: &mut impl FnMut(&mut Action)) {
        f(self);
        self.for_each_child_mut(|child| child.visit_mut(f));
    }
}
/// Rotation lanes own their direction; forward magnitude to avoid applying it twice
pub(crate) fn dial_input(input: &Input) -> Option<(usize, Input)> {
    match input {
        Input::Rotate { delta } if *delta != 0 => Some((
            usize::from(*delta > 0),
            Input::Rotate {
                delta: delta.saturating_abs(),
            },
        )),
        Input::Pulse | Input::Button { pressed: true } => Some((2, Input::Pulse)),
        _ => None,
    }
}
#[derive(Clone)]
pub struct Request {
    pub manual: bool,
    pub activation_policy: ActivationPolicy,
    pub icon_signature: Option<u64>,
    pub pressed: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub action: Action,
    pub queued_at: std::time::Instant,
    pub device_id: String,
    pub control_id: String,
    pub binding_scope: String,
    /// Pins a device-targeted run to its connection. Desktop-only runs have no epoch
    pub connection_epoch: Option<u64>,
    pub input: Input,
    pub cancellation_generation: u64,
}
// Start the actions operation
pub fn start(runtime: Weak<crate::Runtime>, receiver: mpsc::Receiver<Request>) {
    crate::builtins::start(runtime, receiver);
}
// Resolve the execution scope belonging to this action node
pub fn node_scope(scope: &str, path: &[usize]) -> String {
    if path.is_empty() {
        scope.into()
    } else {
        format!(
            "{scope}@{}",
            path.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )
    }
}
/// Final effect boundary. Structural actions and supervised native operations belong to Run
#[allow(clippy::too_many_arguments)] // Preserve physical identity and cancellation at the effect boundary
                                     // Execute leaf
pub(crate) fn execute_leaf(
    runtime: &crate::Runtime,
    action: &Action,
    device: &str,
    control: &str,
    scope: &str,
    connection_epoch: Option<u64>,
    input: &Input,
    generation: u64,
) -> Result<(), String> {
    if generation != runtime.cancellation.load(Ordering::SeqCst) {
        return Err("macro cancelled".into());
    }
    if connection_epoch
        .is_some_and(|epoch| runtime.connection_epochs.lock().unwrap().get(device) != Some(&epoch))
    {
        return Err("device connection changed; pending action discarded".into());
    }
    action.validate(0)?;
    match action {
        Action::None => Ok(()),
        Action::Builtin { .. }
        | Action::Command { .. }
        | Action::Hotkey { .. }
        | Action::Delay { .. }
        | Action::Macro { .. }
        | Action::Dial { .. }
        | Action::Toggle { .. } => Err("Action requires a run context".into()),
        Action::OpenUrl { url } => open(url),
        Action::OpenPath { path } | Action::Application { path } => open(path),
        Action::Media { command } => media(command),
        Action::Volume { delta } => volume(match input {
            Input::Rotate { delta: ticks } => delta.saturating_mul(*ticks).clamp(-100, 100),
            _ => *delta,
        })
        .map(|()| crate::feedback::request_refresh(runtime)),
        Action::Mute => {
            let result = mute();
            crate::feedback::request_refresh(runtime);
            result
        }
        Action::Profile { profile_id } => runtime
            .select_device_profile(device, profile_id, None, true)
            .map(|_| ()),
        Action::Page { page_id } => {
            let state = runtime.snapshot();
            let session = state.session(device);
            runtime
                .select_device_profile(device, &session.profile_id, Some(page_id), false)
                .map(|_| ())
        }
        Action::Navigate { target, direction } => {
            let state = runtime.snapshot();
            let session = state.session(device);
            let (profile, page) = navigation_destination(
                &state.profiles,
                &session.profile_id,
                &session.page_id,
                *target,
                *direction,
            )?;
            runtime
                .select_device_profile(
                    device,
                    &profile,
                    Some(&page),
                    *target == NavigationTarget::Profile,
                )
                .map(|_| ())
        }
        Action::SwipeNavigation {
            target,
            wrap,
            reverse,
        } => {
            let Input::Swipe { direction } = input else {
                return Ok(());
            };
            let state = runtime.snapshot();
            let session = state.session(device);
            let profile = state
                .profiles
                .iter()
                .find(|p| p.id == session.profile_id)
                .ok_or("Current profile missing")?;
            if target == "page" {
                let current = profile
                    .pages
                    .iter()
                    .position(|p| p.id == session.page_id)
                    .ok_or("Current page missing")?;
                if let Some(index) =
                    swipe_index(current, profile.pages.len(), direction, *wrap, *reverse)
                {
                    runtime.select_device_profile(
                        device,
                        &profile.id,
                        Some(&profile.pages[index].id),
                        false,
                    )?;
                }
            } else {
                let profiles: Vec<_> = state
                    .profiles
                    .iter()
                    .filter(|p| p.model_id == profile.model_id)
                    .collect();
                let current = profiles
                    .iter()
                    .position(|p| p.id == profile.id)
                    .unwrap_or(0);
                if let Some(index) =
                    swipe_index(current, profiles.len(), direction, *wrap, *reverse)
                {
                    runtime.select_device_profile(device, &profiles[index].id, None, true)?;
                }
            }
            Ok(())
        }
        Action::Brightness { value } => runtime.brightness(device, *value),
        Action::Lighting { brightness, colors } => {
            runtime.lighting(device, *brightness, colors.clone())
        }
        Action::Sleep => runtime.set_device_sleep(device, true).map(|_| ()),
        Action::Plugin {
            plugin_id,
            action_id,
            settings,
            user_desired_state,
            ..
        } => runtime.plugins.lock().unwrap().dispatch_with_state(
            plugin_id,
            action_id,
            device,
            control,
            scope,
            input,
            settings,
            *user_desired_state,
        ),
    }
}
// Return the current actions status
fn status(command: &mut Command) -> Result<(), String> {
    let output = command.output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
/// Open a path or URL with the system's default handler
pub fn open(value: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg("--").arg(value);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler").arg(value);
        c
    };
    #[cfg(target_os = "linux")]
    let mut cmd = {
        let mut c = Command::new(if value.ends_with(".desktop") {
            "gtk-launch"
        } else {
            "xdg-open"
        });
        c.arg(value);
        c
    };
    status(&mut cmd)
}
// Parse key
pub(crate) fn parse_key(value: &str) -> Result<enigo::Key, String> {
    use enigo::Key;
    Ok(match value.to_lowercase().as_str() {
        "ctrl" | "control" => Key::Control,
        "shift" => Key::Shift,
        "alt" => Key::Alt,
        "meta" | "super" | "cmd" => Key::Meta,
        "enter" => Key::Return,
        "space" => Key::Space,
        "tab" => Key::Tab,
        "escape" => Key::Escape,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        #[cfg(target_os = "macos")]
        "insert" => Key::Help,
        #[cfg(not(target_os = "macos"))]
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "plus" => Key::Unicode('+'),
        "up" => Key::UpArrow,
        "down" => Key::DownArrow,
        "left" => Key::LeftArrow,
        "right" => Key::RightArrow,
        #[cfg(not(target_os = "macos"))]
        "f21" => Key::F21,
        #[cfg(not(target_os = "macos"))]
        "f22" => Key::F22,
        #[cfg(not(target_os = "macos"))]
        "f23" => Key::F23,
        #[cfg(not(target_os = "macos"))]
        "f24" => Key::F24,
        name if name.starts_with('f')
            && name[1..]
                .parse::<usize>()
                .is_ok_and(|n| (1..=20).contains(&n)) =>
        {
            [
                Key::F1,
                Key::F2,
                Key::F3,
                Key::F4,
                Key::F5,
                Key::F6,
                Key::F7,
                Key::F8,
                Key::F9,
                Key::F10,
                Key::F11,
                Key::F12,
                Key::F13,
                Key::F14,
                Key::F15,
                Key::F16,
                Key::F17,
                Key::F18,
                Key::F19,
                Key::F20,
            ][name[1..].parse::<usize>().unwrap() - 1]
        }
        _ => {
            let mut chars = value.chars();
            let c = chars.next().ok_or("empty key")?;
            if chars.next().is_some() {
                return Err(format!("unsupported key {value}"));
            }
            Key::Unicode(c)
        }
    })
}
// Send a media-player command through the platform’s supported control path
fn media(command: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return status(Command::new("playerctl").arg(command));
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let key = match command {
            "play-pause" => enigo::Key::MediaPlayPause,
            "next" => enigo::Key::MediaNextTrack,
            "previous" => enigo::Key::MediaPrevTrack,
            "stop" => {
                #[cfg(target_os = "windows")]
                {
                    enigo::Key::MediaStop
                }
                #[cfg(target_os = "macos")]
                {
                    return Err("System media stop is unavailable on macOS; play/pause and track controls are supported.".into());
                }
            }
            _ => return Err("invalid media control".into()),
        };
        crate::builtins::native::tap_system_key(key, 1)
    }
}
// Adjust the system output volume by the requested percentage
fn volume(delta: i32) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return status(Command::new("pactl").args([
        "set-sink-volume",
        "@DEFAULT_SINK@",
        &format!("{delta:+}%"),
    ]));
    #[cfg(target_os="macos")]
    return status(Command::new("osascript").args(["-e",&format!("set newVolume to (output volume of (get volume settings)) + ({delta})\nif newVolume > 100 then set newVolume to 100\nif newVolume < 0 then set newVolume to 0\nset volume output volume newVolume")]));
    #[cfg(target_os = "windows")]
    {
        crate::builtins::native::tap_system_key(
            if delta > 0 {
                enigo::Key::VolumeUp
            } else {
                enigo::Key::VolumeDown
            },
            delta.unsigned_abs().div_ceil(2),
        )
    }
}
// Toggle mute on the platform’s default audio output
fn mute() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return status(Command::new("pactl").args(["set-sink-mute", "@DEFAULT_SINK@", "toggle"]));
    #[cfg(target_os = "macos")]
    return status(Command::new("osascript").args([
        "-e",
        "set volume output muted (not (output muted of (get volume settings)))",
    ]));
    #[cfg(target_os = "windows")]
    {
        crate::builtins::native::tap_system_key(enigo::Key::VolumeMute, 1)
    }
}
// Compute the next page index with optional wrapping and reversed swipe direction
fn swipe_index(
    current: usize,
    count: usize,
    direction: &str,
    wrap: bool,
    reverse: bool,
) -> Option<usize> {
    let forward = match direction {
        "left" => true,
        "right" => false,
        _ => return None,
    } ^ reverse;
    if count < 2 || current >= count {
        return None;
    }
    if forward {
        (current + 1 < count)
            .then_some(current + 1)
            .or_else(|| wrap.then_some(0))
    } else {
        current.checked_sub(1).or_else(|| wrap.then_some(count - 1))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    // Verify that structural visitors include toggle in order and stop on first error
    fn structural_visitors_include_toggle_in_order_and_stop_on_first_error() {
        let mut action = super::Action::Toggle {
            on: Box::new(super::Action::Macro {
                actions: vec![super::Action::None, super::Action::None],
            }),
            off: Box::new(super::Action::None),
        };
        let mut order = Vec::new();
        action.visit(&mut |a| {
            order.push(match a {
                super::Action::Toggle { .. } => "toggle",
                super::Action::Macro { .. } => "macro",
                _ => "leaf",
            })
        });
        assert_eq!(order, ["toggle", "macro", "leaf", "leaf", "leaf"]);
        let mut count = 0;
        action.visit_mut(&mut |a| {
            if matches!(a, super::Action::None) {
                count += 1;
                *a = super::Action::Delay {
                    milliseconds: count,
                };
            }
        });
        assert_eq!(count, 3);
        assert!(matches!(
            action.child(1),
            Some(super::Action::Delay { milliseconds: 3 })
        ));
        let mut visited = 0;
        let result = action.try_for_each_child_mut(|_| {
            visited += 1;
            Err("stop")
        });
        assert_eq!(result, Err("stop"));
        assert_eq!(visited, 1);
    }

    #[test]
    // Verify that structural traversal visits dial lanes and builtin children
    fn structural_traversal_visits_dial_lanes_and_builtin_children() {
        use crate::builtins::BuiltinAction;
        let mut action = super::Action::Toggle {
            on: Box::new(super::Action::Builtin {
                action: Box::new(BuiltinAction::CycleActions {
                    children: vec![super::Action::Macro {
                        actions: vec![
                            super::Action::Dial {
                                linked_turns: false,
                                actions: vec![super::Action::None, super::Action::None],
                            },
                            super::Action::Macro {
                                actions: vec![super::Action::None],
                            },
                        ],
                    }],
                }),
            }),
            off: Box::new(super::Action::None),
        };
        let mut leaves = 0;
        action.visit_mut(&mut |node| {
            if matches!(node, super::Action::None) {
                leaves += 1;
                *node = super::Action::Delay {
                    milliseconds: leaves,
                };
            }
        });
        assert_eq!(leaves, 4);
        let mut order = Vec::new();
        action.visit(&mut |node| {
            if let super::Action::Delay { milliseconds } = node {
                order.push(*milliseconds);
            }
        });
        assert_eq!(order, [1, 2, 3, 4]);
        assert!(matches!(
            action
                .child_mut(0)
                .unwrap()
                .child_mut(0)
                .unwrap()
                .child_mut(0)
                .unwrap()
                .child_mut(1),
            Some(super::Action::Delay { milliseconds: 2 })
        ));
    }

    use super::*;
    #[test]
    // Verify that swipe direction bounds and reversal
    fn swipe_direction_bounds_and_reversal() {
        assert_eq!(swipe_index(0, 3, "left", false, false), Some(1));
        assert_eq!(swipe_index(0, 3, "right", false, false), None);
        assert_eq!(swipe_index(0, 3, "right", true, false), Some(2));
        assert_eq!(swipe_index(2, 3, "left", true, false), Some(0));
        assert_eq!(swipe_index(0, 3, "right", false, true), Some(1));
        assert_eq!(swipe_index(0, 1, "left", true, false), None);
        assert_eq!(swipe_index(0, 0, "left", true, false), None);
        assert_eq!(swipe_index(0, 3, "up", true, false), None);
        assert!(Action::SwipeNavigation {
            target: "unknown".into(),
            wrap: true,
            reverse: false
        }
        .validate(0)
        .is_err());
    }
    #[test]
    // Verify that macros and URLs are validated
    fn macros_and_urls_are_validated() {
        assert!(Action::OpenUrl {
            url: "javascript:alert(1)".into()
        }
        .validate(0)
        .is_err());
        assert!(Action::Delay {
            milliseconds: 60_001
        }
        .validate(0)
        .is_err());
        assert!(Action::Hotkey {
            keys: vec!["ctrl".into(), "a".into()]
        }
        .validate(0)
        .is_ok());
    }
}

#[cfg(test)]
mod dial_tests {
    use super::*;
    #[test]
    // Verify that dial routing normalizes magnitude and ignores release and zero
    fn dial_routing_normalizes_magnitude_and_ignores_release_and_zero() {
        assert_eq!(
            dial_input(&Input::Rotate { delta: -3 }),
            Some((0, Input::Rotate { delta: 3 }))
        );
        assert_eq!(
            dial_input(&Input::Rotate { delta: 4 }),
            Some((1, Input::Rotate { delta: 4 }))
        );
        assert_eq!(dial_input(&Input::Pulse), Some((2, Input::Pulse)));
        assert_eq!(
            dial_input(&Input::Button { pressed: true }),
            Some((2, Input::Pulse))
        );
        assert_eq!(dial_input(&Input::Button { pressed: false }), None);
        assert_eq!(dial_input(&Input::Rotate { delta: 0 }), None);
        assert_eq!(
            dial_input(&Input::Rotate { delta: i32::MIN }),
            Some((0, Input::Rotate { delta: i32::MAX }))
        );
    }
    #[test]
    // Verify that dial roundtrip validates every branch and traverses plugins
    fn dial_roundtrip_validates_every_branch_and_traverses_plugins() {
        let value = serde_json::json!({"kind":"dial","linked_turns":false,"actions":[
            {"kind":"volume","delta":-5},
            {"kind":"macro","actions":[{"kind":"volume","delta":5},{"kind":"delay","milliseconds":5}]},
            {"kind":"plugin","plugin_id":"p","action_id":"a","settings":{"x":1}}
        ]});
        let mut action: Action = serde_json::from_value(value.clone()).unwrap();
        action.validate(0).unwrap();
        assert_eq!(serde_json::to_value(&action).unwrap(), value);
        let mut count = 0;
        action.visit(&mut |_| count += 1);
        assert_eq!(count, 6);
        assert!(matches!(action.child(2), Some(Action::Plugin { .. })));
        *action.child_mut(0).unwrap() = Action::Volume { delta: 101 };
        assert!(action.validate(0).is_err());
        assert!(Action::Dial {
            linked_turns: false,
            actions: vec![Action::None; 2]
        }
        .validate(0)
        .is_err());
        let mut nested = Action::None;
        for _ in 0..10 {
            nested = Action::Dial {
                linked_turns: false,
                actions: vec![nested, Action::None, Action::None],
            };
        }
        assert!(nested.validate(0).is_err());
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    #[test]
    // Verify that navigate cycles pages and only compatible profiles
    fn navigate_cycles_pages_and_only_compatible_profiles() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::profiles::ProfileStore::open(root.path().join("profiles")).unwrap();
        let mut first = store.load().unwrap().profiles.remove(0);
        first.id = "first".into();
        first.pages.truncate(1);
        first.pages[0].id = "one".into();
        let mut second_page = first.pages[0].clone();
        second_page.id = "two".into();
        first.pages.push(second_page);
        first.active_page = "one".into();
        let mut second = first.clone();
        second.id = "second".into();
        second.active_page = "two".into();
        let mut incompatible = first.clone();
        incompatible.id = "other".into();
        incompatible.model_id = "incompatible-model".into();
        let profiles = vec![first, incompatible, second];
        use NavigationDirection::{Next, Previous};
        use NavigationTarget::{Page, Profile};
        for direction in [Next, Previous] {
            assert_eq!(
                navigation_destination(&profiles, "first", "one", Page, direction).unwrap(),
                ("first".into(), "two".into())
            );
            assert_eq!(
                navigation_destination(&profiles, "first", "one", Profile, direction).unwrap(),
                ("second".into(), "two".into())
            );
            assert_eq!(
                navigation_destination(&profiles, "second", "two", Profile, direction).unwrap(),
                ("first".into(), "one".into())
            );
        }
        assert_eq!(
            navigation_destination(&profiles[..1], "first", "one", Profile, Next).unwrap(),
            ("first".into(), "one".into())
        );
        assert!(navigation_destination(&profiles, "missing", "one", Page, Next).is_err());
        assert!(navigation_destination(&profiles, "first", "missing", Page, Next).is_err());
        let mut profiles = profiles;
        let mut third = profiles[2].clone();
        third.id = "third".into();
        profiles.push(third);
        let mut third_page = profiles[0].pages[0].clone();
        third_page.id = "three".into();
        profiles[0].pages.push(third_page);
        assert_eq!(
            navigation_destination(&profiles, "first", "one", Page, Next)
                .unwrap()
                .1,
            "two"
        );
        assert_eq!(
            navigation_destination(&profiles, "first", "one", Page, Previous)
                .unwrap()
                .1,
            "three"
        );
        assert_eq!(
            navigation_destination(&profiles, "first", "one", Profile, Next)
                .unwrap()
                .0,
            "second"
        );
        assert_eq!(
            navigation_destination(&profiles, "first", "one", Profile, Previous)
                .unwrap()
                .0,
            "third"
        );
    }

    #[test]
    // Verify that navigate wire shape is typed and roundtrips
    fn navigate_wire_shape_is_typed_and_roundtrips() {
        for target in ["page", "profile"] {
            for direction in ["previous", "next"] {
                let value = serde_json::json!({"kind":"navigate", "target": target, "direction": direction});
                let action: Action = serde_json::from_value(value.clone()).unwrap();
                action.validate(0).unwrap();
                assert_eq!(serde_json::to_value(action).unwrap(), value);
            }
        }
        for value in [
            serde_json::json!({"kind":"navigate", "target":"device", "direction":"next"}),
            serde_json::json!({"kind":"navigate", "target":"page", "direction":"left"}),
        ] {
            assert!(serde_json::from_value::<Action>(value).is_err());
        }
    }
}
