use crate::actions::Action;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub source: String,
    pub subject: String,
    pub operator: String,
    pub value: serde_json::Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuiltinAction {
    TypeText {
        text: String,
        template: bool,
    },
    PasteText {
        text: String,
        template: bool,
        restore: bool,
    },
    SetClipboard {
        text: String,
        template: bool,
    },
    ClearClipboard,
    KeyboardMacro {
        recording: crate::keyboard_macro::KeyboardMacroRecording,
        playback: crate::keyboard_macro::KeyboardMacroPlayback,
    },
    HoldKeys {
        keys: Vec<String>,
    },
    LatchKeys {
        keys: Vec<String>,
    },
    MouseClick {
        button: String,
        clicks: i32,
    },
    MouseMove {
        x: i32,
        y: i32,
        relative: bool,
    },
    MouseScroll {
        x: i32,
        y: i32,
    },
    MouseDrag {
        button: String,
        x: i32,
        y: i32,
        relative: bool,
        milliseconds: i32,
    },
    OpenLink {
        url: String,
        template: bool,
    },
    RevealFile {
        path: String,
        template: bool,
    },
    PageStep {
        direction: String,
        wrap: bool,
    },
    PageBack,
    MomentaryPage {
        page_id: String,
    },
    CycleActions {
        children: Vec<Action>,
    },
    RepeatActions {
        count: i32,
        interval_ms: i32,
        children: Vec<Action>,
    },
    RepeatHeld {
        interval_ms: i32,
        children: Vec<Action>,
    },
    Conditional {
        condition: Condition,
        children: Vec<Action>,
    },
    WaitCondition {
        condition: Condition,
        timeout_ms: i32,
        poll_ms: i32,
    },
    CancelMacros,
    BrightnessStep {
        delta: i32,
    },
    AutomationControl {
        operation: String,
    },
    Notification {
        title: String,
        text: String,
        template: bool,
    },
    DateTime {
        format: String,
        utc: bool,
    },
    TransformText {
        operation: String,
    },
    SearchSelection {
        url: String,
        source: String,
    },
    ClipboardSlot {
        name: String,
        operation: String,
        persist: bool,
    },
    InputPrompt {
        title: String,
        text: String,
        output: String,
    },
    ActionMenu {
        title: String,
        labels: Vec<String>,
        children: Vec<Action>,
    },
    Gesture {
        double_ms: i32,
        hold_ms: i32,
        children: Vec<Action>,
    },
    PressRelease {
        children: Vec<Action>,
    },
    WaitRelease {
        timeout_ms: i32,
    },
    Parallel {
        wait: bool,
        children: Vec<Action>,
    },
    FailurePolicy {
        operation: String,
        retries: i32,
        delay_ms: i32,
        children: Vec<Action>,
    },
    SavedMacro {
        macro_id: String,
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        #[ts(
            optional,
            as = "Option<std::collections::BTreeMap<String, crate::macro_inputs::MacroArgument>>"
        )]
        arguments: crate::macro_inputs::Arguments,
    },
    RandomAction {
        children: Vec<Action>,
    },
    ResetState {
        scope: String,
    },
    Cooldown {
        milliseconds: i32,
        children: Vec<Action>,
    },
    BindingLayer {
        page_id: String,
        controls: Vec<String>,
        toggle: bool,
    },
    TriggerBinding {
        device_id: String,
        profile_id: String,
        page_id: String,
        control_id: String,
    },
    ShowManager,
    EditBinding {
        device_id: String,
        profile_id: String,
        page_id: String,
        control_id: String,
    },
    PauseBindings {
        operation: String,
    },
    OpenWith {
        application: String,
        path: String,
        template: bool,
    },
    OpenTerminal {
        program: String,
        path: String,
        template: bool,
    },
    CommandResult {
        program: String,
        args: Vec<String>,
        path: String,
        timeout_ms: i32,
        output: String,
        template: bool,
    },
    FileOperation {
        operation: String,
        source: String,
        destination: String,
        replace: bool,
        template: bool,
    },
    WriteFile {
        path: String,
        text: String,
        operation: String,
        replace: bool,
        template: bool,
    },
    ChoosePath {
        directory: bool,
        output: String,
    },
}

impl BuiltinAction {
    // Return the nested child action nodes
    pub fn children(&self) -> &[Action] {
        match self {
            Self::CycleActions { children, .. } => children,
            Self::RepeatActions { children, .. } => children,
            Self::RepeatHeld { children, .. } => children,
            Self::Conditional { children, .. } => children,
            Self::ActionMenu { children, .. } => children,
            Self::Gesture { children, .. } => children,
            Self::PressRelease { children, .. } => children,
            Self::Parallel { children, .. } => children,
            Self::FailurePolicy { children, .. } => children,
            Self::RandomAction { children, .. } => children,
            Self::Cooldown { children, .. } => children,
            _ => &[],
        }
    }
    // Borrow child action nodes for in-place editing
    pub fn children_mut(&mut self) -> &mut [Action] {
        match self {
            Self::CycleActions { children, .. } => children,
            Self::RepeatActions { children, .. } => children,
            Self::RepeatHeld { children, .. } => children,
            Self::Conditional { children, .. } => children,
            Self::ActionMenu { children, .. } => children,
            Self::Gesture { children, .. } => children,
            Self::PressRelease { children, .. } => children,
            Self::Parallel { children, .. } => children,
            Self::FailurePolicy { children, .. } => children,
            Self::RandomAction { children, .. } => children,
            Self::Cooldown { children, .. } => children,
            _ => &mut [],
        }
    }
}
