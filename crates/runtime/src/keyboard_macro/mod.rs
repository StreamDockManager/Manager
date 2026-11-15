//! First-party keyboard macros: pure data validation and capability-gated native sessions
pub mod capture;
#[cfg(target_os = "linux")]
pub(crate) mod hyprland;
#[cfg(any(target_os = "macos", test))]
pub(crate) mod macos_modifiers;
pub mod platform;
pub mod playback;
pub mod recorder;
pub mod test_session;
pub(crate) mod test_target;
pub mod types;
pub mod validation;
pub use types::*;

use serde_json::{json, Value};
/// Omit recorded keys from diagnostic, simulation and history-comparison views
/// Durable workspace history still retains approved recordings for undo/restore
pub(crate) fn redact_keyboard_recordings(value: &mut Value) {
    if value.get("kind").and_then(Value::as_str) == Some("keyboard_macro") {
        let events = value["recording"]["events"].as_array();
        let event_count = events.map_or(0, Vec::len);
        let duration_ms: u64 = events
            .into_iter()
            .flatten()
            .filter_map(|event| event["delay_ms"].as_u64())
            .fold(0u64, u64::saturating_add);
        let speed = value["playback"]["speed"]
            .as_f64()
            .filter(|speed| speed.is_finite() && (MIN_SPEED..=MAX_SPEED).contains(speed))
            .unwrap_or(1.0);
        *value = json!({"kind":"keyboard_macro","event_count":event_count,
            "duration_ms":duration_ms,"playback_duration_ms":(duration_ms as f64 / speed).ceil() as u64});
        return;
    }
    match value {
        Value::Object(fields) => fields.values_mut().for_each(redact_keyboard_recordings),
        Value::Array(values) => values.iter_mut().for_each(redact_keyboard_recordings),
        _ => {}
    }
}
