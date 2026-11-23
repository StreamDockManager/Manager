use crate::{action_fields::FieldRole, actions::Action, builtins::BuiltinAction};
use serde_json::{json, Value};

#[test]
// Verify that immutable and mutable traversal share nested order and keep plugin JSON opaque
fn immutable_and_mutable_traversal_share_nested_order_and_keep_plugin_json_opaque() {
    let mut action = Action::Toggle {
        on: Box::new(Action::Builtin {
            action: Box::new(BuiltinAction::Parallel {
                wait: true,
                children: vec![Action::Command {
                    program: "tool".into(),
                    args: vec!["first".into(), "second".into()],
                }],
            }),
        }),
        off: Box::new(Action::Plugin {
            user_desired_state: None,
            plugin_id: "plugin".into(),
            action_id: "action".into(),
            settings: json!({"children":[{"kind":"open_url","url":"opaque"}]}),
            resources: Default::default(),
        }),
    };
    let original = serde_json::to_value(&action).unwrap();
    let mut before = Vec::new();
    let mut plugins = Vec::new();
    action
        .try_visit_fields(
            &mut |role, value| {
                before.push((role, value.clone()));
                Ok::<_, String>(())
            },
            &mut |id, action, settings| {
                plugins.push((id.to_owned(), action.to_owned(), settings.clone()));
                Ok(())
            },
        )
        .unwrap();
    let mut changed = Vec::new();
    action
        .try_visit_fields_mut(
            &mut |role, value| {
                changed.push((role, value.clone()));
                value.push('!');
                Ok::<_, String>(())
            },
            &mut |_, _, settings| {
                assert_eq!(settings, &plugins[0].2);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(before, changed);
    assert_eq!(before.len(), 3);
    assert_eq!(before[0].0, FieldRole::Program);
    assert_eq!(before[1].0, FieldRole::Argument);
    assert_eq!(plugins.len(), 1);
    assert_eq!(
        serde_json::to_value(action).unwrap()["off"],
        original["off"]
    );
}

#[test]
// Verify that semantic selectors are not paths and path roles are typed
fn semantic_selectors_are_not_paths_and_path_roles_are_typed() {
    let action = Action::Macro {
        actions: vec![
            Action::Builtin {
                action: Box::new(BuiltinAction::SearchSelection {
                    url: "https://example.com?q={selection}".into(),
                    source: "clipboard".into(),
                }),
            },
            Action::Builtin {
                action: Box::new(BuiltinAction::FileOperation {
                    operation: "copy".into(),
                    source: "/source".into(),
                    destination: "/destination".into(),
                    replace: false,
                    template: false,
                }),
            },
        ],
    };
    let mut paths = Vec::new();
    action
        .try_visit_fields(
            &mut |role, value| {
                if role.is_local_path() {
                    paths.push(value.clone());
                }
                Ok::<_, String>(())
            },
            &mut |_, _, _: &Value| Ok(()),
        )
        .unwrap();
    assert_eq!(paths, ["/source", "/destination"]);
}

#[test]
// Verify that plugin desired state is typed bounded and keeps settings opaque
fn plugin_desired_state_is_typed_bounded_and_keeps_settings_opaque() {
    let value = json!({"kind":"plugin","plugin_id":"p","action_id":"a","settings":{"state":9,"userDesiredState":"opaque"},"user_desired_state":1});
    let action: Action = serde_json::from_value(value.clone()).unwrap();
    action.validate(0).unwrap();
    assert_eq!(serde_json::to_value(&action).unwrap(), value);
    let mut invalid = value.clone();
    invalid["user_desired_state"] = json!(2);
    assert!(serde_json::from_value::<Action>(invalid)
        .unwrap()
        .validate(0)
        .is_err());
    for bad in [json!(-1), json!(256), json!("1"), json!(1.5)] {
        let mut invalid = value.clone();
        invalid["user_desired_state"] = bad;
        assert!(serde_json::from_value::<Action>(invalid).is_err());
    }
    let mut absent = value;
    absent.as_object_mut().unwrap().remove("user_desired_state");
    let action: Action = serde_json::from_value(absent.clone()).unwrap();
    assert_eq!(serde_json::to_value(action).unwrap(), absent);
}

#[test]
// Verify that plugin desired state survives workspace restart and reaches dispatch
fn plugin_desired_state_survives_workspace_restart_and_reaches_dispatch() {
    use std::sync::atomic::Ordering;
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("plugins/fixture.desired.sdPlugin");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(package.join("icon.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="#ffffff"/></svg>"##).unwrap();
    std::fs::write(package.join("manifest.json"), json!({
        "UUID":"fixture.desired", "Name":"Desired state fixture", "Version":"0.1.0.0", "SDKVersion":2,
        "Author":"Manager test suite", "Description":"Disposable desired-state contract fixture", "Icon":"icon",
        "OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],
        "Software":{"MinimumVersion":"6.9"}, "CodePath":"runtime.html",
        "Actions":[{"UUID":"fixture.desired.action", "Name":"State", "Icon":"icon", "Controllers":["Keypad"], "States":[{"Name":"Off","Image":"icon"},{"Name":"On","Image":"icon"}]}]
    }).to_string()).unwrap();
    let action = Action::Plugin {
        plugin_id: "fixture.desired".into(),
        action_id: "fixture.desired.action".into(),
        settings: json!({"userDesiredState":"opaque"}),
        resources: Default::default(),
        user_desired_state: Some(1),
    };
    let runtime = crate::test_support::headless(root.path().into()).unwrap();
    let mut data = runtime.profiles.load().unwrap();
    data.saved_macros.push(crate::builtins::SavedMacro {
        id: "desired".into(),
        name: "Desired".into(),
        inputs: Default::default(),
        action: Action::Macro {
            actions: vec![action.clone()],
        },
    });
    runtime.profiles.save(&data).unwrap();
    runtime.shutdown();
    drop(runtime);
    let runtime = crate::test_support::headless(root.path().into()).unwrap();
    let data = runtime.profiles.load().unwrap();
    let saved = &data
        .saved_macros
        .iter()
        .find(|m| m.id == "desired")
        .unwrap()
        .action;
    let Action::Macro { actions } = saved else {
        panic!("macro missing")
    };
    assert_eq!(
        serde_json::to_value(&actions[0]).unwrap(),
        serde_json::to_value(&action).unwrap()
    );
    if cfg!(target_os = "linux") {
        // The saved value remains portable, but this Elgato action is not
        // executable here; real desired-state dispatch is covered on Mac/Windows
        crate::test_plugin::assert_elgato_platform_blocked(&package);
        runtime.shutdown();
        return;
    }
    {
        let mut host = runtime.plugins.lock().unwrap();
        host.enable("fixture.desired", true).unwrap();
        // This headless dispatch fixture has no visible device surfaces
        host.set_sleep_when_hidden("fixture.desired", false)
            .unwrap();
        host.reconcile_demand(std::collections::HashMap::from([(
            "fixture.desired".into(),
            vec![stream_dock_manager_plugin_host::PluginDemand {
                device_id: "fixture-device".into(),
                profile_id: "fixture-profile".into(),
            }],
        )]));
        host.poll();
    }
    let dispatch = |scope| {
        crate::actions::execute_leaf(
            &runtime,
            &actions[0],
            "fixture-device",
            "key",
            scope,
            None,
            &hardware_core::Input::Pulse,
            runtime.cancellation.load(Ordering::SeqCst),
        )
    };
    dispatch("fixture-profile:page@0").unwrap();
    assert!(dispatch("fixture-profile:page")
        .unwrap_err()
        .contains("Desired state requires"));
    runtime.shutdown();
}
