//! Synthetic persistence and input compatibility regressions. No HID is opened
use crate::{actions::Action, profiles::ProfileStore, Runtime};
use hardware_core::{ControlEvent, Input};
use std::time::{Duration, Instant};

// Wait for until
fn wait_until(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(5);
    while !ready() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(ready(), "Timed out waiting for synthetic macro");
}

#[test]
// Verify that corrected controls preserve saved artwork and action associations
fn corrected_controls_preserve_saved_artwork_and_action_associations() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("app");
    let store = ProfileStore::open(config.join("profiles")).unwrap();
    let source = root.path().join("artwork.svg");
    let artwork = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><path fill="#ff0000" d="M0 0h4v4H0z"/></svg>"##;
    std::fs::write(&source, artwork).unwrap();
    let asset = store.import_asset(&source).unwrap();
    let mut data = store.load().unwrap();
    let template = data.profiles[0].clone();
    let binding = template.pages[0].bindings["key-1"].clone();
    for model in ["n3", "293s", "293s-v3", "293", "293-v3", "m18", "m3", "xl"] {
        let mut profile = template.clone();
        profile.id = format!("current-{model}");
        profile.name = profile.id.clone();
        profile.model_id = model.into();
        profile.pages[0].bindings.clear();
        let keys: Vec<_> = match model {
            "n3" => (7..=9).collect(),
            "293s" | "293s-v3" => (16..=18).collect(),
            "xl" => (1..=32).collect(),
            _ => (1..=15).collect(),
        };
        for key in keys {
            let mut value = binding.clone();
            value.title = format!("Saved key {key}");
            value.asset = Some(asset.clone());
            value.action = if model.starts_with("293s") {
                Action::None
            } else {
                Action::Brightness { value: key }
            };
            profile.pages[0]
                .bindings
                .insert(format!("key-{key}"), value);
        }
        data.profiles.push(profile);
    }
    // Current display-only surfaces preserve artwork but never contain actions
    store.save(&data).unwrap();
    drop(store);
    let runtime = Runtime::build(config.clone(), false).unwrap();
    let expected = runtime.snapshot().profiles;
    for profile in expected.iter().filter(|p| p.id.starts_with("current-")) {
        for (id, binding) in &profile.pages[0].bindings {
            let key: u8 = id.strip_prefix("key-").unwrap().parse().unwrap();
            if profile.model_id.starts_with("293s") {
                assert!(matches!(binding.action, Action::None));
            } else {
                assert!(matches!(binding.action, Action::Brightness { value } if value == key));
            }
            assert_eq!(binding.asset.as_deref(), Some(asset.as_str()));
        }
        let device = format!("simulator:{}:compatibility", profile.model_id);
        runtime.simulate_device(true, &device).unwrap();
        if profile.model_id == "n3" {
            let snapshot = runtime.snapshot();
            let manifest = &snapshot
                .devices
                .iter()
                .find(|d| d.id == device)
                .unwrap()
                .manifest;
            for key in 7..=9 {
                assert!(manifest
                    .control(&format!("key-{key}"))
                    .unwrap()
                    .image_slot
                    .is_none());
            }
        } else if profile.model_id.starts_with("293s") {
            for key in 16..=18 {
                assert!(runtime
                    .simulate_input(
                        &device,
                        ControlEvent {
                            control_id: format!("key-{key}"),
                            input: Input::Pulse
                        }
                    )
                    .unwrap_err()
                    .contains("display-only"));
            }
        }
    }
    let archive = root.path().join("profiles.zip");
    runtime.profiles.export(&archive).unwrap();
    let imported = ProfileStore::open(root.path().join("imported")).unwrap();
    let decoded = imported.import_for_repair(&archive).unwrap();
    for profile in expected.iter().filter(|p| p.id.starts_with("current-")) {
        let copy = decoded
            .profiles
            .iter()
            .find(|p| p.id == profile.id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&copy.pages).unwrap(),
            serde_json::to_value(&profile.pages).unwrap()
        );
    }
    assert_eq!(imported.asset(&asset).unwrap(), artwork);
    runtime.shutdown();
    drop(runtime);
    let reopened = Runtime::build(config, false).unwrap();
    for profile in expected.iter().filter(|p| p.id.starts_with("current-")) {
        let snapshot = reopened.snapshot();
        let copy = snapshot
            .profiles
            .iter()
            .find(|p| p.id == profile.id)
            .unwrap();
        assert_eq!(
            serde_json::to_value(&copy.pages).unwrap(),
            serde_json::to_value(&profile.pages).unwrap()
        );
    }
    assert_eq!(reopened.profiles.asset(&asset).unwrap(), artwork);
    reopened.shutdown();
}

#[test]
// Verify that encoder edges run press lane macro once per press release pair
fn encoder_edges_run_press_lane_macro_once_per_press_release_pair() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    for model in ["n3", "n4"] {
        let device = format!("simulator:{model}:encoder-compatibility");
        runtime.simulate_device(true, &device).unwrap();
        let snapshot = runtime.snapshot();
        let session = snapshot.session(&device);
        let mut profile = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap()
            .clone();
        let page = profile
            .pages
            .iter_mut()
            .find(|p| p.id == session.page_id)
            .unwrap();
        let mut binding = snapshot.profiles[0].pages[0].bindings["key-1"].clone();
        let marker = root.path().join(format!("{model}.txt"));
        binding.action = Action::Dial {
            linked_turns: false,
            actions: vec![
                Action::None,
                Action::None,
                Action::Macro {
                    actions: vec![Action::Builtin {
                        action: Box::new(crate::builtins::BuiltinAction::WriteFile {
                            path: marker.to_string_lossy().into(),
                            text: "x".into(),
                            operation: "append".into(),
                            replace: false,
                            template: false,
                        }),
                    }],
                },
            ],
        };
        page.bindings.insert("dial-1".into(), binding);
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        for pressed in [true, false] {
            runtime
                .simulate_input(
                    &device,
                    ControlEvent {
                        control_id: "dial-1".into(),
                        input: Input::Button { pressed },
                    },
                )
                .unwrap();
        }
        wait_until(|| std::fs::read(&marker).is_ok());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(std::fs::read(&marker).unwrap(), b"x");
        for pressed in [true, false] {
            runtime
                .simulate_input(
                    &device,
                    ControlEvent {
                        control_id: "dial-1".into(),
                        input: Input::Button { pressed },
                    },
                )
                .unwrap();
        }
        wait_until(|| std::fs::read(&marker).is_ok_and(|bytes| bytes.len() >= 2));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(std::fs::read(&marker).unwrap(), b"xx");
    }
    runtime.shutdown();
}

#[test]
// Verify that dial actions round trip for every encoder model without changing legacy actions
fn dial_actions_round_trip_for_every_encoder_model_without_changing_legacy_actions() {
    let root = tempfile::tempdir().unwrap();
    let store = ProfileStore::open(root.path().join("profiles")).unwrap();
    let mut data = store.load().unwrap();
    let source = data.profiles[0].clone();
    let template = source.pages[0].bindings.values().next().unwrap().clone();
    let action = Action::Dial {
        linked_turns: false,
        actions: vec![
            Action::Volume { delta: -5 },
            Action::Macro {
                actions: vec![
                    Action::Volume { delta: 5 },
                    Action::Delay { milliseconds: 10 },
                ],
            },
            Action::Mute,
        ],
    };
    let mut checked = 0;
    for manifest in hardware_registry::manifests() {
        let Some(control) = manifest.controls.iter().find(|c| c.kind == "encoder") else {
            continue;
        };
        let mut profile = source.clone();
        profile.id = format!("dial-routing-{}", manifest.model_id);
        profile.model_id = manifest.model_id.clone();
        profile.pages.truncate(1);
        profile.active_page = profile.pages[0].id.clone();
        profile.pages[0].bindings.clear();
        let mut binding = template.clone();
        binding.action = action.clone();
        profile.pages[0]
            .bindings
            .insert(control.id.clone(), binding);
        profile.validate().unwrap();
        if let Some(button) = manifest.controls.iter().find(|c| c.kind != "encoder") {
            let mut invalid = profile.clone();
            let mut binding = invalid.pages[0].bindings.remove(&control.id).unwrap();
            invalid.pages[0]
                .bindings
                .insert(button.id.clone(), binding.clone());
            assert!(invalid.validate().unwrap_err().contains("encoder"));
            binding.action = Action::Macro {
                actions: vec![action.clone()],
            };
            invalid.pages[0].bindings.insert(button.id.clone(), binding);
            assert!(invalid.validate().unwrap_err().contains("encoder"));
        }
        data.profiles.push(profile);
        checked += 1;
    }
    assert!(checked > 1);
    store.save(&data).unwrap();
    let reloaded = store.load().unwrap();
    for profile in reloaded
        .profiles
        .iter()
        .filter(|p| p.id.starts_with("dial-routing-"))
    {
        assert_eq!(
            serde_json::to_value(
                &profile
                    .pages
                    .iter()
                    .find(|page| page.id == profile.active_page)
                    .unwrap()
                    .bindings
                    .values()
                    .next()
                    .unwrap()
                    .action
            )
            .unwrap(),
            serde_json::to_value(&action).unwrap()
        );
    }
    assert_eq!(
        serde_json::to_value(&reloaded.profiles[0]).unwrap(),
        serde_json::to_value(&source).unwrap()
    );
}
