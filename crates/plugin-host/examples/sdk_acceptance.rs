//! Common SDK conformance suite. Uses the real authenticated host, no physical devices
use hardware_core::Input;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{ActiveAction, PluginHost};
mod support;
// Settle
fn settle(host: &mut PluginHost) {
    let until = Instant::now() + Duration::from_millis(150);
    while Instant::now() < until {
        host.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
}
// Lifecycle cleanup and interpreter startup are asynchronous. A fixed delay
// can synchronize while the plugin is still stopping, discarding visibility.
fn wait_registered(host: &mut PluginHost, id: &str) {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        host.poll();
        if host.list().iter().any(|plugin| {
            plugin.id == id
                && plugin.runtime.state == stream_dock_manager_plugin_host::RuntimeState::Running
        }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "Plugin did not reach registered Running state: {:?}",
        host.list()
    );
}
// Expect title
fn expect_title(host: &mut PluginHost, control: &str, title: &str) {
    let (prefix, count) = title.rsplit_once(' ').expect("Counter title");
    let count: u64 = count.parse().unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        host.poll();
        if let Some(visual) = host.surface("sdk-device", control, "main") {
            if visual["title"] == title
                && visual["feedback"]["prefix"] == prefix
                && visual["feedback"]["count"] == count
                && visual["state"] == count % 2
            {
                assert_eq!(visual["layout"]["id"], "sdk-counter");
                assert_eq!(visual["states"][0]["label"], "Even");
                assert_eq!(visual["states"][1]["label"], "Odd");
                assert!(visual["feedback"]["icon"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("data:image/svg+xml;base64,")));
                let elements = hardware_core::plugin_display::elements(&visual);
                assert_eq!(elements.len(), 3);
                let mut text = elements
                    .iter()
                    .find(|e| e.style.key == "count")
                    .unwrap()
                    .style
                    .clone();
                text.size = 28.;
                text.font = "mono".into();
                text.rect = [10., 60., 80., 38.];
                let appearance = hardware_core::plugin_display::PluginAppearance {
                    vendor_profile: None,
                    plugin_id: visual["plugin_id"].as_str().unwrap().into(),
                    action_id: visual["action_id"].as_str().unwrap().into(),
                    elements: vec![text],
                };
                let styled =
                    hardware_core::plugin_display::styled_visual(&visual, &appearance).unwrap();
                assert_eq!(styled["layout"]["items"][2]["value"], count);
                assert!(!hardware_core::render::visual(&styled, 112, 112, 0)
                    .unwrap()
                    .is_empty());
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("Plugin status: {:?}", host.list());
    eprintln!(
        "Last surface: {:?}",
        host.surface("sdk-device", control, "main")
    );
    for capture in host.logs.list() {
        eprintln!(
            "Captured diagnostics: {:?}",
            host.logs.page(&capture.id, None, false)
        );
    }
    panic!("{control} never published complete live display {title}");
}

// Run the action acceptance probes
fn actions(id: &str, action: &str) -> Vec<ActiveAction> {
    ["key-a", "key-b"]
        .into_iter()
        .enumerate()
        .map(|(i, key)| ActiveAction {
            plugin_id: id.into(),
            action_id: action.into(),
            device_id: "sdk-device".into(),
            control_id: key.into(),
            scope: "main".into(),
            settings: json!({"prefix":"Count","count":0}),
            surface: None,
            column: i,
            row: 0,
            encoder: false,
        })
        .collect()
}
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let archive = std::env::args().nth(1).ok_or("Pass an example ZIP")?;
    if std::env::args().any(|argument| argument == "--storage") {
        return storage_acceptance(&archive);
    }
    let root = tempfile::tempdir()?;
    let mut host = PluginHost::new(root.path().join("plugins"))?;
    let plugin = host.install(std::path::Path::new(&archive))?;
    assert!(!plugin.enabled, "install must stay disabled");
    let id = &plugin.id;
    let action = &plugin.actions[0].id;
    let capture = host.logs.start(id, &plugin.name)?;
    support::runtime(&host, id, false)?;
    host.enable(id, true)?;
    support::demand(&mut host, id, "sdk-device");
    wait_registered(&mut host, id);
    host.synchronize(
        actions(id, action),
        vec![json!({"id":"sdk-device","name":"SDK simulator","type":7})],
    );
    expect_title(&mut host, "key-a", "Count 0");
    settle(&mut host);
    host.drain_effects();
    host.dispatch(
        id,
        action,
        "sdk-device",
        "key-a",
        "main",
        &Input::Button { pressed: true },
        &json!({}),
    )?;
    settle(&mut host);
    assert_eq!(
        host.effective_settings(id, "main", "sdk-device", "key-a", &json!({}))["count"],
        0
    );
    let log_path = root.path().join("counter-capture.jsonl");
    host.logs.export(&capture.id, &log_path)?;
    let before = std::fs::read_to_string(&log_path)?;
    assert!(
        !before
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|value| value["source"] == "rpc"
                && value["message"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Action "))),
        "counter reported completion before its key-release mutation"
    );
    let receipt = host.dispatch_tracked(
        id,
        action,
        "sdk-device",
        "key-a",
        "main",
        &Input::Button { pressed: false },
        &json!({}),
    )?;
    receipt.wait(Duration::from_secs(5), || Ok(()))?;
    expect_title(&mut host, "key-a", "Count 1");
    assert_eq!(
        host.effective_settings(id, "main", "sdk-device", "key-a", &json!({}))["count"],
        1
    );
    assert_eq!(
        host.effective_settings(id, "main", "sdk-device", "key-b", &json!({}))["count"],
        0
    );
    assert_eq!(
        host.surface("sdk-device", "key-b", "main").unwrap()["feedback"]["count"],
        0
    );
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        host.logs.export(&capture.id, &log_path)?;
        let text = std::fs::read_to_string(&log_path)?;
        let count = text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value["source"] == "rpc"
                    && value["stream"] == "stdout"
                    && value["message"].as_str().is_some_and(|text| {
                        text.starts_with("Action ") && text.contains(" completed in ")
                    })
            })
            .count();
        if count > 0 {
            assert_eq!(count, 1, "counter logged duplicate completion");
            break;
        }
        if Instant::now() >= until {
            return Err("counter completion logging missing".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Open a real inspector grant and read its bootstrap token, as a browser would
    let original = host.effective_settings(id, "main", "sdk-device", "key-a", &json!({}));
    let view = host.settings_panel(id, Some(action), &original)?;
    let stripped = view.url.strip_prefix("http://").ok_or("Inspector URL")?;
    let (address, path) = stripped.split_once('/').ok_or("Inspector path")?;
    let mut http = TcpStream::connect(address)?;
    write!(
        http,
        "GET /{path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut page = String::new();
    http.read_to_string(&mut page)?;
    let token = path.split('/').next().ok_or("Inspector token")?;
    // The host uses /<token>/... grants; registration is scoped to that token
    let token = if token == "files" {
        path.split('/').nth(1).ok_or("Inspector token")?
    } else {
        token
    };
    let (mut inspector, _) = tungstenite::connect(format!("ws://{address}"))?;
    inspector.send(tungstenite::Message::Text(
        json!({"event":"registerPropertyInspector","uuid":token})
            .to_string()
            .into(),
    ))?;
    let context = format!("settings-draft:{}", view.token);
    for (settings, title) in [
        (json!({"prefix":"SDK", "count":1}), "SDK 1"),
        (json!({"prefix":"SDK", "count":0}), "SDK 0"),
    ] {
        let before = host.effective_settings(id, "main", "sdk-device", "key-a", &original);
        inspector.send(tungstenite::Message::Text(
            json!({"event":"setSettings","context":context,"payload":settings})
                .to_string()
                .into(),
        ))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while host.panel_settings(&view.token)? != settings {
            host.poll();
            if Instant::now() >= deadline {
                return Err("Inspector draft settings were not saved".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            host.effective_settings(id, "main", "sdk-device", "key-a", &original),
            before,
            "Inspector edits must not mutate the live binding before Apply"
        );
        // This host-only fixture applies the draft through the same active-action
        // synchronization used after the runtime commits an edited binding.
        let mut visible = actions(id, action);
        visible
            .iter_mut()
            .find(|binding| binding.control_id == "key-a")
            .unwrap()
            .settings = settings;
        host.synchronize(visible, vec![]);
        expect_title(&mut host, "key-a", title);
    }
    host.close_inspector(&view.token);
    // Runtime owns profile persistence. Feed the updated saved values back to
    // this host-only fixture, rather than restoring its original Count/0 draft.
    let saved: Vec<_> = actions(id, action)
        .into_iter()
        .map(|binding| {
            host.effective_settings(
                id,
                "main",
                "sdk-device",
                &binding.control_id,
                &binding.settings,
            )
        })
        .collect();
    let saved_actions = || {
        let mut current = actions(id, action);
        for (binding, settings) in current.iter_mut().zip(&saved) {
            binding.settings = settings.clone();
        }
        current
    };
    host.synchronize(vec![], vec![]);
    settle(&mut host);
    assert!(
        host.list()[0].enabled,
        "offscreen plugin retains its profile demand"
    );
    host.synchronize(saved_actions(), vec![]);
    expect_title(&mut host, "key-a", "SDK 0");
    host.enable(id, false)?;
    assert!(!host.list()[0].enabled);
    support::runtime(&host, id, false)?;
    host.enable(id, true)?;
    support::demand(&mut host, id, "sdk-device");
    wait_registered(&mut host, id);
    host.synchronize(saved_actions(), vec![]);
    expect_title(&mut host, "key-a", "SDK 0");
    host.logs.stop(&capture.id)?;
    host.shutdown();
    println!(
        "{} acceptance passed on {}/{}",
        id,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Ok(())
}

// Construct the action used to probe host storage
fn storage_action(id: &str, action: &str, phase: &str) -> ActiveAction {
    ActiveAction {
        plugin_id: id.into(),
        action_id: action.into(),
        device_id: "sdk-device".into(),
        control_id: "key-storage".into(),
        scope: "storage".into(),
        settings: json!({"storage_phase":phase,"sdk_unknown":{"keep":true},"false":false,"zero":0,"null":null,"empty":[],"object":{}}),
        surface: None,
        column: 0,
        row: 0,
        encoder: false,
    }
}
// Run one phase of the host storage acceptance scenario
fn storage_phase(
    host: &mut PluginHost,
    id: &str,
    action: &str,
    phase: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    host.synchronize(
        vec![storage_action(id, action, phase)],
        vec![json!({"id":"sdk-device","name":"SDK storage simulator","type":7})],
    );
    let receipt = host.dispatch_tracked(
        id,
        action,
        "sdk-device",
        "key-storage",
        "storage",
        &Input::Button { pressed: false },
        &json!({}),
    )?;
    receipt.wait(Duration::from_secs(20), || Ok(()))?;
    let expected = format!("storage {phase} passed");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        host.poll();
        if host
            .surface("sdk-device", "key-storage", "storage")
            .is_some_and(|value| value["title"] == expected)
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for capture in host.logs.list() {
        eprintln!(
            "Storage probe diagnostics: {:?}",
            host.logs.page(&capture.id, None, false)
        );
    }
    Err(format!("{id} did not publish {expected}; status {:?}", host.list()).into())
}
// Stop storage host
fn stop_storage_host(host: &mut PluginHost) -> Result<(), Box<dyn std::error::Error>> {
    host.shutdown();
    let deadline = Instant::now() + Duration::from_secs(5);
    for stopped in host.shutdown_waiters() {
        while !stopped.load(std::sync::atomic::Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err("SDK probe cleanup timed out".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}
// Construct the private value used by the storage probe
fn private_probe_value(host: &PluginHost, id: &str) -> Result<Option<serde_json::Value>, String> {
    use stream_dock_manager_storage::rusqlite::OptionalExtension;
    let owner = id.to_owned();
    host.storage().run(move|connection| {
        let value:Option<String>=connection.query_row("SELECT value FROM plugin_storage_records WHERE plugin_id=?1 AND namespace='sdk-storage-v1' AND key='value'",[owner],|row|row.get(0)).optional().map_err(|e|e.to_string())?;
        value.map(|value|serde_json::from_str(&value).map_err(|e|e.to_string())).transpose()
    })
}
// Run the host storage acceptance scenario
fn storage_acceptance(archive: &str) -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let plugins = root.path().join("plugins");
    let mut host = PluginHost::new(plugins.clone())?;
    let plugin = host.install(std::path::Path::new(archive))?;
    let id = plugin.id.clone();
    let action = plugin.actions[0].id.clone();
    host.logs.start(&id, &plugin.name)?;
    let before = host.configuration(&id)?;
    let mut after = before.clone();
    after.settings = serde_json::from_value(
        json!({"sdk_unknown":{"keep":true},"false":false,"zero":0,"null":null,"empty":[],"object":{}}),
    )?;
    host.configuration_transaction_sql(&id, &before, &after, 0, |_| Ok(()))?;
    support::runtime(&host, &id, false)?;
    host.enable(&id, true)?;
    support::demand(&mut host, &id, "sdk-device");
    wait_registered(&mut host, &id);
    storage_phase(&mut host, &id, &action, "write")?;
    let expected = json!({"unicode":"é😀","unknown":{"keep":true},"false":false,"zero":0,"null":null,"empty":[],"object":{}});
    assert_eq!(private_probe_value(&host, &id)?, Some(expected.clone()));
    assert_eq!(
        host.configuration(&id)?.settings["sdk_unknown"],
        json!({"keep":true})
    );
    assert_eq!(
        host.effective_settings(&id, "storage", "sdk-device", "key-storage", &json!({}))
            ["sdk_unknown"],
        json!({"keep":true})
    );
    stop_storage_host(&mut host)?;
    drop(host);
    // Reopening reacquires the configuration lock and uses a fresh authenticated process token
    let mut host = PluginHost::new(plugins)?;
    host.logs.start(&id, &plugin.name)?;
    support::demand(&mut host, &id, "sdk-device");
    wait_registered(&mut host, &id);
    storage_phase(&mut host, &id, &action, "read")?;
    assert_eq!(private_probe_value(&host, &id)?, Some(expected));
    assert_eq!(
        host.configuration(&id)?.settings["sdk_unknown"],
        json!({"keep":true})
    );
    storage_phase(&mut host, &id, &action, "delete")?;
    assert!(private_probe_value(&host, &id)?.is_none());
    let owner = id.clone();
    let revision:String=host.storage().run(move|connection|connection.query_row("SELECT revision FROM plugin_storage_namespaces WHERE plugin_id=?1 AND namespace='sdk-storage-v1'",[owner],|row|row.get(0)).map_err(|e|e.to_string()))?;
    assert!(
        revision.parse::<u64>()? > 1,
        "deleting keys must retain namespace revision"
    );
    stop_storage_host(&mut host)?;
    println!(
        "{id} storage acceptance passed on {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Ok(())
}
