// Check n1 layout
fn check_n1_layout(module: &dyn HardwareModule, strip_pixels: u16) {
    let manifest = module.manifest();
    assert_eq!(manifest.controls.len(), 21);
    assert!(manifest.layout.height > manifest.layout.width);
    let first = manifest.control("key-1").unwrap();
    let third = manifest.control("key-3").unwrap();
    let bottom = manifest.control("key-13").unwrap();
    assert_eq!(first.y, third.y);
    assert_eq!(first.x, bottom.x);
    assert!(bottom.y > first.y);
    let knob = manifest.control("dial-1").unwrap();
    let knob_display = manifest.control("display-3").unwrap();
    assert_eq!(knob.feedback_surface.as_deref(), Some("display-3"));
    assert!(knob.y + knob.height <= knob_display.y);
    assert!(knob_display.y + knob_display.height <= first.y);
    let mut session = module.session();
    for (index, code) in [(1, 30), (2, 31)] {
        let input_id = format!("key-{}", 15 + index);
        let output_id = format!("display-{index}");
        let button = manifest.control(&input_id).unwrap();
        let display = manifest.control(&output_id).unwrap();
        assert_eq!(button.kind, "button");
        assert!(button.image_slot.is_none());
        assert_eq!(button.feedback_surface.as_deref(), Some(output_id.as_str()));
        assert!(button.x + button.width <= knob.x);
        assert_eq!(button.y * 2 + button.height, knob.y * 2 + knob.height);
        assert_eq!(display.kind, "display");
        assert_eq!(display.semantics, ["display"]);
        assert_eq!(display.image_slot, Some(15 + index));
        assert_eq!(display.image_width, Some(strip_pixels));
        assert_eq!(display.image_height, Some(strip_pixels));
        assert_eq!(display.y, knob_display.y);
        assert_eq!(display.height, knob_display.height);
        assert_eq!(display.x, first.x + display.width * u16::from(index - 1));
        for state in [0, 1] {
            assert_eq!(
                session.decode(&input(0, code, state)).unwrap(),
                vec![ControlEvent {
                    control_id: input_id.clone(),
                    input: Input::Button {
                        pressed: state == 1
                    },
                }]
            );
        }
        assert!(session
            .encode(DeviceCommand::Image {
                control_id: input_id,
                data: hardware_core::render::encode_pixels(
                    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                        8,
                        8,
                        image::Rgb([23, 79, 143])
                    )),
                    "png"
                )
                .unwrap(),
            })
            .is_err());
    }
    assert_eq!(knob_display.image_slot, Some(18));
    assert_eq!(knob_display.image_width, Some(strip_pixels));
    let left = manifest.control("display-1").unwrap();
    assert_eq!(left.x, first.x);
    assert_eq!(knob_display.x + knob_display.width, third.x + third.width);
    assert!(knob.x >= knob_display.x);
}
