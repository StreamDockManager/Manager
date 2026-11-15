use stream_dock_manager_hardware_n3::Model;
include!("../../tests/crt_conformance.rs");
#[test]
// Verify source derived codec conformance
fn source_derived_codec_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}

#[test]
// Verify plain keys preserve IDs and encoders expose edges
fn plain_keys_preserve_ids_and_encoders_expose_edges() {
    let manifest = Model.manifest();
    assert_eq!(
        manifest
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some())
            .count(),
        6
    );
    for index in 1..=9 {
        let control = manifest
            .controls
            .iter()
            .find(|c| c.id == format!("key-{index}"))
            .unwrap();
        assert_eq!(control.kind, "button");
        assert_eq!(control.semantics, ["press", "release"]);
        assert_eq!(control.image_slot.is_some(), index <= 6);
    }
    for control in manifest.controls.iter().filter(|c| c.kind == "encoder") {
        assert_eq!(control.semantics, ["press", "release", "pulse", "rotate"]);
    }
    assert!(manifest.capabilities.reliable_encoder_release);
}

#[test]
// Verify variant resolution selects legacy geometry and boot encoding
fn variant_resolution_selects_legacy_geometry_and_boot_encoding() {
    for variant in &Model.manifest().variants {
        let contract = Model.resolve_contract(&variant.id, None).unwrap();
        let legacy = variant.identity.vendor_id == 0x6602;
        assert_eq!(
            contract.manifest.controls[0].image_width,
            Some(if legacy { 60 } else { 64 })
        );
        assert_eq!(
            contract.key_transform.unwrap().rotation,
            if legacy { 0 } else { 90 }
        );
        assert_eq!(
            contract.boot_encoding,
            Some(if legacy {
                WireImageEncoding::Rgb565Le
            } else {
                WireImageEncoding::Jpeg
            })
        );
        assert_eq!(contract.boot_transform.unwrap().rotation, 90);
        assert!(contract.image_output_disabled_reason.is_none());
        assert!(variant.available);
    }
    assert!(Model.resolve_contract("unknown", None).is_err());
    assert!(Model
        .manifest()
        .variants
        .iter()
        .any(|v| v.identity.vendor_id == 0x6602 && v.identity.product_id == 0x1000));
}

#[test]
// Verify resolved boot upload uses variant encoding and exact source canvas
fn resolved_boot_upload_uses_variant_encoding_and_exact_source_canvas() {
    let source = hardware_core::render::encode_pixels(
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            320,
            240,
            image::Rgb([255, 0, 0]),
        )),
        "png",
    )
    .unwrap();
    let swapped = hardware_core::render::encode_pixels(
        image::DynamicImage::ImageRgb8(image::RgbImage::new(240, 320)),
        "png",
    )
    .unwrap();
    for (variant, legacy) in [("usb-6602-1000", true), ("usb-6603-1002", false)] {
        let contract = Model.resolve_contract(variant, None).unwrap();
        let mut session = Model.session_for(&contract).unwrap();
        assert!(session
            .encode(DeviceCommand::BootLogo {
                data: swapped.clone()
            })
            .is_err());
        let tx = session
            .encode(DeviceCommand::BootLogo {
                data: source.clone(),
            })
            .unwrap();
        assert_eq!(&tx.reports[0][6..9], b"LOG");
        let length = u32::from_be_bytes(tx.reports[0][9..13].try_into().unwrap()) as usize;
        let bytes: Vec<_> = tx
            .reports
            .iter()
            .skip(1)
            .flat_map(|r| r[1..].iter().copied())
            .take(length)
            .collect();
        if legacy {
            assert_eq!(length, 320 * 240 * 2);
            assert!(bytes
                .as_chunks::<2>()
                .0
                .iter()
                .all(|pixel| pixel == &[0x00, 0xf8]));
        } else {
            assert_eq!(
                image::guess_format(&bytes).unwrap(),
                image::ImageFormat::Jpeg
            );
            let decoded = image::load_from_memory(&bytes).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (240, 320));
        }
    }
}
