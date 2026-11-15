use serde::{Deserialize, Serialize};
#[cfg(test)]
#[path = "metrics_memory_tests.rs"]
mod memory_tests;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const WINDOW: Duration = Duration::from_secs(300);
const MAX_SAMPLES: usize = 20_000;

#[derive(Serialize, Deserialize, ts_rs::TS)]
pub struct PerformanceCache {
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub hits: u64,
    #[ts(type = "number")]
    pub misses: u64,
    #[serde(flatten)]
    pub details: BTreeMap<String, serde_json::Value>,
}

#[derive(Serialize, ts_rs::TS)]
pub struct PerformanceReport {
    pub manager: crate::manager_resources::ManagerResourceReport,
    pub combined: crate::manager_resources::ManagerResourceReport,
    pub window_seconds: u32,
    pub metrics: Vec<Metric>,
    pub cache: PerformanceCache,
    pub reference_compatibility: serde_json::Value,
    pub runtimes: Vec<serde_json::Value>,
    pub plugins: Vec<serde_json::Value>,
}
const MAX_SERIES: usize = 256;
const MAX_NAME_BYTES: usize = 128;

#[derive(Default)]
pub struct Metrics {
    values: Mutex<BTreeMap<String, Series>>,
    // Lock order: summary_build -> values. Recording takes values only. Holding this
    // gate across sorting coalesces readers without blocking sample producers
    summary_build: Mutex<()>,
    summary_rebuilds: AtomicU64,
    summary_hits: AtomicU64,
    rejected_observations: AtomicU64,
    summary_copy_peak_bytes: AtomicU64,
    captured_shared_capacity_bytes: AtomicU64,
    captured_shared_peak_bytes: AtomicU64,
}

// Sealed blocks are immutable: a reader captures handles, while producers only
// mutate their private tail. Expiry removes whole blocks without retaining a
// high-water deque allocation. At most two partial blocks add capacity slack
const BLOCK_SAMPLES: usize = 256;
type Sample = (Instant, f64);
#[derive(Default)]
struct Samples {
    blocks: VecDeque<Arc<Vec<Sample>>>,
    tail: Vec<Sample>,
    head: usize,
    len: usize,
}
impl Samples {
    // Return the number of metrics entries
    fn len(&self) -> usize {
        self.len
    }
    // Report whether metrics has no entries
    fn is_empty(&self) -> bool {
        self.len == 0
    }
    // Sum allocated sample capacity across immutable blocks and the mutable tail
    fn capacity(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| block.capacity())
            .sum::<usize>()
            + self.tail.capacity()
    }
    // Borrow the oldest retained metric sample without removing it
    fn front(&self) -> Option<&Sample> {
        self.blocks
            .front()
            .map(|block| block.as_slice())
            .unwrap_or(&self.tail)
            .get(self.head)
    }
    // Push back
    fn push_back(&mut self, sample: Sample) {
        if self.tail.len() == BLOCK_SAMPLES {
            self.blocks
                .push_back(Arc::new(std::mem::take(&mut self.tail)));
        }
        self.tail.push(sample);
        self.len += 1;
    }
    // Pop front
    fn pop_front(&mut self) {
        if self.len == 0 {
            return;
        }
        self.len -= 1;
        self.head += 1;
        if self
            .blocks
            .front()
            .is_some_and(|block| self.head == block.len())
        {
            self.blocks.pop_front();
            self.head = 0;
        }
        if self.len == 0 {
            *self = Self::default();
        } else if self.blocks.capacity() > 32 && self.blocks.len() * 4 < self.blocks.capacity() {
            self.blocks.shrink_to_fit();
        }
    }
    // Capture the current metrics observation
    fn capture(&self) -> CapturedSamples {
        CapturedSamples {
            blocks: self.blocks.iter().cloned().collect(),
            tail: self.tail.clone(),
            head: self.head,
            len: self.len,
        }
    }
}
struct CapturedSamples {
    blocks: Vec<Arc<Vec<Sample>>>,
    tail: Vec<Sample>,
    head: usize,
    len: usize,
}
impl CapturedSamples {
    // Count sample bytes held by shared immutable metric blocks
    fn shared_capacity_bytes(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| block.capacity() * std::mem::size_of::<Sample>())
            .sum()
    }
    // Count metric snapshot bytes that must be copied rather than shared
    fn copied_bytes(&self) -> usize {
        self.blocks.capacity() * std::mem::size_of::<Arc<Vec<Sample>>>()
            + self.tail.capacity() * std::mem::size_of::<Sample>()
    }
    // Copy values
    fn copy_values(&self) -> Vec<f64> {
        let mut values = Vec::with_capacity(self.len);
        let mut head = self.head;
        for block in &self.blocks {
            values.extend(block[head..].iter().map(|(_, value)| *value));
            head = 0;
        }
        values.extend(self.tail[head..].iter().map(|(_, value)| *value));
        values
    }
}

#[derive(Default)]
struct Series {
    samples: Samples,
    revision: u64,
    summary: Option<Metric>,
    // None means no retained observations. Expiry is strictly after this instant
    next_expiry: Option<Instant>,
}
impl Series {
    // Respond to a metrics state change
    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.summary = None;
        self.next_expiry = self.samples.front().map(|(at, _)| *at + WINDOW);
    }
    // Expire stale metrics entries
    fn expire(&mut self, now: Instant) {
        if !self.next_expiry.is_some_and(|deadline| now > deadline) {
            return;
        }
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > WINDOW)
        {
            self.samples.pop_front();
        }
        self.changed();
    }
}

/// Cumulative structural counters; deliberately not recursively recorded as samples
#[derive(Debug, Clone, Copy)]
pub struct SummaryDiagnostics {
    pub rebuilds: u64,
    pub cache_hits: u64,
    pub rejected_observations: u64,
    pub series: usize,
    pub active_series: usize,
    pub retained_samples: usize,
    pub sample_capacity_bytes: usize,
    pub sample_capacity_limit_bytes: usize,
    pub summary_copy_peak_bytes: u64,
    pub captured_shared_capacity_bytes: u64,
    pub captured_shared_peak_bytes: u64,
    pub snapshot_shared_capacity_limit_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct Metric {
    pub name: String,
    pub count: usize,
    pub latest: Option<f64>,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub sum: f64,
}

impl Metrics {
    // Record the new metrics observation
    pub fn record(&self, name: &str, value: f64) {
        self.record_with_clock(name, value, Instant::now);
    }

    #[cfg(test)]
    // Record at
    fn record_at(&self, name: &str, value: f64, now: Instant) {
        self.record_with_clock(name, value, || now);
    }

    // Record with clock
    fn record_with_clock(&self, name: &str, value: f64, clock: impl FnOnce() -> Instant) {
        if !value.is_finite() || name.is_empty() || name.len() > MAX_NAME_BYTES {
            self.rejected_observations.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut values = self.values.lock().unwrap();
        if values.len() >= MAX_SERIES && !values.contains_key(name) {
            // Deterministically preserve admitted series; never evict an active
            // metric because another producer supplied a new name
            self.rejected_observations.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let series = values.entry(name.into()).or_default();
        // Sample under the recording lock so concurrent arrivals remain ordered
        let now = clock();
        while series.samples.len() >= MAX_SAMPLES
            || series
                .samples
                .front()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) > WINDOW)
        {
            series.samples.pop_front();
        }
        series.samples.push_back((now, value));
        series.changed();
    }

    // Summarize metric series and their retained allocation costs under one lock
    pub fn summary_diagnostics(&self) -> SummaryDiagnostics {
        let values = self.values.lock().unwrap();
        SummaryDiagnostics {
            series: values.len(),
            active_series: values
                .values()
                .filter(|series| !series.samples.is_empty())
                .count(),
            retained_samples: values.values().map(|series| series.samples.len()).sum(),
            sample_capacity_bytes: values
                .values()
                .map(|series| series.samples.capacity() * std::mem::size_of::<Sample>())
                .sum(),
            // Preserve all admitted exact samples. The existing cardinality and
            // per-series caps imply this aggregate bound, including block slack
            sample_capacity_limit_bytes: MAX_SERIES
                * (MAX_SAMPLES + 2 * BLOCK_SAMPLES)
                * std::mem::size_of::<Sample>(),
            summary_copy_peak_bytes: self.summary_copy_peak_bytes.load(Ordering::Relaxed),
            captured_shared_capacity_bytes: self
                .captured_shared_capacity_bytes
                .load(Ordering::Relaxed),
            captured_shared_peak_bytes: self.captured_shared_peak_bytes.load(Ordering::Relaxed),
            snapshot_shared_capacity_limit_bytes: MAX_SERIES
                * (MAX_SAMPLES + 2 * BLOCK_SAMPLES)
                * std::mem::size_of::<Sample>(),
            rebuilds: self.summary_rebuilds.load(Ordering::Relaxed),
            cache_hits: self.summary_hits.load(Ordering::Relaxed),
            rejected_observations: self.rejected_observations.load(Ordering::Relaxed),
        }
    }

    // Return a snapshot of metrics state
    pub fn snapshot(&self) -> Vec<Metric> {
        let _builder = self.summary_build.lock().unwrap();
        self.snapshot_at(Instant::now(), || {})
    }

    // The builder gate is held by public snapshot. The injectable instant/hook
    // allows deterministic boundary/concurrency tests without wall-clock sleeps
    fn snapshot_at(&self, now: Instant, after_capture: impl FnOnce()) -> Vec<Metric> {
        enum Work {
            Cached(Metric),
            Rebuild {
                name: String,
                revision: u64,
                samples: CapturedSamples,
            },
        }
        struct CaptureReservation<'a>(&'a AtomicU64);
        impl Drop for CaptureReservation<'_> {
            // Release this metrics owner when it leaves scope
            fn drop(&mut self) {
                self.0.store(0, Ordering::Relaxed);
            }
        }
        let _captured = CaptureReservation(&self.captured_shared_capacity_bytes);
        let work: Vec<_> = {
            let mut values = self.values.lock().unwrap();
            let work: Vec<_> = values
                .iter_mut()
                .map(|(name, series)| {
                    series.expire(now);
                    if let Some(summary) = &series.summary {
                        self.summary_hits.fetch_add(1, Ordering::Relaxed);
                        Work::Cached(summary.clone())
                    } else {
                        Work::Rebuild {
                            name: name.clone(),
                            revision: series.revision,
                            samples: series.samples.capture(),
                        }
                    }
                })
                .collect();
            // Publish before releasing the producer lock: any subsequently
            // retired producer blocks remain covered by this reservation. It
            // conservatively counts shared aliases until the build completes
            let shared = work
                .iter()
                .map(|work| match work {
                    Work::Rebuild { samples, .. } => samples.shared_capacity_bytes(),
                    Work::Cached(_) => 0,
                })
                .sum::<usize>() as u64;
            self.captured_shared_capacity_bytes
                .store(shared, Ordering::Relaxed);
            self.captured_shared_peak_bytes
                .fetch_max(shared, Ordering::Relaxed);
            work
        };
        // All series were captured under one producer lock. Shared blocks keep
        // that instant consistent without copying every series' numeric buffer
        let captured_bytes = work
            .iter()
            .map(|work| match work {
                Work::Rebuild { samples, .. } => samples.copied_bytes(),
                Work::Cached(_) => 0,
            })
            .sum::<usize>();
        let largest_sort = work
            .iter()
            .map(|work| match work {
                Work::Rebuild { samples, .. } => samples.len * std::mem::size_of::<f64>(),
                Work::Cached(_) => 0,
            })
            .max()
            .unwrap_or(0);
        self.summary_copy_peak_bytes
            .fetch_max((captured_bytes + largest_sort) as u64, Ordering::Relaxed);
        after_capture();
        // Sorting and summing are intentionally outside the producer lock
        let computed: Vec<_> = work
            .into_iter()
            .map(|work| match work {
                Work::Cached(summary) => (summary, None),
                Work::Rebuild {
                    name,
                    revision,
                    samples,
                } => {
                    self.summary_rebuilds.fetch_add(1, Ordering::Relaxed);
                    let mut sorted = samples.copy_values();
                    let latest = sorted.last().copied();
                    // Total ordering gives equal keys identical bits, so unstable
                    // sorting preserves the exact percentile and sum semantics
                    sorted.sort_unstable_by(f64::total_cmp);
                    let percentile = |fraction: f64| {
                        if sorted.is_empty() {
                            None
                        } else {
                            Some(sorted[((sorted.len() - 1) as f64 * fraction).round() as usize])
                        }
                    };
                    (
                        Metric {
                            name,
                            count: sorted.len(),
                            latest,
                            p50: percentile(0.5),
                            p95: percentile(0.95),
                            sum: sorted.iter().sum(),
                        },
                        Some(revision),
                    )
                }
            })
            .collect();
        {
            let mut values = self.values.lock().unwrap();
            for (summary, revision) in &computed {
                if let Some(revision) = revision {
                    let series = values.get_mut(&summary.name).unwrap();
                    // A concurrent record does not invalidate this captured
                    // point-in-time result, but must invalidate reuse next time
                    if series.revision == *revision {
                        series.summary = Some(summary.clone());
                    }
                }
            }
        }
        computed.into_iter().map(|(summary, _)| summary).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that snapshot preserves latest and reports known percentiles
    fn snapshot_preserves_latest_and_reports_known_percentiles() {
        let metrics = Metrics::default();
        // Arrival order differs from sorted order, including the last observation
        for value in (5..=20).chain(1..=4) {
            metrics.record("render_ms", f64::from(value));
        }
        metrics.record("queue_depth", 2.0);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.len(), 2);
        let render = snapshot.iter().find(|m| m.name == "render_ms").unwrap();
        assert_eq!(render.count, 20);
        assert_eq!(render.latest, Some(4.0));
        assert_eq!(render.p50, Some(11.0));
        assert_eq!(render.p95, Some(19.0));
        assert_eq!(render.sum, 210.0);
        let queue = snapshot.iter().find(|m| m.name == "queue_depth").unwrap();
        assert_eq!(queue.count, 1);
        assert_eq!(queue.latest, Some(2.0));
        assert_eq!(queue.p50, Some(2.0));
        assert_eq!(queue.p95, Some(2.0));
        assert_eq!(queue.sum, 2.0);
    }

    #[test]
    // Verify that record retains only the latest twenty thousand samples
    fn record_retains_only_the_latest_twenty_thousand_samples() {
        let metrics = Metrics::default();
        for value in 0..20_005 {
            metrics.record("events", f64::from(value));
        }

        let snapshot = metrics.snapshot();
        let events = &snapshot[0];
        assert_eq!(events.count, 20_000);
        assert_eq!(events.latest, Some(20_004.0));
        assert_eq!(events.p50, Some(10_005.0));
        assert_eq!(events.p95, Some(19_004.0));
        assert_eq!(events.sum, 200_090_000.0);
    }

    #[test]
    // Verify that recording and idle snapshots expire five minute old samples
    fn recording_and_idle_snapshots_expire_five_minute_old_samples() {
        let metrics = Metrics::default();
        let now = Instant::now();
        let expired = now - Duration::from_secs(301);
        let recent = now - Duration::from_secs(1);
        metrics.record_at("idle", 100.0, expired);
        metrics.record_at("recorded", 100.0, expired);
        metrics.record_at("recorded", 4.0, recent);
        metrics.record_at("snapshot", 100.0, expired);
        metrics.record_at("snapshot", 8.0, recent);
        metrics.record("recorded", 6.0);
        assert_eq!(metrics.values.lock().unwrap()["recorded"].samples.len(), 2);

        let snapshot = metrics.snapshot();
        let idle = snapshot.iter().find(|m| m.name == "idle").unwrap();
        assert_eq!(idle.count, 0);
        assert_eq!(idle.latest, None);
        assert_eq!(idle.p50, None);
        assert_eq!(idle.p95, None);
        assert_eq!(idle.sum, 0.0);
        let recorded = snapshot.iter().find(|m| m.name == "recorded").unwrap();
        assert_eq!(recorded.count, 2);
        assert_eq!(recorded.latest, Some(6.0));
        assert_eq!(recorded.sum, 10.0);
        let retained = snapshot.iter().find(|m| m.name == "snapshot").unwrap();
        assert_eq!(retained.count, 1);
        assert_eq!(retained.latest, Some(8.0));
        assert_eq!(retained.sum, 8.0);
        assert!(metrics.values.lock().unwrap()["idle"].samples.is_empty());
    }
    #[test]
    // Verify that unchanged snapshots reuse and record invalidates
    fn unchanged_snapshots_reuse_and_record_invalidates() {
        let metrics = Metrics::default();
        metrics.record("render", 3.0);
        assert_eq!(metrics.snapshot()[0].sum, 3.0);
        for _ in 0..10 {
            assert_eq!(metrics.snapshot()[0].latest, Some(3.0));
        }
        assert_eq!(metrics.summary_diagnostics().rebuilds, 1);
        assert_eq!(metrics.summary_diagnostics().cache_hits, 10);
        metrics.record("render", 1.0);
        let result = metrics.snapshot();
        assert_eq!(result[0].latest, Some(1.0));
        assert_eq!(result[0].sum, 4.0);
        assert_eq!(metrics.summary_diagnostics().rebuilds, 2);
    }

    #[test]
    // Verify that cached summary expires on idle strict boundary and reuses empty
    fn cached_summary_expires_on_idle_strict_boundary_and_reuses_empty() {
        let metrics = Metrics::default();
        let start = Instant::now();
        metrics.record_at("idle", 7.0, start);
        assert_eq!(metrics.snapshot_at(start, || {})[0].count, 1);
        assert_eq!(metrics.snapshot_at(start + WINDOW, || {})[0].count, 1);
        assert_eq!(metrics.summary_diagnostics().rebuilds, 1);
        let expired = metrics.snapshot_at(start + WINDOW + Duration::from_nanos(1), || {});
        assert_eq!(expired[0].count, 0);
        assert_eq!(expired[0].latest, None);
        assert_eq!(expired[0].sum, 0.0);
        assert_eq!(metrics.summary_diagnostics().rebuilds, 2);
        assert_eq!(metrics.snapshot_at(start + WINDOW * 2, || {})[0].count, 0);
        assert_eq!(metrics.summary_diagnostics().rebuilds, 2);
    }

    #[test]
    // Verify that concurrent record cannot publish stale summary
    fn concurrent_record_cannot_publish_stale_summary() {
        let metrics = Metrics::default();
        let now = Instant::now();
        metrics.record_at("race", 1.0, now);
        let captured = metrics.snapshot_at(now, || metrics.record_at("race", 2.0, now));
        assert_eq!(captured[0].count, 1);
        assert!(metrics.values.lock().unwrap()["race"].summary.is_none());
        let fresh = metrics.snapshot_at(now, || {});
        assert_eq!(fresh[0].count, 2);
        assert_eq!(fresh[0].latest, Some(2.0));
    }

    #[test]
    // Verify that summary gate does not block recording
    fn summary_gate_does_not_block_recording() {
        use std::sync::{mpsc, Arc};
        let metrics = Arc::new(Metrics::default());
        let gate = metrics.summary_build.lock().unwrap();
        let (tx, rx) = mpsc::channel();
        let producer = Arc::clone(&metrics);
        let thread = std::thread::spawn(move || {
            producer.record("producer", 1.0);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("record waited for summary sorting gate");
        drop(gate);
        thread.join().unwrap();
        assert_eq!(metrics.snapshot()[0].count, 1);
    }

    #[test]
    // Verify that simultaneous snapshots share one rebuild
    fn simultaneous_snapshots_share_one_rebuild() {
        use std::sync::{Arc, Barrier};
        let metrics = Arc::new(Metrics::default());
        for value in 0..MAX_SAMPLES {
            metrics.record("load", value as f64);
        }
        let barrier = Arc::new(Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let metrics = Arc::clone(&metrics);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    assert_eq!(metrics.snapshot()[0].count, MAX_SAMPLES);
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(metrics.summary_diagnostics().rebuilds, 1);
        assert_eq!(metrics.summary_diagnostics().cache_hits, 7);
    }

    #[test]
    // Verify that cardinality and invalid observations are bounded and counted
    fn cardinality_and_invalid_observations_are_bounded_and_counted() {
        let metrics = Metrics::default();
        for index in 0..MAX_SERIES {
            metrics.record(&format!("series_{index}"), 1.0);
        }
        metrics.record("overflow", 2.0);
        metrics.record("series_0", f64::NAN);
        metrics.record("series_0", f64::INFINITY);
        metrics.record("series_0", f64::NEG_INFINITY);
        metrics.record("", 1.0);
        metrics.record(&"x".repeat(MAX_NAME_BYTES + 1), 1.0);
        metrics.record("series_0", 3.0);
        let result = metrics.snapshot();
        assert_eq!(result.len(), MAX_SERIES);
        assert_eq!(result[0].name, "series_0");
        assert_eq!(result[0].count, 2);
        assert_eq!(result[0].latest, Some(3.0));
        assert_eq!(metrics.summary_diagnostics().rejected_observations, 6);
    }
    #[test]
    // Verify that cached results match exact reference through expiry and mutation
    fn cached_results_match_exact_reference_through_expiry_and_mutation() {
        let metrics = Metrics::default();
        let mut reference = VecDeque::new();
        let start = Instant::now();
        let mut seed = 17_u64;
        for tick in 0..1000 {
            let now = start + Duration::from_millis(tick * 701);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            if tick % 3 != 0 {
                let value = (seed % 20001) as f64 / 17.0 - 500.0;
                metrics.record_at("reference", value, now);
                reference.push_back((now, value));
            }
            while reference
                .front()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) > WINDOW)
            {
                reference.pop_front();
            }
            let snapshot = metrics.snapshot_at(now, || {});
            if snapshot.is_empty() {
                assert!(reference.is_empty());
                continue;
            }
            let latest = reference.back().map(|(_, value)| *value);
            let mut sorted: Vec<_> = reference.iter().map(|(_, value)| *value).collect();
            sorted.sort_by(f64::total_cmp);
            let rank = |fraction: f64| {
                sorted
                    .get(((sorted.len().saturating_sub(1)) as f64 * fraction).round() as usize)
                    .copied()
            };
            assert_eq!(snapshot[0].count, sorted.len());
            assert_eq!(snapshot[0].latest, latest);
            assert_eq!(snapshot[0].p50, rank(0.5));
            assert_eq!(snapshot[0].p95, rank(0.95));
            assert_eq!(
                snapshot[0].sum.to_bits(),
                sorted.iter().sum::<f64>().to_bits()
            );
        }
    }
    #[test]
    // Verify that finite extreme values preserve reference sum overflow policy
    fn finite_extreme_values_preserve_reference_sum_overflow_policy() {
        let metrics = Metrics::default();
        metrics.record("extreme", f64::MAX);
        metrics.record("extreme", f64::MAX);
        let summary = metrics.snapshot();
        assert_eq!(summary[0].count, 2);
        assert_eq!(summary[0].p50, Some(f64::MAX));
        assert_eq!(summary[0].sum, [f64::MAX, f64::MAX].iter().sum::<f64>());
        assert!(summary[0].sum.is_infinite());
        assert_eq!(metrics.summary_diagnostics().rejected_observations, 0);
        assert_eq!(metrics.snapshot()[0].sum, summary[0].sum);
    }
}
