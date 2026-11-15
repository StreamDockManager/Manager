//! Release-only clone/serialization microbenchmark. No USB, UI, or optical timing claim
#![allow(unexpected_cfgs)]
use std::{hint::black_box, time::Instant};
use stream_dock_manager_runtime::benchmark::{BenchmarkReport, BenchmarkView};
// Run the progress probe example entry point
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let fixture = std::fs::read(&args[1]).expect("fixture");
    let mut report: BenchmarkReport = serde_json::from_slice(&fixture).expect("report schema");
    let large = args.get(2).is_some_and(|v| v == "large");
    if large {
        let template = report.runs[0].clone();
        report.runs.clear();
        for repetition in 1..=3 {
            for mode in [
                "individual_updates",
                "frame_scheduling",
                "grouped_key_updates",
            ] {
                let mut run = template.clone();
                run.repetition = repetition;
                run.mode = mode.into();
                let surface = run.surfaces[0].clone();
                run.surfaces = (0..15)
                    .map(|i| {
                        let mut s = surface.clone();
                        s.surface_id = format!("fixture-surface-{i}");
                        s
                    })
                    .collect();
                report.runs.push(run);
            }
        }
    }
    let view = BenchmarkView {
        running: false,
        cancelling: false,
        run_id: "fixture".into(),
        progress: 1.,
        phase: "completed".into(),
        error: None,
        report: Some(report),
        live_metrics: None,
    };
    let candidate = args.get(3).is_some_and(|v| v == "candidate");
    #[cfg(not(progress_api))]
    assert!(!candidate, "candidate probe requires cargo rustc --release --example progress_probe -- --cfg progress_api");
    let operation = || -> Vec<u8> {
        #[cfg(progress_api)]
        if candidate {
            return serde_json::to_vec(
                &stream_dock_manager_runtime::benchmark::BenchmarkProgress::from_view(
                    black_box(&view),
                    5,
                    Some(5),
                ),
            )
            .unwrap();
        }
        let _ = candidate;
        serde_json::to_vec(&black_box(&view).clone()).unwrap()
    };
    for _ in 0..1000 {
        black_box(operation());
    }
    for repetition in 1..=5 {
        let mut samples = Vec::with_capacity(10000);
        let mut bytes = 0usize;
        let started = Instant::now();
        for _ in 0..10000 {
            let at = Instant::now();
            let value = operation();
            bytes += value.len();
            black_box(value);
            samples.push(at.elapsed().as_nanos() as f64);
        }
        let elapsed = started.elapsed().as_secs_f64() * 1000.;
        samples.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"scope":"benchmark_view_clone_and_json_serialization_microbenchmark_not_ipc","profile":"release","large":large,"candidate":candidate,"progress_api_compiled":cfg!(progress_api),"repetition":repetition,"calls":10000,"serialized_bytes":bytes,"elapsed_ms":elapsed,"per_call_ns":{"p50":samples[4999],"p95":samples[9499],"p99":samples[9899]},"legacy_pollers":2,"legacy_responses_per_second":3,"os":std::env::consts::OS,"arch":std::env::consts::ARCH})
        );
    }
}
