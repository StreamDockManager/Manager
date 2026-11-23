//! Eight independent setImage calls over the plugin socket, followed by a real
//! dirty flush and browser-stream sample. Hardware output runs asynchronously.
use super::*;
use base64::Engine;
use serde_json::{json, Value};

const KEYS: usize = 8;
const BURSTS: usize = 60;

#[test]
#[ignore = "Release plugin burst benchmark with captured USB reports and local loopback"]
fn captured_plugin_raster_bursts() {
    measure("captured", &|_, _| {});
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "Physical N4 Pro benchmark, requires an idle sleeping deck"]
fn physical_plugin_raster_bursts() {
    with_physical_n4(|attach| measure("physical", attach));
}

fn measure(transport: &str, attach: &dyn Fn(&Arc<Runtime>, &mut Connection)) {
    const PLUGIN: &str = "fixture.raster";
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("plugins/fixture");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("runtime.html"),
        "<title>Raster benchmark</title>",
    )
    .unwrap();
    std::fs::write(
        package.join("manifest.json"),
        json!({
            "UUID":PLUGIN,"Name":"Raster benchmark","Version":"0.1.0","CodePath":"runtime.html",
            "Actions":[{"UUID":"fixture.raster.key","Name":"Key"}]
        })
        .to_string(),
    )
    .unwrap();
    crate::test_plugin::modernize(&package);
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let (mut connection, reports) =
        super::super::frame_policy_tests::connection_for_model(&runtime, "n4-pro");
    crate::render_scene::install_burst_fixture(&runtime);
    let device = connection.device_id.clone();
    attach(&runtime, &mut connection);
    let mut connections = HashMap::from([(device.clone(), connection)]);
    {
        let mut state = runtime.state.lock().unwrap();
        let bindings = &mut state.profiles[0].pages[0].bindings;
        bindings.retain(|id, _| (1..=KEYS).any(|n| id == &format!("key-{n}")));
        for (id, binding) in bindings {
            binding.title.clear();
            binding.action = crate::actions::Action::Plugin {
                plugin_id: PLUGIN.into(),
                action_id: "fixture.raster.key".into(),
                settings: json!({"key":id}),
                user_desired_state: None,
                resources: Default::default(),
            };
        }
    }
    runtime
        .plugins
        .lock()
        .unwrap()
        .enable(PLUGIN, true)
        .unwrap();
    crate::test_support::synchronize_plugins(&runtime);
    let url = {
        let mut host = runtime.plugins.lock().unwrap();
        host.poll();
        host.browser_view(PLUGIN).unwrap().unwrap().url
    };
    let parts: Vec<_> = url.split('/').collect();
    let (mut socket, _) = tungstenite::connect(format!("ws://{}", parts[2])).unwrap();
    if let tungstenite::stream::MaybeTlsStream::Plain(tcp) = socket.get_mut() {
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        tcp.set_nodelay(true).unwrap();
    }
    socket
        .send(tungstenite::Message::Text(
            crate::test_plugin::registration(&package, parts[4])
                .to_string()
                .into(),
        ))
        .unwrap();
    let ready = Instant::now();
    loop {
        let mut host = runtime.plugins.lock().unwrap();
        host.poll();
        if host.list().iter().any(|p| {
            p.id == PLUGIN
                && p.runtime.state == stream_dock_manager_plugin_host::RuntimeState::Running
        }) {
            break;
        }
        drop(host);
        assert!(
            ready.elapsed() < Duration::from_secs(5),
            "Plugin registration timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    crate::test_support::synchronize_plugins(&runtime);
    runtime.plugins.lock().unwrap().poll();
    let mut contexts = BTreeMap::new();
    while contexts.len() < KEYS {
        let message = socket.read().unwrap();
        if let Ok(text) = message.to_text() {
            let event: Value = serde_json::from_str(text).unwrap();
            if event["event"] == "willAppear" && event["device"] == device {
                contexts.insert(
                    event["payload"]["settings"]["key"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    event["context"].as_str().unwrap().to_owned(),
                );
            }
        }
    }
    let mut stream = crate::preview::SurfacePreviewStream::for_browser("plugin-burst".into());
    stream.render_next(&runtime, &device).unwrap();
    runtime.plugins.lock().unwrap().drain_effects();
    for format in ["png", "jpeg"] {
        // Prepare source messages outside measurement, with a distinct image per key and tick.
        let bursts: Vec<_> = (0..BURSTS)
            .map(|tick| {
                contexts
                    .values()
                    .enumerate()
                    .map(|(k, context)| {
                        let frame = (tick * KEYS + k) as u32;
                        let pixels = image::RgbImage::from_fn(112, 112, |x, y| {
                            image::Rgb([(x + frame) as u8, (2 * y) as u8, (x + y + frame) as u8])
                        });
                        let bytes = hardware_core::render::encode_pixels(
                            image::DynamicImage::ImageRgb8(pixels),
                            format,
                        )
                        .unwrap();
                        let uri = format!(
                            "data:image/{format};base64,{}",
                            base64::engine::general_purpose::STANDARD.encode(bytes)
                        );
                        json!({"event":"setImage","context":context,"payload":{"image":uri}})
                            .to_string()
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let initial_copied = runtime.profiles.render_cache.lock().unwrap().copied_bytes;
        let initial_usb_bytes = connections[&device].worker.queue_stats().total_report_bytes;
        COMPLETIONS.set(Some(BTreeMap::new()));
        let mut times = [vec![], vec![], vec![]];
        let mut preview_count = 0;
        let mut preview_bytes = 0;
        let start = Instant::now();
        for (tick, burst) in bursts.into_iter().enumerate() {
            let ingress = Instant::now();
            for message in burst {
                socket
                    .send(tungstenite::Message::Text(message.into()))
                    .unwrap();
            }
            let mut dirty = DirtyVisuals::default();
            while dirty.surfaces.len() < KEYS {
                for effect in runtime.plugins.lock().unwrap().drain_effects() {
                    if let HostEffect::Feedback(effect) = effect {
                        if let Some(target) = effect
                            .target
                            .filter(|t| t.device == device && effect.kind == FeedbackKind::Image)
                        {
                            dirty.insert(&device, &target.control, None);
                        }
                    }
                }
                assert!(
                    ingress.elapsed() < Duration::from_secs(3),
                    "missing image effects"
                );
                std::thread::yield_now();
            }
            times[0].push(ingress.elapsed().as_secs_f64() * 1000.);
            let render = Instant::now();
            while !dirty.surfaces.is_empty() {
                flush_plugin_visuals(&runtime, &mut connections, &mut dirty);
            }
            times[1].push(render.elapsed().as_secs_f64() * 1000.);
            let preview = Instant::now();
            if let Some((delta, frames)) = stream.render_next(&runtime, &device) {
                preview_count += delta.surfaces.len();
                preview_bytes += frames.values().map(|v| v.len()).sum::<usize>();
            }
            times[2].push(preview.elapsed().as_secs_f64() * 1000.);
            connections
                .get_mut(&device)
                .unwrap()
                .collect_outcomes(&runtime);
            reports.lock().unwrap().clear();
            std::thread::sleep(
                (start + Duration::from_secs_f64((tick + 1) as f64 / 30.))
                    .saturating_duration_since(Instant::now()),
            );
        }
        let c = connections.get_mut(&device).unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(10)));
        c.collect_outcomes(&runtime);
        let usb_bytes = c.worker.queue_stats().total_report_bytes - initial_usb_bytes;
        let counts = COMPLETIONS.take().unwrap();
        let per_key: Vec<_> = contexts
            .keys()
            .map(|k| counts.get(k).copied().unwrap_or_default())
            .collect();
        let ms = times.map(|mut v| {
            v.sort_by(f64::total_cmp);
            [v[v.len() / 2], v[(v.len() - 1) * 95 / 100]]
        });
        let copied = runtime.profiles.render_cache.lock().unwrap().copied_bytes - initial_copied;
        let peak_media = &runtime.profiles.media_budget.stats()["peak_combined_bytes"];
        let peak_pending = c.worker.queue_stats().peak_pending_bytes;
        eprintln!("burst {format} copied_bytes={copied} peak_combined_media_bytes={peak_media} peak_pending_report_bytes={peak_pending}");
        eprintln!("{transport} {format} ingress_ms={:.3?} hardware_prepare_ms={:.3?} preview_prepare_ms={:.3?} preview_updates={preview_count} preview_bytes={preview_bytes} usb_bytes={usb_bytes} usb_completions={per_key:?} elapsed_s={:.3}",ms[0],ms[1],ms[2],start.elapsed().as_secs_f64());
        assert!(per_key.iter().all(|n| *n > 0) && preview_count > 0);
    }
    socket.close(None).unwrap();
    runtime.shutdown();
}
