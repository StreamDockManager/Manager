use super::*;
// Load the registered N4 Pro capability fixture for power-policy tests
fn caps() -> DeviceCapabilities {
    hardware_registry::manifests()
        .into_iter()
        .find(|model| model.model_id == "n4-pro")
        .unwrap()
        .capabilities
}
// Construct an enabled test policy that dims on idle and sleeps on lock
fn enabled() -> DevicePowerPolicy {
    DevicePowerPolicy {
        enabled: true,
        idle_action: PowerAction::Dim,
        lock_action: PowerAction::Sleep,
        ..Default::default()
    }
}
#[test]
// Verify that defaults disabled and policy output never overwrites the manual baseline
fn defaults_disabled_and_policy_output_never_overwrites_the_manual_baseline() {
    let mut state = DecisionState::default();
    let baseline = DeviceOutput::default();
    let sample = SensorSample {
        idle: Some(Duration::from_secs(3600)),
        locked: Some(true),
    };
    let now = Instant::now();
    assert_eq!(
        state
            .evaluate(&Default::default(), &baseline, &caps(), &sample, 0, 0, now)
            .effective,
        baseline
    );
    let dim = state.evaluate(
        &enabled(),
        &baseline,
        &caps(),
        &SensorSample {
            idle: sample.idle,
            locked: Some(false),
        },
        0,
        0,
        now,
    );
    assert_eq!(dim.effective.brightness, 20);
    assert_eq!(baseline.brightness, 50);
    let sleep = state.evaluate(&enabled(), &baseline, &caps(), &sample, 0, 0, now);
    assert!(sleep.automatic_sleeping);
    assert!(!sleep.dimmed);
    assert_eq!(sleep.effective.brightness, 50);
    let mut manual = baseline.clone();
    manual.sleeping = true;
    state.wake(now);
    let result = state.evaluate(
        &enabled(),
        &manual,
        &caps(),
        &SensorSample {
            idle: Some(Duration::ZERO),
            locked: Some(false),
        },
        0,
        0,
        now,
    );
    assert!(result.effective.sleeping);
    assert!(!result.automatic_sleeping);
}
#[test]
// Verify that wake overrides only current conditions and new conditions still apply
fn wake_overrides_only_current_conditions_and_new_conditions_still_apply() {
    let now = Instant::now();
    let mut state = DecisionState::default();
    let baseline = DeviceOutput::default();
    let mut policy = enabled();
    policy.schedule_action = PowerAction::Sleep;
    let locked = SensorSample {
        idle: Some(Duration::ZERO),
        locked: Some(true),
    };
    assert!(
        state
            .evaluate(&policy, &baseline, &caps(), &locked, 0, 12 * 60, now)
            .effective
            .sleeping
    );
    state.wake(now);
    assert!(
        !state
            .evaluate(&policy, &baseline, &caps(), &locked, 0, 12 * 60, now)
            .effective
            .sleeping
    );
    assert!(
        state
            .evaluate(&policy, &baseline, &caps(), &locked, 0, 23 * 60, now)
            .effective
            .sleeping
    );
    state.evaluate(
        &policy,
        &baseline,
        &caps(),
        &SensorSample {
            idle: Some(Duration::ZERO),
            locked: Some(false),
        },
        0,
        12 * 60,
        now,
    );
    assert!(
        state
            .evaluate(&policy, &baseline, &caps(), &locked, 0, 12 * 60, now)
            .effective
            .sleeping
    );
}
#[test]
// Verify that dimming caps brightness and device interaction restarts idle without undoing manual sleep
fn dimming_caps_brightness_and_device_interaction_restarts_idle_without_undoing_manual_sleep() {
    let now = Instant::now();
    let mut state = DecisionState::default();
    let baseline = DeviceOutput {
        brightness: 5,
        lighting_brightness: 65,
        ..Default::default()
    };
    let idle = SensorSample {
        idle: Some(Duration::from_secs(3600)),
        locked: Some(false),
    };
    let dim = state.evaluate(&enabled(), &baseline, &caps(), &idle, 0, 0, now);
    assert_eq!(dim.effective.brightness, 5);
    assert_eq!(dim.effective.lighting_brightness, 20);
    state.interaction(now);
    assert_eq!(
        state
            .evaluate(&enabled(), &baseline, &caps(), &idle, 0, 0, now)
            .effective,
        baseline
    );
    assert!(
        state
            .evaluate(
                &enabled(),
                &baseline,
                &caps(),
                &idle,
                0,
                0,
                now + Duration::from_secs(601)
            )
            .dimmed
    );
}
#[test]
// Verify that unavailable sensors do not invent conditions or clear an existing suppression
fn unavailable_sensors_do_not_invent_conditions_or_clear_an_existing_suppression() {
    let now = Instant::now();
    let mut state = DecisionState::default();
    let baseline = DeviceOutput::default();
    assert!(
        !state
            .evaluate(
                &enabled(),
                &baseline,
                &caps(),
                &Default::default(),
                0,
                0,
                now
            )
            .effective
            .sleeping
    );
    state.evaluate(
        &enabled(),
        &baseline,
        &caps(),
        &SensorSample {
            idle: None,
            locked: Some(true),
        },
        0,
        0,
        now,
    );
    let unavailable = state.evaluate(
        &enabled(),
        &baseline,
        &caps(),
        &Default::default(),
        0,
        0,
        now,
    );
    assert!(unavailable.effective.sleeping);
    assert_eq!(unavailable.unavailable.len(), 2);
    state.wake(now);
    assert!(
        !state
            .evaluate(
                &enabled(),
                &baseline,
                &caps(),
                &Default::default(),
                0,
                0,
                now
            )
            .effective
            .sleeping
    );
    let mut off = enabled();
    off.enabled = false;
    assert_eq!(
        state
            .evaluate(&off, &baseline, &caps(), &Default::default(), 0, 0, now)
            .effective,
        baseline
    );
}
#[test]
// Verify that schedule crosses midnight and unsupported outputs are explicitly unavailable
fn schedule_crosses_midnight_and_unsupported_outputs_are_explicitly_unavailable() {
    let now = Instant::now();
    let mut state = DecisionState::default();
    let baseline = DeviceOutput::default();
    let policy = DevicePowerPolicy {
        enabled: true,
        schedule_action: PowerAction::Sleep,
        schedule: Schedule {
            days: vec![0],
            start: 22 * 60,
            end: 7 * 60,
        },
        ..Default::default()
    };
    assert!(
        state
            .evaluate(
                &policy,
                &baseline,
                &caps(),
                &Default::default(),
                1,
                6 * 60,
                now
            )
            .effective
            .sleeping
    );
    assert!(
        !state
            .evaluate(
                &policy,
                &baseline,
                &caps(),
                &Default::default(),
                1,
                7 * 60,
                now
            )
            .effective
            .sleeping
    );
    let mut unavailable = caps();
    unavailable.sleep = false;
    unavailable.brightness = false;
    unavailable.lighting = false;
    let result = state.evaluate(
        &policy,
        &baseline,
        &unavailable,
        &Default::default(),
        0,
        23 * 60,
        now,
    );
    assert!(!result.effective.sleeping);
    assert_eq!(result.unavailable.len(), 1);
}
#[test]
// Verify that malformed policies and release only inputs are rejected
fn malformed_policies_and_release_only_inputs_are_rejected() {
    assert!(DevicePowerPolicy::default().validate().is_ok());
    let mut policy = enabled();
    policy.idle_minutes = 0;
    assert!(policy.validate().is_err());
    policy.idle_minutes = 10;
    policy.schedule.days = vec![0, 0];
    assert!(policy.validate().is_err());
    assert!(!positive(&Input::Button { pressed: false }));
    assert!(!positive(&Input::Rotate { delta: 0 }));
    assert!(positive(&Input::Pulse));
}
