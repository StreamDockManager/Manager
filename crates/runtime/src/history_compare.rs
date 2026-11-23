//! Read-only comparisons of retained, integrity-checked portable history
use crate::{actions::Action, workspace::HistoryItem, Binding, ProfileData, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct HistoryFieldChange {
    pub field: String,
    pub before: Option<String>,
    pub after: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryControlAddress {
    pub profile_id: String,
    pub page_id: String,
    pub control_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct HistoryChange {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub operation: String,
    pub fields: Vec<HistoryFieldChange>,
    pub control: Option<HistoryControlAddress>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct HistoryComparison {
    pub item: HistoryItem,
    pub changes: Vec<HistoryChange>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct StaticHistoryPreview {
    pub image: Option<String>,
    pub warning: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct HistoryControlComparison {
    pub before: StaticHistoryPreview,
    pub after: StaticHistoryPreview,
}
struct Entity {
    kind: &'static str,
    label: String,
    value: Value,
    opaque: Vec<Value>,
    recordings: Vec<Value>,
    control: Option<HistoryControlAddress>,
}
// Extract opaque plugin settings and replace them with a comparison-safe marker
fn action(action: &mut Action, opaque: &mut Vec<Value>) {
    // The shared typed visitor defines the only opaque plugin-setting boundary
    let _: Result<(), ()> =
        action.try_visit_fields_mut(&mut |_, _| Ok(()), &mut |_, _, settings| {
            opaque.push(settings.clone());
            *settings = json!("Plugin settings omitted from comparison");
            Ok(())
        });
}
// Index comparable workspace entities with their labels, controls and opaque payloads
fn entities(data: &ProfileData) -> BTreeMap<String, Entity> {
    let mut result = BTreeMap::new();
    let mut add = |id: String, kind, label: String, mut value: Value, opaque, control| {
        // Collect embedded keyboard recordings without descending into their opaque records
        fn recordings(value: &Value, output: &mut Vec<Value>) {
            if value.get("kind").and_then(Value::as_str) == Some("keyboard_macro") {
                output.push(value.clone());
            } else {
                match value {
                    Value::Object(fields) => fields.values().for_each(|v| recordings(v, output)),
                    Value::Array(values) => values.iter().for_each(|v| recordings(v, output)),
                    _ => {}
                }
            }
        }
        let mut keyboard_recordings = vec![];
        recordings(&value, &mut keyboard_recordings);
        crate::keyboard_macro::redact_keyboard_recordings(&mut value);
        result.insert(
            id,
            Entity {
                kind,
                label: crate::macro_debug::bounded(label, 320),
                value,
                opaque,
                recordings: keyboard_recordings,
                control,
            },
        );
    };
    add(
        "workspace".into(),
        "workspace",
        "Workspace".into(),
        json!({"default_profile":data.active_profile,"minimum_manager":data.minimum_manager,"rule_order":data.rules.iter().map(|r| &r.id).collect::<Vec<_>>(),"style_order":data.styles.iter().map(|s| &s.id).collect::<Vec<_>>(),"profile_order":data.profiles.iter().map(|p| &p.id).collect::<Vec<_>>(),"macro_order":data.saved_macros.iter().map(|m| &m.id).collect::<Vec<_>>()}),
        vec![],
        None,
    );
    for profile in &data.profiles {
        let mut value = serde_json::to_value(profile).unwrap();
        value.as_object_mut().unwrap().remove("pages");
        value["page_order"] = json!(profile.pages.iter().map(|p| &p.id).collect::<Vec<_>>());
        add(
            format!("profile:{}", profile.id),
            "profile",
            profile.name.clone(),
            value,
            vec![],
            None,
        );
        for page in &profile.pages {
            add(
                format!("page:{}", json!([profile.id, page.id])),
                "page",
                format!("{} / {}", profile.name, page.name),
                json!({"name":page.name}),
                vec![],
                None,
            );
            for (control, binding) in &page.bindings {
                let mut binding = binding.clone();
                let mut opaque = vec![];
                action(&mut binding.action, &mut opaque);
                add(
                    format!("control:{}", json!([profile.id, page.id, control])),
                    "control",
                    format!("{} / {} / {}", profile.name, page.name, control),
                    serde_json::to_value(binding).unwrap(),
                    opaque,
                    Some(HistoryControlAddress {
                        profile_id: profile.id.clone(),
                        page_id: page.id.clone(),
                        control_id: control.clone(),
                    }),
                );
            }
        }
    }
    for saved in &data.saved_macros {
        let mut saved = saved.clone();
        let mut opaque = vec![];
        action(&mut saved.action, &mut opaque);
        add(
            format!("macro:{}", saved.id),
            "macro",
            saved.name.clone(),
            serde_json::to_value(saved).unwrap(),
            opaque,
            None,
        );
    }
    for rule in &data.rules {
        add(
            format!("rule:{}", rule.id),
            "rule",
            format!("Rule {}", rule.id),
            serde_json::to_value(rule).unwrap(),
            vec![],
            None,
        );
    }
    for style in &data.styles {
        add(
            format!("style:{}", style.id),
            "style",
            style.name.clone(),
            serde_json::to_value(style).unwrap(),
            vec![],
            None,
        );
    }
    result
}
struct Budget {
    remaining: usize,
    truncated: bool,
}
// Format a bounded text snippet for a history comparison value
fn snippet(value: &Value) -> String {
    crate::macro_debug::bounded(
        value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string()),
        2048,
    )
}
// Append one field difference while respecting count and payload budgets
fn field(
    path: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    output: &mut Vec<HistoryFieldChange>,
    budget: &mut Budget,
) {
    if output.len() >= 100 {
        budget.truncated = true;
        return;
    }
    let change = HistoryFieldChange {
        field: crate::macro_debug::bounded(path.replace('_', " "), 320),
        before: before.map(snippet),
        after: after.map(snippet),
    };
    let size = change.field.len()
        + change.before.as_ref().map_or(0, String::len)
        + change.after.as_ref().map_or(0, String::len);
    if size > budget.remaining {
        budget.truncated = true;
        return;
    }
    budget.remaining -= size;
    output.push(change);
}
// Compare nested JSON values within depth and output budgets
fn difference(
    before: Option<&Value>,
    after: Option<&Value>,
    path: String,
    output: &mut Vec<HistoryFieldChange>,
    budget: &mut Budget,
    depth: usize,
) {
    if before == after {
        return;
    }
    if budget.remaining == 0 || output.len() >= 100 {
        budget.truncated = true;
        return;
    }
    if depth < 12 {
        if (before.is_none() || before.is_some_and(Value::is_object))
            && (after.is_none() || after.is_some_and(Value::is_object))
        {
            let keys: BTreeSet<_> = before
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|m| m.keys())
                .chain(
                    after
                        .and_then(Value::as_object)
                        .into_iter()
                        .flat_map(|m| m.keys()),
                )
                .collect();
            for key in keys {
                difference(
                    before.and_then(|v| v.get(key)),
                    after.and_then(|v| v.get(key)),
                    if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path} / {key}")
                    },
                    output,
                    budget,
                    depth + 1,
                );
            }
            return;
        }
        if (before.is_none() || before.is_some_and(Value::is_array))
            && (after.is_none() || after.is_some_and(Value::is_array))
        {
            let count = before
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
                .max(after.and_then(Value::as_array).map_or(0, Vec::len));
            for index in 0..count {
                difference(
                    before.and_then(|v| v.get(index)),
                    after.and_then(|v| v.get(index)),
                    format!("{path} / {}", index + 1),
                    output,
                    budget,
                    depth + 1,
                );
            }
            return;
        }
    }
    field(&path, before, after, output, budget);
}
// Compare the supplied history compare values
pub(crate) fn compare(
    item: HistoryItem,
    before: &ProfileData,
    after: &ProfileData,
) -> HistoryComparison {
    let before = entities(before);
    let after = entities(after);
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut budget = Budget {
        remaining: 512 * 1024,
        truncated: false,
    };
    let mut changes = vec![];
    for key in keys {
        let old = before.get(key);
        let new = after.get(key);
        if old.zip(new).is_some_and(|(a, b)| {
            a.value == b.value && a.opaque == b.opaque && a.recordings == b.recordings
        }) {
            continue;
        }
        if changes.len() >= 250 || budget.remaining == 0 {
            budget.truncated = true;
            break;
        }
        let mut fields = vec![];
        difference(
            old.map(|e| &e.value),
            new.map(|e| &e.value),
            String::new(),
            &mut fields,
            &mut budget,
            0,
        );
        if old.map(|e| &e.opaque) != new.map(|e| &e.opaque)
            && (old.is_some_and(|e| !e.opaque.is_empty())
                || new.is_some_and(|e| !e.opaque.is_empty()))
        {
            let saved = json!("Saved values omitted");
            let changed = json!("Changed values omitted");
            field(
                "Action / plugin settings",
                old.map(|_| &saved),
                new.map(|_| &changed),
                &mut fields,
                &mut budget,
            );
        }
        if old.map(|e| &e.recordings) != new.map(|e| &e.recordings)
            && (old.is_some_and(|e| !e.recordings.is_empty())
                || new.is_some_and(|e| !e.recordings.is_empty()))
        {
            let saved = json!("Saved recording; keys omitted");
            let changed = json!("Changed recording; keys omitted");
            field(
                "Action / keyboard recording",
                old.map(|_| &saved),
                new.map(|_| &changed),
                &mut fields,
                &mut budget,
            );
        }
        let entity = new.or(old).unwrap();
        changes.push(HistoryChange {
            id: key.clone(),
            kind: entity.kind.into(),
            label: entity.label.clone(),
            operation: if old.is_none() {
                "added"
            } else if new.is_none() {
                "removed"
            } else {
                "changed"
            }
            .into(),
            fields,
            control: entity.control.clone(),
        });
    }
    HistoryComparison {
        item,
        changes,
        truncated: budget.truncated,
    }
}
impl Runtime {
    // Compare history
    pub fn compare_history(
        &self,
        expected_revision: u64,
        id: &str,
    ) -> Result<HistoryComparison, String> {
        let (item, before, after, _assets) =
            self.profiles.inspect_history(expected_revision, id)?;
        let mut result = compare(item, &before, &after);
        // Only known in-memory redactions are consulted; no credential provider call
        let secrets = self.credentials.redactions();
        let mut remaining = 512 * 1024;
        for change in &mut result.changes {
            for field in &mut change.fields {
                for value in [&mut field.before, &mut field.after].into_iter().flatten() {
                    *value = crate::macro_debug::bounded(
                        stream_dock_manager_plugin_host::redact_known_values(value, &secrets),
                        2048,
                    );
                }
            }
            change.fields.retain(|field| {
                let size = field.field.len()
                    + field.before.as_ref().map_or(0, String::len)
                    + field.after.as_ref().map_or(0, String::len);
                if size > remaining {
                    result.truncated = true;
                    false
                } else {
                    remaining -= size;
                    true
                }
            });
        }
        Ok(result)
    }
    // Preview history control using the current history compare state
    pub fn preview_history_control(
        &self,
        expected_revision: u64,
        id: &str,
        address: &HistoryControlAddress,
    ) -> Result<HistoryControlComparison, String> {
        let (_, before, after, _assets) = self.profiles.inspect_history(expected_revision, id)?;
        let preview = |data: &ProfileData| {
            let Some(profile) = data.profiles.iter().find(|p| p.id == address.profile_id) else {
                return StaticHistoryPreview {
                    image: None,
                    warning: Some("Profile was absent".into()),
                };
            };
            let binding = profile
                .pages
                .iter()
                .find(|p| p.id == address.page_id)
                .and_then(|p| p.bindings.get(&address.control_id));
            let Some(binding) = binding else {
                return StaticHistoryPreview {
                    image: None,
                    warning: Some("Control was unassigned".into()),
                };
            };
            match self.static_history_image(&profile.model_id, &address.control_id, binding) {
                Ok(image) => StaticHistoryPreview {
                    image: Some(image),
                    warning: matches!(binding.action, Action::Plugin { .. }).then(|| {
                        "Static saved appearance only; plugin feedback is not executed.".into()
                    }),
                },
                Err(error) => StaticHistoryPreview {
                    image: None,
                    warning: Some(error),
                },
            }
        };
        Ok(HistoryControlComparison {
            before: preview(&before),
            after: preview(&after),
        })
    }
    // Render a static historical binding image for the selected model and control
    fn static_history_image(
        &self,
        model: &str,
        control_id: &str,
        binding: &Binding,
    ) -> Result<String, String> {
        use base64::Engine;
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == model)
            .ok_or("Historical model is unavailable")?;
        let source = manifest.control(control_id).map_err(|e| e.to_string())?;
        let control = manifest
            .control(source.feedback_surface.as_deref().unwrap_or(control_id))
            .map_err(|e| e.to_string())?;
        let width = control
            .image_width
            .ok_or("Control has no display surface")?;
        let height = control
            .image_height
            .ok_or("Control has no display surface")?;
        let scale = 160.0 / f64::from(width.max(height)).max(160.0);
        let width = (f64::from(width) * scale).round().max(1.0) as u16;
        let height = (f64::from(height) * scale).round().max(1.0) as u16;
        let rgba = hardware_core::appearance::background_color(&binding.color)
            .ok_or("Invalid saved background color")?;
        let base = if let Some(asset) = &binding.asset {
            let pixels = self.profiles.render_media(
                asset,
                std::time::Duration::ZERO,
                width,
                height,
                0,
                "png",
                &binding.media,
            )?;
            hardware_core::render::over_color(&pixels, rgba).map_err(|e| e.to_string())?
        } else {
            hardware_core::render::tile_rgba(width, height, rgba, &binding.title, 0, "png")
                .map_err(|e| e.to_string())?
        };
        let media_title = hardware_core::appearance::Appearance::default();
        let appearance = binding.appearance.as_deref().or_else(|| {
            (binding.asset.is_some() && !binding.title.is_empty()).then_some(&media_title)
        });
        let image = if let Some(appearance) = appearance {
            hardware_core::appearance::compose(
                &base,
                width,
                height,
                &binding.title,
                appearance,
                "",
                0,
            )
            .map_err(|e| e.to_string())?
        } else {
            base
        };
        Ok(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(image)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        credentials::{CredentialProvider, CredentialState, Credentials},
        workspace::Edit,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[test]
    // Verify that keyboard recording history omits keys but retains change detection
    fn keyboard_recording_history_omits_keys_but_retains_change_detection() {
        use crate::{
            builtins::{BuiltinAction, SavedMacro},
            keyboard_macro::*,
        };
        let mut data = ProfileData::default();
        data.saved_macros.push(SavedMacro {
            id: "recording".into(),
            name: "Recording".into(),
            inputs: vec![],
            action: Action::Builtin {
                action: Box::new(BuiltinAction::KeyboardMacro {
                    recording: KeyboardMacroRecording {
                        events: vec![
                            KeyboardMacroEvent {
                                kind: KeyboardMacroEventKind::KeyDown,
                                key: PhysicalKey::KeyA,
                                delay_ms: 0,
                            },
                            KeyboardMacroEvent {
                                kind: KeyboardMacroEventKind::KeyUp,
                                key: PhysicalKey::KeyA,
                                delay_ms: 50,
                            },
                        ],
                        ..Default::default()
                    },
                    playback: Default::default(),
                }),
            },
        });
        let before = entities(&data);
        if let Action::Builtin { action } = &mut data.saved_macros[0].action {
            if let BuiltinAction::KeyboardMacro { recording, .. } = action.as_mut() {
                for event in &mut recording.events {
                    event.key = PhysicalKey::KeyB;
                }
            }
        }
        let after = entities(&data);
        let old = &before["macro:recording"];
        let new = &after["macro:recording"];
        assert_eq!(old.value, new.value);
        assert_ne!(old.recordings, new.recordings);
        for entity in [old, new] {
            let display = entity.value.to_string();
            assert!(!display.contains("KeyA") && !display.contains("KeyB"));
            assert!(display.contains("event_count"));
        }
    }
    struct NoReads(Arc<AtomicUsize>);
    impl CredentialProvider for NoReads {
        // Read the requested history compare data
        fn read(&self, _: &str) -> Result<String, CredentialState> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(CredentialState::Locked)
        }
        // Write the supplied history compare data
        fn write(&self, _: &str, _: &str) -> Result<(), CredentialState> {
            Err(CredentialState::Locked)
        }
        // Remove the selected history compare entry
        fn delete(&self, _: &str) -> Result<(), CredentialState> {
            Err(CredentialState::Locked)
        }
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; comparison and thumbnails execute no actions"]
    // Verify that semantic changes and static previews do not resolve or execute actions
    fn semantic_changes_and_static_previews_do_not_resolve_or_execute_actions() {
        let root = tempfile::tempdir().unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let runtime = Runtime::build_credentials(
            root.path().into(),
            false,
            Credentials::with_provider(Arc::new(NoReads(reads.clone()))),
        )
        .unwrap();
        let revision = runtime.snapshot().configuration_revision;
        let mut profile = runtime.snapshot().profiles[0].clone();
        profile
            .parameters
            .push(crate::parameters::ProfileParameter {
                name: "token".into(),
                label: "Token".into(),
                r#type: "string".into(),
                required: true,
                default: None,
                sensitive: true,
            });
        let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
        binding.title = "Before".into();
        binding.action = Action::Plugin {
            user_desired_state: None,
            plugin_id: "not-installed".into(),
            action_id: "never-execute".into(),
            settings: json!({"token":{"$parameter":"token"},"private":"old-private"}),
            resources: Default::default(),
        };
        runtime
            .apply_edits(
                revision,
                "First",
                vec![Edit::SaveProfile {
                    profile: profile.clone(),
                }],
            )
            .unwrap();
        profile.pages[0].bindings.get_mut("key-1").unwrap().title = "After".into();
        if let Action::Plugin { settings, .. } =
            &mut profile.pages[0].bindings.get_mut("key-1").unwrap().action
        {
            settings["private"] = json!("new-private");
        }
        let current = runtime
            .apply_edits(revision + 1, "Second", vec![Edit::SaveProfile { profile }])
            .unwrap();
        let id = current.history.undo.last().unwrap().id.clone();
        let document = serde_json::to_vec(&runtime.profiles.load().unwrap()).unwrap();
        let comparison = runtime.compare_history(revision + 2, &id).unwrap();
        let control = comparison
            .changes
            .iter()
            .find(|c| c.kind == "control")
            .unwrap();
        assert!(control.fields.iter().any(|f| f.field == "title"
            && f.before.as_deref() == Some("Before")
            && f.after.as_deref() == Some("After")));
        assert!(control
            .fields
            .iter()
            .any(|f| f.field == "Action / plugin settings"));
        let serialized = serde_json::to_string(&comparison).unwrap();
        assert!(!serialized.contains("old-private") && !serialized.contains("new-private"));
        let image = runtime
            .preview_history_control(revision + 2, &id, control.control.as_ref().unwrap())
            .unwrap();
        assert!(image
            .before
            .image
            .as_deref()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(image
            .after
            .image
            .as_deref()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(
            serde_json::to_vec(&runtime.profiles.load().unwrap()).unwrap(),
            document
        );
        assert_eq!(runtime.snapshot().configuration_revision, revision + 2);
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable host loopback sockets; no effects"]
    // Verify that stale corrupt and unhashed history fail closed without disabling undo
    fn stale_corrupt_and_unhashed_history_fail_closed_without_disabling_undo() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let before = runtime.profiles.load().unwrap();
        let revision = before.configuration_revision;
        let mut profile = before.profiles[0].clone();
        profile.name = "Changed".into();
        let current = runtime
            .apply_edits(revision, "Rename", vec![Edit::SaveProfile { profile }])
            .unwrap();
        let id = current.history.undo.last().unwrap().id.clone();
        assert!(runtime
            .compare_history(0, &id)
            .unwrap_err()
            .contains("changed"));
        assert!(runtime.compare_history(revision + 1, "../escape").is_err());
        assert!(runtime
            .compare_history(revision + 1, &uuid::Uuid::new_v4().to_string())
            .unwrap_err()
            .contains("retained"));
        let original = runtime.profiles.history_bytes(&id).unwrap();
        let mut corrupt: Value = serde_json::from_slice(&original).unwrap();
        corrupt["before_hash"] = json!("corrupt");
        runtime
            .profiles
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::History,
                &id,
                &corrupt,
                32 * 1024 * 1024,
            )
            .unwrap();
        assert!(runtime
            .compare_history(revision + 1, &id)
            .unwrap_err()
            .contains("integrity"));
        let mut after = runtime.profiles.load().unwrap();
        after.history = Default::default();
        runtime
            .profiles
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::History,
                &id,
                &json!({"before":before,"after":after}),
                32 * 1024 * 1024,
            )
            .unwrap();
        // Released history requires integrity hashes. A rejected record must
        // survive inspection and undo attempts until its intact copy is restored.
        let recorded = runtime.profiles.history_bytes(&id).unwrap();
        assert!(runtime.compare_history(revision + 1, &id).is_err());
        assert!(runtime.history_action("undo", revision + 1).is_err());
        assert_eq!(runtime.profiles.history_bytes(&id).unwrap(), recorded);
        let valid: Value = serde_json::from_slice(&original).unwrap();
        runtime
            .profiles
            .storage
            .write_json(
                stream_dock_manager_storage::Domain::History,
                &id,
                &valid,
                32 * 1024 * 1024,
            )
            .unwrap();
        let undone = runtime.history_action("undo", revision + 1).unwrap();
        assert_eq!(undone.profiles[0].name, "Default");
        runtime.shutdown();
    }
}
