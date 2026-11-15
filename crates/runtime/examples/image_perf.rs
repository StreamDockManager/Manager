//! Reproducible CPU-only image/cache microbenchmark. No HID, plugins, or optical claims
//! Run an otherwise idle release build; stdout is JSON, five repetitions per workload
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    time::{Duration, Instant},
};
use stream_dock_manager_runtime::{
    media::{MediaOptions, RenderCache},
    profiles::ProfileStore,
    test_support,
};
#[path = "support/measurement.rs"]
mod measurement;
use measurement::measure;
// Measure cold render
fn measure_cold_render(source: &std::path::Path, expected_pixels: &[u8]) -> serde_json::Value {
    let mut repetitions = Vec::with_capacity(5);
    let options = MediaOptions::default();
    for _ in 0..5 {
        let mut times = Vec::with_capacity(10);
        for _ in 0..10 {
            // Setup/import and cleanup are outside timing; each sample has a new store/cache
            let root = tempfile::tempdir().unwrap();
            let store = ProfileStore::open(root.path().join("profiles")).unwrap();
            let id = store.import_asset(source).unwrap();
            let start = Instant::now();
            let rendered = store
                .render_media(&id, Duration::ZERO, 96, 96, 0, "png", &options)
                .unwrap();
            let elapsed = start.elapsed().as_secs_f64() * 1e6;
            times.push(elapsed);
            // Pixel correctness and hashing are deliberately outside the timed interval
            let decoded = image::load_from_memory(&rendered).unwrap().to_rgba8();
            assert_eq!(decoded.dimensions(), (96, 96));
            assert_eq!(decoded.as_raw().as_slice(), expected_pixels);
            black_box(rendered);
        }
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        times.sort_by(f64::total_cmp);
        repetitions.push(serde_json::json!({"mean_us":mean,"p50_us":times[times.len()/2],"p95_us":times[(times.len()*95/100).min(times.len()-1)]}));
    }
    serde_json::json!({
        "workload":"cold_cache_static_render_96", "iterations_per_repetition":10,
        "setup_excluded":true, "cache_state":"fresh ProfileStore and import per sample; OS file cache not flushed",
        "output_format":"png", "decoded_output_sha256":hex::encode(Sha256::digest(expected_pixels)),
        "repetitions":repetitions
    })
}
// Run the image perf example entry point
fn main() {
    let root = tempfile::tempdir().unwrap();
    let pixels = image::RgbaImage::from_fn(256, 256, |x, y| {
        image::Rgba([
            ((x * 19 + y * 7) % 256) as u8,
            ((x * 5 + y * 31) % 256) as u8,
            ((x * 13 + y * 11) % 256) as u8,
            255,
        ])
    });
    let mut output = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    let png = output.into_inner();
    let source = root.path().join("source.png");
    std::fs::write(&source, &png).unwrap();
    let store = ProfileStore::open(root.path().join("profiles")).unwrap();
    let id = store.import_asset(&source).unwrap();
    let options = MediaOptions::default();
    let rendered = store
        .render_media(&id, Duration::ZERO, 96, 96, 0, "png", &options)
        .unwrap();
    let decoded_output = image::load_from_memory(&rendered).unwrap().to_rgba8();
    let mut cache = RenderCache::default();
    cache.insert("fixture".into(), vec![42; 1024 * 1024]);
    let results = vec![
        measure("validated_public_png_256", 100, || {
            black_box(test_support::encode_image(png.clone(), true, "png", false).unwrap());
        }),
        measure("jpeg_candidate_256", 40, || {
            black_box(test_support::encode_image(png.clone(), true, "jpeg", true).unwrap());
        }),
        measure("warm_static_render_96", 100, || {
            black_box(
                store
                    .render_media(&id, Duration::ZERO, 96, 96, 0, "png", &options)
                    .unwrap(),
            );
        }),
        measure("shared_cache_hit_1mib", 100, || {
            black_box(cache.get_shared("fixture").unwrap());
        }),
        measure_cold_render(&source, decoded_output.as_raw()),
    ];
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"scope":"release software microbenchmark, not hardware or optical FPS","output_format":"png","fixture_sha256":hex::encode(Sha256::digest(&png)),"output_sha256":hex::encode(Sha256::digest(&rendered)),"results":results})).unwrap());
}
