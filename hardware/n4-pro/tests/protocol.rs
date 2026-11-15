use hardware_core::*;
use stream_dock_manager_hardware_n4_pro::N4Pro;
#[test]
// Verify keyboard configuration is not a generic lighting command
fn keyboard_configuration_is_not_a_generic_lighting_command() {
    let mut session = N4Pro.session();
    assert!(matches!(
        session.encode(DeviceCommand::Keyboard(KeyboardSettings {
            brightness: Some(6),
            ..Default::default()
        })),
        Err(Error::Unsupported(_))
    ));
}

#[test]
// Verify reviewed vibration configuration uses vendor field order and never invents defaults
fn reviewed_vibration_configuration_uses_vendor_field_order_and_never_invents_defaults() {
    let fields = [
        "led_follow_key_light",
        "key_light_on_disconnect",
        "check_usb_power",
        "reset_usb_report",
        "enable_boot_video",
    ];
    let mut session = N4Pro.session();
    for bits in 0u8..64 {
        let chosen = [0, 1, 2, 4, 5].map(|bit| bits & (1 << bit) != 0);
        let configuration = fields
            .iter()
            .zip(chosen)
            .map(|(key, value)| ((*key).to_string(), value))
            .collect();
        let transaction = session
            .encode(DeviceCommand::Vibration {
                enabled: bits & 8 != 0,
                configuration,
            })
            .unwrap();
        let mut expected = vec![0, b'C', b'R', b'T', 0, 0, b'Q', b'U', b'C', b'M', b'D'];
        expected.extend((0..6).map(|bit| if bits & (1 << bit) != 0 { 0x11 } else { 0xff }));
        expected.resize(1025, 0);
        assert_eq!(transaction.reports, vec![expected]);
        assert_eq!(
            transaction.coalesce_key.as_deref(),
            Some("device-configuration")
        );
    }
    for configuration in [
        Default::default(),
        std::collections::BTreeMap::from([("unknown".into(), true)]),
    ] {
        assert!(session
            .encode(DeviceCommand::Vibration {
                enabled: true,
                configuration
            })
            .is_err());
    }
}
// Construct the input test fixture
fn input(code: u8, state: u8) -> Vec<u8> {
    let mut b = vec![0; 512];
    b[..3].copy_from_slice(b"ACK");
    b[5..7].copy_from_slice(b"OK");
    b[9] = code;
    b[10] = state;
    b
}
#[test]
// Verify manifest identity mapping and semantics
fn manifest_identity_mapping_and_semantics() {
    let m = N4Pro.manifest();
    m.validate().unwrap();
    assert_eq!(m.controls.iter().filter(|c| c.kind == "button").count(), 10);
    assert_eq!(m.controls.iter().filter(|c| c.kind == "encoder").count(), 4);
    assert_eq!(m.capabilities.media_formats, ["png", "jpeg", "gif", "mp4"]);
    assert_eq!(m.capabilities.image_formats, ["png", "jpeg"]);
    assert_eq!(m.control("key-1").unwrap().image_slot, Some(11));
    assert_eq!(m.control("key-10").unwrap().image_slot, Some(10));
    let selected = &m.variants[0].identity;
    let mut wrong = HidIdentity {
        vendor_id: selected.vendor_id,
        product_id: selected.product_id,
        interface: selected.interface.unwrap(),
        usage_page: selected.usage_page.unwrap(),
        usage: selected.usage,
    };
    wrong.interface = 1;
    assert!(!N4Pro.matches(&wrong));
}
#[test]
// Verify every key edge and optional report ID
fn every_key_edge_and_optional_report_id() {
    let mut session = N4Pro.session();
    for key in 1..=10 {
        for (state, pressed) in [(1, true), (0, false), (2, false)] {
            let report = input(key, state);
            let mut prefixed = vec![0];
            prefixed.extend(&report);
            for r in [&report, &prefixed] {
                assert_eq!(
                    session.decode(r).unwrap(),
                    vec![ControlEvent {
                        control_id: format!("key-{key}"),
                        input: Input::Button { pressed }
                    }]
                );
            }
        }
    }
}
#[test]
// Verify encoders are pulses and rotations
fn encoders_are_pulses_and_rotations() {
    let mut session = N4Pro.session();
    for (i, code) in [0x37, 0x35, 0x33, 0x36].iter().enumerate() {
        assert_eq!(
            session.decode(&input(*code, 1)).unwrap()[0],
            ControlEvent {
                control_id: format!("dial-{}", i + 1),
                input: Input::Pulse
            }
        );
        assert!(session.decode(&input(*code, 0)).unwrap().is_empty());
    }
    for (i, (left, right)) in [(0xa0, 0xa1), (0x50, 0x51), (0x90, 0x91), (0x70, 0x71)]
        .iter()
        .enumerate()
    {
        for (code, delta) in [(*left, -1), (*right, 1)] {
            assert_eq!(
                session.decode(&input(code, 1)).unwrap()[0],
                ControlEvent {
                    control_id: format!("dial-{}", i + 1),
                    input: Input::Rotate { delta }
                }
            );
        }
    }
}
#[test]
// Verify image transfer and black lighting quirk
fn image_transfer_and_black_lighting_quirk() {
    let mut session = N4Pro.session();
    let t = session
        .encode(DeviceCommand::Image {
            control_id: "key-1".into(),
            data: vec![99; 1025],
        })
        .unwrap();
    assert_eq!(&t.reports[0][6..9], b"BAT");
    assert_eq!(&t.reports[0][9..13], &1025u32.to_be_bytes());
    assert_eq!(t.reports[0][13], 11);
    assert_eq!(t.reports.len(), 4);
    let t = session
        .encode(DeviceCommand::Lighting {
            brightness: 50,
            colors: vec![[0; 3]; 4],
        })
        .unwrap();
    assert_eq!(t.reports.len(), 1);
    assert_eq!(&t.reports[0][6..12], b"LBLIG\0");
}
#[test]
// Verify malformed inputs do not panic
fn malformed_inputs_do_not_panic() {
    for len in 0..32 {
        let mut b = vec![0; len];
        if len >= 3 {
            b[..3].copy_from_slice(b"ACK");
        }
        let _ = N4Pro.session().decode(&b);
    }
    let mut b = input(0x40, 1);
    b.truncate(11);
    assert!(N4Pro.session().decode(&b).is_err());
}
#[test]
// Verify synthetic fixture replay
fn synthetic_fixture_replay() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/inputs.synthetic.json")).unwrap();
    assert_eq!(fixture["method"], "simulated");
    for row in fixture["reports"].as_array().unwrap() {
        let report = input(
            row["code"].as_u64().unwrap() as u8,
            row["state"].as_u64().unwrap() as u8,
        );
        assert_eq!(
            N4Pro.session().decode(&report).unwrap()[0].control_id,
            row["control"]
        );
    }
}
#[test]
// Verify observed swipes follow physical coordinate direction
fn observed_swipes_follow_physical_coordinate_direction() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/swipes.observed-replay.json")).unwrap();
    for gesture in fixture["gestures"].as_array().unwrap() {
        let coordinates = gesture["coordinates"].as_array().unwrap();
        let first_x = coordinates.first().unwrap()["x"].as_u64().unwrap();
        let last_x = coordinates.last().unwrap()["x"].as_u64().unwrap();
        assert_ne!(first_x, last_x);
        let physical_direction = if last_x > first_x { "right" } else { "left" };
        assert_eq!(gesture["physical_direction"], physical_direction);
        // Reconstructed reports replay observed decoded fields, not captured raw HID bytes
        for report_id in [false, true] {
            let mut session = N4Pro.session();
            let mut decode = |mut report: Vec<u8>| {
                if report_id {
                    report.insert(0, 0);
                }
                session.decode(&report).unwrap()
            };
            for coordinate in coordinates {
                let phase = coordinate["phase"].as_str().unwrap();
                let x = coordinate["x"].as_u64().unwrap() as u16;
                let y = coordinate["y"].as_u64().unwrap() as u16;
                let mut report = input(0, 1);
                if phase == "move" {
                    report[4..7].copy_from_slice(b"ARX");
                    report[10..12].copy_from_slice(&x.to_be_bytes());
                    report[12..14].copy_from_slice(&y.to_be_bytes());
                } else {
                    assert_eq!(phase, "start");
                    assert_eq!(y, 0);
                    report[11..13].copy_from_slice(&x.to_be_bytes());
                }
                assert_eq!(
                    decode(report),
                    vec![ControlEvent {
                        control_id: "touch-strip".into(),
                        input: Input::Touch {
                            phase: phase.into(),
                            x,
                            y,
                        },
                    }]
                );
            }
            let code = gesture["inferred_swipe_code"].as_u64().unwrap() as u8;
            assert_eq!(
                decode(input(code, 1)),
                vec![ControlEvent {
                    control_id: "touch-strip".into(),
                    input: Input::Swipe {
                        direction: physical_direction.into(),
                    },
                }]
            );
        }
    }
}
#[test]
// Verify captured swipes use motion only on explicit terminals
fn captured_swipes_use_motion_only_on_explicit_terminals() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/swipes.captured.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 5);
    for report_id in [false, true] {
        let mut session = N4Pro.session();
        for case in cases {
            let mut swipes = vec![];
            for row in case["reports"].as_array().unwrap() {
                let prefix = row["prefix_hex"].as_str().unwrap();
                let mut bytes: Vec<u8> = (0..prefix.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&prefix[i..i + 2], 16).unwrap())
                    .collect();
                let length = row["length"].as_u64().unwrap() as usize;
                assert_eq!(
                    bytes.len() + row["zero_padding_bytes"].as_u64().unwrap() as usize,
                    length
                );
                bytes.resize(length, 0);
                if report_id {
                    bytes.insert(0, 0);
                }
                for event in session.decode(&bytes).unwrap() {
                    if let Input::Swipe { direction } = event.input {
                        assert_eq!(event.control_id, "touch-strip");
                        assert_eq!(
                            row["capture_sequence"], case["capture_sequence_end"],
                            "Movement alone must not synthesize a swipe"
                        );
                        swipes.push(direction);
                    }
                }
            }
            let expected: Vec<String> = case["expected_direction"]
                .as_str()
                .map(String::from)
                .into_iter()
                .collect();
            assert_eq!(swipes, expected, "{}", case["name"]);
        }
    }
}

#[test]
// Verify captured vertical and horizontal gestures replay once
fn captured_vertical_and_horizontal_gestures_replay_once() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/vertical-swipes.captured.json")).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 14);
    for report_id in [false, true] {
        let mut session = N4Pro.session();
        for case in fixture["cases"].as_array().unwrap() {
            let mut swipes = vec![];
            for row in case["reports"].as_array().unwrap() {
                let prefix = row["prefix_hex"].as_str().unwrap();
                let mut bytes: Vec<u8> = (0..prefix.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&prefix[i..i + 2], 16).unwrap())
                    .collect();
                assert_eq!(
                    bytes.len() + row["zero_padding_bytes"].as_u64().unwrap() as usize,
                    row["length"].as_u64().unwrap() as usize
                );
                bytes.resize(row["length"].as_u64().unwrap() as usize, 0);
                if report_id {
                    bytes.insert(0, 0);
                }
                for event in session.decode(&bytes).unwrap() {
                    if let Input::Swipe { direction } = event.input {
                        assert_eq!(event.control_id, "touch-strip");
                        assert_eq!(
                            row["capture_sequence"], case["capture_sequence_end"],
                            "Only an explicit terminal completes a swipe"
                        );
                        swipes.push(direction);
                    }
                }
            }
            assert_eq!(
                swipes,
                vec![case["expected_direction"].as_str().unwrap()],
                "{}",
                case["name"]
            );
        }
    }
}

// Touch start
fn touch_start(x: u16) -> Vec<u8> {
    let mut bytes = input(0, 1);
    bytes[11..13].copy_from_slice(&x.to_be_bytes());
    bytes
}
// Touch move
fn touch_move(x: u16, y: u16) -> Vec<u8> {
    let mut bytes = input(0, 1);
    bytes[4..7].copy_from_slice(b"ARX");
    bytes[10..12].copy_from_slice(&x.to_be_bytes());
    bytes[12..14].copy_from_slice(&y.to_be_bytes());
    bytes
}
#[test]
// Verify swipe without usable trajectory fails closed
fn swipe_without_usable_trajectory_fails_closed() {
    for code in [0x38, 0x39] {
        let mut session = N4Pro.session();
        assert!(session.decode(&input(code, 0)).unwrap().is_empty());
        session.decode(&touch_move(600, 50)).unwrap();
        assert!(session.decode(&input(code, 0)).unwrap().is_empty());
        session.decode(&touch_start(100)).unwrap();
        session.decode(&touch_move(100, 50)).unwrap();
        assert!(session.decode(&input(code, 0)).unwrap().is_empty());
        session.decode(&touch_start(700)).unwrap();
        session.decode(&touch_move(100, 50)).unwrap();
        assert_eq!(
            session.decode(&input(code, 0)).unwrap()[0].input,
            Input::Swipe {
                direction: "left".into()
            }
        );
        assert!(session.decode(&input(code, 0)).unwrap().is_empty());
    }
}
#[test]
// Verify tap end new start and invalid reports discard previous motion
fn tap_end_new_start_and_invalid_reports_discard_previous_motion() {
    let resets = [
        touch_start(700),
        input(0x40, 0),
        input(0x41, 0),
        input(0x42, 0),
        input(0x43, 0),
        b"ACK\0ARX".to_vec(),
        b"ACK\0\0OK".to_vec(),
        touch_start(100)[..12].to_vec(),
        touch_start(800),
        touch_move(800, 0),
        touch_move(300, 480),
        b"invalid".to_vec(),
        input(0xff, 0),
    ];
    for reset in resets {
        let mut session = N4Pro.session();
        session.decode(&touch_start(700)).unwrap();
        session.decode(&touch_move(100, 40)).unwrap();
        if let Ok(events) = session.decode(&reset) {
            assert!(events
                .iter()
                .all(|event| !matches!(event.input, Input::Swipe { .. })));
        }
        assert!(session.decode(&input(0x38, 0)).unwrap().is_empty());
        session.decode(&touch_start(100)).unwrap();
        session.decode(&touch_move(700, 40)).unwrap();
        assert_eq!(
            session.decode(&input(0x39, 0)).unwrap()[0].input,
            Input::Swipe {
                direction: "right".into()
            }
        );
    }
    for command in [DeviceCommand::Sleep, DeviceCommand::Clear] {
        let mut session = N4Pro.session();
        session.decode(&touch_start(700)).unwrap();
        session.decode(&touch_move(100, 40)).unwrap();
        session.encode(command).unwrap();
        assert!(session.decode(&input(0x38, 0)).unwrap().is_empty());
    }
    let mut session = N4Pro.session();
    session.decode(&touch_start(700)).unwrap();
    session.decode(&touch_move(100, 40)).unwrap();
    session.initialize().unwrap();
    assert!(session.decode(&input(0x38, 0)).unwrap().is_empty());
}
#[test]
// Verify touch regions coordinates and background rectangles
fn touch_regions_coordinates_and_background_rectangles() {
    let mut session = N4Pro.session();
    for region in 0..4 {
        let mut report = input(0x40 + region, 1);
        report[11..13].copy_from_slice(&523u16.to_be_bytes());
        let events = session.decode(&report).unwrap();
        assert_eq!(
            events[0],
            ControlEvent {
                control_id: format!("touch-{}", region + 1),
                input: Input::Touch {
                    phase: "end".into(),
                    x: 523,
                    y: 0
                }
            }
        );
    }
    let mut coordinate = input(0, 1);
    coordinate[4..7].copy_from_slice(b"ARX");
    coordinate[10..12].copy_from_slice(&420u16.to_be_bytes());
    coordinate[12..14].copy_from_slice(&72u16.to_be_bytes());
    assert_eq!(
        session.decode(&coordinate).unwrap()[0].input,
        Input::Touch {
            phase: "move".into(),
            x: 420,
            y: 72
        }
    );
    let transaction = session
        .encode(DeviceCommand::Background {
            data: vec![7; 32],
            x: 100,
            y: 50,
            width: 200,
            height: 100,
        })
        .unwrap();
    assert_eq!(&transaction.reports[0][6..11], b"BGPIC");
    assert_eq!(&transaction.reports[0][15..17], &500u16.to_be_bytes());
    assert_eq!(&transaction.reports[0][17..19], &330u16.to_be_bytes());
    assert_eq!(transaction.reports.len(), 2);
    assert!(session
        .encode(DeviceCommand::Background {
            data: vec![7],
            x: 799,
            y: 0,
            width: 2,
            height: 1
        })
        .is_err());
}
