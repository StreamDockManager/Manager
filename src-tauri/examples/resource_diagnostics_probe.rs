//! Run against the normal Manager browser fixture served by Vite preview on port 1421.
//! Uses a disposable WKWebView and Manager's actual native diagnostic validator and file logger.
#[cfg(all(target_os = "macos", not(test)))]
#[allow(dead_code)]
#[path = "../src/manager_log.rs"]
mod manager_log;

#[cfg(all(target_os = "macos", not(test)))]
fn main() {
    use std::{
        cell::RefCell,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::Duration,
    };
    thread_local! {
        static VIEW: RefCell<Option<wry::WebView>> = const { RefCell::new(None) };
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().to_path_buf();
    let done = Arc::new(AtomicBool::new(false));
    let result = done.clone();
    let app = tauri::Builder::default()
        .setup(move |app| {
            manager_log::init(app.handle(), &path);
            let window = tauri::WindowBuilder::new(app, "probe")
                .title("Manager resource diagnostic test")
                .visible(false)
                .build()?;
            let handle = app.handle().clone();
            let exit = handle.clone();
            let view = wry::WebViewBuilder::new()
                .with_incognito(true)
                .with_url("http://127.0.0.1:1421/#/editor")
                .with_navigation_handler(|url| url.starts_with("http://127.0.0.1:1421/"))
                .with_initialization_script(include_str!("resource_diagnostics_probe.js"))
                .with_ipc_handler(move |request| {
                    let message = request.body();
                    match message.as_str() {
                        "finish" => {
                            let reports = manager_log::resource_reports();
                            let expected = serde_json::json!({
                                "route": "editor", "owner": "sd-device-preview", "role": "control-surface"
                            });
                            let valid = reports.len() == 1 && serde_json::to_value(&reports[0]).is_ok_and(|v| {
                                v["reason"] == "resource_load" && v["resource"] == expected
                            });
                            result.store(valid, Ordering::Release);
                            handle.exit(if valid {0} else {1});
                        },
                        "failure-editor" | "failure-settings" | "failure-empty-media" | "failure-broken-image" => {
                            eprintln!("WKWebView stage: {message}");
                            handle.exit(1);
                        },
                        _ => {
                            if let Ok(report) = serde_json::from_str(message) {
                                if manager_log::frontend("main", report).is_err() { handle.exit(1); }
                            }
                        }
                    }
                })
                .build(&window)?;
            VIEW.with(|owner| *owner.borrow_mut() = Some(view));
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(60)).await;
                exit.exit(1);
            });
            Ok(())
        })
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    app.run_return(|_, _| {});
    tauri_plugin_log::log::logger().flush();
    assert!(
        done.load(Ordering::Acquire),
        "WKWebView fixture did not report the expected resource failure"
    );
    let log = std::fs::read_dir(manager_log::directory(root.path()))
        .unwrap()
        .flatten()
        .filter(|f| f.path().extension().is_some_and(|e| e == "log"))
        .filter_map(|f| std::fs::read_to_string(f.path()).ok())
        .collect::<String>();
    assert!(
        log.contains("control-surface")
            && log.contains("sd-device-preview")
            && !log.contains("private-fixture")
    );
    println!("WKWebView: two editor round trips, zero empty-media errors, one bounded native log entry for the broken control surface.");
}
#[cfg(any(not(target_os = "macos"), test))]
fn main() {}
