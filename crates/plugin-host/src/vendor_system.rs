//! Vendor OS events are delivered only from real OS sources, to live opted-in plugins
use super::*;
impl Hub {
    // Notify the demand worker after authoritative enabled or connected state changes
    pub(super) fn notify_vendor_demand(&self) {
        if let Some(wake) = &self.vendor_demand_wake {
            wake();
        }
    }
}
impl PluginHost {
    // Install a nonblocking demand notification without exposing host state to the callback
    pub fn set_vendor_demand_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        let mut hub = self.hub.lock().unwrap();
        hub.vendor_demand_wake = Some(wake);
        hub.notify_vendor_demand();
    }

    /// A settings window is opened only by an explicit first-party user request
    pub fn show_settings_interface(&self, id: &str) -> Result<(), String> {
        self.recovery.check()?;
        let plugin = self
            .plugins
            .get(id)
            .ok_or("Plugin is no longer installed")?;
        if !plugin.info.has_settings_interface || plugin.info.dialect != "openaction" {
            return Err("Plugin does not provide an OpenAction settings interface".into());
        }
        let mut hub = self.hub.lock().unwrap();
        if !hub.enabled.contains(id) || !hub.sessions.connected.contains(&plugin.token) {
            return Err(
                "Enable the plugin and wait for it to connect before opening its settings".into(),
            );
        }
        hub.sessions
            .queues
            .entry(id.into())
            .or_default()
            .push_back(json!({"event":"showSettingsInterface"}));
        Ok(())
    }

    // Receive deep link
    pub fn receive_deep_link(&self, url: &str) -> Result<bool, String> {
        let (id, message) = parse_deep_link(url)?;
        let plugin = if url.starts_with("openaction:") {
            // OpenAction addresses the installed directory, not the manifest UUID
            self.plugins
                .values()
                .find(|plugin| plugin.path.file_name().and_then(|name| name.to_str()) == Some(id))
        } else {
            self.plugins.get(id)
        };
        let Some(plugin) = plugin else {
            return Ok(false);
        };
        let id = plugin.info.id.as_str();
        let mut hub = self.hub.lock().unwrap();
        if !hub.enabled.contains(id) || !hub.sessions.connected.contains(&plugin.token) {
            return Ok(false);
        };
        hub.sessions
            .queues
            .entry(id.into())
            .or_default()
            .push_back(json!({"event":"didReceiveDeepLink","payload":{"url":message}}));
        Ok(true)
    }
    #[allow(clippy::too_many_arguments)]
    // Update title parameters
    pub fn update_title_parameters(
        &self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
        title: &str,
        parameters: Value,
    ) -> Result<bool, String> {
        if title.len() > 4096 || !parameters.is_object() || parameters.to_string().len() > 8192 {
            return Err("Invalid title parameters".into());
        }
        let mut hub = self.hub.lock().unwrap();
        let Some(context) = hub.lookup_binding_context(id, scope, device, control) else {
            return Ok(false);
        };
        if hub.context_state.contexts.get(&context).map(String::as_str) != Some(id)
            || !hub.context_state.visible.contains_key(&context)
        {
            return Ok(false);
        };
        let value = json!({"title":title,"titleParameters":parameters});
        if hub.context_state.title_parameters.get(&context) == Some(&value) {
            return Ok(false);
        };
        let settings = hub
            .context_state
            .settings
            .get(&context)
            .cloned()
            .unwrap_or(json!({}));
        let mut event = hub.settings_event(&context, &settings);
        event["event"] = json!("titleParametersDidChange");
        event["payload"]
            .as_object_mut()
            .unwrap()
            .remove("isInMultiAction");
        event["payload"]["title"] = json!(title);
        event["payload"]["titleParameters"] = value["titleParameters"].clone();
        if let Some(state) = hub
            .visual_state
            .visuals
            .get(&context)
            .and_then(|v| v["state"].as_u64())
        {
            event["payload"]["state"] = json!(state);
        }
        hub.context_state.title_parameters.insert(context, value);
        hub.sessions
            .queues
            .entry(id.into())
            .or_default()
            .push_back(event);
        Ok(true)
    }
    // Hide title parameters
    pub fn hide_title_parameters(
        &self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
        title: &str,
    ) -> Result<bool, String> {
        let parameters = {
            let hub = self.hub.lock().unwrap();
            let Some(context) = hub.lookup_binding_context(id, scope, device, control) else {
                return Ok(false);
            };
            let Some(cached) = hub.context_state.title_parameters.get(&context) else {
                return Ok(false);
            };
            let mut parameters = cached["titleParameters"].clone();
            parameters["showTitle"] = json!(false);
            parameters
        };
        self.update_title_parameters(id, scope, device, control, title, parameters)
    }
    // Snapshot OS-event demand from the same enabled, connected owners used for delivery
    pub fn vendor_os_event_demand(&self) -> (Vec<String>, bool) {
        let hub = self.hub.lock().unwrap();
        let mut applications = std::collections::BTreeSet::new();
        let mut wake = false;
        for plugin in self.plugins.values().filter(|plugin| {
            hub.enabled.contains(&plugin.info.id) && hub.sessions.connected.contains(&plugin.token)
        }) {
            // Wake is implicit for live protocol sessions; application names are explicit manifest subscriptions
            wake = true;
            if let Some(names) = plugin.manifest["ApplicationsToMonitor"][sdk_platform()].as_array()
            {
                applications.extend(names.iter().filter_map(Value::as_str).map(str::to_owned));
            }
        }
        (applications.into_iter().collect(), wake)
    }

    // Collect application names requested by currently live event recipients
    pub fn monitored_applications(&self) -> Vec<String> {
        self.vendor_os_event_demand().0
    }
    // Emit application event
    pub fn emit_application_event(&self, event: &str, application: &str) -> usize {
        if !matches!(event, "applicationDidLaunch" | "applicationDidTerminate")
            || application.len() > 256
        {
            return 0;
        }
        self.emit_vendor_system(
            json!({"event":event,"payload":{"application":application}}),
            Some(application),
        )
    }
    // Emit system wake
    pub fn emit_system_wake(&self) -> usize {
        self.emit_vendor_system(json!({"event":"systemDidWakeUp"}), None)
    }
    // Emit vendor system
    fn emit_vendor_system(&self, event: Value, application: Option<&str>) -> usize {
        let mut hub = self.hub.lock().unwrap();
        let mut count = 0;
        for plugin in self.plugins.values() {
            if !hub.enabled.contains(&plugin.info.id)
                || !hub.sessions.connected.contains(&plugin.token)
            {
                continue;
            }
            if let Some(application) = application {
                let monitored = plugin.manifest["ApplicationsToMonitor"][sdk_platform()]
                    .as_array()
                    .is_some_and(|apps| {
                        apps.iter().filter_map(Value::as_str).any(|name| {
                            if cfg!(windows) {
                                name.eq_ignore_ascii_case(application)
                            } else {
                                name == application
                            }
                        })
                    });
                if !monitored {
                    continue;
                }
            }
            hub.sessions
                .queues
                .entry(plugin.info.id.clone())
                .or_default()
                .push_back(event.clone());
            count += 1;
        }
        count
    }
}

// Parse deep link
fn parse_deep_link(url: &str) -> Result<(&str, &str), String> {
    if url.len() > 16 * 1024 || url.chars().any(char::is_control) {
        return Err("Invalid plugin link".into());
    }
    let (id, message) = url
        .strip_prefix("streamdeck://plugins/message/")
        .or_else(|| url.strip_prefix("openaction://plugins/message/"))
        .and_then(|path| path.split_once('/'))
        .ok_or("Unsupported plugin link")?;
    if !safe_id(id) || id.contains('%') {
        return Err("Invalid plugin link identity".into());
    }
    Ok((id, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Registration and revocation wake demand while rejected duplicate registration does not
    fn vendor_demand_signal_tracks_authoritative_sessions() {
        let notifications = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let changed = notifications.clone();
        let mut hub = Hub {
            vendor_demand_wake: Some(Arc::new(move || {
                changed.fetch_add(1, Ordering::SeqCst);
            })),
            ..Hub::default()
        };
        hub.sessions
            .tokens
            .insert("session".into(), "fixture".into());
        let poll = mio::Poll::new().unwrap();
        let wake = Arc::new(mio::Waker::new(poll.registry(), mio::Token(1)).unwrap());
        let request = json!({"event":"registerPlugin","uuid":"session"});
        assert!(transport::register(&mut hub, &request, wake.clone()).is_some());
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        assert!(transport::register(&mut hub, &request, wake).is_none());
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        hub.revoke_plugin_sessions("fixture", "session");
        assert_eq!(notifications.load(Ordering::SeqCst), 2);
        assert!(!hub.sessions.connected.contains("session"));
    }
    #[test]
    // Verify deep links have bounded exact owner and opaque message
    fn deep_links_have_bounded_exact_owner_and_opaque_message() {
        assert_eq!(
            parse_deep_link("streamdeck://plugins/message/org.vendor.plugin/a%2Fb?x=1").unwrap(),
            ("org.vendor.plugin", "a%2Fb?x=1")
        );
        assert_eq!(
            parse_deep_link("openaction://plugins/message/vendor.sdPlugin/a%2Fb?openaction=hidden")
                .unwrap(),
            ("vendor.sdPlugin", "a%2Fb?openaction=hidden")
        );
        for url in [
            "https://plugins/message/org.vendor.plugin/a",
            "streamdeck://plugins/message/../a",
            "streamdeck://plugins/message/org%2Evendor/a",
            "streamdeck://plugins/message/org.vendor.plugin/a\n",
        ] {
            assert!(parse_deep_link(url).is_err(), "{url}");
        }
        assert!(parse_deep_link(&format!(
            "streamdeck://plugins/message/org.vendor.plugin/{}",
            "x".repeat(17000)
        ))
        .is_err());
    }
    #[test]
    // Verify system and deep link events are scoped to enabled registered owners
    fn system_and_deep_link_events_are_scoped_to_enabled_registered_owners() {
        let root = tempfile::tempdir().unwrap();
        lifecycle_tests::package(&root.path().join("fixture"), "fixture.vendor");
        let mut host = lifecycle_tests::host(root.path());
        host.plugins.get_mut("fixture.vendor").unwrap().manifest["ApplicationsToMonitor"] =
            json!({(sdk_platform()):["org.test.application"]});
        assert_eq!(host.emit_system_wake(), 0);
        assert_eq!(host.vendor_os_event_demand(), (vec![], false));
        assert!(!host
            .receive_deep_link("streamdeck://plugins/message/fixture.vendor/message")
            .unwrap());
        host.enable("fixture.vendor", true).unwrap();
        assert_eq!(host.vendor_os_event_demand(), (vec![], false));
        host.plugins.get_mut("fixture.vendor").unwrap().token = "fixture-session".into();
        host.hub
            .lock()
            .unwrap()
            .sessions
            .connected
            .insert("fixture-session".into());
        assert_eq!(host.monitored_applications(), ["org.test.application"]);
        assert!(host.vendor_os_event_demand().1);
        assert_eq!(
            host.emit_application_event("applicationDidLaunch", "other.application"),
            0
        );
        assert_eq!(
            host.emit_application_event("keyDown", "org.test.application"),
            0
        );
        assert_eq!(
            host.emit_application_event("applicationDidLaunch", "org.test.application"),
            1
        );
        assert_eq!(host.emit_system_wake(), 1);
        assert!(host
            .receive_deep_link("streamdeck://plugins/message/fixture.vendor/message?value=1")
            .unwrap());
        assert!(!host
            .receive_deep_link("openaction://plugins/message/fixture.vendor/wrong-owner")
            .unwrap());
        assert!(host
            .receive_deep_link(
                "openaction://plugins/message/fixture/directory-message?openaction=hidden"
            )
            .unwrap());
        let hub = host.hub.lock().unwrap();
        assert!(hub.sessions.queues["fixture.vendor"]
            .iter()
            .any(|event| event["payload"]["url"] == "directory-message?openaction=hidden"));
        assert!(hub.sessions.queues["fixture.vendor"]
            .iter()
            .any(|event| event["event"] == "didReceiveDeepLink"
                && event["payload"]["url"] == "message?value=1"));
        drop(hub);
        host.hub
            .lock()
            .unwrap()
            .sessions
            .connected
            .remove("fixture-session");
        assert_eq!(host.vendor_os_event_demand(), (vec![], false));
        assert_eq!(host.emit_system_wake(), 0);
        assert_eq!(
            host.emit_application_event("applicationDidLaunch", "org.test.application"),
            0
        );
        host.hub
            .lock()
            .unwrap()
            .sessions
            .connected
            .insert("fixture-session".into());
        assert_eq!(host.monitored_applications(), ["org.test.application"]);
        assert_eq!(host.emit_system_wake(), 1);
        // Live sessions retain implicit wake demand even without application-name subscriptions
        host.plugins.get_mut("fixture.vendor").unwrap().manifest["ApplicationsToMonitor"] =
            json!({});
        assert_eq!(host.vendor_os_event_demand(), (vec![], true));
        host.hub.lock().unwrap().enabled.remove("fixture.vendor");
        assert_eq!(host.vendor_os_event_demand(), (vec![], false));
        assert_eq!(host.emit_system_wake(), 0);
    }
}
