//! Local plugin package validation and authenticated Stream Deck / VSD message transport
pub mod manager_diagnostics;
pub mod recovery;
/// V1 wire budget: UTF-8 JSON bytes, shared with all SDKs and legacy hosts
/// Inline base64 images use this envelope; package-backed images keep their
/// separate decoded-resource limits.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub mod connection_contract;
mod registration_environment;
pub use connection_contract::CAPABILITIES;
pub use registration_environment::RegistrationEnvironment;
mod appearances;
mod effects;
mod hub_state;
pub use effects::{
    DesktopInputEffect, EffectTarget, FeedbackEffect, FeedbackKind, HostEffect, ProfileSwitchEffect,
};
use hub_state::{ContextState, Sessions, VisualState};
mod browser;
mod browser_resource_grants;
mod inspector_browser;
pub use inspector_browser::InspectorBrowser;
mod capabilities;
mod configured_actions;
pub use configured_actions::CONFIGURED_ACTIONS_OWNER;
pub mod log_capture;
pub mod manifest;
mod manifest_schema;
mod manual_actions;
mod monitor;
#[cfg(test)]
mod openaction_tests;
mod process_exit;
pub mod process_tree;
mod reconciliation;
mod reference_settings;
mod retired_contexts;
#[cfg(test)]
mod vendor_contract_tests;
mod vendor_presentation;
mod vendor_resources;
mod vendor_system;
pub use manual_actions::ManualActionLease;
mod listener;
mod server_admission;
mod settings;
pub use monitor::*;
pub use reconciliation::PluginReload;
pub use reference_settings::{preserve_references, redact_known_values};
pub use settings::PluginConfiguration;
pub use settings::VaultSettingsClear;
mod theme;
mod transport;
pub use capabilities::{ActionCapability, Capabilities, RefreshPolicy, ThemedAsset};
pub use settings::{SettingsLocationFix, SettingsPanel, SettingsPanelRequest};
use theme::theme_icon;
pub use theme::{
    palette_media_references, AppearancePreferences, CustomPalette, DeviceThemePreferences,
    PaletteWallpaper, ProfilePalette, ProfilePaletteTarget, UiThemeStyle,
};
mod development;
pub use development::LocalPackage;
pub mod backups;
mod deletion;
mod package_export;
pub use package_export::InstalledPackageExport;
mod packages;
pub use packages::prepare_dropped_folder_executables;
mod persistence;
pub use persistence::validate_storage_snapshot;
pub use persistence::Work as PersistenceWork;
pub use storage_rpc::{plugin_storage_generation, plugin_storage_purge_generation};
pub mod permission_rpc;
pub mod permissions;
mod saved_settings;
mod saved_settings_reads;
mod settings_burst;
mod storage_rpc;
mod stored_data;
pub mod text_references;
mod vault_lifecycle;
pub mod vault_references;
pub mod vault_rpc;
pub mod vault_text;
pub mod vault_writes;
pub use saved_settings::OwnedSettingsWrite;
pub use stored_data::StoredPluginData;
pub mod preflight;
mod transactions;
pub use packages::PackageSource;
pub mod artwork_cache;
mod vendor_profile_appearance;
mod visual;
use artwork_cache::{ArtworkCache, ArtworkFiles};
use browser::{bootstrap, sdk_platform, serve_file};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permissions: Option<permissions::PermissionDeclaration>,
    pub id: String,
    #[serde(default)]
    #[ts(optional)]
    pub author: Option<String>,
    pub name: String,
    pub version: String,
    pub dialect: String,
    pub enabled: bool,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub has_settings_interface: bool,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub sleep_when_hidden: bool,
    pub kind: String,
    pub status: String,
    pub actions: Vec<PluginAction>,
    #[serde(default)]
    #[ts(optional)]
    pub capabilities: Option<Capabilities>,
    #[serde(default)]
    #[ts(optional)]
    pub icon: Option<String>,
    #[serde(default)]
    pub appearance: String,
    pub health: PluginHealth,
    #[serde(default)]
    pub runtime: PluginRuntime,
    #[serde(default)]
    #[ts(optional)]
    pub running_runtime: Option<stream_dock_manager_plugin_runtimes::RuntimeInstallation>,
}
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(default)]
pub struct PluginHealth {
    pub totals: HealthCounts,
    pub session: HealthCounts,
    pub events: Vec<HealthEvent>,
    #[ts(type = "number | null")]
    pub last_registration_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub last_unexpected_exit_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub last_issue_ms: Option<u64>,
    pub last_exit: Option<String>,
    pub last_exit_code: Option<i32>,
    pub last_exit_signal: Option<i32>,
    pub blocked_reason: Option<String>,
    #[ts(type = "number | null")]
    pub last_run_peak_resident_bytes: Option<u64>,
    pub restart_attempts: u8,
    pub recent_failures: Vec<String>,
    pub missing_assets: Vec<String>,
    pub unsupported_apis: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginAction {
    pub id: String,
    pub name: String,
    /// Normalized application control kinds declared by the package's controller metadata
    pub control_types: Vec<String>,
    #[serde(default)]
    pub has_settings: bool,
    /// Vendor manifest defaults are true when the flag is omitted
    pub supported_in_multi_actions: bool,
    /// Elgato key-logic eligibility, separate from multi-action eligibility
    pub supported_in_key_logic_actions: bool,
    pub visible_in_actions_list: bool,
    pub user_title_enabled: bool,
    /// Manifest state labels in index order (not the mutable current state)
    pub state_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub multi_action_images: Option<Vec<Option<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encoder_icon: Option<String>,
    #[serde(default)]
    #[ts(optional)]
    pub icon: Option<String>,
}
// Collect the control types accepted by the action
fn action_control_types(action: &Value) -> Vec<String> {
    let Some(controllers) = action.get("Controllers").and_then(Value::as_array) else {
        return vec!["button".into()];
    };
    let has = |name: &str| controllers.iter().any(|value| value.as_str() == Some(name));
    let mut kinds = vec![];
    if has("Keypad") {
        kinds.push("button".into());
    }
    if has("Encoder") || has("Knob") {
        kinds.push("encoder".into());
    }
    if has("Encoder") || has("Information") {
        kinds.push("touch".into());
    }
    kinds
}
struct Plugin {
    manifest: Value,
    metadata: Arc<HashMap<String, Value>>,
    resource_history: VecDeque<ResourceSample>,
    resource_sampled_at: Option<std::time::Instant>,
    info: PluginInfo,
    path: PathBuf,
    entry: PathBuf,
    token: String,
    child: Option<Child>,
    exit_watch: Option<process_exit::Watch>,
    restarts: u8,
    next_restart: std::time::Instant,
    started: Option<std::time::Instant>,
    registered: Option<std::time::Instant>,
    tree: Option<process_tree::ProcessTree>,
    stopping: Option<Arc<AtomicBool>>,
}
impl Plugin {
    // Build the plugin diagnostic record
    fn diagnostic(
        &self,
        event: manager_diagnostics::PluginEvent,
        reason: Option<manager_diagnostics::FailureReason>,
        code: Option<i32>,
        signal: Option<i32>,
        recovery: bool,
    ) {
        manager_diagnostics::plugin(manager_diagnostics::PluginRecord {
            event,
            plugin_id: &self.info.id,
            version: &self.info.version,
            run_id: self.info.runtime.run_id.as_deref(),
            reason,
            exit_code: code,
            signal,
            attempt: self.restarts.into(),
            recovery,
        });
    }
}
#[derive(Debug, Clone)]
pub struct BrowserView {
    pub label: String,
    pub url: String,
}
pub struct ActiveAction {
    pub plugin_id: String,
    pub action_id: String,
    pub device_id: String,
    pub control_id: String,
    pub scope: String,
    pub settings: Value,
    pub surface: Option<Value>,
    pub column: usize,
    pub row: usize,
    pub encoder: bool,
}
#[derive(Clone)]
struct FileGrant {
    root: PathBuf,
    // Bootstrap only the primary runtime document. Inspector grants retain
    // their existing behavior across separately loaded settings documents.
    runtime_entry: Option<PathBuf>,
    bootstrap: String,
    context: Option<String>,
    setup_runtime: bool,
}
#[derive(Default)]
struct Hub {
    persistence_sizes: Mutex<Option<persistence::Sizes>>,
    persistence_work: Mutex<persistence::Work>,
    registration_environment: RegistrationEnvironment,
    visual_state: VisualState,
    sessions: Sessions,
    context_state: ContextState,
    configured_actions: configured_actions::ConfiguredActions,
    storage_admission: Arc<storage_rpc::Admission>,
    workflow: Arc<permission_rpc::Workflow>,
    permission_sessions: HashMap<String, permission_rpc::Session>,
    vault_budget: vault_rpc::ReadBudget,
    vault_reader: Option<Arc<dyn vault_rpc::VaultRead>>,
    vault_sessions: HashMap<String, vault_rpc::SessionScope>,
    settings_writer: Option<std::sync::mpsc::SyncSender<OwnedSettingsWrite>>,
    pending_settings: HashMap<String, (String, String)>,
    settings_bursts: settings_burst::SettingsBursts,
    pending_saved_reads: VecDeque<saved_settings_reads::SavedRead>,
    logs: Option<Arc<log_capture::LogCaptureService>>,
    network_debug_plugins: HashSet<String>,
    network_monitor_visible: bool,
    pending_inspectors: VecDeque<settings::PendingInspectorRequest>,
    global_drafts: HashMap<String, settings::GlobalDraft>,
    configuration_actions: HashMap<String, String>,
    configuration_sessions: HashSet<String>,
    configuration_announced: HashMap<String, String>,
    location_drafts: std::collections::HashSet<String>,
    sampler_wake: Option<Arc<dyn Fn() + Send + Sync>>,
    vendor_demand_wake: Option<Arc<dyn Fn() + Send + Sync>>,
    metadata: HashMap<String, Arc<HashMap<String, Value>>>,
    manifest_dialects: HashMap<String, String>,
    run_details: HashMap<String, (String, Option<String>)>,
    health_dirty: std::collections::BTreeSet<String>,
    network: HashMap<String, monitor::NetworkReport>,
    action_results: HashMap<String, PendingAction>,
    telemetry_topics: HashMap<String, std::collections::BTreeSet<String>>,
    provenance: HashMap<String, Value>,
    backup_receipts: HashMap<String, String>,
    installations: HashMap<String, String>,
    capabilities: HashMap<String, Capabilities>,
    appearance: HashMap<String, String>,
    ui_appearance: String,
    ui_theme_style: UiThemeStyle,
    custom_palettes: Vec<CustomPalette>,
    device_theme: DeviceThemePreferences,
    profile_palettes: HashMap<String, ProfilePalette>,
    manager_theme_device: String,
    system_dark: bool,
    theme_revision: u64,
    palette_theme_preview: Option<theme::PaletteThemePreview>,
    palette_preview_revision: u64,
    theme_observer: Arc<theme::ObserverSignal>,
    enabled: std::collections::HashSet<String>,
    sleep_when_hidden: HashMap<String, bool>,
    retained: HashMap<String, String>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    health: HashMap<String, PluginHealth>,
    effects: VecDeque<HostEffect>,
    profile_switch_requests: HashMap<String, std::time::Instant>,
    secret_redactions: Vec<String>,
    manual_actions: manual_actions::ManualActions,
    globals: HashMap<String, Value>,
    action_defaults: HashMap<String, HashMap<String, Value>>,
    default_drafts: HashMap<String, settings::DefaultDraft>,
    store: Option<stream_dock_manager_storage::Storage>,
    devices: HashMap<String, Value>,
    roots: HashMap<String, PathBuf>,
}
struct PendingAction {
    context: String,
    owner: String,
    session: String,
    sender: std::sync::mpsc::Sender<Result<(), String>>,
}
/// A correlated completion, independent of the host mutex while awaiting plugin work
pub struct ActionReceipt {
    completion_supported: bool,
    id: String,
    receiver: std::sync::mpsc::Receiver<Result<(), String>>,
    hub: Arc<Mutex<Hub>>,
}
impl ActionReceipt {
    pub fn completion_supported(&self) -> bool {
        self.completion_supported
    }
    /// Read an existing receipt without delaying its action lane.
    pub fn try_completion(&self) -> Option<bool> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result.is_ok()),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(false),
        }
    }
    // Wait for the current operation
    pub fn wait(
        &self,
        timeout: Duration,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let end = std::time::Instant::now() + timeout;
        loop {
            check()?;
            let remaining = end.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err("Plugin did not report action completion; icon unchanged".into());
            }
            match self
                .receiver
                .recv_timeout(remaining.min(Duration::from_millis(25)))
            {
                Ok(result) => {
                    check()?;
                    return result;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("Plugin stopped before reporting action completion".into());
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}
impl Drop for ActionReceipt {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.hub.lock().unwrap().action_results.remove(&self.id);
    }
}
impl Hub {
    // Check whether the plugin should stop when it has no visible demand
    fn sleeps_when_hidden(&self, id: &str) -> bool {
        self.sleep_when_hidden.get(id).copied().unwrap_or(true)
    }

    /// Stop/replacement revocation preserves draft inspectors and publishes release
    /// effects only after queuing disappearance/shutdown notices for the old run.
    fn revoke_plugin_sessions(&mut self, id: &str, token: &str) {
        self.workflow.invalidate_owner(id);
        self.permission_sessions.remove(token);
        self.vault_sessions.remove(token);
        self.forget_manual_contexts(id);
        if !token.is_empty() {
            let mut notices = transport::Outgoing::default();
            for event in self.context_state.visible.values().filter(|v| {
                self.context_state
                    .contexts
                    .get(v["context"].as_str().unwrap_or(""))
                    == Some(&id.to_string())
            }) {
                let mut event = event.clone();
                event["event"] = json!("willDisappear");
                notices.push_back(event);
            }
            notices.push_back(json!({"event":"shutdown"}));
            if self.sessions.connected.contains(token) {
                self.sessions.shutdowns.insert(token.to_owned(), notices);
            }
            self.sessions.connected.remove(token);
            self.notify_vendor_demand();
            self.effects
                .push_back(HostEffect::release_session(id, token));
        }
        self.telemetry_topics.remove(id);
        self.network.remove(id);
        let revoked: Vec<_> = self
            .sessions
            .tokens
            .iter()
            .filter(|(_, owner)| *owner == id)
            .map(|(token, _)| token.clone())
            .collect();
        let draft_tokens: std::collections::HashSet<_> = self
            .sessions
            .files
            .iter()
            .filter(|(_, grant)| {
                grant.context.as_ref().is_some_and(|context| {
                    self.context_state.drafts.contains(context)
                        || self.configuration_actions.contains_key(context)
                })
            })
            .map(|(token, _)| token.clone())
            .collect();
        self.sessions
            .tokens
            .retain(|token, owner| owner != id || draft_tokens.contains(token));
        for token in revoked
            .into_iter()
            .filter(|token| !draft_tokens.contains(token))
        {
            self.revoke_storage_session(&token);
            self.sessions.queues.remove(&format!("pi:{token}"));
            self.sessions.retired_contexts.clear_session(&token);
        }
        let tokens: std::collections::HashSet<_> = self.sessions.tokens.keys().cloned().collect();
        self.sessions
            .files
            .retain(|token, _| tokens.contains(token));
        self.sessions.queues.remove(id);
        self.action_results.retain(|_, pending| pending.owner != id);
        let removed: Vec<_> = self
            .context_state
            .contexts
            .iter()
            .filter(|(context, owner)| {
                *owner == id
                    && !self.context_state.drafts.contains(*context)
                    && !self.configuration_actions.contains_key(*context)
            })
            .map(|(context, _)| context.clone())
            .collect();
        for context in removed {
            if let Some(owner) = self.context_state.contexts.get(&context).cloned() {
                self.effects
                    .push_back(HostEffect::release_context(&owner, &context));
            }
            self.visual_state.render_generation =
                self.visual_state.render_generation.wrapping_add(1);
            self.retire_context(&context);
            self.context_state.targets.remove(&context);
            self.context_state.forget_authoring(&context);
            self.context_state.visible.remove(&context);
            self.visual_state.render_generation =
                self.visual_state.render_generation.wrapping_add(1);
            self.visual_state.remove(&context);
        }
        let prefix = format!("{id}:");
        self.effects.retain(|effect| {
            !effect
                .context()
                .is_some_and(|context| context.starts_with(&prefix))
        });
    }

    // Queue log preferences
    fn queue_log_preferences(&mut self, id: &str) {
        if !self.network_debug_plugins.contains(id) {
            return;
        }
        let debug_network = self
            .logs
            .as_ref()
            .is_some_and(|logs| logs.debug_network(id));
        self.sessions
            .queues
            .entry(id.into())
            .or_default()
            .push_back(
                json!({"event":"didReceiveLogSettings","payload":{"debug_network":debug_network}}),
            );
    }
    // Complete action
    fn complete_action(&mut self, owner: &str, session: &str, message: &Value) {
        let Some(id) = message["payload"]["request_id"].as_str() else {
            return;
        };
        if !self.action_results.get(id).is_some_and(|pending| {
            pending.owner == owner
                && pending.session == session
                && message["context"] == pending.context
        }) {
            return;
        }
        if let Some(success) = message["payload"]["success"].as_bool() {
            let pending = self.action_results.remove(id).unwrap();
            let _ = pending.sender.send(if success {
                Ok(())
            } else {
                self.issue(owner, "reported", "Plugin reported an action failure");
                Err("Plugin reported the action failed; icon unchanged".into())
            });
        }
    }
    /// Revocation is immediate. Only the authenticated plugin session may have
    /// already-sent responses ignored briefly; this never grants side effects.
    fn retire_context(&mut self, context: &str) -> Option<String> {
        self.workflow.invalidate_context(context);
        let owner = self.context_state.retire(context)?;
        self.forget_saved_reads_for_context(context);
        self.pending_settings
            .retain(|_, (_, pending_context)| pending_context != context);
        self.sessions.remember_retired_context(&owner, context);
        Some(owner)
    }
    // Retire an action context created for temporary execution
    fn retire_temporary_action_context(&mut self, context: &str) -> Option<HostEffect> {
        let owner = self.retire_context(context);
        if let Some(queue) = owner
            .as_ref()
            .and_then(|owner| self.sessions.queues.get_mut(owner))
        {
            queue.retain(|event| event["context"] != context);
            queue.push_back(json!({"event":"willDisappear","context":context}));
        }
        self.effects
            .retain(|effect| effect.context() != Some(context));
        self.action_results
            .retain(|_, pending| pending.context != context);
        self.context_state.transient_scopes.remove(context);
        self.context_state.targets.remove(context);
        self.context_state.scopes.remove(context);
        self.context_state.addresses.remove(context);
        self.context_state.settings.remove(context);
        self.context_state.configured.remove(context);
        self.context_state.execution.remove(context);
        self.context_state.reference_sources.remove(context);
        self.context_state.visible.remove(context);
        self.visual_state.remove(context);
        self.visual_state.render_generation = self.visual_state.render_generation.wrapping_add(1);
        owner.map(|owner| HostEffect::release_context(&owner, context))
    }
    // Discard retired response
    fn discard_retired_response(&mut self, session: &str, message: &Value) -> bool {
        message["context"].as_str().is_some_and(|context| {
            !self.context_state.contexts.contains_key(context)
                && self.sessions.retired_contexts.contains(
                    session,
                    context,
                    std::time::Instant::now(),
                )
        })
    }
}
/// A validated, private staging directory on the destination filesystem
pub struct PreparedPlugin {
    service_review: Option<(permission_rpc::InstallReview, Vec<permissions::ActionScope>)>,
    inventory: std::collections::BTreeMap<String, String>,
    manifest: manifest::NormalizedManifest,
    origin: PackageSource,
    provenance: Option<Value>,
    directory: tempfile::TempDir,
    source: PathBuf,
    id: String,
}
pub struct RenderFallback {
    root: PathBuf,
    asset: ThemedAsset,
    mode: &'static str,
    palette: String,
    colors: Value,
    title: String,
}
impl RenderFallback {
    /// Resolve after dropping PluginHost and hub locks; selection is immutable
    pub fn resolve(self) -> Option<Value> {
        let source = if self.mode == "light" {
            &self.asset.light
        } else {
            &self.asset.dark
        };
        let variant = theme::variant(&self.root, source, self.mode, &self.palette);
        let path = variant.as_deref().unwrap_or(source);
        let managed = variant.is_some()
            || safe_file(&self.root, path).ok().is_some_and(|path| {
                std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= 256 * 1024)
                    && std::fs::read(&path).is_ok_and(|bytes| {
                        visual::resolve_svg_roles(&bytes, &self.colors).is_some()
                    })
            });
        if managed {
            let image = visual::image_with_theme(&self.root, path, &self.colors).ok()?;
            let colors = &self.colors;
            return Some(json!({"image":image,"title":self.title,"paletteTile":true,
                "layout":{"id":"palette-fallback", "background":colors["surface"], "items":[
                    {"key":"icon", "type":"pixmap", "rect":[50,2,100,52]},
                    {"key":"label", "type":"text", "rect":[7,56,186,34], "color":colors["ink"],"font":{"size":23,"fit":true}}
                ]},"feedback":{"icon":image, "label":self.title}}));
        }
        let image = theme::theme_icon(&self.root, &self.asset, self.mode)?;
        Some(json!({"image":image,"title":"","layout":null,"feedback":{}}))
    }
}

pub struct RenderSurfaceSnapshot {
    pub artwork: HashMap<String, ArtworkFiles>,
    pub generation: u64,
    pub device_palette: Value,
    pub wallpaper: Option<PaletteWallpaper>,
    pub device_theme: DeviceThemePreferences,
    pub plugin_surfaces: HashMap<String, String>,
    pub values: HashMap<String, Arc<Value>>,
    pub fallbacks: HashMap<String, RenderFallback>,
    pub contexts: HashMap<String, String>,
}

pub struct PluginHost {
    recovery: recovery::Recovery,
    transport_threads: Vec<std::thread::JoinHandle<()>>,
    listener_wake: Option<Arc<mio::Waker>>,
    exit_registry: Option<Arc<process_exit::Registry>>,
    pub logs: Arc<log_capture::LogCaptureService>,
    pub runtimes: Arc<stream_dock_manager_plugin_runtimes::RuntimeService>,
    startup: VecDeque<String>,
    visible_plugins: HashSet<String>,
    starting: HashMap<String, std::time::Instant>,
    next_start: std::time::Instant,
    vault_locked: bool,
    vault_setup_waiting: HashSet<String>,
    root: PathBuf,
    plugins: HashMap<String, Plugin>,
    hub: Arc<Mutex<Hub>>,
    stop: Arc<AtomicBool>,
    port: u16,
    theme_revision: u64,
    required: HashMap<String, Vec<PluginDemand>>,
    replacement_claims: Arc<Mutex<HashSet<String>>>,
    last_health_flush: std::time::Instant,
}
impl PluginHost {
    /// Main-editor settings transactions, excluding independent inspector/browser grants
    /// Contention retains the editor; lifecycle commit must not wait on the hub
    pub fn has_settings_drafts(&self) -> bool {
        self.hub
            .try_lock()
            .map(|hub| {
                !hub.context_state.drafts.is_empty()
                    || !hub.global_drafts.is_empty()
                    || !hub.default_drafts.is_empty()
                    || !hub.location_drafts.is_empty()
            })
            .unwrap_or(true)
    }

    // Refresh log configuration
    pub fn refresh_log_configuration(&self) {
        let mut hub = self.hub.lock().unwrap();
        for id in self.plugins.keys() {
            hub.queue_log_preferences(id);
        }
    }
    // Construct the value and initialize its state
    pub fn new(root: PathBuf) -> Result<Self, String> {
        let configuration_root = if root.file_name().is_some_and(|name| name == "plugins") {
            root.parent()
                .ok_or("Plugin directory has no configuration root")?
                .to_path_buf()
        } else {
            root.join(".configuration")
        };
        let storage = stream_dock_manager_storage::Storage::open_validated(
            configuration_root.clone(),
            |connection| {
                validate_storage_snapshot(connection)?;
                stream_dock_manager_plugin_runtimes::validate_storage_snapshot(connection)
            },
        )?;
        let runtimes = stream_dock_manager_plugin_runtimes::RuntimeService::open_shared(
            configuration_root.join("runtimes"),
            storage.clone(),
        )?;
        Self::with_storage(root, runtimes, storage)
    }
    pub fn artwork_cache(&self) -> Arc<ArtworkCache> {
        let mut hub = self.hub.lock().unwrap();
        hub.visual_state
            .artwork_cache
            .get_or_insert_with(|| {
                Arc::new(ArtworkCache::new(
                    self.storage().root().join("cached-media"),
                ))
            })
            .clone()
    }
    // Access the storage used by this subsystem
    pub fn storage(&self) -> &stream_dock_manager_storage::Storage {
        self.runtimes.storage()
    }
    // Construct the service with explicit storage
    pub fn with_storage(
        root: PathBuf,
        runtimes: Arc<stream_dock_manager_plugin_runtimes::RuntimeService>,
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Self, String> {
        if storage.root() != runtimes.storage().root() {
            return Err(
                "Plugin host and runtime service must share one configuration storage authority"
                    .into(),
            );
        }
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let port = listener.local_addr().unwrap().port();
        let listener = listener::Listener::new(listener).map_err(|e| e.to_string())?;
        let listener_wake = listener.wake.clone();
        storage.run(|connection| validate_storage_snapshot(connection))?;
        storage.add_snapshot_validator(validate_storage_snapshot)?;
        deletion::recover(&root, &storage)?;
        transactions::recover(&root, &storage)?;
        let mut initial_hub = Hub::load(&storage)?;
        let artwork_cache = Arc::new(ArtworkCache::new(storage.root().join("cached-media")));
        initial_hub.visual_state.artwork_cache = Some(artwork_cache);
        let logs = log_capture::LogCaptureService::with_storage(storage)?;
        initial_hub.logs = Some(logs.clone());
        initial_hub.refresh_log_secrets();
        if initial_hub.follows_system_theme() {
            if let Some(dark) = theme::system_appearance() {
                initial_hub.system_dark = dark;
            }
        }
        let recovery = recovery::Recovery::open(runtimes.storage().clone())?;
        let hub = Arc::new(Mutex::new(initial_hub));
        let stop = Arc::new(AtomicBool::new(false));
        let observer_hub = hub.clone();
        let observer_stop = stop.clone();
        let observer_thread =
            std::thread::spawn(move || theme::observe(observer_hub, observer_stop));
        let h = hub.clone();
        let s = stop.clone();
        let admission = Arc::new(server_admission::Admission::default());
        let listener_thread = std::thread::spawn(move || {
            listener.run(s.clone(), |stream, completion| {
                spawn_connection(
                    stream,
                    h.clone(),
                    s.clone(),
                    admission.clone(),
                    Some(completion),
                )
            });
        });
        let weak_hub = Arc::downgrade(&hub);
        let exit_observer = process_exit::Registry::new(Arc::new(move || {
            if let Some(hub) = weak_hub.upgrade() {
                let wake = hub.lock().unwrap().wake.clone();
                if let Some(wake) = wake {
                    wake();
                }
            }
        }));
        let mut transport_threads = vec![observer_thread, listener_thread];
        let exit_registry = exit_observer.map(|(registry, thread)| {
            transport_threads.push(thread);
            registry
        });

        let mut host = Self {
            recovery,
            transport_threads,
            exit_registry,
            listener_wake: Some(listener_wake),
            logs,
            runtimes,
            startup: Default::default(),
            visible_plugins: Default::default(),
            starting: Default::default(),
            next_start: std::time::Instant::now(),
            vault_locked: false,
            vault_setup_waiting: Default::default(),
            root,
            plugins: HashMap::new(),
            hub,
            stop,
            port,
            theme_revision: 0,
            required: Default::default(),
            replacement_claims: Default::default(),
            last_health_flush: std::time::Instant::now(),
        };
        host.scan(true)?;
        let enabled = host.hub.lock().unwrap().enabled.clone();
        for id in enabled {
            if let Some(plugin) = host.plugins.get_mut(&id) {
                plugin.info.enabled = true;
                if let Some(reason) = host
                    .hub
                    .lock()
                    .unwrap()
                    .health
                    .get(&id)
                    .and_then(|h| h.blocked_reason.clone())
                {
                    plugin.info.runtime.state = RuntimeState::Blocked;
                    plugin.info.runtime.reason = RuntimeReason::Compatibility;
                    plugin.info.status = reason;
                } else {
                    plugin.info.runtime.state = RuntimeState::Idle;
                    plugin.info.runtime.reason = RuntimeReason::NoActiveProfileDemand;
                    plugin.info.status = "Idle — no active profile or open settings".into();
                }
            }
        }
        Ok(host)
    }
    // Scan the managed entries for this subsystem
    fn scan(&mut self, restore_enabled: bool) -> Result<usize, String> {
        let mut paths = std::fs::read_dir(&self.root)
            .map_err(|e| e.to_string())?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        paths.sort();
        let mut discovered = vec![];
        let mut seen = std::collections::HashSet::new();
        let mut actions: HashSet<String> = self
            .plugins
            .values()
            .flat_map(|p| p.info.actions.iter().map(|a| a.id.clone()))
            .collect();
        for path in paths {
            if path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                if let Ok(plugin) =
                    read_plugin_with_identity(&path, path.file_name().and_then(|s| s.to_str()))
                {
                    if !self.plugins.contains_key(&plugin.info.id)
                        && seen.insert(plugin.info.id.clone())
                        && !plugin.info.actions.iter().any(|a| actions.contains(&a.id))
                    {
                        actions.extend(plugin.info.actions.iter().map(|a| a.id.clone()));
                        discovered.push(plugin);
                    }
                }
            }
        }
        let mut hub = self.hub.lock().unwrap();
        if !restore_enabled {
            // Manually added packages require enabling, even if a stale preference exists
            let enabled = hub.enabled.clone();
            for plugin in &discovered {
                hub.enabled.remove(&plugin.info.id);
            }
            if hub.enabled != enabled {
                if let Err(error) = hub.persist() {
                    hub.enabled = enabled;
                    return Err(error);
                }
            }
        }
        let added = discovered.len();
        for plugin in discovered {
            hub.manifest_dialects
                .insert(plugin.info.id.clone(), plugin.info.dialect.clone());
            hub.metadata
                .insert(plugin.info.id.clone(), plugin.metadata.clone());
            if let Some(c) = &plugin.info.capabilities {
                hub.capabilities.insert(plugin.info.id.clone(), c.clone());
            }
            hub.roots
                .insert(plugin.info.id.clone(), plugin.path.clone());
            self.plugins.insert(plugin.info.id.clone(), plugin);
        }
        Ok(added)
    }
    // List the entries tracked by this subsystem
    pub fn list(&self) -> Vec<PluginInfo> {
        let hub = self.hub.lock().unwrap();
        let mut plugins: Vec<_> = self.plugins.values().collect();
        plugins.sort_by(|a, b| {
            a.info
                .name
                .cmp(&b.info.name)
                .then_with(|| a.info.id.cmp(&b.info.id))
        });
        let mut presentation_bytes = 2 * 1024 * 1024;
        let values: Vec<_> = plugins
            .into_iter()
            .map(|p| {
                let mut info = p.info.clone();
                // Trim each package before collecting, not after accumulating all image clones
                vendor_presentation::bound_plugin(&mut info, &mut presentation_bytes);
                info.appearance = hub
                    .appearance
                    .get(&info.id)
                    .cloned()
                    .unwrap_or("follow-ui".into());
                if let Some(c) = &info.capabilities {
                    let theme = hub.resolved_theme(&info.id);
                    info.icon = theme_icon(&p.path, &c.icon, theme);
                    for action in &mut info.actions {
                        if let Some(ac) = c.actions.get(&action.id) {
                            action.icon = theme_icon(&p.path, &ac.icon, theme);
                        }
                    }
                }
                info.enabled = hub.enabled.contains(&info.id);
                info.sleep_when_hidden = hub.sleeps_when_hidden(&info.id);
                info.runtime.resources = p.resources_at(std::time::Instant::now());
                info.runtime.uptime_ms = p.started.map(|at| at.elapsed().as_millis() as u64);
                if let Some(report) = hub
                    .network
                    .get(&info.id)
                    .filter(|n| Some(n.run_id.as_str()) == info.runtime.run_id.as_deref())
                {
                    info.runtime.network = report.reading.clone();
                    if report.sampled_at.elapsed() > Duration::from_secs(5) {
                        info.runtime.network.state = MeasurementState::Stale;
                        info.runtime.network.upload_bytes_per_second = None;
                        info.runtime.network.download_bytes_per_second = None;
                    }
                }
                info.health = hub.health.get(&info.id).cloned().unwrap_or_default();
                info.health.restart_attempts = p.restarts;
                if !p.info.enabled && p.restarts > 0 {
                    info.health.recent_failures.push(p.info.status.clone());
                }
                info
            })
            .collect();
        values
    }
    // Inspect compatibility before allowing plugin execution
    pub fn preflight(&self, id: &str) -> Result<preflight::PluginPreflight, String> {
        let plugin = self.plugins.get(id).ok_or("Plugin not installed")?;
        Ok(preflight::resolve_runtime(
            preflight::inspect_installed(&plugin.path, &plugin.info.id),
            &self.runtimes,
        ))
    }
    /// Copy an installed source for a read-only bounded setup check. Callers
    /// inspect outside the host mutex and recheck the installation token after.
    pub fn preflight_source(&self, id: &str) -> Result<std::path::PathBuf, String> {
        self.plugins
            .get(id)
            .map(|plugin| plugin.path.clone())
            .ok_or_else(|| "Plugin not installed".into())
    }

    // Collect usage of the installed plugin runtimes
    pub fn runtime_usage(&self) -> Vec<stream_dock_manager_plugin_runtimes::RuntimeUsage> {
        self.plugins
            .values()
            .filter(|p| p.info.kind == "nodejs")
            .filter_map(|p| {
                self.preflight(&p.info.id)
                    .ok()
                    .and_then(|r| r.runtime)
                    .map(|selected| {
                        let running = p.info.running_runtime.clone();
                        let pending_restart = p.child.is_some()
                            && running.as_ref().map(|r| &r.id)
                                != selected.selected.as_ref().map(|r| &r.id);
                        stream_dock_manager_plugin_runtimes::RuntimeUsage {
                            enabled_preference: self
                                .hub
                                .lock()
                                .unwrap()
                                .enabled
                                .contains(&p.info.id),
                            plugin_id: p.info.id.clone(),
                            name: p.info.name.clone(),
                            running,
                            selected,
                            pending_restart,
                        }
                    })
            })
            .collect()
    }
    /// The caller holds the host mutex across the usage check and filesystem change,
    /// preventing a plugin start/restart from racing removal.
    pub fn remove_private_runtime(&self, id: &str) -> Result<(), String> {
        if self
            .plugins
            .values()
            .any(|plugin| plugin.info.kind == "nodejs" && plugin.stopping.is_some())
        {
            return Err(
                "Wait for Node.js plugins to finish stopping before deleting a runtime".into(),
            );
        }
        let running = self
            .plugins
            .values()
            .filter(|plugin| plugin.child.is_some())
            .filter_map(|plugin| plugin.info.running_runtime.clone())
            .collect::<Vec<_>>();
        self.runtimes.remove_private(id, &running)
    }
    // Apply runtime changes
    pub fn apply_runtime_changes(&mut self, ids: &[String]) -> Result<(), String> {
        for id in ids {
            let state = self
                .plugins
                .get(id)
                .ok_or("Plugin not installed")?
                .info
                .runtime
                .state;
            if state == RuntimeState::Running {
                self.restart(id)?;
            } else if state == RuntimeState::Blocked && self.eligible(id) {
                self.retry(id)?;
            }
        }
        Ok(())
    }
    // Set sampler waker
    pub fn set_sampler_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.hub.lock().unwrap().sampler_wake = Some(wake);
    }
    // Set waker
    pub fn set_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.hub.lock().unwrap().wake = Some(wake);
    }
    // Set action address
    pub fn set_action_address(
        &mut self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
        address: Value,
    ) {
        if !self.plugins.get(id).is_some_and(|p| p.info.enabled) {
            return;
        }
        let mut hub = self.hub.lock().unwrap();
        let context = hub.binding_context(id, scope, device, control);
        hub.context_state.addresses.insert(context.clone(), address);
        if hub.context_state.scopes.get(&context).map(String::as_str) != Some(scope) {
            hub.visual_state.render_generation = hub.visual_state.render_generation.wrapping_add(1);
        }
        hub.context_state.scopes.insert(context, scope.into());
    }
    // Resolve the settings effective for this action
    pub fn effective_settings(
        &self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
        fallback: &Value,
    ) -> Value {
        let hub = self.hub.lock().unwrap();
        let context = hub
            .lookup_binding_context(id, scope, device, control)
            .unwrap_or_default();
        preserve_references(
            fallback,
            &hub.durable_settings(
                &context,
                hub.context_state.settings.get(&context).unwrap_or(fallback),
            ),
        )
    }

    // Find the next deadline requiring host attention
    pub fn next_deadline(&self) -> Option<std::time::Instant> {
        let now = std::time::Instant::now();
        let hub = self.hub.lock().unwrap();
        if !hub.effects.is_empty() {
            return Some(now);
        }
        let housekeeping = hub
            .manual_actions
            .next_deadline(now)
            .into_iter()
            .chain(hub.sessions.retired_contexts.next_deadline())
            .chain(
                (!hub.health_dirty.is_empty())
                    .then_some(self.last_health_flush + Duration::from_secs(2)),
            )
            .min();
        let badge_deadline = hub.visual_state.badges.values().copied().min();
        drop(hub);
        self.plugins
            .values()
            .filter_map(|p| match p.info.runtime.state {
                RuntimeState::Running if p.stopping.is_none() => match &p.exit_watch {
                    Some(watch) if watch.ready() => Some(now),
                    Some(watch) if watch.healthy() => None,
                    _ => Some(now + Duration::from_millis(100)),
                },
                RuntimeState::Starting | RuntimeState::Running | RuntimeState::Stopping => {
                    Some(now + Duration::from_millis(100))
                }
                RuntimeState::Retrying => Some(if p.stopping.is_some() {
                    now + Duration::from_millis(100)
                } else {
                    p.next_restart
                }),
                _ if p.stopping.is_some() => Some(now + Duration::from_millis(100)),
                _ => None,
            })
            .chain(badge_deadline)
            .chain(housekeeping)
            .chain((!self.startup.is_empty() && self.starting.len() < 2).then_some(self.next_start))
            .chain(
                self.starting
                    .values()
                    .map(|at| *at + Duration::from_secs(5)),
            )
            .min()
    }
    // Collect the telemetry topics requested by plugins
    pub fn telemetry_topics(&self) -> std::collections::BTreeSet<String> {
        let hub = self.hub.lock().unwrap();
        hub.telemetry_topics
            .iter()
            .filter(|(id, _)| {
                self.plugins
                    .get(*id)
                    .is_some_and(|p| p.info.enabled && hub.sessions.connected.contains(&p.token))
            })
            .flat_map(|(_, topics)| topics.iter().cloned())
            .collect()
    }
    // Publish telemetry
    pub fn publish_telemetry(&self, value: Value) {
        let mut hub = self.hub.lock().unwrap();
        let ids: Vec<_> = hub
            .telemetry_topics
            .iter()
            .filter(|(id, topics)| {
                !topics.is_empty()
                    && self.plugins.get(*id).is_some_and(|p| {
                        p.info.enabled && hub.sessions.connected.contains(&p.token)
                    })
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let queue = hub.sessions.queues.entry(id).or_default();
            queue.retain(|event| event["event"] != "didReceiveTelemetry");
            queue.push_back(json!({"event":"didReceiveTelemetry","payload":value}));
        }
    }
    // Resolve the managed plugin installation root
    pub fn installation_root(&self) -> PathBuf {
        self.root.clone()
    }
    // Prepare install
    pub fn prepare_install(root: &Path, archive: &Path) -> Result<PreparedPlugin, String> {
        let directory = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(root)
            .map_err(|e| e.to_string())?;
        let staging = directory.path().join("unpacked");
        std::fs::create_dir(&staging).map_err(|e| e.to_string())?;
        let origin = if archive.is_dir() {
            let before = packages::inventory(archive)?;
            let name = archive
                .file_name()
                .ok_or("Missing package directory name")?;
            let target = staging.join(name);
            preflight::copy_package(archive, &target)?;
            if packages::inventory(&target)? != before {
                return Err("Source folder changed during staging".into());
            }
            PackageSource {
                format: "folder".into(),
                filename: name.to_string_lossy().into(),
                sha256: hash_inventory(&before)?,
            }
        } else {
            let metadata = std::fs::symlink_metadata(archive).map_err(|e| e.to_string())?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > 256 * 1024 * 1024
            {
                return Err("Invalid or oversized plugin archive".into());
            }
            let copy = directory.path().join("source.zip");
            let size = std::io::copy(
                &mut std::fs::File::open(archive)
                    .map_err(|e| e.to_string())?
                    .take(256 * 1024 * 1024 + 1),
                &mut std::fs::File::create(&copy).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if size != metadata.len() {
                return Err("Source archive changed during staging".into());
            }
            let (_, sha256) =
                packages::hash_reader(std::fs::File::open(&copy).map_err(|e| e.to_string())?)?;
            extract_archive(&copy, &staging)?;
            PackageSource {
                format: "zip".into(),
                filename: archive
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                sha256,
            }
        };
        let source = if archive.is_dir() {
            staging.join(archive.file_name().ok_or("Missing package folder name")?)
        } else {
            packages::select_root(&staging)?
        };
        let report = preflight::inspect(&source);
        if !report.can_install {
            return Err(report
                .checks
                .iter()
                .filter(|c| c.level == "blocker")
                .map(|c| c.message.clone())
                .collect::<Vec<_>>()
                .join("; "));
        }
        let plugin = read_plugin(&source)?;
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(source.join("manifest.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let normalized = manifest::normalize(&source, &manifest)?;
        let inventory = packages::inventory(&source)?;
        Ok(PreparedPlugin {
            service_review: None,
            inventory,
            manifest: normalized,
            origin,
            provenance: None,
            directory,
            source,
            id: plugin.info.id,
        })
    }
    // Register prepared
    pub fn register_prepared(&mut self, prepared: PreparedPlugin) -> Result<PluginInfo, String> {
        if self.root.join(&prepared.id).exists() {
            return Err("plugin already installed; remove it before replacing".into());
        }
        self.replace_prepared_inner(prepared, false)
    }
    // Install for the current operation
    pub fn install(&mut self, archive: &Path) -> Result<PluginInfo, String> {
        let prepared = Self::prepare_install(&self.root, archive)?;
        self.register_prepared(prepared)
    }
    // Collect the labels of plugin-owned webviews
    pub fn view_labels(&self, id: &str) -> Result<Vec<String>, String> {
        self.plugins.get(id).ok_or("Plugin not installed")?;
        let hub = self.hub.lock().unwrap();
        Ok(hub
            .sessions
            .files
            .iter()
            .filter(|(token, _)| {
                hub.sessions
                    .tokens
                    .get(*token)
                    .is_some_and(|owner| owner == id)
            })
            .map(|(token, grant)| {
                format!(
                    "{}-{token}",
                    if grant.context.is_some() {
                        "pi"
                    } else {
                        "plugin"
                    }
                )
            })
            .collect())
    }
    // Resolve the path allowed for plugin deletion
    fn deletion_path(&self, id: &str) -> Result<PathBuf, String> {
        let path = self
            .plugins
            .get(id)
            .ok_or("Plugin not installed")?
            .path
            .clone();
        if self.plugins.get(id).is_some_and(|p| {
            p.child.is_some()
                || p.stopping
                    .as_ref()
                    .is_some_and(|done| !done.load(Ordering::Acquire))
        }) {
            return Err("Stop the plugin and await process cleanup before deletion".into());
        }
        // Only remove the installed directory owned by this host, never a linked source
        if path.parent() != Some(self.root.as_path())
            || std::fs::symlink_metadata(&path)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            || path.canonicalize().map_err(|e| e.to_string())? != path
        {
            return Err(
                "Plugin directory is outside the application plugin folder or is a symlink".into(),
            );
        }
        Ok(path)
    }
    // Delete for the current operation
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.delete_with_retention(id, false)
    }
    // Delete with retention
    pub fn delete_with_retention(&mut self, id: &str, keep: bool) -> Result<(), String> {
        self.delete_owned_package(id, keep, false, 0, |_| Ok(()))
    }
    /// Compose additional installation-owned preferences with the uninstall transaction
    /// The closure uses only its transaction; stage any external assets before calling
    pub fn delete_with_retention_sql(
        &mut self,
        id: &str,
        keep: bool,
        retained_bytes: usize,
        commit: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<(), String>
            + Send
            + 'static,
    ) -> Result<(), String> {
        self.delete_owned_package(id, keep, !keep, retained_bytes, commit)
    }
    // Check whether the plugin manifest declares this action
    pub fn declares_action(&self, id: &str, action: &str) -> bool {
        self.plugins
            .get(id)
            .is_some_and(|plugin| plugin.info.actions.iter().any(|entry| entry.id == action))
    }
    // Check start permission
    fn check_start_permission(&self, id: &str) -> Result<(), String> {
        if self.hub.lock().unwrap().retained.contains_key(id) {
            return Err("This plugin has retained data. Restore it or start fresh in Manage stored data before enabling the plugin.".into());
        }
        if transactions::pending(&self.root)? {
            return Err(
                "Package recovery is pending; restart Manager before enabling plugins".into(),
            );
        }
        Ok(())
    }
    // Enable for the current operation
    pub fn enable(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        if enabled {
            self.check_start_permission(id)?;
        }
        self.plugins.get(id).ok_or("plugin not installed")?;
        {
            let mut hub = self.hub.lock().unwrap();
            let previous = hub.enabled.contains(id);
            if enabled {
                hub.enabled.insert(id.into());
            } else {
                hub.enabled.remove(id);
            }
            if let Err(error) = hub.persist_installation(id) {
                if previous {
                    hub.enabled.insert(id.into());
                } else {
                    hub.enabled.remove(id);
                }
                return Err(error);
            }
            hub.notify_vendor_demand();
            if !enabled {
                hub.revoke_native_inspectors(id);
                hub.revoke_setup_runtimes(id);
                hub.revoke_manual_actions(id);
            }
        }
        if enabled {
            let mut hub = self.hub.lock().unwrap();
            hub.health_mut(id).blocked_reason = None;
        }
        let plugin = self.plugins.get_mut(id).unwrap();
        plugin.info.enabled = enabled;
        if enabled && plugin.info.runtime.state == RuntimeState::Blocked {
            plugin.info.runtime.state = RuntimeState::Idle;
            plugin.restarts = 0;
        }
        plugin.diagnostic(
            if enabled {
                manager_diagnostics::PluginEvent::Enabled
            } else {
                manager_diagnostics::PluginEvent::Disabled
            },
            None,
            None,
            None,
            self.recovery.status.active,
        );
        self.reconcile();
        Ok(())
    }
    // Set sleep when hidden
    pub fn set_sleep_when_hidden(&mut self, id: &str, sleep: bool) -> Result<(), String> {
        self.plugins.get(id).ok_or("plugin not installed")?;
        {
            let mut hub = self.hub.lock().unwrap();
            let previous = hub.sleep_when_hidden.get(id).copied();
            hub.sleep_when_hidden.insert(id.into(), sleep);
            if let Err(error) = hub.persist_installation(id) {
                if let Some(previous) = previous {
                    hub.sleep_when_hidden.insert(id.into(), previous);
                } else {
                    hub.sleep_when_hidden.remove(id);
                }
                return Err(error);
            }
        }
        self.reconcile();
        Ok(())
    }
    /// Publish both demands before any lifecycle decision during a page transition
    pub fn reconcile_visible_demand(
        &mut self,
        required: HashMap<String, Vec<PluginDemand>>,
        actions: &[ActiveAction],
    ) {
        self.visible_plugins = actions
            .iter()
            .map(|action| action.plugin_id.clone())
            .collect();
        self.reconcile_demand(required);
    }
    /// Profile demand is independent of temporary settings-panel leases
    pub fn reconcile_demand(&mut self, required: HashMap<String, Vec<PluginDemand>>) {
        self.required = required;
        self.reconcile();
    }
    // Check whether the candidate is eligible for this operation
    fn eligible(&self, id: &str) -> bool {
        if self.recovery.status.active || self.is_waiting_for_vault(id) {
            return false;
        }
        let (enabled, manual, sleep) = {
            let hub = self.hub.lock().unwrap();
            (
                hub.enabled.contains(id),
                hub.manual_actions.demanded(id)
                    || hub
                        .action_results
                        .values()
                        .any(|pending| pending.owner == id),
                hub.sleeps_when_hidden(id),
            )
        };
        !self.replacement_claims.lock().unwrap().contains(id)
            && ((enabled
                && (manual
                    || (self.required.get(id).is_some_and(|d| !d.is_empty())
                        && (!sleep || self.visible_plugins.contains(id)))))
                || self.configuration_demand().contains(id))
            && self.plugins.contains_key(id)
    }
    // Reconcile for the current operation
    fn reconcile(&mut self) {
        let configuring = self.configuration_demand();
        let mut ids: Vec<_> = self.plugins.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let (enabled, manual, sleep) = {
                let hub = self.hub.lock().unwrap();
                (
                    hub.enabled.contains(&id),
                    hub.manual_actions.demanded(&id)
                        || hub
                            .action_results
                            .values()
                            .any(|pending| pending.owner == id),
                    hub.sleeps_when_hidden(&id),
                )
            };
            let sleeping = enabled && sleep && !self.visible_plugins.contains(&id);
            let profile_demand =
                enabled && !sleeping && self.required.get(&id).is_some_and(|d| !d.is_empty());
            let waiting_for_vault = self.is_waiting_for_vault(&id);
            let demanded = !self.recovery.status.active
                && !waiting_for_vault
                && (profile_demand || (enabled && manual) || configuring.contains(&id))
                && !self.replacement_claims.lock().unwrap().contains(&id);
            let reason = if profile_demand {
                RuntimeReason::RequiredByActiveProfile
            } else if enabled && manual {
                RuntimeReason::ManualAction
            } else {
                RuntimeReason::Configuration
            };
            let plugin = self.plugins.get_mut(&id).unwrap();
            plugin.info.enabled = enabled;
            plugin.info.runtime.demand = self.required.get(&id).cloned().unwrap_or_default();
            if matches!(
                plugin.info.runtime.state,
                RuntimeState::Queued | RuntimeState::Running
            ) {
                plugin.info.runtime.reason = reason;
            }
            if !demanded {
                let blocked = enabled && plugin.info.runtime.state == RuntimeState::Blocked;
                let needs_stop = !plugin.token.is_empty() || plugin.child.is_some();
                if needs_stop {
                    let _ = self.launch(&id, false, false);
                }
                self.startup.retain(|queued| queued != &id);
                self.starting.remove(&id);
                let plugin = self.plugins.get_mut(&id).unwrap();
                if !blocked && plugin.stopping.is_none() {
                    plugin.info.runtime.state = if enabled {
                        RuntimeState::Idle
                    } else {
                        RuntimeState::Disabled
                    };
                    plugin.info.runtime.reason = if enabled && waiting_for_vault {
                        if self.vault_locked {
                            RuntimeReason::VaultLocked
                        } else {
                            RuntimeReason::VaultSetupRequired
                        }
                    } else if sleeping {
                        RuntimeReason::NoVisibleActions
                    } else if enabled {
                        RuntimeReason::NoActiveProfileDemand
                    } else {
                        RuntimeReason::Disabled
                    };
                    plugin.info.status = if enabled && waiting_for_vault {
                        if self.vault_locked {
                            "Paused — unlock Vault"
                        } else {
                            "Paused — review required Vault setup"
                        }
                    } else if sleeping {
                        "Sleeping — no visible actions"
                    } else if enabled {
                        "Idle — not used by an active device profile"
                    } else {
                        "Disabled"
                    }
                    .into();
                }
            } else if matches!(
                plugin.info.runtime.state,
                RuntimeState::Disabled | RuntimeState::Idle
            ) && plugin.stopping.is_none()
            {
                plugin.info.runtime.state = RuntimeState::Queued;
                plugin.info.runtime.reason = reason;
                plugin.info.status = "Queued for startup".into();
                if !self.startup.contains(&id) {
                    self.startup.push_back(id);
                }
            }
        }
        self.prioritize_startup();
    }
    // Choose the next plugins eligible for startup
    fn prioritize_startup(&mut self) {
        let configuring = self.configuration_demand();
        let hub = self.hub.lock().unwrap();
        // Visible actions, manual runs, and open settings go first while each group's
        // existing queue order, including restart order, remains intact.
        self.startup.make_contiguous().sort_by_key(|id| {
            !self.visible_plugins.contains(id)
                && !configuring.contains(id)
                && !hub.manual_actions.demanded(id)
        });
    }
    // Retry for the current operation
    pub fn retry(&mut self, id: &str) -> Result<(), String> {
        if !self.eligible(id) {
            return Err(
                "Retry requires an enabled plugin used by a profile or open settings".into(),
            );
        }
        let p = self.plugins.get_mut(id).ok_or("Plugin not installed")?;
        if p.info.runtime.state != RuntimeState::Blocked {
            return Err("Plugin is not blocked".into());
        }
        p.restarts = 0;
        self.hub.lock().unwrap().health_mut(id).blocked_reason = None;
        p.info.runtime.state = RuntimeState::Idle;
        self.reconcile();
        Ok(())
    }
    // Stop for replacement
    pub fn stop_for_replacement(&mut self, id: &str) -> Result<Option<Arc<AtomicBool>>, String> {
        self.plugins.get(id).ok_or("Plugin not installed")?;
        {
            let mut hub = self.hub.lock().unwrap();
            hub.revoke_setup_runtimes(id);
            hub.revoke_manual_actions(id);
        }
        self.launch(id, false, false)?;
        Ok(self.plugins[id].stopping.clone())
    }
    // Restart the selected plugin lifecycle
    pub fn restart(&mut self, id: &str) -> Result<(), String> {
        if !self.eligible(id) || self.plugins[id].info.runtime.state != RuntimeState::Running {
            return Err(
                "Restart requires a running plugin used by a profile or open settings".into(),
            );
        }
        self.launch(id, false, true)?;
        self.reconcile();
        Ok(())
    }
    // Record and report a plugin startup failure
    pub fn startup_failure(&mut self, id: &str, error: &str) {
        let missing_runtime = self
            .preflight(id)
            .ok()
            .and_then(|r| r.runtime)
            .is_some_and(|r| r.state != "ready");
        if let Some(plugin) = self.plugins.get(id) {
            plugin.diagnostic(
                manager_diagnostics::PluginEvent::StartupFailed,
                Some(if missing_runtime {
                    manager_diagnostics::FailureReason::MissingRuntime
                } else {
                    manager_diagnostics::classify(error)
                }),
                None,
                None,
                self.recovery.status.active,
            );
        }
        let _ = self.launch(id, false, false);
        if let Some(plugin) = self.plugins.get_mut(id) {
            plugin.info.runtime.state = RuntimeState::Blocked;
            plugin.info.runtime.reason = if missing_runtime {
                RuntimeReason::MissingRuntime
            } else {
                RuntimeReason::Compatibility
            };
            plugin.info.status = format!("Blocked: {error}");
        }
        let mut hub = self.hub.lock().unwrap();
        hub.issue(id, "failed_start", error);
        hub.health_mut(id).blocked_reason = Some(format!("Blocked: {error}"));
    }
    // Launch for the current operation
    fn launch(&mut self, id: &str, start: bool, reset_budget: bool) -> Result<(), String> {
        if start {
            self.recovery.check()?;
        }
        if let Some(wake) = &self.hub.lock().unwrap().sampler_wake {
            wake();
        }
        self.startup.retain(|queued| queued != id);
        if !start {
            self.starting.remove(id);
        }
        if start && !self.eligible(id) {
            return Err("Plugin has no active runtime demand".into());
        }
        if start && !self.hub.lock().unwrap().enabled.contains(id) {
            self.check_start_permission(id)?;
        }
        let report = if start {
            Some(self.preflight(id)?)
        } else {
            None
        };
        if report.as_ref().is_some_and(|r| !r.can_enable) {
            return Err(report
                .as_ref()
                .unwrap()
                .checks
                .iter()
                .filter(|c| c.level == "blocker")
                .map(|c| format!("{}: {}", c.message, c.remedy))
                .collect::<Vec<_>>()
                .join("; "));
        }
        let permission_session = start.then(|| self.permission_session(id)).flatten();
        let declares_plugin_notifications = permission_session
            .as_ref()
            .is_some_and(permission_rpc::Session::declares_plugin_notifications);
        let vault_managed = start && self.vault_managed(id);
        let vault_scope = if vault_managed {
            self.vault_launch_scope(id)
        } else {
            None
        };
        let plugin = self.plugins.get_mut(id).ok_or("plugin not installed")?;
        {
            let mut h = self.hub.lock().unwrap();
            h.metadata.insert(id.into(), plugin.metadata.clone());
            h.manifest_dialects
                .insert(id.into(), plugin.info.dialect.clone());
            if let Some(c) = &plugin.info.capabilities {
                h.capabilities.insert(id.into(), c.clone());
            } else {
                h.capabilities.remove(id);
            }
        }
        if reset_budget {
            plugin.restarts = 0;
        }
        plugin.exit_watch = None;
        if let Some(child) = plugin.child.take() {
            plugin.stopping = Some(process_tree::stop(child, plugin.tree.take(), true));
        }
        {
            let mut hub = self.hub.lock().unwrap();
            hub.revoke_plugin_sessions(id, &plugin.token);
        }
        plugin.info.running_runtime = None;
        let had_run = plugin.started.is_some();
        plugin.token.clear();
        plugin.started = None;
        plugin.registered = None;
        let peak = plugin.info.runtime.resources.peak_resident_bytes;
        if had_run {
            let mut hub = self.hub.lock().unwrap();
            hub.health_mut(id).last_run_peak_resident_bytes = (peak > 0).then_some(peak);
        }
        plugin.resource_history = Default::default();
        plugin.resource_sampled_at = None;
        plugin.info.runtime.resources = Default::default();
        plugin.info.runtime.network = Default::default();
        plugin.info.runtime.uptime_ms = None;
        if !start {
            plugin.info.runtime.state = if plugin.stopping.is_some() {
                RuntimeState::Stopping
            } else {
                RuntimeState::Idle
            };
            plugin.info.status = if plugin.stopping.is_some() {
                "Stopping"
            } else {
                "Idle"
            }
            .into();
            return self.recovery.finish(id);
        }
        let report = report.unwrap();
        let mut connection_info = browser::registration_info(
            &self.hub.lock().unwrap(),
            id,
            &plugin.info.version,
            &plugin.info.dialect,
            false,
            plugin.info.kind == "browser",
        )?;
        self.recovery.begin(id, &plugin.info.name)?;
        // Observation only: do not create demand or persist usage history
        plugin.info.runtime.last_wake_reason = Some(plugin.info.runtime.reason);
        plugin.info.runtime.last_wake_at_ms = Some(monitor::timestamp_ms());
        plugin.info.runtime.state = RuntimeState::Starting;
        plugin.info.runtime.reason = RuntimeReason::Registration;
        plugin.info.runtime.started_at_ms = plugin.info.runtime.last_wake_at_ms;
        plugin.started = Some(std::time::Instant::now());
        let run_id = uuid::Uuid::new_v4().to_string();
        plugin.info.runtime.run_id = Some(run_id.clone());
        plugin.diagnostic(
            manager_diagnostics::PluginEvent::Starting,
            None,
            None,
            None,
            false,
        );
        self.hub.lock().unwrap().run_details.insert(
            id.into(),
            (plugin.info.version.clone(), Some(run_id.clone())),
        );
        connection_info["streamDockManager"] =
            connection_contract::manager_registration(&run_id, self.logs.debug_network(id));
        if permission_session.is_none() {
            if let Some(features) =
                connection_info["streamDockManager"]["capabilities"].as_array_mut()
            {
                features.retain(|v| {
                    !matches!(
                        v.as_str(),
                        Some(
                            permission_rpc::PROTOCOL
                                | permission_rpc::BROKER
                                | vault_references::PROTOCOL
                        )
                    )
                });
            }
        }
        let plugin_notifications = self
            .hub
            .lock()
            .unwrap()
            .registration_environment
            .allows_plugin_notifications(
                plugin.info.kind == "browser",
                declares_plugin_notifications,
            );
        if !plugin_notifications {
            if let Some(features) =
                connection_info["streamDockManager"]["capabilities"].as_array_mut()
            {
                features.retain(|v| v != permission_rpc::NOTIFICATIONS);
            }
        }
        if permission_session
            .as_ref()
            .is_none_or(|s| !s.has_reference_declaration())
        {
            if let Some(features) =
                connection_info["streamDockManager"]["capabilities"].as_array_mut()
            {
                features.retain(|v| v != vault_references::PROTOCOL);
            }
        }
        if vault_scope.as_ref().is_none_or(|s| s.writers.is_empty()) {
            if let Some(features) =
                connection_info["streamDockManager"]["capabilities"].as_array_mut()
            {
                features.retain(|v| v != vault_writes::PROTOCOL);
            }
        }
        if vault_scope
            .as_ref()
            .is_none_or(|scope| !scope.slots.iter().any(vault_text::eligible_slot))
            || self.hub.lock().unwrap().vault_reader.is_none()
        {
            if let Some(features) =
                connection_info["streamDockManager"]["capabilities"].as_array_mut()
            {
                features.retain(|v| v != vault_text::PROTOCOL);
            }
        }
        Self::filter_vault_features(
            &mut connection_info["streamDockManager"],
            vault_managed
                && plugin.info.kind != "browser"
                && vault_scope.as_ref().is_some_and(|s| !s.slots.is_empty()),
        );
        if plugin.info.kind == "browser" {
            let token = uuid::Uuid::new_v4().to_string();
            plugin.token = token.clone();
            let bootstrap = bootstrap(
                self.port,
                &token,
                &connection_info,
                browser::BootstrapRole::Runtime,
            );
            let mut hub = self.hub.lock().unwrap();
            hub.sessions.tokens.insert(token.clone(), id.into());
            hub.sessions.files.insert(
                token,
                FileGrant {
                    root: plugin.path.clone(),
                    runtime_entry: Some(plugin.entry.clone()),
                    bootstrap,
                    context: None,
                    setup_runtime: false,
                },
            );
            plugin.info.status = "Starting — waiting for registration".into();
            return Ok(());
        }
        let mut command = if let Some(resolution) = &report.runtime {
            let selected = resolution.selected.as_ref().ok_or("Runtime unavailable")?;
            stream_dock_manager_plugin_runtimes::launch_command(
                selected,
                &resolution.requirement,
                &plugin.path,
                &plugin.entry,
                self.port,
            )?
        } else {
            Command::new(&plugin.entry)
        };
        let token = uuid::Uuid::new_v4().to_string();
        plugin.token = token.clone();
        {
            let mut hub = self.hub.lock().unwrap();
            hub.sessions.tokens.insert(token.clone(), id.into());
            if let Some(s) = permission_session {
                hub.permission_sessions.insert(token.clone(), s);
            }
            if let Some(scope) = vault_scope {
                hub.vault_sessions.insert(token.clone(), scope);
            }
            hub.refresh_log_secrets();
        }
        // The SDK compatibility level is separate from our application release version
        let info = connection_info.to_string();
        process_tree::configure(&mut command);
        let mut child = command
            .current_dir(&plugin.path)
            .args([
                "-port",
                &self.port.to_string(),
                "-pluginUUID",
                &token,
                "-registerEvent",
                "registerPlugin",
                "-info",
                &info,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        plugin.tree = match process_tree::ProcessTree::attach(&child) {
            Ok(tree) => Some(tree),
            Err(error) => {
                plugin.stopping = Some(process_tree::stop(child, None, false));
                return Err(error);
            }
        };
        plugin.info.running_runtime = report.runtime.as_ref().and_then(|r| r.selected.clone());
        self.logs
            .record(id, &run_id, "stdout", "manager", "Plugin process started");
        for (stream, pipe) in [
            (
                "stdout",
                child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
            ),
            (
                "stderr",
                child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
            ),
        ] {
            if let Some(pipe) = pipe {
                let logs = self.logs.clone();
                let id = id.to_string();
                let run = run_id.clone();
                std::thread::spawn(move || logs.drain_pipe(&id, &run, stream, pipe));
            }
        }
        plugin.exit_watch = self
            .exit_registry
            .as_ref()
            .and_then(|registry| registry.register(&child));
        plugin.child = Some(child);
        plugin.next_restart = std::time::Instant::now();
        plugin.info.status = "Starting — waiting for registration".into();
        Ok(())
    }
    // Keep the complete SDK action address explicit at the runtime boundary
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &mut self,
        id: &str,
        action_id: &str,
        device: &str,
        control_id: &str,
        scope: &str,
        input: &hardware_core::Input,
        settings: &Value,
    ) -> Result<(), String> {
        self.dispatch_with_state(
            id, action_id, device, control_id, scope, input, settings, None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    // Dispatch with state
    pub fn dispatch_with_state(
        &mut self,
        id: &str,
        action_id: &str,
        device: &str,
        control_id: &str,
        scope: &str,
        input: &hardware_core::Input,
        settings: &Value,
        user_desired_state: Option<u8>,
    ) -> Result<(), String> {
        self.dispatch_with_activation(
            id,
            action_id,
            device,
            control_id,
            scope,
            input,
            settings,
            user_desired_state,
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_with_activation(
        &mut self,
        id: &str,
        action_id: &str,
        device: &str,
        control_id: &str,
        scope: &str,
        input: &hardware_core::Input,
        settings: &Value,
        user_desired_state: Option<u8>,
        activation: Option<&str>,
    ) -> Result<(), String> {
        self.dispatch_inner(
            id,
            action_id,
            device,
            control_id,
            scope,
            input,
            settings,
            None,
            user_desired_state,
            activation,
        )
    }
    #[allow(clippy::too_many_arguments)]
    // Dispatch tracked
    pub fn dispatch_tracked(
        &mut self,
        id: &str,
        action_id: &str,
        device: &str,
        control_id: &str,
        scope: &str,
        input: &hardware_core::Input,
        settings: &Value,
    ) -> Result<ActionReceipt, String> {
        self.dispatch_tracked_with_state(
            id, action_id, device, control_id, scope, input, settings, None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    // Dispatch tracked with state
    pub fn dispatch_tracked_with_state(
        &mut self,
        id: &str,
        action_id: &str,
        device: &str,
        control_id: &str,
        scope: &str,
        input: &hardware_core::Input,
        settings: &Value,
        user_desired_state: Option<u8>,
    ) -> Result<ActionReceipt, String> {
        self.dispatch_tracked_with_activation(
            id,
            action_id,
            device,
            control_id,
            scope,
            input,
            settings,
            user_desired_state,
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_tracked_with_activation(
        &mut self,
        id: &str,
        action_id: &str,
        device: &str,
        control_id: &str,
        scope: &str,
        input: &hardware_core::Input,
        settings: &Value,
        user_desired_state: Option<u8>,
        activation: Option<&str>,
    ) -> Result<ActionReceipt, String> {
        self.check_vault_runtime(id)?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let session = self
            .plugins
            .get(id)
            .ok_or("plugin not installed")?
            .token
            .clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let context = self
            .hub
            .lock()
            .unwrap()
            .binding_context(id, scope, device, control_id);
        self.hub.lock().unwrap().action_results.insert(
            request_id.clone(),
            PendingAction {
                context,
                owner: id.into(),
                session,
                sender,
            },
        );
        let manager_receipts = self
            .plugins
            .get(id)
            .is_some_and(|p| p.info.capabilities.is_some());
        let receipt = ActionReceipt {
            completion_supported: manager_receipts,
            id: request_id.clone(),
            receiver,
            hub: self.hub.clone(),
        };
        self.dispatch_inner(
            id,
            action_id,
            device,
            control_id,
            scope,
            input,
            settings,
            Some(&request_id),
            user_desired_state,
            activation,
        )?;
        if !manager_receipts {
            if let Some(pending) = self.hub.lock().unwrap().action_results.remove(&request_id) {
                // Vendor protocols acknowledge dispatch, not action completion
                let _ = pending.sender.send(Ok(()));
            }
        }
        Ok(receipt)
    }
    #[allow(clippy::too_many_arguments)]
    // Dispatch inner
    fn dispatch_inner(
        &mut self,
        id: &str,
        action_id: &str,
        device: &str,
        control_id: &str,
        scope: &str,
        input: &hardware_core::Input,
        settings: &Value,
        request_id: Option<&str>,
        user_desired_state: Option<u8>,
        activation: Option<&str>,
    ) -> Result<(), String> {
        self.check_vault_runtime(id)?;
        self.check_action_permissions(id, action_id)?;
        let plugin = self.plugins.get_mut(id).ok_or("plugin not installed")?;
        if !plugin.info.enabled
            || !matches!(
                plugin.info.runtime.state,
                RuntimeState::Starting | RuntimeState::Running
            )
        {
            return Err("Plugin must be enabled and required by an active device profile".into());
        }
        let action = plugin
            .info
            .actions
            .iter()
            .find(|action| action.id == action_id)
            .ok_or("action is not owned by this plugin")?;
        let vendor = plugin.info.capabilities.is_none();
        let mut hub = self.hub.lock().unwrap();
        let context = hub.binding_context(id, scope, device, control_id);
        let coordinates = hub
            .context_state
            .visible
            .get(&context)
            .map(|event| event["payload"]["coordinates"].clone())
            .filter(|value| value.is_object())
            .unwrap_or(json!({"column":0,"row":0}));
        let state = hub
            .visual_state
            .visuals
            .get(&context)
            .and_then(|v| v["state"].as_u64())
            .unwrap_or(0);
        let fresh = !hub.context_state.contexts.contains_key(&context);
        let durable_settings = browser::vsd_action_settings(plugin, action_id, settings);
        let settings = hub.execution_settings(&context, durable_settings.as_ref());
        let settings = browser::vsd_action_settings(plugin, action_id, &settings);
        let encoder = hub
            .context_state
            .visible
            .get(&context)
            .map(|v| v["payload"]["controller"] == "Encoder")
            .unwrap_or_else(|| {
                hub.context_state.transient_scopes.contains_key(&context)
                    && !action.control_types.iter().any(|kind| kind == "button")
                    && action.control_types.iter().any(|kind| kind == "encoder")
            });
        let elgato_multi =
            vendor && plugin.info.dialect == "streamdeck" && scope.contains('@') && !encoder;
        let declared_states = plugin
            .metadata
            .get(action_id)
            .and_then(|metadata| metadata["States"].as_array())
            .map_or(0, Vec::len);
        if let Some(desired) = user_desired_state {
            if !elgato_multi
                || !action.supported_in_multi_actions
                || declared_states != 2
                || desired > 1
            {
                return Err(
                    "Desired state requires a supported two-state Elgato keypad multi-action"
                        .into(),
                );
            }
        } else if elgato_multi && declared_states == 2 {
            return Err("Choose a desired state for this two-state Elgato multi-action".into());
        }
        let mut surface = hub
            .context_state
            .visible
            .get(&context)
            .and_then(|v| v.get("streamDockManager"))
            .cloned()
            .unwrap_or_else(|| connection_contract::default_surface(control_id));
        if let hardware_core::Input::Touch { phase, x, y } = input {
            surface["touch"] = json!({"phase":phase,"x":x,"y":y});
        }
        if let Some(id) = request_id {
            surface["request_id"] = json!(id);
        }
        if let Some(token) = activation.filter(|t| {
            hub.workflow.deliverable(
                t,
                hub.sessions.storage_sessions.get(&plugin.token),
                &context,
                action_id,
            )
        }) {
            surface["activation_token"] = json!(token);
        }
        let resources = hub
            .context_state
            .resources
            .get(&context)
            .cloned()
            .unwrap_or(json!({}));
        // The upstream Keys guide specifies that the press payload contains the
        // newly toggled state. Only two-state keypad actions use this behavior;
        // macro instances have a separate userDesiredState contract.
        let automatic_state = (vendor
            && matches!(plugin.info.dialect.as_str(), "streamdeck" | "openaction")
            && !encoder
            && (plugin.info.dialect == "openaction" || !scope.contains('@'))
            && (matches!(input, hardware_core::Input::Pulse)
                || matches!(input, hardware_core::Input::Button { pressed }
                    if *pressed == (plugin.info.dialect == "streamdeck")))
            && plugin.metadata.get(action_id).is_some_and(|metadata| {
                metadata["DisableAutomaticStates"] != true
                    && metadata["States"]
                        .as_array()
                        .is_some_and(|states| states.len() == 2)
            }))
        .then_some((state + 1) % 2);
        let initial_state = state;
        let state = if plugin.info.dialect == "openaction" && scope.contains('@') {
            initial_state
        } else {
            automatic_state.unwrap_or(state)
        };
        let pressed = hub.context_state.pressed_encoders.contains(&context);
        let queue = hub.sessions.queues.entry(id.into()).or_default();
        let mut messages = Vec::with_capacity(3);
        if fresh {
            let appearance = json!({"event":"willAppear","action":action_id,"context":context,"device":device,"payload":{"settings":settings.as_ref(),"state":initial_state,"isInMultiAction":scope.contains('@'),"controller":if encoder {"Encoder"} else {"Keypad"},"coordinates":coordinates}});
            let title = browser::vsd_initial_title(plugin, &appearance);
            messages.push(appearance);
            messages.extend(title);
        }
        // Touch input on an encoder context retains tap metadata on its dial pulse
        let encoder_tap = !vendor && encoder && matches!(input, hardware_core::Input::Touch { .. });
        let event = match input {
            hardware_core::Input::Rotate { .. } => "dialRotate",
            hardware_core::Input::Touch { .. } if encoder_tap => "dialDown",
            hardware_core::Input::Touch { .. } => "touchTap",
            hardware_core::Input::Button { pressed: false } if encoder => "dialUp",
            hardware_core::Input::Button { pressed: true } if encoder => "dialDown",
            hardware_core::Input::Button { pressed: false } => "keyUp",
            hardware_core::Input::Pulse if encoder => "dialDown",
            _ => "keyDown",
        };
        let mut payload = json!({"settings":settings.as_ref(),"state":state,"isInMultiAction":scope.contains('@'),
                "controller":if encoder {"Encoder"} else {"Keypad"},"coordinates":coordinates});
        if let Some(desired) = user_desired_state {
            payload["userDesiredState"] = json!(desired);
        }
        if let hardware_core::Input::Rotate { delta } = input {
            payload["ticks"] = json!(delta);
            payload["pressed"] = json!(pressed);
        }
        if let (false, hardware_core::Input::Touch { phase, x, y }) = (encoder_tap, input) {
            payload["tapPos"] = json!([x, y]);
            payload["hold"] = json!(phase == "hold");
        }
        let mut press_payload = payload.clone();
        if plugin.info.dialect == "openaction" && matches!(input, hardware_core::Input::Pulse) {
            press_payload["state"] = json!(initial_state);
        }
        messages.push(json!({"event":event,"action":action_id,"context":context,"device":device,"payload":press_payload,"streamDockManager":surface}));
        if encoder_tap || matches!(input, hardware_core::Input::Pulse) {
            messages.push(json!({"event":if encoder{"dialUp"}else{"keyUp"},"action":action_id,"context":context,"device":device,"payload":payload}));
        }
        for message in &mut messages {
            if elgato_multi {
                message["payload"]
                    .as_object_mut()
                    .unwrap()
                    .remove("coordinates");
            }
            message["payload"]["resources"] = resources.clone();
        }
        let result = queue.push_batch(messages);
        if let Err(error) = result {
            if matches!(event, "keyUp" | "dialUp" | "touchTap") {
                // A rejected release must retire the session and its held
                // inputs, rather than leave a previously accepted press active.
                queue.fail(error);
            }
        }
        let result = result.map_err(str::to_owned);
        if result.is_ok() {
            if encoder {
                match input {
                    hardware_core::Input::Button { pressed: true } => {
                        hub.context_state.pressed_encoders.insert(context.clone());
                    }
                    hardware_core::Input::Button { pressed: false } => {
                        hub.context_state.pressed_encoders.remove(&context);
                    }
                    _ => {}
                }
            }
            if fresh || hub.context_state.scopes.get(&context).map(String::as_str) != Some(scope) {
                hub.visual_state.render_generation =
                    hub.visual_state.render_generation.wrapping_add(1);
            }
            hub.context_state
                .scopes
                .insert(context.clone(), scope.into());
            hub.context_state
                .contexts
                .insert(context.clone(), id.into());
            hub.context_state
                .context_actions
                .insert(context.clone(), action_id.into());
            hub.context_state
                .settings
                .entry(context.clone())
                .or_insert_with(|| durable_settings.into_owned());
            hub.context_state
                .targets
                .insert(context.clone(), (device.into(), control_id.into()));
            // Rejected dispatch batches must not change the displayed state
            if let Some(state) = automatic_state {
                let mut message =
                    json!({"event":"setState","context":context,"payload":{"state":state}});
                hub.visual_state.render_generation =
                    hub.visual_state.render_generation.wrapping_add(1);
                if let Err(error) = visual::prepare(&mut hub, id, &mut message) {
                    message["event"] = json!("unsupportedFeature");
                    message["payload"] = json!({"message":error});
                }
                hub.visual_state.record_update(&context, &message);
                if hub.effects.len() < 512 {
                    let effect = hub.message_effect(id, None, &message, None);
                    hub.effects.push_back(effect);
                    if let Some(wake) = &hub.wake {
                        wake();
                    }
                }
            }
        }
        result
    }
    /// Mark an owned transient action context before resolving or dispatching it
    pub fn begin_temporary_context(
        &mut self,
        id: &str,
        scope: &str,
        device: &str,
        control: &str,
    ) -> Result<(), String> {
        let mut hub = self.hub.lock().unwrap();
        let context = hub.binding_context(id, scope, device, control);
        if !hub.context_state.transient_scopes.contains_key(&context)
            && hub.context_state.transient_scopes.len() >= 256
        {
            return Err("Temporary plugin context limit reached".into());
        }
        hub.context_state
            .transient_scopes
            .insert(context, scope.into());
        Ok(())
    }
    /// Retire ephemeral Manager debug scopes, including expanded child contexts
    /// The caller supplies the unique `:debug:<UUID>` suffix it owns
    pub fn clear_action_scope_suffix(&mut self, suffix: &str) -> Vec<HostEffect> {
        if !suffix
            .strip_prefix(":debug:")
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        {
            return vec![];
        }
        let mut hub = self.hub.lock().unwrap();
        let contexts: Vec<_> = hub
            .context_state
            .transient_scopes
            .iter()
            .filter(|(_, scope)| {
                scope
                    .split('@')
                    .next()
                    .is_some_and(|scope| scope.ends_with(suffix))
            })
            .map(|(context, _)| context.clone())
            .collect();
        contexts
            .iter()
            .filter_map(|context| hub.retire_temporary_action_context(context))
            .collect()
    }
    // Synchronize for the current operation
    pub fn synchronize(&mut self, actions: Vec<ActiveAction>, devices: Vec<Value>) {
        self.visible_plugins = actions
            .iter()
            .filter(|action| {
                self.plugins.get(&action.plugin_id).is_some_and(|plugin| {
                    plugin.info.actions.iter().any(|a| a.id == action.action_id)
                })
            })
            .map(|action| action.plugin_id.clone())
            .collect();
        self.prioritize_startup();
        if devices.is_empty() {
            for plugin in self.plugins.values_mut().filter(|p| {
                p.info.runtime.state == RuntimeState::Idle
                    && p.info.enabled
                    && !matches!(
                        p.info.runtime.reason,
                        RuntimeReason::NoVisibleActions
                            | RuntimeReason::VaultLocked
                            | RuntimeReason::VaultSetupRequired
                    )
            }) {
                plugin.info.runtime.reason = RuntimeReason::NoActiveDevice;
                plugin.info.status =
                    "Idle — not used by an active device profile (no active device)".into();
            }
        }

        let mut hub = self.hub.lock().unwrap();
        let enabled: Vec<_> = self
            .plugins
            .values()
            .filter(|p| p.info.enabled)
            .map(|p| p.info.id.clone())
            .collect();
        let next_devices: HashMap<String, Value> = devices
            .into_iter()
            .filter_map(|d| d["id"].as_str().map(|id| (id.to_string(), d.clone())))
            .collect();
        let mut notices = vec![];
        for (id, info) in &next_devices {
            if !hub.devices.contains_key(id) {
                notices.push(json!({"event":"deviceDidConnect","device":id,"deviceInfo":info}));
            } else if hub.devices.get(id) != Some(info) {
                notices.push(json!({"event":"deviceDidChange","device":id,"deviceInfo":info}));
            }
        }
        for id in hub.devices.keys() {
            if !next_devices.contains_key(id) {
                notices.push(json!({"event":"deviceDidDisconnect","device":id}));
            }
        }
        for owner in &enabled {
            let queue = hub.sessions.queues.entry(owner.clone()).or_default();
            for notice in &notices {
                queue.push_back(notice.clone());
            }
        }
        hub.devices = next_devices;
        let mut desired = std::collections::HashSet::new();
        hub.context_state.pending_appearances.clear();
        for action in actions {
            let Some(plugin) = self.plugins.get(&action.plugin_id) else {
                continue;
            };
            if plugin.info.enabled
                && plugin.info.runtime.state == RuntimeState::Queued
                && plugin.info.actions.iter().any(|a| a.id == action.action_id)
            {
                hub.context_state.pending_appearances.push((
                    action.plugin_id.clone(),
                    action.device_id.clone(),
                    action.control_id.clone(),
                    action.scope.clone(),
                ));
            }
            if !plugin.info.enabled
                || !matches!(
                    plugin.info.runtime.state,
                    RuntimeState::Starting | RuntimeState::Running
                )
                || !plugin.info.actions.iter().any(|a| a.id == action.action_id)
            {
                continue;
            }
            let context = hub.binding_context(
                &action.plugin_id,
                &action.scope,
                &action.device_id,
                &action.control_id,
            );
            let context = if hub
                .context_state
                .visible
                .get(&context)
                .is_some_and(|v| v["action"] != action.action_id)
            {
                hub.renew_binding_context(
                    &action.plugin_id,
                    &action.scope,
                    &action.device_id,
                    &action.control_id,
                )
            } else {
                context
            };
            desired.insert(context.clone());
            hub.context_state
                .context_actions
                .insert(context.clone(), action.action_id.clone());
            if hub.context_state.scopes.get(&context) != Some(&action.scope) {
                hub.visual_state.render_generation =
                    hub.visual_state.render_generation.wrapping_add(1);
            }
            hub.context_state
                .scopes
                .insert(context.clone(), action.scope.clone());
            let changed = hub
                .context_state
                .configured
                .get(&context)
                .is_some_and(|old| old != &action.settings);
            hub.context_state
                .configured
                .insert(context.clone(), action.settings.clone());
            if hub
                .context_state
                .visible
                .get(&context)
                .is_some_and(|v| v["action"] == action.action_id)
            {
                if changed {
                    hub.workflow.invalidate_context(&context);
                    let settings =
                        browser::vsd_action_settings(plugin, &action.action_id, &action.settings);
                    hub.context_state
                        .settings
                        .insert(context.clone(), settings.into_owned());
                    let execution = hub.execution_settings(&context, &action.settings);
                    if let Some(visible) = hub.context_state.visible.get_mut(&context) {
                        visible["payload"]["settings"] = execution.clone();
                    }
                    hub.visual_state.render_generation =
                        hub.visual_state.render_generation.wrapping_add(1);
                    let event = hub.settings_event(&context, &execution);
                    hub.sessions
                        .queues
                        .entry(action.plugin_id.clone())
                        .or_default()
                        .push_back(event);
                    let _ = hub.persist();
                }
                continue;
            }
            if let Some(mut previous) = hub.context_state.visible.remove(&context) {
                hub.effects
                    .push_back(HostEffect::release_context(&action.plugin_id, &context));
                previous["event"] = json!("willDisappear");
                hub.sessions
                    .queues
                    .entry(action.plugin_id.clone())
                    .or_default()
                    .push_back(previous);
                hub.context_state
                    .settings
                    .insert(context.clone(), action.settings.clone());
                hub.visual_state.render_generation =
                    hub.visual_state.render_generation.wrapping_add(1);
                hub.visual_state.visuals.remove(&context);
                hub.visual_state.visual_sources.remove(&context);
            }
            // A newly visible binding's saved fields are authoritative. The
            // plugin cache can predate a profile save or come from a different
            // session; retain only its additional plugin-owned fields here.
            let settings = hub
                .context_state
                .settings
                .entry(context.clone())
                .or_insert_with(|| action.settings.clone());
            if let (Some(cached), Some(saved)) =
                (settings.as_object_mut(), action.settings.as_object())
            {
                cached.extend(saved.clone());
            } else {
                *settings = action.settings;
            }
            if let std::borrow::Cow::Owned(merged) =
                browser::vsd_action_settings(plugin, &action.action_id, settings)
            {
                *settings = merged;
            }
            let settings = settings.clone();
            let settings = hub.execution_settings(&context, &settings);
            let initial_state = vendor_profile_appearance::initial_state(&hub, &context);
            let mut event = json!({"event":"willAppear","streamDockManager":action.surface,"context":context,"action":action.action_id,"device":action.device_id,"payload":{"settings":settings,"state":initial_state,"isInMultiAction":action.scope.contains('@'),"controller":if action.encoder{"Encoder"}else{"Keypad"},"coordinates":{"column":action.column,"row":action.row}}});
            event["payload"]["resources"] = hub
                .context_state
                .resources
                .get(&context)
                .cloned()
                .unwrap_or(json!({}));
            hub.context_state
                .contexts
                .insert(context.clone(), action.plugin_id.clone());
            hub.context_state
                .targets
                .insert(context.clone(), (action.device_id, action.control_id));
            hub.context_state
                .visible
                .insert(context.clone(), event.clone());
            let title = if hub
                .context_state
                .vendor_profile_appearances
                .contains_key(&context)
            {
                None // Existing imported title is emitted by visual::prepare(setState).
            } else {
                browser::vsd_initial_title(plugin, &event)
            };
            let queue = hub
                .sessions
                .queues
                .entry(action.plugin_id.clone())
                .or_default();
            queue.push_back(event);
            if let Some(title) = title {
                queue.push_back(title);
            }
            if let Some(metadata) = plugin.metadata.get(&action.action_id) {
                let mut messages = vec![];
                if metadata["States"]
                    .as_array()
                    .is_some_and(|states| !states.is_empty())
                {
                    messages
                        .push(json!({"event":"setState","context":context,"payload":{"state":initial_state}}));
                }
                // A declared encoder layout has no feedback values yet. On a
                // keypad it would replace the initial state image with an empty
                // display until the plugin sends its first visual update.
                if action.encoder {
                    if plugin.info.capabilities.is_none() && plugin.info.dialect == "streamdeck" {
                        match visual::encoder_defaults(&mut hub, &action.plugin_id, &context, metadata) {
                            Ok(defaults) => messages.extend(defaults),
                            Err(error) => messages.push(json!({"event":"unsupportedFeature","context":context,"payload":{"message":error}})),
                        }
                    }
                    if let Some(layout) = plugin
                        .info
                        .capabilities
                        .as_ref()
                        .and_then(|c| c.actions.get(&action.action_id))
                        .and_then(|a| a.feedback_layout.as_deref())
                    {
                        messages.push(json!({"event":"setFeedbackLayout","context":context,"payload":{"layout":layout}}));
                    }
                }
                for mut message in messages {
                    hub.visual_state.render_generation =
                        hub.visual_state.render_generation.wrapping_add(1);
                    if let Err(error) = visual::prepare(&mut hub, &action.plugin_id, &mut message) {
                        message["event"] = json!("unsupportedFeature");
                        message["payload"] = json!({"message":error});
                    }
                    if hub.effects.len() < 512 {
                        let effect = hub.message_effect(&action.plugin_id, None, &message, None);
                        hub.effects.push_back(effect);
                        if let Some(wake) = &hub.wake {
                            wake();
                        }
                    }
                }
            }
        }
        let removed: Vec<_> = hub
            .context_state
            .visible
            .keys()
            .filter(|c| !desired.contains(*c))
            .cloned()
            .collect();
        for context in removed {
            hub.hide_appearance(&context);
        }
    }
    // Retire action appearances belonging to the disconnected device
    pub fn retire_device_appearances(&mut self, device: &str) {
        let mut hub = self.hub.lock().unwrap();
        hub.context_state
            .pending_appearances
            .retain(|(_, d, _, _)| d != device);
        let contexts: Vec<_> = hub
            .context_state
            .visible
            .keys()
            .filter(|context| {
                hub.context_state
                    .targets
                    .get(*context)
                    .is_some_and(|(d, _)| d == device)
            })
            .cloned()
            .collect();
        for context in contexts {
            hub.hide_appearance(&context);
        }
    }
    // Poll for the current operation
    pub fn poll(&mut self) -> bool {
        let revision = self.hub.lock().unwrap().theme_revision;
        let mut changed = self.theme_revision != revision;
        self.theme_revision = revision;
        let now = std::time::Instant::now();
        let connected = {
            let mut hub = self.hub.lock().unwrap();
            hub.sessions.retired_contexts.expire(now);
            changed |= hub.expire_manual_actions(now);
            hub.sessions.connected.clone()
        };
        let mut failures = vec![];
        for (id, p) in &mut self.plugins {
            if p.stopping
                .as_ref()
                .is_some_and(|done| done.load(Ordering::Acquire))
            {
                p.stopping = None;
                if p.info.runtime.state == RuntimeState::Stopping {
                    p.info.runtime.state = RuntimeState::Idle;
                }
                changed = true;
            }
            if p.info.runtime.state == RuntimeState::Starting && connected.contains(&p.token) {
                if let Err(error) = self.recovery.finish(id) {
                    failures.push((id.clone(), error, true, None, None));
                    continue;
                }
                p.info.runtime.state = RuntimeState::Running;
                p.info.runtime.reason =
                    if p.info.enabled && self.required.get(id).is_some_and(|d| !d.is_empty()) {
                        RuntimeReason::RequiredByActiveProfile
                    } else {
                        RuntimeReason::Configuration
                    };
                p.info.status = "Running".into();
                p.diagnostic(
                    manager_diagnostics::PluginEvent::Registered,
                    None,
                    None,
                    None,
                    false,
                );
                p.registered = Some(now);
                self.starting.remove(id);
                let mut hub = self.hub.lock().unwrap();
                hub.health_mut(id).last_registration_ms = Some(monitor::timestamp_ms());
                changed = true;
            }
            // A successful minute must not replenish crash retries indefinitely
            // Only deliberate retry/restart/re-enable resets this launch-session budget
            if !matches!(
                p.info.runtime.state,
                RuntimeState::Starting | RuntimeState::Running
            ) {
                continue;
            }
            if let Some(status) = p
                .child
                .as_mut()
                .filter(|_| {
                    !p.exit_watch
                        .as_ref()
                        .is_some_and(|watch| watch.healthy() && !watch.ready())
                })
                .and_then(|child| process_tree::poll_exit(child, p.tree.as_ref()))
            {
                #[cfg(unix)]
                let signal = {
                    use std::os::unix::process::ExitStatusExt;
                    status.signal()
                };
                #[cfg(not(unix))]
                let signal = None;
                failures.push((
                    id.clone(),
                    format!("Process exited: {status}"),
                    p.info.runtime.state == RuntimeState::Starting,
                    status.code(),
                    signal,
                ));
            } else if p.info.runtime.state == RuntimeState::Running && !connected.contains(&p.token)
            {
                failures.push((
                    id.clone(),
                    "Plugin connection closed unexpectedly".into(),
                    false,
                    None,
                    None,
                ));
            } else if self
                .starting
                .get(id)
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(5))
            {
                failures.push((
                    id.clone(),
                    "Registration timed out".into(),
                    true,
                    None,
                    None,
                ));
            }
        }
        for (id, message, starting, code, signal) in failures {
            self.plugins[&id].diagnostic(
                if starting {
                    manager_diagnostics::PluginEvent::StartupFailed
                } else {
                    manager_diagnostics::PluginEvent::UnexpectedExit
                },
                Some(manager_diagnostics::classify(&message)),
                code,
                signal,
                false,
            );
            let _ = self.launch(&id, false, false);
            let p = self.plugins.get_mut(&id).unwrap();
            let mut hub = self.hub.lock().unwrap();
            hub.issue(
                &id,
                if starting { "failed_start" } else { "crash" },
                &message,
            );
            let health = hub.health_mut(&id);
            health.last_exit = Some(message.clone());
            health.last_exit_code = code;
            health.last_exit_signal = signal;
            if p.restarts < 3 {
                p.restarts += 1;
                p.next_restart = now + Duration::from_secs(1 << p.restarts);
                p.info.runtime.state = RuntimeState::Retrying;
                p.info.runtime.reason = if starting {
                    RuntimeReason::RegistrationTimeout
                } else {
                    RuntimeReason::UnexpectedExit
                };
                p.info.status = format!("Retrying in {} seconds: {message}", 1 << p.restarts);
                p.diagnostic(
                    manager_diagnostics::PluginEvent::RetryScheduled,
                    Some(manager_diagnostics::classify(&message)),
                    code,
                    signal,
                    false,
                );
            } else {
                p.info.runtime.state = RuntimeState::Blocked;
                p.info.runtime.reason = RuntimeReason::RepeatedFailures;
                p.info.status =
                    "Blocked — automatic restart budget exhausted; choose Retry or disable".into();
                hub.health_mut(&id).blocked_reason = Some(p.info.status.clone());
                p.diagnostic(
                    manager_diagnostics::PluginEvent::Blocked,
                    Some(manager_diagnostics::classify(&message)),
                    code,
                    signal,
                    false,
                );
            }
            changed = true;
        }
        self.reconcile();
        let retries: Vec<_> = self
            .plugins
            .iter()
            .filter(|(id, p)| {
                p.info.runtime.state == RuntimeState::Retrying
                    && p.stopping.is_none()
                    && now >= p.next_restart
                    && self.eligible(id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in retries {
            let p = self.plugins.get_mut(&id).unwrap();
            p.info.runtime.state = RuntimeState::Queued;
            p.info.status = "Queued for restart".into();
            self.startup.push_back(id.clone());
            self.hub.lock().unwrap().issue(
                &id,
                "automatic_restart",
                "Automatic restart after unexpected failure",
            );
            changed = true;
        }
        self.prioritize_startup();
        if !self.startup.is_empty() && self.starting.len() < 2 && now >= self.next_start {
            let id = self.startup.pop_front().unwrap();
            if self.eligible(&id) {
                if let Err(error) = self.launch(&id, true, false) {
                    self.startup_failure(&id, &error);
                } else {
                    self.starting.insert(id, now);
                }
                changed = true;
            }
            self.next_start = now + Duration::from_millis(100);
        }
        {
            let mut hub = self.hub.lock().unwrap();
            hub.flush_inspector_requests(&self.plugins);
            let expired: Vec<_> = hub
                .visual_state
                .badges
                .iter()
                .filter(|(_, deadline)| std::time::Instant::now() >= **deadline)
                .map(|(context, _)| context.clone())
                .collect();
            for context in expired {
                hub.visual_state.badges.remove(&context);
                hub.visual_state.visual_generation =
                    hub.visual_state.visual_generation.wrapping_add(1);
                if hub
                    .visual_state
                    .visuals
                    .get(&context)
                    .is_some_and(|value| Arc::strong_count(value) > 1)
                {
                    hub.visual_state.visual_cow_copies =
                        hub.visual_state.visual_cow_copies.saturating_add(1);
                }
                if let Some(visual) = hub.visual_state.visuals.get_mut(&context) {
                    Arc::make_mut(visual)["badge"] = Value::Null;
                }
                if let Some(visual) = hub.visual_state.software_visuals.get_mut(&context) {
                    Arc::make_mut(visual)["badge"] = Value::Null;
                }
                if hub.visual_state.visuals.contains_key(&context)
                    && hub.context_state.targets.contains_key(&context)
                {
                    let event = hub.feedback_effect(
                        FeedbackKind::BadgeExpired,
                        &context,
                        Some(hub.visual_state.render_generation),
                    );
                    if hub.effects.len() < 512 {
                        hub.effects.push_back(event);
                        if let Some(wake) = &hub.wake {
                            wake();
                        }
                    }
                }
            }
        }
        if self.last_health_flush.elapsed() >= Duration::from_secs(2) {
            let _ = self.hub.lock().unwrap().flush_health();
            self.last_health_flush = now;
        }
        changed
    }
    /// Generation and selected visuals are captured under one hub lock. This is
    /// deliberately separate from the theme-only appearance revision.
    pub fn visual_storage_stats(&self) -> Value {
        let hub = self.hub.lock().unwrap();
        json!({"current_visuals":hub.visual_state.visuals.len(),"copy_on_write_mutations":hub.visual_state.visual_cow_copies,"shared_captures":hub.visual_state.visual_shared_captures})
    }
    // Render generation
    pub fn render_generation(&self) -> u64 {
        self.hub.lock().unwrap().visual_state.render_generation
    }
    // Read the current plugin visual generation
    pub fn visual_generation(&self) -> u64 {
        self.hub.lock().unwrap().visual_state.visual_generation
    }
    // Render fallback
    fn render_fallback(
        &self,
        hub: &Hub,
        device: &str,
        id: &str,
        action: &str,
    ) -> Option<RenderFallback> {
        let plugin = self.plugins.get(id)?;
        let capability = plugin.info.capabilities.as_ref()?.actions.get(action)?;
        let mode = hub.resolved_device_theme_for(id, device);
        Some(RenderFallback {
            root: plugin.path.clone(),
            asset: capability.icon.clone(),
            mode,
            palette: hub.plugin_palette_id_for(id, mode, device).into(),
            colors: hub.plugin_colors_for(id, mode, device),
            title: plugin
                .info
                .actions
                .iter()
                .find(|entry| entry.id == action)
                .map(|entry| entry.name.clone())
                .unwrap_or_default(),
        })
    }
    // Resolve the icon declared for this plugin action
    pub fn action_icon(&self, id: &str, action: &str) -> Option<Value> {
        let fallback = self.render_fallback(&self.hub.lock().unwrap(), "", id, action)?;
        fallback.resolve()
    }
    /// `capture` runs after visual copying while the hub lock is held. It must
    /// only take short, leaf locks; never call back into PluginHost or runtime state.
    pub fn render_surfaces<T>(
        &self,
        device: &str,
        requests: &[(String, String)],
        fallback_requests: &[(String, String, String)],
        capture: impl FnOnce() -> T,
    ) -> (RenderSurfaceSnapshot, T) {
        self.render_surfaces_for_target(device, requests, fallback_requests, false, capture)
    }
    // Render surfaces for target
    pub fn render_surfaces_for_target<T>(
        &self,
        device: &str,
        requests: &[(String, String)],
        fallback_requests: &[(String, String, String)],
        software: bool,
        capture: impl FnOnce() -> T,
    ) -> (RenderSurfaceSnapshot, T) {
        let mut hub = self.hub.lock().unwrap();
        hub.visual_state.visual_shared_captures =
            hub.visual_state.visual_shared_captures.saturating_add(1);
        let mut values = HashMap::new();
        let mut artwork = HashMap::new();
        let mut contexts = HashMap::new();
        for (control, scope) in requests {
            let selected = if software {
                &hub.visual_state.software_visuals
            } else {
                &hub.visual_state.visuals
            };
            if let Some((context, value)) = selected
                .iter()
                .chain(
                    hub.visual_state
                        .visuals
                        .iter()
                        .filter(|(context, _)| software && !selected.contains_key(*context)),
                )
                .filter(|(context, _)| {
                    hub.context_state
                        .targets
                        .get(*context)
                        .is_some_and(|(d, c)| d == device && c == control)
                        && hub.context_state.contexts.contains_key(*context)
                        && hub
                            .context_state
                            .scopes
                            .get(*context)
                            .is_some_and(|s| s == scope || s.starts_with(&format!("{scope}@")))
                })
                .max_by_key(|(context, _)| {
                    (if software {
                        &hub.visual_state.software_order
                    } else {
                        &hub.visual_state.visual_order
                    })
                    .get(*context)
                    .copied()
                    .unwrap_or(0)
                })
                .map(|(context, value)| (context.clone(), value.clone()))
            {
                if let Some(files) = hub.visual_state.artwork(&context, software) {
                    artwork.insert(control.clone(), files.clone());
                }
                contexts.insert(control.clone(), context);
                values.insert(control.clone(), value);
            }
        }
        let mut fallbacks = HashMap::new();
        let mut plugin_surfaces = HashMap::new();
        for (source, plugin_id, action_id) in fallback_requests {
            let mode = hub.resolved_device_theme_for(plugin_id, device);
            if (mode != hub.device_mode_for(device)
                || (hub.palette_theme_preview.is_some()
                    && hub.capabilities.contains_key(plugin_id)))
                && !plugin_surfaces.contains_key(plugin_id)
            {
                plugin_surfaces.insert(
                    plugin_id.clone(),
                    hub.plugin_colors_for(plugin_id, mode, device)["surface"]
                        .as_str()
                        .expect("theme surface")
                        .into(),
                );
            }
            if values.contains_key(source) {
                continue;
            }
            if let Some(fallback) = self.render_fallback(&hub, device, plugin_id, action_id) {
                fallbacks.insert(source.clone(), fallback);
            }
        }
        let extra = capture();
        (
            RenderSurfaceSnapshot {
                artwork,
                generation: hub.visual_state.render_generation,
                device_palette: hub.device_colors_for(hub.device_mode_for(device), device),
                wallpaper: hub
                    .device_wallpaper_for(hub.device_mode_for(device), device)
                    .cloned(),
                device_theme: hub.device_theme_for(device),
                plugin_surfaces,
                values,
                fallbacks,
                contexts,
            },
            extra,
        )
    }
    // Resolve the surface value for this operation
    pub fn surface(&self, device: &str, control: &str, scope: &str) -> Option<Value> {
        let hub = self.hub.lock().unwrap();
        let selected = hub
            .visual_state
            .visuals
            .iter()
            .filter(|(context, _)| {
                hub.context_state
                    .targets
                    .get(*context)
                    .is_some_and(|(d, c)| d == device && c == control)
                    && hub.context_state.contexts.contains_key(*context)
                    && hub
                        .context_state
                        .scopes
                        .get(*context)
                        .is_some_and(|s| s == scope || s.starts_with(&format!("{scope}@")))
            })
            .max_by_key(|(context, _)| {
                hub.visual_state
                    .visual_order
                    .get(*context)
                    .copied()
                    .unwrap_or(0)
            })
            .map(|(context, value)| {
                (
                    value.clone(),
                    hub.visual_state.artwork(context, false).cloned(),
                )
            });
        drop(hub);
        let (value, files) = selected?;
        let mut value = value.as_ref().clone();
        if let Some(files) = files {
            files.expand(&mut value).ok()?;
        }
        Some(value)
    }
    /// Read an exact active action without starting or configuring a plugin
    /// Read a declared layout without registering a context or executing code
    pub fn declared_surface(
        &self,
        device: &str,
        id: &str,
        action: &str,
        wide: bool,
    ) -> Option<Value> {
        let plugin = self.plugins.get(id)?;
        let capability = plugin.info.capabilities.as_ref()?.actions.get(action)?;
        let name = capability.feedback_layout.as_ref()?;
        let name = if !wide && name == "display/wide.json" {
            "display/key.json"
        } else {
            name.as_str()
        };
        let (theme, palette, colors) = {
            let hub = self.hub.lock().unwrap();
            let theme = hub.resolved_device_theme_for(id, device);
            (
                theme,
                hub.plugin_palette_id_for(id, theme, device).to_owned(),
                hub.plugin_colors_for(id, theme, device),
            )
        };
        let shaped = if wide && !name.ends_with("wide.json") {
            name.strip_suffix(".json")
                .map(|stem| format!("{stem}-wide.json"))
        } else if !wide {
            name.strip_suffix("-wide.json")
                .map(|stem| format!("{stem}-key.json"))
        } else {
            None
        };
        let name = shaped
            .as_deref()
            .filter(|path| {
                theme::variant(&plugin.path, path, theme, &palette).is_some()
                    || safe_file(&plugin.path, path).is_ok()
            })
            .unwrap_or(name);
        let (mut layout, palette_tile) =
            visual::themed_layout(&plugin.path, name, theme, &palette, &colors).ok()?;
        let asset = if theme == "light" {
            &capability.icon.light
        } else {
            &capability.icon.dark
        };
        let variant = theme::variant(&plugin.path, asset, theme, &palette);
        let icon =
            visual::image_with_theme(&plugin.path, variant.as_deref().unwrap_or(asset), &colors)
                .ok();
        if let Some(items) = layout["items"].as_array_mut() {
            for item in items {
                if palette_tile
                    && matches!(
                        item["key"].as_str(),
                        Some("backdrop" | "caption-background" | "line2")
                    )
                {
                    continue;
                }
                item["value"] = if item["type"] == "pixmap" {
                    icon.clone().unwrap_or(Value::Null)
                } else if item["type"] == "text" {
                    json!("—")
                } else {
                    json!(0)
                };
            }
        }
        Some(
            json!({"plugin_id":id,"action_id":action,"layout":layout,"feedback":{},"title":"—","state":0,"paletteTile":palette_tile}),
        )
    }
    // Find a surface compatible with the requested action
    pub fn matching_surface(
        &self,
        device: &str,
        control: &str,
        scope: &str,
        plugin: &str,
        action: &str,
        settings: &Value,
    ) -> Option<Value> {
        self.matching_surface_for_target(device, control, scope, plugin, action, settings, false)
    }
    #[allow(clippy::too_many_arguments)]
    // Find a compatible surface for the selected target
    pub fn matching_surface_for_target(
        &self,
        device: &str,
        control: &str,
        scope: &str,
        plugin: &str,
        action: &str,
        settings: &Value,
        software: bool,
    ) -> Option<Value> {
        let (visual, files) = self
            .matching_render_surface(device, control, scope, plugin, action, settings, software)?;
        let mut value = visual.as_ref().clone();
        files.expand(&mut value).ok()?;
        Some(value)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn matching_render_surface(
        &self,
        device: &str,
        control: &str,
        scope: &str,
        plugin: &str,
        action: &str,
        settings: &Value,
        software: bool,
    ) -> Option<(Arc<Value>, ArtworkFiles)> {
        let hub = self.hub.lock().unwrap();
        let context = hub.lookup_binding_context(plugin, scope, device, control)?;
        if hub.context_state.contexts.get(&context).map(String::as_str) != Some(plugin)
            || hub
                .context_state
                .visible
                .get(&context)
                .is_none_or(|v| v["action"] != action)
            || (hub.context_state.configured.get(&context) != Some(settings)
                && hub.context_state.settings.get(&context) != Some(settings))
        {
            return None;
        }
        (if software {
            hub.visual_state.software_visuals.get(&context)
        } else {
            None
        })
        .or_else(|| hub.visual_state.visuals.get(&context))
        .map(|value| {
            (
                value.clone(),
                hub.visual_state
                    .artwork(&context, software)
                    .cloned()
                    .unwrap_or_default(),
            )
        })
    }
    // Drain effects
    pub fn drain_effects(&mut self) -> Vec<HostEffect> {
        let mut hub = self.hub.lock().unwrap();
        let size = hub.effects.len().min(64);
        let effects = hub.effects.drain(..size).collect();
        if !hub.effects.is_empty() {
            if let Some(wake) = &hub.wake {
                wake();
            }
        }
        effects
    }
    // Resolve the browser view belonging to the plugin
    pub fn browser_view(&self, id: &str) -> Result<Option<BrowserView>, String> {
        self.recovery.check()?;
        let plugin = self.plugins.get(id).ok_or("plugin not installed")?;
        if plugin.info.kind != "browser"
            || !matches!(
                plugin.info.runtime.state,
                RuntimeState::Starting | RuntimeState::Running
            )
        {
            return Ok(None);
        }
        let relative = plugin
            .entry
            .strip_prefix(&plugin.path)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        Ok(Some(BrowserView {
            label: format!("plugin-{}", plugin.token),
            url: format!(
                "http://127.0.0.1:{}/files/{}/{}",
                self.port, plugin.token, relative
            ),
        }))
    }
    // Close inspector in hub
    fn close_inspector_in_hub(hub: &mut Hub, token: &str) {
        hub.sessions.inspector_origins.remove(token);
        hub.pending_inspectors
            .retain(|request| request.token != token);
        hub.configuration_sessions.remove(token);
        hub.configuration_announced.remove(token);
        hub.location_drafts.remove(token);
        hub.revoke_storage_session(token);
        if let Some(owner) = hub.sessions.tokens.remove(token) {
            if let Some(grant) = hub.sessions.files.remove(token) {
                if let Some(context) = grant.context {
                    let disappearance =
                        hub.inspector_live_context(&context, &owner).map(|context| {
                            hub.inspector_lifecycle_event("propertyInspectorDidDisappear", &context)
                        });
                    hub.configuration_actions.remove(&context);
                    hub.global_drafts.remove(&context);
                    hub.default_drafts.remove(&context);
                    if hub.context_state.drafts.remove(&context) {
                        hub.configuration_actions.remove(&context);
                        hub.global_drafts.remove(&context);
                        hub.default_drafts.remove(&context);
                        hub.context_state.settings.remove(&context);
                        hub.visual_state.render_generation =
                            hub.visual_state.render_generation.wrapping_add(1);
                        hub.retire_context(&context);
                    }
                    if !hub.context_state.visible.contains_key(&context) {
                        hub.visual_state.render_generation =
                            hub.visual_state.render_generation.wrapping_add(1);
                        hub.retire_context(&context);
                        hub.context_state.scopes.remove(&context);
                        hub.context_state.targets.remove(&context);
                        hub.context_state.addresses.remove(&context);
                        hub.context_state.configured.remove(&context);
                        hub.context_state.execution.remove(&context);
                        hub.context_state.reference_sources.remove(&context);
                        hub.visual_state.render_generation =
                            hub.visual_state.render_generation.wrapping_add(1);
                        hub.visual_state.visuals.remove(&context);
                        hub.visual_state.visual_sources.remove(&context);
                    }
                    if let Some(event) = disappearance {
                        hub.sessions
                            .queues
                            .entry(owner)
                            .or_default()
                            .push_back(event);
                    }
                }
            }
            hub.sessions.queues.remove(&format!("pi:{token}"));
        }
    }
    // Close inspector
    pub fn close_inspector(&mut self, token: &str) {
        let mut hub = self.hub.lock().unwrap();
        Self::close_inspector_in_hub(&mut hub, token);
        drop(hub);
        self.reconcile();
    }
    // Shutdown for the current operation
    pub fn shutdown(&mut self) {
        self.required.clear();
        self.startup.clear();
        self.starting.clear();
        let ids: Vec<_> = self.plugins.keys().cloned().collect();
        for id in ids {
            let _ = self.launch(&id, false, false);
        }
        let _ = self.recovery.clear();
        let _ = self.hub.lock().unwrap().flush_health();
        self.stop.store(true, Ordering::SeqCst);
        if let Some(registry) = &self.exit_registry {
            registry.shutdown();
        }
        if let Some(wake) = &self.listener_wake {
            let _ = wake.wake();
        }
        let mut hub = self.hub.lock().unwrap();
        hub.settings_writer = None;
        hub.pending_settings.clear();
        hub.pending_saved_reads.clear();
        for alive in hub.sessions.storage_sessions.values() {
            alive.store(false, Ordering::Release);
        }
        hub.sessions.storage_sessions.clear();
        for id in hub.installations.keys() {
            hub.workflow.invalidate_owner(id);
        }
        hub.permission_sessions.clear();
        hub.vault_sessions.clear();
        hub.theme_observer.wake();
        for queue in hub.sessions.queues.values() {
            if let Some(wake) = &queue.wake {
                let _ = wake.wake();
            }
        }
        drop(hub);
        for thread in self.transport_threads.drain(..) {
            let _ = thread.join();
        }
    }
    // Shutdown waiters
    pub fn shutdown_waiters(&self) -> Vec<Arc<AtomicBool>> {
        self.plugins
            .values()
            .filter_map(|p| p.stopping.clone())
            .collect()
    }
}

impl Drop for PluginHost {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.shutdown();
    }
}
// Validate a plugin identifier before using it in managed paths
fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() < 160
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}
// Validate a package file before accessing its contents
fn safe_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if !safe_relative(relative) {
        return Err("unsafe package path".into());
    }
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("unsafe package path".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if !path.starts_with(root) {
        return Err("package path escapes its root".into());
    }
    Ok(path)
}
// Read plugin
fn read_plugin(path: &Path) -> Result<Plugin, String> {
    read_plugin_with_identity(path, None)
}
// Read plugin with identity
fn read_plugin_with_identity(
    path: &Path,
    installed_identity: Option<&str>,
) -> Result<Plugin, String> {
    let bytes = std::fs::read(path.join("manifest.json")).map_err(|e| e.to_string())?;
    if bytes.len() > 1024 * 1024 {
        return Err("manifest too large".into());
    }
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let normalized = manifest::normalize_with_identity(path, &manifest, installed_identity)?;
    let id = normalized.id.as_str();
    let entry = safe_file(path, &normalized.entry_point)?;
    let kind = normalized.runtime_kind.as_str();
    let inspection = capabilities::inspect(path, &entry, &manifest)?;
    manifest_schema::validate(&manifest, id, &normalized.dialect, &inspection)?;
    let capabilities = inspection.into_manager();
    if (normalized.setup_slots.is_some() || normalized.permissions.is_some())
        && (kind == "browser" || capabilities.is_none())
    {
        return Err(
            "Manager permissions and Vault setup require a native Manager-compatible plugin".into(),
        );
    }
    let elgato = capabilities.is_none()
        && matches!(normalized.dialect.as_str(), "streamdeck" | "openaction");
    let mut thumbnails = vendor_presentation::Thumbnails::new(path);
    if normalized.dialect == "openaction" {
        thumbnails.openaction();
    }
    let plugin_icon = elgato
        .then(|| thumbnails.image(manifest["Icon"].as_str()))
        .flatten();
    let actions = manifest["Actions"]
        .as_array()
        .ok_or("Actions array missing")?
        .iter()
        .filter(|action| !elgato || vendor_presentation::supports_os(action, sdk_platform()))
        .map(|a| {
            Ok(PluginAction {
                id: a["UUID"].as_str().ok_or("action UUID missing")?.into(),
                name: a["Name"].as_str().unwrap_or("Untitled").into(),
                control_types: capabilities
                    .as_ref()
                    .and_then(|c| c.actions.get(a["UUID"].as_str().unwrap_or("")))
                    .map(|c| c.control_types.clone())
                    .unwrap_or_else(|| action_control_types(a)),
                has_settings: capabilities
                    .as_ref()
                    .and_then(|c| c.actions.get(a["UUID"].as_str().unwrap_or("")))
                    .map_or_else(
                        || {
                            a["PropertyInspectorPath"].is_string()
                                || manifest["PropertyInspectorPath"].is_string()
                        },
                        |ac| ac.settings.is_some(),
                    ),
                supported_in_multi_actions: a["SupportedInMultiActions"].as_bool().unwrap_or(true),
                supported_in_key_logic_actions: a["SupportedInKeyLogicActions"]
                    .as_bool()
                    .unwrap_or(true),
                visible_in_actions_list: a["VisibleInActionsList"].as_bool().unwrap_or(true),
                user_title_enabled: a["UserTitleEnabled"].as_bool().unwrap_or(true),
                state_names: a["States"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(64)
                    .enumerate()
                    .map(|(index, state)| {
                        state["Name"]
                            .as_str()
                            .filter(|name| !name.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("State {}", index + 1))
                    })
                    .collect(),
                multi_action_images: elgato.then(|| {
                    a["States"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .take(64)
                        .map(|state| {
                            thumbnails.image(
                                state["MultiActionImage"]
                                    .as_str()
                                    .or_else(|| state["Image"].as_str()),
                            )
                        })
                        .collect()
                }),
                encoder_icon: elgato
                    .then(|| thumbnails.image(a["Encoder"]["Icon"].as_str()))
                    .flatten(),
                icon: elgato
                    .then(|| thumbnails.image(a["Icon"].as_str()))
                    .flatten(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Plugin {
        manifest: manifest.clone(),
        metadata: Arc::new(
            manifest["Actions"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|action| {
                    action["UUID"]
                        .as_str()
                        .map(|id| (id.to_owned(), action.clone()))
                })
                .collect(),
        ),
        resource_history: Default::default(),
        resource_sampled_at: None,
        info: PluginInfo {
            permissions: normalized.permissions.clone(),
            id: id.into(),
            author: normalized.publisher.clone(),
            name: manifest["Name"].as_str().unwrap_or(id).into(),
            version: manifest["Version"].as_str().unwrap_or("unknown").into(),
            dialect: normalized.dialect.clone(),
            enabled: false,
            has_settings_interface: normalized.dialect == "openaction"
                && manifest["HasSettingsInterface"] == true,
            sleep_when_hidden: true,
            kind: kind.into(),
            status: "Disabled — unverified package".into(),
            health: Default::default(),
            runtime: Default::default(),
            running_runtime: None,
            capabilities,
            icon: plugin_icon,
            appearance: "follow-ui".into(),
            actions,
        },
        path: path.into(),
        entry,
        token: String::new(),
        child: None,
        exit_watch: None,
        restarts: 0,
        next_restart: std::time::Instant::now(),
        started: None,
        registered: None,
        tree: None,
        stopping: None,
    })
}
// Extract archive
pub fn extract_archive(archive: &Path, target: &Path) -> Result<(), String> {
    if archive.metadata().map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
        return Err("Compressed plugin exceeds 256 MiB".into());
    }
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| format!("Unsupported or protected plugin container: {e}; choose an unprotected ZIP-based installer"))?;
    stream_dock_manager_plugin_runtimes::archives::validate_lzma_headers(&mut zip)?;
    if zip.len() > 4096 {
        return Err("too many files".into());
    }
    let mut total = 0u64;
    let mut names = std::collections::BTreeMap::<String, bool>::new();
    // Validate every entry before writing any extracted files
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(|e| e.to_string())?;
        let name = entry.name().map_err(|e| e.to_string())?;
        let name = name.trim_end_matches('/');
        if !safe_relative(name) || name.split('/').count() > 64 {
            return Err("unsafe archive path".into());
        }
        let normalized = name.to_lowercase();
        if names.insert(normalized, entry.is_dir()).is_some() {
            return Err("duplicate or case-colliding archive path".into());
        }
        if entry
            .unix_mode()
            .is_some_and(|m| ![0, 0o100000, 0o040000].contains(&(m & 0o170000)))
        {
            return Err("archive symlinks or special files rejected".into());
        }
        total = total
            .checked_add(entry.size())
            .ok_or("archive size overflow")?;
        if total > 256 * 1024 * 1024 || entry.size() > 64 * 1024 * 1024 {
            return Err("plugin exceeds size limits".into());
        }
    }
    stream_dock_manager_plugin_runtimes::archives::validate_lzma_eos(
        &mut zip,
        64 * 1024 * 1024,
        256 * 1024 * 1024,
    )?;
    for name in names.keys() {
        let mut parent = name.as_str();
        while let Some((prefix, _)) = parent.rsplit_once('/') {
            if names.get(prefix) == Some(&false) {
                return Err("archive file/directory collision".into());
            }
            parent = prefix;
        }
    }
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let path = target.join(entry.enclosed_name().ok_or("unsafe archive path")?);
        if entry.is_dir() {
            std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| e.to_string())?;
            let size = entry.size();
            let mode = entry.unix_mode().unwrap_or(0);
            let written = std::io::copy(&mut entry.by_ref().take(64 * 1024 * 1024 + 1), &mut file)
                .map_err(|e| e.to_string())?;
            if written != size {
                return Err("archive entry has an inconsistent size".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600 | (mode & 0o111)))
                    .map_err(|e| e.to_string())?;
            }
            #[cfg(not(unix))]
            let _ = mode;
            file.flush().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// Hash the package file inventory
fn hash_inventory(
    inventory: &std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(inventory).map_err(|e| e.to_string())?,
    )))
}
// Validate a relative path within the package
fn safe_relative(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(['\\', ':', '\0'])
        && value.split('/').all(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit());
            !reserved
                && !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with([' ', '.'])
        })
        && !Path::new(value).is_absolute()
}

// Spawn connection
fn spawn_connection(
    stream: TcpStream,
    hub: Arc<Mutex<Hub>>,
    stop: Arc<AtomicBool>,
    admission: Arc<server_admission::Admission>,
    completion: Option<listener::Completion>,
) -> Option<std::thread::JoinHandle<()>> {
    // Reject before creating a thread, not after allocating its stack.
    if stop.load(Ordering::SeqCst) {
        return None;
    }
    let permit = admission.handshakes.acquire()?;
    std::thread::Builder::new()
        .name("plugin-connection".into())
        .spawn(move || {
            let _completion = completion;
            serve_admitted(stream, hub, stop, admission, permit);
        })
        .ok()
}

#[cfg(test)]
// Serve for the current operation
fn serve(stream: TcpStream, hub: Arc<Mutex<Hub>>, stop: Arc<AtomicBool>) {
    let admission = Arc::new(server_admission::Admission::default());
    let permit = admission.handshakes.acquire().unwrap();
    serve_admitted(stream, hub, stop, admission, permit);
}

// Serve admitted
fn serve_admitted(
    stream: TcpStream,
    hub: Arc<Mutex<Hub>>,
    stop: Arc<AtomicBool>,
    admission: Arc<server_admission::Admission>,
    handshake: server_admission::Permit,
) {
    let mut handshake = Some(handshake);
    // Accepted sockets can inherit O_NONBLOCK from the listener on some OSes
    // Request routing initially uses bounded blocking I/O. The WebSocket
    // handshake below resumes nonblocking without losing pipelined bytes.
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let mut prefix = [0u8; 8192];
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    // A TCP read may contain only part of the request line. Do not route a
    // fragmented ordinary GET into the WebSocket handshake.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match stream.peek(&mut prefix) {
            Ok(0) => return,
            Ok(size) if size >= 12 => {
                if prefix[..size].starts_with(b"GET /files/") {
                    let Some(_transfer) = admission.files.acquire() else {
                        let mut stream = stream;
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                        let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nRetry-After: 1\r\nConnection: close\r\n\r\n");
                        return;
                    };
                    drop(handshake.take());
                    serve_file(stream, hub);
                    return;
                }
                break;
            }
            Ok(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1))
            }
            _ => return,
        }
    }
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES))
        .max_write_buffer_size(2 * MAX_MESSAGE_BYTES);
    let mut browser_origin: Option<String> = None;
    // tungstenite's Callback requires its concrete HTTP ErrorResponse type.
    #[allow(clippy::result_large_err)]
    let mut result = tungstenite::accept_hdr_with_config(
        stream,
        |request: &tungstenite::handshake::server::Request, response| {
            let origins: Vec<_> = request.headers().get_all("origin").iter().collect();
            browser_origin = match origins.as_slice() {
                [] => None,
                [origin] => Some(origin.to_str().unwrap_or("invalid-origin").to_owned()),
                _ => Some("invalid-origin".into()),
            };
            Ok(response)
        },
        Some(config),
    );
    let ws = loop {
        if stop.load(Ordering::SeqCst) || std::time::Instant::now() >= deadline {
            return;
        }
        match result {
            Ok(ws) => break ws,
            Err(tungstenite::HandshakeError::Interrupted(pending)) => {
                // A per-read socket timeout can be renewed indefinitely by a
                // trickled header. Bound the whole handshake, not each read.
                std::thread::sleep(Duration::from_millis(5));
                result = pending.handshake();
            }
            Err(tungstenite::HandshakeError::Failure(_)) => return,
        }
    };
    let Ok(mut ws) = transport::Socket::new(ws) else {
        return;
    };
    let mut owner: Option<String> = None;
    let mut registered_token = String::new();
    let mut inspector_context: Option<String> = None;
    let start = std::time::Instant::now();
    'connection: loop {
        if stop.load(Ordering::SeqCst) && owner.is_none() {
            break;
        }
        if owner.is_none() && start.elapsed() > Duration::from_secs(5) {
            break;
        }
        if let Some(ref id) = owner {
            let shutdown = hub
                .lock()
                .unwrap()
                .sessions
                .shutdowns
                .remove(&registered_token);
            if let Some(mut events) = shutdown {
                loop {
                    let batch = match events.take_batch() {
                        Ok(batch) if batch.is_empty() => break,
                        Ok(batch) => batch,
                        Err(error) => {
                            hub.lock().unwrap().issue(id, "transport_overflow", error);
                            break;
                        }
                    };
                    for event in batch {
                        if ws
                            .send(tungstenite::Message::Text(event.value.to_string().into()))
                            .is_err()
                        {
                            break 'connection;
                        }
                    }
                }
                break;
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let notices = {
                let h = hub.lock().unwrap();
                if h.permission_sessions.contains_key(&registered_token) {
                    h.sessions
                        .storage_sessions
                        .get(&registered_token)
                        .map(|l| h.workflow.notices(l))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
            };
            for notice in notices {
                if ws
                    .send(tungstenite::Message::text(notice.to_string()))
                    .is_err()
                {
                    break 'connection;
                }
            }
            let messages = {
                let mut hub = hub.lock().unwrap();
                if hub.sessions.tokens.get(&registered_token) != Some(id)
                    || !inspector_browser::origin_allowed(
                        &hub,
                        &registered_token,
                        browser_origin.as_deref(),
                    )
                {
                    break;
                }
                // Validation and admission share the revocation lock. Bytes
                // already selected before revocation may finish; no later batch
                // can be selected after the lease has been revoked.
                hub.sessions
                    .queues
                    .entry(if inspector_context.is_some() {
                        format!("pi:{registered_token}")
                    } else {
                        id.clone()
                    })
                    .or_default()
                    .take_batch()
            };
            let messages = match messages {
                Ok(messages) => messages,
                Err(error) => {
                    hub.lock().unwrap().issue(id, "transport_overflow", error);
                    break;
                }
            };
            for message in messages {
                if ws
                    .send(tungstenite::Message::text(message.value.to_string()))
                    .is_err()
                {
                    break 'connection;
                }
            }
        }
        match ws.read() {
            Ok(tungstenite::Message::Text(text)) => {
                let Ok(mut message) = serde_json::from_str::<Value>(&text) else {
                    break;
                };
                if owner.is_none() {
                    // Token/session validation and wake ownership must be one
                    // hub transaction: a restart cannot interleave an old
                    // registration with the replacement connection's queue.
                    let registration = {
                        let mut hub = hub.lock().unwrap();
                        if !inspector_browser::origin_allowed(
                            &hub,
                            message["uuid"].as_str().unwrap_or(""),
                            browser_origin.as_deref(),
                        ) {
                            break;
                        }
                        transport::register(&mut hub, &message, ws.wake.clone())
                    };
                    let Some(registration) = registration else {
                        break;
                    };
                    owner = Some(registration.owner);
                    registered_token = registration.token;
                    inspector_context = registration.context;
                    ws.registered();
                    // Authenticated sessions no longer compete with startup or asset requests
                    drop(handshake.take());
                    continue;
                }
                let mut hub = hub.lock().unwrap();
                if hub.sessions.tokens.get(&registered_token) != owner.as_ref()
                    || !inspector_browser::origin_allowed(
                        &hub,
                        &registered_token,
                        browser_origin.as_deref(),
                    )
                {
                    break;
                }
                if manager_extension(message["event"].as_str().unwrap_or(""))
                    && !hub.capabilities.contains_key(owner.as_ref().unwrap())
                {
                    hub.issue(
                        owner.as_ref().unwrap(),
                        "protocol",
                        "Manager extension was not negotiated",
                    );
                    continue;
                }
                if message["event"] == "streamDockManagerRpcRequest" {
                    if message["protocol_id"] == theme::THEME_RPC {
                        let response = hub.theme_rpc(
                            owner.as_deref().unwrap(),
                            &message,
                            inspector_context.is_some(),
                        );
                        drop(hub);
                        if ws
                            .send(tungstenite::Message::Text(response.to_string().into()))
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    if matches!(
                        message["protocol_id"].as_str(),
                        Some(
                            permission_rpc::PROTOCOL
                                | permission_rpc::BROKER
                                | vault_references::PROTOCOL
                        )
                    ) {
                        let admitted = hub.vault_budget.admit(owner.as_deref().unwrap(), &message);
                        let session = if inspector_context.is_none()
                            && browser_origin.is_none()
                            && hub.enabled.contains(owner.as_ref().unwrap())
                        {
                            hub.permission_sessions.get(&registered_token).cloned()
                        } else {
                            None
                        };
                        let lease = hub
                            .sessions
                            .storage_sessions
                            .get(&registered_token)
                            .cloned();
                        let store = hub.store.clone();
                        let workflow = hub.workflow.clone();
                        drop(hub);
                        if !store.is_some_and(|store| {
                            permission_rpc::dispatch(
                                &workflow,
                                &store,
                                session.as_ref(),
                                lease.as_ref(),
                                (&message, text.len()),
                                admitted,
                                |reply| {
                                    ws.send(tungstenite::Message::text(reply.to_string()))
                                        .is_ok()
                                },
                            )
                        }) {
                            break;
                        }
                        continue;
                    }
                    if message["protocol_id"] == vault_writes::PROTOCOL {
                        let admitted = hub.vault_budget.admit(owner.as_deref().unwrap(), &message);
                        let session = if inspector_context.is_none()
                            && browser_origin.is_none()
                            && hub.enabled.contains(owner.as_ref().unwrap())
                        {
                            hub.vault_sessions.get(&registered_token).cloned()
                        } else {
                            None
                        };
                        let lease = hub
                            .sessions
                            .storage_sessions
                            .get(&registered_token)
                            .cloned();
                        let reader = hub.vault_reader.clone();
                        drop(hub);
                        if !vault_writes::dispatch(
                            reader.as_deref(),
                            session.as_ref(),
                            lease.as_deref(),
                            message,
                            text.len(),
                            admitted,
                            |reply| {
                                ws.send(tungstenite::Message::text(reply.to_string()))
                                    .is_ok()
                            },
                        ) {
                            break;
                        }
                        continue;
                    }
                    if matches!(
                        message["protocol_id"].as_str(),
                        Some(vault_rpc::VALUES | vault_rpc::SECRETS | vault_text::PROTOCOL)
                    ) {
                        let admitted = hub.vault_budget.admit(owner.as_deref().unwrap(), &message);
                        let reader = hub.vault_reader.clone();
                        let scope = if inspector_context.is_none()
                            && browser_origin.is_none()
                            && hub.enabled.contains(owner.as_ref().unwrap())
                        {
                            hub.vault_sessions.get(&registered_token).cloned()
                        } else {
                            None
                        };
                        let alive = hub
                            .sessions
                            .storage_sessions
                            .get(&registered_token)
                            .cloned();
                        drop(hub);
                        if message["protocol_id"] == vault_text::PROTOCOL {
                            if !vault_text::dispatch(
                                reader.as_deref(),
                                scope.as_ref(),
                                alive.as_deref(),
                                &message,
                                text.len(),
                                admitted,
                                |reply| ws.send(tungstenite::Message::Text(reply.into())).is_ok(),
                            ) {
                                break;
                            }
                            continue;
                        }
                        if !vault_rpc::dispatch(
                            reader.as_deref(),
                            scope.as_ref(),
                            alive.as_deref(),
                            &message,
                            text.len(),
                            admitted,
                            |reply| ws.send(tungstenite::Message::Text(reply.into())).is_ok(),
                        ) {
                            break;
                        }
                        continue;
                    }
                    let storage = hub.store.clone();
                    let admission = hub.storage_admission.clone();
                    let alive = hub
                        .sessions
                        .storage_sessions
                        .get(&registered_token)
                        .cloned()
                        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
                    drop(hub);
                    let Some(storage) = storage else {
                        break;
                    };
                    let response = storage_rpc::dispatch(
                        storage,
                        admission,
                        alive,
                        owner.as_ref().unwrap().clone(),
                        message,
                        inspector_context.is_some(),
                        text.len(),
                    );
                    if ws
                        .send(tungstenite::Message::Text(response.to_string().into()))
                        .is_err()
                    {
                        break;
                    }
                    continue;
                }
                if let Some(context) = &inspector_context {
                    if !InboundPolicy::classify(message["event"].as_str().unwrap_or(""))
                        .is_some_and(InboundPolicy::inspector_allowed)
                    {
                        break;
                    }
                    message["context"] = json!(context);
                }
                // Logs never enter visual revisions, effect queues, or runtime history
                if message["event"] == "logMessage" {
                    let logs = hub.logs.clone();
                    let id = owner.as_ref().unwrap();
                    let run = hub
                        .run_details
                        .get(id)
                        .and_then(|(_, run)| run.clone())
                        .unwrap_or_default();
                    drop(hub);
                    if let Some(logs) = logs {
                        let source = if message["payload"]["source"] == "network" {
                            "network"
                        } else {
                            "rpc"
                        };
                        if source == "network" && !logs.debug_network(id) {
                            continue;
                        }
                        let stream = if message["payload"]["stream"] == "stderr" {
                            "stderr"
                        } else {
                            "stdout"
                        };
                        let text = message["payload"]["message"].as_str().unwrap_or("");
                        if source == "network" {
                            if let Ok(detail) =
                                serde_json::from_value::<log_capture::LogNetworkDetail>(
                                    message["payload"]["network"].clone(),
                                )
                            {
                                logs.record_network(id, &run, stream, text, detail);
                            } else {
                                logs.record(id, &run, stream, source, text);
                            }
                        } else {
                            logs.record(id, &run, stream, source, text);
                        }
                        if message["payload"]["incomplete"] == true {
                            logs.mark_incomplete(
                                id,
                                "SDK logging queue overloaded; capture is incomplete.",
                            );
                        }
                    }
                    continue;
                }
                if message["event"] == "desktopInput" {
                    if inspector_context.is_none() && !hub.enabled.contains(owner.as_ref().unwrap())
                    {
                        continue;
                    }
                    if ![
                        "shortcut",
                        "press",
                        "release",
                        "copy",
                        "paste",
                        "captureTarget",
                        "clearTarget",
                    ]
                    .contains(&message["payload"]["op"].as_str().unwrap_or(""))
                    {
                        break;
                    }
                    if inspector_context.is_some()
                        || !hub
                            .capabilities
                            .get(owner.as_ref().unwrap())
                            .is_some_and(|c| c.input_protocol == 1)
                        || message["payload"].to_string().len() > 256 * 1024
                    {
                        break;
                    }
                }
                // Completion belongs to the original request, even if its page
                // has since disappeared. The receipt checks owner/session/context;
                // unknown and expired replies are harmless and must not kill a plugin.
                if message["event"] == "networkPayload" {
                    if inspector_context.is_some() {
                        break;
                    }
                    let run_id = hub
                        .run_details
                        .get(owner.as_ref().unwrap())
                        .and_then(|(_, run)| run.clone())
                        .unwrap_or_default();
                    if let Err(error) = hub.report_network(
                        owner.as_ref().unwrap(),
                        &run_id,
                        &message["payload"],
                        std::time::Instant::now(),
                    ) {
                        hub.issue(owner.as_ref().unwrap(), "protocol", &error);
                        break;
                    }
                    continue;
                }
                if message["event"] == "actionResult" {
                    if inspector_context.is_some() {
                        break;
                    }
                    // Consume authenticated correlation fields internally. No
                    // plugin-provided payload text is published by completion.
                    hub.complete_action(owner.as_ref().unwrap(), &registered_token, &message);
                    continue;
                }
                if let Err(error) = validate_message(
                    &message,
                    owner.as_ref().unwrap(),
                    &hub.context_state.contexts,
                ) {
                    let disposition = error.disposition(
                        inspector_context.is_none()
                            && error == InboundError::UnownedContext
                            && hub.discard_retired_response(&registered_token, &message),
                    );
                    if disposition == InboundDisposition::Ignore {
                        continue;
                    }
                    hub.issue(
                        owner.as_ref().unwrap(),
                        "protocol",
                        &format!(
                            "{}: {}",
                            error,
                            message["event"].as_str().unwrap_or("unknown")
                        ),
                    );
                    let health = hub.health_mut(owner.as_deref().unwrap());
                    if error == InboundError::UnsupportedEvent {
                        let event = message["event"]
                            .as_str()
                            .unwrap_or("unknown")
                            .chars()
                            .take(80)
                            .collect::<String>();
                        if !health.unsupported_apis.contains(&event) {
                            health.unsupported_apis.push(event);
                            if health.unsupported_apis.len() > 20 {
                                health.unsupported_apis.remove(0);
                            }
                        }
                    } else {
                        health.recent_failures.push(error.to_string());
                        if health.recent_failures.len() > 20 {
                            health.recent_failures.remove(0);
                        }
                    }
                    if let Some(wake) = &hub.wake {
                        wake();
                    }
                    match disposition {
                        InboundDisposition::Ignore | InboundDisposition::Reject => continue,
                        InboundDisposition::Disconnect => break,
                    }
                }
                if message["event"] == "switchToProfile" {
                    match effects::ProfileSwitchEffect::parse(
                        owner.as_ref().unwrap(),
                        &registered_token,
                        &message,
                    ) {
                        Ok(effect)
                            if hub.enabled.contains(owner.as_ref().unwrap())
                                && hub.effects.len() < 512 =>
                        {
                            let now = std::time::Instant::now();
                            if !hub.devices.contains_key(&effect.device) {
                                continue;
                            }
                            if hub
                                .profile_switch_requests
                                .get(&effect.plugin)
                                .is_some_and(|at| {
                                    now.duration_since(*at) < Duration::from_millis(250)
                                })
                            {
                                continue;
                            }
                            hub.profile_switch_requests
                                .retain(|_, at| now.duration_since(*at) < Duration::from_secs(60));
                            if hub.profile_switch_requests.len() >= 256 {
                                continue;
                            }
                            hub.profile_switch_requests
                                .insert(effect.plugin.clone(), now);
                            hub.effects.retain(|pending| !matches!(pending,HostEffect::SwitchProfile(old) if old.plugin == effect.plugin && old.device == effect.device));
                            if hub
                                .effects
                                .iter()
                                .filter(|effect| matches!(effect, HostEffect::SwitchProfile(_)))
                                .count()
                                >= 16
                            {
                                continue;
                            }
                            hub.effects.push_back(HostEffect::SwitchProfile(effect));
                            if let Some(wake) = &hub.wake {
                                wake();
                            }
                        }
                        Ok(_) => {}
                        Err(error) => hub.issue(owner.as_ref().unwrap(), "protocol", &error),
                    }
                    continue;
                }
                if message["event"] == "subscribeTelemetry" {
                    if inspector_context.is_some() {
                        break;
                    }
                    let Some(topics) = message["payload"]["topics"]
                        .as_array()
                        .filter(|t| t.len() <= 5)
                    else {
                        break;
                    };
                    if topics.iter().any(|t| {
                        t.as_str().is_none_or(|s| {
                            !["cpu", "memory", "network", "disk", "audio"].contains(&s)
                        })
                    }) {
                        break;
                    }
                    if let Some(wake) = &hub.sampler_wake {
                        wake();
                    }
                    hub.telemetry_topics.insert(
                        owner.as_ref().unwrap().clone(),
                        topics
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect(),
                    );
                    continue;
                }
                let context = message["context"].as_str().unwrap_or("").to_string();
                if message["event"] == "setSettings"
                    && hub.capabilities.contains_key(owner.as_ref().unwrap())
                {
                    message["payload"] = hub.durable_settings(&context, &message["payload"]);
                } else if message["event"] != "desktopInput"
                    && hub.capabilities.contains_key(owner.as_ref().unwrap())
                {
                    if let Some(payload) = message.get_mut("payload") {
                        hub.redact_payload(payload);
                    }
                }
                let response_queue = if inspector_context.is_some() {
                    format!("pi:{registered_token}")
                } else {
                    owner.as_ref().unwrap().clone()
                };
                if inspector_context.is_some()
                    && hub.context_state.drafts.contains(&context)
                    && ["setSettings", "setGlobalSettings"]
                        .contains(&message["event"].as_str().unwrap_or(""))
                {
                    if (!message["payload"].is_object()
                        && !hub
                            .manifest_dialects
                            .get(owner.as_ref().unwrap())
                            .is_some_and(|d| d == "openaction"))
                        || message["payload"].to_string().len() > 256 * 1024
                    {
                        break;
                    }
                    if message["event"] == "setGlobalSettings"
                        && hub
                            .configuration_actions
                            .get(&context)
                            .is_some_and(|action| !action.is_empty())
                    {
                        if let Some(draft) = hub.global_drafts.get_mut(&context) {
                            draft.value = message["payload"].clone();
                        }
                    } else {
                        hub.context_state
                            .settings
                            .insert(context.clone(), message["payload"].clone());
                    }
                    hub.sessions
                        .queues
                        .entry(response_queue)
                        .or_default()
                        .push_back(json!({"event":"settingsDraftSaved","context":context}));
                    continue;
                }
                if inspector_context.is_some()
                    && message["event"] == "sendToPlugin"
                    && hub.defer_inspector_request(&registered_token, &message)
                {
                    continue;
                }
                match message["event"].as_str().unwrap_or("") {
                    "getResources" | "getSettings" => {
                        if hub
                            .read_saved_state(
                                owner.as_ref().unwrap(),
                                &registered_token,
                                &context,
                                message["event"] == "getResources",
                                message.get("id"),
                            )
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    "setResources" => {
                        if hub
                            .queue_resources(
                                owner.as_ref().unwrap(),
                                &registered_token,
                                &context,
                                &message["payload"],
                            )
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    "setSettings" => {
                        if (!message["payload"].is_object()
                            && !hub
                                .manifest_dialects
                                .get(owner.as_ref().unwrap())
                                .is_some_and(|d| d == "openaction"))
                            || message["payload"].to_string().len() > 256 * 1024
                        {
                            break;
                        }
                        if hub.context_state.addresses.contains_key(&context)
                            && !hub.context_state.transient_scopes.contains_key(&context)
                        {
                            if hub
                                .queue_saved_settings(
                                    owner.as_ref().unwrap(),
                                    &registered_token,
                                    &context,
                                    message["payload"].clone(),
                                )
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                        // Opaque runtime contexts and temporary scopes never become durable authority
                        hub.context_state
                            .settings
                            .insert(context.clone(), message["payload"].clone());
                        hub.publish_settings(
                            owner.as_ref().unwrap(),
                            &context,
                            &message["payload"],
                            &registered_token,
                        );
                    }
                    "getGlobalSettings" => {
                        let settings = if inspector_context.is_some()
                            && hub.context_state.drafts.contains(&context)
                        {
                            if hub
                                .configuration_actions
                                .get(&context)
                                .is_some_and(String::is_empty)
                            {
                                hub.context_state
                                    .settings
                                    .get(&context)
                                    .cloned()
                                    .unwrap_or(json!({}))
                            } else {
                                hub.global_drafts
                                    .get(&context)
                                    .map(|d| d.value.clone())
                                    .unwrap_or(json!({}))
                            }
                        } else {
                            hub.globals
                                .get(owner.as_ref().unwrap())
                                .cloned()
                                .unwrap_or(json!({}))
                        };
                        let mut response = json!({"event":"didReceiveGlobalSettings","payload":{"settings":settings}});
                        if let Some(id) = message.get("id") {
                            response["id"] = id.clone();
                        }
                        hub.sessions
                            .queues
                            .entry(response_queue)
                            .or_default()
                            .push_back(response);
                        continue;
                    }
                    "setGlobalSettings" => {
                        if (!message["payload"].is_object()
                            && !hub
                                .manifest_dialects
                                .get(owner.as_ref().unwrap())
                                .is_some_and(|d| d == "openaction"))
                            || message["payload"].to_string().len() > 256 * 1024
                        {
                            break;
                        }
                        let previous = hub
                            .globals
                            .insert(owner.as_ref().unwrap().clone(), message["payload"].clone());
                        if hub
                            .persist_scoped(persistence::Scope {
                                globals: &[owner.as_ref().unwrap()],
                                defaults: &[],
                                metadata: &[],
                            })
                            .is_err()
                        {
                            if let Some(previous) = previous {
                                hub.globals
                                    .insert(owner.as_ref().unwrap().clone(), previous);
                            } else {
                                hub.globals.remove(owner.as_ref().unwrap());
                            }
                            break;
                        }
                        let settings = message["payload"].clone();
                        let global_event = json!({"event":"didReceiveGlobalSettings","payload":{"settings":settings}});
                        if inspector_context.is_some()
                            || hub.capabilities.contains_key(owner.as_ref().unwrap())
                        {
                            hub.sessions
                                .queues
                                .entry(owner.as_ref().unwrap().clone())
                                .or_default()
                                .push_back(global_event.clone());
                        }
                        let inspectors: Vec<_> = hub
                            .sessions
                            .files
                            .iter()
                            .filter(|(token, grant)| {
                                token.as_str() != registered_token
                                    && grant.context.is_some()
                                    && hub.sessions.tokens.get(*token) == owner.as_ref()
                            })
                            .map(|(token, _)| format!("pi:{token}"))
                            .collect();
                        for target in inspectors {
                            hub.sessions
                                .queues
                                .entry(target)
                                .or_default()
                                .push_back(global_event.clone());
                        }
                        continue;
                    }
                    "sendToPlugin" => {
                        if inspector_context.is_some() {
                            let Some(live_context) =
                                hub.inspector_live_context(&context, owner.as_ref().unwrap())
                            else {
                                continue;
                            };
                            message["context"] = json!(live_context);
                        }
                        hub.sessions
                            .queues
                            .entry(owner.as_ref().unwrap().clone())
                            .or_default()
                            .push_back(message);
                        continue;
                    }
                    "sendToPropertyInspector" => {
                        hub.route_property_inspector_reply(
                            owner.as_ref().unwrap(),
                            &context,
                            &message,
                        );
                        continue;
                    }
                    _ => {}
                }
                hub.visual_state.visual_generation =
                    hub.visual_state.visual_generation.wrapping_add(1);
                if let Err(error) = hub.prepare_plugin_visual(
                    owner.as_ref().unwrap(),
                    &mut message,
                    std::time::Instant::now(),
                ) {
                    message["event"] = json!("unsupportedFeature");
                    message["payload"] = json!({"message":error});
                }
                hub.visual_state.record_update(&context, &message);
                if message["event"] == "showAlert" {
                    hub.issue(
                        owner.as_ref().unwrap(),
                        "reported",
                        "Plugin reported an action failure",
                    );
                }
                if message["event"] == "unsupportedFeature" {
                    let health = hub.health_mut(owner.as_deref().unwrap());
                    let detail = message["payload"]["message"]
                        .as_str()
                        .unwrap_or("Unsupported plugin operation")
                        .chars()
                        .take(512)
                        .collect::<String>();
                    let target = if detail.contains("image")
                        || detail.contains("file")
                        || detail.contains("asset")
                    {
                        &mut health.missing_assets
                    } else {
                        &mut health.unsupported_apis
                    };
                    if !target.contains(&detail) {
                        target.push(detail.clone());
                        if target.len() > 20 {
                            target.remove(0);
                        }
                    }
                }
                if message["event"] == "unsupportedFeature" {
                    hub.issue(
                        owner.as_ref().unwrap(),
                        "api_or_asset",
                        message["payload"]["message"]
                            .as_str()
                            .unwrap_or("Unsupported plugin operation"),
                    );
                }
                if hub.effects.len() < 512 {
                    let effect = hub.message_effect(
                        owner.as_ref().unwrap(),
                        Some(&registered_token),
                        &message,
                        Some(hub.visual_state.render_generation),
                    );
                    hub.effects.push_back(effect);
                    if let Some(wake) = &hub.wake {
                        wake();
                    }
                }
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
            _ => {}
        }
    }
    if inspector_context.is_some() {
        let mut h = hub.lock().unwrap();
        let key = format!("pi:{registered_token}");
        if h.sessions
            .queues
            .get(&key)
            .and_then(|queue| queue.wake.as_ref())
            .is_some_and(|wake| Arc::ptr_eq(wake, &ws.wake))
        {
            h.sessions.queues.remove(&key);
        }
        h.configuration_sessions.remove(&registered_token);
        h.configuration_announced.remove(&registered_token);
        h.pending_inspectors
            .retain(|pending| pending.token != registered_token);
        if let (Some(owner), Some(context)) = (owner.as_ref(), inspector_context.as_ref()) {
            if let Some(context) = h
                .configuration_actions
                .contains_key(context)
                .then(|| h.inspector_live_context(context, owner))
                .flatten()
            {
                let event = h.inspector_lifecycle_event("propertyInspectorDidDisappear", &context);
                h.sessions
                    .queues
                    .entry(owner.clone())
                    .or_default()
                    .push_back(event);
            }
        }
    }
    if inspector_context.is_none() {
        if let Some(owner) = owner {
            let mut h = hub.lock().unwrap();
            if h.sessions
                .queues
                .get(&owner)
                .and_then(|queue| queue.wake.as_ref())
                .is_some_and(|wake| Arc::ptr_eq(wake, &ws.wake))
            {
                h.sessions.queues.remove(&owner);
            }
            if let Some(logs) = &h.logs {
                logs.record(
                    &owner,
                    h.run_details
                        .get(&owner)
                        .and_then(|(_, run)| run.as_deref())
                        .unwrap_or(""),
                    "stdout",
                    "manager",
                    "Plugin connection closed",
                );
            }
            h.revoke_storage_session(&registered_token);
            h.sessions.connected.remove(&registered_token);
            h.notify_vendor_demand();
            h.sessions.retired_contexts.clear_session(&registered_token);
            h.sessions.shutdowns.remove(&registered_token);
            if let Some(wake) = &h.sampler_wake {
                wake();
            }
            h.action_results
                .retain(|_, pending| pending.session != registered_token);
            h.effects
                .push_back(HostEffect::release_session(&owner, &registered_token));
            if let Some(wake) = &h.wake {
                wake();
            }
        }
    }
    let _ = ws.close(None);
}
// Recognize inbound events belonging to the Manager protocol extension
fn manager_extension(event: &str) -> bool {
    matches!(
        event,
        "streamDockManagerRpcRequest"
            | "desktopInput"
            | "networkPayload"
            | "actionResult"
            | "subscribeTelemetry"
            | "setDynamicItems"
    )
}

// Ordinary event policy is independent of RPC admission and correlated action
// completion. Those paths authenticate their own fields before this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboundPolicy {
    OwnedContext,
    InspectorOwnedContext,
    ContextFree,
    InspectorContextFree,
}
impl InboundPolicy {
    // Classify the event under the host routing policy
    fn classify(event: &str) -> Option<Self> {
        Some(match event {
            "desktopInput"
            | "setTitle"
            | "setImage"
            | "setState"
            | "setFeedback"
            | "setFeedbackLayout"
            | "setTriggerDescription"
            | "showAlert"
            | "showOk"
            | "sendToPropertyInspector" => Self::OwnedContext,
            "setSettings" | "getSettings" | "setResources" | "getResources" | "sendToPlugin" => {
                Self::InspectorOwnedContext
            }
            "subscribeTelemetry" | "setDynamicItems" | "switchToProfile" => Self::ContextFree,
            "setGlobalSettings" | "getGlobalSettings" | "logMessage" | "openUrl" => {
                Self::InspectorContextFree
            }
            _ => return None,
        })
    }
    // Check whether this event is permitted from an inspector
    fn inspector_allowed(self) -> bool {
        matches!(
            self,
            Self::InspectorOwnedContext | Self::InspectorContextFree
        )
    }
    // Check whether this event requires an action context
    fn context_required(self) -> bool {
        matches!(self, Self::OwnedContext | Self::InspectorOwnedContext)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboundError {
    MissingEvent,
    UnsupportedEvent,
    UnownedContext,
    MissingContext,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InboundDisposition {
    Ignore,
    Reject,
    Disconnect,
}
impl InboundError {
    // Choose how the inbound event should be handled
    fn disposition(self, retired_response: bool) -> InboundDisposition {
        match self {
            Self::UnsupportedEvent => InboundDisposition::Reject,
            Self::UnownedContext if retired_response => InboundDisposition::Ignore,
            Self::MissingEvent | Self::UnownedContext | Self::MissingContext => {
                InboundDisposition::Disconnect
            }
        }
    }
}
impl std::fmt::Display for InboundError {
    // Format the value for display
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MissingEvent => "event missing",
            Self::UnsupportedEvent => "unsupported plugin event",
            Self::UnownedContext => "context is not owned by plugin",
            Self::MissingContext => "context required",
        })
    }
}
// Validate message
fn validate_message(
    message: &Value,
    owner: &str,
    contexts: &HashMap<String, String>,
) -> Result<(), InboundError> {
    let event = message["event"]
        .as_str()
        .ok_or(InboundError::MissingEvent)?;
    let policy = InboundPolicy::classify(event).ok_or(InboundError::UnsupportedEvent)?;
    if !policy.context_required() {
        return Ok(());
    }
    let context = message["context"]
        .as_str()
        .ok_or(InboundError::MissingContext)?;
    if contexts.get(context).map(String::as_str) != Some(owner) {
        return Err(InboundError::UnownedContext);
    }
    Ok(())
}
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod transport_tests;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify network log settings require SDK opt in and an active capture
    fn network_log_settings_require_sdk_opt_in_and_an_active_capture() {
        let root = tempfile::tempdir().unwrap();
        let logs = log_capture::LogCaptureService::open(root.path().join("logs.json")).unwrap();
        let mut hub = Hub {
            logs: Some(logs.clone()),
            ..Default::default()
        };
        hub.queue_log_preferences("old-sdk");
        assert!(!hub.sessions.queues.contains_key("old-sdk"));
        hub.network_debug_plugins.insert("updated-sdk".into());
        let mut preferences = logs.preferences();
        preferences
            .plugins
            .entry("updated-sdk".into())
            .or_default()
            .debug_network = true;
        logs.save_preferences(preferences).unwrap();
        hub.queue_log_preferences("updated-sdk");
        assert_eq!(
            hub.sessions
                .queues
                .get_mut("updated-sdk")
                .unwrap()
                .pop_front()
                .unwrap()["payload"]["debug_network"],
            false
        );
        let capture = logs.start("updated-sdk", "Updated SDK").unwrap();
        hub.queue_log_preferences("updated-sdk");
        assert_eq!(
            hub.sessions
                .queues
                .get_mut("updated-sdk")
                .unwrap()
                .pop_front()
                .unwrap()["payload"]["debug_network"],
            true
        );
        logs.stop(&capture.id).unwrap();
        hub.queue_log_preferences("updated-sdk");
        assert_eq!(
            hub.sessions
                .queues
                .get_mut("updated-sdk")
                .unwrap()
                .pop_front()
                .unwrap()["payload"]["debug_network"],
            false
        );
    }
    #[test]
    // Verify render capture shares visual allocations and updates copy on write
    fn render_capture_shares_visual_allocations_and_updates_copy_on_write() {
        let root = tempfile::tempdir().unwrap();
        let host = lifecycle_tests::socket_host(root.path());
        let first_context = "plugin:profile:page:device:key-1";
        let second_context = "plugin:profile:page:device:key-2";
        {
            let mut hub = host.hub.lock().unwrap();
            hub.roots.insert("plugin".into(), root.path().into());
            hub.capabilities.insert("plugin".into(), Default::default());
            for (context, control) in [(first_context, "key-1"), (second_context, "key-2")] {
                hub.context_state
                    .contexts
                    .insert(context.into(), "plugin".into());
                hub.context_state
                    .scopes
                    .insert(context.into(), "profile:page".into());
                hub.context_state
                    .targets
                    .insert(context.into(), ("device".into(), control.into()));
                hub.visual_state.visuals.insert(context.into(), Arc::new(json!({"title":"old", "image":"x".repeat(1024*1024), "layout":null,"feedback":{}})));
            }
        }
        let requests = [
            ("key-1".into(), "profile:page".into()),
            ("key-2".into(), "profile:page".into()),
        ];
        let (first, ()) = host.render_surfaces("device", &requests, &[], || ());
        let (second, ()) = host.render_surfaces("device", &requests, &[], || ());
        let old = Arc::downgrade(&first.values["key-1"]);
        assert!(Arc::ptr_eq(&first.values["key-1"], &second.values["key-1"]));
        assert!(!Arc::ptr_eq(&first.values["key-1"], &first.values["key-2"]));
        {
            let mut hub = host.hub.lock().unwrap();
            assert!(Arc::ptr_eq(
                &first.values["key-1"],
                &hub.visual_state.visuals[first_context]
            ));
            visual::refresh_themes(&mut hub);
            assert!(
                Arc::ptr_eq(
                    &first.values["key-1"],
                    &hub.visual_state.visuals[first_context]
                ),
                "Unthemed refresh must not copy a shared visual"
            );
            assert_eq!(hub.visual_state.visual_cow_copies, 0);
            let mut message =
                json!({"event":"setTitle","context":first_context,"payload":{"title":"new"}});
            visual::prepare(&mut hub, "plugin", &mut message).unwrap();
            assert_eq!(hub.visual_state.visuals[first_context]["title"], "new");
            assert_eq!(first.values["key-1"]["title"], "old");
            assert_eq!(first.values["key-2"]["title"], "old");
            assert!(!Arc::ptr_eq(
                &first.values["key-1"],
                &hub.visual_state.visuals[first_context]
            ));
            assert_eq!(hub.visual_state.visual_cow_copies, 1);
        }
        assert!(old.upgrade().is_some());
        drop(first);
        drop(second);
        assert!(
            old.upgrade().is_none(),
            "No registry retains expired scene generations"
        );
        assert_eq!(host.visual_storage_stats()["shared_captures"], 2);
    }

    #[test]
    // Verify render snapshot has one coherent hub point under sustained visual updates
    fn render_snapshot_has_one_coherent_hub_point_under_sustained_visual_updates() {
        let root = tempfile::tempdir().unwrap();
        let host = lifecycle_tests::socket_host(root.path());
        let context = "plugin:profile:page:device:key";
        {
            let mut hub = host.hub.lock().unwrap();
            hub.context_state
                .contexts
                .insert(context.into(), "plugin".into());
            hub.context_state
                .scopes
                .insert(context.into(), "profile:page".into());
            hub.context_state
                .targets
                .insert(context.into(), ("device".into(), "key".into()));
            hub.visual_state
                .visuals
                .insert(context.into(), Arc::new(json!({"title":0,"state":0})));
        }
        let shared = host.hub.clone();
        let producer = std::thread::spawn(move || {
            for i in 1..10_000 {
                let mut hub = shared.lock().unwrap();
                hub.visual_state
                    .visuals
                    .insert(context.into(), Arc::new(json!({"title":i,"state":i})));
                hub.visual_state.visual_generation =
                    hub.visual_state.visual_generation.wrapping_add(1);
                drop(hub);
                std::thread::yield_now();
            }
        });
        for _ in 0..100 {
            let (snapshot, ()) = host.render_surfaces(
                "device",
                &[("key".into(), "profile:page".into())],
                &[],
                || {
                    assert!(
                        host.hub.try_lock().is_err(),
                        "Leaf snapshot shares the visual capture lock"
                    );
                },
            );
            assert_eq!(
                snapshot.values["key"]["title"],
                snapshot.values["key"]["state"]
            );
            assert_eq!(snapshot.contexts["key"], context);
            assert_eq!(
                snapshot.generation,
                host.render_generation(),
                "Soft updates must not invalidate routing generation"
            );
        }
        producer.join().unwrap();
    }
    #[test]
    // Verify controller metadata groups buttons dials and touch without guessing unknown types
    fn controller_metadata_groups_buttons_dials_and_touch_without_guessing_unknown_types() {
        assert_eq!(action_control_types(&json!({})), ["button"]);
        assert_eq!(
            action_control_types(&json!({"Controllers":["Encoder"]})),
            ["encoder", "touch"]
        );
        assert_eq!(
            action_control_types(&json!({"Controllers":["Keypad","Information","Knob"]})),
            ["button", "encoder", "touch"]
        );
        assert!(action_control_types(&json!({"Controllers":["Neo"]})).is_empty());
    }
    #[test]
    // Verify context isolation
    fn context_isolation() {
        let contexts = HashMap::from([("a".into(), "plugin-a".into())]);
        assert!(validate_message(
            &json!({"event":"setTitle","context":"a","payload":{"title":"hello"}}),
            "plugin-b",
            &contexts
        )
        .is_err());
        assert!(validate_message(
            &json!({"event":"setTitle","context":"a"}),
            "plugin-a",
            &contexts
        )
        .is_ok());
    }
    #[test]
    // Verify archive traversal is rejected
    fn archive_traversal_is_rejected() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&root).unwrap();
        let archive = root.join("evil.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        zip.start_file("../escape", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"unsafe").unwrap();
        zip.finish().unwrap();
        assert!(extract_archive(&archive, &root.join("out")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod display_scope_tests {
    use super::*;
    #[test]
    // Verify plugin state uses the registered action for a nonvisible context
    fn plugin_state_uses_the_registered_action_for_a_nonvisible_context() {
        let root = tempfile::tempdir().unwrap();
        let mut hub = Hub::default();
        hub.roots.insert("plugin".into(), root.path().into());
        hub.context_state
            .context_actions
            .insert("macro-step".into(), "counter".into());
        hub.metadata.insert(
            "plugin".into(),
            Arc::new(HashMap::from([(
                "counter".into(),
                json!({"States":[{"Title":"Even"},{"Title":"Odd"}]}),
            )])),
        );
        let mut message = json!({"event":"setState","context":"macro-step","payload":{"state":1}});
        visual::prepare(&mut hub, "plugin", &mut message).unwrap();
        assert_eq!(
            hub.visual_state.visuals["macro-step"]["action_id"],
            "counter"
        );
        assert_eq!(hub.visual_state.visuals["macro-step"]["title"], "Odd");
        assert!(hub.context_state.visible.is_empty());
    }

    #[test]
    // Verify invalid plugin state does not change the style state
    fn invalid_plugin_state_does_not_change_the_style_state() {
        let root = tempfile::tempdir().unwrap();
        let mut hub = Hub::default();
        hub.roots.insert("plugin".into(), root.path().into());
        hub.context_state
            .visible
            .insert("context".into(), json!({"action":"counter"}));
        hub.metadata.insert(
            "plugin".into(),
            Arc::new(HashMap::from([(
                "counter".into(),
                json!({"States":[{"Title":"Even"},{"Title":"Odd"}]}),
            )])),
        );
        let mut message = json!({"event":"setState","context":"context","payload":{"state":1}});
        visual::prepare(&mut hub, "plugin", &mut message).unwrap();
        assert_eq!(hub.visual_state.visuals["context"]["state"], 1);
        message["payload"]["state"] = json!(99);
        assert!(visual::prepare(&mut hub, "plugin", &mut message).is_err());
        assert_eq!(hub.visual_state.visuals["context"]["state"], 1);
    }
    #[test]
    // Verify preview surface requires the exact context action and saved settings
    fn preview_surface_requires_the_exact_context_action_and_saved_settings() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::socket_host(root.path());
        let context =
            host.hub
                .lock()
                .unwrap()
                .binding_context("plugin", "profile:page", "device", "key");
        let settings = json!({"source":"one"});
        {
            let mut hub = host.hub.lock().unwrap();
            hub.visual_state.render_generation = hub.visual_state.render_generation.wrapping_add(1);
            hub.context_state
                .contexts
                .insert(context.clone(), "plugin".into());
            hub.context_state
                .visible
                .insert(context.clone(), json!({"action":"action"}));
            hub.context_state
                .configured
                .insert(context.clone(), settings.clone());
            hub.visual_state
                .visuals
                .insert(context.clone(), Arc::new(json!({"title":"Current"})));
        }
        assert_eq!(
            host.matching_surface(
                "device",
                "key",
                "profile:page",
                "plugin",
                "action",
                &settings
            )
            .unwrap()["title"],
            "Current"
        );
        for (device, control, scope, plugin, action, settings) in [
            (
                "other",
                "key",
                "profile:page",
                "plugin",
                "action",
                settings.clone(),
            ),
            (
                "device",
                "other",
                "profile:page",
                "plugin",
                "action",
                settings.clone(),
            ),
            (
                "device",
                "key",
                "other:page",
                "plugin",
                "action",
                settings.clone(),
            ),
            (
                "device",
                "key",
                "profile:page",
                "other",
                "action",
                settings.clone(),
            ),
            (
                "device",
                "key",
                "profile:page",
                "plugin",
                "other",
                settings.clone(),
            ),
            (
                "device",
                "key",
                "profile:page",
                "plugin",
                "action",
                json!({"source":"changed draft"}),
            ),
        ] {
            assert!(host
                .matching_surface(device, control, scope, plugin, action, &settings)
                .is_none());
        }
        assert!(host.drain_effects().is_empty());
    }
}

#[cfg(test)]
mod temporary_context_tests {
    use super::*;
    #[test]
    // Verify debug settings never persist and retirement keeps other owners
    fn debug_settings_never_persist_and_retirement_keeps_other_owners() {
        let root = tempfile::tempdir().unwrap();
        let mut host = lifecycle_tests::socket_host(root.path());
        let id = uuid::Uuid::new_v4().to_string();
        let scope = format!("profile:page:debug:{id}@0");
        let context = host
            .hub
            .lock()
            .unwrap()
            .binding_context("plugin", &scope, "device", "key-1");
        host.begin_temporary_context("plugin", &scope, "device", "key-1")
            .unwrap();
        host.set_execution_settings(
            "plugin",
            &scope,
            "device",
            "key-1",
            &json!({"token":"secret-token"}),
            &json!({"token":"secret-token"}),
            vec!["secret-token".into()],
        );
        {
            let mut hub = host.hub.lock().unwrap();
            hub.visual_state.render_generation = hub.visual_state.render_generation.wrapping_add(1);
            hub.context_state
                .contexts
                .insert(context.clone(), "plugin".into());
            hub.visual_state.render_generation = hub.visual_state.render_generation.wrapping_add(1);
            hub.context_state
                .scopes
                .insert(context.clone(), scope.clone());
            hub.context_state
                .settings
                .insert("ordinary".into(), json!({"keep":true}));
            hub.persist().unwrap();
        }
        let saved = Hub::load(host.storage()).unwrap();
        assert!(saved.context_state.settings.is_empty());
        assert!(host.clear_action_scope_suffix(":debug:invalid").is_empty());
        assert_eq!(
            host.clear_action_scope_suffix(&format!(":debug:{id}"))
                .len(),
            1
        );
        let hub = host.hub.lock().unwrap();
        assert!(!hub.context_state.contexts.contains_key(&context));
        assert!(!hub.context_state.settings.contains_key(&context));
        assert!(!hub.context_state.execution.contains_key(&context));
        assert!(hub.context_state.transient_scopes.is_empty());
        assert_eq!(hub.context_state.settings["ordinary"]["keep"], true);
    }
}
