//! Human-style native interaction with generated fixtures only. No injected consent
use super::vendor_inspectors::{self, probe};
use std::{path::Path, sync::Mutex, time::Duration};
use stream_dock_manager_plugin_host::PluginHost;
const BYTES: &str = "Disposable native picker fixture bytes\n";

// Prepare for the current operation
pub fn prepare(root: &Path, package: &Path) -> Result<(), String> {
    let files = root.join("generated-files");
    std::fs::create_dir(&files).map_err(|e| e.to_string())?;
    std::fs::write(files.join("chosen.txt"), BYTES).map_err(|e| e.to_string())?;
    std::fs::write(files.join("unselected.txt"), "Do not select this fixture")
        .map_err(|e| e.to_string())?;
    std::fs::write(package.join("pi.html"), include_str!("interactive.html"))
        .map_err(|e| e.to_string())?;
    std::fs::write(package.join("child.html"), "<!doctype html><html><body style='font:18px system-ui;padding:30px'><h1>Disposable child</h1><button onclick='window.close()' style='font:18px system-ui;padding:12px'>Close dummy child</button></body></html>")
        .map_err(|e| e.to_string())?;
    Ok(())
}
// Evaluate the interactive inspector probe script
async fn eval(
    app: &tauri::AppHandle,
    label: &str,
    script: &str,
) -> Result<serde_json::Value, String> {
    probe::evaluate(app, label.into(), script.into()).await
}
// Wait for the probe condition within its deadline
async fn until(app: &tauri::AppHandle, label: &str, condition: &str) -> Result<(), String> {
    for _ in 0..1200 {
        if eval(app, label, condition).await? == true {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!(
        "Interactive fixture condition timed out: {condition}"
    ))
}
// Require for the current operation
fn require(value: bool, message: &str) -> Result<(), String> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}
// Run for the current operation
pub async fn run(
    app: &tauri::AppHandle,
    host: &Mutex<PluginHost>,
    root: &Path,
) -> Result<(), String> {
    let files = root
        .join("generated-files")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let chosen = files.join("chosen.txt");
    let sibling = files.join("unselected.txt");
    probe::enable_interactive(app, files).await?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(8);
    host.lock().unwrap().set_settings_writer(sender);
    for generation in 0..3 {
        let (panel, lease) = {
            let mut host = host.lock().unwrap();
            let panel = host.settings_panel(
                "org.manager.disposable-probe",
                Some("org.manager.disposable-probe.action"),
                &serde_json::json!({}),
            )?;
            host.set_panel_resources(&panel.token, serde_json::json!({}), serde_json::json!({}))?;
            let lease = host.isolate_inspector("org.manager.disposable-probe", &panel)?;
            (panel, lease)
        };
        vendor_inspectors::open(app, lease.clone(), [500.0, 650.0]).await?;
        let windows = probe::windows(app).await?;
        require(
            windows.len() == 1 && windows[0]["automatic_popups"] == false,
            "Production automatic-popup policy must stay enabled",
        )?;
        let label = windows[0]["label"].as_str().unwrap().to_owned();
        until(app, &label, "window.socketReady===true").await?;
        if generation == 0 {
            eval(
                app,
                &label,
                "setStatus('Click Open dummy child, then Cancel the native popup dialog.');true",
            )
            .await?;
            println!("INTERACTIVE STEP 1: click Open dummy child, then native Cancel");
            until(app, &label, "window.phase==='popup-cancelled'").await?;
            require(
                eval(app, &label, "window.trustedPopup").await? == true,
                "Popup activation must be a trusted click",
            )?;
            require(
                probe::windows(app).await?.len() == 1,
                "Cancelled popup allocated a child",
            )?;
            println!("INTERACTIVE PASS: trusted popup click and native cancellation");
        } else if generation == 1 {
            eval(
                app,
                &label,
                "setStatus('Click Open dummy child, then Allow window.');true",
            )
            .await?;
            println!("INTERACTIVE STEP 2: click Open dummy child, then native Allow window");
            until(app, &label, "window.phase==='popup-allowed'").await?;
            require(
                eval(app, &label, "window.trustedPopup && child!==null").await? == true,
                "Trusted approved popup",
            )?;
            let windows = probe::windows(app).await?;
            require(
                windows.len() == 2 && windows.iter().all(|w| w["automatic_popups"] == false),
                "Approved child and unchanged automatic-popup policy",
            )?;
            let child = windows
                .iter()
                .find(|window| !window["parent"].is_null())
                .unwrap()["label"]
                .as_str()
                .unwrap()
                .to_owned();
            until(app, &child, "document.readyState==='complete'").await?;
            require(eval(app,&child,"opener!==null && opener.opener===null && typeof window.__TAURI_INTERNALS__==='undefined'").await?==true,"Approved child IPC/opener boundary")?;
            println!("INTERACTIVE STEP 3: click Close dummy child");
            for _ in 0..1200 {
                if probe::windows(app).await?.len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            require(probe::windows(app).await?.len() == 1, "Child did not close")?;
            eval(app,&label,"setStatus('Click Choose generated fixture, Cancel native picker, then Verify picker cancellation.');true").await?;
            println!("INTERACTIVE STEP 4: open native file picker, Cancel, then Verify picker cancellation");
            until(app, &label, "window.phase==='picker-cancelled'").await?;
            require(
                eval(
                    app,
                    &label,
                    "window.trustedFile && window.changes===0 && file.files.length===0",
                )
                .await?
                    == true,
                "Cancelled native picker must start from a trusted empty input",
            )?;
            require(
                receiver.try_recv().is_err(),
                "Cancelled picker admitted a resource",
            )?;
            eval(app,&label,"setStatus('Choose generated fixture again. Select only chosen.txt and click Open.');true").await?;
            println!("INTERACTIVE STEP 5: select only chosen.txt in the generated-files directory, click Open");
            until(app, &label, "window.phase==='file-selected'").await?;
            require(
                eval(app, &label, "window.trustedFile && window.changes===1").await? == true,
                "File selection must originate in trusted input click",
            )?;
            require(
                eval(app, &label, "window.selectedBytes").await? == BYTES,
                "Native File bytes",
            )?;
            require(
                eval(app, &label, "window.projected").await? == chosen.to_str().unwrap(),
                "Exact native path projection",
            )?;
            let mut request = None;
            for _ in 0..100 {
                if let Ok(value) = receiver.try_recv() {
                    request = Some(value);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            let request = request.ok_or("Native selection did not authorize resource request")?;
            let snapshot = request
                .resources
                .as_ref()
                .and_then(|value| value["file"].as_str())
                .ok_or("Missing selected snapshot")?;
            require(
                Path::new(snapshot) != chosen,
                "Selected resource must use a private snapshot",
            )?;
            std::fs::write(&chosen, "Changed disposable source after selection")
                .map_err(|e| e.to_string())?;
            require(
                std::fs::read_to_string(snapshot).map_err(|e| e.to_string())? == BYTES,
                "Native grant preserves selected bytes",
            )?;
            host.lock().unwrap().reject_saved_settings(
                &request,
                "Probe inspected request without persistent commit",
            );
            eval(
                app,
                &label,
                &format!(
                    "window.sendResource({});true",
                    serde_json::to_string(&sibling).unwrap()
                ),
            )
            .await?;
            until(app, &label, "window.socketClosed===true").await?;
            require(
                receiver.try_recv().is_err(),
                "Unselected sibling was admitted",
            )?;
            println!("INTERACTIVE PASS: native picker cancellation, trusted selection, File bytes, exact path, snapshot and sibling rejection");
        } else {
            eval(
                app,
                &label,
                &format!(
                    "window.sendResource({});true",
                    serde_json::to_string(&chosen).unwrap()
                ),
            )
            .await?;
            until(app, &label, "window.socketClosed===true").await?;
            require(
                receiver.try_recv().is_err(),
                "Prior-session selected file was admitted",
            )?;
            println!("INTERACTIVE PASS: selected-file grant does not survive session teardown");
        }
        lease.revoke();
        for _ in 0..100 {
            if probe::windows(app).await?.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        require(
            probe::windows(app).await?.is_empty(),
            "Revocation did not remove windows",
        )?;
        vendor_inspectors::close(app, &panel.token);
        host.lock().unwrap().close_inspector(&panel.token);
    }
    Ok(())
}
