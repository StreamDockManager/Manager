use std::time::Instant;
// Measure the measurement operation
pub fn measure(name: &str, iterations: usize, mut operation: impl FnMut()) -> serde_json::Value {
    for _ in 0..5 {
        operation();
    }
    let mut repetitions = vec![];
    for _ in 0..5 {
        let mut times = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let start = Instant::now();
            operation();
            times.push(start.elapsed().as_secs_f64() * 1e6);
        }
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        times.sort_by(f64::total_cmp);
        repetitions.push(serde_json::json!({"mean_us":mean,"p50_us":times[times.len()/2],"p95_us":times[(times.len()*95/100).min(times.len()-1)]}));
    }
    serde_json::json!({"workload":name,"iterations_per_repetition":iterations,"repetitions":repetitions})
}
