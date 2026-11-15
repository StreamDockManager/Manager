//! Imported profile typography belongs to the ephemeral binding appearance
use super::*;
use hardware_core::plugin_display::VendorProfileAppearance;

impl PluginHost {
    /// Configure before synchronize so willAppear and the first pixels agree
    #[allow(clippy::too_many_arguments)]
    pub fn configure_profile_appearance(
        &self,
        id: &str,
        action: &str,
        scope: &str,
        device: &str,
        control: &str,
        appearance: Option<VendorProfileAppearance>,
    ) -> Result<(), String> {
        if let Some(value) = &appearance {
            value.validate()?;
        }
        let mut hub = self.hub.lock().unwrap();
        if let Some(value) = &appearance {
            let states = hub
                .metadata
                .get(id)
                .and_then(|m| m.get(action))
                .and_then(|a| a["States"].as_array())
                .ok_or("Imported profile action states are unavailable")?;
            if value.states.len() != states.len() {
                return Err("Imported profile states do not match the current action".into());
            }
        }
        let mut context = hub.binding_context(id, scope, device, control);
        if hub
            .context_state
            .visible
            .get(&context)
            .is_some_and(|event| event["action"] != action)
        {
            context = hub.renew_binding_context(id, scope, device, control);
        }
        let removed = appearance.is_none()
            && hub
                .context_state
                .vendor_profile_appearances
                .contains_key(&context);
        match appearance {
            Some(value) => {
                hub.context_state
                    .vendor_profile_appearances
                    .insert(context.clone(), value);
            }
            None => {
                hub.context_state
                    .vendor_profile_appearances
                    .remove(&context);
            }
        }
        // Refresh typography without resetting a running plugin's selected state
        if let Some(state) = hub
            .visual_state
            .visuals
            .get(&context)
            .and_then(|v| v["state"].as_u64())
        {
            if let Some(parameters) = hub
                .context_state
                .vendor_profile_appearances
                .get(&context)
                .and_then(|p| p.parameters(state as u32))
                .or_else(|| {
                    removed.then(|| {
                        hub.metadata
                            .get(id)
                            .and_then(|m| m.get(action))
                            .and_then(|a| a["States"].get(state as usize))
                            .map(visual::manifest_title_parameters)
                            .unwrap_or(json!({}))
                    })
                })
            {
                let visual = Arc::make_mut(hub.visual_state.visuals.get_mut(&context).unwrap());
                if visual["titleParameters"] != parameters {
                    visual["titleParameters"] = parameters;
                    hub.visual_state.render_generation =
                        hub.visual_state.render_generation.wrapping_add(1);
                }
                publish(&mut hub, id, &context);
            }
        }
        Ok(())
    }
}

// Resolve the initial visual state of the binding
pub(super) fn initial_state(hub: &Hub, context: &str) -> u32 {
    hub.context_state
        .vendor_profile_appearances
        .get(context)
        .map_or(0, |p| p.initial_state)
}

// Publish for the current operation
pub(super) fn publish(hub: &mut Hub, owner: &str, context: &str) {
    let Some(visual) = hub.visual_state.visuals.get(context) else {
        return;
    };
    let value = json!({"title":visual["title"].as_str().unwrap_or(""), "titleParameters":visual["titleParameters"], "state":visual["state"]});
    if hub.context_state.title_parameters.get(context) == Some(&value) {
        return;
    }
    let settings = hub
        .context_state
        .settings
        .get(context)
        .cloned()
        .unwrap_or(json!({}));
    let mut event = hub.settings_event(context, &settings);
    event["event"] = json!("titleParametersDidChange");
    event["payload"]
        .as_object_mut()
        .unwrap()
        .remove("isInMultiAction");
    for key in ["title", "titleParameters", "state"] {
        event["payload"][key] = value[key].clone();
    }
    if let Some(visible) = hub.context_state.visible.get_mut(context) {
        visible["payload"]["state"] = value["state"].clone();
    }
    hub.context_state
        .title_parameters
        .insert(context.into(), value);
    hub.sessions
        .queues
        .entry(owner.into())
        .or_default()
        .push_back(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hardware_core::plugin_display::VendorTitleStyle;
    #[test]
    // Verify imported states seed once project events and retire
    fn imported_states_seed_once_project_events_and_retire() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("plugin"), "fixture.profile");
        let mut host = lifecycle_tests::host(root.path());
        host.enable("fixture.profile", true).unwrap();
        let states = json!({"States":[{"Title":"First"},{"Title":"Second"}]});
        let plugin = host.plugins.get_mut("fixture.profile").unwrap();
        plugin.info.runtime.state = RuntimeState::Running;
        plugin.info.capabilities = None;
        Arc::make_mut(&mut plugin.metadata).insert("fixture.profile.action".into(), states.clone());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.capabilities.remove("fixture.profile");
            Arc::make_mut(hub.metadata.get_mut("fixture.profile").unwrap())
                .insert("fixture.profile.action".into(), states);
        }
        let action = || ActiveAction {
            plugin_id: "fixture.profile".into(),
            action_id: "fixture.profile.action".into(),
            device_id: "device".into(),
            control_id: "key".into(),
            scope: "scope".into(),
            settings: json!({}),
            surface: None,
            column: 0,
            row: 0,
            encoder: false,
        };
        let appearance = VendorProfileAppearance {
            linked_title: true,
            initial_state: 1,
            states: vec![
                VendorTitleStyle {
                    font_family: "".into(),
                    font_size: 12.0,
                    font_style: "Regular".into(),
                    font_underline: false,
                    outline_thickness: 0.0,
                    show_title: true,
                    title_alignment: "top".into(),
                    title_color: "#ffffff".into(),
                },
                VendorTitleStyle {
                    font_family: "".into(),
                    font_size: 24.0,
                    font_style: "Bold Italic".into(),
                    font_underline: true,
                    outline_thickness: 2.0,
                    show_title: false,
                    title_alignment: "bottom".into(),
                    title_color: "#ff0000".into(),
                },
            ],
        };
        host.configure_profile_appearance(
            "fixture.profile",
            "fixture.profile.action",
            "scope",
            "device",
            "key",
            Some(appearance.clone()),
        )
        .unwrap();
        host.synchronize(vec![action()], vec![]);
        let context;
        {
            let mut hub = host.hub.lock().unwrap();
            context = hub
                .lookup_binding_context("fixture.profile", "scope", "device", "key")
                .unwrap();
            assert_eq!(hub.visual_state.visuals[&context]["state"], 1);
            assert_eq!(
                hub.visual_state.visuals[&context]["titleParameters"],
                appearance.parameters(1).unwrap()
            );
            let events = &hub.sessions.queues["fixture.profile"];
            assert!(events
                .iter()
                .any(|e| e["event"] == "willAppear" && e["payload"]["state"] == 1));
            assert!(events
                .iter()
                .any(|e| e["event"] == "titleParametersDidChange"
                    && e["payload"]["titleParameters"] == appearance.parameters(1).unwrap()));
            visual::prepare(
                &mut hub,
                "fixture.profile",
                &mut json!({"event":"setState","context":context,"payload":{"state":0}}),
            )
            .unwrap();
            assert_eq!(
                hub.visual_state.visuals[&context]["titleParameters"],
                appearance.parameters(0).unwrap()
            );
            let event = hub.sessions.queues["fixture.profile"]
                .iter()
                .last()
                .unwrap();
            assert_eq!(event["event"], "titleParametersDidChange");
            assert_eq!(event["payload"]["state"], 0);
        }
        host.configure_profile_appearance(
            "fixture.profile",
            "fixture.profile.action",
            "scope",
            "device",
            "key",
            Some(appearance.clone()),
        )
        .unwrap();
        host.synchronize(vec![action()], vec![]);
        assert_eq!(
            host.hub.lock().unwrap().visual_state.visuals[&context]["state"],
            0
        );
        host.configure_profile_appearance(
            "fixture.profile",
            "fixture.profile.action",
            "scope",
            "device",
            "key",
            None,
        )
        .unwrap();
        {
            let hub = host.hub.lock().unwrap();
            assert_eq!(
                hub.visual_state.visuals[&context]["titleParameters"],
                json!({})
            );
            assert_eq!(
                hub.sessions.queues["fixture.profile"]
                    .iter()
                    .last()
                    .unwrap()["payload"]["titleParameters"],
                json!({})
            );
        }
        host.configure_profile_appearance(
            "fixture.profile",
            "fixture.profile.action",
            "scope",
            "device",
            "key",
            Some(appearance.clone()),
        )
        .unwrap();
        host.synchronize(vec![], vec![]);
        assert!(!host
            .hub
            .lock()
            .unwrap()
            .context_state
            .vendor_profile_appearances
            .contains_key(&context));
        {
            let mut hub = host.hub.lock().unwrap();
            Arc::make_mut(hub.metadata.get_mut("fixture.profile").unwrap())
                .insert("fixture.profile.action".into(), json!({"States":[{}]}));
        }
        assert!(host
            .configure_profile_appearance(
                "fixture.profile",
                "fixture.profile.action",
                "scope",
                "device",
                "key",
                Some(appearance)
            )
            .is_err());
    }
}
