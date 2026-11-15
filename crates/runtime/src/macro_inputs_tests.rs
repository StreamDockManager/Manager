use crate::{
    actions::Action,
    builtins::{BuiltinAction, SavedMacro},
    macro_inputs::*,
};
use serde_json::json;
use std::collections::BTreeMap;

// Handle the incoming macro inputs tests input
fn input(name: &str, kind: &str, required: bool, default: Option<serde_json::Value>) -> MacroInput {
    MacroInput {
        name: name.into(),
        label: name.into(),
        r#type: kind.into(),
        required,
        default,
    }
}

#[test]
// Verify that call boundaries validate required types defaults and explicit sources
fn call_boundaries_validate_required_types_defaults_and_explicit_sources() {
    let inputs = vec![
        input("text", "string", true, None),
        input("count", "number", false, Some(json!(2))),
        input("enabled", "boolean", false, None),
    ];
    let empty = InputValues::new();
    assert!(bind(
        &inputs,
        &Arguments::new(),
        &empty,
        &empty,
        |_| unreachable!()
    )
    .unwrap_err()
    .contains("required"));
    let caller = BTreeMap::from([("caller_text".into(), json!("forwarded"))]);
    let outputs = BTreeMap::from([("result.ok".into(), json!(true))]);
    let arguments = BTreeMap::from([
        (
            "text".into(),
            MacroArgument::Input {
                name: "caller_text".into(),
            },
        ),
        (
            "count".into(),
            MacroArgument::Parameter {
                name: "count".into(),
            },
        ),
        (
            "enabled".into(),
            MacroArgument::Output {
                name: "result.ok".into(),
            },
        ),
    ]);
    let bound = bind(&inputs, &arguments, &caller, &outputs, |_| Ok(json!(7))).unwrap();
    assert_eq!(bound["text"], "forwarded");
    assert_eq!(bound["count"], 7);
    assert_eq!(bound["enabled"], true);
    let literal = BTreeMap::from([(
        "text".into(),
        MacroArgument::Literal {
            value: json!("literal"),
        },
    )]);
    assert_eq!(
        bind(&inputs, &literal, &empty, &empty, |_| unreachable!()).unwrap()["count"],
        2
    );
    assert!(!bind(&inputs, &literal, &empty, &empty, |_| unreachable!())
        .unwrap()
        .contains_key("enabled"));
    let wrong = BTreeMap::from([("text".into(), MacroArgument::Literal { value: json!(3) })]);
    assert!(bind(&inputs, &wrong, &empty, &empty, |_| unreachable!())
        .unwrap_err()
        .contains("requires a string"));
    assert!(
        bind(&inputs, &arguments, &empty, &outputs, |_| unreachable!())
            .unwrap_err()
            .contains("Caller input")
    );
    assert!(bind(&inputs, &arguments, &caller, &empty, |_| Ok(json!(7)))
        .unwrap_err()
        .contains("Prior run output"));
}

#[test]
// Verify that declared plugin inputs preserve types without reinterpreting opaque settings
fn declared_plugin_inputs_preserve_types_without_reinterpreting_opaque_settings() {
    let action = Action::Plugin {
        user_desired_state: None,
        plugin_id: "test".into(),
        action_id: "test.action".into(),
        settings: json!({
            "count":{"$input":"count"}, "opaque":{"path":"${input.hidden}"}
        }),
        resources: Default::default(),
    };
    let values = BTreeMap::from([("count".into(), json!(3))]);
    let result = resolve_action(&action, &values, &|_, _| vec!["count".into()]).unwrap();
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(result["settings"]["count"], 3);
    assert_eq!(result["settings"]["opaque"]["path"], "${input.hidden}");
    assert!(resolve_action(&action, &values, &|_, _| vec![])
        .unwrap_err()
        .contains("does not declare"));
    assert_eq!(
        serde_json::to_value(action).unwrap()["settings"]["count"],
        json!({"$input":"count"})
    );
}

#[test]
// Verify that definition and forwarding contracts reject undeclared or incompatible inputs
fn definition_and_forwarding_contracts_reject_undeclared_or_incompatible_inputs() {
    let target = SavedMacro {
        id: "target".into(),
        name: "Target".into(),
        inputs: vec![input("value", "boolean", true, None)],
        action: Action::None,
    };
    assert!(validate_definitions(&[
        input("same", "string", false, None),
        input("same", "number", false, None)
    ])
    .is_err());
    assert!(
        validate_definitions(&[input("value", "boolean", false, Some(json!("wrong")))]).is_err()
    );
    let args = BTreeMap::from([(
        "value".into(),
        MacroArgument::Input {
            name: "outer".into(),
        },
    )]);
    assert!(validate_call(&target, &args, &[]).is_err());
    assert!(validate_call(&target, &args, &[input("outer", "string", false, None)]).is_err());
    validate_call(&target, &args, &[input("outer", "boolean", false, None)]).unwrap();
    let call = Action::Builtin {
        action: Box::new(BuiltinAction::SavedMacro {
            macro_id: "target".into(),
            arguments: args,
        }),
    };
    assert!(validate_action_calls(&call, &[target], &[]).is_err());
}

#[test]
// Verify that requirements use unreleased baseline and preserve stricter floors
fn requirements_use_unreleased_baseline_and_preserve_stricter_floors() {
    let mut data = crate::ProfileData {
        minimum_manager: Some("0.1.0".into()),
        ..Default::default()
    };
    data.update_requirements();
    assert_eq!(data.minimum_manager.as_deref(), Some("0.1.0"));
    data.minimum_manager = None;
    data.saved_macros.push(SavedMacro {
        id: "macro".into(),
        name: "Macro".into(),
        inputs: vec![input("value", "string", false, None)],
        action: Action::None,
    });
    data.update_requirements();
    assert_eq!(data.minimum_manager.as_deref(), Some("0.1.0"));
    data.minimum_manager = Some("9.0.0".into());
    data.update_requirements();
    assert_eq!(data.minimum_manager.as_deref(), Some("9.0.0"));
    let old: SavedMacro =
        serde_json::from_value(json!({"id":"old","name":"Old","action":{"kind":"none"}})).unwrap();
    assert!(old.inputs.is_empty());
    assert!(serde_json::to_value(old).unwrap().get("inputs").is_none());
}
