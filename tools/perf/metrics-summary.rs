mod metrics;
use metrics::Metrics;
use std::{hint::black_box, time::Instant};
// Run this executable entry point
fn main() {
    let repetitions: usize = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(5);
    assert!((1..=100).contains(&repetitions));
    for repetition in 0..repetitions {
        let metrics = Metrics::default();
        for series in 0..8 {
            for i in 0..20_000 {
                metrics.record(&format!("series_{series}"), f64::from((i * 7919) % 20_000));
            }
        }
        let start = Instant::now();
        black_box(metrics.snapshot());
        let cold = start.elapsed().as_nanos();
        for scenario in ["unchanged", "active"] {
            let mut ns = Vec::new();
            for iteration in 0..100 {
                if scenario == "active" {
                    for series in 0..8 {
                        metrics.record(&format!("series_{series}"), f64::from(iteration));
                    }
                }
                let start = Instant::now();
                black_box(metrics.snapshot());
                ns.push(start.elapsed().as_nanos());
            }
            ns.sort();
            println!("{{\"repetition\":{},\"scenario\":\"{}\",\"series\":8,\"samples_per_series\":20000,\"calls\":100,\"cold_ns\":{},\"total_ns\":{},\"p50_ns\":{},\"p95_ns\":{}}}",repetition,scenario,cold,ns.iter().sum::<u128>(),ns[49],ns[94]);
        }
        let mut recorded = Vec::with_capacity(100_000);
        for value in 0..100_000 {
            let start = Instant::now();
            metrics.record("record_only", f64::from(value));
            recorded.push(start.elapsed().as_nanos());
        }
        recorded.sort();
        println!("{{\"repetition\":{},\"scenario\":\"record_only\",\"series\":1,\"samples_per_series\":20000,\"calls\":100000,\"cold_ns\":0,\"total_ns\":{},\"p50_ns\":{},\"p95_ns\":{}}}", repetition, recorded.iter().sum::<u128>(), recorded[49_999], recorded[94_999]);
    }
}
