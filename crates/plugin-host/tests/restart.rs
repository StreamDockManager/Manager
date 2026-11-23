#[path = "../examples/support/mod.rs"]
mod support;
use serde_json::json;
use stream_dock_manager_plugin_host::PluginHost;

// Construct the host test fixture
fn host(root: &std::path::Path) -> PluginHost {
    let host = PluginHost::new(root.into()).unwrap();
    support::set_test_display(&host).unwrap();
    host
}

#[test]
#[ignore = "Requires local loopback sockets; launches no executables or webviews"]
// Verify enabled plugins restart with fresh tokens and disabled plugins stay disabled
fn enabled_plugins_restart_with_fresh_tokens_and_disabled_plugins_stay_disabled() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(
        path.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(
        path.join("icon.svg"),
        include_bytes!("../../../tests/fixtures/plugins/browser/icon.svg"),
    )
    .unwrap();
    std::fs::write(path.join("manifest.json"), serde_json::to_vec(&json!({
        "UUID": "fixture.restart", "Name": "Restart fixture", "Version": "1.0.0", "CodePath": "runtime.html", "Author":"Fixture", "Icon":"icon.svg", "OS":[{"Platform":"mac"},{"Platform":"linux"},{"Platform":"windows"}],
        "Actions": [{ "UUID": "fixture.restart.action", "Name": "Fixture", "Icon":"icon.svg", "States":[{"Image":"icon.svg"}] }]
    })).unwrap()).unwrap();
    // A fresh database never opts a discovered package into execution
    let mut host = host(root.path());
    assert!(
        !host
            .list()
            .into_iter()
            .find(|plugin| plugin.id == "fixture.restart")
            .expect("discover restart fixture")
            .enabled
    );
    host.set_sleep_when_hidden("fixture.restart", false)
        .unwrap();
    host.enable("fixture.restart", true).unwrap();
    support::demand(&mut host, "fixture.restart", "test");
    let first = host.browser_view("fixture.restart").unwrap().unwrap().url;
    drop(host);
    let mut host = self::host(root.path());
    assert!(host.list()[0].enabled);
    assert!(host.browser_view("fixture.restart").unwrap().is_none());
    support::demand(&mut host, "fixture.restart", "test");
    assert_ne!(
        host.browser_view("fixture.restart").unwrap().unwrap().url,
        first
    );
    let running_url = host.browser_view("fixture.restart").unwrap().unwrap().url;
    // A failed preference write reports failure and preserves the prior running state
    host.storage()
        .run(|conn| {
            conn.execute_batch("PRAGMA query_only=ON")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(host.enable("fixture.restart", false).is_err());
    assert!(host.list()[0].enabled);
    assert_eq!(
        host.browser_view("fixture.restart").unwrap().unwrap().url,
        running_url
    );
    host.storage()
        .run(|conn| {
            conn.execute_batch("PRAGMA query_only=OFF")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    host.startup_failure("fixture.restart", "Fixture view failure");
    assert!(host.list()[0].enabled);
    drop(host);
    let mut host = self::host(root.path());
    assert!(host.list()[0].enabled);
    host.enable("fixture.restart", false).unwrap();
    drop(host);
    let host = self::host(root.path());
    assert!(
        !host
            .list()
            .into_iter()
            .find(|plugin| plugin.id == "fixture.restart")
            .expect("discover restart fixture")
            .enabled
    );
    assert!(host.browser_view("fixture.restart").unwrap().is_none());
}
