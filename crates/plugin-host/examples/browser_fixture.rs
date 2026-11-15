use std::{
    io::{self, Write},
    time::Duration,
};
use stream_dock_manager_plugin_host::PluginHost;
mod support;
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "stream-dock-manager-browser-fixture-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root)?;
    let archive = root.join("browser.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive)?);
    for (name, data) in [
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
    ] {
        zip.start_file(
            format!("app.streamdockmanager.plugins.fixture.browser.sdPlugin/{name}"),
            zip::write::SimpleFileOptions::default(),
        )?;
        zip.write_all(data)?;
    }
    zip.finish()?;
    let mut host = PluginHost::new(root.join("plugins"))?;
    let plugin = host.install(&archive)?;
    host.enable(&plugin.id, true)?;
    support::demand(&mut host, &plugin.id, "device-a");
    let view = host
        .browser_view(&plugin.id)?
        .ok_or("browser view missing")?;
    let inspector = host.settings_panel(
        &plugin.id,
        Some("app.streamdockmanager.plugins.fixture.browser.action"),
        &serde_json::json!({"title":"Initial"}),
    )?;
    println!(
        "{}",
        serde_json::json!({"plugin":view.url,"inspector":inspector.url})
    );
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    host.enable(&plugin.id, false)?;
    println!("disabled");
    io::stdout().flush()?;
    std::thread::sleep(Duration::from_millis(100));
    line.clear();
    io::stdin().read_line(&mut line)?;
    host.shutdown();
    drop(host);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
