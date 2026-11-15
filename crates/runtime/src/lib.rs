mod action_fields;
#[cfg(test)]
mod action_fields_tests;
pub mod actions;
pub mod application_data;
pub mod archive_import;
pub mod artwork;
mod asset_leases;
mod assignment_target;
mod audio_observer;
pub mod automation;
pub mod automation_application;
pub mod benchmark;
pub mod boot_logo;
pub mod builtins;
pub mod cache;
#[cfg(test)]
mod codec_compatibility_tests;
pub mod configuration_recovery;
#[cfg(test)]
mod configuration_tests;
#[cfg(target_os = "macos")]
mod core_audio;
pub mod credentials;
pub mod desktop;
mod desktop_input;
mod device_contract;
mod device_mailbox;
pub mod devices;
pub mod diagnostics;
pub mod dial_response;
mod discovery_readiness;
mod editor_retirement;
pub mod encoding;
pub mod export_review;
#[cfg(test)]
mod feature_tests;
pub mod feedback;
pub mod firmware;
pub mod hardware_preview;
pub mod history_compare;
#[cfg(test)]
mod icon_toggle_tests;
pub mod icons;
pub mod import_recipes;
pub mod importing;
pub mod keyboard;
pub mod keyboard_device;
mod keyboard_layout;
pub mod keyboard_macro;
mod keyboard_navigation;
pub mod keyboard_preview;
#[cfg(test)]
mod layered_composition_probe;
pub mod library_organization;
mod locate;
pub mod macro_debug;
pub mod macro_inputs;
#[cfg(test)]
mod macro_inputs_tests;
pub mod macro_sharing;
pub mod manager_resources;
pub mod media;
mod media_budget;
#[cfg(test)]
mod media_budget_tests;
pub mod media_cleanup;
pub mod media_deletion;
pub mod media_library;
mod media_memory;
pub mod media_optimization;
pub mod metrics;
mod native_pool;
mod native_process;
pub mod onboarding;
pub mod parameter_credentials;
#[cfg(test)]
mod parameter_credentials_tests;
pub mod parameters;
mod plugin_monitor;
pub mod plugin_packages;
pub mod plugin_policy;
mod plugin_profile_transactions;
pub mod plugin_profiles;
pub mod plugin_rollback;
mod plugin_settings_persistence;
mod plugin_styles;
pub mod power_policy;
pub mod preparation;
pub mod preview;
pub mod preview_subscriptions;
mod process_resources;
pub mod profile_conversion;
mod profile_database;
mod profile_versions;
pub mod profiles;
mod render_scene;
pub mod resource_alerts;
mod review_store;
mod sampler;
pub mod setup_capabilities;
#[cfg(test)]
mod simulator_tests;
pub mod startup_permissions;
mod substitution;
pub mod telemetry;
#[cfg(test)]
mod test_plugin;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
mod text_checks;
mod theme_wallpaper;
pub mod touch_panel;
mod transfer_receipt;
mod transition_input;
pub mod updates;
pub mod validation;
mod vendor_animation;
mod vendor_os_events;
mod vendor_profile_archive;
mod vendor_profile_files;
pub mod vendor_profiles;
mod vendor_resources;
pub mod video;
pub mod video_recordings;
#[cfg(target_os = "linux")]
pub mod wayland;
pub mod workspace;
pub mod workspace_stream;
use hardware_core::{ControlEvent, ModelManifest};
pub use profiles::*;
use serde::{Deserialize, Serialize};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct DeviceInfo {
    pub id: String,
    pub persistence_key: String,
    pub identity_port_dependent: bool,
    pub model_id: String,
    pub name: String,
    pub variant_id: String,
    pub firmware: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usb_release: Option<String>,
    pub connected: bool,
    pub simulated: bool,
    pub manifest: ModelManifest,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct Snapshot {
    #[serde(default)]
    #[ts(
        optional,
        as = "Option<std::collections::BTreeMap<String,std::collections::BTreeMap<String,builtins::EffectiveBinding>>>"
    )]
    pub binding_overrides: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, builtins::EffectiveBinding>,
    >,
    #[ts(type = "number")]
    pub revision: u64,
    #[ts(type = "number")]
    pub configuration_revision: u64,
    pub sessions: std::collections::BTreeMap<String, workspace::DeviceSession>,
    #[serde(default)]
    pub saved_macros: Vec<builtins::SavedMacro>,
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<workspace::AppearancePreset>>")]
    pub styles: Vec<workspace::AppearancePreset>,
    pub rules: Vec<workspace::AppRule>,
    pub history: workspace::HistoryIndex,
    pub devices: Vec<DeviceInfo>,
    pub profiles: Vec<Profile>,
    pub active_profile: String,
    pub plugins: Vec<stream_dock_manager_plugin_host::PluginInfo>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct RuntimeEvent {
    #[serde(default)]
    #[ts(optional, type = "number | null")]
    pub timestamp_ms: Option<u64>,
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub revision: u64,
    pub kind: String,
    pub device_id: Option<String>,
    pub control: Option<ControlEvent>,
    pub message: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct SurfacePreview {
    #[serde(default)]
    #[ts(optional, as = "Option<f64>")]
    pub configuration_revision: u64,
    pub surfaces: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub trigger_descriptions:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    pub background: Option<String>,
    pub animated: bool,
}
pub struct Runtime {
    discovery_readiness: discovery_readiness::Readiness,
    profile_theme_sync: Mutex<()>,
    hardware_previews: Mutex<hardware_preview::Previews>,
    keyboard_previews: Mutex<keyboard_preview::Previews>,
    keyboard_observations: keyboard_device::Observations,
    locate: Mutex<locate::Locate>,
    dial_responses: Mutex<dial_response::Responses>,
    pub(crate) settings_writer: Mutex<Option<std::thread::JoinHandle<()>>>,
    manager_resources: Mutex<manager_resources::Sampler>,
    process_resources: Arc<Mutex<process_resources::Sampler>>,
    pub plugin_logs: Arc<stream_dock_manager_plugin_host::log_capture::LogCaptureService>,
    boot_logos: boot_logo::BootLogos,
    pub(crate) benchmark: benchmark::BenchmarkService,
    pub(crate) setup_capabilities: setup_capabilities::SetupCapabilities,
    pub(crate) credentials: credentials::Credentials,
    sampling_signal: Arc<sampler::Signal>,
    preparations: preparation::Preparations,
    pub(crate) video: video::VideoService,
    telemetry: Mutex<telemetry::TelemetrySnapshot>,
    pub builtin_state: builtins::State,
    pub updates: Arc<updates::UpdateService>,
    cache_maintenance: cache::Maintenance,
    pub icons: icons::IconStore,
    pub(crate) plugin_rollbacks: Mutex<review_store::ReviewStore<plugin_rollback::StagedRollback>>,
    pub(crate) plugin_packages: Mutex<review_store::ReviewStore<plugin_packages::StagedPackage>>,
    media_start: std::time::Instant,
    vendor_animations: Mutex<vendor_animation::Registry>,
    plugin_monitor_visible: std::sync::atomic::AtomicBool,
    plugin_monitor_consumers: Mutex<std::collections::HashSet<String>>,
    pub(crate) power: power_policy::Controller,
    pub(crate) audio_signal: Arc<audio_observer::Signal>,
    pub(crate) feedback_state: Mutex<feedback::FeedbackState>,
    pub(crate) foreground_title: Mutex<Option<String>>,
    pub(crate) exports: Mutex<review_store::ReviewStore<export_review::StagedExport>>,
    pub(crate) imports: Mutex<review_store::ReviewStore<importing::StagedImport>>,
    pub(crate) vendor_profile_switches: Mutex<vendor_profiles::SwitchRequests>,
    pub(crate) vendor_profile_reviews: Mutex<vendor_profiles::Reviews>,
    pub(crate) plugin_profile_imports:
        Mutex<review_store::ReviewStore<plugin_profiles::StagedPluginProfile>>,
    macro_imports: Mutex<review_store::ReviewStore<macro_sharing::StagedMacroImport>>,
    desktop_settings: Mutex<desktop::DesktopSettings>,
    config_root: std::path::PathBuf,
    restarted: bool,
    pub(crate) library_preferences: Mutex<()>,
    pub(crate) plugin_preferences: Mutex<()>,
    pub(crate) resource_alerts: Mutex<resource_alerts::ResourceAlerts>,
    pub(crate) repair_preferences: Mutex<()>,
    event_sequence: std::sync::atomic::AtomicU64,
    next_simulator_epoch: std::sync::atomic::AtomicU64,
    pub(crate) firmware: firmware::Firmware,
    pub(crate) transition_inputs: Mutex<transition_input::Transitions>,
    pub(crate) connection_epochs: Mutex<std::collections::HashMap<String, u64>>,
    pub(crate) state: Mutex<Snapshot>,
    workspace_stream: Mutex<workspace_stream::WorkspaceStream>,
    pub workspace_cancellations:
        Mutex<std::collections::HashMap<String, Arc<std::sync::atomic::AtomicBool>>>,
    pub(crate) subscribers: Mutex<Vec<mpsc::SyncSender<RuntimeEvent>>>,
    pub(crate) profiles: ProfileStore,
    conversions: Mutex<review_store::ReviewStore<profile_conversion::StagedConversion>>,
    media_selections: Mutex<std::collections::BTreeMap<String, asset_leases::AssetLease>>,
    media_cleanups: Mutex<review_store::ReviewStore<media_cleanup::StagedMediaCleanup>>,
    media_deletions: Mutex<review_store::ReviewStore<media_deletion::StagedMediaDeletion>>,
    pub metrics: metrics::Metrics,
    pub preview_subscriptions: preview_subscriptions::PreviewSubscriptions,
    pub drafts: Mutex<std::collections::BTreeMap<String, workspace::Draft>>,
    pub automation_status: Mutex<automation::AutomationStatus>,
    pub(crate) device_admission: startup_permissions::DeviceAdmission,
    pub(crate) device_wake_pending: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) stopping: std::sync::atomic::AtomicBool,
    pub(crate) device_commands: device_mailbox::Sender,
    pub(crate) action_sender: mpsc::SyncSender<actions::Request>,
    pub(crate) cancellation: Arc<std::sync::atomic::AtomicU64>,
    pub runtimes: Arc<stream_dock_manager_plugin_runtimes::RuntimeService>,
    pub plugins: Mutex<stream_dock_manager_plugin_host::PluginHost>,
}
// Read a millisecond UNIX timestamp for runtime events
fn event_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Runtime {
    // Construct recovery
    pub fn new_recovery(root: std::path::PathBuf, recovery: bool) -> Result<Arc<Self>, String> {
        Self::build_credentials_recovery(root, true, Default::default(), recovery)
    }
    // Construct recovery with registration environment
    pub fn new_recovery_with_registration_environment(
        root: std::path::PathBuf,
        recovery: bool,
        environment: stream_dock_manager_plugin_host::RegistrationEnvironment,
    ) -> Result<Arc<Self>, String> {
        Self::build_credentials_recovery_environment(
            root,
            true,
            Default::default(),
            recovery,
            Some(environment),
        )
    }
    // Initialize src state from the supplied inputs
    pub fn new(root: std::path::PathBuf) -> Result<Arc<Self>, String> {
        Self::build(root, true)
    }
    // Build src from the configured inputs
    fn build(root: std::path::PathBuf, hardware: bool) -> Result<Arc<Self>, String> {
        Self::build_credentials(root, hardware, Default::default())
    }
    // Build credentials
    fn build_credentials(
        root: std::path::PathBuf,
        hardware: bool,
        credentials: credentials::Credentials,
    ) -> Result<Arc<Self>, String> {
        Self::build_credentials_recovery(root, hardware, credentials, false)
    }
    // Build credentials recovery
    fn build_credentials_recovery(
        root: std::path::PathBuf,
        hardware: bool,
        credentials: credentials::Credentials,
        recovery: bool,
    ) -> Result<Arc<Self>, String> {
        Self::build_credentials_recovery_environment(root, hardware, credentials, recovery, None)
    }
    // Build credentials recovery environment
    fn build_credentials_recovery_environment(
        root: std::path::PathBuf,
        hardware: bool,
        credentials: credentials::Credentials,
        recovery: bool,
        registration_environment: Option<stream_dock_manager_plugin_host::RegistrationEnvironment>,
    ) -> Result<Arc<Self>, String> {
        let require_permission_setup =
            cfg!(target_os = "macos") && registration_environment.is_some();
        let storage = stream_dock_manager_storage::Storage::open_validated(
            root.clone(),
            crate::application_data::validate_snapshot,
        )?;
        let requested = crate::profile_database::LocalPreference::RestartRecoveryOnce
            .record::<bool>()
            .read(&storage)?;
        let new_install = storage
            .read_json::<serde_json::Value>(
                stream_dock_manager_storage::Domain::Workspace,
                "current",
                16 * 1024 * 1024,
            )?
            .is_none();
        let profiles = ProfileStore::with_storage(root.join("profiles"), storage.clone())?;
        let data = profiles.load()?;
        let sessions = profiles
            .read_preference(crate::profile_database::LocalPreference::Sessions)?
            .unwrap_or_default();
        keyboard::validate_sessions(&sessions)?;
        let (device_commands, device_receiver) = device_mailbox::channel();
        let (action_sender, action_receiver) = mpsc::sync_channel(128);
        let downloads = Arc::new(stream_dock_manager_downloads::DownloadCache::open(
            root.join("download-cache"),
        )?);
        let runtimes = stream_dock_manager_plugin_runtimes::RuntimeService::open_with_storage(
            root.join("runtimes"),
            downloads.clone(),
            storage.clone(),
        )?;
        if hardware {
            runtimes.start_daily();
        }
        let mut plugins = stream_dock_manager_plugin_host::PluginHost::with_storage(
            root.join("plugins"),
            runtimes.clone(),
            storage.clone(),
        )?;
        plugins.set_registration_environment(match registration_environment {
            Some(environment) => environment,
            None => stream_dock_manager_plugin_host::RegistrationEnvironment::new(
                Some(desktop::application_language()),
                None,
            )?,
        });
        if requested.unwrap_or(recovery) {
            plugins.enter_recovery();
        }
        let plugin_logs = plugins.logs.clone();
        let mut desktop_settings = desktop::SETTINGS_RECORD.read(&storage)?.unwrap_or_default();
        desktop_settings
            .onboarding
            .get_or_insert(onboarding::SetupProgress {
                new_install,
                ..Default::default()
            });
        let updates = updates::UpdateService::open_with_storage(
            root.join("updates"),
            downloads.clone(),
            credentials.provider.clone(),
            storage.clone(),
        )?;
        if hardware {
            updates.start_daily();
        }
        let process_resources = Arc::new(Mutex::new(process_resources::Sampler::default()));
        let runtime = Arc::new(Self {
            discovery_readiness: Default::default(),
            profile_theme_sync: Mutex::new(()),
            settings_writer: Default::default(),
            boot_logos: Default::default(),
            benchmark: Default::default(),
            credentials,
            plugin_logs,
            library_preferences: Default::default(),
            plugin_preferences: Default::default(),
            resource_alerts: Mutex::new(resource_alerts::ResourceAlerts::with_storage(
                storage.clone(),
            )?),
            repair_preferences: Default::default(),
            sampling_signal: Default::default(),
            manager_resources: Mutex::new(manager_resources::Sampler::new(
                process_resources.clone(),
            )),
            process_resources,
            icons: icons::IconStore::new(root.join("icons")),
            preparations: Default::default(),
            video: video::VideoService::with_budget(profiles.media_budget.clone()),
            telemetry: Default::default(),
            builtin_state: builtins::State::with_storage(storage.clone())?,
            updates,
            cache_maintenance: cache::Maintenance::new(downloads),
            plugin_packages: Default::default(),
            plugin_rollbacks: Default::default(),
            hardware_previews: Mutex::new(Default::default()),
            keyboard_previews: Default::default(),
            locate: Mutex::new(Default::default()),
            dial_responses: Mutex::new(Default::default()),
            media_start: std::time::Instant::now(),
            vendor_animations: Default::default(),
            plugin_monitor_visible: std::sync::atomic::AtomicBool::new(false),
            plugin_monitor_consumers: Default::default(),
            power: power_policy::Controller::with_storage(storage.clone())?,
            audio_signal: Default::default(),
            setup_capabilities: Default::default(),
            feedback_state: Default::default(),
            foreground_title: Default::default(),
            imports: Default::default(),
            vendor_profile_switches: Default::default(),
            vendor_profile_reviews: Default::default(),
            plugin_profile_imports: Default::default(),
            exports: Default::default(),
            conversions: Default::default(),
            media_selections: Default::default(),
            media_cleanups: Default::default(),
            media_deletions: Default::default(),
            macro_imports: Default::default(),
            desktop_settings: Mutex::new(desktop_settings),
            config_root: root.clone(),
            restarted: requested.is_some(),
            event_sequence: std::sync::atomic::AtomicU64::new(0),
            next_simulator_epoch: std::sync::atomic::AtomicU64::new(1),
            firmware: Default::default(),
            keyboard_observations: Default::default(),
            transition_inputs: Default::default(),
            connection_epochs: Mutex::new(std::collections::HashMap::new()),
            state: Mutex::new(Snapshot {
                binding_overrides: Default::default(),
                revision: 1,
                configuration_revision: data.configuration_revision,
                sessions,

                saved_macros: data.saved_macros,

                styles: data.styles,
                rules: data.rules,
                history: data.history,
                devices: vec![],
                profiles: data.profiles,
                active_profile: data.active_profile,
                plugins: plugins.list(),
            }),
            subscribers: Mutex::new(vec![]),
            workspace_stream: Default::default(),
            workspace_cancellations: Default::default(),
            drafts: Mutex::new(profiles.load_media_drafts()?),
            profiles,
            metrics: Default::default(),
            preview_subscriptions: Default::default(),
            automation_status: Mutex::new(automation::status()),
            device_admission: startup_permissions::DeviceAdmission::new(!require_permission_setup),
            device_wake_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            stopping: std::sync::atomic::AtomicBool::new(false),
            device_commands,
            action_sender,
            cancellation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            runtimes,
            plugins: Mutex::new(plugins),
        });
        // Consume one-shot routing only after every fallible runtime constructor
        // and configuration/session validator has succeeded
        take_restart_recovery_request(&runtime.profiles.storage)?;
        runtime.refresh_profile_palettes();
        let signal = runtime.sampling_signal.clone();
        Self::start_settings_writer(&runtime);
        let audio_signal = runtime.audio_signal.clone();
        runtime
            .plugins
            .lock()
            .unwrap()
            .set_sampler_waker(Arc::new(move || {
                signal.notify();
                audio_signal.demand_changed();
            }));
        let sender = runtime.device_commands.clone();
        let pending = runtime.device_wake_pending.clone();
        runtime.plugins.lock().unwrap().set_waker(Arc::new(move || {
            if !pending.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let _ = sender.send(devices::Request::Wake);
            }
        }));
        if hardware {
            cache::start(Arc::downgrade(&runtime));
            plugin_monitor::start(Arc::downgrade(&runtime));
            automation::start(Arc::downgrade(&runtime));
            vendor_os_events::start(Arc::downgrade(&runtime));
            power_policy::start(Arc::downgrade(&runtime));
            feedback::start(Arc::downgrade(&runtime));
            telemetry::start(Arc::downgrade(&runtime));
            devices::start(Arc::downgrade(&runtime), device_receiver);
        }
        actions::start(Arc::downgrade(&runtime), action_receiver);
        if hardware {
            let weak = Arc::downgrade(&runtime);
            std::thread::spawn(move || loop {
                let Some(runtime) = weak.upgrade() else { break };
                if runtime.is_stopping() {
                    break;
                };
                for message in runtime.updates.notifications(&runtime.update_status()) {
                    runtime.emit("updates", None, None, Some(message));
                }
                drop(runtime);
                std::thread::sleep(std::time::Duration::from_secs(2));
            });
        }
        Ok(runtime)
    }
    // Read plugin crash-recovery state under the host lock
    pub fn recovery_status(&self) -> stream_dock_manager_plugin_host::recovery::RecoveryStatus {
        self.plugins.lock().unwrap().recovery_status()
    }
    // Resolve interrupted startup
    pub fn resolve_interrupted_startup(
        &self,
        normal: bool,
    ) -> Result<stream_dock_manager_plugin_host::recovery::RecoveryStatus, String> {
        let status = self
            .plugins
            .lock()
            .unwrap()
            .resolve_interrupted_startup(normal)?;
        self.emit("plugins", None, None, None);
        Ok(status)
    }
    // Report the restarted condition for src
    pub fn restarted(&self) -> bool {
        self.restarted
    }
    // Request restart
    pub fn request_restart(&self, recovery: bool) -> Result<std::path::PathBuf, String> {
        crate::profile_database::LocalPreference::RestartRecoveryOnce
            .record::<bool>()
            .write(&self.profiles.storage, &recovery)?;
        Ok(self.config_root.clone())
    }
    // Supply the default root
    pub fn default_root() -> Result<std::path::PathBuf, String> {
        directories::ProjectDirs::from(
            "app",
            "streamdockmanager",
            if cfg!(target_os = "linux") {
                "stream-dock-manager"
            } else {
                "Stream Dock Manager"
            },
        )
        .map(|p| p.config_dir().to_path_buf())
        .ok_or("configuration directory unavailable".into())
    }
    // Copy the current desktop preferences under their mutex
    pub fn desktop_settings(&self) -> desktop::DesktopSettings {
        self.desktop_settings.lock().unwrap().clone()
    }
    // Check ownership of export destination
    pub fn owns_export_destination(&self, path: &std::path::Path) -> bool {
        self.config_root
            .canonicalize()
            .map_or(true, |root| path.starts_with(root))
    }

    // Open data folder
    pub fn open_data_folder(&self) -> Result<(), String> {
        actions::open(
            self.config_root
                .to_str()
                .ok_or("Application data path is not UTF-8")?,
        )
    }
    // Resolve the logs directory beneath the active configuration root
    pub fn logs_directory(&self) -> std::path::PathBuf {
        self.profiles.storage.root().join("logs")
    }
    // Save desktop settings
    pub fn save_desktop_settings(
        &self,
        mut settings: desktop::DesktopSettings,
    ) -> Result<(), String> {
        self.update_desktop_settings(|current| {
            if settings.onboarding.is_none() {
                settings.onboarding = current.onboarding.clone();
            }
            settings.show_manager_shortcut = current.show_manager_shortcut;
            settings.plugin_links = current.plugin_links;
            settings.openaction_links = current.openaction_links;
            settings.openaction_link_previous_handler =
                current.openaction_link_previous_handler.clone();
            settings.plugin_link_previous_handler = current.plugin_link_previous_handler.clone();
            *current = settings;
        })
    }
    // Set plugin link settings
    pub fn set_plugin_link_settings(
        &self,
        enabled: bool,
        previous_handler: Option<String>,
    ) -> Result<(), String> {
        self.update_desktop_settings(|settings| {
            settings.plugin_links = enabled;
            settings.plugin_link_previous_handler = previous_handler;
        })
    }
    // Set openaction link settings
    pub fn set_openaction_link_settings(
        &self,
        enabled: bool,
        previous_handler: Option<String>,
    ) -> Result<(), String> {
        self.update_desktop_settings(|settings| {
            settings.openaction_links = enabled;
            settings.openaction_link_previous_handler = previous_handler;
        })
    }
    // Dispatch plugin link
    pub fn dispatch_plugin_link(&self, url: &str) -> Result<bool, String> {
        let settings = self.desktop_settings();
        let enabled = if url.starts_with("openaction:") {
            settings.openaction_links
        } else {
            settings.plugin_links
        };
        if !enabled {
            return Ok(false);
        }
        self.plugins.lock().unwrap().receive_deep_link(url)
    }
    // Set show manager shortcut
    pub fn set_show_manager_shortcut(&self, enabled: bool) -> Result<(), String> {
        self.update_desktop_settings(|settings| settings.show_manager_shortcut = Some(enabled))
    }

    // Update desktop settings
    pub(crate) fn update_desktop_settings(
        &self,
        update: impl FnOnce(&mut desktop::DesktopSettings),
    ) -> Result<(), String> {
        let mut current = self.desktop_settings.lock().unwrap();
        let mut next = current.clone();
        update(&mut next);
        desktop::SETTINGS_RECORD.write(&self.profiles.storage, &next)?;
        *current = next;
        Ok(())
    }
    /// Wait for the first complete discovery pass, including an empty device list
    /// Does not wait for a physical device to appear and never waits indefinitely
    pub fn wait_for_discovery(&self, timeout: std::time::Duration) -> Result<(), String> {
        self.discovery_readiness.wait(timeout)
    }

    // Return a snapshot of src state
    pub fn snapshot(&self) -> Snapshot {
        let mut s = self.state.lock().unwrap().clone();
        s.plugins = self.plugins.lock().unwrap().list();
        s.binding_overrides = self.builtin_state.overrides(&s);
        s
    }
    // Emit plugin runtime
    fn emit_plugin_runtime(&self) {
        self.emit_plugin_status("plugin-runtime");
    }
    // Emit a plugin status update after a lifecycle transition
    pub fn plugin_lifecycle_changed(&self) {
        self.emit_plugin_status("plugin-lifecycle");
    }
    // Emit plugin status
    fn emit_plugin_status(&self, kind: &str) {
        {
            let mut state = self.state.lock().unwrap();
            self.workspace_event(kind, &mut state);
        }
        // Dedicated plugin updates do not consume the configuration event sequence
        // Otherwise the editor's gap detection would request a full snapshot
        let event = RuntimeEvent {
            timestamp_ms: Some(event_timestamp_ms()),
            kind: kind.into(),
            sequence: self
                .event_sequence
                .load(std::sync::atomic::Ordering::Relaxed),
            revision: self.state.lock().unwrap().revision,
            device_id: None,
            control: None,
            message: None,
        };
        self.subscribers
            .lock()
            .unwrap()
            .retain(|sender| match sender.try_send(event.clone()) {
                Ok(_) | Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            });
    }
    // Notify clients of plugin changes and request restored device output
    pub fn plugins_changed(&self) -> Snapshot {
        self.emit("plugins", None, None, None);
        let _ = self.device_commands.send(devices::Request::Restore);
        self.snapshot()
    }
    // Reload installed plugins and reconcile retained runtime state when cleanup is safe
    pub fn reload_plugins(&self) -> Result<stream_dock_manager_plugin_host::PluginReload, String> {
        let (result, installed) = {
            let mut host = self.plugins.lock().unwrap();
            let result = host.reload()?;
            let installed = host.list().into_iter().map(|p| p.id).collect();
            (result, installed)
        };
        if result.cleanup_safe {
            self.reconcile_plugin_update_preferences(&installed)?;
            self.reconcile_plugin_library_marks(&installed)?;
            self.resource_alerts
                .lock()
                .unwrap()
                .retain_plugins(&installed)?;
        }
        Ok(result)
    }
    // Report whether stopping
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(std::sync::atomic::Ordering::SeqCst)
    }
    // Cache stats
    pub fn cache_stats(&self) -> serde_json::Value {
        let mut media_memory = media_memory::MediaMemory::default();
        self.profiles.collect_media_memory(&mut media_memory);
        self.video.collect_memory(&mut media_memory);
        let preparation_memory = self.preparations.memory_stats();
        let sources = self.profiles.source_work_stats();
        let encoding = encoding::work_stats();
        let plugin_visuals = self.plugins.lock().unwrap().visual_storage_stats();
        let metrics = self.metrics.summary_diagnostics();
        let mut cache = self.profiles.render_cache.lock().unwrap();
        let resident_bytes = cache.resident_bytes();
        let evicted_in_flight_bytes = cache.in_flight_bytes();
        serde_json::json!({"bytes":cache.bytes,"limit_bytes":64*1024*1024,"hits":cache.hits,"misses":cache.misses,
        "resident_bytes":resident_bytes,"evicted_in_flight_bytes":evicted_in_flight_bytes,
        "owned_copy_bytes":cache.copied_bytes,"admission_rejections":cache.rejected,
        "source_work":sources,"encoding_work":encoding,"plugin_visual_storage":plugin_visuals,
        "media_payload_storage":media_memory.summary(),"preparation_storage":preparation_memory,
        "media_budget": self.profiles.media_budget.stats(),
        "device_mailbox":self.device_commands.stats(),
        "metric_storage": {
            "series":metrics.series,"active_series":metrics.active_series,"retained_samples":metrics.retained_samples,
            "producer_capacity_bytes":metrics.sample_capacity_bytes,"producer_capacity_limit_bytes":metrics.sample_capacity_limit_bytes,
            "captured_shared_capacity_bytes":metrics.captured_shared_capacity_bytes,"captured_shared_peak_bytes":metrics.captured_shared_peak_bytes,
            "snapshot_shared_capacity_limit_bytes":metrics.snapshot_shared_capacity_limit_bytes,
            "summary_copy_peak_bytes":metrics.summary_copy_peak_bytes,"rebuilds":metrics.rebuilds,"cache_hits":metrics.cache_hits,
            "rejected_observations":metrics.rejected_observations
        }})
    }
    // Render media preview
    pub fn render_media_preview(
        &self,
        asset: &str,
        size: [u16; 2],
        options: &media::MediaOptions,
        formats: &[String],
    ) -> Result<Vec<u8>, String> {
        self.render_media_frame(asset, self.media_elapsed(), size, 0, options, formats)
    }
    // Render still source preview
    pub fn render_still_source_preview(&self, asset: &str) -> Result<Vec<u8>, String> {
        self.profiles.render_still_source_preview(asset)
    }
    // Render artwork source preview
    pub fn render_artwork_source_preview(
        &self,
        asset: &str,
        options: &media::MediaOptions,
    ) -> Result<Vec<u8>, String> {
        options.validate()?;
        if let Some(clip) = self.profiles.clip(asset)? {
            let pixels = image::load_from_memory(
                &clip.frames[options.frame_index(&clip, self.media_elapsed())],
            )
            .map_err(|e| e.to_string())?;
            hardware_core::render::encode_pixels(pixels.thumbnail(640, 640), "png")
                .map_err(|e| e.to_string())
        } else {
            self.render_still_source_preview(asset)
        }
    }
    // Update theme palette
    pub fn update_theme_palette(
        &self,
        mut palette: Option<stream_dock_manager_plugin_host::CustomPalette>,
        delete: Option<String>,
    ) -> Result<stream_dock_manager_plugin_host::AppearancePreferences, String> {
        let _media = self.profiles.media_gate.exclusive()?;
        if let Some(wallpaper) = palette
            .as_mut()
            .and_then(|palette| palette.wallpaper.as_mut())
        {
            let extension = self.profiles.asset_extension(&wallpaper.asset_id)?;
            let options: media::MediaOptions = serde_json::from_value(wallpaper.options.clone())
                .map_err(|error| error.to_string())?;
            options.validate()?;
            wallpaper.svg = None;
            if let Some(clip) = self.profiles.clip_under_gate(&wallpaper.asset_id)? {
                if options.trim_start_ms >= clip.duration_ms
                    || options
                        .trim_end_ms
                        .is_some_and(|end| end > clip.duration_ms)
                {
                    return Err("Wallpaper trim exceeds clip duration".into());
                }
                wallpaper.format.clone_from(&clip.source_format);
                wallpaper.animated = clip.frames.len() > 1;
            } else {
                let bytes = self.profiles.asset(&wallpaper.asset_id)?;
                wallpaper.format = media::detect(&bytes)?.into();
                wallpaper.animated = false;
                if extension == "svg" && bytes.len() <= 64 * 1024 {
                    wallpaper.svg = String::from_utf8(bytes).ok();
                }
            }
        }
        let mut plugins = self.plugins.lock().unwrap();
        plugins.update_custom_palette(palette, delete)?;
        Ok(plugins.appearance_preferences())
    }

    // Describe animation timing when an asset is a clip, otherwise report a static image
    pub fn media_details(&self, asset: &str) -> Result<serde_json::Value, String> {
        let clip = self.profiles.clip(asset)?;
        Ok(if let Some(clip) = clip {
            serde_json::json!({"animated":clip.frames.len()>1,"duration_ms":clip.duration_ms,"frames":clip.frames.len(),"fps":clip.prepared_fps(),"format":clip.source_format,"source_fps":clip.prepared_fps()})
        } else {
            serde_json::json!({"animated":false,"duration_ms":0,"frames":1,"fps":0})
        })
    }
    // Transfer uniquely owned render bytes or charge and perform the required copy
    fn owned_render_bytes(&self, bytes: media::SharedBytes) -> Vec<u8> {
        match Arc::try_unwrap(bytes) {
            Ok(bytes) => bytes.into_vec(),
            Err(bytes) => {
                let mut cache = self.profiles.render_cache.lock().unwrap();
                cache.copied_bytes = cache.copied_bytes.saturating_add(bytes.len() as u64);
                drop(cache);
                bytes.to_vec()
            }
        }
    }
    // Look up cached render
    fn cached_render(
        &self,
        key: String,
        render: impl FnOnce() -> Result<media::SharedBytes, String>,
    ) -> Result<media::SharedBytes, String> {
        if let Some(bytes) = self.profiles.render_cache.lock().unwrap().get_shared(&key) {
            return Ok(bytes);
        }
        let start = std::time::Instant::now();
        let result = render();
        self.metrics
            .record("render_ms", start.elapsed().as_secs_f64() * 1000.0);
        if let Ok(bytes) = &result {
            self.profiles
                .render_cache
                .lock()
                .unwrap()
                .insert_frame(key, bytes.clone())?;
        }
        result
    }
    // Render visual
    pub(crate) fn render_visual(
        &self,
        visual: &serde_json::Value,
        width: u16,
        height: u16,
        rotation: u16,
    ) -> Result<media::SharedBytes, String> {
        self.cached_render(
            format!(
                "visual:{}:{width}:{height}:{rotation}:{visual}",
                hardware_core::fonts::generation()
            ),
            || {
                hardware_core::render::visual(visual, width, height, rotation)
                    .map(media::shared_bytes)
                    .map_err(|e| e.to_string())
            },
        )
    }
    // Refresh profile palettes
    fn refresh_profile_palettes(&self) {
        let _sync = self.profile_theme_sync.lock().unwrap();
        let state = self.state.lock().unwrap();
        let palettes = std::iter::once("")
            .chain(state.devices.iter().map(|device| device.id.as_str()))
            .filter_map(|device| {
                let session = state.session(device);
                state
                    .profiles
                    .iter()
                    .find(|profile| profile.id == session.profile_id)
                    .and_then(|profile| profile.palette.clone())
                    .map(|palette| (device.to_string(), palette))
            })
            .collect();
        drop(state);
        // Serialize derived updates without holding runtime state across host work
        self.plugins.lock().unwrap().set_profile_palettes(palettes);
    }

    // Set profile theme context
    pub fn set_profile_theme_context(&self, device: &str) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        if !device.is_empty() && !state.devices.iter().any(|item| item.id == device) {
            return Err("Device no longer exists".into());
        }
        drop(state);
        self.plugins
            .lock()
            .unwrap()
            .set_manager_theme_device(device);
        Ok(())
    }

    // Subscribe to src updates
    pub fn subscribe(&self) -> mpsc::Receiver<RuntimeEvent> {
        let (tx, rx) = mpsc::sync_channel(512);
        self.subscribers.lock().unwrap().push(tx);
        rx
    }
    // Deliver the src event to its consumer
    pub(crate) fn emit(
        &self,
        kind: &str,
        device_id: Option<String>,
        control: Option<ControlEvent>,
        message: Option<String>,
    ) {
        let message = message.map(|value| self.credentials.redact_event(kind, value));
        let revision = {
            let mut s = self.state.lock().unwrap();
            if ["profiles", "devices", "sessions", "device-output"].contains(&kind) {
                s.revision += 1;
            }
            self.workspace_event(kind, &mut s);
            s.revision
        };
        self.notify(kind, device_id, control, message, revision);
    }
    // Notify consumers of the src change
    fn notify(
        &self,
        kind: &str,
        device_id: Option<String>,
        control: Option<ControlEvent>,
        message: Option<String>,
        revision: u64,
    ) {
        if matches!(
            kind,
            "profiles" | "devices" | "sessions" | "workspace-reset"
        ) {
            self.refresh_profile_palettes();
        }
        if matches!(
            kind,
            "sessions"
                | "devices"
                | "device-output"
                | "device-power"
                | "workspace-reset"
                | "profiles"
        ) {
            self.invalidate_locate(device_id.as_deref());
            if kind != "profiles" {
                self.invalidate_hardware_preview(device_id.as_deref());
            }
        }
        if matches!(
            kind,
            "profiles"
                | "devices"
                | "sessions"
                | "binding-overrides"
                | "workspace"
                | "workspace-reset"
        ) {
            self.audio_signal.demand_changed();
        }
        if matches!(
            kind,
            "devices" | "sessions" | "device-output" | "workspace-reset"
        ) {
            self.power.signal.notify();
        }
        let event = RuntimeEvent {
            timestamp_ms: Some(event_timestamp_ms()),
            sequence: self
                .event_sequence
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1,
            revision,
            kind: kind.into(),
            device_id,
            control,
            message,
        };
        self.subscribers.lock().unwrap().retain(|tx| {
            !matches!(
                tx.try_send(event.clone()),
                Err(mpsc::TrySendError::Disconnected(_))
            )
        });
    }
    // Save profile
    pub fn save_profile(
        &self,
        profile: Profile,
        expected_revision: u64,
    ) -> Result<Snapshot, String> {
        self.apply_edits(
            expected_revision,
            "Save profile",
            vec![workspace::Edit::SaveProfile { profile }],
        )
    }
    // Delete profile
    pub fn delete_profile(&self, id: &str, expected_revision: u64) -> Result<Snapshot, String> {
        self.apply_edits(
            expected_revision,
            "Delete profile",
            vec![workspace::Edit::DeleteProfile { id: id.into() }],
        )
    }
    // Delete page
    pub fn delete_page(
        &self,
        profile_id: &str,
        page_id: &str,
        expected_revision: u64,
    ) -> Result<Snapshot, String> {
        self.apply_edits(
            expected_revision,
            "Remove page",
            vec![workspace::Edit::DeletePage {
                profile_id: profile_id.into(),
                page_id: page_id.into(),
            }],
        )
    }
    // Apply edits
    pub fn apply_edits(
        &self,
        expected_revision: u64,
        description: &str,
        edits: Vec<workspace::Edit>,
    ) -> Result<Snapshot, String> {
        self.apply_workspace_edits(expected_revision, description, edits)?;
        Ok(self.snapshot())
    }
    // Apply workspace edits
    pub fn apply_workspace_edits(
        &self,
        expected_revision: u64,
        description: &str,
        edits: Vec<workspace::Edit>,
    ) -> Result<workspace_stream::WorkspaceBatch, String> {
        self.apply_workspace_edits_for_device(expected_revision, description, edits, None)
    }
    // Apply assignment edits for an explicitly selected connected device without separate profile creation
    pub fn apply_workspace_edits_for_device(
        &self,
        expected_revision: u64,
        description: &str,
        edits: Vec<workspace::Edit>,
        device_id: Option<&str>,
    ) -> Result<workspace_stream::WorkspaceBatch, String> {
        self.apply_workspace_edits_with_target(
            expected_revision,
            description,
            edits,
            None,
            device_id,
        )
    }
    // Apply workspace edits in session
    fn apply_workspace_edits_in_session(
        &self,
        expected_revision: u64,
        description: &str,
        edits: Vec<workspace::Edit>,
        expected_session: Option<(&str, &str, &str)>,
    ) -> Result<workspace_stream::WorkspaceBatch, String> {
        self.apply_workspace_edits_with_target(
            expected_revision,
            description,
            edits,
            expected_session,
            None,
        )
    }
    // Resolve assignment targets inside the same revision-checked transaction as their binding writes
    fn apply_workspace_edits_with_target(
        &self,
        expected_revision: u64,
        description: &str,
        edits: Vec<workspace::Edit>,
        expected_session: Option<(&str, &str, &str)>,
        device_id: Option<&str>,
    ) -> Result<workspace_stream::WorkspaceBatch, String> {
        if edits.is_empty() || edits.len() > 100 {
            return Err("Choose 1–100 edits".into());
        }
        let mut stream_edits = Vec::new();
        let destructive = edits.iter().any(|edit| {
            matches!(
                edit,
                workspace::Edit::DeleteProfile { .. }
                    | workspace::Edit::DeletePage { .. }
                    | workspace::Edit::DeleteMacro { .. }
            )
        });
        let mut state = self.state.lock().unwrap();

        if let Some((device, profile, page)) = expected_session {
            let session = state.session(device);
            if !state.devices.iter().any(|d| d.id == device && d.connected)
                || session.profile_id != profile
                || session.page_id != page
            {
                return Err("Device selection changed. Reopen Slice artwork.".into());
            }
        }
        let data = self.profiles.edit(expected_revision, description, |data| {
            for mut edit in edits {
                // Removal-only edits never create a profile as a side effect
                let target = match &mut edit {
                    workspace::Edit::Bindings {
                        profile_id,
                        page_id,
                        bindings,
                    } if bindings.values().any(Option::is_some) => Some((profile_id, page_id)),
                    workspace::Edit::AssignControl {
                        profile_id,
                        page_id,
                        ..
                    } => Some((profile_id, page_id)),
                    _ => None,
                };
                if let Some((profile_id, page_id)) = target {
                    let resolved =
                        assignment_target::resolve(&state, data, device_id, profile_id, page_id)?;
                    *profile_id = resolved.0;
                    *page_id = resolved.1;
                }
                stream_edits.push(edit.clone());
                workspace::apply_edits(data, vec![edit])?;
            }
            self.validate_sensitive_definition_changes(&state, data)?;
            self.validate_workspace(data)
        })?;
        let disabled_now = data.profiles.iter().any(|profile| {
            profile.pages.iter().any(|page| {
                page.bindings.iter().any(|(id, binding)| {
                    binding.disabled
                        && state
                            .profiles
                            .iter()
                            .find(|p| p.id == profile.id)
                            .and_then(|p| p.pages.iter().find(|p| p.id == page.id))
                            .and_then(|p| p.bindings.get(id))
                            .is_some_and(|old| !old.disabled)
                })
            })
        });
        let retired = state.profiles.iter().any(|old_profile| {
            old_profile.pages.iter().any(|old_page| {
                old_page.bindings.iter().any(|(id, old)| {
                    let new = data
                        .profiles
                        .iter()
                        .find(|p| p.id == old_profile.id)
                        .and_then(|p| p.pages.iter().find(|p| p.id == old_page.id))
                        .and_then(|p| p.bindings.get(id));
                    let Some(new) = new else { return true };
                    if !matches!(old.action, actions::Action::None)
                        && matches!(new.action, actions::Action::None)
                    {
                        return true;
                    }
                    let mut owns = false;
                    old.action.visit(&mut |action| {
                        if let actions::Action::Builtin { action } = action {
                            if builtins::requires_release(action)
                                || matches!(
                                    **action,
                                    builtins::BuiltinAction::LatchKeys { .. }
                                        | builtins::BuiltinAction::BindingLayer { .. }
                                        | builtins::BuiltinAction::SavedMacro { .. }
                                )
                            {
                                owns = true;
                            }
                        }
                    });
                    owns && serde_json::to_value(&old.action).ok()
                        != serde_json::to_value(&new.action).ok()
                })
            })
        });

        if destructive
            || disabled_now
            || retired
            || serde_json::to_value(&state.saved_macros).ok()
                != serde_json::to_value(&data.saved_macros).ok()
        {
            // Fence already queued recognitions before publishing the changed
            // action graph; keep ordinary frozen macro execution unchanged
            self.builtin_state.cancel_key_logic(None);
        }
        Self::apply_data(&mut state, data);
        state.revision += 1;
        let batch = self.edited_workspace(&mut state, &stream_edits);
        drop(state);

        if destructive || disabled_now || retired {
            self.cancel_macros();
        }
        self.notify("profiles", None, None, None, batch.revision);
        let _ = self.device_commands.send(devices::Request::Restore);
        Ok(batch)
    }
    // Validate workspace
    fn validate_workspace(&self, data: &ProfileData) -> Result<(), String> {
        let plugins = self.plugins.lock().unwrap().list();
        for action in data.saved_macros.iter().map(|saved| &saved.action).chain(
            data.profiles
                .iter()
                .flat_map(|profile| &profile.pages)
                .flat_map(|page| page.bindings.values())
                .filter(|binding| !binding.disabled)
                .map(|binding| &binding.action),
        ) {
            builtins::validate_key_logic_plugins(
                action,
                |id| {
                    data.saved_macros
                        .iter()
                        .find(|saved| saved.id == id)
                        .map(|saved| &saved.action)
                },
                &plugins,
            )?;
        }
        for profile in &data.profiles {
            let manifests = hardware_registry::manifests();
            let manifest = manifests
                .iter()
                .find(|m| m.model_id == profile.model_id)
                .ok_or("Unsupported model")?;
            for binding in profile.pages.iter().flat_map(|p| p.bindings.iter()) {
                if binding.1.disabled {
                    continue;
                }
                let control = manifest.control(binding.0).map_err(|error| {
                    format!(
                        "Unsupported profile control {}/{}: {error}",
                        profile.model_id, binding.0
                    )
                })?;
                self.validate_binding(binding.1, control).map_err(|error| {
                    format!(
                        "Invalid profile control {}/{}: {error}",
                        profile.model_id, binding.0
                    )
                })?;
                if let Some(id) = &binding.1.asset {
                    if let Some(clip) = self.profiles.clip(id)? {
                        if binding.1.media.trim_start_ms >= clip.duration_ms
                            || binding
                                .1
                                .media
                                .trim_end_ms
                                .is_some_and(|end| end > clip.duration_ms)
                        {
                            return Err("Trim boundaries exceed the clip duration".into());
                        }
                    }
                }
            }
            if let Some(id) = &profile.background_asset {
                if let Some(clip) = self.profiles.clip(id)? {
                    if profile.background_media.trim_start_ms >= clip.duration_ms
                        || profile
                            .background_media
                            .trim_end_ms
                            .is_some_and(|end| end > clip.duration_ms)
                    {
                        return Err("Background trim exceeds clip duration".into());
                    }
                }
            }
            for id in profile
                .pages
                .iter()
                .flat_map(|p| p.bindings.values())
                .filter(|b| !b.disabled)
                .filter_map(|b| b.asset.as_ref())
                .chain(profile.background_asset.as_ref())
            {
                let clip = self.profiles.clip(id)?;
                let format = clip
                    .as_ref()
                    .map(|c| c.source_format.as_str())
                    .unwrap_or("png");
                if !manifest
                    .capabilities
                    .media_formats
                    .iter()
                    .any(|f| f == format)
                {
                    return Err("Media format unsupported by this model".into());
                }
            }
        }

        Ok(())
    }
    // Validate binding
    fn validate_binding(
        &self,
        binding: &Binding,
        control: &hardware_core::Control,
    ) -> Result<(), String> {
        if binding.disabled {
            return Ok(());
        }
        if control.kind == "display" && !matches!(binding.action, actions::Action::None) {
            return Err("A display-only surface accepts appearance settings, not an action".into());
        }
        // Check the current src conditions
        fn check(
            action: &actions::Action,
            control: &hardware_core::Control,
            plugins: &[stream_dock_manager_plugin_host::PluginInfo],
        ) -> Result<(), String> {
            match action {
                actions::Action::SwipeNavigation { .. }
                    if !control.semantics.iter().any(|s| s == "swipe") =>
                {
                    return Err("Touch bar navigation needs a control with swipe support".into());
                }
                actions::Action::Plugin {
                    plugin_id,
                    action_id,
                    ..
                } => {
                    if let Some(action) = plugins
                        .iter()
                        .find(|p| &p.id == plugin_id)
                        .and_then(|p| p.actions.iter().find(|a| &a.id == action_id))
                    {
                        if !action.control_types.contains(&control.kind) {
                            return Err("Plugin action is incompatible with this control".into());
                        }
                    }
                }
                actions::Action::Builtin { action } => {
                    if builtins::requires_release(action)
                        && !control.semantics.iter().any(|s| s == "release")
                    {
                        return Err("This action requires physical button release events".into());
                    }
                    for child in action.children() {
                        check(child, control, plugins)?;
                    }
                }
                actions::Action::Dial { actions, .. } => {
                    if control.kind != "encoder" {
                        return Err("Dial actions require an encoder control".into());
                    }
                    // Direction and press lanes are independent activations, not held buttons
                    let mut lane_control = control.clone();
                    lane_control
                        .semantics
                        .retain(|semantic| semantic != "release");
                    for action in actions {
                        check(action, &lane_control, plugins)?;
                    }
                }
                actions::Action::Macro { actions } => {
                    for a in actions {
                        check(a, control, plugins)?;
                    }
                }
                actions::Action::Toggle { on, off } => {
                    check(on, control, plugins)?;
                    check(off, control, plugins)?;
                }
                _ => {}
            }
            Ok(())
        }
        check(
            &binding.action,
            control,
            &self.plugins.lock().unwrap().list(),
        )
    }
    // Apply data
    fn apply_data(state: &mut Snapshot, data: ProfileData) {
        workspace::repair_removed_pages(&state.profiles, &data.profiles, &mut state.sessions);
        state.configuration_revision = data.configuration_revision;
        state.active_profile = data.active_profile;
        state.profiles = data.profiles;

        state.saved_macros = data.saved_macros;

        state.styles = data.styles;
        state.rules = data.rules;
        state.history = data.history;
        let ids: Vec<_> = state
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.persistence_key.clone()))
            .collect();
        for (id, key) in ids {
            let session = state.session(&id);
            state.sessions.insert(key, session);
        }
    }
    // Called only after the durable transaction and all mutation guards are released
    // Keep cancellation before publication and device restoration in this order
    fn finish_workspace_commit(&self) {
        self.cancel_macros();
        self.emit("profiles", None, None, None);
        let _ = self.device_commands.send(devices::Request::Restore);
    }

    // Apply revision-checked undo or redo through the appropriate workspace history path
    pub fn history_action(
        &self,
        operation: &str,
        expected_revision: u64,
    ) -> Result<Snapshot, String> {
        if let Some(snapshot) = self.plugin_profile_history(operation, expected_revision)? {
            return Ok(snapshot);
        }
        let mut state = self.state.lock().unwrap();

        let data = self.profiles.history_action(expected_revision, operation)?;

        Self::apply_data(&mut state, data);
        drop(state);

        self.finish_workspace_commit();
        Ok(self.snapshot())
    }
    // Capture binding
    pub fn capture_binding(&self, device_id: &str, control_id: &str) -> Result<Binding, String> {
        let state = self.snapshot();
        let session = state.session(device_id);
        let mut binding = state
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .and_then(|p| p.pages.iter().find(|p| p.id == session.page_id))
            .and_then(|p| p.bindings.get(control_id))
            .cloned()
            .ok_or("Binding missing")?;
        // Capture the current src observation
        fn capture(
            action: &mut actions::Action,
            path: Vec<usize>,
            host: &stream_dock_manager_plugin_host::PluginHost,
            base: &str,
            device: &str,
            control: &str,
        ) {
            match action {
                actions::Action::Plugin {
                    plugin_id,
                    settings,
                    ..
                } => {
                    *settings = host.effective_settings(
                        plugin_id,
                        &actions::node_scope(base, &path),
                        device,
                        control,
                        settings,
                    )
                }
                actions::Action::Builtin { action } => {
                    for (index, child) in action.children_mut().iter_mut().enumerate() {
                        let mut p = path.clone();
                        p.push(index);
                        capture(child, p, host, base, device, control);
                    }
                }
                actions::Action::Macro { actions: children }
                | actions::Action::Dial {
                    actions: children, ..
                } => {
                    for (index, action) in children.iter_mut().enumerate() {
                        let mut child = path.clone();
                        child.push(index);
                        capture(action, child, host, base, device, control);
                    }
                }
                actions::Action::Toggle { on, off } => {
                    let mut left = path.clone();
                    left.push(0);
                    capture(on, left, host, base, device, control);
                    let mut right = path;
                    right.push(1);
                    capture(off, right, host, base, device, control);
                }
                _ => {}
            }
        }
        capture(
            &mut binding.action,
            vec![],
            &self.plugins.lock().unwrap(),
            &format!("{}:{}", session.profile_id, session.page_id),
            device_id,
            control_id,
        );
        Ok(binding)
    }
    // Activate the requested profile on the first available device
    pub fn activate_profile(&self, id: &str) -> Result<Snapshot, String> {
        let device = self
            .snapshot()
            .devices
            .first()
            .map(|d| d.id.clone())
            .unwrap_or_default();
        self.select_device_profile(&device, id, None, true)
    }
    // Select device profile
    pub fn select_device_profile(
        &self,
        device_id: &str,
        id: &str,
        page_id: Option<&str>,
        manual: bool,
    ) -> Result<Snapshot, String> {
        self.select_device_profile_checked(device_id, id, page_id, manual, None, None, None)
    }
    // Read the current connection epoch used to reject stale device operations
    pub fn connection_generation(&self, device_id: &str) -> Option<u64> {
        self.connection_epochs
            .lock()
            .unwrap()
            .get(device_id)
            .copied()
    }
    // Select from tray
    pub fn select_from_tray(
        &self,
        device_id: &str,
        epoch: u64,
        profile_id: &str,
        page_id: Option<&str>,
    ) -> Result<Snapshot, String> {
        self.select_device_profile_checked(
            device_id,
            profile_id,
            page_id,
            true,
            Some(epoch),
            None,
            None,
        )
    }
    // Select vendor profile
    pub(crate) fn select_vendor_profile(
        &self,
        device: &str,
        profile: &str,
        page: &str,
        proof: &vendor_profiles::Navigation,
    ) -> Result<Snapshot, String> {
        self.select_device_profile_checked(
            device,
            profile,
            Some(page),
            false,
            None,
            Some(proof),
            None,
        )
    }
    // Select automatic profile
    pub(crate) fn select_automatic_profile(
        &self,
        device: &str,
        profile: &str,
        page: Option<&str>,
        matched: bool,
        expected_revision: u64,
        expected_session: &workspace::DeviceSession,
    ) -> Result<Snapshot, String> {
        self.select_device_profile_checked(
            device,
            profile,
            page,
            false,
            None,
            None,
            Some((matched, expected_revision, expected_session)),
        )
    }
    #[allow(clippy::too_many_arguments)]
    // Select device profile checked
    fn select_device_profile_checked(
        &self,
        device_id: &str,
        id: &str,
        page_id: Option<&str>,
        manual: bool,
        expected_epoch: Option<u64>,
        vendor: Option<&vendor_profiles::Navigation>,
        automatic: Option<(bool, u64, &workspace::DeviceSession)>,
    ) -> Result<Snapshot, String> {
        let epochs = Some(self.connection_epochs.lock().unwrap());
        if expected_epoch
            .is_some_and(|epoch| epochs.as_ref().unwrap().get(device_id) != Some(&epoch))
        {
            return Err("Device connection changed. Reopen the tray menu.".into());
        }
        let mut state = self.state.lock().unwrap();
        if let Some((_, expected_revision, expected_session)) = automatic {
            let session = state.session(device_id);
            if state.configuration_revision != expected_revision
                || session.profile_id != expected_session.profile_id
                || session.page_id != expected_session.page_id
                || session.automation_restore_on_exit != expected_session.automation_restore_on_exit
                || session.automation_origin != expected_session.automation_origin
            {
                return Ok(state.clone());
            }
            if !session.automation_enabled || session.automation_paused {
                return Ok(state.clone());
            }
        }
        if expected_epoch.is_some()
            && !state
                .devices
                .iter()
                .any(|device| device.id == device_id && device.connected)
        {
            return Err("Device is no longer connected".into());
        }
        let device = state.devices.iter().find(|d| d.id == device_id);
        let vendor_host = vendor.map(|_| self.plugins.lock().unwrap());
        if let Some(proof) = vendor {
            let device = device.ok_or("Profile target device is no longer connected")?;
            proof.verify(
                vendor_host.as_ref().unwrap(),
                device,
                &state,
                epochs.as_ref().unwrap().get(device_id).copied(),
                (id, page_id.ok_or("Vendor target page missing")?),
                &self.vendor_index()?,
            )?;
        }
        let physical = device.is_some_and(|d| !d.simulated);
        let key = device
            .map(|d| d.persistence_key.clone())
            .unwrap_or_else(|| device_id.into());
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == id && device.is_none_or(|d| d.model_id == p.model_id))
            .ok_or("Profile is incompatible with this device")?;
        if page_id.is_some_and(|id| !profile.pages.iter().any(|p| p.id == id)) {
            return Err("Page missing".into());
        }
        let before = state.session(device_id);
        let mut session = before.clone();
        if let Some((matched, _, _)) = automatic {
            if matched && session.automation_restore_on_exit {
                if session.automation_origin.is_none() {
                    session.automation_origin =
                        Some((session.profile_id.clone(), session.page_id.clone()));
                }
            } else {
                session.automation_origin = None;
            }
        } else if before.profile_id != id {
            session.automation_origin = None;
        }
        session.profile_id = profile.id.clone();
        session.page_id = page_id
            .map(String::from)
            .or_else(|| session.remembered_pages.get(id).cloned())
            .filter(|id| profile.pages.iter().any(|p| &p.id == id))
            .unwrap_or_else(|| profile.active_page.clone());
        session
            .remembered_pages
            .insert(id.into(), session.page_id.clone());
        if manual && before.profile_id != id {
            session.manual_profile = id.into();
            session.automation_paused = session.automation_enabled;
        }
        let mut sessions = state.sessions.clone();
        sessions.insert(key.clone(), session);
        let saved = sessions.clone();
        let vendor_proof = vendor;
        let vendor = vendor.map(|proof| {
            (
                proof.plugin.clone(),
                proof.installation.clone(),
                proof.manifest_name.is_none(),
            )
        });
        let commit = || {
            self.profiles
                .storage
                .transaction_with_cost(2 * 1024 * 1024, move |tx| {
                    vendor_profiles::navigate(
                        tx,
                        &key,
                        &before,
                        vendor
                            .as_ref()
                            .map(|(o, i, r)| (o.as_str(), i.as_str(), *r)),
                        manual,
                    )?;
                    stream_dock_manager_storage::write_json(
                        tx,
                        stream_dock_manager_storage::Domain::LocalPreferences,
                        "sessions",
                        &saved,
                        16 * 1024 * 1024,
                    )
                })
        };
        if let Some(proof) = vendor_proof {
            vendor_host.as_ref().unwrap().with_profile_switch_session(
                &proof.plugin,
                &proof.session,
                commit,
            )?;
        } else {
            commit()?;
        }
        drop(vendor_host);
        if physical {
            // The mapping stays behind the same state lock until the input gate
            // is closed. Dispatch rechecks admission before queuing an action
            let epoch = epochs
                .as_ref()
                .and_then(|epochs| epochs.get(device_id).copied());
            if let Some(epoch) = epoch {
                self.transition_inputs
                    .lock()
                    .unwrap()
                    .begin(device_id, epoch);
            }
        }
        // Simulator mappings change without the physical transfer gate, but
        // a pending gesture still belongs to the old page/profile
        self.builtin_state.cancel_key_logic(Some(device_id));
        state.sessions = sessions;
        if device_id.is_empty() {
            state.active_profile = id.into();
        }
        drop(state);
        drop(epochs);
        if physical {
            self.release_transition_inputs(device_id);
        }
        self.plugins
            .lock()
            .unwrap()
            .retire_device_appearances(device_id);
        self.emit("sessions", Some(device_id.into()), None, None);
        if let Some(epoch) = self.connection_generation(device_id) {
            let _ = self
                .device_commands
                .send(devices::Request::RestoreDevice(device_id.into(), epoch));
        }
        Ok(self.snapshot())
    }
    /// Apply one device's rule draft and enable/pause state as a single durable change
    pub fn save_automation_configuration(
        &self,
        draft: automation::AutomationConfigurationDraft,
    ) -> Result<Snapshot, String> {
        {
            let status = self.automation_status.lock().unwrap();
            if draft.enabled && !status.available && !status.schedules_available {
                return Err(status.reason.clone());
            }
        }
        let mut state = self.state.lock().unwrap();
        let key = state
            .devices
            .iter()
            .find(|d| d.id == draft.device_id)
            .ok_or("Device missing")?
            .persistence_key
            .clone();
        let mut session = state.session(&draft.device_id);
        if state.configuration_revision != draft.expected_revision
            || session.automation_enabled != draft.expected_enabled
            || session.automation_paused != draft.expected_paused
            || session.automation_restore_on_exit != draft.expected_restore_on_exit
        {
            return Err("Automatic profiles changed. Close and reopen before applying.".into());
        }
        if draft.rules.iter().any(|rule| rule.device_key != key) {
            return Err("Automatic profile rules must belong to the selected device".into());
        }
        let mut rules: Vec<_> = state
            .rules
            .iter()
            .filter(|rule| rule.device_key != key)
            .cloned()
            .collect();
        rules.extend(draft.rules);
        if !draft.enabled
            || draft.paused
            || session.automation_restore_on_exit != draft.restore_on_exit
        {
            session.automation_origin = None;
        }
        session.automation_edge = None;
        session.automation_restore_on_exit = draft.restore_on_exit;
        session.automation_enabled = draft.enabled;
        session.automation_paused = draft.paused;
        let mut sessions = state.sessions.clone();
        sessions.insert(key, session);
        let gate = self.profiles.transaction.lock().unwrap();
        let prepared = self.profiles.prepare_import_transaction(
            draft.expected_revision,
            "Update automatic profiles",
            None,
            0,
            |data| workspace::apply_edits(data, vec![workspace::Edit::Rules { rules }]),
        )?;
        let cost = prepared.retained_bytes()?
            + profile_database::encoded_size(&sessions, 16 * 1024 * 1024)?;
        let committed_sessions = sessions.clone();
        let data = self
            .profiles
            .storage
            .transaction_with_cost(cost, move |tx| {
                stream_dock_manager_storage::write_json(
                    tx,
                    stream_dock_manager_storage::Domain::LocalPreferences,
                    "sessions",
                    &committed_sessions,
                    16 * 1024 * 1024,
                )?;
                prepared.commit(tx)
            })?;
        state.sessions = sessions;
        Self::apply_data(&mut state, data);
        drop(gate);
        drop(state);
        self.emit("profiles", None, None, None);
        self.emit("sessions", Some(draft.device_id), None, None);
        Ok(self.snapshot())
    }
    // Set automation
    pub fn set_automation(
        &self,
        device_id: &str,
        enabled: bool,
        paused: bool,
    ) -> Result<Snapshot, String> {
        {
            let status = self.automation_status.lock().unwrap();
            if enabled && !status.available && !status.schedules_available {
                return Err(status.reason.clone());
            }
        }
        let mut state = self.state.lock().unwrap();
        let key = state
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or("Device missing")?
            .persistence_key
            .clone();
        let mut session = state.session(device_id);
        if !enabled || paused {
            session.automation_origin = None;
        }
        session.automation_edge = None;
        session.automation_enabled = enabled;
        session.automation_paused = paused;
        let mut sessions = state.sessions.clone();
        sessions.insert(key, session);
        self.profiles.write_preference(
            crate::profile_database::LocalPreference::Sessions,
            &sessions,
        )?;
        state.sessions = sessions;
        drop(state);
        self.emit("sessions", Some(device_id.into()), None, None);
        Ok(self.snapshot())
    }
    // Save draft
    pub fn save_draft(&self, key: &str, draft: Option<workspace::Draft>) -> Result<(), String> {
        if key.len() > 512 {
            return Err("Draft address exceeds limit".into());
        }
        if let Some(draft) = &draft {
            draft.validate()?;
        }
        let state = self.state.lock().unwrap();
        if draft.is_some() {
            if let Some(device) = serde_json::from_str::<serde_json::Value>(key)
                .ok()
                .and_then(|address| address.get(0)?.as_str().map(String::from))
            {
                if device.starts_with("simulator:")
                    && !state.devices.iter().any(|d| d.id == device)
                    && !state.sessions.contains_key(&device)
                {
                    return Err("Simulator was removed; its draft cannot be restored".into());
                }
            }
        }
        let _transaction = self.profiles.transaction.lock().unwrap();
        let mut drafts = self.drafts.lock().unwrap();
        // The transaction and draft locks keep the candidate private. Only the
        // affected entry needs rollback ownership; other drafts stay in place
        let previous = if let Some(mut draft) = draft {
            self.profiles.detach_deleted_draft(&mut draft);
            if drafts.len() >= 1000 && !drafts.contains_key(key) {
                return Err("Draft limit reached".into());
            }
            drafts.insert(key.into(), draft)
        } else {
            drafts.remove(key)
        };
        if let Err(error) = self
            .profiles
            .write_preference(crate::profile_database::LocalPreference::Drafts, &*drafts)
        {
            if let Some(previous) = previous {
                drafts.insert(key.into(), previous);
            } else {
                drafts.remove(key);
            }
            return Err(error);
        }
        Ok(())
    }
    // Simulate the requested src behavior
    pub fn simulate(&self, enabled: bool) -> Result<Snapshot, String> {
        self.simulate_device(enabled, "simulator:n4-pro")
    }
    // Enable or disable a validated simulator identity using its hardware manifest
    pub fn simulate_device(&self, enabled: bool, id: &str) -> Result<Snapshot, String> {
        if !id.starts_with("simulator:") || id.len() <= 10 || id.len() > 100 {
            return Err("Invalid simulator identity".into());
        }
        let manifests = hardware_registry::manifests();
        let identity = id.strip_prefix("simulator:").unwrap();
        let model_id = identity.split(':').next().unwrap();
        let manifest = manifests.iter().find(|m| m.model_id == model_id);
        if identity.contains(':') && manifest.is_none() {
            return Err("Unknown simulator model".into());
        }
        let base = manifest.unwrap_or(&manifests[0]);
        let module = hardware_registry::modules()
            .into_iter()
            .find(|module| module.manifest().model_id == base.model_id)
            .ok_or("Unknown simulator module")?;
        let m = module
            .representative_contract()
            .map_err(|error| error.to_string())?
            .manifest;
        let mut epochs = self.connection_epochs.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let mut candidate = state.clone();
        candidate.devices.retain(|d| d.id != id);
        if enabled {
            let new_profile = if !candidate.profiles.iter().any(|p| p.model_id == m.model_id)
                && !candidate.sessions.contains_key(id)
            {
                let page_id = uuid::Uuid::new_v4().to_string();
                let mut profile = Profile {
                    palette: None,
                    parameters: vec![],
                    id: uuid::Uuid::new_v4().to_string(),
                    name: profiles::DEFAULT_PROFILE_NAME.into(),
                    model_id: m.model_id.clone(),
                    active_page: page_id.clone(),
                    pages: vec![profiles::Page {
                        id: page_id,
                        name: "Main page".into(),
                        bindings: Default::default(),
                    }],
                    background_asset: None,
                    background_media: Default::default(),
                };
                crate::keyboard_navigation::normalize_profile(&mut profile)?;
                if crate::keyboard_navigation::supported(&profile.model_id) {
                    profile.active_page = crate::keyboard_navigation::PAGE_ID.into();
                }
                candidate.profiles.push(profile.clone());
                Some(profile)
            } else {
                None
            };
            let suffix = identity.split(':').next_back().unwrap();
            candidate.devices.push(DeviceInfo {
                id: id.into(),
                persistence_key: id.into(),
                identity_port_dependent: false,
                model_id: m.model_id.clone(),
                name: format!(
                    "{} Simulator ({})",
                    m.name,
                    suffix.chars().take(8).collect::<String>()
                ),
                variant_id: "simulated".into(),
                firmware: "simulated".into(),
                usb_release: None,
                connected: true,
                simulated: true,
                manifest: m,
            });
            let session = candidate.session(id);
            candidate.sessions.insert(id.into(), session);
            let _gate = self.profiles.transaction.lock().unwrap();
            if let Some(profile) = new_profile {
                let prepared = self.profiles.prepare_import_transaction(
                    state.configuration_revision,
                    "Create simulator profile",
                    None,
                    0,
                    |data| {
                        workspace::apply_edits(data, vec![workspace::Edit::SaveProfile { profile }])
                    },
                )?;
                let sessions = candidate.sessions.clone();
                let cost = prepared.retained_bytes()?
                    + profile_database::encoded_size(&sessions, 16 * 1024 * 1024)?;
                let data = self
                    .profiles
                    .storage
                    .transaction_with_cost(cost, move |tx| {
                        stream_dock_manager_storage::write_json(
                            tx,
                            stream_dock_manager_storage::Domain::LocalPreferences,
                            "sessions",
                            &sessions,
                            16 * 1024 * 1024,
                        )?;
                        prepared.commit(tx)
                    })?;
                Self::apply_data(&mut candidate, data);
            } else {
                self.profiles.write_preference(
                    crate::profile_database::LocalPreference::Sessions,
                    &candidate.sessions,
                )?;
            }
            epochs.insert(
                id.into(),
                self.next_simulator_epoch
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
            );
        } else {
            epochs.remove(id);
        }
        *state = candidate;
        drop(state);
        drop(epochs);
        if !enabled {
            self.builtin_state.cleanup(Some(id));
        }
        self.emit("devices", None, None, None);
        Ok(self.snapshot())
    }
    // Remove simulator
    pub fn remove_simulator(&self, id: &str) -> Result<Snapshot, String> {
        if !id.starts_with("simulator:") || id.len() > 100 {
            return Err("Invalid simulator identity".into());
        }
        let mut epochs = self.connection_epochs.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let _gate = self.profiles.transaction.lock().unwrap();
        let mut drafts = self.drafts.lock().unwrap();
        let belongs = |key: &str| {
            serde_json::from_str::<serde_json::Value>(key)
                .ok()
                .is_some_and(|address| {
                    address.get(0).and_then(serde_json::Value::as_str) == Some(id)
                })
        };
        if !state.devices.iter().any(|d| d.id == id && d.simulated)
            && !state.sessions.contains_key(id)
            && !drafts.keys().any(|key| belongs(key))
            && !state.rules.iter().any(|rule| rule.device_key == id)
        {
            return Err("Simulator not found".into());
        }
        let mut sessions = state.sessions.clone();
        sessions.remove(id);
        let mut remaining_drafts = drafts.clone();
        remaining_drafts.retain(|key, _| !belongs(key));
        let prepared = self.profiles.prepare_import_transaction(
            state.configuration_revision,
            "Remove simulator rules",
            None,
            0,
            |data| {
                data.rules.retain(|rule| rule.device_key != id);
                Ok(())
            },
        )?;
        let next_sessions = sessions.clone();
        let next_drafts = remaining_drafts.clone();
        let cost = prepared.retained_bytes()?
            + profile_database::encoded_size(&next_sessions, 16 * 1024 * 1024)?
            + profile_database::encoded_size(&next_drafts, 16 * 1024 * 1024)?;
        let data = self
            .profiles
            .storage
            .transaction_with_cost(cost, move |tx| {
                stream_dock_manager_storage::write_json(
                    tx,
                    stream_dock_manager_storage::Domain::LocalPreferences,
                    "sessions",
                    &next_sessions,
                    16 * 1024 * 1024,
                )?;
                stream_dock_manager_storage::write_json(
                    tx,
                    stream_dock_manager_storage::Domain::LocalPreferences,
                    "drafts",
                    &next_drafts,
                    16 * 1024 * 1024,
                )?;
                prepared.commit(tx)
            })?;
        Self::apply_data(&mut state, data);
        state.sessions = sessions;
        *drafts = remaining_drafts;
        drop(_gate);
        state.devices.retain(|d| d.id != id);
        epochs.remove(id);
        drop(drafts);
        drop(state);
        drop(epochs);
        self.builtin_state.cleanup(Some(id));
        self.emit("devices", Some(id.into()), None, None);
        let _ = self.device_commands.send(devices::Request::Restore);
        Ok(self.snapshot())
    }
    // Admit synthetic input only for a registered simulated device
    pub fn simulate_input(&self, device_id: &str, event: ControlEvent) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        state
            .devices
            .iter()
            .find(|d| d.id == device_id && d.simulated)
            .ok_or("not a simulator")?;
        drop(state);
        self.trigger_input(device_id, event)
    }
    // Trigger input
    pub fn trigger_input(&self, device_id: &str, event: ControlEvent) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or("device not connected")?;
        let control = device
            .manifest
            .control(&event.control_id)
            .map_err(|e| e.to_string())?;
        if control.kind == "display" {
            return Err("A display-only surface has no input".into());
        }
        let baseline = state.session(device_id).output;
        if baseline.sleeping {
            return Err("Device is sleeping".into());
        }
        let simulated = device.simulated;
        drop(state);
        if simulated {
            let epoch = self
                .connection_epochs
                .lock()
                .unwrap()
                .get(device_id)
                .copied();
            if let Some(epoch) = epoch {
                self.setup_capabilities
                    .observe_input(device_id, epoch, true, &event);
            }
        }
        let epoch = self
            .connection_epochs
            .lock()
            .unwrap()
            .get(device_id)
            .copied();
        if let Some(epoch) = epoch {
            if simulated && self.power_input(device_id, epoch, &event) {
                self.emit(
                    "input",
                    Some(device_id.into()),
                    Some(event),
                    Some("Simulator wake input consumed".into()),
                );
                return Ok(());
            }
        }
        if epoch.is_some_and(|epoch| self.power.effective(device_id, epoch, &baseline).sleeping) {
            return Err(
                "Device is automatically sleeping; use Wake or a supported dock interaction".into(),
            );
        }
        self.dispatch(device_id, &event);
        self.emit(
            "input",
            Some(device_id.into()),
            Some(event),
            Some("Simulator input received".into()),
        );
        Ok(())
    }
    // Run saved macro
    pub fn run_saved_macro(
        &self,
        device_id: Option<&str>,
        id: &str,
        values: macro_inputs::InputValues,
    ) -> Result<(), String> {
        let connection_epoch = device_id
            .map(|id| {
                self.connection_epochs
                    .lock()
                    .unwrap()
                    .get(id)
                    .copied()
                    .ok_or("The selected device disconnected")
            })
            .transpose()?;
        let snapshot = self.snapshot();
        let device_id = device_id.unwrap_or_default();
        let session = snapshot.session(device_id);
        if let Some(epoch) = connection_epoch {
            if !snapshot
                .devices
                .iter()
                .any(|d| d.id == device_id && d.connected)
            {
                return Err("The selected device disconnected".into());
            }
            if self
                .power
                .effective(device_id, epoch, &session.output)
                .sleeping
            {
                return Err("Wake the selected device before running the macro".into());
            }
        }
        let saved = snapshot
            .saved_macros
            .iter()
            .find(|m| m.id == id)
            .ok_or("Saved macro missing")?;
        builtins::require_macro_plugins(
            &saved.action,
            |id| {
                snapshot
                    .saved_macros
                    .iter()
                    .find(|saved| saved.id == id)
                    .map(|saved| &saved.action)
            },
            |id, action| {
                snapshot.plugins.iter().any(|plugin| {
                    plugin.id == id && plugin.actions.iter().any(|entry| entry.id == action)
                })
            },
        )?;
        let arguments = values
            .into_iter()
            .map(|(name, value)| (name, macro_inputs::MacroArgument::Literal { value }))
            .collect();
        macro_inputs::bind(
            &saved.inputs,
            &arguments,
            &Default::default(),
            &Default::default(),
            |_| Err("Manual macro inputs must be literal values".into()),
        )?;
        self.queue_action(actions::Request {
            manual: true,
            activation_policy: Default::default(),
            icon_signature: None,
            pressed: None,
            action: actions::Action::Builtin {
                action: Box::new(builtins::BuiltinAction::SavedMacro {
                    macro_id: id.into(),
                    arguments,
                }),
            },
            queued_at: std::time::Instant::now(),
            device_id: device_id.into(),
            control_id: format!("macro:{id}"),
            binding_scope: format!("{}:{}", session.profile_id, session.page_id),
            connection_epoch,
            input: hardware_core::Input::Pulse,
            cancellation_generation: self.cancellation.load(std::sync::atomic::Ordering::SeqCst),
        })
    }
    // Update the selected device’s output brightness through the normal persistence path
    pub fn brightness(&self, device_id: &str, value: u8) -> Result<(), String> {
        self.update_device_output(device_id, |output| output.brightness = value)
            .map(|_| ())
    }
    // Set device output
    pub fn set_device_output(
        &self,
        device_id: &str,
        output: workspace::DeviceOutput,
    ) -> Result<Snapshot, String> {
        self.update_device_output(device_id, |current| {
            let keyboard = current.keyboard.clone();
            *current = output;
            current.keyboard = keyboard;
        })
    }
    /// Change sleep state while retaining the latest brightness and lighting settings
    pub fn set_device_sleep(&self, device_id: &str, sleeping: bool) -> Result<Snapshot, String> {
        let snapshot = self.update_device_output(device_id, |output| output.sleeping = sleeping)?;
        if !sleeping {
            self.power_wake(device_id);
        }
        Ok(snapshot)
    }
    // Update device output
    fn update_device_output(
        &self,
        device_id: &str,
        update: impl FnOnce(&mut workspace::DeviceOutput),
    ) -> Result<Snapshot, String> {
        let mut state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|device| device.id == device_id && device.connected)
            .ok_or("device not connected")?;
        let key = device.persistence_key.clone();
        let simulated = device.simulated;
        let capabilities = &device.manifest.capabilities;
        let mut session = state.session(device_id);
        let was_sleeping = session.output.sleeping;
        let previous_frame_policy = session.output.frame_policy.clone();
        let previous_vibration = session.output.vibration_enabled;
        let previous_configuration = session.output.device_configuration.clone();
        update(&mut session.output);
        let vibration_changed = previous_vibration != session.output.vibration_enabled
            || previous_configuration != session.output.device_configuration;
        if let Some(policy) = &session.output.frame_policy {
            policy.validate(capabilities)?;
        }
        if vibration_changed
            && (session.output.vibration_enabled.is_some()
                || !session.output.device_configuration.is_empty())
        {
            let capability = capabilities
                .vibration
                .as_ref()
                .filter(|v| v.toggle)
                .ok_or("Vibration is unsupported")?;
            if !simulated && !capability.writable {
                return Err(capability.unavailable_reason.clone());
            }
            if session.output.vibration_enabled.is_none() {
                return Err("Choose vibration on or off when applying device configuration".into());
            }
            capability
                .validate_configuration(&session.output.device_configuration)
                .map_err(|e| e.to_string())?;
            if !simulated {
                if let Some(error) =
                    self.vibration_firmware_error(device_id, capability, &device.variant_id)
                {
                    return Err(error);
                }
            }
        }
        let frame_policy_changed = previous_frame_policy != session.output.frame_policy;
        if session.output.sleeping && !capabilities.sleep {
            return Err("Device does not support sleep".into());
        }
        if session.output.brightness > 100 || session.output.lighting_brightness > 100 {
            return Err("brightness must be 0..100".into());
        }
        if !session.output.lighting_colors.is_empty()
            && session.output.lighting_colors.len() != capabilities.lighting_channels.len()
        {
            return Err("lighting does not match device capabilities".into());
        }
        session.output.lighting_colors =
            session.output.colors(capabilities.lighting_channels.len());
        let output = session.output.clone();
        let supports_brightness = capabilities.brightness;
        let supports_lighting = capabilities.lighting;
        let managed = self.power.manages(device_id);
        let sleeping = session.output.sleeping;
        let waking_manual_sleep = was_sleeping && !sleeping;
        let mut sessions = state.sessions.clone();
        sessions.insert(key, session);
        self.profiles.write_preference(
            crate::profile_database::LocalPreference::Sessions,
            &sessions,
        )?;
        state.sessions = sessions;
        drop(state);
        if !was_sleeping && sleeping {
            self.builtin_state.cancel_device(device_id);
        }
        if waking_manual_sleep {
            self.power_wake(device_id);
        } else {
            self.refresh_power();
        }
        if !simulated && (!managed || frame_policy_changed || vibration_changed) {
            if was_sleeping != sleeping || frame_policy_changed || vibration_changed {
                if let Some(epoch) = self
                    .connection_epochs
                    .lock()
                    .unwrap()
                    .get(device_id)
                    .copied()
                {
                    self.device_commands
                        .send(devices::Request::RestoreDevice(device_id.into(), epoch))
                        .map_err(|error| error.to_string())?;
                }
            } else if !sleeping {
                if supports_brightness {
                    self.send_device_command(
                        device_id,
                        hardware_core::DeviceCommand::Brightness(output.brightness),
                    )
                    .map_err(|error| error.to_string())?;
                }
                if supports_lighting {
                    self.send_device_command(
                        device_id,
                        hardware_core::DeviceCommand::Lighting {
                            brightness: output.lighting_brightness,
                            colors: output.lighting_colors,
                        },
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
        }
        if was_sleeping != sleeping {
            devices::sync_plugins(self);
        }
        self.emit("device-output", Some(device_id.into()), None, None);
        Ok(self.snapshot())
    }
    /// Diagnostic image upload. Normal editor output is restored on reconnect or profile change
    pub fn upload_image(
        &self,
        device_id: &str,
        control_id: Option<&str>,
        path: &std::path::Path,
        format: &str,
    ) -> Result<bool, String> {
        use std::io::Read;
        let state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|d| d.id == device_id && !d.simulated)
            .ok_or("physical device not connected")?
            .clone();
        drop(state);
        if !device
            .manifest
            .capabilities
            .image_formats
            .iter()
            .any(|f| f == format)
        {
            return Err("image format is unsupported".into());
        }
        let size = if let Some(id) = control_id {
            let control = device.manifest.control(id).map_err(|e| e.to_string())?;
            [
                control.image_width.ok_or("control has no display")?,
                control.image_height.ok_or("control has no display")?,
            ]
        } else {
            device
                .manifest
                .capabilities
                .background_size
                .ok_or("device has no background display")?
        };
        let epoch = *self
            .connection_epochs
            .lock()
            .unwrap()
            .get(device_id)
            .ok_or("device disconnected")?;
        let mut bytes = vec![];
        std::fs::File::open(path)
            .map_err(|e| e.to_string())?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > media::MAX_SOURCE {
            return Err("Media source exceeds 16 MiB".into());
        }
        let source_format = media::detect(&bytes)?;
        if !device
            .manifest
            .capabilities
            .media_formats
            .iter()
            .any(|f| f == source_format || (source_format == "svg" && f == "png"))
        {
            return Err("Media format is unsupported by this model".into());
        }
        if ["gif", "mp4"].contains(&source_format) {
            let (asset, lease) = self.profiles.import_asset_pinned(
                path,
                &Default::default(),
                "Temporary device playback",
            )?;
            let clip = self
                .profiles
                .clip(&asset)?
                .ok_or("Media changed during import")?;
            if clip.source_format != source_format {
                return Err("Media changed during import".into());
            }
            self.device_commands
                .send(devices::Request::Playback {
                    device: device_id.into(),
                    control: control_id.map(String::from),
                    asset,
                    lease,
                    format: format.into(),
                    epoch,
                })
                .map_err(|e| e.to_string())?;
            return Ok(true);
        }
        let data = hardware_core::render::render(
            &bytes,
            size[0],
            size[1],
            device.manifest.layout.rotation,
            format,
        )
        .map_err(|e| e.to_string())?;
        let command = if let Some(id) = control_id {
            hardware_core::DeviceCommand::Image {
                control_id: id.into(),
                data,
            }
        } else {
            hardware_core::DeviceCommand::Background {
                data,
                x: 0,
                y: 0,
                width: size[0],
                height: size[1],
            }
        };
        self.device_commands
            .send(devices::Request::Command(device_id.into(), epoch, command))
            .map_err(|e| e.to_string())?;
        Ok(false)
    }
    // Export profiles
    pub fn export_profiles(&self, path: &std::path::Path) -> Result<(), String> {
        self.profiles.export(path)
    }

    // Apply supported device lighting brightness and color settings
    pub fn lighting(
        &self,
        device_id: &str,
        brightness: u8,
        colors: Vec<[u8; 3]>,
    ) -> Result<(), String> {
        let state = self.snapshot();
        let device = state
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or("device not connected")?;
        if !device.manifest.capabilities.lighting
            || colors.len() != device.manifest.capabilities.lighting_channels.len()
        {
            return Err("lighting does not match device capabilities".into());
        }
        self.update_device_output(device_id, |output| {
            output.lighting_brightness = brightness;
            output.lighting_colors = colors;
        })
        .map(|_| ())
    }
    // Import profiles
    pub fn import_profiles(&self, path: &std::path::Path) -> Result<Snapshot, String> {
        let preview = self.prepare_profile_import(path)?;
        let preview = match self.update_profile_import(&preview.id, "replace", vec![]) {
            Ok(preview) => preview,
            Err(error) => {
                self.cancel_profile_import(&preview.id);
                return Err(error);
            }
        };
        if !preview.issues.is_empty() {
            let message = preview
                .issues
                .iter()
                .map(|issue| issue.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            self.cancel_profile_import(&preview.id);
            return Err(format!(
                "Import requires repairs in the desktop review wizard: {message}"
            ));
        }
        match self.commit_profile_import(&preview.id, preview.expected_revision) {
            Ok(snapshot) => Ok(snapshot),
            Err(error) => {
                self.cancel_profile_import(&preview.id);
                Err(error)
            }
        }
    }
    // Import asset
    pub fn import_asset(&self, path: &std::path::Path) -> Result<String, String> {
        let (asset, lease) = self.profiles.import_asset_pinned(
            path,
            &Default::default(),
            "Imported media pending selection",
        )?;
        let mut selections = self.media_selections.lock().unwrap();
        if selections.len() >= 128 {
            return Err("Close an unfinished media selection first".into());
        }
        selections.insert(asset.clone(), lease);
        Ok(asset)
    }
    // Read elapsed time on the runtime’s shared media clock
    pub(crate) fn media_elapsed(&self) -> std::time::Duration {
        self.media_start.elapsed()
    }
    // Report whether there is background override
    fn has_background_override(binding: &Binding) -> bool {
        binding.background_source == Some(profiles::BackgroundSource::Palette)
            || binding.custom_background
            || binding.asset.is_some()
            || !matches!(
                binding.color.to_ascii_lowercase().as_str(),
                "#00000000" | "#c61e3200"
            )
    }

    // Apply device theme colors only where the binding’s palette choices permit overrides
    fn themed_button_binding(
        control: &hardware_core::Control,
        binding: &Binding,
        theme: stream_dock_manager_plugin_host::DeviceThemePreferences,
        surface: &str,
        ink: &str,
    ) -> Option<Binding> {
        let explicit_palette =
            binding.background_source == Some(profiles::BackgroundSource::Palette);
        if control.image_width.is_none() || control.image_height.is_none() {
            return None;
        }
        if !explicit_palette
            && (!theme.sync
                || !matches!(control.kind.as_str(), "button" | "touch")
                || Self::has_background_override(binding))
        {
            return None;
        }
        let mut binding = binding.clone();
        binding.color = if !explicit_palette && theme.transparent_buttons {
            "#00000000".into()
        } else {
            surface.into()
        };
        if explicit_palette {
            binding.asset = None;
        }
        // This render-only binding owns the background, just like a saved override
        binding.custom_background = true;
        if !matches!(binding.action, actions::Action::Plugin { .. })
            && binding.appearance.is_none()
            && binding.composition.is_none()
            && !binding.title.is_empty()
        {
            binding.appearance = Some(Box::new(hardware_core::appearance::Appearance {
                foreground: ink.into(),
                ..Default::default()
            }));
        }
        Some(binding)
    }

    // Render control base at
    fn render_control_base_at(
        &self,
        control: &hardware_core::Control,
        binding: Option<&Binding>,
        rotation: u16,
        media_elapsed: std::time::Duration,
        clips: Option<&profiles::ClipCycle>,
    ) -> Result<media::SharedBytes, String> {
        let width = control.image_width.ok_or("control has no image surface")?;
        let height = control.image_height.ok_or("control has no image surface")?;
        let color = binding.map(|b| b.color.as_str()).unwrap_or("#00000000");
        let rgba =
            hardware_core::appearance::background_color(color).ok_or("Invalid background color")?;
        if let Some(asset) = binding.and_then(|b| b.asset.as_ref()).filter(|asset| {
            !binding.is_some_and(|b| b.disabled) || self.profiles.asset(asset).is_ok()
        }) {
            let bytes = self.profiles.render_media_shared_in_cycle(
                asset,
                media_elapsed,
                width,
                height,
                rotation,
                "png",
                &binding.unwrap().media,
                clips,
            )?;
            return hardware_core::render::over_color(&bytes, rgba)
                .map(media::shared_bytes)
                .map_err(|error| error.to_string());
        }
        self.cached_render(
            format!(
                "tile:{width}:{height}:{rotation}:{color}:{}",
                binding.map(|b| b.title.as_str()).unwrap_or("")
            ),
            || {
                hardware_core::render::tile_rgba(
                    width,
                    height,
                    rgba,
                    binding.map(|b| b.title.as_str()).unwrap_or(""),
                    rotation,
                    "png",
                )
                .map(media::shared_bytes)
                .map_err(|e| e.to_string())
            },
        )
    }
    // Render binding
    pub fn render_binding(
        &self,
        control: &hardware_core::Control,
        binding: Option<&Binding>,
        rotation: u16,
        feedback: &str,
        visual: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>, String> {
        self.render_binding_at(
            control,
            binding,
            rotation,
            feedback,
            visual,
            self.media_elapsed(),
            true,
        )
        .map(|bytes| self.owned_render_bytes(bytes))
    }
    #[allow(clippy::too_many_arguments)]
    // Render binding at
    fn render_binding_at(
        &self,
        control: &hardware_core::Control,
        binding: Option<&Binding>,
        rotation: u16,
        feedback: &str,
        visual: Option<&serde_json::Value>,
        media_elapsed: std::time::Duration,
        live_fallback: bool,
    ) -> Result<media::SharedBytes, String> {
        self.render_binding_in_cycle(
            control,
            binding,
            rotation,
            feedback,
            visual,
            media_elapsed,
            live_fallback,
            None,
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    // Render binding in cycle
    fn render_binding_in_cycle(
        &self,
        control: &hardware_core::Control,
        binding: Option<&Binding>,
        rotation: u16,
        feedback: &str,
        visual: Option<&serde_json::Value>,
        media_elapsed: std::time::Duration,
        live_fallback: bool,
        clips: Option<&profiles::ClipCycle>,
        navigation_ink: Option<&str>,
    ) -> Result<media::SharedBytes, String> {
        let width = control.image_width.ok_or("No display surface")?;
        let height = control.image_height.ok_or("No display surface")?;
        let binding = binding.filter(|binding| binding.has_content());
        let color = binding
            .map(|binding| binding.color.as_str())
            .unwrap_or("#00000000");
        let rgba =
            hardware_core::appearance::background_color(color).ok_or("Invalid background color")?;
        let composition = binding.and_then(|b| b.composition.as_ref());
        let fallback = if visual.is_none() && live_fallback && composition.is_none() {
            binding.filter(|b| !b.disabled).and_then(|b| {
                if let actions::Action::Plugin {
                    plugin_id,
                    action_id,
                    ..
                } = &b.action
                {
                    self.plugins
                        .lock()
                        .unwrap()
                        .action_icon(plugin_id, action_id)
                } else {
                    None
                }
            })
        } else {
            None
        };
        let mut visual = visual.or(fallback.as_ref()).cloned();
        if let Some(visual) = &mut visual {
            let replace_background = binding.is_some_and(Self::has_background_override);
            if replace_background || visual["paletteTile"] != true && rgba[3] < 255 {
                visual["background"] = serde_json::json!("#00000000");
                if visual["layout"].is_object() {
                    visual["layout"]["background"] = serde_json::json!("#00000000");
                }
            }
            if replace_background {
                hardware_core::plugin_display::remove_background_layers(visual);
            }
        }
        if let Some(composition) = composition {
            let sources = self.display_sources(
                control,
                binding.unwrap(),
                feedback,
                visual.as_ref(),
                media_elapsed,
                false,
            )?;
            let composed = composition.visual(
                &sources,
                if navigation_ink.is_some() {
                    "#00000000"
                } else {
                    color
                },
                visual
                    .as_ref()
                    .and_then(|v| v["state"].as_u64())
                    .map(|s| s as u32),
            )?;
            if let Some(ink) = navigation_ink {
                let foreground = self.render_visual(&composed, width, height, 0)?;
                let foreground = hardware_core::render::layer_pixels(&foreground, width, height)
                    .map_err(|e| e.to_string())?;
                let pixels = hardware_core::render::over_image_rgba(
                    navigation_background(width, height, rgba, ink),
                    &foreground,
                )
                .map_err(|e| e.to_string())?;
                let image = image::DynamicImage::ImageRgba8(pixels);
                let image = match rotation {
                    0 => image,
                    90 => image.rotate90(),
                    180 => image.rotate180(),
                    270 => image.rotate270(),
                    _ => return Err("Invalid rotation".into()),
                };
                return hardware_core::render::encode_pixels(image, "png")
                    .map(media::shared_bytes)
                    .map_err(|e| e.to_string());
            }
            return self.render_visual(&composed, width, height, rotation);
        }
        let has_media = binding.and_then(|b| b.asset.as_ref()).is_some_and(|asset| {
            !binding.is_some_and(|b| b.disabled) || self.profiles.asset(asset).is_ok()
        });
        let overlays = binding.is_some_and(|b| b.appearance.is_some())
            || !feedback.is_empty()
            || (has_media && visual.is_none() && binding.is_some_and(|b| !b.title.is_empty()));
        if !overlays && visual.is_none() {
            return self.render_control_base_at(control, binding, rotation, media_elapsed, clips);
        }
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err("Invalid layer dimensions".into());
        }
        let media_pixels = || {
            let binding = binding.ok_or("Missing media binding")?;
            let bytes = self.profiles.render_media_shared_in_cycle(
                binding.asset.as_deref().ok_or("Missing media asset")?,
                media_elapsed,
                width,
                height,
                0,
                "png",
                &binding.media,
                clips,
            )?;
            let pixels = hardware_core::render::layer_pixels(&bytes, width, height)
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(hardware_core::render::over_color_rgba(pixels, rgba))
        };
        let base = if let Some(mut visual) = visual {
            if visual["layout"]["id"] == "palette-fallback"
                && u32::from(width) * 10 > u32::from(height) * 13
            {
                let icon = u32::from(height) * 7 / 10;
                visual["layout"]["items"][0]["rect"] =
                    serde_json::json!([4, 15, icon * 200 / u32::from(width), 70]);
                visual["layout"]["items"][1]["rect"] = serde_json::json!([76, 10, 120, 80]);
            }
            if let Some(style) = binding.and_then(|b| b.plugin_appearance.as_ref()) {
                visual = hardware_core::plugin_display::styled_visual(&visual, style)
                    .map_err(|e| e.to_string())?;
            }
            let bytes = self.render_visual(&visual, width, height, 0)?;
            let pixels = hardware_core::render::layer_pixels(&bytes, width, height)
                .map_err(|e| e.to_string())?;
            if has_media {
                hardware_core::render::over_image_rgba(media_pixels()?, &pixels)
                    .map_err(|error| error.to_string())?
            } else {
                hardware_core::render::over_color_rgba(pixels, rgba)
            }
        } else if has_media {
            media_pixels()?
        } else if let Some(ink) = navigation_ink {
            navigation_background(width, height, rgba, ink)
        } else {
            image::RgbaImage::from_pixel(width.into(), height.into(), image::Rgba(rgba))
        };
        if !overlays {
            let image = image::DynamicImage::ImageRgba8(base);
            let image = match rotation {
                0 => image,
                90 => image.rotate90(),
                180 => image.rotate180(),
                270 => image.rotate270(),
                _ => return Err("Invalid rotation".into()),
            };
            return hardware_core::render::encode_pixels(image, "png")
                .map(media::shared_bytes)
                .map_err(|e| e.to_string());
        }
        let appearance = binding
            .and_then(|b| b.appearance.clone())
            .unwrap_or_default();
        let title = binding.map(|b| b.title.as_str()).unwrap_or("");
        let cache_key = format!(
            "composed-pixels-v1:{width}:{height}:{rotation}:{}:{title}:{feedback}:{}:{}",
            serde_json::to_string(&appearance).unwrap_or_default(),
            {
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                base.as_raw().hash(&mut hash);
                hash.finish()
            },
            hardware_core::fonts::generation()
        );
        self.cached_render(cache_key, || {
            let pixels = hardware_core::appearance::compose_pixels(
                base,
                width,
                height,
                title,
                &appearance,
                feedback,
                rotation,
            )
            .map_err(|e| e.to_string())?;
            hardware_core::render::encode_pixels(pixels, "png")
                .map(media::shared_bytes)
                .map_err(|e| e.to_string())
        })
    }
    #[cfg(test)]
    // Render bound surface
    pub(crate) fn render_bound_surface(
        &self,
        control: &hardware_core::Control,
        binding: Option<&Binding>,
        rotation: u16,
        device: &str,
        scope: &str,
        source: &str,
    ) -> Result<media::SharedBytes, String> {
        let context = self.render_scene(device)?;
        self.render_bound_surface_in_scene(control, binding, rotation, scope, source, &context)
    }
    // Render bound surface in scene
    pub(crate) fn render_bound_surface_in_scene(
        &self,
        control: &hardware_core::Control,
        binding: Option<&Binding>,
        rotation: u16,
        scope: &str,
        source: &str,
        context: &render_scene::RenderSceneContext,
    ) -> Result<media::SharedBytes, String> {
        let draft = context
            .draft
            .as_ref()
            .and_then(|draft| draft.bindings.get(source));
        let effective = draft
            .is_none()
            .then(|| context.overrides.get(source))
            .flatten();
        let binding = draft
            .or_else(|| effective.map(|v| &v.binding))
            .or(binding)
            .filter(|b| b.has_content());
        let saved_binding = binding;
        let navigation = context.keyboard_navigation
            && scope == context.scope
            && effective.is_none()
            && (1..=6).any(|i| source == format!("key-{i}"));
        let navigation_default = binding.filter(|_| navigation).map(|b| {
            let mut b = b.clone();
            if b.appearance.is_none() && b.composition.is_none() {
                b.appearance = Some(Box::new(hardware_core::appearance::Appearance {
                    position: "center".into(),
                    align: "center".into(),
                    foreground: context.device_ink.clone(),
                    ..Default::default()
                }));
            }
            b
        });
        let binding = navigation_default.as_ref().or(binding);
        let navigation_ink = (navigation
            && binding
                .is_some_and(|b| b.background_source == Some(profiles::BackgroundSource::Palette)))
        .then_some(context.device_ink.as_str());
        let themed_binding = binding.and_then(|binding| {
            let surface = match &binding.action {
                actions::Action::Plugin { plugin_id, .. }
                    if binding.background_source != Some(profiles::BackgroundSource::Palette) =>
                {
                    context.plugin_surfaces.get(plugin_id)
                }
                _ => None,
            };
            Self::themed_button_binding(
                control,
                binding,
                context.device_theme,
                surface.unwrap_or(&context.device_surface),
                &context.device_ink,
            )
        });
        let binding = themed_binding.as_ref().or(binding);
        let scope = effective.map(|v| v.scope.as_str()).unwrap_or(scope);
        // Compatibility callers may supply a preview scope. Never borrow an
        // active plugin's pixels for a different profile/page on the same control
        let captured_scope = effective
            .map(|v| v.scope.as_str())
            .unwrap_or(&context.scope);
        let matches_scope = scope == captured_scope;
        let dynamic_visual = matches_scope
            && binding.is_some_and(|b| !b.disabled)
            && context.visuals.contains_key(source);
        let visual = if matches_scope && binding.is_some_and(|b| !b.disabled) {
            context
                .visuals
                .get(source)
                .map(AsRef::as_ref)
                .or_else(|| context.fallbacks.get(source))
        } else {
            None
        };
        let text = binding
            .map(|b| {
                context.feedback.text(
                    b.feedback.as_deref().unwrap_or("off"),
                    &context.device,
                    scope,
                    source,
                )
            })
            .unwrap_or_default();
        let alternate = binding
            .filter(|_| {
                saved_binding.is_some_and(|saved| {
                    context
                        .feedback
                        .icon_toggled(&context.device, scope, source, saved)
                })
            })
            .map(|binding| {
                let mut binding = binding.clone();
                binding.appearance = binding
                    .appearance
                    .map(|a| Box::new(a.with_icon_state(true)));
                binding
            });
        let rendered_binding = alternate.as_ref().or(binding);
        if let Some(binding) = rendered_binding.filter(|binding| {
            !binding.disabled
                && binding.title.is_empty()
                && binding.appearance.is_none()
                && binding.composition.is_none()
                && visual.is_none()
                && text.is_empty()
        }) {
            if let Some(asset) = binding.asset.as_deref() {
                if let Some(clip) = context.clips.get(&self.profiles, asset)? {
                    return self.render_animation_frame_shared(
                        &clip,
                        binding.media.frame_index(&clip, context.media_elapsed),
                        [
                            control.image_width.ok_or("No display surface")?,
                            control.image_height.ok_or("No display surface")?,
                        ],
                        rotation,
                        &binding.media,
                        &context.formats,
                        Some(
                            hardware_core::appearance::background_color(&binding.color)
                                .ok_or("Invalid background color")?,
                        ),
                    );
                }
            }
        }
        let mut pixels = rendered_binding.cloned();
        if let Some(binding) = pixels.as_mut() {
            if let Some(asset) = binding.asset.as_ref() {
                binding.asset = Some(
                    match self.profiles.media_pixel_identity_in_cycle(
                        asset,
                        &binding.media,
                        context.media_elapsed,
                        Some(&context.clips),
                    ) {
                        Ok(identity) => identity,
                        Err(_) if binding.disabled => "unavailable".into(),
                        Err(error) => return Err(error),
                    },
                );
            }
            binding.action = actions::Action::None;
            binding.media.min_fps = None;
            binding.media.max_fps = None;
            binding.media.speed = 1.0;
            binding.media.trim_start_ms = 0;
            binding.media.trim_end_ms = None;
        }
        use sha2::{Digest, Sha256};
        let key = format!(
            "surface-v1:{}",
            hex::encode(Sha256::digest(
                serde_json::to_vec(&serde_json::json!([
                    control.image_width,
                    control.image_height,
                    rotation,
                    pixels,
                    text,
                    visual,
                    dynamic_visual,
                    context.formats,
                    context.contract_identity,
                    "static-render-source-v1",
                    navigation_ink,
                    hardware_core::fonts::generation()
                ]))
                .map_err(|e| e.to_string())?
            ))
        );
        self.cached_render(key, || {
            self.metrics.record("surface_render_invocations", 1.0);
            let bytes = self.render_binding_in_cycle(
                control,
                rendered_binding,
                rotation,
                &text,
                visual,
                context.media_elapsed,
                false,
                Some(&context.clips),
                navigation_ink,
            )?;
            self.encode_surface_validated_png(
                bytes,
                binding,
                &context.formats,
                dynamic_visual || !text.is_empty(),
            )
        })
    }
    // Preview surfaces using the current src state
    pub fn preview_surfaces(&self, device_id: Option<&str>) -> Result<SurfacePreview, String> {
        use base64::Engine;
        self.preview_surfaces_encoded(device_id, |bytes| {
            format!(
                "data:{};base64,{}",
                encoding::mime(&bytes),
                base64::engine::general_purpose::STANDARD.encode(&**bytes)
            )
        })
    }
    // Preview surfaces binary using the current src state
    pub fn preview_surfaces_binary(
        &self,
        device_id: &str,
    ) -> Result<(SurfacePreview, std::collections::BTreeMap<String, Vec<u8>>), String> {
        use sha2::{Digest, Sha256};
        let mut frames = std::collections::BTreeMap::new();
        let preview = self.preview_surfaces_encoded(Some(device_id), |bytes| {
            let id = hex::encode(Sha256::digest(&**bytes));
            frames
                .entry(id.clone())
                .or_insert_with(|| self.owned_render_bytes(bytes));
            id
        })?;
        Ok((preview, frames))
    }
    // Preview surfaces encoded using the current src state
    fn preview_surfaces_encoded(
        &self,
        device_id: Option<&str>,
        mut encode: impl FnMut(media::SharedBytes) -> String,
    ) -> Result<SurfacePreview, String> {
        let state = self.snapshot();
        let session = state.session(device_id.unwrap_or(""));
        let Some(profile) = state.profiles.iter().find(|p| p.id == session.profile_id) else {
            return Ok(SurfacePreview {
                configuration_revision: state.configuration_revision,
                surfaces: Default::default(),
                trigger_descriptions: Default::default(),
                background: None,
                animated: false,
            });
        };
        let manifests = hardware_registry::manifests();
        let manifest = state
            .devices
            .iter()
            .find(|device| device_id.is_some_and(|id| device.id == id))
            .map(|device| &device.manifest)
            .or_else(|| manifests.iter().find(|m| m.model_id == profile.model_id))
            .ok_or("unsupported model")?;
        let page = profile
            .pages
            .iter()
            .find(|p| p.id == session.page_id)
            .ok_or("active page missing")?;
        let device = state
            .devices
            .iter()
            .find(|device| {
                device_id.map_or(device.model_id == profile.model_id, |id| device.id == id)
            })
            .map(|d| d.id.as_str())
            .unwrap_or("");
        let context = self.render_scene_for_target(device, true)?;
        if !context.matches_snapshot(&state) {
            return Err("Render scene changed during capture".into());
        }
        let mut output = std::collections::BTreeMap::new();
        let mut animated = !self
            .vendor_animations
            .lock()
            .unwrap()
            .active(device, true)
            .is_empty();
        for control in &manifest.controls {
            if control.image_slot.is_none() {
                continue;
            }
            let source = context.surface_source(manifest, &control.id);
            let binding = page.bindings.get(source);
            if let Some(asset) = binding.and_then(|b| b.asset.as_ref()).filter(|asset| {
                !binding.is_some_and(|b| b.disabled) || self.profiles.asset(asset).is_ok()
            }) {
                animated |= self
                    .profiles
                    .clip(asset)?
                    .is_some_and(|clip| clip.frames.len() > 1);
            }
            let bytes = self.render_bound_surface_in_scene(
                control,
                binding,
                manifest.layout.rotation,
                &format!("{}:{}", profile.id, page.id),
                source,
                &context,
            )?;
            let bytes = self.preview_image(bytes, manifest.layout.rotation)?;
            output.insert(control.id.clone(), encode(bytes));
        }
        let background = if let Some(size) = manifest.capabilities.background_size {
            let bytes = self.render_scene_wallpaper(
                &context,
                Some(profile),
                size,
                manifest.layout.rotation,
                &manifest.capabilities.image_formats,
            )?;
            animated |= context
                .background_media(Some(profile))
                .is_some_and(|(asset, _)| {
                    self.profiles
                        .clip(asset)
                        .ok()
                        .flatten()
                        .is_some_and(|clip| clip.frames.len() > 1)
                });
            Some(encode(self.preview_image(bytes, manifest.layout.rotation)?))
        } else {
            None
        };
        if !context.current(self) {
            return Err("Render scene changed during rendering".into());
        }
        Ok(SurfacePreview {
            configuration_revision: state.configuration_revision,
            surfaces: output,
            trigger_descriptions: context
                .visuals
                .iter()
                .filter_map(|(control, visual)| {
                    serde_json::from_value::<std::collections::BTreeMap<String, String>>(
                        visual["triggerDescriptions"].clone(),
                    )
                    .ok()
                    .map(|descriptions| (control.clone(), descriptions))
                })
                .collect(),
            background,
            animated,
        })
    }
    // Cancel macros
    pub fn cancel_macros(&self) {
        keyboard_macro::test_session::cancel_all();
        self.builtin_state.cleanup(None);
        self.emit("sessions", None, None, None);
        let _ = self.device_commands.send(devices::Request::Restore);
        self.cancellation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    // Shut down the src worker
    pub fn shutdown(&self) {
        keyboard_macro::recorder::cancel_all();
        keyboard_macro::test_session::cancel_all();
        self.cancel_all_keyboard_previews();
        self.power.stop();
        self.audio_signal.stop();
        self.setup_capabilities.cancel_all();
        self.sampling_signal.stop();
        self.preparations.cancel_all();
        desktop_input::shutdown();
        if self
            .stopping
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        // The actor remains alive until benchmark cancellation has requested
        // restoration. A stuck OS HID write cannot be preempted portably
        if !self.benchmark_shutdown(std::time::Duration::from_secs(3)) {
            self.emit("benchmark-error", None, None,
                Some("Benchmark cancellation exceeded shutdown grace period; physical output cleanup may require reconnect.".into()));
        }
        #[cfg(target_os = "linux")]
        wayland::shutdown();
        self.cancel_macros();
        self.updates.cancel_all();
        self.runtimes.pause_downloads();
        let (reply, done) = mpsc::channel();
        if self
            .device_commands
            .send(devices::Request::Shutdown(reply))
            .is_ok()
        {
            let _ = done.recv_timeout(std::time::Duration::from_secs(2));
        }
        let waiters = {
            let mut host = self.plugins.lock().unwrap();
            host.shutdown();
            host.shutdown_waiters()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while waiters
            .iter()
            .any(|done| !done.load(std::sync::atomic::Ordering::Acquire))
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        self.plugin_logs.shutdown();
        if let Some(worker) = self.settings_writer.lock().unwrap().take() {
            if worker.thread().id() != std::thread::current().id() {
                let _ = worker.join();
            }
        }
    }
    // Dispatch the src request
    pub(crate) fn dispatch(&self, device_id: &str, event: &ControlEvent) {
        let Some(connection_epoch) = self
            .connection_epochs
            .lock()
            .unwrap()
            .get(device_id)
            .copied()
        else {
            return;
        };
        if self
            .transition_inputs
            .lock()
            .unwrap()
            .discard(device_id, connection_epoch, event)
        {
            self.metrics.record("transition_inputs_discarded", 1.0);
            return;
        }
        let admitted_at = std::time::Instant::now();
        if self.locate_input(device_id, connection_epoch, event) {
            return;
        }
        if self.handle_panel_gesture(device_id, event) {
            return;
        }
        let key = format!("{device_id}|{}", event.control_id);
        let state = self.snapshot();
        if state
            .devices
            .iter()
            .find(|device| device.id == device_id)
            .and_then(|device| device.manifest.control(&event.control_id).ok())
            .is_none_or(|control| control.kind == "display")
        {
            return;
        }
        let session = state.session(device_id);
        if self
            .power
            .effective(device_id, connection_epoch, &session.output)
            .sleeping
        {
            return;
        }
        let effective = self
            .builtin_state
            .effective_binding(device_id, &event.control_id, &state)
            .or_else(|| {
                state
                    .binding_overrides
                    .get(device_id)
                    .and_then(|values| values.get(&event.control_id))
                    .cloned()
            })
            .or_else(|| {
                state
                    .profiles
                    .iter()
                    .find(|p| p.id == session.profile_id)
                    .and_then(|p| p.pages.iter().find(|p| p.id == session.page_id))
                    .and_then(|p| p.bindings.get(&event.control_id))
                    .map(|binding| builtins::EffectiveBinding {
                        profile_id: session.profile_id.clone(),
                        page_id: session.page_id.clone(),
                        scope: format!("{}:{}", session.profile_id, session.page_id),
                        binding: binding.clone(),
                    })
            });
        let input = self.dial_responses.lock().unwrap().transform(
            device_id,
            &event.control_id,
            connection_epoch,
            serde_json::to_string(&effective).unwrap_or_default(),
            effective
                .as_ref()
                .and_then(|value| value.binding.dial_response),
            &event.input,
            std::time::Instant::now(),
        );
        let mut resolved = effective.as_ref().map(|value| {
            (
                if value.binding.disabled {
                    actions::Action::None
                } else {
                    value.binding.action.clone()
                },
                value.scope.clone(),
            )
        });
        let (action, scope) =
            if matches!(event.input, hardware_core::Input::Button { pressed: false }) {
                let capture = self.builtin_state.captures.lock().unwrap().remove(&key);
                if let Some(c) = capture {
                    c.pressed.store(false, std::sync::atomic::Ordering::SeqCst);
                    if matches!(c.action, actions::Action::Plugin { .. }) {
                        (c.action, c.scope)
                    } else {
                        return;
                    }
                } else {
                    return;
                }
            } else {
                {
                    let paused = self.builtin_state.paused.lock().unwrap();
                    if let Some(unlock) = paused.get(device_id) {
                        if unlock != &event.control_id {
                            return;
                        }
                        resolved = Some((
                            actions::Action::Builtin {
                                action: Box::new(builtins::BuiltinAction::PauseBindings {
                                    operation: "resume".into(),
                                }),
                            },
                            format!("{}:{}", session.profile_id, session.page_id),
                        ));
                    }
                }
                let Some((action, scope)) = resolved else {
                    return;
                };
                if matches!(event.input, hardware_core::Input::Button { pressed: true }) {
                    if self
                        .builtin_state
                        .captures
                        .lock()
                        .unwrap()
                        .get(&key)
                        .is_some_and(|c| c.pressed.load(std::sync::atomic::Ordering::SeqCst))
                    {
                        return;
                    }
                    self.builtin_state.captures.lock().unwrap().insert(
                        key,
                        builtins::Capture {
                            action: action.clone(),
                            scope: scope.clone(),
                            pressed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                        },
                    );
                }
                (action, scope)
            };
        if matches!(&event.input,hardware_core::Input::Touch{phase,..}if phase!="end") {
            return;
        }
        let pressed = self
            .builtin_state
            .captures
            .lock()
            .unwrap()
            .get(&format!("{device_id}|{}", event.control_id))
            .map(|c| c.pressed.clone());
        let activation_policy = effective
            .as_ref()
            .map(|value| value.binding.activation_policy)
            .unwrap_or_default();
        let mut request = actions::Request {
            manual: false,
            activation_policy,
            icon_signature: None,
            pressed,
            action,
            device_id: device_id.into(),
            control_id: event.control_id.clone(),
            binding_scope: scope,
            queued_at: admitted_at,
            connection_epoch: Some(connection_epoch),
            input,
            cancellation_generation: self.cancellation.load(std::sync::atomic::Ordering::SeqCst),
        };
        request.icon_signature = feedback::request_icon_signature(self, &request);
        if matches!(&request.action,actions::Action::Builtin{action}if matches!(**action,builtins::BuiltinAction::CancelMacros))
        {
            self.cancel_macros();
            request.cancellation_generation =
                self.cancellation.load(std::sync::atomic::Ordering::SeqCst);
            feedback::icon_succeeded(self, &request);
            feedback::finished(self, &request, "Completed", false);
            return;
        }
        if matches!(
            request.input,
            hardware_core::Input::Button { pressed: false }
        ) {
            if let actions::Action::Plugin {
                plugin_id,
                action_id,
                settings,
                user_desired_state,
                ..
            } = &request.action
            {
                let result = self.plugins.lock().unwrap().dispatch_with_state(
                    plugin_id,
                    action_id,
                    device_id,
                    &request.control_id,
                    &request.binding_scope,
                    &request.input,
                    settings,
                    *user_desired_state,
                );
                if let Err(error) = result {
                    self.emit("action-error", Some(device_id.into()), None, Some(error));
                }
                return;
            }
        }
        if let Err(error) = self.queue_action(request) {
            self.emit("action-error", Some(device_id.into()), None, Some(error));
        }
    }
    // Admit an action request against transition state before submitting it to the execution queue
    fn queue_action(&self, request: actions::Request) -> Result<(), String> {
        feedback::queued(self, &request);
        let mut input_gate = self.transition_inputs.lock().unwrap();
        if !request.manual
            && !input_gate.permits_request(
                &request.device_id,
                request.connection_epoch,
                request.queued_at,
            )
        {
            if let Some(pressed) = &request.pressed {
                pressed.store(false, std::sync::atomic::Ordering::SeqCst);
            }
            if matches!(
                request.input,
                hardware_core::Input::Button { pressed: true }
            ) {
                input_gate.suppress(&request.device_id, &request.control_id);
            }
            drop(input_gate);
            feedback::finished(self, &request, "Cancelled", false);
            return Ok(());
        }
        let result = self.action_sender.try_send(request);
        drop(input_gate);
        if let Err(error) = result {
            let failed = match &error {
                mpsc::TrySendError::Full(r) | mpsc::TrySendError::Disconnected(r) => r,
            };
            feedback::finished(self, failed, "Failed", false);
            return Err(error.to_string());
        }
        Ok(())
    }
}

impl Drop for Runtime {
    // Release this src owner when it leaves scope
    fn drop(&mut self) {
        self.power.stop();
        self.audio_signal.stop();
        self.sampling_signal.stop();
    }
}

#[cfg(test)]
mod bump_tests;

// Take restart recovery request
fn take_restart_recovery_request(
    storage: &stream_dock_manager_storage::Storage,
) -> Result<Option<bool>, String> {
    storage.transaction(|tx| {
        let record = crate::profile_database::LocalPreference::RestartRecoveryOnce.record::<bool>();
        let requested = record.read_from(tx)?;
        let bypass =
            crate::profile_database::LocalPreference::ConfigurationRecoveryBypass.record::<bool>();
        if bypass.read_from(tx)?.is_some() {
            stream_dock_manager_storage::delete(tx, bypass.domain, bypass.key)?;
        }
        if requested.is_some() {
            stream_dock_manager_storage::delete(tx, record.domain, record.key)?;
        }
        Ok(requested)
    })
}

#[cfg(test)]
mod restart_key_tests {
    #[test]
    // Verify that only current restart key is consumed exactly once
    fn only_current_restart_key_is_consumed_exactly_once() {
        use stream_dock_manager_storage::{Domain, Storage};
        for (old, current) in [(false, false), (true, false), (false, true), (true, true)] {
            let root = tempfile::tempdir().unwrap();
            let storage = Storage::open(root.path().into()).unwrap();
            if old {
                storage
                    .write_json(Domain::LocalPreferences, "recovery-once", &true, 16)
                    .unwrap();
            }
            if current {
                storage
                    .write_json(Domain::LocalPreferences, "restart-recovery-once", &true, 16)
                    .unwrap();
            }
            assert_eq!(
                super::take_restart_recovery_request(&storage).unwrap(),
                current.then_some(true)
            );
            assert_eq!(
                super::take_restart_recovery_request(&storage).unwrap(),
                None
            );
            assert_eq!(
                storage
                    .read_json::<bool>(Domain::LocalPreferences, "recovery-once", 16)
                    .unwrap(),
                old.then_some(true)
            );
        }
    }
}

/// A soft, rounded keycap without a hard outline, derived from the current palette
fn navigation_background(width: u16, height: u16, surface: [u8; 4], ink: &str) -> image::RgbaImage {
    let ink = hardware_core::appearance::background_color(ink).unwrap_or([255, 255, 255, 255]);
    let edge = f32::from(width.min(height));
    let inset = (edge * 0.045).max(1.0);
    let radius = edge * 0.14;
    let half_width = f32::from(width) / 2.0;
    let half_height = f32::from(height) / 2.0;
    image::RgbaImage::from_fn(width.into(), height.into(), |x, y| {
        // Signed distance gives smooth rounded edges at any LCD resolution
        let qx = (x as f32 + 0.5 - half_width).abs() - (half_width - inset - radius);
        let qy = (y as f32 + 0.5 - half_height).abs() - (half_height - inset - radius);
        let distance = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
        let coverage = (0.5 - distance).clamp(0.0, 1.0);
        let diagonal = (x as f32 / width.max(1) as f32 + y as f32 / height.max(1) as f32) / 2.0;
        let amount = coverage * (0.035 + 0.065 * (1.0 - diagonal));
        let mut pixel = surface;
        for channel in 0..3 {
            pixel[channel] = (surface[channel] as f32 * (1.0 - amount)
                + ink[channel] as f32 * amount)
                .round() as u8;
        }
        image::Rgba(pixel)
    })
}
