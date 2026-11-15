//! Generic registration/lifecycle probe; action-specific acceptance is a separate host
//! Always owns a fresh disposable configuration. Never attaches HID or native input effects
use serde_json::json;
use std::time::{Duration, Instant};
use stream_dock_manager_plugin_host::{ActiveAction, PluginHost, RuntimeState};
mod support;

// Wait for the current operation
fn wait(host: &mut PluginHost, ready: impl Fn(&PluginHost) -> bool) -> Result<(), String> {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        host.poll();
        if ready(host) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("Protocol probe timed out: {:?}", host.list()))
}
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let archive = std::env::args()
        .nth(1)
        .ok_or("Pass one explicitly selected plugin ZIP")?;
    if std::env::args().len() != 2 {
        return Err(
            "The probe accepts one ZIP and always creates its own disposable configuration".into(),
        );
    }
    let root = tempfile::tempdir()?;
    let mut host = PluginHost::new(root.path().join("plugins"))?;
    let plugin = host.install(std::path::Path::new(&archive))?;
    assert!(!plugin.enabled, "installation must remain disabled");
    if host.browser_view(&plugin.id)?.is_some() {
        return Err("This executable probe requires an executable/script project".into());
    }
    support::runtime(&host, &plugin.id, false)?;
    host.enable(&plugin.id, true)?;
    support::demand(&mut host, &plugin.id, "protocol-fixture");
    wait(&mut host, |host| {
        host.list()
            .iter()
            .any(|item| item.id == plugin.id && item.runtime.state == RuntimeState::Running)
    })?;
    let startup_health = host
        .list()
        .into_iter()
        .find(|item| item.id == plugin.id)
        .ok_or("Registered plugin is missing")?
        .health
        .session;
    let action = plugin.actions.first().ok_or("The package has no action")?;
    let encoder = !action.control_types.iter().any(|kind| kind == "button");
    host.synchronize(vec![ActiveAction {
        plugin_id: plugin.id.clone(), action_id: action.id.clone(), device_id: "protocol-fixture".into(), control_id: "probe-control".into(), scope: "probe".into(), settings: json!({}), surface: None, column: 0, row: 0, encoder,
    }], vec![json!({"id":"protocol-fixture","name":"Disposable protocol fixture","type":if encoder {7}else{0}})]);
    let until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < until {
        host.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    host.synchronize(vec![], vec![]);
    host.enable(&plugin.id, false)?;
    wait(&mut host, |host| {
        host.list()
            .iter()
            .any(|item| item.id == plugin.id && item.runtime.state == RuntimeState::Disabled)
    })?;
    host.shutdown();
    println!(
        "{}",
        json!({"identity":plugin.id,"version":plugin.version,"authenticated_registration":true,"startup_health":startup_health,"owned_lifecycle":true,"configuration":"disposable","action_semantics":"not checked; select a dedicated scenario","native_qualification":false})
    );
    Ok(())
}
