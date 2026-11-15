use stream_dock_manager_hardware_293s::Model;
include!("../../tests/crt_conformance.rs");
#[test]
// Verify source derived codec conformance
fn source_derived_codec_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}

#[test]
// Verify secondary displays preserve artwork IDs without inputs
fn secondary_displays_preserve_artwork_ids_without_inputs() {
    let manifest = Model.manifest();
    for index in 16..=18 {
        let control = manifest
            .controls
            .iter()
            .find(|c| c.id == format!("key-{index}"))
            .unwrap();
        assert_eq!(control.kind, "display");
        assert_eq!(control.semantics, ["display"]);
        assert_eq!(control.image_slot, Some(index));
        assert_eq!(control.image_width, Some(80));
    }
    let expected = [13, 10, 7, 4, 1, 14, 11, 8, 5, 2, 15, 12, 9, 6, 3];
    for (index, slot) in expected.into_iter().enumerate() {
        let control = manifest
            .controls
            .iter()
            .find(|c| c.id == format!("key-{}", index + 1))
            .unwrap();
        assert_eq!(control.image_slot, Some(slot));
    }
}
