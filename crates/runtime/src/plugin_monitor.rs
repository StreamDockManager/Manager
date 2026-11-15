//! A single sampler. OS process refresh runs outside lifecycle, state, and input locks
use std::collections::{HashMap, HashSet};
use std::sync::{atomic::Ordering, Weak};
use std::time::{Duration, Instant};
use stream_dock_manager_plugin_host::{
    MeasurementSource, MeasurementState, PluginHealth, PluginHealthSnapshot, PluginResources,
    PluginRuntimeSnapshot,
};

// Choose the shorter sampling interval only while plugin monitoring is visible
fn sample_interval(visible: bool) -> Duration {
    Duration::from_secs(if visible { 1 } else { 5 })
}

// Compute the remaining delay before the next visibility-dependent sample
fn sample_remaining(last: Option<Instant>, now: Instant, visible: bool) -> Duration {
    last.map(|last| sample_interval(visible).saturating_sub(now.saturating_duration_since(last)))
        .unwrap_or_default()
}

struct HealthObserver {
    previous: HashMap<String, PluginHealth>,
    incidents: HashMap<String, HashMap<(String, String), Instant>>,
    started_at_ms: u64,
}

impl HealthObserver {
    // Initialize plugin monitor state from the supplied inputs
    fn new(snapshots: Vec<PluginHealthSnapshot>) -> Self {
        Self {
            previous: snapshots.into_iter().map(|s| (s.id, s.health)).collect(),
            incidents: HashMap::new(),
            started_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    // Update plugin monitor from the new observation
    fn observe(
        &mut self,
        snapshots: Vec<PluginHealthSnapshot>,
        now: Instant,
    ) -> (bool, Vec<String>) {
        let mut previous = HashMap::new();
        let mut changed = false;
        let mut alerts = vec![];
        for snapshot in snapshots {
            let old = self.previous.get(&snapshot.id);
            let health = &snapshot.health;
            changed |= old != Some(health);
            let incidents = self.incidents.entry(snapshot.id.clone()).or_default();
            // Keep at most the bounded health history's distinct issues. A minute
            // without another occurrence rearms an issue after recovery
            incidents.retain(|(category, message), seen| {
                now.saturating_duration_since(*seen) < Duration::from_secs(60)
                    && health
                        .events
                        .iter()
                        .any(|e| &e.category == category && &e.message == message)
            });
            let mut cause = None;
            for event in &health.events {
                if !matches!(event.severity.as_str(), "warning" | "error")
                    // Resource thresholds already have their own incident alerts
                    || event.category == "resource_warning"
                    || old.is_some_and(|h| h.events.iter().any(|e| e.id == event.id && e.occurrences == event.occurrences))
                    || (old.is_none() && event.timestamp_ms < self.started_at_ms)
                {
                    continue;
                }
                let key = (event.category.clone(), event.message.clone());
                if incidents.insert(key, now).is_none() && snapshot.enabled {
                    cause = Some(event.message.clone());
                }
            }
            // Escalation to blocked is actionable even if automatic retries
            // keep reporting the same crash. Retain the underlying explanation
            if snapshot.enabled && old.is_some_and(|h| h.blocked_reason != health.blocked_reason) {
                if let Some(reason) = &health.blocked_reason {
                    cause = Some(
                        match health.events.iter().rev().find(|e| e.severity == "error") {
                            Some(event) if !reason.contains(&event.message) => {
                                format!("{reason}. {}", event.message)
                            }
                            _ => reason.clone(),
                        },
                    );
                }
            }
            if let Some(cause) = cause {
                alerts.push(format!(
                    "Plugin \"{}\" has a health issue: {cause}. Open Plugins → Health and compatibility for details.",
                    snapshot.name
                ));
            }
            previous.insert(snapshot.id, snapshot.health);
        }
        changed |= self.previous.keys().any(|id| !previous.contains_key(id));
        self.incidents.retain(|id, _| previous.contains_key(id));
        self.previous = previous;
        (changed, alerts)
    }
}

impl crate::Runtime {
    // Read the plugin lifecycle and resource snapshot under the plugin lock
    pub fn plugin_runtime_snapshot(&self) -> Vec<PluginRuntimeSnapshot> {
        self.plugins.lock().unwrap().runtime_snapshot()
    }
    // Update the qualification consumer’s plugin-monitor visibility demand
    pub fn plugin_monitor_visible(&self, visible: bool) {
        let _ = self.plugin_monitor_visibility("qualification", visible);
    }
    // Track visibility per consumer and wake sampling when demand changes
    pub fn plugin_monitor_visibility(&self, consumer: &str, visible: bool) -> Result<(), String> {
        if consumer.is_empty() || consumer.len() > 80 {
            return Err("Invalid monitoring consumer".into());
        }
        let mut consumers = self.plugin_monitor_consumers.lock().unwrap();
        if visible {
            if consumers.len() >= 8 && !consumers.contains(consumer) {
                return Err("Too many monitoring views".into());
            }
            consumers.insert(consumer.into());
        } else {
            consumers.remove(consumer);
        }
        self.plugin_monitor_visible
            .store(!consumers.is_empty(), Ordering::Relaxed);
        self.sampling_signal.notify();
        Ok(())
    }
}
#[derive(Default)]
struct Ownership {
    root: Option<(u32, u64)>,
    processes: HashSet<(u32, u64)>,
    samples: usize,
}
use crate::process_resources::Process as ProcessMeasurement;
impl Ownership {
    // Measure the plugin monitor operation
    fn measure(
        &mut self,
        pid: u32,
        processes: &HashMap<u32, ProcessMeasurement>,
        children: &HashMap<u32, Vec<u32>>,
        logical: usize,
        started_at_ms: u64,
    ) -> Option<(Option<f64>, u64, usize)> {
        let root = processes.get(&pid)?;
        let identity = (pid, root.created);
        if self.root.is_none() {
            if root.created.saturating_add(1) < started_at_ms / 1000 {
                return None;
            }
            self.root = Some(identity);
            self.processes.insert(identity);
        }
        if self.root != Some(identity) {
            return None;
        }
        self.processes
            .retain(|(pid, created)| processes.get(pid).is_some_and(|p| p.created == *created));
        let mut pending: Vec<_> = self.processes.iter().map(|(pid, _)| *pid).collect();
        while let Some(parent) = pending.pop() {
            for pid in children.get(&parent).into_iter().flatten() {
                if *pid == std::process::id() {
                    continue;
                }
                let process = &processes[pid];
                // Windows can retain a dead parent's PID on a surviving child
                // If that PID is reused, an older process is not a descendant
                // of the newly launched parent whose identity we own
                if process.created < processes[&parent].created {
                    continue;
                }
                if self.processes.insert((*pid, process.created)) {
                    pending.push(*pid);
                }
            }
        }
        let mut memory = 0u64;
        let mut cpu = 0f64;
        for (pid, _) in &self.processes {
            if let Some(p) = processes.get(pid) {
                memory = memory.saturating_add(p.memory);
                cpu += p.cpu;
            }
        }
        if memory == 0 {
            return None;
        }
        let cpu = (self.samples > 0 && cpu.is_finite())
            .then_some((cpu / logical.max(1) as f64).clamp(0., 100.));
        self.samples += 1;
        Some((cpu, memory, self.processes.len()))
    }
}
// Index child processes by parent PID without summing their resources
fn children_of(processes: &HashMap<u32, ProcessMeasurement>) -> HashMap<u32, Vec<u32>> {
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for (pid, process) in processes {
        if let Some(parent) = process.parent {
            children.entry(parent).or_default().push(*pid);
        }
    }
    children
}
// Spawn the demand-driven plugin health and process-resource sampler
pub(crate) fn start(runtime: Weak<crate::Runtime>) {
    let Some(owner) = runtime.upgrade() else {
        return;
    };
    let signal = owner.sampling_signal.clone();
    let mut health = HealthObserver::new(owner.plugins.lock().unwrap().health_snapshot());
    drop(owner);
    // Keep sampling ownership weak so the monitor cannot retain a stopped runtime
    std::thread::spawn(move || {
        let mut observed = 0;
        let mut owners = HashMap::<String, Ownership>::new();
        let mut last_sample = None;
        loop {
            let Some(runtime) = runtime.upgrade() else {
                break;
            };
            if runtime.is_stopping() {
                break;
            }
            let targets = runtime.plugins.lock().unwrap().monitor_targets();
            let has_targets = !targets.is_empty();
            let visible = has_targets && runtime.plugin_monitor_visible.load(Ordering::Relaxed);
            {
                let mut alerts = runtime.resource_alerts.lock().unwrap();
                if alerts.has_incidents() {
                    alerts.retain_runs(
                        &targets
                            .iter()
                            .map(|t| (t.id.clone(), t.run_id.clone()))
                            .collect(),
                        Instant::now(),
                    );
                }
            }
            if targets.is_empty() {
                owners.clear();
                runtime
                    .process_resources
                    .lock()
                    .unwrap()
                    .retire_if_idle(Instant::now());
            }
            if sample_remaining(last_sample, Instant::now(), visible).is_zero() {
                let at = Instant::now();
                let snapshots = runtime.plugins.lock().unwrap().health_snapshot();
                let (changed, alerts) = health.observe(snapshots, at);
                for message in alerts {
                    runtime.emit("plugin-health-warning", None, None, Some(message));
                }
                let native = targets.iter().any(|t| !t.browser);
                let logical_cpus = std::thread::available_parallelism()
                    .map(|c| c.get())
                    .unwrap_or(1);
                // Share the OS observation, not ownership/history. Drop its lock
                // before acquiring plugin state or recording alerts
                let snapshot = native
                    .then(|| runtime.process_resources.lock().unwrap().sample().ok())
                    .flatten();
                let empty = HashMap::new();
                let processes = snapshot.as_ref().map_or(&empty, |s| &s.processes);
                let available = snapshot.is_some();
                let children = children_of(processes);
                let now_ms = snapshot.as_ref().map_or_else(
                    || {
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64
                    },
                    |s| s.timestamp_ms,
                );
                let runs: HashSet<_> = targets.iter().map(|t| t.run_id.clone()).collect();
                owners.retain(|run, _| runs.contains(run));
                for target in targets {
                    let mut resources = PluginResources {
                        state: MeasurementState::Unavailable,
                        source: if target.browser {
                            MeasurementSource::SharedWebview
                        } else {
                            MeasurementSource::ProcessTreeRss
                        },
                        sampled_at_ms: Some(now_ms),
                        root_pid: target.root_pid,
                        ..Default::default()
                    };
                    if let Some(pid) = target
                        .root_pid
                        .filter(|pid| available && *pid != std::process::id())
                    {
                        let owner = owners.entry(target.run_id.clone()).or_default();
                        if let Some((cpu, memory, count)) = owner.measure(
                            pid,
                            processes,
                            &children,
                            logical_cpus,
                            target.started_at_ms,
                        ) {
                            resources.state = if cpu.is_some() {
                                MeasurementState::Available
                            } else {
                                MeasurementState::WarmingUp
                            };
                            resources.cpu_percent = cpu;
                            resources.resident_bytes = Some(memory);
                            resources.process_count = Some(count);
                        } else {
                            resources.state = MeasurementState::Stale;
                        }
                    }
                    let mut host = runtime.plugins.lock().unwrap();
                    let warning = if host.resource_run_current(&target.id, &target.run_id) {
                        runtime.resource_alerts.lock().unwrap().observe(
                            &target.id,
                            &target.run_id,
                            now_ms.saturating_sub(target.started_at_ms),
                            Instant::now(),
                            &resources,
                        )
                    } else {
                        None
                    };
                    host.record_resources(&target.id, &target.run_id, resources);
                    let warning = warning.filter(|message| {
                        host.record_resource_warning(&target.id, &target.run_id, message)
                    });
                    drop(host);
                    if let Some(message) = warning {
                        runtime.emit(
                            "resource-warning",
                            None,
                            None,
                            Some(format!("{}: {message}", target.id)),
                        );
                    }
                }
                runtime
                    .metrics
                    .record("plugin_sample_ms", at.elapsed().as_secs_f64() * 1000.);
                // Dedicated event, without configuration revisions or device restore/render work
                if changed {
                    runtime.emit("plugin-health", None, None, None);
                } else if has_targets {
                    runtime.emit_plugin_runtime();
                }
                last_sample = Some(Instant::now());
            }
            // Keep a slow health check even with no running targets: a failed
            // start or a blocked plugin must still reach the global alert stack
            let timeout = Some(sample_remaining(last_sample, Instant::now(), visible));
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
    use stream_dock_manager_plugin_host::HealthEvent;

    // Construct the enabled plugin health snapshot used by the test
    fn health_sample() -> PluginHealthSnapshot {
        PluginHealthSnapshot {
            id: "fixture.health".into(),
            name: "Health fixture".into(),
            enabled: true,
            health: PluginHealth::default(),
        }
    }

    // Construct a single crash event with a fixed run identity for the test
    fn failure(id: &str, message: &str) -> HealthEvent {
        HealthEvent {
            id: id.into(),
            timestamp_ms: 1,
            version: "0.1.0".into(),
            run_id: Some("run".into()),
            category: "crash".into(),
            severity: "error".into(),
            message: message.into(),
            occurrences: 1,
        }
    }

    #[test]
    // Verify that health changes alert once per issue and rearm after a quiet minute
    fn health_changes_alert_once_per_issue_and_rearm_after_a_quiet_minute() {
        let now = Instant::now();
        let mut sample = health_sample();
        sample
            .health
            .events
            .push(failure("historic", "An old failure"));
        let mut observer = HealthObserver::new(vec![sample.clone()]);
        assert_eq!(observer.observe(vec![sample.clone()], now), (false, vec![]));

        sample
            .health
            .events
            .push(failure("first", "Plugin connection closed unexpectedly"));
        let (changed, alerts) = observer.observe(vec![sample.clone()], now);
        assert!(changed);
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].contains("Health fixture"));
        assert!(alerts[0].contains("Plugin connection closed unexpectedly"));
        assert!(alerts[0].contains("Health and compatibility"));
        assert_eq!(observer.observe(vec![sample.clone()], now), (false, vec![]));

        sample.health.events.last_mut().unwrap().occurrences += 1;
        let (changed, alerts) =
            observer.observe(vec![sample.clone()], now + Duration::from_secs(5));
        assert!(changed);
        assert!(alerts.is_empty());
        // The host gives repeated events new IDs after its ten-second grouping window
        sample
            .health
            .events
            .push(failure("repeated", "Plugin connection closed unexpectedly"));
        assert!(observer
            .observe(vec![sample.clone()], now + Duration::from_secs(15))
            .1
            .is_empty());
        sample.health.events.last_mut().unwrap().occurrences += 1;
        assert_eq!(
            observer
                .observe(vec![sample.clone()], now + Duration::from_secs(76))
                .1
                .len(),
            1
        );

        sample.health.blocked_reason =
            Some("Three automatic restart attempts failed; choose Retry".into());
        let (_, alerts) = observer.observe(vec![sample.clone()], now + Duration::from_secs(77));
        assert_eq!(alerts.len(), 1);
        assert!(alerts[0].contains("choose Retry"));
        assert!(alerts[0].contains("connection closed unexpectedly"));
        assert_eq!(
            observer.observe(vec![sample], now + Duration::from_secs(78)),
            (false, vec![])
        );
        observer.observe(vec![], now);
        assert!(observer.previous.is_empty());
        assert!(observer.incidents.is_empty());
    }

    #[test]
    // Verify that new plugins do not replay history or duplicate resource alerts
    fn new_plugins_do_not_replay_history_or_duplicate_resource_alerts() {
        let now = Instant::now();
        let mut observer = HealthObserver::new(vec![]);
        let mut sample = health_sample();
        sample
            .health
            .events
            .push(failure("historic", "An old failure"));
        assert!(observer.observe(vec![sample.clone()], now).1.is_empty());
        let mut resource = failure("resource", "Memory threshold exceeded");
        resource.category = "resource_warning".into();
        resource.severity = "warning".into();
        sample.health.events.push(resource);
        assert!(observer.observe(vec![sample.clone()], now).1.is_empty());
        sample.enabled = false;
        sample
            .health
            .events
            .push(failure("disabled", "Connection closed"));
        assert!(observer.observe(vec![sample], now).1.is_empty());

        let mut newly_failed = health_sample();
        newly_failed.id = "new.plugin".into();
        let mut event = failure("new", "Registration timed out");
        event.timestamp_ms = observer.started_at_ms;
        newly_failed.health.events.push(event);
        assert_eq!(observer.observe(vec![newly_failed], now).1.len(), 1);
    }

    #[test]
    // Verify that monitor deadlines follow visibility without wakeups causing extra checks
    fn monitor_deadlines_follow_visibility_without_wakeups_causing_extra_checks() {
        let now = Instant::now();
        assert!(sample_remaining(None, now, false).is_zero());
        // Frequent lifecycle wakes do not bypass the background interval
        for elapsed in [0, 1, 2, 4] {
            assert!(
                !sample_remaining(Some(now), now + Duration::from_secs(elapsed), false).is_zero()
            );
        }
        assert!(sample_remaining(Some(now), now + Duration::from_secs(5), false).is_zero());
        assert!(sample_remaining(Some(now), now + Duration::from_secs(1), true).is_zero());
        // Hiding the last running card extends the deadline immediately
        assert_eq!(
            sample_remaining(Some(now), now + Duration::from_secs(1), false),
            Duration::from_secs(4)
        );
    }

    #[test]
    // Verify that background health reports a failed start without running targets
    fn background_health_reports_a_failed_start_without_running_targets() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/fixture.health.sdPlugin");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("runtime.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        std::fs::write(
            package.join("manifest.json"),
            serde_json::json!({
                "UUID": "fixture.health", "Name": "Health fixture", "Version": "0.1.0",
                "CodePath": "runtime.html", "Actions": []
            })
            .to_string(),
        )
        .unwrap();
        crate::test_plugin::modernize(&package);
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable("fixture.health", true)
            .unwrap();
        let events = runtime.subscribe();
        let revision = runtime.snapshot().revision;
        start(std::sync::Arc::downgrade(&runtime));
        runtime
            .plugins
            .lock()
            .unwrap()
            .startup_failure("fixture.health", "Registration timed out");
        assert!(runtime.plugins.lock().unwrap().monitor_targets().is_empty());
        let deadline = Instant::now() + Duration::from_secs(7);
        let mut warning = None;
        let mut health_events = 0;
        while warning.is_none() || health_events == 0 {
            let event = events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if event.kind == "plugin-health-warning" {
                warning = event.message;
            }
            if event.kind == "plugin-health" {
                health_events += 1;
            }
        }
        let message = warning.unwrap();
        assert!(message.contains("Health fixture"));
        assert!(message.contains("Registration timed out"));
        assert_eq!(runtime.snapshot().revision, revision);
        assert_eq!(health_events, 1);
        runtime.shutdown();
    }
    #[test]
    // Verify that process tree sampling warms up normalizes aggregates and rejects PID reuse
    fn process_tree_sampling_warms_up_normalizes_aggregates_and_rejects_pid_reuse() {
        let mut owner = Ownership::default();
        let process = |parent, created, memory, cpu| ProcessMeasurement {
            parent,
            created,
            memory,
            cpu,
        };
        let mut processes = HashMap::from([
            (101, process(None, 100, 10, 100.)),
            (102, process(Some(101), 101, 20, 100.)),
            (103, process(Some(102), 102, 30, 100.)),
            (104, process(None, 99, 999, 100.)),
        ]);
        assert_eq!(
            owner.measure(101, &processes, &children_of(&processes), 4, 100_000),
            Some((None, 60, 3))
        );
        assert_eq!(
            owner.measure(101, &processes, &children_of(&processes), 4, 100_000),
            Some((Some(75.), 60, 3))
        );
        processes.get_mut(&103).unwrap().parent = None;
        assert_eq!(
            owner
                .measure(101, &processes, &children_of(&processes), 4, 100_000)
                .unwrap()
                .1,
            60
        );
        processes.insert(103, process(None, 105, 999, 100.));
        assert_eq!(
            owner
                .measure(101, &processes, &children_of(&processes), 4, 100_000)
                .unwrap()
                .1,
            30
        );
        processes.get_mut(&101).unwrap().created = 106;
        assert!(owner
            .measure(101, &processes, &children_of(&processes), 4, 100_000)
            .is_none());
        assert!(Ownership::default()
            .measure(104, &processes, &children_of(&processes), 4, 200_000)
            .is_none());
    }
    #[test]
    // Verify that missing and inaccessible processes are unavailable and new runs warm up again
    fn missing_and_inaccessible_processes_are_unavailable_and_new_runs_warm_up_again() {
        let mut owner = Ownership::default();
        assert!(owner
            .measure(1, &HashMap::new(), &HashMap::new(), 1, 0)
            .is_none());
        let processes = HashMap::from([(
            1,
            ProcessMeasurement {
                parent: None,
                created: 1,
                memory: 0,
                cpu: f64::NAN,
            },
        )]);
        assert!(owner
            .measure(1, &processes, &children_of(&processes), 1, 0)
            .is_none());
        let processes = HashMap::from([(
            2,
            ProcessMeasurement {
                parent: None,
                created: 1,
                memory: 40,
                cpu: 200.,
            },
        )]);
        assert_eq!(
            Ownership::default().measure(2, &processes, &children_of(&processes), 4, 0),
            Some((None, 40, 1))
        );
    }
    #[test]
    // Verify that surviving children of a reused parent PID are not owned
    fn surviving_children_of_a_reused_parent_pid_are_not_owned() {
        let process = |parent, created, memory| ProcessMeasurement {
            parent,
            created,
            memory,
            cpu: 0.,
        };
        let processes = HashMap::from([
            (101, process(None, 200, 10)),
            // Survived the prior process with PID 101.
            (102, process(Some(101), 100, 1_000)),
            (103, process(Some(102), 201, 2_000)),
            // Creation timestamps have second precision, so equality is valid
            (104, process(Some(101), 200, 20)),
            (105, process(Some(104), 201, 30)),
        ]);
        assert_eq!(
            Ownership::default().measure(101, &processes, &children_of(&processes), 4, 200_000),
            Some((None, 60, 3))
        );
    }
}
