use crate::{
    actions::Action,
    automation::{AutomationStatus, Schedule, TitleCondition},
    workspace::{AppRule, Edit},
    ProfileData, ProfileStore, Runtime,
};
use std::{collections::BTreeMap, io::Write};

// Construct the feature tests archive
fn archive(path: &std::path::Path, data: &ProfileData) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("profiles.json", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&serde_json::to_vec(data).unwrap()).unwrap();
    zip.finish().unwrap();
}
// Construct the rule fixture for feature tests
fn rule(id: &str) -> AppRule {
    AppRule {
        id: id.into(),
        device_key: "device".into(),
        application: "editor".into(),
        title: None,
        schedule: None,
        profile_id: "default".into(),
        page_id: None,
    }
}

#[test]
// Verify that profile palettes follow sessions history and restart
fn profile_palettes_follow_sessions_history_and_restart() {
    use stream_dock_manager_plugin_host::{ProfilePalette, ProfilePaletteTarget};
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let a = "simulator:n4-pro:a";
    let b = "simulator:n4-pro:b";
    runtime.simulate_device(true, a).unwrap();
    runtime.simulate_device(true, b).unwrap();
    let mut profile = runtime.snapshot().profiles[0].clone();
    profile.palette = Some(ProfilePalette {
        target: ProfilePaletteTarget::Both,
        light_palette: "sage".into(),
        dark_palette: "midnight".into(),
    });
    let original = profile.id.clone();
    runtime
        .save_profile(profile.clone(), runtime.snapshot().configuration_revision)
        .unwrap();
    profile.id = "other-palette".into();
    profile.name = "Other palette".into();
    profile.palette.as_mut().unwrap().light_palette = "sea-glass".into();
    profile.palette.as_mut().unwrap().dark_palette = "deep-teal".into();
    let second = profile.id.clone();
    runtime
        .save_profile(profile.clone(), runtime.snapshot().configuration_revision)
        .unwrap();
    runtime
        .select_device_profile(b, &second, None, true)
        .unwrap();
    runtime.set_profile_theme_context(a).unwrap();
    let colors = |runtime: &Runtime, device: &str| {
        runtime
            .plugins
            .lock()
            .unwrap()
            .device_display_colors(device, None)
            .1
    };
    let system_dark = runtime
        .plugins
        .lock()
        .unwrap()
        .appearance_preferences()
        .system_dark;
    let global = if system_dark { "light" } else { "dark" };
    runtime
        .plugins
        .lock()
        .unwrap()
        .set_appearance(None, global, None)
        .unwrap();
    assert_ne!(colors(&runtime, a), colors(&runtime, b));
    assert_eq!(
        runtime
            .plugins
            .lock()
            .unwrap()
            .appearance_preferences()
            .profile_palette
            .unwrap()
            .light_palette,
        "sage"
    );
    runtime.set_profile_theme_context(b).unwrap();
    assert_eq!(
        runtime
            .plugins
            .lock()
            .unwrap()
            .appearance_preferences()
            .profile_palette
            .unwrap()
            .light_palette,
        "sea-glass"
    );
    let before = colors(&runtime, b);
    profile.palette.as_mut().unwrap().target = ProfilePaletteTarget::Global;
    runtime
        .save_profile(profile, runtime.snapshot().configuration_revision)
        .unwrap();
    assert_ne!(colors(&runtime, b), before);
    runtime
        .history_action("undo", runtime.snapshot().configuration_revision)
        .unwrap();
    assert_eq!(colors(&runtime, b), before);
    runtime
        .history_action("redo", runtime.snapshot().configuration_revision)
        .unwrap();
    assert_ne!(colors(&runtime, b), before);
    runtime
        .history_action("undo", runtime.snapshot().configuration_revision)
        .unwrap();
    runtime
        .select_device_profile(a, &second, None, true)
        .unwrap();
    assert_eq!(colors(&runtime, a), colors(&runtime, b));
    runtime.shutdown();
    drop(runtime);
    let reopened = Runtime::build(root.path().into(), false).unwrap();
    reopened.simulate_device(true, a).unwrap();
    assert_eq!(reopened.snapshot().session(a).profile_id, second);
    assert_eq!(
        reopened
            .snapshot()
            .profiles
            .iter()
            .find(|p| p.id == original)
            .unwrap()
            .palette
            .as_ref()
            .unwrap()
            .light_palette,
        "sage"
    );
    reopened.set_profile_theme_context(a).unwrap();
    assert_eq!(
        reopened
            .plugins
            .lock()
            .unwrap()
            .appearance_preferences()
            .profile_palette
            .unwrap()
            .light_palette,
        "sea-glass"
    );
    assert_eq!(
        reopened
            .plugins
            .lock()
            .unwrap()
            .appearance_preferences()
            .preference,
        global
    );
    reopened.shutdown();
}

#[test]
// Verify that overnight schedules use starting weekday and title rules explain precedence
fn overnight_schedules_use_starting_weekday_and_title_rules_explain_precedence() {
    let overnight = Schedule {
        days: vec![0],
        start: 22 * 60,
        end: 2 * 60,
    };
    assert!(overnight.matches(0, 23 * 60));
    assert!(overnight.matches(1, 60));
    assert!(!overnight.matches(1, 2 * 60));
    assert!(!overnight.matches(0, 60));
    assert!(Schedule {
        days: vec![6],
        start: 0,
        end: 0
    }
    .matches(6, 1439));
    let mut first = rule("first");
    first.title = Some(TitleCondition {
        value: "project".into(),
        mode: "contains".into(),
        case_sensitive: false,
    });
    let second = rule("fallback");
    let capabilities = AutomationStatus {
        available: true,
        title_available: true,
        schedules_available: true,
        ..Default::default()
    };
    let results = crate::automation::explain_rules(
        &[first.clone(), second.clone()],
        "device",
        "editor",
        Some("PROJECT.md"),
        &capabilities,
        0,
        600,
    );
    assert!(results[0].winner);
    assert!(results[1].matched && !results[1].winner);
    let unavailable = AutomationStatus {
        available: false,
        title_available: false,
        schedules_available: true,
        ..Default::default()
    };
    let mut scheduled = rule("scheduled");
    scheduled.application.clear();
    scheduled.schedule = Some(Schedule {
        days: vec![0],
        start: 540,
        end: 1020,
    });
    let results = crate::automation::explain_rules(
        &[first, second, scheduled],
        "device",
        "",
        None,
        &unavailable,
        0,
        600,
    );
    assert!(!results[0].matched);
    assert!(results[0].reasons.iter().any(|r| r.contains("unavailable")));
    assert!(results[2].winner);
}
#[test]
// Verify that schema v1 defaults optional fields without rewriting source
fn schema_v1_defaults_optional_fields_without_rewriting_source() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProfileStore::open(dir.path().into()).unwrap();
    let mut source = serde_json::to_value(ProfileData::default()).unwrap();
    source.as_object_mut().unwrap().remove("styles");
    for binding in source["profiles"][0]["pages"][0]["bindings"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        let binding = binding.as_object_mut().unwrap();
        binding.remove("appearance");
        binding.remove("feedback");
        binding.remove("disabled");
    }
    let original = serde_json::to_vec(&source).unwrap();
    std::fs::write(dir.path().join("profiles.json"), &original).unwrap();
    let loaded = store.read(&dir.path().join("profiles.json")).unwrap();
    assert_eq!(loaded.schema_version, 1);
    assert_eq!(
        std::fs::read(dir.path().join("profiles.json")).unwrap(),
        original
    );
    assert!(loaded.styles.is_empty());
    assert!(loaded.profiles[0].pages[0]
        .bindings
        .values()
        .all(|b| b.appearance.is_none() && b.feedback.is_none() && !b.disabled));
}
#[test]
// Verify that unsupported profile versions are rejected for files and archives
fn unsupported_profile_versions_are_rejected_for_files_and_archives() {
    let dir = tempfile::tempdir().unwrap();
    let store = ProfileStore::open(dir.path().into()).unwrap();
    let path = dir.path().join("unsupported.zip");
    for version in [0, 2, 3, 999] {
        let data = ProfileData {
            saved_macros: vec![],

            schema_version: version,
            ..Default::default()
        };
        assert!(store.save(&data).is_err());
        archive(&path, &data);
        assert!(store
            .import_for_repair(&path)
            .unwrap_err()
            .contains("archive version"));
        assert!(store
            .import_for_repair(&path)
            .unwrap_err()
            .contains("archive version"));
        let original = serde_json::to_vec(&data).unwrap();
        std::fs::write(dir.path().join("profiles.json"), &original).unwrap();
        assert!(store
            .read(&dir.path().join("profiles.json"))
            .unwrap_err()
            .contains("unsupported profile version"));
        assert_eq!(
            std::fs::read(dir.path().join("profiles.json")).unwrap(),
            original
        );
    }
}
#[test]
// Verify that portable appearance uses bundled fonts escapes text and rotates once
fn portable_appearance_uses_bundled_fonts_escapes_text_and_rotates_once() {
    let base = hardware_core::render::encode_pixels(
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(112, 48, image::Rgb([0, 0, 0]))),
        "png",
    )
    .unwrap();
    let appearance = hardware_core::appearance::Appearance {
        icon: "volume-2".into(),
        position: "top".into(),
        ..Default::default()
    };
    let bytes =
        hardware_core::appearance::compose(&base, 112, 48, "<&\" label", &appearance, "50%", 90)
            .unwrap();
    let image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((image.width(), image.height()), (48, 112));
    assert!(image.to_rgb8().pixels().any(|p| p.0 != [0, 0, 0]));
    let invalid = hardware_core::appearance::Appearance {
        icon: "../../private".into(),
        ..appearance
    };
    assert!(invalid.validate().is_err());
}
#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that repaired import merges remaps nested references and is undoable
fn repaired_import_merges_remaps_nested_references_and_is_undoable() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let mut source = ProfileData::default();
    source.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .action = Action::Macro {
        actions: vec![
            Action::Profile {
                profile_id: "default".into(),
            },
            Action::Page {
                page_id: "main".into(),
            },
            Action::Command {
                program: "/definitely/missing/program".into(),
                args: vec![],
            },
        ],
    };
    let path = root.path().join("source.zip");
    archive(&path, &source);
    let preview = runtime.prepare_profile_import(&path).unwrap();
    assert_eq!(preview.issues.len(), 1);
    assert_eq!(runtime.snapshot().profiles.len(), 1);
    let issue = &preview.issues[0];
    assert_eq!(issue.action_path, vec![2]);
    let repaired = runtime
        .update_profile_import(
            &preview.id,
            "merge",
            vec![crate::importing::ImportRepair {
                key: issue.key.clone(),
                replacement: String::new(),
                disable: true,
            }],
        )
        .unwrap();
    assert!(repaired.issues.is_empty());
    let merged = runtime
        .commit_profile_import(&preview.id, repaired.expected_revision)
        .unwrap();
    assert_eq!(merged.profiles.len(), 2);
    let imported = &merged.profiles[1];
    assert_ne!(imported.id, "default");
    assert_ne!(imported.pages[0].id, "main");
    let binding = &imported.pages[0].bindings["key-1"];
    assert!(binding.disabled);
    if let Action::Macro { actions } = &binding.action {
        assert!(matches!(&actions[0],Action::Profile{profile_id} if profile_id==&imported.id));
        assert!(matches!(&actions[1],Action::Page{page_id} if page_id==&imported.pages[0].id));
    } else {
        panic!("macro lost");
    }
    assert_eq!(
        runtime
            .history_action("undo", merged.configuration_revision)
            .unwrap()
            .profiles
            .len(),
        1
    );
    runtime.shutdown();
}
#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that staged import retains repairs after revision conflict and cancel keeps workspace
fn staged_import_retains_repairs_after_revision_conflict_and_cancel_keeps_workspace() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let path = root.path().join("source.zip");
    archive(&path, &ProfileData::default());
    let preview = runtime.prepare_profile_import(&path).unwrap();
    let state = runtime.snapshot();
    let mut profile = state.profiles[0].clone();
    profile.name = "Changed while reviewing".into();
    runtime
        .save_profile(profile, state.configuration_revision)
        .unwrap();
    assert!(runtime
        .commit_profile_import(&preview.id, preview.expected_revision)
        .unwrap_err()
        .contains("Recheck"));
    let refreshed = runtime
        .update_profile_import(&preview.id, "merge", vec![])
        .unwrap();
    assert!(refreshed.expected_revision > preview.expected_revision);
    assert_eq!(
        runtime
            .commit_profile_import(&preview.id, refreshed.expected_revision)
            .unwrap()
            .profiles
            .len(),
        2
    );
    let preview = runtime.prepare_profile_import(&path).unwrap();
    runtime.cancel_profile_import(&preview.id);
    assert!(runtime
        .commit_profile_import(&preview.id, preview.expected_revision)
        .is_err());
    assert_eq!(runtime.snapshot().profiles.len(), 2);
    runtime.shutdown();
}
#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that native preview and bound surface share overlay and live state
fn native_preview_and_bound_surface_share_overlay_and_live_state() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    runtime.simulate_device(true, "simulator:test").unwrap();
    let mut profile = runtime.snapshot().profiles[0].clone();
    let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
    binding.appearance = Some(Box::new(hardware_core::appearance::Appearance {
        icon: "music".into(),
        ..Default::default()
    }));
    binding.feedback = Some("volume".into());
    runtime.feedback_state.lock().unwrap().audio.volume = Some(73);
    runtime
        .save_profile(profile, runtime.snapshot().configuration_revision)
        .unwrap();
    let state = runtime.snapshot();
    let device = &state.devices[0];
    let profile = &state.profiles[0];
    let binding = &profile.pages[0].bindings["key-1"];
    let control = device.manifest.control("key-1").unwrap();
    let bound = runtime
        .render_bound_surface(
            control,
            Some(binding),
            0,
            &device.id,
            "default:main",
            "key-1",
        )
        .unwrap();
    let preview = runtime.preview_surfaces(Some(&device.id)).unwrap();
    use base64::Engine;
    let rendered = base64::engine::general_purpose::STANDARD
        .decode(preview.surfaces["key-1"].split_once(',').unwrap().1)
        .unwrap();
    assert_eq!(&**bound, rendered.as_slice());
    assert_ne!(
        &**bound,
        runtime
            .render_binding(control, Some(binding), 0, "74%", None)
            .unwrap()
            .as_slice()
    );
    runtime.shutdown();
}
#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that photographic auto encoding and preview follow policy and device support
fn photographic_auto_encoding_and_preview_follow_policy_and_device_support() {
    use base64::Engine;
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let device_id = "simulator:encoding";
    runtime.simulate_device(true, device_id).unwrap();
    // A small deterministic textured image exercises photographic encoding
    // without depending on external image files or a large photo corpus
    let image = image::RgbImage::from_fn(128, 128, |x, y| {
        let noise = x.wrapping_mul(1_664_525) ^ y.wrapping_mul(1_013_904_223);
        image::Rgb([
            (x * 2 + (noise & 63)) as u8,
            (y * 2 + ((noise >> 8) & 63)) as u8,
            (x + y + ((noise >> 16) & 63)) as u8,
        ])
    });
    let jpeg_path = root.path().join("photo.jpg");
    image::codecs::jpeg::JpegEncoder::new_with_quality(
        std::fs::File::create(&jpeg_path).unwrap(),
        95,
    )
    .encode_image(&image)
    .unwrap();
    let png_path = root.path().join("graphic.png");
    image.save(&png_path).unwrap();
    let jpeg_asset = runtime.import_asset(&jpeg_path).unwrap();
    let png_asset = runtime.import_asset(&png_path).unwrap();
    runtime.feedback_state.lock().unwrap().audio.volume = Some(73);
    for (asset, policy, supports_jpeg, title, text, feedback, expected_mime) in [
        (&jpeg_asset, "auto", true, false, false, false, "image/jpeg"),
        (&jpeg_asset, "auto", true, true, true, false, "image/png"),
        (&jpeg_asset, "auto", true, true, false, false, "image/png"),
        (&jpeg_asset, "auto", true, false, false, true, "image/png"),
        (&jpeg_asset, "png", true, false, false, false, "image/png"),
        (&jpeg_asset, "auto", false, false, false, false, "image/png"),
        (&jpeg_asset, "auto", true, false, false, false, "image/jpeg"),
        (&png_asset, "auto", true, false, false, false, "image/png"),
    ] {
        let mut profile = runtime.snapshot().profiles[0].clone();
        let binding = profile.pages[0].bindings.get_mut("key-1").unwrap();
        binding.title = if title { "Title 123" } else { "" }.into();
        binding.asset = Some(asset.clone());
        binding.action = Action::None;
        binding.color = "#000000".into();
        binding.media.encoding = policy.into();
        binding.appearance = text.then(Box::default);
        binding.feedback = feedback.then(|| "volume".into());
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        runtime.state.lock().unwrap().devices[0]
            .manifest
            .capabilities
            .image_formats = if supports_jpeg {
            vec!["png".into(), "jpeg".into()]
        } else {
            vec!["png".into()]
        };
        let state = runtime.snapshot();
        let device = &state.devices[0];
        let profile = &state.profiles[0];
        let binding = &profile.pages[0].bindings["key-1"];
        let control = device.manifest.control("key-1").unwrap();
        let rotation = device.manifest.layout.rotation;
        let bound = runtime
            .render_bound_surface(
                control,
                Some(binding),
                rotation,
                device_id,
                "default:main",
                "key-1",
            )
            .unwrap();
        let preview = runtime.preview_surfaces(Some(device_id)).unwrap();
        let (prefix, encoded) = preview.surfaces["key-1"].split_once(',').unwrap();
        let preview_mime = if rotation == 0 {
            expected_mime
        } else {
            "image/png"
        };
        assert_eq!(prefix, format!("data:{preview_mime};base64"));
        let output = image::load_from_memory(&bound).unwrap();
        let upright = match rotation {
            90 => output.rotate270(),
            180 => output.rotate180(),
            270 => output.rotate90(),
            _ => output,
        };
        let preview = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(
            image::load_from_memory(&preview).unwrap().to_rgba8(),
            upright.to_rgba8()
        );
        assert_eq!(crate::encoding::mime(&bound), expected_mime);
        if expected_mime == "image/jpeg" {
            let png = runtime
                .render_binding(control, Some(binding), rotation, "", None)
                .unwrap();
            assert!(bound.len() * 5 <= png.len() * 4);
            assert_eq!(
                bound,
                crate::encoding::choose_shared_quality(
                    crate::media::shared_bytes(png),
                    true,
                    "jpeg",
                    true,
                    false,
                    90
                )
                .unwrap()
            );
        }
    }
    runtime.shutdown();
}
#[test]
#[ignore = "Requires loopback sockets and FFmpeg; never opens HID"]
// Verify that applied media refreshes preview for images GIF and MP4 on keys and touch screens
fn applied_media_refreshes_preview_for_images_gif_and_mp4_on_keys_and_touch_screens() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let id = "simulator:n4-pro:applied-media";
    runtime.simulate_device(true, id).unwrap();
    let image = root.path().join("image.png");
    image::RgbaImage::from_pixel(16, 16, image::Rgba([35, 110, 190, 255]))
        .save(&image)
        .unwrap();
    let gif = root.path().join("animation.gif");
    std::fs::write(&gif, crate::media::tests::sample_gif()).unwrap();
    let video = root.path().join("video.mp4");
    std::fs::write(
        &video,
        include_bytes!("../../../tests/fixtures/media/red-blue.mp4"),
    )
    .unwrap();
    let mut previous = runtime.preview_surfaces(Some(id)).unwrap();
    for (source, animated) in [(&image, false), (&gif, true), (&video, true)] {
        let asset = runtime.import_asset(source).unwrap();
        let saved = runtime.snapshot();
        let mut profile = saved.profiles[0].clone();
        let mut binding = profile.pages[0].bindings["key-1"].clone();
        binding.title = "Media".into();
        binding.color = "#c61e3200".into();
        binding.action = Action::None;
        binding.asset = Some(asset);
        binding.appearance = None;
        binding.feedback = None;
        // Each N4 Pro touch zone renders its own binding
        for control in ["key-6", "touch-1", "touch-2", "dial-2"] {
            profile.pages[0]
                .bindings
                .insert(control.into(), binding.clone());
        }
        let saved = runtime
            .save_profile(profile, saved.configuration_revision)
            .unwrap();
        let current = runtime.preview_surfaces(Some(id)).unwrap();
        assert_eq!(current.configuration_revision, saved.configuration_revision);
        assert_eq!(current.animated, animated);
        let delta = crate::preview::difference(Some(&previous), &current, "media", 1).unwrap();
        for control in ["key-6", "touch-1", "touch-2"] {
            assert_ne!(current.surfaces[control], previous.surfaces[control]);
            assert_eq!(
                delta.surfaces[control].as_ref(),
                Some(&current.surfaces[control])
            );
        }
        if animated {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                std::thread::sleep(std::time::Duration::from_millis(25));
                let next = runtime.preview_surfaces(Some(id)).unwrap();
                if next.surfaces["touch-1"] != current.surfaces["touch-1"] {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Animated preview did not advance"
                );
            }
        }
        previous = current;
    }
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that styles and disabled bindings survive history and portable archives
fn styles_and_disabled_bindings_survive_history_and_portable_archives() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let before = runtime.snapshot();
    let mut binding = before.profiles[0].pages[0].bindings["key-1"].clone();
    binding.plugin_appearance = Some(Box::new(hardware_core::plugin_display::PluginAppearance {
        vendor_profile: None,
        plugin_id: "fixture".into(),
        action_id: "counter".into(),
        elements: vec![hardware_core::plugin_display::ElementStyle {
            layout: "counter".into(),
            key: "count".into(),
            size: 24.,
            ..Default::default()
        }],
    }));
    binding.composition = Some(Box::new(hardware_core::composition::DisplayComposition {
        layers: vec![hardware_core::composition::DisplayLayer {
            id: "label".into(),
            label: "Label".into(),
            source: hardware_core::composition::DisplaySource {
                kind: "text".into(),
                ..Default::default()
            },
            style: hardware_core::plugin_display::ElementStyle {
                layout: "$composition".into(),
                key: "label".into(),
                ..Default::default()
            },
            text: "Portable layer".into(),
        }],
    }));
    binding.disabled = true;
    binding.dial_response = Some(crate::dial_response::DialResponse {
        curve: crate::dial_response::DialCurve::Fast,
        max_steps: 17,
    });
    binding.appearance = Some(Default::default());
    let edited = runtime
        .apply_edits(
            before.configuration_revision,
            "Style and disable",
            vec![
                Edit::SaveStyle {
                    style: crate::workspace::AppearancePreset {
                        background_source: None,
                        id: "style".into(),
                        name: "Favorite".into(),
                        color: Some("#c61e32".into()),
                        appearance: Default::default(),
                        plugin_appearance: binding.plugin_appearance.clone(),
                        composition: binding.composition.clone(),
                        overlay_enabled: None,
                    },
                },
                Edit::Bindings {
                    profile_id: "default".into(),
                    page_id: "main".into(),
                    bindings: BTreeMap::from([("key-1".into(), Some(binding))]),
                },
            ],
        )
        .unwrap();
    let path = root.path().join("export.zip");
    runtime.export_profiles(&path).unwrap();
    let store = ProfileStore::open(root.path().join("imported")).unwrap();
    let imported = store.import_for_repair(&path).unwrap();
    assert_eq!(imported.schema_version, 1);
    assert_eq!(imported.styles.len(), 1);
    assert_eq!(
        imported.styles[0]
            .plugin_appearance
            .as_ref()
            .unwrap()
            .elements[0]
            .size,
        24.
    );
    assert_eq!(
        imported.profiles[0].pages[0].bindings["key-1"]
            .plugin_appearance
            .as_ref()
            .unwrap()
            .elements[0]
            .key,
        "count"
    );
    assert_eq!(
        imported.styles[0].composition.as_ref().unwrap().layers[0].text,
        "Portable layer"
    );
    assert_eq!(
        imported.profiles[0].pages[0].bindings["key-1"]
            .composition
            .as_ref()
            .unwrap()
            .layers[0]
            .text,
        "Portable layer"
    );
    assert!(imported.profiles[0].pages[0].bindings["key-1"].disabled);
    assert_eq!(
        imported.profiles[0].pages[0].bindings["key-1"].dial_response,
        Some(crate::dial_response::DialResponse {
            curve: crate::dial_response::DialCurve::Fast,
            max_steps: 17
        })
    );
    assert!(runtime
        .history_action("undo", edited.configuration_revision)
        .unwrap()
        .styles
        .is_empty());
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that disabled missing media survives repair export and reimport
fn disabled_missing_media_survives_repair_export_and_reimport() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let mut data = ProfileData::default();
    data.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .asset = Some("11111111-2222-4333-8444-555555555555".into());
    let path = root.path().join("broken.zip");
    archive(&path, &data);
    let preview = runtime.prepare_profile_import(&path).unwrap();
    assert_eq!(preview.issues[0].kind, "media");
    let fixed = runtime
        .update_profile_import(
            &preview.id,
            "merge",
            vec![crate::importing::ImportRepair {
                key: preview.issues[0].key.clone(),
                replacement: String::new(),
                disable: true,
            }],
        )
        .unwrap();
    runtime
        .commit_profile_import(&preview.id, fixed.expected_revision)
        .unwrap();
    let snapshot = runtime.simulate_device(true, "simulator:repaired").unwrap();
    runtime
        .select_device_profile("simulator:repaired", &snapshot.profiles[1].id, None, true)
        .unwrap();
    assert!(runtime.preview_surfaces(Some("simulator:repaired")).is_ok());
    let export = root.path().join("retained.zip");
    runtime.export_profiles(&export).unwrap();
    let store = ProfileStore::open(root.path().join("roundtrip")).unwrap();
    let imported = store.import_for_repair(&export).unwrap();
    assert!(imported.profiles[1].pages[0].bindings["key-1"].disabled);
    assert_eq!(
        imported.profiles[1].pages[0].bindings["key-1"]
            .asset
            .as_deref(),
        Some("11111111-2222-4333-8444-555555555555")
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that disabling a binding cancels its queued execution
fn disabling_a_binding_cancels_its_queued_execution() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let before = runtime.simulate_device(true, "simulator:test").unwrap();
    let mut profile = before.profiles[0].clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().action = Action::Macro {
        actions: vec![Action::Delay { milliseconds: 2000 }, Action::None],
    };
    runtime
        .save_profile(profile, before.configuration_revision)
        .unwrap();
    runtime
        .connection_epochs
        .lock()
        .unwrap()
        .insert("simulator:test".into(), 1);
    runtime.dispatch(
        "simulator:test",
        &hardware_core::ControlEvent {
            control_id: "key-1".into(),
            input: hardware_core::Input::Button { pressed: true },
        },
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while crate::feedback::text(&runtime, "macro", "simulator:test", "default:main", "key-1")
        != "Running"
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        crate::feedback::text(&runtime, "macro", "simulator:test", "default:main", "key-1"),
        "Running"
    );
    let state = runtime.snapshot();
    let mut profile = state.profiles[0].clone();
    profile.pages[0].bindings.get_mut("key-1").unwrap().disabled = true;
    runtime
        .save_profile(profile, state.configuration_revision)
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while crate::feedback::text(&runtime, "macro", "simulator:test", "default:main", "key-1")
        != "Cancelled"
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        crate::feedback::text(&runtime, "macro", "simulator:test", "default:main", "key-1"),
        "Cancelled"
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that schedule only automation can be enabled without foreground capability
fn schedule_only_automation_can_be_enabled_without_foreground_capability() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let snapshot = runtime
        .simulate_device(true, "simulator:scheduled")
        .unwrap();
    runtime.state.lock().unwrap().devices[0].simulated = false;
    let mut profile = snapshot.profiles[0].clone();
    profile.id = "scheduled".into();
    profile.name = "Scheduled".into();
    let saved = runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    {
        let mut status = runtime.automation_status.lock().unwrap();
        status.available = false;
        status.title_available = false;
        status.schedules_available = true;
    }
    runtime
        .set_automation("simulator:scheduled", true, false)
        .unwrap();
    let mut scheduled = rule("time-only");
    scheduled.application.clear();
    scheduled.device_key = "simulator:scheduled".into();
    scheduled.profile_id = "scheduled".into();
    scheduled.schedule = Some(Schedule {
        days: vec![0, 1, 2, 3, 4, 5, 6],
        start: 0,
        end: 0,
    });
    runtime
        .apply_edits(
            saved.configuration_revision,
            "Schedule-only rule",
            vec![Edit::Rules {
                rules: vec![scheduled],
            }],
        )
        .unwrap();
    crate::automation::evaluate(&runtime, "");
    assert_eq!(
        runtime.snapshot().session("simulator:scheduled").profile_id,
        "scheduled"
    );
    runtime.shutdown();
}

#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that merged profiles preserve page IDs used by cross profile macros
fn merged_profiles_preserve_page_ids_used_by_cross_profile_macros() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let mut source = ProfileData::default();
    let mut second = source.profiles[0].clone();
    second.id = "work".into();
    second.name = "Work".into();
    source.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .action = Action::Macro {
        actions: vec![
            Action::Profile {
                profile_id: "work".into(),
            },
            Action::Page {
                page_id: "main".into(),
            },
        ],
    };
    source.profiles.push(second);
    let path = root.path().join("cross-profile.zip");
    archive(&path, &source);
    let preview = runtime.prepare_profile_import(&path).unwrap();
    assert!(preview.issues.is_empty());
    let snapshot = runtime
        .commit_profile_import(&preview.id, preview.expected_revision)
        .unwrap();
    assert_eq!(snapshot.profiles.len(), 3);
    let imported = &snapshot.profiles[1];
    let destination = &snapshot.profiles[2];
    assert_eq!(imported.pages[0].id, destination.pages[0].id);
    if let Action::Macro { actions } = &imported.pages[0].bindings["key-1"].action {
        assert!(matches!(&actions[1],Action::Page{page_id} if page_id==&destination.pages[0].id));
    } else {
        panic!("Macro lost");
    }
    runtime.shutdown();
}
#[test]
#[ignore = "Requires loopback sockets, never HID"]
// Verify that cli replacement uses validated staging and rejects unresolved dependencies
fn cli_replacement_uses_validated_staging_and_rejects_unresolved_dependencies() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let path = root.path().join("cli.zip");
    let mut data = ProfileData::default();
    data.profiles[0].name = "Imported via CLI".into();
    archive(&path, &data);
    let imported = runtime.import_profiles(&path).unwrap();
    assert_eq!(imported.profiles.len(), 1);
    assert_eq!(imported.profiles[0].name, "Imported via CLI");
    data.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .action = Action::Command {
        program: "/definitely/missing".into(),
        args: vec![],
    };
    archive(&path, &data);
    assert!(runtime
        .import_profiles(&path)
        .unwrap_err()
        .contains("requires repairs"));
    assert_eq!(runtime.snapshot().profiles[0].name, "Imported via CLI");
    runtime.shutdown();
}

#[test]
// Verify that empty and defined controls keep transparent backgrounds through native rendering
fn empty_and_defined_controls_keep_transparent_backgrounds_through_native_rendering() {
    use image::GenericImageView;
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let mut profile = ProfileData::default().profiles.remove(0);
    let manifest = hardware_registry::manifests()
        .into_iter()
        .find(|manifest| manifest.model_id == profile.model_id)
        .unwrap();
    for control in manifest
        .controls
        .iter()
        .filter(|control| control.image_slot.is_some())
    {
        let empty = runtime.render_binding(control, None, 0, "", None).unwrap();
        assert!(image::load_from_memory(&empty)
            .unwrap()
            .to_rgba8()
            .pixels()
            .all(|pixel| pixel[3] == 0));
    }
    let control = manifest
        .controls
        .iter()
        .find(|control| control.image_width.is_some())
        .unwrap();
    let mut binding = crate::Binding {
        dial_response: None,
        title: String::new(),
        color: "#242424".into(),
        custom_background: false,
        background_source: None,
        action: Action::None,
        asset: None,
        media: Default::default(),
        plugin_appearance: None,
        composition: None,
        appearance: None,
        feedback: None,
        activation_policy: Default::default(),
        disabled: false,
    };
    let cleared = runtime
        .render_binding(control, Some(&binding), 0, "", None)
        .unwrap();
    assert!(image::load_from_memory(&cleared)
        .unwrap()
        .to_rgba8()
        .pixels()
        .all(|pixel| pixel[3] == 0));
    binding.title = "Visible label".into();
    binding.action = Action::Mute;
    binding.color = "#2468ac00".into();
    binding.appearance = Some(Box::new(hardware_core::appearance::Appearance {
        icon: "music".into(),
        ..Default::default()
    }));
    binding.validate().unwrap();
    let bytes = runtime
        .render_binding(control, Some(&binding), 90, "50%", None)
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!(
        image.dimensions(),
        (
            u32::from(control.image_height.unwrap()),
            u32::from(control.image_width.unwrap())
        )
    );
    assert_eq!(image.get_pixel(0, 0)[3], 0);
    assert!(image.pixels().any(|pixel| pixel[3] == 255));
    for invalid in [
        "#12345",
        "#1234567",
        "#123456xx",
        "transparent",
        "#123456000",
    ] {
        binding.color = invalid.into();
        assert!(binding.validate().is_err());
    }
    binding.color = "#2468ac00".into();
    binding.appearance = None;
    let visual = serde_json::json!({"layout":{"background":"#080808","items":[{"key":"value","type":"text","rect":[0,20,200,60]},{"key":"progress","type":"bar","rect":[0,90,200,10],"bar_bg_c":"#333333","bar_fill_c":"#00ff00"}]},"feedback":{"value":"Plugin","progress":50}});
    let plugin = runtime
        .render_binding(control, Some(&binding), 0, "", Some(&visual))
        .unwrap();
    let plugin = image::load_from_memory(&plugin).unwrap().to_rgba8();
    assert_eq!(plugin.get_pixel(0, 0)[3], 0);
    assert_eq!(
        plugin
            .get_pixel(plugin.width() - 1, plugin.height() * 9 / 10 + 2)
            .0,
        [51, 51, 51, 255]
    );
    assert!(plugin.pixels().any(|pixel| pixel[3] == 255));
    binding.color = "#2468ac".into();
    let opaque = runtime
        .render_binding(control, Some(&binding), 0, "", Some(&visual))
        .unwrap();
    assert_eq!(
        image::load_from_memory(&opaque).unwrap().get_pixel(0, 0).0,
        [36, 104, 172, 255]
    );
    profile.pages[0]
        .bindings
        .insert(control.id.clone(), binding);
    profile.pages[0]
        .bindings
        .get_mut(&control.id)
        .unwrap()
        .color = "#2468ac00".into();
    profile.validate().unwrap();
    runtime.shutdown();
}

#[test]
// Verify that media assets stay behind binding content on keys and touch sections
fn media_assets_stay_behind_binding_content_on_keys_and_touch_sections() {
    use hardware_core::composition::{DisplayComposition, DisplayLayer, DisplaySource};
    use hardware_core::plugin_display::ElementStyle;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    let path = root.path().join("background.gif");
    let colors = [[24, 80, 150, 255], [120, 40, 60, 255]];
    {
        let mut encoder =
            image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
        encoder
            .set_repeat(image::codecs::gif::Repeat::Infinite)
            .unwrap();
        for color in colors {
            encoder
                .encode_frame(image::Frame::from_parts(
                    image::RgbaImage::from_pixel(32, 32, image::Rgba(color)),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(100, 1),
                ))
                .unwrap();
        }
    }
    let asset = runtime.import_asset(&path).unwrap();
    let translucent_path = root.path().join("translucent.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([30, 60, 90, 128]))
        .save(&translucent_path)
        .unwrap();
    let translucent = runtime.import_asset(&translucent_path).unwrap();
    let mut profile = ProfileData::default().profiles.remove(0);
    let manifest = hardware_registry::manifests()
        .into_iter()
        .find(|m| m.model_id == profile.model_id)
        .unwrap();
    let visual = serde_json::json!({"plugin_id":"weather","action_id":"conditions","layout":{"id":"weather-v1","background":"#000000","items":[{"key":"temperature","type":"text","rect":[10,20,180,60],"font":{"size":24},"color":"#ffffff","value":"54 °F"}]}});
    for id in ["key-1", "touch-1"] {
        let control = manifest.control(id).unwrap();
        let mut binding = profile.pages[0].bindings["key-1"].clone();
        binding.title = String::new();
        binding.color = "#010203".into();
        binding.action = Action::None;
        binding.asset = Some(asset.clone());
        binding.appearance = None;
        binding.feedback = None;
        binding.composition = None;
        let render = |binding: &crate::Binding, visual, elapsed, rotation| {
            image::load_from_memory(
                &runtime
                    .render_binding_at(control, Some(binding), rotation, "", visual, elapsed, false)
                    .unwrap(),
            )
            .unwrap()
            .to_rgba8()
        };
        let base = render(&binding, None, Duration::ZERO, 0);
        assert!(base.pixels().all(|p| p.0 == colors[0]));
        // A plain title, an appearance icon and runtime feedback all overlay media
        binding.title = "Visible".into();
        let titled = render(&binding, None, Duration::ZERO, 0);
        assert_ne!(titled, base);
        assert_eq!(titled.get_pixel(0, 0).0, colors[0]);
        binding.appearance = Some(Box::new(hardware_core::appearance::Appearance {
            icon: "sun".into(),
            ..Default::default()
        }));
        let bytes = runtime
            .render_binding_at(
                control,
                Some(&binding),
                0,
                "50%",
                None,
                Duration::ZERO,
                false,
            )
            .unwrap();
        assert_ne!(image::load_from_memory(&bytes).unwrap().to_rgba8(), titled);
        binding.appearance = None;
        binding.title.clear();
        assert!(runtime
            .display_sources(control, &binding, "", Some(&visual), Duration::ZERO, false)
            .unwrap()
            .iter()
            .any(|s| s.source.kind == "media" && s.available && s.style.visible));
        let live = render(&binding, Some(&visual), Duration::ZERO, 0);
        assert_ne!(live, base);
        assert_eq!(live.get_pixel(0, 0).0, colors[0]);
        let next = render(&binding, Some(&visual), Duration::from_millis(150), 0);
        assert_eq!(next.get_pixel(0, 0).0, colors[1]);
        assert!(live
            .pixels()
            .zip(next.pixels())
            .any(|(a, b)| a == b && a.0 != colors[0]));
        assert_eq!(
            render(&binding, Some(&visual), Duration::ZERO, 90),
            image::imageops::rotate90(&live)
        );
        let layer = DisplayLayer {
            id: "label".into(),
            label: "Label".into(),
            source: DisplaySource {
                kind: "text".into(),
                ..Default::default()
            },
            style: ElementStyle {
                layout: "$composition".into(),
                key: "label".into(),
                rect: [10., 20., 80., 60.],
                size: 24.,
                ..Default::default()
            },
            text: "Overlay".into(),
        };
        binding.composition = Some(Box::new(DisplayComposition {
            layers: vec![layer],
        }));
        let canvas = render(&binding, Some(&visual), Duration::ZERO, 0);
        assert_eq!(canvas.get_pixel(0, 0).0, colors[0]);
        assert_ne!(canvas, base);
        assert_eq!(
            render(&binding, Some(&visual), Duration::from_millis(150), 0)
                .get_pixel(0, 0)
                .0,
            colors[1]
        );
        // Explicit canvas media layers retain their visibility and bounds
        binding.composition.as_mut().unwrap().layers.insert(
            0,
            DisplayLayer {
                id: "media".into(),
                label: "Media".into(),
                source: DisplaySource {
                    kind: "media".into(),
                    ..Default::default()
                },
                style: ElementStyle {
                    layout: "$composition".into(),
                    key: "media".into(),
                    kind: "pixmap".into(),
                    visible: false,
                    rect: [0., 0., 50., 50.],
                    ..Default::default()
                },
                text: String::new(),
            },
        );
        assert_eq!(
            render(&binding, Some(&visual), Duration::ZERO, 0)
                .get_pixel(0, 0)
                .0,
            [1, 2, 3, 255]
        );
        binding.composition.as_mut().unwrap().layers[0]
            .style
            .visible = true;
        let positioned = render(&binding, Some(&visual), Duration::ZERO, 0);
        assert_eq!(positioned.get_pixel(0, 0).0, colors[0]);
        assert_eq!(
            positioned
                .get_pixel(positioned.width() - 1, positioned.height() - 1)
                .0,
            [1, 2, 3, 255]
        );
        binding.composition.as_mut().unwrap().layers.remove(0);
        binding.asset = Some(translucent.clone());
        binding.color = "#64646480".into();
        let composed_alpha = render(&binding, Some(&visual), Duration::ZERO, 0);
        let mut media_only = binding.clone();
        media_only.composition = None;
        assert_eq!(
            composed_alpha.get_pixel(0, 0),
            render(&media_only, None, Duration::ZERO, 0).get_pixel(0, 0)
        );
        binding.color = "#010203".into();
        binding.asset = None;
        assert_eq!(
            render(&binding, Some(&visual), Duration::ZERO, 0)
                .get_pixel(0, 0)
                .0,
            [1, 2, 3, 255]
        );
        profile.pages[0].bindings.insert(id.into(), binding);
    }
    runtime.shutdown();
}

#[test]
// Verify that automatic title regex is validated and case aware
fn automatic_title_regex_is_validated_and_case_aware() {
    let mut rule = rule("regex");
    rule.title = Some(TitleCondition {
        value: r"^Project \d+ – Editor$".into(),
        mode: "regex".into(),
        case_sensitive: false,
    });
    assert!(rule.validate().is_ok());
    let capabilities = AutomationStatus {
        available: true,
        title_available: true,
        schedules_available: true,
        ..Default::default()
    };
    let matches = |rule: &AppRule, title: &str| {
        crate::automation::explain_rules(
            std::slice::from_ref(rule),
            "device",
            "editor",
            Some(title),
            &capabilities,
            0,
            0,
        )[0]
        .matched
    };
    assert!(matches(&rule, "project 12 – editor"));
    assert!(!matches(&rule, "Other Project 12 – Editor"));
    rule.title.as_mut().unwrap().case_sensitive = true;
    assert!(!matches(&rule, "project 12 – editor"));
    assert!(matches(&rule, "Project 12 – Editor"));
    for pattern in ["[", r"(a)\1", r"(?=unsafe)", "a{100000000}"] {
        rule.title.as_mut().unwrap().value = pattern.into();
        assert!(rule.validate().is_err(), "{pattern}");
        assert!(!matches(&rule, "anything"));
    }
}

#[test]
// Verify that automatic rules switch pages within the same profile
fn automatic_rules_switch_pages_within_the_same_profile() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let device = "simulator:automatic-pages";
    let snapshot = runtime.simulate_device(true, device).unwrap();
    runtime.state.lock().unwrap().devices[0].simulated = false;
    let mut profile = snapshot.profiles[0].clone();
    let first_page = profile.pages[0].id.clone();
    let mut second = profile.pages[0].clone();
    second.id = "automatic-second".into();
    second.name = "Second".into();
    profile.pages.push(second);
    let profile_id = profile.id.clone();
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    runtime.automation_status.lock().unwrap().available = true;
    runtime.set_automation(device, true, false).unwrap();
    let make_rule = |id: &str, page: &str| {
        let mut value = rule(id);
        value.application = id.into();
        value.device_key = device.into();
        value.profile_id = profile_id.clone();
        value.page_id = Some(page.into());
        value
    };
    runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Page rules",
            vec![Edit::Rules {
                rules: vec![
                    make_rule("first", &first_page),
                    make_rule("second", "automatic-second"),
                ],
            }],
        )
        .unwrap();
    crate::automation::evaluate(&runtime, "second");
    assert_eq!(
        runtime.snapshot().session(device).page_id,
        "automatic-second"
    );
    crate::automation::evaluate(&runtime, "first");
    assert_eq!(runtime.snapshot().session(device).page_id, first_page);
    let invalid = make_rule("invalid", "missing");
    assert!(runtime
        .apply_edits(
            runtime.snapshot().configuration_revision,
            "Invalid page",
            vec![Edit::Rules {
                rules: vec![invalid]
            }]
        )
        .is_err());
    runtime.shutdown();
}

#[test]
// Verify that automatic rules for removed targets are removed without retargeting
fn automatic_rules_for_removed_targets_are_removed_without_retargeting() {
    let mut data = ProfileData::default();
    // Use the normal factory rather than constructing profile schemas by hand
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let snapshot = runtime
        .simulate_device(true, "simulator:rule-removal")
        .unwrap();
    let mut profile = snapshot.profiles[0].clone();
    let profile_id = profile.id.clone();
    let mut page = profile.pages[0].clone();
    page.id = "removed-page".into();
    profile.pages.push(page);
    data.profiles = vec![profile];
    let mut page_rule = rule("page");
    page_rule.profile_id = profile_id.clone();
    page_rule.page_id = Some("removed-page".into());
    let mut profile_rule = rule("profile");
    profile_rule.profile_id = profile_id.clone();
    data.rules = vec![page_rule, profile_rule];
    data.delete_page(&profile_id, "removed-page").unwrap();
    assert_eq!(data.rules.len(), 1);
    assert_eq!(data.rules[0].id, "profile");
    data.delete_profile(&profile_id).unwrap();
    assert!(data.rules.is_empty());
    runtime.shutdown();
}

#[test]
// Verify that automatic configuration draft is atomic and rejects stale changes
fn automatic_configuration_draft_is_atomic_and_rejects_stale_changes() {
    use crate::automation::AutomationConfigurationDraft;
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let device = "simulator:atomic-automation";
    let snapshot = runtime.simulate_device(true, device).unwrap();
    runtime.automation_status.lock().unwrap().available = true;
    let mut target_rule = rule("atomic");
    target_rule.device_key = device.into();
    target_rule.profile_id = snapshot.session(device).profile_id;
    let request = AutomationConfigurationDraft {
        device_id: device.into(),
        expected_revision: snapshot.configuration_revision,
        expected_enabled: false,
        expected_paused: false,
        expected_restore_on_exit: false,
        restore_on_exit: false,
        rules: vec![target_rule],
        enabled: true,
        paused: false,
    };
    let mut invalid = request.clone();
    invalid.rules[0].page_id = Some("missing".into());
    assert!(runtime.save_automation_configuration(invalid).is_err());
    assert!(!runtime.snapshot().session(device).automation_enabled);
    assert!(runtime.snapshot().rules.is_empty());
    runtime.set_automation(device, false, true).unwrap();
    assert!(runtime
        .save_automation_configuration(request.clone())
        .is_err());
    assert!(runtime.snapshot().session(device).automation_paused);
    runtime.set_automation(device, false, false).unwrap();
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=ON")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    assert!(runtime
        .save_automation_configuration(request.clone())
        .is_err());
    assert!(!runtime.snapshot().session(device).automation_enabled);
    assert!(runtime.profiles.load().unwrap().rules.is_empty());
    runtime
        .profiles
        .storage
        .run(|connection| {
            connection
                .execute_batch("PRAGMA query_only=OFF")
                .map_err(|e| e.to_string())
        })
        .unwrap();
    let applied = runtime
        .save_automation_configuration(request.clone())
        .unwrap();
    assert!(applied.session(device).automation_enabled);
    assert_eq!(applied.rules[0].id, "atomic");
    assert_eq!(runtime.profiles.load().unwrap().rules[0].id, "atomic");
    let stored: std::collections::BTreeMap<String, crate::workspace::DeviceSession> = runtime
        .profiles
        .read_preference(crate::profile_database::LocalPreference::Sessions)
        .unwrap()
        .unwrap();
    assert!(stored[device].automation_enabled);
    assert!(runtime.save_automation_configuration(request).is_err());
    runtime.shutdown();
}

#[test]
// Verify that automatic inverse restores original page after matching chain
fn automatic_inverse_restores_original_page_after_matching_chain() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let device = "simulator:inverse";
    let snapshot = runtime.simulate_device(true, device).unwrap();
    runtime.state.lock().unwrap().devices[0].simulated = false;
    runtime.automation_status.lock().unwrap().available = true;
    let mut profile = snapshot.profiles[0].clone();
    let original = profile.pages[0].id.clone();
    for id in ["work", "chat"] {
        let mut page = profile.pages[0].clone();
        page.id = id.into();
        page.name = id.into();
        profile.pages.push(page);
    }
    let profile_id = profile.id.clone();
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    let rules = ["work", "chat"]
        .into_iter()
        .map(|id| {
            let mut r = rule(id);
            r.application = id.into();
            r.device_key = device.into();
            r.profile_id = profile_id.clone();
            r.page_id = Some(id.into());
            r
        })
        .collect();
    runtime
        .save_automation_configuration(crate::automation::AutomationConfigurationDraft {
            device_id: device.into(),
            expected_revision: runtime.snapshot().configuration_revision,
            expected_enabled: false,
            expected_paused: false,
            expected_restore_on_exit: false,
            rules,
            enabled: true,
            paused: false,
            restore_on_exit: true,
        })
        .unwrap();
    let before_match = runtime.snapshot().session(device);
    crate::automation::evaluate(&runtime, "work");
    assert_eq!(runtime.snapshot().session(device).page_id, "work");
    let current = runtime.snapshot();
    runtime
        .select_automatic_profile(
            device,
            &profile_id,
            Some("chat"),
            true,
            current.configuration_revision,
            &before_match,
        )
        .unwrap();
    assert_eq!(runtime.snapshot().session(device).page_id, "work");
    runtime
        .select_automatic_profile(
            device,
            &profile_id,
            Some("chat"),
            true,
            current.configuration_revision.saturating_sub(1),
            &current.session(device),
        )
        .unwrap();
    assert_eq!(runtime.snapshot().session(device).page_id, "work");
    runtime
        .select_automatic_profile(
            device,
            "deleted-profile",
            Some("deleted-page"),
            true,
            current.configuration_revision.saturating_sub(1),
            &current.session(device),
        )
        .unwrap();
    assert_eq!(runtime.snapshot().session(device).page_id, "work");
    crate::automation::evaluate(&runtime, "chat");
    assert_eq!(runtime.snapshot().session(device).page_id, "chat");
    assert_eq!(
        runtime.snapshot().session(device).automation_origin,
        Some((profile_id.clone(), original.clone()))
    );
    crate::automation::evaluate(&runtime, "unmatched");
    assert_eq!(runtime.snapshot().session(device).page_id, original);
    assert!(runtime
        .snapshot()
        .session(device)
        .automation_origin
        .is_none());
    let revision = runtime.snapshot().revision;
    crate::automation::evaluate(&runtime, "unmatched");
    assert_eq!(runtime.snapshot().revision, revision);
    crate::automation::evaluate(&runtime, "work");
    runtime
        .select_device_profile(device, &profile_id, Some("chat"), true)
        .unwrap();
    assert!(!runtime.snapshot().session(device).automation_paused);
    assert_eq!(
        runtime.snapshot().session(device).automation_origin,
        Some((profile_id.clone(), original.clone()))
    );
    for _ in 0..3 {
        *runtime.foreground_title.lock().unwrap() = Some("title chatter".into());
        crate::automation::evaluate(&runtime, "work");
        assert_eq!(runtime.snapshot().session(device).page_id, "chat");
    }
    runtime.set_automation(device, true, false).unwrap();
    crate::automation::evaluate(&runtime, "work");
    assert_eq!(runtime.snapshot().session(device).page_id, "work");
    runtime
        .select_device_profile(device, &profile_id, Some("chat"), false)
        .unwrap();
    crate::automation::evaluate(&runtime, "app.streamdockmanager.manager");
    crate::automation::evaluate(&runtime, "work");
    assert_eq!(runtime.snapshot().session(device).page_id, "chat");
    crate::automation::evaluate(&runtime, "unmatched");
    assert_eq!(runtime.snapshot().session(device).page_id, original);
    runtime
        .select_device_profile(device, &profile_id, Some("chat"), false)
        .unwrap();
    runtime.set_automation(device, true, false).unwrap();
    crate::automation::evaluate(&runtime, "work");
    runtime
        .delete_page(
            &profile_id,
            "chat",
            runtime.snapshot().configuration_revision,
        )
        .unwrap();
    crate::automation::evaluate(&runtime, "unmatched");
    assert_eq!(runtime.snapshot().session(device).page_id, original);
    assert!(runtime
        .snapshot()
        .session(device)
        .automation_origin
        .is_none());
    let mut other = runtime.snapshot().profiles[0].clone();
    other.id = "manual-other".into();
    runtime
        .save_profile(other, runtime.snapshot().configuration_revision)
        .unwrap();
    crate::automation::evaluate(&runtime, "work");
    runtime
        .select_device_profile(device, "manual-other", None, true)
        .unwrap();
    assert!(runtime.snapshot().session(device).automation_paused);
    assert!(runtime
        .snapshot()
        .session(device)
        .automation_origin
        .is_none());
    crate::automation::evaluate(&runtime, "unmatched");
    assert_eq!(
        runtime.snapshot().session(device).profile_id,
        "manual-other"
    );
    runtime.shutdown();
}

#[test]
// Verify that automatic edges follow winner changes and explicit apply not title chatter
fn automatic_edges_follow_winner_changes_and_explicit_apply_not_title_chatter() {
    use crate::automation::AutomationConfigurationDraft;
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    let device = "simulator:edge-title";
    let snapshot = runtime.simulate_device(true, device).unwrap();
    runtime.state.lock().unwrap().devices[0].simulated = false;
    {
        let mut status = runtime.automation_status.lock().unwrap();
        status.available = true;
        status.title_available = true;
    }
    let mut profile = snapshot.profiles[0].clone();
    let profile_id = profile.id.clone();
    let original = profile.pages[0].id.clone();
    for name in ["work", "other"] {
        let mut page = profile.pages[0].clone();
        page.id = name.into();
        page.name = name.into();
        profile.pages.push(page);
    }
    runtime
        .save_profile(profile, snapshot.configuration_revision)
        .unwrap();
    let mut work = rule("work");
    work.application = "editor".into();
    work.device_key = device.into();
    work.profile_id = profile_id.clone();
    work.page_id = Some("work".into());
    work.title = Some(TitleCondition {
        value: "work".into(),
        mode: "contains".into(),
        case_sensitive: false,
    });
    let mut other = work.clone();
    other.id = "other".into();
    other.title = None;
    other.page_id = Some("other".into());
    let mut draft = AutomationConfigurationDraft {
        device_id: device.into(),
        expected_revision: runtime.snapshot().configuration_revision,
        expected_enabled: false,
        expected_paused: false,
        expected_restore_on_exit: false,
        rules: vec![work, other],
        enabled: true,
        paused: false,
        restore_on_exit: false,
    };
    runtime
        .save_automation_configuration(draft.clone())
        .unwrap();
    *runtime.foreground_title.lock().unwrap() = Some("work file".into());
    crate::automation::evaluate(&runtime, "editor");
    assert_eq!(runtime.snapshot().session(device).page_id, "work");
    runtime
        .select_device_profile(device, &profile_id, Some(&original), false)
        .unwrap();
    *runtime.foreground_title.lock().unwrap() = Some("work renamed file".into());
    crate::automation::evaluate(&runtime, "editor");
    assert_eq!(runtime.snapshot().session(device).page_id, original);
    *runtime.foreground_title.lock().unwrap() = Some("notes".into());
    crate::automation::evaluate(&runtime, "editor");
    assert_eq!(runtime.snapshot().session(device).page_id, "other");
    runtime
        .select_device_profile(device, &profile_id, Some(&original), false)
        .unwrap();
    draft.expected_revision = runtime.snapshot().configuration_revision;
    draft.expected_enabled = true;
    runtime.save_automation_configuration(draft).unwrap();
    crate::automation::evaluate(&runtime, "editor");
    assert_eq!(runtime.snapshot().session(device).page_id, "other");
    runtime.shutdown();
}
