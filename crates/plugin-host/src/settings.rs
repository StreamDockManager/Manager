use super::*;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfiguration {
    pub settings: Value,
    pub action_defaults: std::collections::BTreeMap<String, Value>,
}
impl Default for PluginConfiguration {
    // Construct the default value
    fn default() -> Self {
        Self {
            settings: json!({}),
            action_defaults: Default::default(),
        }
    }
}
pub(super) struct GlobalDraft {
    pub(super) base: Value,
    pub(super) value: Value,
}
pub(super) struct DefaultDraft {
    owner: String,
    action: String,
    base: Value,
}

/// Authenticated editor request. Vendor addresses are validated against saved actions
#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct SettingsPanelRequest {
    pub id: String,
    pub action: Option<String>,
    pub settings: Value,
    pub setup: Option<bool>,
    pub address: Option<Value>,
    pub resources: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Serialize, ts_rs::TS)]
pub struct SettingsPanel {
    pub immediate: bool,
    pub native_window: bool,
    pub location: bool,
    pub url: String,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path_picker_modes: Option<std::collections::BTreeMap<String, String>>,
}

const SETTINGS_MODULE_LIMIT: usize = 256 * 1024;

// Read at most the strict limit, including the byte that disqualifies an exactly
// limit-sized module. Do not trust metadata: files can grow after opening and
// sparse files must not determine allocation size. Missing/invalid schemas keep
// the existing null behavior and do not grant selectors or location access.
fn parse_settings_module(reader: impl std::io::Read) -> Value {
    use std::io::Read;
    let mut bytes = Vec::new();
    match reader
        .take(SETTINGS_MODULE_LIMIT as u64)
        .read_to_end(&mut bytes)
    {
        Ok(length) if length < SETTINGS_MODULE_LIMIT => {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}

fn read_settings_module(root: &Path) -> Value {
    std::fs::File::open(root.join("settings-module.json"))
        .map(parse_settings_module)
        .unwrap_or(Value::Null)
}

// List the fields exposed by the plugin settings schema
fn settings_fields<'a>(module: &'a Value, action: Option<&str>) -> &'a [Value] {
    let fields = match action {
        Some(action) => &module["actions"][action],
        None => &module["plugin"],
    };
    fields.as_array().map(Vec::as_slice).unwrap_or_default()
}

// List settings fields that contain filesystem paths
fn path_fields(fields: &[Value]) -> impl Iterator<Item = (&str, &Value)> {
    fields.iter().filter_map(|field| {
        if field["type"] != "path"
            || !matches!(
                field["path_picker"].as_str(),
                Some("file" | "folder" | "save")
            )
        {
            return None;
        }
        Some((field["key"].as_str()?, field))
    })
}

/// One-shot settings preview only; this value is never persisted by the host
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
pub struct SettingsLocationFix {
    pub latitude: f64,
    pub longitude: f64,
    pub accuracy_m: f64,
    pub timestamp: f64,
}
impl PluginHost {
    /// Durable purge generation for coordinated settings history. Ordinary
    /// settings writes leave it unchanged; clearing owned data invalidates it.
    pub fn configuration_generation(&self, id: &str) -> Result<u64, String> {
        if !safe_id(id) {
            return Err("Invalid plugin identity".into());
        }
        let owner = id.to_owned();
        self.storage()
            .run(move |db| storage_rpc::plugin_storage_purge_generation(db, &owner))
    }
    // Read the plugin configuration
    pub fn configuration(&self, id: &str) -> Result<PluginConfiguration, String> {
        if !self.plugins.contains_key(id) {
            return Err("Plugin is no longer installed".into());
        }
        let h = self.hub.lock().unwrap();
        Ok(PluginConfiguration {
            settings: h.globals.get(id).cloned().unwrap_or(json!({})),
            action_defaults: h
                .action_defaults
                .get(id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        })
    }
    /// Publish a coordinated workspace/configuration change in the shared SQLite transaction
    /// The caller stages and validates external files before calling; no callbacks into the
    /// host or runtime may occur on the database worker.
    pub fn configuration_transaction_sql<T: Send + 'static>(
        &mut self,
        id: &str,
        expected: &PluginConfiguration,
        value: &PluginConfiguration,
        retained_bytes: usize,
        commit: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<T, String>
            + Send
            + 'static,
    ) -> Result<T, String> {
        let plugin = self
            .plugins
            .get(id)
            .ok_or("Plugin is no longer installed")?;
        if (!value.settings.is_object() && plugin.info.dialect != "openaction")
            || value.settings.to_string().len() > 256 * 1024
        {
            return Err("Invalid global settings for this plugin".into());
        }
        if value.action_defaults.iter().any(|(id, value)| {
            (!value.is_object() && plugin.info.dialect != "openaction")
                || value.to_string().len() > 256 * 1024
                || !plugin.info.actions.iter().any(|action| action.id == *id)
        }) {
            return Err("Invalid action defaults for this plugin".into());
        }
        let settings_bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?.len();
        let expected_bytes = serde_json::to_vec(expected)
            .map_err(|e| e.to_string())?
            .len();
        let retained_bytes = retained_bytes
            .checked_add(settings_bytes)
            .and_then(|bytes| bytes.checked_add(expected_bytes))
            .ok_or("Plugin transaction exceeds admission limit")?;
        if settings_bytes > 16 * 1024 * 1024 {
            return Err("Plugin configuration exceeds 16 MiB".into());
        }
        let mut hub = self.hub.lock().unwrap();
        let current = PluginConfiguration {
            settings: hub.globals.get(id).cloned().unwrap_or(json!({})),
            action_defaults: hub
                .action_defaults
                .get(id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        };
        if current != *expected {
            return Err("Plugin settings changed. Review the import again.".into());
        }
        let owner = id.to_owned();
        let expected = expected.clone();
        let candidate = value.clone();
        let result = hub
            .store
            .as_ref()
            .ok_or("Plugin storage is unavailable")?
            .transaction_with_cost(retained_bytes, move |tx| {
                use stream_dock_manager_storage::Domain;
                let stored = PluginConfiguration {
                    settings: stream_dock_manager_storage::read_json(
                        tx,
                        Domain::PluginGlobals,
                        &owner,
                        16 * 1024 * 1024,
                    )?
                    .unwrap_or(json!({})),
                    action_defaults: stream_dock_manager_storage::read_json(
                        tx,
                        Domain::PluginDefaults,
                        &owner,
                        16 * 1024 * 1024,
                    )?
                    .unwrap_or_default(),
                };
                if stored != expected {
                    return Err("Plugin settings changed. Review the import again.".into());
                }
                stream_dock_manager_storage::write_json(
                    tx,
                    Domain::PluginGlobals,
                    &owner,
                    &candidate.settings,
                    16 * 1024 * 1024,
                )?;
                stream_dock_manager_storage::write_json(
                    tx,
                    Domain::PluginDefaults,
                    &owner,
                    &candidate.action_defaults,
                    16 * 1024 * 1024,
                )?;
                stored_data::mark_updated(tx, &owner)?;
                commit(tx)
            })?;
        hub.invalidate_persistence_sizes();
        hub.globals.insert(id.into(), value.settings.clone());
        hub.action_defaults.insert(
            id.into(),
            value.action_defaults.clone().into_iter().collect(),
        );
        hub.refresh_log_secrets();
        Ok(result)
    }
    // Notify an enabled plugin that its global configuration changed
    pub fn configuration_changed(&self, id: &str) {
        let mut h = self.hub.lock().unwrap();
        if h.enabled.contains(id) {
            let settings = h.globals.get(id).cloned().unwrap_or(json!({}));
            h.sessions.queues.entry(id.into()).or_default().push_back(
                json!({"event":"didReceiveGlobalSettings","payload":{"settings":settings}}),
            );
        }
    }
    // Read the installed plugin manifest
    pub fn plugin_manifest(&self, id: &str) -> Result<Value, String> {
        self.plugins
            .get(id)
            .map(|p| p.manifest.clone())
            .ok_or_else(|| "Plugin is no longer installed".into())
    }
    /// Keep accepted navigation ahead of session retirement through its durable commit
    pub fn with_profile_switch_session<T>(
        &self,
        id: &str,
        session: &str,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let hub = self.hub.lock().unwrap();
        if !self
            .plugins
            .get(id)
            .is_some_and(|p| p.token == session && p.info.dialect == "streamdeck")
            || !hub.enabled.contains(id)
            || !hub.sessions.connected.contains(session)
        {
            return Err("Plugin connection changed before profile commit".into());
        }
        operation()
    }
    // Check whether profile switch session is current
    pub fn profile_switch_session_is_current(&self, id: &str, session: &str) -> bool {
        let hub = self.hub.lock().unwrap();
        self.plugins
            .get(id)
            .is_some_and(|p| p.token == session && p.info.dialect == "streamdeck")
            && hub.enabled.contains(id)
            && hub.sessions.connected.contains(session)
    }
    // Resolve the installed plugin directory
    pub fn plugin_directory(&self, id: &str) -> Result<PathBuf, String> {
        self.plugins
            .get(id)
            .map(|p| p.path.clone())
            .ok_or_else(|| "Plugin is no longer installed".into())
    }
    // Resolve the staged plugin directory
    pub fn prepared_directory(prepared: &PreparedPlugin) -> &Path {
        &prepared.source
    }
    // Track runtime demand from an open configuration inspector
    pub(super) fn configuration_demand(&self) -> HashSet<String> {
        let h = self.hub.lock().unwrap();
        h.sessions
            .files
            .iter()
            .filter_map(|(token, grant)| {
                let context = grant.context.as_ref()?;
                let owner = h.sessions.tokens.get(token)?;
                (h.configuration_actions.contains_key(context)
                    && h.context_state.contexts.get(context) == Some(owner)
                    && h.inspector_runtime_allowed(token, owner))
                .then(|| owner.clone())
            })
            .collect()
    }
    // Check whether the plugin uses Manager-managed settings
    pub fn managed_settings(&self, id: &str) -> Result<bool, String> {
        Ok(self
            .plugins
            .get(id)
            .ok_or("Plugin not installed")?
            .info
            .capabilities
            .is_some())
    }
    // Bind vendor panel
    pub fn bind_vendor_panel(&mut self, token: &str, address: Value) -> Result<(), String> {
        let mut hub = self.hub.lock().unwrap();
        let owner = hub
            .sessions
            .tokens
            .get(token)
            .cloned()
            .ok_or("Inspector expired")?;
        if hub.capabilities.contains_key(&owner) {
            return Err("Managed inspectors require draft transactions".into());
        }
        let context = hub
            .sessions
            .files
            .get(token)
            .and_then(|g| g.context.clone())
            .ok_or("Inspector expired")?;
        let action = hub
            .configuration_actions
            .get(&context)
            .filter(|a| !a.is_empty())
            .cloned()
            .ok_or("Action inspector required")?;
        hub.context_state.drafts.remove(&context);
        hub.global_drafts.remove(&context);
        hub.context_state
            .context_actions
            .insert(context.clone(), action);
        hub.context_state.addresses.insert(context, address);
        Ok(())
    }
    // Resolve the requested plugin settings panel
    pub fn settings_panel(
        &mut self,
        id: &str,
        action: Option<&str>,
        settings: &Value,
    ) -> Result<SettingsPanel, String> {
        self.open_settings_panel(id, action, settings, false)
    }
    /// The setup page explicitly grants this plugin-wide draft a temporary runtime
    pub fn setup_settings_panel(&mut self, id: &str) -> Result<SettingsPanel, String> {
        self.open_settings_panel(id, None, &Value::Null, true)
    }
    // Open settings panel
    fn open_settings_panel(
        &mut self,
        id: &str,
        action: Option<&str>,
        settings: &Value,
        setup: bool,
    ) -> Result<SettingsPanel, String> {
        self.recovery.check()?;
        let plugin = self.plugins.get(id).ok_or("plugin not installed")?;
        let relative = if let Some(capability) = &plugin.info.capabilities {
            if capability.settings_protocol != 1 {
                return Err("Managed settings protocol 1 required".into());
            }
            if let Some(action) = action {
                capability
                    .actions
                    .get(action)
                    .and_then(|a| a.settings.clone())
            } else {
                capability.plugin_settings.clone()
            }
        } else {
            // Vendor inspectors use standard messages and opaque JSON settings
            // They do not opt into Manager's storage or configuration RPCs
            let action = action.ok_or("Vendor plugins expose action inspectors only")?;
            let metadata = plugin
                .metadata
                .get(action)
                .ok_or("Action is not owned by plugin")?;
            if let Some(path) = metadata["PropertyInspectorPath"].as_str() {
                Some(path.to_owned())
            } else {
                let manifest: Value = serde_json::from_slice(
                    &std::fs::read(plugin.path.join("manifest.json")).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                manifest["PropertyInspectorPath"]
                    .as_str()
                    .map(str::to_owned)
            }
        }
        .ok_or("no settings for this scope")?;
        let entry = safe_file(&plugin.path, &relative)?;
        let relative = entry
            .strip_prefix(&plugin.path)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let token = uuid::Uuid::new_v4().to_string();
        let context = format!("settings-draft:{token}");
        let mut h = self.hub.lock().unwrap();
        if let Some(capability) = &plugin.info.capabilities {
            h.capabilities.insert(id.into(), capability.clone());
        }
        let settings = if let Some(action) = action {
            browser::vsd_action_settings(plugin, action, settings).into_owned()
        } else {
            h.globals.get(id).cloned().unwrap_or(json!({}))
        };
        if (!settings.is_object() && plugin.info.dialect != "openaction")
            || settings.to_string().len() > 256 * 1024
        {
            return Err("invalid settings object".into());
        }
        let setup_runtime = setup && !plugin.info.enabled;
        let mut info = browser::registration_info(
            &h,
            id,
            &plugin.info.version,
            &plugin.info.dialect,
            true,
            true,
        )?;
        info["streamDockManager"] = json!({"theme":h.theme_payload(id, true),"enabled":plugin.info.enabled,"setup_runtime":setup_runtime,"inline_path_pickers":true});
        let details = json!({"action":action.unwrap_or(""),"context":context,"payload":{"settings":settings,"scope":if action.is_some(){"action"}else{"plugin"}}});
        h.context_state.contexts.insert(context.clone(), id.into());
        h.configuration_actions
            .insert(context.clone(), action.unwrap_or("").into());
        h.context_state
            .reference_sources
            .insert(context.clone(), settings.clone());
        h.context_state.settings.insert(context.clone(), settings);
        h.context_state.drafts.insert(context.clone());
        let globals = h.globals.get(id).cloned().unwrap_or(json!({}));
        h.global_drafts.insert(
            context.clone(),
            GlobalDraft {
                base: globals.clone(),
                value: globals,
            },
        );
        h.sessions.tokens.insert(token.clone(), id.into());
        h.sessions.files.insert(
            token.clone(),
            FileGrant {
                root: plugin.path.clone(),
                runtime_entry: None,
                bootstrap: bootstrap(
                    self.port,
                    &token,
                    &info,
                    browser::BootstrapRole::Inspector(&details),
                ),
                context: Some(context),
                setup_runtime,
            },
        );
        let module = read_settings_module(&plugin.path);
        let fields = settings_fields(&module, action);
        let path_picker_modes: std::collections::BTreeMap<_, _> = path_fields(fields)
            .filter_map(|(key, field)| match field["path_picker"].as_str() {
                Some(mode @ ("file" | "folder" | "save")) => Some((key.into(), mode.into())),
                _ => None,
            })
            .collect();
        let location = module["location"] == true
            && (action.is_none()
                || ["latitude", "longitude"].iter().all(|key| {
                    fields
                        .iter()
                        .any(|f| f["key"] == *key && f["type"] == "number")
                }));
        if location {
            h.location_drafts.insert(token.clone());
        }
        let panel = SettingsPanel {
            immediate: false,
            native_window: false,
            location,
            path_picker_modes: (!path_picker_modes.is_empty()).then_some(path_picker_modes),
            url: format!("http://127.0.0.1:{}/files/{token}/{relative}", self.port),
            token,
        };
        if let Some(wake) = &h.sampler_wake {
            wake();
        }
        if let Some(wake) = &h.wake {
            wake();
        }
        drop(h);
        self.reconcile();
        Ok(panel)
    }
    /// Local defaults are copied to a newly selected action; existing bindings own their values
    pub fn action_defaults(&self, id: &str, action: &str) -> Result<Value, String> {
        let plugin = self.plugins.get(id).ok_or("plugin not installed")?;
        if !plugin.info.actions.iter().any(|entry| entry.id == action) {
            return Err("action is not owned by plugin".into());
        }
        let hub = self.hub.lock().unwrap();
        Ok(hub
            .action_defaults
            .get(id)
            .and_then(|actions| actions.get(action))
            .cloned()
            .unwrap_or(json!({})))
    }
    // Choose the default settings panel for the plugin
    pub fn default_settings_panel(
        &mut self,
        id: &str,
        action: &str,
    ) -> Result<SettingsPanel, String> {
        let base = self.action_defaults(id, action)?;
        let panel = self.settings_panel(id, Some(action), &base)?;
        let context = format!("settings-draft:{}", panel.token);
        self.hub.lock().unwrap().default_drafts.insert(
            context,
            DefaultDraft {
                owner: id.into(),
                action: action.into(),
                base,
            },
        );
        Ok(panel)
    }
    // Read the settings for the selected panel
    pub fn panel_settings(&self, token: &str) -> Result<Value, String> {
        let h = self.hub.lock().unwrap();
        let context = h
            .sessions
            .files
            .get(token)
            .and_then(|g| g.context.as_ref())
            .ok_or("settings inspector expired")?;
        Ok(h.context_state
            .settings
            .get(context)
            .cloned()
            .unwrap_or(json!({})))
    }
    // Commit plugin settings
    pub fn commit_plugin_settings(&mut self, id: &str, token: &str) -> Result<(), String> {
        self.commit_panel_changes(id, token, true)
    }
    // Commit panel globals
    pub fn commit_panel_globals(&mut self, token: &str) -> Result<(), String> {
        let id = self
            .hub
            .lock()
            .unwrap()
            .sessions
            .tokens
            .get(token)
            .cloned()
            .ok_or("settings draft expired")?;
        self.commit_panel_changes(&id, token, false)
    }
    // Commit panel changes
    fn commit_panel_changes(
        &mut self,
        id: &str,
        token: &str,
        definition: bool,
    ) -> Result<(), String> {
        self.plugins
            .get(id)
            .ok_or("plugin is no longer installed")?;
        let settings = self.panel_settings(token)?;
        let mut h = self.hub.lock().unwrap();
        if h.sessions.tokens.get(token).map(String::as_str) != Some(id) {
            return Err("settings draft owner mismatch".into());
        }
        let context = h
            .sessions
            .files
            .get(token)
            .and_then(|grant| grant.context.clone())
            .ok_or("settings draft expired")?;
        let action = h
            .configuration_actions
            .get(&context)
            .ok_or("settings scope missing")?;
        let global_scope = action.is_empty();
        let defaults = if definition && !global_scope {
            let draft = h
                .default_drafts
                .get(&context)
                .ok_or("Binding settings must be saved by their parent editor")?;
            if draft.owner != id {
                return Err("action defaults owner mismatch".into());
            }
            let current = h
                .action_defaults
                .get(id)
                .and_then(|actions| actions.get(&draft.action))
                .cloned()
                .unwrap_or(json!({}));
            if current != draft.base {
                return Err("Action defaults changed. Reopen before saving.".into());
            }
            Some((draft.action.clone(), settings.clone()))
        } else {
            None
        };
        let draft = h
            .global_drafts
            .get(&context)
            .ok_or("global draft expired")?;
        let global = if definition && global_scope {
            Some(settings)
        } else if !global_scope && draft.value != draft.base {
            Some(draft.value.clone())
        } else {
            None
        };
        if global.is_some() && h.globals.get(id).cloned().unwrap_or(json!({})) != draft.base {
            return Err("Plugin settings changed. Reopen before saving.".into());
        }
        if global.is_none() && defaults.is_none() {
            return Ok(());
        }
        let previous_globals = h.globals.get(id).cloned();
        let previous_defaults = h.action_defaults.get(id).cloned();
        if let Some(value) = &global {
            let mut safe = value.clone();
            h.redact_payload(&mut safe);
            h.globals.insert(id.into(), safe);
        }
        if let Some((action, mut value)) = defaults {
            h.redact_payload(&mut value);
            h.action_defaults
                .entry(id.into())
                .or_default()
                .insert(action, value);
        }
        if let Err(error) = h.persist_scoped(persistence::Scope {
            globals: &[id],
            defaults: &[id],
            metadata: &[],
        }) {
            if let Some(value) = previous_globals {
                h.globals.insert(id.into(), value);
            } else {
                h.globals.remove(id);
            }
            if let Some(value) = previous_defaults {
                h.action_defaults.insert(id.into(), value);
            } else {
                h.action_defaults.remove(id);
            }
            return Err(error);
        }
        if global.is_some() && h.enabled.contains(id) {
            let settings = h.globals.get(id).cloned().unwrap_or(json!({}));
            h.sessions.queues.entry(id.into()).or_default().push_back(
                json!({"event":"didReceiveGlobalSettings","payload":{"settings":settings}}),
            );
        }
        Ok(())
    }
    // Deliver a desktop input result only to the context’s owning plugin
    pub fn desktop_input_result(&mut self, id: &str, context: &str, payload: Value) {
        let mut h = self.hub.lock().unwrap();
        if h.context_state.contexts.get(context).map(String::as_str) == Some(id) {
            h.sessions.queues.entry(id.into()).or_default().push_back(
                json!({"event":"desktopInputResult","context":context,"payload":payload}),
            );
        }
    }
}

impl PluginHost {
    // Check whether desktop input allowed
    pub fn desktop_input_allowed(&self, id: &str, context: &str, session: &str) -> bool {
        let h = self.hub.lock().unwrap();
        self.plugins.get(id).is_some_and(|p| {
            p.info.enabled && p.token == session && h.sessions.connected.contains(&p.token)
        }) && h.context_state.contexts.get(context).map(String::as_str) == Some(id)
            && !h.context_state.drafts.contains(context)
    }
}

impl PluginHost {
    // Check whether settings location allowed
    pub fn settings_location_allowed(&self, token: &str) -> Result<(), String> {
        let h = self.hub.lock().unwrap();
        let owner = h
            .sessions
            .tokens
            .get(token)
            .ok_or("Settings panel expired")?;
        let context = h
            .sessions
            .files
            .get(token)
            .and_then(|g| g.context.as_ref())
            .ok_or("Settings panel expired")?;
        if !h.location_drafts.contains(token)
            || !h.context_state.drafts.contains(context)
            || h.context_state.contexts.get(context) != Some(owner)
            || !self.plugins.contains_key(owner)
        {
            return Err("Location access is not available for this settings panel".into());
        }
        Ok(())
    }
    // Select settings path
    pub fn select_settings_path(
        &mut self,
        token: &str,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        if key.len() > 80 || value.len() > 4096 || !Path::new(value).is_absolute() {
            return Err("invalid selected path".into());
        }
        let mut h = self.hub.lock().unwrap();
        let grant = h
            .sessions
            .files
            .get(token)
            .ok_or("settings panel expired")?;
        let context = grant
            .context
            .clone()
            .filter(|c| h.context_state.drafts.contains(c))
            .ok_or("settings draft missing")?;
        let action = h
            .configuration_actions
            .get(&context)
            .ok_or("settings scope missing")?;
        let module = read_settings_module(&grant.root);
        if !path_fields(settings_fields(
            &module,
            (!action.is_empty()).then_some(action.as_str()),
        ))
        .any(|(field, _)| field == key)
        {
            return Err("field is not a path selector".into());
        }
        h.sessions.queues.entry(format!("pi:{token}")).or_default().push_back(json!({"event":"settingsFieldSelected","context":context,"payload":{"key":key,"value":value}}));
        Ok(())
    }
}

pub(super) struct PendingInspectorRequest {
    pub(super) token: String,
    owner: String,
    context: String,
    message: Value,
    queued: std::time::Instant,
}
impl Hub {
    // Build the inspector lifecycle notification with its action metadata
    pub(super) fn inspector_lifecycle_event(&self, event: &str, context: &str) -> Value {
        let visible = self.context_state.visible.get(context);
        let action = visible
            .and_then(|value| value["action"].as_str())
            .or_else(|| {
                self.context_state
                    .context_actions
                    .get(context)
                    .map(String::as_str)
            })
            .or_else(|| self.configuration_actions.get(context).map(String::as_str))
            .unwrap_or("");
        let device = visible
            .and_then(|value| value["device"].as_str())
            .or_else(|| {
                self.context_state
                    .targets
                    .get(context)
                    .map(|(device, _)| device.as_str())
            })
            .unwrap_or("");
        json!({"event":event,"context":context,"action":action,"device":device})
    }

    // Revoke setup runtimes
    pub(super) fn revoke_setup_runtimes(&mut self, id: &str) {
        for (token, grant) in &mut self.sessions.files {
            if self
                .sessions
                .tokens
                .get(token)
                .is_some_and(|owner| owner == id)
            {
                grant.setup_runtime = false;
            }
        }
    }

    // Check whether inspector runtime allowed
    fn inspector_runtime_allowed(&self, token: &str, owner: &str) -> bool {
        self.enabled.contains(owner)
            || self
                .sessions
                .files
                .get(token)
                .is_some_and(|grant| grant.setup_runtime)
    }

    // Announce configuration inspectors
    pub(super) fn announce_configuration_inspectors(&mut self, owner: &str) {
        let Some(session) = self
            .sessions
            .connected
            .iter()
            .find(|session| self.sessions.tokens.get(*session).map(String::as_str) == Some(owner))
            .cloned()
        else {
            return;
        };
        for token in self.configuration_sessions.clone() {
            if self.sessions.tokens.get(&token).map(String::as_str) != Some(owner)
                || !self.inspector_runtime_allowed(&token, owner)
                || self.configuration_announced.get(&token) == Some(&session)
            {
                continue;
            }
            let Some(context) = self
                .sessions
                .files
                .get(&token)
                .and_then(|g| g.context.as_ref())
            else {
                continue;
            };
            if !self.configuration_actions.contains_key(context) {
                continue;
            }
            let Some(live_context) = self.inspector_live_context(context, owner) else {
                continue;
            };
            let event = self.inspector_lifecycle_event("propertyInspectorDidAppear", &live_context);
            self.sessions
                .queues
                .entry(owner.into())
                .or_default()
                .push_back(event);
            self.configuration_announced.insert(token, session.clone());
        }
    }
    // Construct the error response for a rejected inspector request
    fn inspector_request_error(&mut self, token: &str, context: &str, error: &str) {
        self.sessions.queues.entry(format!("pi:{token}")).or_default().push_back(json!({"event":"sendToPropertyInspector","context":context,"payload":{"error":error,"available":false}}));
    }
    // Registration can race the first click in an inspector. Hold bounded
    // requests separately from process queues, which are reset during launch.
    pub(super) fn defer_inspector_request(&mut self, token: &str, message: &Value) -> bool {
        let Some(owner) = self.sessions.tokens.get(token).cloned() else {
            return true;
        };
        let context = message["context"].as_str().unwrap_or("");
        let cancel = message["payload"]["op"] == "cancel_location_search";
        if cancel || message["payload"]["op"] == "search_location" {
            self.pending_inspectors.retain(|pending| {
                pending.context != context || pending.message["payload"]["op"] != "search_location"
            });
        }
        if !self.inspector_runtime_allowed(token, &owner) {
            self.inspector_request_error(token,context,"Enable this plugin in Plugins to use discovery and testing. No active binding is required.");
            return true;
        }
        if self
            .sessions
            .connected
            .iter()
            .any(|session| self.sessions.tokens.get(session) == Some(&owner))
        {
            return false;
        }
        if cancel {
            return true;
        }
        if self.pending_inspectors.len() >= 32
            || self
                .pending_inspectors
                .iter()
                .filter(|pending| pending.token == token)
                .count()
                >= 4
        {
            self.inspector_request_error(
                token,
                context,
                "Plugin request queue is busy. Wait and try again.",
            );
            return true;
        }
        self.pending_inspectors.push_back(PendingInspectorRequest {
            token: token.into(),
            owner,
            context: context.into(),
            message: message.clone(),
            queued: std::time::Instant::now(),
        });
        self.sessions.queues.entry(format!("pi:{token}")).or_default().push_back(json!({"event":"sendToPropertyInspector","context":context,"payload":{"pending":true,"message":"Starting plugin…"}}));
        if let Some(wake) = &self.sampler_wake {
            wake();
        }
        true
    }
    // Flush inspector requests
    pub(super) fn flush_inspector_requests(&mut self, plugins: &HashMap<String, Plugin>) {
        for pending in std::mem::take(&mut self.pending_inspectors) {
            if self.sessions.tokens.get(&pending.token) != Some(&pending.owner)
                || !self
                    .sessions
                    .files
                    .get(&pending.token)
                    .is_some_and(|g| g.context.as_ref() == Some(&pending.context))
            {
                continue;
            }
            let error = if !self.inspector_runtime_allowed(&pending.token, &pending.owner) {
                Some("Plugin is disabled. Enable it in Plugins.".to_string())
            } else if let Some(plugin) = plugins.get(&pending.owner) {
                if plugin.info.runtime.state == RuntimeState::Blocked {
                    Some(plugin.info.status.clone())
                } else if pending.queued.elapsed() > Duration::from_secs(15) {
                    Some("Plugin startup timed out. Check its health and runtime in Plugins, then try again.".into())
                } else {
                    None
                }
            } else {
                Some("Plugin is no longer installed".into())
            };
            if let Some(error) = error {
                self.inspector_request_error(&pending.token, &pending.context, &error);
            } else if self
                .sessions
                .connected
                .iter()
                .any(|session| self.sessions.tokens.get(session) == Some(&pending.owner))
            {
                if let Some(context) = self.inspector_live_context(&pending.context, &pending.owner)
                {
                    let mut message = pending.message;
                    message["context"] = json!(context);
                    self.sessions
                        .queues
                        .entry(pending.owner)
                        .or_default()
                        .push_back(message);
                } else {
                    self.inspector_request_error(
                        &pending.token,
                        &pending.context,
                        "This saved action is not active on a device.",
                    );
                }
            } else {
                self.pending_inspectors.push_back(pending);
            }
        }
    }
}

#[cfg(test)]
mod default_tests {
    use super::*;
    #[test]
    // Verify inspector lifecycle payload preserves live action and device
    fn inspector_lifecycle_payload_preserves_live_action_and_device() {
        let root = tempfile::tempdir().unwrap();
        let host = fixture(root.path());
        let mut hub = host.hub.lock().unwrap();
        hub.context_state.visible.insert(
            "live".into(),
            json!({
                "action":"fixture.weather.action", "device":"device-one"
            }),
        );
        for event in [
            "propertyInspectorDidAppear",
            "propertyInspectorDidDisappear",
        ] {
            assert_eq!(
                hub.inspector_lifecycle_event(event, "live"),
                json!({
                    "event":event, "context":"live", "action":"fixture.weather.action", "device":"device-one"
                })
            );
        }
        hub.configuration_actions
            .insert("setup".into(), "fixture.weather.action".into());
        let setup = hub.inspector_lifecycle_event("propertyInspectorDidAppear", "setup");
        assert_eq!(setup["action"], "fixture.weather.action");
        assert_eq!(setup["device"], "");
        hub.context_state
            .context_actions
            .insert("bound".into(), "fixture.weather.action".into());
        hub.context_state
            .targets
            .insert("bound".into(), ("device-two".into(), "key".into()));
        assert_eq!(
            hub.inspector_lifecycle_event("propertyInspectorDidAppear", "bound")["device"],
            "device-two"
        );
    }

    #[test]
    // Verify editor retirement settings drafts exclude independent browser runtime
    fn editor_retirement_settings_drafts_exclude_independent_browser_runtime() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        host.enable("fixture.weather", true).unwrap();
        super::super::lifecycle_tests::demand(&mut host, "fixture.weather");
        let browser = host.browser_view("fixture.weather").unwrap().unwrap();
        assert!(!host.has_settings_drafts());
        let panel = host
            .settings_panel(
                "fixture.weather",
                Some("fixture.weather.action"),
                &json!({}),
            )
            .unwrap();
        assert!(host.has_settings_drafts());
        host.close_inspector(&panel.token);
        assert!(!host.has_settings_drafts());
        assert!(host
            .view_labels("fixture.weather")
            .unwrap()
            .contains(&browser.label));
    }

    // Construct the isolated test fixture
    fn fixture(root: &Path) -> PluginHost {
        let path = root.join("fixture.weather");
        super::super::lifecycle_tests::package(&path, "fixture.weather");
        let manifest = path.join("manifest.json");
        let mut value: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        value["Actions"]
            .as_array_mut()
            .unwrap()
            .push(json!({"UUID":"fixture.weather.forecast","Name":"Forecast"}));
        super::super::lifecycle_tests::current_descriptor(&path, &mut value);
        let mut host = super::super::lifecycle_tests::host(root);
        host.plugins
            .get_mut("fixture.weather")
            .unwrap()
            .info
            .capabilities = Some(Capabilities {
            id: "fixture.weather".into(),
            settings_protocol: 1,
            actions: std::collections::BTreeMap::from([
                (
                    "fixture.weather.action".into(),
                    ActionCapability {
                        settings: Some("inspector.html".into()),
                        ..Default::default()
                    },
                ),
                (
                    "fixture.weather.forecast".into(),
                    ActionCapability {
                        settings: Some("inspector.html".into()),
                        ..Default::default()
                    },
                ),
            ]),
            ..Default::default()
        });
        host
    }
    // Set for the current operation
    fn set(host: &mut PluginHost, panel: &SettingsPanel, value: Value) {
        host.hub
            .lock()
            .unwrap()
            .context_state
            .settings
            .insert(format!("settings-draft:{}", panel.token), value);
    }
    #[test]
    // Verify configuration inspector can request and receive data during runtime startup
    fn configuration_inspector_can_request_and_receive_data_during_runtime_startup() {
        exercise_configuration_inspector_runtime(false);
    }
    #[test]
    // Verify disabled setup tools run and save before enablement
    fn disabled_setup_tools_run_and_save_before_enablement() {
        exercise_configuration_inspector_runtime(true);
    }
    // Exercise the configuration inspector against an isolated socket host
    fn exercise_configuration_inspector_runtime(setup: bool) {
        use tungstenite::{client, Message};
        let root = tempfile::tempdir().unwrap();
        super::super::lifecycle_tests::package(&root.path().join("fixture"), "fixture.configure");
        let mut host = super::super::lifecycle_tests::socket_host(root.path());
        let panel = if setup {
            host.plugins
                .get_mut("fixture.configure")
                .unwrap()
                .info
                .capabilities = Some(Capabilities {
                id: "fixture.configure".into(),
                plugin_settings: Some("inspector.html".into()),
                settings_protocol: 1,
                ..Default::default()
            });
            host.setup_settings_panel("fixture.configure").unwrap()
        } else {
            host.enable("fixture.configure", true).unwrap();
            host.default_settings_panel("fixture.configure", "fixture.configure.action")
                .unwrap()
        };
        let context = format!("settings-draft:{}", panel.token);
        let socket = || {
            let stream = TcpStream::connect(("127.0.0.1", host.port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            client(format!("ws://127.0.0.1:{}", host.port), stream)
                .unwrap()
                .0
        };
        let mut inspector = socket();
        inspector
            .send(Message::text(
                json!({"event":"registerPropertyInspector","uuid":panel.token}).to_string(),
            ))
            .unwrap();
        inspector.send(Message::text(json!({"event":"sendToPlugin","context":context,"action":"fixture.configure.action","payload":{"op":"search_location","request_id":"search-1"}}).to_string())).unwrap();
        loop {
            let reply: Value =
                serde_json::from_str(inspector.read().unwrap().to_text().unwrap()).unwrap();
            if reply["payload"]["pending"] == true {
                break;
            }
        }
        assert!(host.required.is_empty());
        assert!(host.plugins["fixture.configure"].token.is_empty());
        host.poll();
        let session = host.plugins["fixture.configure"].token.clone();
        let mut runtime = {
            let stream = TcpStream::connect(("127.0.0.1", host.port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            client(format!("ws://127.0.0.1:{}", host.port), stream)
                .unwrap()
                .0
        };
        runtime
            .send(Message::text(
                json!({"event":"registerPlugin","uuid":session,"payload":{"capabilities":host.plugins["fixture.configure"].info.capabilities}}).to_string(),
            ))
            .unwrap();
        let start = std::time::Instant::now();
        while !host
            .hub
            .lock()
            .unwrap()
            .sessions
            .connected
            .contains(&session)
        {
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        host.poll();
        if setup {
            assert!(!host.list()[0].enabled);
            assert!(!host.plugins["fixture.configure"].info.enabled);
            assert!(host.browser_view("fixture.configure").unwrap().is_some());
            assert_eq!(
                host.plugins["fixture.configure"].info.runtime.reason,
                RuntimeReason::Configuration
            );
            host.synchronize(
                vec![ActiveAction {
                    plugin_id: "fixture.configure".into(),
                    action_id: "fixture.configure.action".into(),
                    device_id: "device".into(),
                    control_id: "key".into(),
                    scope: "profile".into(),
                    settings: json!({}),
                    surface: None,
                    column: 0,
                    row: 0,
                    encoder: false,
                }],
                vec![],
            );
            assert!(host.hub.lock().unwrap().context_state.visible.is_empty());
        }
        let mut events = Vec::new();
        loop {
            let event: Value =
                serde_json::from_str(runtime.read().unwrap().to_text().unwrap()).unwrap();
            events.push(event.clone());
            if event["event"] == "sendToPlugin" {
                break;
            }
        }
        assert_eq!(events[0]["event"], "didReceiveGlobalSettings");
        assert_eq!(
            events
                .iter()
                .filter(|e| e["event"] == "propertyInspectorDidAppear" && e["context"] == context)
                .count(),
            1
        );
        assert_eq!(events.last().unwrap()["payload"]["request_id"], "search-1");
        runtime.send(Message::text(json!({"event":"sendToPropertyInspector","context":context,"payload":{"op":"location_results","request_id":"search-1","results":[]}}).to_string())).unwrap();
        loop {
            let reply: Value =
                serde_json::from_str(inspector.read().unwrap().to_text().unwrap()).unwrap();
            if reply["payload"]["op"] == "location_results" {
                assert_eq!(reply["payload"]["request_id"], "search-1");
                break;
            }
        }
        {
            let mut hub = host.hub.lock().unwrap();
            hub.announce_configuration_inspectors("fixture.configure");
            assert!(!hub
                .sessions
                .queues
                .get("fixture.configure")
                .is_some_and(|q| q.iter().any(|e| e["event"] == "propertyInspectorDidAppear")));
        }
        if setup {
            inspector
                .send(Message::text(
                    json!({"event":"setGlobalSettings","payload":{"server":"chosen"}}).to_string(),
                ))
                .unwrap();
            loop {
                let reply: Value =
                    serde_json::from_str(inspector.read().unwrap().to_text().unwrap()).unwrap();
                if reply["event"] == "settingsDraftSaved" {
                    break;
                }
            }
            assert!(host
                .configuration("fixture.configure")
                .unwrap()
                .settings
                .as_object()
                .unwrap()
                .is_empty());
            host.commit_plugin_settings("fixture.configure", &panel.token)
                .unwrap();
            assert_eq!(
                host.configuration("fixture.configure").unwrap().settings["server"],
                "chosen"
            );
            assert!(!host
                .hub
                .lock()
                .unwrap()
                .enabled
                .contains("fixture.configure"));
        }
        host.close_inspector(&panel.token);
        assert!(host.plugins["fixture.configure"].token.is_empty());
        assert!(host.hub.lock().unwrap().configuration_actions.is_empty());
        if setup {
            let panel = host.setup_settings_panel("fixture.configure").unwrap();
            assert!(host.configuration_demand().contains("fixture.configure"));
            host.stop_for_replacement("fixture.configure").unwrap();
            assert!(!host.configuration_demand().contains("fixture.configure"));
            host.close_inspector(&panel.token);
        }
        host.shutdown();
    }
    #[test]
    // Verify enabled settings panels hold runtime leases without profile or binding demand
    fn enabled_settings_panels_hold_runtime_leases_without_profile_or_binding_demand() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        host.enable("fixture.weather", true).unwrap();
        assert_eq!(
            host.plugins["fixture.weather"].info.runtime.state,
            RuntimeState::Idle
        );
        let first = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        let second = host
            .settings_panel(
                "fixture.weather",
                Some("fixture.weather.forecast"),
                &json!({}),
            )
            .unwrap();
        assert_eq!(
            host.plugins["fixture.weather"].info.runtime.state,
            RuntimeState::Queued
        );
        assert!(host.required.is_empty());
        host.poll();
        let token = host.plugins["fixture.weather"].token.clone();
        assert!(!token.is_empty());
        host.hub
            .lock()
            .unwrap()
            .sessions
            .connected
            .insert(token.clone());
        host.poll();
        assert_eq!(
            host.plugins["fixture.weather"].info.runtime.reason,
            RuntimeReason::Configuration
        );
        host.reconcile_demand(Default::default());
        host.close_inspector(&first.token);
        assert_eq!(host.plugins["fixture.weather"].token, token);
        assert_eq!(
            host.plugins["fixture.weather"].info.runtime.state,
            RuntimeState::Running
        );
        host.close_inspector(&second.token);
        assert!(host.plugins["fixture.weather"].token.is_empty());
        assert_eq!(
            host.plugins["fixture.weather"].info.runtime.state,
            RuntimeState::Idle
        );
        assert!(host.plugins["fixture.weather"].info.enabled);
    }
    #[test]
    // Verify closing configuration preserves profile demand and disabled forms do not launch
    fn closing_configuration_preserves_profile_demand_and_disabled_forms_do_not_launch() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        // Explicit background mode preserves profile demand when the settings lease closes
        host.set_sleep_when_hidden("fixture.weather", false)
            .unwrap();
        let disabled = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        host.poll();
        assert!(host.plugins["fixture.weather"].token.is_empty());
        host.enable("fixture.weather", true).unwrap();
        host.poll();
        let token = host.plugins["fixture.weather"].token.clone();
        assert!(!token.is_empty());
        host.hub
            .lock()
            .unwrap()
            .sessions
            .connected
            .insert(token.clone());
        host.poll();
        host.reconcile_demand(HashMap::from([(
            "fixture.weather".into(),
            vec![PluginDemand {
                device_id: "device".into(),
                profile_id: "profile".into(),
            }],
        )]));
        host.close_inspector(&disabled.token);
        assert_eq!(host.plugins["fixture.weather"].token, token);
        assert_eq!(
            host.plugins["fixture.weather"].info.runtime.reason,
            RuntimeReason::RequiredByActiveProfile
        );
        host.reconcile_demand(Default::default());
        assert!(host.plugins["fixture.weather"].token.is_empty());
    }
    #[test]
    // Verify inspector requests wait for registration survive launch and cancel with the panel
    fn inspector_requests_wait_for_registration_survive_launch_and_cancel_with_the_panel() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        host.enable("fixture.weather", true).unwrap();
        let panel = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        let context = format!("settings-draft:{}", panel.token);
        let request = json!({"event":"sendToPlugin","context":context,"action":"fixture.weather.action","payload":{"op":"search_location","request_id":"first"}});
        assert!(host
            .hub
            .lock()
            .unwrap()
            .defer_inspector_request(&panel.token, &request));
        host.poll();
        let token = host.plugins["fixture.weather"].token.clone();
        assert_eq!(host.hub.lock().unwrap().pending_inspectors.len(), 1);
        {
            let mut hub = host.hub.lock().unwrap();
            hub.sessions.connected.insert(token);
            hub.sessions
                .queues
                .entry("fixture.weather".into())
                .or_default()
                .push_back(json!({"event":"didReceiveGlobalSettings"}));
        }
        host.poll();
        {
            let mut hub = host.hub.lock().unwrap();
            assert!(hub.pending_inspectors.is_empty());
            let queue = hub.sessions.queues.get_mut("fixture.weather").unwrap();
            assert_eq!(
                queue.pop_front().unwrap()["event"],
                "didReceiveGlobalSettings"
            );
            assert_eq!(queue.pop_front().unwrap(), request);
        }
        host.close_inspector(&panel.token);
        let panel = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        let mut request = request;
        request["context"] = json!(format!("settings-draft:{}", panel.token));
        host.hub
            .lock()
            .unwrap()
            .defer_inspector_request(&panel.token, &request);
        let mut cancel = request.clone();
        cancel["payload"]["op"] = json!("cancel_location_search");
        host.hub
            .lock()
            .unwrap()
            .defer_inspector_request(&panel.token, &cancel);
        assert!(host.hub.lock().unwrap().pending_inspectors.is_empty());
        host.hub
            .lock()
            .unwrap()
            .defer_inspector_request(&panel.token, &request);
        host.close_inspector(&panel.token);
        assert!(host.hub.lock().unwrap().pending_inspectors.is_empty());
    }
    #[test]
    // Verify unavailable runtime requests fail in the inspector without requiring a binding
    fn unavailable_runtime_requests_fail_in_the_inspector_without_requiring_a_binding() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        let panel = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        let context = format!("settings-draft:{}", panel.token);
        let request = json!({"event":"sendToPlugin","context":context,"payload":{"op":"test"}});
        assert!(host
            .hub
            .lock()
            .unwrap()
            .defer_inspector_request(&panel.token, &request));
        assert!(host.hub.lock().unwrap().pending_inspectors.is_empty());
        host.enable("fixture.weather", true).unwrap();
        host.hub
            .lock()
            .unwrap()
            .defer_inspector_request(&panel.token, &request);
        host.plugins
            .get_mut("fixture.weather")
            .unwrap()
            .info
            .runtime
            .state = RuntimeState::Blocked;
        host.plugins.get_mut("fixture.weather").unwrap().info.status =
            "Blocked: runtime missing".into();
        host.hub
            .lock()
            .unwrap()
            .flush_inspector_requests(&host.plugins);
        let mut hub = host.hub.lock().unwrap();
        assert!(hub.pending_inspectors.is_empty());
        assert!(hub
            .sessions
            .queues
            .get_mut(&format!("pi:{}", panel.token))
            .unwrap()
            .iter()
            .any(|m| m["payload"]["error"] == "Blocked: runtime missing"));
    }
    #[test]
    // Verify vendor baseline opens standard inspector but not manager setup
    fn vendor_baseline_opens_standard_inspector_but_not_manager_setup() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        host.plugins
            .get_mut("fixture.weather")
            .unwrap()
            .info
            .capabilities = None;
        let panel = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        host.close_inspector(&panel.token);
        assert!(host.setup_settings_panel("fixture.weather").is_err());
        assert!(host.configuration_demand().is_empty());
    }
    #[test]
    // Verify deleting a plugin revokes configuration panels and queued requests
    fn deleting_a_plugin_revokes_configuration_panels_and_queued_requests() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        host.enable("fixture.weather", true).unwrap();
        let panel = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        let context = format!("settings-draft:{}", panel.token);
        host.hub.lock().unwrap().defer_inspector_request(
            &panel.token,
            &json!({"event":"sendToPlugin","context":context,"payload":{"op":"search_location"}}),
        );
        host.delete("fixture.weather").unwrap();
        assert!(host.panel_settings(&panel.token).is_err());
        assert!(host.configuration_demand().is_empty());
        let hub = host.hub.lock().unwrap();
        assert!(hub.pending_inspectors.is_empty());
        assert!(hub.configuration_actions.is_empty());
        assert!(hub.context_state.drafts.is_empty());
        assert!(hub.sessions.files.is_empty());
    }
    #[test]
    // Verify managed inspector global and action writes are independent and cancel isolated
    fn managed_inspector_global_and_action_writes_are_independent_and_cancel_isolated() {
        use tungstenite::{client, Message};
        let root = tempfile::tempdir().unwrap();
        super::super::lifecycle_tests::package(&root.path().join("legacy"), "fixture.legacy");
        let mut host = super::super::lifecycle_tests::socket_host(root.path());
        let panel = host
            .settings_panel(
                "fixture.legacy",
                Some("fixture.legacy.action"),
                &json!({"units":"celsius","unknown":true}),
            )
            .unwrap();
        let stream = TcpStream::connect(("127.0.0.1", host.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (mut socket, _) = client(format!("ws://127.0.0.1:{}", host.port), stream).unwrap();
        for message in [
            json!({"event":"registerPropertyInspector","uuid":panel.token}),
            json!({"event":"setGlobalSettings","payload":{"host":"https://example.test","flag":false}}),
            json!({"event":"setSettings","payload":{"units":"fahrenheit","unknown":true}}),
            json!({"event":"getGlobalSettings"}),
        ] {
            socket.send(Message::text(message.to_string())).unwrap();
        }
        loop {
            let response: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            if response["event"] == "didReceiveGlobalSettings" {
                assert_eq!(response["payload"]["settings"]["flag"], false);
                break;
            }
        }
        assert_eq!(
            host.panel_settings(&panel.token).unwrap(),
            json!({"units":"fahrenheit","unknown":true})
        );
        assert!(!host
            .hub
            .lock()
            .unwrap()
            .globals
            .contains_key("fixture.legacy"));
        host.commit_panel_globals(&panel.token).unwrap();
        host.close_inspector(&panel.token);
        assert_eq!(
            host.hub.lock().unwrap().globals["fixture.legacy"],
            json!({"host":"https://example.test","flag":false})
        );
        let cancelled = host
            .settings_panel("fixture.legacy", Some("fixture.legacy.action"), &json!({}))
            .unwrap();
        host.hub
            .lock()
            .unwrap()
            .global_drafts
            .get_mut(&format!("settings-draft:{}", cancelled.token))
            .unwrap()
            .value = json!({"host":"cancelled"});
        host.close_inspector(&cancelled.token);
        assert_eq!(
            host.hub.lock().unwrap().globals["fixture.legacy"]["host"],
            "https://example.test"
        );
        host.shutdown();
    }
    #[test]
    // Verify global conflicts cannot partially commit another actions defaults
    fn global_conflicts_cannot_partially_commit_another_actions_defaults() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        let first = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        let second = host
            .default_settings_panel("fixture.weather", "fixture.weather.forecast")
            .unwrap();
        {
            let mut hub = host.hub.lock().unwrap();
            hub.global_drafts
                .get_mut(&format!("settings-draft:{}", first.token))
                .unwrap()
                .value = json!({"host":"first"});
            hub.global_drafts
                .get_mut(&format!("settings-draft:{}", second.token))
                .unwrap()
                .value = json!({"host":"second"});
        }
        set(&mut host, &first, json!({"units":"fahrenheit"}));
        set(&mut host, &second, json!({"units":"celsius"}));
        host.commit_plugin_settings("fixture.weather", &first.token)
            .unwrap();
        assert!(host
            .commit_plugin_settings("fixture.weather", &second.token)
            .unwrap_err()
            .contains("Plugin settings changed"));
        assert_eq!(
            host.action_defaults("fixture.weather", "fixture.weather.forecast")
                .unwrap(),
            json!({})
        );
        assert_eq!(
            host.hub.lock().unwrap().globals["fixture.weather"]["host"],
            "first"
        );
    }
    #[test]
    // Verify path picker grants are limited to the active settings scope
    fn path_picker_grants_are_limited_to_the_active_settings_scope() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        std::fs::write(
            root.path().join("fixture.weather/settings-module.json"),
            json!({
                "plugin": [{"key":"folder","type":"path","path_picker":"folder"}],
                "actions": {
                    "fixture.weather.action": [{"key":"output","type":"path","path_picker":"save"}],
                    "fixture.weather.forecast": [{"key":"input","type":"path"}]
                }
            })
            .to_string(),
        )
        .unwrap();
        let panel = host
            .settings_panel(
                "fixture.weather",
                Some("fixture.weather.action"),
                &json!({}),
            )
            .unwrap();
        assert_eq!(panel.path_picker_modes.unwrap()["output"], "save");
        let missing = host
            .settings_panel(
                "fixture.weather",
                Some("fixture.weather.forecast"),
                &json!({}),
            )
            .unwrap();
        assert!(missing.path_picker_modes.is_none());
        assert!(host
            .select_settings_path(&missing.token, "input", "/tmp/fixture")
            .is_err());
        host.close_inspector(&missing.token);
        let destination = root.path().join("new.png");
        let destination = destination.to_str().unwrap();
        for field in ["input", "folder", "other"] {
            assert!(host
                .select_settings_path(&panel.token, field, destination)
                .is_err());
        }
        assert!(host
            .select_settings_path(&panel.token, "output", "relative.png")
            .is_err());
        host.select_settings_path(&panel.token, "output", destination)
            .unwrap();
        let selected = host
            .hub
            .lock()
            .unwrap()
            .sessions
            .queues
            .get_mut(&format!("pi:{}", panel.token))
            .unwrap()
            .pop_front()
            .unwrap();
        assert_eq!(
            selected["payload"],
            json!({"key": "output", "value": destination}),
        );
        host.close_inspector(&panel.token);
        assert!(host
            .select_settings_path(&panel.token, "output", destination)
            .is_err());
    }
    #[test]
    // Verify location grants require declared coordinate fields and expire with the panel
    fn location_grants_require_declared_coordinate_fields_and_expire_with_the_panel() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        let path = root.path().join("fixture.weather/settings-module.json");
        std::fs::write(&path,serde_json::to_vec(&json!({"location":true,"actions":{"fixture.weather.action":[{"key":"latitude","type":"number"},{"key":"longitude","type":"number"}]}})).unwrap()).unwrap();
        let panel = host
            .settings_panel(
                "fixture.weather",
                Some("fixture.weather.action"),
                &json!({}),
            )
            .unwrap();
        assert!(panel.location);
        assert!(host.settings_location_allowed(&panel.token).is_ok());
        assert!(host.settings_location_allowed("forged").is_err());
        let other = host
            .settings_panel(
                "fixture.weather",
                Some("fixture.weather.forecast"),
                &json!({}),
            )
            .unwrap();
        assert!(!other.location);
        assert!(host.settings_location_allowed(&other.token).is_err());
        host.close_inspector(&panel.token);
        assert!(host.settings_location_allowed(&panel.token).is_err());
    }
    #[test]
    // Verify action defaults survive other forms restart and cancellation without changing bindings
    fn action_defaults_survive_other_forms_restart_and_cancellation_without_changing_bindings() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        let conditions = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        set(
            &mut host,
            &conditions,
            json!({"latitude":51.5,"longitude":-0.12,"noncommercial":true,"unknown":false}),
        );
        host.commit_plugin_settings("fixture.weather", &conditions.token)
            .unwrap();
        host.close_inspector(&conditions.token);
        let forecast = host
            .default_settings_panel("fixture.weather", "fixture.weather.forecast")
            .unwrap();
        assert_eq!(host.panel_settings(&forecast.token).unwrap(), json!({}));
        set(
            &mut host,
            &forecast,
            json!({"latitude":40.7,"longitude":-74.0,"noncommercial":true}),
        );
        host.commit_plugin_settings("fixture.weather", &forecast.token)
            .unwrap();
        host.close_inspector(&forecast.token);
        let binding = host
            .settings_panel(
                "fixture.weather",
                Some("fixture.weather.action"),
                &json!({"latitude":35.6,"longitude":139.7}),
            )
            .unwrap();
        set(&mut host, &binding, json!({"latitude":0}));
        host.close_inspector(&binding.token);
        assert_eq!(
            host.action_defaults("fixture.weather", "fixture.weather.action")
                .unwrap()["latitude"],
            51.5
        );
        let mut copy = host
            .action_defaults("fixture.weather", "fixture.weather.action")
            .unwrap();
        copy["latitude"] = json!(0);
        assert_eq!(
            host.action_defaults("fixture.weather", "fixture.weather.action")
                .unwrap()["latitude"],
            51.5
        );
        host.shutdown();
        drop(host);
        let mut restored = fixture(root.path());
        assert_eq!(
            restored
                .action_defaults("fixture.weather", "fixture.weather.action")
                .unwrap()["unknown"],
            false
        );
        assert_eq!(
            restored
                .action_defaults("fixture.weather", "fixture.weather.forecast")
                .unwrap()["latitude"],
            40.7
        );
        let cancelled = restored
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        set(&mut restored, &cancelled, json!({"latitude":-55}));
        restored.close_inspector(&cancelled.token);
        assert_eq!(
            restored
                .action_defaults("fixture.weather", "fixture.weather.action")
                .unwrap()["latitude"],
            51.5
        );
        restored.shutdown();
    }
    #[test]
    // Verify concurrent defaults and failed writes retain the last committed values
    fn concurrent_defaults_and_failed_writes_retain_the_last_committed_values() {
        let root = tempfile::tempdir().unwrap();
        let mut host = fixture(root.path());
        let first = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        let second = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        set(&mut host, &first, json!({"latitude":1}));
        set(&mut host, &second, json!({"latitude":2}));
        host.commit_plugin_settings("fixture.weather", &first.token)
            .unwrap();
        assert!(host
            .commit_plugin_settings("fixture.weather", &second.token)
            .unwrap_err()
            .contains("changed"));
        assert_eq!(
            host.action_defaults("fixture.weather", "fixture.weather.action")
                .unwrap()["latitude"],
            1
        );
        let failing = host
            .default_settings_panel("fixture.weather", "fixture.weather.action")
            .unwrap();
        set(&mut host, &failing, json!({"latitude":3}));
        host.storage()
            .run(|conn| {
                conn.execute_batch("PRAGMA query_only=ON")
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert!(host
            .commit_plugin_settings("fixture.weather", &failing.token)
            .is_err());
        assert_eq!(
            host.action_defaults("fixture.weather", "fixture.weather.action")
                .unwrap()["latitude"],
            1
        );
        host.shutdown();
    }
}

#[cfg(test)]
mod bounded_schema_tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn strict_schema_boundary_preserves_null_behavior() {
        for length in [
            SETTINGS_MODULE_LIMIT - 1,
            SETTINGS_MODULE_LIMIT,
            SETTINGS_MODULE_LIMIT + 1,
        ] {
            let mut bytes = b"{\"plugin\":[]}".to_vec();
            bytes.resize(length, b' ');
            let result = parse_settings_module(Cursor::new(bytes));
            assert_eq!(
                result,
                if length < SETTINGS_MODULE_LIMIT {
                    json!({"plugin": []})
                } else {
                    Value::Null
                }
            );
        }
        for bytes in [b"".as_slice(), b"{", b"{} trailing", &[0xff]] {
            assert_eq!(parse_settings_module(bytes), Value::Null);
        }
    }

    #[test]
    fn reader_never_consumes_more_than_limit_even_without_eof() {
        struct Endless {
            read: usize,
        }
        impl Read for Endless {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                out.fill(b' ');
                self.read += out.len();
                Ok(out.len())
            }
        }
        let mut reader = Endless { read: 0 };
        assert_eq!(parse_settings_module(&mut reader), Value::Null);
        assert_eq!(reader.read, SETTINGS_MODULE_LIMIT);
    }

    #[test]
    fn read_error_after_valid_prefix_does_not_authorize_schema() {
        struct FailsAfterPrefix(bool);
        impl Read for FailsAfterPrefix {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("fixture read failure"));
                }
                self.0 = true;
                out[..2].copy_from_slice(b"{}");
                Ok(2)
            }
        }
        assert_eq!(parse_settings_module(FailsAfterPrefix(false)), Value::Null);
    }

    #[test]
    fn missing_sparse_and_replaced_schema_are_not_cached() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("settings-module.json");
        assert_eq!(read_settings_module(root.path()), Value::Null);
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(8 * 1024 * 1024 * 1024).unwrap();
        assert_eq!(read_settings_module(root.path()), Value::Null);
        std::fs::write(&path, br#"{"plugin":[]}"#).unwrap();
        assert_eq!(read_settings_module(root.path()), json!({"plugin":[]}));
        std::fs::write(&path, b"malformed").unwrap();
        assert_eq!(read_settings_module(root.path()), Value::Null);
    }

    #[cfg(unix)]
    #[test]
    fn schema_symlink_behavior_is_unchanged_and_still_bounded() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("schema.json");
        std::fs::write(&target, br#"{"plugin":[]}"#).unwrap();
        std::os::unix::fs::symlink(&target, root.path().join("settings-module.json")).unwrap();
        assert_eq!(read_settings_module(root.path()), json!({"plugin":[]}));
        std::fs::write(&target, vec![b' '; SETTINGS_MODULE_LIMIT]).unwrap();
        assert_eq!(read_settings_module(root.path()), Value::Null);
    }
}

impl PluginHost {
    /// Serialize this edit with socket writes under the same hub guard. Persist the
    /// complete vendor JSON with the host's scoped admission and rollback behavior.
    pub fn edit_vault_setting(&self, id: &str, value: Value) -> Result<(), String> {
        let mut hub = self.hub.lock().unwrap();
        let id = id.to_owned();
        let setting = self
            .storage()
            .run(move |db| stream_dock_manager_storage::plugin_settings::setting(db, &id))?;
        let owner = &setting.owner;
        let installed = self.plugins.get(owner);
        let dialect = if let Some(plugin) = installed {
            plugin.info.dialect.clone()
        } else {
            self.storage()
                .read_json::<(String, bool)>(
                    stream_dock_manager_storage::Domain::PluginMetadata,
                    &format!("retained-family:{owner}"),
                    1024,
                )?
                .ok_or("The retained package dialect is unavailable")?
                .0
        };
        if value.to_string().len() > 256 * 1024 || (!value.is_object() && dialect != "openaction") {
            return Err(
                "The settings must match the plugin’s JSON format and fit within 256 KiB.".into(),
            );
        }
        let ids = [owner.as_str()];
        if setting.domain == stream_dock_manager_storage::Domain::PluginGlobals {
            let slot = hub
                .globals
                .get_mut(owner)
                .ok_or("Global settings changed")?;
            let old = std::mem::replace(slot, value.clone());
            if let Err(e) = hub.persist_scoped(persistence::Scope {
                globals: &ids,
                defaults: &[],
                metadata: &[],
            }) {
                hub.globals.insert(owner.clone(), old);
                return Err(e);
            }
            if hub.enabled.contains(owner) {
                hub.sessions
                    .queues
                    .entry(owner.clone())
                    .or_default()
                    .push_back(
                        json!({"event":"didReceiveGlobalSettings","payload":{"settings":value}}),
                    );
            }
        } else if setting.domain == stream_dock_manager_storage::Domain::PluginDefaults {
            let action = setting.action.ok_or("Missing action default")?;
            let slot = hub
                .action_defaults
                .get_mut(owner)
                .and_then(|d| d.get_mut(&action))
                .ok_or("Action default changed")?;
            let old = std::mem::replace(slot, value);
            if let Err(e) = hub.persist_scoped(persistence::Scope {
                globals: &[],
                defaults: &ids,
                metadata: &[],
            }) {
                hub.action_defaults
                    .get_mut(owner)
                    .unwrap()
                    .insert(action, old);
                return Err(e);
            }
        } else {
            return Err("Choose a plugin configuration record".into());
        }
        Ok(())
    }
}

/// The exact managed settings location to retire after a successful Vault edit
pub enum VaultSettingsClear {
    Global,
    Default(String),
    Binding,
    All,
}

impl PluginHost {
    /// The editor owns review and workspace locks. Refuse a live session instead
    /// of racing its writes or clearing another action's running cache.
    pub fn clear_vault_settings<T: Send + 'static>(
        &self,
        owner: &str,
        operation: impl FnOnce(
                &stream_dock_manager_storage::rusqlite::Transaction<'_>,
            ) -> Result<(T, VaultSettingsClear), String>
            + Send
            + 'static,
    ) -> Result<T, String> {
        use stream_dock_manager_storage::Domain;
        if !safe_id(owner) {
            return Err("Invalid plugin identity".into());
        }
        let mut hub = self.hub.lock().unwrap();
        if hub.enabled.contains(owner)
            || hub.sessions.tokens.values().any(|id| id == owner)
            || self.plugins.get(owner).is_some_and(|plugin| {
                plugin.child.is_some()
                    || plugin
                        .stopping
                        .as_ref()
                        .is_some_and(|done| !done.load(Ordering::Acquire))
            })
        {
            return Err(
                "Disable the plugin and close its settings before deleting saved data.".into(),
            );
        }
        let id = owner.to_owned();
        let (result, scope) = self.storage().transaction(move |tx| {
            let (result, scope) = operation(tx)?;
            match &scope {
                VaultSettingsClear::Global | VaultSettingsClear::All => {
                    stream_dock_manager_storage::delete(tx, Domain::PluginGlobals, &id)?;
                }
                _ => {}
            }
            match &scope {
                VaultSettingsClear::All => {
                    stream_dock_manager_storage::delete(tx, Domain::PluginDefaults, &id)?;
                    storage_rpc::purge(tx, &id)?;
                }
                VaultSettingsClear::Default(action) => {
                    let mut values: HashMap<String, Value> =
                        stream_dock_manager_storage::read_json(
                            tx,
                            Domain::PluginDefaults,
                            &id,
                            16 * 1024 * 1024,
                        )?
                        .ok_or("Action defaults changed")?;
                    if values.remove(action).is_none() {
                        return Err("Action defaults changed".into());
                    }
                    if values.is_empty() {
                        stream_dock_manager_storage::delete(tx, Domain::PluginDefaults, &id)?;
                    } else {
                        stream_dock_manager_storage::write_json(
                            tx,
                            Domain::PluginDefaults,
                            &id,
                            &values,
                            16 * 1024 * 1024,
                        )?;
                    }
                }
                _ => {}
            }
            stored_data::mark_updated(tx, &id)?;
            Ok((result, scope))
        })?;
        hub.invalidate_persistence_sizes();
        if matches!(scope, VaultSettingsClear::Global | VaultSettingsClear::All) {
            hub.globals.remove(owner);
        }
        match &scope {
            VaultSettingsClear::All => {
                hub.action_defaults.remove(owner);
            }
            VaultSettingsClear::Default(action) => {
                if let Some(values) = hub.action_defaults.get_mut(owner) {
                    values.remove(action);
                    if values.is_empty() {
                        hub.action_defaults.remove(owner);
                    }
                }
            }
            _ => {}
        }
        if matches!(scope, VaultSettingsClear::Binding | VaultSettingsClear::All) {
            let prefix = format!("{owner}:");
            hub.context_state
                .binding_settings
                .retain(|key, _| !key.starts_with(&prefix));
            let contexts: HashSet<_> = hub
                .context_state
                .contexts
                .iter()
                .filter(|(_, id)| id.as_str() == owner)
                .map(|(context, _)| context.clone())
                .collect();
            for context in &contexts {
                hub.context_state.settings.remove(context);
                hub.context_state.configured.remove(context);
                hub.context_state.execution.remove(context);
                hub.context_state.reference_sources.remove(context);
                hub.forget_saved_reads_for_context(context);
            }
            hub.pending_settings
                .retain(|_, (_, context)| !contexts.contains(context));
            hub.settings_bursts.revoke_owner(owner);
        }
        hub.refresh_log_secrets();
        Ok(result)
    }
}
