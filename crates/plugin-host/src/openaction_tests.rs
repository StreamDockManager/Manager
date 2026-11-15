use super::*;

// Construct the package representation
fn package(root: &Path, disabled: bool) {
    let path = root.join("org.openaction.fixture.sdPlugin");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("plugin.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(path.join("icon.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="#f00"/></svg>"##).unwrap();
    let value = json!({"Name":"OpenAction fixture","Author":"Manager tests","Version":"1.0.0","Icon":"icon","CodePath":"plugin.html","PropertyInspectorPath":"plugin.html","HasSettingsInterface":true,"OS":[{"Platform":"mac"},{"Platform":"windows"},{"Platform":"linux"}],"Actions":[{"UUID":"org.openaction.fixture.toggle","Name":"Toggle","Icon":"icon","DisableAutomaticStates":disabled,"States":[{"Title":"Off"},{"Title":"On","Image":"actionDefaultImage"}]}]});
    std::fs::write(path.join("manifest.json"), value.to_string()).unwrap();
}
#[test]
// Verify openaction toggle occurs on release and pulse preserves both states
fn openaction_toggle_occurs_on_release_and_pulse_preserves_both_states() {
    for disabled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        package(root.path(), disabled);
        let mut host = lifecycle_tests::host(root.path());
        let id = "org.openaction.fixture";
        host.enable(id, true).unwrap();
        lifecycle_tests::demand(&mut host, id);
        for input in [
            hardware_core::Input::Button { pressed: true },
            hardware_core::Input::Button { pressed: false },
            hardware_core::Input::Pulse,
        ] {
            host.dispatch(
                id,
                "org.openaction.fixture.toggle",
                "device",
                "key",
                "profile:page",
                &input,
                &json!({}),
            )
            .unwrap();
        }
        let hub = host.hub.lock().unwrap();
        let states: Vec<_> = hub.sessions.queues[id]
            .iter()
            .filter(|e| matches!(e["event"].as_str(), Some("keyUp" | "keyDown")))
            .map(|e| e["payload"]["state"].as_u64().unwrap())
            .collect();
        assert_eq!(
            states,
            if disabled {
                vec![0, 0, 0, 0]
            } else {
                vec![0, 1, 1, 0]
            }
        );
    }
}
#[test]
// Verify openaction SDK null visual reset uses action icon and defaults
fn openaction_sdk_null_visual_reset_uses_action_icon_and_defaults() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), false);
    let mut host = lifecycle_tests::host(root.path());
    let id = "org.openaction.fixture";
    host.enable(id, true).unwrap();
    lifecycle_tests::demand(&mut host, id);
    host.dispatch(
        id,
        "org.openaction.fixture.toggle",
        "device",
        "key",
        "profile:page",
        &hardware_core::Input::Pulse,
        &json!({}),
    )
    .unwrap();
    let mut hub = host.hub.lock().unwrap();
    let context = hub.binding_context(id, "profile:page", "device", "key");
    visual::prepare(&mut hub,id,&mut json!({"event":"setTitle","context":context,"payload":{"title":"Custom","state":null}})).unwrap();
    assert_eq!(hub.visual_state.visuals[&context]["title"], "Custom");
    for (event, key) in [("setTitle", "title"), ("setImage", "image")] {
        visual::prepare(
            &mut hub,
            id,
            &mut json!({"event":event,"context":context,"payload":{key:null,"state":null}}),
        )
        .unwrap();
    }
    let v = &hub.visual_state.visuals[&context];
    assert_eq!(v["title"], "On");
    assert!(v["image"].as_str().unwrap().starts_with("data:image/"));
    assert_eq!(v["titleParameters"]["fontSize"], 16.0);
    assert_eq!(v["titleParameters"]["titleAlignment"], "middle");
}
#[test]
// Verify openaction settings interface requires connected enabled opt in
fn openaction_settings_interface_requires_connected_enabled_opt_in() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), false);
    let mut host = lifecycle_tests::host(root.path());
    let id = "org.openaction.fixture";
    assert!(host.show_settings_interface(id).is_err());
    host.enable(id, true).unwrap();
    assert!(host.show_settings_interface(id).is_err());
    host.plugins.get_mut(id).unwrap().token = "fixture-session".into();
    host.hub
        .lock()
        .unwrap()
        .sessions
        .connected
        .insert("fixture-session".into());
    host.show_settings_interface(id).unwrap();
    assert!(host.hub.lock().unwrap().sessions.queues[id]
        .iter()
        .any(|e| *e == json!({"event":"showSettingsInterface"})));
    host.plugins
        .get_mut(id)
        .unwrap()
        .info
        .has_settings_interface = false;
    assert!(host.show_settings_interface(id).is_err());
}
#[test]
// Verify openaction browser callback is available without elgato alias
fn openaction_browser_callback_is_available_without_elgato_alias() {
    let bootstrap = browser::bootstrap(1234, "token", "registerPlugin", &json!({}), &json!({}));
    assert!(
        bootstrap.contains("window.connectOpenActionSocket||window.connectElgatoStreamDeckSocket")
    );
}

#[test]
// Verify openaction opaque global settings survive snapshot validation and restart
fn openaction_opaque_global_settings_survive_snapshot_validation_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let host = lifecycle_tests::host(root.path());
    {
        let mut hub = host.hub.lock().unwrap();
        hub.globals.insert(
            "org.openaction.fixture".into(),
            json!([null, 7, {"nested":true}]),
        );
        hub.persist().unwrap();
    }
    host.storage()
        .run(|connection| validate_storage_snapshot(connection))
        .unwrap();
    let hub = Hub::load(host.storage()).unwrap();
    assert_eq!(
        hub.globals["org.openaction.fixture"],
        json!([null, 7, {"nested":true}])
    );
}

#[test]
// Verify openaction opaque defaults commit and configuration roundtrip
fn openaction_opaque_defaults_commit_and_configuration_roundtrip() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), false);
    let mut host = lifecycle_tests::host(root.path());
    let id = "org.openaction.fixture";
    let action = "org.openaction.fixture.toggle";
    let panel = host.default_settings_panel(id, action).unwrap();
    let context = format!("settings-draft:{}", panel.token);
    host.hub
        .lock()
        .unwrap()
        .context_state
        .settings
        .insert(context, json!([1, null]));
    host.commit_plugin_settings(id, &panel.token).unwrap();
    host.close_inspector(&panel.token);
    host.storage()
        .run(|connection| validate_storage_snapshot(connection))
        .unwrap();
    assert_eq!(
        Hub::load(host.storage()).unwrap().action_defaults[id][action],
        json!([1, null])
    );
    host.hub
        .lock()
        .unwrap()
        .globals
        .insert(id.into(), json!([2]));
    host.hub.lock().unwrap().persist().unwrap();
    let current = host.configuration(id).unwrap();
    assert_eq!(current.settings, json!([2]));
    let mut changed = current.clone();
    changed.settings = json!([3]);
    host.configuration_transaction_sql(id, &current, &changed, 0, |_| Ok(()))
        .unwrap();
    assert_eq!(host.configuration(id).unwrap().settings, json!([3]));
    host.storage()
        .run(|connection| validate_storage_snapshot(connection))
        .unwrap();
}

#[test]
// Verify openaction multi action release toggles after event without affecting press
fn openaction_multi_action_release_toggles_after_event_without_affecting_press() {
    for disabled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        package(root.path(), disabled);
        let mut host = lifecycle_tests::host(root.path());
        let id = "org.openaction.fixture";
        host.enable(id, true).unwrap();
        lifecycle_tests::demand(&mut host, id);
        for _ in 0..2 {
            host.dispatch(
                id,
                "org.openaction.fixture.toggle",
                "device",
                "key",
                "profile:page@0",
                &hardware_core::Input::Pulse,
                &json!(["opaque"]),
            )
            .unwrap();
        }
        let hub = host.hub.lock().unwrap();
        let events: Vec<_> = hub.sessions.queues[id]
            .iter()
            .filter(|event| matches!(event["event"].as_str(), Some("keyDown" | "keyUp")))
            .collect();
        let states: Vec<_> = events
            .iter()
            .map(|e| e["payload"]["state"].as_u64().unwrap())
            .collect();
        assert_eq!(
            states,
            if disabled {
                vec![0, 0, 0, 0]
            } else {
                vec![0, 0, 1, 1]
            }
        );
        assert!(events
            .iter()
            .all(|e| e["payload"]["isInMultiAction"] == true));
        assert!(events
            .iter()
            .all(|e| e["payload"]["settings"] == json!(["opaque"])));
    }
}

#[test]
// Verify openaction fresh pulse announces initial state before release toggle
fn openaction_fresh_pulse_announces_initial_state_before_release_toggle() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), false);
    let mut host = lifecycle_tests::host(root.path());
    let id = "org.openaction.fixture";
    host.enable(id, true).unwrap();
    lifecycle_tests::demand(&mut host, id);
    host.dispatch(
        id,
        "org.openaction.fixture.toggle",
        "device",
        "key",
        "profile:page",
        &hardware_core::Input::Pulse,
        &json!({}),
    )
    .unwrap();
    let hub = host.hub.lock().unwrap();
    let events: Vec<_> = hub.sessions.queues[id]
        .iter()
        .filter(|e| {
            matches!(
                e["event"].as_str(),
                Some("willAppear" | "keyDown" | "keyUp")
            )
        })
        .collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["payload"]["state"], 0);
    assert_eq!(events[1]["payload"]["state"], 0);
    assert_eq!(events[2]["payload"]["state"], 1);
}

#[test]
// Verify openaction state without title clears previous encoder feedback title
fn openaction_state_without_title_clears_previous_encoder_feedback_title() {
    let root = tempfile::tempdir().unwrap();
    package(root.path(), false);
    let mut host = lifecycle_tests::host(root.path());
    let id = "org.openaction.fixture";
    host.enable(id, true).unwrap();
    lifecycle_tests::demand(&mut host, id);
    host.dispatch(
        id,
        "org.openaction.fixture.toggle",
        "device",
        "key",
        "profile:page",
        &hardware_core::Input::Button { pressed: true },
        &json!({}),
    )
    .unwrap();
    let mut hub = host.hub.lock().unwrap();
    let context = hub.binding_context(id, "profile:page", "device", "key");
    Arc::make_mut(hub.metadata.get_mut(id).unwrap())
        .get_mut("org.openaction.fixture.toggle")
        .unwrap()["States"][1]
        .as_object_mut()
        .unwrap()
        .remove("Title");
    visual::prepare(
        &mut hub,
        id,
        &mut json!({"event":"setTitle","context":context,"payload":{"title":"old"}}),
    )
    .unwrap();
    // An all-state override still wins. Clear it to restore declared defaults
    visual::prepare(
        &mut hub,
        id,
        &mut json!({"event":"setTitle","context":context,"payload":{"title":null}}),
    )
    .unwrap();
    visual::prepare(
        &mut hub,
        id,
        &mut json!({"event":"setState","context":context,"payload":{"state":1}}),
    )
    .unwrap();
    assert_eq!(hub.visual_state.visuals[&context]["title"], "");
    assert_eq!(hub.visual_state.visuals[&context]["feedback"]["title"], "");
}

#[test]
// Verify openaction socket misc and encoder feedback routes are scoped
fn openaction_socket_misc_and_encoder_feedback_routes_are_scoped() {
    use tungstenite::{stream::MaybeTlsStream, Message, WebSocket};
    // Wait until the fixture host has processed preceding events
    fn barrier(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, marker: u8) {
        socket.send(Message::Ping(vec![marker].into())).unwrap();
        loop {
            match socket.read().unwrap() {
                Message::Pong(bytes) if bytes.as_ref() == [marker] => break,
                Message::Close(_) => panic!("OpenAction fixture connection closed"),
                _ => {}
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    package(root.path(), false);
    let mut host = lifecycle_tests::socket_host(root.path());
    let id = "org.openaction.fixture";
    host.enable(id, true).unwrap();
    lifecycle_tests::demand(&mut host, id);
    let token = host.plugins[id].token.clone();
    host.dispatch(
        id,
        "org.openaction.fixture.toggle",
        "device",
        "key",
        "profile:page",
        &hardware_core::Input::Button { pressed: true },
        &json!({}),
    )
    .unwrap();
    let context = host
        .hub
        .lock()
        .unwrap()
        .binding_context(id, "profile:page", "device", "key");
    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{}", host.port)).unwrap();
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    socket
        .send(Message::text(
            json!({"event":"registerPlugin","uuid":token}).to_string(),
        ))
        .unwrap();
    barrier(&mut socket, 0);
    host.drain_effects();
    let generation = host.hub.lock().unwrap().visual_state.visual_generation;
    socket
        .send(Message::text(
            json!({"event":"logMessage","payload":{"message":"OpenAction conformance log"}})
                .to_string(),
        ))
        .unwrap();
    barrier(&mut socket, 1);
    assert!(
        host.drain_effects().is_empty(),
        "logs must not become device effects"
    );
    assert_eq!(
        host.hub.lock().unwrap().visual_state.visual_generation,
        generation
    );
    let requests = [
        json!({"event":"setFeedbackLayout","context":context,"payload":{"layout":"$A0"}}),
        json!({"event":"setFeedback","context":context,"payload":{"title":"OpenAction encoder","value":"42"}}),
        json!({"event":"showAlert","context":context}),
        json!({"event":"showOk","context":context}),
        json!({"event":"openUrl","payload":{"url":"https://example.com/openaction-test"}}),
    ];
    for (index, request) in requests.into_iter().enumerate() {
        socket.send(Message::text(request.to_string())).unwrap();
        barrier(&mut socket, index as u8 + 2);
        let effects = host.drain_effects();
        if index == 4 {
            assert!(effects.iter().any(|effect| matches!(effect,HostEffect::OpenUrl {url,..} if url=="https://example.com/openaction-test")));
        } else {
            let kind = [
                FeedbackKind::Layout,
                FeedbackKind::Feedback,
                FeedbackKind::Alert,
                FeedbackKind::Ok,
            ][index];
            assert!(effects.iter().any(|effect| matches!(effect,HostEffect::Feedback(value) if value.kind==kind && value.context.as_deref()==Some(context.as_str()))), "Expected {kind:?}: {effects:?}");
        }
    }
    let hub = host.hub.lock().unwrap();
    assert_eq!(hub.visual_state.visuals[&context]["layoutReference"], "$A0");
    assert_eq!(
        hub.visual_state.visuals[&context]["feedback"]["value"],
        "42"
    );
    assert_eq!(hub.visual_state.visuals[&context]["badge"], "OK");
    drop(hub);
    drop(socket);
    host.shutdown();
}
