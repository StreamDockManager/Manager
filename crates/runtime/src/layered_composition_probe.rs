//! Isolated release-only profiling gate for PATCH M6. No device or plugin processes
use crate::{render_scene::install_burst_fixture, Runtime};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};

#[cfg(debug_assertions)]
// Require release mode
fn require_release_mode() {
    panic!("Run this probe with --release");
}

#[cfg(not(debug_assertions))]
// Require release mode
fn require_release_mode() {}

// Run five measurement repetitions and summarize their sorted timing samples
fn measurements(name: &str, count: usize, mut run: impl FnMut(usize) -> f64) -> serde_json::Value {
    let mut repetitions = vec![];
    for repetition in 0..5 {
        let mut times: Vec<_> = (0..count)
            .map(|index| run(repetition * count + index))
            .collect();
        times.sort_by(f64::total_cmp);
        repetitions.push(serde_json::json!({
            "status":"completed", "mean_us":times.iter().sum::<f64>() / count as f64,
            "p50_us":times[count/2], "p95_us":times[(count*95/100).min(count-1)],
        }));
    }
    serde_json::json!({"name":name,"iterations_per_repetition":count,"repetitions":repetitions})
}

#[test]
#[ignore = "release-only five-run layered composition profiling gate; no HID or plugin process"]
// Verify that layered composition baseline
fn layered_composition_baseline() {
    require_release_mode();
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    install_burst_fixture(&runtime);
    let source_pixels = image::RgbaImage::from_fn(256, 256, |x, y| {
        image::Rgba([
            ((x * 19 + y * 7) % 256) as u8,
            ((x * 5 + y * 31) % 256) as u8,
            ((x * 13 + y * 11) % 256) as u8,
            (64 + (x + y) % 192) as u8,
        ])
    });
    let png =
        hardware_core::render::encode_pixels(image::DynamicImage::ImageRgba8(source_pixels), "png")
            .unwrap();
    let source = root.path().join("fixture.png");
    std::fs::write(&source, &png).unwrap();
    let asset = runtime.profiles.import_asset(&source).unwrap();
    let visual = serde_json::json!({"image":format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(&png)),"title":"Plugin"});
    let manifest = hardware_registry::manifests()
        .into_iter()
        .find(|m| m.model_id == "n4-pro")
        .unwrap();
    let mut reports = vec![];
    for (width, height) in [(96, 96), (176, 112)] {
        let mut control = manifest.control("key-1").unwrap().clone();
        control.image_width = Some(width);
        control.image_height = Some(height);
        let mut context = runtime.render_scene("fixture").unwrap();
        context
            .visuals
            .insert("key-1".into(), std::sync::Arc::new(visual.clone()));
        let mut binding = context.page.as_ref().unwrap().bindings["key-1"].clone();
        binding.asset = Some(asset.clone());
        binding.color = "#21436580".into();
        binding.appearance = Some(Box::new(hardware_core::appearance::Appearance {
            icon: "volume-2".into(),
            ..Default::default()
        }));
        binding.media.encoding = "png".into();
        let render = |binding: &crate::Binding| {
            runtime
                .render_bound_surface_in_scene(
                    &control,
                    Some(binding),
                    90,
                    &context.scope,
                    "key-1",
                    &context,
                )
                .unwrap()
        };
        let expected = render(&binding);
        let expected_pixels = image::load_from_memory(&expected).unwrap().to_rgba8();
        assert_eq!(
            expected_pixels.dimensions(),
            (u32::from(height), u32::from(width))
        );
        // Compare the optimized scene against the separate PNG round trips
        // using today's shipped font metrics and title layout at both sizes.
        let media = runtime
            .profiles
            .render_media_shared(
                &asset,
                std::time::Duration::ZERO,
                width,
                height,
                0,
                "png",
                &binding.media,
            )
            .unwrap();
        let background = hardware_core::render::over_color(
            &media,
            hardware_core::appearance::background_color(&binding.color).unwrap(),
        )
        .unwrap();
        let overlay = runtime.render_visual(&visual, width, height, 0).unwrap();
        let reference = hardware_core::appearance::compose(
            &over_image(&background, &overlay).unwrap(),
            width,
            height,
            &binding.title,
            binding.appearance.as_ref().unwrap(),
            "",
            90,
        )
        .unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(expected_pixels.as_raw())),
            hex::encode(Sha256::digest(
                image::load_from_memory(&reference)
                    .unwrap()
                    .into_rgba8()
                    .as_raw()
            )),
            "optimized and reference pixels differ at {width}x{height}"
        );
        let source_before = runtime.profiles.source_work_stats();
        let mut results = vec![measurements("warm_surface_cache", 100, |_| {
            let started = Instant::now();
            let bytes = render(&binding);
            let elapsed = started.elapsed().as_secs_f64() * 1e6;
            assert_eq!(bytes, expected);
            black_box(bytes);
            elapsed
        })];
        results.push(measurements("cold_render_cache", 20, |_| {
            runtime.profiles.render_cache.lock().unwrap().clear();
            let started = Instant::now();
            let bytes = render(&binding);
            let elapsed = started.elapsed().as_secs_f64() * 1e6;
            assert_eq!(
                image::load_from_memory(&bytes).unwrap().to_rgba8(),
                expected_pixels
            );
            black_box(bytes);
            elapsed
        }));
        results.push(measurements(
            "changing_title_warm_media_and_plugin",
            20,
            |index| {
                binding.title = format!("Frame {index}");
                let started = Instant::now();
                let bytes = render(&binding);
                let elapsed = started.elapsed().as_secs_f64() * 1e6;
                black_box(bytes);
                elapsed
            },
        ));
        let resized = hardware_core::render::render(&png, width, height, 0, "png").unwrap();
        let mut stage = |name, decodes, encodes, mut run: Box<dyn FnMut() -> Vec<u8> + '_>| {
            let mut value = measurements(name, 100, |_| {
                let started = Instant::now();
                black_box(run());
                started.elapsed().as_secs_f64() * 1e6
            });
            // Counts are audited codec calls per invocation of these unchanged functions,
            // not instrumentation of arbitrary renderer paths
            value["audited_decodes_per_call"] = serde_json::json!(decodes);
            value["audited_encodes_per_call"] = serde_json::json!(encodes);
            results.push(value);
        };
        stage(
            "color_composition",
            1,
            1,
            Box::new(|| hardware_core::render::over_color(&resized, [33, 67, 101, 128]).unwrap()),
        );
        stage(
            "image_composition",
            2,
            1,
            Box::new(|| over_image(&resized, &resized).unwrap()),
        );
        stage(
            "appearance_composition",
            1,
            1,
            Box::new(|| {
                hardware_core::appearance::compose(
                    &resized,
                    width,
                    height,
                    "Layered",
                    binding.appearance.as_ref().unwrap(),
                    "42%",
                    90,
                )
                .unwrap()
            }),
        );
        reports.push(serde_json::json!({
            "size":[width,height], "rotation":90, "results":results,
            "fixture_sha256":hex::encode(Sha256::digest(&png)),
            "decoded_output_sha256":hex::encode(Sha256::digest(expected_pixels.as_raw())),
            "source_work_before":source_before,"source_work_after":runtime.profiles.source_work_stats(),
            "resident_render_cache_bytes":runtime.profiles.render_cache.lock().unwrap().resident_bytes(),
            "audited_codec_counts":{"warm_surface_cache":[0,0],"cold_render_cache":[5,4],"changing_title_warm_media_and_plugin":[2,1]},
        }));
    }
    println!(
        "PATCH_LAYERED {}",
        serde_json::json!({"schema_version":1,"scope":"software profiling only, not hardware FPS","cache_note":"cold render cache; OS file cache and font database warm; setup and output validation excluded","codec_count_order":["decode","encode"],"reports":reports})
    );
    runtime.shutdown();
}

#[test]
// Verify that layered pixels match legacy round trips for crop fonts alpha and rotation
fn layered_pixels_match_legacy_round_trips_for_crop_fonts_alpha_and_rotation() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    install_burst_fixture(&runtime);
    let source = image::RgbaImage::from_fn(64, 40, |x, y| {
        image::Rgba([
            (x * 3) as u8,
            (y * 5) as u8,
            (x + y) as u8,
            ((x * 7 + y * 13) % 256) as u8,
        ])
    });
    let png = hardware_core::render::encode_pixels(image::DynamicImage::ImageRgba8(source), "png")
        .unwrap();
    let path = root.path().join("fixture.png");
    std::fs::write(&path, &png).unwrap();
    let asset = runtime.profiles.import_asset(&path).unwrap();
    let visual = serde_json::json!({"image":format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png)),"title":"Plugin","background":"#00000000"});
    let manifest = hardware_registry::manifests()
        .into_iter()
        .find(|m| m.model_id == "n4-pro")
        .unwrap();
    let mut control = manifest.control("key-1").unwrap().clone();
    let context = runtime.render_scene("fixture").unwrap();
    let mut binding = context.page.as_ref().unwrap().bindings["key-1"].clone();
    binding.asset = Some(asset.clone());
    binding.title = "Layered title".into();
    binding.media.crop = [0.1, 0.1, 0.7, 0.7];
    for (width, height) in [(48, 48), (64, 40)] {
        control.image_width = Some(width);
        control.image_height = Some(height);
        for fit in ["fit", "fill"] {
            binding.media.fit = fit.into();
            for (font, color) in [("sans", "#21436580"), ("mono", "#000000")] {
                binding.color = color.into();
                let appearance = hardware_core::appearance::Appearance {
                    font: font.into(),
                    icon: "volume-2".into(),
                    ..Default::default()
                };
                binding.appearance = Some(Box::new(appearance.clone()));
                let media = runtime
                    .profiles
                    .render_media_shared(
                        &asset,
                        std::time::Duration::ZERO,
                        width,
                        height,
                        0,
                        "png",
                        &binding.media,
                    )
                    .unwrap();
                let base = hardware_core::render::over_color(
                    &media,
                    hardware_core::appearance::background_color(color).unwrap(),
                )
                .unwrap();
                for plugin in [false, true] {
                    let legacy_base = if plugin {
                        over_image(
                            &base,
                            &runtime.render_visual(&visual, width, height, 0).unwrap(),
                        )
                        .unwrap()
                    } else {
                        base.clone()
                    };
                    for rotation in [0, 90, 180, 270] {
                        let legacy = hardware_core::appearance::compose(
                            &legacy_base,
                            width,
                            height,
                            &binding.title,
                            &appearance,
                            "42%",
                            rotation,
                        )
                        .unwrap();
                        let current = runtime
                            .render_binding_in_cycle(
                                &control,
                                Some(&binding),
                                rotation,
                                "42%",
                                plugin.then_some(&visual),
                                std::time::Duration::ZERO,
                                false,
                                None,
                                None,
                                None,
                                true,
                            )
                            .unwrap();
                        assert_eq!(
                            image::load_from_memory(&current).unwrap().into_rgba8(),
                            image::load_from_memory(&legacy).unwrap().into_rgba8(),
                            "{width}x{height} {fit} {font} {rotation} plugin={plugin}"
                        );
                    }
                }
            }
        }
    }
    runtime.shutdown();
}

/// Blend display content over a media frame in logical orientation
fn over_image(background: &[u8], foreground: &[u8]) -> hardware_core::Result<Vec<u8>> {
    let mut canvas = image::load_from_memory(background)
        .map_err(|e| hardware_core::Error::Protocol(e.to_string()))?
        .to_rgba8();
    let foreground = image::load_from_memory(foreground)
        .map_err(|e| hardware_core::Error::Protocol(e.to_string()))?
        .to_rgba8();
    if canvas.dimensions() != foreground.dimensions() {
        return Err(hardware_core::Error::Protocol(
            "Display layer dimensions differ".into(),
        ));
    }
    image::imageops::overlay(&mut canvas, &foreground, 0, 0);
    hardware_core::render::encode_pixels(image::DynamicImage::ImageRgba8(canvas), "png")
}
