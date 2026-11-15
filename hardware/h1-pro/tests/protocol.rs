use stream_dock_manager_hardware_h1_pro::Model;
include!("../../tests/crt_conformance.rs");
#[test]
// Verify source derived codec conformance
fn source_derived_codec_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}

#[test]
// Verify firmware geometry is recognized or fails closed
fn firmware_geometry_is_recognized_or_fails_closed() {
    for variant in &Model.manifest().variants {
        for firmware in ["V25.H1 Pro.01.000", "V3.H1Pro.00.001"] {
            let contract = Model.resolve_contract(&variant.id, Some(firmware)).unwrap();
            assert!(contract.image_output_disabled_reason.is_none());
            assert_eq!(contract.key_transform.unwrap().rotation, 270);
            assert!(contract
                .manifest
                .controls
                .iter()
                .filter(|c| c.image_slot.is_some())
                .all(|c| c.image_width == Some(64) && c.image_height == Some(64)));
            assert!(contract.manifest.capabilities.boot_logo.is_none());
        }
        for firmware in [
            None,
            Some(""),
            Some("unknown"),
            Some("V2"),
            Some("V4"),
            Some("V30.H1.01.001"),
            Some("V4.H1Pro.V3.00.001"),
            Some("V3.Other.H1.00.001"),
            Some("V3.H1Pro.00.001.extra"),
            Some("V3.H1Pro.invalid"),
            Some("V25.N3.01.001"),
            Some("H1-V300-1.0"),
        ] {
            let contract = Model.resolve_contract(&variant.id, firmware).unwrap();
            assert!(contract.image_output_disabled_reason.is_some());
            // Artwork IDs remain visible even when unsafe device output is disabled
            assert_eq!(
                serde_json::to_value(&contract.manifest.controls).unwrap(),
                serde_json::to_value(&Model.manifest().controls).unwrap()
            );
        }
    }
}

#[test]
// Verify unknown firmware cannot write images but still decodes keys
fn unknown_firmware_cannot_write_images_but_still_decodes_keys() {
    let variant = &Model.manifest().variants[0].id;
    let contract = Model.resolve_contract(variant, None).unwrap();
    let mut session = Model.session_for(&contract).unwrap();
    assert!(session
        .encode(DeviceCommand::Image {
            control_id: "key-1".into(),
            data: vec![]
        })
        .is_err());
    let report = input(0, 1, 1);
    assert_eq!(
        session.decode(&report).unwrap(),
        [ControlEvent {
            control_id: "key-1".into(),
            input: Input::Button { pressed: true }
        }]
    );
}
