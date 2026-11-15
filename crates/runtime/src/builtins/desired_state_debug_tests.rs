use super::*;
use crate::macro_debug::{DebugCommand, DebugMode, DebugStart, DebugStatus};

#[test]
// Verify that named vendor root live debug matches saved run without shifting debug paths
fn named_vendor_root_live_debug_matches_saved_run_without_shifting_debug_paths() {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("app/plugins/fixture.desired.sdPlugin");
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
    let mut run = tests::fixture(root.path());
    let desired = Action::Plugin {
        plugin_id: "fixture.desired".into(),
        action_id: "fixture.desired.action".into(),
        settings: json!({}),
        resources: Default::default(),
        user_desired_state: Some(1),
    };
    // Demand must come from saved workspace state. A one-off host demand is
    // replaced by the simulator's normal reconciliation and cannot authorize runs
    let snapshot = run.runtime.snapshot();
    let mut profile = snapshot
        .profiles
        .iter()
        .find(|p| p.id == "default")
        .unwrap()
        .clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = desired.clone();
    run.runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    run.macros = run.macros.with_draft(SavedMacro {
        id: "desired".into(),
        name: "Desired".into(),
        inputs: Default::default(),
        action: desired.clone(),
    });
    {
        let mut host = run.runtime.plugins.lock().unwrap();
        host.enable("fixture.desired", true).unwrap();
        host.reconcile_demand(HashMap::from([(
            "fixture.desired".into(),
            vec![stream_dock_manager_plugin_host::PluginDemand {
                device_id: run.request.device_id.clone(),
                profile_id: "default".into(),
            }],
        )]));
        host.poll();
    }
    let call = tests::action("saved_macro", json!({"macro_id":"desired"}));
    crate::devices::sync_plugins(&run.runtime);
    run.runtime.plugins.lock().unwrap().poll();
    crate::devices::sync_plugins(&run.runtime);
    execute(&mut run, &call, 0, vec![]).unwrap();
    assert_eq!(run.plugin_scope(&[0]), "default:main@0");
    run.debug_macro_root = true;
    assert_eq!(run.plugin_scope(&[]), "default:main@0");
    assert_eq!(run.plugin_scope(&[1, 2]), "default:main@0.1.2");
    let view = run
        .runtime
        .start_macro_debug(DebugStart {
            action: desired,
            mode: DebugMode::Live,
            start_running: false,
            device_id: run.request.device_id.clone(),
            profile_id: "default".into(),
            page_id: "main".into(),
            control_id: "key-1".into(),
            inputs: vec![],
            arguments: Default::default(),
            macro_id: Some("desired".into()),
            breakpoints: vec![vec![]],
            simulation: Default::default(),
        })
        .unwrap();
    let wait = |predicate: fn(&crate::macro_debug::DebugView) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let view = run.runtime.macro_debug_view(&view.id, 0).unwrap();
            if predicate(&view) {
                break view;
            }
            assert!(Instant::now() < deadline, "Debug session stalled: {view:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let paused = wait(|view| view.status == DebugStatus::Paused);
    assert_eq!(paused.paused_paths, vec![Vec::<usize>::new()]);
    run.runtime
        .macro_debug_command(&view.id, DebugCommand::Continue)
        .unwrap();
    let finished = wait(|view| view.status == DebugStatus::Finished);
    assert_eq!(
        finished.outcome,
        Some(crate::actions::RunOutcome::Completed),
        "{:?}",
        finished.events
    );
    assert!(finished
        .events
        .iter()
        .any(|event| event.path.is_empty() && event.phase == "completed"));
    assert!(finished.events.iter().all(|event| event.path.is_empty()));
    // Debug teardown already retired every prefixed context owned by this session
    assert!(run
        .runtime
        .plugins
        .lock()
        .unwrap()
        .clear_action_scope_suffix(&format!(":debug:{}", view.id))
        .is_empty());
    run.runtime.shutdown();
}
