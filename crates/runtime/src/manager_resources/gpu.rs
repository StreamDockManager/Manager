//! GPU counters are process-owned; missing driver data is never presented as 0%.
#[cfg(any(not(target_os = "windows"), test))]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use stream_dock_manager_plugin_host::MeasurementState;

#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub(super) struct Reading {
    pub value: Option<f64>,
    pub state: MeasurementState,
    pub note: &'static str,
}
impl Reading {
    // Construct an explicitly unavailable GPU reading without inventing a usage value
    fn unavailable() -> Self {
        Self {
            value: None,
            state: MeasurementState::Unavailable,
            note: "GPU usage is unavailable for these processes.",
        }
    }
    // Mark GPU usage as warming up until a second comparable observation exists
    fn warming() -> Self {
        Self {
            value: None,
            state: MeasurementState::WarmingUp,
            note: "Collecting a second GPU sample…",
        }
    }
    // Construct an available GPU reading clamped to the percentage range
    fn available(value: f64) -> Self {
        Self {
            value: Some(value.clamp(0., 100.)),
            state: MeasurementState::Available,
            note: "Process GPU engine activity; availability depends on the graphics driver.",
        }
    }
}

#[cfg(any(not(target_os = "windows"), test))]
#[derive(Clone)]
struct Counter {
    engine: String,
    nanos: u64,
    capacity: u64,
}

/// One tick's Linux fdinfo reads, shared by all process scopes
#[derive(Default)]
pub(super) struct Snapshot {
    #[cfg(target_os = "linux")]
    linux: linux::Snapshot,
}
impl Snapshot {
    // Collect the GPU values
    pub fn collect(pids: &BTreeSet<u32>) -> Self {
        #[cfg(target_os = "linux")]
        {
            Self {
                linux: linux::Snapshot::collect(pids),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pids;
            Self::default()
        }
    }
}

#[derive(Default)]
pub(super) struct Sampler {
    #[cfg(target_os = "windows")]
    windows: windows::Sampler,
    #[cfg(not(target_os = "windows"))]
    previous: BTreeMap<String, Counter>,
    at: Option<Instant>,
}
impl Sampler {
    // Sample with snapshot
    pub fn sample_with_snapshot(
        &mut self,
        pids: &BTreeSet<u32>,
        now: Instant,
        snapshot: &Snapshot,
    ) -> Reading {
        #[cfg(not(target_os = "linux"))]
        let _ = snapshot;
        if pids.is_empty() {
            self.at = None;
            #[cfg(not(target_os = "windows"))]
            self.previous.clear();
            #[cfg(target_os = "windows")]
            self.windows.reset();
            return Reading::unavailable();
        }
        #[cfg(target_os = "windows")]
        {
            if self
                .at
                .is_none_or(|at| now.saturating_duration_since(at) > Duration::from_secs(3))
            {
                self.windows.reset();
            }
            self.at = Some(now);
            self.windows.sample(pids)
        }
        #[cfg(not(target_os = "windows"))]
        {
            #[cfg(target_os = "macos")]
            let counters = macos::counters(pids);
            #[cfg(target_os = "linux")]
            let counters = snapshot.linux.counters(pids);
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let counters = {
                let _ = pids;
                BTreeMap::new()
            };
            let Some(elapsed) = self
                .at
                .map(|at| now.saturating_duration_since(at))
                .filter(|elapsed| *elapsed <= Duration::from_secs(3))
            else {
                self.previous = counters;
                self.at = Some(now);
                return if self.previous.is_empty() {
                    Reading::unavailable()
                } else {
                    Reading::warming()
                };
            };
            let result = utilization(&mut self.previous, &counters, elapsed);
            self.previous.retain(|key, _| counters.contains_key(key));
            self.at = Some(now);
            result
        }
    }
}

#[cfg(any(not(target_os = "windows"), test))]
// Compute GPU engine utilization from comparable counter deltas and retain the new baseline
fn utilization(
    previous: &mut BTreeMap<String, Counter>,
    current: &BTreeMap<String, Counter>,
    elapsed: Duration,
) -> Reading {
    if current.is_empty() || elapsed.is_zero() {
        previous.clear();
        return Reading::unavailable();
    }
    let mut engines = BTreeMap::<String, f64>::new();
    let mut warmed = true;
    for (key, counter) in current {
        if let Some(old) = previous.get(key) {
            // DRM counters may briefly decrease: retain the high-water mark
            let delta = counter.nanos.saturating_sub(old.nanos);
            *engines.entry(counter.engine.clone()).or_default() +=
                delta as f64 / elapsed.as_nanos() as f64 / counter.capacity.max(1) as f64 * 100.;
        } else {
            warmed = false;
        }
        previous
            .entry(key.clone())
            .and_modify(|old| old.nanos = old.nanos.max(counter.nanos))
            .or_insert_with(|| counter.clone());
    }
    if !warmed {
        return Reading::warming();
    }
    Reading::available(engines.values().copied().fold(0., f64::max))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    // Verify that shared snapshot preserves independent scope warmup and reset
    fn shared_snapshot_preserves_independent_scope_warmup_and_reset() {
        let manager = BTreeSet::from([10]);
        let combined = BTreeSet::from([10, 20]);
        let snapshot = Snapshot {
            linux: linux::Snapshot::collect_with(&combined, |pid| {
                BTreeMap::from([(
                    pid.to_string(),
                    Counter {
                        engine: "render".into(),
                        nanos: 100,
                        capacity: 1,
                    },
                )])
            }),
        };
        let now = Instant::now();
        let mut own = Sampler::default();
        let mut all = Sampler::default();
        assert_eq!(
            own.sample_with_snapshot(&manager, now, &snapshot).state,
            MeasurementState::WarmingUp
        );
        assert_eq!(
            all.sample_with_snapshot(&combined, now, &snapshot).state,
            MeasurementState::WarmingUp
        );
        let later = now + Duration::from_secs(1);
        assert_eq!(
            own.sample_with_snapshot(&manager, later, &snapshot).value,
            Some(0.)
        );
        assert_eq!(
            all.sample_with_snapshot(&combined, later, &snapshot).value,
            Some(0.)
        );
        assert_eq!(
            own.sample_with_snapshot(&BTreeSet::new(), later, &snapshot)
                .state,
            MeasurementState::Unavailable
        );
        assert_eq!(
            own.sample_with_snapshot(&manager, later, &snapshot).state,
            MeasurementState::WarmingUp
        );
        assert_eq!(
            all.sample_with_snapshot(&combined, later + Duration::from_secs(4), &snapshot)
                .state,
            MeasurementState::WarmingUp
        );
    }

    #[test]
    // Verify that sums owned clients per engine without double counting parallel engines
    fn sums_owned_clients_per_engine_without_double_counting_parallel_engines() {
        let c = |engine: &str, nanos| Counter {
            engine: engine.into(),
            nanos,
            capacity: 1,
        };
        let mut previous = BTreeMap::from([
            ("a".into(), c("render", 0)),
            ("b".into(), c("render", 0)),
            ("c".into(), c("copy", 0)),
        ]);
        let current = BTreeMap::from([
            ("a".into(), c("render", 200_000_000)),
            ("b".into(), c("render", 300_000_000)),
            ("c".into(), c("copy", 400_000_000)),
        ]);
        assert_eq!(
            utilization(&mut previous, &current, Duration::from_secs(1)).value,
            Some(50.)
        );
        let rollback = BTreeMap::from([("a".into(), c("render", 100_000_000))]);
        assert_eq!(
            utilization(&mut previous, &rollback, Duration::from_secs(1)).value,
            Some(0.)
        );
        assert_eq!(previous["a"].nanos, 200_000_000);
        assert_eq!(
            utilization(&mut previous, &BTreeMap::new(), Duration::from_secs(1)).value,
            None
        );
        assert_eq!(
            utilization(&mut previous, &current, Duration::from_secs(1)).state,
            MeasurementState::WarmingUp
        );
    }
}
