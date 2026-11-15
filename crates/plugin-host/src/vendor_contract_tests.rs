//! Read-only independent upstream package admission and security regressions
use super::*;
// Construct a representative vendor fixture
fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/plugins/vendor/vsd/VSDJavaScriptSDK/com.vsd.craft.xxx.sdPlugin")
}
#[test]
// Verify untouched VSD package has vendor identity controls and inspector
fn untouched_vsd_package_has_vendor_identity_controls_and_inspector() {
    let path = sample();
    let plugin = read_plugin(&path).unwrap();
    assert_eq!(plugin.info.id, "com.vsd.craft.xxx");
    assert_eq!(plugin.info.dialect, "vsd");
    assert_eq!(
        plugin.info.author.as_deref(),
        plugin.manifest["Author"].as_str()
    );
    assert!(plugin.info.capabilities.is_none());
    assert_eq!(
        plugin.info.actions[0].id,
        "com.hotspot.streamdock.xxx.action1"
    );
    assert_eq!(
        plugin.info.actions[0].control_types,
        ["button", "encoder", "touch"]
    );
    assert!(plugin.info.actions[0].has_settings);
    let installed = read_plugin_with_identity(&path, Some("com.vsd.craft.xxx")).unwrap();
    assert_eq!(installed.info.id, plugin.info.id);
}
#[test]
// Verify package identity and manager claims cannot be spoofed
fn package_identity_and_manager_claims_cannot_be_spoofed() {
    let path = sample();
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
    value["UUID"] = json!("com.someone.else");
    assert!(manifest::normalize(&path, &value).is_err());
    value.as_object_mut().unwrap().remove("UUID");
    value["StreamDockManager"] = json!({"ManifestDialect":"unknown"});
    assert!(manifest::normalize(&path, &value).is_err());
    value["StreamDockManager"] = json!({"Capabilities":{}});
    assert!(capabilities::inspect(&path, &path.join("plugin/index.html"), &value).is_err());
}
#[test]
// Verify vendor baseline never grants manager extensions
fn vendor_baseline_never_grants_manager_extensions() {
    for event in [
        "streamDockManagerRpcRequest",
        "desktopInput",
        "actionResult",
        "subscribeTelemetry",
        "setDynamicItems",
        "networkPayload",
    ] {
        assert!(manager_extension(event), "{event}");
    }
    for event in [
        "setSettings",
        "getGlobalSettings",
        "setFeedback",
        "sendToPlugin",
        "setTitle",
    ] {
        assert!(!manager_extension(event), "{event}");
        assert!(InboundPolicy::classify(event).is_some(), "{event}");
    }
}
#[test]
// Verify missing descriptor and malformed descriptor are distinct
fn missing_descriptor_and_malformed_descriptor_are_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("plugin.js");
    std::fs::write(&executable, "// standard vendor plugin").unwrap();
    assert!(matches!(
        capabilities::inspect(temp.path(), &executable, &json!({})).unwrap(),
        capabilities::Inspection::VendorBaseline
    ));
    std::fs::write(&executable, "SDMCAP1:00000010:truncated").unwrap();
    assert!(capabilities::inspect(temp.path(), &executable, &json!({})).is_err());
}

#[test]
// Verify settings events preserve vendor context and opaque JSON
fn settings_events_preserve_vendor_context_and_opaque_json() {
    let mut hub = Hub::default();
    hub.context_state.visible.insert("ctx".into(), json!({"event":"willAppear","context":"ctx","action":"vendor.action","device":"device","payload":{"controller":"Encoder","coordinates":{"column":2,"row":0},"state":1,"isInMultiAction":false}}));
    let settings = json!({"arbitrary":[null,{"key":"${input.literal}"}],"secret":{"nested":true}});
    let event = hub.settings_event("ctx", &settings);
    assert_eq!(event["event"], "didReceiveSettings");
    assert_eq!(event["action"], "vendor.action");
    assert_eq!(event["payload"]["coordinates"]["column"], 2);
    assert_eq!(event["payload"]["state"], 1);
    assert_eq!(event["payload"]["settings"], settings);
}

#[test]
// Verify checked in vendor sources match pinned hashes
fn checked_in_vendor_sources_match_pinned_hashes() {
    use sha2::{Digest, Sha256};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/plugins");
    let inventory: Value =
        serde_json::from_slice(&std::fs::read(root.join("vendor-sources.json")).unwrap()).unwrap();
    for file in inventory["files"].as_array().unwrap() {
        let path = file["path"].as_str().unwrap();
        let bytes = std::fs::read(safe_file(&root, path).unwrap()).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(bytes)),
            file["sha256"].as_str().unwrap(),
            "{path}"
        );
    }
}

#[test]
// Verify vendor dials have signed ticks pressed state coordinates and touch arrays
fn vendor_dials_have_signed_ticks_pressed_state_coordinates_and_touch_arrays() {
    let root = tempfile::tempdir().unwrap();
    lifecycle_tests::package(&root.path().join("fixture"), "fixture.vendor");
    let mut host = lifecycle_tests::host(root.path());
    host.plugins
        .get_mut("fixture.vendor")
        .unwrap()
        .info
        .capabilities = None;
    host.hub
        .lock()
        .unwrap()
        .capabilities
        .remove("fixture.vendor");
    host.enable("fixture.vendor", true).unwrap();
    lifecycle_tests::demand(&mut host, "fixture.vendor");
    let context = {
        let mut hub = host.hub.lock().unwrap();
        let context = hub.binding_context("fixture.vendor", "profile:page", "device", "dial");
        hub.context_state.visible.insert(context.clone(), json!({"context":context,"payload":{"controller":"Encoder","coordinates":{"column":2,"row":0}}}));
        hub.visual_state
            .visuals
            .insert(context.clone(), Arc::new(json!({"state":1})));
        context
    };
    for input in [
        hardware_core::Input::Button { pressed: true },
        hardware_core::Input::Rotate { delta: -2 },
        hardware_core::Input::Button { pressed: false },
        hardware_core::Input::Touch {
            phase: "hold".into(),
            x: 11,
            y: 12,
        },
    ] {
        host.dispatch(
            "fixture.vendor",
            "fixture.vendor.action",
            "device",
            "dial",
            "profile:page",
            &input,
            &json!({}),
        )
        .unwrap();
    }
    let hub = host.hub.lock().unwrap();
    let events: Vec<_> = hub.sessions.queues["fixture.vendor"].iter().collect();
    let rotate = events.iter().find(|e| e["event"] == "dialRotate").unwrap();
    assert_eq!(rotate["payload"]["ticks"], -2);
    assert_eq!(rotate["payload"]["pressed"], true);
    assert_eq!(rotate["payload"]["state"], 1);
    assert_eq!(rotate["payload"]["coordinates"]["column"], 2);
    assert!(events.iter().any(|e| e["event"] == "dialDown"));
    assert!(events.iter().any(|e| e["event"] == "dialUp"));
    let touch = events.iter().find(|e| e["event"] == "touchTap").unwrap();
    assert_eq!(touch["payload"]["tapPos"], json!([11, 12]));
    assert_eq!(touch["payload"]["hold"], true);
    assert!(!hub.context_state.pressed_encoders.contains(&context));
}

#[test]
// Verify standard vendor dispatch does not wait for manager receipt
fn standard_vendor_dispatch_does_not_wait_for_manager_receipt() {
    let root = tempfile::tempdir().unwrap();
    lifecycle_tests::package(&root.path().join("fixture"), "fixture.vendor");
    let mut host = lifecycle_tests::host(root.path());
    host.plugins
        .get_mut("fixture.vendor")
        .unwrap()
        .info
        .capabilities = None;
    host.hub
        .lock()
        .unwrap()
        .capabilities
        .remove("fixture.vendor");
    host.enable("fixture.vendor", true).unwrap();
    lifecycle_tests::demand(&mut host, "fixture.vendor");
    let receipt = host
        .dispatch_tracked(
            "fixture.vendor",
            "fixture.vendor.action",
            "device",
            "key",
            "profile:page",
            &hardware_core::Input::Pulse,
            &json!({}),
        )
        .unwrap();
    receipt.wait(Duration::from_millis(50), || Ok(())).unwrap();
    assert!(host.hub.lock().unwrap().action_results.is_empty());
}

#[test]
// Verify software only update cannot promote stale hardware macro context
fn software_only_update_cannot_promote_stale_hardware_macro_context() {
    let root = tempfile::tempdir().unwrap();
    let host = lifecycle_tests::host(root.path());
    {
        let mut hub = host.hub.lock().unwrap();
        hub.roots.insert("plugin".into(), root.path().into());
        for (context, scope, title) in [
            ("older", "profile:page", "old hardware"),
            ("newer", "profile:page@macro", "new hardware"),
        ] {
            hub.context_state
                .contexts
                .insert(context.into(), "plugin".into());
            hub.context_state
                .scopes
                .insert(context.into(), scope.into());
            hub.context_state
                .targets
                .insert(context.into(), ("device".into(), "key".into()));
            let mut message =
                json!({"event":"setTitle","context":context,"payload":{"title":title,"target":0}});
            visual::prepare(&mut hub, "plugin", &mut message).unwrap();
            hub.visual_state.record_update(context, &message);
        }
        let mut message = json!({"event":"setTitle","context":"older","payload":{"title":"editor only","target":2}});
        visual::prepare(&mut hub, "plugin", &mut message).unwrap();
        hub.visual_state.record_update("older", &message);
    }
    let requests = vec![("key".into(), "profile:page".into())];
    let (hardware, ()) = host.render_surfaces_for_target("device", &requests, &[], false, || ());
    let (software, ()) = host.render_surfaces_for_target("device", &requests, &[], true, || ());
    assert_eq!(hardware.values["key"]["title"], "new hardware");
    assert_eq!(software.values["key"]["title"], "editor only");
}

#[test]
// Verify device changes emit change without fabricating reconnect
fn device_changes_emit_change_without_fabricating_reconnect() {
    let root = tempfile::tempdir().unwrap();
    lifecycle_tests::package(&root.path().join("fixture"), "fixture.vendor");
    let mut host = lifecycle_tests::host(root.path());
    host.enable("fixture.vendor", true).unwrap();
    host.synchronize(
        vec![],
        vec![json!({"id":"device","name":"Original","type":0,"size":{"columns":3,"rows":2}})],
    );
    host.hub
        .lock()
        .unwrap()
        .sessions
        .queues
        .entry("fixture.vendor".into())
        .or_default()
        .clear();
    host.synchronize(
        vec![],
        vec![json!({"id":"device","name":"Renamed","type":0,"size":{"columns":3,"rows":2}})],
    );
    let hub = host.hub.lock().unwrap();
    assert!(hub.sessions.queues["fixture.vendor"]
        .iter()
        .any(
            |event| event["event"] == "deviceDidChange" && event["deviceInfo"]["name"] == "Renamed"
        ));
    assert!(!hub.sessions.queues["fixture.vendor"]
        .iter()
        .any(|event| event["event"] == "deviceDidConnect"));
}

#[test]
// Verify authored title changes are deduplicated and removed overlays are hidden
fn authored_title_changes_are_deduplicated_and_removed_overlays_are_hidden() {
    let root = tempfile::tempdir().unwrap();
    let host = lifecycle_tests::host(root.path());
    {
        let mut hub = host.hub.lock().unwrap();
        let context = hub.binding_context("plugin", "scope", "device", "key");
        hub.context_state
            .contexts
            .insert(context.clone(), "plugin".into());
        hub.context_state.visible.insert(context.clone(),json!({"event":"willAppear","context":context,"action":"plugin.action","device":"device","payload":{"controller":"Keypad","coordinates":{"column":1,"row":0},"state":0,"isInMultiAction":false}}));
    }
    let parameters = json!({"fontFamily":"sans","fontSize":18,"fontStyle":"Regular","fontUnderline":false,"showTitle":true,"titleAlignment":"bottom","titleColor":"#ffffff"});
    assert!(host
        .update_title_parameters(
            "plugin",
            "scope",
            "device",
            "key",
            "Authored",
            parameters.clone()
        )
        .unwrap());
    assert!(!host
        .update_title_parameters("plugin", "scope", "device", "key", "Authored", parameters)
        .unwrap());
    assert!(host
        .hide_title_parameters("plugin", "scope", "device", "key", "Authored")
        .unwrap());
    assert!(!host
        .hide_title_parameters("plugin", "scope", "device", "other", "Authored")
        .unwrap());
    let hub = host.hub.lock().unwrap();
    let events: Vec<_> = hub.sessions.queues["plugin"].iter().collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["payload"]["titleParameters"]["showTitle"], false);
    assert!(events[0]["payload"].get("isInMultiAction").is_none());
}

#[test]
// Verify untouched vendor package installs disabled without executing its code
fn untouched_vendor_package_installs_disabled_without_executing_its_code() {
    let root = tempfile::tempdir().unwrap();
    let mut host = lifecycle_tests::host(root.path());
    let prepared = PluginHost::prepare_install(&host.root, &sample()).unwrap();
    let info = host.register_prepared(prepared).unwrap();
    assert_eq!(info.id, "com.vsd.craft.xxx");
    assert!(!info.enabled);
    assert!(info.capabilities.is_none());
    assert!(host.plugins[&info.id].child.is_none());
    assert!(host.plugins[&info.id].token.is_empty());
}

// Construct the vendor package manifest fixture
fn manifest_vendor_package(root: &Path, disabled: bool) {
    let path = root.join("fixture");
    lifecycle_tests::package(&path, "fixture.vendor");
    let mut manifest = manifest_schema::fixture("fixture.vendor", "runtime.html");
    manifest["Actions"][0]["States"] = json!([{"Image":"icon","Title":"off","Name":"Off"},{"Image":"icon","Title":"on","Name":"On"}]);
    manifest["Actions"][0]["DisableAutomaticStates"] = json!(disabled);
    std::fs::write(
        path.join("runtime.html"),
        "<!doctype html><title>Vendor fixture</title>",
    )
    .unwrap();
    std::fs::write(
        path.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
// Verify vendor manifest action flags are projected with upstream true defaults
fn vendor_manifest_action_flags_are_projected_with_upstream_true_defaults() {
    let root = tempfile::tempdir().unwrap();
    manifest_vendor_package(root.path(), false);
    let path = root.path().join("fixture");
    let plugin = read_plugin(&path).unwrap();
    let action = &plugin.info.actions[0];
    assert!(action.supported_in_multi_actions);
    assert!(action.supported_in_key_logic_actions);
    assert!(action.visible_in_actions_list);
    assert!(action.user_title_enabled);
    assert_eq!(action.state_names, ["Off", "On"]);
    let mut manifest = plugin.manifest;
    manifest["Software"]["MinimumVersion"] = json!("7.0");
    for field in [
        "SupportedInMultiActions",
        "SupportedInKeyLogicActions",
        "VisibleInActionsList",
        "UserTitleEnabled",
    ] {
        manifest["Actions"][0][field] = json!(false);
    }
    std::fs::write(
        path.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let plugin = read_plugin(&path).unwrap();
    let action = &plugin.info.actions[0];
    assert!(!action.supported_in_multi_actions);
    assert!(!action.supported_in_key_logic_actions);
    assert!(!action.visible_in_actions_list);
    assert!(!action.user_title_enabled);
    // Hidden actions stay addressable by packaged/imported profiles
    assert!(plugin.metadata.contains_key("fixture.vendor.action"));
    let sample = read_plugin(&sample()).unwrap();
    assert!(!sample.info.actions[0].supported_in_multi_actions);
    assert!(sample.info.actions[0].user_title_enabled);
    for field in [
        "SupportedInMultiActions",
        "SupportedInKeyLogicActions",
        "VisibleInActionsList",
        "UserTitleEnabled",
        "DisableAutomaticStates",
    ] {
        let mut invalid = manifest.clone();
        invalid["Actions"][0][field] = json!("false");
        std::fs::write(
            path.join("manifest.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert!(read_plugin(&path).is_err(), "{field}");
    }
}

#[test]
// Verify vendor automatic key states toggle before press payload and respect opt out
fn vendor_automatic_key_states_toggle_before_press_payload_and_respect_opt_out() {
    for disabled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        manifest_vendor_package(root.path(), disabled);
        let mut host = lifecycle_tests::host(root.path());
        host.enable("fixture.vendor", true).unwrap();
        lifecycle_tests::demand(&mut host, "fixture.vendor");
        let context = host.hub.lock().unwrap().binding_context(
            "fixture.vendor",
            "profile:page",
            "device",
            "key",
        );
        for input in [
            hardware_core::Input::Button { pressed: true },
            hardware_core::Input::Button { pressed: false },
            hardware_core::Input::Pulse,
        ] {
            host.dispatch(
                "fixture.vendor",
                "fixture.vendor.action",
                "device",
                "key",
                "profile:page",
                &input,
                &json!({}),
            )
            .unwrap();
        }
        let mut hub = host.hub.lock().unwrap();
        let states: Vec<_> = hub.sessions.queues["fixture.vendor"]
            .iter()
            .filter(|event| event["event"] == "keyDown" || event["event"] == "keyUp")
            .map(|event| event["payload"]["state"].as_u64().unwrap())
            .collect();
        assert_eq!(
            states,
            if disabled {
                vec![0, 0, 0, 0]
            } else {
                vec![1, 1, 0, 0]
            }
        );
        if !disabled {
            assert_eq!(hub.visual_state.visuals[&context]["state"], 0);
            assert_eq!(hub.visual_state.visuals[&context]["title"], "off");
            assert_eq!(hub.visual_state.software_visuals[&context]["state"], 0);
        }
        let before = hub.visual_state.visuals.get(&context).cloned();
        hub.sessions
            .queues
            .get_mut("fixture.vendor")
            .unwrap()
            .fail("test rejected queue");
        drop(hub);
        assert!(host
            .dispatch(
                "fixture.vendor",
                "fixture.vendor.action",
                "device",
                "key",
                "profile:page",
                &hardware_core::Input::Pulse,
                &json!({})
            )
            .is_err());
        assert_eq!(
            host.hub.lock().unwrap().visual_state.visuals.get(&context),
            before.as_ref()
        );
    }
}

#[test]
// Verify automatic states do not apply to encoder macro or manager contexts
fn automatic_states_do_not_apply_to_encoder_macro_or_manager_contexts() {
    for mode in ["encoder", "macro", "manager", "vsd"] {
        let root = tempfile::tempdir().unwrap();
        manifest_vendor_package(root.path(), false);
        let mut host = lifecycle_tests::host(root.path());
        if mode == "vsd" {
            host.plugins.get_mut("fixture.vendor").unwrap().info.dialect = "vsd".into();
        }
        if mode == "manager" {
            host.plugins
                .get_mut("fixture.vendor")
                .unwrap()
                .info
                .capabilities = Some(Default::default());
        }
        host.enable("fixture.vendor", true).unwrap();
        lifecycle_tests::demand(&mut host, "fixture.vendor");
        let scope = if mode == "macro" {
            "profile:page@macro"
        } else {
            "profile:page"
        };
        let context =
            host.hub
                .lock()
                .unwrap()
                .binding_context("fixture.vendor", scope, "device", "key");
        if mode == "encoder" {
            host.hub
                .lock()
                .unwrap()
                .context_state
                .visible
                .insert(context.clone(), json!({"payload":{"controller":"Encoder"}}));
        }
        host.dispatch_with_state(
            "fixture.vendor",
            "fixture.vendor.action",
            "device",
            "key",
            scope,
            &hardware_core::Input::Pulse,
            &json!({}),
            (mode == "macro").then_some(0),
        )
        .unwrap();
        let hub = host.hub.lock().unwrap();
        assert!(
            hub.sessions.queues["fixture.vendor"]
                .iter()
                .all(|event| event["payload"]["state"] == 0),
            "{mode}"
        );
        assert!(!hub.visual_state.visuals.contains_key(&context), "{mode}");
    }
}

#[test]
// Verify elgato multi action desired state is explicit separate and context local
fn elgato_multi_action_desired_state_is_explicit_separate_and_context_local() {
    let root = tempfile::tempdir().unwrap();
    manifest_vendor_package(root.path(), false);
    let mut host = lifecycle_tests::host(root.path());
    host.enable("fixture.vendor", true).unwrap();
    lifecycle_tests::demand(&mut host, "fixture.vendor");
    let settings = json!({"state": "opaque", "userDesiredState": "also opaque"});
    for (scope, current, desired) in [("profile:page@0", 1, 0), ("profile:page@1", 0, 1)] {
        let context =
            host.hub
                .lock()
                .unwrap()
                .binding_context("fixture.vendor", scope, "device", "key");
        host.hub
            .lock()
            .unwrap()
            .visual_state
            .visuals
            .insert(context.clone(), json!({"state":current}).into());
        host.dispatch_with_state(
            "fixture.vendor",
            "fixture.vendor.action",
            "device",
            "key",
            scope,
            &hardware_core::Input::Pulse,
            &settings,
            Some(desired),
        )
        .unwrap();
        let hub = host.hub.lock().unwrap();
        let events: Vec<_> = hub.sessions.queues["fixture.vendor"]
            .iter()
            .filter(|event| event["context"] == context)
            .collect();
        assert_eq!(events.len(), 3);
        for event in &events {
            assert_eq!(event["payload"]["state"], current);
            assert_eq!(event["payload"]["settings"], settings);
            assert_eq!(event["payload"]["isInMultiAction"], true);
            assert!(event["payload"].get("coordinates").is_none());
            assert_eq!(event["payload"]["controller"], "Keypad");
        }
        assert!(events[0]["payload"].get("userDesiredState").is_none());
        for event in &events[1..] {
            assert_eq!(event["payload"]["userDesiredState"], desired);
        }
        assert_eq!(hub.visual_state.visuals[&context]["state"], current);
    }
}

#[test]
// Verify elgato multi action selection rejects missing out of range and wrong context
fn elgato_multi_action_selection_rejects_missing_out_of_range_and_wrong_context() {
    for mode in [
        "missing",
        "range",
        "single",
        "unsupported",
        "vsd",
        "manager",
        "encoder",
        "one-state",
    ] {
        let root = tempfile::tempdir().unwrap();
        manifest_vendor_package(root.path(), false);
        let mut host = lifecycle_tests::host(root.path());
        let plugin = host.plugins.get_mut("fixture.vendor").unwrap();
        if mode == "unsupported" {
            plugin.info.actions[0].supported_in_multi_actions = false;
        }
        if mode == "vsd" {
            plugin.info.dialect = "vsd".into();
        }
        if mode == "manager" {
            plugin.info.capabilities = Some(Default::default());
        }
        if mode == "one-state" {
            Arc::make_mut(&mut plugin.metadata)
                .get_mut("fixture.vendor.action")
                .unwrap()["States"] = json!([{}]);
        }
        host.enable("fixture.vendor", true).unwrap();
        lifecycle_tests::demand(&mut host, "fixture.vendor");
        let scope = if mode == "single" {
            "profile:page"
        } else {
            "profile:page@0"
        };
        if mode == "encoder" {
            let mut hub = host.hub.lock().unwrap();
            let context = hub.binding_context("fixture.vendor", scope, "device", "key");
            hub.context_state
                .visible
                .insert(context, json!({"payload":{"controller":"Encoder"}}));
        }
        let desired = if mode == "missing" {
            None
        } else if mode == "range" {
            Some(2)
        } else {
            Some(1)
        };
        let before = host
            .hub
            .lock()
            .unwrap()
            .sessions
            .queues
            .get("fixture.vendor")
            .map(|q| q.iter().count())
            .unwrap_or(0);
        assert!(
            host.dispatch_with_state(
                "fixture.vendor",
                "fixture.vendor.action",
                "device",
                "key",
                scope,
                &hardware_core::Input::Pulse,
                &json!({}),
                desired
            )
            .is_err(),
            "{mode}"
        );
        let after = host
            .hub
            .lock()
            .unwrap()
            .sessions
            .queues
            .get("fixture.vendor")
            .map(|q| q.iter().count())
            .unwrap_or(0);
        assert_eq!(before, after, "{mode}");
    }
}

#[test]
// Verify elgato encoder defaults initialize existing layout and keep runtime overrides
fn elgato_encoder_defaults_initialize_existing_layout_and_keep_runtime_overrides() {
    for mode in ["encoder", "keypad", "vsd"] {
        let root = tempfile::tempdir().unwrap();
        manifest_vendor_package(root.path(), false);
        let package = root.path().join("fixture");
        std::fs::write(package.join("background.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#ff0000"/></svg>"##).unwrap();
        std::fs::write(
            package.join("layout.json"),
            json!({"id":"declared","items":[{"key":"title","type":"text","rect":[0,0,200,30]}]})
                .to_string(),
        )
        .unwrap();
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
        manifest["Actions"][0]["Encoder"] = json!({"layout":"layout.json","background":"background","TriggerDescription":{"Push":"Mute","Rotate":"Adjust"}});
        std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
        let mut host = lifecycle_tests::host(root.path());
        if mode == "vsd" {
            host.plugins.get_mut("fixture.vendor").unwrap().info.dialect = "vsd".into();
        }
        host.enable("fixture.vendor", true).unwrap();
        lifecycle_tests::demand(&mut host, "fixture.vendor");
        let action = || ActiveAction {
            plugin_id: "fixture.vendor".into(),
            action_id: "fixture.vendor.action".into(),
            device_id: "device".into(),
            control_id: "dial".into(),
            scope: "profile:page".into(),
            settings: json!({}),
            surface: None,
            column: 0,
            row: 0,
            encoder: mode != "keypad",
        };
        host.synchronize(vec![action()], vec![]);
        let context = host.hub.lock().unwrap().binding_context(
            "fixture.vendor",
            "profile:page",
            "device",
            "dial",
        );
        {
            let mut hub = host.hub.lock().unwrap();
            let visual = &hub.visual_state.visuals[&context];
            if mode != "encoder" {
                assert!(visual["layout"].is_null());
                assert!(visual.get("encoderBackground").is_none());
                assert!(visual.get("triggerDescriptions").is_none());
                continue;
            }
            assert_eq!(visual["layout"]["id"], "declared");
            assert_eq!(
                visual["triggerDescriptions"],
                json!({"push":"Mute","rotate":"Adjust"})
            );
            assert!(visual["encoderBackground"].as_str().unwrap().len() < 512 * 1024);
            assert_eq!(
                hub.visual_state.software_visuals[&context]["encoderBackground"],
                visual["encoderBackground"]
            );
            for mut message in [
                json!({"event":"setFeedbackLayout","context":context,"payload":{"layout":"$A0"}}),
                json!({"event":"setTriggerDescription","context":context,"payload":{"push":"Runtime override"}}),
            ] {
                visual::prepare(&mut hub, "fixture.vendor", &mut message).unwrap();
            }
        }
        host.synchronize(vec![action()], vec![]);
        let hub = host.hub.lock().unwrap();
        assert_eq!(hub.visual_state.visuals[&context]["layoutReference"], "$A0");
        assert_eq!(
            hub.visual_state.visuals[&context]["triggerDescriptions"],
            json!({"push":"Runtime override"})
        );
    }
}

#[test]
// Verify elgato action OS filter keeps other actions and retains raw metadata
fn elgato_action_os_filter_keeps_other_actions_and_retains_raw_metadata() {
    let root = tempfile::tempdir().unwrap();
    manifest_vendor_package(root.path(), false);
    let package = root.path().join("fixture");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
    let mut excluded = manifest["Actions"][0].clone();
    excluded["UUID"] = json!("fixture.vendor.other");
    excluded["OS"] = json!([if sdk_platform() == "mac" {
        "windows"
    } else {
        "mac"
    }]);
    manifest["Actions"].as_array_mut().unwrap().push(excluded);
    std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
    let plugin = read_plugin(&package).unwrap();
    assert_eq!(plugin.info.actions.len(), 1);
    assert_eq!(plugin.info.actions[0].id, "fixture.vendor.action");
    assert!(plugin.metadata.contains_key("fixture.vendor.other"));
    assert!(vendor_presentation::supports_os(
        &json!({"OS":["mac"]}),
        "mac"
    ));
    assert!(!vendor_presentation::supports_os(
        &json!({"OS":["windows"]}),
        "mac"
    ));
    assert!(vendor_presentation::supports_os(&json!({}), "linux"));
}

#[test]
// Verify elgato catalogue uses distinct manifest thumbnails with a global output budget
fn elgato_catalogue_uses_distinct_manifest_thumbnails_with_a_global_output_budget() {
    use base64::Engine;
    let root = tempfile::tempdir().unwrap();
    manifest_vendor_package(root.path(), false);
    let package = root.path().join("fixture");
    for (name, color) in [
        ("list", "#ff0000"),
        ("multi", "#0000ff"),
        ("dial", "#00ff00"),
    ] {
        std::fs::write(package.join(format!("{name}.svg")),format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="{color}"/></svg>"#)).unwrap();
    }
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
    manifest["Actions"][0]["Icon"] = json!("list");
    manifest["Actions"][0]["Encoder"]["Icon"] = json!("dial");
    manifest["Actions"][0]["Encoder"]["StackColor"] = json!("#D60270");
    manifest["Actions"][0]["States"][0]["Image"] = json!("list");
    manifest["Actions"][0]["States"][0]["MultiActionImage"] = json!("multi");
    manifest["Actions"][0]["States"][1]["Image"] = json!("list");
    std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
    let plugin = read_plugin(&package).unwrap();
    let action = &plugin.info.actions[0];
    let images = action.multi_action_images.as_ref().unwrap();
    assert_ne!(images[0], action.icon);
    assert_eq!(images[1], action.icon);
    assert_ne!(action.encoder_icon, action.icon);
    for image in [&action.icon, &action.encoder_icon, &images[0]] {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(image.as_ref().unwrap().split_once(',').unwrap().1)
            .unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 72);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 72);
    }
    let mut plugins = vec![plugin.info; 4];
    for plugin in &mut plugins {
        plugin.icon = Some("x".repeat(700_000));
    }
    vendor_presentation::bound_catalogue(&mut plugins);
    let total: usize = plugins
        .iter()
        .map(|plugin| {
            plugin.icon.as_ref().map_or(0, String::len)
                + plugin
                    .actions
                    .iter()
                    .map(|action| {
                        action.icon.as_ref().map_or(0, String::len)
                            + action.encoder_icon.as_ref().map_or(0, String::len)
                            + action
                                .multi_action_images
                                .as_ref()
                                .into_iter()
                                .flatten()
                                .flatten()
                                .map(String::len)
                                .sum::<usize>()
                    })
                    .sum::<usize>()
        })
        .sum();
    assert!(total <= 2 * 1024 * 1024);
    assert!(plugins.iter().any(|plugin| plugin.icon.is_none()));
}
