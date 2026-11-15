use super::*;
use crate::media::allocation_tests::measure_allocations;

// Construct the library fixture for ownership tests
fn library() -> Vec<SavedMacro> {
    (0..128)
        .map(|index| SavedMacro {
            id: format!("macro-{index}"),
            name: format!("Macro {index}"),
            action: Action::Plugin {
                user_desired_state: None,
                plugin_id: "fixture".into(),
                action_id: "fixture".into(),
                settings: json!({"payload": "x".repeat(16 * 1024)}),
                resources: Default::default(),
            },
            inputs: vec![],
        })
        .collect()
}

#[test]
// Verify that execution input allocation probe
fn execution_input_allocation_probe() {
    let root = tempfile::tempdir().unwrap();
    let mut run = tests::fixture(root.path());
    run.runtime.state.lock().unwrap().saved_macros = library();
    let mut observations = vec![];
    for repetition in 0..5 {
        // Retain the original activation operation as an executable control
        let (_, legacy_allocations, legacy_bytes) = measure_allocations(|| {
            let snapshot = run.runtime.snapshot();
            std::hint::black_box((
                Arc::new(run.runtime.freeze_parameters(&snapshot)),
                snapshot.saved_macros.clone(),
            ));
        });
        *run.runtime.builtin_state.execution_inputs.lock().unwrap() = None;
        let (cold, cold_allocations, cold_bytes) =
            measure_allocations(|| run.runtime.builtin_state.execution_inputs(&run.runtime));
        let ((inputs, parameters), warm_allocations, warm_bytes) = measure_allocations(|| {
            let inputs = run.runtime.builtin_state.execution_inputs(&run.runtime);
            let parameters = Arc::new(inputs.freeze_parameters(&run.runtime));
            (inputs, parameters)
        });
        assert!(Arc::ptr_eq(&cold.macros.saved, &inputs.macros.saved));
        assert!(Arc::ptr_eq(&cold.profiles, &parameters.profiles));
        run.parameters = Some(parameters);
        run.macros = inputs.macros;
        let (_, branch_allocations, branch_bytes) = measure_allocations(|| {
            for _ in 0..8 {
                std::hint::black_box(run.clone());
            }
        });
        assert!(warm_bytes < legacy_bytes / 100);
        assert!(branch_bytes < 16 * 1024);
        observations.push(json!({
            "repetition": repetition,
            "library_entries": 128,
            "setting_payload_bytes_each": 16384,
            "branches": 8,
            "legacy_activation_allocations": legacy_allocations,
            "legacy_activation_allocated_bytes": legacy_bytes,
            "cold_revision_allocations": cold_allocations,
            "cold_revision_allocated_bytes": cold_bytes,
            "warm_activation_allocations": warm_allocations,
            "warm_activation_allocated_bytes": warm_bytes,
            "branch_allocations": branch_allocations,
            "branch_allocated_bytes": branch_bytes,
        }));
    }
    eprintln!("EXECUTION_INPUT_CANDIDATE={}", json!(observations));
    run.runtime.shutdown();
}

#[test]
// Verify that revisions freeze macro actions and parameter definitions
fn revisions_freeze_macro_actions_and_parameter_definitions() {
    let root = tempfile::tempdir().unwrap();
    let mut run = tests::fixture(root.path());
    let path = root.path().join("revision.txt");
    {
        let mut state = run.runtime.state.lock().unwrap();
        state.profiles[0].parameters = vec![crate::parameters::ProfileParameter {
            name: "value".into(),
            label: "Value".into(),
            r#type: "string".into(),
            sensitive: false,
            required: false,
            default: Some(json!("old")),
        }];
        state.saved_macros = vec![SavedMacro {
            id: "fixture".into(),
            name: "Fixture".into(),
            inputs: vec![],
            action: tests::action(
                "write_file",
                json!({
                    "path": path, "text": "${parameter.value}", "operation": "replace", "replace": true
                }),
            ),
        }];
    }
    let before = run.runtime.builtin_state.execution_inputs(&run.runtime);
    run.macros = before.macros.clone();
    run.parameters = Some(Arc::new(before.freeze_parameters(&run.runtime)));
    {
        let mut state = run.runtime.state.lock().unwrap();
        state.configuration_revision += 1;
        state.profiles[0].parameters[0].default = Some(json!("new"));
        state.saved_macros[0].name = "Edited".into();
        state.saved_macros[0].action = tests::action(
            "write_file",
            json!({
                "path": path, "text": "${parameter.value}", "operation": "append"
            }),
        );
    }
    let after = run.runtime.builtin_state.execution_inputs(&run.runtime);
    assert!(!Arc::ptr_eq(&before.macros.saved, &after.macros.saved));
    assert!(!Arc::ptr_eq(&before.profiles, &after.profiles));
    let call = tests::action("saved_macro", json!({"macro_id": "fixture"}));
    execute(&mut run, &call, 0, vec![]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
    let old_revision = Arc::downgrade(&before.macros.saved);
    run.macros = after.macros.clone();
    run.parameters = Some(Arc::new(after.freeze_parameters(&run.runtime)));
    drop(before);
    assert!(old_revision.upgrade().is_none());
    execute(&mut run, &call, 0, vec![]).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "oldnew");
    run.runtime.shutdown();
}

#[test]
// Verify that debugger overlay and branch outputs are isolated without copying library
fn debugger_overlay_and_branch_outputs_are_isolated_without_copying_library() {
    let root = tempfile::tempdir().unwrap();
    let mut run = tests::fixture(root.path());
    run.runtime.state.lock().unwrap().saved_macros = library();
    let inputs = run.runtime.builtin_state.execution_inputs(&run.runtime);
    run.macros = inputs.macros.clone();
    run.locals
        .insert("output".into(), json!({"value":"parent"}));
    run.inputs.insert("argument".into(), json!("parent"));
    let mut child = run.clone();
    assert!(Arc::ptr_eq(&run.request, &child.request));
    assert!(Arc::ptr_eq(&run.macros.saved, &child.macros.saved));
    child.locals.get_mut("output").unwrap()["value"] = json!("child");
    child.inputs.insert("argument".into(), json!("child"));
    Arc::make_mut(&mut child.request).binding_scope = "child:page".into();
    assert_eq!(run.locals["output"]["value"], "parent");
    assert_eq!(run.inputs["argument"], "parent");
    assert_eq!(run.request.binding_scope, "default:main");
    let overlay = inputs.macros.with_draft(SavedMacro {
        id: "macro-0".into(),
        name: "Unsaved".into(),
        action: Action::None,
        inputs: vec![],
    });
    assert!(Arc::ptr_eq(&run.macros.saved, &overlay.saved));
    assert_eq!(overlay.iter().count(), 128);
    assert_eq!(
        overlay.iter().find(|m| m.id == "macro-0").unwrap().name,
        "Unsaved"
    );
    assert_eq!(
        run.macros.iter().find(|m| m.id == "macro-0").unwrap().name,
        "Macro 0"
    );
    validate_macro_graph_refs(&overlay.iter().collect::<Vec<_>>()).unwrap();
    run.runtime.shutdown();
}

#[test]
// Verify that leaf boundary rejects run owned actions and checks final identity
fn leaf_boundary_rejects_run_owned_actions_and_checks_final_identity() {
    let root = tempfile::tempdir().unwrap();
    let run = tests::fixture(root.path());
    let leaf = |action: &Action| {
        crate::actions::execute_leaf(
            &run.runtime,
            action,
            &run.request.device_id,
            &run.request.control_id,
            &run.request.binding_scope,
            run.request.connection_epoch,
            &run.request.input,
            run.request.cancellation_generation,
        )
    };
    for action in [
        Action::Delay { milliseconds: 0 },
        Action::Macro { actions: vec![] },
        Action::Toggle {
            on: Box::new(Action::None),
            off: Box::new(Action::None),
        },
        Action::Command {
            program: "must-not-launch".into(),
            args: vec![],
        },
        Action::Hotkey {
            keys: vec!["a".into()],
        },
        tests::action(
            "repeat_actions",
            json!({"count":1,"interval_ms":0,"children":[]}),
        ),
    ] {
        assert_eq!(leaf(&action).unwrap_err(), "Action requires a run context");
    }
    leaf(&Action::None).unwrap();
    run.runtime.connection_epochs.lock().unwrap().insert(
        run.request.device_id.clone(),
        run.request.connection_epoch.unwrap() + 1,
    );
    assert_eq!(
        leaf(&Action::None).unwrap_err(),
        "device connection changed; pending action discarded"
    );
    run.runtime.cancellation.fetch_add(1, Ordering::SeqCst);
    assert_eq!(leaf(&Action::None).unwrap_err(), "macro cancelled");
    run.runtime.shutdown();
}

#[test]
// Verify that nested macro repeat and toggle have one sequence owner
fn nested_macro_repeat_and_toggle_have_one_sequence_owner() {
    let root = tempfile::tempdir().unwrap();
    let mut run = tests::fixture(root.path());
    let output = root.path().join("sequence.txt");
    let write = |text: &str| {
        tests::action(
            "write_file",
            json!({
                "path":output, "text":text, "operation":"append"
            }),
        )
    };
    let action = Action::Macro {
        actions: vec![tests::action(
            "repeat_actions",
            json!({
                "count":4,"interval_ms":0,"children":[Action::Toggle {
                    on:Box::new(write("on")), off:Box::new(write("off"))
                }]
            }),
        )],
    };
    execute(&mut run, &action, 0, vec![]).unwrap();
    assert_eq!(std::fs::read_to_string(output).unwrap(), "onoffonoff");
    run.runtime.shutdown();
}
