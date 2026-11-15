//! Pure ingress validation. Never capture, prompt, map native keys, or inject input here
use super::types::*;
use crate::{actions::Action, builtins::BuiltinAction};
use std::collections::HashSet;

/// Shared ceiling for an expanded action and every actual run (including references/repeats)
pub const MAX_ACTION_EVENTS: usize = 100_000;

// Validate the supplied validation value
pub fn validate(
    recording: &KeyboardMacroRecording,
    playback: &KeyboardMacroPlayback,
) -> Result<KeyboardMacroSummary, String> {
    validate_with_empty_draft(recording, playback, false)
}
/// An editor placeholder may have no events. All metadata and nonempty sequences
/// remain validated; this entry point must never authorize persistence as an action or output
pub(crate) fn validate_draft(
    recording: &KeyboardMacroRecording,
    playback: &KeyboardMacroPlayback,
) -> Result<KeyboardMacroSummary, String> {
    validate_with_empty_draft(recording, playback, true)
}
// Validate with empty draft
fn validate_with_empty_draft(
    recording: &KeyboardMacroRecording,
    playback: &KeyboardMacroPlayback,
    allow_empty: bool,
) -> Result<KeyboardMacroSummary, String> {
    if recording.version != RECORDING_VERSION {
        return Err("recording.version: unsupported keyboard recording version".into());
    }
    if recording.layout.as_ref().is_some_and(|layout| {
        layout.is_empty() || layout.len() > 256 || layout.chars().any(char::is_control)
    }) {
        return Err("recording.layout: expected a nonempty identifier of at most 256 bytes".into());
    }
    if !playback.speed.is_finite() || !(MIN_SPEED..=MAX_SPEED).contains(&playback.speed) {
        return Err("playback.speed: expected a finite number from 0.25 to 4".into());
    }
    if (!allow_empty && recording.events.is_empty()) || recording.events.len() > MAX_EVENTS {
        return Err("recording.events: expected 1–10,000 events".into());
    }
    let bytes = serde_json::to_vec(&(recording, playback))
        .map_err(|_| "recording: cannot encode keyboard recording")?;
    if bytes.len() > MAX_SERIALIZED_BYTES {
        return Err("recording: serialized keyboard macro exceeds 1 MiB".into());
    }
    let mut held = HashSet::new();
    let mut duration_ms = 0u64;
    for (index, event) in recording.events.iter().enumerate() {
        duration_ms = duration_ms
            .checked_add(u64::from(event.delay_ms))
            .ok_or_else(|| format!("recording.events[{index}].delay_ms: duration overflow"))?;
        if duration_ms > MAX_DURATION_MS {
            return Err(format!(
                "recording.events[{index}].delay_ms: recording exceeds five minutes"
            ));
        }
        let invalid = match event.kind {
            KeyboardMacroEventKind::KeyDown if !held.insert(event.key) => {
                Some("key is already held; use an explicit repeat event")
            }
            KeyboardMacroEventKind::KeyUp if !held.remove(&event.key) => {
                Some("release has no preceding press")
            }
            KeyboardMacroEventKind::Repeat if !held.contains(&event.key) => {
                Some("repeat has no preceding press")
            }
            _ => None,
        };
        if let Some(error) = invalid {
            return Err(format!("recording.events[{index}].kind: {error}"));
        }
    }
    if !held.is_empty() {
        return Err("recording.events: every pressed key requires a release".into());
    }
    Ok(KeyboardMacroSummary {
        event_count: recording.events.len(),
        duration_ms,
        playback_duration_ms: (duration_ms as f64 / playback.speed).ceil() as u64,
    })
}

/// Count statically expanded keyboard events without materializing the expansion
/// Saved references are checked across the library and share the runtime counter
pub fn action_event_count(action: &Action) -> Result<usize, String> {
    expanded_event_count(action, |_| None)
}

// Calculate playback event count after applying repeat settings
pub fn expanded_event_count<'a>(
    action: &Action,
    lookup: impl Fn(&str) -> Option<&'a Action>,
) -> Result<usize, String> {
    // Visit the current validation value
    fn visit<'a>(
        action: &Action,
        depth: usize,
        lookup: &impl Fn(&str) -> Option<&'a Action>,
        stack: &mut HashSet<String>,
        cache: &mut std::collections::HashMap<(String, usize), usize>,
    ) -> Result<usize, String> {
        if depth > 8 {
            return Err("Keyboard macro action nesting exceeds 8".into());
        }
        if let Action::Builtin { action } = action {
            match action.as_ref() {
                BuiltinAction::KeyboardMacro { recording, .. } => {
                    return Ok(recording.events.len())
                }
                BuiltinAction::SavedMacro { macro_id, .. } => {
                    if stack.contains(macro_id) {
                        return Err("Saved macro reference cycle".into());
                    }
                    let key = (macro_id.clone(), depth);
                    if let Some(count) = cache.get(&key) {
                        return Ok(*count);
                    }
                    if let Some(action) = lookup(macro_id) {
                        stack.insert(macro_id.clone());
                        let count = visit(action, depth + 1, lookup, stack, cache)?;
                        stack.remove(macro_id);
                        cache.insert(key, count);
                        return Ok(count);
                    }
                }
                _ => {}
            }
        }
        let mut count = 0usize;
        for child in (0..).map_while(|index| action.child(index)) {
            count = count
                .checked_add(visit(child, depth + 1, lookup, stack, cache)?)
                .ok_or("Keyboard macro expanded event budget exceeded")?;
        }
        if let Action::Builtin { action } = action {
            let multiplier = match action.as_ref() {
                BuiltinAction::RepeatActions { count, .. } => (*count).max(0) as usize,
                BuiltinAction::FailurePolicy {
                    operation, retries, ..
                } if operation == "retry" => (*retries).max(0) as usize + 1,
                _ => 1,
            };
            count = count
                .checked_mul(multiplier)
                .ok_or("Keyboard macro expanded event budget exceeded")?;
        }
        if count > MAX_ACTION_EVENTS {
            return Err("Keyboard macro expanded event budget exceeds 100,000 events".into());
        }
        Ok(count)
    }
    visit(
        action,
        0,
        &lookup,
        &mut Default::default(),
        &mut Default::default(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    // Verify that repeat mode defaults exact and rejects unknown values
    fn repeat_mode_defaults_exact_and_rejects_unknown_values() {
        use super::*;
        let legacy = serde_json::json!({"speed":1.0,"focus_policy":"stay_on_target"});
        let exact: KeyboardMacroPlayback = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(exact.repeat_mode, KeyboardMacroRepeatMode::Exact);
        let mut system = legacy.clone();
        system["repeat_mode"] = serde_json::json!("system");
        let system: KeyboardMacroPlayback = serde_json::from_value(system).unwrap();
        assert_eq!(system.repeat_mode, KeyboardMacroRepeatMode::System);
        assert_eq!(
            serde_json::to_value(&system).unwrap()["repeat_mode"],
            "system"
        );
        let mut invalid = legacy;
        invalid["repeat_mode"] = serde_json::json!("approximate");
        assert!(serde_json::from_value::<KeyboardMacroPlayback>(invalid).is_err());
        validate(&fixture(), &system).unwrap();
        let mut unbalanced = fixture();
        unbalanced.events.remove(0);
        assert!(validate(&unbalanced, &system).is_err());
    }
    use super::*;
    use serde_json::json;
    // Handle the incoming validation event
    fn event(kind: KeyboardMacroEventKind, key: PhysicalKey, delay_ms: u32) -> KeyboardMacroEvent {
        KeyboardMacroEvent {
            kind,
            key,
            delay_ms,
        }
    }
    // Construct an isolated validation fixture
    pub(crate) fn fixture() -> KeyboardMacroRecording {
        use KeyboardMacroEventKind::*;
        use PhysicalKey::*;
        KeyboardMacroRecording {
            events: vec![
                event(KeyDown, ControlLeft, 0),
                event(KeyDown, KeyC, 25),
                event(KeyUp, KeyC, 25),
                event(KeyUp, ControlLeft, 0),
            ],
            ..Default::default()
        }
    }
    #[test]
    // Verify that physical codes round trip without aliases or layout translation
    fn physical_codes_round_trip_without_aliases_or_layout_translation() {
        for key in PhysicalKey::ALL {
            let json = serde_json::to_value(key).unwrap();
            assert_eq!(PhysicalKey::from_code(json.as_str().unwrap()), Some(*key));
        }
        for invalid in [
            "a",
            "ctrl",
            "Fn",
            "MediaPlayPause",
            "Unidentified",
            "OSLeft",
        ] {
            assert!(PhysicalKey::from_code(invalid).is_none());
            let error = serde_json::from_value::<PhysicalKey>(json!(invalid))
                .unwrap_err()
                .to_string();
            assert_eq!(error, "unsupported physical keyboard code");
        }
        assert_ne!(PhysicalKey::ControlLeft, PhysicalKey::ControlRight);
    }
    #[test]
    // Verify that sequence preserves overlaps repeats modifier sides and equal time order
    fn sequence_preserves_overlaps_repeats_modifier_sides_and_equal_time_order() {
        use KeyboardMacroEventKind::*;
        use PhysicalKey::*;
        let mut r = fixture();
        r.events.insert(2, event(Repeat, KeyC, 50));
        r.events.insert(1, event(KeyDown, ShiftRight, 0));
        r.events.push(event(KeyUp, ShiftRight, 0));
        let summary = validate(&r, &KeyboardMacroPlayback::default()).unwrap();
        assert_eq!(summary.duration_ms, 100);
        assert_eq!(summary.event_count, 7);
        let roundtrip: KeyboardMacroRecording =
            serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(roundtrip, r);
    }
    #[test]
    // Verify that delays scale once at cumulative deadline
    fn delays_scale_once_at_cumulative_deadline() {
        let mut playback = KeyboardMacroPlayback {
            speed: 4.0,
            ..Default::default()
        };
        assert_eq!(
            validate(&fixture(), &playback)
                .unwrap()
                .playback_duration_ms,
            13
        );
        playback.speed = 0.25;
        assert_eq!(
            validate(&fixture(), &playback)
                .unwrap()
                .playback_duration_ms,
            200
        );
    }
    #[test]
    // Verify that malformed ownership is rejected without including keys in error
    fn malformed_ownership_is_rejected_without_including_keys_in_error() {
        use KeyboardMacroEventKind::*;
        let r = fixture();
        for events in [
            vec![r.events[1]],
            vec![r.events[2]],
            vec![r.events[1], r.events[1]],
            vec![event(Repeat, PhysicalKey::KeyC, 0)],
        ] {
            let result = validate(
                &KeyboardMacroRecording {
                    events,
                    ..r.clone()
                },
                &Default::default(),
            )
            .unwrap_err();
            assert!(result.starts_with("recording.events"));
            assert!(!result.contains("KeyC"));
        }
    }
    #[test]
    // Verify that contract rejects unknown fields keys kinds versions and invalid numbers
    fn contract_rejects_unknown_fields_keys_kinds_versions_and_invalid_numbers() {
        let original = serde_json::to_value(fixture()).unwrap();
        for (pointer, value) in [
            ("/events/0/key", json!("Fn")),
            ("/events/0/kind", json!("tap")),
            ("/events/0/delay_ms", json!(-1)),
            ("/events/0/delay_ms", json!(0.5)),
            ("/events/0/delay_ms", json!(u64::MAX)),
            ("/key_mode", json!("text")),
        ] {
            let mut data = original.clone();
            *data.pointer_mut(pointer).unwrap() = value;
            assert!(serde_json::from_value::<KeyboardMacroRecording>(data).is_err());
        }
        let mut data = original.clone();
        data["events"][0]["native_code"] = json!(42);
        assert!(serde_json::from_value::<KeyboardMacroRecording>(data).is_err());
        let mut data = original;
        data["extra"] = json!(true);
        assert!(serde_json::from_value::<KeyboardMacroRecording>(data).is_err());
        let mut r = fixture();
        r.version = 2;
        assert!(validate(&r, &Default::default())
            .unwrap_err()
            .contains("version"));
        for speed in [
            0.0,
            -1.0,
            0.249,
            4.001,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            assert!(validate(
                &fixture(),
                &KeyboardMacroPlayback {
                    speed,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .contains("speed"));
        }
    }
    #[test]
    // Verify that bounded duration events layout and nonempty content
    fn bounded_duration_events_layout_and_nonempty_content() {
        let mut r = fixture();
        r.events[1].delay_ms = 299_975;
        assert_eq!(
            validate(&r, &Default::default()).unwrap().duration_ms,
            MAX_DURATION_MS
        );
        r.events[1].delay_ms += 1;
        assert!(validate(&r, &Default::default()).is_err());
        r.events[1].delay_ms = u32::MAX;
        assert!(validate(&r, &Default::default()).is_err());
        let mut r = fixture();
        r.events.clear();
        assert!(validate(&r, &Default::default()).is_err());
        r.events = fixture().events.repeat(MAX_EVENTS / 4);
        assert_eq!(
            validate(&r, &Default::default()).unwrap().event_count,
            MAX_EVENTS
        );
        r.events.extend(fixture().events);
        assert!(validate(&r, &Default::default()).is_err());
        for layout in ["".into(), "x".repeat(257), "bad\nlayout".into()] {
            let r = KeyboardMacroRecording {
                layout: Some(layout),
                ..fixture()
            };
            assert!(validate(&r, &Default::default())
                .unwrap_err()
                .contains("layout"));
        }
    }
    #[test]
    // Verify that aggregate repeat budget is checked without expansion
    fn aggregate_repeat_budget_is_checked_without_expansion() {
        let leaf = Action::Builtin {
            action: Box::new(BuiltinAction::KeyboardMacro {
                recording: fixture(),
                playback: Default::default(),
            }),
        };
        let repeated = Action::Builtin {
            action: Box::new(BuiltinAction::RepeatActions {
                count: 1000,
                interval_ms: 0,
                children: vec![leaf],
            }),
        };
        assert_eq!(action_event_count(&repeated).unwrap(), 4000);
        let too_many = Action::Builtin {
            action: Box::new(BuiltinAction::RepeatActions {
                count: 1000,
                interval_ms: 0,
                children: vec![repeated],
            }),
        };
        assert!(action_event_count(&too_many).is_err());
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;
    // Construct an empty macro action with current default playback settings
    fn empty_action() -> Action {
        Action::Builtin {
            action: Box::new(BuiltinAction::KeyboardMacro {
                recording: Default::default(),
                playback: Default::default(),
            }),
        }
    }
    #[test]
    // Verify that empty keyboard placeholder is valid only in editor drafts
    fn empty_keyboard_placeholder_is_valid_only_in_editor_drafts() {
        let empty = empty_action();
        for action in [
            empty.clone(),
            Action::Macro {
                actions: vec![empty.clone()],
            },
            Action::Dial {
                linked_turns: false,
                actions: vec![empty.clone(), Action::None, Action::None],
            },
            Action::Toggle {
                on: Box::new(empty.clone()),
                off: Box::new(Action::None),
            },
            Action::Builtin {
                action: Box::new(BuiltinAction::RepeatActions {
                    count: 2,
                    interval_ms: 0,
                    children: vec![empty.clone()],
                }),
            },
        ] {
            action.validate_draft(0).unwrap();
            assert!(action.validate(0).unwrap_err().contains("recording.events"));
        }
        let draft = crate::workspace::Draft {
            key: "keyboard-new".into(),
            base: None,
            value: serde_json::json!({"actionDraft":{"action":empty}, "dialActions":[{"action":empty_action()},{"action":{"kind":"none"}},{"action":{"kind":"none"}}]}),
        };
        draft.validate().unwrap();
    }
    #[test]
    // Verify that draft validation keeps metadata nonempty sequences and other actions strict
    fn draft_validation_keeps_metadata_nonempty_sequences_and_other_actions_strict() {
        let mut recording = KeyboardMacroRecording::default();
        let mut playback = KeyboardMacroPlayback::default();
        recording.version = 2;
        assert!(validate_draft(&recording, &playback).is_err());
        recording.version = RECORDING_VERSION;
        playback.speed = 0.0;
        assert!(validate_draft(&recording, &playback).is_err());
        playback.speed = 1.0;
        recording.layout = Some("bad\nlayout".into());
        assert!(validate_draft(&recording, &playback).is_err());
        recording.layout = None;
        recording.events.push(KeyboardMacroEvent {
            kind: KeyboardMacroEventKind::KeyDown,
            key: PhysicalKey::KeyA,
            delay_ms: 0,
        });
        assert!(validate_draft(&recording, &playback).is_err());
        assert!(Action::OpenUrl {
            url: "not-a-url".into()
        }
        .validate_draft(0)
        .is_err());
    }
    #[test]
    // Verify that initial selection preview and draft save do not execute or save an empty binding
    fn initial_selection_preview_and_draft_save_do_not_execute_or_save_an_empty_binding() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        runtime
            .simulate_device(true, "simulator:keyboard-draft")
            .unwrap();
        let before = runtime.snapshot();
        let mut binding = before.profiles[0].pages[0].bindings["key-1"].clone();
        binding.action = empty_action();
        assert!(binding.validate().is_err());
        binding.validate_draft().unwrap();
        let preview = runtime
            .preview_binding_details("simulator:keyboard-draft", "key-1", &binding, None)
            .unwrap();
        assert!(preview.image.starts_with("data:image/"));
        let key = "keyboard-new";
        runtime
            .save_draft(
                key,
                Some(crate::workspace::Draft {
                    key: key.into(),
                    base: None,
                    value: serde_json::json!({"actionDraft":{"action":binding.action}}),
                }),
            )
            .unwrap();
        let stored: std::collections::BTreeMap<String, crate::workspace::Draft> = runtime
            .profiles
            .read_preference(crate::profile_database::LocalPreference::Drafts)
            .unwrap()
            .unwrap();
        stored[key].validate().unwrap();
        assert_eq!(
            runtime.snapshot().configuration_revision,
            before.configuration_revision
        );
        assert_eq!(
            serde_json::to_value(runtime.snapshot().profiles).unwrap(),
            serde_json::to_value(before.profiles).unwrap()
        );
        runtime.save_draft(key, None).unwrap();
        runtime.shutdown();
    }
}
