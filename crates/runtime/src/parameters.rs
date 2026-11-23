//! Portable definitions; installation and device values never enter profile archives
use crate::{actions::Action, Profile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileParameter {
    pub name: String,
    pub label: String,
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub sensitive: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ParameterOverrides {
    pub installation: BTreeMap<String, BTreeMap<String, Value>>,
    pub devices: BTreeMap<String, BTreeMap<String, BTreeMap<String, Value>>>,
}
// Report the valid type condition for parameters
pub(crate) fn valid_type(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        _ => false,
    }
}
impl ProfileParameter {
    // Validate the supplied parameters value
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty()
            || self.name.len() > 80
            || !self
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || self.label.is_empty()
            || !["string", "number", "boolean"].contains(&self.r#type.as_str())
            || self
                .default
                .as_ref()
                .is_some_and(|v| !valid_type(&self.r#type, v))
            || (self.sensitive && (self.r#type != "string" || self.default.is_some()))
        {
            return Err("Invalid profile parameter definition".into());
        }
        Ok(())
    }
}
// Extract the identifier from a complete supported parameter reference
pub fn reference(value: &str) -> Option<&str> {
    value
        .strip_prefix("${parameter.")
        .and_then(|s| s.strip_suffix('}'))
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}
// Resolve the supplied parameters request
pub(crate) fn resolve(
    profile: &Profile,
    overrides: &ParameterOverrides,
    device: &str,
    name: &str,
) -> Result<Value, String> {
    let definition = profile
        .parameters
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Profile {} has no parameter {name}", profile.name))?;
    let value = overrides
        .devices
        .get(device)
        .and_then(|v| v.get(&profile.id))
        .and_then(|v| v.get(name))
        .or_else(|| {
            overrides
                .installation
                .get(&profile.id)
                .and_then(|v| v.get(name))
        })
        .or(definition.default.as_ref());
    let Some(value) = value else {
        return Err(format!(
            "Set parameter ‘{}’ for profile ‘{}’ in Parameters before running this action",
            definition.label, profile.name
        ));
    };
    if !valid_type(&definition.r#type, value) {
        return Err(format!(
            "Parameter {} requires a {}",
            definition.label, definition.r#type
        ));
    }
    Ok(value.clone())
}
// Resolve action
pub fn resolve_action(
    action: &Action,
    profile: &Profile,
    overrides: &ParameterOverrides,
    device: &str,
    plugin_fields: &impl Fn(&str, &str) -> Vec<String>,
) -> Result<Action, String> {
    for parameter in profile
        .parameters
        .iter()
        .filter(|parameter| parameter.required)
    {
        resolve(profile, overrides, device, &parameter.name)?;
    }
    resolve_action_values(action, plugin_fields, &|name| {
        resolve(profile, overrides, device, name)
    })
}

// Resolve action values
pub(crate) fn resolve_action_values(
    action: &Action,
    plugin_fields: &impl Fn(&str, &str) -> Vec<String>,
    resolve: &impl Fn(&str) -> Result<Value, String>,
) -> Result<Action, String> {
    let action = crate::substitution::action(
        action,
        &mut |value| {
            let Some(name) = reference(value) else {
                return Ok(None);
            };
            let resolved = resolve(name)?;
            Ok(Some(
                resolved
                    .as_str()
                    .ok_or_else(|| {
                        format!("Parameter {name} requires a string for this action field")
                    })?
                    .to_owned(),
            ))
        },
        &mut |plugin_id, action_id, settings| {
            let allowed = plugin_fields(plugin_id, action_id);
            if let Some(settings) = settings.as_object_mut() {
                for (key, value) in settings {
                    if allowed.contains(key) {
                        let name = match &*value {
                            Value::String(text) => reference(text),
                            Value::Object(map) if map.len() == 1 => {
                                map.get("$parameter").and_then(Value::as_str)
                            }
                            _ => None,
                        };
                        if let Some(name) = name {
                            *value = resolve(name)?;
                        }
                    } else if value
                        .as_object()
                        .is_some_and(|m| m.contains_key("$parameter"))
                        || value.as_str().and_then(reference).is_some()
                    {
                        return Err(format!(
                            "Plugin does not declare parameter support for setting {key}"
                        ));
                    }
                }
            }
            Ok(())
        },
    )?;
    action.validate(0)?;
    Ok(action)
}
#[derive(Clone)]
pub(crate) struct FrozenParameters {
    pub(crate) load_error: Option<String>,
    pub profiles: std::sync::Arc<[Profile]>,
    pub overrides: ParameterOverrides,
}
impl FrozenParameters {
    // Initialize parameters state from the supplied inputs
    pub(crate) fn new(
        profiles: std::sync::Arc<[Profile]>,
        overrides: Result<ParameterOverrides, String>,
    ) -> Self {
        Self {
            profiles,
            load_error: overrides.as_ref().err().cloned(),
            overrides: overrides.unwrap_or_default(),
        }
    }
}
impl crate::Runtime {
    // List the parameter-enabled fields declared by the installed plugin action
    pub(crate) fn plugin_parameter_fields(&self, id: &str, action: &str) -> Vec<String> {
        self.plugins
            .lock()
            .unwrap()
            .list()
            .iter()
            .find(|p| p.id == id)
            .and_then(|p| p.capabilities.as_ref())
            .and_then(|c| c.actions.get(action))
            .map(|a| a.parameter_fields.clone())
            .unwrap_or_default()
    }
    // Resolve parameter
    pub(crate) fn resolve_parameter(
        &self,
        name: &str,
        device: &str,
        scope: &str,
    ) -> Result<Value, String> {
        let state = self.snapshot();
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == scope.split(':').next().unwrap_or(""))
            .ok_or("Parameter profile is missing")?;
        let mut overrides = self.stored_parameter_overrides()?;
        self.resolve_sensitive_override(profile, &mut overrides, device, name)?;
        resolve(profile, &overrides, device, name)
    }
    // Read persisted parameter overrides or use empty defaults
    pub(crate) fn stored_parameter_overrides(&self) -> Result<ParameterOverrides, String> {
        Ok(self
            .profiles
            .read_preference(crate::profile_database::LocalPreference::ParameterOverrides)?
            .unwrap_or_default())
    }
    // Filter overrides to current parameters and hide sensitive values that are not credential references
    pub fn parameter_overrides(&self) -> Result<ParameterOverrides, String> {
        let mut value = self.stored_parameter_overrides()?;
        let snapshot = self.snapshot();
        let retain = |profiles: &mut BTreeMap<String, BTreeMap<String, Value>>| {
            profiles.retain(|id, values| {
                let Some(profile) = snapshot.profiles.iter().find(|profile| profile.id == *id)
                else {
                    return false;
                };
                values.retain(|name, _| profile.parameters.iter().any(|p| p.name == *name));
                !values.is_empty()
            });
        };
        retain(&mut value.installation);
        for profiles in value.devices.values_mut() {
            retain(profiles);
        }
        for (id, values) in value.installation.iter_mut().chain(
            value
                .devices
                .values_mut()
                .flat_map(|profiles| profiles.iter_mut()),
        ) {
            if let Some(profile) = snapshot.profiles.iter().find(|profile| profile.id == *id) {
                for parameter in profile
                    .parameters
                    .iter()
                    .filter(|parameter| parameter.sensitive)
                {
                    if let Some(value) = values.get_mut(&parameter.name) {
                        if crate::credentials::reference(value).is_none() {
                            *value = Value::Null;
                        }
                    }
                }
            }
        }
        Ok(value)
    }
    // Save parameter overrides
    pub fn save_parameter_overrides(&self, mut value: ParameterOverrides) -> Result<(), String> {
        let _change = self.credentials.changes.lock().unwrap();
        let state = self.snapshot();
        self.preserve_credentials(&mut value, &state)?;
        for (id, values) in value
            .installation
            .iter()
            .chain(value.devices.values().flat_map(|v| v.iter()))
        {
            let profile = state
                .profiles
                .iter()
                .find(|p| p.id == *id)
                .ok_or("Unknown parameter profile")?;
            for (name, value) in values {
                let p = profile
                    .parameters
                    .iter()
                    .find(|p| p.name == *name)
                    .ok_or("Unknown parameter override")?;
                if !p.sensitive && !valid_type(&p.r#type, value) {
                    return Err("Parameter override type mismatch".into());
                }
            }
        }
        let previous = self.stored_parameter_overrides()?;
        if previous.installation == value.installation && previous.devices == value.devices {
            return Ok(());
        }
        self.profiles.write_parameter_overrides(&value)?;
        self.label_overrides.lock().unwrap().take();
        let mut changed = false;
        for device in state.devices.iter().filter(|d| d.connected) {
            let session = state.session(&device.id);
            let Some(profile) = state.profiles.iter().find(|p| p.id == session.profile_id) else {
                continue;
            };
            let (_, layers) = self.builtin_state.device_overrides(&state, &device.id);
            let preview = self.hardware_preview(&device.id);
            let draft = preview.as_ref().map(|(_, d)| d);
            let page = profile.pages.iter().find(|p| p.id == session.page_id);
            let affects = device
                .manifest
                .controls
                .iter()
                .filter(|c| c.image_slot.is_some() || c.feedback_surface.is_some())
                .any(|c| {
                    let active = if let Some(binding) = draft.and_then(|d| d.bindings.get(&c.id)) {
                        Some((profile, binding))
                    } else if let Some(layer) = layers.get(&c.id) {
                        state
                            .profiles
                            .iter()
                            .find(|p| p.id == layer.profile_id)
                            .map(|p| (p, &layer.binding))
                    } else {
                        page.and_then(|p| p.bindings.get(&c.id))
                            .map(|b| (profile, b))
                    };
                    active.is_some_and(|(profile, b)| {
                        b.live_label.iter().any(|s| {
                            let crate::live_label::LabelSegment::Parameter { name, .. } = s else {
                                return false;
                            };
                            crate::live_label::parameter(profile, &previous, &device.id, name)
                                != crate::live_label::parameter(profile, &value, &device.id, name)
                        })
                    })
                });
            if affects {
                self.emit("live-feedback", Some(device.id.clone()), None, None);
                changed = true;
            }
        }
        if changed
            && !self
                .device_wake_pending
                .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            let _ = self.device_commands.send(crate::devices::Request::Wake);
        }
        Ok(())
    }
    // Resolve parameters
    pub(crate) fn resolve_parameters(
        &self,
        action: &Action,
        device: &str,
        scope: &str,
    ) -> Result<Action, String> {
        self.resolve_frozen_parameters(
            action,
            device,
            scope,
            &self.freeze_parameters(&self.snapshot()),
        )
    }
    // Resolve execution parameters before the action begins
    pub(crate) fn freeze_parameters(&self, state: &crate::Snapshot) -> FrozenParameters {
        FrozenParameters::new(
            state.profiles.clone().into(),
            self.stored_parameter_overrides(),
        )
    }
    // Resolve frozen parameters
    pub(crate) fn resolve_frozen_parameters(
        &self,
        action: &Action,
        device: &str,
        scope: &str,
        frozen: &FrozenParameters,
    ) -> Result<Action, String> {
        if let Some(error) = &frozen.load_error {
            return Err(error.clone());
        }
        let profile_id = scope.split(':').next().unwrap_or("");
        let Some(profile) = frozen.profiles.iter().find(|p| p.id == profile_id) else {
            return Ok(action.clone());
        };
        // References can outlive their definitions, including after deleting the
        // final parameter. Resolve them so missing values cannot reach an action
        // No reference can resolve without a definition, so ordinary actions need
        // neither the local override file nor the plugin capability inventory
        if profile.parameters.is_empty() {
            return resolve_action(
                action,
                profile,
                &ParameterOverrides::default(),
                device,
                &|_, _| vec![],
            );
        }
        let mut overrides = frozen.overrides.clone();
        let plugins = self.plugins.lock().unwrap().list();
        let names = std::cell::RefCell::new(std::collections::BTreeSet::new());
        action.try_visit_fields(
            &mut |role, value| {
                if role.supports_parameter() {
                    if let Some(name) = reference(value) {
                        names.borrow_mut().insert(name.to_owned());
                    }
                }
                Ok::<_, String>(())
            },
            &mut |id, action, settings| {
                let allowed = plugins
                    .iter()
                    .find(|p| p.id == id)
                    .and_then(|p| p.capabilities.as_ref())
                    .and_then(|c| c.actions.get(action))
                    .map(|a| a.parameter_fields.as_slice())
                    .unwrap_or_default();
                if let Some(settings) = settings.as_object() {
                    for (key, value) in settings {
                        if allowed.contains(key) {
                            if let Some(name) = value.as_str().and_then(reference).or_else(|| {
                                value
                                    .as_object()
                                    .filter(|m| m.len() == 1)
                                    .and_then(|m| m.get("$parameter"))
                                    .and_then(Value::as_str)
                            }) {
                                names.borrow_mut().insert(name.to_owned());
                            }
                        }
                    }
                }
                Ok(())
            },
        )?;
        let mut names = names.into_inner();
        names.extend(
            profile
                .parameters
                .iter()
                .filter(|parameter| parameter.required)
                .map(|parameter| parameter.name.clone()),
        );
        for name in names {
            self.resolve_sensitive_override(profile, &mut overrides, device, &name)?;
        }
        resolve_action(action, profile, &overrides, device, &|id, action| {
            plugins
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.capabilities.as_ref())
                .and_then(|c| c.actions.get(action))
                .map(|a| a.parameter_fields.clone())
                .unwrap_or_default()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    // Verify that frozen constructor preserves each sample and load error
    fn frozen_constructor_preserves_each_sample_and_load_error() {
        let profiles: std::sync::Arc<[Profile]> = Vec::new().into();
        let mut overrides = ParameterOverrides::default();
        overrides.installation.insert(
            "profile".into(),
            BTreeMap::from([("value".into(), serde_json::json!(1))]),
        );
        let first = FrozenParameters::new(profiles.clone(), Ok(overrides.clone()));
        overrides
            .installation
            .get_mut("profile")
            .unwrap()
            .insert("value".into(), serde_json::json!(2));
        let second = FrozenParameters::new(profiles.clone(), Ok(overrides));
        assert_eq!(first.overrides.installation["profile"]["value"], 1);
        assert_eq!(second.overrides.installation["profile"]["value"], 2);
        assert!(std::sync::Arc::ptr_eq(&first.profiles, &second.profiles));
        let failed = FrozenParameters::new(profiles, Err("unreadable".into()));
        assert_eq!(failed.load_error.as_deref(), Some("unreadable"));
        assert!(failed.overrides.installation.is_empty());
    }

    #[test]
    // Verify that typed fields resolve nested actions and declared plugin values only
    fn typed_fields_resolve_nested_actions_and_declared_plugin_values_only() {
        let mut profile = crate::ProfileData::default().profiles.remove(0);
        profile.parameters = vec![
            ProfileParameter {
                sensitive: false,
                name: "value".into(),
                label: "Value".into(),
                r#type: "string".into(),
                required: false,
                default: Some(json!("resolved")),
            },
            ProfileParameter {
                sensitive: false,
                name: "enabled".into(),
                label: "Enabled".into(),
                r#type: "boolean".into(),
                required: false,
                default: Some(json!(true)),
            },
        ];
        let action = Action::Toggle {
            on: Box::new(Action::Builtin {
                action: Box::new(crate::builtins::BuiltinAction::RepeatActions {
                    count: 1,
                    interval_ms: 0,
                    children: vec![Action::Command {
                        program: "${parameter.value}".into(),
                        args: vec!["${parameter.value}".into()],
                    }],
                }),
            }),
            off: Box::new(Action::Plugin {
                user_desired_state: None,
                plugin_id: "plugin".into(),
                action_id: "action".into(),
                settings: json!({
                    "enabled": {"$parameter":"enabled"},
                    "opaque": {"children":[{"kind":"open_path","path":"${parameter.value}"}]}
                }),
                resources: Default::default(),
            }),
        };
        let original = serde_json::to_value(&action).unwrap();
        let resolved = resolve_action(
            &action,
            &profile,
            &ParameterOverrides::default(),
            "device",
            &|id, action| {
                assert_eq!((id, action), ("plugin", "action"));
                vec!["enabled".into()]
            },
        )
        .unwrap();
        let resolved = serde_json::to_value(resolved).unwrap();
        assert_eq!(
            resolved["on"]["action"]["children"][0]["program"],
            "resolved"
        );
        assert_eq!(
            resolved["on"]["action"]["children"][0]["args"][0],
            "resolved"
        );
        assert_eq!(resolved["off"]["settings"]["enabled"], true);
        assert_eq!(
            resolved["off"]["settings"]["opaque"],
            original["off"]["settings"]["opaque"]
        );
        assert_eq!(serde_json::to_value(&action).unwrap(), original);
        assert!(resolve_action(
            &action,
            &profile,
            &ParameterOverrides::default(),
            "device",
            &|_, _| vec![]
        )
        .unwrap_err()
        .contains("does not declare"));
        assert!(resolve_action(
            &Action::OpenPath {
                path: "${parameter.enabled}".into()
            },
            &profile,
            &ParameterOverrides::default(),
            "device",
            &|_, _| vec![]
        )
        .unwrap_err()
        .contains("requires a string"));
    }
    #[test]
    // Verify that missing definitions reject references before execution
    fn missing_definitions_reject_references_before_execution() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        assert!(runtime.snapshot().profiles[0].parameters.is_empty());
        let action = Action::Macro {
            actions: vec![Action::OpenUrl {
                url: "${parameter.site}".into(),
            }],
        };
        let error = runtime
            .resolve_parameters(&action, "device", "default:main")
            .unwrap_err();
        assert!(error.contains("has no parameter site"), "{error}");
        let plugin = Action::Plugin {
            user_desired_state: None,
            plugin_id: "example".into(),
            action_id: "example.action".into(),
            settings: json!({"site":{"$parameter":"site"}}),
            resources: Default::default(),
        };
        assert!(runtime
            .resolve_parameters(&plugin, "device", "default:main")
            .is_err());
        assert!(runtime
            .resolve_parameters(
                &Action::OpenUrl {
                    url: "https://example.com".into(),
                },
                "device",
                "default:main",
            )
            .is_ok());
        runtime.shutdown();
    }
    #[test]
    // Verify that overrides are typed and never rewrite actions
    fn overrides_are_typed_and_never_rewrite_actions() {
        let mut profile = crate::ProfileData::default().profiles.remove(0);
        profile.parameters = vec![ProfileParameter {
            sensitive: false,
            name: "site".into(),
            label: "Site URL".into(),
            r#type: "string".into(),
            required: true,
            default: Some(json!("https://default.example")),
        }];
        let action = Action::OpenUrl {
            url: "${parameter.site}".into(),
        };
        let mut overrides = ParameterOverrides::default();
        overrides.installation.insert(
            profile.id.clone(),
            BTreeMap::from([("site".into(), json!("https://local.example"))]),
        );
        overrides.devices.insert(
            "device".into(),
            BTreeMap::from([(
                profile.id.clone(),
                BTreeMap::from([("site".into(), json!("https://device.example"))]),
            )]),
        );
        let result =
            resolve_action(&action, &profile, &overrides, "device", &|_, _| vec![]).unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap()["url"],
            "https://device.example"
        );
        assert_eq!(
            serde_json::to_value(action).unwrap()["url"],
            "${parameter.site}"
        );
        profile.parameters[0].default = None;
        assert!(resolve_action(
            &Action::OpenUrl {
                url: "${parameter.site}".into()
            },
            &profile,
            &ParameterOverrides::default(),
            "device",
            &|_, _| vec![]
        )
        .unwrap_err()
        .contains("Site URL"));
    }
    #[test]
    #[ignore = "Requires isolated host loopback sockets; no native effects"]
    // Verify that accepted runs freeze definitions and ordinary overrides
    fn accepted_runs_freeze_definitions_and_ordinary_overrides() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.parameters = vec![ProfileParameter {
            name: "site".into(),
            label: "Site".into(),
            r#type: "string".into(),
            required: true,
            default: Some(json!("https://before.example")),
            sensitive: false,
        }];
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        let mut overrides = ParameterOverrides::default();
        overrides.installation.insert(
            "default".into(),
            BTreeMap::from([("site".into(), json!("https://frozen.example"))]),
        );
        runtime.save_parameter_overrides(overrides.clone()).unwrap();
        let frozen = runtime.freeze_parameters(&runtime.snapshot());
        overrides
            .installation
            .get_mut("default")
            .unwrap()
            .insert("site".into(), json!("https://later.example"));
        runtime.save_parameter_overrides(overrides).unwrap();
        let action = Action::OpenUrl {
            url: "${parameter.site}".into(),
        };
        let frozen_action = runtime
            .resolve_frozen_parameters(&action, "fixture", "default:main", &frozen)
            .unwrap();
        assert_eq!(
            serde_json::to_value(frozen_action).unwrap()["url"],
            "https://frozen.example"
        );
        assert_eq!(
            serde_json::to_value(
                runtime
                    .resolve_parameters(&action, "fixture", "default:main")
                    .unwrap()
            )
            .unwrap()["url"],
            "https://later.example"
        );
        runtime.state.lock().unwrap().profiles[0].parameters.clear();
        assert!(runtime
            .resolve_frozen_parameters(&action, "fixture", "default:main", &frozen)
            .is_ok());
        assert!(runtime
            .resolve_parameters(&action, "fixture", "default:main")
            .is_err());
        runtime.shutdown();
    }
}
