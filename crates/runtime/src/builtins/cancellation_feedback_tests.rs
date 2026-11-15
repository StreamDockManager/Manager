use super::*;
use crate::actions::{NavigationDirection, NavigationTarget};

// Construct the physical fixture
fn physical_fixture(root: &std::path::Path) -> Run {
    let run = tests::fixture(root);
    let snapshot = run.runtime.snapshot();
    let session = snapshot.session(&run.request.device_id);
    let mut profile = snapshot
        .profiles
        .iter()
        .find(|p| p.id == session.profile_id)
        .unwrap()
        .clone();
    let mut page = profile.pages[0].clone();
    page.id = "second-page".into();
    profile.pages.push(page);
    run.runtime
        .save_profile(profile.clone(), snapshot.configuration_revision)
        .unwrap();
    profile.id = "second-profile".into();
    run.runtime
        .save_profile(profile, run.runtime.snapshot().configuration_revision)
        .unwrap();
    // Exercise physical transition admission without opening any hardware
    run.runtime
        .state
        .lock()
        .unwrap()
        .devices
        .iter_mut()
        .find(|d| d.id == run.request.device_id)
        .unwrap()
        .simulated = false;
    run
}

// Construct the queued outcome fixture for cancellation feedback tests
fn queued_outcome(run: &Run, action: Action) -> Vec<crate::RuntimeEvent> {
    let events = run.runtime.subscribe();
    let mut request = (*run.request).clone();
    request.action = action;
    request.input = Input::Rotate { delta: 1 };
    request.queued_at = Instant::now();
    run.runtime.action_sender.send(request).unwrap();
    let mut captured = vec![];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut finished = false;
    loop {
        if let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            finished |= event.kind == "action-run";
            captured.push(event);
        }
        if finished && run.runtime.builtin_state.active.load(Ordering::SeqCst) == 0 {
            captured.extend(events.try_iter());
            return captured;
        }
        assert!(Instant::now() < deadline, "action did not finish");
    }
}

#[test]
// Verify that physical page and profile navigation cancel old run without error toast
fn physical_page_and_profile_navigation_cancel_old_run_without_error_toast() {
    for action in [
        Action::Page {
            page_id: "second-page".into(),
        },
        Action::Profile {
            profile_id: "second-profile".into(),
        },
        Action::Navigate {
            target: NavigationTarget::Page,
            direction: NavigationDirection::Next,
        },
        Action::Navigate {
            target: NavigationTarget::Profile,
            direction: NavigationDirection::Next,
        },
    ] {
        let root = tempfile::tempdir().unwrap();
        let run = physical_fixture(root.path());
        let events = queued_outcome(
            &run,
            Action::Dial {
                linked_turns: false,
                actions: vec![Action::None, action, Action::None],
            },
        );
        assert!(events.iter().any(|e| e.kind == "sessions"));
        assert!(events
            .iter()
            .any(|e| e.kind == "action-run" && e.message.as_ref().unwrap().contains("cancelled")));
        assert!(
            !events.iter().any(|e| e.kind == "action-error"),
            "{events:?}"
        );
        run.runtime.shutdown();
    }
}

#[test]
// Verify that navigation in multiaction stops old page steps without error toast
fn navigation_in_multiaction_stops_old_page_steps_without_error_toast() {
    let root = tempfile::tempdir().unwrap();
    let run = physical_fixture(root.path());
    let events = queued_outcome(
        &run,
        Action::Macro {
            actions: vec![
                Action::Navigate {
                    target: NavigationTarget::Page,
                    direction: NavigationDirection::Next,
                },
                Action::Page {
                    page_id: "must-not-run".into(),
                },
            ],
        },
    );
    assert_eq!(
        run.runtime
            .snapshot()
            .session(&run.request.device_id)
            .page_id,
        "second-page"
    );
    assert!(
        !events.iter().any(|e| e.kind == "action-error"),
        "{events:?}"
    );
    assert!(events
        .iter()
        .any(|e| e.kind == "action-run" && e.message.as_ref().unwrap().contains("cancelled")));
    run.runtime.shutdown();
}

#[test]
// Verify that invalid navigation still reports real error
fn invalid_navigation_still_reports_real_error() {
    let root = tempfile::tempdir().unwrap();
    let run = physical_fixture(root.path());
    let events = queued_outcome(
        &run,
        Action::Page {
            page_id: "missing".into(),
        },
    );
    assert!(events
        .iter()
        .any(|e| e.kind == "action-error" && e.message.as_deref() == Some("Page missing")));
    run.runtime.shutdown();
}

#[test]
// Verify that cancellation race does not mask real execution failure
fn cancellation_race_does_not_mask_real_execution_failure() {
    let root = tempfile::tempdir().unwrap();
    let run = physical_fixture(root.path());
    let events = run.runtime.subscribe();
    run.runtime
        .builtin_state
        .cancel_device(&run.request.device_id);
    let result = run.completion_result(Err("Actual failure".into()));
    assert_eq!(result, Err("Actual failure".into()));
    run.report_execution_error(result.as_ref().unwrap_err());
    assert!(events
        .try_iter()
        .any(|e| e.kind == "action-error" && e.message.as_deref() == Some("Actual failure")));
    assert_eq!(run.completion_result(Ok(())), Err("Cancelled".into()));
    run.report_execution_error("Cancelled");
    assert!(!events.try_iter().any(|e| e.kind == "action-error"));
    run.runtime.shutdown();
}

#[test]
// Verify that unrecognized cancelled error is not silenced
fn unrecognized_cancelled_error_is_not_silenced() {
    let root = tempfile::tempdir().unwrap();
    let run = physical_fixture(root.path());
    let events = run.runtime.subscribe();
    run.report_execution_error("Cancelled");
    assert!(events.try_iter().any(|e| e.kind == "action-error"));
    run.runtime.shutdown();
}
