//! Real command deserialization and the compiled desktop ACL, without Wry, HID or OS actions
use super::*;
use serde_json::{json, Value};
use tauri::test::MockRuntime;

// Handle the current browser package desktop command
fn current_browser_package(root: &std::path::Path, id: &str) {
    use base64::Engine;
    let asset = json!({"light":"icon.svg","dark":"icon.svg"});
    let capabilities = json!({"version":1,"id":id,"plugin_version":"0.1.0","theme_protocol":0,"settings_protocol":0,"input_protocol":0,"icon":asset,"actions":{format!("{id}.run"):{"control_types":["button"],"icon":asset,"states":[asset]}}});
    let manifest = json!({"UUID":id,"Name":"IPC fixture","Version":"0.1.0","SDKVersion":1,"CodePath":"index.html","Icon":"icon.svg","StreamDockManager":{"ManifestDialect":"vsd","Capabilities":capabilities},"Actions":[{"UUID":format!("{id}.run"),"Name":"Run","Controllers":["Keypad"],"Icon":"icon.svg","States":[{"Image":"icon.svg"}]}]});
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&capabilities).unwrap());
    std::fs::write(
        root.join("index.html"),
        format!(
            "<!doctype html><!-- SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1 -->",
            encoded.len()
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("icon.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    std::fs::write(root.join("manifest.json"), manifest.to_string()).unwrap();
}

// Dispatch the authorized recovery command
fn invoke(
    window: &tauri::WebviewWindow<MockRuntime>,
    name: &str,
    body: Value,
) -> Result<Value, Value> {
    invoke_body(window, name, tauri::ipc::InvokeBody::Json(body), &[])
}

// Handle the invoke body desktop command
fn invoke_body(
    window: &tauri::WebviewWindow<MockRuntime>,
    name: &str,
    body: tauri::ipc::InvokeBody,
    headers: &[(&str, &str)],
) -> Result<Value, Value> {
    tauri::test::get_ipc_response(
        window,
        tauri::webview::InvokeRequest {
            cmd: name.into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: if cfg!(target_os = "windows") {
                "http://tauri.localhost"
            } else {
                "tauri://localhost"
            }
            .parse()
            .unwrap(),
            body,
            headers: std::iter::once(("x-stream-dock-manager-editor-generation", "1"))
                .chain(headers.iter().copied())
                .map(|(key, value)| (key.parse().unwrap(), value.parse().unwrap()))
                .collect(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        },
    )
    .map(|response| response.deserialize().unwrap())
}

struct Shutdown(Arc<Runtime>);
impl Drop for Shutdown {
    // Handle the drop desktop command
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

#[test]
// Verify keyboard settings are editor only and validate real IPC payloads
fn keyboard_settings_are_editor_only_and_validate_real_ipc_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    runtime.simulate_device(true, "simulator:k1-pro").unwrap();
    runtime.simulate_device(true, "simulator:n4-pro").unwrap();
    let app = tauri::test::mock_builder()
        .manage(runtime.clone())
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![apply_keyboard_settings])
        .build(crate::desktop_context())
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let plugin = tauri::WebviewWindowBuilder::new(&app, "plugin-fixture", Default::default())
        .build()
        .unwrap();
    let payload = json!({
        "deviceId": "simulator:k1-pro",
        "settings": {"brightness": 70, "color": [255, 0, 32], "os_mode": "macos", "effect":1, "speed":1}
    });
    assert!(invoke(&plugin, "apply_keyboard_settings", payload.clone())
        .unwrap_err()
        .to_string()
        .contains("not allowed"));
    for settings in [
        json!({}),
        json!({"brightness": 101}),
        json!({"brightness": -1}),
        json!({"brightness": 1.5}),
        json!({"color": [256, 0, 0]}),
        json!({"color": [0, 0]}),
        json!({"os_mode": "linux"}),
        json!({"effect": 16}),
        json!({"speed": 8}),
    ] {
        assert!(
            invoke(
                &main,
                "apply_keyboard_settings",
                json!({"deviceId": "simulator:k1-pro", "settings": settings})
            )
            .is_err(),
            "invalid keyboard settings were accepted: {settings}"
        );
    }
    assert!(invoke(
        &main,
        "apply_keyboard_settings",
        json!({"deviceId": "simulator:n4-pro", "settings": {"brightness": 3}})
    )
    .is_err());
    let result = invoke(&main, "apply_keyboard_settings", payload).unwrap();
    assert_eq!(result["status"], "simulated");
    assert_eq!(result["saved"], true);
    let keyboard = &result["snapshot"]["sessions"]["simulator:k1-pro"]["output"]["keyboard"];
    assert_eq!(keyboard["brightness"], 70);
    assert_eq!(keyboard["effect"], 1);
    assert_eq!(keyboard["speed"], 1);
    assert_eq!(keyboard["color"], json!([255, 0, 32]));
    assert_eq!(keyboard["os_mode"], "macos");
    assert_eq!(
        result["snapshot"]["sessions"]["simulator:k1-pro"]["output"]["brightness"],
        65
    );
}

#[test]
// Verify manager log folder is not available to plugin or auxiliary windows
fn manager_log_folder_is_not_available_to_plugin_or_auxiliary_windows() {
    let app = tauri::test::mock_builder()
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![open_manager_logs])
        .build(crate::desktop_context())
        .unwrap();
    for label in [
        "plugin-fixture",
        "log-reader-fixture",
        "action-popup-fixture",
    ] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        let error = invoke(&window, "open_manager_logs", json!({})).unwrap_err();
        assert!(error.to_string().contains("not allowed"), "{error}");
    }
}

#[test]
// Verify still source preview rasterizes full SVG without the draft crop
fn still_source_preview_rasterizes_full_svg_without_the_draft_crop() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let source = directory.path().join("landscape.svg");
    std::fs::write(&source, br#"<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="640"><path fill="red" d="M0 0h640v640H0z"/><path fill="blue" d="M640 0h640v640H640z"/></svg>"#).unwrap();
    let asset = runtime.import_asset(&source).unwrap();
    let app = tauri::test::mock_builder()
        .manage(runtime)
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![media_preview])
        .build(crate::desktop_context())
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let bytes = invoke(
        &main,
        "media_preview",
        json!({
            "asset": asset, "deviceId": "fixture", "source": true,
            "options": { "crop": [0.5, 0, 0.5, 1] }
        }),
    )
    .unwrap();
    let bytes: Vec<u8> = serde_json::from_value(bytes).unwrap();
    let len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let header: Value = serde_json::from_slice(&bytes[4..4 + len]).unwrap();
    assert_eq!(header["mime"], "image/png");
    assert_eq!(header["animated"], false);
    let png = &bytes[4 + len..];
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 640);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 320);
}

#[test]
// Verify dropped archive binary transfer is editor only and releases the original filename
fn dropped_archive_binary_transfer_is_editor_only_and_releases_the_original_filename() {
    use crate::archive_import::ArchiveDrops;
    use base64::{engine::general_purpose::STANDARD, Engine};

    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let filename = "Shared profiles.zip";
    let source = directory.path().join(filename);
    runtime.export_profiles(&source).unwrap();
    let bytes = std::fs::read(&source).unwrap();
    let app = tauri::test::mock_builder()
        .manage(ArchiveDrops::default())
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![
            crate::archive_import::append_archive_drop,
            crate::archive_import::inspect_archive_drop,
            crate::archive_import::discard_archive_drop
        ])
        .build(crate::desktop_context())
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let popup = tauri::WebviewWindowBuilder::new(&app, "action-popup-fixture", Default::default())
        .build()
        .unwrap();
    let size = bytes.len().to_string();
    let encoded_name = STANDARD.encode(filename);
    let append = |window, chunk: &[u8], offset| {
        invoke_body(
            window,
            "append_archive_drop",
            tauri::ipc::InvokeBody::Raw(chunk.to_vec()),
            &[
                ("x-archive-size", &size),
                ("x-archive-offset", offset),
                ("x-archive-name", &encoded_name),
            ],
        )
    };
    assert!(append(&popup, &bytes[..1], "0")
        .unwrap_err()
        .to_string()
        .contains("not allowed"));
    append(&main, &bytes[..1], "0").unwrap();
    assert!(invoke(&main, "inspect_archive_drop", json!({"path":null})).is_err());
    assert!(append(&main, &bytes[2..], "2").is_err());
    append(&main, &bytes[1..], "1").unwrap();
    let reviewed = invoke(&main, "inspect_archive_drop", json!({"path":null})).unwrap();
    let path = std::path::Path::new(reviewed[0].as_str().unwrap());
    assert_eq!(path.file_name().unwrap(), filename);
    assert_eq!(reviewed[1], "profiles");
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    invoke(&main, "discard_archive_drop", json!({})).unwrap();
    assert!(!path.exists());
}

#[test]
// Verify dropped media is scoped to the editor and retired after use
fn dropped_media_is_scoped_to_the_editor_and_retired_after_use() {
    use crate::media_drop::MediaDrops;

    let app = tauri::test::mock_builder()
        .manage(MediaDrops::default())
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![
            crate::media_drop::append_media_drop,
            crate::media_drop::finish_media_drop,
            crate::media_drop::discard_media_drop
        ])
        .build(crate::desktop_context())
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let popup = tauri::WebviewWindowBuilder::new(&app, "action-popup-fixture", Default::default())
        .build()
        .unwrap();
    let formats: [(&str, &[u8]); 5] = [
        ("png", b"\x89PNG\r\n\x1a\n"),
        ("jpeg", b"\xff\xd8\xff\xe0"),
        ("svg", br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><path d="M0 0h2v2H0z"/></svg>"#),
        ("gif", b"GIF89a"),
        ("mp4", b"\x00\x00\x00\x0cftypmp42"),
    ];
    for (format, bytes) in formats {
        let size = bytes.len().to_string();
        let id = uuid::Uuid::new_v4().to_string();
        let append = |window, chunk: &[u8], offset| {
            invoke_body(
                window,
                "append_media_drop",
                tauri::ipc::InvokeBody::Raw(chunk.to_vec()),
                &[
                    ("x-media-id", &id),
                    ("x-media-format", format),
                    ("x-media-size", &size),
                    ("x-media-offset", offset),
                ],
            )
        };
        assert!(append(&popup, &bytes[..1], "0")
            .unwrap_err()
            .to_string()
            .contains("not allowed"));
        append(&main, &bytes[..1], "0").unwrap();
        assert!(invoke(&main, "finish_media_drop", json!({"id":id})).is_err());
        assert!(append(&main, &bytes[1..], "2").is_err());
        append(&main, &bytes[1..], "1").unwrap();
        let finished = invoke(&main, "finish_media_drop", json!({"id":id})).unwrap();
        let path = std::path::Path::new(finished.as_str().unwrap());
        assert_eq!(path.extension().unwrap(), format);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        invoke(&main, "discard_media_drop", json!({"id":id})).unwrap();
        assert!(!path.exists());
    }
}

#[test]
// Verify interface theme writes are editor only and popups can read the result
fn interface_theme_writes_are_editor_only_and_popups_can_read_the_result() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let app = tauri::test::mock_builder()
        .manage(runtime)
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![
            crate::appearance_preferences,
            crate::set_appearance
        ])
        .build(crate::desktop_context())
        .unwrap();
    let editor = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let popup = tauri::WebviewWindowBuilder::new(&app, "action-popup-fixture", Default::default())
        .build()
        .unwrap();
    let before = invoke(&editor, "appearance_preferences", json!({})).unwrap();
    assert_eq!(before["style"]["textScale"], 100);
    let args = json!({
        "style": {"lightPalette":"mist-blue", "darkPalette":"deep-teal", "font":"sans", "textScale":125}
    });
    assert!(invoke(&popup, "set_appearance", args.clone()).is_err());
    invoke(&editor, "set_appearance", args.clone()).unwrap();
    let committed = invoke(&popup, "appearance_preferences", json!({})).unwrap();
    assert_eq!(committed["style"], args["style"]);
    assert_eq!(committed["preference"], before["preference"]);
    let mut selection = args;
    selection["preference"] = json!("dark");
    selection["style"]["darkPalette"] = json!("ember");
    assert!(invoke(&popup, "set_appearance", selection.clone()).is_err());
    invoke(&editor, "set_appearance", selection.clone()).unwrap();
    let committed = invoke(&popup, "appearance_preferences", json!({})).unwrap();
    assert_eq!(committed["preference"], "dark");
    assert_eq!(committed["style"], selection["style"]);
    let mut invalid = selection;
    invalid["preference"] = json!("light");
    invalid["style"]["textScale"] = json!(127);
    assert!(invoke(&editor, "set_appearance", invalid).is_err());
    assert_eq!(
        invoke(&editor, "appearance_preferences", json!({})).unwrap(),
        committed
    );
}

#[test]
// Verify log reader windows use capture commands without workspace permissions
fn log_reader_windows_use_capture_commands_without_workspace_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let capture = runtime
        .plugin_logs
        .start("org.test.logs", "Log fixture")
        .unwrap();
    runtime
        .plugin_logs
        .record("org.test.logs", "run", "stdout", "rpc", "retained");
    let app = tauri::test::mock_builder()
        .manage(runtime.clone())
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![
            plugin_log_preferences,
            plugin_log_captures,
            read_plugin_logs,
            plugin_log_network_detail,
            stop_plugin_log_capture,
            list_plugin_profiles
        ])
        .build(crate::desktop_context())
        .unwrap();
    let popup = tauri::WebviewWindowBuilder::new(&app, "log-reader-fixture", Default::default())
        .build()
        .unwrap();
    assert_eq!(
        invoke(&popup, "plugin_log_captures", json!({})).unwrap()[0]["id"],
        capture.id
    );
    let lease = runtime
        .plugin_logs
        .subscribe_owned(&capture.id, popup.label())
        .unwrap();
    let page = invoke(
        &popup,
        "read_plugin_logs",
        json!({"lease":lease,"cursor":null,"forward":false}),
    )
    .unwrap();
    assert_eq!(page["entries"][0]["message"], "retained");
    let mut preferences = runtime.plugin_logs.preferences();
    preferences
        .plugins
        .entry("org.test.logs".into())
        .or_default()
        .debug_network = true;
    runtime.plugin_logs.save_preferences(preferences).unwrap();
    runtime.plugin_logs.record_network(
        "org.test.logs",
        "run",
        "stdout",
        "HTTP POST service.test",
        stream_dock_manager_plugin_host::log_capture::LogNetworkDetail {
            request_id: "1".into(),
            request_headers: None,
            response_headers: None,
            request: stream_dock_manager_plugin_host::log_capture::LogNetworkBody {
                text: "request body".into(),
                encoding: "utf8".into(),
                complete: true,
                ..Default::default()
            },
            response: stream_dock_manager_plugin_host::log_capture::LogNetworkBody {
                text: "response body".into(),
                encoding: "utf8".into(),
                complete: true,
                ..Default::default()
            },
        },
    );
    let page = invoke(
        &popup,
        "read_plugin_logs",
        json!({"lease":lease,"cursor":null,"forward":false}),
    )
    .unwrap();
    let cursor = &page["entries"][1]["cursor"];
    let detail = invoke(
        &popup,
        "plugin_log_network_detail",
        json!({"lease":lease,"cursor":cursor}),
    )
    .unwrap();
    assert_eq!(detail["request"]["text"], "request body");
    assert_eq!(detail["response"]["text"], "response body");
    assert!(invoke(
        &popup,
        "list_plugin_profiles",
        json!({"id":"org.test.logs"})
    )
    .unwrap_err()
    .as_str()
    .unwrap()
    .contains("not allowed"));
    for label in ["pi-log-fixture", "action-popup-log-fixture"] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        assert!(invoke(
            &window,
            "plugin_log_network_detail",
            json!({"lease":lease,"cursor":cursor})
        )
        .unwrap_err()
        .as_str()
        .unwrap()
        .contains("not allowed"));
        assert!(invoke(&window, "plugin_log_captures", json!({}))
            .unwrap_err()
            .as_str()
            .unwrap()
            .contains("not allowed"));
    }
    invoke(&popup, "stop_plugin_log_capture", json!({"id":capture.id})).unwrap();
    assert!(!runtime.plugin_logs.list()[0].capturing);
}

#[test]
// Verify suggested profiles use the real native commands and main window permissions
fn suggested_profiles_use_the_real_native_commands_and_main_window_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let source = directory.path().join("org.test.ipc.sdPlugin");
    std::fs::create_dir_all(source.join("profiles")).unwrap();
    std::fs::write(
        source.join("index.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    current_browser_package(&source, "org.test.ipc");
    std::fs::write(source.join("profiles/n4-pro.json"), json!({"schema_version":1,"configuration_revision":0,"saved_macros":[],"rules":[],"history":stream_dock_manager_runtime::workspace::HistoryIndex::default(),"active_profile":"profile","profiles":[{"id":"profile","name":"Suggested","model_id":"n4-pro","active_page":"page","pages":[{"id":"page","name":"Suggested page","bindings":{}}]}]}).to_string()).unwrap();
    let review = runtime.prepare_plugin_package(&source).unwrap();
    runtime
        .commit_plugin_package(&review.id, false, false)
        .unwrap();

    let app = tauri::test::mock_builder()
        .manage(runtime.clone())
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![
            pending_plugin_profile_setup,
            list_plugin_profiles,
            acknowledge_plugin_profile_setup,
            prepare_plugin_profile_import
        ])
        .build(crate::desktop_context())
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let pending = invoke(
        &main,
        "pending_plugin_profile_setup",
        json!({"ids":["org.test.ipc"]}),
    )
    .unwrap();
    assert_eq!(pending[0]["plugin_id"], "org.test.ipc");
    let catalog = invoke(&main, "list_plugin_profiles", json!({"id":"org.test.ipc"})).unwrap();
    assert_eq!(pending[0], catalog);
    assert_eq!(catalog["suggestions"][0]["error"], Value::Null, "{catalog}");
    let preview = invoke(&main, "prepare_plugin_profile_import", json!({"id":"org.test.ipc","key":"profiles/n4-pro.json","targetProfile":null,"targetPage":null})).unwrap();
    assert!(preview["id"].is_string());
    runtime.cancel_profile_import(preview["id"].as_str().unwrap());
    invoke(
        &main,
        "acknowledge_plugin_profile_setup",
        json!({"id":"org.test.ipc","fingerprint":catalog["fingerprint"]}),
    )
    .unwrap();
    assert_eq!(
        invoke(
            &main,
            "pending_plugin_profile_setup",
            json!({"ids":["org.test.ipc"]})
        )
        .unwrap(),
        json!([])
    );
    assert!(invoke(
        &main,
        "pending_plugin_profile_setup",
        json!({"ids":"wrong type"})
    )
    .is_err());

    for label in ["action-popup-fixture", "pi-fixture"] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        let denied =
            invoke(&window, "pending_plugin_profile_setup", json!({"ids":[]})).unwrap_err();
        assert!(denied.as_str().unwrap().contains("not allowed"), "{denied}");
    }
}

#[test]
// Verify frontend reports are closed bounded and only available to first party windows
fn frontend_reports_are_closed_bounded_and_only_available_to_first_party_windows() {
    let app = tauri::test::mock_builder()
        .invoke_handler(tauri::generate_handler![report_frontend_error])
        .build(crate::desktop_context())
        .unwrap();
    let report = json!({"event":"runtime_error","reason":"type","frames":[{"component":"main","line":42,"column":9}]});
    for label in ["main", "log-reader-fixture", "action-popup-fixture"] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        // Bootstrap logging does not depend on editor admission or even a Runtime
        invoke_body(
            &window,
            "report_frontend_error",
            tauri::ipc::InvokeBody::Json(json!({"report":report})),
            &[("x-stream-dock-manager-editor-generation", "0")],
        )
        .unwrap();
        let mut private = report.clone();
        private["message"] = json!("Bearer private-token /Users/private");
        assert!(invoke(&window, "report_frontend_error", json!({"report":private})).is_err());
        let mut oversized = report.clone();
        oversized["frames"] = json!(vec![report["frames"][0].clone(); 13]);
        assert!(invoke(
            &window,
            "report_frontend_error",
            json!({"report":oversized})
        )
        .is_err());
    }
    for label in ["plugin-fixture", "pi-fixture", "tray-device-status"] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        assert!(
            invoke(&window, "report_frontend_error", json!({"report":report}))
                .unwrap_err()
                .to_string()
                .contains("not allowed")
        );
    }
}

#[test]
// Verify recovery plugin enablement commands save without opening a plugin view
fn recovery_plugin_enablement_commands_save_without_opening_a_plugin_view() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let source = directory.path().join("org.test.recovery.sdPlugin");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("index.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    current_browser_package(&source, "org.test.recovery");
    let review = runtime.prepare_plugin_package(&source).unwrap();
    runtime
        .commit_plugin_package(&review.id, false, false)
        .unwrap();
    runtime.plugins.lock().unwrap().enter_recovery();
    let app = tauri::test::mock_builder()
        .manage(runtime.clone())
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![crate::enable_plugin])
        .build(crate::desktop_context())
        .unwrap();
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    for enabled in [true, false, true] {
        let snapshot = invoke(
            &window,
            "enable_plugin",
            json!({"id":"org.test.recovery", "enabled":enabled}),
        )
        .unwrap();
        let plugin = snapshot["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|plugin| plugin["id"] == "org.test.recovery")
            .unwrap();
        assert_eq!(plugin["enabled"], enabled);
        assert_ne!(plugin["runtime"]["state"], "running");
        assert_ne!(plugin["runtime"]["state"], "starting");
        assert!(runtime.recovery_status().active);
        assert_eq!(app.webview_windows().len(), 1);
    }
}

#[test]
// Verify private runtime removal is editor only and rejects unknown installations
fn private_runtime_removal_is_editor_only_and_rejects_unknown_installations() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let app = tauri::test::mock_builder()
        .manage(runtime)
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![runtime_remove])
        .build(crate::desktop_context())
        .unwrap();
    for label in [
        "plugin-fixture",
        "log-reader-fixture",
        "action-popup-fixture",
    ] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        let error = invoke(
            &window,
            "runtime_remove",
            json!({"installationId":"external-node"}),
        )
        .unwrap_err();
        assert!(error.to_string().contains("not allowed"), "{error}");
    }
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let error = invoke(
        &window,
        "runtime_remove",
        json!({"installationId":"../outside"}),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Unknown private runtime installation"),
        "{error}"
    );
}

#[test]
// Verify cache commands are editor only and clear only disposable fixture data
fn cache_commands_are_editor_only_and_clear_only_disposable_fixture_data() {
    let directory = tempfile::tempdir().unwrap();
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))
            .unwrap();
    let _shutdown = Shutdown(runtime.clone());
    let app = tauri::test::mock_builder()
        .manage(runtime)
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![cache_status, clear_cache])
        .build(crate::desktop_context())
        .unwrap();
    let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let status = invoke(&main, "cache_status", json!({})).unwrap();
    assert_eq!(status["folders"].as_array().unwrap().len(), 3);
    let cleared = invoke(&main, "clear_cache", json!({})).unwrap();
    assert_eq!(cleared["removed_entries"], 0);
    for label in [
        "plugin-fixture",
        "log-reader-fixture",
        "action-popup-fixture",
    ] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        for name in ["cache_status", "clear_cache"] {
            let error = invoke(&window, name, json!({})).unwrap_err();
            assert!(error.to_string().contains("not allowed"), "{error}");
        }
    }
}

#[test]
// Verify keyboard macro recorder commands are main editor only
fn keyboard_macro_recorder_commands_are_main_editor_only() {
    let app = tauri::test::mock_builder()
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![
            keyboard_macro_capabilities,
            start_keyboard_macro_recording,
            append_keyboard_macro_events,
            heartbeat_keyboard_macro_recording,
            stop_keyboard_macro_recording,
            cancel_keyboard_macro_recording,
            begin_keyboard_macro_test,
            start_keyboard_macro_test,
            keyboard_macro_test_status,
            cancel_keyboard_macro_test,
        ])
        .build(crate::desktop_context())
        .unwrap();
    let plugin =
        tauri::WebviewWindowBuilder::new(&app, "plugin-recorder-probe", Default::default())
            .build()
            .unwrap();
    for command in [
        "keyboard_macro_capabilities",
        "start_keyboard_macro_recording",
        "append_keyboard_macro_events",
        "heartbeat_keyboard_macro_recording",
        "stop_keyboard_macro_recording",
        "cancel_keyboard_macro_recording",
        "begin_keyboard_macro_test",
        "start_keyboard_macro_test",
        "keyboard_macro_test_status",
        "cancel_keyboard_macro_test",
    ] {
        assert!(
            invoke(&plugin, command, json!({}))
                .unwrap_err()
                .to_string()
                .contains("not allowed"),
            "{command}"
        );
    }
}

#[test]
// Verify startup permission commands are main editor only
fn startup_permission_commands_are_main_editor_only() {
    let app = tauri::test::mock_builder()
        .manage(Arc::new(crate::editor_lifecycle::EditorLifecycle::default()))
        .invoke_handler(tauri::generate_handler![
            startup_permissions,
            complete_startup_permissions,
            open_startup_permission_settings,
        ])
        .build(crate::desktop_context())
        .unwrap();
    for label in [
        "plugin-permission-probe",
        "action-popup-permission-probe",
        "log-reader-permission-probe",
    ] {
        let window = tauri::WebviewWindowBuilder::new(&app, label, Default::default())
            .build()
            .unwrap();
        for command in [
            "startup_permissions",
            "complete_startup_permissions",
            "open_startup_permission_settings",
        ] {
            let error = invoke(&window, command, json!({"id":"accessibility"})).unwrap_err();
            assert!(
                error.to_string().contains("not allowed"),
                "{command}: {error}"
            );
        }
    }
}
