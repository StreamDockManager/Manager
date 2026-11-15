#[path = "../examples/support/mod.rs"]
mod support;
use hardware_core::Input;
use std::{io::Write, time::Duration};
use stream_dock_manager_plugin_host::{FeedbackKind, HostEffect, PluginHost};

#[test]
#[ignore = "Requires STREAM_DOCK_MANAGER_NODE set to an absolute Node 24 executable and local loopback sockets"]
// Verify node package registers receives input and returns scoped feedback
fn node_package_registers_receives_input_and_returns_scoped_feedback() {
    let root = std::env::temp_dir().join(format!(
        "stream-dock-manager-plugin-test-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let archive = root.join("counter.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    for (name, bytes) in [
        (
            "manifest.json",
            include_bytes!("../../../tests/fixtures/plugins/counter/manifest.json").as_slice(),
        ),
        (
            "counter.mjs",
            include_bytes!("../../../tests/fixtures/plugins/counter/counter.mjs").as_slice(),
        ),
        (
            "icon.svg",
            include_bytes!("../../../tests/fixtures/plugins/counter/icon.svg").as_slice(),
        ),
    ] {
        zip.start_file(
            format!("app.streamdockmanager.plugins.fixture.counter.sdPlugin/{name}"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    let mut host = support::protocol_host(&root).unwrap();
    let info = host.install(&archive).unwrap();
    assert!(!info.enabled);
    support::runtime(&host, &info.id, false).unwrap();
    host.enable(&info.id, true).unwrap();
    support::demand(&mut host, &info.id, "device-a");
    host.dispatch(
        &info.id,
        "app.streamdockmanager.plugins.fixture.counter.increment",
        "device-a",
        "key-2",
        "default:main",
        &Input::Button { pressed: true },
        &serde_json::json!({}),
    )
    .unwrap();
    let mut feedback = vec![];
    for _ in 0..100 {
        feedback.extend(host.drain_effects());
        if !feedback.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(feedback.len(), 1);
    let HostEffect::Feedback(first) = &feedback[0] else {
        panic!("feedback expected");
    };
    assert_eq!(first.kind, FeedbackKind::Title);
    assert_eq!(first.visual.as_ref().unwrap()["title"], "1");
    let target = first.target.as_ref().unwrap();
    assert_eq!(target.device, "device-a");
    assert_eq!(target.control, "key-2");
    let added = root.join("plugins/new-browser");
    std::fs::create_dir(&added).unwrap();
    std::fs::write(added.join("runtime.html"), "<!doctype html>").unwrap();
    std::fs::write(added.join("manifest.json"), serde_json::to_vec(&serde_json::json!({
        "UUID": "fixture.new-browser", "Name": "New browser", "Version": "1.0", "CodePath": "runtime.html", "Actions": []
    })).unwrap()).unwrap();
    assert_eq!(host.reload().unwrap().added, 1);
    assert!(
        !host
            .list()
            .iter()
            .find(|p| p.id == "fixture.new-browser")
            .unwrap()
            .enabled
    );
    host.dispatch(
        &info.id,
        "app.streamdockmanager.plugins.fixture.counter.increment",
        "device-a",
        "key-2",
        "default:main",
        &Input::Button { pressed: true },
        &serde_json::json!({}),
    )
    .unwrap();
    feedback.clear();
    for _ in 0..100 {
        feedback.extend(host.drain_effects());
        if !feedback.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(feedback.len(), 1);
    let HostEffect::Feedback(second) = &feedback[0] else {
        panic!("feedback expected");
    };
    assert_eq!(second.visual.as_ref().unwrap()["title"], "2");
    host.enable(&info.id, false).unwrap();
    if let Some(stopped) = host.stop_for_replacement(&info.id).unwrap() {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !stopped.load(std::sync::atomic::Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < deadline,
                "owned process cleanup timed out"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    host.delete(&info.id).unwrap();
    assert_eq!(host.list().len(), 1);
    assert!(!root.join("plugins").join(&info.id).exists());
    assert!(host
        .dispatch(
            &info.id,
            "app.streamdockmanager.plugins.fixture.counter.increment",
            "device-a",
            "key-2",
            "default:main",
            &Input::Pulse,
            &serde_json::json!({})
        )
        .is_err());
    host.shutdown();
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "Requires built official SDK fixture, explicit Node 24, and loopback sockets"]
// Verify official elgato SDK executes legacy and rejects older manifest in default mode
fn official_elgato_sdk_executes_legacy_and_rejects_older_manifest_in_default_mode() {
    use serde_json::json;
    use stream_dock_manager_plugin_host::ActiveAction;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/plugins/elgato-sdk-executable");
    let bundle =
        std::fs::read(fixture.join("package/com.manager.fixture.elgato.sdPlugin/plugin.js"))
            .expect("Run npm ci --ignore-scripts && npm run build in elgato-sdk-executable first");
    for legacy in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("com.manager.fixture.elgato.sdPlugin");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(package.join("plugin.js"), &bundle).unwrap();
        std::fs::copy(
            fixture.join("src/manifest.json"),
            package.join("manifest.json"),
        )
        .unwrap();
        // Source-valid disposable artwork; no installed vendor assets are read
        for (name, size) in [("plugin-icon", 256), ("action-icon", 20), ("key", 72)] {
            for (suffix, scale) in [("", 1), ("@2x", 2)] {
                image::RgbaImage::new(size * scale, size * scale)
                    .save(package.join(format!("{name}{suffix}.png")))
                    .unwrap();
            }
        }
        std::fs::write(package.join("package.json"), r#"{"type":"module"}"#).unwrap();
        std::fs::write(
            package.join("mode.json"),
            json!({"legacy":legacy}).to_string(),
        )
        .unwrap();
        let mut host = support::protocol_host(root.path()).unwrap();
        let prepared = PluginHost::prepare_install(&host.installation_root(), &package).unwrap();
        let info = host.replace_prepared(prepared).unwrap();
        assert!(info.capabilities.is_none());
        // The official 6.9 schema declares Node 20. Node 24 is a deliberate
        // test-only runtime override, never ordinary package qualification.
        let mismatch = support::runtime(&host, &info.id, false).unwrap_err();
        assert!(
            mismatch.contains("Acknowledge the compatibility warning"),
            "{mismatch}"
        );
        support::runtime(&host, &info.id, true).unwrap();
        let runtime = host.preflight(&info.id).unwrap().runtime.unwrap();
        assert_eq!(runtime.requirement.version.as_deref(), Some("20"));
        assert!(runtime.overridden);
        assert_eq!(runtime.selected.as_ref().unwrap().line, "24");
        assert_eq!(runtime.state, "ready");
        host.enable(&info.id, true).unwrap();
        support::demand(&mut host, &info.id, "device-a");
        let result_path = host
            .installation_root()
            .join(&info.id)
            .join("connection-result.json");
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !result_path.exists() {
            host.poll();
            assert!(
                std::time::Instant::now() < deadline,
                "SDK did not report connection result: {:?}",
                host.list()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(result_path).unwrap()).unwrap();
        if !legacy {
            // The genuine SDK checks the plugin manifest minimum independently
            // of the now-supported host level. Do not bypass this declaration.
            assert_eq!(result["connected"], false);
            let error = result["error"].as_str().unwrap();
            assert!(
                error.contains("Software.MinimumVersion") && error.contains("7.1"),
                "{result}"
            );
            assert!(!error.contains("but current version is"), "{result}");
            host.shutdown();
            continue;
        }
        assert_eq!(result["connected"], true, "{result}");
        host.synchronize(vec![ActiveAction{plugin_id:info.id.clone(),action_id:info.actions[0].id.clone(),device_id:"device-a".into(),control_id:"key-1".into(),scope:"fixture".into(),settings:json!({"marker":"initial"}),surface:None,column:0,row:0,encoder:false}], vec![json!({"id":"device-a","name":"Disposable simulator","type":0,"size":{"columns":5,"rows":3}})]);
        let take_title = |host: &mut PluginHost| -> String {
            let deadline = std::time::Instant::now() + Duration::from_secs(8);
            loop {
                host.poll();
                for effect in host.drain_effects() {
                    if let HostEffect::Feedback(feedback) = effect {
                        if feedback.kind == FeedbackKind::Title {
                            return feedback.message["payload"]["title"]
                                .as_str()
                                .unwrap()
                                .into();
                        }
                    }
                }
                assert!(std::time::Instant::now() < deadline, "SDK feedback timeout");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        assert_eq!(take_title(&mut host), "ready:initial");
        host.dispatch(
            &info.id,
            &info.actions[0].id,
            "device-a",
            "key-1",
            "fixture",
            &Input::Button { pressed: true },
            &json!({"marker":"initial"}),
        )
        .unwrap();
        let outcome: serde_json::Value = serde_json::from_str(&take_title(&mut host)).unwrap();
        assert_eq!(outcome["settings"], json!({"marker":"saved","count":2}));
        assert_eq!(outcome["globals"], json!({"fixture":"global-roundtrip"}));
        assert_eq!(outcome["version"], "7.1.0");
        host.shutdown();
    }
}

#[test]
// Verify protocol host discovers offline metadata without installing a runtime
fn protocol_host_discovers_offline_metadata_without_installing_a_runtime() {
    let root = tempfile::tempdir().unwrap();
    let host = support::protocol_host(root.path()).unwrap();
    let status = host.runtimes.status();
    assert!(status.error.is_none());
    assert_eq!(status.lines.len(), 1);
    assert_eq!(status.lines[0].line, "24");
    assert_eq!(status.releases.len(), 1);
    assert_eq!(status.releases[0].version, "24.20.0");
    assert!(status.installations.is_empty());
}
