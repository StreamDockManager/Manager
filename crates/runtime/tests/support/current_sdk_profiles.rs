//! Genuine SDK commands against real profile review, queue and SQLite transactions
//! Only the independent driver executes; authentic upstream archives/manifests stay untouched
use super::{
    close, guard_candidate, protocol, seed_offline_metadata, start, write_artwork, Running,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::{mpsc::Receiver, Arc},
    time::{Duration, Instant},
};
use stream_dock_manager_runtime::{actions::Action, test_support, Runtime, RuntimeEvent};

const PLUGIN: &str = "com.elgato.lightsout";
const DEVICE: &str = "simulator:m3:genuine-sdk-profile";
const ARCHIVE: &[u8] = include_bytes!(
    "../../../../tests/fixtures/plugins/vendor/elgato/lights-out-profiles/sd.streamDeckProfile"
);

// Construct the package fixture for current SDK profiles
fn package(root: &Path) -> PathBuf {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/plugins/elgato-sdk-executable");
    let package = root.join(format!("{PLUGIN}.sdPlugin"));
    std::fs::create_dir_all(&package).unwrap();
    // This manifest is authored independently, never derived by weakening the upstream manifest
    std::fs::copy(
        fixture.join("src/bundled-profiles.manifest.json"),
        package.join("manifest.json"),
    )
    .unwrap();
    std::fs::copy(
        fixture.join("package/com.manager.fixture.elgato.sdPlugin/bundled-profiles.js"),
        package.join("plugin.js"),
    )
    .unwrap();
    std::fs::write(package.join("package.json"), r#"{"type":"module"}"#).unwrap();
    std::fs::write(package.join("sd.streamDeckProfile"), ARCHIVE).unwrap();
    write_artwork(&package);
    package
}

// Advance timed current SDK profiles work
fn tick(runtime: &Arc<Runtime>) {
    runtime.plugins.lock().unwrap().poll();
    // Drain only real authenticated SDK effects; never manufacture a ProfileSwitchEffect
    test_support::pump_vendor_profile_switches(runtime);
}

// Report the until condition for current SDK profiles
fn until(runtime: &Arc<Runtime>, label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        tick(runtime);
        if predicate() {
            return;
        }
        if Instant::now() >= deadline {
            let host: Vec<_> = runtime
                .plugins
                .lock()
                .unwrap()
                .list()
                .into_iter()
                .map(|plugin| (plugin.id, plugin.status, plugin.health.events))
                .collect();
            panic!("Timed out waiting for {label}; host: {host:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Handle the incoming current SDK profiles event
fn event(runtime: &Arc<Runtime>, events: &Receiver<RuntimeEvent>, kind: &str, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut observed = Vec::new();
    loop {
        tick(runtime);
        for event in events.try_iter() {
            if event.kind == kind
                && event
                    .message
                    .as_deref()
                    .unwrap_or_default()
                    .contains(message)
            {
                return;
            }
            if observed.len() < 32 {
                observed.push((event.kind, event.message));
            }
        }
        assert!(
            Instant::now() < deadline,
            "Missing {kind} containing {message:?}; observed: {observed:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Construct the saved fixture for current SDK profiles
fn saved(runtime: &Runtime) -> Value {
    let snapshot = runtime.snapshot();
    json!({"revision":snapshot.configuration_revision,"profiles":snapshot.profiles,"sessions":snapshot.sessions})
}

// Construct the selection fixture for current SDK profiles
fn selection(runtime: &Runtime) -> (String, String) {
    let session = runtime.snapshot().session(DEVICE);
    (session.profile_id, session.page_id)
}

struct Driver {
    package: PathBuf,
    sequence: u64,
    last_sent: Option<Instant>,
    _cleanup: Option<ProcessCleanup>,
}

// Does not depend on Runtime/PluginHost locks, including during assertion unwinding
struct ProcessCleanup {
    pid: sysinfo::Pid,
    started: u64,
    package: PathBuf,
    executable: PathBuf,
}
impl ProcessCleanup {
    // Capture the current current SDK profiles observation
    fn capture(pid: u32, package: &Path) -> Self {
        let pid = sysinfo::Pid::from_u32(pid);
        let mut system = sysinfo::System::new().expect("Cannot observe disposable SDK process");
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::everything(),
        );
        let process = system
            .process(pid)
            .expect("Disposable SDK process is missing");
        let cleanup = Self {
            pid,
            started: process.start_time(),
            package: package.canonicalize().unwrap(),
            executable: PathBuf::from(std::env::var_os("STREAM_DOCK_MANAGER_NODE").unwrap())
                .canonicalize()
                .unwrap(),
        };
        assert!(cleanup.started > 0);
        assert_eq!(process.cwd(), Some(cleanup.package.as_path()));
        assert_eq!(process.exe(), Some(cleanup.executable.as_path()));
        eprintln!("PROFILE_DRIVER pid={pid} cwd={}", cleanup.package.display());
        cleanup
    }
}
impl Drop for ProcessCleanup {
    // Release this current SDK profiles owner when it leaves scope
    fn drop(&mut self) {
        let Ok(mut system) = sysinfo::System::new() else {
            eprintln!(
                "PROFILE_DRIVER cleanup skipped: cannot verify PID {}",
                self.pid
            );
            return;
        };
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[self.pid]),
            true,
            sysinfo::ProcessRefreshKind::everything(),
        );
        let Some(process) = system.process(self.pid) else {
            return;
        };
        if process.start_time() == self.started
            && process.cwd() == Some(self.package.as_path())
            && process.exe() == Some(self.executable.as_path())
        {
            // Identity was rechecked immediately before signaling only our fixture
            let _ = process.kill_with(sysinfo::Signal::Term);
        }
    }
}
impl Driver {
    // Assemble the current SDK profiles report
    fn report(&self) -> Value {
        std::fs::read(self.package.join("report.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null)
    }
    // Launch the requested current SDK profiles operation
    fn launch(runtime: &Arc<Runtime>) -> Self {
        let resolution = runtime
            .plugins
            .lock()
            .unwrap()
            .preflight(PLUGIN)
            .unwrap()
            .runtime
            .unwrap();
        assert_eq!(resolution.requirement.version.as_deref(), Some("24"));
        assert!(!resolution.overridden);
        let package = runtime
            .plugins
            .lock()
            .unwrap()
            .installation_root()
            .join(PLUGIN);
        for name in ["command.json", "command.tmp", "report.json", "report.tmp"] {
            let file = package.join(name);
            if file.exists() {
                std::fs::remove_file(file).unwrap();
            }
        }
        let mut driver = Self {
            package,
            sequence: 0,
            last_sent: None,
            _cleanup: None,
        };
        runtime.simulate_device(true, DEVICE).unwrap();
        test_support::synchronize_plugins(runtime);
        until(runtime, "genuine profile SDK registration", || {
            driver.report()["response"]["sequence"] == 0
        });
        let report = driver.report();
        driver._cleanup = Some(ProcessCleanup::capture(
            u32::try_from(report["response"]["pid"].as_u64().unwrap()).unwrap(),
            &driver.package,
        ));
        assert_eq!(report["response"]["connected"], true, "{report}");
        assert_eq!(report["response"]["legacy"], false);
        assert_eq!(report["response"]["version"], "7.1.0");
        test_support::synchronize_plugins(runtime);
        until(runtime, "profile driver appearance", || {
            driver.report()["appearances"]
                .as_array()
                .is_some_and(|events| !events.is_empty())
        });
        driver
    }
    // Switch to the selected current SDK profiles state
    fn switch(&mut self, runtime: &Arc<Runtime>, profile: Option<&str>, page: Option<u32>) {
        // Respect the production 250 ms request throttle. This is not an ordering fence
        if let Some(last) = self.last_sent {
            std::thread::sleep(Duration::from_millis(300).saturating_sub(last.elapsed()));
        }
        self.sequence += 1;
        let mut command = json!({"sequence":self.sequence,"device":DEVICE});
        if let Some(profile) = profile {
            command["profile"] = json!(profile);
        }
        if let Some(page) = page {
            command["page"] = json!(page);
        }
        std::fs::write(self.package.join("command.tmp"), command.to_string()).unwrap();
        std::fs::rename(
            self.package.join("command.tmp"),
            self.package.join("command.json"),
        )
        .unwrap();
        until(runtime, "SDK switchToProfile send", || {
            self.report()["response"]["sequence"] == self.sequence
        });
        self.last_sent = Some(Instant::now());
        let report = self.report();
        assert_eq!(report["response"]["sent"], true, "{report}");
        let call = report["calls"].as_array().unwrap().last().unwrap();
        assert_eq!(call["device"], DEVICE);
        assert_eq!(call.get("profile"), profile.map(|_| &command["profile"]));
        assert_eq!(call.get("page"), page.map(|_| &command["page"]));
    }
}

// Construct the setup fixture for current SDK profiles
fn setup() -> (tempfile::TempDir, PathBuf, Running, Driver, String) {
    guard_candidate();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("manager");
    seed_offline_metadata(&root);
    let source = package(&temp.path().join("source"));
    let running = start(&root);
    let runtime = &running.0;
    let review = runtime.prepare_plugin_package(&source).unwrap();
    runtime
        .commit_plugin_package(&review.id, false, false)
        .unwrap();
    {
        let host = runtime.plugins.lock().unwrap();
        protocol::runtime(&host, PLUGIN, false).unwrap();
        let resolution = host.preflight(PLUGIN).unwrap().runtime.unwrap();
        assert_eq!(resolution.requirement.version.as_deref(), Some("24"));
        assert!(!resolution.overridden);
    }
    runtime.simulate_device(true, DEVICE).unwrap();
    let snapshot = runtime.snapshot();
    let selected = snapshot.session(DEVICE);
    let control = snapshot
        .devices
        .iter()
        .find(|device| device.id == DEVICE)
        .unwrap()
        .manifest
        .controls
        .iter()
        .find(|control| control.kind == "button")
        .unwrap();
    let mut profile = snapshot
        .profiles
        .iter()
        .find(|p| p.id == selected.profile_id)
        .unwrap()
        .clone();
    let action = Action::Plugin {
        user_desired_state: None,
        plugin_id: PLUGIN.into(),
        action_id: "com.elgato.lightsout.gamepiece".into(),
        settings: json!({"driver":true}),
        resources: Default::default(),
    };
    profile.pages[0].bindings.insert(
        control.id.clone(),
        serde_json::from_value(json!({"title":"","color":"#00000000","action":action})).unwrap(),
    );
    let mut second = profile.pages[0].clone();
    second.id = uuid::Uuid::new_v4().to_string();
    let second_page = second.id.clone();
    profile.pages.push(second);
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    runtime
        .plugins
        .lock()
        .unwrap()
        .enable(PLUGIN, true)
        .unwrap();
    let driver = Driver::launch(runtime);
    let pending = runtime.pending_vendor_profile_reviews().unwrap();
    assert!(
        pending.reviews.is_empty(),
        "AutoInstall false must stay lazy"
    );
    assert!(pending.warnings.is_empty(), "{:?}", pending.warnings);
    assert_eq!(
        std::fs::read(driver.package.join("sd.streamDeckProfile")).unwrap(),
        ARCHIVE
    );
    (temp, root, running, driver, second_page)
}

// Request review
fn request_review(
    runtime: &Arc<Runtime>,
    driver: &mut Driver,
) -> stream_dock_manager_runtime::vendor_profiles::VendorProfileReview {
    let events = runtime.subscribe();
    driver.switch(runtime, Some("sd"), None);
    event(runtime, &events, "vendor-profile-review", "");
    let pending = runtime.pending_vendor_profile_reviews().unwrap();
    assert!(pending.warnings.is_empty(), "{:?}", pending.warnings);
    assert_eq!(pending.reviews.len(), 1);
    let review = pending.reviews.into_iter().next().unwrap();
    assert_eq!(review.plugin_id, PLUGIN);
    assert_eq!(review.manifest_name, "sd");
    assert_eq!(review.device_id, DEVICE);
    assert_eq!(review.binding_count, 15);
    assert_eq!(review.archive_sha256, hex::encode(Sha256::digest(ARCHIVE)));
    assert!(review.switch_after_install);
    review
}

#[test]
#[ignore = "Isolated 7.1 candidate: genuine SDK, explicit Node 24 and disposable profiles"]
// Verify that official SDK bundled profiles review decline accept restart and return
fn official_sdk_bundled_profiles_review_decline_accept_restart_and_return() {
    let (_temp, root, running, mut driver, second_page) = setup();
    let runtime = &running.0;
    let previous = selection(runtime);
    let before = saved(runtime);
    let review = request_review(runtime, &mut driver);
    assert_eq!(
        saved(runtime),
        before,
        "Review must not persist a profile or selection"
    );
    runtime
        .resolve_vendor_profile_review(&review.id, false)
        .unwrap();
    assert_eq!(saved(runtime), before, "Decline must not persist anything");
    assert!(runtime
        .pending_vendor_profile_reviews()
        .unwrap()
        .reviews
        .is_empty());
    assert!(runtime
        .resolve_vendor_profile_review(&review.id, true)
        .is_err());
    close(running);

    // Decline suppression is session-local; a full restart permits fresh user review
    let running = start(&root);
    let runtime = &running.0;
    let mut driver = Driver::launch(runtime);
    assert_eq!(saved(runtime), before);
    let review = request_review(runtime, &mut driver);
    let accepted = runtime
        .resolve_vendor_profile_review(&review.id, true)
        .unwrap();
    assert_eq!(
        accepted.configuration_revision,
        before["revision"].as_u64().unwrap() + 1
    );
    assert_eq!(
        accepted.profiles.len(),
        before["profiles"].as_array().unwrap().len() + 1
    );
    let installed = selection(runtime);
    assert_ne!(installed.0, previous.0);
    let imported = accepted
        .profiles
        .iter()
        .find(|p| p.id == installed.0)
        .unwrap();
    assert_eq!(imported.model_id, "m3");
    assert_eq!(imported.pages.len(), 1);
    assert_eq!(imported.pages[0].bindings.len(), 15);
    for binding in imported.pages[0].bindings.values() {
        assert!(matches!(&binding.action, Action::Plugin { plugin_id, .. } if plugin_id == PLUGIN));
    }
    let imported = serde_json::to_value(imported).unwrap();
    test_support::synchronize_plugins(runtime);
    close(running);

    // New Runtime, new SQLite readers and new SDK process recover mapping and return selection
    let running = start(&root);
    let runtime = &running.0;
    let mut driver = Driver::launch(runtime);
    assert_eq!(selection(runtime), installed);
    assert_eq!(
        serde_json::to_value(
            runtime
                .snapshot()
                .profiles
                .iter()
                .find(|p| p.id == installed.0)
                .unwrap()
        )
        .unwrap(),
        imported
    );
    assert!(runtime
        .pending_vendor_profile_reviews()
        .unwrap()
        .reviews
        .is_empty());
    let before_invalid = saved(runtime);
    driver.switch(runtime, Some("sd"), Some(1));
    until(runtime, "explicit unsupported page rejection", || {
        runtime
            .plugins
            .lock()
            .unwrap()
            .health_snapshot()
            .iter()
            .any(|p| {
                p.id == PLUGIN
                    && p.health.events.iter().any(|e| {
                        e.message
                            .contains("Only bundled single-page profiles (page 0) are supported")
                    })
            })
    });
    assert_eq!(saved(runtime), before_invalid);

    driver.switch(runtime, None, None);
    until(runtime, "SDK return to prior selection", || {
        selection(runtime) == previous
    });
    driver.switch(runtime, Some("sd"), Some(0));
    until(runtime, "SDK explicit page zero switch", || {
        selection(runtime) == installed
    });
    assert_eq!(
        runtime.snapshot().profiles.len(),
        before_invalid["profiles"].as_array().unwrap().len()
    );
    assert!(runtime
        .pending_vendor_profile_reviews()
        .unwrap()
        .reviews
        .is_empty());

    // Even automatic non-vendor navigation invalidates the previous-profile chain
    runtime
        .select_device_profile(DEVICE, &previous.0, Some(&second_page), false)
        .unwrap();
    test_support::synchronize_plugins(runtime);
    let navigation = saved(runtime);
    let events = runtime.subscribe();
    driver.switch(runtime, None, None);
    event(
        runtime,
        &events,
        "plugin-error",
        "No previous profile is tracked",
    );
    assert_eq!(saved(runtime), navigation);
    driver.switch(runtime, Some("sd"), None);
    until(runtime, "SDK installed-profile switch without page", || {
        selection(runtime) == installed
    });
    driver.switch(runtime, None, Some(0));
    until(runtime, "SDK return restores exact previous page", || {
        selection(runtime) == (previous.0.clone(), second_page.clone())
    });
    assert_eq!(
        std::fs::read(driver.package.join("sd.streamDeckProfile")).unwrap(),
        ARCHIVE
    );
    close(running);
}

#[test]
#[ignore = "Isolated 7.1 candidate: genuine SDK and held real profile queue"]
// Verify that official SDK bundled profiles reject stale queued navigation and reconnection
fn official_sdk_bundled_profiles_reject_stale_queued_navigation_and_reconnection() {
    for reconnect in [false, true] {
        // Independent lifetimes keep the production duplicate-error suppression intact
        let (_temp, _root, running, mut driver, second_page) = setup();
        let runtime = &running.0;
        let mut hold = None;
        until(runtime, "idle real profile queue", || {
            hold = test_support::hold_vendor_profile_switches(runtime).ok();
            hold.is_some()
        });
        let hold = hold.unwrap();
        let events = runtime.subscribe();
        driver.switch(runtime, Some("sd"), None);
        until(runtime, "genuine request captured in held queue", || {
            hold.pending() == 1
        });
        assert!(runtime
            .pending_vendor_profile_reviews()
            .unwrap()
            .reviews
            .is_empty());
        if reconnect {
            runtime.simulate_device(false, DEVICE).unwrap();
            runtime.simulate_device(true, DEVICE).unwrap();
        } else {
            let selected = selection(runtime);
            runtime
                .select_device_profile(DEVICE, &selected.0, Some(&second_page), true)
                .unwrap();
        }
        let before_release = saved(runtime);
        drop(hold); // Resume the real worker with its original captured admission
        event(
            runtime,
            &events,
            "plugin-error",
            "Queued profile request became stale",
        );
        assert_eq!(saved(runtime), before_release);
        assert!(runtime
            .pending_vendor_profile_reviews()
            .unwrap()
            .reviews
            .is_empty());
        close(running);
    }
}
