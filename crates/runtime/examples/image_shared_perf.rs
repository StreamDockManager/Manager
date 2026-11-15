//! Shared internal consumer variant of image_perf; same fixture and read-only workload
//! Run an otherwise idle release build; stdout is JSON, five repetitions per workload
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Duration};
use stream_dock_manager_runtime::{
    media::{MediaOptions, RenderCache},
    profiles::ProfileStore,
    test_support,
};
#[path = "support/measurement.rs"]
mod measurement;
use measurement::measure;
// Run the image shared perf example entry point
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
        .render_media_shared(&id, Duration::ZERO, 96, 96, 0, "png", &options)
        .unwrap();
    let mut cache = RenderCache::default();
    cache.insert("fixture".into(), vec![42; 1024 * 1024]);
    let results = vec![
        measure("validated_public_png_256", 100, || {
            black_box(test_support::encode_image(png.clone(), true, "png", false).unwrap());
        }),
        measure("jpeg_candidate_256", 40, || {
            black_box(test_support::encode_image(png.clone(), true, "jpeg", true).unwrap());
        }),
        measure("warm_static_shared_render_96", 100, || {
            black_box(
                store
                    .render_media_shared(&id, Duration::ZERO, 96, 96, 0, "png", &options)
                    .unwrap(),
            );
        }),
        measure("shared_cache_hit_1mib", 100, || {
            black_box(cache.get_shared("fixture").unwrap());
        }),
    ];
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"scope":"release software microbenchmark, not hardware or optical FPS","fixture_sha256":hex::encode(Sha256::digest(&png)),"output_sha256":hex::encode(Sha256::digest(rendered.as_ref().as_ref())),"results":results})).unwrap());
}
