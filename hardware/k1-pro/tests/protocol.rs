use stream_dock_manager_hardware_k1_pro::Model;
include!("../../tests/crt_conformance.rs");
#[test]
// Verify source derived codec conformance
fn source_derived_codec_conformance() {
    check(&Model, include_str!("../fixtures/protocol.synthetic.json"));
}

#[test]
// Verify dials are above key rows without reindexing controls
fn dials_are_above_key_rows_without_reindexing_controls() {
    let manifest = Model.manifest();
    assert_eq!(
        manifest
            .controls
            .iter()
            .map(|control| control.id.as_str())
            .collect::<Vec<_>>(),
        ["key-1", "key-2", "key-3", "key-4", "key-5", "key-6", "dial-1", "dial-2", "dial-3",]
    );
    let keys = &manifest.controls[..6];
    let dials = &manifest.controls[6..];
    for (column, dial) in dials.iter().enumerate() {
        let upper_key = &keys[column];
        let lower_key = &keys[column + 3];
        assert_eq!(dial.kind, "encoder");
        assert_eq!(dial.image_slot, None);
        assert_eq!(dial.y, dials[0].y);
        assert!(dial.y + dial.height < upper_key.y);
        assert!(upper_key.y + upper_key.height < lower_key.y);
        assert_eq!(dial.x, upper_key.x);
        assert_eq!(dial.x, lower_key.x);
        if column > 0 {
            assert!(dials[column - 1].x + dials[column - 1].width < dial.x);
        }
    }
    for (index, key) in keys.iter().enumerate() {
        assert_eq!(key.kind, "button");
        assert_eq!(key.image_slot, Some([5, 3, 1, 6, 4, 2][index]));
        assert_eq!((key.image_width, key.image_height), (Some(64), Some(64)));
        assert_eq!(key.y, keys[(index / 3) * 3].y);
    }
    manifest.validate().unwrap();
}

#[test]
// Verify current SDK states are model specific and exhaustive
fn current_sdk_states_are_model_specific_and_exhaustive() {
    let mut session = Model.session();
    // Logical keys use the same hardware address for input and image uploads
    for (code, control_id) in [
        (5, "key-1"),
        (3, "key-2"),
        (1, "key-3"),
        (6, "key-4"),
        (4, "key-5"),
        (2, "key-6"),
        (0x25, "dial-1"),
        (0x30, "dial-2"),
        (0x31, "dial-3"),
    ] {
        for state in 0..=u8::MAX {
            let expected = match state {
                _ if code <= 6 => Some(Input::Button {
                    pressed: state == 1,
                }),
                1 => Some(Input::Pulse),
                _ => None,
            };
            assert_eq!(
                session.decode(&input(4, code, state)).unwrap(),
                expected
                    .into_iter()
                    .map(|input| ControlEvent {
                        control_id: control_id.into(),
                        input
                    })
                    .collect::<Vec<_>>(),
                "code {code:#x}, state {state}"
            );
        }
    }
    for (code, control_id, delta) in [
        (0x50, "dial-1", -1),
        (0x51, "dial-1", 1),
        (0x60, "dial-2", -1),
        (0x61, "dial-2", 1),
        (0x90, "dial-3", -1),
        (0x91, "dial-3", 1),
    ] {
        for state in 0..=u8::MAX {
            let expected = Some(ControlEvent {
                control_id: control_id.into(),
                input: Input::Rotate { delta },
            });
            assert_eq!(
                session.decode(&input(4, code, state)).unwrap(),
                expected.into_iter().collect::<Vec<_>>(),
                "code {code:#x}, state {state}"
            );
        }
    }
    for id in [0, 1, 3, 5, 255] {
        assert!(session.decode(&input(id, 5, 1)).unwrap().is_empty());
    }
    // An ignored state must not suppress the next valid press or rotation
    let mut reconnected = Model.session();
    for (code, state) in [(5, 1), (5, 0), (0x50, 0), (0x25, 1)] {
        let report = input(4, code, state);
        assert_eq!(
            session.decode(&report).unwrap(),
            reconnected.decode(&report).unwrap()
        );
    }
}

#[test]
// Verify every image slot has numbered chunks and clockwise orientation
fn every_image_slot_has_numbered_chunks_and_clockwise_orientation() {
    let mut session = Model.session();
    // Four color quadrants with deterministic detail ensure a multipart JPEG
    let colors = [[210, 40, 40], [40, 210, 40], [40, 40, 210], [210, 210, 40]];
    let source = image::RgbImage::from_fn(64, 64, |x, y| {
        let color = colors[((y / 32) * 2 + x / 32) as usize];
        let noise = ((x * 17 + y * 23) % 41) as i16 - 20;
        image::Rgb(color.map(|channel| (channel as i16 + noise) as u8))
    });
    let mut png = std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(source)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    for slot in 1..=6 {
        let tx = session
            .encode(DeviceCommand::Image {
                control_id: format!("key-{slot}"),
                data: png.get_ref().clone(),
            })
            .unwrap();
        assert!(tx
            .reports
            .iter()
            .all(|report| report.len() == 1025 && report[0] == 4));
        assert_eq!(&tx.reports[0][1..9], b"CRT\0\0BAT");
        assert_eq!(tx.reports[0][13], [5, 3, 1, 6, 4, 2][slot as usize - 1]);
        let length = u32::from_be_bytes(tx.reports[0][9..13].try_into().unwrap()) as usize;
        assert!(length > 1023, "fixture must cover multipart framing");
        assert_eq!(tx.reports.len(), length.div_ceil(1023) + 2);
        assert_eq!(&tx.reports.last().unwrap()[1..9], b"CRT\0\0STP");
        let chunks = &tx.reports[1..tx.reports.len() - 1];
        assert!(chunks.iter().all(|report| report[1024] == 0));
        let last_used = (length - 1) % 1023 + 1;
        assert!(chunks.last().unwrap()[1 + last_used..]
            .iter()
            .all(|byte| *byte == 0));
        let jpeg: Vec<_> = chunks
            .iter()
            .flat_map(|report| report[1..1024].iter().copied())
            .take(length)
            .collect();
        assert_eq!(
            image::guess_format(&jpeg).unwrap(),
            image::ImageFormat::Jpeg
        );
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (64, 64));
        // Clockwise 90 degrees: lower-left, upper-left, lower-right, upper-right
        for (x, y, expected) in [
            (16, 16, colors[2]),
            (48, 16, colors[0]),
            (16, 48, colors[3]),
            (48, 48, colors[1]),
        ] {
            let actual = decoded.get_pixel(x, y).0;
            assert!(
                actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| actual.abs_diff(expected) < 45),
                "slot {slot}, point ({x}, {y}): {actual:?}"
            );
        }
    }
}

#[test]
// Verify all source backed variants are available without broadening interfaces
fn all_source_backed_variants_are_available_without_broadening_interfaces() {
    let manifest = Model.manifest();
    assert_eq!(manifest.variants.len(), 3);
    for variant in &manifest.variants {
        assert!(variant.available);
        assert!(variant.unavailable_reason.is_none());
        let identity = HidIdentity {
            vendor_id: variant.identity.vendor_id,
            product_id: variant.identity.product_id,
            usage_page: 0xffa0,
            usage: 1,
            interface: 0,
        };
        assert!(Model.matches(&identity));
        assert!(!Model.matches(&HidIdentity {
            usage_page: 1,
            ..identity.clone()
        }));
        assert!(!Model.matches(&HidIdentity {
            usage: 6,
            ..identity
        }));
    }
    assert_eq!(manifest.status, "experimental");
    assert!(!manifest.capabilities.background);
    assert!(manifest.capabilities.boot_logo.is_none());
    assert!(!manifest.capabilities.reliable_encoder_release);
}

#[test]
// Verify keyboard reports are numbered padded and ordered with mode last
fn keyboard_reports_are_numbered_padded_and_ordered_with_mode_last() {
    let mut session = Model.session();
    for brightness in 0..=100 {
        for (mode, byte) in [
            (KeyboardOsMode::Windows, b'W'),
            (KeyboardOsMode::Macos, b'M'),
            (
                KeyboardOsMode::Auto,
                if cfg!(target_os = "macos") {
                    b'M'
                } else {
                    b'W'
                },
            ),
        ] {
            let tx = session
                .encode(DeviceCommand::Keyboard(KeyboardSettings {
                    brightness: Some(brightness),
                    color: Some([0, 127, 255]),
                    os_mode: Some(mode),
                    ..Default::default()
                }))
                .unwrap();
            assert_eq!(tx.coalesce_key, None);
            let expected = [
                vec![
                    b'C', b'O', b'L', b'O', b'R', 1, 0, 127, 255, 0, 210, 255, 255,
                ],
                vec![b'L', b'L', b'U', b'M', 0, brightness],
                vec![b'C', b'P', b'O', b'S', 0, byte],
            ];
            assert_eq!(tx.reports.len(), expected.len());
            for (report, suffix) in tx.reports.iter().zip(expected) {
                let mut bytes = vec![4, b'C', b'R', b'T', 0, 0];
                bytes.extend(suffix);
                bytes.resize(1025, 0);
                assert_eq!(*report, bytes);
            }
        }
    }
    for settings in [
        KeyboardSettings {
            brightness: Some(0),
            ..Default::default()
        },
        KeyboardSettings {
            color: Some([255, 0, 1]),
            ..Default::default()
        },
        KeyboardSettings {
            os_mode: Some(KeyboardOsMode::Windows),
            ..Default::default()
        },
    ] {
        assert_eq!(
            session
                .encode(DeviceCommand::Keyboard(settings))
                .unwrap()
                .reports
                .len(),
            1
        );
    }
}

#[test]
// Verify keyboard rejects empty ranges unknown fields and unsupported capabilities
fn keyboard_rejects_empty_ranges_unknown_fields_and_unsupported_capabilities() {
    let mut session = Model.session();
    assert!(session
        .encode(DeviceCommand::Keyboard(KeyboardSettings::default()))
        .is_err());
    for brightness in 101..=255 {
        assert!(session
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                brightness: Some(brightness),
                ..Default::default()
            }))
            .is_err());
    }
    for input in [
        r#"{"effect":256}"#,
        r#"{"speed":256}"#,
        r#"{"brightness":-1}"#,
        r#"{"brightness":256}"#,
        r#"{"color":[1,2]}"#,
        r#"{"color":[1,2,256]}"#,
        r#"{"os_mode":"linux"}"#,
        r#"{"remap":{}}"#,
    ] {
        assert!(
            serde_json::from_str::<KeyboardSettings>(input).is_err(),
            "{input}"
        );
    }
    let caps = Model.manifest().capabilities.keyboard.as_ref().unwrap();
    assert_eq!(caps.brightness_max, 100);
    assert!(caps.global_rgb);
    assert!(!Model.manifest().capabilities.lighting);
    assert_eq!(caps.effect_max, Some(15));
    assert_eq!(caps.speed_max, Some(5));
    let mut limited = caps.clone();
    limited.global_rgb = false;
    limited.brightness_max = 2;
    limited.os_modes = vec![KeyboardOsMode::Windows];
    for settings in [
        KeyboardSettings {
            brightness: Some(3),
            ..Default::default()
        },
        KeyboardSettings {
            color: Some([0, 0, 0]),
            ..Default::default()
        },
        KeyboardSettings {
            os_mode: Some(KeyboardOsMode::Macos),
            ..Default::default()
        },
    ] {
        assert!(settings.validate_for(&limited).is_err());
    }
    static CODEC: std::sync::LazyLock<hardware_protocols::vendor_crt::Codec> =
        std::sync::LazyLock::new(|| {
            hardware_protocols::vendor_crt::Codec::parse(
                include_str!("../codec.toml"),
                Model.manifest(),
            )
            .unwrap()
        });
    let mut shared = hardware_protocols::vendor_crt::Session::new(Model.manifest(), &CODEC);
    assert!(matches!(
        shared.encode(DeviceCommand::Keyboard(KeyboardSettings {
            brightness: Some(1),
            ..Default::default()
        })),
        Err(Error::Unsupported(_))
    ));
}

#[test]
// Verify keyboard native evidence and fixture gates are pinned
fn keyboard_native_evidence_and_fixture_gates_are_pinned() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/keyboard.synthetic.json")).unwrap();
    assert_eq!(fixture["method"], "simulated");
    assert_eq!(fixture["physical_qualification"], false);
    let inventory: serde_json::Value =
        serde_json::from_str(include_str!("../protocol-sources.v1.json")).unwrap();
    let sources = inventory["sources"].as_array().unwrap();
    for hash in [
        "94ebce805493f3f45aa9dd3f49c0606a8e639ee1aec825aa27214697435033a1",
        "29be8e616742495476d1b8e3c8b8697e7b89a6178a741c993f2b9525288ced60",
    ] {
        assert!(sources.iter().any(|source| source["sha256"] == hash));
    }
    let mut session = Model.session();
    for case in fixture["cases"].as_array().unwrap() {
        let settings: KeyboardSettings = serde_json::from_value(case["settings"].clone()).unwrap();
        let tx = session.encode(DeviceCommand::Keyboard(settings)).unwrap();
        let prefix = case["report_prefix"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_u64().unwrap() as u8)
            .collect::<Vec<_>>();
        assert_eq!(tx.reports.len(), 1);
        assert_eq!(&tx.reports[0][..prefix.len()], prefix);
        assert!(tx.reports[0][prefix.len()..].iter().all(|b| *b == 0));
    }
    assert!(include_str!("../README.md").contains("LMOD"));
    assert!(include_str!("../verification/plan.toml").contains("keyboard-mode-reenumeration"));
}

#[test]
// Verify keyboard encoding respects connection local capabilities
fn keyboard_encoding_respects_connection_local_capabilities() {
    let mut contract = Model.representative_contract().unwrap();
    contract.manifest.capabilities.keyboard = None;
    let mut disabled = Model.session_for(&contract).unwrap();
    assert!(matches!(
        disabled.encode(DeviceCommand::Keyboard(KeyboardSettings {
            brightness: Some(1),
            ..Default::default()
        })),
        Err(Error::Unsupported(_))
    ));

    let mut contract = Model.representative_contract().unwrap();
    let caps = contract.manifest.capabilities.keyboard.as_mut().unwrap();
    caps.brightness_max = 2;
    caps.global_rgb = false;
    caps.os_modes = vec![KeyboardOsMode::Windows];
    let mut restricted = Model.session_for(&contract).unwrap();
    for settings in [
        KeyboardSettings {
            brightness: Some(3),
            ..Default::default()
        },
        KeyboardSettings {
            color: Some([0, 127, 255]),
            ..Default::default()
        },
        KeyboardSettings {
            os_mode: Some(KeyboardOsMode::Macos),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            restricted.encode(DeviceCommand::Keyboard(settings)),
            Err(Error::Unsupported(_))
        ));
    }
    for brightness in 0..=2 {
        let tx = restricted
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                brightness: Some(brightness),
                os_mode: Some(KeyboardOsMode::Windows),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(tx.reports.len(), 2);
        assert_eq!(tx.reports[0][11], brightness);
        assert_eq!(tx.reports[1][11], b'W');
    }
    // Restrictions on this connection do not mutate the model's default session
    assert!(Model
        .session()
        .encode(DeviceCommand::Keyboard(KeyboardSettings {
            brightness: Some(6),
            color: Some([0, 127, 255]),
            os_mode: Some(KeyboardOsMode::Macos),
            ..Default::default()
        }))
        .is_ok());
}

#[test]
// Verify web speed and python static sequence are ordered
fn web_speed_and_python_static_sequence_are_ordered() {
    let tx = Model
        .session()
        .encode(DeviceCommand::Keyboard(KeyboardSettings {
            effect: Some(0),
            speed: Some(5),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(tx.reports.len(), 3);
    assert_eq!(&tx.reports[0][6..12], b"LSPD\0\0");
    assert_eq!(&tx.reports[1][6..12], b"LMOD\0\0");
    assert_eq!(&tx.reports[2][6..12], b"LSPD\0\x05");
    for effect in 0..=15 {
        assert!(Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                effect: Some(effect),
                ..Default::default()
            }))
            .is_ok());
    }
    for effect in 16..=255 {
        assert!(Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                effect: Some(effect),
                ..Default::default()
            }))
            .is_err());
    }
    // Source-derived official sendSpeed vectors, including every slider level
    // Assert exact opcode/offset rather than merely accepting a valid setting
    for speed in 0..=5 {
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                speed: Some(speed),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(tx.reports.len(), 1);
        assert_eq!(
            &tx.reports[0][..12],
            &[4, 67, 82, 84, 0, 0, 76, 83, 80, 68, 0, speed]
        );
        assert_eq!(tx.reports[0].len(), 1025);
        assert!(tx.reports[0][12..].iter().all(|byte| *byte == 0));
    }
    for speed in 6..=255 {
        assert!(Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                speed: Some(speed),
                ..Default::default()
            }))
            .is_err());
    }
}

#[test]
// Verify python lifecycle and screen brightness payloads
fn python_lifecycle_and_screen_brightness_payloads() {
    let mut session = Model.session();
    let initialize = session.initialize().unwrap();
    let prefixes = [b"DIS".as_slice(), b"CLE\0\0\xff", b"STP"];
    for (tx, suffix) in initialize.iter().zip(prefixes) {
        assert_eq!(&tx.reports[0][6..6 + suffix.len()], suffix);
        assert!(tx.reports[0][6 + suffix.len()..].iter().all(|v| *v == 0));
    }
    for value in 0..=100 {
        let tx = session.encode(DeviceCommand::Brightness(value)).unwrap();
        assert_eq!(&tx.reports[0][6..12], &[b'L', b'I', b'G', 0, 0, value]);
    }
    for (command, suffix) in [
        (DeviceCommand::Wake, b"DIS".as_slice()),
        (DeviceCommand::Sleep, b"HAN"),
        (DeviceCommand::Heartbeat, b"CONNECT"),
    ] {
        let tx = session.encode(command).unwrap();
        assert_eq!(&tx.reports[0][6..6 + suffix.len()], suffix);
    }
}

#[test]
// Verify custom RGB flag hsv and effect before color match vendor web app
fn custom_rgb_flag_hsv_and_effect_before_color_match_vendor_web_app() {
    // Independently calculated source-derived cases, not captured hardware
    for (rgb, hsv) in [
        ([255, 0, 0], [0, 0, 255, 255]),
        ([0, 255, 0], [0, 120, 255, 255]),
        ([0, 0, 255], [0, 240, 255, 255]),
        ([255, 0, 255], [1, 44, 255, 255]),
        ([0, 255, 255], [0, 180, 255, 255]),
        ([255, 255, 0], [0, 60, 255, 255]),
        ([128, 128, 128], [0, 0, 0, 128]),
        ([0, 0, 0], [0, 0, 0, 0]),
        ([255, 255, 255], [0, 0, 0, 255]),
        ([240, 120, 60], [0, 20, 191, 239]),
    ] {
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                effect: Some(4),
                speed: Some(3),
                color: Some(rgb),
                os_mode: Some(KeyboardOsMode::Windows),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(tx.reports.len(), 4);
        assert_eq!(&tx.reports[0][6..12], b"LMOD\0\x04");
        assert_eq!(&tx.reports[1][6..12], b"LSPD\0\x03");
        assert_eq!(&tx.reports[2][6..12], b"COLOR\x01");
        assert_eq!(&tx.reports[2][12..15], &rgb);
        assert_eq!(&tx.reports[2][15..19], &hsv);
        assert!(tx.reports[2][19..].iter().all(|byte| *byte == 0));
        assert_eq!(&tx.reports[3][6..12], b"CPOS\0W");
    }
}

#[test]
// Verify named effects and background hue saturation match official web app
fn named_effects_and_background_hue_saturation_match_official_web_app() {
    let caps = Model.manifest().capabilities.keyboard.as_ref().unwrap();
    assert_eq!(
        caps.effect_names,
        [
            "Static",
            "Gradient",
            "Breathing",
            "Wave",
            "Ripple",
            "Windmill",
            "Fountain",
            "Fireworks",
            "Band",
            "Pixel",
            "Flash",
            "Halo",
            "Rush",
            "Grid",
            "Ripple",
            "Game",
        ]
    );
    assert_eq!(caps.background_color_effects, [14]);
    // Wire IDs are array indexes. The vendor decorative Ripple ID is 9, not 14
    for index in 1..16 {
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                effect: Some(index),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(&tx.reports[0][6..12], &[b'L', b'M', b'O', b'D', 0, index]);
    }
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/keyboard-background.synthetic.json"
    ))
    .unwrap();
    assert_eq!(fixture["physical_qualification"], false);
    for case in fixture["cases"].as_array().unwrap() {
        let settings: KeyboardSettings = serde_json::from_value(case["settings"].clone()).unwrap();
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(settings))
            .unwrap();
        let prefix: Vec<u8> = case["report_prefix"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect();
        assert_eq!(tx.reports.len(), 1);
        assert_eq!(&tx.reports[0][..prefix.len()], prefix);
        assert_eq!(tx.reports[0].len(), 1025);
        assert!(tx.reports[0][prefix.len()..].iter().all(|byte| *byte == 0));
    }
    let tx = Model
        .session()
        .encode(DeviceCommand::Keyboard(KeyboardSettings {
            effect: Some(14),
            speed: Some(2),
            color: Some([255, 0, 0]),
            background_color: Some(KeyboardBackgroundColor {
                hue: 240,
                saturation: 100,
            }),
            os_mode: Some(KeyboardOsMode::Windows),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(tx.reports.len(), 5);
    assert_eq!(&tx.reports[0][6..12], b"LMOD\0\x0e");
    assert_eq!(&tx.reports[2][6..12], b"COLOR\x01");
    assert_eq!(&tx.reports[3][6..15], b"BASECO\0\xf0\xff");
    assert_eq!(&tx.reports[4][6..12], b"CPOS\0W");
    for background in [
        KeyboardBackgroundColor {
            hue: 360,
            saturation: 100,
        },
        KeyboardBackgroundColor {
            hue: 0,
            saturation: 101,
        },
    ] {
        assert!(Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                background_color: Some(background),
                ..Default::default()
            }))
            .is_err());
    }
    let mut limited = caps.clone();
    limited.background_color_effects.clear();
    assert!(KeyboardSettings {
        background_color: Some(KeyboardBackgroundColor {
            hue: 0,
            saturation: 0
        }),
        ..Default::default()
    }
    .validate_for(&limited)
    .is_err());
}

#[test]
// Verify compound keyboard writes restore luminance after visual commands
fn compound_keyboard_writes_restore_luminance_after_visual_commands() {
    for brightness in [0, 1, 35, 100] {
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                brightness: Some(brightness),
                effect: Some(14),
                speed: Some(3),
                color: Some([255, 0, 0]),
                background_color: Some(KeyboardBackgroundColor {
                    hue: 240,
                    saturation: 100,
                }),
                os_mode: Some(KeyboardOsMode::Macos),
                custom_color: Some(true),
                ..Default::default()
            }))
            .unwrap();
        let commands: Vec<_> = tx.reports.iter().map(|r| &r[6..10]).collect();
        assert_eq!(
            commands,
            [b"LMOD", b"LSPD", b"COLO", b"BASE", b"LLUM", b"CPOS"]
        );
        assert_eq!(tx.reports[4][11], brightness);
    }
}

#[test]
// Verify custom color toggle matches web flag and preserves selected RGB
fn custom_color_toggle_matches_web_flag_and_preserves_selected_rgb() {
    let color = [0, 127, 255];
    let mut reports = Vec::new();
    for enabled in [true, false, true] {
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                color: Some(color),
                custom_color: Some(enabled),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(tx.reports.len(), 1);
        assert_eq!(&tx.reports[0][6..11], b"COLOR");
        assert_eq!(tx.reports[0][11], u8::from(enabled));
        assert_eq!(&tx.reports[0][12..15], &color);
        reports.push(tx.reports[0].clone());
    }
    assert_eq!(reports[0], reports[2]);
    reports[1][11] = 1;
    assert_eq!(reports[0], reports[1]);
    let off = Model
        .session()
        .encode(DeviceCommand::Keyboard(KeyboardSettings {
            custom_color: Some(false),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(&off.reports[0][6..12], b"COLOR\0");
    assert!(Model
        .session()
        .encode(DeviceCommand::Keyboard(KeyboardSettings {
            custom_color: Some(true),
            ..Default::default()
        }))
        .is_err());
}

#[test]
// Verify sleep timer uses big endian seconds and never sentinel
fn sleep_timer_uses_big_endian_seconds_and_never_sentinel() {
    let expected = [60, 300, 900, 1800, 3600, 7200, u32::MAX];
    let caps = Model.manifest().capabilities.keyboard.as_ref().unwrap();
    assert_eq!(caps.sleep_timeout_seconds_options, expected);
    for seconds in expected {
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                sleep_timeout_seconds: Some(seconds),
                ..Default::default()
            }))
            .unwrap();
        assert_eq!(tx.reports.len(), 1);
        let report = &tx.reports[0];
        assert_eq!(report[0], 4);
        assert_eq!(&report[1..6], b"CRT\0\0");
        assert_eq!(&report[6..11], b"SLPT\0");
        assert_eq!(&report[11..15], &seconds.to_be_bytes());
        assert!(report[15..].iter().all(|byte| *byte == 0));
        assert!(tx.coalesce_key.is_none());
    }
    for seconds in [0, 1, 59, 600, 7201, u32::MAX - 1] {
        assert!(Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                sleep_timeout_seconds: Some(seconds),
                ..Default::default()
            }))
            .is_err());
    }
    let tx = Model
        .session()
        .encode(DeviceCommand::Keyboard(KeyboardSettings {
            sleep_timeout_seconds: Some(7200),
            os_mode: Some(KeyboardOsMode::Windows),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(&tx.reports[0][6..11], b"SLPT\0");
    assert_eq!(&tx.reports[1][6..10], b"CPOS");
    for tx in Model.session().initialize().unwrap() {
        assert!(tx.reports.iter().all(|report| &report[6..10] != b"SLPT"));
    }
}

#[test]
// Verify every effect keeps the selected custom color after mode selection
fn every_effect_keeps_the_selected_custom_color_after_mode_selection() {
    for effect in 0..=15 {
        let tx = Model
            .session()
            .encode(DeviceCommand::Keyboard(KeyboardSettings {
                effect: Some(effect),
                speed: Some(2),
                color: Some([18, 52, 86]),
                custom_color: Some(true),
                ..Default::default()
            }))
            .unwrap();
        let mode = tx
            .reports
            .iter()
            .position(|r| &r[6..10] == b"LMOD")
            .unwrap();
        let color = tx
            .reports
            .iter()
            .position(|r| &r[6..11] == b"COLOR")
            .unwrap();
        assert!(color > mode);
        assert_eq!(tx.reports[mode][11], effect);
        assert_eq!(&tx.reports[color][11..15], &[1, 18, 52, 86]);
    }
}

#[test]
// Verify keyboard factory reset and query match vendor layered k1 sequence
fn keyboard_factory_reset_and_query_match_vendor_layered_k1_sequence() {
    let caps = Model.manifest().capabilities.keyboard.as_ref().unwrap();
    assert!(caps.factory_reset && caps.settings_query);
    let tx = Model
        .session()
        .encode(DeviceCommand::KeyboardFactoryReset)
        .unwrap();
    assert_eq!(tx.reports.len(), 2);
    assert_eq!(&tx.reports[0][1..12], b"CRT\0\0LYRST\xff");
    assert_eq!(&tx.reports[1][1..11], b"CRT\0\0REREC");
    assert!(tx.coalesce_key.is_none());
    for report in tx.reports {
        assert_eq!(report[0], 4);
        assert_eq!(report.len(), 1025);
    }
    let query = Model
        .session()
        .encode(DeviceCommand::KeyboardSettingsQuery)
        .unwrap();
    assert_eq!(&query.reports[0][1..11], b"CRT\0\0INFO\0");
}

// Construct the keyboard JSON report test fixture
fn keyboard_json_report(value: serde_json::Value) -> Vec<u8> {
    let mut report = vec![4];
    report.extend_from_slice(b"DEVC\0\0\0\0\0\0\0");
    report.extend(serde_json::to_vec(&value).unwrap());
    report.resize(513, 0);
    report
}

#[test]
// Verify keyboard readback decodes reported values without filling unknown fields
fn keyboard_readback_decodes_reported_values_without_filling_unknown_fields() {
    let mut session = Model.session();
    let report = keyboard_json_report(serde_json::json!({
        "led_info": {"mode":14,"speed":3,"brightness":77,"flag":1,
            "hsv":[240,255,128],"base_hs":[120,128]},
        "os":"MacOS", "slpt":900, "version":"1.0"
    }));
    let actual = session.decode_keyboard_settings(&report).unwrap().unwrap();
    assert_eq!(actual.effect, Some(14));
    assert_eq!(actual.speed, Some(3));
    assert_eq!(actual.brightness, Some(77));
    assert_eq!(actual.custom_color, Some(true));
    assert_eq!(actual.color, Some([0, 0, 128]));
    assert_eq!(actual.os_mode, Some(KeyboardOsMode::Macos));
    assert_eq!(actual.sleep_timeout_seconds, Some(900));
    assert_eq!(
        actual.background_color,
        Some(KeyboardBackgroundColor {
            hue: 120,
            saturation: 51
        })
    );
    assert_eq!(
        session.decode_keyboard_settings(&report[1..]).unwrap(),
        Some(actual)
    );
    let partial = session
        .decode_keyboard_settings(&keyboard_json_report(serde_json::json!({
            "led_info":{"brightness":42},"slpt":0
        })))
        .unwrap()
        .unwrap();
    assert_eq!(
        partial,
        KeyboardSettings {
            brightness: Some(42),
            ..Default::default()
        }
    );
    let mut binary = vec![4];
    binary.extend_from_slice(b"DEVC\0\0\0\x02\x03W\x64firmware\0");
    let actual = session.decode_keyboard_settings(&binary).unwrap().unwrap();
    assert_eq!(
        actual,
        KeyboardSettings {
            effect: Some(2),
            speed: Some(3),
            os_mode: Some(KeyboardOsMode::Windows),
            brightness: Some(100),
            ..Default::default()
        }
    );
    assert_eq!(
        session.decode_keyboard_settings(b"\x04ACK\0").unwrap(),
        None
    );
}

#[test]
// Verify keyboard readback rejects truncation and invalid values
fn keyboard_readback_rejects_truncation_and_invalid_values() {
    let mut session = Model.session();
    for report in [
        b"\x04DEVC".to_vec(),
        keyboard_json_report(serde_json::json!({"led_info":{"mode":16}})),
        keyboard_json_report(serde_json::json!({"led_info":{"speed":6}})),
        keyboard_json_report(serde_json::json!({"led_info":{"flag":2}})),
        keyboard_json_report(serde_json::json!({"led_info":{"hsv":[0,256,255]}})),
        keyboard_json_report(serde_json::json!({"led_info":{"hsv":[0,255]}})),
        keyboard_json_report(serde_json::json!({"led_info":{"brightness":101}})),
        keyboard_json_report(serde_json::json!({"led_info":{"brightness":12},"os":"unknown"})),
    ] {
        assert!(session.decode_keyboard_settings(&report).is_err());
    }
}

#[test]
// Verify keyboard readback accepts both vendor never encodings but not other negatives
fn keyboard_readback_accepts_both_vendor_never_encodings_but_not_other_negatives() {
    let mut session = Model.session();
    for timer in [serde_json::json!(-1), serde_json::json!(u32::MAX)] {
        let reply =
            keyboard_json_report(serde_json::json!({"led_info":{"brightness":42},"slpt":timer}));
        assert_eq!(
            session
                .decode_keyboard_settings(&reply)
                .unwrap()
                .unwrap()
                .sleep_timeout_seconds,
            Some(u32::MAX)
        );
    }
    let invalid = keyboard_json_report(serde_json::json!({"led_info":{"brightness":42},"slpt":-2}));
    assert!(session.decode_keyboard_settings(&invalid).is_err());
}
