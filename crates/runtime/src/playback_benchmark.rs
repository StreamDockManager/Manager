//! Run with cargo test --release -p stream-dock-manager-runtime --lib
//! captured_media_playback -- --ignored --nocapture --test-threads=1 (requires FFmpeg).
use super::*;
use crate::media::{MediaOptions, PreparationControl};

thread_local! {
    static COMPLETION_TIMES:std::cell::RefCell<Option<BTreeMap<String,Vec<Instant>>>>=const {std::cell::RefCell::new(None)};
    static COMPLETIONS: std::cell::RefCell<Option<BTreeMap<String, u32>>> = const { std::cell::RefCell::new(None) };
}

// Count committed transfers even when newer pixels have already superseded the
// revision in the scheduler. Playback.last_frame tracks only the current revision.
pub(super) fn record_completion(outcome: &OutputOutcome) {
    if outcome.status == OutputStatus::Completed {
        if let Some((_, finished)) = outcome.write_interval {
            COMPLETION_TIMES.with_borrow_mut(|times| {
                if let Some(values) = times {
                    values
                        .entry(outcome.surface_id.clone())
                        .or_default()
                        .push(finished);
                }
            });
        }
        COMPLETIONS.with_borrow_mut(|counts| {
            if let Some(counts) = counts {
                *counts.entry(outcome.surface_id.clone()).or_default() += 1;
            }
        });
    }
}

#[test]
#[ignore = "Playback throughput benchmark with FFmpeg, local loopback and captured N4 Pro output"]
fn captured_media_playback() {
    measure_playback("captured", |_, _| {});
}

fn measure_playback(transport: &str, attach: impl Fn(&Arc<Runtime>, &mut Connection)) {
    for (format, source) in [
        (
            "gif",
            include_bytes!("../../../tests/fixtures/media/source-25.gif").as_slice(),
        ),
        (
            "mp4",
            include_bytes!("../../../tests/fixtures/media/source-25-vp9.mp4").as_slice(),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let clip = Arc::new(crate::media::prepare(source).unwrap());
        let (asset, lease) = runtime
            .profiles
            .publish_prepared_clip_shared(clip.clone(), source, &PreparationControl::default())
            .unwrap();
        let (mut connection, reports) =
            super::frame_policy_tests::connection_for_model(&runtime, "n4-pro");
        let device = connection.device_id.clone();
        crate::render_scene::install_burst_fixture(&runtime);
        let background = MediaOptions {
            fit: "fit".into(),
            letterbox: Some("black".into()),
            ..Default::default()
        };
        let mut playbacks = vec![("@background".to_string(), background.clone())];
        {
            let mut state = runtime.state.lock().unwrap();
            let profile = &mut state.profiles[0];
            profile.background_asset = Some(asset.clone());
            profile.background_media = background;
            let bindings = &mut profile.pages[0].bindings;
            bindings.retain(|key, _| (1..=8).any(|index| key == &format!("key-{index}")));
            for index in 1..=8 {
                let key = format!("key-{index}");
                let binding = bindings.get_mut(&key).unwrap();
                binding.title.clear();
                binding.asset = Some(asset.clone());
                // Separate crops force eight independent renders, even at the same timestamp.
                binding.media.crop = [f64::from(index - 1) / 64., 0., 0.85, 1.];
                playbacks.push((key, binding.media.clone()));
            }
        }

        // Sample every source frame once with cold output caches and no transport pacing.
        let mut hardware_ms = Vec::new();
        let profile = runtime.state.lock().unwrap().profiles[0].clone();
        for index in 0..clip.frames.len() {
            let mut scene = runtime.render_scene(&device).unwrap();
            scene.media_elapsed =
                Duration::from_micros(clip.delays_us[..index].iter().map(|n| u64::from(*n)).sum());
            let start = Instant::now();
            for (key, _) in &playbacks[1..] {
                runtime
                    .render_bound_surface_in_scene(
                        connection.manifest.control(key).unwrap(),
                        scene.page.as_ref().unwrap().bindings.get(key),
                        connection.manifest.layout.rotation,
                        &scene.scope,
                        key,
                        &scene,
                    )
                    .unwrap();
            }
            runtime
                .render_scene_wallpaper(
                    &scene,
                    Some(&profile),
                    connection.manifest.capabilities.background_size.unwrap(),
                    connection.manifest.layout.rotation,
                    &connection.manifest.capabilities.image_formats,
                )
                .unwrap();
            hardware_ms.push(start.elapsed().as_secs_f64() * 1000.);
        }
        let cold_stats = runtime.metrics.snapshot();
        for metric in cold_stats.iter().filter(|m| {
            m.name.starts_with("animation_source_") || m.name == "animation_decoded_source_bytes"
        }) {
            eprintln!(
                "cold {format} {} count={} sum={} p95={:?}",
                metric.name, metric.count, metric.sum, metric.p95
            );
        }
        runtime.profiles.render_cache.lock().unwrap().clear();

        attach(&runtime, &mut connection);
        for (key, media) in &playbacks {
            connection.start_playback(
                key.clone(),
                Playback {
                    vendor_started: None,
                    clip: None,
                    asset: asset.clone(),
                    _assets: lease.clone(),
                    control: (key != "@background").then(|| key.clone()),
                    format: "png".into(),
                    last_frame: None,
                    next_sample: Duration::ZERO,
                    pending_frame: None,
                    media: media.clone(),
                },
            );
        }
        let start = Instant::now();
        let duration = Duration::from_secs(3);
        let preview_runtime = runtime.clone();
        let preview_device = device.clone();
        let closed =
            std::env::var("STREAM_DOCK_MANAGER_PLAYBACK_PREVIEW").as_deref() == Ok("closed");
        let preview = std::thread::spawn(move || {
            let mut stream = crate::preview::SurfacePreviewStream::for_browser("benchmark".into());
            let mut milliseconds = Vec::new();
            let mut counts = [0_u32; 9];
            let mut bytes = 0;
            let mut deadline = start;
            while !closed && start.elapsed() < duration {
                let tick = Instant::now();
                if let Some((delta, frames)) = stream.render_next(&preview_runtime, &preview_device)
                {
                    for index in 1..=8 {
                        counts[index - 1] +=
                            u32::from(delta.surfaces.contains_key(&format!("key-{index}")));
                    }
                    counts[8] += u32::from(delta.background_changed);
                    bytes += frames.values().map(|frame| frame.len()).sum::<usize>();
                }
                milliseconds.push(tick.elapsed().as_secs_f64() * 1000.);
                deadline =
                    crate::media::advance_deadline(deadline, Instant::now(), stream.interval());
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
            (milliseconds, counts, bytes)
        });
        COMPLETIONS.set(Some(BTreeMap::new()));
        COMPLETION_TIMES.set(Some(BTreeMap::new()));
        // Snapshot at one-second boundaries to compare first play with repeated frames.
        let counters = || {
            let cache = runtime.profiles.render_cache.lock().unwrap();
            (cache.hits, cache.misses, cache.copied_bytes, cache.bytes)
        };
        let mut prev = counters();
        let mut prev_queue = connection.worker.queue_stats();
        let mut next_sample = 1;
        while start.elapsed() < duration {
            animate(&runtime, &device, &mut connection);
            connection.collect_outcomes(&runtime);
            if start.elapsed().as_secs() >= next_sample {
                let cache = counters();
                let queue = connection.worker.queue_stats();
                eprintln!("window {format} second={next_sample} cache_hits={} cache_misses={} copied_bytes={} retained_cache_bytes={} completed={} replaced={} host_bytes={} commits={}",
                    cache.0-prev.0, cache.1-prev.1, cache.2-prev.2, cache.3,
                    queue.total_completed-prev_queue.total_completed, queue.total_dropped-prev_queue.total_dropped,
                    queue.total_report_bytes-prev_queue.total_report_bytes, queue.total_commits-prev_queue.total_commits);
                prev = cache;
                prev_queue = queue;
                next_sample += 1;
            }
            // Release captured protocol reports throughout the benchmark.
            reports.lock().unwrap().clear();
            std::thread::sleep(Duration::from_millis(1));
        }
        connection.collect_outcomes(&runtime);
        let completed = COMPLETIONS.take().unwrap();
        for (surface, times) in COMPLETION_TIMES.take().unwrap() {
            let mut gaps: Vec<_> = times
                .windows(2)
                .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.)
                .collect();
            gaps.sort_by(f64::total_cmp);
            if let Some(worst) = gaps.last() {
                eprintln!(
                    "cadence {format} {surface} n={} p50_ms={:.2} p95_ms={:.2} worst_ms={worst:.2}",
                    gaps.len(),
                    gaps[gaps.len() / 2],
                    gaps[(gaps.len() - 1) * 95 / 100]
                );
            }
        }
        let counts: Vec<_> = playbacks
            .iter()
            .map(|(key, _)| completed.get(key).copied().unwrap_or_default())
            .collect();
        let (mut preview_ms, preview_counts, bytes) = preview.join().unwrap();
        assert!(connection.worker.wait_idle(Duration::from_secs(5)));
        connection.collect_outcomes(&runtime);
        let percentiles = |times: &mut Vec<f64>| {
            if times.is_empty() {
                return [0.; 2];
            }
            times.sort_by(f64::total_cmp);
            [times[times.len() / 2], times[(times.len() - 1) * 95 / 100]]
        };
        eprintln!("{transport} {format} cold_hardware_ms_p50_p95={:.2?} preview_tick_ms_p50_p95={:.2?} preview_key_updates={:?} preview_background_updates={} preview_bytes={} hardware_key_updates={:?} hardware_background_updates={} seconds=3", percentiles(&mut hardware_ms), percentiles(&mut preview_ms), &preview_counts[..8], preview_counts[8], bytes, &counts[1..], counts[0]);
        assert!(counts.iter().all(|n| *n > 0));
        assert!(closed || preview_counts.iter().all(|n| *n > 0));
        if let Some(age) = runtime
            .metrics
            .snapshot()
            .into_iter()
            .find(|m| m.name == "animation_presentation_to_final_write_ms")
        {
            eprintln!(
                "presentation {format} samples={} p50_ms={:?} p95_ms={:?}",
                age.count, age.p50, age.p95
            );
        }
        runtime.shutdown();
    }
}

/// Run only on an idle, sleeping N4 Pro after Manager has quit. The same sleep
/// command that Manager uses on clean exit returns the display to standby on drop.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "Physical N4 Pro playback, requires explicit authorization and an idle sleeping deck"]
fn physical_n4_media_playback() {
    with_physical_n4(|attach| measure_playback("physical", attach));
}

#[cfg(target_os = "macos")]
fn with_physical_n4(run: impl FnOnce(&dyn Fn(&Arc<Runtime>, &mut Connection))) {
    assert_eq!(
        std::env::var("STREAM_DOCK_MANAGER_PLAYBACK_DEVICE").as_deref(),
        Ok("n4-pro")
    );
    let module = hardware_registry::modules()
        .into_iter()
        .find(|m| m.manifest().model_id == "n4-pro")
        .unwrap();
    let api = hidapi::HidApi::new().unwrap();
    api.set_open_exclusive(true);
    let manifest = module.manifest();
    let mut matches = api.device_list().filter_map(|info| {
        let identity = identity(info);
        manifest
            .variants
            .iter()
            .find(|variant| variant.available && variant.identity.matches(&identity))
            .map(|variant| (info, &variant.id))
    });
    let (info, variant) = matches.next().expect("No N4 Pro found");
    assert!(
        matches.next().is_none(),
        "Multiple N4 Pro interfaces matched"
    );
    let hid = Arc::new(Mutex::new(
        api.open_path(info.path())
            .expect("N4 Pro must be available for exclusive use"),
    ));
    assert!(hid.lock().unwrap().is_open_exclusive().unwrap());
    let firmware = query_firmware(
        &hid,
        &manifest
            .capabilities
            .firmware_report
            .clone()
            .unwrap_or_default(),
    )
    .ok()
    .flatten();
    let contract = module
        .resolve_contract(variant, firmware.as_deref())
        .unwrap();
    assert!(contract.image_output_disabled_reason.is_none());
    struct Standby {
        hid: Arc<Mutex<hidapi::HidDevice>>,
        reports: Vec<Vec<u8>>,
    }
    impl Drop for Standby {
        fn drop(&mut self) {
            let hid = self.hid.lock().unwrap();
            for report in &self.reports {
                let written = hid.write(report);
                if written.ok() != Some(report.len()) {
                    eprintln!("N4 Pro standby write failed");
                    if !std::thread::panicking() {
                        panic!("Could not restore standby");
                    }
                    return;
                }
            }
            eprintln!("N4 Pro standby write completed");
        }
    }
    let _standby = Standby {
        hid: hid.clone(),
        reports: module
            .session_for(&contract)
            .unwrap()
            .encode(DeviceCommand::Sleep)
            .unwrap()
            .reports,
    };
    run(&|runtime, connection| {
        {
            let mut state = runtime.state.lock().unwrap();
            let device = state
                .devices
                .iter_mut()
                .find(|d| d.id == connection.device_id)
                .unwrap();
            device.manifest = contract.manifest.clone();
            device.variant_id = variant.clone();
            device.firmware = firmware.clone().unwrap_or_default();
        }
        connection.manifest = contract.manifest.clone();
        connection.contract_identity = contract.identity();
        assert!(connection.image_admission().unwrap()());
        connection.session = Arc::new(Mutex::new(module.session_for(&contract).unwrap()));
        connection.policy = OutputPolicy::default();
        connection.worker = OutputWorker::spawn_with_policy(
            Box::new(HidTransport {
                device: hid.clone(),
                runtime: Arc::downgrade(runtime),
            }),
            20,
            connection.policy,
        )
        .unwrap();
        initialize_output(
            connection.session.lock().unwrap().as_mut(),
            &connection.worker,
        )
        .unwrap();
    });
}

/// Isolate the native cost of turning eight already rendered, rotated PNG keys into
/// editor frames. Both variants use the same hardware pixels and cold cache entries.
#[test]
#[ignore = "Release preview orientation benchmark, temporary loopback runtime"]
fn plugin_png_preview_preparation() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let frames: Vec<_> = (0..192)
        .map(|frame| {
            let pixels = image::RgbaImage::from_fn(112, 112, |x, y| {
                image::Rgba([
                    (x + frame) as u8,
                    (y * 2) as u8,
                    (x + y + frame) as u8,
                    if x + y < 78 { 0 } else { 200 },
                ])
            });
            crate::media::shared_bytes(
                hardware_core::render::encode_pixels(
                    image::DynamicImage::ImageRgba8(pixels),
                    "png",
                )
                .unwrap(),
            )
        })
        .collect();
    for browser in [false, true] {
        runtime.profiles.render_cache.lock().unwrap().clear();
        let mut times = Vec::new();
        for group in frames.as_chunks::<8>().0 {
            let start = Instant::now();
            for (key, bytes) in group.iter().enumerate() {
                std::hint::black_box(
                    runtime
                        .preview_image_for_surface(
                            bytes.clone(),
                            180,
                            Some(("fixture", &key.to_string())),
                            browser,
                        )
                        .unwrap(),
                );
            }
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        times.sort_by(f64::total_cmp);
        eprintln!("png preview orientation browser_metadata={browser} eight_keys_p50_ms={:.3} eight_keys_p95_ms={:.3}",times[times.len()/2],times[(times.len()-1)*95/100]);
    }
    runtime.shutdown();
}

#[path = "plugin_burst_benchmark.rs"]
mod plugin_burst;
