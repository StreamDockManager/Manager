//! Saved action settings use the workspace transaction, including on headless hosts
use crate::{actions::Action, profiles::ProfileData, Runtime, Snapshot};
use std::sync::{mpsc, Arc};
use stream_dock_manager_plugin_host::OwnedSettingsWrite;

// Verify the saved action owner and revision before replacing settings or resources
fn update(
    data: &mut ProfileData,
    request: &OwnedSettingsWrite,
) -> Result<serde_json::Value, String> {
    validate_settings_revision(data.configuration_revision, request)?;
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

// Reject inspector writes made against an outdated workspace revision
fn validate_settings_revision(revision: u64, request: &OwnedSettingsWrite) -> Result<(), String> {
    if request
        .address
        .get("inspector_revision")
        .is_some_and(|r| r.as_u64() != Some(revision))
    {
        return Err("Workspace changed. Reopen the vendor inspector before editing.".into());
    }
    Ok(())
}

// Propagate saved settings to any linked controls in the same profile
fn sync_settings_link(data: &mut ProfileData, request: &OwnedSettingsWrite) -> Result<(), String> {
    if let (Some(id), Some(page), Some(control)) = (
        request.address["profile_id"].as_str(),
        request.address["page_id"].as_str(),
        request.address["control_id"].as_str(),
    ) {
        if let Some(profile) = data.profiles.iter_mut().find(|profile| profile.id == id) {
            profile.sync_link(page, control)?;
        }
    }
    Ok(())
}

// Borrow mutable saved action state for plugin settings persistence
fn saved_action_mut<'a>(
    data: &'a mut ProfileData,
    address: &serde_json::Value,
) -> Result<&'a mut Action, String> {
    workspace_action_mut(&mut data.profiles, &mut data.saved_macros, address)
}

// Locate the saved action in either the committed runtime snapshot or the workspace being edited
fn workspace_action_mut<'a>(
    profiles: &'a mut [crate::profiles::Profile],
    macros: &'a mut [crate::builtins::SavedMacro],
    address: &serde_json::Value,
) -> Result<&'a mut Action, String> {
    let path: Vec<usize> = serde_json::from_value(address["path"].clone())
        .map_err(|_| "Invalid saved settings address")?;
    let action = if let Some(id) = address["macro_id"].as_str() {
        &mut macros
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
        &mut profiles
            .iter_mut()
            .find(|value| value.id == profile)
            .and_then(|value| value.pages.iter_mut().find(|value| value.id == page))
            .and_then(|value| value.bindings.get_mut(control))
            .ok_or("Saved settings binding was removed")?
            .action
    };
    crate::workspace::action_at_mut(action, &path)
}

// Follow the same bounded action paths used for live plugin contexts
fn uses_saved_macro(
    action: &Action,
    macros: &[crate::builtins::SavedMacro],
    wanted: &str,
    depth: usize,
) -> bool {
    if depth > 8 {
        return false;
    }
    match action {
        Action::Builtin { action } => {
            if let crate::builtins::BuiltinAction::SavedMacro { macro_id, .. } = action.as_ref() {
                macro_id == wanted
                    || macros
                        .iter()
                        .find(|value| value.id == *macro_id)
                        .is_some_and(|value| {
                            uses_saved_macro(&value.action, macros, wanted, depth + 1)
                        })
            } else {
                action
                    .children()
                    .iter()
                    .any(|child| uses_saved_macro(child, macros, wanted, depth + 1))
            }
        }
        Action::Toggle { on, off } => {
            uses_saved_macro(on, macros, wanted, depth + 1)
                || uses_saved_macro(off, macros, wanted, depth + 1)
        }
        _ => action
            .children()
            .iter()
            .any(|child| uses_saved_macro(child, macros, wanted, depth + 1)),
    }
}

// Refresh only live surfaces which consume a committed binding, link or saved macro
fn refresh_targets(
    snapshot: &crate::Snapshot,
    address: &serde_json::Value,
) -> Vec<(String, Vec<String>)> {
    let mut targets = Vec::new();
    for device in snapshot
        .devices
        .iter()
        .filter(|device| device.connected && !device.simulated)
    {
        let session = snapshot.session(&device.id);
        let Some(profile) = snapshot
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id && p.model_id == device.model_id)
        else {
            continue;
        };
        let Some(page) = profile.pages.iter().find(|p| p.id == session.page_id) else {
            continue;
        };
        let mut sources = Vec::new();
        if let Some(macro_id) = address["macro_id"].as_str() {
            sources.extend(
                page.bindings
                    .iter()
                    .filter(|(_, binding)| {
                        !binding.disabled
                            && uses_saved_macro(
                                &binding.action,
                                &snapshot.saved_macros,
                                macro_id,
                                0,
                            )
                    })
                    .map(|(source, _)| source.clone()),
            );
        } else if address["profile_id"].as_str() == Some(profile.id.as_str()) {
            if let (Some(page_id), Some(control)) =
                (address["page_id"].as_str(), address["control_id"].as_str())
            {
                if page_id == page.id {
                    sources.push(control.into());
                }
                if let Some(linked) = profile.linked_controls.values().find(|group| {
                    group
                        .iter()
                        .any(|member| member.page_id == page_id && member.control_id == control)
                }) {
                    sources.extend(
                        linked
                            .iter()
                            .filter(|m| m.page_id == page.id)
                            .map(|m| m.control_id.clone()),
                    );
                }
            }
        }
        sources.sort_unstable();
        sources.dedup();
        if !sources.is_empty() {
            targets.push((device.id.clone(), sources));
        }
    }
    targets
}

impl Runtime {
    /// Open a vendor inspector with the current persisted action rather than stale editor settings
    pub fn open_vendor_inspector(
        &self,
        id: &str,
        action_id: &str,
        mut address: serde_json::Value,
    ) -> Result<stream_dock_manager_plugin_host::SettingsPanel, String> {
        if !address.is_object() {
            return Err("Invalid saved settings address".into());
        }
        let _state = self.state.lock().unwrap();
        let _gate = self.profiles.transaction.lock().unwrap();
        let mut data = self.profiles.load()?;
        address["inspector_revision"] = serde_json::json!(data.configuration_revision);
        let action = saved_action_mut(&mut data, &address)
            .map_err(|_| "Save this action before opening its vendor inspector")?;
        let Action::Plugin {
            plugin_id,
            action_id: saved_id,
            settings,
            resources,
            ..
        } = action
        else {
            return Err("Save this action before opening its vendor inspector".into());
        };
        if plugin_id != id || saved_id != action_id {
            return Err("Save this action before opening its vendor inspector".into());
        }
        let paths = serde_json::to_value(crate::vendor_resources::paths(
            &self.profiles.root,
            resources,
        )?)
        .map_err(|e| e.to_string())?;
        let ids = serde_json::to_value(resources).map_err(|e| e.to_string())?;
        let mut host = self.plugins.lock().unwrap();
        if host.managed_settings(id)? {
            return Err("Managed inspectors require draft transactions".into());
        }
        let mut panel = host.settings_panel(id, Some(action_id), settings)?;
        if let Err(error) = host
            .set_panel_resources(&panel.token, ids, paths)
            .and_then(|_| host.bind_vendor_panel(&panel.token, address))
        {
            host.close_inspector(&panel.token);
            return Err(error);
        }
        panel.immediate = true;
        Ok(panel)
    }

    // Run immediate saves in arrival order and periodically collect due VSD settings batches
    pub(crate) fn start_settings_writer(runtime: &Arc<Self>) {
        let (sender, receiver) = mpsc::sync_channel::<OwnedSettingsWrite>(8);
        runtime.plugins.lock().unwrap().set_settings_writer(sender);
        let weak = Arc::downgrade(runtime);
        let worker = std::thread::spawn(move || loop {
            let request = match receiver.recv_timeout(std::time::Duration::from_millis(250)) {
                Ok(request) => Some(request),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => None,
            };
            let Some(runtime) = weak.upgrade() else { break };
            if runtime.is_stopping() {
                break;
            }
            if let Some(request) = request {
                if let Err(error) = runtime.persist_saved_plugin_settings(&request) {
                    runtime.reject_plugin_settings([&request], error);
                }
            }
            let due = runtime.plugins.lock().unwrap().due_burst_settings();
            if !due.is_empty() {
                if let Err(error) = runtime.persist_burst_settings(&due) {
                    runtime.reject_plugin_settings(&due, error);
                }
            }
        });
        *runtime.settings_writer.lock().unwrap() = Some(worker);
    }

    // Release ordered reads on failed saves and surface the persistence error
    fn reject_plugin_settings<'a>(
        &self,
        requests: impl IntoIterator<Item = &'a OwnedSettingsWrite>,
        error: String,
    ) {
        {
            let mut host = self.plugins.lock().unwrap();
            for request in requests {
                host.reject_saved_settings(request, &error);
            }
        }
        self.emit("plugin-error", None, None, Some(error));
    }

    // Persist the newest due settings for all affected bindings in one workspace transaction
    fn persist_burst_settings(&self, due: &[OwnedSettingsWrite]) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let gate = self.profiles.transaction.lock().unwrap();
        let mut committed = Vec::with_capacity(due.len());
        let mut unchanged = Vec::new();
        for request in due {
            let Snapshot {
                profiles,
                saved_macros,
                ..
            } = &mut *state;
            let valid = workspace_action_mut(profiles, saved_macros, &request.address).ok();
            match valid {
                Some(Action::Plugin {
                    plugin_id,
                    action_id,
                    settings,
                    ..
                }) if plugin_id == &request.plugin_id && action_id == &request.action_id => {
                    if *settings == request.settings
                        || *settings
                            == stream_dock_manager_plugin_host::preserve_references(
                                settings,
                                &request.settings,
                            )
                    {
                        unchanged.push(request);
                    } else {
                        committed.push(request.clone());
                    }
                }
                _ => self
                    .plugins
                    .lock()
                    .unwrap()
                    .reject_saved_settings(request, "Saved settings target changed"),
            }
        }
        let mut host = self.plugins.lock().unwrap();
        for request in unchanged {
            let _ = host.finish_unchanged_saved_settings(request);
        }
        if committed.is_empty() {
            return Ok(());
        }
        let prepared = self.profiles.prepare_import_transaction(
            state.configuration_revision,
            "Update plugin settings",
            None,
            0,
            |data| {
                for request in &mut committed {
                    request.settings = update(data, request)?;
                    sync_settings_link(data, request)?;
                }
                Ok(())
            },
        )?;
        let data =
            host.commit_burst_settings(&committed, prepared.retained_bytes()?, move |tx| {
                prepared.commit(tx)
            })?;
        if data.configuration_revision != state.configuration_revision {
            host.advance_inspectors_after_settings_save(data.configuration_revision);
        }
        Self::apply_data(&mut state, data);
        let mut targets: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for request in &committed {
            for (device, sources) in refresh_targets(&state, &request.address) {
                targets.entry(device).or_default().extend(sources);
            }
        }
        drop(host);
        drop(gate);
        drop(state);
        for sources in targets.values_mut() {
            sources.sort_unstable();
            sources.dedup();
        }
        self.publish_settings_refresh(targets);
        Ok(())
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
        // Compare with committed state before loading SQLite or staging another workspace edit
        if request.resources.is_none() {
            validate_settings_revision(state.configuration_revision, request)?;
            let Snapshot {
                profiles,
                saved_macros,
                ..
            } = &mut *state;
            if let Ok(Action::Plugin {
                plugin_id,
                action_id,
                settings,
                ..
            }) = workspace_action_mut(profiles, saved_macros, &request.address)
            {
                if *plugin_id == request.plugin_id
                    && *action_id == request.action_id
                    && (*settings == request.settings
                        || *settings
                            == stream_dock_manager_plugin_host::preserve_references(
                                settings,
                                &request.settings,
                            ))
                {
                    return self
                        .plugins
                        .lock()
                        .unwrap()
                        .finish_unchanged_saved_settings(request);
                }
            }
        }
        let mut committed_request = request.clone();
        let prepared = self.profiles.prepare_import_transaction(
            state.configuration_revision,
            "Update plugin settings",
            None,
            0,
            |data| {
                committed_request.settings = update(data, request)?;
                sync_settings_link(data, request)?;
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
        if let Some(ids) = resource_ids {
            host.advance_inspector_revision(&request.context, data.configuration_revision);
            host.update_saved_resource_ids(&request.context, ids);
        } else if data.configuration_revision != state.configuration_revision {
            host.advance_inspectors_after_settings_save(data.configuration_revision);
        }
        Self::apply_data(&mut state, data);
        let targets = refresh_targets(&state, &request.address);
        drop(host);
        drop(gate);
        drop(state);
        self.publish_settings_refresh(targets);
        Ok(())
    }

    // Refresh only screens that read a changed settings binding
    fn publish_settings_refresh(&self, targets: impl IntoIterator<Item = (String, Vec<String>)>) {
        self.emit("profiles", None, None, None);
        // A writing plugin (even an idle timer) must not restart any hardware scene
        for (device, sources) in targets {
            if let Some(epoch) = self.connection_generation(&device) {
                let _ = self
                    .device_commands
                    .send(crate::devices::Request::RefreshPluginBindings {
                        device,
                        epoch,
                        sources,
                    });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;

    #[test]
    // An active N4 timer write must never invalidate another model's Navigation scene
    fn saved_settings_refresh_targets_only_consuming_docks() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let mut snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        profile.model_id = "n4-pro".into();
        let source = profile.pages[0].id.clone();
        let mut linked = profile.pages[0].clone();
        linked.id = "linked-page".into();
        profile.linked_controls.insert(
            "linked-group".into(),
            vec![
                crate::LinkMember {
                    page_id: source.clone(),
                    control_id: "key-1".into(),
                },
                crate::LinkMember {
                    page_id: linked.id.clone(),
                    control_id: "key-2".into(),
                },
            ],
        );
        profile.pages.push(linked);
        snapshot.profiles = vec![profile.clone()];
        snapshot.devices.clear();
        for (id, model) in [
            ("timer", "n4-pro"),
            ("linked", "n4-pro"),
            ("unbound-keyboard", "k1-pro"),
        ] {
            let module = hardware_registry::modules()
                .into_iter()
                .find(|m| m.manifest().model_id == model)
                .unwrap();
            let contract = module.representative_contract().unwrap();
            snapshot.devices.push(crate::DeviceInfo {
                id: id.into(),
                persistence_key: id.into(),
                identity_port_dependent: false,
                model_id: model.into(),
                name: id.into(),
                variant_id: contract.variant_id.clone(),
                firmware: contract.firmware.clone().unwrap_or_default(),
                usb_release: None,
                connected: true,
                simulated: false,
                manifest: contract.manifest.clone(),
            });
            snapshot.sessions.insert(
                id.into(),
                crate::workspace::DeviceSession {
                    profile_id: profile.id.clone(),
                    page_id: if id == "linked" {
                        "linked-page".into()
                    } else {
                        source.clone()
                    },
                    ..Default::default()
                },
            );
        }
        assert_eq!(
            refresh_targets(
                &snapshot,
                &json!({"profile_id":profile.id,"page_id":source,"control_id":"key-1"})
            ),
            vec![
                ("timer".into(), vec!["key-1".into()]),
                ("linked".into(), vec!["key-2".into()])
            ]
        );
        assert!(refresh_targets(
            &snapshot,
            &json!({"profile_id":profile.id,"page_id":"inactive","control_id":"key-1"})
        )
        .is_empty());
        runtime.shutdown();
    }

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
        let runtime = Runtime::build_credentials(
            root.path().into(),
            false,
            crate::credentials::Credentials::with_provider(Arc::new(
                crate::vault::tests::TestKeys::default(),
            )),
        )
        .unwrap();
        runtime
            .vault
            .initialize_password("locked fixture", "locked fixture")
            .unwrap();
        runtime.vault.lock();
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
        assert!(runtime
            .open_vendor_inspector(owner, action_id, json!("invalid"))
            .is_err());
        for (index, address) in addresses.iter().enumerate() {
            assert!(runtime
                .open_vendor_inspector(owner, "foreign.action", address.clone())
                .is_err());
            let panel = runtime
                .open_vendor_inspector(owner, action_id, address.clone())
                .unwrap();
            assert!(panel.immediate);
            let initial = runtime
                .plugins
                .lock()
                .unwrap()
                .panel_value(&panel.token)
                .unwrap();
            assert_eq!(initial["settings"]["original"], true);
            assert!(initial["settings"].get("unsaved").is_none());
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
        let runtime = Runtime::build_credentials(
            root.path().into(),
            false,
            crate::credentials::Credentials::with_provider(Arc::new(
                crate::vault::tests::TestKeys::default(),
            )),
        )
        .unwrap();
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
        assert!(runtime.vault_settings_plugins().is_err());
        runtime.vault.unlock_password("locked fixture").unwrap();
        let plugin = runtime
            .vault_settings_plugins()
            .unwrap()
            .into_iter()
            .find(|p| p.id == owner)
            .unwrap();
        let row = plugin
            .rows
            .iter()
            .find(|r| r.location.contains("key-2"))
            .unwrap();
        let masked = runtime
            .vault_settings_document(row.revision.clone(), false)
            .unwrap();
        assert_eq!(masked.fields["original"], "boolean");
        assert!(masked.json.is_none());
        runtime
            .vault_save_settings(
                row.revision.clone(),
                json!({"opaque":["saved-marker-d9a8",false,null]}),
            )
            .unwrap();
        assert!(runtime
            .vault_save_settings(row.revision.clone(), json!({"stale":true}))
            .unwrap_err()
            .contains("changed"));
        let review = runtime.vault.review_keystore_reset(7).unwrap();
        runtime
            .vault
            .delete_keystore(7, &review, "CLEAR VAULT ENTRIES")
            .unwrap();
        assert_eq!(
            runtime
                .plugins
                .lock()
                .unwrap()
                .configuration(owner)
                .unwrap()
                .settings,
            json!({"global":1})
        );
        let backup = root.path().join("public.sqlite3");
        runtime.profiles.storage().backup(backup.clone()).unwrap();
        for path in [
            &backup,
            &stream_dock_manager_storage::database_path(
                root.path(),
                stream_dock_manager_storage::DATABASE_NAME,
            ),
        ] {
            assert!(!std::fs::read(path)
                .unwrap()
                .windows(17)
                .any(|w| w == b"saved-marker-d9a8"));
        }
        runtime.shutdown();
    }
    #[test]
    // Verify that base action settings commit through headless owner and survive restart
    fn base_action_settings_commit_through_headless_owner_and_survive_restart() {
        for dialect in ["native", "openaction", "streamdeck"] {
            runtime_roundtrip(dialect);
        }
    }

    #[test]
    // Verify two bursty vendor bindings share one durable write and getters see buffered settings
    fn vsd_settings_burst_batches_two_bindings_and_keeps_getters_responsive() {
        runtime_roundtrip("vsd");
    }

    fn runtime_roundtrip(dialect: &str) {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/com.fixture.settings.sdPlugin");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("runtime.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        std::fs::write(package.join("manifest.json"), json!({"UUID":"com.fixture.settings","Name":"Settings fixture","Version":"0.1.0","SDKVersion":1,"CodePath":"runtime.html","Actions":[{"UUID":"com.fixture.settings.action","Name":"Action"}]}).to_string()).unwrap();
        if dialect == "native" {
            crate::test_plugin::modernize(&package);
        } else {
            let mut manifest = json!({"UUID":"com.fixture.settings","Name":"Settings fixture","Version":"1.0.0.0","Author":"Manager tests","Description":"Test settings persistence","Icon":"icon","CodePath":"runtime.html","Software":{"MinimumVersion":"6.9"},"OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],"Actions":[{"UUID":"com.fixture.settings.action","Name":"Action","Icon":"icon","Controllers":["Keypad"],"States":[{"Image":"icon"}]}]});
            if dialect == "streamdeck" {
                manifest["SDKVersion"] = json!(2);
            } else {
                // OpenAction and VSD can declare Linux without violating the
                // native Stream Deck manifest schema
                manifest["OS"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"Platform":"linux"}));
                if dialect == "vsd" {
                    manifest["SDKVersion"] = json!(1);
                    manifest.as_object_mut().unwrap().remove("Software");
                }
            }
            std::fs::write(
                package.join("icon.svg"),
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"/>"#,
            )
            .unwrap();
            std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
        }
        if dialect == "streamdeck" && cfg!(target_os = "linux") {
            crate::test_plugin::assert_elgato_platform_blocked(&package);
            return;
        }
        let runtime = crate::test_support::headless(root.path().into()).unwrap();
        runtime
            .vault
            .initialize_password("fixture password", "fixture password")
            .unwrap();
        runtime.vault.lock();
        assert_eq!(
            runtime
                .plugins
                .lock()
                .unwrap()
                .list()
                .into_iter()
                .find(|p| p.id == "com.fixture.settings")
                .unwrap()
                .dialect,
            if dialect == "native" { "vsd" } else { dialect }
        );
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
            plugin_id: "com.fixture.settings".into(),
            action_id: "com.fixture.settings.action".into(),
            settings: json!({"old":true}),
            resources: Default::default(),
        };
        if dialect == "vsd" {
            profile.pages[0].bindings.get_mut("key-2").unwrap().action = Action::Plugin {
                user_desired_state: None,
                plugin_id: "com.fixture.settings".into(),
                action_id: "com.fixture.settings.action".into(),
                settings: json!({"second":true}),
                resources: Default::default(),
            };
        }
        runtime
            .save_profile(profile, before.configuration_revision)
            .unwrap();
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable("com.fixture.settings", true)
            .unwrap();
        crate::test_support::synchronize_plugins(&runtime);
        let url = {
            let mut host = runtime.plugins.lock().unwrap();
            host.poll();
            host.browser_view("com.fixture.settings")
                .unwrap()
                .unwrap_or_else(|| {
                    panic!(
                        "Plugin could not start: {}",
                        host.list()
                            .into_iter()
                            .find(|p| p.id == "com.fixture.settings")
                            .unwrap()
                            .status
                    )
                })
                .url
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
                (if dialect == "native" {
                    crate::test_plugin::registration(&package, parts[4])
                } else {
                    json!({"event":"registerPlugin","uuid":parts[4]})
                })
                .to_string()
                .into(),
            ))
            .unwrap();
        let mut first = None;
        let mut second = None;
        while first.is_none() || (dialect == "vsd" && second.is_none()) {
            let message = socket.read().unwrap();
            let Ok(text) = message.to_text() else {
                continue;
            };
            let value: Value = serde_json::from_str(text).unwrap();
            if value["event"] == "willAppear" {
                let context = value["context"].as_str().unwrap().to_owned();
                if dialect == "vsd" && value["payload"]["settings"]["second"] == true {
                    second = Some(context);
                } else {
                    first = Some(context);
                }
            }
        }
        let context = first.unwrap();
        let settings = if dialect == "openaction" {
            json!([false,0,null,{"unknown":"kept"}])
        } else {
            json!({"unknown":{"kept":null},"zero":0,"disabled":false})
        };
        socket
            .send(tungstenite::Message::Text(
                json!({"event":"setSettings","context":context,"payload":settings})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        // Vendor transports may suppress the originating writer's unsolicited event.
        // Poll the documented getter until the asynchronous workspace commit publishes.
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            socket
                .send(tungstenite::Message::Text(
                    json!({"event":"getSettings","context":context,"id":"committed"})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            let message = socket
                .read()
                .unwrap_or_else(|err| panic!("{dialect} getSettings: {err}"));
            if let Ok(text) = message.to_text() {
                let v: Value = serde_json::from_str(text).unwrap();
                if v["event"] == "didReceiveSettings"
                    && v["id"] == "committed"
                    && v["payload"]["settings"] == settings
                {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{dialect} settings did not commit"
            );
            std::thread::sleep(Duration::from_millis(15));
        }
        // Repeated writes must release ordered getters without advancing the SQLite commit counter
        let commit_sequence = || {
            runtime
                .profiles
                .storage()
                .run(|conn| {
                    conn.query_row(
                        "SELECT CAST(value AS INTEGER) FROM database_metadata WHERE key='commit_sequence'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|error| error.to_string())
                })
                .unwrap()
        };
        let before = commit_sequence();
        let revision = runtime.snapshot().configuration_revision;
        for index in 0..3 {
            let id = format!("duplicate-{index}");
            for message in [
                json!({"event":"setSettings","context":context,"payload":settings}),
                json!({"event":"getSettings","context":context,"id":id}),
            ] {
                socket
                    .send(tungstenite::Message::Text(message.to_string().into()))
                    .unwrap();
            }
            loop {
                let message = socket.read().expect("duplicate settings getter response");
                if let Ok(text) = message.to_text() {
                    let value: Value = serde_json::from_str(text).unwrap();
                    if value["event"] == "didReceiveSettings" && value["id"] == id {
                        assert_eq!(value["payload"]["settings"], settings);
                        break;
                    }
                }
            }
        }
        assert_eq!(runtime.snapshot().configuration_revision, revision);
        assert_eq!(
            commit_sequence(),
            before,
            "{dialect} duplicate wrote to SQLite"
        );
        if let Some(second) = second {
            for (target, payload) in [
                (&context, json!({"timer":{"first":true},"conflict":"old"})),
                (&second, json!({"counter":17})),
                (&context, json!({"timer":{"latest":true},"conflict":"new"})),
            ] {
                socket
                    .send(tungstenite::Message::Text(
                        json!({"event":"setSettings","context":target,"payload":payload})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
            }
            let mut expected = settings.clone();
            expected["timer"] = json!({"first":true,"latest":true});
            expected["conflict"] = json!("new");
            for (target, value) in [(&context, &expected), (&second, &json!({"counter":17}))] {
                socket
                    .send(tungstenite::Message::Text(
                        json!({"event":"getSettings","context":target,"id":"during-burst"})
                            .to_string()
                            .into(),
                    ))
                    .unwrap();
                loop {
                    let msg = socket
                        .read()
                        .expect("getter must not wait until the burst flush");
                    let Ok(text) = msg.to_text() else { continue };
                    let response: Value = serde_json::from_str(text).unwrap();
                    if response["id"] == "during-burst" {
                        assert_eq!(&response["payload"]["settings"], value);
                        break;
                    }
                }
            }
            assert_eq!(commit_sequence(), before);
            let deadline = std::time::Instant::now() + Duration::from_secs(13);
            while commit_sequence() == before {
                assert!(
                    std::time::Instant::now() < deadline,
                    "VSD tail never committed"
                );
                std::thread::sleep(Duration::from_millis(40));
            }
            let stored = runtime.profiles.load().unwrap();
            let page = &stored
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .unwrap()
                .pages[0];
            for (control, value) in [("key-1", expected), ("key-2", json!({"counter":17}))] {
                let Action::Plugin { settings, .. } = &page.bindings[control].action else {
                    panic!("Plugin binding changed");
                };
                assert_eq!(settings, &value);
            }
            assert_eq!(commit_sequence(), before + 1);
            assert_eq!(stored.configuration_revision, revision + 1);
            runtime.shutdown();
            return;
        }
        socket
            .send(tungstenite::Message::Text(
                json!({"event":"setGlobalSettings","context":context,"payload":{"key":false}})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        for (event, expected) in [
            ("getSettings", settings.clone()),
            ("getGlobalSettings", json!({"key":false})),
        ] {
            socket
                .send(tungstenite::Message::Text(
                    json!({"event":event,"context":context,"id":event})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            loop {
                let response = socket.read().unwrap();
                if let Ok(text) = response.to_text() {
                    let v: Value = serde_json::from_str(text).unwrap();
                    if v["id"] == event {
                        assert_eq!(v["payload"]["settings"], expected);
                        break;
                    }
                }
            }
        }
        if dialect == "native" {
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
        assert_eq!(
            reopened
                .plugins
                .lock()
                .unwrap()
                .configuration("com.fixture.settings")
                .unwrap()
                .settings,
            json!({"key":false})
        );
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
        if dialect == "native" {
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
        }
        reopened.shutdown();
    }
}
