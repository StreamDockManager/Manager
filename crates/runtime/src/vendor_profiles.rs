//! Source-backed bundled vendor profiles. Reviews do not run plugins or write records
use crate::{
    profiles::{Binding, Page, Profile},
    Runtime, Snapshot,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
    time::{Duration, Instant},
};
use stream_dock_manager_storage::{rusqlite::Connection, Domain};
const KEY: &str = "vendor-profile-installations";
const LIMIT: usize = 1024 * 1024;

// Only this trusted module can unwrap a validated conversion for canonical profile construction
pub(crate) struct ValidatedConversion(crate::vendor_profile_archive::ConversionPlan);

#[cfg(test)]
impl ValidatedConversion {
    // Expose accepted plan fields to archive regression tests without widening the production API
    pub(crate) fn test_plan(self) -> crate::vendor_profile_archive::ConversionPlan {
        self.0
    }
}

// Treat converter output as untrusted data before it can become a staged canonical profile
pub(crate) fn validate_conversion(
    plan: crate::vendor_profile_archive::ConversionPlan,
    owner: &stream_dock_manager_plugin_host::PluginInfo,
    declared_device_type: u32,
) -> Result<ValidatedConversion, String> {
    let expected_geometry = crate::vendor_profile_archive::geometry(&plan.device_model)?;
    if (plan.device_type, plan.columns, plan.rows) != expected_geometry
        || owner.dialect != "streamdeck"
        || plan.device_type != declared_device_type
        || plan.rows == 0
        || plan.columns == 0
        || plan.actions.len() > usize::from(plan.rows) * usize::from(plan.columns)
        || plan.name.is_empty()
        || plan.name.len() > 256
        || plan.name.chars().any(char::is_control)
    {
        return Err("Invalid vendor profile conversion metadata".into());
    }
    let mut occupied = BTreeSet::new();
    let mut settings_bytes = 0usize;
    for action in &plan.actions {
        let manifest = owner
            .actions
            .iter()
            .find(|entry| {
                entry.id == action.uuid && entry.control_types.iter().any(|kind| kind == "button")
            })
            .ok_or("Profile action is not owned by the declaring plugin Keypad manifest")?;
        settings_bytes = settings_bytes.saturating_add(action.settings.to_string().len());
        if action.column >= plan.columns
            || action.row >= plan.rows
            || !occupied.insert((action.column, action.row))
            || !action.settings.is_object()
            || settings_bytes > 256 * 1024
            || action.name != manifest.name
            || !action.linked_title
            || action.states.is_empty()
            || action.states.len() > 64
            || action.states.len() != manifest.state_names.len()
            || action.current_state >= action.states.len()
        {
            return Err("Invalid vendor profile conversion action".into());
        }
        for state in &action.states {
            state.validate()?;
        }
    }
    Ok(ValidatedConversion(plan))
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct VendorProfileReview {
    pub id: String,
    pub plugin_id: String,
    pub plugin_name: String,
    pub manifest_name: String,
    pub profile_name: String,
    pub device_id: String,
    pub device_name: String,
    pub binding_count: usize,
    pub archive_sha256: String,
    pub switch_after_install: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct VendorProfileReviews {
    pub reviews: Vec<VendorProfileReview>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Installed {
    plugin_id: String,
    manifest_name: String,
    device_key: String,
    model_id: String,
    vendor_device_model: String,
    installation: String,
    manifest_sha256: String,
    archive_sha256: String,
    profile_id: String,
    page_id: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Previous {
    plugin_id: String,
    installation: String,
    profile_id: String,
    page_id: String,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Index {
    installed: Vec<Installed>,
    previous: BTreeMap<String, Previous>,
}
// Compute a content digest for vendor profiles
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
// Report the valid string condition for vendor profiles
fn valid_string(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}
// Report the hash condition for vendor profiles
fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
impl Index {
    // Validate the supplied vendor profiles value
    fn validate(&self) -> Result<(), String> {
        if self.installed.len() > 1024 || self.previous.len() > 256 {
            return Err("Vendor profile index exceeds limits".into());
        }
        let mut seen = BTreeSet::new();
        for entry in &self.installed {
            if [
                &entry.plugin_id,
                &entry.manifest_name,
                &entry.device_key,
                &entry.model_id,
                &entry.vendor_device_model,
                &entry.profile_id,
                &entry.page_id,
            ]
            .iter()
            .any(|s| !valid_string(s))
                || [
                    &entry.installation,
                    &entry.manifest_sha256,
                    &entry.archive_sha256,
                ]
                .iter()
                .any(|s| !hash(s))
                || !seen.insert((&entry.plugin_id, &entry.manifest_name, &entry.device_key))
            {
                return Err("Invalid vendor profile installation provenance".into());
            }
        }
        for (device, entry) in &self.previous {
            if [device, &entry.plugin_id, &entry.profile_id, &entry.page_id]
                .iter()
                .any(|s| !valid_string(s))
                || !hash(&entry.installation)
            {
                return Err("Invalid vendor profile return selection".into());
            }
        }
        Ok(())
    }
}
// Read index
fn read_index(tx: &Connection) -> Result<Index, String> {
    let value: Option<serde_json::Value> =
        stream_dock_manager_storage::read_json(tx, Domain::LocalPreferences, KEY, LIMIT)?;
    if value.as_ref().is_some_and(|v| !v.is_object()) {
        return Err("Invalid vendor profile index object".into());
    }
    let index: Index = value
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    index.validate()?;
    Ok(index)
}
// Validate snapshot
pub(crate) fn validate_snapshot(tx: &Connection) -> Result<(), String> {
    read_index(tx).map(|_| ())
}
// Discard the selected all state
pub(crate) fn forget_all(tx: &Connection) -> Result<(), String> {
    stream_dock_manager_storage::delete(tx, Domain::LocalPreferences, KEY)
}
/// Generic deletion/import/history may remove a profile, but cannot mint provenance
pub(crate) fn prune(tx: &Connection, profiles: &[Profile]) -> Result<(), String> {
    let mut index = read_index(tx)?;
    let before = index.installed.len() + index.previous.len();
    let exists = |id: &str, page: &str| {
        profiles
            .iter()
            .any(|p| p.id == id && p.pages.iter().any(|p| p.id == page))
    };
    index
        .installed
        .retain(|entry| exists(&entry.profile_id, &entry.page_id));
    index
        .previous
        .retain(|_, entry| exists(&entry.profile_id, &entry.page_id));
    if before != index.installed.len() + index.previous.len() {
        stream_dock_manager_storage::write_json(tx, Domain::LocalPreferences, KEY, &index, LIMIT)?;
    }
    Ok(())
}
/// Used inside the same transaction as session navigation. No nested return stack
pub(crate) fn navigate(
    tx: &Connection,
    device: &str,
    before: &crate::workspace::DeviceSession,
    vendor: Option<(&str, &str, bool)>,
    _manual: bool,
) -> Result<(), String> {
    let mut index = read_index(tx)?;
    if let Some((owner, installation, restore)) = vendor {
        if restore {
            let previous = index
                .previous
                .get(device)
                .ok_or("No previous profile is tracked for this device")?;
            if previous.plugin_id != owner || previous.installation != installation {
                return Err("Previous profile belongs to another plugin installation".into());
            }
            index.previous.remove(device);
        } else if let Some(previous) = index.previous.get(device) {
            if previous.plugin_id != owner || previous.installation != installation {
                return Err("Nested profile switches between plugins are unsupported".into());
            }
        } else {
            index.previous.insert(
                device.into(),
                Previous {
                    plugin_id: owner.into(),
                    installation: installation.into(),
                    profile_id: before.profile_id.clone(),
                    page_id: before.page_id.clone(),
                },
            );
        }
    } else {
        // Any non-vendor navigation ends the return chain, including automation
        if index.previous.remove(device).is_none() {
            return Ok(());
        }
    }
    index.validate()?;
    stream_dock_manager_storage::write_json(tx, Domain::LocalPreferences, KEY, &index, LIMIT)
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct Declaration {
    name: String,
    device_type: u32,
    #[serde(default = "yes")]
    auto_install: bool,
    #[serde(default)]
    dont_auto_switch_when_installed: bool,
    #[serde(default)]
    readonly: bool,
}
// Report the yes condition for vendor profiles
fn yes() -> bool {
    true
}
// Validate bounded, unique, extensionless bundled-profile declarations from a vendor manifest
fn declarations(manifest: &serde_json::Value) -> Result<Vec<Declaration>, String> {
    let values: Vec<Declaration> = serde_json::from_value(
        manifest
            .get("Profiles")
            .cloned()
            .unwrap_or(serde_json::json!([])),
    )
    .map_err(|_| "Unsupported bundled profile declaration")?;
    if values.len() > 32 {
        return Err("Too many bundled vendor profiles".into());
    }
    let mut seen = BTreeSet::new();
    for value in &values {
        if !valid_string(&value.name)
            || value.name.len() > 256
            || value.name.contains('\\')
            || value
                .name
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | ".."))
            || value.name.ends_with(".streamDeckProfile")
            || Path::new(&value.name)
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
            || !seen.insert(&value.name)
        {
            return Err("Bundled profile names must be unique extensionless package paths".into());
        }
    }
    Ok(values)
}
// Read archive
fn read_archive(root: &Path, name: &str) -> Result<Vec<u8>, String> {
    crate::vendor_profile_files::read(
        root,
        Path::new(&format!("{name}.streamDeckProfile")),
        4 * 1024 * 1024,
    )
}
// Read and hash a bounded package manifest only if it still matches the host's loaded manifest
fn manifest_digest(
    host: &stream_dock_manager_plugin_host::PluginHost,
    id: &str,
) -> Result<String, String> {
    let bytes = crate::vendor_profile_files::read(
        &host.plugin_directory(id)?,
        Path::new("manifest.json"),
        1024 * 1024,
    )?;
    let disk: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "Package manifest changed")?;
    if disk != host.plugin_manifest(id)? {
        return Err("Package manifest changed; reload the plugin and review again".into());
    }
    Ok(digest(&bytes))
}
// Recognize a complete rectangular button grid without duplicate coordinates
fn geometry(device: &crate::DeviceInfo) -> Option<(usize, usize)> {
    let keys: Vec<_> = device
        .manifest
        .controls
        .iter()
        .filter(|c| c.kind == "button")
        .collect();
    let xs: BTreeSet<_> = keys.iter().map(|c| c.x).collect();
    let ys: BTreeSet<_> = keys.iter().map(|c| c.y).collect();
    (xs.len() * ys.len() == keys.len()
        && keys
            .iter()
            .map(|c| (c.x, c.y))
            .collect::<BTreeSet<_>>()
            .len()
            == keys.len())
    .then_some((xs.len(), ys.len()))
}
// Map a supported button grid and encoder presence to the vendor profile device type
pub(crate) fn device_type(device: &crate::DeviceInfo) -> Option<u32> {
    match geometry(device)? {
        (5, 3) => Some(0),
        (3, 2) => Some(1),
        (8, 4) => Some(2),
        (8, 8) => Some(3),
        (4, 2) => Some(
            if device.manifest.controls.iter().any(|c| c.kind == "encoder") {
                7
            } else {
                9
            },
        ),
        _ => None,
    }
}
struct Staged {
    review: VendorProfileReview,
    archive: Vec<u8>,
    installation: String,
    manifest_sha256: String,
    profile: Profile,
    device_key: String,
    vendor_device_model: String,
    epoch: Option<u64>,
    configuration_revision: u64,
    previous: (String, String),
    request_session: Option<String>,
    created: Instant,
}
#[derive(Default)]
pub(crate) struct Reviews {
    staged: BTreeMap<String, Staged>,
    dismissed: BTreeSet<String>,
    failures: BTreeMap<String, String>,
}
// Encode plugin, profile, device and installation identity into a stable review key
fn review_key(plugin: &str, name: &str, device: &str, installation: &str) -> String {
    serde_json::json!([plugin, name, device, installation]).to_string()
}
pub(crate) struct Navigation {
    pub plugin: String,
    pub installation: String,
    pub session: String,
    pub manifest_name: Option<String>,
    pub device_key: String,
    pub epoch: Option<u64>,
    pub configuration_revision: u64,
    pub before: (String, String),
}
impl Navigation {
    // Verify the vendor profiles evidence
    pub(crate) fn verify(
        &self,
        host: &stream_dock_manager_plugin_host::PluginHost,
        device: &crate::DeviceInfo,
        snapshot: &Snapshot,
        epoch: Option<u64>,
        selection: (&str, &str),
        index: &Index,
    ) -> Result<(), String> {
        let (target, page) = selection;
        let current = snapshot.session(&device.id);
        if !device.connected
            || device.persistence_key != self.device_key
            || epoch != self.epoch
            || snapshot.configuration_revision != self.configuration_revision
            || (current.profile_id, current.page_id) != self.before
            || !host.profile_switch_session_is_current(&self.plugin, &self.session)
            || host.installation_token(&self.plugin).as_ref() != Some(&self.installation)
        {
            return Err("Profile switch became stale; device, selection or plugin changed".into());
        }
        if let Some(name) = &self.manifest_name {
            let entry = index
                .installed
                .iter()
                .find(|e| {
                    e.plugin_id == self.plugin
                        && e.installation == self.installation
                        && e.device_key == self.device_key
                        && e.manifest_name == *name
                        && e.profile_id == target
                        && e.page_id == page
                        && e.model_id == device.model_id
                })
                .ok_or("Owned bundled profile mapping is unavailable")?;
            if manifest_digest(host, &self.plugin)? != entry.manifest_sha256
                || digest(&read_archive(&host.plugin_directory(&self.plugin)?, name)?)
                    != entry.archive_sha256
            {
                return Err("Bundled profile package provenance changed".into());
            }
        } else {
            let previous = index
                .previous
                .get(&self.device_key)
                .ok_or("Previous profile is no longer available")?;
            if previous.plugin_id != self.plugin
                || previous.installation != self.installation
                || previous.profile_id != target
                || previous.page_id != page
            {
                return Err("Previous profile owner or selection changed".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone)]
struct Admission {
    device_key: String,
    model_id: String,
    epoch: Option<u64>,
    revision: u64,
    before: (String, String),
}
impl Admission {
    // Capture the current vendor profiles observation
    fn capture(device: &crate::DeviceInfo, snapshot: &Snapshot, epoch: Option<u64>) -> Self {
        let before = snapshot.session(&device.id);
        Self {
            device_key: device.persistence_key.clone(),
            model_id: device.model_id.clone(),
            epoch,
            revision: snapshot.configuration_revision,
            before: (before.profile_id, before.page_id),
        }
    }
    // Check the current vendor profiles conditions
    fn check(
        &self,
        device: &crate::DeviceInfo,
        snapshot: &Snapshot,
        epoch: Option<u64>,
    ) -> Result<(), String> {
        let before = snapshot.session(&device.id);
        if !device.connected
            || device.persistence_key != self.device_key
            || device.model_id != self.model_id
            || epoch != self.epoch
            || snapshot.configuration_revision != self.revision
            || (before.profile_id, before.page_id) != self.before
        {
            return Err(
                "Queued profile request became stale after device or workspace navigation".into(),
            );
        }
        Ok(())
    }
}
struct QueuedSwitch {
    effect: stream_dock_manager_plugin_host::ProfileSwitchEffect,
    admission: Admission,
}
#[derive(Default)]
pub(crate) struct SwitchRequests {
    pending: BTreeMap<(String, String), QueuedSwitch>,
    running: bool,
    paused: bool,
    reported: BTreeMap<(String, String), (String, Instant)>,
}
#[cfg(any(test, feature = "test-support"))]
pub struct VendorProfileSwitchHold {
    runtime: std::sync::Weak<Runtime>,
}
#[cfg(any(test, feature = "test-support"))]
impl VendorProfileSwitchHold {
    // Count queued vendor profile switches while the owning runtime remains available
    pub fn pending(&self) -> usize {
        self.runtime.upgrade().map_or(0, |runtime| {
            runtime
                .vendor_profile_switches
                .lock()
                .unwrap()
                .pending
                .len()
        })
    }
}
#[cfg(any(test, feature = "test-support"))]
impl Drop for VendorProfileSwitchHold {
    // Release this vendor profiles owner when it leaves scope
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            let start = {
                let mut queue = runtime.vendor_profile_switches.lock().unwrap();
                queue.paused = false;
                let start = !queue.running && !queue.pending.is_empty();
                if start {
                    queue.running = true;
                }
                start
            };
            if start {
                runtime.start_vendor_profile_worker();
            }
        }
    }
}
impl Runtime {
    #[cfg(any(test, feature = "test-support"))]
    // Pause an idle vendor profile switch queue and return an owner-scoped release guard
    pub(crate) fn hold_vendor_profile_switches(
        self: &std::sync::Arc<Self>,
    ) -> Result<VendorProfileSwitchHold, String> {
        let mut queue = self.vendor_profile_switches.lock().unwrap();
        if queue.running || queue.paused {
            return Err("Vendor profile queue is already held or processing".into());
        }
        queue.paused = true;
        Ok(VendorProfileSwitchHold {
            runtime: std::sync::Arc::downgrade(self),
        })
    }
    /// Non-blocking bounded handoff: archive validation and SQL never run in the device loop
    pub(crate) fn queue_vendor_profile_switch(
        self: &std::sync::Arc<Self>,
        effect: stream_dock_manager_plugin_host::ProfileSwitchEffect,
    ) {
        let admission = {
            let Ok(epochs) = self.connection_epochs.try_lock() else {
                return;
            };
            let Ok(snapshot) = self.state.try_lock() else {
                return;
            };
            let Some(device) = snapshot
                .devices
                .iter()
                .find(|d| d.id == effect.device && d.connected)
            else {
                return;
            };
            Admission::capture(device, &snapshot, epochs.get(&effect.device).copied())
        };
        let Ok(mut queue) = self.vendor_profile_switches.try_lock() else {
            return;
        };
        let key = (effect.plugin.clone(), effect.device.clone());
        if queue.pending.len() >= 16 && !queue.pending.contains_key(&key) {
            return;
        }
        queue
            .pending
            .insert(key, QueuedSwitch { effect, admission });
        if queue.running || queue.paused {
            return;
        }
        queue.running = true;
        drop(queue);
        self.start_vendor_profile_worker();
    }
    // Start vendor profile worker
    fn start_vendor_profile_worker(self: &std::sync::Arc<Self>) {
        let runtime = std::sync::Arc::downgrade(self);
        if std::thread::Builder::new()
            .name("vendor-profile-reviews".into())
            .spawn(move || {
                while let Some(runtime) = runtime.upgrade() {
                    let effect = {
                        let mut queue = runtime.vendor_profile_switches.lock().unwrap();
                        if runtime.stopping.load(std::sync::atomic::Ordering::Acquire) {
                            queue.pending.clear();
                        }
                        let Some((_, effect)) = queue.pending.pop_first() else {
                            queue.running = false;
                            break;
                        };
                        effect
                    };
                    let owner = (effect.effect.plugin.clone(), effect.effect.device.clone());
                    if let Err(error) = runtime.handle_vendor_profile_switch_admitted(
                        effect.effect,
                        Some(&effect.admission),
                    ) {
                        let error = error.chars().take(512).collect::<String>();
                        let report = {
                            let mut queue = runtime.vendor_profile_switches.lock().unwrap();
                            queue
                                .reported
                                .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(60));
                            if queue
                                .reported
                                .get(&owner)
                                .is_some_and(|(old, _)| old == &error)
                                || queue.reported.len() >= 128
                            {
                                false
                            } else {
                                queue
                                    .reported
                                    .insert(owner, (error.clone(), Instant::now()));
                                true
                            }
                        };
                        if report {
                            runtime.emit("plugin-error", None, None, Some(error));
                        }
                    }
                }
            })
            .is_err()
        {
            self.vendor_profile_switches.lock().unwrap().running = false;
        }
    }
    // Read and validate the saved vendor profile index or supply its default state
    pub(crate) fn vendor_index(&self) -> Result<Index, String> {
        let value: Option<Index> =
            self.profiles
                .storage
                .read_json(Domain::LocalPreferences, KEY, LIMIT)?;
        let index = value.unwrap_or_default();
        index.validate()?;
        Ok(index)
    }
    // Stage vendor profile
    fn stage_vendor_profile(
        &self,
        plugin_id: &str,
        name: &str,
        device_id: &str,
        request_session: Option<&str>,
    ) -> Result<(), String> {
        self.stage_vendor_profile_admitted(plugin_id, name, device_id, request_session, None)
    }
    // Stage vendor profile admitted
    fn stage_vendor_profile_admitted(
        &self,
        plugin_id: &str,
        name: &str,
        device_id: &str,
        request_session: Option<&str>,
        admission: Option<&Admission>,
    ) -> Result<(), String> {
        let mut reviews = self.vendor_profile_reviews.lock().unwrap();
        reviews
            .staged
            .retain(|_, s| s.created.elapsed() < Duration::from_secs(3600));
        let epochs = self.connection_epochs.lock().unwrap();
        let state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|d| d.id == device_id && d.connected)
            .ok_or("Profile target device is not connected")?;
        if let Some(admission) = admission {
            admission.check(device, &state, epochs.get(device_id).copied())?;
        }
        let host = self.plugins.lock().unwrap();
        if request_session
            .is_some_and(|session| !host.profile_switch_session_is_current(plugin_id, session))
        {
            return Err("Plugin connection changed".into());
        }
        let info = host
            .list()
            .into_iter()
            .find(|p| p.id == plugin_id && p.dialect == "streamdeck")
            .ok_or("Vendor plugin is no longer installed")?;
        let manifest = host.plugin_manifest(plugin_id)?;
        let declaration = declarations(&manifest)?
            .into_iter()
            .find(|d| d.name == name)
            .ok_or("Profile is not declared by this plugin")?;
        if declaration.readonly {
            return Err("Readonly bundled profiles are not supported".into());
        }
        if device_type(device) != Some(declaration.device_type) {
            return Err("Bundled profile is incompatible with this device geometry".into());
        }
        let installation = host
            .installation_token(plugin_id)
            .ok_or("Plugin installation unavailable")?;
        let key = review_key(plugin_id, name, &device.persistence_key, &installation);
        if reviews.dismissed.contains(&key)
            || reviews.staged.contains_key(&key)
            || reviews.failures.contains_key(&key)
        {
            return Ok(());
        }
        if reviews.staged.len() >= 16 || reviews.dismissed.len() >= 1024 {
            return Err("Bundled profile review limit reached".into());
        }
        let bytes = read_archive(&host.plugin_directory(plugin_id)?, name)?;
        let ValidatedConversion(parsed) = validate_conversion(
            crate::vendor_profile_archive::convert(&bytes)?,
            &info,
            declaration.device_type,
        )?;
        if parsed.device_type != declaration.device_type {
            return Err("Archive device type changed".into());
        }
        let profile_id = uuid::Uuid::new_v4().to_string();
        let page_id = uuid::Uuid::new_v4().to_string();
        let keys: Vec<_> = device
            .manifest
            .controls
            .iter()
            .filter(|c| c.kind == "button")
            .collect();
        let xs: BTreeSet<_> = keys.iter().map(|c| c.x).collect();
        let ys: BTreeSet<_> = keys.iter().map(|c| c.y).collect();
        if (usize::from(parsed.columns), usize::from(parsed.rows)) != (xs.len(), ys.len()) {
            return Err("Archive geometry does not match the target device".into());
        }
        let mut bindings = BTreeMap::new();
        for action in parsed.actions {
            let x = xs
                .iter()
                .nth(usize::from(action.column))
                .ok_or("Invalid profile column")?;
            let y = ys
                .iter()
                .nth(usize::from(action.row))
                .ok_or("Invalid profile row")?;
            let control = keys
                .iter()
                .find(|c| c.x == *x && c.y == *y)
                .ok_or("Profile key does not exist")?;
            let binding: Binding = serde_json::from_value(serde_json::json!({"title":"","color":"#00000000","action":{"kind":"plugin","plugin_id":plugin_id,"action_id":action.uuid,"settings":action.settings},"plugin_appearance":{"plugin_id":plugin_id,"action_id":action.uuid,"elements":[],"vendor_profile":{"linked_title":action.linked_title,"initial_state":action.current_state,"states":action.states}}})).map_err(|e| e.to_string())?;
            bindings.insert(control.id.clone(), binding);
        }
        let profile = Profile {
            id: profile_id,
            name: parsed.name,
            model_id: device.model_id.clone(),
            active_page: page_id.clone(),
            pages: vec![Page {
                id: page_id,
                name: "Main page".into(),
                bindings,
            }],
            palette: None,
            parameters: vec![],
            background_asset: None,
            background_media: Default::default(),
        };
        profile.validate()?;
        let session = state.session(device_id);
        let review = VendorProfileReview {
            id: uuid::Uuid::new_v4().to_string(),
            plugin_id: plugin_id.into(),
            plugin_name: info.name,
            manifest_name: name.into(),
            profile_name: profile.name.clone(),
            device_id: device_id.into(),
            device_name: device.name.clone(),
            binding_count: profile.pages[0].bindings.len(),
            archive_sha256: digest(&bytes),
            switch_after_install: request_session.is_some()
                || !declaration.dont_auto_switch_when_installed,
        };
        reviews.staged.insert(
            key,
            Staged {
                review,
                archive: bytes,
                installation,
                manifest_sha256: manifest_digest(&host, plugin_id)?,
                profile,
                device_key: device.persistence_key.clone(),
                vendor_device_model: parsed.device_model,
                epoch: epochs.get(device_id).copied(),
                configuration_revision: state.configuration_revision,
                previous: (session.profile_id, session.page_id),
                request_session: request_session.map(str::to_owned),
                created: Instant::now(),
            },
        );
        Ok(())
    }
    // Collect bounded vendor profile review candidates and failures for current plugin installations
    pub fn pending_vendor_profile_reviews(&self) -> Result<VendorProfileReviews, String> {
        let snapshot = self.snapshot();
        let mut candidates = vec![];
        let mut failures = vec![];
        let mut installations = BTreeMap::new();
        {
            let host = self.plugins.lock().unwrap();
            for (position, plugin) in host
                .list()
                .iter()
                .filter(|p| p.dialect == "streamdeck")
                .enumerate()
            {
                let installation = host
                    .installation_token(&plugin.id)
                    .ok_or("Plugin installation unavailable")?;
                installations.insert(plugin.id.clone(), installation.clone());
                if position >= 100 {
                    continue;
                }
                let declared = match declarations(&host.plugin_manifest(&plugin.id)?) {
                    Ok(value) => value,
                    Err(error) => {
                        failures.push((
                            review_key(&plugin.id, "@manifest", "", &installation),
                            format!("{}: {error}", plugin.id)
                                .chars()
                                .take(512)
                                .collect::<String>(),
                        ));
                        continue;
                    }
                };
                for declaration in declared {
                    if !declaration.auto_install {
                        continue;
                    }
                    for device in snapshot
                        .devices
                        .iter()
                        .filter(|d| d.connected && device_type(d) == Some(declaration.device_type))
                    {
                        candidates.push((
                            plugin.id.clone(),
                            declaration.name.clone(),
                            device.id.clone(),
                            device.persistence_key.clone(),
                            installation.clone(),
                        ));
                    }
                }
            }
        }
        {
            let mut reviews = self.vendor_profile_reviews.lock().unwrap();
            // Reclaim before any skip/cap test, even when every old review expired
            reviews.staged.retain(|_, stage| {
                stage.created.elapsed() < Duration::from_secs(3600)
                    && installations.get(&stage.review.plugin_id) == Some(&stage.installation)
                    && snapshot.devices.iter().any(|device| {
                        device.connected
                            && device.id == stage.review.device_id
                            && device.persistence_key == stage.device_key
                    })
            });
            let current = |key: &String| {
                serde_json::from_str::<Vec<String>>(key)
                    .ok()
                    .is_some_and(|parts| {
                        parts.len() == 4 && installations.get(&parts[0]) == Some(&parts[3])
                    })
            };
            reviews.failures.retain(|key, _| current(key));
            reviews.dismissed.retain(current);
            for (key, error) in failures {
                if reviews.failures.len() < 128 {
                    reviews.failures.insert(key, error);
                }
            }
        }
        let index = self.vendor_index()?;
        let mut attempted = 0;
        for (plugin, name, device, key, installation) in candidates {
            let candidate_key = review_key(&plugin, &name, &key, &installation);
            {
                let reviews = self.vendor_profile_reviews.lock().unwrap();
                if reviews.dismissed.contains(&candidate_key)
                    || reviews.failures.contains_key(&candidate_key)
                    || reviews.staged.contains_key(&candidate_key)
                {
                    continue;
                }
                if reviews.staged.len() >= 16 || reviews.failures.len() >= 128 {
                    break;
                }
            }
            if attempted >= 64 {
                break;
            }
            attempted += 1;
            if !index.installed.iter().any(|e| {
                e.plugin_id == plugin
                    && e.manifest_name == name
                    && e.device_key == key
                    && e.installation == installation
            }) {
                let review_key = review_key(&plugin, &name, &key, &installation);
                {
                    let reviews = self.vendor_profile_reviews.lock().unwrap();
                    if reviews.staged.len() >= 16 {
                        break;
                    }
                }
                if let Err(error) = self.stage_vendor_profile(&plugin, &name, &device, None) {
                    let mut reviews = self.vendor_profile_reviews.lock().unwrap();
                    if reviews.failures.len() < 128 {
                        reviews.failures.insert(
                            review_key,
                            format!("{plugin}: {name}: {error}")
                                .chars()
                                .take(512)
                                .collect(),
                        );
                    }
                }
            }
        }
        let reviews = self.vendor_profile_reviews.lock().unwrap();
        Ok(VendorProfileReviews {
            reviews: reviews.staged.values().map(|s| s.review.clone()).collect(),
            warnings: reviews.failures.values().take(16).cloned().collect(),
        })
    }
    // Resolve vendor profile review
    pub fn resolve_vendor_profile_review(
        &self,
        id: &str,
        accept: bool,
    ) -> Result<Snapshot, String> {
        let mut reviews = self.vendor_profile_reviews.lock().unwrap();
        let key = reviews
            .staged
            .iter()
            .find(|(_, s)| s.review.id == id)
            .map(|(key, _)| key.clone());
        let Some(key) = key else {
            return if accept {
                Err("Bundled profile review expired".into())
            } else {
                Ok(self.snapshot())
            };
        };
        if !accept {
            reviews.staged.remove(&key);
            reviews.dismissed.insert(key);
            return Ok(self.snapshot());
        }
        // Failed acceptance consumes the stale snapshot; reopening produces a fresh review
        let stage = reviews.staged.remove(&key).unwrap();
        if stage.created.elapsed() >= Duration::from_secs(3600) {
            return Err("Bundled profile review expired".into());
        }
        let epochs = self.connection_epochs.lock().unwrap();
        let mut state = self.state.lock().unwrap();
        let device = state
            .devices
            .iter()
            .find(|d| {
                d.id == stage.review.device_id
                    && d.connected
                    && d.persistence_key == stage.device_key
                    && d.model_id == stage.profile.model_id
            })
            .ok_or("Profile target device changed")?;
        if epochs.get(&device.id).copied() != stage.epoch {
            return Err("Device connection changed. Start a new profile review.".into());
        }
        let session = state.session(&device.id);
        if state.configuration_revision != stage.configuration_revision
            || (session.profile_id.clone(), session.page_id.clone()) != stage.previous
        {
            return Err(
                "Workspace or selected profile changed. Start a new profile review.".into(),
            );
        }
        let _transaction = self.profiles.transaction.lock().unwrap();
        let host = self.plugins.lock().unwrap();
        if host.installation_token(&stage.review.plugin_id).as_ref() != Some(&stage.installation)
            || stage.request_session.as_ref().is_some_and(|token| {
                !host.profile_switch_session_is_current(&stage.review.plugin_id, token)
            })
            || manifest_digest(&host, &stage.review.plugin_id)? != stage.manifest_sha256
            || read_archive(
                &host.plugin_directory(&stage.review.plugin_id)?,
                &stage.review.manifest_name,
            )? != stage.archive
        {
            return Err("Plugin package changed. Start a new profile review.".into());
        }
        let entry = Installed {
            plugin_id: stage.review.plugin_id.clone(),
            manifest_name: stage.review.manifest_name.clone(),
            device_key: stage.device_key.clone(),
            model_id: stage.profile.model_id.clone(),
            vendor_device_model: stage.vendor_device_model.clone(),
            installation: stage.installation.clone(),
            manifest_sha256: stage.manifest_sha256.clone(),
            archive_sha256: stage.review.archive_sha256.clone(),
            profile_id: stage.profile.id.clone(),
            page_id: stage.profile.active_page.clone(),
        };
        let switch = stage.review.switch_after_install;
        let device_id = device.id.clone();
        let physical = !device.simulated;
        let epoch = stage.epoch;
        let mut sessions = state.sessions.clone();
        let mut selected = session.clone();
        selected.profile_id = stage.profile.id.clone();
        selected.page_id = stage.profile.active_page.clone();
        selected
            .remembered_pages
            .insert(selected.profile_id.clone(), selected.page_id.clone());
        if switch {
            sessions.insert(stage.device_key.clone(), selected);
        }
        let saved_sessions = sessions.clone();
        let profile = stage.profile.clone();
        let prepared = self.profiles.prepare_import_transaction(
            stage.configuration_revision,
            "Install bundled vendor profile",
            None,
            0,
            |data| {
                data.profiles.push(profile);
                Ok(())
            },
        )?;
        let cost = prepared.retained_bytes()? + 2 * LIMIT;
        let commit = || {
            self.profiles
                .storage
                .transaction_with_cost(cost, move |tx| {
                    if switch {
                        navigate(
                            tx,
                            &entry.device_key,
                            &session,
                            Some((&entry.plugin_id, &entry.installation, false)),
                            false,
                        )?;
                    }
                    let mut index = read_index(tx)?;
                    index.installed.retain(|e| {
                        !(e.plugin_id == entry.plugin_id
                            && e.manifest_name == entry.manifest_name
                            && e.device_key == entry.device_key)
                    });
                    index.installed.push(entry);
                    index.validate()?;
                    // Profile, owner mapping, previous selection and optional activation
                    // have one commit boundary, while device/package/state guards are held
                    let data = prepared.commit(tx)?;
                    stream_dock_manager_storage::write_json(
                        tx,
                        Domain::LocalPreferences,
                        KEY,
                        &index,
                        LIMIT,
                    )?;
                    if switch {
                        stream_dock_manager_storage::write_json(
                            tx,
                            Domain::LocalPreferences,
                            "sessions",
                            &saved_sessions,
                            16 * LIMIT,
                        )?;
                    }
                    Ok(data)
                })
        };
        let data = if let Some(session) = &stage.request_session {
            host.with_profile_switch_session(&stage.review.plugin_id, session, commit)?
        } else {
            commit()?
        };
        Self::apply_data(&mut state, data);
        if switch {
            state.sessions = sessions;
            self.builtin_state.cancel_key_logic(Some(&device_id));
            if physical {
                if let Some(epoch) = epoch {
                    self.transition_inputs
                        .lock()
                        .unwrap()
                        .begin(&device_id, epoch);
                }
            }
        }
        reviews.staged.remove(&key);
        drop(host);
        drop(_transaction);
        drop(state);
        drop(epochs);
        drop(reviews);
        if switch {
            if physical {
                self.release_transition_inputs(&device_id);
            }
            self.plugins
                .lock()
                .unwrap()
                .retire_device_appearances(&device_id);
            self.emit("sessions", Some(device_id), None, None);
        }
        self.finish_workspace_commit();
        Ok(self.snapshot())
    }
    // Handle vendor profile switch admitted
    fn handle_vendor_profile_switch_admitted(
        &self,
        effect: stream_dock_manager_plugin_host::ProfileSwitchEffect,
        admission: Option<&Admission>,
    ) -> Result<(), String> {
        let snapshot = self.snapshot();
        let device = snapshot
            .devices
            .iter()
            .find(|d| d.id == effect.device && d.connected)
            .ok_or("Profile target device is not connected")?;
        let epoch = self.connection_generation(&effect.device);
        if let Some(admission) = admission {
            admission.check(device, &snapshot, epoch)?;
        }
        let index = self.vendor_index()?;
        let host = self.plugins.lock().unwrap();
        if !host.profile_switch_session_is_current(&effect.plugin, &effect.session) {
            return Err("Plugin connection changed".into());
        }
        let installation = host
            .installation_token(&effect.plugin)
            .ok_or("Plugin installation unavailable")?;
        let selected = snapshot.session(&effect.device);
        let proof = Navigation {
            plugin: effect.plugin.clone(),
            installation: installation.clone(),
            session: effect.session.clone(),
            manifest_name: effect.profile.clone(),
            device_key: device.persistence_key.clone(),
            epoch,
            configuration_revision: snapshot.configuration_revision,
            before: (selected.profile_id, selected.page_id),
        };
        if let Some(name) = &effect.profile {
            let manifest = host.plugin_manifest(&effect.plugin)?;
            let declaration = declarations(&manifest)?
                .into_iter()
                .find(|d| d.name == *name)
                .ok_or("Profile is not declared by this plugin")?;
            if declaration.readonly || device_type(device) != Some(declaration.device_type) {
                return Err("Bundled profile flags or device geometry are unsupported".into());
            }
            if let Some(entry) = index.installed.iter().find(|e| {
                e.plugin_id == effect.plugin
                    && e.manifest_name == *name
                    && e.device_key == device.persistence_key
                    && e.installation == installation
            }) {
                let selected = snapshot.session(&effect.device);
                if selected.profile_id == entry.profile_id && selected.page_id == entry.page_id {
                    return Ok(());
                }
                if entry.model_id != device.model_id
                    || entry.manifest_sha256 != manifest_digest(&host, &effect.plugin)?
                    || entry.archive_sha256
                        != digest(&read_archive(
                            &host.plugin_directory(&effect.plugin)?,
                            name,
                        )?)
                {
                    return Err(
                        "Installed bundled profile provenance changed. Review this package again."
                            .into(),
                    );
                }
                let target = (entry.profile_id.clone(), entry.page_id.clone());
                drop(host);
                self.select_vendor_profile(&effect.device, &target.0, &target.1, &proof)?;
            } else {
                drop(host);
                self.stage_vendor_profile_admitted(
                    &effect.plugin,
                    name,
                    &effect.device,
                    Some(&effect.session),
                    admission,
                )?;
                self.emit("vendor-profile-review", Some(effect.device), None, None);
            }
        } else {
            let previous = index
                .previous
                .get(&device.persistence_key)
                .filter(|p| p.plugin_id == effect.plugin && p.installation == installation)
                .ok_or("No previous profile is tracked for this plugin and device")?;
            let target = (previous.profile_id.clone(), previous.page_id.clone());
            drop(host);
            self.select_vendor_profile(&effect.device, &target.0, &target.1, &proof)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "vendor_profiles_tests.rs"]
mod tests;
