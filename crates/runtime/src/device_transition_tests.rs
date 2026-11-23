use super::*;
use serde_json::json;

type CapturedReports = Arc<Mutex<Vec<Vec<u8>>>>;

// Construct an isolated device transition tests fixture
fn fixture() -> (tempfile::TempDir, Arc<Runtime>, Connection, CapturedReports) {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let (mut c, reports) =
        crate::devices::frame_policy_tests::connection_for_model(&runtime, "n4-pro");
    c.manifest
        .controls
        .retain(|c| ["key-1", "key-2", "touch-1", "dial-1"].contains(&c.id.as_str()));
    c.manifest.capabilities.background_size = Some([32, 32]);
    crate::render_scene::install_burst_fixture(&runtime);
    {
        let mut state = runtime.state.lock().unwrap();
        let mut page = state.profiles[0].pages[0].clone();
        page.id = "b".into();
        page.bindings.remove("key-1");
        state.profiles[0].pages.push(page);
        state.devices.push(DeviceInfo {
            id: c.device_id.clone(),
            persistence_key: c.device_id.clone(),
            identity_port_dependent: true,
            model_id: c.manifest.model_id.clone(),
            name: "Transition fixture".into(),
            variant_id: c.manifest.variants[0].id.clone(),
            firmware: String::new(),
            usb_release: None,
            connected: true,
            simulated: false,
            manifest: c.manifest.clone(),
        });
        state.sessions.insert(
            c.device_id.clone(),
            crate::workspace::DeviceSession {
                profile_id: "fixture-profile".into(),
                page_id: "fixture-page".into(),
                output: crate::workspace::DeviceOutput {
                    frame_policy: Some(crate::workspace::DeviceFramePolicy {
                        policy: OutputPolicy {
                            max_batch_bytes: 1,
                            max_batch_surfaces: 1,
                            max_batch_estimated_ms: 1,
                            ..Default::default()
                        },
                        encoding: FrameEncoding::IndividualUpdates,
                    }),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    }
    runtime
        .connection_epochs
        .lock()
        .unwrap()
        .insert(c.device_id.clone(), c.epoch);
    (root, runtime, c, reports)
}
// Construct the images fixture for device transition tests
fn images(reports: &[Vec<u8>]) -> usize {
    reports
        .iter()
        .filter(|r| r.get(6..9) == Some(b"BAT") || r.get(6..11) == Some(b"BGPIC"))
        .count()
}
// Finalize the pending device transition tests operation
fn finish(runtime: &Runtime, c: &mut Connection) {
    crate::devices::tests::finish_transition(runtime, c);
    assert!(c.worker.wait_idle(Duration::from_secs(3)));
}
#[test]
// Verify that static scene has no plugin wait and no partial images during preparation
fn static_scene_has_no_plugin_wait_and_no_partial_images_during_preparation() {
    let (_root, runtime, mut c, reports) = fixture();
    restore(&runtime, &mut c);
    assert!(c.worker.wait_idle(Duration::from_secs(2)));
    assert_eq!(images(&reports.lock().unwrap()), 0);
    advance(&runtime, &mut c);
    let transition = c.transition.as_ref().unwrap();
    assert!(
        transition.context.is_some(),
        "static scenes freeze immediately"
    );
    assert!(transition.receipt.is_none());
    assert_eq!(images(&reports.lock().unwrap()), 0);
    let event = ControlEvent {
        control_id: "key-1".into(),
        input: Input::Button { pressed: true },
    };
    assert!(runtime
        .transition_inputs
        .lock()
        .unwrap()
        .discard(&c.device_id, c.epoch, &event));
    finish(&runtime, &mut c);
    let reports = reports.lock().unwrap();
    assert!(images(&reports) >= 3);
    assert_eq!(
        reports
            .iter()
            .filter(|r| r.get(6..9) == Some(b"STP"))
            .count(),
        2
    );
    assert!(runtime
        .transition_inputs
        .lock()
        .unwrap()
        .pending(&c.device_id, c.epoch)
        .is_none());
    assert!(runtime
        .transition_inputs
        .lock()
        .unwrap()
        .discard(&c.device_id, c.epoch, &event));
    drop(reports);
    runtime.shutdown();
}
#[test]
fn navigation_trace_only_counts_completed_revisions_from_its_input() {
    for supersede in [false, true] {
        let (_root, runtime, mut c, _) = fixture();
        let token = runtime.arm_input_trace(&c.device_id).unwrap().token;
        let event = ControlEvent {
            control_id: "key-1".into(),
            input: Input::Button { pressed: true },
        };
        assert_eq!(
            runtime
                .input_trace
                .ingress_at(
                    &c.device_id,
                    c.epoch,
                    &event,
                    Instant::now() - Duration::from_millis(10)
                )
                .as_ref(),
            Some(&token)
        );
        runtime
            .select_device_profile_checked(
                &c.device_id,
                "fixture-profile",
                Some("b"),
                false,
                None,
                None,
                None,
                None,
                Some(&token),
            )
            .unwrap();
        runtime
            .input_trace
            .record(Some(&token), "Action complete", true);
        assert!(runtime.read_input_trace(&token).unwrap().outcome.is_none());
        if supersede {
            runtime
                .select_device_profile(&c.device_id, "fixture-profile", Some("fixture-page"), true)
                .unwrap();
        }
        restore(&runtime, &mut c);
        finish(&runtime, &mut c);
        c.collect_outcomes(&runtime);
        let trace = runtime.read_input_trace(&token).unwrap();
        let measured = runtime
            .metrics
            .snapshot()
            .into_iter()
            .find(|m| m.name == "input_to_final_write_ms");
        if supersede {
            assert!(measured.is_none());
        } else {
            assert!(measured.unwrap().p50.unwrap() >= 10.);
            assert!(trace.steps.iter().any(|s| s.contains("write completed")));
            assert_eq!(trace.outcome.as_deref(), Some("Action complete"));
        }
        runtime.shutdown();
    }
}

#[test]
// Verify that rapid navigation replaces prepared scene even when returning to same page
fn rapid_navigation_replaces_prepared_scene_even_when_returning_to_same_page() {
    let (_root, runtime, mut c, reports) = fixture();
    restore(&runtime, &mut c);
    advance(&runtime, &mut c);
    let first = c.transition.as_ref().unwrap().generation;
    runtime
        .select_device_profile(&c.device_id, "fixture-profile", Some("b"), true)
        .unwrap();
    runtime
        .select_device_profile(&c.device_id, "fixture-profile", Some("fixture-page"), true)
        .unwrap();
    advance(&runtime, &mut c);
    assert_ne!(c.transition.as_ref().unwrap().generation, first);
    assert_eq!(images(&reports.lock().unwrap()), 0);
    finish(&runtime, &mut c);
    assert_eq!(
        reports
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.get(6..9) == Some(b"STP"))
            .count(),
        2
    );
    runtime.shutdown();
}
#[test]
// Verify that sleep cancels pending scene and failure does not release input
fn sleep_cancels_pending_scene_and_failure_does_not_release_input() {
    let (_root, runtime, mut c, reports) = fixture();
    restore(&runtime, &mut c);
    advance(&runtime, &mut c);
    let id = c.device_id.clone();
    let epoch = c.epoch;
    let mut connections = HashMap::from([(id.clone(), c)]);
    dispatch_command(&runtime, &mut connections, &id, epoch, DeviceCommand::Sleep).unwrap();
    let c = connections.get_mut(&id).unwrap();
    assert!(c.transition.is_none());
    assert_eq!(images(&reports.lock().unwrap()), 0);
    assert!(runtime
        .transition_inputs
        .lock()
        .unwrap()
        .pending(&id, epoch)
        .is_some());
    restore(&runtime, c);
    c.worker.disconnect();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !c.transition.as_ref().unwrap().failed {
        assert!(Instant::now() < deadline);
        advance(&runtime, c);
    }
    assert!(runtime
        .transition_inputs
        .lock()
        .unwrap()
        .pending(&id, epoch)
        .is_some());
    runtime.shutdown();
}
#[test]
// Verify that silent plugin wait is bounded and valid bursts settle before freezing
fn silent_plugin_wait_is_bounded_and_valid_bursts_settle_before_freezing() {
    let (root, runtime, mut c, reports) = fixture();
    let package = root.path().join("plugins/transition");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(package.join("manifest.json"),json!({"UUID":"fixture.transition","Name":"Transition","Version":"0.1.0","CodePath":"runtime.html","Actions":[{"UUID":"fixture.transition.action","Name":"Transition"}]}).to_string()).unwrap();
    crate::test_plugin::modernize(&package);
    runtime.plugins.lock().unwrap().reload().unwrap();
    runtime
        .plugins
        .lock()
        .unwrap()
        .enable("fixture.transition", true)
        .unwrap();
    {
        let mut state = runtime.state.lock().unwrap();
        state.plugins = runtime.plugins.lock().unwrap().list();
        state.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action = crate::actions::Action::Plugin {
            user_desired_state: None,
            plugin_id: "fixture.transition".into(),
            action_id: "fixture.transition.action".into(),
            settings: json!({}),
            resources: Default::default(),
        };
    }
    sync_plugins(&runtime);
    runtime.plugins.lock().unwrap().poll();
    restore(&runtime, &mut c);
    let url = runtime
        .plugins
        .lock()
        .unwrap()
        .browser_view("fixture.transition")
        .unwrap()
        .unwrap()
        .url;
    let parts: Vec<_> = url.split('/').collect();
    let (mut socket, _) = tungstenite::connect(format!("ws://{}", parts[2])).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    socket
        .send(tungstenite::Message::text(
            crate::test_plugin::registration(&package, parts[4]).to_string(),
        ))
        .unwrap();
    let context = loop {
        let message = socket.read().unwrap();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.to_string()) {
            if value["event"] == "willAppear" {
                break value["context"].as_str().unwrap().to_owned();
            }
        }
    };
    // Inject actor time, leaving plugin time untouched. No sleeps are needed to
    // establish the timeout behavior, and no destination pixels were submitted
    c.transition.as_mut().unwrap().started = Instant::now();
    advance(&runtime, &mut c);
    assert!(c.transition.as_ref().unwrap().context.is_none());
    assert_eq!(images(&reports.lock().unwrap()), 0);
    c.transition.as_mut().unwrap().started = Instant::now() - READINESS_WAIT;
    advance(&runtime, &mut c);
    assert!(c.transition.as_ref().unwrap().context.is_some());
    finish(&runtime, &mut c);
    assert!(runtime
        .metrics
        .snapshot()
        .iter()
        .any(|m| m.name == "transition_readiness_timeouts" && m.sum == 1.0));
    // A later content update refreshes normally; a new request waits for its
    // settling interval without asking the SDK to implement readiness messages
    socket
        .send(tungstenite::Message::text(
            json!({"event":"setTitle","context":context,"payload":{"title":"Ready"}}).to_string(),
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while runtime
        .plugins
        .lock()
        .unwrap()
        .surface(&c.device_id, "key-1", "fixture-profile:fixture-page")
        .is_none_or(|v| v["title"] != "Ready")
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    restore(&runtime, &mut c);
    finish(&runtime, &mut c);
    socket.close(None).unwrap();
    runtime.shutdown();
}

#[test]
// Verify that PNG wire models preserve pixels and commit boundaries with smaller uploads
fn png_wire_models_preserve_pixels_and_commit_boundaries_with_smaller_uploads() {
    let mut checked = Vec::new();
    for module in hardware_registry::modules() {
        let rotation = match module.manifest().model_id.as_str() {
            "n4-pro" => 0, // Runtime has already oriented the N4 Pro's direct PNG
            "m3" => 270,
            "xl" => 180,
            _ => continue,
        };
        let control = module.manifest().control("key-1").unwrap();
        let data = hardware_core::render::tile(
            control.image_width.unwrap(),
            control.image_height.unwrap(),
            [36, 87, 178],
            "A key-1",
            0,
            "png",
        )
        .unwrap();
        let mut pixels = image::load_from_memory(&data).unwrap().into_rgba8();
        pixels.put_pixel(0, 0, image::Rgba([247, 31, 82, 0]));
        pixels.put_pixel(1, 0, image::Rgba([17, 239, 62, 127]));
        let pixels = image::DynamicImage::ImageRgba8(pixels);
        let data = hardware_core::render::encode_pixels(pixels.clone(), "png").unwrap();
        let expected = match rotation {
            0 => pixels,
            180 => pixels.rotate180(),
            270 => pixels.rotate270(),
            _ => unreachable!(),
        };
        let legacy = hardware_core::render::encode_pixels(expected.clone(), "png").unwrap();
        for encoding in [
            FrameEncoding::IndividualUpdates,
            FrameEncoding::GroupedKeyUpdates,
        ] {
            let upload = module
                .session()
                .encode_frame(
                    vec![DeviceCommand::Image {
                        control_id: control.id.clone(),
                        data: data.clone(),
                    }],
                    encoding,
                )
                .unwrap()
                .remove(0);
            let reports = &upload.transaction.reports;
            assert_eq!(&reports[0][6..9], b"BAT");
            assert_eq!(reports[0][13], control.image_slot.unwrap());
            let len = u32::from_be_bytes(reports[0][9..13].try_into().unwrap()) as usize;
            let payload: Vec<_> = reports[1..]
                .iter()
                .flat_map(|r| r[1..].iter().copied())
                .take(len)
                .collect();
            assert_eq!(
                image::load_from_memory(&payload).unwrap(),
                expected,
                "{}",
                module.manifest().model_id
            );
            assert!(len.div_ceil(1024) * 2 < legacy.len().div_ceil(1024));
            let individual = encoding == FrameEncoding::IndividualUpdates;
            assert_eq!(
                reports.len(),
                1 + len.div_ceil(1024) + usize::from(individual)
            );
            assert_eq!(upload.embedded_commit_count, usize::from(individual));
            assert_eq!(upload.commit.is_some(), !individual);
        }
        checked.push(module.manifest().model_id.clone());
    }
    assert_eq!(checked, ["n4-pro", "m3", "xl"]);
}

#[test]
// Verify that every hardware module preserves its transition commit boundaries
fn every_hardware_module_preserves_its_transition_commit_boundaries() {
    let fixture_root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
    for module in hardware_registry::modules() {
        let (mut c, reports) = crate::devices::frame_policy_tests::connection_for_model(
            &runtime,
            &module.manifest().model_id,
        );
        c.policy.max_batch_surfaces = 1;
        c.policy.max_batch_bytes = 1;
        c.policy.max_batch_estimated_ms = 1;
        c.worker.set_policy(c.policy).unwrap();
        c.scene_revision = c.worker.scene_revision();
        let data =
            hardware_core::render::encode_pixels(image::DynamicImage::new_rgb8(16, 16), "png")
                .unwrap();
        let commands: Vec<_> = c
            .manifest
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some())
            .map(|c| DeviceCommand::Image {
                control_id: c.id.clone(),
                data: data.clone(),
            })
            .collect();
        let encoding = if c
            .manifest
            .capabilities
            .frame_output
            .as_ref()
            .is_some_and(|f| f.grouped_key_updates)
            && c.session
                .lock()
                .unwrap()
                .supports_frame_encoding(FrameEncoding::GroupedKeyUpdates)
        {
            FrameEncoding::GroupedKeyUpdates
        } else {
            FrameEncoding::IndividualUpdates
        };
        let mut uploads: VecDeque<_> = c
            .session
            .lock()
            .unwrap()
            .encode_frame(commands.clone(), encoding)
            .unwrap()
            .into();
        let mut expected = Vec::new();
        while let Some(first) = uploads.pop_front() {
            let commit = first.commit.clone();
            expected.extend(first.transaction.reports);
            if let Some(commit) = commit {
                let mut index = 0;
                while index < uploads.len() {
                    if uploads[index].commit.as_ref() == Some(&commit) {
                        expected.extend(uploads.remove(index).unwrap().transaction.reports);
                    } else {
                        index += 1;
                    }
                }
                expected.extend(commit.reports);
            }
        }
        let receipt = submit(&mut c, commands, None).unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(3)).unwrap(),
            OutputStatus::Completed,
            "{}",
            c.manifest.model_id
        );
        assert_eq!(
            *reports.lock().unwrap(),
            expected,
            "{}",
            c.manifest.model_id
        );
    }
}

#[test]
// Verify that profile changes are device local while another scene prepares
fn profile_changes_are_device_local_while_another_scene_prepares() {
    let (_root, runtime, mut first, first_reports) = fixture();
    let (mut second, second_reports) =
        crate::devices::frame_policy_tests::connection_for_model(&runtime, "n4-pro");
    second.device_id = "second-device".into();
    second.manifest = first.manifest.clone();
    {
        let mut state = runtime.state.lock().unwrap();
        let mut device = state.devices[0].clone();
        device.id = second.device_id.clone();
        device.persistence_key = second.device_id.clone();
        state.devices.push(device);
        let session = state.session(&first.device_id);
        state.sessions.insert(second.device_id.clone(), session);
        let mut profile = state.profiles[0].clone();
        profile.id = "destination-profile".into();
        profile.pages[0].bindings.clear();
        state.profiles.push(profile);
    }
    runtime
        .connection_epochs
        .lock()
        .unwrap()
        .insert(second.device_id.clone(), second.epoch);
    restore(&runtime, &mut first);
    restore(&runtime, &mut second);
    advance(&runtime, &mut second);
    let second_generation = second.transition.as_ref().unwrap().generation;
    runtime
        .select_device_profile(&first.device_id, "fixture-profile", Some("b"), true)
        .unwrap();
    runtime
        .select_device_profile(
            &first.device_id,
            "destination-profile",
            Some("fixture-page"),
            true,
        )
        .unwrap();
    advance(&runtime, &mut first);
    finish(&runtime, &mut second);
    assert!(images(&second_reports.lock().unwrap()) > 0);
    assert_eq!(images(&first_reports.lock().unwrap()), 0);
    assert!(runtime.transition_inputs.lock().unwrap().current(
        &second.device_id,
        second.epoch,
        second_generation
    ));
    assert!(runtime
        .transition_inputs
        .lock()
        .unwrap()
        .pending(&first.device_id, first.epoch)
        .is_some());
    finish(&runtime, &mut first);
    assert_eq!(
        runtime.snapshot().session(&second.device_id).profile_id,
        "fixture-profile"
    );
    runtime.shutdown();
}

#[test]
// Verify that missing media falls back and explicit individual policy remains effective
fn missing_media_falls_back_and_explicit_individual_policy_remains_effective() {
    let (_root, runtime, mut c, reports) = fixture();
    {
        let mut state = runtime.state.lock().unwrap();
        state.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .asset = Some(uuid::Uuid::new_v4().to_string());
        state
            .sessions
            .get_mut(&c.device_id)
            .unwrap()
            .output
            .frame_policy
            .as_mut()
            .unwrap()
            .policy
            .mode = OutputMode::IndividualUpdates;
    }
    restore(&runtime, &mut c);
    finish(&runtime, &mut c);
    assert_eq!(
        reports
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.get(6..9) == Some(b"STP"))
            .count(),
        3
    );
    assert!(runtime
        .transition_inputs
        .lock()
        .unwrap()
        .pending(&c.device_id, c.epoch)
        .is_none());
    assert!(runtime
        .metrics
        .snapshot()
        .iter()
        .any(|m| m.name == "transition_render_fallbacks" && m.sum >= 1.0));
    runtime.shutdown();
}

#[test]
// Verify that navigation gate prevents updates before the actor begins preparation
fn navigation_gate_prevents_updates_before_the_actor_begins_preparation() {
    let (_root, runtime, mut c, reports) = fixture();
    restore(&runtime, &mut c);
    finish(&runtime, &mut c);
    reports.lock().unwrap().clear();
    runtime
        .select_device_profile(&c.device_id, "fixture-profile", Some("b"), true)
        .unwrap();
    assert!(c.transition.is_none());
    let data =
        hardware_core::render::encode_pixels(image::DynamicImage::new_rgb8(16, 16), "png").unwrap();
    send_current(
        &runtime,
        &mut c,
        DeviceCommand::Image {
            control_id: "key-1".into(),
            data,
        },
    )
    .unwrap();
    assert!(!refresh_display_theme(&runtime, &mut c, true));
    assert!(c.worker.wait_idle(Duration::from_secs(2)));
    assert!(reports.lock().unwrap().is_empty());
    restore(&runtime, &mut c);
    let started = Instant::now() - READINESS_WAIT;
    c.transition.as_mut().unwrap().started = started;
    advance(&runtime, &mut c);
    runtime.state.lock().unwrap().configuration_revision += 1;
    advance(&runtime, &mut c);
    assert_eq!(
        c.transition.as_ref().unwrap().started,
        started,
        "A re-render must not extend plugin waiting"
    );
    finish(&runtime, &mut c);
    runtime.shutdown();
}

/// Explicit opt-in only. LIST enumerates supported identities without opening HID;
/// RUN uses one connected device and a disposable, action-free workspace
#[test]
#[ignore = "Physical recording probe: set STREAM_DOCK_MANAGER_PROFILE_PHYSICAL=LIST or RUN explicitly"]
// Verify that physical profile transition recording
fn physical_profile_transition_recording() {
    let mode = std::env::var("STREAM_DOCK_MANAGER_PROFILE_PHYSICAL").unwrap_or_default();
    assert!(
        mode == "LIST" || mode == "RUN",
        "Explicit physical probe mode required"
    );
    let api = hidapi::HidApi::new().unwrap();
    let supported: Vec<_> = api
        .device_list()
        .filter_map(|info| hardware_registry::find(&identity(info)).map(|module| (info, module)))
        .collect();
    for (_, module) in &supported {
        println!("Supported connected model: {}", module.manifest().model_id);
    }
    if mode == "LIST" {
        return;
    }
    assert_eq!(
        supported.len(),
        1,
        "Recording probe requires exactly one supported device"
    );
    let (info, module) = &supported[0];
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let manifest = module.manifest();
    let pages = [
        ("a", "A", "#2457b2"),
        ("b", "B", "#b23938"),
        ("c", "C", "#287c4b"),
    ]
    .into_iter()
    .map(|(id, title, color)| {
        let bindings = manifest
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some())
            .map(|control| {
                let binding = serde_json::from_value(serde_json::json!({
                    "title": format!("{title} {}", control.id),
                    "color": color,
                    "custom_background": true,
                    "action": {"kind": "none"}
                }))
                .unwrap();
                (control.id.clone(), binding)
            })
            .collect();
        crate::profiles::Page {
            id: id.into(),
            name: title.into(),
            bindings,
        }
    })
    .collect();
    let profile = crate::profiles::Profile {
        id: "physical-transition-probe".into(),
        name: "Action-free transition recording".into(),
        model_id: manifest.model_id.clone(),
        active_page: "a".into(),
        pages,
        background_asset: None,
        background_media: Default::default(),
        palette: None,
        parameters: Vec::new(),
        linked_controls: Default::default(),
    };
    profile.validate().unwrap();
    {
        let mut state = runtime.state.lock().unwrap();
        state.active_profile = profile.id.clone();
        state.profiles = vec![profile.clone()];
        state.sessions.clear();
    }
    let mut c = open_connection(&runtime, info, &api, module.as_ref(), 1, false).unwrap();
    finish(&runtime, &mut c);
    println!(
        "HOST POLICY {:?}, grouped key capability {}",
        c.policy.mode,
        c.session
            .lock()
            .unwrap()
            .supports_frame_encoding(FrameEncoding::GroupedKeyUpdates)
    );
    println!("RECORDING: A blue baseline; beginning slow A/B/C switches in 5 seconds");
    std::thread::sleep(Duration::from_secs(5));
    for page in ["b", "c", "a", "b", "c", "a"] {
        runtime
            .select_device_profile(&c.device_id, &profile.id, Some(page), true)
            .unwrap();
        restore(&runtime, &mut c);
        finish(&runtime, &mut c);
        println!("Completed page {page}");
        std::thread::sleep(Duration::from_secs(2));
    }
    println!("RECORDING: rapid A/B/C and A/B/A supersession");
    for destination in ["c", "a", "c", "a"] {
        runtime
            .select_device_profile(&c.device_id, &profile.id, Some("b"), true)
            .unwrap();
        restore(&runtime, &mut c);
        advance(&runtime, &mut c);
        runtime
            .select_device_profile(&c.device_id, &profile.id, Some(destination), true)
            .unwrap();
        advance(&runtime, &mut c);
        finish(&runtime, &mut c);
        std::thread::sleep(Duration::from_secs(1));
    }
    println!(
        "HOST TRANSFER METRICS {}",
        serde_json::to_string(
            &runtime
                .metrics
                .snapshot()
                .into_iter()
                .filter(|m| m.name.starts_with("transition_") || m.name.starts_with("hid_output_"))
                .collect::<Vec<_>>()
        )
        .unwrap()
    );
    println!(
        "HOST OUTPUT STATS {}",
        serde_json::to_string(&c.worker.queue_stats()).unwrap()
    );
    println!("Recording probe completed; optical acceptance requires review of the recording.");
    drop(c);
    runtime.shutdown();
}
