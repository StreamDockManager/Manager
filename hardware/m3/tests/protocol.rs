use stream_dock_manager_hardware_m3::Model;
include!("../../tests/crt_conformance.rs");
#[test]
// Verify source derived codec conformance
fn source_derived_codec_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}

#[test]
// Verify output slots follow logical keys without relabeling controls
fn output_slots_follow_logical_keys_without_relabeling_controls() {
    let manifest = Model.manifest();
    for index in 1..=15 {
        let control = manifest
            .controls
            .iter()
            .find(|c| c.id == format!("key-{index}"))
            .unwrap();
        assert_eq!(control.image_slot, Some(index));
    }
}

#[test]
// Verify dial layout preserves raw control associations
fn dial_layout_preserves_raw_control_associations() {
    let manifest = Model.manifest();
    let dials: Vec<_> = (1..=3)
        .map(|i| {
            manifest
                .controls
                .iter()
                .find(|c| c.id == format!("dial-{i}"))
                .unwrap()
        })
        .collect();
    assert_eq!(
        dials.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(),
        ["Top dial", "Middle dial", "Bottom dial"]
    );
    assert!(dials[0].y < dials[1].y && dials[1].y < dials[2].y);
    assert!(dials.iter().all(|c| c.x == dials[0].x));
}

#[test]
// Verify host JPEG choice uses animated JPEG budget
fn host_jpeg_choice_uses_animated_jpeg_budget() {
    let control = Model.manifest().control("key-1").unwrap();
    let size = u32::from(control.image_width.unwrap());
    let pixels = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(size, size, |x, y| {
        let n = (x.wrapping_mul(1664525) ^ y.wrapping_mul(1013904223)).wrapping_mul(2654435761);
        image::Rgb([n as u8, (n >> 8) as u8, (n >> 16) as u8])
    }));
    let mut source = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut source, 100)
        .encode_image(&pixels)
        .unwrap();
    let uploads = Model
        .session()
        .encode_frame_with_purpose(
            vec![DeviceCommand::Image {
                control_id: "key-1".into(),
                data: source,
            }],
            FrameEncoding::IndividualUpdates,
            ImagePurpose::Animated,
        )
        .unwrap();
    let tx = &uploads[0].transaction;
    let length = u32::from_be_bytes(tx.reports[0][9..13].try_into().unwrap()) as usize;
    let bytes: Vec<u8> = tx.reports[1..tx.reports.len() - 1]
        .iter()
        .flat_map(|r| r[1..].iter().copied())
        .take(length)
        .collect();
    assert_eq!(
        image::guess_format(&bytes).unwrap(),
        image::ImageFormat::Jpeg
    );
    assert!(length <= 3000);
}
