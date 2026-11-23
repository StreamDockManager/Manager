use crate::{
    actions::Action,
    workspace::{AppRule, Draft, Edit},
    Runtime,
};
use hardware_core::{ControlEvent, Input};
use std::{
    path::Path,
    time::{Duration, Instant},
};

// Construct the marker fixture for simulator tests
fn marker(path: &Path) -> Action {
    #[cfg(unix)]
    {
        Action::Command {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf executed > \"$1\"".into(),
                "fixture".into(),
                path.to_string_lossy().into(),
            ],
        }
    }
    #[cfg(windows)]
    {
        Action::Command {
            program: "powershell.exe".into(),
            args: vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-WindowStyle".into(),
                "Hidden".into(),
                "-Command".into(),
                // cmd.exe does not decode quoted arguments like a normal Windows executable
                format!(
                    "[System.IO.File]::WriteAllText('{}', 'executed')",
                    path.to_string_lossy().replace('\'', "''")
                ),
            ],
        }
    }
}
// Wait for until
fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(condition(), "Timed out waiting for simulator action");
}
// Save action
fn save_action(runtime: &Runtime, action: Action) {
    let state = runtime.snapshot();
    let mut profile = state.profiles[0].clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = action;
    runtime
        .save_profile(profile, state.configuration_revision)
        .unwrap();
}

#[test]
// Verify that saved backgrounds survive sync changes and legacy sessions
fn saved_backgrounds_survive_sync_changes_and_legacy_sessions() {
    use stream_dock_manager_plugin_host::{
        CustomPalette, DeviceThemePreferences, PaletteWallpaper, UiThemeStyle,
    };
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("app");
    let runtime = Runtime::build(config.clone(), false).unwrap();
    let a = "simulator:n4-pro:appearance-a";
    let b = "simulator:n4-pro:appearance-b";
    runtime.simulate_device(true, a).unwrap();
    runtime.simulate_device(true, b).unwrap();

    let source = root.path().join("wallpaper.svg");
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 16"><defs><linearGradient id="wash"><stop stop-color="#005741"/><stop offset="1" stop-color="#13466e"/></linearGradient></defs><path fill="url(#wash)" d="M0 0h32v16H0z"/></svg>"##;
    std::fs::write(&source, svg).unwrap();
    let asset = runtime.profiles.import_asset(&source).unwrap();
    assert_eq!(runtime.profiles.asset_extension(&asset).unwrap(), "svg");
    assert_eq!(runtime.profiles.asset(&asset).unwrap(), svg);
    let state = runtime.snapshot();
    assert_eq!(state.session(a).profile_id, state.session(b).profile_id);
    let mut profile = state
        .profiles
        .iter()
        .find(|p| p.id == state.session(a).profile_id)
        .unwrap()
        .clone();
    profile.background_asset = Some(asset.clone());
    let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
    binding.asset = Some(asset.clone());
    binding.color = "#ff0000".into();
    runtime
        .save_profile(profile.clone(), state.configuration_revision)
        .unwrap();
    let appearance = runtime
        .update_theme_palette(
            Some(CustomPalette {
                id: String::new(),
                name: "Test palette".into(),
                mode: "dark".into(),
                colors: [("surface".into(), "#172138".into())].into(),
                wallpaper: Some(PaletteWallpaper {
                    asset_id: asset.clone(),
                    format: String::new(),
                    options: serde_json::json!({}),
                    animated: false,
                    svg: None,
                }),
            }),
            None,
        )
        .unwrap();
    let palette = appearance.custom_palettes[0].id.clone();
    runtime
        .plugins
        .lock()
        .unwrap()
        .set_ui_theme_style(
            UiThemeStyle {
                dark_palette: palette.clone(),
                ..Default::default()
            },
            Some("dark"),
        )
        .unwrap();
    assert!(runtime
        .prepare_media_deletion(&asset)
        .unwrap_err()
        .contains("wallpaper"));
    let archive = root.path().join("svg-profile.zip");
    runtime.profiles.export(&archive).unwrap();
    let imported = crate::profiles::ProfileStore::open(root.path().join("imported")).unwrap();
    let decoded = imported.import_for_repair(&archive).unwrap();
    let restored_asset = decoded
        .profiles
        .iter()
        .find_map(|profile| profile.background_asset.as_ref())
        .unwrap();
    assert_eq!(imported.asset(restored_asset).unwrap(), svg);
    let bad = root.path().join("unsafe.svg");
    std::fs::write(&bad, br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><script>alert(1)</script></svg>"#).unwrap();
    assert!(runtime.profiles.import_asset(&bad).is_err());

    let before = runtime.preview_surfaces(Some(a)).unwrap();
    for sync in [false, true] {
        for transparent_buttons in [false, true] {
            runtime
                .plugins
                .lock()
                .unwrap()
                .set_device_theme_preferences(DeviceThemePreferences {
                    sync,
                    transparent_buttons,
                })
                .unwrap();
            for device in [a, b] {
                let preview = runtime.preview_surfaces(Some(device)).unwrap();
                assert_eq!(preview.background, before.background);
                assert_eq!(preview.surfaces["key-1"], before.surfaces["key-1"]);
            }
        }
    }

    // Older persisted sessions used a mode that masked every saved design
    let mut sessions = serde_json::to_value(runtime.snapshot().sessions).unwrap();
    let session = sessions
        .get_mut(
            &runtime
                .snapshot()
                .devices
                .iter()
                .find(|d| d.id == a)
                .unwrap()
                .persistence_key,
        )
        .unwrap();
    session["display_appearance"] = serde_json::json!("palette");
    runtime
        .profiles
        .write_preference(
            crate::profile_database::LocalPreference::Sessions,
            &sessions,
        )
        .unwrap();
    runtime.shutdown();
    drop(runtime);

    let runtime = Runtime::build(config, false).unwrap();
    runtime.simulate_device(true, a).unwrap();
    let restored = runtime.preview_surfaces(Some(a)).unwrap();
    assert_eq!(restored.background, before.background);
    assert_eq!(restored.surfaces["key-1"], before.surfaces["key-1"]);
    assert_eq!(
        runtime
            .update_theme_palette(None, Some(palette))
            .unwrap()
            .style
            .dark_palette,
        "default"
    );
    runtime.shutdown();
}

#[test]
// Verify that themed plugin background respects explicit transparency and color
fn themed_plugin_background_respects_explicit_transparency_and_color() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let state = runtime.snapshot();
    let mut binding = state.profiles[0].pages[0].bindings["key-1"].clone();
    let models = hardware_registry::manifests();
    let control = models
        .iter()
        .find(|m| m.model_id == "n4-pro")
        .unwrap()
        .control("key-1")
        .unwrap();
    binding.color = "#c61e3200".into();
    binding.action = Action::Plugin {
        user_desired_state: None,
        plugin_id: "fixture.example".into(),
        action_id: "control".into(),
        settings: serde_json::json!({}),
        resources: Default::default(),
    };
    let art = |color| {
        use base64::Engine;
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="{color}"/></svg>"#
        );
        format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        )
    };
    let visual = std::sync::Arc::new(serde_json::json!({
        "paletteTile": true,
        "layout": { "id": "tile", "background": "#102030", "items": [
            {"key":"backdrop","type":"pixmap","rect":[0,0,200,100],"value":art("#cc0000"),"streamDockManager":{"role":"background"}},
            {"key":"decoration","type":"pixmap","rect":[0,0,40,30],"value":art("#cc0000"),
             "streamDockManager":{"role":"background"}},
            {"key":"glyph","type":"pixmap","rect":[50,25,100,50],"value":art("#33aa44")}
        ]}
    }));
    let mut scene = runtime.render_scene("fixture").unwrap();
    scene.visuals.insert("key-1".into(), visual.clone());
    scene.device_surface = "#254668".into();
    let corner = |scene: &crate::render_scene::RenderSceneContext, binding: &crate::Binding| {
        let png = runtime
            .render_bound_surface_in_scene(control, Some(binding), 0, &scene.scope, "key-1", scene)
            .unwrap();
        let pixels = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(
            pixels.get_pixel(pixels.width() / 2, pixels.height() / 2).0,
            [51, 170, 68, 255]
        );
        pixels.get_pixel(0, 0).0
    };
    assert_eq!(corner(&scene, &binding)[3], 0);
    scene.device_theme.transparent_buttons = false;
    assert_eq!(corner(&scene, &binding), [37, 70, 104, 255]);
    scene
        .plugin_surfaces
        .insert("fixture.example".into(), "#446688".into());
    assert_eq!(corner(&scene, &binding), [68, 102, 136, 255]);
    scene.device_theme.sync = false;
    scene.device_theme.transparent_buttons = true;
    assert_eq!(corner(&scene, &binding), [204, 0, 0, 255]);
    binding.custom_background = true;
    assert_eq!(corner(&scene, &binding)[3], 0);
    binding.color = "#ff0099".into();
    scene.device_theme.sync = true;
    assert_eq!(corner(&scene, &binding), [255, 0, 153, 255]);
    let media = root.path().join("background.png");
    image::RgbImage::from_pixel(16, 16, image::Rgb([16, 64, 128]))
        .save(&media)
        .unwrap();
    binding.asset = Some(runtime.profiles.import_asset(&media).unwrap());
    assert_eq!(corner(&scene, &binding), [16, 64, 128, 255]);
    let saved_asset = binding.asset.clone();
    binding.background_source = Some(crate::profiles::BackgroundSource::Palette);
    for sync in [false, true] {
        scene.device_theme.sync = sync;
        for transparent in [false, true] {
            scene.device_theme.transparent_buttons = transparent;
            assert_eq!(corner(&scene, &binding), [37, 70, 104, 255]);
        }
    }
    // Palette edits change rendered output without mutating the saved custom style
    scene.device_surface = "#123456".into();
    assert_eq!(corner(&scene, &binding), [18, 52, 86, 255]);
    assert_eq!(binding.asset, saved_asset);
    assert_eq!(binding.color, "#ff0099");
    let palette_draft = binding.clone();
    binding.background_source = Some(crate::profiles::BackgroundSource::Custom);
    assert_eq!(corner(&scene, &binding), [16, 64, 128, 255]);
    scene.draft = Some(crate::hardware_preview::HardwareDraft {
        display_test: None,
        palette: None,
        bindings: std::collections::BTreeMap::from([("key-1".into(), palette_draft)]),
        background: None,
        scope: None,
        preview_states: Default::default(),
    });
    assert_eq!(corner(&scene, &binding), [18, 52, 86, 255]);
    scene.draft = None;
    assert_eq!(corner(&scene, &binding), [16, 64, 128, 255]);
    assert_eq!(visual["layout"]["items"].as_array().unwrap().len(), 3);
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; runs on a disposable simulator and writes only a temporary marker"]
// Verify that manual saved macro run uses device context and saved inputs
fn manual_saved_macro_run_uses_device_context_and_saved_inputs() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let device = "simulator:manual-macro";
    runtime.simulate_device(true, device).unwrap();
    let path = root.path().join("manual.txt");
    let action = Action::Macro {
        actions: vec![
            Action::Brightness { value: 43 },
            Action::Builtin {
                action: Box::new(crate::builtins::BuiltinAction::WriteFile {
                    path: "${input.destination}".into(),
                    text: "${input.message}".into(),
                    operation: "create".into(),
                    replace: false,
                    template: false,
                }),
            },
        ],
    };
    let snapshot = runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Save manual macro fixture",
            vec![Edit::SaveMacro {
                value: crate::builtins::SavedMacro {
                    id: "manual".into(),
                    name: "Manual macro".into(),
                    action,
                    inputs: vec![
                        crate::macro_inputs::MacroInput {
                            name: "destination".into(),
                            label: "Destination".into(),
                            r#type: "string".into(),
                            required: true,
                            default: None,
                        },
                        crate::macro_inputs::MacroInput {
                            name: "message".into(),
                            label: "Message".into(),
                            r#type: "string".into(),
                            required: false,
                            default: Some(serde_json::json!("manual run")),
                        },
                    ],
                },
            }],
        )
        .unwrap();
    let values = [("destination".into(), serde_json::json!(path))]
        .into_iter()
        .collect::<crate::macro_inputs::InputValues>();
    assert!(runtime
        .run_saved_macro(Some(device), "missing", values.clone())
        .is_err());
    assert!(runtime
        .run_saved_macro(Some("disconnected"), "manual", values.clone())
        .is_err());
    assert!(runtime
        .run_saved_macro(Some(device), "manual", Default::default())
        .is_err());
    assert!(!path.exists());
    runtime
        .run_saved_macro(Some(device), "manual", values)
        .unwrap();
    wait_until(|| std::fs::read_to_string(&path).is_ok_and(|text| text == "manual run"));
    let after = runtime.snapshot();
    assert_eq!(after.session(device).output.brightness, 43);
    assert_eq!(
        after.configuration_revision,
        snapshot.configuration_revision
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; runs with no device and writes only a temporary marker"]
// Verify that manual desktop macro runs without a device and device steps fail cleanly
fn manual_desktop_macro_runs_without_a_device_and_device_steps_fail_cleanly() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let path = root.path().join("desktop-only.txt");
    let snapshot = runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Save desktop-only macros",
            vec![
                Edit::SaveMacro {
                    value: crate::builtins::SavedMacro {
                        id: "desktop".into(),
                        name: "Desktop".into(),
                        action: Action::Macro {
                            actions: vec![Action::None, marker(&path)],
                        },
                        inputs: vec![],
                    },
                },
                Edit::SaveMacro {
                    value: crate::builtins::SavedMacro {
                        id: "navigation".into(),
                        name: "Navigation".into(),
                        action: Action::Page {
                            page_id: "main".into(),
                        },
                        inputs: vec![],
                    },
                },
            ],
        )
        .unwrap();
    assert!(snapshot.devices.is_empty());
    let events = runtime.subscribe();
    runtime
        .run_saved_macro(None, "desktop", Default::default())
        .unwrap();
    wait_until(|| std::fs::read(&path).is_ok_and(|bytes| bytes == b"executed"));
    runtime
        .run_saved_macro(None, "navigation", Default::default())
        .unwrap();
    let mut failure = None;
    wait_until(|| {
        if failure.is_none() {
            failure = events.try_iter().find(|event| event.kind == "action-error");
        }
        failure.is_some()
    });
    assert_eq!(
        failure.unwrap().message.as_deref(),
        Some("This macro step requires a connected device or simulator")
    );
    let after = runtime.snapshot();
    assert!(after.devices.is_empty());
    assert!(!after.sessions.contains_key(""));
    assert_eq!(
        after.configuration_revision,
        snapshot.configuration_revision
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; uses a disposable browser plugin with no device"]
// Verify that manual macro starts and reuses a plugin with no device
fn manual_macro_starts_and_reuses_a_plugin_with_no_device() {
    use serde_json::{json, Value};
    const PLUGIN: &str = "fixture.manual";
    const ACTION: &str = "fixture.manual.action";
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("plugins/fixture");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(package.join("manifest.json"), json!({
        "UUID": PLUGIN, "Name": "Manual fixture", "Version": "0.1.0", "CodePath": "runtime.html",
        "Actions": [{"UUID": ACTION, "Name": "Action"}]
    }).to_string()).unwrap();
    crate::test_plugin::modernize(&package);
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    runtime
        .plugins
        .lock()
        .unwrap()
        .enable(PLUGIN, true)
        .unwrap();
    let snapshot = runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Save plugin macro",
            vec![Edit::SaveMacro {
                value: crate::builtins::SavedMacro {
                    id: "plugin-run".into(),
                    name: "Plugin run".into(),
                    inputs: vec![],
                    action: Action::Macro {
                        actions: vec![Action::Plugin {
                            user_desired_state: None,
                            plugin_id: PLUGIN.into(),
                            action_id: ACTION.into(),
                            settings: json!({"message":"from the saved macro"}),
                            resources: Default::default(),
                        }],
                    },
                },
            }],
        )
        .unwrap();
    assert!(snapshot.devices.is_empty());
    assert!(runtime
        .plugins
        .lock()
        .unwrap()
        .browser_view(PLUGIN)
        .unwrap()
        .is_none());
    runtime
        .run_saved_macro(None, "plugin-run", Default::default())
        .unwrap();
    let mut view = None;
    wait_until(|| {
        let mut host = runtime.plugins.lock().unwrap();
        host.poll();
        view = host.browser_view(PLUGIN).unwrap();
        view.is_some()
    });
    let url = view.unwrap().url;
    let parts: Vec<_> = url.split('/').collect();
    let (mut socket, _) = tungstenite::connect(format!("ws://{}", parts[2])).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }
    socket
        .send(tungstenite::Message::Text(
            crate::test_plugin::registration(&package, parts[4])
                .to_string()
                .into(),
        ))
        .unwrap();
    wait_until(|| {
        let mut host = runtime.plugins.lock().unwrap();
        host.poll();
        host.list().iter().any(|plugin| {
            plugin.id == PLUGIN
                && plugin.runtime.state == stream_dock_manager_plugin_host::RuntimeState::Running
                && plugin.runtime.reason
                    == stream_dock_manager_plugin_host::RuntimeReason::ManualAction
        })
    });
    let mut read_event = |kind: &str| loop {
        let message = socket.read().unwrap();
        if let Ok(text) = message.to_text() {
            let event: Value = serde_json::from_str(text).unwrap();
            if event["event"] == kind {
                break event;
            }
        }
    };
    let appeared = read_event("willAppear");
    let pressed = read_event("keyDown");
    let released = read_event("keyUp");
    assert_eq!(appeared["device"], "");
    assert_eq!(pressed["action"], ACTION);
    assert_eq!(pressed["context"], appeared["context"]);
    assert_eq!(released["context"], appeared["context"]);
    assert_eq!(
        pressed["payload"]["settings"]["message"],
        "from the saved macro"
    );
    runtime
        .run_saved_macro(None, "plugin-run", Default::default())
        .unwrap();
    assert_eq!(read_event("keyDown")["context"], pressed["context"]);
    assert_eq!(
        runtime
            .plugins
            .lock()
            .unwrap()
            .browser_view(PLUGIN)
            .unwrap()
            .unwrap()
            .url,
        url
    );
    assert!(runtime.snapshot().devices.is_empty());
    socket.close(None).unwrap();
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; executes only a marker command in a temporary folder, never HID"]
// Verify that simulator runs saved commands and macros with disabled bindings respected
fn simulator_runs_saved_commands_and_macros_with_disabled_bindings_respected() {
    let root = tempfile::Builder::new()
        .prefix("stream-dock-manager O'Brien & $marker% ")
        .tempdir()
        .unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    runtime.simulate_device(true, "simulator:execute").unwrap();
    let output = root.path().join("executed.txt");
    save_action(
        &runtime,
        Action::Macro {
            actions: vec![Action::Brightness { value: 42 }, marker(&output)],
        },
    );
    runtime
        .simulate_input(
            "simulator:execute",
            ControlEvent {
                control_id: "key-1".into(),
                input: Input::Pulse,
            },
        )
        .unwrap();
    wait_until(|| std::fs::read(&output).is_ok_and(|bytes| bytes == b"executed"));
    assert_eq!(
        runtime
            .snapshot()
            .session("simulator:execute")
            .output
            .brightness,
        42
    );
    wait_until(|| {
        crate::feedback::text(
            &runtime,
            "macro",
            "simulator:execute",
            "default:main",
            "key-1",
        ) == "Completed"
    });
    let blocked = root.path().join("must-not-execute.txt");
    let state = runtime.snapshot();
    let mut profile = state.profiles[0].clone();
    let binding = profile.pages[0].bindings.get_mut("key-2").unwrap();
    binding.action = marker(&blocked);
    binding.disabled = true;
    runtime
        .save_profile(profile, state.configuration_revision)
        .unwrap();
    runtime
        .simulate_input(
            "simulator:execute",
            ControlEvent {
                control_id: "key-2".into(),
                input: Input::Pulse,
            },
        )
        .unwrap();
    assert_eq!(
        crate::feedback::text(
            &runtime,
            "macro",
            "simulator:execute",
            "default:main",
            "key-2"
        ),
        "Idle"
    );
    assert!(!blocked.exists());
    assert!(runtime
        .simulate_input(
            "not-a-simulator",
            ControlEvent {
                control_id: "key-1".into(),
                input: Input::Pulse
            }
        )
        .is_err());
    assert!(runtime
        .simulate_input(
            "simulator:execute",
            ControlEvent {
                control_id: "missing".into(),
                input: Input::Pulse
            }
        )
        .is_err());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; cancelled marker commands never execute, never HID"]
// Verify that disconnect and reconnect cancel pending simulator actions
fn disconnect_and_reconnect_cancel_pending_simulator_actions() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    runtime.simulate_device(true, "simulator:cancel").unwrap();
    let output = root.path().join("must-not-execute.txt");
    save_action(
        &runtime,
        Action::Macro {
            actions: vec![Action::Delay { milliseconds: 300 }, marker(&output)],
        },
    );
    runtime
        .simulate_input(
            "simulator:cancel",
            ControlEvent {
                control_id: "key-1".into(),
                input: Input::Pulse,
            },
        )
        .unwrap();
    wait_until(|| {
        crate::feedback::text(
            &runtime,
            "macro",
            "simulator:cancel",
            "default:main",
            "key-1",
        ) == "Running"
    });
    let epoch = runtime.connection_epochs.lock().unwrap()["simulator:cancel"];
    runtime.simulate_device(false, "simulator:cancel").unwrap();
    runtime.simulate_device(true, "simulator:cancel").unwrap();
    assert_ne!(
        runtime.connection_epochs.lock().unwrap()["simulator:cancel"],
        epoch
    );
    wait_until(|| {
        crate::feedback::text(
            &runtime,
            "macro",
            "simulator:cancel",
            "default:main",
            "key-1",
        ) == "Cancelled"
    });
    std::thread::sleep(Duration::from_millis(350));
    assert!(!output.exists());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; never executes host actions or opens HID"]
// Verify that removing a saved simulator purges its records and preserves other content
fn removing_a_saved_simulator_purges_its_records_and_preserves_other_content() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    for id in ["simulator:a", "simulator:ab"] {
        runtime.simulate_device(true, id).unwrap();
        runtime.brightness(id, 23).unwrap();
        let key = serde_json::to_string(&(id, "default", "main", "key-1")).unwrap();
        runtime
            .save_draft(
                &key,
                Some(Draft {
                    key: key.clone(),
                    base: None,
                    value: serde_json::json!({"title":id}),
                }),
            )
            .unwrap();
    }
    let state = runtime.snapshot();
    runtime
        .apply_edits(
            state.configuration_revision,
            "Simulator rules",
            vec![Edit::Rules {
                rules: ["simulator:a", "simulator:ab"]
                    .iter()
                    .map(|id| AppRule {
                        id: (*id).into(),
                        device_key: (*id).into(),
                        application: "editor".into(),
                        title: None,
                        schedule: None,
                        profile_id: "default".into(),
                        page_id: None,
                    })
                    .collect(),
            }],
        )
        .unwrap();
    let profiles = serde_json::to_value(&runtime.snapshot().profiles).unwrap();
    runtime.simulate_device(false, "simulator:a").unwrap();
    let result = runtime.remove_simulator("simulator:a").unwrap();
    assert!(!result.sessions.contains_key("simulator:a"));
    assert_eq!(result.session("simulator:ab").output.brightness, 23);
    assert_eq!(result.rules.len(), 1);
    assert_eq!(result.rules[0].device_key, "simulator:ab");
    assert_eq!(serde_json::to_value(&result.profiles).unwrap(), profiles);
    assert!(runtime
        .connection_epochs
        .lock()
        .unwrap()
        .get("simulator:a")
        .is_none());
    let drafts = runtime.drafts.lock().unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(
        drafts.values().next().unwrap().value["title"],
        "simulator:ab"
    );
    drop(drafts);
    let saved_sessions: serde_json::Value = runtime
        .profiles
        .local(crate::profile_database::LocalPreference::Sessions);
    assert!(saved_sessions.get("simulator:a").is_none());
    let saved_drafts: serde_json::Value = runtime
        .profiles
        .local(crate::profile_database::LocalPreference::Drafts);
    assert_eq!(saved_drafts.as_object().unwrap().len(), 1);
    let key = serde_json::to_string(&["simulator:a", "default", "main", "key-1"]).unwrap();
    assert!(runtime
        .save_draft(
            &key,
            Some(Draft {
                key: key.clone(),
                base: None,
                value: serde_json::json!({})
            })
        )
        .is_err());
    let restored = runtime.simulate_device(true, "simulator:a").unwrap();
    assert_eq!(restored.session("simulator:a").output.brightness, 50);
    assert_eq!(runtime.profiles.load().unwrap().rules.len(), 1);
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; never executes host actions or opens HID"]
// Verify that failed simulator removal preserves device and saved settings
fn failed_simulator_removal_preserves_device_and_saved_settings() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    runtime.simulate_device(true, "simulator:keep").unwrap();
    runtime.brightness("simulator:keep", 37).unwrap();
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(runtime.remove_simulator("simulator:keep").is_err());
    assert_eq!(runtime.snapshot().devices.len(), 1);
    assert_eq!(
        runtime
            .snapshot()
            .session("simulator:keep")
            .output
            .brightness,
        37
    );
    assert!(runtime
        .connection_epochs
        .lock()
        .unwrap()
        .contains_key("simulator:keep"));
    let saved: serde_json::Value = runtime
        .profiles
        .local(crate::profile_database::LocalPreference::Sessions);
    assert_eq!(saved["simulator:keep"]["output"]["brightness"], 37);
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; uses simulator-only layouts and never opens HID"]
// Verify that every model has a persistent editable profile and native preview
fn every_model_has_a_persistent_editable_profile_and_native_preview() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let manifests = hardware_registry::manifests();
    let mut expected_profiles = std::collections::BTreeMap::new();
    for manifest in &manifests {
        let id = format!("simulator:{}:test", manifest.model_id);
        let snapshot = runtime.simulate_device(true, &id).unwrap();
        let device = snapshot.devices.iter().find(|d| d.id == id).unwrap();
        assert_eq!(device.manifest.controls.len(), manifest.controls.len());
        let session = snapshot.session(&id);
        let profile = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap();
        assert_eq!(profile.model_id, manifest.model_id);
        assert!(expected_profiles
            .insert(manifest.model_id.clone(), profile.id.clone())
            .is_none());
        profile.validate().unwrap();
        runtime.preview_surfaces(Some(&id)).unwrap();
        let mut profile = profile.clone();
        profile.name = format!("Edited {}", manifest.name);
        runtime
            .save_profile(profile, snapshot.configuration_revision)
            .unwrap();
        runtime.simulate_device(false, &id).unwrap();
        let snapshot = runtime.simulate_device(true, &id).unwrap();
        assert_eq!(snapshot.session(&id).profile_id, session.profile_id);
    }
    let persisted = runtime.profiles.load().unwrap().profiles;
    assert_eq!(persisted.len(), manifests.len());
    let actual_profiles: std::collections::BTreeMap<_, _> = persisted
        .iter()
        .map(|profile| (profile.model_id.clone(), profile.id.clone()))
        .collect();
    assert_eq!(actual_profiles, expected_profiles);
    assert!(persisted
        .iter()
        .all(|profile| profile.name.starts_with("Edited ")));
    assert!(runtime
        .simulate_device(true, "simulator:unknown:test")
        .is_err());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; never executes host actions or opens HID"]
// Verify that sleep survives reconnect and restart and blocks preview input until woken
fn sleep_survives_reconnect_and_restart_and_blocks_preview_input_until_woken() {
    let root = tempfile::tempdir().unwrap();
    let id = "simulator:n4-pro:sleep";
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let snapshot = runtime.simulate_device(true, id).unwrap();
    let mut output = snapshot.session(id).output;
    output.brightness = 37;
    output.sleeping = true;
    runtime.set_device_output(id, output.clone()).unwrap();
    runtime.simulate_device(false, id).unwrap();
    assert!(
        runtime
            .simulate_device(true, id)
            .unwrap()
            .session(id)
            .output
            .sleeping
    );
    let stopped = std::sync::Arc::downgrade(&runtime);
    runtime.shutdown();
    drop(runtime);
    wait_until(|| stopped.strong_count() == 0);
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let snapshot = runtime.simulate_device(true, id).unwrap();
    assert_eq!(snapshot.session(id).output.brightness, 37);
    assert!(snapshot.session(id).output.sleeping);
    let event = ControlEvent {
        control_id: "key-1".into(),
        input: Input::Pulse,
    };
    assert!(runtime.trigger_input(id, event.clone()).is_err());
    output.sleeping = false;
    runtime.set_device_output(id, output).unwrap();
    save_action(&runtime, Action::Brightness { value: 41 });
    runtime.trigger_input(id, event).unwrap();
    wait_until(|| runtime.snapshot().session(id).output.brightness == 41);
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; never opens HID"]
// Verify that device sleep changes preserve output target one device and publish new revisions
fn device_sleep_changes_preserve_output_target_one_device_and_publish_new_revisions() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let a = "simulator:n4-pro:tray-a";
    let b = "simulator:n4-pro:tray-b";
    runtime.simulate_device(true, a).unwrap();
    let snapshot = runtime.simulate_device(true, b).unwrap();
    let mut output = snapshot.session(a).output;
    output.brightness = 37;
    output.lighting_brightness = 28;
    output.lighting_colors = vec![[12, 34, 56]; 4];
    let initial = runtime.set_device_output(a, output.clone()).unwrap();
    let events = runtime.subscribe();
    let asleep = runtime.set_device_sleep(a, true).unwrap();
    assert!(asleep.revision > initial.revision);
    assert_eq!(
        asleep.configuration_revision,
        initial.configuration_revision
    );
    assert!(asleep.session(a).output.sleeping);
    assert!(!asleep.session(b).output.sleeping);
    assert_eq!(asleep.session(a).output.brightness, 37);
    assert_eq!(asleep.session(a).output.lighting_brightness, 28);
    assert_eq!(
        asleep.session(a).output.lighting_colors,
        output.lighting_colors
    );
    let event = events.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(event.kind, "device-output");
    assert_eq!(event.device_id.as_deref(), Some(a));
    assert_eq!(event.revision, asleep.revision);
    // Repeated/stale Sleep clicks request sleep, rather than toggling back to wake
    assert!(
        runtime
            .set_device_sleep(a, true)
            .unwrap()
            .session(a)
            .output
            .sleeping
    );
    runtime.brightness(a, 48).unwrap();
    let awake = runtime.set_device_sleep(a, false).unwrap();
    assert!(!awake.session(a).output.sleeping);
    assert_eq!(awake.session(a).output.brightness, 48);
    assert_eq!(
        awake.session(a).output.lighting_colors,
        output.lighting_colors
    );
    let sessions: std::collections::BTreeMap<String, crate::workspace::DeviceSession> = runtime
        .profiles
        .read_preference(crate::profile_database::LocalPreference::Sessions)
        .unwrap()
        .unwrap();
    assert!(!sessions[a].output.sleeping);
    assert_eq!(sessions[a].output.brightness, 48);
    {
        let mut state = runtime.state.lock().unwrap();
        state
            .devices
            .iter_mut()
            .find(|device| device.id == a)
            .unwrap()
            .manifest
            .capabilities
            .sleep = false;
    }
    assert!(runtime
        .set_device_sleep(a, true)
        .unwrap_err()
        .contains("does not support sleep"));
    assert!(!runtime.snapshot().session(a).output.sleeping);
    runtime.simulate_device(false, a).unwrap();
    assert!(runtime
        .set_device_sleep(a, false)
        .unwrap_err()
        .contains("not connected"));
    assert!(!runtime.snapshot().session(b).output.sleeping);
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; never opens HID"]
// Verify that failed simulator creation does not add a device profile or connection epoch
fn failed_simulator_creation_does_not_add_a_device_profile_or_connection_epoch() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(runtime
        .simulate_device(true, "simulator:m3:failed")
        .is_err());
    assert!(runtime.snapshot().devices.is_empty());
    assert_eq!(runtime.profiles.load().unwrap().profiles.len(), 1);
    assert!(!runtime
        .connection_epochs
        .lock()
        .unwrap()
        .contains_key("simulator:m3:failed"));
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets; never opens HID"]
// Verify that swipe navigation is directional device local and restores profile pages
fn swipe_navigation_is_directional_device_local_and_restores_profile_pages() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let a = "simulator:swipe-a";
    let b = "simulator:swipe-b";
    runtime.simulate_device(true, a).unwrap();
    runtime.simulate_device(true, b).unwrap();
    let initial = runtime.snapshot();
    let mut first = initial.profiles[0].clone();
    let mut strip = first.pages[0].bindings["key-1"].clone();
    strip.action = Action::SwipeNavigation {
        target: "page".into(),
        wrap: true,
        reverse: false,
    };
    first.pages[0].bindings.insert("touch-strip".into(), strip);
    let mut extra = first.pages[0].clone();
    extra.id = "extra".into();
    first.pages.push(extra);
    let saved = runtime
        .save_profile(first.clone(), initial.configuration_revision)
        .unwrap();
    let mut incompatible = first.clone();
    incompatible.id = "other-model".into();
    // Use a registered model and empty bindings so the profile remains portable
    incompatible.model_id = hardware_registry::manifests()
        .iter()
        .find(|m| m.model_id != first.model_id)
        .unwrap()
        .model_id
        .clone();
    for page in &mut incompatible.pages {
        page.bindings.clear();
    }
    let saved = runtime
        .save_profile(incompatible, saved.configuration_revision)
        .unwrap();
    let mut second = first.clone();
    second.id = "scene-two".into();
    let saved = runtime
        .save_profile(second, saved.configuration_revision)
        .unwrap();
    runtime
        .select_device_profile(a, &first.id, Some("main"), true)
        .unwrap();
    runtime
        .select_device_profile(b, &first.id, Some("main"), true)
        .unwrap();
    let send = |input| {
        runtime
            .simulate_input(
                a,
                ControlEvent {
                    control_id: "touch-strip".into(),
                    input,
                },
            )
            .unwrap()
    };
    send(Input::Touch {
        phase: "end".into(),
        x: 500,
        y: 0,
    });
    send(Input::Swipe {
        direction: "up".into(),
    });
    wait_until(|| runtime.snapshot().session(a).profile_id == "scene-two");
    assert_eq!(runtime.snapshot().session(b).profile_id, first.id);
    assert_eq!(runtime.snapshot().session(a).page_id, "main");
    send(Input::Swipe {
        direction: "down".into(),
    });
    wait_until(|| runtime.snapshot().session(a).profile_id == first.id);
    send(Input::Swipe {
        direction: "left".into(),
    });
    wait_until(|| runtime.snapshot().session(a).page_id == "extra");
    assert_eq!(runtime.snapshot().session(b).page_id, "main");
    send(Input::Swipe {
        direction: "left".into(),
    });
    wait_until(|| runtime.snapshot().session(a).page_id == "main");
    send(Input::Swipe {
        direction: "right".into(),
    });
    wait_until(|| runtime.snapshot().session(a).page_id == "extra");
    // Saved custom navigation no longer owns the reserved gesture
    let revision = runtime.snapshot().configuration_revision;
    send(Input::Swipe {
        direction: "left".into(),
    });
    wait_until(|| runtime.snapshot().session(a).page_id == "main");
    assert_eq!(runtime.snapshot().session(a).profile_id, "default");
    assert_eq!(runtime.snapshot().session(b).profile_id, "default");
    assert_eq!(runtime.snapshot().configuration_revision, revision);
    assert!(revision >= saved.configuration_revision);
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets; never opens HID"]
fn saved_strip_events_override_defaults_and_restore_device_local_pages() {
    use crate::actions::{
        EventSlot, NavigationDirection as Direction, NavigationTarget as Target, DEFAULT_HOLD_MS,
        DEFAULT_MULTI_PRESS_MS,
    };

    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let device = "simulator:swipe-events";
    runtime.simulate_device(true, device).unwrap();
    let snapshot = runtime.snapshot();
    let mut first = snapshot.profiles[0].clone();
    let mut strip = first.pages[0].bindings["key-1"].clone();
    let mut actions = vec![Action::None; EventSlot::COUNT];
    actions[EventSlot::SwipeRight.index()] = Action::Navigate {
        target: Target::Page,
        direction: Direction::Previous,
    };
    actions[EventSlot::SwipeUp.index()] = Action::Navigate {
        target: Target::Profile,
        direction: Direction::Next,
    };
    strip.action = Action::Events {
        actions,
        multi_press_ms: DEFAULT_MULTI_PRESS_MS,
        hold_ms: DEFAULT_HOLD_MS,
    };
    first.pages[0].bindings.insert("touch-strip".into(), strip);
    let mut extra = first.pages[0].clone();
    extra.id = "extra".into();
    first.pages.push(extra);
    let mut second = first.clone();
    second.id = "second".into();
    let saved = runtime
        .save_profile(first.clone(), snapshot.configuration_revision)
        .unwrap();
    runtime
        .save_profile(second.clone(), saved.configuration_revision)
        .unwrap();
    runtime
        .select_device_profile(device, &second.id, Some("extra"), true)
        .unwrap();
    runtime
        .select_device_profile(device, &first.id, Some("main"), true)
        .unwrap();

    let swipe = |direction: &str| {
        runtime
            .simulate_input(
                device,
                ControlEvent {
                    control_id: "touch-strip".into(),
                    input: Input::Swipe {
                        direction: direction.into(),
                    },
                },
            )
            .unwrap();
    };
    // Left has no action, even though the implicit default would advance the page.
    swipe("left");
    assert_eq!(runtime.snapshot().session(device).page_id, "main");
    swipe("right");
    wait_until(|| runtime.snapshot().session(device).page_id == "extra");
    swipe("up");
    wait_until(|| runtime.snapshot().session(device).profile_id == second.id);
    assert_eq!(runtime.snapshot().session(device).page_id, "extra");
    runtime.shutdown();
}

#[test]
#[ignore = "Requires local loopback sockets for isolated plugin host; never opens HID"]
// Verify that experimental output policy is device local persisted and explicitly unqualified
fn experimental_output_policy_is_device_local_persisted_and_explicitly_unqualified() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let first = "simulator:frame-policy-a";
    let second = "simulator:frame-policy-b";
    runtime.simulate_device(true, first).unwrap();
    runtime.simulate_device(true, second).unwrap();
    let mut output = runtime.snapshot().session(first).output;
    output.frame_policy = Some(crate::workspace::DeviceFramePolicy {
        policy: hardware_core::OutputPolicy {
            mode: hardware_core::OutputMode::FrameScheduling,
            ..Default::default()
        },
        encoding: hardware_core::FrameEncoding::GroupedKeyUpdates,
    });
    runtime.set_device_output(first, output.clone()).unwrap();
    assert!(runtime
        .snapshot()
        .session(second)
        .output
        .frame_policy
        .is_none());
    runtime.simulate_device(false, first).unwrap();
    let restored = runtime.simulate_device(true, first).unwrap();
    assert_eq!(
        restored.session(first).output.frame_policy,
        output.frame_policy
    );
    let persisted: std::collections::BTreeMap<String, crate::workspace::DeviceSession> = runtime
        .profiles
        .read_preference(crate::profile_database::LocalPreference::Sessions)
        .unwrap()
        .unwrap();
    assert_eq!(persisted[first].output.frame_policy, output.frame_policy);
    assert!(!runtime
        .profiles
        .read_preference::<serde_json::Value>(crate::profile_database::LocalPreference::Sessions)
        .unwrap()
        .unwrap()
        .to_string()
        .contains("acknowledge_unqualified"));
    // One-switch rollback leaves unrelated brightness and per-device state intact
    output.frame_policy = None;
    runtime.set_device_output(first, output).unwrap();
    assert!(runtime
        .snapshot()
        .session(first)
        .output
        .frame_policy
        .is_none());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets; never executes host actions or opens HID"]
// Verify that failed draft writes restore only the affected entry
fn failed_draft_writes_restore_only_the_affected_entry() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let draft = |key: &str, title: &str| Draft {
        key: key.into(),
        base: None,
        value: serde_json::json!({"title":title}),
    };
    runtime
        .save_draft("one", Some(draft("one", "Original")))
        .unwrap();
    runtime
        .save_draft("two", Some(draft("two", "Untouched")))
        .unwrap();
    let original: serde_json::Value = runtime
        .profiles
        .read_preference(crate::profile_database::LocalPreference::Drafts)
        .unwrap()
        .unwrap();
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|error| error.to_string())
        })
        .unwrap();
    for (key, candidate) in [
        ("one", Some(draft("one", "Replacement"))),
        ("one", None),
        ("new", Some(draft("new", "Created"))),
    ] {
        assert!(runtime.save_draft(key, candidate).is_err());
        let retained = runtime.drafts.lock().unwrap();
        assert_eq!(retained.len(), 2);
        assert_eq!(retained["one"].value["title"], "Original");
        assert_eq!(retained["two"].value["title"], "Untouched");
    }
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=OFF")
                .map_err(|error| error.to_string())
        })
        .unwrap();
    assert_eq!(
        runtime
            .profiles
            .read_preference::<serde_json::Value>(crate::profile_database::LocalPreference::Drafts)
            .unwrap()
            .unwrap(),
        original
    );
    runtime.save_draft("one", None).unwrap();
    let persisted: std::collections::BTreeMap<String, Draft> = runtime
        .profiles
        .local(crate::profile_database::LocalPreference::Drafts);
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted["two"].value["title"], "Untouched");
    runtime.shutdown();
}

#[test]
// Verify that explicit palette binding roundtrips and covers display surfaces
fn explicit_palette_binding_roundtrips_and_covers_display_surfaces() {
    use crate::profiles::{BackgroundSource, Binding};
    let legacy = serde_json::json!({
        "title": "", "color": "#00000000", "action": {"kind": "none"}
    });
    let mut binding: Binding = serde_json::from_value(legacy.clone()).unwrap();
    assert!(binding.background_source.is_none());
    assert!(!binding.has_content());
    assert!(serde_json::to_value(&binding)
        .unwrap()
        .get("background_source")
        .is_none());
    binding.background_source = Some(BackgroundSource::Palette);
    assert!(binding.has_content());
    let saved = serde_json::to_value(&binding).unwrap();
    assert_eq!(saved["background_source"], "palette");
    assert_eq!(
        serde_json::from_value::<Binding>(saved)
            .unwrap()
            .background_source,
        Some(BackgroundSource::Palette)
    );
    let mut invalid = legacy;
    invalid["background_source"] = serde_json::json!("invalid");
    assert!(serde_json::from_value::<Binding>(invalid).is_err());
    let theme = stream_dock_manager_plugin_host::DeviceThemePreferences {
        sync: false,
        transparent_buttons: true,
    };
    let manifests = hardware_registry::manifests();
    let mut surface_count = 0;
    for control in manifests.iter().flat_map(|model| &model.controls) {
        let themed = Runtime::themed_button_binding(control, &binding, theme, "#123456", "#ffffff");
        if control.image_width.is_some() && control.image_height.is_some() {
            surface_count += 1;
            let themed = themed.unwrap();
            assert_eq!(themed.color, "#123456");
            assert!(themed.has_content());
        } else {
            assert!(themed.is_none());
        }
    }
    assert!(surface_count > 0);
    let preset: crate::workspace::AppearancePreset = serde_json::from_value(serde_json::json!({
        "id": "palette", "name": "Palette", "appearance": {},
        "background_source": "palette"
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(preset).unwrap()["background_source"],
        "palette"
    );
}
