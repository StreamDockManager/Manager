use super::*;
// Submit the tests request
fn request() -> SetupCapabilityRequest {
    SetupCapabilityRequest {
        mode: SetupCapabilityMode::DeviceInput,
        device_id: Some("selected".into()),
        plugin_ids: vec![],
    }
}
// Handle the incoming tests input
fn input(pressed: bool) -> hardware_core::ControlEvent {
    hardware_core::ControlEvent {
        control_id: "key-1".into(),
        input: hardware_core::Input::Button { pressed },
    }
}
#[test]
// Verify that input observation is scoped to connection source and positive interaction
fn input_observation_is_scoped_to_connection_source_and_positive_interaction() {
    let checks = SetupCapabilities::default();
    let job = checks.reserve(request(), Some(42), false).unwrap();
    checks.observe_input("other", 42, false, &input(true));
    checks.observe_input("selected", 41, false, &input(true));
    checks.observe_input("selected", 42, true, &input(true));
    checks.observe_input("selected", 42, false, &input(false));
    assert!(checks.input(&job.id).is_none());
    checks.observe_input("selected", 42, false, &input(true));
    assert_eq!(checks.input(&job.id).as_deref(), Some("key-1"));
    checks.finish(&job.id, Ok(()));
    assert_eq!(checks.report(&job.id).unwrap().state, "complete");
}
#[test]
// Verify that cancelled and expired checks ignore late input and retention is bounded
fn cancelled_and_expired_checks_ignore_late_input_and_retention_is_bounded() {
    let checks = SetupCapabilities::default();
    let first = checks.reserve(request(), Some(1), true).unwrap();
    assert!(checks.reserve(request(), Some(1), true).is_err());
    checks.cancel(&first.id).unwrap();
    assert!(checks.reserve(request(), Some(1), true).is_err());
    checks.observe_input("selected", 1, true, &input(true));
    checks.append(&first.id, check("late", "Late", "ok", "ignored"));
    assert!(checks.input(&first.id).is_none());
    checks.finish(&first.id, Ok(()));
    assert_eq!(checks.report(&first.id).unwrap().state, "cancelled");
    assert!(checks.report(&first.id).unwrap().checks.is_empty());
    let expired = checks.reserve(request(), Some(1), true).unwrap();
    checks
        .state
        .lock()
        .unwrap()
        .jobs
        .back_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_secs(1);
    assert!(checks
        .control(&expired.id)
        .unwrap_err()
        .contains("deadline"));
    checks.observe_input("selected", 1, true, &input(true));
    assert!(checks.input(&expired.id).is_none());
    checks.finish(&expired.id, Err("deadline".into()));
    for _ in 0..20 {
        let job = checks.reserve(request(), Some(1), true).unwrap();
        checks.finish(&job.id, Ok(()));
    }
    assert_eq!(checks.state.lock().unwrap().jobs.len(), 8);
    assert!(checks.report(&first.id).is_err());
}
#[test]
// Verify that result fields are bounded without breaking unicode and shutdown cancels
fn result_fields_are_bounded_without_breaking_unicode_and_shutdown_cancels() {
    let checks = SetupCapabilities::default();
    let job = checks.reserve(request(), Some(1), true).unwrap();
    for index in 0..100 {
        checks.append(
            &job.id,
            check(
                &format!("id-{index}"),
                &"界".repeat(400),
                "ok",
                "界".repeat(4000),
            ),
        );
    }
    checks.finish(&job.id, Err("incomplete".into()));
    let report = checks.report(&job.id).unwrap();
    assert_eq!(report.checks.len(), 80);
    assert!(report.checks[0].detail.len() <= 2051);
    assert!(report.checks[0].title.len() <= 259);
    let next = checks.reserve(request(), Some(1), true).unwrap();
    checks.cancel_all();
    assert!(checks.control(&next.id).is_err());
}
#[test]
#[ignore = "Requires disposable loopback host sockets; never opens HID or dispatches actions"]
// Verify that native input check cancels and rejects a reconnected simulator without workspace edits
fn native_input_check_cancels_and_rejects_a_reconnected_simulator_without_workspace_edits() {
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::test_support::headless(root.path().into()).unwrap();
    let snapshot = runtime.simulate(true).unwrap();
    let device = snapshot
        .devices
        .iter()
        .find(|device| device.simulated)
        .unwrap();
    let mut req = request();
    req.device_id = Some(device.id.clone());
    let before = runtime.snapshot();
    let job = runtime.start_setup_capabilities(req.clone()).unwrap();
    let epoch = runtime.connection_epochs.lock().unwrap()[&device.id];
    runtime
        .setup_capabilities
        .observe_input(&device.id, epoch + 1, true, &input(true));
    assert!(runtime.setup_capabilities.input(&job.id).is_none());
    runtime.cancel_setup_capabilities(&job.id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime
        .setup_capabilities
        .state
        .lock()
        .unwrap()
        .active
        .is_some()
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        runtime.setup_capability_status(&job.id).unwrap().state,
        "cancelled"
    );
    let next = runtime.start_setup_capabilities(req).unwrap();
    runtime
        .connection_epochs
        .lock()
        .unwrap()
        .insert(device.id.clone(), epoch + 1);
    loop {
        let report = runtime.setup_capability_status(&next.id).unwrap();
        if report.state != "running" {
            assert_eq!(report.state, "failed");
            assert!(report
                .checks
                .iter()
                .any(|check| check.detail.contains("reconnected")));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let after = runtime.snapshot();
    assert_eq!(before.configuration_revision, after.configuration_revision);
    assert_eq!(
        serde_json::to_value(before.history).unwrap(),
        serde_json::to_value(after.history).unwrap()
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires disposable loopback host sockets; reads native permissions and a configured helper version, without prompts/capture/input/HID"]
// Verify that native passive readiness does not select runtimes or edit workspace
fn native_passive_readiness_does_not_select_runtimes_or_edit_workspace() {
    let root = tempfile::tempdir().unwrap();
    let runtime = crate::test_support::headless(root.path().into()).unwrap();
    let before = runtime.snapshot();
    let runtimes = serde_json::to_value(runtime.runtimes.status()).unwrap();
    let request = SetupCapabilityRequest {
        mode: SetupCapabilityMode::Passive,
        device_id: None,
        plugin_ids: vec![],
    };
    let job = runtime.start_setup_capabilities(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let report = runtime.setup_capability_status(&job.id).unwrap();
        if report.state != "running" {
            assert_eq!(report.state, "complete");
            assert!(report.checks.iter().any(|item| item.id == "ffmpeg"));
            assert!(report.qualification.contains("do not establish physical"));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        runtimes,
        serde_json::to_value(runtime.runtimes.status()).unwrap()
    );
    let after = runtime.snapshot();
    assert_eq!(before.configuration_revision, after.configuration_revision);
    assert_eq!(
        serde_json::to_value(before.history).unwrap(),
        serde_json::to_value(after.history).unwrap()
    );
    runtime.shutdown();
}

#[test]
// Verify that editor retirement waits for cancelled capability worker to finish
fn editor_retirement_waits_for_cancelled_capability_worker_to_finish() {
    let checks = SetupCapabilities::default();
    let job = checks.reserve(request(), Some(42), false).unwrap();
    assert!(checks.editor_retirement_blocked());
    checks.cancel(&job.id).unwrap();
    assert!(checks.editor_retirement_blocked());
    checks.finish(&job.id, Ok(()));
    assert!(!checks.editor_retirement_blocked());
}
