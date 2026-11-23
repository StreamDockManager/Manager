//! Runtime owns saved binding settings; Hub owns only their live projection and inspector drafts
use super::*;
use stream_dock_manager_storage::rusqlite::Transaction;

#[derive(Clone, Debug)]
pub struct OwnedSettingsWrite {
    pub operation_id: String,
    pub plugin_id: String,
    pub action_id: String,
    pub context: String,
    pub address: Value,
    pub settings: Value,
    pub session: String,
    pub resources: Option<Value>,
    /// Keep already-authorized snapshot bytes alive until this write completes
    pub resource_snapshots: Vec<Arc<tempfile::TempDir>>,
}
impl Hub {
    // Revoke storage session
    pub(super) fn revoke_storage_session(&mut self, token: &str) {
        self.sessions.revoke_storage(token);
        self.settings_bursts.revoke_session(token);
        self.forget_saved_reads_for_session(token);
        self.pending_settings
            .retain(|_, (session, _)| session != token);
    }
    // Queue saved settings
    pub(super) fn queue_saved_settings(
        &mut self,
        owner: &str,
        session: &str,
        context: &str,
        settings: Value,
    ) -> Result<(), String> {
        self.queue_owned_write(owner, session, context, settings, None)
    }
    // Queue owned write
    pub(super) fn queue_owned_write(
        &mut self,
        owner: &str,
        session: &str,
        context: &str,
        settings: Value,
        resources: Option<Value>,
    ) -> Result<(), String> {
        let action_id = self
            .context_state
            .visible
            .get(context)
            .and_then(|v| v["action"].as_str())
            .or_else(|| {
                self.context_state
                    .context_actions
                    .get(context)
                    .map(String::as_str)
            })
            .unwrap_or("")
            .to_owned();
        let operation_id = uuid::Uuid::new_v4().to_string();
        let request = OwnedSettingsWrite {
            operation_id: operation_id.clone(),
            plugin_id: owner.into(),
            action_id,
            context: context.into(),
            address: self
                .context_state
                .addresses
                .get(context)
                .cloned()
                .unwrap_or(Value::Null),
            settings,
            resources,
            resource_snapshots: self
                .sessions
                .resource_selections
                .get(session)
                .map(|grants| grants.batches.clone())
                .unwrap_or_default(),
            session: session.into(),
        };
        let burst_eligible = request.resources.is_none()
            && !request.address.is_null()
            && self
                .manifest_dialects
                .get(owner)
                .is_some_and(|dialect| dialect == "vsd")
            && !self.capabilities.contains_key(owner)
            && self
                .sessions
                .files
                .get(session)
                .is_none_or(|grant| grant.context.is_none());
        let request = if burst_eligible {
            let Some(request) = self
                .settings_bursts
                .offer(request, std::time::Instant::now())?
            else {
                return Ok(());
            };
            request
        } else {
            request
        };
        let override_target =
            (!burst_eligible && request.resources.is_none()).then(|| request.address.clone());
        if self.pending_settings.len() >= 64
            || self
                .pending_settings
                .values()
                .filter(|(s, _)| s == session)
                .count()
                >= 8
        {
            return Err("Settings queue is full".into());
        }
        let sender = self
            .settings_writer
            .as_ref()
            .ok_or("Saved settings writer is unavailable")?;
        sender
            .try_send(request)
            .map_err(|_| "Saved settings queue is unavailable")?;
        self.pending_settings
            .insert(operation_id, (session.into(), context.into()));
        if let Some(address) = override_target {
            self.settings_bursts.discard_target(owner, &address);
        }
        Ok(())
    }
    // Build the settings event sent to a plugin
    pub(super) fn settings_event(&self, context: &str, settings: &Value) -> Value {
        let peer = self.context_state.contexts.get(context).and_then(|owner| {
            let mut peers = self.saved_context_peers(context, owner);
            peers.sort();
            peers
                .into_iter()
                .find(|peer| self.context_state.visible.contains_key(peer))
        });
        let mut event = self
            .context_state
            .visible
            .get(context)
            .or_else(|| {
                peer.as_ref()
                    .and_then(|peer| self.context_state.visible.get(peer))
            })
            .cloned()
            .unwrap_or(json!({"context":context}));
        event["context"] = json!(context);
        event["event"] = json!("didReceiveSettings");
        if event.get("action").is_none() {
            if let Some(action) = self
                .context_state
                .context_actions
                .get(context)
                .or_else(|| self.configuration_actions.get(context))
                .filter(|action| !action.is_empty())
            {
                event["action"] = json!(action);
            }
        }
        if !event["payload"].is_object() {
            event["payload"] = json!({});
        }
        if event.get("device").is_none() {
            event["device"] = json!(self
                .context_state
                .targets
                .get(context)
                .map(|(device, _)| device.as_str())
                .unwrap_or(""));
        }
        if event["payload"].get("coordinates").is_none() {
            event["payload"]["coordinates"] = json!({"row":0,"column":0});
        }
        if event["payload"].get("isInMultiAction").is_none() {
            event["payload"]["isInMultiAction"] = json!(self
                .context_state
                .scopes
                .get(context)
                .is_some_and(|scope| scope.contains('@')));
        }
        event["payload"]["settings"] = settings.clone();
        event
    }
    // Route a reply to the inspector that owns the request
    pub(super) fn route_property_inspector_reply(
        &mut self,
        owner: &str,
        context: &str,
        message: &Value,
    ) {
        let peers = self.saved_context_peers(context, owner);
        let recipients: Vec<_> = self
            .sessions
            .files
            .iter()
            .filter_map(|(token, grant)| {
                grant
                    .context
                    .as_ref()
                    .filter(|c| peers.contains(c))
                    .map(|c| (format!("pi:{token}"), c.clone()))
            })
            .collect();
        for (recipient, context) in recipients {
            let mut response = message.clone();
            response["context"] = json!(context);
            self.sessions
                .queues
                .entry(recipient)
                .or_default()
                .push_back(response);
        }
    }
    // Resolve the inspector’s current live action context
    pub(super) fn inspector_live_context(&self, context: &str, owner: &str) -> Option<String> {
        if self.capabilities.contains_key(owner)
            || !self.context_state.addresses.contains_key(context)
        {
            return Some(context.into());
        }
        self.saved_context_peers(context, owner)
            .into_iter()
            .find(|peer| self.context_state.visible.contains_key(peer))
    }
    // Find live contexts sharing the saved settings address
    pub(super) fn saved_context_peers(&self, context: &str, owner: &str) -> Vec<String> {
        let address = self.context_state.addresses.get(context);
        self.context_state
            .contexts
            .iter()
            .filter(|(candidate, id)| {
                id.as_str() == owner
                    && (candidate.as_str() == context
                        || self
                            .context_state
                            .addresses
                            .get(*candidate)
                            .zip(address)
                            .is_some_and(|(a, b)| {
                                ["profile_id", "page_id", "control_id", "macro_id", "path"]
                                    .iter()
                                    .all(|key| a[*key] == b[*key])
                            }))
            })
            .map(|(context, _)| context.clone())
            .collect()
    }
    // Publish settings
    pub(super) fn publish_settings(
        &mut self,
        owner: &str,
        context: &str,
        settings: &Value,
        session: &str,
    ) {
        let managed = self.capabilities.contains_key(owner);
        let settings = if managed {
            self.execution_settings(context, settings)
        } else {
            settings.clone()
        };
        let peers = if managed {
            vec![context.into()]
        } else {
            self.saved_context_peers(context, owner)
        };
        let inspector = self
            .sessions
            .files
            .get(session)
            .is_some_and(|g| g.context.is_some());
        for peer in &peers {
            if !managed {
                self.context_state
                    .settings
                    .insert(peer.clone(), settings.clone());
                self.context_state
                    .configured
                    .insert(peer.clone(), settings.clone());
            }
            let event = self.settings_event(peer, &settings);
            if managed || (inspector && self.context_state.visible.contains_key(peer)) {
                let runtime_settings = self.execution_settings(peer, &settings);
                let runtime_event = self.settings_event(peer, &runtime_settings);
                self.sessions
                    .queues
                    .entry(owner.into())
                    .or_default()
                    .push_back(runtime_event);
            }
            for (token, grant) in &self.sessions.files {
                if grant.context.as_ref() == Some(peer) && (managed || token != session) {
                    self.sessions
                        .queues
                        .entry(format!("pi:{token}"))
                        .or_default()
                        .push_back(event.clone());
                }
            }
        }
    }
    // Publish the committed workspace settings to the live context and its peers
    fn accept_settings(&mut self, request: &OwnedSettingsWrite) {
        self.context_state
            .settings
            .insert(request.context.clone(), request.settings.clone());
        self.context_state
            .configured
            .insert(request.context.clone(), request.settings.clone());
        self.publish_settings(
            &request.plugin_id,
            &request.context,
            &request.settings,
            &request.session,
        );
    }

    // Verify that the original session and action still own the exact saved target
    fn owns_saved_write(&self, request: &OwnedSettingsWrite) -> bool {
        self.sessions.tokens.get(&request.session) == Some(&request.plugin_id)
            && self.context_state.contexts.get(&request.context) == Some(&request.plugin_id)
            && self.context_state.context_actions.get(&request.context) == Some(&request.action_id)
            && self.context_state.addresses.get(&request.context) == Some(&request.address)
            && !self.context_state.drafts.contains(&request.context)
            && !self
                .context_state
                .transient_scopes
                .contains_key(&request.context)
    }

    // Close out a queued write only if its original session still owns the target
    fn take_saved_write(&mut self, request: &OwnedSettingsWrite) -> Result<(), String> {
        let pending = self.pending_settings.remove(&request.operation_id);
        if pending.as_ref() != Some(&(request.session.clone(), request.context.clone()))
            || !self.owns_saved_write(request)
        {
            self.flush_saved_reads();
            return Err("Saved settings context or session was retired".into());
        }
        Ok(())
    }
}
impl PluginHost {
    // Resolve the current destination of a saved settings write
    pub fn current_saved_write_address(
        &self,
        request: &OwnedSettingsWrite,
    ) -> Result<Value, String> {
        let hub = self.hub.lock().unwrap();
        if hub.pending_settings.get(&request.operation_id)
            != Some(&(request.session.clone(), request.context.clone()))
            || hub.sessions.tokens.get(&request.session) != Some(&request.plugin_id)
        {
            return Err("Settings session expired".into());
        }
        Ok(hub
            .context_state
            .addresses
            .get(&request.context)
            .cloned()
            .unwrap_or(request.address.clone()))
    }
    // Keep inspectors current across settings-only writes without reviving stale panels
    pub fn advance_inspectors_after_settings_save(&mut self, revision: u64) {
        let Some(previous) = revision.checked_sub(1) else {
            return;
        };
        for address in self
            .hub
            .lock()
            .unwrap()
            .context_state
            .addresses
            .values_mut()
        {
            if address["inspector_revision"].as_u64() == Some(previous) {
                address["inspector_revision"] = json!(revision);
            }
        }
    }
    // Advance the revision used to correlate inspector settings
    pub fn advance_inspector_revision(&mut self, context: &str, revision: u64) {
        let mut hub = self.hub.lock().unwrap();
        let owner = hub
            .context_state
            .contexts
            .get(context)
            .cloned()
            .unwrap_or_default();
        for peer in hub.saved_context_peers(context, &owner) {
            if let Some(address) = hub.context_state.addresses.get_mut(&peer) {
                if address["inspector_revision"].as_u64() == revision.checked_sub(1) {
                    address["inspector_revision"] = json!(revision);
                }
            }
        }
    }
    // Set settings writer
    pub fn set_settings_writer(&mut self, sender: std::sync::mpsc::SyncSender<OwnedSettingsWrite>) {
        self.hub.lock().unwrap().settings_writer = Some(sender);
    }
    // Reject saved settings
    pub fn reject_saved_settings(&mut self, request: &OwnedSettingsWrite, _error: &str) {
        let mut hub = self.hub.lock().unwrap();
        hub.pending_settings.remove(&request.operation_id);
        hub.flush_saved_reads();
        // A rejected write releases getters with actual committed state, never an optimistic value
    }
    // Promote due saves into the ordinary ordered-read fence only during the actual commit
    pub fn due_burst_settings(&mut self) -> Vec<OwnedSettingsWrite> {
        let mut hub = self.hub.lock().unwrap();
        let mut due = hub.settings_bursts.take_due(std::time::Instant::now());
        due.retain(|request| hub.owns_saved_write(request));
        for request in &due {
            hub.pending_settings.insert(
                request.operation_id.clone(),
                (request.session.clone(), request.context.clone()),
            );
        }
        due
    }

    // Commit due settings together, publishing only after the transaction succeeds
    pub fn commit_burst_settings<T: Send + 'static>(
        &mut self,
        requests: &[OwnedSettingsWrite],
        retained_bytes: usize,
        commit: impl FnOnce(&Transaction<'_>) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let mut hub = self.hub.lock().unwrap();
        for request in requests {
            hub.take_saved_write(request)?;
        }
        let result = self.storage().transaction_with_cost(retained_bytes, commit);
        if result.is_ok() {
            for request in requests {
                hub.accept_settings(request);
            }
            hub.refresh_log_secrets();
        }
        hub.flush_saved_reads();
        result
    }

    // Complete an unchanged settings write and release getters waiting behind it
    pub fn finish_unchanged_saved_settings(
        &mut self,
        request: &OwnedSettingsWrite,
    ) -> Result<(), String> {
        let mut hub = self.hub.lock().unwrap();
        hub.take_saved_write(request)?;
        hub.flush_saved_reads();
        Ok(())
    }

    // Commit saved settings
    pub fn commit_saved_settings<T: Send + 'static>(
        &mut self,
        request: &OwnedSettingsWrite,
        retained_bytes: usize,
        commit: impl FnOnce(&Transaction<'_>) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let mut hub = self.hub.lock().unwrap();
        hub.take_saved_write(request)?;
        let value = match self.storage().transaction_with_cost(retained_bytes, commit) {
            Ok(value) => value,
            Err(error) => {
                hub.flush_saved_reads();
                return Err(error);
            }
        };
        if let Some(resources) = &request.resources {
            hub.context_state
                .resources
                .insert(request.context.clone(), resources.clone());
            if let Some(event) = hub.context_state.visible.get_mut(&request.context) {
                event["payload"]["resources"] = resources.clone();
            }
            hub.publish_resources(&request.plugin_id, &request.context, &request.session);
            hub.flush_saved_reads();
            return Ok(value);
        }
        hub.accept_settings(request);
        hub.refresh_log_secrets();
        hub.flush_saved_reads();
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Advance independent open inspectors without reviving panels stale before the save
    fn settings_only_save_advances_current_inspectors() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            for (context, revision) in [("first", 4), ("other", 4), ("stale", 3)] {
                hub.context_state
                    .addresses
                    .insert(context.into(), json!({"inspector_revision":revision}));
            }
            hub.context_state
                .addresses
                .insert("runtime".into(), json!({"profile_id":"profile"}));
        }
        host.advance_inspectors_after_settings_save(5);
        let hub = host.hub.lock().unwrap();
        for (context, revision) in [("first", 5), ("other", 5), ("stale", 3)] {
            assert_eq!(
                hub.context_state.addresses[context]["inspector_revision"],
                revision
            );
        }
        assert!(hub.context_state.addresses["runtime"]
            .get("inspector_revision")
            .is_none());
    }

    #[test]
    // Verify inspector settings event keeps token and resolves live device metadata
    fn inspector_settings_event_keeps_token_and_resolves_live_device_metadata() {
        let root = tempfile::tempdir().unwrap();
        let host = lifecycle_tests::host(root.path());
        let mut hub = host.hub.lock().unwrap();
        let address =
            json!({"profile_id":"profile", "page_id":"page", "control_id":"key", "path":[]});
        for context in ["live", "inspector"] {
            hub.context_state
                .contexts
                .insert(context.into(), "vendor".into());
            hub.context_state
                .addresses
                .insert(context.into(), address.clone());
        }
        hub.context_state.visible.insert("live".into(), json!({"context":"live", "action":"vendor.action", "device":"device-one", "payload":{"coordinates":{"row":2,"column":3},"isInMultiAction":true}}));
        let event = hub.settings_event("inspector", &json!(["opaque"]));
        assert_eq!(event["context"], "inspector");
        assert_eq!(event["action"], "vendor.action");
        assert_eq!(event["device"], "device-one");
        assert_eq!(event["payload"]["coordinates"], json!({"row":2,"column":3}));
        assert_eq!(event["payload"]["isInMultiAction"], true);
        assert_eq!(event["payload"]["settings"], json!(["opaque"]));
        let draft = hub.settings_event("draft", &Value::Null);
        assert_eq!(draft["device"], "");
        assert_eq!(draft["payload"]["coordinates"], json!({"row":0,"column":0}));
        assert_eq!(draft["payload"]["isInMultiAction"], false);
    }

    // Create a pending settings-write fixture with a live session
    fn pending(host: &mut PluginHost) -> OwnedSettingsWrite {
        let (sender, receiver) = std::sync::mpsc::sync_channel(8);
        host.set_settings_writer(sender);
        let mut hub = host.hub.lock().unwrap();
        hub.sessions
            .tokens
            .insert("session".into(), "fixture".into());
        hub.context_state
            .contexts
            .insert("context".into(), "fixture".into());
        hub.context_state
            .context_actions
            .insert("context".into(), "fixture.action".into());
        hub.context_state.addresses.insert(
            "context".into(),
            json!({"profile_id":"profile","page_id":"page","control_id":"key","path":[]}),
        );
        hub.context_state
            .settings
            .insert("context".into(), json!({"old":true}));
        hub.queue_saved_settings(
            "fixture",
            "session",
            "context",
            json!({"new":true,"unknown":{"preserved":true}}),
        )
        .unwrap();
        drop(hub);
        receiver.recv().unwrap()
    }
    #[test]
    // Verify vendor publication resolves each live context but keeps inspector references
    fn vendor_publication_resolves_each_live_context_but_keeps_inspector_references() {
        let root = tempfile::tempdir().unwrap();
        let host = lifecycle_tests::host(root.path());
        let mut hub = host.hub.lock().unwrap();
        let raw = json!({"value":{"$parameter":"device_value"},"other":7});
        let address = json!({"profile_id":"profile","page_id":"page","control_id":"key","path":[]});
        for (context, value) in [
            ("live-one", "first"),
            ("live-two", "second"),
            ("inspector", "unused"),
        ] {
            hub.context_state
                .contexts
                .insert(context.into(), "vendor".into());
            hub.context_state
                .addresses
                .insert(context.into(), address.clone());
            hub.context_state
                .reference_sources
                .insert(context.into(), raw.clone());
            hub.context_state
                .execution
                .insert(context.into(), json!({"value":value}));
            if context != "inspector" {
                hub.context_state.visible.insert(
                    context.into(),
                    json!({"context":context,"action":"vendor.action","payload":{}}),
                );
            }
        }
        hub.sessions.files.insert(
            "pi".into(),
            FileGrant {
                root: root.path().into(),
                runtime_entry: None,
                bootstrap: String::new(),
                context: Some("inspector".into()),
                setup_runtime: false,
            },
        );
        hub.publish_settings("vendor", "inspector", &raw, "pi");
        let events: Vec<_> = hub.sessions.queues["vendor"].iter().cloned().collect();
        assert_eq!(events.len(), 2);
        for (context, value) in [("live-one", "first"), ("live-two", "second")] {
            let event = events.iter().find(|e| e["context"] == context).unwrap();
            assert_eq!(event["payload"]["settings"]["value"], value);
            assert_eq!(hub.context_state.settings[context], raw);
            assert_eq!(hub.context_state.configured[context], raw);
        }
        assert_eq!(hub.context_state.settings["inspector"], raw);
        hub.publish_settings("vendor", "live-one", &raw, "runtime");
        assert_eq!(
            hub.sessions.queues["pi:pi"].iter().next().unwrap()["payload"]["settings"],
            raw
        );
    }
    #[test]
    // Verify managed publication keeps durable reference projection
    fn managed_publication_keeps_durable_reference_projection() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(root.path());
        let request = pending(&mut host);
        let mut hub = host.hub.lock().unwrap();
        hub.capabilities
            .insert("fixture".into(), Default::default());
        let raw = json!({"token":{"$parameter":"api_key"}});
        hub.context_state
            .settings
            .insert("context".into(), raw.clone());
        hub.context_state
            .configured
            .insert("context".into(), raw.clone());
        hub.context_state
            .reference_sources
            .insert("context".into(), raw.clone());
        hub.context_state
            .execution
            .insert("context".into(), json!({"token":"resolved-only"}));
        hub.publish_settings("fixture", "context", &raw, &request.session);
        assert_eq!(hub.context_state.settings["context"], raw);
        assert_eq!(hub.context_state.configured["context"], raw);
        assert_eq!(
            hub.sessions.queues["fixture"].iter().next().unwrap()["payload"]["settings"]["token"],
            "resolved-only"
        );
    }
    #[test]
    // Verify immediate configuration survives restart and closes atomically after pending writes
    fn immediate_configuration_survives_restart_and_closes_atomically_after_pending_writes() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(root.path());
        {
            let mut hub = host.hub.lock().unwrap();
            hub.sessions.tokens.insert("pi".into(), "vendor".into());
            hub.sessions.files.insert(
                "pi".into(),
                FileGrant {
                    root: root.path().into(),
                    runtime_entry: None,
                    bootstrap: String::new(),
                    context: Some("inspector".into()),
                    setup_runtime: false,
                },
            );
            hub.context_state
                .contexts
                .insert("inspector".into(), "vendor".into());
            hub.configuration_actions
                .insert("inspector".into(), "vendor.action".into());
            hub.context_state
                .settings
                .insert("inspector".into(), json!({"saved":true}));
            hub.enabled.insert("vendor".into());
            hub.pending_settings
                .insert("pending".into(), ("pi".into(), "inspector".into()));
            hub.revoke_plugin_sessions("vendor", "runtime");
            assert_eq!(
                hub.sessions.tokens.get("pi").map(String::as_str),
                Some("vendor")
            );
            assert!(hub.context_state.contexts.contains_key("inspector"));
        }
        assert!(host.configuration_demand().contains("vendor"));
        assert!(host
            .finish_vendor_panel("pi")
            .unwrap_err()
            .contains("still being prepared"));
        host.hub.lock().unwrap().pending_settings.clear();
        assert_eq!(
            host.finish_vendor_panel("pi").unwrap()["settings"],
            json!({"saved":true})
        );
        assert!(!host.hub.lock().unwrap().sessions.tokens.contains_key("pi"));
        assert!(host.finish_vendor_panel("pi").is_err());
    }
    #[test]
    // Verify vendor inspector changes notify matching live context without self echo
    fn vendor_inspector_changes_notify_matching_live_context_without_self_echo() {
        let root = tempfile::tempdir().unwrap();
        let host = lifecycle_tests::host(root.path());
        let mut hub = host.hub.lock().unwrap();
        let address = json!({"profile_id":"profile","page_id":"page","control_id":"key","path":[]});
        for context in ["inspector", "live", "unrelated"] {
            hub.context_state
                .contexts
                .insert(context.into(), "vendor".into());
            hub.context_state
                .addresses
                .insert(context.into(), address.clone());
        }
        hub.context_state.addresses.get_mut("unrelated").unwrap()["control_id"] = json!("other");
        hub.context_state.visible.insert(
            "live".into(),
            json!({"event":"willAppear","context":"live","action":"vendor.action","payload":{}}),
        );
        hub.context_state.visible.insert(
            "unrelated".into(),
            json!({"context":"unrelated","action":"vendor.action","payload":{}}),
        );
        hub.sessions.files.insert(
            "pi".into(),
            FileGrant {
                root: root.path().into(),
                runtime_entry: None,
                bootstrap: String::new(),
                context: Some("inspector".into()),
                setup_runtime: false,
            },
        );
        hub.publish_settings("vendor", "inspector", &json!({"value":1}), "pi");
        let events = hub.sessions.queues.get_mut("vendor").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events.pop_front().unwrap()["context"], "live");
        assert!(!hub.sessions.queues.contains_key("pi:pi"));
        hub.publish_settings("vendor", "live", &json!({"value":2}), "runtime");
        assert!(hub.sessions.queues["vendor"].is_empty());
        assert_eq!(hub.sessions.queues["pi:pi"].len(), 1);
        assert_eq!(hub.context_state.settings["inspector"], json!({"value":2}));
        assert_eq!(
            hub.sessions.queues["pi:pi"].iter().next().unwrap()["context"],
            "inspector"
        );
        hub.sessions.queues.clear();
        hub.context_state
            .resources
            .insert("inspector".into(), json!({"file":"managed.txt"}));
        hub.publish_resources("vendor", "inspector", "pi");
        assert_eq!(hub.sessions.queues["vendor"].len(), 1);
        assert_eq!(
            hub.sessions.queues["vendor"].iter().next().unwrap()["context"],
            "live"
        );
        assert_eq!(
            hub.sessions.queues["vendor"].iter().next().unwrap()["event"],
            "didReceiveResources"
        );
        assert!(hub.sessions.queues["vendor"]
            .iter()
            .next()
            .unwrap()
            .get("id")
            .is_none());
        assert!(!hub.sessions.queues.contains_key("pi:pi"));
        hub.sessions.queues.clear();
        hub.context_state
            .resources
            .insert("live".into(), json!({"file":"changed.txt"}));
        hub.publish_resources("vendor", "live", "runtime");
        assert!(!hub.sessions.queues.contains_key("vendor"));
        assert_eq!(
            hub.context_state.resources["inspector"]["file"],
            "changed.txt"
        );
        assert_eq!(
            hub.sessions.queues["pi:pi"].iter().next().unwrap()["context"],
            "inspector"
        );
        assert_eq!(
            hub.inspector_live_context("inspector", "vendor"),
            Some("live".into())
        );
        hub.sessions.queues.clear();
        hub.route_property_inspector_reply(
            "vendor",
            "live",
            &json!({"event":"sendToPropertyInspector","context":"live","payload":{"answer":42}}),
        );
        assert_eq!(
            hub.sessions.queues["pi:pi"].iter().next().unwrap()["context"],
            "inspector"
        );
        assert_eq!(
            hub.sessions.queues["pi:pi"].iter().next().unwrap()["payload"]["answer"],
            42
        );
        hub.sessions.tokens.insert("pi".into(), "vendor".into());
        hub.sessions
            .tokens
            .insert("runtime".into(), "vendor".into());
        hub.sessions.connected.insert("runtime".into());
        hub.enabled.insert("vendor".into());
        hub.configuration_sessions.insert("pi".into());
        hub.configuration_actions
            .insert("inspector".into(), "vendor.action".into());
        hub.announce_configuration_inspectors("vendor");
        assert_eq!(
            hub.sessions.queues["vendor"].iter().next().unwrap()["event"],
            "propertyInspectorDidAppear"
        );
        assert_eq!(
            hub.sessions.queues["vendor"].iter().next().unwrap()["context"],
            "live"
        );
    }
    #[test]
    // Verify saved settings publish only after workspace commit and rejection keeps projection
    fn saved_settings_publish_only_after_workspace_commit_and_rejection_keeps_projection() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(root.path());
        let request = pending(&mut host);
        assert_eq!(
            host.hub.lock().unwrap().context_state.settings["context"],
            json!({"old":true})
        );
        let result: Result<(), String> = host.commit_saved_settings(&request, 1024, |tx| {
            stream_dock_manager_storage::write_json(
                tx,
                stream_dock_manager_storage::Domain::Workspace,
                "fixture",
                &json!({"new":true}),
                1024,
            )?;
            Err("injected workspace rejection".into())
        });
        assert!(result.is_err());
        assert!(host
            .storage()
            .read_json::<Value>(
                stream_dock_manager_storage::Domain::Workspace,
                "fixture",
                1024
            )
            .unwrap()
            .is_none());
        assert_eq!(
            host.hub.lock().unwrap().context_state.settings["context"],
            json!({"old":true})
        );
        let request = pending(&mut host);
        host.commit_saved_settings(&request, 1024, |tx| {
            stream_dock_manager_storage::write_json(
                tx,
                stream_dock_manager_storage::Domain::Workspace,
                "fixture",
                &json!({"new":true}),
                1024,
            )
        })
        .unwrap();
        let hub = host.hub.lock().unwrap();
        assert_eq!(hub.context_state.settings["context"], request.settings);
        assert!(hub
            .sessions
            .queues
            .get("fixture")
            .is_none_or(|queue| queue.is_empty()));
        assert!(!host
            .storage()
            .run(|connection| connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='plugin_contexts')",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .map_err(|e| e.to_string()))
            .unwrap());
    }
    #[test]
    // Verify workspace normalized references are the published projection
    fn workspace_normalized_references_are_the_published_projection() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(root.path());
        let mut request = pending(&mut host);
        request.settings = json!({"token":{"$parameter":"api_key"},"unknown":{"keep":true}});
        let committed = request.settings.clone();
        host.commit_saved_settings(&request, 1024, move |tx| {
            stream_dock_manager_storage::write_json(
                tx,
                stream_dock_manager_storage::Domain::Workspace,
                "normalized",
                &committed,
                1024,
            )
        })
        .unwrap();
        assert_eq!(
            host.hub.lock().unwrap().context_state.settings["context"]["token"],
            json!({"$parameter":"api_key"})
        );
        assert_eq!(
            host.storage()
                .read_json::<Value>(
                    stream_dock_manager_storage::Domain::Workspace,
                    "normalized",
                    1024
                )
                .unwrap()
                .unwrap(),
            request.settings
        );
    }
    #[test]
    // Verify retired session or changed address cannot commit saved settings
    fn retired_session_or_changed_address_cannot_commit_saved_settings() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::host(root.path());
        let mut request = pending(&mut host);
        let retained = Arc::new(tempfile::tempdir().unwrap());
        let selected = retained.path().join("selected.txt");
        std::fs::write(&selected, "selected bytes").unwrap();
        request.resource_snapshots.push(retained);
        host.hub.lock().unwrap().revoke_storage_session("session");
        assert!(host
            .commit_saved_settings::<()>(&request, 0, |_| panic!(
                "retired request reached database"
            ))
            .is_err());
        assert!(selected.exists());
        drop(request);
        assert!(!selected.exists());
        let request = pending(&mut host);
        host.hub
            .lock()
            .unwrap()
            .context_state
            .addresses
            .insert("context".into(), json!({"different":true}));
        assert!(host
            .commit_saved_settings::<()>(&request, 0, |_| panic!("stale address reached database"))
            .is_err());
    }
}
