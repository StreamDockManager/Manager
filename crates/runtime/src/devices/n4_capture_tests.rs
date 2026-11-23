//! Pure N4 render/output regression without HID or the running application
use super::*;

// Reassemble BAT payload bytes from captured HIDAPI reports using the declared byte length
fn payload(reports: &[Vec<u8>], slot: u8) -> Vec<u8> {
    let index = reports
        .iter()
        .position(|r| r.get(6..9) == Some(b"BAT") && r.get(13) == Some(&slot))
        .expect("BAT slot missing");
    let len = u32::from_be_bytes(reports[index][9..13].try_into().unwrap()) as usize;
    let bytes: Vec<_> = reports[index + 1..]
        .iter()
        .flat_map(|r| r[1..].iter().copied())
        .take(len)
        .collect();
    assert_eq!(bytes.len(), len);
    bytes
}

#[test]
fn populated_n4_render_and_transparent_payload_survive_recorded_transport() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    crate::render_scene::install_burst_fixture(&runtime);
    let (mut c, reports) = super::frame_policy_tests::connection_for_model(&runtime, "n4-pro");
    let context = runtime.render_scene(&c.device_id).unwrap();
    let control = c.manifest.control("key-1").unwrap().clone();
    assert_eq!(
        (
            control.image_width,
            control.image_height,
            c.manifest.layout.rotation
        ),
        (Some(112), Some(112), 180)
    );
    let binding = context
        .page
        .as_ref()
        .unwrap()
        .bindings
        .get("key-1")
        .unwrap();
    let rendered = runtime
        .render_bound_surface_in_scene(
            &control,
            Some(binding),
            180,
            &context.scope,
            "key-1",
            &context,
        )
        .unwrap();
    let expected = image::load_from_memory(&rendered).unwrap().to_rgba8();
    assert!(expected.pixels().any(|p| p[3] != 0));
    assert!(
        expected.pixels().any(|p| p != expected.get_pixel(0, 0)),
        "Fixture label must be visible"
    );
    for encoding in [
        FrameEncoding::IndividualUpdates,
        FrameEncoding::GroupedKeyUpdates,
    ] {
        c.encoding = encoding;
        c.delivery.clear();
        reports.lock().unwrap().clear();
        send_surfaces_resolved(
            &mut c,
            vec![DeviceCommand::Image {
                control_id: "key-1".into(),
                data: rendered.to_vec(),
            }],
            OutputPriority::Normal,
            false,
        )
        .unwrap();
        assert!(c.worker.wait_idle(Duration::from_secs(5)));
        c.collect_outcomes(&runtime);
        let captured = reports.lock().unwrap();
        assert!(captured.iter().all(|r| r.len() == 1025 && r[0] == 0));
        assert!(captured.iter().any(|r| r.get(6..9) == Some(b"STP")));
        let wire = payload(&captured, control.image_slot.unwrap());
        let decoded = image::load_from_memory(&wire).unwrap().to_rgba8();
        assert_eq!(
            decoded, expected,
            "Renderer owns rotation; N4 wire must not rotate twice"
        );
    }
    // Asymmetric transparent pixels represent an authored plugin overlay, not a live plugin
    let rgba = image::RgbaImage::from_fn(112, 112, |x, y| {
        image::Rgba([x as u8, y as u8, 230, if x < 40 { 0 } else { 255 }])
    });
    let transparent =
        hardware_core::render::encode_pixels(image::DynamicImage::ImageRgba8(rgba.clone()), "png")
            .unwrap();
    c.delivery.clear();
    reports.lock().unwrap().clear();
    send_surfaces_resolved(
        &mut c,
        vec![DeviceCommand::Image {
            control_id: "key-1".into(),
            data: transparent,
        }],
        OutputPriority::Normal,
        false,
    )
    .unwrap();
    assert!(c.worker.wait_idle(Duration::from_secs(5)));
    let decoded = image::load_from_memory(&payload(
        &reports.lock().unwrap(),
        control.image_slot.unwrap(),
    ))
    .unwrap()
    .to_rgba8();
    assert_eq!(decoded, rgba);
    drop(c);
    runtime.shutdown();
}

// Exercise startup and full scene restoration rather than only an isolated image transfer
#[test]
fn n4_startup_restore_records_populated_keys_after_wallpaper() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().join("app"), false).unwrap();
    crate::render_scene::install_burst_fixture(&runtime);
    let (mut c, reports) = super::frame_policy_tests::connection_for_model(&runtime, "n4-pro");
    initialize_output(c.session.lock().unwrap().as_mut(), &c.worker).unwrap();
    restore(&runtime, &mut c);
    super::tests::finish_transition(&runtime, &mut c);
    assert!(c.worker.wait_idle(Duration::from_secs(5)));
    c.collect_outcomes(&runtime);
    let captured = reports.lock().unwrap();
    let bg = captured
        .iter()
        .position(|r| r.get(6..11) == Some(b"BGPIC"))
        .expect("wallpaper");
    for slot in [11, 12, 13, 14, 15, 6, 7, 8, 9, 10] {
        let key = captured
            .iter()
            .position(|r| r.get(6..9) == Some(b"BAT") && r.get(13) == Some(&slot))
            .expect("key upload");
        assert!(key > bg, "LCD follows wallpaper");
        let rgba = image::load_from_memory(&payload(&captured, slot))
            .unwrap()
            .to_rgba8();
        assert_eq!(rgba.dimensions(), (112, 112));
        assert!(rgba.pixels().any(|p| p[3] > 0));
        assert!(
            rgba.pixels().any(|p| p != rgba.get_pixel(0, 0)),
            "visible fixture label"
        );
    }
    let last_key = captured
        .iter()
        .rposition(|r| r.get(6..9) == Some(b"BAT"))
        .unwrap();
    assert!(!captured[last_key + 1..]
        .iter()
        .any(|r| r.get(6..9) == Some(b"CLE") || r.get(6..11) == Some(b"BGPIC")));
    eprintln!(
        "N4 startup recorder reports={} background_before_all_10_populated_keys=true later_clear_or_background=false",
        captured.len()
    );
    drop(captured);
    drop(c);
    runtime.shutdown();
}
