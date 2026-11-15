use stream_dock_manager_hardware_293::Model;
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
