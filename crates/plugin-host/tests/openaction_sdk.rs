//! Genuine pinned upstream SDK against the production host, opt-in and disposable
//! Contract: OpenActionAPI/docs at ffe2564bd42f54b19443cf1dac821adf6a85a848,
//! src/4_clientbound/. Synthetic device and OS events qualify wire behavior only,
//! not physical hardware, OS notifications or a graphical property inspector.
#[path = "../examples/support/mod.rs"]
mod support;
use hardware_core::Input;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{ActiveAction, FeedbackKind, HostEffect, PluginHost};
const ID: &str = "com.manager.fixture.openaction";
const ACTION: &str = "com.manager.fixture.openaction.probe";
struct Fixture {
    host: PluginHost,
    report: PathBuf,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.host.shutdown();
    }
}
impl Fixture {
    // Collect the events emitted by this operation
    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.report)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    // Wait for the current operation
    fn wait(&mut self, event: &str, count: usize) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.host.poll();
            let events: Vec<_> = self
                .events()
                .into_iter()
                .filter(|e| e["event"] == event)
                .collect();
            if events.len() >= count {
                return events[count - 1].clone();
            }
            assert!(
                Instant::now() < deadline,
                "Timeout for {event}: {:?}; host {:?}",
                self.events(),
                self.host.list()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // Synchronize for the current operation
    fn synchronize(&mut self, active: bool) {
        let actions = if active {
            vec![false, true]
                .into_iter()
                .map(|encoder| ActiveAction {
                    plugin_id: ID.into(),
                    action_id: ACTION.into(),
                    device_id: "test-device".into(),
                    control_id: if encoder { "dial" } else { "key" }.into(),
                    scope: "fixture".into(),
                    settings: json!(["opaque", encoder]),
                    surface: None,
                    row: 0,
                    column: if encoder { 1 } else { 0 },
                    encoder,
                })
                .collect()
        } else {
            vec![]
        };
        self.host.synchronize(actions,vec![json!({"id":"test-device","name":"Disposable test device","type":0,"size":{"rows":3,"columns":5}})]);
    }
    // Dispatch for the current operation
    fn dispatch(&mut self, encoder: bool, input: Input) {
        self.host
            .dispatch(
                ID,
                ACTION,
                "test-device",
                if encoder { "dial" } else { "key" },
                "fixture",
                &input,
                &json!(["opaque", encoder]),
            )
            .unwrap();
    }
}
#[test]
#[ignore = "Requires explicit built pinned OpenAction fixture and loopback sockets; no installed plugins/hardware"]
// Verify genuine openaction SDK lifecycle input settings and visuals
fn genuine_openaction_sdk_lifecycle_input_settings_and_visuals() {
    let executable = PathBuf::from(std::env::var_os("STREAM_DOCK_MANAGER_OPENACTION_FIXTURE").expect("Build tests/fixtures/plugins/openaction-sdk-executable and provide its absolute binary path"));
    assert!(executable.is_absolute() && executable.is_file());
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join(format!("{ID}.sdPlugin"));
    std::fs::create_dir(&package).unwrap();
    let binary = if cfg!(windows) { "probe.exe" } else { "probe" };
    std::fs::copy(executable, package.join(binary)).unwrap();
    std::fs::write(package.join("icon.svg"),r##"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="#123456"/></svg>"##).unwrap();
    std::fs::write(
        package.join("inspector.html"),
        "<!doctype html><title>Transport fixture</title>",
    )
    .unwrap();
    std::fs::write(package.join("manifest.json"),json!({
        "Name":"OpenAction SDK probe","Author":"Manager tests","Version":"0.1.0","Icon":"icon",
        "CodePath":binary,"OS":[{"Platform":"mac"},{"Platform":"windows"},{"Platform":"linux"}],
        "HasSettingsInterface":true,"PropertyInspectorPath":"inspector.html",
        "Actions":[{"Name":"Probe","UUID":ACTION,"Icon":"icon","Controllers":["Keypad","Encoder"],"States":[{"Title":"first"},{"Title":"second"}]}]
    }).to_string()).unwrap();
    let mut host = support::protocol_host(root.path()).unwrap();
    let prepared = PluginHost::prepare_install(&host.installation_root(), &package).unwrap();
    let info = host.replace_prepared(prepared).unwrap();
    assert_eq!(info.id, ID);
    host.enable(ID, true).unwrap();
    support::demand(&mut host, ID, "test-device");
    let report = host.installation_root().join(ID).join("probe.jsonl");
    let mut f = Fixture {
        host,
        report,
        _root: root,
    };
    f.wait("pluginReady", 1);
    let info = f.wait("registrationInfo", 1);
    assert!(info["payload"]["devices"].is_array());
    // Registration can enqueue the initial empty global settings before our query
    let globals = f.wait("didReceiveGlobalSettings", 2);
    assert_eq!(globals["payload"], json!(["opaque", 7]));
    f.synchronize(true);
    f.wait("willAppear", 2);
    let connected = f.wait("deviceDidConnect", 1);
    assert_eq!(connected["payload"]["device"], "test-device");
    assert_eq!(connected["payload"]["rows"], 3);
    assert_eq!(connected["payload"]["columns"], 5);
    f.wait("didReceiveSettings", 2);
    let effects = f.host.drain_effects();
    assert!(effects.iter().any(|e| matches!(e,HostEffect::Feedback(v) if v.kind==FeedbackKind::Title && v.message["payload"]["title"]=="SDK ready")),"Expected genuine SDK title (state:null): {effects:?}");
    assert!(
        effects
            .iter()
            .any(|e| matches!(e,HostEffect::Feedback(v) if v.kind==FeedbackKind::Image)),
        "Expected genuine SDK extensionless SVG image"
    );
    f.dispatch(false, Input::Button { pressed: true });
    assert_eq!(f.wait("keyDown", 1)["payload"]["state"], 0);
    f.dispatch(false, Input::Button { pressed: false });
    assert_eq!(f.wait("keyUp", 1)["payload"]["state"], 1);
    f.dispatch(true, Input::Rotate { delta: -2 });
    assert_eq!(f.wait("dialRotate", 1)["payload"]["ticks"], -2);
    f.dispatch(true, Input::Button { pressed: true });
    f.wait("dialDown", 1);
    f.dispatch(true, Input::Button { pressed: false });
    f.wait("dialUp", 1);
    f.dispatch(
        true,
        Input::Touch {
            phase: "hold".into(),
            x: 11,
            y: 12,
        },
    );
    let touch = f.wait("touchTap", 1);
    assert_eq!(touch["payload"], json!({"position":[11,12],"hold":true}));
    assert!(f.host.update_title_parameters(ID, "fixture", "test-device", "key", "Changed title", json!({"fontFamily":"Sans","fontSize":18,"fontStyle":"Bold","fontUnderline":false,"showTitle":true,"titleAlignment":"middle","titleColor":"#ffffff"})).unwrap());
    let title = f.wait("titleParametersDidChange", 1);
    assert_eq!(title["payload"]["title"], "Changed title");
    assert_eq!(title["payload"]["fontSize"], 18);
    assert_eq!(title["payload"]["showTitle"], true);
    assert_eq!(f.host.emit_system_wake(), 1);
    f.wait("systemDidWakeUp", 1);
    f.host.show_settings_interface(ID).unwrap();
    f.wait("showSettingsInterface", 1);
    // A bound property-inspector transport, without creating a browser window
    let address =
        json!({"profile_id":"test-profile","page_id":"main","control_id":"key","path":[]});
    f.host
        .set_action_address(ID, "fixture", "test-device", "key", address.clone());
    let panel = f
        .host
        .settings_panel(ID, Some(ACTION), &json!(["opaque", false]))
        .unwrap();
    f.host.bind_vendor_panel(&panel.token, address).unwrap();
    let endpoint = panel.url.split('/').nth(2).unwrap();
    let stream = std::net::TcpStream::connect(endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (mut pi, _) = tungstenite::client(format!("ws://{endpoint}"), stream).unwrap();
    pi.send(tungstenite::Message::text(
        json!({"event":"registerPropertyInspector","uuid":panel.token}).to_string(),
    ))
    .unwrap();
    pi.send(tungstenite::Message::text(json!({"event":"sendToPlugin","action":ACTION,"context":panel.token,"payload":{"marker":"PI roundtrip"}}).to_string())).unwrap();
    f.wait("propertyInspectorDidAppear", 1);
    f.wait("sendToPlugin", 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        f.host.poll();
        let message = pi.read().unwrap();
        if let Ok(text) = message.to_text() {
            let event: Value = serde_json::from_str(text).unwrap();
            if event["event"] == "sendToPropertyInspector" {
                assert_eq!(event["payload"]["echo"], json!({"marker":"PI roundtrip"}));
                break;
            }
        }
        assert!(Instant::now() < deadline, "PI response timeout");
    }
    drop(pi);
    f.wait("propertyInspectorDidDisappear", 1);
    f.synchronize(false);
    f.wait("willDisappear", 2);
    f.host.synchronize(vec![], vec![]);
    assert_eq!(
        f.wait("deviceDidDisconnect", 1)["payload"]["device"],
        "test-device"
    );
}
