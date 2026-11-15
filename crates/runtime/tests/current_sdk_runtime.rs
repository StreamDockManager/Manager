//! Explicit opt-in genuine SDK -> production Runtime persistence qualification
//! Uses disposable storage, a synthetic device and no hardware/desktop actors
#![cfg(feature = "test-support")]

#[path = "support/current_sdk_profiles.rs"]
mod bundled_profiles;
#[path = "../../plugin-host/examples/support/mod.rs"]
mod protocol;

use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{connection_contract, PluginHost, RegistrationEnvironment};
use stream_dock_manager_runtime::{actions::Action, test_support, Runtime};

const ID: &str = "com.manager.fixture.elgato";
const ACTION: &str = "com.manager.fixture.elgato.probe";
const DEVICE: &str = "simulator:n4-pro:sdk-runtime";

struct Running(Arc<Runtime>);
impl Drop for Running {
    // Release this current SDK runtime owner when it leaves scope
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

// Construct the guard candidate fixture for current SDK runtime
fn guard_candidate() {
    assert_eq!(connection_contract::STREAMDECK.application_version, "7.1.0");
    assert_eq!(
        connection_contract::STREAMDECK.compatibility_label,
        if std::env::var_os("STREAM_DOCK_MANAGER_PRODUCTION_QUALIFICATION").is_some() {
            "streamdeck-7.1-subset"
        } else {
            "streamdeck-7.1-candidate-subset"
        }
    );
    assert!(
        std::env::var_os("STREAM_DOCK_MANAGER_CANDIDATE_QUALIFICATION").is_some()
            || std::env::var_os("STREAM_DOCK_MANAGER_PRODUCTION_QUALIFICATION").is_some(),
        "Use the qualification runner; never change production claims merely for a test"
    );
}

// Prepare package
fn prepare_package(root: &Path) -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/plugins/elgato-sdk-executable");
    let package = root.join(format!("{ID}.sdPlugin"));
    std::fs::create_dir_all(&package).unwrap();
    std::fs::copy(
        fixture.join("package/com.manager.fixture.elgato.sdPlugin/current-mode.js"),
        package.join("plugin.js"),
    )
    .unwrap();
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(fixture.join("src/manifest.json")).unwrap()).unwrap();
    manifest["Software"]["MinimumVersion"] = json!("7.1");
    manifest["Nodejs"]["Version"] = json!("24");
    std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(package.join("package.json"), r#"{"type":"module"}"#).unwrap();
    write_artwork(&package);
    package
}

// Write artwork
fn write_artwork(package: &Path) {
    for (name, size) in [("plugin-icon", 256), ("action-icon", 20), ("key", 72)] {
        for (suffix, scale) in [("", 1), ("@2x", 2)] {
            image::RgbaImage::new(size * scale, size * scale)
                .save(package.join(format!("{name}{suffix}.png")))
                .unwrap();
        }
    }
}

// Construct the seed offline metadata fixture for current SDK runtime
fn seed_offline_metadata(root: &Path) {
    // Persist the same metadata used by protocol qualification, then close this
    // service before the real Runtime opens its own real shared SQLite storage
    let service = stream_dock_manager_plugin_runtimes::RuntimeService::with_dependencies(
        root.join("runtimes"),
        Arc::new(protocol::ProtocolMetadata),
        Arc::new(protocol::ProtocolMetadata),
    )
    .unwrap();
    assert!(service.check().unwrap().error.is_none());
    service.automatic(false).unwrap();
    assert!(service.status().installations.is_empty());
}

// Start the current SDK runtime operation
fn start(root: &Path) -> Running {
    let runtime = test_support::headless(root.to_path_buf()).unwrap();
    assert!(!runtime.runtimes.status().automatic);
    runtime
        .plugins
        .lock()
        .unwrap()
        .set_registration_environment(RegistrationEnvironment::new(Some("en"), Some(1.0)).unwrap());
    Running(runtime)
}

// Close the owned current SDK runtime resource
fn close(runtime: Running) {
    let weak = Arc::downgrade(&runtime.0);
    drop(runtime);
    let deadline = Instant::now() + Duration::from_secs(5);
    while weak.upgrade().is_some() {
        assert!(
            Instant::now() < deadline,
            "Real runtime services retained the disposable store after shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Probe {
    package: PathBuf,
    sequence: u64,
}
impl Probe {
    // Initialize current SDK runtime state from the supplied inputs
    fn new(runtime: &Runtime) -> Self {
        let package = runtime.plugins.lock().unwrap().installation_root().join(ID);
        for filename in ["command.json", "command.tmp", "report.json", "report.tmp"] {
            let path = package.join(filename);
            if path.exists() {
                std::fs::remove_file(path).unwrap();
            }
        }
        Self {
            package,
            sequence: 0,
        }
    }
    // Assemble the current SDK runtime report
    fn report(&self) -> Value {
        std::fs::read(self.package.join("report.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null)
    }
    // Wait for the current SDK runtime condition or result
    fn wait(&self, runtime: &Runtime, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            runtime.plugins.lock().unwrap().poll();
            let report = self.report();
            if predicate(&report) {
                return report;
            }
            assert!(
                Instant::now() < deadline,
                "Runtime SDK timeout: {report}; host: {:?}",
                runtime.plugins.lock().unwrap().list()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    // Launch the requested current SDK runtime operation
    fn launch(&self, runtime: &Runtime) {
        runtime.simulate_device(true, DEVICE).unwrap();
        test_support::synchronize_plugins(runtime);
        let connected = self.wait(runtime, |v| v["response"]["sequence"] == 0);
        assert_eq!(connected["response"]["connected"], true, "{connected}");
        assert_eq!(connected["response"]["legacy"], false);
        assert_eq!(connected["response"]["version"], "7.1.0");
        // A newly launched session needs the saved production action projection
        test_support::synchronize_plugins(runtime);
        self.wait(runtime, |v| {
            v["events"]["appearances"]
                .as_array()
                .is_some_and(|a| a.len() == 2)
        });
    }
    // Construct the context fixture for current SDK runtime
    fn context(&self, marker: &str) -> String {
        self.report()["events"]["appearances"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|a| a["settings"]["marker"] == marker)
            .unwrap()["context"]
            .as_str()
            .unwrap()
            .into()
    }
    // Construct the command fixture for current SDK runtime
    fn command(
        &mut self,
        runtime: &Runtime,
        operation: &str,
        context: &str,
        mut fields: Value,
    ) -> Value {
        self.sequence += 1;
        fields["sequence"] = json!(self.sequence);
        fields["operation"] = json!(operation);
        fields["context"] = json!(context);
        std::fs::write(self.package.join("command.tmp"), fields.to_string()).unwrap();
        std::fs::rename(
            self.package.join("command.tmp"),
            self.package.join("command.json"),
        )
        .unwrap();
        let report = self.wait(runtime, |v| v["response"]["sequence"] == self.sequence);
        assert!(report["response"].get("error").is_none(), "{report}");
        report
    }
}

// Construct the binding fixture for current SDK runtime
fn binding(runtime: &Runtime, profile: &str, control: &str) -> (Value, BTreeMap<String, String>) {
    let snapshot = runtime.snapshot();
    let profile = snapshot.profiles.iter().find(|p| p.id == profile).unwrap();
    match &profile.pages[0].bindings[control].action {
        Action::Plugin {
            plugin_id,
            action_id,
            settings,
            resources,
            ..
        } => {
            assert_eq!(plugin_id, ID);
            assert_eq!(action_id, ACTION);
            (settings.clone(), resources.clone())
        }
        _ => panic!("Saved plugin binding changed"),
    }
}

#[test]
#[ignore = "Isolated7.1 candidate only: genuine SDK, explicit Node24, disposable SQLite/config and loopback"]
// Verify that official SDK real runtime settings resources survive restart and rejection
fn official_sdk_real_runtime_settings_resources_survive_restart_and_rejection() {
    guard_candidate();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("manager");
    seed_offline_metadata(&root);
    let package = prepare_package(&temp.path().join("source"));
    let running = start(&root);
    let runtime = &running.0;
    {
        let mut host = runtime.plugins.lock().unwrap();
        let prepared = PluginHost::prepare_install(&host.installation_root(), &package).unwrap();
        assert!(host
            .replace_prepared(prepared)
            .unwrap()
            .capabilities
            .is_none());
        protocol::runtime(&host, ID, false).unwrap();
        let resolution = host.preflight(ID).unwrap().runtime.unwrap();
        assert_eq!(resolution.requirement.version.as_deref(), Some("24"));
        assert!(!resolution.overridden);
    }
    runtime.simulate_device(true, DEVICE).unwrap();
    let before = runtime.snapshot();
    let profile_id = before.session(DEVICE).profile_id;
    let mut profile = before
        .profiles
        .iter()
        .find(|p| p.id == profile_id)
        .unwrap()
        .clone();
    for (control, marker) in [("key-1", "initial"), ("key-2", "other")] {
        profile.pages[0].bindings.get_mut(control).unwrap().action = Action::Plugin {
            user_desired_state: None,
            plugin_id: ID.into(),
            action_id: ACTION.into(),
            settings: json!({"marker":marker}),
            resources: Default::default(),
        };
    }
    runtime
        .save_profile(profile, before.configuration_revision)
        .unwrap();
    let mut probe = Probe::new(runtime);
    runtime.plugins.lock().unwrap().enable(ID, true).unwrap();
    probe.launch(runtime);
    let context = probe.context("initial");
    let other = probe.context("other");
    let settings = json!({"marker":"durable","nested":{"null":null,"zero":0,"enabled":false}});
    let revision = runtime.snapshot().configuration_revision;
    let written = probe.command(runtime, "selfWrite", &context, json!({"settings":settings}));
    assert_eq!(written["response"]["result"]["settings"], settings);
    assert_eq!(written["events"]["settings"], json!([]));
    assert_eq!(written["events"]["globals"], json!([]));
    assert_eq!(binding(runtime, &profile_id, "key-1").0, settings);
    assert!(runtime.snapshot().configuration_revision > revision);

    let source = probe.package.join("runtime-durability-source.txt");
    let bytes = b"Genuine SDK resource bytes survive source deletion and Runtime restart.";
    std::fs::write(&source, bytes).unwrap();
    let written = probe.command(
        runtime,
        "resourceRoundtrip",
        &context,
        json!({"resources":{"document":source}}),
    );
    let returned = written["response"]["result"]["resources"].clone();
    let managed_path = PathBuf::from(returned["document"].as_str().unwrap());
    assert_ne!(managed_path, source);
    assert!(managed_path
        .canonicalize()
        .unwrap()
        .starts_with(root.join("profiles").canonicalize().unwrap()));
    assert_eq!(std::fs::read(&managed_path).unwrap(), bytes);
    assert_eq!(written["events"]["resources"], json!([]));
    let ids = binding(runtime, &profile_id, "key-1").1;
    assert_eq!(ids.len(), 1);
    assert_eq!(
        ids["document"],
        managed_path.file_name().unwrap().to_str().unwrap()
    );
    let isolated = probe.command(runtime, "queryResources", &other, json!({}));
    assert_eq!(isolated["response"]["result"]["resources"], json!({}));
    assert_eq!(
        binding(runtime, &profile_id, "key-2"),
        (json!({"marker":"other"}), BTreeMap::new())
    );

    // Real staging must reject a nonexistent file, release the SDK's getter, and
    // retain the previous committed resource rather than a fake test commit
    let events = runtime.subscribe();
    let revision = runtime.snapshot().configuration_revision;
    let missing = probe.package.join("missing-resource-must-not-exist.txt");
    assert!(!missing.exists());
    let rejected = probe.command(
        runtime,
        "resourceRoundtrip",
        &context,
        json!({"resources":{"document":missing}}),
    );
    assert_eq!(rejected["response"]["result"]["resources"], returned);
    assert_eq!(rejected["events"]["resources"], json!([]));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("Real runtime did not report rejected staging");
        if event.kind == "plugin-error" {
            assert!(
                event
                    .message
                    .as_ref()
                    .is_some_and(|m| m.contains("os error 2")),
                "Unexpected staging failure: {:?}",
                event.message
            );
            break;
        }
    }
    assert_eq!(runtime.snapshot().configuration_revision, revision);
    assert_eq!(binding(runtime, &profile_id, "key-1").1, ids);
    std::fs::remove_file(&source).unwrap();
    assert_eq!(std::fs::read(&managed_path).unwrap(), bytes);
    close(running);

    // This is a full new Runtime and SDK process, not a cache or projection reset
    let reopened = start(&root);
    assert_eq!(
        binding(&reopened.0, &profile_id, "key-1"),
        (settings.clone(), ids.clone())
    );
    let resolution = reopened
        .0
        .plugins
        .lock()
        .unwrap()
        .preflight(ID)
        .unwrap()
        .runtime
        .unwrap();
    assert!(!resolution.overridden);
    assert_eq!(resolution.state, "ready");
    let mut probe = Probe::new(&reopened.0);
    probe.launch(&reopened.0);
    let context = probe.context("durable");
    let query = probe.command(&reopened.0, "query", &context, json!({}));
    assert_eq!(query["response"]["result"]["settings"], settings);
    assert_eq!(
        query["response"]["result"]["globals"],
        json!({"source":"runtime"})
    );
    let resources = probe.command(&reopened.0, "queryResources", &context, json!({}));
    assert_eq!(resources["response"]["result"]["resources"], returned);
    assert_eq!(
        std::fs::read(
            resources["response"]["result"]["resources"]["document"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        bytes
    );
    assert!(!source.exists());
    assert_eq!(resources["events"]["resources"], json!([]));
    assert_eq!(resources["events"]["settings"], json!([]));
    close(reopened);
}
