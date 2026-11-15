use super::*;

#[derive(Debug, Default)]
pub struct PluginReload {
    pub added: usize,
    pub removed: Vec<String>,
    pub closed_views: Vec<String>,
    /// Invalid packages make absence ambiguous, including after a folder rename
    pub cleanup_safe: bool,
}

impl PluginHost {
    // Reload plugin state before reconciling demand
    pub fn reload(&mut self) -> Result<PluginReload, String> {
        // Finish the entire inventory before mutating installed or preference state
        let mut entries = std::fs::read_dir(&self.root)
            .map_err(|e| e.to_string())?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort();
        let mut present = HashSet::new();
        let mut valid = HashMap::new();
        let mut result = PluginReload {
            cleanup_safe: true,
            ..Default::default()
        };
        for path in entries {
            if path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.is_dir() && !metadata.file_type().is_symlink() {
                continue;
            }
            present.insert(path.clone());
            match read_plugin_with_identity(&path, path.file_name().and_then(|n| n.to_str())) {
                Ok(plugin) => {
                    if !valid.contains_key(&plugin.info.id)
                        || self
                            .plugins
                            .get(&plugin.info.id)
                            .is_some_and(|current| current.path == plugin.path)
                    {
                        valid.insert(plugin.info.id.clone(), plugin);
                    }
                }
                Err(_) => result.cleanup_safe = false,
            }
        }
        // A renamed directory or valid duplicate retains installation-local preferences
        for (id, replacement) in &valid {
            if let Some(plugin) = self.plugins.get_mut(id) {
                if plugin.path != replacement.path {
                    let old = plugin.path.clone();
                    plugin.path = replacement.path.clone();
                    plugin.entry = replacement.entry.clone();
                    plugin.metadata = replacement.metadata.clone();
                    let mut hub = self.hub.lock().unwrap();
                    hub.roots.insert(id.clone(), replacement.path.clone());
                    for grant in hub.sessions.files.values_mut() {
                        if grant.root == old {
                            grant.root = replacement.path.clone();
                        }
                    }
                }
            }
        }
        if result.cleanup_safe {
            let mut missing: std::collections::BTreeSet<String> = self
                .plugins
                .iter()
                .filter(|(id, p)| !valid.contains_key(*id) && !present.contains(&p.path))
                .map(|(id, _)| id.clone())
                .collect();
            {
                let hub = self.hub.lock().unwrap();
                // Also retire confirmed orphan records loaded from earlier runs
                for id in hub
                    .globals
                    .keys()
                    .chain(hub.action_defaults.keys())
                    .chain(hub.provenance.keys())
                    .chain(hub.installations.keys())
                    .chain(hub.appearance.keys())
                    .chain(hub.enabled.iter())
                    .chain(hub.sleep_when_hidden.keys())
                    .chain(hub.health.keys())
                {
                    if !valid.contains_key(id)
                        && !self.plugins.contains_key(id)
                        && !hub.retained.contains_key(id)
                    {
                        missing.insert(id.clone());
                    }
                }
                for context in hub.context_state.settings.keys() {
                    if let Some((id, _)) = context.split_once(':') {
                        if safe_id(id)
                            && !valid.contains_key(id)
                            && !self.plugins.contains_key(id)
                            && !hub.context_state.drafts.contains(context)
                        {
                            missing.insert(id.into());
                        }
                    }
                }
            }
            for id in self.runtimes.status().plugins.keys() {
                if !valid.contains_key(id) && !self.plugins.contains_key(id) {
                    missing.insert(id.clone());
                }
            }
            for id in missing {
                if self.plugins.contains_key(&id) {
                    result.closed_views.extend(self.view_labels(&id)?);
                    self.enable(&id, false)?;
                    if let Some(done) = self.stop_for_replacement(&id)? {
                        let deadline = std::time::Instant::now() + Duration::from_secs(3);
                        while !done.load(Ordering::Acquire) {
                            if std::time::Instant::now() >= deadline {
                                return Err(
                                    "Plugin process cleanup is still in progress; reload to retry"
                                        .into(),
                                );
                            }
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                }
                self.forget_plugin(&id)?;
                result.removed.push(id);
            }
        }
        let discovered: Vec<_> = valid
            .into_values()
            .filter(|p| !self.plugins.contains_key(&p.info.id))
            .collect();
        {
            let mut hub = self.hub.lock().unwrap();
            let enabled = hub.enabled.clone();
            for plugin in &discovered {
                hub.enabled.remove(&plugin.info.id);
            }
            if let Err(error) = hub.persist() {
                hub.enabled = enabled;
                return Err(error);
            }
            result.added = discovered.len();
            for plugin in discovered {
                hub.manifest_dialects
                    .insert(plugin.info.id.clone(), plugin.info.dialect.clone());
                hub.metadata
                    .insert(plugin.info.id.clone(), plugin.metadata.clone());
                if let Some(capabilities) = &plugin.info.capabilities {
                    hub.capabilities
                        .insert(plugin.info.id.clone(), capabilities.clone());
                }
                hub.roots
                    .insert(plugin.info.id.clone(), plugin.path.clone());
                self.plugins.insert(plugin.info.id.clone(), plugin);
            }
        }
        Ok(result)
    }

    /// Shared by directory reconciliation and explicit deletion. No package files
    /// or assignment configuration are removed here. Failed writes are retryable.
    pub(super) fn forget_plugin(&mut self, id: &str) -> Result<(), String> {
        self.forget_plugin_transaction(id, None, 0, |_| Ok(()))
    }
    // Forget plugin transaction
    pub(super) fn forget_plugin_transaction(
        &mut self,
        id: &str,
        deletion: Option<(String, Value)>,
        retained_bytes: usize,
        commit: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<(), String>
            + Send
            + 'static,
    ) -> Result<(), String> {
        let tokens: Vec<_> = self
            .hub
            .lock()
            .unwrap()
            .sessions
            .tokens
            .iter()
            .filter(|(_, owner)| owner.as_str() == id)
            .map(|(token, _)| token.clone())
            .collect();
        for token in tokens {
            self.close_inspector(&token);
        }
        let mut hub = self.hub.lock().unwrap();
        let prefix = format!("{id}:");
        let previous = (
            hub.enabled.clone(),
            hub.sleep_when_hidden.clone(),
            hub.globals.clone(),
            hub.action_defaults.clone(),
            hub.context_state.settings.clone(),
            hub.provenance.clone(),
            hub.installations.clone(),
            hub.appearance.clone(),
            hub.backup_receipts.clone(),
        );
        hub.enabled.remove(id);
        hub.sleep_when_hidden.remove(id);
        let retain = hub.retained.contains_key(id);
        if !retain {
            hub.globals.remove(id);
            hub.action_defaults.remove(id);
        }
        hub.context_state
            .settings
            .retain(|context, _| !context.starts_with(&prefix));
        hub.provenance.remove(id);
        hub.installations.remove(id);
        hub.appearance.remove(id);
        let purge_extras = deletion
            .as_ref()
            .is_some_and(|(_, journal)| journal.get("backups").is_some());
        if let Some(backups) = deletion
            .as_ref()
            .and_then(|(_, journal)| journal["backups"].as_array())
        {
            for backup in backups {
                if backup["sole_package"] == true {
                    if let Some(id) = backup["id"].as_str() {
                        hub.backup_receipts.remove(id);
                    }
                }
            }
        }
        let owner = id.to_owned();
        let result = hub.snapshot().and_then(|snapshot| {
            let cost = snapshot
                .bytes
                .checked_add(retained_bytes)
                .ok_or("Plugin removal exceeds admission limit")?;
            self.runtimes.forget_plugin_with(id, cost, move |tx| {
                snapshot.write(tx)?;
                stream_dock_manager_storage::write_json(
                    tx,
                    stream_dock_manager_storage::Domain::PluginMetadata,
                    &format!("storage-removed:{owner}"),
                    &stored_data::now_ms(),
                    128,
                )?;
                if !retain {
                    storage_rpc::purge(tx, &owner)?;
                }
                if purge_extras {
                    use stream_dock_manager_storage::{read_json, write_json, Domain};
                    if let Some(mut preferences) = read_json::<log_capture::LogPreferences>(
                        tx,
                        Domain::LoggingPreferences,
                        "preferences",
                        1024 * 1024,
                    )? {
                        if preferences.plugins.remove(&owner).is_some() {
                            write_json(
                                tx,
                                Domain::LoggingPreferences,
                                "preferences",
                                &preferences,
                                1024 * 1024,
                            )?;
                        }
                    }
                }
                stream_dock_manager_storage::delete(
                    tx,
                    stream_dock_manager_storage::Domain::PluginHealth,
                    &owner,
                )?;
                if let Some((key, mut journal)) = deletion {
                    journal["committed"] = json!(true);
                    stream_dock_manager_storage::write_json(
                        tx,
                        stream_dock_manager_storage::Domain::PluginMetadata,
                        &key,
                        &journal,
                        1024 * 1024,
                    )?;
                }
                commit(tx)
            })
        });
        if let Err(error) = result {
            (
                hub.enabled,
                hub.sleep_when_hidden,
                hub.globals,
                hub.action_defaults,
                hub.context_state.settings,
                hub.provenance,
                hub.installations,
                hub.appearance,
                hub.backup_receipts,
            ) = previous;
            return Err(error);
        }
        hub.context_state
            .binding_settings
            .retain(|key, _| !key.starts_with(&prefix));
        hub.context_state
            .binding_contexts
            .retain(|key, _| !key.starts_with(&prefix));
        hub.health.remove(id);
        hub.refresh_log_secrets();
        hub.roots.remove(id);
        hub.metadata.remove(id);
        hub.manifest_dialects.remove(id);
        hub.capabilities.remove(id);
        hub.sessions.queues.remove(id);
        hub.network.remove(id);
        hub.telemetry_topics.remove(id);
        hub.action_results.retain(|_, pending| pending.owner != id);
        self.required.remove(id);
        self.startup.retain(|queued| queued != id);
        self.starting.remove(id);
        drop(hub);
        self.logs.retire(id);
        self.plugins.remove(id);
        Ok(())
    }
}
