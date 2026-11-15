use super::*;
use crate::media::allocation_tests::measure_allocations;

#[test]
// Verify that metric capacity and summary allocation probe
fn metric_capacity_and_summary_allocation_probe() {
    let mut observations = vec![];
    for repetition in 0..5 {
        let metrics = Metrics::default();
        let now = Instant::now();
        for series in 0..8 {
            let name = format!("series_{series}");
            for sample in 0..MAX_SAMPLES {
                metrics.record_at(&name, ((sample * 7919) % MAX_SAMPLES) as f64, now);
            }
        }
        let slots = || {
            metrics
                .values
                .lock()
                .unwrap()
                .values()
                .map(|series| series.samples.capacity())
                .sum::<usize>()
        };
        let full_slots = slots();
        let (summary, allocations, allocated_bytes) =
            measure_allocations(|| metrics.snapshot_at(now, || {}));
        assert!(summary
            .iter()
            .all(|metric| metric.count == MAX_SAMPLES && metric.sum == 199_990_000.0));
        assert!(metrics
            .snapshot_at(now + WINDOW, || {})
            .iter()
            .all(|metric| metric.count == MAX_SAMPLES));
        let expired = metrics.snapshot_at(now + WINDOW + Duration::from_nanos(1), || {});
        assert_eq!(expired.len(), 8);
        assert_eq!(slots(), 0);
        assert!(
            metrics.summary_diagnostics().summary_copy_peak_bytes
                < 2 * MAX_SAMPLES as u64 * std::mem::size_of::<f64>() as u64
        );
        assert!(expired.iter().all(|metric| metric.count == 0));
        observations.push(serde_json::json!({
            "repetition": repetition,
            "series": 8,
            "samples_per_series": MAX_SAMPLES,
            "full_capacity_slots": full_slots,
            "full_capacity_bytes": full_slots * std::mem::size_of::<(Instant, f64)>(),
            "expired_capacity_slots": slots(),
            "summary_allocations": allocations,
            "summary_allocated_bytes": allocated_bytes,
            "summary_copy_peak_bytes": metrics.summary_diagnostics().summary_copy_peak_bytes,
            "sample_capacity_limit_bytes": metrics.summary_diagnostics().sample_capacity_limit_bytes,
            "captured_shared_peak_bytes": metrics.summary_diagnostics().captured_shared_peak_bytes,
            "captured_shared_after_completion_bytes": metrics.summary_diagnostics().captured_shared_capacity_bytes,
        }));
    }
    eprintln!(
        "METRIC_MEMORY_CANDIDATE={}",
        serde_json::json!(observations)
    );
}

#[test]
// Verify that captured blocks survive concurrent expiry and release after summary
fn captured_blocks_survive_concurrent_expiry_and_release_after_summary() {
    let metrics = Metrics::default();
    let now = Instant::now();
    for value in 0..MAX_SAMPLES {
        metrics.record_at("race", value as f64, now);
    }
    let retired = metrics.values.lock().unwrap()["race"]
        .samples
        .blocks
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    let later = now + WINDOW + Duration::from_nanos(1);
    let captured = metrics.snapshot_at(now, || {
        metrics.record_at("race", -0.0, later);
        assert!(retired.iter().all(|block| block.upgrade().is_some()));
        let diagnostics = metrics.summary_diagnostics();
        assert_eq!(diagnostics.retained_samples, 1);
        let old_bytes = retired
            .iter()
            .map(|block| block.upgrade().unwrap().capacity() * std::mem::size_of::<Sample>())
            .sum::<usize>();
        assert_eq!(diagnostics.captured_shared_capacity_bytes, old_bytes as u64);
        assert!(old_bytes > diagnostics.sample_capacity_bytes);
        assert!(
            diagnostics.captured_shared_capacity_bytes
                <= diagnostics.snapshot_shared_capacity_limit_bytes as u64
        );
    });
    assert_eq!(captured[0].count, MAX_SAMPLES);
    assert_eq!(captured[0].latest, Some((MAX_SAMPLES - 1) as f64));
    assert_eq!(captured[0].sum, 199_990_000.0);
    assert!(retired.iter().all(|block| block.upgrade().is_none()));
    assert_eq!(
        metrics.summary_diagnostics().captured_shared_capacity_bytes,
        0
    );
    assert!(metrics.values.lock().unwrap()["race"].summary.is_none());
    let fresh = metrics.snapshot_at(later, || {});
    assert_eq!(fresh[0].count, 1);
    assert_eq!(fresh[0].latest.unwrap().to_bits(), (-0.0f64).to_bits());
}

#[test]
// Verify that every admitted series obeys aggregate capacity and releases on expiry
fn every_admitted_series_obeys_aggregate_capacity_and_releases_on_expiry() {
    let metrics = Metrics::default();
    let now = Instant::now();
    for index in 0..MAX_SERIES {
        let name = format!("series_{index}");
        for value in 0..MAX_SAMPLES + BLOCK_SAMPLES * 2 + 17 {
            metrics.record_at(&name, value as f64, now);
        }
    }
    let full = metrics.summary_diagnostics();
    assert_eq!(full.series, MAX_SERIES);
    assert_eq!(full.active_series, MAX_SERIES);
    assert_eq!(full.retained_samples, MAX_SERIES * MAX_SAMPLES);
    assert!(full.sample_capacity_bytes <= full.sample_capacity_limit_bytes);
    let expired = metrics.snapshot_at(now + WINDOW + Duration::from_nanos(1), || {});
    assert_eq!(expired.len(), MAX_SERIES);
    assert!(expired.iter().all(|metric| metric.count == 0));
    let empty = metrics.summary_diagnostics();
    assert_eq!(empty.series, MAX_SERIES);
    assert_eq!(empty.active_series, 0);
    assert_eq!(empty.sample_capacity_bytes, 0);
}

#[test]
// Verify that block rollover and partial expiry match arrival order reference
fn block_rollover_and_partial_expiry_match_arrival_order_reference() {
    let metrics = Metrics::default();
    let mut reference = VecDeque::new();
    let start = Instant::now();
    for index in 0..25000 {
        let now = start + Duration::from_millis(index * 13);
        let value = ((index * 7919) % 20001) as f64 - 10000.0;
        metrics.record_at("reference", value, now);
        reference.push_back((now, value));
        while reference.len() > MAX_SAMPLES
            || reference
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > WINDOW)
        {
            reference.pop_front();
        }
        if index % 2500 == 0 || index == 24999 {
            let summary = metrics.snapshot_at(now, || {});
            let mut sorted = reference
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>();
            sorted.sort_by(f64::total_cmp);
            assert_eq!(summary[0].count, sorted.len());
            assert_eq!(summary[0].latest, reference.back().map(|(_, value)| *value));
            assert_eq!(
                summary[0].p50,
                Some(sorted[((sorted.len() - 1) as f64 * 0.5).round() as usize])
            );
            assert_eq!(
                summary[0].p95,
                Some(sorted[((sorted.len() - 1) as f64 * 0.95).round() as usize])
            );
            assert_eq!(
                summary[0].sum.to_bits(),
                sorted.iter().sum::<f64>().to_bits()
            );
        }
    }
}
