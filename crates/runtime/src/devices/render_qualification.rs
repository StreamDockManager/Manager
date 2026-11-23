//! Exclusive, transient display and input probe. No user profile is loaded or dispatched.
use super::palette_physical_support::{blank_scene, open_models, BlankOnDrop, PhysicalTransport};
use super::*;
use image::{DynamicImage, Rgb, RgbImage};

fn fixture(width: u16, height: u16, frame: u8) -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(width.into(), height.into(), |x, y| {
        let f = u32::from(frame);
        Rgb([
            (((x + f) * 3) ^ (y * 7)) as u8,
            (y * 2 + f * 3) as u8,
            (x + y + f) as u8,
        ])
    }))
}

fn submit(
    worker: &OutputWorker,
    session: &mut dyn HardwareSession,
    encoding: FrameEncoding,
    revision: u64,
    cmd: DeviceCommand,
    priority: OutputPriority,
) -> serde_json::Value {
    let surface = command_surface(&cmd).unwrap();
    let purpose = if priority == OutputPriority::Animation {
        ImagePurpose::Animated
    } else {
        ImagePurpose::Static
    };
    let start = Instant::now();
    let before = AdapterWork::current();
    let upload = session
        .encode_frame_with_purpose(vec![cmd], encoding, purpose)
        .unwrap()
        .pop()
        .unwrap();
    let elapsed = start.elapsed();
    let after = AdapterWork::current();
    let reports =
        upload.transaction.reports.len() + upload.commit.as_ref().map_or(0, |c| c.reports.len());
    worker
        .submit_frame(
            worker.generation(),
            worker.scene_revision(),
            vec![SurfaceSubmission {
                upload,
                surface_id: surface,
                content_revision: revision,
                priority,
            }],
        )
        .unwrap();
    serde_json::json!({"revision":revision,"adapter_ms":elapsed.as_secs_f64()*1000.0,"framing_ms":(after.framing-before.framing).as_secs_f64()*1000.0,"target_attempts":after.jpeg_attempts-before.jpeg_attempts,"payload_bytes":after.payload_bytes-before.payload_bytes,"reports_if_alone":reports})
}

#[test]
#[ignore = "Requires explicit exclusive physical display authorization and a closed Manager"]
fn physical_render_matrix() {
    let model = std::env::var("STREAM_DOCK_MANAGER_RENDER_DEVICE").unwrap();
    let model = ["n4-pro", "n3-vsd", "k1-pro"]
        .into_iter()
        .find(|id| *id == model)
        .expect("select a supported connected model");
    let physical = open_models(&[model], true).pop().unwrap();
    let manifest = &physical.contract.manifest;
    let modules = hardware_registry::modules();
    let module = modules
        .iter()
        .find(|m| m.manifest().model_id == model)
        .unwrap();
    let mut cleanup = blank_scene(manifest);
    if model != "k1-pro" {
        cleanup.push(DeviceCommand::Sleep);
    }
    let _clean = BlankOnDrop {
        model: model.into(),
        device: physical.device.clone(),
        session: module.session_for(&physical.contract).unwrap(),
        commands: cleanup,
    };
    let mut session = module.session_for(&physical.contract).unwrap();
    let encoding = if session.supports_frame_encoding(FrameEncoding::GroupedKeyUpdates) {
        FrameEncoding::GroupedKeyUpdates
    } else {
        FrameEncoding::IndividualUpdates
    };
    let worker = OutputWorker::spawn_with_policy(
        Box::new(PhysicalTransport {
            device: physical.device.clone(),
            writes: Default::default(),
        }),
        20,
        OutputPolicy::default(),
    )
    .unwrap();
    initialize_output(session.as_mut(), &worker).unwrap();
    let keys: Vec<_> = manifest
        .controls
        .iter()
        .filter(|c| c.kind == "button" && c.image_slot.is_some())
        .collect();
    let key = keys[0];
    let w = key.image_width.unwrap();
    let h = key.image_height.unwrap();
    let still = hardware_core::render::encode_pixels(fixture(w, h, 0), "png").unwrap();
    let mut rev = 1;
    let image = |id: &str, data: Vec<u8>| DeviceCommand::Image {
        control_id: id.into(),
        data,
    };
    // Switch complete LCD pages using the same frozen scene receipt as navigation.
    for page in 0..2 {
        let scene = worker.scene_revision() + 1;
        worker.set_scene_revision(scene);
        let commands = keys
            .iter()
            .map(|key| {
                let data = if page == 0 {
                    still.clone()
                } else {
                    hardware_core::render::tile(
                        w,
                        h,
                        [60, 105, 145],
                        &key.id,
                        manifest.layout.rotation,
                        "png",
                    )
                    .unwrap()
                };
                image(&key.id, data)
            })
            .collect();
        let started = Instant::now();
        let uploads = session.encode_frame(commands, encoding).unwrap();
        let surfaces = uploads
            .into_iter()
            .zip(&keys)
            .map(|(upload, key)| {
                rev += 1;
                SurfaceSubmission {
                    upload,
                    surface_id: key.id.clone(),
                    content_revision: rev,
                    priority: OutputPriority::Interaction,
                }
            })
            .collect();
        let receipt = worker
            .submit_scene_guarded(worker.generation(), scene, surfaces, None)
            .unwrap();
        assert_eq!(
            receipt.recv_timeout(Duration::from_secs(10)).unwrap(),
            OutputStatus::Completed
        );
        assert!(worker.wait_idle(Duration::from_secs(10)));
        let outcomes = worker.drain_outcomes();
        assert_eq!(outcomes.len(), keys.len());
        assert!(outcomes
            .iter()
            .all(|o| o.status == OutputStatus::Completed && o.scene_revision == scene));
        eprintln!(
            "PAGE {}",
            serde_json::json!({"model":model,"scene":scene,"keys":keys.len(),"host_ms":started.elapsed().as_secs_f64()*1000.,"report_bytes":outcomes.iter().map(|o|o.report_bytes).sum::<usize>(),"commits":outcomes.iter().map(|o|o.commit_count).sum::<usize>()})
        );
    }
    rev += 1;
    let background = manifest.capabilities.background_size;
    let bounds = background.unwrap_or([w, h]);
    let order = std::env::var("STREAM_DOCK_MANAGER_RENDER_COMPARE_WALLPAPER").ok();
    let phases: &[(bool, bool)] = match order.as_deref() {
        None => &[(false, false), (true, false)],
        Some("baseline-first") => &[(true, false), (true, true)],
        Some("candidate-first") => &[(true, true), (true, false)],
        _ => panic!("Unknown wallpaper comparison order"),
    };
    let timed = std::env::var("STREAM_DOCK_MANAGER_RENDER_TIMED_KEYS").as_deref() == Ok("1");
    if order.is_some() {
        assert_eq!(model, "n4-pro");
        assert_eq!(physical.contract.variant_id, "vsdinside");
    }
    let prepare = |subsampled: bool| -> Vec<Vec<u8>> {
        (0..8)
            .map(|index| {
                let pixels = fixture(bounds[0], bounds[1], index);
                let start = Instant::now();
                let bytes = if subsampled {
                    crate::encoding::encode_wallpaper_420(pixels.clone(), 75).unwrap()
                } else {
                    hardware_core::render::encode_pixels_quality(pixels.clone(), "jpeg", 75)
                        .unwrap()
                };
                let encode_ms = start.elapsed().as_secs_f64() * 1000.;
                if order.is_some() {
                    let decoded = image::load_from_memory(&bytes).unwrap().into_rgb8();
                    let source = pixels.into_rgb8();
                    assert_eq!(decoded.dimensions(), source.dimensions());
                    let mse = decoded
                        .iter()
                        .zip(source.iter())
                        .map(|(a, b)| {
                            let diff = f64::from(*a) - f64::from(*b);
                            diff * diff
                        })
                        .sum::<f64>()
                        / decoded.len() as f64;
                    eprintln!(
                        "WALLPAPER_FRAME {}",
                        serde_json::json!({
                            "subsampled":subsampled, "index":index, "encode_ms":encode_ms,
                            "bytes":bytes.len(), "rgb_mse":mse
                        })
                    );
                }
                bytes
            })
            .collect()
    };
    let reference = prepare(false);
    let candidate = order.as_ref().map(|_| prepare(true));
    let window = std::env::var("STREAM_DOCK_MANAGER_RENDER_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(2)
        .clamp(1, 60);
    let mut report = [0; 1025];
    // An operator can start both loaded windows whenever they reach the deck.
    if order.is_some() && !timed {
        eprintln!("WALLPAPER_ARMED Tap an LCD key to start both measurement windows");
        let wait = Instant::now();
        loop {
            assert!(wait.elapsed() < Duration::from_secs(180), "No start tap");
            let n = physical.device.lock().unwrap().read_timeout(&mut report, 0);
            let n = n.expect("Read N4 Pro start tap");
            if n > 0
                && session.decode(&report[..n]).unwrap().iter().any(|e| {
                    matches!(e.input, Input::Button { pressed: true } | Input::Pulse)
                        && keys.iter().any(|k| k.id == e.control_id)
                })
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    for &(streaming, subsampled) in phases {
        let frames = if subsampled {
            candidate.as_ref().unwrap()
        } else {
            &reference
        };
        let sampling = if subsampled { "420" } else { "444" };
        eprintln!("WALLPAPER_PHASE sampling={sampling} streaming={streaming} seconds={window}");
        let stats = worker.queue_stats();
        let start = Instant::now();
        let mut next = start;
        let mut next_response = start;
        let mut frame = 0;
        let mut samples = Vec::new();
        let mut inputs = HashMap::new();
        while start.elapsed() < Duration::from_secs(window) {
            if streaming && Instant::now() >= next {
                let data = frames[frame % frames.len()].clone();
                let command = if let Some([width, height]) = background {
                    DeviceCommand::Background {
                        data,
                        x: 0,
                        y: 0,
                        width,
                        height,
                    }
                } else {
                    image(&keys[1].id, data)
                };
                samples.push(submit(
                    &worker,
                    session.as_mut(),
                    encoding,
                    rev,
                    command,
                    OutputPriority::Animation,
                ));
                rev += 1;
                frame += 1;
                next = Instant::now() + Duration::from_millis(40);
            }
            let len = physical
                .device
                .lock()
                .unwrap()
                .read_timeout(&mut report, 0)
                .unwrap();
            let ingress = Instant::now();
            let mut keys_received = Vec::new();
            if len > 0 && !(order.is_some() && timed) {
                for event in session.decode(&report[..len]).unwrap() {
                    if matches!(event.input, Input::Button { pressed: true } | Input::Pulse)
                        && keys.iter().any(|k| k.id == event.control_id)
                    {
                        keys_received.push((event.control_id, "physical"));
                    }
                }
            }
            if timed && ingress >= next_response {
                keys_received.push((key.id.clone(), "timer"));
                next_response = ingress + Duration::from_millis(200);
            }
            for (id, kind) in keys_received {
                let data = hardware_core::render::tile(
                    w,
                    h,
                    if rev % 2 == 0 {
                        [220, 160, 10]
                    } else {
                        [30, 160, 220]
                    },
                    &rev.to_string(),
                    manifest.layout.rotation,
                    "png",
                )
                .unwrap();
                inputs.insert(
                    rev,
                    (
                        ingress,
                        kind,
                        id.clone(),
                        worker.generation(),
                        worker.scene_revision(),
                    ),
                );
                samples.push(submit(
                    &worker,
                    session.as_mut(),
                    encoding,
                    rev,
                    image(&id, data),
                    OutputPriority::Interaction,
                ));
                rev += 1;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(worker.wait_idle(Duration::from_secs(10)));
        let outcomes = worker.drain_outcomes();
        let end = worker.queue_stats();
        for o in &outcomes {
            if let Some((at, kind, id, epoch, scene)) = inputs.get(&o.content_revision) {
                assert_eq!(
                    (&o.surface_id, worker.generation(), o.scene_revision),
                    (id, *epoch, *scene)
                );
                eprintln!(
                    "INTERACTION {}",
                    serde_json::json!({"model":model,"sampling":sampling,"source":kind,"loaded":streaming,"surface":id,"epoch":epoch,"scene":scene,"revision":o.content_revision,"status":o.status,"receipt_to_write_start_ms":o.write_interval.map(|(a,_)|(a-*at).as_secs_f64()*1000.),"receipt_to_final_write_ms":o.write_interval.map(|(_,b)|(b-*at).as_secs_f64()*1000.)})
                );
            }
        }
        eprintln!(
            "MATRIX {}",
            serde_json::json!({"model":model,"sampling":sampling,"offered_frames":frame,"seconds":window,"variant":physical.contract.variant_id,"firmware":physical.contract.firmware,"loaded":streaming,"inputs":inputs.len(),"completed":end.total_completed-stats.total_completed,"dropped":end.total_dropped-stats.total_dropped,"bytes":end.total_report_bytes-stats.total_report_bytes,"samples":samples,"outcomes":outcomes})
        );
        assert_eq!(end.total_failed, stats.total_failed);
        if streaming {
            assert!(end.total_completed > stats.total_completed);
        }
    }
    if order.is_some() {
        // Use the actual Diagnostics benchmark generator on every LCD key.
        let page = crate::benchmark::qualification_key_numbers(manifest).unwrap();
        for cmd in page {
            rev += 1;
            submit(
                &worker,
                session.as_mut(),
                encoding,
                rev,
                cmd,
                OutputPriority::Interaction,
            );
        }
        assert!(worker.wait_idle(Duration::from_secs(10)));
        let done = worker.drain_outcomes();
        assert_eq!(done.len(), keys.len());
        assert!(done.iter().all(|o| o.status == OutputStatus::Completed));
        if !timed {
            eprintln!("ORIENTATION Showing benchmark digits 1–10 for 20 seconds");
            // The observation is complete before neutral cleanup. No dismissal input is needed.
            std::thread::sleep(Duration::from_secs(20));
        }
        return;
    }
    if model == "n4-pro" {
        if let Ok(path) = std::env::var("STREAM_DOCK_MANAGER_RENDER_PHOTO") {
            let source = std::fs::read(path).unwrap();
            let pixels = hardware_core::render::decode_raster_pixels(&source).unwrap();
            let reference = hardware_core::render::edited_decoded_pixels(
                &pixels,
                800,
                480,
                180,
                [0., 0., 1., 1.],
                false,
                Some([16, 1024]),
            )
            .unwrap()
            .into_rgb8();
            for detail in [256, 512, 1024] {
                for quality in [60, 75, 90] {
                    let start = Instant::now();
                    let transformed = hardware_core::render::edited_decoded_pixels(
                        &pixels,
                        800,
                        480,
                        180,
                        [0., 0., 1., 1.],
                        false,
                        Some([16, detail]),
                    )
                    .unwrap();
                    let data =
                        hardware_core::render::encode_pixels_quality(transformed, "jpeg", quality)
                            .unwrap();
                    let render_ms = start.elapsed().as_secs_f64() * 1000.;
                    let decoded = image::load_from_memory(&data).unwrap().into_rgb8();
                    let mse = decoded
                        .as_raw()
                        .iter()
                        .zip(reference.as_raw())
                        .map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2))
                        .sum::<f64>()
                        / reference.as_raw().len() as f64;
                    let sample = submit(
                        &worker,
                        session.as_mut(),
                        encoding,
                        rev,
                        DeviceCommand::Background {
                            data,
                            x: 0,
                            y: 0,
                            width: 800,
                            height: 480,
                        },
                        OutputPriority::Animation,
                    );
                    assert!(worker.wait_idle(Duration::from_secs(10)));
                    let o = worker.drain_outcomes().pop().unwrap();
                    assert_eq!(
                        (rev, o.status),
                        (o.content_revision, OutputStatus::Completed)
                    );
                    eprintln!(
                        "PHOTO {}",
                        serde_json::json!({"detail":detail,"quality":quality,"render_ms":render_ms,"rgb_mse":mse,"sample":sample,"outcome":o})
                    );
                    rev += 1;
                }
            }
        }
        // Re-establish a known full base before the single subrectangle. The following
        // independently encoded full reference displays the exact same logical pixels.
        // Previous key images overlap the wallpaper coordinates on this layout.
        // Clear the temporary scene before comparing the two wallpaper updates.
        let clear = worker
            .submit_tracked(
                worker.generation(),
                session.encode(DeviceCommand::Clear).unwrap(),
            )
            .unwrap();
        assert_eq!(
            clear.recv_timeout(Duration::from_secs(10)).unwrap(),
            OutputStatus::Completed
        );
        let hold = std::env::var("STREAM_DOCK_MANAGER_RECTANGLE_HOLD_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(2)
            .clamp(1, 30);
        let base = fixture(800, 480, 0).into_rgb8();
        let mut changed = base.clone();
        image::imageops::replace(
            &mut changed,
            &RgbImage::from_pixel(64, 48, Rgb([246, 215, 25])),
            96,
            96,
        );
        let mut completed_base: Option<&RgbImage> = None;
        for phase in ["base", "rectangle", "full-reference"] {
            let desired = if phase == "base" { &base } else { &changed };
            let (x, y, width, height) = if phase == "rectangle" {
                let previous = completed_base.expect("rectangle requires a completed full base");
                let mut min = [desired.width(), desired.height()];
                let mut end = [0, 0];
                for (x, y, pixel) in desired.enumerate_pixels() {
                    if pixel != previous.get_pixel(x, y) {
                        min = [min[0].min(x), min[1].min(y)];
                        end = [end[0].max(x + 1), end[1].max(y + 1)];
                    }
                }
                assert!(end[0] > min[0] && end[1] > min[1]);
                (min[0], min[1], end[0] - min[0], end[1] - min[1])
            } else {
                (0, 0, desired.width(), desired.height())
            };
            let pixels = image::imageops::crop_imm(desired, x, y, width, height).to_image();
            let data = hardware_core::render::encode_pixels(
                DynamicImage::ImageRgb8(pixels).rotate180(),
                "png",
            )
            .unwrap();
            let sample = submit(
                &worker,
                session.as_mut(),
                encoding,
                rev,
                DeviceCommand::Background {
                    data,
                    x: x as u16,
                    y: y as u16,
                    width: width as u16,
                    height: height as u16,
                },
                OutputPriority::Interaction,
            );
            assert!(worker.wait_idle(Duration::from_secs(10)));
            let o = worker.drain_outcomes().pop().unwrap();
            assert_eq!(
                (rev, o.status),
                (o.content_revision, OutputStatus::Completed)
            );
            completed_base = Some(desired);
            eprintln!(
                "BGPIC {}",
                serde_json::json!({"phase":phase,"sample":sample,"outcome":o})
            );
            rev += 1;
            std::thread::sleep(Duration::from_secs(hold));
        }
    }
}
