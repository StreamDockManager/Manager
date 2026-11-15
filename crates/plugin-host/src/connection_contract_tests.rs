use super::*;
use crate::*;

// Construct the connection contract test inputs
fn inputs() -> (Value, Value, validation::Registry) {
    (
        serde_json::from_str(include_str!("../connection-contract.v1.json")).unwrap(),
        serde_json::from_str(include_str!("../connection-contract.schema.json")).unwrap(),
        serde_json::from_str(include_str!("../connection-implementations.v1.json")).unwrap(),
    )
}

#[test]
// Verify schema rejects invalid static configuration
fn schema_rejects_invalid_static_configuration() {
    let (config, schema, registry) = inputs();
    validation::validate(&config, &schema, &registry).unwrap();
    validation::validate_evidence(Path::new(env!("CARGO_MANIFEST_DIR")), &registry).unwrap();
    for (pointer, value) in [
        ("/schema_version", json!(2)),
        ("/manager_contract_version", json!("1")),
        ("/surface_contract_version", json!(0)),
        ("/capabilities", json!([])),
        (
            "/capabilities",
            json!(["plugin-theme-v1", "plugin-theme-v1"]),
        ),
        ("/vendors/streamdeck/sdk_versions", json!([2, 2])),
        ("/vendors/vsd/application_version", json!("version3")),
        ("/vendors/vsd/compatibility_label", json!("full")),
    ] {
        let mut invalid = config.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        assert!(
            validation::validate(&invalid, &schema, &registry).is_err(),
            "{pointer}"
        );
    }
    for key in ["schema_version", "capabilities", "vendors"] {
        let mut missing = config.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(
            validation::validate(&missing, &schema, &registry).is_err(),
            "{key}"
        );
    }
    // Session and machine state cannot become static configuration by accident
    for key in [
        "version",
        "run_id",
        "session_token",
        "platform",
        "language",
        "theme",
        "devicePixelRatio",
        "permissions",
    ] {
        let mut invalid = config.clone();
        invalid[key] = json!("runtime-only");
        assert!(
            validation::validate(&invalid, &schema, &registry).is_err(),
            "{key}"
        );
    }
}

#[test]
// Verify config cannot expand implementation claims
fn config_cannot_expand_implementation_claims() {
    let (config, schema, registry) = inputs();
    // All of these edits satisfy the JSON Schema but lack implemented support
    for (pointer, value) in [
        ("/manager_contract_version", json!(2)),
        ("/surface_contract_version", json!(2)),
        ("/capabilities", json!(["unsupported-v1"])),
        ("/vendors/streamdeck/application_version", json!("7.6.0")),
        ("/vendors/vsd/application_version", json!("4.0.0")),
        (
            "/vendors/streamdeck/compatibility_label",
            json!("streamdeck-7.6-subset"),
        ),
        ("/vendors/streamdeck/sdk_versions", json!([2, 3, 4])),
        ("/vendors/vsd/sdk_versions", json!([1, 2])),
    ] {
        let mut unsupported = config.clone();
        *unsupported.pointer_mut(pointer).unwrap() = value;
        assert!(jsonschema::is_valid(&schema, &unsupported));
        assert!(
            validation::validate(&unsupported, &schema, &registry).is_err(),
            "{pointer}"
        );
    }
    let (_, _, mut no_evidence) = inputs();
    no_evidence.capabilities[0].tests.clear();
    assert!(
        validation::validate_evidence(Path::new(env!("CARGO_MANIFEST_DIR")), &no_evidence).is_err()
    );
}

#[test]
// Verify registration and admission share the embedded contract
fn registration_and_admission_share_the_embedded_contract() {
    let (config, schema, registry) = inputs();
    let config = validation::validate(&config, &schema, &registry).unwrap();
    assert_eq!(CAPABILITIES, config.capabilities);
    let info = manager_registration("runtime-a", false);
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["capabilities"], json!(CAPABILITIES));
    assert_eq!(info["contract_version"], config.manager_contract_version);
    assert_eq!(
        info["compatibility"],
        config.vendors.streamdeck.compatibility_label
    );
    assert_eq!(
        info["vsd_compatibility"],
        config.vendors.vsd.compatibility_label
    );
    let next = manager_registration("runtime-b", true);
    assert_eq!(next["run_id"], "runtime-b");
    assert_eq!(next["debug_network"], true);
    assert_eq!(info["run_id"], "runtime-a");
    assert_eq!(info["debug_network"], false);
    let hub = Hub {
        registration_environment: RegistrationEnvironment::new(Some("en-US"), Some(2.0)).unwrap(),
        ..Default::default()
    };
    let host_info =
        browser::registration_info(&hub, "plugin", "9.8.7", "streamdeck", false, false).unwrap();
    assert_eq!(
        host_info["application"]["version"],
        STREAMDECK.application_version
    );
    assert_eq!(host_info["plugin"]["version"], "9.8.7");
    for contract in [&STREAMDECK, &VSD] {
        for inspector in [false, true] {
            let info = browser::registration_info(
                &hub,
                "plugin",
                "9.8.7",
                contract.dialect,
                inspector,
                inspector,
            )
            .unwrap();
            assert_eq!(info["application"]["version"], contract.application_version);
            assert_eq!(info["plugin"]["version"], "9.8.7");
        }
        for &sdk in contract.sdk_versions {
            let mut manifest = json!({"SDKVersion":sdk});
            assert_eq!(manifest::dialect(&manifest), contract.dialect);
            assert_eq!(vendor_for_sdk(u64::from(sdk)), Some(contract));
            manifest["StreamDockManager"]["ManifestDialect"] = json!(contract.dialect);
            assert_eq!(manifest::dialect(&manifest), contract.dialect);
        }
    }
    assert!(
        browser::registration_info(&hub, "plugin", "9.8.7", "unsupported", false, false).is_err()
    );
    for sdk in [0, 4, u64::MAX] {
        assert!(vendor_for_sdk(sdk).is_none());
        assert_eq!(manifest::dialect(&json!({"SDKVersion":sdk})), "unsupported");
    }
    assert_eq!(
        manifest::dialect(
            &json!({"SDKVersion":1,"StreamDockManager":{"ManifestDialect":"unknown"}})
        ),
        "unsupported"
    );
}

// Write vendor
fn write_vendor(root: &Path, contract: &VendorContract, minimum: &str) {
    let mut manifest = if contract.dialect == "streamdeck" {
        manifest_schema::fixture("org.contract.fixture", "plugin.html")
    } else {
        json!({"UUID":"org.contract.fixture","Name":"Contract fixture","Version":"0.1.0","SDKVersion":contract.sdk_versions[0],"CodePath":"plugin.html","Actions":[]})
    };
    manifest["Software"] = json!({"MinimumVersion":minimum});
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("plugin.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
}

#[test]
// Verify vendor minimums remain fail closed
fn vendor_minimums_remain_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    for contract in [&STREAMDECK, &VSD] {
        write_vendor(
            root.path(),
            contract,
            if contract.dialect == "streamdeck" {
                "7.1"
            } else {
                contract.application_version
            },
        );
        let report = preflight::inspect(root.path());
        assert!(report
            .checks
            .iter()
            .any(|check| check.code == "vendor_contract" && check.level == "warning"));
        assert!(!report
            .checks
            .iter()
            .any(|check| check.code == "vendor_contract" && check.level == "blocker"));
        write_vendor(root.path(), contract, "99.0");
        let report = preflight::inspect(root.path());
        assert!(!report.can_enable);
        assert!(report.checks.iter().any(|check| matches!(
            check.code.as_str(),
            "vendor_contract" | "manifest_schema"
        ) && check.level == "blocker"));
        write_vendor(root.path(), contract, "invalid");
        assert!(!preflight::inspect(root.path()).can_install);
    }
    // A required capability is admitted by the very same inventory advertised
    write_vendor(root.path(), &STREAMDECK, "6.9");
    let path = root.path().join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["StreamDockManager"] =
        json!({"MinimumVersion":env!("CARGO_PKG_VERSION"),"RequiredCapabilities":CAPABILITIES});
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(!preflight::inspect(root.path())
        .checks
        .iter()
        .any(|check| check.code == "manager_contract"));
    manifest["StreamDockManager"]["RequiredCapabilities"] = json!(["unsupported-v1"]);
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(!preflight::inspect(root.path()).can_install);
}

#[test]
// Verify surface metadata uses the embedded version
fn surface_metadata_uses_the_embedded_version() {
    let root = tempfile::tempdir().unwrap();
    lifecycle_tests::package(&root.path().join("plugin"), "fixture.surface");
    let mut host = lifecycle_tests::host(root.path());
    host.enable("fixture.surface", true).unwrap();
    lifecycle_tests::demand(&mut host, "fixture.surface");
    host.dispatch(
        "fixture.surface",
        "fixture.surface.action",
        "device",
        "key",
        "profile:page",
        &hardware_core::Input::Button { pressed: true },
        &json!({}),
    )
    .unwrap();
    let mut hub = host.hub.lock().unwrap();
    let queue = hub.sessions.queues.get_mut("fixture.surface").unwrap();
    let event = queue
        .iter()
        .find(|message| message["event"] == "keyDown")
        .unwrap();
    assert_eq!(
        event["streamDockManager"]["contract_version"],
        SURFACE_CONTRACT_VERSION
    );
    assert_eq!(event["streamDockManager"]["control_id"], "key");
}

#[test]
// Verify managed controls and telemetry use authenticated transport
fn managed_controls_and_telemetry_use_authenticated_transport() {
    let root = tempfile::tempdir().unwrap();
    lifecycle_tests::package(&root.path().join("plugin"), "fixture.controls");
    let mut host = lifecycle_tests::socket_host(root.path());
    host.enable("fixture.controls", true).unwrap();
    lifecycle_tests::demand(&mut host, "fixture.controls");
    let token = host.plugins["fixture.controls"].token.clone();
    let capabilities = host.plugins["fixture.controls"]
        .info
        .capabilities
        .clone()
        .unwrap();
    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{}", host.port)).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    let send = |socket: &mut tungstenite::WebSocket<_>, value: Value| {
        socket
            .send(tungstenite::Message::text(value.to_string()))
            .unwrap()
    };
    send(
        &mut socket,
        json!({"event":"registerPlugin","uuid":token,"payload":{"capabilities":capabilities}}),
    );
    send(
        &mut socket,
        json!({"event":"subscribeTelemetry","payload":{"topics":["cpu","memory"]}}),
    );
    send(
        &mut socket,
        json!({"event":"setDynamicItems","payload":{"items":[{"id":"one","title":"First"}]}}),
    );
    // Registration also emits global settings. Match this request's echoed ID
    // so that unsolicited startup state cannot satisfy the FIFO barrier.
    let barrier = "connection-contract-extension-barrier";
    send(
        &mut socket,
        json!({"event":"getGlobalSettings","id":barrier}),
    );
    loop {
        let message: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        if message["event"] == "didReceiveGlobalSettings" && message["id"] == barrier {
            break;
        }
    }
    assert_eq!(
        host.telemetry_topics(),
        std::collections::BTreeSet::from(["cpu".into(), "memory".into()])
    );
    assert!(host.drain_effects().iter().any(|effect| matches!(effect, HostEffect::Feedback(effect) if effect.message["event"] == "setDynamicItems" && effect.message["payload"]["items"][0]["id"] == "one")));
    host.publish_telemetry(json!({"cpu":{"usage":0.25}}));
    loop {
        let message: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        if message["event"] == "didReceiveTelemetry" {
            assert_eq!(message["payload"]["cpu"]["usage"], 0.25);
            break;
        }
    }
    socket.close(None).unwrap();
}

#[test]
// Verify openaction is an independent bounded contract without SDK claims
fn openaction_is_an_independent_bounded_contract_without_sdk_claims() {
    let contract = vendor("openaction").unwrap();
    assert_eq!(contract, &OPENACTION);
    assert!(contract.sdk_versions.is_empty());
    assert_eq!(manifest::dialect(&json!({"CodePaths":{}})), "openaction");
    for sdk in [1, 2, 3] {
        assert_ne!(vendor_for_sdk(sdk), Some(contract));
    }
    assert_eq!(
        manager_registration("fixture", false)["openaction_compatibility"],
        contract.compatibility_label
    );
    let (mut config, schema, registry) = inputs();
    config["vendors"]["openaction"]["sdk_versions"] = json!([2]);
    assert!(validation::validate(&config, &schema, &registry).is_err());
}
