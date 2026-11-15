//! Versioned, portable physical-key recording contract. No native keycodes are persisted
use serde::{Deserialize, Serialize};

pub const RECORDING_VERSION: u32 = 1;
pub const MAX_EVENTS: usize = 10_000;
pub const MAX_DURATION_MS: u64 = 300_000;
pub const MAX_SERIALIZED_BYTES: usize = 1024 * 1024;
pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;

/// Keep schema, generated UI inventory and wire decoding on one physical-code list
macro_rules! physical_keys {
    ($($key:ident),+ $(,)?) => {
        /// DOM physical codes. Consumer, vendor, Fn and composition keys are absent
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ts_rs::TS, schemars::JsonSchema)]
        pub enum PhysicalKey { $($key),+ }
        impl PhysicalKey {
            pub const ALL: &'static [Self] = &[$(Self::$key),+];
            // Construct from code
            pub fn from_code(code: &str) -> Option<Self> {
                match code { $(stringify!($key) => Some(Self::$key),)+ _ => None }
            }
            // Report whether modifier
            pub fn is_modifier(self) -> bool {
                matches!(self, Self::ShiftLeft | Self::ShiftRight | Self::ControlLeft | Self::ControlRight | Self::AltLeft | Self::AltRight | Self::MetaLeft | Self::MetaRight)
            }
        }
    };
}
physical_keys! {
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Backquote,
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    Comma,
    Period,
    Slash,
    IntlBackslash,
    IntlRo,
    IntlYen,
    Backspace,
    Tab,
    Enter,
    Space,
    Escape,
    CapsLock,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    MetaLeft,
    MetaRight,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    PrintScreen,
    ScrollLock,
    Pause,
    NumLock,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadDecimal,
    NumpadEnter,
    NumpadEqual,
    NumpadComma,
}
impl<'de> Deserialize<'de> for PhysicalKey {
    // Decode types from the incoming serialized value
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;
        impl serde::de::Visitor<'_> for KeyVisitor {
            type Value = PhysicalKey;
            // Describe the physical-key representation accepted by deserialization
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a supported physical keyboard code")
            }
            // Visit str
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                PhysicalKey::from_code(value)
                    .ok_or_else(|| E::custom("unsupported physical keyboard code"))
            }
        }
        deserializer.deserialize_str(KeyVisitor)
    }
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardMacroEventKind {
    KeyDown,
    KeyUp,
    Repeat,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroEvent {
    pub kind: KeyboardMacroEventKind,
    pub key: PhysicalKey,
    pub delay_ms: u32,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardMacroPlatform {
    Macos,
    Windows,
    Linux,
    Unknown,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyboardMacroKeyMode {
    Physical,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroRecording {
    pub version: u32,
    pub platform: KeyboardMacroPlatform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub layout: Option<String>,
    pub key_mode: KeyboardMacroKeyMode,
    pub events: Vec<KeyboardMacroEvent>,
}
impl Default for KeyboardMacroRecording {
    // Construct the default types value
    fn default() -> Self {
        Self {
            version: RECORDING_VERSION,
            platform: KeyboardMacroPlatform::Unknown,
            layout: None,
            key_mode: KeyboardMacroKeyMode::Physical,
            events: vec![],
        }
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
pub enum KeyboardMacroFocusPolicy {
    #[default]
    StayOnTarget,
    AllowFocusChanges,
}
/// Repeat behavior is explicit saved action intent, never a platform fallback
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
pub enum KeyboardMacroRepeatMode {
    #[default]
    Exact,
    /// Linux X11 supplies repeats from the user's existing keyboard controls
    /// Recorded Repeat events retain their timing but are not injected
    System,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroPlayback {
    pub speed: f64,
    pub focus_policy: KeyboardMacroFocusPolicy,
    #[serde(default)]
    pub repeat_mode: KeyboardMacroRepeatMode,
}
impl Default for KeyboardMacroPlayback {
    // Construct the default types value
    fn default() -> Self {
        Self {
            speed: 1.0,
            focus_policy: KeyboardMacroFocusPolicy::StayOnTarget,
            repeat_mode: KeyboardMacroRepeatMode::Exact,
        }
    }
}
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct KeyboardMacroSummary {
    pub event_count: usize,
    #[ts(type = "number")]
    pub duration_ms: u64,
    #[ts(type = "number")]
    pub playback_duration_ms: u64,
}
