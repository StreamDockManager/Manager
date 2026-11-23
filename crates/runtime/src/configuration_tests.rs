use crate::{Page, Runtime};

#[test]
// Verify that default configuration root matches the platform application identity
fn default_configuration_root_matches_the_platform_application_identity() {
    let base = directories::BaseDirs::new().unwrap();
    #[cfg(target_os = "macos")]
    let expected = base
        .config_dir()
        .join("app.streamdockmanager.Stream-Dock-Manager");
    #[cfg(target_os = "windows")]
    let expected = base
        .config_dir()
        .join("streamdockmanager")
        .join("Stream Dock Manager")
        .join("config");
    #[cfg(target_os = "linux")]
    let expected = base.config_dir().join("stream-dock-manager");
    assert_eq!(Runtime::default_root().unwrap(), expected);
}

#[test]
fn fresh_runtime_places_only_live_databases_in_the_database_directory() {
    use std::sync::Arc;
    use stream_dock_manager_storage::{database_directory, database_path, DATABASE_NAME};

    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("disposable-config");
    let runtime = Runtime::build_credentials(
        config.clone(),
        false,
        crate::credentials::Credentials::with_provider(Arc::new(
            crate::vault::tests::TestKeys::default(),
        )),
    )
    .unwrap();
    let db_dir = database_directory(&config);
    assert!(db_dir.is_dir());
    assert!(database_path(&config, DATABASE_NAME).is_file());
    assert!(database_path(&config, "datastore.sqlite3").is_file());
    assert!(!database_path(&config, "keystore.sqlite3").exists());
    runtime.cache_status().unwrap();
    assert!(database_path(&config, "cache.sqlite3").is_file());
    runtime.vault.initialize().unwrap();
    assert!(database_path(&config, "keystore.sqlite3").is_file());
    assert!(std::fs::read_dir(&db_dir).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".keystore-key-")
    }));
    for file in [
        DATABASE_NAME,
        "datastore.sqlite3",
        "keystore.sqlite3",
        "cache.sqlite3",
    ] {
        assert!(!config.join(file).exists());
    }
    assert!(!config.join("recovery").exists());
    assert!(config.join("profiles/writer.lock").is_file());
    assert!(!db_dir.join("profiles/writer.lock").exists());
    assert!(runtime
        .profiles
        .storage()
        .backup(database_path(&config, DATABASE_NAME))
        .is_err());
    runtime.shutdown();
    drop(runtime);
}

#[test]
// Verify that startup recreates a deleted database without replacing preserved backups
fn startup_recreates_a_deleted_database_without_replacing_preserved_backups() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().to_owned();
    let runtime = Runtime::build(config.clone(), false).unwrap();
    let initial = runtime.snapshot();
    let mut profile = initial.profiles[0].clone();
    profile.name = "Old profile".into();
    runtime
        .save_profile(profile, initial.configuration_revision)
        .unwrap();
    runtime
        .save_client_preference("offline_icons", serde_json::json!(true))
        .unwrap();
    runtime.shutdown();
    drop(runtime);

    let backups = std::fs::read_dir(stream_dock_manager_storage::recovery_directory(&config))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sqlite3")
        })
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect::<Vec<_>>();
    assert!(!backups.is_empty());
    std::fs::remove_file(stream_dock_manager_storage::database_path(
        &config,
        stream_dock_manager_storage::DATABASE_NAME,
    ))
    .unwrap();
    let fresh = Runtime::build(config.clone(), false).unwrap();
    assert!(stream_dock_manager_storage::database_path(
        &config,
        stream_dock_manager_storage::DATABASE_NAME
    )
    .exists());
    assert!(fresh
        .snapshot()
        .profiles
        .iter()
        .all(|profile| profile.name != "Old profile"));
    fresh.shutdown();
    drop(fresh);
    for (path, bytes) in backups {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
#[ignore = "Requires local loopback sockets for the plugin host; never opens HID"]
// Verify that device output is validated isolated and restored after restart
fn device_output_is_validated_isolated_and_restored_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    runtime.simulate_device(true, "simulator:first").unwrap();
    runtime.simulate_device(true, "simulator:second").unwrap();
    let output = crate::workspace::DeviceOutput {
        keyboard: None,
        sleeping: false,
        brightness: 42,
        lighting_brightness: 70,
        lighting_colors: vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [0, 0, 0]],
        vibration_enabled: Some(true),
        device_configuration: runtime.snapshot().devices[0]
            .manifest
            .capabilities
            .vibration
            .as_ref()
            .unwrap()
            .configuration_fields
            .iter()
            .enumerate()
            .map(|(i, field)| (field.id.clone(), i % 2 == 0))
            .collect(),
        frame_policy: None,
    };
    let snapshot = runtime
        .set_device_output("simulator:first", output.clone())
        .unwrap();
    assert_eq!(
        snapshot.session("simulator:first").output.lighting_colors,
        output.lighting_colors
    );
    assert_eq!(snapshot.session("simulator:second").output.brightness, 50);
    assert!(snapshot
        .session("simulator:second")
        .output
        .vibration_enabled
        .is_none());
    assert_eq!(snapshot.configuration_revision, 1);
    let mut invalid = output.clone();
    invalid.brightness = 101;
    assert!(runtime
        .set_device_output("simulator:first", invalid)
        .is_err());
    let mut invalid = output.clone();
    invalid.lighting_colors.pop();
    assert!(runtime
        .set_device_output("simulator:first", invalid)
        .is_err());
    assert!(runtime
        .set_device_output("missing", output.clone())
        .is_err());
    assert_eq!(
        runtime
            .snapshot()
            .session("simulator:first")
            .output
            .brightness,
        42
    );
    runtime.brightness("simulator:first", 50).unwrap();
    assert_eq!(
        runtime
            .snapshot()
            .session("simulator:first")
            .output
            .lighting_colors,
        output.lighting_colors
    );
    let stopped = std::sync::Arc::downgrade(&runtime);
    runtime.shutdown();
    drop(runtime);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while stopped.strong_count() > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(stopped.strong_count(), 0, "runtime workers did not stop");
    // Arc's strong count reaches zero before its destructor finishes on a worker thread
    // Observe the actual configuration lock release before reopening the same store
    let reopen_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let runtime = loop {
        match Runtime::build(root.path().into(), false) {
            Ok(runtime) => break runtime,
            Err(error)
                if error.starts_with("Another Stream Dock Manager process")
                    && std::time::Instant::now() < reopen_deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            Err(error) => panic!("Could not reopen stopped runtime: {error}"),
        }
    };
    let snapshot = runtime.simulate_device(true, "simulator:first").unwrap();
    assert_eq!(
        snapshot
            .session("simulator:first")
            .output
            .device_configuration,
        output.device_configuration
    );
    assert_eq!(
        snapshot.session("simulator:first").output.vibration_enabled,
        Some(true)
    );
    assert_eq!(snapshot.session("simulator:first").output.brightness, 50);
    assert_eq!(
        snapshot
            .session("simulator:first")
            .output
            .lighting_brightness,
        70
    );
    assert_eq!(
        snapshot.session("simulator:first").output.lighting_colors,
        output.lighting_colors
    );
    runtime.shutdown();
}

#[test]
// Verify that vibration requires complete review and rejects known older firmware
fn vibration_requires_complete_review_and_rejects_known_older_firmware() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let id = "simulator:n4-pro:configuration";
    runtime.simulate_device(true, id).unwrap();
    let mut output = runtime.snapshot().session(id).output;
    output.vibration_enabled = Some(true);
    assert!(runtime
        .set_device_output(id, output.clone())
        .unwrap_err()
        .contains("Review and choose"));
    output.device_configuration = runtime.snapshot().devices[0]
        .manifest
        .capabilities
        .vibration
        .as_ref()
        .unwrap()
        .configuration_fields
        .iter()
        .map(|field| (field.id.clone(), false))
        .collect();
    let mut unknown = output.clone();
    unknown
        .device_configuration
        .insert("unexpected".into(), true);
    assert!(runtime.set_device_output(id, unknown).is_err());
    {
        let mut state = runtime.state.lock().unwrap();
        let device = state.devices.iter_mut().find(|d| d.id == id).unwrap();
        device.simulated = false; // Controlled fixture only; never opens a HID device
        device.variant_id = "mirabox".into();
    }
    runtime
        .connection_epochs
        .lock()
        .unwrap()
        .insert(id.into(), 42);
    runtime.observe_firmware(id, 42, Ok(Some("V4.N4 Pro.02.005".into())));
    assert!(runtime
        .set_device_output(id, output.clone())
        .unwrap_err()
        .contains("02.006"));
    assert!(runtime
        .snapshot()
        .session(id)
        .output
        .vibration_enabled
        .is_none());
    runtime.observe_firmware(id, 42, Ok(Some("V4.N4 Pro.02.006".into())));
    // No hardware worker runs in this controlled runtime. Valid choices must
    // remain saved for reconnect even when dispatch reports a lost transport
    let error = runtime.set_device_output(id, output.clone()).unwrap_err();
    assert_eq!(error, "Device service is stopping or unavailable");
    assert_eq!(
        runtime.snapshot().session(id).output.device_configuration,
        output.device_configuration
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets for the plugin host; never opens HID"]
// Verify that deletion persists fallbacks and rejects stale or failed writes
fn deletion_persists_fallbacks_and_rejects_stale_or_failed_writes() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let runtime = Runtime::build(config.clone(), false).unwrap();
    let initial = runtime.snapshot();
    let mut copy = initial.profiles[0].clone();
    copy.id = "copy".into();
    copy.pages.push(Page {
        id: "extra".into(),
        name: "Extra".into(),
        bindings: Default::default(),
    });
    copy.active_page = "extra".into();
    runtime
        .save_profile(copy, initial.configuration_revision)
        .unwrap();
    let snapshot = runtime.activate_profile("copy").unwrap();
    assert!(runtime
        .delete_page("copy", "extra", initial.configuration_revision)
        .is_err());
    assert!(runtime
        .delete_profile("copy", initial.configuration_revision)
        .is_err());
    assert_eq!(runtime.snapshot().profiles[1].pages.len(), 2);

    // A failed disk write must leave the selected profile and in-memory configuration intact
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(runtime
        .delete_profile("copy", snapshot.configuration_revision)
        .is_err());
    assert_eq!(runtime.snapshot().revision, snapshot.revision);
    assert_eq!(runtime.snapshot().active_profile, "copy");
    assert_eq!(runtime.snapshot().profiles.len(), 2);
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=OFF")
                .map_err(|error| error.to_string())
        })
        .unwrap();

    let snapshot = runtime
        .delete_page("copy", "extra", snapshot.configuration_revision)
        .unwrap();
    let data = runtime.profiles.load().unwrap();
    assert_eq!(data.profiles[1].active_page, "main");
    assert_eq!(data.profiles[1].pages.len(), 1);
    assert!(runtime
        .delete_page("copy", "main", snapshot.configuration_revision)
        .is_err());
    assert_eq!(runtime.snapshot().revision, snapshot.revision);

    let snapshot = runtime
        .delete_profile("copy", snapshot.configuration_revision)
        .unwrap();
    assert_eq!(snapshot.active_profile, "default");
    assert_eq!(snapshot.profiles.len(), 1);
    let snapshot = runtime
        .delete_profile("default", snapshot.configuration_revision)
        .unwrap();
    assert!(snapshot.profiles.is_empty());
    assert!(snapshot.active_profile.is_empty());
    let data = runtime.profiles.load().unwrap();
    assert!(data.active_profile.is_empty());
    assert!(data.profiles.is_empty());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets for the plugin host; never opens HID"]
// Verify that deleting last profiles clears device sessions and allows recreation
fn deleting_last_profiles_clears_device_sessions_and_allows_recreation() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let first = "simulator:n4-pro:first";
    let second = "simulator:mini:second";
    runtime.simulate_device(true, first).unwrap();
    let snapshot = runtime.simulate_device(true, second).unwrap();
    let original = snapshot
        .profiles
        .iter()
        .find(|p| p.id == "default")
        .unwrap()
        .clone();
    let other = snapshot.session(second).profile_id;
    let snapshot = runtime
        .delete_profile(&other, snapshot.configuration_revision)
        .unwrap();
    assert_eq!(snapshot.session(first).profile_id, "default");
    assert!(snapshot.session(second).profile_id.is_empty());
    assert!(snapshot.session(second).page_id.is_empty());
    assert!(snapshot.session(second).manual_profile.is_empty());
    assert!(runtime
        .preview_surfaces(Some(second))
        .unwrap()
        .surfaces
        .is_empty());
    runtime.simulate_device(false, second).unwrap();
    let snapshot = runtime.simulate_device(true, second).unwrap();
    assert_eq!(
        snapshot.profiles.len(),
        1,
        "reconnecting must not recreate a deleted profile"
    );
    let snapshot = runtime
        .delete_profile("default", snapshot.configuration_revision)
        .unwrap();
    assert!(snapshot.profiles.is_empty());
    for id in [first, second] {
        assert!(snapshot.session(id).profile_id.is_empty());
        let preview = runtime.preview_surfaces(Some(id)).unwrap();
        assert!(preview.surfaces.is_empty());
        assert!(preview.background.is_none());
        assert!(!preview.animated);
    }
    let imported = root.path().join("import.zip");
    {
        let source = crate::ProfileStore::open(root.path().join("source")).unwrap();
        source.load().unwrap();
        source.export(&imported).unwrap();
    }
    let preview = runtime.prepare_profile_import(&imported).unwrap();
    let snapshot = runtime
        .commit_profile_import(&preview.id, snapshot.configuration_revision)
        .unwrap();
    assert_eq!(snapshot.active_profile, "default");
    assert_eq!(snapshot.session(first).profile_id, "default");
    assert!(snapshot.session(second).profile_id.is_empty());
    let snapshot = runtime
        .delete_profile("default", snapshot.configuration_revision)
        .unwrap();
    let snapshot = runtime
        .save_profile(original, snapshot.configuration_revision)
        .unwrap();
    assert_eq!(snapshot.active_profile, "default");
    assert_eq!(snapshot.session(first).profile_id, "default");
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets for the plugin host; never opens HID"]
// Verify that delayed macros yield and device navigation does not conflict with edits
fn delayed_macros_yield_and_device_navigation_does_not_conflict_with_edits() {
    use crate::actions::{Action, Request};
    use std::time::{Duration, Instant};
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let initial = runtime.snapshot();
    let mut first = initial.profiles[0].clone();
    first.id = "first".into();
    let saved = runtime
        .save_profile(first, initial.configuration_revision)
        .unwrap();
    let mut second = saved.profiles[0].clone();
    second.id = "second".into();
    let saved = runtime
        .save_profile(second, saved.configuration_revision)
        .unwrap();
    runtime
        .connection_epochs
        .lock()
        .unwrap()
        .insert("test-device".into(), 1);
    let request = |control: &str, action: Action| Request {
        event_slot: None,
        physical_activation: None,
        manual: false,
        activation_policy: Default::default(),
        icon_signature: None,
        pressed: None,
        action,
        queued_at: Instant::now(),
        device_id: "test-device".into(),
        control_id: control.into(),
        binding_scope: "default:main".into(),
        connection_epoch: Some(1),
        input: hardware_core::Input::Pulse,
        trace: None,
        cancellation_generation: 0,
    };
    runtime
        .action_sender
        .send(request(
            "key-1",
            Action::Macro {
                actions: vec![
                    Action::Delay { milliseconds: 400 },
                    Action::Profile {
                        profile_id: "first".into(),
                    },
                ],
            },
        ))
        .unwrap();
    runtime
        .action_sender
        .send(request(
            "key-2",
            Action::Profile {
                profile_id: "second".into(),
            },
        ))
        .unwrap();
    let start = Instant::now();
    while runtime
        .snapshot()
        .sessions
        .get("test-device")
        .is_none_or(|s| s.profile_id != "second")
        && start.elapsed() < Duration::from_millis(250)
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        runtime.snapshot().sessions["test-device"].profile_id,
        "second"
    );
    assert!(start.elapsed() < Duration::from_millis(350));
    assert_eq!(
        runtime.snapshot().configuration_revision,
        saved.configuration_revision
    );
    let mut profile = runtime.snapshot().profiles[0].clone();
    profile.name = "Edited after navigation".into();
    runtime
        .save_profile(profile, saved.configuration_revision)
        .unwrap();
    let start = Instant::now();
    while runtime.snapshot().sessions["test-device"].profile_id != "first"
        && start.elapsed() < Duration::from_secs(1)
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        runtime.snapshot().sessions["test-device"].profile_id,
        "first"
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets for the plugin host; never opens HID"]
// Verify that application rules are ordered and manual selection pauses until resumed
fn application_rules_are_ordered_and_manual_selection_pauses_until_resumed() {
    use crate::{
        workspace::{AppRule, Edit},
        DeviceInfo,
    };
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let initial = runtime.snapshot();
    let mut work = initial.profiles[0].clone();
    work.id = "work".into();
    let mut personal = work.clone();
    personal.id = "personal".into();
    let saved = runtime
        .apply_edits(
            initial.configuration_revision,
            "Profiles",
            vec![
                Edit::SaveProfile { profile: work },
                Edit::SaveProfile { profile: personal },
            ],
        )
        .unwrap();
    let manifest = hardware_registry::modules()[0].manifest().clone();
    runtime.state.lock().unwrap().devices.push(DeviceInfo {
        id: "a".into(),
        persistence_key: "a".into(),
        identity_port_dependent: false,
        model_id: "n4-pro".into(),
        name: "A".into(),
        variant_id: "fixture".into(),
        firmware: "fixture".into(),
        usb_release: None,
        connected: true,
        simulated: false,
        manifest,
    });
    let rules = vec![
        AppRule {
            title: None,
            schedule: None,
            id: "first".into(),
            device_key: "a".into(),
            application: "fixture.application".into(),
            profile_id: "work".into(),
            page_id: None,
        },
        AppRule {
            title: None,
            schedule: None,
            id: "second".into(),
            device_key: "a".into(),
            application: "fixture.application".into(),
            profile_id: "personal".into(),
            page_id: None,
        },
    ];
    let saved = runtime
        .apply_edits(
            saved.configuration_revision,
            "Rules",
            vec![Edit::Rules { rules }],
        )
        .unwrap();
    // This fixture supplies foreground observations, so capabilities must not depend
    // on the runner's desktop session (for example, Wayland disables detection)
    *runtime.automation_status.lock().unwrap() = crate::automation::AutomationStatus {
        available: false,
        schedules_available: true,
        ..Default::default()
    };
    runtime.set_automation("a", true, false).unwrap();
    crate::automation::evaluate(&runtime, "fixture.application");
    assert_eq!(runtime.snapshot().session("a").profile_id, "default");
    runtime.automation_status.lock().unwrap().available = true;
    crate::automation::evaluate(&runtime, "fixture.application");
    assert_eq!(runtime.snapshot().session("a").profile_id, "work");
    assert_eq!(
        runtime.snapshot().configuration_revision,
        saved.configuration_revision
    );
    runtime
        .select_device_profile("a", "personal", None, true)
        .unwrap();
    crate::automation::evaluate(&runtime, "fixture.application");
    assert_eq!(runtime.snapshot().session("a").profile_id, "personal");
    assert!(runtime.snapshot().session("a").automation_paused);
    runtime.set_automation("a", true, false).unwrap();
    crate::automation::evaluate(&runtime, "fixture.application");
    assert_eq!(runtime.snapshot().session("a").profile_id, "work");
    crate::automation::evaluate(&runtime, "unmatched.application");
    assert_eq!(runtime.snapshot().session("a").profile_id, "personal");
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets for the isolated plugin host; never opens HID"]
// Verify that activation policies keep active run and classify pending work
fn activation_policies_keep_active_run_and_classify_pending_work() {
    use crate::actions::{Action, ActivationPolicy, Request};
    use std::time::{Duration, Instant};
    for (policy, expected) in [
        (ActivationPolicy::Queue, [3, 0, 0]),
        (ActivationPolicy::IgnoreBusy, [1, 2, 0]),
        (ActivationPolicy::ReplacePending, [2, 0, 1]),
    ] {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert("fixture".into(), 1);
        let events = runtime.subscribe();
        for index in 0..3 {
            runtime
                .action_sender
                .send(Request {
                    event_slot: None,
                    physical_activation: None,
                    manual: false,
                    activation_policy: policy,
                    icon_signature: None,
                    pressed: None,
                    action: if index == 0 {
                        Action::Delay { milliseconds: 350 }
                    } else {
                        Action::None
                    },
                    queued_at: Instant::now(),
                    device_id: "fixture".into(),
                    control_id: "key-1".into(),
                    binding_scope: "default:main".into(),
                    connection_epoch: Some(1),
                    input: hardware_core::Input::Pulse,
                    trace: None,
                    cancellation_generation: 0,
                })
                .unwrap();
        }
        let mut outcomes = vec![];
        let deadline = Instant::now() + Duration::from_secs(4);
        while outcomes.len() < 3 && Instant::now() < deadline {
            if let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
                if event.kind == "action-run" {
                    let value: serde_json::Value =
                        serde_json::from_str(event.message.as_deref().unwrap()).unwrap();
                    assert!(uuid::Uuid::parse_str(value["run_id"].as_str().unwrap()).is_ok());
                    outcomes.push(value["outcome"].as_str().unwrap().to_string());
                }
            }
        }
        assert_eq!(outcomes.len(), 3, "{policy:?}: {outcomes:?}");
        let counts = ["completed", "ignored", "superseded"]
            .map(|status| outcomes.iter().filter(|value| *value == status).count());
        assert_eq!(counts, expected, "{policy:?}: {outcomes:?}");
        runtime.shutdown();
    }
}

#[test]
fn owned_second_press_waits_for_cleanup_and_distinguishes_restart_from_cancel() {
    use crate::actions::{Action, ActivationPolicy, Request};
    use std::time::{Duration, Instant};
    for policy in [ActivationPolicy::Restart, ActivationPolicy::Cancel] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(dir.path().into(), false).unwrap();
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert("fixture".into(), 1);
        let events = runtime.subscribe();
        let press = |delay| {
            runtime
                .action_sender
                .send(Request {
                    event_slot: None,
                    physical_activation: None,
                    manual: false,
                    activation_policy: policy,
                    icon_signature: None,
                    pressed: None,
                    action: Action::Delay {
                        milliseconds: delay,
                    },
                    queued_at: Instant::now(),
                    device_id: "fixture".into(),
                    control_id: "key-1".into(),
                    binding_scope: "default:main".into(),
                    connection_epoch: Some(1),
                    input: hardware_core::Input::Pulse,
                    trace: None,
                    cancellation_generation: 0,
                })
                .unwrap();
        };
        press(2000);
        press(1);
        let mut outcomes = vec![];
        let end = Instant::now() + Duration::from_secs(4);
        while outcomes.len() < 2 && Instant::now() < end {
            if let Ok(e) = events.recv_timeout(Duration::from_millis(50)) {
                if e.kind == "action-run" {
                    let v: serde_json::Value = serde_json::from_str(&e.message.unwrap()).unwrap();
                    outcomes.push(v["outcome"].as_str().unwrap().to_owned());
                }
            }
        }
        if policy == ActivationPolicy::Restart {
            assert_eq!(outcomes, ["cancelled", "completed"]);
        } else {
            outcomes.sort();
            assert_eq!(outcomes, ["cancelled", "ignored"]);
        }
        runtime.shutdown();
    }
}

#[test]
// Default missing screen preferences without changing saved extremes or unrelated outputs
fn screen_brightness_defaults_to_fifty_and_preserves_saved_values() {
    let defaults = crate::workspace::DeviceOutput::default();
    assert_eq!(defaults.brightness, 50);
    assert_eq!(defaults.lighting_brightness, 15);
    assert!(!defaults.sleeping);
    let mut saved = serde_json::to_value(&defaults).unwrap();
    saved.as_object_mut().unwrap().remove("brightness");
    let missing: crate::workspace::DeviceOutput = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(missing.brightness, 50);
    for brightness in [0, 65, 100] {
        saved["brightness"] = serde_json::json!(brightness);
        saved["sleeping"] = serde_json::json!(true);
        saved["lighting_brightness"] = serde_json::json!(73);
        let output: crate::workspace::DeviceOutput = serde_json::from_value(saved.clone()).unwrap();
        assert_eq!(output.brightness, brightness);
        assert_eq!(output.lighting_brightness, 73);
        assert!(output.sleeping);
        assert_eq!(serde_json::to_value(output).unwrap(), saved);
    }
}
