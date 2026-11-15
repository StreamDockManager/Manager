//! Typed portable action fields shared by parameter resolution and archive operations
//!
//! Plugin settings remain opaque. Their visitor must use the plugin's declared
//! field contract; this traversal never interprets arbitrary plugin JSON
use crate::{actions::Action, builtins::BuiltinAction};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldRole {
    Url,
    Path,
    Program,
    Argument,
    Text,
    Title,
    Application,
    SourcePath,
    DestinationPath,
    Format,
    Profile,
    Page,
    Device,
    Control,
    Macro,
}

impl FieldRole {
    // Report support for parameter
    pub(crate) fn supports_parameter(self) -> bool {
        matches!(
            self,
            Self::Url | Self::Path | Self::Program | Self::Argument | Self::Text
        )
    }

    // Report whether local path
    pub(crate) fn is_local_path(self) -> bool {
        matches!(
            self,
            Self::Path
                | Self::Program
                | Self::Application
                | Self::SourcePath
                | Self::DestinationPath
        )
    }
}

// Generate immutable and mutable access from the same exhaustive typed match
// New action variants must explicitly choose their portable fields here
macro_rules! field_traversal {
    ($name:ident, $borrow:ident, $children:ident, $($mutable:tt)?) => {
        pub(crate) fn $name<E>(
            &$($mutable)? self,
            text: &mut impl FnMut(FieldRole, &$($mutable)? String) -> Result<(), E>,
            plugin: &mut impl FnMut(&str, &str, &$($mutable)? Value) -> Result<(), E>,
        ) -> Result<(), E> {
            use FieldRole as F;
            match self {
                Action::OpenUrl { url } => text(F::Url, url)?,
                Action::OpenPath { path } | Action::Application { path } => text(F::Path, path)?,
                Action::Command { program, args } => {
                    text(F::Program, program)?;
                    for argument in args { text(F::Argument, argument)?; }
                }
                Action::Profile { profile_id } => text(F::Profile, profile_id)?,
                Action::Page { page_id } => text(F::Page, page_id)?,
                Action::Plugin { plugin_id, action_id, settings , ..} => plugin(plugin_id, action_id, settings)?,
                Action::Builtin { action } => match action.$borrow() {
                    BuiltinAction::TypeText { text: value, .. }
                    | BuiltinAction::PasteText { text: value, .. }
                    | BuiltinAction::SetClipboard { text: value, .. } => text(F::Text, value)?,
                    BuiltinAction::OpenLink { url, .. }
                    | BuiltinAction::SearchSelection { url, .. } => text(F::Url, url)?,
                    BuiltinAction::RevealFile { path, .. } => text(F::Path, path)?,
                    BuiltinAction::MomentaryPage { page_id } => text(F::Page, page_id)?,
                    BuiltinAction::BindingLayer { page_id, controls, .. } => {
                        text(F::Page, page_id)?;
                        for control in controls { text(F::Control, control)?; }
                    }
                    BuiltinAction::Notification { title, text: value, .. }
                    | BuiltinAction::InputPrompt { title, text: value, .. } => {
                        text(F::Title, title)?;
                        text(F::Text, value)?;
                    }
                    BuiltinAction::DateTime { format, .. } => text(F::Format, format)?,
                    BuiltinAction::ActionMenu { title, labels, .. } => {
                        text(F::Title, title)?;
                        for label in labels { text(F::Title, label)?; }
                    }
                    BuiltinAction::SavedMacro { macro_id, .. } => text(F::Macro, macro_id)?,
                    BuiltinAction::TriggerBinding { device_id, profile_id, page_id, control_id }
                    | BuiltinAction::EditBinding { device_id, profile_id, page_id, control_id } => {
                        text(F::Device, device_id)?;
                        text(F::Profile, profile_id)?;
                        text(F::Page, page_id)?;
                        text(F::Control, control_id)?;
                    }
                    BuiltinAction::OpenWith { application, path, .. } => {
                        text(F::Application, application)?;
                        text(F::Path, path)?;
                    }
                    BuiltinAction::OpenTerminal { program, path, .. } => {
                        text(F::Program, program)?;
                        text(F::Path, path)?;
                    }
                    BuiltinAction::CommandResult { program, args, path, .. } => {
                        text(F::Program, program)?;
                        for argument in args { text(F::Argument, argument)?; }
                        text(F::Path, path)?;
                    }
                    BuiltinAction::FileOperation { source, destination, .. } => {
                        text(F::SourcePath, source)?;
                        text(F::DestinationPath, destination)?;
                    }
                    BuiltinAction::WriteFile { path, text: value, .. } => {
                        text(F::Path, path)?;
                        text(F::Text, value)?;
                    }
                    BuiltinAction::KeyboardMacro { .. } | BuiltinAction::ClearClipboard | BuiltinAction::HoldKeys { .. }
                    | BuiltinAction::LatchKeys { .. } | BuiltinAction::MouseClick { .. }
                    | BuiltinAction::MouseMove { .. } | BuiltinAction::MouseScroll { .. }
                    | BuiltinAction::MouseDrag { .. } | BuiltinAction::PageStep { .. }
                    | BuiltinAction::PageBack | BuiltinAction::CycleActions { .. }
                    | BuiltinAction::RepeatActions { .. } | BuiltinAction::RepeatHeld { .. }
                    | BuiltinAction::Conditional { .. } | BuiltinAction::WaitCondition { .. }
                    | BuiltinAction::CancelMacros | BuiltinAction::BrightnessStep { .. }
                    | BuiltinAction::AutomationControl { .. } | BuiltinAction::TransformText { .. }
                    | BuiltinAction::ClipboardSlot { .. } | BuiltinAction::Gesture { .. }
                    | BuiltinAction::PressRelease { .. } | BuiltinAction::WaitRelease { .. }
                    | BuiltinAction::Parallel { .. } | BuiltinAction::FailurePolicy { .. } | BuiltinAction::RandomAction { .. }
                    | BuiltinAction::ResetState { .. } | BuiltinAction::Cooldown { .. }
                    | BuiltinAction::ShowManager | BuiltinAction::PauseBindings { .. }
                    | BuiltinAction::ChoosePath { .. } => {}
                },
                Action::None | Action::Hotkey { .. } | Action::Media { .. } | Action::Volume { .. }
                | Action::Mute | Action::Navigate { .. } | Action::SwipeNavigation { .. } | Action::Delay { .. }
                | Action::Macro { .. } | Action::Dial { .. } | Action::Toggle { .. } | Action::Brightness { .. }
                | Action::Lighting { .. } | Action::Sleep => {}
            }
            self.$children(|child| child.$name(text, plugin))?;
            Ok(())
        }
    };
}

impl Action {
    field_traversal!(try_visit_fields, as_ref, try_for_each_child,);
    field_traversal!(try_visit_fields_mut, as_mut, try_for_each_child_mut, mut);
}
