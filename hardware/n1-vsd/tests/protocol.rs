use stream_dock_manager_hardware_n1_vsd::Model;
include!("../../tests/crt_conformance.rs");
include!("../../tests/n1_layout.rs");

#[test]
// Verify reviewed n1 protocol conformance
fn reviewed_n1_protocol_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}

#[test]
// Verify n1 uses only its exact vendor interface
fn n1_uses_only_its_exact_vendor_interface() {
    let vendor = HidIdentity {
        vendor_id: 0x5548,
        product_id: 0x1002,
        interface: 0,
        usage_page: 0xffa0,
        usage: 1,
    };
    assert!(Model.matches(&vendor));
    for foreign in [
        HidIdentity {
            interface: 1,
            usage_page: 1,
            usage: 6,
            ..vendor.clone()
        },
        HidIdentity {
            interface: 1,
            ..vendor.clone()
        },
        HidIdentity {
            usage_page: 0xffa1,
            ..vendor.clone()
        },
        HidIdentity {
            product_id: 0x1001,
            ..vendor.clone()
        },
        HidIdentity {
            vendor_id: 0x6603,
            ..vendor
        },
    ] {
        assert!(!Model.matches(&foreign));
    }
}

#[test]
// Verify connection initialization retains software mode and initial brightness
fn connection_initialization_retains_software_mode_and_initial_brightness() {
    let mut session = Model.session();
    let initialized = session.initialize().unwrap();
    assert_eq!(initialized.len(), 3);
    let setup = &initialized[0].reports;
    assert_eq!(setup.len(), 3);
    assert_eq!(&setup[0][6..12], b"MOD\0\x003");
    assert_eq!(&setup[1][1..9], b"CRT\0\0DIS");
    assert_eq!(&setup[2][6..12], b"LIG\0\0\0");
    assert_eq!(&initialized[1].reports[0][6..13], b"CLE\0\0\0\xff");
    assert_eq!(&initialized[2].reports[0][6..9], b"STP");
    session.encode(DeviceCommand::Wake).unwrap();
    let reconnected = Model.session().initialize().unwrap();
    assert_eq!(reconnected.len(), initialized.len());
    for (before, after) in initialized.iter().zip(reconnected) {
        assert_eq!(before.reports, after.reports);
    }
}

#[test]
// Verify each connection settles after mode before other initialization reports
fn each_connection_settles_after_mode_before_other_initialization_reports() {
    let expected = Model.session().initialize().unwrap();
    for _ in 0..2 {
        let mut session = Model.session();
        let plan = session.initialization_plan().unwrap();
        assert_eq!(plan.len(), 5);
        let InitializationStep::Write(mode) = &plan[0] else {
            panic!("mode must be written before settling");
        };
        assert_eq!(mode.reports, vec![expected[0].reports[0].clone()]);
        assert!(matches!(
            plan[1],
            InitializationStep::Wait(duration) if duration == std::time::Duration::from_millis(200)
        ));
        let InitializationStep::Write(wake) = &plan[2] else {
            panic!("wake must follow the mode settle interval");
        };
        assert_eq!(wake.reports, expected[0].reports[1..]);
        let mut waits = 0;
        let actual: Vec<_> = plan
            .into_iter()
            .flat_map(|step| match step {
                InitializationStep::Write(transaction) => transaction.reports,
                InitializationStep::Wait(_) => {
                    waits += 1;
                    Vec::new()
                }
            })
            .collect();
        assert_eq!(waits, 1);
        assert_eq!(
            actual,
            expected
                .iter()
                .flat_map(|transaction| transaction.reports.iter().cloned())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            session.encode(DeviceCommand::Wake).unwrap().reports.len(),
            1
        );
    }
}

#[test]
// Verify shared crt initialization keeps its default without a wait
fn shared_crt_initialization_keeps_its_default_without_a_wait() {
    static MODEL: Model = Model;
    let codec = hardware_protocols::vendor_crt::Codec::parse(
        include_str!("../codec.toml"),
        MODEL.manifest(),
    )
    .unwrap();
    let codec = Box::leak(Box::new(codec));
    let mut session = hardware_protocols::vendor_crt::Session::new(MODEL.manifest(), codec);
    let expected = session.initialize().unwrap();
    let plan = session.initialization_plan().unwrap();
    assert_eq!(plan.len(), expected.len());
    for (step, transaction) in plan.into_iter().zip(expected) {
        let InitializationStep::Write(actual) = step else {
            panic!("shared CRT initialization must not acquire N1 timing");
        };
        assert_eq!(actual.reports, transaction.reports);
        assert_eq!(actual.coalesce_key, transaction.coalesce_key);
    }
}

#[test]
// Verify repeated wakes and sleep resume do not reselect mode or zero brightness
fn repeated_wakes_and_sleep_resume_do_not_reselect_mode_or_zero_brightness() {
    let mut session = Model.session();
    let initialized = session.initialize().unwrap();
    let expected_wake = vec![initialized[0].reports[1].clone()];
    for _ in 0..3 {
        assert_eq!(
            session.encode(DeviceCommand::Wake).unwrap().reports,
            expected_wake
        );
        for encoding in [
            FrameEncoding::IndividualUpdates,
            FrameEncoding::GroupedKeyUpdates,
        ] {
            let frame = session
                .encode_frame(vec![DeviceCommand::Wake], encoding)
                .unwrap();
            assert_eq!(frame.len(), 1);
            assert_eq!(frame[0].transaction.reports, expected_wake);
            assert!(frame[0].commit.is_none());
        }
    }
    let sleep = session.encode(DeviceCommand::Sleep).unwrap();
    assert_eq!(sleep.reports.len(), 1);
    assert_eq!(&sleep.reports[0][6..9], b"HAN");
    let resumed = session
        .encode_frame(
            vec![DeviceCommand::Wake, DeviceCommand::Brightness(65)],
            FrameEncoding::IndividualUpdates,
        )
        .unwrap();
    assert_eq!(resumed.len(), 2);
    assert_eq!(resumed[0].transaction.reports, expected_wake);
    assert_eq!(resumed[1].transaction.reports.len(), 1);
    assert_eq!(&resumed[1].transaction.reports[0][6..12], b"LIG\0\0\x41");
}

#[test]
// Verify wake configuration preserves legacy default and rejects invalid values
fn wake_configuration_preserves_legacy_default_and_rejects_invalid_values() {
    static MODEL: Model = Model;
    let source = include_str!("../codec.toml");
    let legacy = source.replace("reinitialize_on_wake = false", "");
    let legacy = hardware_protocols::vendor_crt::Codec::parse(&legacy, Model.manifest()).unwrap();
    assert!(legacy.reinitialize_on_wake);
    let legacy = Box::leak(Box::new(legacy));
    let mut session = hardware_protocols::vendor_crt::Session::new(MODEL.manifest(), legacy);
    assert_eq!(
        session.encode(DeviceCommand::Wake).unwrap().reports,
        session.initialize().unwrap()[0].reports,
    );
    let invalid = source.replace("reinitialize_on_wake = false", "reinitialize_on_wake = 0");
    assert!(hardware_protocols::vendor_crt::Codec::parse(&invalid, Model.manifest()).is_err());
    let invalid = source.replace("wake_brightness = 0", "wake_brightness = 101");
    assert!(hardware_protocols::vendor_crt::Codec::parse(&invalid, Model.manifest()).is_err());
}

#[test]
// Verify first key edges do not require the manufacturer ok marker
fn first_key_edges_do_not_require_the_manufacturer_ok_marker() {
    for prefix_report_id in [false, true] {
        let mut session = Model.session();
        for pressed in [true, false] {
            let mut report = input(0, 2, u8::from(pressed));
            report[5..7].fill(0);
            if prefix_report_id {
                report.insert(0, 0);
            }
            assert_eq!(
                session.decode(&report).unwrap(),
                vec![ControlEvent {
                    control_id: "key-2".into(),
                    input: Input::Button { pressed },
                }]
            );
        }
        // Image-write acknowledgements must never trigger the dice binding
        assert!(session.decode(&input(0, 0xaa, 0xff)).unwrap().is_empty());
        let mut foreign = input(0, 2, 1);
        foreign[0] = b'X';
        assert!(session.decode(&foreign).unwrap().is_empty());
    }
}

#[test]
// Verify encoder and its display have independent ownership
fn encoder_and_its_display_have_independent_ownership() {
    check_n1_layout(&Model, 64);
    let manifest = Model.manifest();
    assert_eq!(manifest.controls.len(), 21);
    assert!(manifest.layout.height > manifest.layout.width);
    let first = manifest.control("key-1").unwrap();
    let top_right = manifest.control("key-3").unwrap();
    let bottom_left = manifest.control("key-13").unwrap();
    assert_eq!(first.y, top_right.y);
    assert_eq!(first.x, bottom_left.x);
    assert!(bottom_left.y > first.y);
    let display = manifest.control("display-3").unwrap();
    let knob = manifest.control("dial-1").unwrap();
    assert_eq!(display.kind, "display");
    assert_eq!(display.image_slot, Some(18));
    assert_eq!(display.image_width, Some(64));
    assert!(display.y < first.y);
    assert_eq!(knob.feedback_surface.as_deref(), Some("display-3"));
    assert!(knob.image_slot.is_none());
    let mut session = Model.session();
    for state in [1, 0] {
        assert_eq!(
            session.decode(&input(0, 0x23, state)).unwrap(),
            vec![ControlEvent {
                control_id: "dial-1".into(),
                input: Input::Button {
                    pressed: state == 1
                }
            }]
        );
        assert!(session.decode(&input(0, 18, state)).unwrap().is_empty());
        assert!(session.decode(&input(0, 0xff, state)).unwrap().is_empty());
    }
}

#[test]
// Verify resolved connection retains variant initialization and input policy
fn resolved_connection_retains_variant_initialization_and_input_policy() {
    let variant = &Model.manifest().variants[0].id;
    let contract = Model.resolve_contract(variant, None).unwrap();
    let mut resolved = Model.session_for(&contract).unwrap();
    let mut original = Model.session();
    let a = resolved.initialization_plan().unwrap();
    let b = original.initialization_plan().unwrap();
    assert_eq!(format!("{a:?}"), format!("{b:?}"));
    for code in 0..=255 {
        for state in [0, 1, 2, 255] {
            let report = [b'A', b'C', b'K', 0, 0, 0, 0, 0, 0, code, state];
            assert_eq!(
                resolved.decode(&report).unwrap(),
                original.decode(&report).unwrap()
            );
        }
    }
}
