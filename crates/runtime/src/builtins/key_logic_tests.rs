use super::*;

// Construct the vendor fixture
fn vendor_fixture(root: &std::path::Path) -> Run {
    vendor_flags_fixture(root, Some(false), true)
}
// Construct the vendor flags fixture
fn vendor_flags_fixture(root: &std::path::Path, key_logic: Option<bool>, multi: bool) -> Run {
    let package = root.join("app/plugins/fixture.keylogic.sdPlugin");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(package.join("icon.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="#ffffff"/></svg>"##).unwrap();
    let mut manifest = json!({
        "UUID":"fixture.keylogic", "Name":"Key logic fixture", "Version":"0.1.0.0", "SDKVersion":2,
        "Author":"Manager test suite", "Description":"Disposable key-logic fixture", "Icon":"icon",
        "OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],
        "Software":{"MinimumVersion":"7.0"}, "CodePath":"runtime.html",
        "Actions":[{"UUID":"fixture.keylogic.action", "Name":"State", "Icon":"icon", "Controllers":["Keypad"],
            "SupportedInMultiActions":multi,
            "States":[{"Name":"Off","Image":"icon"},{"Name":"On","Image":"icon"}]}]
    });
    if let Some(value) = key_logic {
        manifest["Actions"][0]["SupportedInKeyLogicActions"] = json!(value);
    } else {
        manifest["Software"]["MinimumVersion"] = json!("6.9");
        // The execution test measures multi-action scope and settings; the
        // explicit Elgato declarations above still test its admission rules
        crate::test_plugin::use_portable_vendor_on_linux(&mut manifest);
    }
    std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
    let report = stream_dock_manager_plugin_host::preflight::inspect(&package);
    assert!(report.can_install, "{report:#?}");
    tests::fixture(root)
}
// Construct the vendor action fixture for key logic tests
fn vendor_action() -> Action {
    Action::Plugin {
        plugin_id: "fixture.keylogic".into(),
        action_id: "fixture.keylogic.action".into(),
        settings: json!({}),
        resources: Default::default(),
        user_desired_state: Some(1),
    }
}

#[test]
// Verify that key logic eligibility is independent and checks nested saved macros and workspace writes
fn key_logic_eligibility_is_independent_and_checks_nested_saved_macros_and_workspace_writes() {
    let root = tempfile::tempdir().unwrap();
    let mut run = vendor_fixture(root.path());
    let plugin_action = vendor_action();
    let mut plugins = run.runtime.plugins.lock().unwrap().list();
    assert!(
        plugins.iter().any(|plugin| plugin.id == "fixture.keylogic"
            && plugin.actions.len() == 1
            && !plugin.actions[0].supported_in_key_logic_actions),
        "{plugins:#?}"
    );
    let call = tests::action("saved_macro", json!({"macro_id":"nested"}));
    let gesture = tests::action(
        "gesture",
        json!({"children":[call,Action::None,Action::None]}),
    );
    run.macros = vec![SavedMacro {
        id: "nested".into(),
        name: "Nested".into(),
        inputs: vec![],
        action: plugin_action.clone(),
    }]
    .into();
    let check = |action: &Action, plugins: &[_]| {
        validate_key_logic_plugins(
            action,
            |id| {
                run.macros
                    .iter()
                    .find(|saved| saved.id == id)
                    .map(|saved| &saved.action)
            },
            plugins,
        )
    };
    assert!(
        check(&plugin_action, &plugins).is_ok(),
        "Standalone action stays usable"
    );
    assert!(
        check(
            &Action::Macro {
                actions: vec![plugin_action.clone()]
            },
            &plugins
        )
        .is_ok(),
        "Existing multi-actions stay usable"
    );
    assert!(check(&gesture, &plugins)
        .unwrap_err()
        .contains("does not support key logic"));
    plugins[0].actions[0].supported_in_key_logic_actions = true;
    plugins[0].actions[0].supported_in_multi_actions = false;
    assert!(check(&gesture, &plugins)
        .unwrap_err()
        .contains("also requires multi-action support"));
    plugins[0].actions[0].supported_in_multi_actions = true;
    assert!(check(&gesture, &plugins).is_ok());
    // Runtime admission uses the installed manifest, not renderer-provided metadata
    let snapshot = run.runtime.snapshot();
    let mut profile = snapshot.profiles[0].clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = tests::action(
        "gesture",
        json!({"children":[plugin_action,Action::None,Action::None]}),
    );
    assert!(run
        .runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap_err()
        .contains("does not support key logic"));
    assert_eq!(
        run.runtime.snapshot().configuration_revision,
        snapshot.configuration_revision
    );
    assert!(execute(&mut run, &gesture, 0, vec![])
        .unwrap_err()
        .contains("does not support key logic"));
    assert_eq!(run.runtime.builtin_state.taps.pending(), 0);
    run.runtime.shutdown();
}

#[test]
// Verify that key logic current payload requires timings and exact three branches
fn key_logic_current_payload_requires_timings_and_exact_three_branches() {
    let valid = tests::action("gesture", json!({}));
    let mut encoded = serde_json::to_value(valid).unwrap();
    let decoded: Action = serde_json::from_value(encoded.clone()).unwrap();
    decoded.validate(0).unwrap();
    encoded["action"]
        .as_object_mut()
        .unwrap()
        .remove("double_ms");
    assert!(serde_json::from_value::<Action>(encoded).is_err());
    for count in [0, 2, 4] {
        assert!(
            tests::action("gesture", json!({"children":vec![Action::None; count]}))
                .validate(0)
                .is_err()
        );
    }
    assert!(tests::action("gesture", json!({"hold_ms":-1}))
        .validate(0)
        .is_err());
}

#[test]
// Verify that key logic shared macro graphs are bounded and cycles are rejected
fn key_logic_shared_macro_graphs_are_bounded_and_cycles_are_rejected() {
    let call = |id: &str| tests::action("saved_macro", json!({"macro_id":id}));
    let macros: Vec<_> = (0..4)
        .map(|level| SavedMacro {
            id: format!("m{level}"),
            name: format!("M{level}"),
            inputs: vec![],
            action: Action::Macro {
                actions: vec![
                    if level == 3 {
                        Action::None
                    } else {
                        call(&format!("m{}", level + 1))
                    };
                    100
                ],
            },
        })
        .collect();
    let visits = std::cell::Cell::new(0);
    validate_key_logic_plugins(
        &call("m0"),
        |id| {
            visits.set(visits.get() + 1);
            macros
                .iter()
                .find(|saved| saved.id == id)
                .map(|saved| &saved.action)
        },
        &[],
    )
    .unwrap();
    assert!(visits.get() < 500, "Shared macros were expanded repeatedly");
    assert!(!key_logic::contains_gesture(&call("m0"), &macros.clone().into()).unwrap());
    let cycle = vec![SavedMacro {
        id: "loop".into(),
        name: "Loop".into(),
        inputs: vec![],
        action: call("loop"),
    }];
    assert!(
        validate_key_logic_plugins(&call("loop"), |_| Some(&cycle[0].action), &[])
            .unwrap_err()
            .contains("cycle")
    );
    assert!(key_logic::contains_gesture(&call("loop"), &cycle.into())
        .unwrap_err()
        .contains("cycle"));
    let oversized = Action::Macro {
        actions: vec![
            Action::Macro {
                actions: vec![Action::None; 100]
            };
            100
        ],
    };
    assert!(validate_key_logic_plugins(&oversized, |_| None, &[])
        .unwrap_err()
        .contains("10000"));
    assert!(
        key_logic::contains_gesture(&oversized, &MacroLibrary::default())
            .unwrap_err()
            .contains("10000")
    );
}

#[test]
// Preserve unqualified event rows without evaluating their legacy plugin eligibility
fn control_events_check_only_qualified_key_logic_children() {
    use crate::actions::{EventSlot, DEFAULT_HOLD_MS, DEFAULT_MULTI_PRESS_MS};
    let call = tests::action("saved_macro", json!({"macro_id":"cycle"}));
    let slot = EventSlot::HeldTurnLeft;
    let mut actions = vec![Action::None; EventSlot::COUNT];
    actions[slot.index()] = call.clone();
    let events = Action::Events {
        actions,
        multi_press_ms: DEFAULT_MULTI_PRESS_MS,
        hold_ms: DEFAULT_HOLD_MS,
    };
    validate_key_logic_event_plugins(
        &events,
        |_| Some(&call),
        &[],
        Some(EventSlot::TurnLeft.bit()),
    )
    .unwrap();
    assert!(
        validate_key_logic_event_plugins(&events, |_| Some(&call), &[], Some(slot.bit()))
            .unwrap_err()
            .contains("cycle")
    );
}

#[test]
// Verify that key logic navigation fences requests that have not registered recognition
fn key_logic_navigation_fences_requests_that_have_not_registered_recognition() {
    let root = tempfile::tempdir().unwrap();
    let mut run = tests::fixture(root.path());
    let snapshot = run.runtime.snapshot();
    let mut profile = snapshot.profiles[0].clone();
    let mut page = profile.pages[0].clone();
    page.id = "next".into();
    profile.pages.push(page);
    run.runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    Arc::make_mut(&mut run.request).cancellation_generation =
        run.runtime.cancellation.load(Ordering::SeqCst);
    Arc::make_mut(&mut run.request).queued_at = Instant::now();
    // The old request exists, but recognition has not registered its token yet
    run.runtime
        .select_device_profile(&run.request.device_id, "default", Some("next"), true)
        .unwrap();
    assert_eq!(
        execute(&mut run, &gesture(root.path(), 0, 100), 0, vec![]).unwrap_err(),
        "Cancelled"
    );
    assert!(!root.path().join("tap").exists());
    assert!(!run.key_logic);
    run.runtime.shutdown();
}

#[test]
// Verify that key logic saved macro edits retire frozen pending branches
fn key_logic_saved_macro_edits_retire_frozen_pending_branches() {
    let root = tempfile::tempdir().unwrap();
    let run = tests::fixture(root.path());
    let runtime = &run.runtime;
    let mut saved = SavedMacro {
        id: "branch".into(),
        name: "Branch".into(),
        inputs: vec![],
        action: tests::action(
            "write_file",
            json!({"path":root.path().join("old"),"text":"x","operation":"append"}),
        ),
    };
    runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Test saved branch",
            vec![crate::workspace::Edit::SaveMacro {
                value: saved.clone(),
            }],
        )
        .unwrap();
    let snapshot = runtime.snapshot();
    let mut profile = snapshot.profiles[0].clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = tests::action(
        "gesture",
        json!({"double_ms":500,"children":[tests::action("saved_macro",json!({"macro_id":"branch"})),Action::None,Action::None]}),
    );
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    for pressed in [true, false] {
        runtime
            .simulate_input(
                &run.request.device_id,
                hardware_core::ControlEvent {
                    control_id: "key-1".into(),
                    input: Input::Button { pressed },
                },
            )
            .unwrap();
    }
    wait(|| runtime.builtin_state.taps.pending() == 1);
    saved.action = Action::None;
    runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Replace saved branch",
            vec![crate::workspace::Edit::SaveMacro { value: saved }],
        )
        .unwrap();
    wait(|| runtime.builtin_state.active.load(Ordering::SeqCst) == 0);
    assert!(!root.path().join("old").exists());
    assert_eq!(runtime.builtin_state.taps.pending(), 0);
    runtime.shutdown();
}

#[test]
// Verify that key logic dynamic targets check both flags and restore ordinary run scope
fn key_logic_dynamic_targets_check_both_flags_and_restore_ordinary_run_scope() {
    for (key_logic, multi, expected) in [
        (false, true, "does not support key logic"),
        (true, false, "also requires multi-action support"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut run = vendor_flags_fixture(root.path(), Some(key_logic), multi);
        let trigger = tests::action("trigger_binding", json!({"control_id":"key-2"}));
        let action = tests::action(
            "gesture",
            json!({"double_ms":0,"children":[trigger,Action::None,Action::None]}),
        );
        // Target is not part of the gesture tree and is rebound after planning
        run.runtime.state.lock().unwrap().profiles[0].pages[0]
            .bindings
            .get_mut("key-2")
            .unwrap()
            .action = vendor_action();
        assert!(execute(&mut run, &action, 0, vec![])
            .unwrap_err()
            .contains(expected));
        assert!(!run.key_logic);
        assert!(run.branch_cancellations.is_empty());
        // Ineligibility is local to the gesture, never sticky on this run
        let error = execute(&mut run, &vendor_action(), 0, vec![]).unwrap_err();
        assert!(!error.contains("key logic"), "{error}");
        run.runtime.shutdown();
    }
}

#[test]
// Verify that key logic eligible plugin branches reuse nested context settings and desired states
fn key_logic_eligible_plugin_branches_reuse_nested_context_settings_and_desired_states() {
    let root = tempfile::tempdir().unwrap();
    let mut run = vendor_flags_fixture(root.path(), None, true);
    {
        let mut host = run.runtime.plugins.lock().unwrap();
        host.enable("fixture.keylogic", true).unwrap();
        // This headless dispatch fixture has no visible device surfaces
        host.set_sleep_when_hidden("fixture.keylogic", false)
            .unwrap();
        host.reconcile_demand(HashMap::from([(
            "fixture.keylogic".into(),
            vec![stream_dock_manager_plugin_host::PluginDemand {
                device_id: run.request.device_id.clone(),
                profile_id: "default".into(),
            }],
        )]));
        host.poll();
    }
    let mut first = vendor_action();
    let mut hold = vendor_action();
    if let Action::Plugin {
        settings,
        user_desired_state,
        ..
    } = &mut first
    {
        *settings = json!({"branch":"tap"});
        *user_desired_state = (!cfg!(target_os = "linux")).then_some(1);
    }
    if let Action::Plugin {
        settings,
        user_desired_state,
        ..
    } = &mut hold
    {
        *settings = json!({"branch":"hold"});
        *user_desired_state = (!cfg!(target_os = "linux")).then_some(0);
    }
    let action = tests::action(
        "gesture",
        json!({"double_ms":0,"hold_ms":0,"children":[first,Action::None,hold]}),
    );
    execute(&mut run, &action, 0, vec![]).unwrap();
    let mut held_run = run.clone();
    held_run.pressed.store(true, Ordering::SeqCst);
    let pressed = held_run.pressed.clone();
    let held_action = action.clone();
    let thread = std::thread::spawn(move || execute(&mut held_run, &held_action, 0, vec![]));
    wait(|| {
        run.runtime.plugins.lock().unwrap().effective_settings(
            "fixture.keylogic",
            "default:main@2",
            &run.request.device_id,
            "key-1",
            &json!({}),
        )["branch"]
            == "hold"
    });
    pressed.store(false, Ordering::SeqCst);
    thread.join().unwrap().unwrap();
    let host = run.runtime.plugins.lock().unwrap();
    assert_eq!(
        host.effective_settings(
            "fixture.keylogic",
            "default:main@0",
            &run.request.device_id,
            "key-1",
            &json!({})
        ),
        json!({"branch":"tap"})
    );
    assert_eq!(
        host.effective_settings(
            "fixture.keylogic",
            "default:main@1",
            &run.request.device_id,
            "key-1",
            &json!({})
        ),
        json!({})
    );
    drop(host);
    assert!(!run.key_logic);
    run.runtime.shutdown();
}

// Wait for the key logic tests condition or result
fn wait(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < end,
            "Key logic did not reach the expected state"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
// Construct the gesture fixture for key logic tests
fn gesture(root: &std::path::Path, double_ms: i32, hold_ms: i32) -> Action {
    tests::action(
        "gesture",
        json!({
            "double_ms":double_ms,"hold_ms":hold_ms,
            "children":(["tap","double","hold"].map(|name| tests::action("write_file", json!({
                "path":root.join(name),"text":"x","operation":"append"
            }))))
        }),
    )
}

#[test]
// Verify that key logic single double hold and nested paths are independent
fn key_logic_single_double_hold_and_nested_paths_are_independent() {
    let root = tempfile::tempdir().unwrap();
    let mut first = tests::fixture(root.path());
    let action = gesture(root.path(), 200, 20);
    let mut second = first.clone();
    let thread_action = action.clone();
    let thread = std::thread::spawn(move || execute(&mut first, &thread_action, 0, vec![0]));
    wait(|| second.runtime.builtin_state.taps.pending() == 1);
    execute(&mut second, &action, 0, vec![0]).unwrap();
    thread.join().unwrap().unwrap();
    assert!(!root.path().join("tap").exists());
    assert_eq!(std::fs::read(root.path().join("double")).unwrap(), b"x");
    // Separate branch paths cannot consume each other's pending first tap
    let mut other = second.clone();
    let thread_action = action.clone();
    let thread = std::thread::spawn(move || execute(&mut other, &thread_action, 0, vec![1]));
    wait(|| second.runtime.builtin_state.taps.pending() == 1);
    execute(&mut second, &action, 0, vec![2]).unwrap();
    thread.join().unwrap().unwrap();
    assert_eq!(std::fs::read(root.path().join("tap")).unwrap(), b"xx");
    second.pressed.store(true, Ordering::SeqCst);
    let release = second.pressed.clone();
    let held = root.path().join("hold");
    let thread = std::thread::spawn(move || {
        wait(|| held.exists());
        release.store(false, Ordering::SeqCst);
    });
    execute(&mut second, &action, 0, vec![0]).unwrap();
    thread.join().unwrap();
    assert_eq!(std::fs::read(root.path().join("hold")).unwrap(), b"x");
    assert_eq!(second.runtime.builtin_state.taps.pending(), 0);
    assert_eq!(second.plugin_scope(&[0]), "default:main@0");
    assert_eq!(second.plugin_scope(&[1]), "default:main@1");
    assert!(matches!(second.plugin_input(&[0]), Input::Pulse));
    second.runtime.shutdown();
}

#[test]
// Verify that key logic pending release is retired by cancel navigation edit removal and shutdown
fn key_logic_pending_release_is_retired_by_cancel_navigation_edit_removal_and_shutdown() {
    for mode in [
        "cancel", "page", "profile", "edit", "disable", "remove", "shutdown",
    ] {
        let root = tempfile::tempdir().unwrap();
        let run = tests::fixture(root.path());
        let runtime = run.runtime.clone();
        let device = run.request.device_id.clone();
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().action =
            gesture(root.path(), 500, 1000);
        let mut next_page = profile.pages[0].clone();
        next_page.id = "next".into();
        profile.pages.push(next_page);
        runtime
            .save_profile(profile.clone(), snapshot.configuration_revision)
            .unwrap();
        profile.id = "next-profile".into();
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        for pressed in [true, false] {
            runtime
                .simulate_input(
                    &device,
                    hardware_core::ControlEvent {
                        control_id: "key-1".into(),
                        input: Input::Button { pressed },
                    },
                )
                .unwrap();
        }
        wait(|| runtime.builtin_state.taps.pending() == 1);
        match mode {
            "cancel" => runtime.cancel_macros(),
            "page" => {
                runtime
                    .select_device_profile(&device, "default", Some("next"), true)
                    .unwrap();
            }
            "profile" => {
                runtime
                    .select_device_profile(&device, "next-profile", None, true)
                    .unwrap();
            }
            "edit" | "disable" => {
                let snapshot = runtime.snapshot();
                let mut profile = snapshot
                    .profiles
                    .iter()
                    .find(|p| p.id == "default")
                    .unwrap()
                    .clone();
                let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
                if mode == "disable" {
                    binding.disabled = true;
                } else {
                    binding.action = Action::None;
                }
                runtime
                    .save_profile(profile, snapshot.configuration_revision)
                    .unwrap();
            }
            "remove" => {
                runtime.remove_simulator(&device).unwrap();
            }
            "shutdown" => runtime.shutdown(),
            _ => unreachable!(),
        }
        wait(|| runtime.builtin_state.active.load(Ordering::SeqCst) == 0);
        assert_eq!(runtime.builtin_state.taps.pending(), 0, "{mode}");
        for branch in ["tap", "double", "hold"] {
            assert!(!root.path().join(branch).exists(), "{mode}: {branch}");
        }
        runtime.shutdown();
    }
}

#[test]
// Verify that key logic cancellation during hold releases capture without tap
fn key_logic_cancellation_during_hold_releases_capture_without_tap() {
    let root = tempfile::tempdir().unwrap();
    let mut run = tests::fixture(root.path());
    run.pressed.store(true, Ordering::SeqCst);
    let runtime = run.runtime.clone();
    let action = gesture(root.path(), 100, 10);
    let thread = std::thread::spawn(move || execute(&mut run, &action, 0, vec![]));
    wait(|| root.path().join("hold").exists());
    runtime.cancel_macros();
    assert_eq!(thread.join().unwrap().unwrap_err(), "Cancelled");
    assert!(!root.path().join("tap").exists());
    assert!(!root.path().join("double").exists());
    assert_eq!(std::fs::read(root.path().join("hold")).unwrap(), b"x");
    runtime.shutdown();
}
