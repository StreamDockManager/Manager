//! One demand-driven sampler shared by subscribed plugins
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::Weak,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct Measurement {
    pub value: Option<f64>,
    #[ts(type = "number")]
    pub observed_at: u64,
    #[ts(type = "number")]
    pub max_age_ms: u64,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct TelemetrySnapshot {
    pub audio_observer: bool,
    pub cpu: Measurement,
    pub memory: Measurement,
    pub network: Measurement,
    pub disks: BTreeMap<String, Measurement>,
    pub audio_volume: Measurement,
    pub audio_muted: Measurement,
}
// Read the clock used for telemetry
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[test]
// Verify that current telemetry requires observer hint
fn current_telemetry_requires_observer_hint() {
    let mut value = serde_json::to_value(TelemetrySnapshot::default()).unwrap();
    value.as_object_mut().unwrap().remove("audio_observer");
    assert!(serde_json::from_value::<TelemetrySnapshot>(value).is_err());
    for enabled in [false, true] {
        let value = TelemetrySnapshot {
            audio_observer: enabled,
            ..Default::default()
        };
        let decoded: TelemetrySnapshot =
            serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap();
        assert_eq!(decoded.audio_observer, enabled);
    }
}
impl Measurement {
    // Collect a telemetry sample
    fn sample(value: Option<f64>, max_age_ms: u64, error: Option<String>) -> Self {
        Self {
            value: value.filter(|v| v.is_finite()),
            observed_at: now(),
            max_age_ms,
            error,
        }
    }
    // Report the fresh condition for telemetry
    pub fn fresh(&self) -> bool {
        self.value.is_some()
            && self.error.is_none()
            && now() >= self.observed_at
            && now().saturating_sub(self.observed_at) <= self.max_age_ms
    }
}
impl TelemetrySnapshot {
    // Resolve a named telemetry measurement using the longest matching disk mount for path queries
    pub fn field(&self, name: &str) -> Option<&Measurement> {
        match name {
            "cpu" => Some(&self.cpu),
            "memory" => Some(&self.memory),
            "network" => Some(&self.network),
            "volume" => Some(&self.audio_volume),
            "muted" => Some(&self.audio_muted),
            _ => name.strip_prefix("disk:").and_then(|path| {
                self.disks
                    .iter()
                    .filter(|(mount, _)| std::path::Path::new(path).starts_with(mount))
                    .max_by_key(|(mount, _)| mount.len())
                    .map(|(_, value)| value)
            }),
        }
    }
}
// Represent unavailable system statistics as an expiring measurement with an explanatory error
fn unavailable(kind: &str, error: &sysinfo::Error) -> Measurement {
    Measurement::sample(
        None,
        2500,
        Some(format!("{kind} statistics unavailable: {error}")),
    )
}

// Start the telemetry operation
pub fn start(weak: Weak<crate::Runtime>) {
    let Some(runtime) = weak.upgrade() else {
        return;
    };
    let signal = runtime.sampling_signal.clone();
    drop(runtime);
    std::thread::spawn(move || {
        let mut observed = 0;
        let mut value = TelemetrySnapshot::default();
        let mut system = sysinfo::System::new();
        let mut networks = sysinfo::Networks::new();
        let mut disks = sysinfo::Disks::new();
        let mut host_at = Instant::now() - Duration::from_secs(5);
        let mut disk_at = host_at;
        let mut primed = false;
        let mut network_primed = false;
        let mut audio_at = None;
        loop {
            let Some(runtime) = weak.upgrade() else { break };
            if runtime.is_stopping() {
                break;
            }
            let topics = runtime.plugins.lock().unwrap().telemetry_topics();

            let mut changed = false;
            if !topics.contains("cpu") {
                primed = false;
            }
            if !topics.contains("network") {
                network_primed = false;
            }
            if topics.is_empty() {
                primed = false;
                network_primed = false;
            }
            if host_at.elapsed() >= Duration::from_secs(1)
                && topics
                    .iter()
                    .any(|t| ["cpu", "memory", "network"].contains(&t.as_str()))
            {
                let elapsed = host_at.elapsed().as_secs_f64();
                host_at = Instant::now();
                if system.is_err() && (topics.contains("cpu") || topics.contains("memory")) {
                    system = sysinfo::System::new();
                }
                if topics.contains("cpu") {
                    if let Ok(system) = &mut system {
                        system.refresh_cpu_usage();
                        value.cpu = Measurement::sample(
                            (primed && !system.cpus().is_empty())
                                .then(|| f64::from(system.global_cpu_usage())),
                            2500,
                            if system.cpus().is_empty() {
                                Some("CPU statistics unavailable".into())
                            } else {
                                (!primed).then(|| "Warming up".into())
                            },
                        );
                        primed = true;
                    } else {
                        value.cpu = unavailable("CPU", system.as_ref().unwrap_err());
                        primed = false;
                    }
                }
                if topics.contains("memory") {
                    if let Ok(system) = &mut system {
                        system.refresh_memory();
                        let total = system.total_memory();
                        value.memory = Measurement::sample(
                            (total > 0).then(|| system.used_memory() as f64 * 100.0 / total as f64),
                            2500,
                            (total == 0).then(|| "Memory statistics unavailable".into()),
                        );
                    } else {
                        value.memory = unavailable("Memory", system.as_ref().unwrap_err());
                    }
                }
                if topics.contains("network") {
                    if networks.is_err() {
                        networks = sysinfo::Networks::new();
                    }
                    if let Ok(networks) = &mut networks {
                        networks.refresh(true);
                        let bytes = networks
                            .values()
                            .map(|v| v.received() + v.transmitted())
                            .sum::<u64>();
                        value.network = Measurement::sample(
                            network_primed.then(|| bytes as f64 / elapsed),
                            2500,
                            (!network_primed).then(|| "Warming up".into()),
                        );
                        network_primed = true;
                    } else {
                        value.network = unavailable("Network", networks.as_ref().unwrap_err());
                        network_primed = false;
                    }
                }
                changed = true;
            }
            if topics.contains("disk") && disk_at.elapsed() >= Duration::from_secs(5) {
                disk_at = Instant::now();
                if disks.is_err() {
                    disks = sysinfo::Disks::new();
                }
                if let Ok(disks) = &mut disks {
                    disks.refresh(true);
                    value.disks = disks
                        .iter()
                        .map(|disk| {
                            let total = disk.total_space();
                            (
                                disk.mount_point().to_string_lossy().into_owned(),
                                Measurement::sample(
                                    (total > 0).then(|| {
                                        (total - disk.available_space()) as f64 * 100.0
                                            / total as f64
                                    }),
                                    11000,
                                    (total == 0).then(|| "Disk statistics unavailable".into()),
                                ),
                            )
                        })
                        .collect();
                } else {
                    for measurement in value.disks.values_mut() {
                        *measurement = unavailable("Disk", disks.as_ref().unwrap_err());
                    }
                }
                changed = true;
            }
            if topics.contains("audio") {
                let feedback = runtime.feedback_state.lock().unwrap();
                if value.audio_observer != feedback.audio_observer {
                    value.audio_observer = feedback.audio_observer;
                    changed = true;
                }
                if feedback
                    .sampled_at
                    .is_some_and(|at| at.elapsed() < Duration::from_secs(2) && Some(at) != audio_at)
                {
                    audio_at = feedback.sampled_at;
                    value.audio_volume = Measurement::sample(
                        feedback.audio.volume.map(f64::from),
                        if feedback.audio_observer { 11000 } else { 1000 },
                        feedback.audio.error.clone(),
                    );
                    value.audio_muted = Measurement::sample(
                        feedback.audio.muted.map(|v| if v { 1.0 } else { 0.0 }),
                        if feedback.audio_observer { 11000 } else { 1000 },
                        feedback.audio.error.clone(),
                    );
                    let observed = now()
                        .saturating_sub(feedback.sampled_at.unwrap().elapsed().as_millis() as u64);
                    value.audio_volume.observed_at = observed;
                    value.audio_muted.observed_at = observed;
                    changed = true;
                }
            } else if value.audio_observer {
                value.audio_observer = false;
                // Stop advertising an observation lease once no subscribed
                // consumer is watching it. Retain the actual sample timestamp
                value.audio_volume.max_age_ms = 1000;
                value.audio_muted.max_age_ms = 1000;
                changed = true;
            }
            if changed {
                *runtime.telemetry.lock().unwrap() = value.clone();
                runtime
                    .plugins
                    .lock()
                    .unwrap()
                    .publish_telemetry(serde_json::to_value(&value).unwrap());
                crate::feedback::wake(&runtime);
            }
            let mut timeout: Option<Duration> = None;
            let mut due = |remaining: Duration| {
                timeout = Some(timeout.map_or(remaining, |old| old.min(remaining)));
            };
            if topics
                .iter()
                .any(|t| ["cpu", "memory", "network"].contains(&t.as_str()))
            {
                due(Duration::from_secs(1).saturating_sub(host_at.elapsed()));
            }
            if topics.contains("disk") {
                due(Duration::from_secs(5).saturating_sub(disk_at.elapsed()));
            }
            if topics.contains("audio") {
                due(Duration::from_millis(250));
            }
            if topics.is_empty() {
                system = sysinfo::System::new();
                networks = sysinfo::Networks::new();
                disks = sysinfo::Disks::new();
            }
            drop(runtime);
            if !signal.wait(&mut observed, timeout) {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that timestamps and unavailable values never become fresh
    fn timestamps_and_unavailable_values_never_become_fresh() {
        let mut value = Measurement::sample(Some(42.0), 1000, None);
        assert!(value.fresh());
        value.observed_at = now() - 2000;
        assert!(!value.fresh());
        value.observed_at = now() + 10000;
        assert!(!value.fresh());
        assert!(!Measurement::sample(None, 1000, Some("Unavailable".into())).fresh());
        assert!(!Measurement::sample(Some(f64::NAN), 1000, None).fresh());
    }
}
