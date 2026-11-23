use stream_dock_manager_hardware_n4::Model;
include!("../../tests/crt_conformance.rs");
#[test]
// Verify source derived codec conformance
fn source_derived_codec_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}

#[test]
// Verify the secondary display zones pair with the matching dial without changing their inputs
fn secondary_strip_pairs_auxiliary_buttons_and_horizontal_swipes() {
    let mut manifest = Model.manifest().clone();
    let panel = manifest.capabilities.touch_panel.as_ref().unwrap();
    assert_eq!(panel.gesture_control, "touch-strip");
    assert_eq!(panel.swipe_directions, ["left", "right"]);
    assert_eq!(
        panel.button_controls,
        ["aux-key-1", "aux-key-2", "aux-key-3", "aux-key-4"]
    );
    assert!(!manifest.capabilities.background);
    let mut session = Model.session();
    for (index, id) in panel.button_controls.iter().enumerate() {
        let dial = manifest.control(&format!("dial-{}", index + 1)).unwrap();
        assert_eq!(dial.feedback_surface.as_deref(), Some(id.as_str()));
        let zone = manifest.control(id).unwrap();
        assert_eq!(zone.kind, "button");
        assert_eq!(zone.image_slot, Some(index as u8 + 1));
        assert_eq!(zone.semantics, ["press", "release"]);
        let code = 0x40 + index as u8;
        for (state, pressed) in [(1, true), (0, false)] {
            let events = session.decode(&input(0, code, state)).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(&events[0].control_id, id);
            assert_eq!(events[0].input, Input::Button { pressed });
        }
    }
    for code in [0x00, 0xb1, 0xb2] {
        assert!(session.decode(&input(0, code, 0)).unwrap().is_empty());
    }
    let zone = manifest
        .controls
        .iter_mut()
        .find(|zone| zone.id == "aux-key-1")
        .unwrap();
    zone.semantics.retain(|semantic| semantic != "release");
    assert!(manifest.validate().is_err());
}
