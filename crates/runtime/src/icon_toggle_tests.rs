use crate::{actions::Action, feedback, Runtime};
use hardware_core::{
    appearance::{Appearance, ToggleIcon},
    ControlEvent, Input,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const DEVICE: &str = "simulator:icons";
const SCOPE: &str = "default:main";
// Wait for the icon toggle tests condition or result
fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !condition() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(condition(), "Timed out awaiting icon action");
}
// Construct an isolated icon toggle tests fixture
fn fixture(path: &std::path::Path, action: Action) -> Arc<Runtime> {
    let runtime = Runtime::build(path.to_owned(), false).unwrap();
    runtime.simulate_device(true, DEVICE).unwrap();
    let state = runtime.snapshot();
    let mut profile = state.profiles[0].clone();
    let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
    binding.action = action;
    binding.feedback = None; // Icon transitions must reach hardware without live text enabled
    binding.appearance = Some(Box::new(Appearance {
        icon: "play".into(),
        toggle_icon: Some(Box::new(ToggleIcon {
            icon: "pause".into(),
            icon_asset: None,
        })),
        ..Default::default()
    }));
    runtime
        .save_profile(profile, state.configuration_revision)
        .unwrap();
    runtime
}
// Report the toggled condition for icon toggle tests
fn toggled(runtime: &Runtime, device: &str) -> bool {
    let state = runtime.snapshot();
    feedback::icon_toggled(
        runtime,
        device,
        SCOPE,
        "key-1",
        &state.profiles[0].pages[0].bindings["key-1"],
    )
}
// Return the current icon toggle tests status
fn status(runtime: &Runtime) -> String {
    feedback::text(runtime, "macro", DEVICE, SCOPE, "key-1")
}
// Press the requested icon toggle tests input
fn press(runtime: &Runtime) {
    runtime
        .simulate_input(
            DEVICE,
            ControlEvent {
                control_id: "key-1".into(),
                input: Input::Pulse,
            },
        )
        .unwrap();
}
// Construct the alternating fixture for icon toggle tests
fn alternating(off: Action) -> Action {
    Action::Toggle {
        on: Box::new(Action::Brightness { value: 41 }),
        off: Box::new(off),
    }
}
#[test]
// Verify that icon toggle changes only after success and is scoped to the device
fn icon_toggle_changes_only_after_success_and_is_scoped_to_the_device() {
    let root = tempfile::tempdir().unwrap();
    let runtime = fixture(root.path(), alternating(Action::Brightness { value: 42 }));
    runtime.simulate_device(true, "simulator:other").unwrap();
    let initial = runtime.preview_surfaces(Some(DEVICE)).unwrap().surfaces["key-1"].clone();
    assert!(!toggled(&runtime, DEVICE));
    press(&runtime);
    wait(|| status(&runtime) == "Completed");
    assert!(toggled(&runtime, DEVICE));
    assert!(!toggled(&runtime, "simulator:other"));
    assert_ne!(
        runtime.preview_surfaces(Some(DEVICE)).unwrap().surfaces["key-1"],
        initial
    );
    press(&runtime);
    wait(|| status(&runtime) == "Completed");
    assert!(!toggled(&runtime, DEVICE));
    assert_eq!(
        runtime.preview_surfaces(Some(DEVICE)).unwrap().surfaces["key-1"],
        initial
    );
    runtime.shutdown();
}
#[test]
// Verify that icon toggle keeps its current icon when an action fails
fn icon_toggle_keeps_its_current_icon_when_an_action_fails() {
    let root = tempfile::tempdir().unwrap();
    let runtime = fixture(
        root.path(),
        alternating(Action::Profile {
            profile_id: "missing-profile".into(),
        }),
    );
    press(&runtime);
    wait(|| status(&runtime) == "Completed");
    assert!(toggled(&runtime, DEVICE));
    for _ in 0..2 {
        press(&runtime);
        wait(|| status(&runtime) == "Failed");
        assert!(toggled(&runtime, DEVICE));
    }
    runtime.shutdown();
}
#[test]
// Verify that icon toggle ignores cancelled runs changed bindings and reconnected devices
fn icon_toggle_ignores_cancelled_runs_changed_bindings_and_reconnected_devices() {
    let root = tempfile::tempdir().unwrap();
    let runtime = fixture(
        root.path(),
        alternating(Action::Delay { milliseconds: 300 }),
    );
    press(&runtime);
    wait(|| status(&runtime) == "Completed");
    press(&runtime);
    wait(|| status(&runtime) == "Running");
    assert!(toggled(&runtime, DEVICE));
    runtime.cancel_macros();
    wait(|| status(&runtime) == "Cancelled");
    assert!(toggled(&runtime, DEVICE));
    press(&runtime);
    wait(|| status(&runtime) == "Running");
    let state = runtime.snapshot();
    let mut profile = state.profiles[0].clone();
    profile.pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .appearance
        .as_mut()
        .unwrap()
        .toggle_icon
        .as_mut()
        .unwrap()
        .icon = "music".into();
    runtime
        .save_profile(profile, state.configuration_revision)
        .unwrap();
    wait(|| status(&runtime) == "Completed");
    assert!(!toggled(&runtime, DEVICE));
    press(&runtime);
    wait(|| status(&runtime) == "Completed");
    assert!(toggled(&runtime, DEVICE));
    runtime.simulate_device(false, DEVICE).unwrap();
    runtime.simulate_device(true, DEVICE).unwrap();
    assert!(!toggled(&runtime, DEVICE));
    runtime.shutdown();
}

#[test]
// Verify that icon toggle waits for detached parallel work and keeps the icon if a branch fails
fn icon_toggle_waits_for_detached_parallel_work_and_keeps_the_icon_if_a_branch_fails() {
    let root = tempfile::tempdir().unwrap();
    let runtime = fixture(
        root.path(),
        Action::Macro {
            actions: vec![
                Action::Builtin {
                    action: Box::new(crate::builtins::BuiltinAction::Parallel {
                        wait: false,
                        children: vec![Action::Macro {
                            actions: vec![
                                Action::Delay { milliseconds: 200 },
                                Action::Profile {
                                    profile_id: "missing".into(),
                                },
                            ],
                        }],
                    }),
                },
                Action::Brightness { value: 37 },
            ],
        },
    );
    press(&runtime);
    wait(|| runtime.snapshot().session(DEVICE).output.brightness == 37);
    assert!(!toggled(&runtime, DEVICE));
    wait(|| status(&runtime) == "Failed");
    assert!(!toggled(&runtime, DEVICE));
    runtime.shutdown();
}

#[test]
// Verify that icon toggle waits for authenticated plugin success and ignores late cancelled replies
fn icon_toggle_waits_for_authenticated_plugin_success_and_ignores_late_cancelled_replies() {
    use serde_json::{json, Value};
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("plugins/fixture");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(package.join("manifest.json"), json!({
        "UUID":"fixture.icons", "Name":"Icon fixture", "Version":"0.1.0", "CodePath":"runtime.html",
        "Actions":[{"UUID":"fixture.icons.action","Name":"Action"}]
    }).to_string()).unwrap();
    crate::test_plugin::modernize(&package);
    let runtime = fixture(
        root.path(),
        Action::Plugin {
            user_desired_state: None,
            plugin_id: "fixture.icons".into(),
            action_id: "fixture.icons.action".into(),
            settings: json!({}),
            resources: Default::default(),
        },
    );
    runtime
        .plugins
        .lock()
        .unwrap()
        .enable("fixture.icons", true)
        .unwrap();
    crate::test_support::synchronize_plugins(&runtime);
    let url = {
        let mut plugins = runtime.plugins.lock().unwrap();
        plugins.poll();
        plugins.browser_view("fixture.icons").unwrap().unwrap().url
    };
    let parts: Vec<_> = url.split('/').collect();
    let token = parts[4];
    let (mut socket, _) = tungstenite::connect(format!("ws://{}", parts[2])).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    socket
        .send(tungstenite::Message::Text(
            crate::test_plugin::registration(&package, token)
                .to_string()
                .into(),
        ))
        .unwrap();
    let next = |socket: &mut tungstenite::WebSocket<
        tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
    >|
     -> Value {
        loop {
            let message = socket.read().unwrap();
            let Ok(text) = message.to_text() else {
                continue;
            };
            let value: Value = serde_json::from_str(text).unwrap();
            if value["event"] == "keyDown" {
                return value;
            }
        }
    };
    let reply = |socket: &mut tungstenite::WebSocket<
        tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
    >,
                 event: &Value,
                 success: bool| {
        socket.send(tungstenite::Message::Text(json!({"event":"actionResult","context":event["context"],"payload":{"request_id":event["streamDockManager"]["request_id"],"success":success}}).to_string().into())).unwrap();
    };
    press(&runtime);
    let first = next(&mut socket);
    assert!(!toggled(&runtime, DEVICE));
    assert_eq!(status(&runtime), "Running");
    reply(&mut socket, &first, true);
    wait(|| status(&runtime) == "Completed");
    assert!(toggled(&runtime, DEVICE));
    press(&runtime);
    let failed = next(&mut socket);
    reply(&mut socket, &failed, false);
    wait(|| status(&runtime) == "Failed");
    assert!(toggled(&runtime, DEVICE));
    press(&runtime);
    let cancelled = next(&mut socket);
    runtime.cancel_macros();
    wait(|| status(&runtime) == "Cancelled");
    reply(&mut socket, &cancelled, true);
    press(&runtime);
    let again = next(&mut socket);
    assert_ne!(
        again["streamDockManager"]["request_id"],
        cancelled["streamDockManager"]["request_id"]
    );
    assert!(toggled(&runtime, DEVICE));
    reply(&mut socket, &again, true);
    wait(|| status(&runtime) == "Completed");
    assert!(!toggled(&runtime, DEVICE));
    socket.close(None).unwrap();
    runtime.shutdown();
}

#[test]
// Verify that icon toggle follows successful pause and resume of the same binding
fn icon_toggle_follows_successful_pause_and_resume_of_the_same_binding() {
    let root = tempfile::tempdir().unwrap();
    let runtime = fixture(
        root.path(),
        Action::Builtin {
            action: Box::new(crate::builtins::BuiltinAction::PauseBindings {
                operation: "toggle".into(),
            }),
        },
    );
    press(&runtime);
    wait(|| status(&runtime) == "Completed");
    assert!(toggled(&runtime, DEVICE));
    press(&runtime); // Runtime resolves the unlock press to an explicit resume action
    wait(|| status(&runtime) == "Completed");
    assert!(!toggled(&runtime, DEVICE));
    runtime.shutdown();
}
