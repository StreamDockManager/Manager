//! Native protocol qualification is gated independently of the frontend's Node 24 toolchain
#[path = "../examples/support/mod.rs"]
mod support;
use base64::Engine as _;
use hardware_core::Input;
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{ActiveAction, HostEffect, PluginHost, RuntimeState};
use stream_dock_manager_plugin_runtimes::{RuntimeKind, RuntimeService};
#[test]
#[ignore = "Explicit upstream runtime download and loopback qualification"]
// Verify verified private runtime launches without system interpreter and restarts offline
fn verified_private_runtime_launches_without_system_interpreter_and_restarts_offline() {
    assert_eq!(
        std::env::var("STREAM_DOCK_MANAGER_QUALIFY_RUNTIMES").as_deref(),
        Ok("1")
    );
    let kind = match std::env::var("STREAM_DOCK_MANAGER_QUALIFY_KIND").as_deref() {
        Ok("nodejs") | Err(_) => RuntimeKind::Nodejs,
        Ok(kind) => panic!("Unsupported runtime kind: {kind}"),
    };
    let line = std::env::var("STREAM_DOCK_MANAGER_QUALIFY_LINE").ok();
    let root = tempfile::tempdir().unwrap();
    let service = RuntimeService::open(root.path().join("runtimes")).unwrap();
    let mut host = PluginHost::with_storage(
        root.path().join("plugins"),
        service.clone(),
        service.storage().clone(),
    )
    .unwrap();
    // Explicit synthetic registration context for disposable protocol qualification
    host.set_registration_environment(
        stream_dock_manager_plugin_host::RegistrationEnvironment::new(Some("en"), Some(1.0))
            .unwrap(),
    );
    let package = root.path().join("fixture");
    std::fs::create_dir_all(&package).unwrap();
    let icon = json!({"light":"icon.svg", "dark":"icon.svg"});
    let capabilities = json!({"version":1,"id":"app.streamdockmanager.plugins.runtime.protocol","plugin_version":"0.1.0","theme_protocol":1,"settings_protocol":1,"input_protocol":0,"icon":icon,"actions":{"app.streamdockmanager.plugins.runtime.protocol.counter":{"control_types":["button"],"icon":icon,"states":[icon]}}});
    let declaration = json!({"UUID":"app.streamdockmanager.plugins.runtime.protocol","Name":"Runtime protocol","Version":"0.1.0","SDKVersion":1,"CodePath":"main.mjs","Actions":[{"UUID":"app.streamdockmanager.plugins.runtime.protocol.counter","Name":"Counter"}],"StreamDockManager":{"ManifestDialect":"vsd","Capabilities":capabilities,"Runtime":{"Kind":kind,"Version":line.clone().unwrap_or("*".into()),"Channel":kind.channel()}}});
    std::fs::write(
        package.join("icon.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    std::fs::write(
        package.join("manifest.json"),
        serde_json::to_vec(&declaration).unwrap(),
    )
    .unwrap();
    std::fs::write(package.join("main.mjs"), {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&capabilities).unwrap());
        format!(
            "// SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1\nconst capabilities = {capabilities};\n{}",
            encoded.len(),
            include_str!("../../../tests/fixtures/plugins/runtime/main.mjs")
        )
    })
    .unwrap();
    let plugin = host.install(&package).unwrap();
    assert!(!plugin.enabled);
    assert_eq!(
        host.preflight(&plugin.id).unwrap().runtime.unwrap().state,
        "missing"
    );
    let status = service.check().unwrap();
    let release = status
        .releases
        .iter()
        .filter(|r| r.kind == kind && line.as_ref().is_none_or(|l| r.line == *l))
        .max_by_key(|r| semver::Version::parse(&r.version).unwrap())
        .expect("Official supported release metadata");
    let id = service.start_install(&release.id).unwrap();
    let until = Instant::now() + Duration::from_secs(300);
    loop {
        let job = service
            .status()
            .jobs
            .into_iter()
            .find(|j| j.id == id)
            .unwrap();
        if job.state == "complete" {
            break;
        }
        assert_eq!(job.state, "installing", "{:?}", job.error);
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !host.list()[0].enabled,
        "installation must not enable a plugin"
    );
    // Even imports from the allowed host port must not trigger runtime downloads
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let forbidden = package.join("forbidden.mjs");
    let script =
        format!("try {{ await import('http://127.0.0.1:{port}/dependency.mjs'); }} catch {{}}\n");
    std::fs::write(&forbidden, script).unwrap();
    let installed = service
        .status()
        .installations
        .into_iter()
        .find(|i| i.id == release.id)
        .unwrap();
    eprintln!(
        "Runtime qualification verification: {}; {}",
        installed.verification, installed.provenance
    );
    let requirement = stream_dock_manager_plugin_runtimes::RuntimeRequirement {
        kind,
        channel: kind.channel().into(),
        version: None,
    };
    let mut child = stream_dock_manager_plugin_runtimes::launch_command(
        &installed,
        &requirement,
        &package,
        &forbidden,
        port,
    )
    .unwrap()
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut downloads = 0;
    loop {
        while let Ok((stream, _)) = listener.accept() {
            downloads += 1;
            drop(stream);
        }
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "permission probe failed");
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("import/permission probe timed out");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(downloads, 0, "runtime attempted a startup import download");
    let request = |host: &mut PluginHost| {
        host.enable(&plugin.id, true).unwrap();
        support::demand(host, &plugin.id, "test");
        // Startup and retirement are asynchronous. Publish visibility only
        // after this process has completed its authenticated registration.
        let registration_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            host.poll();
            // Plugin logs are captured by the host, not emitted as runtime effects
            host.drain_effects();
            let plugins = host.list();
            if plugins[0].runtime.state == RuntimeState::Running {
                eprintln!(
                    "Runtime qualification registration health: {:?}",
                    plugins[0].health.totals
                );
                break;
            }
            assert!(
                Instant::now() < registration_deadline,
                "runtime did not complete authenticated registration: {plugins:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        host.synchronize(
            vec![ActiveAction {
                plugin_id: plugin.id.clone(),
                action_id: plugin.actions[0].id.clone(),
                device_id: "test".into(),
                control_id: "key-1".into(),
                scope: "main".into(),
                settings: json!({}),
                surface: None,
                column: 0,
                row: 0,
                encoder: false,
            }],
            vec![],
        );
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            host.poll();
            if host
                .drain_effects()
                .iter()
                .any(|effect| matches!(effect, HostEffect::Feedback(feedback) if feedback.visual.as_ref().is_some_and(|visual| visual["title"] == "0")))
            {
                break;
            }
            assert!(
                Instant::now() < until,
                "registered runtime did not render its initial appearance: {:?}",
                host.list()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let receipt = host
            .dispatch_tracked(
                &plugin.id,
                &plugin.actions[0].id,
                "test",
                "key-1",
                "main",
                &Input::Button { pressed: false },
                &json!({}),
            )
            .unwrap();
        receipt.wait(Duration::from_secs(5), || Ok(())).unwrap();
        assert!(host
            .drain_effects()
            .iter()
            .any(|effect| matches!(effect, HostEffect::Feedback(feedback) if feedback.visual.as_ref().is_some_and(|visual| visual["title"] == "1"))));
        assert_eq!(
            host.list()[0].running_runtime.as_ref().unwrap().source,
            "private"
        );
    };
    request(&mut host);
    host.shutdown();
    drop(host);
    drop(service);
    // Reopen cached inventory, perform no upstream checks, and use the same shared resolver
    let offline = RuntimeService::open(root.path().join("runtimes")).unwrap();
    let mut host = PluginHost::with_storage(
        root.path().join("plugins"),
        Arc::clone(&offline),
        offline.storage().clone(),
    )
    .unwrap();
    request(&mut host);
    host.shutdown();
}
