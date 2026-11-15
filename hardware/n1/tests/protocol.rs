use stream_dock_manager_hardware_n1::Model;
include!("../../tests/crt_conformance.rs");
include!("../../tests/n1_layout.rs");
#[test]
// Verify source derived codec conformance
fn source_derived_codec_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}
#[test]
// Verify plain buttons and display strip have separate ownership
fn plain_buttons_and_display_strip_have_separate_ownership() {
    check_n1_layout(&Model, 80);
}
