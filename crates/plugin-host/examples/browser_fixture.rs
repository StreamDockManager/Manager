use std::{
    io::{self, Write},
    time::Duration,
};
use stream_dock_manager_plugin_host::PluginHost;
mod support;
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::Builder::new()
        .prefix("stream-dock-manager-browser-fixture-")
        .tempdir()?;
    let root = directory.path();
    let shared = std::env::args().any(|argument| argument == "--shared-html");
    let archive = root.join("browser.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive)?);
    let files = [
        (
            "manifest.json",
            include_bytes!("../../../tests/fixtures/plugins/browser/manifest.json").as_slice(),
        ),
        (
            "plugin.html",
            include_bytes!("../../../tests/fixtures/plugins/browser/plugin.html").as_slice(),
        ),
        (
            "plugin.js",
            include_bytes!("../../../tests/fixtures/plugins/browser/plugin.js").as_slice(),
        ),
        (
            "inspector.html",
            include_bytes!("../../../tests/fixtures/plugins/browser/inspector.html").as_slice(),
        ),
        (
            "inspector.js",
            include_bytes!("../../../tests/fixtures/plugins/browser/inspector.js").as_slice(),
        ),
        (
            "icon.svg",
            include_bytes!("../../../tests/fixtures/plugins/browser/icon.svg").as_slice(),
        ),
    ];
    let shared_files: &[(&str, &[u8])] = &[
        (
            "manifest.json",
            include_bytes!("../../../tests/fixtures/plugins/browser/shared-manifest.json"),
        ),
        (
            "ui/index.HTML",
            include_bytes!("../../../tests/fixtures/plugins/browser/shared.html"),
        ),
        (
            "ui/shared.js",
            include_bytes!("../../../tests/fixtures/plugins/browser/shared.js"),
        ),
        (
            "ui/interval.js",
            include_bytes!("../../../tests/fixtures/plugins/browser/interval.js"),
        ),
        (
            "ui/child.html",
            b"<!doctype html><title>Auxiliary page</title>",
        ),
        (
            "ui/assets/kn.mp3",
            b"synthetic content for MIME validation only",
        ),
        (
            "ui/fonts/GEFORCE-BOLD.TTF",
            b"synthetic content for MIME validation only",
        ),
        (
            "icon.svg",
            include_bytes!("../../../tests/fixtures/plugins/browser/icon.svg"),
        ),
    ];
    for (name, data) in if shared { shared_files } else { &files } {
        zip.start_file(
            format!("app.streamdockmanager.plugins.fixture.browser.sdPlugin/{name}"),
            zip::write::SimpleFileOptions::default(),
        )?;
        zip.write_all(data)?;
    }
    zip.finish()?;
    let mut host = PluginHost::new(root.join("plugins"))?;
    support::set_test_display(&host)?;
    let plugin = host.install(&archive)?;
    host.enable(&plugin.id, true)?;
    let visible = stream_dock_manager_plugin_host::ActiveAction {
        plugin_id: plugin.id.clone(),
        action_id: plugin.actions[0].id.clone(),
        device_id: "device-a".into(),
        control_id: "key-1".into(),
        scope: "test-profile:main".into(),
        settings: if shared {
            serde_json::json!({"title":"Initial","amplify":false})
        } else {
            serde_json::json!({})
        },
        surface: None,
        column: 0,
        row: 0,
        encoder: false,
    };
    host.reconcile_visible_demand(
        std::collections::HashMap::from([(
            plugin.id.clone(),
            vec![stream_dock_manager_plugin_host::PluginDemand {
                device_id: "device-a".into(),
                profile_id: "test-profile".into(),
            }],
        )]),
        std::slice::from_ref(&visible),
    );
    host.poll();
    if shared {
        host.synchronize(vec![visible], vec![]);
    }
    let view = host
        .browser_view(&plugin.id)?
        .ok_or("browser view missing")?;
    let inspector = host.settings_panel(
        &plugin.id,
        Some("app.streamdockmanager.plugins.fixture.browser.action"),
        &if shared {
            serde_json::json!({"title":"Initial","amplify":false})
        } else {
            serde_json::json!({"title":"Initial"})
        },
    )?;
    let lease = shared
        .then(|| host.isolate_inspector(&plugin.id, &inspector))
        .transpose()?;
    let inspector_url = lease
        .as_ref()
        .map_or(inspector.url.as_str(), |lease| lease.url());
    println!(
        "{}",
        serde_json::json!({"plugin": view.url, "inspector": inspector_url})
    );
    io::stdout().flush()?;
    let mut line = String::new();
    if shared {
        io::stdin().read_line(&mut line)?;
        if line.trim() != "read-title" {
            return Err("Expected a fixture title read".into());
        }
        let mut title = None;
        let mut has_image = false;
        for _ in 0..100 {
            let (snapshot, ()) = host.render_surfaces(
                "device-a",
                &[("key-1".into(), "test-profile:main".into())],
                &[],
                || (),
            );
            title = snapshot
                .values
                .get("key-1")
                .and_then(|value| value["title"].as_str())
                .map(str::to_owned);
            has_image = snapshot
                .values
                .get("key-1")
                .is_some_and(|value| value["image"].is_string());
            if title.is_some() && has_image {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("{}", serde_json::json!({"title":title,"image":has_image}));
        io::stdout().flush()?;
        line.clear();
    }
    io::stdin().read_line(&mut line)?;
    host.enable(&plugin.id, false)?;
    println!("disabled");
    io::stdout().flush()?;
    std::thread::sleep(Duration::from_millis(100));
    line.clear();
    io::stdin().read_line(&mut line)?;
    host.shutdown();
    drop(host);
    Ok(())
}
