//! Saved action settings use the workspace transaction, including on headless hosts
use crate::{actions::Action, profiles::ProfileData, Runtime};
use std::sync::{mpsc, Arc};
use stream_dock_manager_plugin_host::OwnedSettingsWrite;

// Verify the saved action owner and revision before replacing settings or resources
fn update(
    data: &mut ProfileData,
    request: &OwnedSettingsWrite,
) -> Result<serde_json::Value, String> {
    if request
        .address
        .get("inspector_revision")
        .is_some_and(|r| r.as_u64() != Some(data.configuration_revision))
    {
        return Err("Workspace changed. Reopen the vendor inspector before editing.".into());
    }
    let action = saved_action_mut(data, &request.address)?;
    let Action::Plugin {
        plugin_id,
        action_id,
        settings,
        resources,
        ..
    } = action
    else {
        return Err("Saved settings action changed".into());
    };
    if plugin_id != &request.plugin_id || action_id != &request.action_id {
        return Err("Saved settings owner changed".into());
    }
    if let Some(value) = &request.resources {
        *resources =
            serde_json::from_value(value.clone()).map_err(|_| "Invalid managed resources")?;
        return Ok(settings.clone());
    }
    *settings = stream_dock_manager_plugin_host::preserve_references(settings, &request.settings);
    Ok(settings.clone())
}

// Borrow mutable saved action state for plugin settings persistence
fn saved_action_mut<'a>(
    data: &'a mut ProfileData,
    address: &serde_json::Value,
) -> Result<&'a mut Action, String> {
    let path: Vec<usize> = serde_json::from_value(address["path"].clone())
        .map_err(|_| "Invalid saved settings address")?;
    let action = if let Some(id) = address["macro_id"].as_str() {
        &mut data
            .saved_macros
            .iter_mut()
            .find(|value| value.id == id)
            .ok_or("Saved macro was removed")?
            .action
    } else {
        let profile = address["profile_id"]
            .as_str()
            .ok_or("Missing settings profile")?;
        let page = address["page_id"].as_str().ok_or("Missing settings page")?;
        let control = address["control_id"]
            .as_str()
            .ok_or("Missing settings control")?;
        &mut data
            .profiles
            .iter_mut()
            .find(|value| value.id == profile)
            .and_then(|value| value.pages.iter_mut().find(|value| value.id == page))
            .and_then(|value| value.bindings.get_mut(control))
            .ok_or("Saved settings binding was removed")?
            .action
    };
    crate::workspace::action_at_mut(action, &path)
}

impl Runtime {
    /// Bind a vendor inspector to an exact persisted action, never an unsaved editor value
    pub fn bind_vendor_inspector(
        &self,
        token: &str,
        id: &str,
        action_id: &str,
        settings: &serde_json::Value,
        resources: &crate::vendor_resources::Resources,
        mut address: serde_json::Value,
    ) -> Result<(), String> {
        let _state = self.state.lock().unwrap();
        let _gate = self.profiles.transaction.lock().unwrap();
        let mut data = self.profiles.load()?;
        address["inspector_revision"] = serde_json::json!(data.configuration_revision);
        let action = saved_action_mut(&mut data, &address)?;
        let desired = match action {
            Action::Plugin {
                user_desired_state, ..
            } => *user_desired_state,
            _ => None,
        };
        let expected = Action::Plugin {
            plugin_id: id.into(),
            action_id: action_id.into(),
            settings: settings.clone(),
            resources: resources.clone(),
            user_desired_state: desired,
        };
        if serde_json::to_value(action).map_err(|e| e.to_string())?
            != serde_json::to_value(expected).map_err(|e| e.to_string())?
        {
            return Err("Save this action before opening its vendor inspector".into());
        }
        self.plugins
            .lock()
            .unwrap()
            .bind_vendor_panel(token, address)
    }
    // Start settings writer
    pub(crate) fn start_settings_writer(runtime: &Arc<Self>) {
        let (sender, receiver) = mpsc::sync_channel::<OwnedSettingsWrite>(8);
        runtime.plugins.lock().unwrap().set_settings_writer(sender);
        let weak = Arc::downgrade(runtime);
        let worker = std::thread::spawn(move || loop {
            let request = match receiver.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if weak.upgrade().is_none_or(|runtime| runtime.is_stopping()) {
                        break;
                    }
                    continue;
                }
            };
            let Some(runtime) = weak.upgrade() else { break };
            if let Err(error) = runtime.persist_saved_plugin_settings(&request) {
                runtime
                    .plugins
                    .lock()
                    .unwrap()
                    .reject_saved_settings(&request, &error);
                runtime.emit("plugin-error", None, None, Some(error));
            }
        });
        *runtime.settings_writer.lock().unwrap() = Some(worker);
    }

    // Persist saved plugin settings
    fn persist_saved_plugin_settings(&self, request: &OwnedSettingsWrite) -> Result<(), String> {
        if self.is_stopping() {
            return Err("Manager is stopping".into());
        }
        let mut request = request.clone();
        request.address = self
            .plugins
            .lock()
            .unwrap()
            .current_saved_write_address(&request)?;
        let resource_paths = if let Some(sources) = &request.resources {
            let sources =
                serde_json::from_value(sources.clone()).map_err(|_| "Invalid resource paths")?;
            let ids = crate::vendor_resources::stage(&self.profiles.root, &sources)?;
            let paths =
                serde_json::to_value(crate::vendor_resources::paths(&self.profiles.root, &ids)?)
                    .map_err(|e| e.to_string())?;
            let ids = serde_json::to_value(ids).map_err(|e| e.to_string())?;
            if request.address.is_null() {
                return self
                    .plugins
                    .lock()
                    .unwrap()
                    .commit_draft_resources(&request, ids, paths);
            }
            request.resources = Some(ids);
            Some(paths)
        } else {
            None
        };
        let resource_ids = request.resources.clone();
        let request = &request;
        // Match ordinary workspace commits: state -> profile gate -> host -> database
        let mut state = self.state.lock().unwrap();
        let gate = self.profiles.transaction.lock().unwrap();
        let mut committed_request = request.clone();
        let prepared = self.profiles.prepare_import_transaction(
            state.configuration_revision,
            "Update plugin settings",
            None,
            0,
            |data| {
                committed_request.settings = update(data, request)?;
                Ok(())
            },
        )?;
        committed_request.resources = resource_paths;
        let mut host = self.plugins.lock().unwrap();
        let data = host.commit_saved_settings(
            &committed_request,
            prepared.retained_bytes()?,
            move |tx| prepared.commit(tx),
        )?;
        host.advance_inspector_revision(&request.context, data.configuration_revision);
        if let Some(ids) = resource_ids {
            host.update_saved_resource_ids(&request.context, ids);
        }
        Self::apply_data(&mut state, data);
        drop(host);
        drop(gate);
        drop(state);
        self.emit("profiles", None, None, None);
        let _ = self.device_commands.send(crate::devices::Request::Restore);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;

    #[test]
    // Verify that immediate inspector rejects stale revision deleted target and changed owner
    fn immediate_inspector_rejects_stale_revision_deleted_target_and_changed_owner() {
        let mut data = ProfileData::default();
        let profile = &mut data.profiles[0];
        let address = json!({"profile_id":profile.id,"page_id":profile.pages[0].id,"control_id":"key-1","path":[],"inspector_revision":data.configuration_revision});
        profile.pages[0].bindings.get_mut("key-1").unwrap().action = Action::Plugin {
            user_desired_state: None,
            plugin_id: "vendor".into(),
            action_id: "vendor.action".into(),
            settings: json!({"old":true}),
            resources: Default::default(),
        };
        let request = OwnedSettingsWrite {
            resource_snapshots: vec![],
            operation_id: "test".into(),
            plugin_id: "vendor".into(),
            action_id: "vendor.action".into(),
            context: "inspector".into(),
            address,
            settings: json!({"new":true}),
            session: "session".into(),
            resources: None,
        };
        data.configuration_revision += 1;
        let before = serde_json::to_value(&data).unwrap();
        assert!(update(&mut data, &request)
            .unwrap_err()
            .contains("Workspace changed"));
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
        data.configuration_revision -= 1;
        let mut foreign = request.clone();
        foreign.plugin_id = "foreign".into();
        let before = serde_json::to_value(&data).unwrap();
        assert!(update(&mut data, &foreign).is_err());
        assert_eq!(serde_json::to_value(&data).unwrap(), before);
        data.profiles[0].pages[0].bindings.remove("key-1");
        assert!(update(&mut data, &request).is_err());
    }
    #[test]
    // Verify that vendor inspector binding and saved macro writes survive close and restart
    fn vendor_inspector_binding_and_saved_macro_writes_survive_close_and_restart() {
        // Copy the settings fixture into the requested representation
        fn copy(from: &std::path::Path, to: &std::path::Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy(&entry.path(), &target);
                } else {
                    std::fs::copy(entry.path(), target).unwrap();
                }
            }
        }
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/com.vsd.craft.xxx.sdPlugin");
        copy(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/plugins/vendor/vsd/VSDJavaScriptSDK/com.vsd.craft.xxx.sdPlugin"), &package);
        let owner = "com.vsd.craft.xxx";
        let action_id = "com.hotspot.streamdock.xxx.action1";
        let runtime = crate::test_support::headless(root.path().into()).unwrap();
        let mut data = runtime.profiles.load().unwrap();
        let plugin_action = Action::Plugin {
            user_desired_state: None,
            plugin_id: owner.into(),
            action_id: action_id.into(),
            settings: json!({"original":true}),
            resources: Default::default(),
        };
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action = plugin_action.clone();
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-2")
            .unwrap()
            .action = plugin_action.clone();
        let mut plugin_action = plugin_action;
        if let Action::Plugin {
            user_desired_state, ..
        } = &mut plugin_action
        {
            *user_desired_state = Some(1);
        }
        data.saved_macros.push(crate::builtins::SavedMacro {
            id: "vendor-macro".into(),
            name: "Vendor".into(),
            inputs: Default::default(),
            action: Action::Macro {
                actions: vec![plugin_action],
            },
        });
        runtime.profiles.save(&data).unwrap();
        let addresses = [
            json!({"profile_id":data.profiles[0].id,"page_id":data.profiles[0].pages[0].id,"control_id":"key-1","path":[]}),
            json!({"macro_id":"vendor-macro","path":[0]}),
        ];
        for (index, address) in addresses.iter().enumerate() {
            let panel = runtime
                .plugins
                .lock()
                .unwrap()
                .settings_panel(owner, Some(action_id), &json!({"original":true}))
                .unwrap();
            assert!(runtime
                .bind_vendor_inspector(
                    &panel.token,
                    owner,
                    action_id,
                    &json!({"unsaved":true}),
                    &Default::default(),
                    address.clone()
                )
                .is_err());
            runtime
                .bind_vendor_inspector(
                    &panel.token,
                    owner,
                    action_id,
                    &json!({"original":true}),
                    &Default::default(),
                    address.clone(),
                )
                .unwrap();
            let authority = panel.url.split('/').nth(2).unwrap();
            let (mut socket, _) = tungstenite::connect(format!("ws://{authority}")).unwrap();
            if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
            }
            socket
                .send(tungstenite::Message::Text(
                    json!({"event":"registerPropertyInspector","uuid":panel.token})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let context = format!("settings-draft:{}", panel.token);
            let settings = json!({"saved":index,"false":false,"nested":{"value":null}});
            socket
                .send(tungstenite::Message::Text(
                    json!({"event":"setSettings","context":context,"payload":settings})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                socket
                    .send(tungstenite::Message::Text(
                        json!({"event":"getSettings","context":context,"id":"saved-check"})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                let message = socket.read().unwrap();
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let event: Value = serde_json::from_str(text).unwrap();
                assert_ne!(event["event"], "settingsDraftSaved");
                if event["id"] == "saved-check" && event["payload"]["settings"] == settings {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Settings did not commit: {event}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let source = package.join(format!("resource-{index}.txt"));
            std::fs::write(&source, format!("resource {index}")).unwrap();
            socket
                .send(tungstenite::Message::Text(
                    json!({"event":"setResources","context":context,"payload":{"file":source}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                socket
                    .send(tungstenite::Message::Text(
                        json!({"event":"getResources","context":context,"id":"resource-check"})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                let message = socket.read().unwrap();
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let event: Value = serde_json::from_str(text).unwrap();
                assert_ne!(event["event"], "settingsDraftSaved");
                if event["id"] == "resource-check" {
                    assert_eq!(event["context"], context);
                    if let Some(file) = event["payload"]["resources"]["file"].as_str() {
                        assert_eq!(
                            std::fs::read_to_string(file).unwrap(),
                            format!("resource {index}")
                        );
                        break;
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Resources did not commit"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            socket.send(tungstenite::Message::Text(json!({"event":"setGlobalSettings","context":context,"payload":{"global":index}}).to_string().into())).unwrap();
            socket
                .send(tungstenite::Message::Text(
                    json!({"event":"getGlobalSettings","context":context,"id":"verify"})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            loop {
                let message = socket.read().unwrap();
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let event: Value = serde_json::from_str(text).unwrap();
                if event["id"] == "verify" {
                    assert_eq!(event["payload"]["settings"], json!({"global":index}));
                    break;
                }
            }
            runtime
                .plugins
                .lock()
                .unwrap()
                .close_inspector(&panel.token);
        }
        runtime.shutdown();
        drop(runtime);
        let runtime = crate::test_support::headless(root.path().into()).unwrap();
        assert_eq!(
            runtime
                .plugins
                .lock()
                .unwrap()
                .configuration(owner)
                .unwrap()
                .settings
                .get("global"),
            Some(&json!(1))
        );
        let mut data = runtime.profiles.load().unwrap();
        for (index, address) in addresses.iter().enumerate() {
            let Action::Plugin {
                settings,
                resources,
                user_desired_state,
                ..
            } = saved_action_mut(&mut data, address).unwrap()
            else {
                panic!("plugin missing")
            };
            assert_eq!(
                *settings,
                json!({"saved":index,"false":false,"nested":{"value":null}})
            );
            assert_eq!(resources.len(), 1);
            assert_eq!(*user_desired_state, if index == 1 { Some(1) } else { None });
        }
        let Action::Plugin { settings, .. } = &data.profiles[0].pages[0].bindings["key-2"].action
        else {
            panic!("unrelated missing")
        };
        assert_eq!(*settings, json!({"original":true}));
        runtime.shutdown();
    }
    #[test]
    // Verify that base action settings commit through headless owner and survive restart
    fn base_action_settings_commit_through_headless_owner_and_survive_restart() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/fixture");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("runtime.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        std::fs::write(package.join("manifest.json"), json!({"UUID":"fixture.settings","Name":"Settings fixture","Version":"0.1.0","SDKVersion":1,"CodePath":"runtime.html","Actions":[{"UUID":"fixture.settings.action","Name":"Action"}]}).to_string()).unwrap();
        crate::test_plugin::modernize(&package);
        let runtime = crate::test_support::headless(root.path().into()).unwrap();
        runtime.simulate_device(true, "simulator:settings").unwrap();
        let before = runtime.snapshot();
        let profile_id = before.session("simulator:settings").profile_id;
        let mut profile = before
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .unwrap()
            .clone();
        profile.pages[0].bindings.get_mut("key-1").unwrap().action = Action::Plugin {
            user_desired_state: None,
            plugin_id: "fixture.settings".into(),
            action_id: "fixture.settings.action".into(),
            settings: json!({"old":true}),
            resources: Default::default(),
        };
        runtime
            .save_profile(profile, before.configuration_revision)
            .unwrap();
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable("fixture.settings", true)
            .unwrap();
        crate::test_support::synchronize_plugins(&runtime);
        let url = {
            let mut host = runtime.plugins.lock().unwrap();
            host.poll();
            host.browser_view("fixture.settings").unwrap().unwrap().url
        };
        // The first poll launches the queued plugin and resets its session projection
        // Headless fixtures explicitly republish the saved active actions after launch
        crate::test_support::synchronize_plugins(&runtime);
        let parts: Vec<_> = url.split('/').collect();
        let (mut socket, _) = tungstenite::connect(format!("ws://{}", parts[2])).unwrap();
        if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
        }
        socket
            .send(tungstenite::Message::Text(
                crate::test_plugin::registration(&package, parts[4])
                    .to_string()
                    .into(),
            ))
            .unwrap();
        let context = loop {
            let message = socket.read().unwrap();
            let Ok(text) = message.to_text() else {
                continue;
            };
            let value: Value = serde_json::from_str(text).unwrap();
            if value["event"] == "willAppear" {
                break value["context"].as_str().unwrap().to_owned();
            }
        };
        let settings = json!({"unknown":{"kept":null},"zero":0,"disabled":false});
        socket
            .send(tungstenite::Message::Text(
                json!({"event":"setSettings","context":context,"payload":settings})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        loop {
            let message = socket.read().unwrap();
            let Ok(text) = message.to_text() else {
                continue;
            };
            let value: Value = serde_json::from_str(text).unwrap();
            if value["event"] == "didReceiveSettings" && value["payload"]["settings"] == settings {
                break;
            }
        }
        let resource_source = package.join("resource.txt");
        std::fs::write(&resource_source, "persisted vendor resource").unwrap();
        socket.send(tungstenite::Message::Text(json!({"event":"setResources","context":context,"payload":{"document":resource_source}}).to_string().into())).unwrap();
        let returned_resource = loop {
            let message = socket.read().unwrap();
            let Ok(text) = message.to_text() else {
                continue;
            };
            let value: Value = serde_json::from_str(text).unwrap();
            if value["event"] == "didReceiveResources" {
                break value["payload"]["resources"]["document"]
                    .as_str()
                    .unwrap()
                    .to_owned();
            }
        };
        assert_eq!(
            std::fs::read(&returned_resource).unwrap(),
            b"persisted vendor resource"
        );
        std::fs::remove_file(&resource_source).unwrap();
        socket
            .send(tungstenite::Message::Text(
                json!({"event":"getResources","context":context,"id":"resource-request"})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        loop {
            let message = socket.read().unwrap();
            let Ok(text) = message.to_text() else {
                continue;
            };
            let value: Value = serde_json::from_str(text).unwrap();
            if value["event"] == "didReceiveResources" && value["id"] == "resource-request" {
                assert_eq!(value["payload"]["resources"]["document"], returned_resource);
                break;
            }
        }
        let data = runtime.profiles.load().unwrap();
        let Action::Plugin {
            settings: committed,
            ..
        } = &data
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .unwrap()
            .pages[0]
            .bindings["key-1"]
            .action
        else {
            panic!("Plugin binding changed");
        };
        assert_eq!(committed, &settings);
        assert_eq!(
            serde_json::to_value(runtime.snapshot().profiles).unwrap(),
            serde_json::to_value(data.profiles).unwrap()
        );
        drop(socket);
        runtime.shutdown();
        drop(runtime);
        let reopened = crate::test_support::headless(root.path().into()).unwrap();
        let snapshot = reopened.snapshot();
        let Action::Plugin {
            settings: restored,
            resources: restored_resources,
            ..
        } = &snapshot
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .unwrap()
            .pages[0]
            .bindings["key-1"]
            .action
        else {
            panic!("Plugin binding changed");
        };
        assert_eq!(restored, &settings);
        assert_eq!(restored_resources.len(), 1);
        assert_eq!(
            std::fs::read(
                crate::vendor_resources::paths(&reopened.profiles.root, restored_resources)
                    .unwrap()["document"]
                    .clone()
            )
            .unwrap(),
            b"persisted vendor resource"
        );
        reopened.shutdown();
    }
}
