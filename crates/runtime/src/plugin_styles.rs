use crate::{actions::Action, workspace::Draft, Binding, ProfileData, Runtime, Snapshot};
use hardware_core::{composition::DisplayComposition, plugin_display::PluginAppearance};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use stream_dock_manager_storage::{rusqlite::Connection, Domain};

const DEFAULT_COLOR: &str = "#c61e3200";
const DRAFTS_KEY: &str = "drafts";
const DRAFTS_LIMIT: usize = 16 * 1024 * 1024;

// Check ownership of style
fn owns_style(style: Option<&PluginAppearance>, id: &str) -> bool {
    style.is_some_and(|style| style.plugin_id == id)
}

// Check ownership of layer
fn owns_layer(layer: &hardware_core::composition::DisplayLayer, id: &str) -> bool {
    layer.source.kind == "plugin" && layer.source.plugin_id == id
}

// Clear layers
fn clear_layers(composition: &mut Option<Box<DisplayComposition>>, id: &str) -> bool {
    let Some(canvas) = composition else {
        return false;
    };
    let count = canvas.layers.len();
    canvas.layers.retain(|layer| !owns_layer(layer, id));
    let changed = count != canvas.layers.len();
    if changed && canvas.layers.is_empty() {
        *composition = None;
    }
    changed
}

// Clear binding
fn clear_binding(binding: &mut Binding, id: &str) -> bool {
    if owns_action(&binding.action, id) {
        let changed = Runtime::has_background_override(binding)
            || binding.appearance.is_some()
            || binding.plugin_appearance.is_some()
            || binding.composition.is_some()
            || binding.media != Default::default();
        if changed {
            binding.color = DEFAULT_COLOR.into();
            binding.custom_background = false;
            binding.background_source = None;
            binding.asset = None;
            binding.media = Default::default();
            binding.appearance = None;
            binding.plugin_appearance = None;
            binding.composition = None;
        }
        return changed;
    }
    let changed = owns_style(binding.plugin_appearance.as_deref(), id);
    if changed {
        binding.plugin_appearance = None;
    }
    clear_layers(&mut binding.composition, id) || changed
}

// Check ownership of action
fn owns_action(action: &Action, id: &str) -> bool {
    matches!(action, Action::Plugin { plugin_id, .. } if plugin_id == id)
}

// A macro retains only the action identity needed to recognize a missing dependency
// and recover the step after reinstalling the plugin
fn clear_action_settings(action: &mut Action, id: &str) -> bool {
    if let Action::Plugin {
        plugin_id,
        settings,
        resources,
        ..
    } = action
    {
        if plugin_id == id && (*settings != json!({}) || !resources.is_empty()) {
            *settings = json!({});
            resources.clear();
            return true;
        }
    }
    let mut changed = false;
    action.for_each_child_mut(|child| changed |= clear_action_settings(child, id));
    changed
}

// Clear workspace
fn clear_workspace(data: &mut ProfileData, id: &str, purge: bool) -> bool {
    let mut changed = false;
    for page in data.profiles.iter_mut().flat_map(|p| &mut p.pages) {
        page.bindings.retain(|_, binding| {
            if purge && owns_action(&binding.action, id) {
                changed = true;
                return false;
            }
            changed |= clear_binding(binding, id);
            if purge {
                changed |= clear_action_settings(&mut binding.action, id);
            }
            true
        });
    }
    if purge {
        for saved in &mut data.saved_macros {
            changed |= clear_action_settings(&mut saved.action, id);
        }
    }
    let before = data.styles.len();
    data.styles.retain(|preset| {
        !owns_style(preset.plugin_appearance.as_deref(), id)
            && !preset
                .composition
                .as_ref()
                .is_some_and(|canvas| canvas.layers.iter().any(|layer| owns_layer(layer, id)))
    });
    changed || before != data.styles.len()
}

// Clear draft
fn clear_draft(draft: &mut Draft, id: &str) -> bool {
    let mut changed = draft
        .base
        .as_mut()
        .is_some_and(|binding| clear_binding(binding, id));
    let Some(value) = draft.value.as_object_mut() else {
        return changed;
    };
    if value.get("actionDraft").is_some_and(|draft| {
        draft["action"]["kind"] == "plugin" && draft["action"]["plugin_id"] == id
    }) && value.get("dialActions").is_none_or(Value::is_null)
        && value.get("eventActions").is_none()
    {
        for key in ["asset", "pluginAppearance", "composition"] {
            changed |= value.remove(key).is_some();
        }
        for (key, default) in [
            ("color", json!(DEFAULT_COLOR)),
            ("customBackground", json!(false)),
            (
                "appearance",
                json!(hardware_core::appearance::Appearance::default()),
            ),
            ("appearanceEnabled", json!(false)),
            ("media", json!(crate::media::MediaOptions::default())),
        ] {
            if value.get(key) != Some(&default) {
                value.insert(key.into(), default);
                changed = true;
            }
        }
    } else {
        if value
            .get("pluginAppearance")
            .and_then(|style| style["plugin_id"].as_str())
            == Some(id)
        {
            value.remove("pluginAppearance");
            changed = true;
        }
        if let Some(layers) = value
            .get_mut("composition")
            .and_then(|canvas| canvas.get_mut("layers"))
            .and_then(Value::as_array_mut)
        {
            let before = layers.len();
            layers.retain(|layer| {
                !(layer["source"]["kind"] == "plugin" && layer["source"]["plugin_id"] == id)
            });
            if before != layers.len() {
                changed = true;
                if layers.is_empty() {
                    value.remove("composition");
                }
            }
        }
    }
    changed
}

#[derive(Default)]
pub(crate) struct StyleChanges {
    data: Option<ProfileData>,
    drafts: Option<BTreeMap<String, Draft>>,
}

impl StyleChanges {
    // Clear in
    pub(crate) fn clear_in(tx: &Connection, id: &str, purge: bool) -> Result<Self, String> {
        let mut data = crate::profile_database::load(tx)?.ok_or("Workspace is not initialized")?;
        let mut changed = clear_workspace(&mut data, id, purge);
        changed |= crate::workspace::rewrite_history(tx, &mut data.history, id, purge, |data| {
            clear_workspace(data, id, purge)
        })?;
        let mut drafts: BTreeMap<String, Draft> = stream_dock_manager_storage::read_json(
            tx,
            Domain::LocalPreferences,
            DRAFTS_KEY,
            DRAFTS_LIMIT,
        )?
        .unwrap_or_default();
        let mut drafts_changed = false;
        drafts.retain(|_, draft| {
            if purge
                && draft.value["actionDraft"]["action"]["kind"] == "plugin"
                && draft.value["actionDraft"]["action"]["plugin_id"] == id
                && draft.value.get("dialActions").is_none_or(Value::is_null)
                && draft.value.get("eventActions").is_none()
            {
                drafts_changed = true;
                return false;
            }
            drafts_changed |= clear_draft(draft, id);
            if purge {
                if draft
                    .base
                    .as_ref()
                    .is_some_and(|base| owns_action(&base.action, id))
                {
                    draft.base = None;
                    drafts_changed = true;
                }
                if let Some(base) = &mut draft.base {
                    drafts_changed |= clear_action_settings(&mut base.action, id);
                }
                // Report the purge action draft condition for plugin styles
                fn purge_action_draft(value: &mut Value, id: &str) -> bool {
                    let Ok(mut action) = serde_json::from_value::<Action>(value["action"].clone())
                    else {
                        return false;
                    };
                    let mut owns = false;
                    action.visit(&mut |a| owns |= owns_action(a, id));
                    let changed = clear_action_settings(&mut action, id);
                    if changed {
                        value["action"] = json!(action);
                    }
                    // Incomplete advanced JSON may contain an earlier secret/resource value
                    // Never let recovery restore it after the owner was explicitly purged
                    if owns && !value["rawValue"].is_null() {
                        value["rawValue"] = Value::Null;
                        return true;
                    }
                    changed
                }
                if let Some(action) = draft.value.get_mut("actionDraft") {
                    drafts_changed |= purge_action_draft(action, id);
                }
                for key in ["dialActions", "eventActions"] {
                    if let Some(lanes) = draft.value.get_mut(key).and_then(Value::as_array_mut) {
                        for lane in lanes {
                            drafts_changed |= purge_action_draft(lane, id);
                        }
                    }
                }
            }
            true
        });
        if drafts_changed {
            stream_dock_manager_storage::write_json(
                tx,
                Domain::LocalPreferences,
                DRAFTS_KEY,
                &drafts,
                DRAFTS_LIMIT,
            )?;
        }
        if changed || drafts_changed {
            data.configuration_revision = data
                .configuration_revision
                .checked_add(1)
                .ok_or("Workspace revision limit reached")?;
            data.update_requirements();
            crate::profiles::validate_data(&data)?;
            crate::profile_database::save(tx, &data)?;
        }
        Ok(Self {
            data: (changed || drafts_changed).then_some(data),
            drafts: drafts_changed.then_some(drafts),
        })
    }

    // Publish the current plugin styles result
    pub(crate) fn publish(
        self,
        state: &mut Snapshot,
        drafts: &mut BTreeMap<String, Draft>,
    ) -> bool {
        if let Some(updated) = self.drafts {
            *drafts = updated;
        }
        if let Some(updated) = self.data {
            Runtime::apply_data(state, updated);
            return true;
        }
        false
    }
}

impl Runtime {
    // Reset plugin styles
    pub fn reset_plugin_styles(&self, id: &str) -> Result<Snapshot, String> {
        let mut state = self.state.lock().unwrap();
        let _transaction = self.profiles.transaction.lock().unwrap();
        let mut drafts = self.drafts.lock().unwrap();
        let owner = id.to_owned();
        let changes = self
            .plugins
            .lock()
            .unwrap()
            .reset_appearance_with(id, move |tx| StyleChanges::clear_in(tx, &owner, false))?;
        let changed = changes.publish(&mut state, &mut drafts);
        drop(drafts);
        drop(_transaction);
        drop(state);
        if changed {
            self.emit("profiles", None, None, None);
        }
        Ok(self.plugins_changed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{AppearancePreset, Edit};

    #[test]
    // Verify that resources only nested actions are purged without touching other owners
    fn resources_only_nested_actions_are_purged_without_touching_other_owners() {
        let mut action = Action::Macro {
            actions: vec![
                Action::Plugin {
                    user_desired_state: None,
                    plugin_id: "removed".into(),
                    action_id: "removed.run".into(),
                    settings: json!({}),
                    resources: BTreeMap::from([("file".into(), "shared-resource".into())]),
                },
                Action::Plugin {
                    user_desired_state: None,
                    plugin_id: "retained".into(),
                    action_id: "retained.run".into(),
                    settings: json!({}),
                    resources: BTreeMap::from([("file".into(), "shared-resource".into())]),
                },
            ],
        };
        assert!(clear_action_settings(&mut action, "removed"));
        let json = json!(action);
        assert!(json["actions"][0].get("resources").is_none());
        assert_eq!(json["actions"][1]["resources"]["file"], "shared-resource");
        assert!(!clear_action_settings(&mut action, "removed"));
    }
    #[test]
    // Verify that clears every matching macro child without short circuiting
    fn clears_every_matching_macro_child_without_short_circuiting() {
        let plugin = |id: &str| Action::Plugin {
            user_desired_state: None,
            plugin_id: id.into(),
            action_id: format!("{id}.press"),
            settings: json!({"private": "test-only"}),
            resources: Default::default(),
        };
        let mut action = Action::Macro {
            actions: vec![
                plugin("fixture.styles"),
                Action::Toggle {
                    on: Box::new(plugin("fixture.styles")),
                    off: Box::new(plugin("fixture.styles")),
                },
                plugin("other.plugin"),
                plugin("fixture.styles"),
            ],
        };

        assert!(clear_action_settings(&mut action, "fixture.styles"));
        let serialized = json!(action);
        assert_eq!(serialized["actions"][0]["settings"], json!({}));
        assert_eq!(serialized["actions"][1]["on"]["settings"], json!({}));
        assert_eq!(serialized["actions"][1]["off"]["settings"], json!({}));
        assert_eq!(
            serialized["actions"][2]["settings"],
            json!({"private": "test-only"})
        );
        assert_eq!(serialized["actions"][3]["settings"], json!({}));
        assert!(!clear_action_settings(&mut action, "fixture.styles"));
    }

    #[test]
    // Verify that reset and uninstall scope workspace history drafts and macro dependencies
    fn reset_and_uninstall_scope_workspace_history_drafts_and_macro_dependencies() {
        const ID: &str = "fixture.styles";
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/fixture.styles.sdPlugin");
        let install = || {
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(package.join("runtime.html"), "<title>Fixture</title>").unwrap();
            std::fs::write(
                package.join("manifest.json"),
                json!({"UUID":ID,"Name":"Fixture","Version":"0.1.0","CodePath":"runtime.html",
                    "Actions":[{"UUID":"fixture.styles.press","Name":"Press"}]})
                .to_string(),
            )
            .unwrap();
            crate::test_plugin::modernize(&package);
        };
        install();
        let storage = stream_dock_manager_storage::Storage::open(root.path().to_owned()).unwrap();
        storage
            .write_json(
                Domain::PluginMetadata,
                "appearance",
                &json!({(ID): "dark"}),
                4096,
            )
            .unwrap();
        drop(storage);
        let runtime = Runtime::build(root.path().to_owned(), false).unwrap();
        let action = Action::Plugin {
            user_desired_state: None,
            plugin_id: ID.into(),
            action_id: "fixture.styles.press".into(),
            settings: json!({"private":"test-only"}),
            resources: BTreeMap::from([("file".into(), format!("{}.bin", "a".repeat(64)))]),
        };
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        let unrelated = json!(profile.pages[0].bindings["key-2"]);
        let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
        binding.action = action.clone();
        binding.color = "#345678".into();
        binding.custom_background = true;
        let style = Box::new(PluginAppearance {
            vendor_profile: None,
            plugin_id: ID.into(),
            action_id: "fixture.styles.press".into(),
            elements: vec![],
        });
        binding.plugin_appearance = Some(style.clone());
        runtime.save_draft("fixture", Some(Draft {
            key: "fixture".into(),
            base: Some(binding.clone()),
            value: json!({"actionDraft":{"action":action,"advanced":false,"rawValue":null},"dialActions":null,"color":"#345678"}),
        })).unwrap();
        let mut other = action.clone();
        if let Action::Plugin {
            plugin_id,
            action_id,
            ..
        } = &mut other
        {
            *plugin_id = "other.plugin".into();
            *action_id = "other.plugin.run".into();
        }
        let draft = |action: &Action| json!({"action":action,"advanced":true,"rawValue":"incomplete private input"});
        runtime.save_draft("lanes", Some(Draft { key: "lanes".into(), base: Some(binding.clone()), value: json!({"actionDraft":draft(&action), "dialActions":[draft(&action),draft(&other),draft(&Action::Macro { actions: vec![action.clone()] })], "dialActionIndex":0}) })).unwrap();
        runtime
            .apply_edits(
                snapshot.configuration_revision,
                "Seed plugin styles",
                vec![
                    Edit::SaveProfile { profile },
                    Edit::SaveStyle {
                        style: AppearancePreset {
                            background_source: None,
                            id: "fixture.preset".into(),
                            name: "Fixture preset".into(),
                            appearance: Default::default(),
                            plugin_appearance: Some(style),
                            composition: None,
                            overlay_enabled: None,
                            color: Some("#345678".into()),
                        },
                    },
                    Edit::SaveMacro {
                        value: crate::builtins::SavedMacro {
                            id: "fixture.macro".into(),
                            name: "Fixture macro".into(),
                            action: Action::Macro {
                                actions: vec![action.clone(), Action::Mute],
                            },
                            inputs: vec![],
                        },
                    },
                ],
            )
            .unwrap();
        let reset = runtime.reset_plugin_styles(ID).unwrap();
        let binding = &reset.profiles[0].pages[0].bindings["key-1"];
        assert_eq!(binding.color, DEFAULT_COLOR);
        assert_eq!(json!(binding.action), json!(action));
        assert!(binding.plugin_appearance.is_none());
        assert!(reset.styles.is_empty());
        assert_eq!(reset.plugins[0].appearance, "follow-ui");
        assert_eq!(
            runtime.drafts.lock().unwrap()["fixture"].value["color"],
            DEFAULT_COLOR
        );
        runtime
            .history_action("undo", reset.configuration_revision)
            .unwrap();
        let restored = runtime
            .history_action("redo", runtime.snapshot().configuration_revision)
            .unwrap();
        assert!(restored.styles.is_empty());
        assert_eq!(
            restored.profiles[0].pages[0].bindings["key-1"].color,
            DEFAULT_COLOR
        );

        let mut restyled = restored.profiles[0].clone();
        restyled.pages[0].bindings.get_mut("key-1").unwrap().color = "#456789".into();
        runtime
            .save_profile(restyled, restored.configuration_revision)
            .unwrap();
        runtime.delete_plugin(ID, true).unwrap();
        let kept = runtime.snapshot();
        let binding = &kept.profiles[0].pages[0].bindings["key-1"];
        assert_eq!(binding.color, DEFAULT_COLOR);
        assert_eq!(json!(binding.action), json!(action));
        install();
        runtime.reload_plugins().unwrap();

        let backup = root
            .path()
            .join("plugins/.backups")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(backup.join(ID)).unwrap();
        std::fs::create_dir_all(backup.join("other.plugin")).unwrap();
        runtime.delete_plugin(ID, false).unwrap();
        let snapshot = runtime.snapshot();
        assert!(!package.exists());
        assert!(!backup.join(ID).exists());
        assert!(backup.join("other.plugin").exists());
        assert!(!snapshot.profiles[0].pages[0].bindings.contains_key("key-1"));
        assert_eq!(
            json!(snapshot.profiles[0].pages[0].bindings["key-2"]),
            unrelated
        );
        let drafts = runtime.drafts.lock().unwrap();
        assert_eq!(drafts.len(), 1);
        let value = &drafts["lanes"].value;
        assert!(drafts["lanes"].base.is_none());
        assert!(value["actionDraft"]["action"].get("resources").is_none());
        assert!(value["actionDraft"]["rawValue"].is_null());
        assert!(value["dialActions"][0]["action"].get("resources").is_none());
        assert!(value["dialActions"][2]["action"]["actions"][0]
            .get("resources")
            .is_none());
        assert_eq!(
            value["dialActions"][1]["action"]["resources"]["file"],
            format!("{}.bin", "a".repeat(64))
        );
        drop(drafts);
        let steps = snapshot.saved_macros[0].action.children();
        assert_eq!(json!(steps[0])["settings"], json!({}));
        assert!(json!(steps[0]).get("resources").is_none());
        assert!(matches!(steps[1], Action::Mute));
        assert!(runtime
            .run_saved_macro(None, "fixture.macro", Default::default())
            .unwrap_err()
            .contains("Incompatible macro"));
        runtime
            .history_action("undo", snapshot.configuration_revision)
            .unwrap();
        let restored = runtime
            .history_action("redo", runtime.snapshot().configuration_revision)
            .unwrap();
        assert!(!restored.profiles[0].pages[0].bindings.contains_key("key-1"));
        install();
        runtime.reload_plugins().unwrap();
        let host = runtime.plugins.lock().unwrap();
        crate::builtins::require_macro_plugins(
            &restored.saved_macros[0].action,
            |_| None,
            |plugin, action| host.declares_action(plugin, action),
        )
        .unwrap();
        drop(host);
        runtime.shutdown();
    }
}
