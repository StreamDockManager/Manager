//! Lexical saved-macro inputs. Assignments are portable; evaluated values are run-local
use crate::{actions::Action, builtins::SavedMacro};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MacroInput {
    pub name: String,
    pub label: String,
    pub r#type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MacroArgument {
    Literal { value: Value },
    Parameter { name: String },
    Input { name: String },
    Output { name: String },
}

pub type Arguments = BTreeMap<String, MacroArgument>;
pub type InputValues = BTreeMap<String, Value>;

// Report the valid name condition for macro inputs
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 80
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
// Report the valid type condition for macro inputs
pub fn valid_type(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value
            .as_str()
            .is_some_and(|value| value.len() <= 256 * 1024),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        _ => false,
    }
}
// Recognize an exact validated macro-input template reference
pub fn reference(value: &str) -> Option<&str> {
    value
        .strip_prefix("${input.")
        .and_then(|s| s.strip_suffix('}'))
        .filter(|s| valid_name(s))
}
// Validate definitions
pub fn validate_definitions(inputs: &[MacroInput]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    if inputs.len() > 64 {
        return Err("A macro supports up to 64 inputs".into());
    }
    for input in inputs {
        if !valid_name(&input.name)
            || !names.insert(&input.name)
            || input.label.trim().is_empty()
            || input.label.len() > 256
            || !["string", "number", "boolean"].contains(&input.r#type.as_str())
            || input
                .default
                .as_ref()
                .is_some_and(|value| !valid_type(&input.r#type, value))
        {
            return Err("Invalid or duplicate macro input definition".into());
        }
    }
    Ok(())
}
// Validate assignments
pub fn validate_assignments(arguments: &Arguments) -> Result<(), String> {
    if arguments.len() > 64 {
        return Err("A macro call supports up to 64 arguments".into());
    }
    for (name, argument) in arguments {
        if !valid_name(name) {
            return Err("Invalid macro argument name".into());
        }
        match argument {
            MacroArgument::Literal { value }
                if !(valid_type("string", value) || value.is_boolean() || value.is_number()) =>
            {
                return Err("Macro arguments must be text, numbers or booleans".into())
            }
            MacroArgument::Parameter { name } | MacroArgument::Input { name }
                if !valid_name(name) =>
            {
                return Err("Invalid macro argument reference".into())
            }
            MacroArgument::Output { name }
                if name.is_empty()
                    || name.len() > 160
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.') =>
            {
                return Err("Invalid run output reference".into())
            }
            _ => {}
        }
    }
    Ok(())
}

// Bind the selected macro inputs target
pub fn bind(
    inputs: &[MacroInput],
    arguments: &Arguments,
    caller: &InputValues,
    outputs: &InputValues,
    mut parameter: impl FnMut(&str) -> Result<Value, String>,
) -> Result<InputValues, String> {
    validate_definitions(inputs)?;
    validate_assignments(arguments)?;
    if arguments
        .keys()
        .any(|name| !inputs.iter().any(|input| input.name == *name))
    {
        return Err("Macro call assigns an unknown input".into());
    }
    let mut values = BTreeMap::new();
    for input in inputs {
        let value = match arguments.get(&input.name) {
            Some(MacroArgument::Literal { value }) => Some(value.clone()),
            Some(MacroArgument::Parameter { name }) => Some(parameter(name)?),
            Some(MacroArgument::Input { name }) => Some(
                caller
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("Caller input {name} is missing"))?,
            ),
            Some(MacroArgument::Output { name }) => Some(
                outputs
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("Prior run output {name} is missing"))?,
            ),
            None => input.default.clone(),
        };
        if let Some(value) = value {
            if !valid_type(&input.r#type, &value) {
                return Err(format!(
                    "Macro input {} requires a {}",
                    input.label, input.r#type
                ));
            }
            values.insert(input.name.clone(), value);
        } else if input.required {
            return Err(format!(
                "Set required macro input {} before running this call",
                input.label
            ));
        }
    }
    Ok(values)
}

// Resolve action
pub fn resolve_action(
    action: &Action,
    values: &InputValues,
    fields: &impl Fn(&str, &str) -> Vec<String>,
) -> Result<Action, String> {
    let action = crate::substitution::action(
        action,
        &mut |value| {
            let Some(name) = reference(value) else {
                return Ok(None);
            };
            let resolved = values
                .get(name)
                .ok_or_else(|| format!("Macro input {name} is missing"))?;
            Ok(Some(
                resolved
                    .as_str()
                    .ok_or_else(|| {
                        format!("Macro input {name} requires text for this action field")
                    })?
                    .to_owned(),
            ))
        },
        &mut |id, action_id, settings| {
            let supported = fields(id, action_id);
            if let Some(settings) = settings.as_object_mut() {
                for (key, value) in settings {
                    let name = value
                        .as_str()
                        .and_then(reference)
                        .map(str::to_owned)
                        .or_else(|| {
                            value
                                .as_object()
                                .filter(|map| map.len() == 1)
                                .and_then(|map| map.get("$input"))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        });
                    if let Some(name) = name {
                        if !supported.contains(key) {
                            return Err(format!(
                                "Plugin does not declare input support for setting {key}"
                            ));
                        }
                        *value = values
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| format!("Macro input {name} is missing"))?;
                    }
                }
            }
            Ok(())
        },
    )?;
    Ok(action)
}

// Validate call
pub fn validate_call(
    target: &SavedMacro,
    arguments: &Arguments,
    caller: &[MacroInput],
) -> Result<(), String> {
    validate_assignments(arguments)?;
    if arguments
        .keys()
        .any(|name| !target.inputs.iter().any(|input| input.name == *name))
    {
        return Err(format!(
            "Macro {} has an unknown argument assignment",
            target.name
        ));
    }
    for input in &target.inputs {
        match arguments.get(&input.name) {
            None if input.required && input.default.is_none() => {
                return Err(format!(
                    "Macro {} requires input {}",
                    target.name, input.label
                ))
            }
            Some(MacroArgument::Literal { value }) if !valid_type(&input.r#type, value) => {
                return Err(format!(
                    "Macro input {} requires a {}",
                    input.label, input.r#type
                ))
            }
            Some(MacroArgument::Input { name }) => {
                let source = caller
                    .iter()
                    .find(|source| source.name == *name)
                    .ok_or("Forwarded caller input is not declared")?;
                if source.r#type != input.r#type {
                    return Err("Forwarded macro input type mismatch".into());
                }
            }
            _ => {}
        }
    }
    Ok(())
}

// Validate action calls
pub fn validate_action_calls(
    action: &Action,
    macros: &[SavedMacro],
    inputs: &[MacroInput],
) -> Result<(), String> {
    let mut result = Ok(());
    action.visit(&mut |node| {
        if result.is_err() {
            return;
        }
        if let Action::Builtin { action } = node {
            if let crate::builtins::BuiltinAction::SavedMacro {
                macro_id,
                arguments,
            } = action.as_ref()
            {
                if let Some(target) = macros.iter().find(|target| target.id == *macro_id) {
                    result = validate_call(target, arguments, inputs);
                }
            }
        }
    });
    result?;
    action.try_visit_fields(
        &mut |role, value| {
            if role.supports_parameter() {
                if let Some(name) = reference(value) {
                    let input =
                        inputs
                            .iter()
                            .find(|input| input.name == name)
                            .ok_or_else(|| {
                                format!("Macro input {name} is not declared in this scope")
                            })?;
                    if input.r#type != "string" {
                        return Err(format!(
                            "Macro input {name} requires text for this action field"
                        ));
                    }
                }
            }
            Ok(())
        },
        &mut |_, _, settings| {
            if let Some(settings) = settings.as_object() {
                for value in settings.values() {
                    let name = value.as_str().and_then(reference).or_else(|| {
                        value
                            .as_object()
                            .filter(|map| map.len() == 1)
                            .and_then(|map| map.get("$input"))
                            .and_then(Value::as_str)
                    });
                    if let Some(name) = name {
                        if !inputs.iter().any(|input| input.name == name) {
                            return Err(format!(
                                "Macro input {name} is not declared in this scope"
                            ));
                        }
                    }
                }
            }
            Ok(())
        },
    )
}

// Report use of arguments
pub fn uses_arguments(action: &Action) -> bool {
    let mut found = false;
    action.visit(&mut |node| {
        if let Action::Builtin { action } = node {
            if let crate::builtins::BuiltinAction::SavedMacro { arguments, .. } = action.as_ref() {
                found |= !arguments.is_empty();
            }
        }
    });
    found
}
