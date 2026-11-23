use super::*;

#[test]
// Verify sleep is default persistent and unions visible and configuration demand
fn sleep_is_default_persistent_and_unions_visible_and_configuration_demand() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("plugin"), "fixture.sleep");
    let mut host = host(root.path());
    let required = HashMap::from([(
        "fixture.sleep".into(),
        vec![PluginDemand {
            device_id: "a".into(),
            profile_id: "profile".into(),
        }],
    )]);
    let action = |device: &str| ActiveAction {
        plugin_id: "fixture.sleep".into(),
        action_id: "fixture.sleep.action".into(),
        device_id: device.into(),
        control_id: "key".into(),
        scope: "profile:page".into(),
        settings: json!({}),
        surface: None,
        column: 0,
        row: 0,
        encoder: false,
    };
    host.enable("fixture.sleep", true).unwrap();
    host.reconcile_visible_demand(required.clone(), &[]);
    assert!(host.list()[0].sleep_when_hidden);
    assert!(!host.eligible("fixture.sleep"));
    host.set_sleep_when_hidden("fixture.sleep", false).unwrap();
    assert!(
        host.eligible("fixture.sleep"),
        "explicit opt-out preserves off-page work"
    );
    let loaded = Hub::load(host.storage()).unwrap();
    assert_eq!(loaded.sleep_when_hidden.get("fixture.sleep"), Some(&false));
    assert!(!loaded.sleeps_when_hidden("fixture.sleep"));
    host.set_sleep_when_hidden("fixture.sleep", true).unwrap();
    assert!(!host.eligible("fixture.sleep"));
    assert_eq!(
        host.plugins["fixture.sleep"].info.runtime.reason,
        RuntimeReason::NoVisibleActions
    );
    assert!(host.plugins["fixture.sleep"].info.enabled);
    host.reconcile_visible_demand(required.clone(), &[action("a"), action("b")]);
    assert!(host.eligible("fixture.sleep"));
    host.reconcile_visible_demand(required.clone(), &[action("b")]);
    assert!(
        host.eligible("fixture.sleep"),
        "another device keeps it awake"
    );
    host.reconcile_visible_demand(required.clone(), &[]);
    let panel = host
        .settings_panel("fixture.sleep", Some("fixture.sleep.action"), &json!({}))
        .unwrap();
    assert!(
        host.eligible("fixture.sleep"),
        "configuration wakes a sleeping plugin"
    );
    host.close_inspector(&panel.token);
    host.reconcile_visible_demand(required, &[]);
    assert!(!host.eligible("fixture.sleep"));
    assert_eq!(host.plugins["fixture.sleep"].restarts, 0);
    let loaded = Hub::load(host.storage()).unwrap();
    assert_eq!(loaded.sleep_when_hidden.get("fixture.sleep"), Some(&true));
    host.forget_plugin("fixture.sleep").unwrap();
    assert!(!Hub::load(host.storage())
        .unwrap()
        .sleep_when_hidden
        .contains_key("fixture.sleep"));
}

#[test]
fn installation_rows_keep_explicit_sleep_overrides_and_default_new_plugins_to_sleep() {
    use stream_dock_manager_storage::{
        plugin_installations::{self, State},
        Storage,
    };
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::open(root.path().join("state")).unwrap();
    let token = uuid::Uuid::new_v4().to_string();
    storage
        .transaction({
            let token = token.clone();
            move |tx| {
                plugin_installations::put(
                    tx,
                    "fixture.awake",
                    &State {
                        enabled: true,
                        sleep_when_hidden: Some(false),
                        installation_uuid: Some(token),
                        provenance: Some(json!({"source":"synthetic"})),
                        ..Default::default()
                    },
                )?;
                plugin_installations::put(
                    tx,
                    "fixture.sleep",
                    &State {
                        sleep_when_hidden: Some(true),
                        retained_name: Some("Fixture Sleep".into()),
                        ..Default::default()
                    },
                )
            }
        })
        .unwrap();
    let loaded = Hub::load(&storage).unwrap();
    assert!(loaded.enabled.contains("fixture.awake"));
    assert_eq!(loaded.installations.get("fixture.awake"), Some(&token));
    assert_eq!(
        loaded.provenance["fixture.awake"],
        json!({"source":"synthetic"})
    );
    assert_eq!(loaded.retained["fixture.sleep"], "Fixture Sleep");
    assert!(!loaded.sleeps_when_hidden("fixture.awake"));
    assert!(loaded.sleeps_when_hidden("fixture.sleep"));
    assert!(loaded.sleeps_when_hidden("fixture.new"));
    loaded.persist().unwrap();
    assert_eq!(
        Hub::load(&storage).unwrap().sleep_when_hidden,
        loaded.sleep_when_hidden
    );
}

#[test]
fn malformed_saved_installation_state_is_not_silently_reset() {
    use stream_dock_manager_storage::{
        plugin_installations::{self, State},
        Storage,
    };
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::open(root.path().join("state")).unwrap();
    storage.transaction(|tx| {
        assert!(plugin_installations::put(tx, "../invalid", &State { enabled: true, ..Default::default() }).is_err());
        assert!(plugin_installations::put(tx, "fixture.plugin", &State {
            installation_uuid: Some("invalid".into()), ..Default::default()
        }).is_err());
        tx.execute("INSERT INTO plugin_installation_state (plugin_id,enabled,installation_uuid) VALUES('fixture.plugin',1,'invalid')", [])
            .map_err(|error| error.to_string())?;
        Ok(())
    }).unwrap();
    assert!(Hub::load(&storage).is_err());
}

// Construct the package representation
pub(super) fn package(path: &Path, id: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(
        path.join("runtime.html"),
        "<!doctype html><title>Fixture</title>",
    )
    .unwrap();
    std::fs::write(
        path.join("inspector.html"),
        "<!doctype html><title>Inspector</title>",
    )
    .unwrap();
    let mut manifest = json!({
        "UUID":id,"Name":id,"Version":"1.0","SDKVersion":1,
        "CodePath":"runtime.html","StreamDockManager":{"ManifestDialect":"vsd"},
        "Actions":[{"UUID":format!("{id}.action"),"Name":"Action","Controllers":["Keypad"],"PropertyInspectorPath":"inspector.html"}]
    });
    current_descriptor(path, &mut manifest);
    read_plugin(path).unwrap();
}

/// Keep synthetic packages on the same explicit descriptor contract as producers
pub(super) fn current_descriptor(path: &Path, manifest: &mut Value) {
    use base64::Engine;
    if !path.join("icon.svg").exists() {
        std::fs::write(path.join("icon.svg"), "<svg/>").unwrap();
    }
    let asset = json!({"light":"icon.svg","dark":"icon.svg"});
    let actions: serde_json::Map<String, Value> = manifest["Actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| {
            let mut capability = json!({"control_types":["button"],"icon":asset,"states":[asset]});
            if let Some(settings) = action["PropertyInspectorPath"].as_str() {
                capability["settings"] = json!(settings);
            }
            (action["UUID"].as_str().unwrap().to_owned(), capability)
        })
        .collect();
    let mut capabilities = json!({"version":1,"id":manifest["UUID"],"plugin_version":manifest["Version"],"theme_protocol":1,"settings_protocol":1,"input_protocol":0,"icon":asset,"actions":actions});
    if let Some(scopes) = manifest["StreamDockManager"].get("Permissions") {
        capabilities["permissions"] = scopes.clone();
    }
    manifest["StreamDockManager"]["Capabilities"] = capabilities.clone();
    let encoded = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&capabilities).unwrap());
    let frame = format!(
        "<!-- {} SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1 -->",
        manifest["Version"].as_str().unwrap(),
        encoded.len()
    );
    std::fs::write(path.join(manifest["CodePath"].as_str().unwrap()), frame).unwrap();
    std::fs::write(
        path.join("manifest.json"),
        serde_json::to_vec(manifest).unwrap(),
    )
    .unwrap();
}

// Exercise file ownership, persistence and token revocation without sockets or executables
pub(super) fn host(root: &Path) -> PluginHost {
    std::fs::create_dir_all(root).unwrap();
    let root = root.canonicalize().unwrap();
    let storage = stream_dock_manager_storage::Storage::open(root.join(".configuration")).unwrap();
    let logs = log_capture::LogCaptureService::with_storage(storage.clone()).unwrap();
    let mut host = PluginHost {
        recovery: crate::recovery::Recovery::open(storage.clone()).unwrap(),
        transport_threads: vec![],
        listener_wake: None,
        exit_registry: None,
        logs,
        runtimes: stream_dock_manager_plugin_runtimes::RuntimeService::open_shared(
            // TempDir roots are siblings: their parent is shared by parallel
            // tests, so keep runtime settings inside this fixture's root.
            root.join(".runtimes"),
            storage.clone(),
        )
        .unwrap(),
        startup: Default::default(),
        visible_plugins: Default::default(),
        starting: Default::default(),
        next_start: std::time::Instant::now(),
        vault_locked: false,
        vault_setup_waiting: Default::default(),
        hub: Arc::new(Mutex::new(Hub::load(&storage).unwrap())),
        root,
        plugins: HashMap::new(),
        stop: Arc::new(AtomicBool::new(false)),
        port: 0,
        theme_revision: 0,
        required: Default::default(),
        replacement_claims: Default::default(),
        last_health_flush: std::time::Instant::now(),
    };
    host.scan(true).unwrap();
    host.set_registration_environment(RegistrationEnvironment::new(Some("en"), Some(1.0)).unwrap());
    host
}

// Start the isolated plugin socket host
pub(super) fn socket_host(root: &Path) -> PluginHost {
    let storage = stream_dock_manager_storage::Storage::open(root.join(".configuration")).unwrap();
    let runtimes = stream_dock_manager_plugin_runtimes::RuntimeService::open_shared(
        root.join(".runtimes"),
        storage.clone(),
    )
    .unwrap();
    let host = PluginHost::with_storage(root.into(), runtimes, storage).unwrap();
    host.set_registration_environment(RegistrationEnvironment::new(Some("en"), Some(1.0)).unwrap());
    host
}

// Construct demand for the lifecycle test
pub(super) fn demand(host: &mut PluginHost, id: &str) {
    // This helper models a visible action, not hidden-page background demand
    host.visible_plugins.insert(id.into());
    host.reconcile_demand(HashMap::from([(
        id.into(),
        vec![PluginDemand {
            device_id: "device".into(),
            profile_id: "default".into(),
        }],
    )]));
    host.poll();
}

#[test]
// Verify health snapshot excludes live runtime measurements and tracks real issues
fn health_snapshot_excludes_live_runtime_measurements_and_tracks_real_issues() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("plugin"), "fixture.health");
    let mut host = host(root.path());
    let before = host.health_snapshot().remove(0).health;
    let plugin = host.plugins.get_mut("fixture.health").unwrap();
    plugin.started = Some(std::time::Instant::now() - Duration::from_secs(10));
    plugin.info.runtime.resources.resident_bytes = Some(10_000_000);
    plugin.info.runtime.network.sent_bytes = Some(1234);
    assert_eq!(host.health_snapshot().remove(0).health, before);
    host.hub
        .lock()
        .unwrap()
        .issue("fixture.health", "failed_start", "Registration timed out");
    let after = host.health_snapshot().remove(0).health;
    assert_ne!(after, before);
    assert_eq!(
        after.events.last().unwrap().message,
        "Registration timed out"
    );
}

#[test]
// Verify initial key artwork survives declared encoder layout without plugin input
fn initial_key_artwork_survives_declared_encoder_layout_without_plugin_input() {
    {
        let root = tempfile::tempdir().unwrap();
        let package_root = root.path().join("plugin");
        package(&package_root, "fixture.initial");
        std::fs::write(
            package_root.join("icon.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><circle cx="16" cy="16" r="10" fill="red"/></svg>"#,
        )
        .unwrap();
        std::fs::write(
            package_root.join("layout.json"),
            json!({"items":[{"key":"value","type":"text","rect":[0,0,200,100]}]}).to_string(),
        )
        .unwrap();
        std::fs::write(
            package_root.join("manifest.json"),
            json!({
                "UUID":"fixture.initial", "Name":"Initial artwork", "Version":"0.1.0",
                "CodePath":"runtime.html",
                "Actions":[{"UUID":"fixture.initial.action", "Name":"Action",
                    "States":[{"Image":"icon.svg"}], "Encoder":{"layout":"layout.json"}}]
            })
            .to_string(),
        )
        .unwrap();
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(package_root.join("manifest.json")).unwrap())
                .unwrap();
        manifest["SDKVersion"] = json!(1);
        manifest["StreamDockManager"] = json!({"ManifestDialect":"vsd"});
        current_descriptor(&package_root, &mut manifest);
        let mut host = host(root.path());
        host.enable("fixture.initial", true).unwrap();
        let plugin = host.plugins.get_mut("fixture.initial").unwrap();
        plugin.info.runtime.state = RuntimeState::Running;
        {
            let capabilities = Capabilities {
                actions: std::collections::BTreeMap::from([(
                    "fixture.initial.action".into(),
                    ActionCapability {
                        feedback_layout: Some("layout.json".into()),
                        icon: ThemedAsset {
                            light: "icon.svg".into(),
                            dark: "icon.svg".into(),
                        },
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            };
            plugin.info.capabilities = Some(capabilities.clone());
            host.hub
                .lock()
                .unwrap()
                .capabilities
                .insert("fixture.initial".into(), capabilities);
        }
        let action = |control: &str, encoder| ActiveAction {
            plugin_id: "fixture.initial".into(),
            action_id: "fixture.initial.action".into(),
            device_id: "device".into(),
            control_id: control.into(),
            scope: "profile:page".into(),
            settings: json!({}),
            surface: None,
            column: 0,
            row: 0,
            encoder,
        };
        host.synchronize(vec![action("key-1", false), action("dial-1", true)], vec![]);
        let initial = host.surface("device", "key-1", "profile:page").unwrap();
        let pixels = hardware_core::render::visual(&initial, 64, 64, 0).unwrap();
        let pixels = image::load_from_memory(&pixels).unwrap().to_rgba8();
        assert_eq!(
            pixels.get_pixel(32, 32).0,
            [255, 0, 0, 255],
            "the initial state icon must reach the LCD before any plugin message or key press"
        );
        let encoder = host.surface("device", "dial-1", "profile:page").unwrap();
        assert!(
            encoder["layout"]["items"].is_array(),
            "encoder feedback still receives its declared layout"
        );
        host.shutdown();
    }
}

#[test]
// Verify fixture runtime preferences do not leak to sibling hosts
fn fixture_runtime_preferences_do_not_leak_to_sibling_hosts() {
    let root = tempfile::tempdir().unwrap();
    let first_root = root.path().join("first");
    let second_root = root.path().join("second");
    let mut first = socket_host(&first_root);
    first.runtimes.automatic(false).unwrap();
    let mut second = socket_host(&second_root);
    assert!(second.runtimes.status().automatic);
    assert!(!first.runtimes.status().automatic);
    first.shutdown();
    second.shutdown();
    drop(first);
    drop(second);
    let mut reopened = socket_host(&first_root);
    assert!(!reopened.runtimes.status().automatic);
    reopened.shutdown();
}

#[test]
// Verify reload adds new packages disabled and keeps running plugins and inspectors
fn reload_adds_new_packages_disabled_and_keeps_running_plugins_and_inspectors() {
    let root = tempfile::tempdir().unwrap();
    let existing = root.path().join("existing");
    package(&existing, "fixture.running");
    let mut host = host(root.path());
    host.enable("fixture.running", true).unwrap();
    demand(&mut host, "fixture.running");
    host.settings_panel(
        "fixture.running",
        Some("fixture.running.action"),
        &json!({"keep": true}),
    )
    .unwrap();
    let view = host.browser_view("fixture.running").unwrap().unwrap().url;
    let mut labels = host.view_labels("fixture.running").unwrap();
    labels.sort();
    host.plugins.get_mut("fixture.running").unwrap().restarts = 2;
    package(&existing, "fixture.running");
    package(&root.path().join("duplicate-existing"), "fixture.running");
    package(&root.path().join("new-a"), "fixture.new");
    package(&root.path().join("new-b-duplicate"), "fixture.new");
    package(&root.path().join(".install-hidden"), "fixture.hidden");
    let incomplete = root.path().join("incomplete");
    package(&incomplete, "fixture.incomplete");
    std::fs::remove_file(incomplete.join("runtime.html")).unwrap();
    std::fs::write(
        root.path().join("not-a-package.zip"),
        "Not an unpacked package",
    )
    .unwrap();
    std::fs::create_dir(root.path().join("invalid")).unwrap();
    std::fs::write(root.path().join("invalid/manifest.json"), "broken").unwrap();
    {
        let mut hub = host.hub.lock().unwrap();
        hub.enabled.insert("fixture.new".into());
        hub.persist().unwrap();
    }
    assert_eq!(host.reload().unwrap().added, 1);
    assert_eq!(host.list().len(), 2);
    assert!(host.plugins["fixture.running"].info.enabled);
    assert_eq!(host.plugins["fixture.running"].restarts, 2);
    assert_eq!(
        host.browser_view("fixture.running").unwrap().unwrap().url,
        view
    );
    let mut after = host.view_labels("fixture.running").unwrap();
    after.sort();
    assert_eq!(after, labels);
    assert!(!host.plugins["fixture.new"].info.enabled);
    assert!(host.view_labels("fixture.new").unwrap().is_empty());
    assert!(!host.hub.lock().unwrap().enabled.contains("fixture.new"));
    let preferences = Hub::load(host.storage()).unwrap();
    assert!(preferences.enabled.contains("fixture.running"));
    assert!(!preferences.enabled.contains("fixture.new"));
    assert_eq!(host.reload().unwrap().added, 0);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(incomplete.join("manifest.json")).unwrap()).unwrap();
    current_descriptor(&incomplete, &mut manifest);
    assert_eq!(host.reload().unwrap().added, 1);
    assert!(!host.plugins["fixture.incomplete"].info.enabled);
    assert_eq!(host.reload().unwrap().added, 0);
}

#[test]
// Verify failed reload does not publish new packages or change existing preferences
fn failed_reload_does_not_publish_new_packages_or_change_existing_preferences() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("existing"), "fixture.running");
    let mut host = host(root.path());
    host.enable("fixture.running", true).unwrap();
    demand(&mut host, "fixture.running");
    package(&root.path().join("new"), "fixture.new");
    host.hub
        .lock()
        .unwrap()
        .enabled
        .insert("fixture.new".into());
    host.storage()
        .run(|conn| {
            conn.execute_batch("PRAGMA query_only=ON")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(host.reload().is_err());
    assert_eq!(host.list().len(), 1);
    assert!(host.list()[0].enabled);
    let hub = host.hub.lock().unwrap();
    assert!(hub.enabled.contains("fixture.new"));
    assert!(!hub.roots.contains_key("fixture.new"));
}

#[test]
// Verify reload removes confirmed absence recovery preferences and tokens but retains other plugins
fn reload_removes_confirmed_absence_recovery_preferences_and_tokens_but_retains_other_plugins() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("gone"), "fixture.gone");
    package(&root.path().join("keep"), "fixture.keep");
    let mut host = host(root.path());
    let panel = host
        .settings_panel(
            "fixture.gone",
            Some("fixture.gone.action"),
            &json!({"draft":true}),
        )
        .unwrap();
    {
        let mut hub = host.hub.lock().unwrap();
        hub.enabled.insert("fixture.gone".into());
        hub.globals
            .insert("fixture.gone".into(), json!({"remove":true}));
        hub.globals
            .insert("fixture.keep".into(), json!({"keep":true}));
        hub.appearance.insert("fixture.gone".into(), "dark".into());
        hub.context_state.settings.insert(
            "fixture.gone:profile:device:key-1".into(),
            json!({"remove":true}),
        );
        hub.persist().unwrap();
    }
    std::fs::remove_dir_all(root.path().join("gone")).unwrap();
    let result = host.reload().unwrap();
    assert_eq!(result.removed, vec!["fixture.gone"]);
    assert_eq!(result.added, 0);
    assert!(host.panel_settings(&panel.token).is_err());
    assert_eq!(host.list().len(), 1);
    let saved = Hub::load(host.storage()).unwrap();
    assert!(!saved.globals.contains_key("fixture.gone"));
    assert!(!saved.appearance.contains_key("fixture.gone"));
    assert!(saved.context_state.settings.is_empty());
    assert_eq!(saved.globals["fixture.keep"]["keep"], true);
    assert!(host.reload().unwrap().removed.is_empty());
    package(&root.path().join("reinstalled"), "fixture.gone");
    assert_eq!(host.reload().unwrap().added, 1);
    assert!(!host.plugins["fixture.gone"].info.enabled);
}

#[test]
// Verify reload preserves renames duplicates and ambiguous invalid inventory
fn reload_preserves_renames_duplicates_and_ambiguous_invalid_inventory() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("original"), "fixture.renamed");
    let mut host = host(root.path());
    host.hub
        .lock()
        .unwrap()
        .globals
        .insert("fixture.renamed".into(), json!({"keep":true}));
    std::fs::rename(root.path().join("original"), root.path().join("renamed")).unwrap();
    assert!(host.reload().unwrap().removed.is_empty());
    assert!(host.plugins["fixture.renamed"].path.ends_with("renamed"));
    package(&root.path().join("duplicate"), "fixture.renamed");
    std::fs::remove_dir_all(root.path().join("renamed")).unwrap();
    assert!(host.reload().unwrap().removed.is_empty());
    assert!(host.plugins["fixture.renamed"].path.ends_with("duplicate"));
    std::fs::remove_dir_all(root.path().join("duplicate")).unwrap();
    std::fs::create_dir(root.path().join("invalid-package")).unwrap();
    let result = host.reload().unwrap();
    assert!(!result.cleanup_safe);
    assert!(result.removed.is_empty());
    assert_eq!(
        host.hub.lock().unwrap().globals["fixture.renamed"]["keep"],
        true
    );
    std::fs::remove_dir(root.path().join("invalid-package")).unwrap();
    assert_eq!(host.reload().unwrap().removed, vec!["fixture.renamed"]);
}

#[test]
// Verify deletion removes enabled or disabled packages settings and grants and allows reinstall
fn deletion_removes_enabled_or_disabled_packages_settings_and_grants_and_allows_reinstall() {
    for enabled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fixture.delete.sdPlugin");
        package(&source, "fixture.delete");
        let mut host = host(&root.path().join("plugins"));
        host.install(&source).unwrap();
        let other = host.root.join("other");
        package(&other, "fixture.delete.extra");
        host.scan(true).unwrap();
        let context = host.hub.lock().unwrap().binding_context(
            "fixture.delete",
            "default:main",
            "device",
            "key-1",
        );
        let other_context = "fixture.delete.extra:default:main:device:key-1";
        {
            let mut hub = host.hub.lock().unwrap();
            hub.context_state
                .settings
                .insert(context.clone(), json!({"saved": true}));
            hub.context_state
                .settings
                .insert(other_context.into(), json!({"keep": true}));
            hub.action_defaults.insert(
                "fixture.delete".into(),
                HashMap::from([("fixture.delete.action".into(), json!({"latitude":1}))]),
            );
            hub.action_defaults.insert(
                "fixture.delete.extra".into(),
                HashMap::from([("fixture.delete.extra.action".into(), json!({"keep":true}))]),
            );
            hub.globals
                .insert("fixture.delete".into(), json!({"secret": true}));
            hub.globals
                .insert("fixture.delete.extra".into(), json!({"keep": true}));
            hub.persist().unwrap();
        }
        if enabled {
            host.enable("fixture.delete", true).unwrap();
            demand(&mut host, "fixture.delete");
            host.synchronize(
                vec![ActiveAction {
                    plugin_id: "fixture.delete".into(),
                    action_id: "fixture.delete.action".into(),
                    device_id: "device".into(),
                    control_id: "key-1".into(),
                    scope: "default:main".into(),
                    settings: json!({"saved": true}),
                    surface: None,
                    column: 0,
                    row: 0,
                    encoder: false,
                }],
                vec![],
            );
            host.settings_panel("fixture.delete", Some("fixture.delete.action"), &json!({}))
                .unwrap();
            assert_eq!(host.view_labels("fixture.delete").unwrap().len(), 2);
            let mut hub = host.hub.lock().unwrap();
            hub.visual_state.badges.insert(
                context.clone(),
                std::time::Instant::now() + Duration::from_secs(60),
            );
            for (event, context) in [
                ("setTitle", context.as_str()),
                ("setSettings", context.as_str()),
                ("setTitle", other_context),
            ] {
                let effect = hub.message_effect(
                    "fixture.delete",
                    None,
                    &json!({"event":event,"context":context}),
                    None,
                );
                hub.effects.push_back(effect);
            }
        }
        let path = host.plugins["fixture.delete"].path.clone();
        host.delete("fixture.delete").unwrap();
        assert!(!path.exists());
        assert!(source.join("manifest.json").is_file());
        assert!(other.join("manifest.json").is_file());
        assert_eq!(host.list().len(), 1);
        assert_eq!(host.list()[0].id, "fixture.delete.extra");
        assert!(host.delete("../source.sdPlugin").is_err());
        {
            let hub = host.hub.lock().unwrap();
            assert!(!hub
                .sessions
                .tokens
                .values()
                .any(|owner| owner == "fixture.delete"));
            assert!(!hub
                .context_state
                .contexts
                .values()
                .any(|owner| owner == "fixture.delete"));
            assert!(hub.sessions.files.is_empty());
            assert!(!hub.sessions.queues.keys().any(|key| key.starts_with("pi:")));
            assert!(!hub.roots.contains_key("fixture.delete"));
            assert!(!hub.visual_state.badges.contains_key(&context));
            assert!(!hub
                .effects
                .iter()
                .any(|effect| effect.context() == Some(context.as_str())));
        }
        let saved = Hub::load(host.storage()).unwrap();
        assert!(!saved.action_defaults.contains_key("fixture.delete"));
        assert_eq!(
            saved.action_defaults["fixture.delete.extra"]["fixture.delete.extra.action"]["keep"],
            true
        );
        assert!(!saved.globals.contains_key("fixture.delete"));
        assert!(saved.context_state.settings.is_empty());
        assert_eq!(saved.globals["fixture.delete.extra"]["keep"], true);
        assert!(!saved.enabled.contains("fixture.delete"));
        let installed = host.install(&source).unwrap();
        assert!(!installed.enabled);
        assert!(host.view_labels(&installed.id).unwrap().is_empty());
    }
}

#[test]
// Verify deletion keeps package and running state when preferences cannot be saved
fn deletion_keeps_package_and_running_state_when_preferences_cannot_be_saved() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.delete");
    let mut host = host(root.path());
    host.enable("fixture.delete", true).unwrap();
    demand(&mut host, "fixture.delete");
    let views = host.view_labels("fixture.delete").unwrap();
    host.storage()
        .run(|conn| {
            conn.execute_batch("PRAGMA query_only=ON")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(host.delete("fixture.delete").is_err());
    assert!(host.plugins["fixture.delete"]
        .path
        .join("manifest.json")
        .is_file());
    assert!(host.list()[0].enabled);
    assert_eq!(host.view_labels("fixture.delete").unwrap(), views);
}

#[cfg(unix)]
#[test]
// Verify deletion rejects linked packages without touching the source
fn deletion_rejects_linked_packages_without_touching_the_source() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    package(&source, "fixture.delete");
    let plugins = root.path().join("plugins");
    std::fs::create_dir(&plugins).unwrap();
    std::os::unix::fs::symlink(&source, plugins.join("linked")).unwrap();
    let mut host = host(&plugins);
    assert!(host
        .delete("fixture.delete")
        .unwrap_err()
        .contains("symlink"));
    assert!(source.join("manifest.json").is_file());
    assert!(plugins.join("linked").exists());
}

#[test]
// Verify rejected release retires transport and rejected pulses are atomic
fn rejected_release_retires_transport_and_rejected_pulses_are_atomic() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.input");
    let mut host = host(root.path());
    host.enable("fixture.input", true).unwrap();
    demand(&mut host, "fixture.input");
    {
        let mut hub = host.hub.lock().unwrap();
        let queue = hub
            .sessions
            .queues
            .entry("fixture.input".into())
            .or_default();
        queue.clear();
        for _ in 0..transport::OUTGOING_MESSAGES - 2 {
            queue.push_back(json!({"event":"keyDown"}));
        }
    }
    // A fresh pulse needs appearance, press and release: two free slots do not
    // admit a partial operation or leave the fresh context marked as appeared.
    assert!(host
        .dispatch(
            "fixture.input",
            "fixture.input.action",
            "device",
            "key-1",
            "default:main",
            &hardware_core::Input::Pulse,
            &json!({})
        )
        .is_err());
    {
        let mut hub = host.hub.lock().unwrap();
        assert!(!hub
            .context_state
            .contexts
            .contains_key("fixture.input:default:main:device:key-1"));
        for map in [&hub.context_state.scopes, &hub.context_state.contexts] {
            assert!(!map.contains_key("fixture.input:default:main:device:key-1"));
        }
        assert!(!hub
            .context_state
            .settings
            .contains_key("fixture.input:default:main:device:key-1"));
        assert!(!hub
            .context_state
            .targets
            .contains_key("fixture.input:default:main:device:key-1"));
        let queue = hub.sessions.queues.get_mut("fixture.input").unwrap();
        assert_eq!(queue.len(), transport::OUTGOING_MESSAGES - 2);
        queue.clear();
    }
    host.dispatch(
        "fixture.input",
        "fixture.input.action",
        "device",
        "key-1",
        "default:main",
        &hardware_core::Input::Button { pressed: true },
        &json!({}),
    )
    .unwrap();
    {
        let mut hub = host.hub.lock().unwrap();
        let queue = hub.sessions.queues.get_mut("fixture.input").unwrap();
        while queue.len() < transport::OUTGOING_MESSAGES {
            queue.push_back(json!({"event":"keyDown"}));
        }
    }
    assert!(host
        .dispatch(
            "fixture.input",
            "fixture.input.action",
            "device",
            "key-1",
            "default:main",
            &hardware_core::Input::Button { pressed: false },
            &json!({})
        )
        .is_err());
    assert!(host
        .hub
        .lock()
        .unwrap()
        .sessions
        .queues
        .get_mut("fixture.input")
        .unwrap()
        .take_batch()
        .is_err());
}

#[test]
// Verify tracked plugin actions require a correlated result from the owning session
fn tracked_plugin_actions_require_a_correlated_result_from_the_owning_session() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.icons");
    let mut host = host(root.path());
    host.enable("fixture.icons", true).unwrap();
    demand(&mut host, "fixture.icons");
    let dispatch = |host: &mut PluginHost| {
        host.dispatch_tracked(
            "fixture.icons",
            "fixture.icons.action",
            "device",
            "key-1",
            "default:main",
            &hardware_core::Input::Pulse,
            &json!({}),
        )
        .unwrap()
    };
    let receipt = dispatch(&mut host);
    let session = host.plugins["fixture.icons"].token.clone();
    let context = host
        .hub
        .lock()
        .unwrap()
        .lookup_binding_context("fixture.icons", "default:main", "device", "key-1")
        .unwrap();
    let message = json!({"event":"actionResult","context":context,"payload":{"request_id":receipt.id,"success":true}});
    {
        let mut hub = host.hub.lock().unwrap();
        let request = hub.sessions.queues["fixture.icons"]
            .iter()
            .find(|e| e["event"] == "keyDown")
            .unwrap();
        assert_eq!(request["streamDockManager"]["request_id"], receipt.id);
        // Enqueueing, another plugin, a retired session, and an unrelated context
        // are never treated as successful completion.
        assert!(matches!(
            receipt.receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        hub.complete_action("other", &session, &message);
        hub.complete_action("fixture.icons", "retired-session", &message);
        let mut wrong = message.clone();
        wrong["context"] = json!("fixture.icons:default:main:device:key-2");
        hub.complete_action("fixture.icons", &session, &wrong);
        assert!(matches!(
            receipt.receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        hub.complete_action("fixture.icons", &session, &message);
        hub.complete_action("fixture.icons", &session, &message); // Duplicate result is ignored.
    }
    receipt.wait(Duration::from_secs(1), || Ok(())).unwrap();
    let failed = dispatch(&mut host);
    let mut failure = message.clone();
    failure["payload"] = json!({"request_id":failed.id,"success":false});
    host.hub
        .lock()
        .unwrap()
        .complete_action("fixture.icons", &session, &failure);
    assert!(failed
        .wait(Duration::from_secs(1), || Ok(()))
        .unwrap_err()
        .contains("failed"));
    let cancelled = dispatch(&mut host);
    assert_eq!(
        cancelled
            .wait(Duration::from_secs(1), || Err("Cancelled".into()))
            .unwrap_err(),
        "Cancelled"
    );
    drop(cancelled);
    let expired = dispatch(&mut host);
    let id = expired.id.clone();
    assert!(expired
        .wait(Duration::from_millis(1), || Ok(()))
        .unwrap_err()
        .contains("did not report"));
    drop(expired);
    assert!(!host.hub.lock().unwrap().action_results.contains_key(&id));
    let late = dispatch(&mut host);
    let mut completion = message.clone();
    completion["payload"]["request_id"] = json!(late.id);
    {
        let mut hub = host.hub.lock().unwrap();
        hub.retire_context(&context);
        assert_eq!(
            validate_message(
                &json!({"event":"setTitle","context":context}),
                "fixture.icons",
                &hub.context_state.contexts
            ),
            Err(InboundError::UnownedContext),
        );
        // Completion owns its original correlation even after page retirement
        hub.complete_action("fixture.icons", &session, &completion);
    }
    late.wait(Duration::from_secs(1), || Ok(())).unwrap();
    let outstanding = dispatch(&mut host);
    host.enable("fixture.icons", false).unwrap();
    assert!(outstanding
        .wait(Duration::from_secs(1), || Ok(()))
        .unwrap_err()
        .contains("stopped"));
}

#[test]
// Verify missing runtime preserves enabled preferences without crash retries
fn missing_runtime_preserves_enabled_preferences_without_crash_retries() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("plugins");
    let path = root.join("org.test.runtime");
    package(&path, "org.test.runtime");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(path.join("manifest.json")).unwrap()).unwrap();
    manifest["CodePath"] = json!("main.mjs");
    manifest["StreamDockManager"]["Runtime"] =
        json!({"Kind":"nodejs","Version":">=22, <27","Channel":"lts"});
    current_descriptor(&path, &mut manifest);
    std::fs::write(
        path.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let mut host = host(&root);
    host.hub
        .lock()
        .unwrap()
        .enabled
        .insert("org.test.runtime".into());
    host.hub.lock().unwrap().persist().unwrap();
    host.plugins
        .get_mut("org.test.runtime")
        .unwrap()
        .info
        .enabled = true;
    demand(&mut host, "org.test.runtime");
    for _ in 0..5 {
        host.poll();
    }
    assert!(host
        .hub
        .lock()
        .unwrap()
        .enabled
        .contains("org.test.runtime"));
    assert_eq!(host.plugins["org.test.runtime"].restarts, 0);
    assert!(host.plugins["org.test.runtime"]
        .info
        .status
        .contains("Blocked"));
    assert!(host.runtime_usage()[0].enabled_preference);
}

#[test]
// Verify preference and profile demand are independent and stale starts are cancelled
fn preference_and_profile_demand_are_independent_and_stale_starts_are_cancelled() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.demand");
    let mut host = host(root.path());
    host.enable("fixture.demand", true).unwrap();
    for _ in 0..3 {
        host.poll();
    }
    assert_eq!(host.list()[0].runtime.state, RuntimeState::Idle);
    assert!(host.list()[0].enabled);
    assert!(host.browser_view("fixture.demand").unwrap().is_none());
    demand(&mut host, "fixture.demand");
    let token = host.plugins["fixture.demand"].token.clone();
    assert!(!token.is_empty());
    host.hub
        .lock()
        .unwrap()
        .sessions
        .connected
        .insert(token.clone());
    host.poll();
    assert_eq!(host.list()[0].runtime.state, RuntimeState::Running);
    let both = vec![
        PluginDemand {
            device_id: "a".into(),
            profile_id: "p".into(),
        },
        PluginDemand {
            device_id: "b".into(),
            profile_id: "p".into(),
        },
    ];
    host.reconcile_demand(HashMap::from([("fixture.demand".into(), both.clone())]));
    host.reconcile_demand(HashMap::from([("fixture.demand".into(), both[1..].into())]));
    assert_eq!(host.plugins["fixture.demand"].token, token);
    host.reconcile_demand(Default::default());
    assert!(host.list()[0].enabled);
    assert_eq!(host.list()[0].runtime.state, RuntimeState::Idle);
    assert!(host.browser_view("fixture.demand").unwrap().is_none());
    assert!(!host
        .hub
        .lock()
        .unwrap()
        .sessions
        .tokens
        .contains_key(&token));
    assert_eq!(host.list()[0].health.totals.crashes, 0);
    assert!(host.restart("fixture.demand").is_err());
    assert!(host.retry("fixture.demand").is_err());
    host.enable("fixture.demand", false).unwrap();
    demand(&mut host, "fixture.demand");
    assert_eq!(host.list()[0].runtime.state, RuntimeState::Disabled);
    host.enable("fixture.demand", true).unwrap();
    assert_eq!(host.list()[0].runtime.state, RuntimeState::Queued);
    host.reconcile_demand(Default::default());
    host.poll();
    assert!(host.plugins["fixture.demand"].token.is_empty());
}

#[test]
// Verify visible page prioritizes only queued starts and preserves startup limits
fn visible_page_prioritizes_only_queued_starts_and_preserves_startup_limits() {
    let root = tempfile::tempdir().unwrap();
    let ids = ["fixture.a", "fixture.b", "fixture.y", "fixture.z"];
    for id in ids {
        package(&root.path().join(id), id);
    }
    let mut host = host(root.path());
    for id in ids {
        host.enable(id, true).unwrap();
        // This test covers explicit background operation and startup ordering
        host.set_sleep_when_hidden(id, false).unwrap();
    }
    host.reconcile_demand(
        ids.into_iter()
            .map(|id| {
                (
                    id.into(),
                    vec![PluginDemand {
                        device_id: "device".into(),
                        profile_id: "default".into(),
                    }],
                )
            })
            .collect(),
    );
    let visible = |host: &mut PluginHost, id: &str| {
        host.synchronize(
            vec![ActiveAction {
                plugin_id: id.into(),
                action_id: format!("{id}.action"),
                device_id: "device".into(),
                control_id: "key-1".into(),
                scope: "default:main".into(),
                settings: json!({}),
                surface: None,
                column: 0,
                row: 0,
                encoder: false,
            }],
            vec![json!({"id":"device"})],
        );
    };
    visible(&mut host, "fixture.z");
    host.poll();
    let token = host.plugins["fixture.z"].token.clone();
    assert!(!token.is_empty());
    assert!(host.plugins["fixture.a"].token.is_empty());
    assert_eq!(host.starting.len(), 1);
    assert_eq!(
        host.next_start.duration_since(host.starting["fixture.z"]),
        Duration::from_millis(100)
    );

    let deadline = host.next_start;
    visible(&mut host, "fixture.y");
    assert_eq!(host.startup.front().map(String::as_str), Some("fixture.y"));
    assert_eq!(host.plugins["fixture.z"].token, token);
    assert_eq!(host.next_start, deadline);
    // Exercise the deadline guard without relying on the test machine's speed
    host.next_start = std::time::Instant::now() + Duration::from_secs(10);
    host.poll();
    assert_eq!(host.starting.len(), 1);
    host.next_start = std::time::Instant::now();
    host.poll();
    assert_eq!(host.starting.len(), 2);
    assert!(!host.plugins["fixture.y"].token.is_empty());

    visible(&mut host, "fixture.b");
    host.next_start = std::time::Instant::now();
    host.poll();
    assert_eq!(host.starting.len(), 2);
    assert!(host.plugins["fixture.b"].token.is_empty());
    host.hub
        .lock()
        .unwrap()
        .sessions
        .connected
        .insert(token.clone());
    host.poll();
    assert_eq!(host.starting.len(), 2);
    assert_eq!(
        host.plugins["fixture.z"].info.runtime.state,
        RuntimeState::Running
    );
    assert_eq!(host.plugins["fixture.z"].token, token);
    assert!(!host.plugins["fixture.b"].token.is_empty());
    assert_eq!(host.startup.front().map(String::as_str), Some("fixture.a"));
}

#[test]
// Verify registration failures back off three times then require explicit recovery
fn registration_failures_back_off_three_times_then_require_explicit_recovery() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.retry");
    let mut host = host(root.path());
    host.enable("fixture.retry", true).unwrap();
    demand(&mut host, "fixture.retry");
    for attempt in 0..4 {
        assert_eq!(host.list()[0].runtime.state, RuntimeState::Starting);
        host.starting.insert(
            "fixture.retry".into(),
            std::time::Instant::now() - Duration::from_secs(6),
        );
        host.poll();
        if attempt < 3 {
            assert_eq!(host.list()[0].runtime.state, RuntimeState::Retrying);
            let remaining = host.plugins["fixture.retry"]
                .next_restart
                .saturating_duration_since(std::time::Instant::now());
            assert!(
                remaining <= Duration::from_secs(2 << attempt)
                    && remaining > Duration::from_secs((2 << attempt) - 1)
            );
            host.plugins.get_mut("fixture.retry").unwrap().next_restart = std::time::Instant::now();
            host.next_start = std::time::Instant::now();
            host.poll();
        }
    }
    assert!(host.list()[0].enabled);
    assert_eq!(host.list()[0].runtime.state, RuntimeState::Blocked);
    assert_eq!(host.list()[0].health.totals.failed_starts, 4);
    assert_eq!(host.list()[0].health.totals.automatic_restarts, 3);
    host.reconcile_demand(Default::default());
    demand(&mut host, "fixture.retry");
    assert_eq!(host.list()[0].runtime.state, RuntimeState::Blocked);
    host.retry("fixture.retry").unwrap();
    assert_eq!(host.plugins["fixture.retry"].restarts, 0);
    host.shutdown();
    let health = Hub::load(host.storage())
        .unwrap()
        .health
        .remove("fixture.retry")
        .unwrap();
    assert_eq!(health.totals.failed_starts, 4);
    assert_eq!(health.session.failed_starts, 0);
}

#[test]
// Verify resource snapshots warm up expire and recover without wall clock dependence
fn resource_snapshots_warm_up_expire_and_recover_without_wall_clock_dependence() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.metrics");
    let mut host = host(root.path());
    assert_eq!(
        host.runtime_snapshot()[0].runtime.resources.state,
        MeasurementState::NotRunning
    );
    host.enable("fixture.metrics", true).unwrap();
    demand(&mut host, "fixture.metrics");
    let browser = host.runtime_snapshot()[0].runtime.resources.clone();
    assert_eq!(browser.state, MeasurementState::Unavailable);
    assert_eq!(browser.source, MeasurementSource::SharedWebview);

    // Exercise native snapshot states without launching an OS-dependent executable
    let plugin = host.plugins.get_mut("fixture.metrics").unwrap();
    plugin.info.kind = "native".into();
    let started = plugin.started.unwrap();
    let run = plugin.info.runtime.run_id.clone().unwrap();
    assert_eq!(
        plugin.resources_at(started + Duration::from_secs(10)).state,
        MeasurementState::WarmingUp
    );
    assert_eq!(
        plugin
            .resources_at(started + Duration::from_millis(10_001))
            .state,
        MeasurementState::Stale
    );
    assert_eq!(
        host.runtime_snapshot()[0].runtime.resources.state,
        MeasurementState::WarmingUp
    );

    let sample = PluginResources {
        state: MeasurementState::Available,
        source: MeasurementSource::ProcessTreeRss,
        resident_bytes: Some(42),
        cpu_percent: Some(5.),
        process_count: Some(2),
        // A future wall timestamp must not prevent monotonic expiry
        sampled_at_ms: Some(u64::MAX),
        ..Default::default()
    };
    host.record_resources("fixture.metrics", &run, sample.clone());
    let plugin = &host.plugins["fixture.metrics"];
    let sampled = plugin.resource_sampled_at.unwrap();
    assert_eq!(
        plugin.resources_at(sampled + Duration::from_secs(10)).state,
        MeasurementState::Available
    );
    let stale = plugin.resources_at(sampled + Duration::from_millis(10_001));
    assert_eq!(stale.state, MeasurementState::Stale);
    assert_eq!(stale.cpu_percent, None);
    assert_eq!(stale.resident_bytes, None);
    assert_eq!(stale.process_count, None);
    assert_eq!(stale.peak_resident_bytes, 42);
    assert_eq!(stale.history.len(), 1);
    assert_eq!(stale.sampled_at_ms, Some(u64::MAX));

    // Both initial full snapshots and dedicated monitoring snapshots age readings
    host.plugins
        .get_mut("fixture.metrics")
        .unwrap()
        .resource_sampled_at = Some(std::time::Instant::now() - Duration::from_secs(11));
    assert_eq!(
        host.list()[0].runtime.resources.state,
        MeasurementState::Stale
    );
    assert_eq!(
        host.runtime_snapshot()[0].runtime.resources.state,
        MeasurementState::Stale
    );
    host.record_resources("fixture.metrics", "old-run", sample.clone());
    assert_eq!(
        host.runtime_snapshot()[0].runtime.resources.state,
        MeasurementState::Stale
    );
    host.record_resources("fixture.metrics", &run, sample);
    assert_eq!(
        host.runtime_snapshot()[0].runtime.resources.state,
        MeasurementState::Available
    );
    host.reconcile_demand(Default::default());
    assert_eq!(
        host.runtime_snapshot()[0].runtime.resources.state,
        MeasurementState::NotRunning
    );
    assert!(host.plugins["fixture.metrics"]
        .resource_sampled_at
        .is_none());
}

#[test]
// Verify resource history expires from snapshots even when sampling has stopped
fn resource_history_expires_from_snapshots_even_when_sampling_has_stopped() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.metrics");
    let mut host = host(root.path());
    let now = monitor::timestamp_ms();
    host.plugins
        .get_mut("fixture.metrics")
        .unwrap()
        .resource_history = VecDeque::from([
        ResourceSample {
            timestamp_ms: now - 301_000,
            cpu_percent: Some(50.),
            resident_bytes: Some(100),
        },
        ResourceSample {
            timestamp_ms: now,
            cpu_percent: Some(5.),
            resident_bytes: Some(10),
        },
    ]);
    for resources in [
        host.list()[0].runtime.resources.clone(),
        host.runtime_snapshot()[0].runtime.resources.clone(),
    ] {
        assert_eq!(resources.history.len(), 1);
        assert_eq!(resources.history[0].resident_bytes, Some(10));
    }
}

#[test]
// Verify resource updates are scoped to runs and history and health are bounded
fn resource_updates_are_scoped_to_runs_and_history_and_health_are_bounded() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.metrics");
    let mut host = host(root.path());
    host.enable("fixture.metrics", true).unwrap();
    demand(&mut host, "fixture.metrics");
    let token = host.plugins["fixture.metrics"].token.clone();
    let run = host.plugins["fixture.metrics"]
        .info
        .runtime
        .run_id
        .clone()
        .unwrap();
    assert_ne!(
        run, token,
        "monitoring identifiers must not expose authentication"
    );
    let sample = PluginResources {
        state: MeasurementState::Available,
        resident_bytes: Some(10),
        cpu_percent: Some(5.),
        ..Default::default()
    };
    host.record_resources("fixture.metrics", "old-run", sample.clone());
    assert_eq!(host.list()[0].runtime.resources.resident_bytes, None);
    for _ in 0..400 {
        host.record_resources("fixture.metrics", &run, sample.clone());
    }
    assert!(host.list()[0].runtime.resources.history.len() <= 301);
    {
        let mut hub = host.hub.lock().unwrap();
        hub.issue("fixture.metrics", "api", "same issue");
        hub.issue("fixture.metrics", "api", "same issue");
        assert_eq!(hub.health["fixture.metrics"].totals.issues, 1);
        assert_eq!(hub.health["fixture.metrics"].events[0].occurrences, 2);
        for i in 0..150 {
            hub.issue("fixture.metrics", "api", &format!("issue {i}"));
        }
        assert_eq!(hub.health["fixture.metrics"].events.len(), 100);
    }
    host.reconcile_demand(Default::default());
    assert_eq!(host.list()[0].runtime.resources.resident_bytes, None);
    assert!(host.list()[0].runtime.resources.history.is_empty());
    assert_eq!(host.list()[0].health.last_run_peak_resident_bytes, Some(10));
    for _ in 0..3 {
        host.runtime_snapshot();
    }
    assert_eq!(host.list()[0].health.totals.issues, 151);
    host.hub.lock().unwrap().flush_health().unwrap();
    let persisted = host
        .storage()
        .run(|conn| {
            let mut saved = stream_dock_manager_storage::plugin_health::read(conn)?;
            let health = monitor::from_stored_health(saved.remove("fixture.metrics").unwrap());
            assert_eq!(health.events.len(), 100);
            assert_eq!(health.totals.issues, 151);
            assert_eq!(health.last_run_peak_resident_bytes, Some(10));
            serde_json::to_string(&health).map_err(|err| err.to_string())
        })
        .unwrap();
    assert!(!persisted.contains(&token));
    assert!(persisted.contains(&run));
}

#[test]
// Verify disappearing contexts release visual and configuration indices but keep settings
fn disappearing_contexts_release_visual_and_configuration_indices_but_keep_settings() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.cleanup");
    let mut host = host(root.path());
    {
        let mut hub = host.hub.lock().unwrap();
        let id = hub.binding_context("fixture.cleanup", "profile", "device", "key");
        hub.context_state
            .contexts
            .insert(id.clone(), "fixture.cleanup".into());
        hub.context_state.visible.insert(
            id.clone(),
            json!({"event":"willAppear","context":id,"action":"action"}),
        );
        hub.visual_state
            .visuals
            .insert(id.clone(), Arc::new(json!({"image":"large cached image"})));
        hub.visual_state
            .visual_sources
            .insert(id.clone(), Default::default());
        hub.context_state
            .configured
            .insert(id.clone(), json!({"value":1}));
        hub.context_state
            .settings
            .insert(id.clone(), json!({"value":42}));
        hub.context_state
            .scopes
            .insert(id.clone(), "profile".into());
        hub.context_state
            .addresses
            .insert(id.clone(), json!({"address":"key"}));
    }
    host.synchronize(vec![], vec![]);
    let mut hub = host.hub.lock().unwrap();
    assert!(
        hub.visual_state.visuals.is_empty()
            && hub.visual_state.visual_sources.is_empty()
            && hub.context_state.configured.is_empty()
    );
    assert!(
        hub.context_state.contexts.is_empty()
            && hub.context_state.scopes.is_empty()
            && hub.context_state.addresses.is_empty()
    );
    assert!(
        hub.context_state.settings.is_empty(),
        "Retired incarnations must not accumulate settings"
    );
    let next = hub.binding_context("fixture.cleanup", "profile", "device", "key");
    assert_eq!(hub.context_state.settings[&next]["value"], 42);
}

#[test]
// Verify credential redaction cannot modify authenticated action result IDs
fn credential_redaction_cannot_modify_authenticated_action_result_ids() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.correlation");
    let mut host = socket_host(root.path());
    host.enable("fixture.correlation", true).unwrap();
    demand(&mut host, "fixture.correlation");
    let session = host.plugins["fixture.correlation"].token.clone();
    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{}", host.port)).unwrap();
    socket
        .send(tungstenite::Message::text(
            json!({"event":"registerPlugin","uuid":session,"payload":{"capabilities":host.plugins["fixture.correlation"].info.capabilities}}).to_string(),
        ))
        .unwrap();
    let start = std::time::Instant::now();
    while !host
        .hub
        .lock()
        .unwrap()
        .sessions
        .connected
        .contains(&session)
    {
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(5));
    }
    let receipt = host
        .dispatch_tracked(
            "fixture.correlation",
            "fixture.correlation.action",
            "device",
            "key-1",
            "default:main",
            &hardware_core::Input::Pulse,
            &json!({}),
        )
        .unwrap();
    host.hub.lock().unwrap().secret_redactions =
        "0123456789abcdef".chars().map(|c| c.to_string()).collect();
    let context = host
        .hub
        .lock()
        .unwrap()
        .lookup_binding_context("fixture.correlation", "default:main", "device", "key-1")
        .unwrap();
    let effects_before = host.hub.lock().unwrap().effects.len();
    socket.send(tungstenite::Message::text(json!({"event":"actionResult","context":context,"payload":{"request_id":receipt.id,"success":true,"ignored":"private-text"}}).to_string())).unwrap();
    receipt.wait(Duration::from_secs(2), || Ok(())).unwrap();
    assert_eq!(
        host.hub.lock().unwrap().effects.len(),
        effects_before,
        "Correlated action results must not escape as runtime effects"
    );
    host.shutdown();
}

#[test]
// Verify saved player replaces stale plugin cache on startup and profile switch
fn saved_player_replaces_stale_plugin_cache_on_startup_and_profile_switch() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.hiscores");
    let action = |scope: &str, player: &str| ActiveAction {
        plugin_id: "fixture.hiscores".into(),
        action_id: "fixture.hiscores.action".into(),
        device_id: "device".into(),
        control_id: "key".into(),
        scope: scope.into(),
        settings: json!({"username":player,"board":"normal","skill":"1"}),
        surface: None,
        column: 0,
        row: 0,
        encoder: false,
    };
    {
        let host = host(root.path());
        let mut hub = host.hub.lock().unwrap();
        for scope in ["profile:page", "other:page"] {
            hub.context_state.settings.insert(
                format!("fixture.hiscores:{scope}:device:key"),
                json!({"username":"","board":"normal","plugin_extension":true}),
            );
        }
        hub.persist().unwrap();
    }
    let mut host = host(root.path());
    host.enable("fixture.hiscores", true).unwrap();
    demand(&mut host, "fixture.hiscores");
    for (scope, player) in [("profile:page", "Echolocate"), ("other:page", "Other")] {
        host.synchronize(vec![action(scope, player)], vec![]);
        let hub = host.hub.lock().unwrap();
        let event = &hub.context_state.visible[&hub
            .lookup_binding_context("fixture.hiscores", scope, "device", "key")
            .unwrap()];
        assert_eq!(event["payload"]["settings"]["username"], player);
        assert!(
            event["payload"]["settings"]
                .get("plugin_extension")
                .is_none(),
            "old runtime projections cannot overwrite saved workspace settings"
        );
    }
    host.synchronize(vec![action("other:page", "Echolocate")], vec![]);
    let hub = host.hub.lock().unwrap();
    assert_eq!(
        hub.context_state.visible[&hub
            .lookup_binding_context("fixture.hiscores", "other:page", "device", "key")
            .unwrap()]["payload"]["settings"]["username"],
        "Echolocate"
    );
}

#[test]
// Verify changed settings replace cached startup payload for every action
fn changed_settings_replace_cached_startup_payload_for_every_action() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.settings");
    let mut host = host(root.path());
    host.enable("fixture.settings", true).unwrap();
    demand(&mut host, "fixture.settings");
    let action = |units: &str| ActiveAction {
        plugin_id: "fixture.settings".into(),
        action_id: "fixture.settings.action".into(),
        device_id: "device".into(),
        control_id: "key".into(),
        scope: "profile:page".into(),
        settings: json!({"units":units,"zero":0,"flag":false,"unknown":{"keep":true}}),
        surface: None,
        column: 0,
        row: 0,
        encoder: false,
    };
    host.synchronize(vec![action("celsius")], vec![]);
    host.synchronize(vec![action("fahrenheit")], vec![]);
    let hub = host.hub.lock().unwrap();
    let cached = &hub.context_state.visible[&hub
        .lookup_binding_context("fixture.settings", "profile:page", "device", "key")
        .unwrap()];
    assert_eq!(cached["event"], "willAppear");
    assert_eq!(cached["payload"]["settings"]["units"], "fahrenheit");
    assert_eq!(cached["payload"]["settings"]["zero"], 0);
    assert_eq!(cached["payload"]["settings"]["flag"], false);
}

#[test]
// Verify resolved setting changes update startup values without losing encoder metadata
fn resolved_setting_changes_update_startup_values_without_losing_encoder_metadata() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.parameters");
    let mut host = host(root.path());
    host.enable("fixture.parameters", true).unwrap();
    demand(&mut host, "fixture.parameters");
    let durable = json!({"units":"${parameter.units}","unknown":true});
    host.set_execution_settings(
        "fixture.parameters",
        "profile:page",
        "device",
        "dial",
        &durable,
        &json!({"units":"celsius","unknown":true}),
        vec![],
    );
    host.synchronize(
        vec![ActiveAction {
            plugin_id: "fixture.parameters".into(),
            action_id: "fixture.parameters.action".into(),
            device_id: "device".into(),
            control_id: "dial".into(),
            scope: "profile:page".into(),
            settings: durable.clone(),
            surface: None,
            column: 2,
            row: 1,
            encoder: true,
        }],
        vec![],
    );
    host.set_execution_settings(
        "fixture.parameters",
        "profile:page",
        "device",
        "dial",
        &durable,
        &json!({"units":"fahrenheit","unknown":true}),
        vec![],
    );
    let hub = host.hub.lock().unwrap();
    let cached = &hub.context_state.visible[&hub
        .lookup_binding_context("fixture.parameters", "profile:page", "device", "dial")
        .unwrap()];
    assert_eq!(cached["event"], "willAppear");
    assert_eq!(cached["payload"]["settings"]["units"], "fahrenheit");
    assert_eq!(cached["payload"]["controller"], "Encoder");
    assert_eq!(cached["payload"]["coordinates"]["column"], 2);
    assert_eq!(
        hub.context_state.settings[&hub
            .lookup_binding_context("fixture.parameters", "profile:page", "device", "dial")
            .unwrap()],
        durable
    );
}

#[test]
// Verify manager diagnostics record registration failures and retry without private text
fn manager_diagnostics_record_registration_failures_and_retry_without_private_text() {
    use crate::manager_diagnostics::take_events;
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("plugin"), "fixture.diagnostics");
    let mut host = host(root.path());
    take_events();
    host.enable("fixture.diagnostics", true).unwrap();
    demand(&mut host, "fixture.diagnostics");
    let run = host.plugins["fixture.diagnostics"]
        .info
        .runtime
        .run_id
        .clone()
        .unwrap();
    let token = host.plugins["fixture.diagnostics"].token.clone();
    host.hub
        .lock()
        .unwrap()
        .sessions
        .connected
        .insert(token.clone());
    host.poll();
    host.hub.lock().unwrap().sessions.connected.remove(&token);
    host.poll();
    host.startup_failure("fixture.diagnostics", "Permission denied: Bearer private-token https://user:password@example.org /Users/private/config");
    let records = take_events()
        .into_iter()
        .map(|entry| serde_json::from_str::<Value>(&entry).unwrap())
        .collect::<Vec<_>>();
    assert!(records
        .iter()
        .any(|r| r["event"] == "starting" && r["run_id"] == run && r["version"] == "1.0"));
    assert!(records
        .iter()
        .any(|r| r["event"] == "registered" && r["run_id"] == run));
    assert!(records
        .iter()
        .any(|r| r["event"] == "unexpected_exit" && r["reason"] == "connection_closed"));
    assert!(records
        .iter()
        .any(|r| r["event"] == "retry_scheduled" && r["attempt"] == 1));
    assert!(records
        .iter()
        .any(|r| r["event"] == "startup_failed" && r["reason"] == "permission_denied"));
    let text = serde_json::to_string(&records).unwrap();
    for private in [
        "private-token",
        "password",
        "/Users",
        "https:",
        "device",
        "default",
    ] {
        assert!(!text.contains(private));
    }
}

#[cfg(unix)]
#[test]
// Verify manager diagnostics preserve real process exit codes
fn manager_diagnostics_preserve_real_process_exit_codes() {
    use crate::manager_diagnostics::take_events;
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("plugin"), "fixture.exit");
    let mut host = host(root.path());
    host.enable("fixture.exit", true).unwrap();
    demand(&mut host, "fixture.exit");
    // Exercise the real child-exit poll with a disposable process, without a plugin SDK
    let child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 7"])
        .spawn()
        .unwrap();
    host.plugins.get_mut("fixture.exit").unwrap().child = Some(child);
    take_events();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut records = Vec::new();
    while std::time::Instant::now() < deadline {
        host.poll();
        records.extend(take_events());
        if records.iter().any(|r| r.contains("startup_failed")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let records = records
        .iter()
        .map(|r| serde_json::from_str::<Value>(r).unwrap())
        .collect::<Vec<_>>();
    assert!(records.iter().any(|r| r["event"] == "startup_failed"
        && r["exit_code"] == 7
        && r["reason"] == "process_exit"));
}

#[test]
// Verify maintained browser fixture prepares registers and opens managed settings
fn maintained_browser_fixture_prepares_registers_and_opens_managed_settings() {
    use tungstenite::Message;
    let root = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/plugins/browser");
    let mut host = socket_host(root.path());
    let installed = host.install(&source).unwrap();
    assert_eq!(installed.dialect, "vsd");
    assert_eq!(
        installed.capabilities.as_ref().unwrap().settings_protocol,
        1
    );
    host.enable(&installed.id, true).unwrap();
    demand(&mut host, &installed.id);
    let token = host.plugins[&installed.id].token.clone();
    let (mut socket, _) = tungstenite::connect(format!("ws://127.0.0.1:{}", host.port)).unwrap();
    socket.send(Message::text(json!({"event":"registerPlugin","uuid":token,"payload":{"capabilities":installed.capabilities}}).to_string())).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !host.hub.lock().unwrap().sessions.connected.contains(&token) {
        assert!(
            std::time::Instant::now() < deadline,
            "current browser descriptor registration rejected"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    host.poll();
    let panel = host
        .settings_panel(
            &installed.id,
            Some("app.streamdockmanager.plugins.fixture.browser.action"),
            &json!({"title":"Native close","unknown":42}),
        )
        .unwrap();
    assert!(panel.url.ends_with("/inspector.html"));
    assert_eq!(host.panel_settings(&panel.token).unwrap()["unknown"], 42);
    host.close_inspector(&panel.token);
    assert!(host.panel_settings(&panel.token).is_err());
    let _ = socket.close(None);
    host.shutdown();
}

#[test]
// Verify runtime removal waits for stopping plugins even after running metadata is cleared
fn runtime_removal_waits_for_stopping_plugins_even_after_running_metadata_is_cleared() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("plugin"), "fixture.runtime-removal");
    let mut host = host(root.path());
    let plugin = host.plugins.get_mut("fixture.runtime-removal").unwrap();
    plugin.info.kind = "nodejs".into();
    plugin.info.running_runtime = None;
    plugin.stopping = Some(Arc::new(AtomicBool::new(false)));
    assert!(host
        .remove_private_runtime("private-runtime")
        .unwrap_err()
        .contains("finish stopping"));
    host.plugins
        .get_mut("fixture.runtime-removal")
        .unwrap()
        .stopping = None;
    assert!(host
        .remove_private_runtime("private-runtime")
        .unwrap_err()
        .contains("Unknown private runtime installation"));
}

#[test]
// Verify named session revocation preserves draft grants and other plugins
fn named_session_revocation_preserves_draft_grants_and_other_plugins() {
    let mut hub = Hub::default();
    hub.sessions.connected.insert("old-run".into());
    for (token, owner, context) in [
        ("old-run", "plugin", None),
        ("inspector", "plugin", Some("visible")),
        ("draft-token", "plugin", Some("draft")),
        ("peer", "other", None),
    ] {
        hub.sessions.tokens.insert(token.into(), owner.into());
        hub.sessions.files.insert(
            token.into(),
            FileGrant {
                root: PathBuf::new(),
                runtime_entry: None,
                bootstrap: String::new(),
                context: context.map(str::to_owned),
                setup_runtime: false,
            },
        );
        hub.sessions
            .queues
            .entry(format!("pi:{token}"))
            .or_default();
    }
    hub.context_state
        .contexts
        .insert("visible".into(), "plugin".into());
    hub.context_state
        .contexts
        .insert("draft".into(), "plugin".into());
    hub.context_state.drafts.insert("draft".into());
    hub.context_state.visible.insert(
        "visible".into(),
        json!({"event":"willAppear","context":"visible"}),
    );
    hub.revoke_plugin_sessions("plugin", "old-run");
    let notices: Vec<_> = hub.sessions.shutdowns["old-run"]
        .iter()
        .map(|event| event["event"].as_str().unwrap())
        .collect();
    assert_eq!(notices, ["willDisappear", "shutdown"]);
    assert!(!hub.sessions.connected.contains("old-run"));
    assert!(!hub.sessions.tokens.contains_key("old-run"));
    assert!(!hub.sessions.files.contains_key("inspector"));
    assert!(!hub.sessions.queues.contains_key("pi:inspector"));
    assert!(hub.sessions.tokens.contains_key("draft-token"));
    assert!(hub.sessions.files.contains_key("draft-token"));
    assert!(hub.context_state.contexts.contains_key("draft"));
    assert!(hub.sessions.tokens.contains_key("peer"));
    assert!(hub.sessions.files.contains_key("peer"));
}

#[test]
// Verify dial lane scope retirement clears resources and held encoder state
fn dial_lane_scope_retirement_clears_resources_and_held_encoder_state() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.dial");
    let mut host = host(root.path());
    host.enable("fixture.dial", true).unwrap();
    demand(&mut host, "fixture.dial");
    let action = |index: usize| ActiveAction {
        plugin_id: "fixture.dial".into(),
        action_id: "fixture.dial.action".into(),
        device_id: "device".into(),
        control_id: "dial".into(),
        scope: format!("profile:page@{index}"),
        settings: json!({"entry":index}),
        surface: None,
        column: 0,
        row: 0,
        encoder: true,
    };
    host.synchronize(vec![action(0)], vec![]);
    let old = {
        let mut hub = host.hub.lock().unwrap();
        let context = hub
            .lookup_binding_context("fixture.dial", "profile:page@0", "device", "dial")
            .unwrap();
        hub.context_state.pressed_encoders.insert(context.clone());
        hub.context_state
            .resources
            .insert(context.clone(), json!({"file":"disposable-owned-resource"}));
        context
    };
    host.synchronize(vec![action(1)], vec![]);
    {
        let hub = host.hub.lock().unwrap();
        assert!(!hub.context_state.visible.contains_key(&old));
        assert!(!hub.context_state.resources.contains_key(&old));
        assert!(!hub.context_state.pressed_encoders.contains(&old));
        assert_eq!(hub.context_state.visible.len(), 1);
        let context = hub
            .lookup_binding_context("fixture.dial", "profile:page@1", "device", "dial")
            .unwrap();
        assert_eq!(
            hub.context_state.visible[&context]["payload"]["settings"]["entry"],
            1
        );
    }
    host.synchronize(vec![action(0)], vec![]);
    let hub = host.hub.lock().unwrap();
    let renewed = hub
        .lookup_binding_context("fixture.dial", "profile:page@0", "device", "dial")
        .unwrap();
    assert_ne!(
        renewed, old,
        "reactivation cannot reuse retired runtime ownership"
    );
    assert!(!hub.context_state.pressed_encoders.contains(&renewed));
    assert!(!hub.context_state.resources.contains_key(&renewed));
    assert_eq!(
        hub.context_state.visible[&renewed]["payload"]["settings"]["entry"],
        0
    );
}

#[test]
// Verify resource peaks and last start are run scoped and keep no payloads
fn resource_peaks_and_last_start_are_run_scoped_and_keep_no_payloads() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.metrics");
    let mut host = host(root.path());
    host.enable("fixture.metrics", true).unwrap();
    demand(&mut host, "fixture.metrics");
    let runtime = &host.plugins["fixture.metrics"].info.runtime;
    let run = runtime.run_id.clone().unwrap();
    assert_eq!(
        runtime.last_wake_reason,
        Some(RuntimeReason::RequiredByActiveProfile)
    );
    assert_eq!(runtime.last_wake_at_ms, runtime.started_at_ms);
    let wake = runtime.last_wake_at_ms;
    for cpu in [Some(0.), Some(25.), None, Some(10.), Some(f64::NAN)] {
        host.record_resources(
            "fixture.metrics",
            &run,
            PluginResources {
                state: MeasurementState::Available,
                cpu_percent: cpu,
                resident_bytes: Some(10),
                ..Default::default()
            },
        );
    }
    assert_eq!(host.list()[0].runtime.resources.peak_cpu_percent, Some(25.));
    assert_eq!(host.list()[0].runtime.resources.cpu_percent, None);
    host.enable("fixture.metrics", false).unwrap();
    let runtime = &host.list()[0].runtime;
    assert_eq!(runtime.last_wake_at_ms, wake);
    assert_eq!(
        runtime.last_wake_reason,
        Some(RuntimeReason::RequiredByActiveProfile)
    );
    assert_eq!(runtime.resources.peak_cpu_percent, None);
    assert!(runtime.resources.history.is_empty());
}

#[test]
// Verify stable registration does not replenish automatic restart budget
fn stable_registration_does_not_replenish_automatic_restart_budget() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("fixture"), "fixture.budget");
    let mut host = host(root.path());
    host.enable("fixture.budget", true).unwrap();
    demand(&mut host, "fixture.budget");
    let token = host.plugins["fixture.budget"].token.clone();
    host.hub.lock().unwrap().sessions.connected.insert(token);
    host.poll();
    let plugin = host.plugins.get_mut("fixture.budget").unwrap();
    plugin.restarts = 3;
    plugin.registered = Some(std::time::Instant::now() - Duration::from_secs(120));
    host.poll();
    assert_eq!(host.plugins["fixture.budget"].restarts, 3);
    host.shutdown();
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn observed_running_plugin_parks_but_preserves_housekeeping_and_fallback_deadlines() {
    let root = tempfile::tempdir().unwrap();
    package(&root.path().join("plugin"), "fixture.exit-watch");
    let mut host = host(root.path());
    let (registry, observer) = process_exit::Registry::new(Arc::new(|| {})).unwrap();
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    process_tree::configure(&mut command);
    let child = command.spawn().unwrap();
    let tree = process_tree::ProcessTree::attach(&child).unwrap();
    let plugin = host.plugins.get_mut("fixture.exit-watch").unwrap();
    plugin.info.runtime.state = RuntimeState::Running;
    plugin.exit_watch = registry.register(&child);
    assert!(plugin.exit_watch.is_some());
    plugin.child = Some(child);
    plugin.tree = Some(tree);
    host.hub.lock().unwrap().health_dirty.clear();
    assert!(host.next_deadline().is_none());
    let badge = std::time::Instant::now() + Duration::from_secs(5);
    host.hub
        .lock()
        .unwrap()
        .visual_state
        .badges
        .insert("fixture".into(), badge);
    assert_eq!(host.next_deadline(), Some(badge));
    host.hub.lock().unwrap().health_mut("fixture.exit-watch");
    assert_eq!(
        host.next_deadline(),
        Some(host.last_health_flush + Duration::from_secs(2))
    );
    registry.shutdown();
    observer.join().unwrap();
    assert!(host.next_deadline().unwrap() < std::time::Instant::now() + Duration::from_millis(150));
    let plugin = host.plugins.get_mut("fixture.exit-watch").unwrap();
    plugin.exit_watch = None;
    plugin.tree.take().unwrap().terminate();
    plugin.child.take().unwrap().wait().unwrap();
}
