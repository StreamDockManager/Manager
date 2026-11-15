//! Vendor file maps are projected by the runtime after managed-file staging succeeds
use super::*;
impl Hub {
    // Build the resources notification sent to the plugin
    pub(super) fn resources_event(&self, context: &str) -> Value {
        let settings = self.execution_settings(context, &json!({}));
        let mut event = self.settings_event(context, &settings);
        event["event"] = json!("didReceiveResources");
        event["payload"]["resources"] = self
            .context_state
            .resources
            .get(context)
            .cloned()
            .unwrap_or(json!({}));
        event
    }
    // Queue resources
    pub(super) fn queue_resources(
        &mut self,
        owner: &str,
        session: &str,
        context: &str,
        payload: &Value,
    ) -> Result<(), String> {
        let map = payload.as_object().ok_or("Resources must be a file map")?;
        if map.len() > 32
            || map.iter().any(|(key, value)| {
                key.is_empty()
                    || key.len() > 128
                    || value
                        .as_str()
                        .is_none_or(|s| s.is_empty() || s.len() > 4096)
            })
        {
            return Err("Invalid or oversized resource map".into());
        }
        if self.context_state.drafts.contains(context)
            && !self.context_state.resource_ids.contains_key(context)
        {
            return Err("Resources require an action binding editor".into());
        }
        let mut sources = payload.clone();
        // Browser sessions cannot turn this API into an arbitrary local-file reader
        // Native plugin processes already hold their own local filesystem authority
        if let Some(grant) = self.sessions.files.get(session) {
            let root = grant.root.canonicalize().map_err(|e| e.to_string())?;
            for (key, value) in map {
                let requested = Path::new(value.as_str().unwrap());
                if let Some(selected) = self
                    .sessions
                    .resource_selections
                    .get(session)
                    .and_then(|grants| grants.resolve(owner, context, requested))
                {
                    sources[key] = json!(selected);
                    continue;
                }
                let source = requested.canonicalize().map_err(|e| e.to_string())?;
                let existing = self
                    .context_state
                    .resources
                    .get(context)
                    .and_then(Value::as_object)
                    .is_some_and(|resources| {
                        resources.values().any(|p| p.as_str() == source.to_str())
                    });
                if !source.starts_with(&root) && !existing {
                    return Err(
                        "Browser resource is outside its authorized package or action files".into(),
                    );
                }
            }
        }
        self.queue_owned_write(owner, session, context, Value::Null, Some(sources))
    }
    // Publish resources
    pub(super) fn publish_resources(&mut self, owner: &str, context: &str, session: &str) {
        let resources = self
            .context_state
            .resources
            .get(context)
            .cloned()
            .unwrap_or(json!({}));
        let managed = self.capabilities.contains_key(owner);
        let inspector = self
            .sessions
            .files
            .get(session)
            .is_some_and(|g| g.context.is_some());
        let peers = if managed {
            vec![context.into()]
        } else {
            self.saved_context_peers(context, owner)
        };
        for peer in peers {
            self.context_state
                .resources
                .insert(peer.clone(), resources.clone());
            if let Some(event) = self.context_state.visible.get_mut(&peer) {
                event["payload"]["resources"] = resources.clone();
            }
            let event = self.resources_event(&peer);
            if managed || (inspector && self.context_state.visible.contains_key(&peer)) {
                self.sessions
                    .queues
                    .entry(owner.into())
                    .or_default()
                    .push_back(event.clone());
            }
            for (token, grant) in &self.sessions.files {
                if grant.context.as_ref() == Some(&peer) && (managed || token != session) {
                    self.sessions
                        .queues
                        .entry(format!("pi:{token}"))
                        .or_default()
                        .push_back(event.clone());
                }
            }
        }
    }
}
impl PluginHost {
    // Update saved resource IDs
    pub fn update_saved_resource_ids(&mut self, context: &str, ids: Value) {
        let mut hub = self.hub.lock().unwrap();
        let owner = hub
            .context_state
            .contexts
            .get(context)
            .cloned()
            .unwrap_or_default();
        for peer in hub.saved_context_peers(context, &owner) {
            hub.context_state.resource_ids.insert(peer, ids.clone());
        }
    }

    // Set action resources
    pub fn set_action_resources(
        &mut self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
        resources: Value,
    ) {
        let mut hub = self.hub.lock().unwrap();
        let context = hub.binding_context(id, scope, device, control);
        hub.context_state
            .resources
            .insert(context.clone(), resources.clone());
        if let Some(event) = hub.context_state.visible.get_mut(&context) {
            event["payload"]["resources"] = resources;
        }
    }
    // Set panel resources
    pub fn set_panel_resources(
        &mut self,
        token: &str,
        ids: Value,
        paths: Value,
    ) -> Result<(), String> {
        let mut hub = self.hub.lock().unwrap();
        let context = hub
            .sessions
            .files
            .get(token)
            .and_then(|grant| grant.context.clone())
            .filter(|c| hub.context_state.drafts.contains(c))
            .ok_or("Settings draft expired")?;
        hub.context_state.resource_ids.insert(context.clone(), ids);
        hub.context_state.resources.insert(context, paths);
        Ok(())
    }
    // Finish vendor panel
    pub fn finish_vendor_panel(&mut self, token: &str) -> Result<Value, String> {
        let mut hub = self.hub.lock().unwrap();
        let context = hub
            .sessions
            .files
            .get(token)
            .and_then(|g| g.context.clone())
            .ok_or("Settings inspector expired")?;
        if hub.context_state.drafts.contains(&context) {
            return Err("Managed settings require Apply or Cancel".into());
        }
        if hub.pending_settings.values().any(|(_, c)| c == &context) {
            return Err("Action resources are still being prepared. Try applying again.".into());
        }
        let value = json!({"settings":hub.context_state.settings.get(&context).cloned().unwrap_or(json!({})),"resources":hub.context_state.resource_ids.get(&context).cloned().unwrap_or(json!({}))});
        Self::close_inspector_in_hub(&mut hub, token);
        drop(hub);
        self.reconcile();
        Ok(value)
    }
    // Resolve a settings panel value
    pub fn panel_value(&self, token: &str) -> Result<Value, String> {
        let settings = self.panel_settings(token)?;
        let hub = self.hub.lock().unwrap();
        let context = hub
            .sessions
            .files
            .get(token)
            .and_then(|grant| grant.context.as_ref())
            .ok_or("Settings draft expired")?;
        if hub.pending_settings.values().any(|(_, c)| c == context) {
            return Err("Action resources are still being prepared. Try applying again.".into());
        }
        Ok(
            json!({"settings":settings,"resources":hub.context_state.resource_ids.get(context).cloned().unwrap_or(json!({}))}),
        )
    }
    // Commit draft resources
    pub fn commit_draft_resources(
        &mut self,
        request: &OwnedSettingsWrite,
        ids: Value,
        paths: Value,
    ) -> Result<(), String> {
        let mut hub = self.hub.lock().unwrap();
        let pending = hub.pending_settings.remove(&request.operation_id);
        if pending != Some((request.session.clone(), request.context.clone()))
            || hub.sessions.tokens.get(&request.session) != Some(&request.plugin_id)
            || !hub.context_state.drafts.contains(&request.context)
            || hub.context_state.contexts.get(&request.context) != Some(&request.plugin_id)
        {
            hub.flush_saved_reads();
            return Err("Resource draft or session was retired".into());
        }
        hub.context_state
            .resource_ids
            .insert(request.context.clone(), ids);
        hub.context_state
            .resources
            .insert(request.context.clone(), paths);
        hub.publish_resources(&request.plugin_id, &request.context, &request.session);
        hub.flush_saved_reads();
        Ok(())
    }
}
