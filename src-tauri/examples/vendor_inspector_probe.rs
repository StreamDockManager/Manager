//! Opt-in, disposable macOS WebKit integration probe. Never launches Manager,
//! Runtime, hardware discovery, an installed plugin, or a user configuration
#[cfg(target_os = "macos")]
#[path = "vendor_inspector_probe/interactive.rs"]
mod interactive;
#[cfg(target_os = "macos")]
#[path = "../src/vendor_file_picker.rs"]
mod vendor_file_picker;
#[cfg(target_os = "macos")]
#[path = "../src/vendor_inspectors.rs"]
mod vendor_inspectors;

#[cfg(target_os = "macos")]
// Run this executable entry point
fn main() {
    use std::sync::{Arc, Mutex};
    let root = tempfile::tempdir().expect("Disposable root");
    println!("DISPOSABLE_FIXTURE_ROOT {}", root.path().display());
    let interactive = std::env::args().any(|argument| argument == "--interactive");
    let package = root.path().join("plugins/probe");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("manifest.json"),serde_json::json!({
        "UUID":"org.manager.disposable-probe","Name":"Disposable inspector probe","Version":"0.1.0","SDKVersion":1,
        "CodePath":"runtime.html","StreamDockManager":{"ManifestDialect":"vsd"},
        "Actions":[{"UUID":"org.manager.disposable-probe.action","Name":"Probe","Controllers":["Keypad"],"PropertyInspectorPath":"pi.html"}]
    }).to_string()).unwrap();
    for file in ["runtime.html", "pi.html", "child.html"] {
        std::fs::write(
            package.join(file),
            "<!doctype html><html><body><h1>Disposable native inspector probe</h1><script>addEventListener('message',e=>{if(e.origin===location.origin&&e.data==='fixture-init')window.receivedFromOpener=e.source===opener;});</script></body></html>",
        )
        .unwrap();
    }
    if interactive {
        interactive::prepare(root.path(), &package).expect("Disposable interactive fixtures");
    }
    let fixture_root = root.path().to_path_buf();
    let host = Arc::new(Mutex::new(
        stream_dock_manager_plugin_host::PluginHost::new(root.path().join("plugins"))
            .expect("Disposable host"),
    ));
    host.lock().unwrap().runtimes.automatic(false).unwrap();
    host.lock().unwrap().set_registration_environment(
        stream_dock_manager_plugin_host::RegistrationEnvironment::new(Some("en"), Some(1.0))
            .unwrap(),
    );
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let outcome = failed.clone();
    let app=tauri::Builder::default().setup(move|app|{
        // A native window only, not a privileged/default-storage webview
        tauri::WindowBuilder::new(app,"main").title("Disposable probe owner").visible(false).build()?;
        let handle=app.handle().clone();let host=host.clone();let outcome=outcome.clone();
        tauri::async_runtime::spawn(async move {
            let result=if interactive {
                tokio::time::timeout(std::time::Duration::from_secs(300),interactive::run(&handle,&host,&fixture_root)).await
            } else {
                tokio::time::timeout(std::time::Duration::from_secs(30),run(&handle,&host)).await
            }.unwrap_or_else(|_|Err("Probe deadline exceeded".into()));
            match result {Ok(())=>{if interactive {println!("NATIVE_INSPECTOR_INTERACTIVE PASS: actual trusted popup activation, native Cancel/Allow, child closure, native picker Cancel/selection with generated files only, path and byte projection, immutable snapshot, unselected sibling rejection, grant revocation and clean teardown. No injected consent, user files, OS permission changes or vendor reference host.");}else{println!("NATIVE_INSPECTOR_PROBE PASS: actual WK root/children, no IPC/editor opener, nonpersistent shared-descendant store, cross-session cookie isolation, navigation rejection, bounded geometry/count/depth, subtree/session teardown. Consent and activation injected for dummy fixture only; no native permission or file-selection qualification.");}outcome.store(false,std::sync::atomic::Ordering::Release);},Err(error)=>eprintln!("NATIVE_INSPECTOR_PROBE FAIL: {error}")}
            host.lock().unwrap().shutdown();
            handle.exit(if outcome.load(std::sync::atomic::Ordering::Acquire){1}else{0});
        });Ok(())
    }).build(tauri::test::mock_context(tauri::test::noop_assets())).expect("Probe event loop");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            vendor_inspectors::close_all(app);
        }
    });
    drop(root);
    if failed.load(std::sync::atomic::Ordering::Acquire) {
        std::process::exit(1);
    }
}
#[cfg(target_os = "macos")]
// Run for the current operation
async fn run(
    app: &tauri::AppHandle,
    host: &std::sync::Mutex<stream_dock_manager_plugin_host::PluginHost>,
) -> Result<(), String> {
    use vendor_inspectors::probe;
    // Count the recorded inspector probe events
    async fn count(
        app: &tauri::AppHandle,
        wanted: usize,
    ) -> Result<Vec<serde_json::Value>, String> {
        for _ in 0..100 {
            let windows = probe::windows(app).await?;
            if windows.len() == wanted {
                return Ok(windows);
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        Err(format!("Window count did not reach {wanted}"))
    }
    // Evaluate a probe script in the inspector webview
    async fn eval(
        app: &tauri::AppHandle,
        label: &str,
        script: &str,
    ) -> Result<serde_json::Value, String> {
        probe::evaluate(app, label.into(), script.into()).await
    }
    // Check whether the lifecycle is ready for the next transition
    async fn ready(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
        for _ in 0..100 {
            if eval(
                app,
                label,
                "location.protocol === 'http:' && document.readyState === 'complete'",
            )
            .await?
                == true
            {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        Err("Page did not load".into())
    }
    // Require for the current operation
    fn require(value: bool, message: &str) -> Result<(), String> {
        if value {
            Ok(())
        } else {
            Err(message.into())
        }
    }
    probe::enable(app).await?;
    for generation in 0..2 {
        let (panel, lease) = {
            let mut host = host.lock().unwrap();
            let panel = host
                .settings_panel(
                    "org.manager.disposable-probe",
                    Some("org.manager.disposable-probe.action"),
                    &serde_json::json!({}),
                )
                .map_err(|e| format!("Panel: {e}"))?;
            let lease = host.isolate_inspector("org.manager.disposable-probe", &panel)?;
            (panel, lease)
        };
        vendor_inspectors::open(app, lease.clone(), [500.0, 650.0]).await?;
        let windows = count(app, 1).await?;
        let root = windows[0]["label"].as_str().unwrap().to_owned();
        ready(app, &root).await?;
        require(
            windows[0]["persistent"] == false,
            "Root data store must be nonpersistent",
        )?;
        require(eval(app,&root,"typeof window.__TAURI_INTERNALS__ === 'undefined' && typeof window.ipc === 'undefined' && window.opener === null").await?==true,"Root IPC/opener boundary")?;
        require(
            eval(app, &root, "!document.cookie.includes('probe=')").await? == true,
            "Cross-session cookie isolation",
        )?;
        eval(
            app,
            &root,
            "document.cookie='probe=1; Path=/'; localStorage.setItem('probe','owned'); true",
        )
        .await?;
        require(
            eval(app, &root, "document.cookie.includes('probe=1')").await? == true,
            "Fixture cookie write readback",
        )?;
        if generation == 0 {
            eval(app, &root, "location.href='tauri://localhost/'; true").await?;
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            require(
                eval(app, &root, "location.protocol === 'http:'").await? == true,
                "Blocked app navigation",
            )?;
            require(eval(app,&root,"window.child=window.open('child.html','probe-child','width=999999,height=1'); child!==null").await?==true,"Child creation with injected confirmation")?;
            let windows = count(app, 2).await?;
            let child = windows.iter().find(|v| !v["parent"].is_null()).unwrap();
            let child_label = child["label"].as_str().unwrap().to_owned();
            ready(app, &child_label).await?;
            require(
                child["width"] == 1200.0 && child["height"] == 240.0,
                "Bounded explicit dimensions",
            )?;
            require(
                windows[0]["store"] == windows[1]["store"],
                "Descendant data store mismatch",
            )?;
            require(eval(app,&child_label,"typeof window.__TAURI_INTERNALS__==='undefined' && typeof window.ipc==='undefined' && opener!==null && opener.opener===null && localStorage.getItem('probe')==='owned'").await?==true,"Child IPC/opener/storage boundary")?;
            require(
                eval(app, &root, "window.open('','probe-child')===window.child").await? == true,
                "Named popup reuse",
            )?;
            count(app, 2).await?;
            ready(app, &child_label).await?;
            eval(
                app,
                &root,
                "window.child.postMessage('fixture-init',location.origin);true",
            )
            .await?;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            require(
                eval(app, &child_label, "window.receivedFromOpener===true").await? == true,
                "Named popup opener postMessage",
            )?;

            require(
                eval(
                    app,
                    &child_label,
                    "window.open('https://example.com')===null",
                )
                .await?
                    == true,
                "External popup rejection",
            )?;
            eval(
                app,
                &child_label,
                "window.grandchild=window.open('child.html'); grandchild!==null",
            )
            .await?;
            let windows = count(app, 3).await?;
            let grandchild = windows.iter().find(|v| v["parent"] == child_label).unwrap();
            let grand = grandchild["label"].as_str().unwrap().to_owned();
            ready(app, &grand).await?;
            require(
                grandchild["width"] == 500.0 && grandchild["height"] == 650.0,
                "Published popup default dimensions",
            )?;
            require(
                eval(app, &grand, "window.open('child.html')===null").await? == true,
                "Depth bound",
            )?;
            eval(
                app,
                &root,
                "window.sibling=window.open('child.html'); sibling!==null",
            )
            .await?;
            count(app, 4).await?;
            require(
                eval(app, &root, "window.open('child.html')===null").await? == true,
                "Active-window bound",
            )?;
            eval(app, &child_label, "window.close();true").await?;
            count(app, 2).await?;
        }
        lease.revoke();
        count(app, 0).await?;
        vendor_inspectors::close(app, &panel.token);
        host.lock().unwrap().close_inspector(&panel.token);
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
// Run this executable entry point
fn main() {
    eprintln!("This disposable probe requires native macOS WebKit");
    std::process::exit(2);
}
