//! Opt-in resource warnings. Sampling, lifecycle control and alert decisions remain separate
use crate::Runtime;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::{MeasurementSource, MeasurementState, PluginResources};
const GRACE: Duration = Duration::from_secs(30);
const SUSTAIN: Duration = Duration::from_secs(30);
const COOLDOWN: Duration = Duration::from_secs(300);
const MAX_GAP: Duration = Duration::from_secs(10);
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceAlertPreferences {
    pub enabled: bool,
    pub cpu_percent: Option<f64>,
    pub memory_mib: Option<u32>,
}
impl Default for ResourceAlertPreferences {
    // Construct the default resource alerts value
    fn default() -> Self {
        Self {
            enabled: false,
            cpu_percent: Some(80.0),
            memory_mib: Some(512),
        }
    }
}
impl ResourceAlertPreferences {
    // Validate the supplied resource alerts value
    fn validate(&self) -> Result<(), String> {
        if self
            .cpu_percent
            .is_some_and(|v| !v.is_finite() || v <= 0.0 || v > 100.0)
            || self.memory_mib.is_some_and(|v| v == 0 || v > 1_048_576)
            || (self.enabled && self.cpu_percent.is_none() && self.memory_mib.is_none())
        {
            return Err("Choose CPU above 0 and at most 100%, memory 1–1,048,576 MiB, and at least one enabled threshold".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ResourceAlertSettings {
    pub id: String,
    #[ts(type = "number")]
    pub revision: u64,
    pub preferences: ResourceAlertPreferences,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Store {
    schema_version: u32,
    revision: u64,
    entries: BTreeMap<String, ResourceAlertPreferences>,
}
impl Default for Store {
    // Construct the default resource alerts value
    fn default() -> Self {
        Self {
            schema_version: 1,
            revision: 0,
            entries: BTreeMap::new(),
        }
    }
}
#[derive(Default)]
struct Incident {
    run: Option<String>,
    grace_until: Option<Instant>,
    seen: Option<Instant>,
    cpu_since: Option<Instant>,
    memory_since: Option<Instant>,
    latched: bool,
    last_warning: Option<Instant>,
}
impl Incident {
    // Reset the resource alerts state
    fn reset(&mut self) {
        self.run = None;
        self.grace_until = None;
        self.seen = None;
        self.cpu_since = None;
        self.memory_since = None;
        self.latched = false;
    }
    // Update resource alerts from the new observation
    fn observe(
        &mut self,
        prefs: &ResourceAlertPreferences,
        run: &str,
        uptime_ms: u64,
        now: Instant,
        resources: &PluginResources,
    ) -> Option<String> {
        if self.run.as_deref() != Some(run) {
            self.reset();
            self.run = Some(run.into());
            self.grace_until = Some(now + GRACE.saturating_sub(Duration::from_millis(uptime_ms)));
        }
        let valid = matches!(
            resources.state,
            MeasurementState::Available | MeasurementState::WarmingUp
        ) && resources.source == MeasurementSource::ProcessTreeRss
            && resources.root_pid.is_some();
        if !valid {
            self.seen = None;
            self.cpu_since = None;
            self.memory_since = None;
            return None;
        }
        if self
            .seen
            .is_some_and(|seen| now.saturating_duration_since(seen) > MAX_GAP)
        {
            self.cpu_since = None;
            self.memory_since = None;
        }
        self.seen = Some(now);
        if self.grace_until.is_some_and(|until| now < until) {
            self.cpu_since = None;
            self.memory_since = None;
            return None;
        }
        let cpu = resources
            .cpu_percent
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v));
        let memory = resources.resident_bytes;
        let cpu_high = prefs
            .cpu_percent
            .zip(cpu)
            .is_some_and(|(threshold, value)| value >= threshold);
        let memory_high = prefs
            .memory_mib
            .zip(memory)
            .is_some_and(|(threshold, value)| value >= u64::from(threshold) * 1024 * 1024);
        if cpu_high {
            self.cpu_since.get_or_insert(now);
        } else {
            self.cpu_since = None;
        }
        if memory_high {
            self.memory_since.get_or_insert(now);
        } else {
            self.memory_since = None;
        }
        let all_measured = (prefs.cpu_percent.is_none() || cpu.is_some())
            && (prefs.memory_mib.is_none() || memory.is_some());
        if all_measured && !cpu_high && !memory_high {
            self.latched = false;
        }
        let sustained = |since: Option<Instant>| {
            since.is_some_and(|since| now.saturating_duration_since(since) >= SUSTAIN)
        };
        let cpu_due = sustained(self.cpu_since);
        let memory_due = sustained(self.memory_since);
        if self.latched
            || !(cpu_due || memory_due)
            || self
                .last_warning
                .is_some_and(|at| now.saturating_duration_since(at) < COOLDOWN)
        {
            return None;
        }
        self.latched = true;
        self.last_warning = Some(now);
        let mut details = vec![];
        if cpu_due {
            details.push(format!(
                "CPU {:.1}% (threshold {:.1}%)",
                cpu.unwrap(),
                prefs.cpu_percent.unwrap()
            ));
        }
        if memory_due {
            details.push(format!(
                "resident memory {} MiB (threshold {} MiB)",
                memory.unwrap() / 1024 / 1024,
                prefs.memory_mib.unwrap()
            ));
        }
        Some(format!("Resource alert: {} remained at or above its threshold for 30 seconds. Resource alerts do not restart plugins.",details.join(" and ")))
    }
}
pub(crate) struct ResourceAlerts {
    storage: stream_dock_manager_storage::Storage,
    store: Store,
    failure: Option<String>,
    incidents: HashMap<String, Incident>,
}
// Read the requested resource alerts data
fn read(storage: &stream_dock_manager_storage::Storage) -> Result<Store, String> {
    let store: Store = crate::profile_database::LocalPreference::ResourceAlerts
        .record()
        .read(storage)?
        .unwrap_or_default();
    validate_store(store)
}
// Validate store
fn validate_store(store: Store) -> Result<Store, String> {
    if store.schema_version != 1
        || store.revision > 9_007_199_254_740_991
        || store.entries.len() > 1024
    {
        return Err("Unsupported or oversized resource alert preferences; file preserved".into());
    }
    for (id, prefs) in &store.entries {
        if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
            return Err("Invalid resource alert plugin identity; file preserved".into());
        }
        prefs.validate()?;
    }
    Ok(store)
}

// Validate storage snapshot
pub(crate) fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let store = crate::profile_database::LocalPreference::ResourceAlerts
        .snapshot_record()
        .read_from(connection)?
        .unwrap_or_default();
    validate_store(store).map(|_| ())
}

impl ResourceAlerts {
    // Retain plugins
    pub(crate) fn retain_plugins(
        &mut self,
        installed: &std::collections::BTreeSet<String>,
    ) -> Result<(), String> {
        self.reload()?;
        let mut store = self.store.clone();
        store.entries.retain(|id, _| installed.contains(id));
        if store.entries != self.store.entries {
            store.revision += 1;
            crate::profile_database::LocalPreference::ResourceAlerts
                .record::<Store>()
                .write(&self.storage, &store)?;
            self.store = store;
        }
        self.incidents.retain(|id, _| installed.contains(id));
        Ok(())
    }
    #[cfg(test)]
    // Open the requested resource alerts resource
    pub(crate) fn open(path: PathBuf) -> Self {
        Self::with_storage(
            stream_dock_manager_storage::Storage::open(path.parent().unwrap().to_path_buf())
                .unwrap(),
        )
        .unwrap()
    }
    // Construct using storage
    pub(crate) fn with_storage(
        storage: stream_dock_manager_storage::Storage,
    ) -> Result<Self, String> {
        let store = read(&storage)?;
        Ok(Self {
            storage,
            store,
            failure: None,
            incidents: HashMap::new(),
        })
    }
    // Reload resource alerts from its current source
    pub(crate) fn reload(&mut self) -> Result<(), String> {
        match read(&self.storage) {
            Ok(store) => {
                if self.store != store {
                    for incident in self.incidents.values_mut() {
                        incident.reset();
                    }
                }
                self.store = store;
                self.failure = None;
                Ok(())
            }
            Err(error) => {
                for incident in self.incidents.values_mut() {
                    incident.seen = None;
                    incident.cpu_since = None;
                    incident.memory_since = None;
                }
                self.failure = Some(error.clone());
                Err(error)
            }
        }
    }
    // Reload alert preferences and return the selected plugin settings with their revision
    fn settings(&mut self, id: &str) -> Result<ResourceAlertSettings, String> {
        self.reload()?;
        Ok(ResourceAlertSettings {
            id: id.into(),
            revision: self.store.revision,
            preferences: self.store.entries.get(id).cloned().unwrap_or_default(),
        })
    }
    // Save the supplied resource alerts state
    fn save(&mut self, value: ResourceAlertSettings) -> Result<ResourceAlertSettings, String> {
        value.preferences.validate()?;
        self.reload()?;
        let mut store = self.store.clone();
        if store.revision != value.revision {
            return Err("Resource alert preferences changed; reload before saving".into());
        }
        store
            .entries
            .insert(value.id.clone(), value.preferences.clone());
        if store.entries.len() > 1024 {
            return Err("Resource alerts exceed 1,024 plugin preferences".into());
        }
        store.revision = store
            .revision
            .checked_add(1)
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or("Resource preference revision exhausted")?;
        if serde_json::to_vec(&store).map_err(|e| e.to_string())?.len() > 1024 * 1024 {
            return Err("Resource alert preferences exceed 1 MiB".into());
        }
        crate::profile_database::LocalPreference::ResourceAlerts
            .record::<Store>()
            .write(&self.storage, &store)?;
        let revision = store.revision;
        self.store = store;
        self.failure = None;
        if let Some(incident) = self.incidents.get_mut(&value.id) {
            incident.reset();
        }
        Ok(ResourceAlertSettings { revision, ..value })
    }
    // Report whether there is incidents
    pub(crate) fn has_incidents(&self) -> bool {
        !self.incidents.is_empty()
    }
    // Retain runs
    pub(crate) fn retain_runs(&mut self, runs: &HashSet<(String, String)>, now: Instant) {
        self.incidents.retain(|id, incident| {
            if incident
                .run
                .as_ref()
                .is_none_or(|run| !runs.contains(&(id.clone(), run.clone())))
            {
                incident.reset();
            }
            incident.run.is_some()
                || incident
                    .last_warning
                    .is_some_and(|at| now.saturating_duration_since(at) < COOLDOWN)
        });
    }
    // Update resource alerts from the new observation
    pub(crate) fn observe(
        &mut self,
        id: &str,
        run: &str,
        uptime_ms: u64,
        now: Instant,
        resources: &PluginResources,
    ) -> Option<String> {
        if self.failure.is_some() {
            return None;
        }
        let prefs = self.store.entries.get(id).filter(|p| p.enabled)?;
        self.incidents
            .entry(id.into())
            .or_default()
            .observe(prefs, run, uptime_ms, now, resources)
    }
}
impl Runtime {
    // Read resource-alert settings only for an installed plugin
    pub fn resource_alert_settings(&self, id: &str) -> Result<ResourceAlertSettings, String> {
        if !self
            .plugins
            .lock()
            .unwrap()
            .list()
            .iter()
            .any(|p| p.id == id)
        {
            return Err("Plugin is not installed".into());
        }
        self.resource_alerts.lock().unwrap().settings(id)
    }
    // Save resource alert settings
    pub fn save_resource_alert_settings(
        &self,
        value: ResourceAlertSettings,
    ) -> Result<ResourceAlertSettings, String> {
        if !self
            .plugins
            .lock()
            .unwrap()
            .list()
            .iter()
            .any(|p| p.id == value.id)
        {
            return Err("Plugin is not installed".into());
        }
        let result = self.resource_alerts.lock().unwrap().save(value)?;
        self.sampling_signal.notify();
        Ok(result)
    }
}

#[cfg(test)]
mod tests;

// Discard the selected plugin in state
pub(crate) fn forget_plugin_in(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
    id: &str,
) -> Result<(), String> {
    validate_storage_snapshot(connection)?;
    if let Some(mut stored) = crate::profile_database::LocalPreference::ResourceAlerts
        .record::<Store>()
        .read_from(connection)?
    {
        if stored.entries.remove(id).is_some() {
            stored.revision = stored
                .revision
                .checked_add(1)
                .ok_or("Resource alert revision exhausted")?;
            crate::profile_database::LocalPreference::ResourceAlerts
                .record::<Store>()
                .write_to(connection, &stored)?;
        }
    }
    Ok(())
}
