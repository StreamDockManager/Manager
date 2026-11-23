//! Controlled native CPU/memory/child-process qualification; no physical devices
use serde_json::json;
use std::{
    path::Path,
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{MeasurementState, RuntimeState};
use stream_dock_manager_runtime::actions::Action;
// Run the plugin monitor example entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::args()
        .nth(1)
        .ok_or("Pass the controlled native_fixture executable")?;
    let temporary = tempfile::tempdir()?;
    let runtime =
        stream_dock_manager_runtime::test_support::headless(temporary.path().join("manager"))?;
    runtime
        .plugins
        .lock()
        .unwrap()
        .set_registration_environment(
            stream_dock_manager_plugin_host::RegistrationEnvironment::new(Some("en"), Some(1.0))?,
        );
    let ids: Vec<_> = (0..3).map(|i| format!("org.test.monitor.{i}")).collect();
    for id in &ids {
        let source = temporary.path().join(format!("{id}.sdPlugin"));
        std::fs::create_dir(&source)?;
        let entry = if cfg!(windows) {
            "plugin.exe"
        } else {
            "plugin"
        };
        std::fs::copy(Path::new(&executable), source.join(entry))?;
        std::fs::write(
            source.join("icon.svg"),
            include_bytes!("../../../tests/fixtures/plugins/browser/icon.svg"),
        )?;
        std::fs::write(source.join("manifest.json"), json!({"UUID":id,"Name":id,"Author":"Fixture","Icon":"icon.svg","OS":[{"Platform":"mac"},{"Platform":"linux"},{"Platform":"windows"}],"Version":"0.1.0","CodePath":entry,"Actions":[{"UUID":format!("{id}.action"),"Name":"Fixture","Icon":"icon.svg","States":[{"Image":"icon.svg"}],"Controllers":["Keypad"]}]}).to_string())?;
        let review = runtime.prepare_plugin_package(&source)?;
        assert!(review.can_import && review.can_run);
        runtime.commit_plugin_package(&review.id, false, false)?;
        runtime.plugins.lock().unwrap().enable(id, true)?;
    }
    assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
    runtime.simulate_device(true, "simulator:monitor")?;
    let mut profile = runtime.snapshot().profiles[0].clone();
    let keys: Vec<_> = profile.pages[0]
        .bindings
        .keys()
        .filter(|id| id.starts_with("key-"))
        .take(3)
        .cloned()
        .collect();
    assert_eq!(keys.len(), 3);
    for (id, key) in ids.iter().zip(keys) {
        profile.pages[0].bindings.get_mut(&key).unwrap().action = Action::Plugin {
            plugin_id: id.clone(),
            action_id: format!("{id}.action"),
            settings: json!({}),
            resources: Default::default(),
            user_desired_state: None,
        };
    }
    runtime.save_profile(profile, runtime.snapshot().configuration_revision)?;
    runtime.plugin_monitor_visible(true);
    stream_dock_manager_runtime::test_support::synchronize_plugins(&runtime);
    let revision = runtime.snapshot().configuration_revision;
    let until = Instant::now() + Duration::from_secs(15);
    let measured = loop {
        let changed = runtime.plugins.lock().unwrap().poll();
        if changed {
            stream_dock_manager_runtime::test_support::synchronize_plugins(&runtime);
        }
        let snapshots = runtime.plugin_runtime_snapshot();
        if snapshots.iter().all(|s| {
            s.runtime.state == RuntimeState::Running
                && s.runtime.resources.state == MeasurementState::Available
                && s.runtime.resources.process_count.unwrap_or(0) >= 2
        }) {
            break snapshots;
        }
        if Instant::now() > until {
            let sampling = runtime
                .metrics
                .snapshot()
                .into_iter()
                .filter(|metric| metric.name == "plugin_sample_ms")
                .collect::<Vec<_>>();
            return Err(format!(
                "Measurement timed out; completed sampling metrics: {sampling:?}; snapshots: {snapshots:?}"
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    for snapshot in &measured {
        assert!(snapshot.runtime.resources.resident_bytes.unwrap() >= 24 * 1024 * 1024);
        assert!(snapshot
            .runtime
            .resources
            .cpu_percent
            .is_some_and(|c| c > 0. && c <= 100.));
    }
    assert_eq!(runtime.snapshot().configuration_revision, revision);
    runtime.simulate_device(false, "simulator:monitor")?;
    stream_dock_manager_runtime::test_support::synchronize_plugins(&runtime);
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        runtime.plugins.lock().unwrap().poll();
        if runtime
            .plugin_runtime_snapshot()
            .iter()
            .all(|s| s.runtime.state == RuntimeState::Idle)
        {
            break;
        }
        if Instant::now() > until {
            return Err("Owned process trees did not stop".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let stopped = runtime.plugin_runtime_snapshot();
    for snapshot in &stopped {
        assert!(snapshot.enabled);
        assert_eq!(
            snapshot.runtime.resources.state,
            MeasurementState::NotRunning
        );
        assert!(snapshot.runtime.resources.root_pid.is_none());
        assert_eq!(snapshot.health.totals.crashes, 0);
    }
    let metrics = runtime.metrics.snapshot();
    runtime.shutdown();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,"measurements":measured,"stopped":stopped,"overhead":metrics})
        )?
    );
    Ok(())
}
