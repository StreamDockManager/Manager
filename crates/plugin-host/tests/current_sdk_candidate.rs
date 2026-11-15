//! Explicit opt-in production/candidate qualification. This is host/SDK
//! transport evidence, not runtime persistence, a browser inspector or a release claim.
#[path = "../examples/support/mod.rs"]
mod support;

use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{
    connection_contract, ActiveAction, OwnedSettingsWrite, PluginHost,
};

const ID: &str = "com.manager.fixture.elgato";
const ACTION: &str = "com.manager.fixture.elgato.probe";

struct Fixture {
    host: PluginHost,
    package: PathBuf,
    receiver: mpsc::Receiver<OwnedSettingsWrite>,
    sequence: u64,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.host.shutdown();
    }
}
impl Fixture {
    // Construct the value and initialize its state
    fn new() -> Self {
        assert_eq!(
            connection_contract::STREAMDECK.application_version,
            "7.1.0",
            "Run scripts/qualify-current-sdk.py --production"
        );
        assert_eq!(
            connection_contract::STREAMDECK.compatibility_label,
            if std::env::var_os("STREAM_DOCK_MANAGER_PRODUCTION_QUALIFICATION").is_some() {
                "streamdeck-7.1-subset"
            } else {
                "streamdeck-7.1-candidate-subset"
            }
        );
        assert!(
            std::env::var_os("STREAM_DOCK_MANAGER_CANDIDATE_QUALIFICATION").is_some()
                || std::env::var_os("STREAM_DOCK_MANAGER_PRODUCTION_QUALIFICATION").is_some(),
            "Explicit qualification runner execution required"
        );
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/plugins/elgato-sdk-executable");
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join(format!("{ID}.sdPlugin"));
        std::fs::create_dir(&package).unwrap();
        std::fs::copy(
            source.join("package/com.manager.fixture.elgato.sdPlugin/current-mode.js"),
            package.join("plugin.js"),
        )
        .expect("Build the locked official SDK candidate probe first");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(source.join("src/manifest.json")).unwrap())
                .unwrap();
        manifest["Software"]["MinimumVersion"] = json!("7.1");
        manifest["Nodejs"]["Version"] = json!("24");
        manifest["PropertyInspectorPath"] = json!("inspector.html");
        std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(package.join("package.json"), r#"{"type":"module"}"#).unwrap();
        std::fs::write(
            package.join("inspector.html"),
            "<!doctype html><title>Transport fixture only</title>",
        )
        .unwrap();
        for (name, size) in [("plugin-icon", 256), ("action-icon", 20), ("key", 72)] {
            for (suffix, scale) in [("", 1), ("@2x", 2)] {
                image::RgbaImage::new(size * scale, size * scale)
                    .save(package.join(format!("{name}{suffix}.png")))
                    .unwrap();
            }
        }
        let mut host = support::protocol_host(root.path()).unwrap();
        let prepared = PluginHost::prepare_install(&host.installation_root(), &package).unwrap();
        let info = host.replace_prepared(prepared).unwrap();
        assert!(info.capabilities.is_none());
        support::runtime(&host, ID, false).unwrap();
        let resolution = host.preflight(ID).unwrap().runtime.unwrap();
        assert_eq!(resolution.requirement.version.as_deref(), Some("24"));
        assert!(!resolution.overridden);
        let (sender, receiver) = mpsc::sync_channel(8);
        host.set_settings_writer(sender);
        host.enable(ID, true).unwrap();
        support::demand(&mut host, ID, "device");
        let package = host.installation_root().join(ID);
        let mut fixture = Self {
            host,
            package,
            receiver,
            sequence: 0,
            _root: root,
        };
        let report = fixture.wait(|value| value["response"]["sequence"] == 0);
        assert_eq!(report["response"]["connected"], true, "{report}");
        assert_eq!(report["response"]["legacy"], false);
        assert_eq!(report["response"]["version"], "7.1.0");
        fixture.synchronize("Original", &["a", "b"]);
        fixture.wait(|value| {
            value["events"]["appearances"]
                .as_array()
                .is_some_and(|a| a.len() == 2)
        });
        fixture
    }
    // Synchronize for the current operation
    fn synchronize(&mut self, name: &str, markers: &[&str]) {
        let actions = markers
            .iter()
            .enumerate()
            .map(|(index, marker)| ActiveAction {
                plugin_id: ID.into(),
                action_id: ACTION.into(),
                device_id: "device".into(),
                control_id: format!("key-{}", index + 1),
                scope: "candidate".into(),
                settings: json!({"marker":marker}),
                surface: None,
                column: index,
                row: 0,
                encoder: false,
            })
            .collect();
        self.host.synchronize(
            actions,
            vec![json!({"id":"device","name":name,"type":0,"size":{"columns":5,"rows":3}})],
        );
    }
    // Build the report for this operation
    fn report(&self) -> Value {
        std::fs::read(self.package.join("report.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null)
    }
    // Wait for the current operation
    fn wait(&mut self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.host.poll();
            let report = self.report();
            if predicate(&report) {
                return report;
            }
            assert!(
                Instant::now() < deadline,
                "SDK candidate timeout: {report}; host: {:?}",
                self.host.list()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // Resolve the action context used by this operation
    fn context(&self, marker: &str) -> String {
        self.report()["events"]["appearances"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|event| event["settings"]["marker"] == marker)
            .unwrap()["context"]
            .as_str()
            .unwrap()
            .into()
    }
    // Send for the current operation
    fn send(&mut self, operation: &str, context: &str, fields: Value) -> u64 {
        self.sequence += 1;
        let mut command = fields;
        command["sequence"] = json!(self.sequence);
        command["operation"] = json!(operation);
        command["context"] = json!(context);
        std::fs::write(self.package.join("command.tmp"), command.to_string()).unwrap();
        std::fs::rename(
            self.package.join("command.tmp"),
            self.package.join("command.json"),
        )
        .unwrap();
        self.sequence
    }
    // Construct the result test fixture
    fn result(&mut self, sequence: u64) -> Value {
        let value = self.wait(|value| value["response"]["sequence"] == sequence);
        assert!(value["response"].get("error").is_none(), "{value}");
        value
    }
    // Construct the command representation
    fn command(&mut self, operation: &str, context: &str, fields: Value) -> Value {
        let sequence = self.send(operation, context, fields);
        self.result(sequence)
    }
    // Resolve the address used by this operation
    fn address(control: &str) -> Value {
        json!({"profile_id":"candidate-profile","page_id":"main","control_id":control,"path":[]})
    }
    // Bind for the current operation
    fn bind(&mut self) {
        for control in ["key-1", "key-2"] {
            self.host.set_action_address(
                ID,
                "candidate",
                "device",
                control,
                Self::address(control),
            );
        }
    }
    // Report the pending work tracked by this owner
    fn pending(&mut self) -> OwnedSettingsWrite {
        self.receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("SDK command did not reach the owner writer")
    }
    // Commit for the current operation
    fn commit(&mut self, request: &OwnedSettingsWrite) {
        // Deliberately only a transport commit. Real staging/durability belongs to runtime tests
        self.host
            .commit_saved_settings(request, 0, |_| Ok(()))
            .unwrap();
    }
    // Construct the inspector test fixture
    fn inspector(&mut self) -> (String, tungstenite::WebSocket<std::net::TcpStream>) {
        let panel = self
            .host
            .settings_panel(ID, Some(ACTION), &json!({"marker":"a"}))
            .unwrap();
        self.host
            .bind_vendor_panel(&panel.token, Self::address("key-1"))
            .unwrap();
        let address = panel.url.split('/').nth(2).unwrap();
        let stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (mut socket, _) = tungstenite::client(format!("ws://{address}"), stream).unwrap();
        socket
            .send(tungstenite::Message::text(
                json!({"event":"registerPropertyInspector","uuid":panel.token}).to_string(),
            ))
            .unwrap();
        (panel.token, socket)
    }
}

#[test]
#[ignore = "Candidate source copy only, built official SDK, explicit Node24 and loopback required"]
// Verify official SDK default mode candidate semantics
fn official_sdk_default_mode_candidate_semantics() {
    let mut f = Fixture::new();
    let a = f.context("a");
    let b = f.context("b");
    // Startup notification correctness is asserted independently below so it cannot
    // hide failures in cache, PI, resources or changed-device behavior.
    let initial_globals = f.report()["events"]["globals"].as_array().unwrap().clone();
    let query = f.command("query", &a, json!({}));
    assert_eq!(
        query["response"]["result"]["settings"],
        json!({"marker":"a"}),
        "SDK cache must return independent copies"
    );
    assert_eq!(query["events"]["settings"], json!([]));
    assert_eq!(query["events"]["globals"], json!(initial_globals));
    let write = f.command("selfWrite", &a, json!({"settings":{"marker":"runtime"}}));
    assert_eq!(
        write["response"]["result"]["settings"],
        json!({"marker":"runtime"})
    );
    assert_eq!(
        write["response"]["result"]["globals"],
        json!({"source":"runtime"})
    );
    assert_eq!(
        write["events"]["settings"],
        json!([]),
        "Runtime writes and ID getters must not trigger callbacks"
    );
    assert_eq!(write["events"]["globals"], json!(initial_globals));
    let query = f.command("query", &b, json!({}));
    assert_eq!(
        query["response"]["result"]["settings"],
        json!({"marker":"b"})
    );

    f.bind();
    let (token, mut inspector) = f.inspector();
    inspector
        .send(tungstenite::Message::text(
            json!({"event":"setSettings","payload":{"marker":"inspector"}}).to_string(),
        ))
        .unwrap();
    let request = f.pending();
    f.commit(&request);
    let settings = f.wait(|v| {
        v["events"]["settings"]
            .as_array()
            .is_some_and(|a| a.len() == 1)
    });
    assert_eq!(settings["events"]["settings"][0]["context"], a);
    assert_eq!(
        settings["events"]["settings"][0]["cached"],
        json!({"marker":"inspector"})
    );
    inspector
        .send(tungstenite::Message::text(
            json!({"event":"setGlobalSettings","payload":{"source":"inspector"}}).to_string(),
        ))
        .unwrap();
    f.wait(|v| {
        v["events"]["globals"]
            .as_array()
            .is_some_and(|a| a.len() == initial_globals.len() + 1)
    });
    let query = f.command("query", &a, json!({}));
    assert_eq!(
        query["response"]["result"]["settings"],
        json!({"marker":"inspector"})
    );
    assert_eq!(query["events"]["settings"].as_array().unwrap().len(), 1);
    let mut expected_globals = initial_globals;
    expected_globals.push(json!({"source":"inspector"}));
    assert_eq!(query["events"]["globals"], json!(expected_globals));

    let resource = f.package.join("candidate-resource.txt");
    std::fs::write(&resource, "SDK transport resource").unwrap();
    let resources = json!({"document":resource});
    f.command("setResourcesOnly", &a, json!({"resources":resources}));
    let request = f.pending();
    assert_eq!(request.resources, Some(resources.clone()));
    f.commit(&request);
    let query = f.command("queryResources", &a, json!({}));
    assert_eq!(query["response"]["result"]["resources"], resources);
    assert_eq!(query["events"]["resources"], json!([]));
    inspector
        .send(tungstenite::Message::text(
            json!({"event":"setResources","payload":{}}).to_string(),
        ))
        .unwrap();
    let request = f.pending();
    f.commit(&request);
    f.wait(|v| {
        v["events"]["resources"]
            .as_array()
            .is_some_and(|a| a.len() == 1)
    });
    let query = f.command("queryResources", &a, json!({}));
    assert_eq!(query["response"]["result"]["resources"], json!({}));
    assert_eq!(query["events"]["resources"].as_array().unwrap().len(), 1);
    let other = f.command("queryResources", &b, json!({}));
    assert_eq!(other["response"]["result"]["resources"], json!({}));

    f.host.close_inspector(&token);
    f.synchronize("Renamed", &["inspector", "b"]);
    let changed = f.wait(|v| {
        v["events"]["devices"]
            .as_array()
            .is_some_and(|a| a.len() == 1)
    });
    assert_eq!(
        changed["events"]["devices"][0],
        json!({"id":"device","name":"Renamed","size":{"columns":5,"rows":3}})
    );
    f.synchronize("Renamed", &[]);
    f.wait(|v| {
        v["events"]["disappearances"]
            .as_array()
            .is_some_and(|a| a.len() == 2)
    });
    f.synchronize("Renamed", &["reappeared", "b"]);
    f.wait(|v| {
        v["events"]["appearances"]
            .as_array()
            .is_some_and(|a| a.len() == 4)
    });
    let next = f.context("reappeared");
    assert_ne!(next, a);
    let query = f.command("query", &next, json!({}));
    assert_eq!(
        query["response"]["result"]["settings"],
        json!({"marker":"reappeared"})
    );
}

#[test]
#[ignore = "Candidate source copy only; deterministically delays owner commit to expose stale SDK cache"]
// Verify official SDK default mode candidate saved read after write
fn official_sdk_default_mode_candidate_saved_read_after_write() {
    let mut f = Fixture::new();
    let context = f.context("a");
    f.bind();
    let sequence = f.send(
        "selfWrite",
        &context,
        json!({"settings":{"marker":"committed"}}),
    );
    let request = f.pending();
    // Give the genuine SDK's immediate getter time to arrive while the owner is held
    // A correct host cannot finish this read with the obsolete saved value
    std::thread::sleep(Duration::from_millis(150));
    let before = f.report();
    f.commit(&request);
    let result = f.result(sequence);
    assert_eq!(
        result["response"]["result"]["settings"],
        json!({"marker":"committed"}),
        "Read-before-commit observation: {before}"
    );
    assert_eq!(result["events"]["settings"], json!([]));
    let cached = f.command("query", &context, json!({}));
    assert_eq!(
        cached["response"]["result"]["settings"],
        json!({"marker":"committed"})
    );
}

#[test]
#[ignore = "Candidate source copy only; deterministically delays resource owner commit"]
// Verify official SDK default mode candidate resource read after write
fn official_sdk_default_mode_candidate_resource_read_after_write() {
    let mut f = Fixture::new();
    let context = f.context("a");
    f.bind();
    let resource = f.package.join("ordering.txt");
    std::fs::write(&resource, "candidate resource ordering").unwrap();
    let resources = json!({"document":resource});
    let sequence = f.send(
        "resourceRoundtrip",
        &context,
        json!({"resources":resources}),
    );
    let request = f.pending();
    std::thread::sleep(Duration::from_millis(150));
    let before = f.report();
    f.commit(&request);
    let result = f.result(sequence);
    assert_eq!(
        result["response"]["result"]["resources"], resources,
        "Read-before-commit observation: {before}"
    );
    assert_eq!(result["events"]["resources"], json!([]));
}

#[test]
#[ignore = "Candidate source copy only; startup must not impersonate a PI global edit"]
// Verify official SDK default mode candidate startup callbacks
fn official_sdk_default_mode_candidate_startup_callbacks() {
    let mut f = Fixture::new();
    let context = f.context("a");
    let report = f.command("query", &context, json!({}));
    assert_eq!(report["events"]["globals"], json!([]));
    assert_eq!(report["events"]["settings"], json!([]));
    assert_eq!(report["events"]["resources"], json!([]));
}

#[test]
#[ignore = "Candidate source copy only; genuine SDK single and multi-action resource payloads"]
// Verify official SDK candidate single and multi action resource payloads
fn official_sdk_candidate_single_and_multi_action_resource_payloads() {
    let mut f = Fixture::new();
    for event in f.report()["events"]["appearances"].as_array().unwrap() {
        assert_eq!(event["payload"]["resources"], json!({}));
    }
    let single = json!({"document":f.package.join("single.txt")});
    let multi = json!({"document":f.package.join("multi.txt")});
    for (scope, control, resources, marker) in [
        ("candidate", "key-1", single.clone(), "a"),
        ("candidate@multi", "key-2", multi.clone(), "multi"),
    ] {
        f.host
            .set_action_resources(ID, scope, "device", control, resources);
        f.host
            .dispatch(
                ID,
                ACTION,
                "device",
                control,
                scope,
                &hardware_core::Input::Pulse,
                &json!({"marker":marker}),
            )
            .unwrap();
    }
    let report = f.wait(|v| v["events"]["keys"].as_array().is_some_and(|a| a.len() == 2));
    let events = report["events"]["keys"].as_array().unwrap();
    let single_event = events
        .iter()
        .find(|e| e["payload"]["isInMultiAction"] == false)
        .unwrap();
    let multi_event = events
        .iter()
        .find(|e| e["payload"]["isInMultiAction"] == true)
        .unwrap();
    assert_eq!(single_event["payload"]["resources"], single);
    assert_eq!(multi_event["payload"]["resources"], multi);
    assert!(single_event["payload"].get("coordinates").is_some());
    assert!(multi_event["payload"].get("coordinates").is_none());
}

#[test]
#[ignore = "Candidate source copy only; externally authored settings retain 7.1 resource payload shape"]
// Verify official SDK candidate changed settings resource payload
fn official_sdk_candidate_changed_settings_resource_payload() {
    let mut f = Fixture::new();
    let resources = json!({"document":f.package.join("authored.txt")});
    f.host
        .set_action_resources(ID, "candidate", "device", "key-1", resources.clone());
    f.synchronize("Original", &["changed", "b"]);
    let report = f.wait(|v| {
        v["events"]["settings"]
            .as_array()
            .is_some_and(|a| a.len() == 1)
    });
    let payload = &report["events"]["settings"][0]["eventPayload"];
    assert_eq!(payload["resources"], resources);
    assert_eq!(payload["controller"], "Keypad");
    assert_eq!(payload["isInMultiAction"], false);
    assert_eq!(payload["coordinates"], json!({"column":0,"row":0}));
}

#[test]
#[ignore = "Candidate source copy only; failed owner commits release getters with the prior value"]
// Verify official SDK candidate rejected writes preserve values
fn official_sdk_candidate_rejected_writes_preserve_values() {
    let mut f = Fixture::new();
    let context = f.context("a");
    f.bind();
    let sequence = f.send(
        "selfWrite",
        &context,
        json!({"settings":{"marker":"rejected"}}),
    );
    let request = f.pending();
    std::thread::sleep(Duration::from_millis(150));
    f.host
        .reject_saved_settings(&request, "Deliberate candidate test rejection");
    let result = f.result(sequence);
    assert_eq!(
        result["response"]["result"]["settings"],
        json!({"marker":"a"})
    );
    assert_eq!(result["events"]["settings"], json!([]));
    let sequence = f.send(
        "resourceRoundtrip",
        &context,
        json!({"resources":{"document":f.package.join("rejected.txt")}}),
    );
    let request = f.pending();
    std::thread::sleep(Duration::from_millis(150));
    f.host
        .reject_saved_settings(&request, "Deliberate candidate test rejection");
    let result = f.result(sequence);
    assert_eq!(result["response"]["result"]["resources"], json!({}));
    assert_eq!(result["events"]["resources"], json!([]));
}
