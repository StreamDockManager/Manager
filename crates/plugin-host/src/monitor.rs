//! Cached runtime status and bounded, versioned health records. Live samples are never persisted
use super::*;

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    #[default]
    Disabled,
    Idle,
    Queued,
    Starting,
    Running,
    Stopping,
    Retrying,
    Blocked,
}
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeReason {
    #[default]
    Disabled,
    NoActiveDevice,
    NoActiveProfileDemand,
    NoVisibleActions,
    RequiredByActiveProfile,
    ManualAction,
    Configuration,
    Registration,
    MissingRuntime,
    Compatibility,
    UnexpectedExit,
    RegistrationTimeout,
    RepeatedFailures,
    UserRestart,
    Shutdown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginDemand {
    pub device_id: String,
    pub profile_id: String,
}
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementState {
    WarmingUp,
    Available,
    Stale,
    #[default]
    Unavailable,
    NotRunning,
}
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementSource {
    #[default]
    Unavailable,
    ProcessTreeRss,
    SharedWebview,
    SdkPayload,
}
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementCoverage {
    #[default]
    Unavailable,
    Partial,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ResourceSample {
    #[ts(type = "number")]
    pub timestamp_ms: u64,
    pub cpu_percent: Option<f64>,
    #[ts(type = "number | null")]
    pub resident_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginResources {
    pub state: MeasurementState,
    pub source: MeasurementSource,
    pub cpu_percent: Option<f64>,
    #[ts(type = "number | null")]
    pub resident_bytes: Option<u64>,
    #[ts(type = "number")]
    pub peak_resident_bytes: u64,
    #[serde(default)]
    pub peak_cpu_percent: Option<f64>,
    pub process_count: Option<usize>,
    pub root_pid: Option<u32>,
    #[ts(type = "number | null")]
    pub sampled_at_ms: Option<u64>,
    pub history: Vec<ResourceSample>,
}
impl Default for PluginResources {
    // Construct the default value
    fn default() -> Self {
        Self {
            state: MeasurementState::NotRunning,
            source: MeasurementSource::Unavailable,
            cpu_percent: None,
            resident_bytes: None,
            peak_resident_bytes: 0,
            peak_cpu_percent: None,
            process_count: None,
            root_pid: None,
            sampled_at_ms: None,
            history: vec![],
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginNetwork {
    pub state: MeasurementState,
    pub source: MeasurementSource,
    pub coverage: MeasurementCoverage,
    #[ts(type = "number | null")]
    pub sent_bytes: Option<u64>,
    #[ts(type = "number | null")]
    pub received_bytes: Option<u64>,
    pub upload_bytes_per_second: Option<f64>,
    pub download_bytes_per_second: Option<f64>,
    #[ts(type = "number | null")]
    pub sampled_at_ms: Option<u64>,
}
impl Default for PluginNetwork {
    // Construct the default value
    fn default() -> Self {
        Self {
            state: MeasurementState::Unavailable,
            source: MeasurementSource::Unavailable,
            coverage: MeasurementCoverage::Unavailable,
            sent_bytes: None,
            received_bytes: None,
            upload_bytes_per_second: None,
            download_bytes_per_second: None,
            sampled_at_ms: None,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginRuntime {
    pub state: RuntimeState,
    pub reason: RuntimeReason,
    pub demand: Vec<PluginDemand>,
    pub run_id: Option<String>,
    #[ts(type = "number | null")]
    pub started_at_ms: Option<u64>,
    /// Last admitted start in this Manager session; retains only categorical reasons
    #[serde(default)]
    pub last_wake_reason: Option<RuntimeReason>,
    #[serde(default)]
    #[ts(type = "number | null")]
    pub last_wake_at_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub uptime_ms: Option<u64>,
    pub resources: PluginResources,
    pub network: PluginNetwork,
}
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
pub struct HealthCounts {
    #[ts(type = "number")]
    pub crashes: u64,
    #[ts(type = "number")]
    pub failed_starts: u64,
    #[ts(type = "number")]
    pub automatic_restarts: u64,
    #[ts(type = "number")]
    pub issues: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct HealthEvent {
    pub id: String,
    #[ts(type = "number")]
    pub timestamp_ms: u64,
    pub version: String,
    pub run_id: Option<String>,
    pub category: String,
    pub severity: String,
    pub message: String,
    #[ts(type = "number")]
    pub occurrences: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginRuntimeSnapshot {
    pub id: String,
    pub status: String,
    pub enabled: bool,
    pub runtime: PluginRuntime,
    pub health: PluginHealth,
}
#[derive(Clone)]
pub struct MonitorTarget {
    pub id: String,
    pub run_id: String,
    pub root_pid: Option<u32>,
    pub browser: bool,
    pub started_at_ms: u64,
}

/// Stable health data only: no uptime, resource history, icons or action metadata
#[derive(Clone)]
pub struct PluginHealthSnapshot {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub health: PluginHealth,
}
// Read the current timestamp in milliseconds
pub(super) fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Plugin {
    // Read the resource observation at the supplied time
    pub(super) fn resources_at(&self, now: std::time::Instant) -> PluginResources {
        let mut resources = self.info.runtime.resources.clone();
        let wall_now = timestamp_ms();
        resources.history = self
            .resource_history
            .iter()
            .filter(|sample| wall_now.saturating_sub(sample.timestamp_ms) <= 300_000)
            .cloned()
            .collect();
        if !matches!(
            self.info.runtime.state,
            RuntimeState::Starting | RuntimeState::Running
        ) {
            return resources;
        }
        if self.resource_sampled_at.is_none() {
            resources.source = if self.info.kind == "browser" {
                MeasurementSource::SharedWebview
            } else {
                MeasurementSource::ProcessTreeRss
            };
            resources.state = if self.info.kind == "browser" {
                MeasurementState::Unavailable
            } else {
                MeasurementState::WarmingUp
            };
            resources.root_pid = self.child.as_ref().map(Child::id);
        }
        // The slowest sampling cadence is five seconds. Allow two intervals,
        // then stop presenting an old reading (or an endless warmup) as current.
        // Use monotonic time: wall-clock corrections must not revive stale data
        if matches!(
            resources.state,
            MeasurementState::Available | MeasurementState::WarmingUp
        ) && self
            .resource_sampled_at
            .or(self.started)
            .is_some_and(|at| now.saturating_duration_since(at) > Duration::from_secs(10))
        {
            resources.state = MeasurementState::Stale;
            resources.cpu_percent = None;
            resources.resident_bytes = None;
            resources.process_count = None;
        }
        resources
    }
}

impl PluginHealth {
    // Record for the current operation
    pub(super) fn record(
        &mut self,
        category: &str,
        message: &str,
        version: &str,
        run_id: Option<String>,
    ) {
        let now = timestamp_ms();
        let message: String = message.chars().take(1024).collect();
        if let Some(event) = self.events.iter_mut().rev().find(|e| {
            e.category == category
                && e.message == message
                && e.run_id == run_id
                && now.saturating_sub(e.timestamp_ms) <= 10_000
        }) {
            event.occurrences = event.occurrences.saturating_add(1);
            if category != "automatic_restart" {
                self.last_issue_ms = Some(now);
            }
            return;
        }
        for counts in [&mut self.session, &mut self.totals] {
            match category {
                "crash" => counts.crashes += 1,
                "failed_start" => counts.failed_starts += 1,
                "automatic_restart" => counts.automatic_restarts += 1,
                _ => {}
            }
            if category != "automatic_restart" {
                counts.issues += 1;
            }
        }
        if category != "automatic_restart" {
            self.last_issue_ms = Some(now);
        }
        if category == "crash" {
            self.last_unexpected_exit_ms = Some(now);
        }
        self.events.push(HealthEvent {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp_ms: now,
            version: version.into(),
            run_id,
            category: category.into(),
            severity: if category == "automatic_restart" {
                "info"
            } else if category == "resource_warning" {
                "warning"
            } else {
                "error"
            }
            .into(),
            message: message.clone(),
            occurrences: 1,
        });
        if self.events.len() > 100 {
            self.events.remove(0);
        }
        if category != "automatic_restart" && category != "resource_warning" {
            self.recent_failures.push(message);
            if self.recent_failures.len() > 20 {
                self.recent_failures.remove(0);
            }
        }
    }
}
pub(super) struct NetworkReport {
    pub run_id: String,
    pub sampled_at: std::time::Instant,
    sent: u64,
    received: u64,
    last_sent: u64,
    last_received: u64,
    pub reading: PluginNetwork,
}
impl Hub {
    // Report network
    pub(super) fn report_network(
        &mut self,
        owner: &str,
        run: &str,
        payload: &Value,
        now: std::time::Instant,
    ) -> Result<bool, String> {
        if payload["run_id"].as_str() != Some(run) || payload.to_string().len() > 1024 {
            return Err("Invalid network payload report".into());
        }
        let (sent, received) = payload["sent_bytes"]
            .as_u64()
            .zip(payload["received_bytes"].as_u64())
            .filter(|(s, r)| *s <= 9_007_199_254_740_991 && *r <= 9_007_199_254_740_991)
            .ok_or("Invalid network payload counters")?;
        let mut rates = None;
        if let Some(previous) = self.network.get_mut(owner) {
            if previous.run_id != run
                || sent < previous.last_sent
                || received < previous.last_received
            {
                return Err("Non-monotonic or stale-run network counters".into());
            }
            previous.last_sent = sent;
            previous.last_received = received;
            if now.duration_since(previous.sampled_at) < Duration::from_millis(900) {
                return Ok(false);
            }
            let seconds = now.duration_since(previous.sampled_at).as_secs_f64();
            rates = Some((
                (sent - previous.sent) as f64 / seconds,
                (received - previous.received) as f64 / seconds,
            ));
        }
        let reading = PluginNetwork {
            state: if rates.is_some() {
                MeasurementState::Available
            } else {
                MeasurementState::WarmingUp
            },
            source: MeasurementSource::SdkPayload,
            coverage: MeasurementCoverage::Partial,
            sent_bytes: Some(sent),
            received_bytes: Some(received),
            upload_bytes_per_second: rates.map(|v| v.0),
            download_bytes_per_second: rates.map(|v| v.1),
            sampled_at_ms: Some(timestamp_ms()),
        };
        self.network.insert(
            owner.into(),
            NetworkReport {
                run_id: run.into(),
                sampled_at: now,
                sent,
                received,
                last_sent: sent,
                last_received: received,
                reading,
            },
        );
        Ok(true)
    }

    // Construct a health issue for the plugin monitor
    pub(super) fn issue(&mut self, id: &str, category: &str, message: &str) {
        let (version, run_id) = self.run_details.get(id).cloned().unwrap_or_default();
        self.health
            .entry(id.into())
            .or_default()
            .record(category, message, &version, run_id);
        self.health_dirty = true;
    }
    // Flush health
    pub(super) fn flush_health(&mut self) -> Result<(), String> {
        if !self.health_dirty {
            return Ok(());
        }
        let Some(storage) = &self.store else {
            return Ok(());
        };
        if self.health.len() > 4096 {
            return Err("Plugin health index exceeds bounds".into());
        }
        let mut retained_bytes = 0usize;
        for (id, health) in &self.health {
            retained_bytes = retained_bytes
                .checked_add(id.len())
                .and_then(|size| {
                    serde_json::to_vec(health)
                        .ok()
                        .and_then(|value| size.checked_add(value.len()))
                })
                .ok_or("Plugin health exceeds bounds")?;
            if retained_bytes > 16 * 1024 * 1024 {
                return Err("Plugin health exceeds 16 MiB".into());
            }
        }
        let health = self.health.clone();
        storage.transaction_with_cost(retained_bytes, move |tx| {
            let domain = stream_dock_manager_storage::Domain::PluginHealth;
            tx.execute(&format!("DELETE FROM {}", domain.table()), [])
                .map_err(|e| e.to_string())?;
            for (id, value) in health {
                stream_dock_manager_storage::write_json(tx, domain, &id, &value, 16 * 1024 * 1024)?;
            }
            Ok(())
        })?;
        self.health_dirty = false;
        Ok(())
    }
}
impl PluginHost {
    // Capture the current plugin health state
    pub fn health_snapshot(&self) -> Vec<PluginHealthSnapshot> {
        let hub = self.hub.lock().unwrap();
        self.plugins
            .values()
            .map(|plugin| {
                let mut health = hub.health.get(&plugin.info.id).cloned().unwrap_or_default();
                health.restart_attempts = plugin.restarts;
                PluginHealthSnapshot {
                    id: plugin.info.id.clone(),
                    name: plugin.info.name.clone(),
                    enabled: hub.enabled.contains(&plugin.info.id),
                    health,
                }
            })
            .collect()
    }
    // Capture the current plugin runtime state
    pub fn runtime_snapshot(&self) -> Vec<PluginRuntimeSnapshot> {
        let hub = self.hub.lock().unwrap();
        let mut snapshots: Vec<_> = self
            .plugins
            .values()
            .map(|p| {
                let mut runtime = p.info.runtime.clone();
                runtime.resources = p.resources_at(std::time::Instant::now());
                runtime.uptime_ms = p.started.map(|at| at.elapsed().as_millis() as u64);
                if let Some(report) = hub
                    .network
                    .get(&p.info.id)
                    .filter(|n| Some(n.run_id.as_str()) == runtime.run_id.as_deref())
                {
                    runtime.network = report.reading.clone();
                    if report.sampled_at.elapsed() > Duration::from_secs(5) {
                        runtime.network.state = MeasurementState::Stale;
                        runtime.network.upload_bytes_per_second = None;
                        runtime.network.download_bytes_per_second = None;
                    }
                }
                let mut health = hub.health.get(&p.info.id).cloned().unwrap_or_default();
                health.restart_attempts = p.restarts;
                PluginRuntimeSnapshot {
                    id: p.info.id.clone(),
                    status: p.info.status.clone(),
                    enabled: hub.enabled.contains(&p.info.id),
                    runtime,
                    health,
                }
            })
            .collect();
        snapshots.sort_by(|a, b| a.id.cmp(&b.id));
        snapshots
    }
    // Collect processes eligible for plugin resource monitoring
    pub fn monitor_targets(&self) -> Vec<MonitorTarget> {
        self.plugins
            .values()
            .filter(|p| {
                matches!(
                    p.info.runtime.state,
                    RuntimeState::Starting | RuntimeState::Running
                )
            })
            .filter_map(|p| {
                Some(MonitorTarget {
                    id: p.info.id.clone(),
                    run_id: p.info.runtime.run_id.clone()?,
                    root_pid: p.child.as_ref().map(Child::id),
                    browser: p.info.kind == "browser",
                    started_at_ms: p.info.runtime.started_at_ms.unwrap_or(0),
                })
            })
            .collect()
    }
    // Check whether a sample belongs to the current plugin run
    pub fn resource_run_current(&self, id: &str, run: &str) -> bool {
        self.plugins.get(id).is_some_and(|p| {
            p.info.runtime.run_id.as_deref() == Some(run)
                && p.info.runtime.state == RuntimeState::Running
                && p.info.enabled
        })
    }
    // Record resource warning
    pub fn record_resource_warning(&mut self, id: &str, run: &str, message: &str) -> bool {
        if !self.resource_run_current(id, run) {
            return false;
        }
        self.hub
            .lock()
            .unwrap()
            .issue(id, "resource_warning", message);
        true
    }
    // Record resources
    pub fn record_resources(&mut self, id: &str, run_id: &str, mut resources: PluginResources) {
        let Some(plugin) = self.plugins.get_mut(id).filter(|p| {
            p.info.runtime.run_id.as_deref() == Some(run_id)
                && matches!(
                    p.info.runtime.state,
                    RuntimeState::Running | RuntimeState::Starting
                )
        }) else {
            return;
        };
        resources.cpu_percent = resources
            .cpu_percent
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v));
        let previous = &plugin.info.runtime.resources;
        resources.peak_cpu_percent = match (previous.peak_cpu_percent, resources.cpu_percent) {
            (Some(old), Some(current)) => Some(old.max(current)),
            (old, current) => old.or(current),
        };
        resources.peak_resident_bytes = previous
            .peak_resident_bytes
            .max(resources.resident_bytes.unwrap_or(0));
        let now = timestamp_ms();
        while plugin
            .resource_history
            .front()
            .is_some_and(|s| now.saturating_sub(s.timestamp_ms) > 300_000)
        {
            plugin.resource_history.pop_front();
        }
        if plugin.resource_history.len() >= 301 {
            plugin.resource_history.pop_front();
        }
        plugin.resource_history.push_back(ResourceSample {
            timestamp_ms: now,
            cpu_percent: resources.cpu_percent,
            resident_bytes: resources.resident_bytes,
        });
        resources.history.clear();
        plugin.resource_sampled_at = Some(std::time::Instant::now());
        plugin.info.runtime.resources = resources;
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;
    #[test]
    // Verify cumulative payload reports recover drops and reject stale runs decreases and invalid values
    fn cumulative_payload_reports_recover_drops_and_reject_stale_runs_decreases_and_invalid_values()
    {
        let mut hub = Hub::default();
        let at = std::time::Instant::now();
        let report =
            |run, sent, received| json!({"run_id":run,"sent_bytes":sent,"received_bytes":received});
        hub.report_network("plugin", "run", &report("run", 10, 20), at)
            .unwrap();
        assert_eq!(
            hub.network["plugin"].reading.state,
            MeasurementState::WarmingUp
        );
        assert!(!hub
            .report_network(
                "plugin",
                "run",
                &report("run", 20, 30),
                at + Duration::from_millis(100)
            )
            .unwrap());
        assert!(hub
            .report_network(
                "plugin",
                "run",
                &report("run", 310, 620),
                at + Duration::from_secs(3)
            )
            .unwrap());
        assert_eq!(
            hub.network["plugin"].reading.upload_bytes_per_second,
            Some(100.)
        );
        assert_eq!(
            hub.network["plugin"].reading.download_bytes_per_second,
            Some(200.)
        );
        assert_eq!(
            hub.network["plugin"].reading.coverage,
            MeasurementCoverage::Partial
        );
        assert!(hub
            .report_network(
                "plugin",
                "run",
                &report("old", 400, 800),
                at + Duration::from_secs(4)
            )
            .is_err());
        assert!(hub
            .report_network(
                "plugin",
                "run",
                &report("run", 1, 620),
                at + Duration::from_secs(4)
            )
            .is_err());
        assert!(hub
            .report_network(
                "plugin",
                "run",
                &json!({"run_id":"run","sent_bytes":-1,"received_bytes":1}),
                at + Duration::from_secs(4)
            )
            .is_err());
        hub.network.remove("plugin");
        hub.report_network(
            "plugin",
            "new",
            &report("new", 0, 0),
            at + Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(hub.network["plugin"].reading.sent_bytes, Some(0));
        assert_eq!(hub.network["plugin"].reading.upload_bytes_per_second, None);
    }
}
