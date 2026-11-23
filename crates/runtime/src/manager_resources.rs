//! On-demand, bounded Manager measurements. No polling when Diagnostics is closed
mod accounting;
mod footprint;
mod gpu;
pub use accounting::{MemoryAccounting, MemoryGroup, MemoryProcess};
mod webkit;
use crate::process_resources::Snapshot;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use stream_dock_manager_plugin_host::MeasurementState;
use sysinfo::System;

const WINDOW: Duration = Duration::from_secs(300);
const INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ManagerResourceSample {
    #[ts(type = "number")]
    pub timestamp_ms: u64,
    pub cpu_percent: Option<f64>,
    pub gpu_percent: Option<f64>,
    #[ts(type = "number | null")]
    pub resident_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ManagerResourceReport {
    pub memory: MemoryAccounting,
    pub process_count: usize,
    pub process_note: String,
    pub cpu_state: MeasurementState,
    pub gpu_state: MeasurementState,
    pub gpu_note: String,
    pub samples: Vec<ManagerResourceSample>,
}

#[derive(Default)]
pub(crate) struct Sampler {
    processes: Arc<Mutex<crate::process_resources::Sampler>>,
    manager: Scope,
    combined: Scope,
    plugin_identities: HashMap<String, (u32, u64)>,
    webkit: webkit::Cache,
    last: Option<Instant>,
    report: Option<(ManagerResourceReport, ManagerResourceReport)>,
}
#[derive(Default)]
struct Scope {
    gpu: gpu::Sampler,
    previous_processes: HashSet<(u32, u64)>,
    previous_observation: Option<Instant>,
    history: VecDeque<(Instant, ManagerResourceSample)>,
}
// Child ownership follows parent IDs and creation times; plugin roots and their
// entire subtrees are excluded. Never attribute unrelated or reused PIDs
fn manager_processes(
    root: u32,
    processes: &HashMap<u32, (Option<u32>, u64)>,
    excluded: &HashSet<u32>,
) -> BTreeSet<u32> {
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for (&pid, &(parent, created)) in processes {
        if let Some(parent) = parent.filter(|parent| {
            processes
                .get(parent)
                .is_some_and(|(_, start)| created >= *start)
        }) {
            children.entry(parent).or_default().push(pid);
        }
    }
    let mut owned = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        if !processes.contains_key(&pid) || excluded.contains(&pid) || !owned.insert(pid) {
            continue;
        }
        pending.extend(children.get(&pid).into_iter().flatten());
    }
    owned
}

// Combine Manager-owned descendants with verified WebKit roots while excluding plugin processes
fn manager_scope(
    root: u32,
    webkit_roots: &BTreeSet<u32>,
    processes: &HashMap<u32, (Option<u32>, u64)>,
    excluded: &HashSet<u32>,
) -> BTreeSet<u32> {
    let mut owned = manager_processes(root, processes, excluded);
    for pid in webkit_roots {
        owned.extend(manager_processes(*pid, processes, excluded));
    }
    owned
}

impl Scope {
    // Measure the manager resources operation
    fn measure(
        &mut self,
        system: Option<&Snapshot>,
        owned: &BTreeSet<u32>,
        now: Instant,
        previous_at: Option<Instant>,
        complete: bool,
    ) -> ManagerResourceReport {
        let snapshot = gpu::Snapshot::collect(owned);
        self.measure_with_snapshot(system, owned, now, previous_at, complete, &snapshot)
    }

    // Measure with snapshot
    fn measure_with_snapshot(
        &mut self,
        system: Option<&Snapshot>,
        owned: &BTreeSet<u32>,
        now: Instant,
        previous_at: Option<Instant>,
        complete: bool,
        snapshot: &gpu::Snapshot,
    ) -> ManagerResourceReport {
        let identities: HashSet<_> = owned
            .iter()
            .filter_map(|pid| {
                system
                    .and_then(|system| system.processes.get(pid))
                    .map(|p| (*pid, p.created))
            })
            .collect();
        let warmed = complete
            && !owned.is_empty()
            && identities.is_subset(&self.previous_processes)
            && previous_at.is_some_and(|last| now.duration_since(last) <= Duration::from_secs(3))
            && system.is_some_and(|snapshot| {
                self.previous_observation
                    .is_some_and(|previous| snapshot.at > previous)
            })
            && system.is_some_and(|snapshot| {
                snapshot.previous_at.is_some_and(|before| {
                    snapshot.at.saturating_duration_since(before) <= Duration::from_secs(3)
                })
            });
        let logical = std::thread::available_parallelism().map_or(1, |n| n.get());
        let mut cpu = 0.;
        let mut memory = 0u64;
        for pid in owned {
            if let Some(process) = system.and_then(|system| system.processes.get(pid)) {
                cpu += process.cpu;
                memory = memory.saturating_add(process.memory);
            }
        }
        let gpu = self.gpu.sample_with_snapshot(owned, now, snapshot);
        let sample = ManagerResourceSample {
            timestamp_ms: system.map_or_else(
                || {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64
                },
                |snapshot| snapshot.timestamp_ms,
            ),
            cpu_percent: (warmed && cpu.is_finite())
                .then_some((cpu / logical as f64).clamp(0., 100.)),
            gpu_percent: if complete { gpu.value } else { None },
            resident_bytes: (complete && !owned.is_empty() && memory > 0).then_some(memory),
        };
        self.history.push_back((now, sample));
        while self.history.len() > 300
            || self
                .history
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            self.history.pop_front();
        }
        self.previous_processes = identities;
        self.previous_observation = system.map(|snapshot| snapshot.at);
        ManagerResourceReport {
            memory: MemoryAccounting::default(),
            process_count: owned.len(),
            process_note: String::new(),
            cpu_state: if !complete || owned.is_empty() {
                MeasurementState::Unavailable
            } else if warmed {
                MeasurementState::Available
            } else {
                MeasurementState::WarmingUp
            },
            gpu_state: if complete {
                gpu.state
            } else {
                MeasurementState::Unavailable
            },
            gpu_note: if complete {
                gpu.note
            } else {
                "A running plugin process could not be measured."
            }
            .into(),
            samples: self.history.iter().map(|(_, s)| s.clone()).collect(),
        }
    }
}
impl Sampler {
    // Initialize manager resources state from the supplied inputs
    pub(crate) fn new(processes: Arc<Mutex<crate::process_resources::Sampler>>) -> Self {
        Self {
            processes,
            ..Default::default()
        }
    }
    // Plugin run IDs prevent PID reuse from moving unrelated processes into the combined scope
    pub fn sample(
        &mut self,
        plugins: &[(String, u32, u64)],
    ) -> (ManagerResourceReport, ManagerResourceReport) {
        self.sample_with(plugins, System::new)
    }

    // Reuse a recent process snapshot or refresh it with the supplied clock and initializer
    fn sample_with(
        &mut self,
        plugins: &[(String, u32, u64)],
        initialize: impl FnOnce() -> Result<System, sysinfo::Error>,
    ) -> (ManagerResourceReport, ManagerResourceReport) {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < INTERVAL)
        {
            return self.report.as_ref().unwrap().clone();
        }
        let observation = {
            let mut shared = self.processes.lock().unwrap();
            shared.sample_with(Instant::now(), initialize)
        };
        let system = match observation {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.plugin_identities.clear();
                self.webkit.invalidate();
                let mut result = (
                    self.manager
                        .measure(None, &BTreeSet::new(), now, self.last, false),
                    self.combined
                        .measure(None, &BTreeSet::new(), now, self.last, false),
                );
                let note = format!("System statistics unavailable: {error}");
                result.0.gpu_note = note.clone();
                result.1.gpu_note = note.clone();
                result.0.process_note = note.clone();
                result.1.process_note = note;
                self.last = Some(now);
                self.report = Some(result.clone());
                return result;
            }
        };
        let identities: HashMap<_, _> = system
            .processes
            .iter()
            .map(|(pid, p)| (*pid, (p.parent, p.created)))
            .collect();
        let helpers = self.webkit.discover(std::process::id(), &system, now);
        let mut excluded = HashSet::new();
        let mut combined = manager_scope(
            std::process::id(),
            &helpers.pids,
            &identities,
            &HashSet::new(),
        );
        let mut complete = true;
        self.plugin_identities
            .retain(|run, _| plugins.iter().any(|(current, _, _)| current == run));
        for (run, pid, started_ms) in plugins {
            let Some((_, created)) = identities.get(pid) else {
                complete = false;
                continue;
            };
            let identity = (*pid, *created);
            let retained = self
                .plugin_identities
                .entry(run.clone())
                .or_insert(identity);
            if *retained != identity || created.saturating_add(1) < started_ms / 1000 {
                complete = false;
                continue;
            }
            excluded.insert(*pid);
            combined.extend(manager_processes(*pid, &identities, &HashSet::new()));
        }
        let manager = manager_scope(std::process::id(), &helpers.pids, &identities, &excluded);
        // GPU counters are collected now, independently of a reused CPU/RSS epoch
        let now = Instant::now();
        let gpu = gpu::Snapshot::collect(&combined);
        let mut result = (
            self.manager
                .measure_with_snapshot(Some(&system), &manager, now, self.last, true, &gpu),
            self.combined.measure_with_snapshot(
                Some(&system),
                &combined,
                now,
                self.last,
                complete,
                &gpu,
            ),
        );
        // One attributed native pass supplies both scopes without changing historical RSS
        let memory = accounting::collect(&system, &combined, &manager, &helpers, complete);
        result.0.memory = memory.0;
        result.1.memory = memory.1;
        result.0.process_note = helpers.note.into();
        result.1.process_note = helpers.note.into();
        self.last = Some(now);
        self.report = Some(result.clone());
        result
    }
}
impl crate::Runtime {
    /// Retire ownership proof after a known webview lifecycle change; no OS work here.
    pub fn invalidate_resource_attribution(&self) {
        self.resource_attribution_invalidated
            .store(true, std::sync::atomic::Ordering::Release);
    }

    // Sample Manager resources separately from native plugin process ownership
    pub fn resource_usage(&self) -> (ManagerResourceReport, ManagerResourceReport) {
        let plugins = self
            .plugins
            .lock()
            .unwrap()
            .monitor_targets()
            .into_iter()
            .filter(|t| !t.browser)
            .filter_map(|t| t.root_pid.map(|pid| (t.run_id, pid, t.started_at_ms)))
            .collect::<Vec<_>>();
        // OS queries run without plugin, configuration, or device locks
        let mut sampler = self.manager_resources.lock().unwrap();
        if self
            .resource_attribution_invalidated
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            sampler.webkit.invalidate();
            sampler.last = None;
            sampler.report = None;
        }
        sampler.sample(&plugins)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that initialization failure is unavailable rate limited and recovers
    fn initialization_failure_is_unavailable_rate_limited_and_recovers() {
        let mut sampler = Sampler::default();
        let (manager, combined) = sampler.sample_with(&[], || Err(sysinfo::Error::Unsupported));
        for report in [manager, combined] {
            assert_eq!(report.cpu_state, MeasurementState::Unavailable);
            assert_eq!(report.gpu_state, MeasurementState::Unavailable);
            assert_eq!(report.process_count, 0);
            assert!(report.process_note.contains("unsupported"));
            assert_eq!(report.samples.len(), 1);
            assert!(report.samples[0].resident_bytes.is_none());
            assert!(report.samples[0].cpu_percent.is_none());
        }
        sampler.sample_with(&[], || {
            panic!("failed collection must retain its sampling interval")
        });
        sampler.last = Some(Instant::now() - INTERVAL);
        std::thread::sleep(INTERVAL);
        let (manager, _) = sampler.sample_with(&[], System::new);
        assert!(sampler.processes.lock().unwrap().sample().is_ok());
        assert!(!manager.process_note.contains("statistics unavailable"));
        assert_eq!(manager.samples.len(), 2);
        assert!(manager.samples[0].resident_bytes.is_none());
    }

    #[test]
    // Verify that ownership excludes plugin trees and reused parent IDs
    fn ownership_excludes_plugin_trees_and_reused_parent_ids() {
        let processes = HashMap::from([
            (10, (None, 100)),
            (11, (Some(10), 101)),
            (12, (Some(10), 101)),
            (13, (Some(12), 102)),
            (14, (Some(10), 90)),
            (15, (None, 100)),
        ]);
        assert_eq!(
            manager_processes(10, &processes, &HashSet::from([12])),
            BTreeSet::from([10, 11])
        );
        assert!(manager_processes(99, &processes, &HashSet::new()).is_empty());
        let combined = manager_processes(10, &processes, &HashSet::new());
        assert_eq!(combined, BTreeSet::from([10, 11, 12, 13]));
        let mut union = combined.clone();
        union.extend(manager_processes(12, &processes, &HashSet::new()));
        assert_eq!(
            union, combined,
            "plugins already in Manager's tree are counted once"
        );
    }
    #[test]
    // Verify that webkit helpers and descendants join both scopes without double counting
    fn webkit_helpers_and_descendants_join_both_scopes_without_double_counting() {
        let processes = HashMap::from([
            (10, (None, 100)),
            (11, (Some(10), 101)),
            (12, (Some(10), 101)),
            (13, (Some(12), 102)),
            (20, (Some(1), 103)),
            (21, (Some(20), 104)),
            (30, (Some(1), 103)),
        ]);
        let helpers = BTreeSet::from([11, 20]);
        assert_eq!(
            manager_scope(10, &helpers, &processes, &HashSet::from([12])),
            BTreeSet::from([10, 11, 20, 21])
        );
        assert_eq!(
            manager_scope(10, &helpers, &processes, &HashSet::new()),
            BTreeSet::from([10, 11, 12, 13, 20, 21])
        );
        assert_eq!(
            manager_scope(10, &BTreeSet::new(), &processes, &HashSet::new()),
            BTreeSet::from([10, 11, 12, 13]),
            "a failed discovery must not retain previously discovered helper PIDs"
        );
    }
    #[test]
    // Verify that histories expire and remain bounded independently
    fn histories_expire_and_remain_bounded_independently() {
        let mut scope = Scope::default();
        let now = Instant::now();
        for i in 1..=400 {
            scope.history.push_front((
                now - Duration::from_secs(i),
                ManagerResourceSample {
                    timestamp_ms: i,
                    cpu_percent: Some(10.),
                    gpu_percent: None,
                    resident_bytes: Some(1024),
                },
            ));
        }
        let report = scope.measure(None, &BTreeSet::new(), now, None, true);
        assert_eq!(report.samples.len(), 300);
        assert_eq!(report.samples.first().unwrap().timestamp_ms, 299);
        assert!(report.samples.last().unwrap().cpu_percent.is_none());
        assert_eq!(report.cpu_state, MeasurementState::Unavailable);
    }
    #[cfg(unix)]
    #[test]
    // Verify that combined scope adds a real plugin process exactly once
    fn combined_scope_adds_a_real_plugin_process_exactly_once() {
        struct Child(std::process::Child);
        impl Drop for Child {
            // Release the fixture guard at the end of its scope
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let child = Child(
            std::process::Command::new("/bin/sleep")
                .arg("5")
                .spawn()
                .unwrap(),
        );
        let (manager, combined) =
            Sampler::default().sample(&[("fixture-plugin-run".into(), child.0.id(), started)]);
        assert_eq!(combined.process_count, manager.process_count + 1);
        assert!(
            combined.samples[0].resident_bytes.unwrap()
                >= manager.samples[0].resident_bytes.unwrap()
        );
    }
    #[test]
    // Verify that live sample warms up and repeated read reuses the same observation
    fn live_sample_warms_up_and_repeated_read_reuses_the_same_observation() {
        let mut sampler = Sampler::default();
        let (report, combined) = sampler.sample(&[]);
        assert!(!report.samples.is_empty());
        assert_eq!(report.cpu_state, MeasurementState::WarmingUp);
        assert_eq!(
            report.samples[0].resident_bytes,
            combined.samples[0].resident_bytes
        );
        assert!(report.samples[0].cpu_percent.is_none());
        assert!(report.samples[0]
            .resident_bytes
            .is_some_and(|bytes| bytes > 0));
        assert_eq!(sampler.sample(&[]).0.samples.len(), 1);
        let previous = sampler.manager.previous_processes.clone();
        let previous_at = sampler.last.unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let (next, combined) = sampler.sample(&[]);
        // Other parallel tests can spawn children of this process. A newly
        // observed descendant correctly needs its own CPU warm-up sample
        let warmed = sampler.manager.previous_processes.is_subset(&previous)
            && sampler.last.unwrap().duration_since(previous_at) <= Duration::from_secs(3);
        assert_eq!(
            next.cpu_state,
            if warmed {
                MeasurementState::Available
            } else {
                MeasurementState::WarmingUp
            }
        );
        let cpu = next.samples.last().unwrap().cpu_percent;
        assert_eq!(cpu.is_some(), warmed);
        if let Some(cpu) = cpu {
            assert!(cpu.is_finite() && (0.0..=100.0).contains(&cpu));
        }
        assert_eq!(
            next.samples.last().unwrap().cpu_percent,
            combined.samples.last().unwrap().cpu_percent
        );
        // A fixed process identity must warm even while unrelated test-owned
        // descendants change. Use the real refreshed OS observation above
        let mut scope = Scope::default();
        let owned = BTreeSet::from([std::process::id()]);
        let now = Instant::now();
        assert_eq!(
            scope
                .measure(
                    Some(&sampler.processes.lock().unwrap().sample().unwrap()),
                    &owned,
                    now,
                    None,
                    true
                )
                .cpu_state,
            MeasurementState::WarmingUp
        );
        // A cached epoch must not manufacture a second CPU observation
        assert_eq!(
            scope
                .measure(
                    Some(&sampler.processes.lock().unwrap().sample().unwrap()),
                    &owned,
                    now,
                    Some(now),
                    true
                )
                .cpu_state,
            MeasurementState::WarmingUp
        );
        let next_snapshot = sampler
            .processes
            .lock()
            .unwrap()
            .sample_with(now + INTERVAL, System::new)
            .unwrap();
        assert_eq!(
            scope
                .measure(
                    Some(&next_snapshot),
                    &owned,
                    now + INTERVAL,
                    Some(now),
                    true
                )
                .cpu_state,
            MeasurementState::Available
        );
    }
}
