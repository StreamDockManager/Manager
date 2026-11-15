//! Process-wide counters must not also enumerate Linux thread/task entries
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::ProcessRefreshKind;

#[derive(Clone, Copy)]
pub(crate) struct Process {
    pub parent: Option<u32>,
    pub created: u64,
    pub memory: u64,
    pub cpu: f64,
}

pub(crate) struct Snapshot {
    pub processes: HashMap<u32, Process>,
    pub at: Instant,
    pub previous_at: Option<Instant>,
    pub timestamp_ms: u64,
}

impl Snapshot {
    // Copy process counters and observation timestamps into a shared snapshot
    pub(crate) fn from_system(
        system: &sysinfo::System,
        at: Instant,
        previous_at: Option<Instant>,
    ) -> Self {
        Self {
            processes: system
                .processes()
                .iter()
                .map(|(pid, p)| {
                    (
                        pid.as_u32(),
                        Process {
                            parent: p.parent().map(|p| p.as_u32()),
                            created: p.start_time(),
                            memory: p.memory(),
                            cpu: p.cpu_usage() as f64,
                        },
                    )
                })
                .collect(),
            at,
            previous_at,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

// Refresh process CPU and memory counters without duplicate Linux task entries
pub(crate) fn refresh_kind() -> ProcessRefreshKind {
    // sysinfo's `nothing()` still enables tasks on Linux. Each task exposes
    // its process's RSS, so treating tasks as children multiplies memory by
    // the thread count (and double-counts CPU already included by the process)
    ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .without_tasks()
}

/// Demand-driven process observation shared by diagnostics and plugin monitoring
/// The mutex owner keeps OS collection outside lifecycle/configuration locks
#[derive(Default)]
pub(crate) struct Sampler {
    system: Option<sysinfo::System>,
    at: Option<std::time::Instant>,
    error: Option<String>,
    snapshot: Option<Arc<Snapshot>>,
    #[cfg(test)]
    refreshes: usize,
}

impl Sampler {
    /// The existing health worker calls this when no plugins need sampling
    /// Recent diagnostics observations keep the collector alive; otherwise its
    /// process table must not remain allocated indefinitely in the tray
    pub(crate) fn retire_if_idle(&mut self, now: Instant) {
        if self.at.is_some_and(|at| {
            now.saturating_duration_since(at) >= std::time::Duration::from_secs(5)
        }) {
            *self = Self::default();
        }
    }

    // Collect a process resources sample
    pub(crate) fn sample(&mut self) -> Result<Arc<Snapshot>, String> {
        self.sample_with(std::time::Instant::now(), sysinfo::System::new)
    }

    // Reuse a recent process snapshot or refresh it with the supplied clock and initializer
    pub(crate) fn sample_with(
        &mut self,
        now: std::time::Instant,
        initialize: impl FnOnce() -> Result<sysinfo::System, sysinfo::Error>,
    ) -> Result<Arc<Snapshot>, String> {
        // Sharing does not create a polling worker: hidden plugins keep their
        // five-second cadence and a closed Diagnostics view requests nothing
        if self
            .at
            .is_none_or(|at| now.saturating_duration_since(at) >= std::time::Duration::from_secs(1))
        {
            if self.system.is_none() {
                match initialize() {
                    Ok(system) => {
                        self.system = Some(system);
                        self.error = None;
                    }
                    Err(error) => {
                        self.error = Some(error.to_string());
                    }
                }
            }
            if let Some(system) = &mut self.system {
                system.refresh_processes_specifics(
                    sysinfo::ProcessesToUpdate::All,
                    true,
                    refresh_kind(),
                );
                #[cfg(test)]
                {
                    self.refreshes += 1;
                }
                self.snapshot = Some(Arc::new(Snapshot::from_system(system, now, self.at)));
            }
            self.at = Some(now);
        }
        // Report collector initialization failure only when no reusable observation exists
        self.snapshot.clone().ok_or_else(|| {
            self.error
                .clone()
                .unwrap_or_else(|| "Process statistics unavailable".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that independent consumers reuse one observation until it expires
    fn independent_consumers_reuse_one_observation_until_it_expires() {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Sampler::default()));
        let diagnostics = shared.clone();
        let plugins = shared.clone();
        let now = std::time::Instant::now();
        assert_eq!(
            diagnostics
                .lock()
                .unwrap()
                .sample_with(now, sysinfo::System::new)
                .unwrap()
                .at,
            now
        );
        assert_eq!(
            plugins
                .lock()
                .unwrap()
                .sample_with(now + std::time::Duration::from_millis(500), || panic!(
                    "must reuse initialized collector"
                ))
                .unwrap()
                .at,
            now
        );
        assert_eq!(shared.lock().unwrap().refreshes, 1);
        let next = now + std::time::Duration::from_secs(1);
        assert_eq!(
            plugins
                .lock()
                .unwrap()
                .sample_with(next, || panic!("must reuse System"))
                .unwrap()
                .at,
            next
        );
        assert_eq!(shared.lock().unwrap().refreshes, 2);
        assert_eq!(
            diagnostics
                .lock()
                .unwrap()
                .sample_with(next, || panic!("same epoch"))
                .unwrap()
                .at,
            next
        );
        assert_eq!(shared.lock().unwrap().refreshes, 2);
    }

    #[test]
    // Verify that idle retirement preserves recent diagnostics and resets CPU baseline
    fn idle_retirement_preserves_recent_diagnostics_and_resets_cpu_baseline() {
        let mut sampler = Sampler::default();
        let now = Instant::now();
        let held = sampler.sample_with(now, sysinfo::System::new).unwrap();
        sampler.retire_if_idle(now + std::time::Duration::from_secs(4));
        assert!(sampler.snapshot.is_some());
        sampler.retire_if_idle(now + std::time::Duration::from_secs(5));
        assert!(sampler.system.is_none());
        assert!(sampler.snapshot.is_none());
        assert!(
            !held.processes.is_empty(),
            "in-flight immutable observations remain usable"
        );
        let next = sampler
            .sample_with(
                now + std::time::Duration::from_secs(6),
                sysinfo::System::new,
            )
            .unwrap();
        assert!(next.previous_at.is_none());
    }

    #[test]
    // Verify that failed shared initialization is rate limited and retried
    fn failed_shared_initialization_is_rate_limited_and_retried() {
        let mut sampler = Sampler::default();
        let now = std::time::Instant::now();
        assert!(sampler
            .sample_with(now, || Err(sysinfo::Error::Unsupported))
            .is_err());
        assert!(sampler
            .sample_with(now, || panic!("failure must be rate limited"))
            .is_err());
        assert!(sampler
            .sample_with(
                now + std::time::Duration::from_secs(1),
                sysinfo::System::new
            )
            .is_ok());
        assert_eq!(sampler.refreshes, 1);
    }

    #[test]
    // Verify that process counters explicitly disable task enumeration
    fn process_counters_explicitly_disable_task_enumeration() {
        let kind = refresh_kind();
        assert!(kind.cpu());
        assert!(kind.memory());
        assert!(!kind.tasks());
    }

    #[cfg(target_os = "linux")]
    #[test]
    // Verify that multithreaded process is not expanded into duplicate rss entries
    fn multithreaded_process_is_not_expanded_into_duplicate_rss_entries() {
        use std::sync::{Arc, Barrier};
        use sysinfo::{ProcessesToUpdate, System};
        let ready = Arc::new(Barrier::new(9));
        let release = Arc::new(Barrier::new(9));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let ready = ready.clone();
                let release = release.clone();
                scope.spawn(move || {
                    ready.wait();
                    release.wait();
                });
            }
            ready.wait();
            let pid = sysinfo::Pid::from_u32(std::process::id());
            let mut system = System::new();
            if let Ok(system) = &mut system {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[pid]),
                    true,
                    refresh_kind(),
                );
            }
            // Release workers before assertions so a failure cannot deadlock scope exit
            release.wait();
            let system = system.unwrap();
            assert!(system.process(pid).is_some());
            assert!(system.process(pid).unwrap().memory() > 0);
            assert_eq!(
                system.processes().len(),
                1,
                "threads must not be billed as child processes"
            );
        });
    }
}
