//! Resolve authored label segments without retaining the displayed values in a profile.
use crate::Binding;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LabelSegment {
    Literal { text: String },
    Parameter { name: String, fallback: String },
    MacroState { fallback: String },
    Display { key: String, fallback: String },
}

pub(super) fn validate(segments: &[LabelSegment]) -> Result<(), String> {
    if segments.len() > 16 {
        return Err("A live label supports up to 16 segments.".into());
    }
    for segment in segments {
        let text = match segment {
            LabelSegment::Literal { text } => text,
            LabelSegment::MacroState { fallback } => fallback,
            LabelSegment::Parameter {
                name: key,
                fallback,
            }
            | LabelSegment::Display { key, fallback } => {
                if key.is_empty() || key.len() > 160 {
                    return Err("Choose a valid live label field.".into());
                }
                fallback
            }
        };
        if text.chars().count() > 160 {
            return Err("Live label text exceeds 160 characters.".into());
        }
    }
    Ok(())
}

pub(super) fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.chars().take(160).collect()),
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

pub(super) fn resolve(
    binding: &Binding,
    parameter: impl Fn(&str) -> Option<String>,
    macro_state: impl Fn() -> String,
    visual: Option<&Value>,
) -> Option<Binding> {
    if binding.live_label.is_empty() {
        return None;
    }
    let mut result = String::new();
    for segment in &binding.live_label {
        let value = match segment {
            LabelSegment::Literal { text } => text.clone(),
            LabelSegment::MacroState { fallback } => {
                let state = macro_state();
                if state.is_empty() {
                    fallback.clone()
                } else {
                    state
                }
            }
            LabelSegment::Parameter { name, fallback } => {
                parameter(name).unwrap_or_else(|| fallback.clone())
            }
            LabelSegment::Display { key, fallback } => visual
                .and_then(|v| hardware_core::plugin_display::scalar_field(v, key))
                .as_ref()
                .and_then(scalar)
                .unwrap_or_else(|| fallback.clone()),
        };
        result.extend(value.chars().take(160 - result.chars().count()));
        if result.chars().count() == 160 {
            break;
        }
    }
    let mut rendered = binding.clone();
    if !result.is_empty() {
        rendered.title = result;
    }
    Some(rendered)
}

fn is_template(text: &str) -> bool {
    text.contains("${vault.")
}

pub(super) fn authored_segment(segment: &LabelSegment) -> &str {
    match segment {
        LabelSegment::Literal { text } => text,
        LabelSegment::Parameter { fallback, .. }
        | LabelSegment::MacroState { fallback }
        | LabelSegment::Display { fallback, .. } => fallback,
    }
}

pub(super) fn has_vault_title(binding: &Binding) -> bool {
    is_template(&binding.title)
        || binding
            .live_label
            .iter()
            .any(|s| is_template(authored_segment(s)))
}

pub(super) fn has_vault_layer(binding: &Binding) -> bool {
    binding.composition.as_ref().is_some_and(|composition| {
        composition
            .layers
            .iter()
            .any(|layer| layer.source.kind == "text" && is_template(&layer.text))
    })
}

pub(super) fn has_vault_text(binding: &Binding) -> bool {
    has_vault_title(binding) || has_vault_layer(binding)
}

impl crate::Runtime {
    /// Resolve the render copy, not the editor's authored values. Plugin values, profile
    /// parameter values and runtime feedback are data; never interpret them as templates.
    pub(super) fn display_label_binding(
        &self,
        binding: &Binding,
        parameter: impl Fn(&str) -> Option<String>,
        macro_state: impl Fn() -> String,
        visual: Option<&Value>,
    ) -> Option<Binding> {
        let layer_templates = has_vault_layer(binding);
        if !has_vault_title(binding) && !layer_templates {
            return resolve(binding, parameter, macro_state, visual);
        }
        let rendered = self.vault.with_display_text(|expand| {
            let mut rendered = binding.clone();
            if is_template(&rendered.title) {
                rendered.title = expand(&rendered.title, 640).chars().take(160).collect();
            }
            if layer_templates {
                for layer in &mut rendered.composition.as_mut().unwrap().layers {
                    if layer.source.kind == "text" && is_template(&layer.text) {
                        layer.text = expand(&layer.text, 4096);
                    }
                }
            }
            for segment in &mut rendered.live_label {
                let text = match segment {
                    LabelSegment::Literal { text } => text,
                    LabelSegment::Parameter { fallback, .. }
                    | LabelSegment::MacroState { fallback }
                    | LabelSegment::Display { fallback, .. } => fallback,
                };
                if is_template(text) {
                    *text = expand(text, 640);
                }
            }
            rendered
        });
        Some(resolve(&rendered, parameter, macro_state, visual).unwrap_or(rendered))
    }
}

pub(super) fn parameter(
    profile: &crate::Profile,
    overrides: &crate::parameters::ParameterOverrides,
    device: &str,
    name: &str,
) -> Option<String> {
    if !profile
        .parameters
        .iter()
        .any(|p| p.name == name && !p.sensitive)
    {
        return None;
    }
    crate::parameters::resolve(profile, overrides, device, name)
        .ok()
        .as_ref()
        .and_then(scalar)
}

/// Scene construction may run at animation speed. Read SQLite once per configuration or
/// store generation, and retire the cached snapshot when parameter preferences change.
pub(super) struct CachedOverrides {
    revision: u64,
    generation: u64,
    value: std::sync::Arc<crate::parameters::ParameterOverrides>,
}
impl crate::Runtime {
    pub(super) fn label_overrides(
        &self,
        revision: u64,
    ) -> Result<std::sync::Arc<crate::parameters::ParameterOverrides>, String> {
        let generation = self.profiles.storage().generation();
        let mut cache = self.label_overrides.lock().unwrap();
        if let Some(entry) = cache
            .as_ref()
            .filter(|c| c.revision == revision && c.generation == generation)
        {
            return Ok(entry.value.clone());
        }
        let value = std::sync::Arc::new(self.stored_parameter_overrides()?);
        *cache = Some(CachedOverrides {
            revision,
            generation,
            value: value.clone(),
        });
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn scalar_labels_preserve_false_zero_and_literals_and_bound_unicode() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::ProfileStore::open(root.path().into()).unwrap();
        let data = store.load().unwrap();
        let profile = &data.profiles[0];
        let mut b = profile.pages[0].bindings["key-1"].clone();
        b.title = "{unchanged}".into();
        assert!(resolve(&b, |_| None, String::new, None).is_none());
        b.live_label = vec![
            LabelSegment::Parameter {
                name: "a".into(),
                fallback: "missing".into(),
            },
            LabelSegment::Literal { text: " / ".into() },
            LabelSegment::Parameter {
                name: "b".into(),
                fallback: "missing".into(),
            },
        ];
        let zero = json!(0);
        let resolved = resolve(
            &b,
            |n| scalar(if n == "a" { &Value::Bool(false) } else { &zero }),
            String::new,
            None,
        )
        .unwrap();
        assert_eq!(resolved.title, "false / 0");
        assert_eq!(b.title, "{unchanged}");
        assert_eq!(
            resolve(&b, |_| None, String::new, None).unwrap().title,
            "missing / missing"
        );
        b.live_label = vec![
            LabelSegment::Literal {
                text: "界".repeat(150),
            },
            LabelSegment::Literal {
                text: "語".repeat(30),
            },
        ];
        let text = resolve(&b, |_| None, String::new, None).unwrap().title;
        assert_eq!(text.chars().count(), 160);
        assert!(text.ends_with("語"));
        assert!(scalar(&json!({"value":"private"})).is_none());
    }
}
