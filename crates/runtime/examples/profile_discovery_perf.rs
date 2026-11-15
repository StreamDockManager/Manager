//! Release-mode, disposable profile-discovery/host-lock benchmark. No HID or enabled plugins
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    hint::black_box,
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

// Run the profile discovery perf example entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prefix = std::env::args().nth(1).ok_or("Pass a new output prefix")?;
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("org.test.profile-perf.sdPlugin");
    std::fs::create_dir_all(source.join("profiles/assets"))?;
    std::fs::write(
        source.join("index.html"),
        "<!doctype html><title>Disabled benchmark fixture</title>",
    )?;
    std::fs::write(source.join("manifest.json"), json!({"UUID":"org.test.profile-perf","Name":"Profile benchmark","Version":"0.1.0","SDKVersion":1,"CodePath":"index.html","Actions":[{"UUID":"org.test.profile-perf.run","Name":"Run","Controllers":["Keypad"]}]}).to_string())?;
    let mut asset_bytes = 0;
    let mut assets = vec![];
    let mut seed = 0x12345678u32;
    for index in 0..4 {
        let id = format!("{index:08x}-0000-4000-8000-000000000000");
        let image = image::RgbaImage::from_fn(1024, 1024, |_, _| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let [r, g, b, _] = seed.to_le_bytes();
            image::Rgba([r, g, b, 255])
        });
        let mut bytes = vec![];
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)?;
        asset_bytes += bytes.len();
        std::fs::write(source.join(format!("profiles/assets/{id}.png")), bytes)?;
        assets.push(id);
    }
    let manifests = hardware_registry::manifests();
    for (index, model) in manifests.iter().enumerate() {
        let control = model
            .controls
            .iter()
            .find(|control| control.image_width.is_some())
            .ok_or("Missing image control")?;
        let document = json!({"schema_version":1,"active_profile":"profile","profiles":[{"id":"profile","name":"Benchmark","model_id":model.model_id,"active_page":"page","pages":[{"id":"page","name":"Page","bindings":{(control.id.clone()):{"title":"Fixture","color":"#00000000","asset":assets[index%assets.len()],"action":{"kind":"plugin","plugin_id":"org.test.profile-perf","action_id":"org.test.profile-perf.run","settings":{}}}}}]}]});
        std::fs::write(
            source.join(format!("profiles/{}.json", model.model_id)),
            document.to_string(),
        )?;
    }
    let runtime =
        stream_dock_manager_runtime::test_support::headless(directory.path().join("manager"))?;
    let review = runtime.prepare_plugin_package(&source)?;
    runtime.commit_plugin_package(&review.id, false, false)?;
    let id = "org.test.profile-perf";
    let catalog = runtime.list_plugin_profiles(id)?;
    assert_eq!(catalog.suggestions.len(), manifests.len());
    assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
    assert!(catalog.suggestions.iter().all(|item| item.error.is_none()));
    for _ in 0..2 {
        black_box(runtime.list_plugin_profiles(id)?);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let waits = Arc::new(Mutex::new(Vec::<f64>::new()));
    let observer = {
        let runtime = runtime.clone();
        let stop = stop.clone();
        let waits = waits.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let start = Instant::now();
                {
                    drop(black_box(runtime.plugins.lock().unwrap()));
                }
                waits
                    .lock()
                    .unwrap()
                    .push(start.elapsed().as_secs_f64() * 1000.0);
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let mut latency = vec![];
    let mut contention = vec![];
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..3 {
            black_box(runtime.list_plugin_profiles(id)?);
        }
        latency
            .push(json!({"status":"completed","value":start.elapsed().as_secs_f64()*1000.0/3.0}));
        std::thread::sleep(Duration::from_millis(10));
        let observations = std::mem::take(&mut *waits.lock().unwrap());
        contention.push(
            json!({"status":"completed","value":observations.into_iter().fold(0.0, f64::max)}),
        );
    }
    stop.store(true, Ordering::Release);
    observer.join().unwrap();
    runtime.shutdown();
    let manager = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let git = |args: &[&str]| {
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .arg("-C")
                .arg(&manager)
                .args(args)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_owned()
    };
    for (metric, runs) in [
        ("profile_discovery", latency),
        ("plugin_host_lock", contention),
    ] {
        let report = json!({"schema_version":1,"scope":"software","source_revision":git(&["rev-parse","HEAD"]),"source_dirty":!git(&["status","--porcelain"]).is_empty(),"source_patch_sha256":hex::encode(Sha256::digest(git(&["diff","--binary"]).as_bytes())),"harness_sha256":hex::encode(Sha256::digest(include_bytes!("profile_discovery_perf.rs"))),"environment":format!("{}-{}-{}",std::env::consts::OS,std::env::consts::ARCH,if cfg!(debug_assertions){"debug"}else{"release"}),"workload":"four seeded 1024-square PNGs, every hardware layout, disabled browser plugin, warm file cache, three discoveries per sample","unit":"ms","metric":metric,"direction":"lower","runs":runs,"asset_bytes":asset_bytes,"suggestion_fingerprint":catalog.fingerprint,"limitations":["Shared host; no hardware, device frame-rate, or other-platform qualification.","Lock metric is the largest observed wait in each sample; not an OS scheduling guarantee."]});
        let path = format!("{prefix}-{metric}.json");
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(&serde_json::to_vec_pretty(&report)?)?;
        println!("{path}");
    }
    Ok(())
}
