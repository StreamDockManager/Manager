//! Ephemeral scratch-target identity. Never accepts a caller-supplied PID, handle,
//! or title. The service calls capture only after an explicit selection countdown
//! and keeps the returned object on that same worker until cancellation/completion
#[cfg(target_os = "macos")]
#[path = "macos_test_target.rs"]
mod native;
#[cfg(target_os = "windows")]
#[path = "windows_test_target.rs"]
mod native;

#[cfg(any(target_os = "linux", test))]
#[path = "linux_test_target.rs"]
mod linux;

pub(crate) trait TestTarget {
    // Return the display label for test target
    fn label(&self) -> &str;
    /// Focus may leave the target while the user returns to Manager to confirm
    fn check_alive(&mut self) -> Result<(), String>;
    /// Playback additionally requires the exact pinned window to be foreground
    fn check_focused(&mut self) -> Result<(), String>;
    /// Checked observer teardown must finish before the service reports completion
    fn finish(&mut self) -> Result<(), String> {
        Ok(())
    }
}

// Explain why the current operation is unavailable
pub(crate) fn unavailable_reason() -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    return native::unavailable_reason();
    #[cfg(target_os = "linux")]
    return if super::hyprland::selected() {
        super::hyprland::readiness().err()
    } else {
        linux::unavailable_reason()
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    Some("Scratch-target selection is unavailable on this platform: a qualified foreground-window identity and lifetime observer are required. Use simulation instead.".into())
}

// Capture the current test target observation
pub(crate) fn capture() -> Result<Box<dyn TestTarget>, String> {
    if let Some(reason) = unavailable_reason() {
        return Err(reason);
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    return native::capture();
    #[cfg(target_os = "linux")]
    return if super::hyprland::selected() {
        super::hyprland::capture_target()
    } else {
        linux::capture()
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    Err("Scratch-target selection is unavailable on this platform".into())
}

/// Reuse the X11 window/client/process lifetime contract for ordinary playback
/// The caller chooses strict pinning or revalidates each explicitly permitted
/// focus change; this never changes focus or accepts a caller-supplied identity
#[cfg(target_os = "linux")]
// Capture X11 playback
pub(crate) fn capture_x11_playback() -> Result<Box<dyn TestTarget>, String> {
    linux::capture()
}

/// A short, one-line native-derived label for the confirmation UI, never an
/// executable identity. Strip controls, bidi overrides and invisible formatting
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux", test))]
// Build the user-facing label for the selected scratch target
pub(crate) fn display_label(application: &str, window: &str) -> String {
    // Remove obsolete test target state
    fn clean(value: &str, bytes: usize) -> String {
        let mut result = String::new();
        let mut space = false;
        for character in value.chars().take(4096) {
            if character.is_whitespace() || character.is_control() {
                space = !result.is_empty();
                continue;
            }
            if matches!(character, '\u{00ad}' | '\u{061c}' | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
            {
                continue;
            }
            if result.len() + usize::from(space) + character.len_utf8() > bytes {
                break;
            }
            if space {
                result.push(' ');
                space = false;
            }
            result.push(character);
        }
        result
    }
    let application = clean(application, 112);
    let window = clean(window, 192);
    let application = if application.is_empty() {
        "Application"
    } else {
        &application
    };
    if window.is_empty() {
        application.into()
    } else {
        format!("{application}: {window}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that target labels are bounded sanitized presentation only
    fn target_labels_are_bounded_sanitized_presentation_only() {
        assert_eq!(
            display_label("  Editor\nApp\u{202e}", "A\t document\u{200b}"),
            "Editor App: A document"
        );
        assert_eq!(display_label("\u{202e}\n", ""), "Application");
        let label = display_label(&"界".repeat(4096), &"😀".repeat(4096));
        assert!(label.len() <= 306);
        assert!(!label.chars().any(char::is_control));
    }
}
