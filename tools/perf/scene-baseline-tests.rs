#[cfg(test)]
mod followup_scene_baseline {
    use super::*;
    #[test]
    #[ignore = "release-only deterministic source performance fixture; no HID or plugin actions"]
    // Verify scene snapshot baseline
    fn scene_snapshot_baseline() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        {
            let mut state = runtime.state.lock().unwrap();
            let base = state.profiles[0].clone();
            for index in 1..1000 {
                let mut profile = base.clone();
                profile.id = format!("fixture-{index}");
                state.profiles.push(profile);
            }
        }
        for repetition in 0..6 {
            let started = std::time::Instant::now();
            for _cycle in 0..100 {
                for _surface in 0..10 {
                    let snapshot = runtime.snapshot();
                    std::hint::black_box(snapshot.session("fixture-device"));
                    std::hint::black_box(snapshot);
                }
            }
            if repetition > 0 {
                println!("FOLLOWUP_SCENE {{\"variant\":\"baseline\",\"repetition\":{repetition},\"cycles\":100,\"surfaces\":10,\"profiles\":1000,\"snapshot_calls\":1000,\"elapsed_ms\":{}}}", started.elapsed().as_secs_f64()*1000.0);
            }
        }
        runtime.shutdown();
    }
}
#[cfg(test)]
mod followup_plugin_burst_baseline {
    use super::*;
    // Install burst fixture
    fn install_burst_fixture(runtime: &Runtime) {
        let mut state = runtime.state.lock().unwrap();
        let mut profile = state.profiles[0].clone();
        profile.id = "fixture-profile".into();
        profile.model_id = "n4-pro".into();
        profile.active_page = "fixture-page".into();
        let bindings = (1..=10)
            .map(|index| {
                (
                    format!("key-{index}"),
                    crate::Binding {
                        activation_policy: Default::default(),
                        title: format!("Fixture key-{index}"),
                        color: "#336699".into(),
                        action: crate::actions::Action::None,
                        asset: None,
                        media: Default::default(),
                        appearance: None,
                        plugin_appearance: None,
                        feedback: None,
                        disabled: false,
                    },
                )
            })
            .collect();
        profile.pages = vec![crate::Page {
            id: "fixture-page".into(),
            name: "Fixture".into(),
            bindings,
        }];
        state.active_profile = profile.id.clone();
        state.profiles = vec![profile];
        state.sessions.clear();
    }

    #[test]
    #[ignore = "release-only five-field host visual burst fixture; no plugin process or HID"]
    // Verify plugin render burst baseline
    fn plugin_render_burst_baseline() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "n4-pro")
            .unwrap();
        install_burst_fixture(&runtime);
        let snapshot = runtime.snapshot();
        let profile = &snapshot.profiles[0];
        let page = &profile.pages[0];
        let scope = format!("{}:{}", profile.id, page.id);
        // The pre-change drain renders once for every hostVisual message. This
        // fixture isolates that actual renderer call, excluding SDK/USB latency.
        for repetition in 0..6 {
            let started = std::time::Instant::now();
            let mut renders = 0;
            for cycle in 0..10 {
                for control in manifest
                    .controls
                    .iter()
                    .filter(|c| c.image_slot.is_some())
                    .take(10)
                {
                    let mut owned = page.bindings.get(&control.id).unwrap().clone();
                    owned.title = format!("Fixture {cycle} {}", control.id);
                    owned.color = "#336699".into();
                    let binding = Some(&owned);
                    let mut previous = None;
                    for _field in [
                        "setFeedbackLayout",
                        "setState",
                        "setImage",
                        "setTitle",
                        "setFeedback",
                    ] {
                        let bytes = runtime
                            .render_bound_surface(
                                control,
                                binding,
                                manifest.layout.rotation,
                                "fixture-device",
                                &scope,
                                &control.id,
                            )
                            .unwrap();
                        if repetition == 0 {
                            let pixels = image::load_from_memory(&bytes).unwrap().to_rgba8();
                            assert!(pixels.pixels().any(|p| p[0] != 0 || p[1] != 0 || p[2] != 0));
                        }
                        if repetition == 0 {
                            if let Some(old) = previous.as_ref() {
                                assert_eq!(&bytes, old);
                            }
                            previous = Some(bytes);
                        } else {
                            std::hint::black_box(bytes);
                        }
                        renders += 1;
                    }
                }
            }
            if repetition > 0 {
                println!("FOLLOWUP_BURST {{\"variant\":\"baseline\",\"repetition\":{repetition},\"cycles\":10,\"surfaces\":10,\"visual_effects\":500,\"render_calls\":{renders},\"elapsed_ms\":{}}}", started.elapsed().as_secs_f64()*1000.0);
            }
        }
        runtime.shutdown();
    }
}
