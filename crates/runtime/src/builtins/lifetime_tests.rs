use super::*;
use crate::macro_debug::{DebugCommand, DebugMode, DebugStart, DebugStatus};

// Wait for the lifetime tests condition or result
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !ready() {
        assert!(Instant::now() < deadline, "execution fixture timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

// Construct the debug request fixture for lifetime tests
fn debug_request() -> DebugStart {
    DebugStart {
        action: tests::action(
            "parallel",
            json!({"wait":false,"children":[{
                "kind":"macro","actions":[
                    {"kind":"delay","milliseconds":2},
                    {"kind":"builtin","action":{"kind":"cycle_actions","children":[{"kind":"none"}]}},
                    {"kind":"builtin","action":{"kind":"cooldown","milliseconds":100,"children":[{"kind":"none"}]}}
                ]
            }]}),
        ),
        mode: DebugMode::Live,
        start_running: false,
        device_id: "simulator:builtins".into(),
        profile_id: "default".into(),
        page_id: "main".into(),
        control_id: "key-1".into(),
        inputs: vec![],
        arguments: Default::default(),
        macro_id: None,
        breakpoints: vec![],
        simulation: Default::default(),
    }
}

#[test]
// Verify that completed debug contexts and lanes probe
fn completed_debug_contexts_and_lanes_probe() {
    let root = tempfile::tempdir().unwrap();
    let run = tests::fixture(root.path());
    let state = &run.runtime.builtin_state;
    let mut observations = vec![];
    for repetition in 0..5 {
        for _ in 0..16 {
            let id = run.runtime.start_macro_debug(debug_request()).unwrap().id;
            run.runtime
                .macro_debug_command(&id, DebugCommand::Continue)
                .unwrap();
            wait(|| run.runtime.macro_debug_view(&id, 0).unwrap().status == DebugStatus::Finished);
        }
        wait(|| {
            state.indices.lock().unwrap().is_empty() && state.cooldowns.lock().unwrap().is_empty()
        });
        for index in 0..128 {
            let lane = state.lane(format!("retired-{repetition}-{index}"));
            let retired = Arc::downgrade(&lane);
            drop(lane);
            assert!(retired.upgrade().is_none());
        }
        assert!(state.lanes.lock().unwrap().len() <= 1);
        observations.push(json!({
            "repetition": repetition,
            "completed_debug_runs": (repetition + 1) * 16,
            "retained_debug_indices": state.indices.lock().unwrap().len(),
            "retained_debug_cooldowns": state.cooldowns.lock().unwrap().len(),
            "retained_lanes": state.lanes.lock().unwrap().len(),
            "retained_debug_sessions": state.debug_sessions.lock().unwrap().len(),
        }));
    }
    eprintln!("BUILTIN_LIFETIME_CANDIDATE={}", json!(observations));
    run.runtime.shutdown();
}

#[test]
// Verify that oversized builtin values do not replace durable or live state
fn oversized_builtin_values_do_not_replace_durable_or_live_state() {
    let root = tempfile::tempdir().unwrap();
    let state = State::new(root.path().join("values.json"));
    state
        .store("clipboard:old".into(), json!("saved"), true)
        .unwrap();
    assert!(state
        .store(
            "clipboard:large".into(),
            json!("x".repeat(4 * 1024 * 1024)),
            true
        )
        .is_err());
    assert!(state.get("clipboard:large").is_none());
    assert_eq!(
        State::with_storage(state.storage.clone())
            .unwrap()
            .get("clipboard:old"),
        Some(json!("saved"))
    );
    assert!(!root.path().join("values.json").exists());
}

#[test]
// Verify that invalid database values are reported without empty reset
fn invalid_database_values_are_reported_without_empty_reset() {
    let root = tempfile::tempdir().unwrap();
    let storage = stream_dock_manager_storage::Storage::open(root.path().to_path_buf()).unwrap();
    storage
        .write_json(
            stream_dock_manager_storage::Domain::LocalPreferences,
            "builtin-values",
            &json!(["invalid map"]),
            1024,
        )
        .unwrap();
    assert!(State::with_storage(storage.clone()).is_err());
    assert_eq!(
        storage
            .read_json::<Value>(
                stream_dock_manager_storage::Domain::LocalPreferences,
                "builtin-values",
                1024
            )
            .unwrap(),
        Some(json!(["invalid map"]))
    );
}

#[test]
// Verify that failed database write keeps previous builtin values
fn failed_database_write_keeps_previous_builtin_values() {
    let root = tempfile::tempdir().unwrap();
    let state = State::new(root.path().join("values.json"));
    state
        .store("clipboard:old".into(), json!("saved"), true)
        .unwrap();
    state
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(state
        .store("clipboard:old".into(), json!("changed"), true)
        .is_err());
    assert_eq!(state.get("clipboard:old"), Some(json!("saved")));
    assert_eq!(
        State::with_storage(state.storage.clone())
            .unwrap()
            .get("clipboard:old"),
        Some(json!("saved"))
    );
    state
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=OFF")
                .map_err(|e| e.to_string())
        })
        .unwrap();
}

#[test]
// Verify that lane users share fifo and debug lease preserves live and normal state
fn lane_users_share_fifo_and_debug_lease_preserves_live_and_normal_state() {
    let root = tempfile::tempdir().unwrap();
    let mut run = tests::fixture(root.path());
    let first = run.runtime.builtin_state.lane("control".into());
    let second = run.runtime.builtin_state.lane("control".into());
    assert!(Arc::ptr_eq(&first, &second));
    first.lock().unwrap().push_back("first".into());
    second.lock().unwrap().push_back("second".into());
    drop(first);
    assert_eq!(
        second.lock().unwrap().front().map(String::as_str),
        Some("first")
    );
    drop(second);
    drop(run.runtime.builtin_state.lane("other".into()));
    assert!(!run
        .runtime
        .builtin_state
        .lanes
        .lock()
        .unwrap()
        .contains_key("control"));

    execute(
        &mut run,
        &tests::action("cycle_actions", json!({"children":[{"kind":"none"}]})),
        0,
        vec![],
    )
    .unwrap();
    execute(
        &mut run,
        &tests::action(
            "cooldown",
            json!({"milliseconds":100,"children":[{"kind":"none"}]}),
        ),
        0,
        vec![],
    )
    .unwrap();
    let state = &run.runtime.builtin_state;
    state
        .history
        .lock()
        .unwrap()
        .insert("device".into(), vec![("profile".into(), "page".into())]);
    let lease = DebugStateLease::new(&run.runtime, "owner");
    let branch = lease.clone();
    let mut taps = vec![];
    for key in ["debug:owner|control", "debug:owner-other|control"] {
        state.indices.lock().unwrap().insert(key.into(), 1);
        state
            .cooldowns
            .lock()
            .unwrap()
            .insert(key.into(), Instant::now());
        taps.push(
            state
                .taps
                .register(key.into(), Instant::now(), Duration::from_secs(60))
                .unwrap(),
        );
    }
    drop(lease);
    assert!(state
        .indices
        .lock()
        .unwrap()
        .contains_key("debug:owner|control"));
    drop(branch);
    assert!(!state
        .indices
        .lock()
        .unwrap()
        .contains_key("debug:owner|control"));
    assert!(!state
        .cooldowns
        .lock()
        .unwrap()
        .contains_key("debug:owner|control"));
    assert!(!state.taps.contains("debug:owner|control"));
    assert!(state.taps.contains("debug:owner-other|control"));
    assert!(state
        .indices
        .lock()
        .unwrap()
        .contains_key("debug:owner-other|control"));
    assert!(state
        .indices
        .lock()
        .unwrap()
        .contains_key(&format!("{}:[]", run.key())));
    assert!(state
        .cooldowns
        .lock()
        .unwrap()
        .contains_key(&format!("{}:[]", run.key())));
    assert_eq!(state.history.lock().unwrap()["device"].len(), 1);
    run.runtime.shutdown();
}

#[test]
// Verify that cancelling live debug retires only its execution state
fn cancelling_live_debug_retires_only_its_execution_state() {
    let root = tempfile::tempdir().unwrap();
    let run = tests::fixture(root.path());
    let mut request = debug_request();
    request.action = Action::Macro {
        actions: vec![
            tests::action("cycle_actions", json!({"children":[{"kind":"none"}]})),
            tests::action(
                "cooldown",
                json!({"milliseconds":100,"children":[{"kind":"none"}]}),
            ),
            Action::Delay { milliseconds: 1000 },
        ],
    };
    let id = run.runtime.start_macro_debug(request).unwrap().id;
    run.runtime
        .macro_debug_command(&id, DebugCommand::Continue)
        .unwrap();
    let prefix = format!("debug:{id}|");
    wait(|| {
        run.runtime
            .builtin_state
            .cooldowns
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.starts_with(&prefix))
    });
    run.runtime
        .macro_debug_command(&id, DebugCommand::Stop)
        .unwrap();
    wait(|| run.runtime.macro_debug_view(&id, 0).unwrap().status == DebugStatus::Finished);
    wait(|| {
        !run.runtime
            .builtin_state
            .indices
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.starts_with(&prefix))
            && !run
                .runtime
                .builtin_state
                .cooldowns
                .lock()
                .unwrap()
                .keys()
                .any(|key| key.starts_with(&prefix))
    });
    assert_eq!(
        run.runtime.macro_debug_view(&id, 0).unwrap().outcome,
        Some(crate::actions::RunOutcome::Cancelled)
    );
    run.runtime.shutdown();
}
