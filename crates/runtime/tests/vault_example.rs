//! Run with SDM_VAULT_EXAMPLE_PACKAGE pointing at a locally built package for this platform.
#![cfg(feature = "test-support")]
use serde_json::json;
use std::{
    io::Read,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{
    permission_rpc::{Services, WebhookEndpoint},
    ActiveAction, PluginDemand, PluginHost, RegistrationEnvironment,
};
use stream_dock_manager_runtime::{permission_broker, test_support};
use zeroize::Zeroizing;

#[derive(Default)]
struct ReviewWindows(Mutex<Vec<String>>);
impl Services for ReviewWindows {
    fn clipboard_available(&self) -> bool {
        false
    }
    fn copy_available(&self) -> bool {
        false
    }
    fn capture(&self, _: &dyn Fn() -> bool) -> Result<Zeroizing<String>, &'static str> {
        Err("operation_unavailable")
    }
    fn copy(&self, _: &str) -> Result<(), &'static str> {
        Err("operation_unavailable")
    }
    fn summarize(&self, _: &str, _: &dyn Fn() -> bool) -> Result<Zeroizing<String>, &'static str> {
        Err("operation_unavailable")
    }
    fn open(&self, id: &str) -> Result<(), &'static str> {
        self.0.lock().unwrap().push(id.into());
        Ok(())
    }
    fn close(&self, _: &str) {}
    fn webhook_available(&self) -> bool {
        true
    }
    fn webhook(
        &self,
        endpoint: &WebhookEndpoint,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), &'static str> {
        permission_broker::webhook(endpoint, cancelled)
    }
}

#[track_caller]
fn until<T>(host: &mut PluginHost, mut check: impl FnMut(&PluginHost) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        host.poll();
        if let Some(v) = check(host) {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "example did not respond before timeout: {:?}",
            host.logs
                .list()
                .iter()
                .filter_map(|capture| host.logs.page(&capture.id, None, false).ok())
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(25))
    }
}

#[test]
#[ignore = "launches the explicitly supplied native example package and uses its two loopback ports"]
fn packaged_vault_example_receives_http_and_https_from_each_sender() {
    const ID: &str = "app.streamdockmanager.plugins.sdm-vault-example";
    let path = PathBuf::from(
        std::env::var_os("SDM_VAULT_EXAMPLE_PACKAGE").expect("supply the native example ZIP"),
    );
    let root = tempfile::tempdir().unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut macros = vec![];
    archive
        .by_name(&format!("{ID}.sdPlugin/examples/vault-variables.zip"))
        .unwrap()
        .read_to_end(&mut macros)
        .unwrap();
    let macros_path = root.path().join("variables.zip");
    std::fs::write(&macros_path, macros).unwrap();
    let runtime = test_support::headless(root.path().join("runtime")).unwrap();
    assert_eq!(
        runtime
            .prepare_macro_import(&macros_path)
            .unwrap()
            .macros
            .len(),
        3
    );
    runtime.shutdown();
    let dir = root.path().join("plugins");
    let mut host = PluginHost::new(dir.clone()).unwrap();
    host.set_registration_environment(RegistrationEnvironment::new(Some("en"), Some(1.0)).unwrap());
    let windows = Arc::new(ReviewWindows::default());
    let flow = host.permission_workflow();
    flow.attach(windows.clone());
    let mut candidate = PluginHost::prepare_install(&host.installation_root(), &path).unwrap();
    assert_eq!(
        PluginHost::prepared_info(&candidate).unwrap().version,
        "0.1.0"
    );
    let grants: Vec<_> = host
        .reviewable_services(&candidate)
        .into_iter()
        .filter(|s| s.action.ends_with("-post"))
        .collect();
    assert_eq!(grants.len(), 5);
    host.stage_service_grants(&mut candidate, &host.service_review(ID).unwrap(), &grants)
        .unwrap();
    host.register_prepared(candidate).unwrap();
    host.logs.start(ID, "SDM Vault Example").unwrap();
    host.enable(ID, true).unwrap();
    host.reconcile_demand(std::collections::HashMap::from([(
        ID.into(),
        vec![PluginDemand {
            device_id: "dock".into(),
            profile_id: "profile".into(),
        }],
    )]));
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let page = || {
        client
            .get("http://127.0.0.1:39480/")
            .send()
            .ok()?
            .text()
            .ok()
    };
    let mut rows = 0;

    for sender in ["plugin", "manager"] {
        for transport in ["https", "http"] {
            let action = format!("{ID}.{sender}-post");
            let control = format!("key-{}", rows + 1);
            let settings = json!({"transport":transport});
            let binding = || ActiveAction {
                plugin_id: ID.into(),
                action_id: action.clone(),
                device_id: "dock".into(),
                control_id: control.clone(),
                scope: "profile:page".into(),
                settings: settings.clone(),
                surface: None,
                column: rows,
                row: 0,
                encoder: false,
            };
            host.synchronize(vec![binding()], vec![json!({"id":"dock"})]);
            until(&mut host, |h| {
                h.list()
                    .iter()
                    .any(|p| {
                        p.id == ID
                            && p.runtime.state
                                == stream_dock_manager_plugin_host::RuntimeState::Running
                    })
                    .then_some(())
            });
            host.synchronize(vec![binding()], vec![json!({"id":"dock"})]);
            let press = |host: &mut PluginHost| {
                let token = until(host, |h| {
                    h.begin_physical_permission(
                        ID,
                        &action,
                        "dock",
                        &control,
                        "profile:page",
                        Arc::new(AtomicBool::new(false)),
                    )
                });
                host.dispatch_with_activation(
                    ID,
                    &action,
                    "dock",
                    &control,
                    "profile:page",
                    &hardware_core::Input::Pulse,
                    &settings,
                    None,
                    Some(&token),
                )
                .unwrap();
            };
            press(&mut host);
            if sender == "manager" {
                let mut id = until(&mut host, |_| windows.0.lock().unwrap().pop());
                flow.cancel(&id);
                assert!(flow.respond(host.storage(), &id, "send", None).is_err());
                assert_eq!(page().unwrap().matches("<td>ping</td>").count(), rows);
                press(&mut host);
                id = until(&mut host, |_| windows.0.lock().unwrap().pop());
                let view = flow.view(host.storage(), &id).unwrap();
                assert_eq!(view["kind"], "webhook");
                assert_eq!(
                    view["origin"],
                    if transport == "http" {
                        "http://127.0.0.1:39480"
                    } else {
                        "https://127.0.0.1:39481"
                    }
                );
                assert_eq!(page().unwrap().matches("<td>ping</td>").count(), rows);
                flow.respond(host.storage(), &id, "send", None).unwrap();
                assert_eq!(flow.view(host.storage(), &id).unwrap()["kind"], "result");
                flow.cancel(&id);
            }
            rows += 1;
            let history = until(&mut host, |_| {
                page().filter(|p| p.matches("<td>ping</td>").count() == rows)
            });
            assert!(history.contains(&format!(
                "<td>{}</td><td><code>POST /webhooks/{sender}",
                transport.to_uppercase()
            )));
        }
    }
    host.shutdown();
}

// Run every direct slot against the installed executable and a temporary password Vault.
#[test]
#[ignore = "requires a locally built native example package"]
fn packaged_example_reads_and_rotates_each_credential() {
    use stream_dock_manager_plugin_host::{permissions::PermissionScope, HostEffect, RuntimeState};
    use stream_dock_manager_runtime::vault::VaultEntryInput;
    const ID: &str = "app.streamdockmanager.plugins.sdm-vault-example";
    let path = PathBuf::from(
        std::env::var_os("SDM_VAULT_EXAMPLE_PACKAGE").expect("supply the native example ZIP"),
    );
    let root = tempfile::tempdir().unwrap();
    let runtime = test_support::headless(root.path().join("runtime")).unwrap();
    runtime
        .vault
        .initialize_password("Disposable example password", "Disposable example password")
        .unwrap();
    let review = runtime.prepare_plugin_package(&path).unwrap();
    let editor = 1;
    let read = format!("{ID}.read");
    for slot in review
        .manifest
        .setup_slots
        .as_ref()
        .unwrap()
        .iter()
        .filter(|s| s.action == read || s.action == format!("{ID}.location"))
    {
        if slot.requires_direct_approval() {
            runtime
                .vault_setup_permission(&review.id, &slot.id, &slot.action, editor, true)
                .unwrap();
        }
        let picker = runtime
            .vault_setup_slot_entries(&review.id, &slot.id, &slot.action, editor)
            .unwrap();
        let entry = runtime
            .vault_setup_create_entry(
                &review.id,
                &slot.id,
                &slot.action,
                editor,
                &picker.ticket,
                VaultEntryInput {
                    label: format!("Example {}", slot.id),
                    kind: serde_json::from_value(json!(slot.entry_kind)).unwrap(),
                    protected: slot.requires_direct_approval(),
                    value: if slot.kind == "location" {
                        json!({"name":"Synthetic private place","latitude":12.34567,"longitude":0})
                    } else {
                        json!("fixture-value-kept-private")
                    },
                },
            )
            .unwrap();
        runtime
            .vault_setup_select(
                &review.id,
                &slot.id,
                &slot.action,
                editor,
                &picker.ticket,
                &entry.id,
                &entry.revision,
                slot.requires_direct_approval(),
            )
            .unwrap();
    }
    let writers: Vec<_> = review
        .reviewable_services
        .unwrap()
        .into_iter()
        .filter(|s| matches!(s.scope, PermissionScope::WriteSecret { .. }))
        .collect();
    assert_eq!(writers.len(), 3);
    runtime
        .commit_plugin_package_permissions(&review.id, false, false, Some(editor), &writers)
        .unwrap();
    let mut host = runtime.plugins.lock().unwrap();
    host.set_registration_environment(RegistrationEnvironment::new(Some("en"), Some(1.0)).unwrap());
    host.logs.start(ID, "SDM Vault Example").unwrap();
    host.enable(ID, true).unwrap();
    host.reconcile_demand(std::collections::HashMap::from([(
        ID.into(),
        vec![PluginDemand {
            device_id: "dock".into(),
            profile_id: "profile".into(),
        }],
    )]));
    let mut run = |action: &str, settings: serde_json::Value, expected: &str| {
        let action = format!("{ID}.{action}");
        let binding = || ActiveAction {
            plugin_id: ID.into(),
            action_id: action.clone(),
            device_id: "dock".into(),
            control_id: "key-1".into(),
            scope: "profile:page".into(),
            settings: settings.clone(),
            surface: None,
            column: 0,
            row: 0,
            encoder: false,
        };
        host.synchronize(vec![binding()], vec![json!({"id":"dock"})]);
        until(&mut host, |h| {
            h.list()
                .iter()
                .any(|p| p.id == ID && p.runtime.state == RuntimeState::Running)
                .then_some(())
        });
        host.synchronize(vec![binding()], vec![json!({"id":"dock"})]);
        host.drain_effects();
        host.dispatch(
            ID,
            &action,
            "dock",
            "key-1",
            "profile:page",
            &hardware_core::Input::Pulse,
            &settings,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut titles = vec![];
        loop {
            host.poll();
            for effect in host.drain_effects() {
                if let HostEffect::Feedback(feedback) = effect {
                    assert!(!feedback
                        .message
                        .to_string()
                        .contains("fixture-value-kept-private"));
                    if let Some(title) = feedback.message["payload"]["title"].as_str() {
                        titles.push(title.to_string())
                    }
                }
            }
            if titles.iter().any(|v| v == expected) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected {expected}, received {titles:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    run("location", json!({}), "Location read");
    runtime
        .vault
        .revoke_direct_reads(ID, &format!("{ID}.location"))
        .unwrap();
    run("location", json!({}), "needs setup");
    run("read", json!({"kind":"text"}), "Text read");
    for kind in ["api-key", "bearer-token", "credential"] {
        run("read", json!({"kind":kind}), "Credential read");
        run("write", json!({"kind":kind}), "Saved revision 1");
        run(
            "write",
            json!({"kind":kind,"expected_revision":"1"}),
            "Saved revision 2",
        );
        run(
            "write",
            json!({"kind":kind,"expected_revision":"1"}),
            "stale revision",
        );
    }
    drop(host);
    runtime.shutdown();
}
