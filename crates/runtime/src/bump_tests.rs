//! Disposable fixtures: no HID, desktop actions, or external plugin processes
use super::*;
use std::collections::BTreeMap;
const DOCK: &str = "simulator:n4-pro:bump";
// Construct an isolated bump tests fixture
fn fixture() -> (tempfile::TempDir, Arc<Runtime>) {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("configuration"), false).unwrap();
    runtime.simulate_device(true, DOCK).unwrap();
    (root, runtime)
}
// Construct the binding fixture for bump tests
fn binding() -> Binding {
    serde_json::from_value(serde_json::json!({"title":"Kept title","color":"#000000","action":{"kind":"none"},"media":media::MediaOptions::default()})).unwrap()
}
#[test]
// Verify that artwork is one undoable edit preserves bindings and samples one animation phase
fn artwork_is_one_undoable_edit_preserves_bindings_and_samples_one_animation_phase() {
    let (root, runtime) = fixture();
    let state = runtime.snapshot();
    let session = state.session(DOCK);
    let mut profile = state
        .profiles
        .iter()
        .find(|p| p.id == session.profile_id)
        .unwrap()
        .clone();
    let mut original = binding();
    original.appearance = Some(Box::default());
    original.disabled = true;
    profile.pages[0]
        .bindings
        .insert("key-1".into(), original.clone());
    runtime
        .save_profile(profile, state.configuration_revision)
        .unwrap();
    for format in ["png", "gif", "mp4"] {
        let frames: Vec<_> = [[210, 30, 10, 255], [10, 30, 210, 255]]
            .into_iter()
            .map(|color| {
                hardware_core::render::encode_pixels(
                    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                        240,
                        120,
                        image::Rgba(color),
                    )),
                    "png",
                )
                .unwrap()
            })
            .collect();
        let asset = if format == "png" {
            let file = root.path().join("art.png");
            std::fs::write(&file, &frames[0]).unwrap();
            runtime.import_asset(&file).unwrap()
        } else {
            // Prepared-media fixture, including MP4's production archive path
            let clip = media::Clip::new(format, frames, vec![100, 100]).unwrap();
            runtime
                .profiles
                .publish_prepared_clip_shared(
                    std::sync::Arc::new(clip),
                    if format == "gif" {
                        include_bytes!("../../../tests/fixtures/media/source-25.gif").as_slice()
                    } else {
                        include_bytes!("../../../tests/fixtures/media/red-blue.mp4").as_slice()
                    },
                    &Default::default(),
                )
                .unwrap()
                .0
        };
        let state = runtime.snapshot();
        let device = &state.devices[0];
        let request = artwork::ArtworkSlice {
            device_id: DOCK.into(),
            profile_id: session.profile_id.clone(),
            page_id: session.page_id.clone(),
            controls: vec!["key-1".into(), "key-2".into()],
            asset: asset.clone(),
            media: media::MediaOptions::default(),
            image_rect: [
                0.,
                0.,
                f64::from(device.manifest.layout.width),
                f64::from(device.manifest.layout.height),
            ],
        };
        let before = serde_json::to_value(&state.profiles).unwrap();
        let sliced = runtime
            .slice_artwork(request.clone(), state.configuration_revision)
            .unwrap();
        assert_eq!(
            sliced.configuration_revision,
            state.configuration_revision + 1
        );
        assert!(runtime
            .slice_artwork(request.clone(), state.configuration_revision)
            .is_err());
        let page = &sliced
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap()
            .pages[0];
        let first = &page.bindings["key-1"];
        let second = &page.bindings["key-2"];
        assert_eq!(first.title, original.title);
        assert!(first.disabled && first.appearance.is_some());
        assert_eq!(first.asset, second.asset);
        assert_eq!(first.media.speed, second.media.speed);
        assert!(first.media.crop[0] + first.media.crop[2] < second.media.crop[0]);
        if format != "png" {
            for ms in [0, 99, 100, 199, 215, 350] {
                let colors: Vec<_> = [first, second]
                    .into_iter()
                    .map(|b| {
                        let bytes = runtime
                            .profiles
                            .render_media(
                                &asset,
                                std::time::Duration::from_millis(ms),
                                30,
                                30,
                                0,
                                "png",
                                &b.media,
                            )
                            .unwrap();
                        image::load_from_memory(&bytes)
                            .unwrap()
                            .to_rgba8()
                            .get_pixel(15, 15)
                            .0
                    })
                    .collect();
                assert_eq!(colors[0], colors[1]);
                assert_eq!(colors[0][2] > colors[0][0], ms % 200 >= 100);
            }
        }
        let undone = runtime
            .history_action("undo", sliced.configuration_revision)
            .unwrap();
        assert_eq!(serde_json::to_value(&undone.profiles).unwrap(), before);
        let mut invalid = request.clone();
        invalid.image_rect[2] = 1.;
        assert!(runtime
            .slice_artwork(invalid, undone.configuration_revision)
            .is_err());
        let mut changed = undone
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap()
            .clone();
        changed.pages.push(Page {
            id: "other".into(),
            name: "Other".into(),
            bindings: BTreeMap::new(),
        });
        let changed = runtime
            .save_profile(changed, undone.configuration_revision)
            .unwrap();
        let prepared = runtime
            .artwork_bindings(&request, changed.configuration_revision)
            .unwrap();
        runtime
            .select_device_profile(DOCK, &session.profile_id, Some("other"), false)
            .unwrap();
        assert!(runtime
            .apply_workspace_edits_in_session(
                changed.configuration_revision,
                "Stale slices",
                vec![workspace::Edit::Bindings {
                    profile_id: session.profile_id.clone(),
                    page_id: session.page_id.clone(),
                    bindings: prepared
                }],
                Some((DOCK, &session.profile_id, &session.page_id))
            )
            .is_err());
        runtime
            .select_device_profile(DOCK, &session.profile_id, Some(&session.page_id), false)
            .unwrap();
        // Remove the helper page before the next format iteration
        runtime
            .delete_page(
                &session.profile_id,
                "other",
                runtime.snapshot().configuration_revision,
            )
            .unwrap();
    }
    runtime.shutdown();
}
#[test]
// Verify that tray revalidates connection profile page and distinguishes page navigation
fn tray_revalidates_connection_profile_page_and_distinguishes_page_navigation() {
    let (_root, runtime) = fixture();
    let state = runtime.snapshot();
    let session = state.session(DOCK);
    let mut profile = state
        .profiles
        .iter()
        .find(|p| p.id == session.profile_id)
        .unwrap()
        .clone();
    profile.pages.push(Page {
        id: "second".into(),
        name: "Second".into(),
        bindings: BTreeMap::new(),
    });
    runtime
        .save_profile(profile.clone(), state.configuration_revision)
        .unwrap();
    let mut other = profile.clone();
    other.id = "other-profile".into();
    runtime
        .save_profile(other, runtime.snapshot().configuration_revision)
        .unwrap();
    runtime
        .state
        .lock()
        .unwrap()
        .sessions
        .get_mut(DOCK)
        .unwrap()
        .automation_enabled = true;
    let epoch = runtime.connection_generation(DOCK).unwrap();
    let selected = runtime
        .select_from_tray(DOCK, epoch, &profile.id, Some("second"))
        .unwrap();
    assert!(!selected.session(DOCK).automation_paused);
    let selected = runtime
        .select_from_tray(DOCK, epoch, "other-profile", None)
        .unwrap();
    assert!(selected.session(DOCK).automation_paused);
    let before = serde_json::to_value(&selected.sessions).unwrap();
    for result in [
        runtime.select_from_tray(DOCK, epoch + 1, &profile.id, None),
        runtime.select_from_tray(DOCK, epoch, "removed", None),
        runtime.select_from_tray(DOCK, epoch, &profile.id, Some("removed")),
    ] {
        assert!(result.is_err());
    }
    assert_eq!(
        serde_json::to_value(&runtime.snapshot().sessions).unwrap(),
        before
    );
    runtime.simulate_device(false, DOCK).unwrap();
    runtime.simulate_device(true, DOCK).unwrap();
    assert!(runtime
        .select_from_tray(DOCK, epoch, &profile.id, None)
        .is_err());
    runtime.shutdown();
}
#[test]
// Verify that native title checks are advisory and include composition clipping and sampled contrast
fn native_title_checks_are_advisory_and_include_composition_clipping_and_sampled_contrast() {
    let (_root, runtime) = fixture();
    let mut draft = binding();
    draft.custom_background = true;
    draft.title = "WIDE TITLE\nSECOND LINE".into();
    draft.appearance = Some(Box::new(hardware_core::appearance::Appearance {
        size: 60,
        foreground: "#000000".into(),
        ..Default::default()
    }));
    let preview = runtime
        .preview_binding_details(DOCK, "key-1", &draft, None)
        .unwrap();
    // Appearance titles now fit their surface and render each line separately
    // The requested size is a ceiling, not a promise of overflowing glyphs
    assert!(!preview.text_warnings.iter().any(|w| w.contains("clipped")));
    assert!(!preview
        .text_warnings
        .iter()
        .any(|w| w.contains("Line breaks")));
    assert_eq!(draft.appearance.as_ref().unwrap().size, 60);
    assert!(preview.text_warnings.iter().any(|w| w.contains("4.5:1")));
    assert!(!preview.image.is_empty());
    let sources = hardware_core::composition::sources(&draft.title, None, None, "", None, false);
    let source = sources.iter().find(|s| s.source.kind == "title").unwrap();
    let mut style = source.style.clone();
    style.size = 70.;
    style.rect = [0., 0., 20., 20.];
    style.foreground = "#000000".into();
    draft.composition = Some(Box::new(hardware_core::composition::DisplayComposition {
        layers: vec![hardware_core::composition::DisplayLayer {
            id: "title".into(),
            label: "Title".into(),
            source: source.source.clone(),
            style,
            text: String::new(),
        }],
    }));
    let preview = runtime
        .preview_binding_details(DOCK, "key-1", &draft, None)
        .unwrap();
    assert!(preview.text_warnings.iter().any(|w| w.contains("clipped")));
    draft.composition = None;
    draft.title = "Hi".into();
    draft.appearance.as_mut().unwrap().size = 18;
    draft.appearance.as_mut().unwrap().foreground = "#ffffff".into();
    assert!(runtime
        .preview_binding_details(DOCK, "key-1", &draft, None)
        .unwrap()
        .text_warnings
        .is_empty());
    runtime.shutdown();
}

#[test]
// Verify that recovery flag and restart request are configuration local and one shot
fn recovery_flag_and_restart_request_are_configuration_local_and_one_shot() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("configuration");
    let runtime =
        Runtime::build_credentials_recovery(config.clone(), false, Default::default(), true)
            .unwrap();
    assert!(runtime.recovery_status().active);
    assert!(!runtime.recovery_status().prompt);
    runtime.shutdown();
    drop(runtime);
    let runtime = Runtime::build(config.clone(), false).unwrap();
    assert!(!runtime.recovery_status().active);
    assert_eq!(runtime.request_restart(true).unwrap(), config);
    runtime.shutdown();
    drop(runtime);
    let runtime = Runtime::build(config.clone(), false).unwrap();
    assert!(runtime.recovery_status().active);
    assert!(!runtime.recovery_status().prompt);
    runtime.shutdown();
    drop(runtime);
    let runtime = Runtime::build(config, false).unwrap();
    assert!(!runtime.recovery_status().active);
    runtime.shutdown();
}

#[test]
// Verify that exit recovery restart overrides the launch flag once and keeps preferences
fn exit_recovery_restart_overrides_the_launch_flag_once_and_keeps_preferences() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("configuration");
    let runtime =
        Runtime::build_credentials_recovery(config.clone(), false, Default::default(), true)
            .unwrap();
    let before = runtime.snapshot().profiles;
    assert!(runtime.recovery_status().active);
    assert!(!runtime.restarted());
    runtime.request_restart(false).unwrap();
    // Requesting a restart does not resume plugins in the running process
    assert!(runtime.recovery_status().active);
    runtime.shutdown();
    drop(runtime);
    let other = Runtime::build(root.path().join("other"), false).unwrap();
    assert!(!other.restarted());
    other.shutdown();
    let runtime =
        Runtime::build_credentials_recovery(config.clone(), false, Default::default(), true)
            .unwrap();
    assert!(runtime.restarted());
    assert!(!runtime.recovery_status().active);
    assert!(!runtime.recovery_status().prompt);
    assert_eq!(
        serde_json::to_value(runtime.snapshot().profiles).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    runtime.shutdown();
    drop(runtime);
    let runtime =
        Runtime::build_credentials_recovery(config, false, Default::default(), true).unwrap();
    assert!(!runtime.restarted());
    assert!(runtime.recovery_status().active);
    runtime.shutdown();
}
