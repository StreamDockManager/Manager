use hardware_core::*;
// Construct the input test fixture
fn input(id: u8, code: u8, state: u8) -> Vec<u8> {
    let mut data = b"ACK\0\0OK\0\0\0\0".to_vec();
    data[9] = code;
    data[10] = state;
    if id != 0 {
        data.insert(0, id);
    }
    data
}
// Check for the current operation
fn check(module: &dyn HardwareModule, source: &str) {
    let fixture: serde_json::Value = serde_json::from_str(source).unwrap();
    assert_eq!(fixture["method"], "simulated");
    let manifest = module.manifest();
    manifest.validate().unwrap();
    assert_eq!(fixture["model_id"], manifest.model_id);
    assert_eq!(manifest.status, "experimental");
    let usb: Vec<_> = manifest
        .variants
        .iter()
        .map(|v| format!("{:04x}:{:04x}", v.identity.vendor_id, v.identity.product_id))
        .collect();
    assert_eq!(serde_json::json!(usb), fixture["usb_ids"]);
    for v in &manifest.variants {
        for interface in [-1, 0, 7] {
            let identity = HidIdentity {
                vendor_id: v.identity.vendor_id,
                product_id: v.identity.product_id,
                usage: 1,
                usage_page: v.identity.usage_page.unwrap_or(1026),
                interface,
            };
            assert_eq!(
                module.matches(&identity),
                v.available && v.identity.interface.is_none_or(|i| i == interface)
            );
            assert!(!module.matches(&HidIdentity {
                usage_page: 1025,
                ..identity.clone()
            }));
            assert!(!module.matches(&HidIdentity {
                usage: 2,
                ..identity.clone()
            }));
            assert!(!module.matches(&HidIdentity {
                product_id: 0xffff,
                ..identity.clone()
            }));
            assert!(!module.matches(&HidIdentity {
                vendor_id: 0xffff,
                ..identity
            }));
        }
    }
    let report_id = fixture["report_id"].as_u64().unwrap() as u8;
    let report_size = fixture["report_size"].as_u64().unwrap() as usize;
    let payload = report_size - if report_id == 0 { 1 } else { 2 };
    let mut session = module.session();
    for row in fixture["inputs"].as_array().unwrap() {
        let code = row["code"].as_u64().unwrap() as u8;
        let id = row["control_id"].as_str().unwrap();
        for state in [0, 1, 2] {
            let expected = match row["event"].as_str().unwrap() {
                "button" => Some(Input::Button {
                    pressed: state == 1,
                }),
                "pulse" if state == 1 => Some(Input::Pulse),
                "pulse" => None,
                "rotate" => Some(Input::Rotate {
                    delta: row["delta"].as_i64().unwrap() as i32,
                }),
                "swipe" => Some(Input::Swipe {
                    direction: row["direction"].as_str().unwrap().into(),
                }),
                other => panic!("unsupported fixture event {other}"),
            };
            // A model may narrow accepted states without changing older CRT codecs
            let expected = expected.filter(|_| {
                row["states"].as_array().is_none_or(|states| {
                    states
                        .iter()
                        .any(|value| value.as_u64() == Some(u64::from(state)))
                })
            });
            let raw = input(report_id, code, state);
            let mut reports = vec![raw.clone()];
            if report_id == 0 {
                let mut padded = vec![0];
                padded.extend(raw);
                reports.push(padded);
            }
            for data in reports {
                assert_eq!(
                    session.decode(&data).unwrap(),
                    expected
                        .clone()
                        .into_iter()
                        .map(|input| ControlEvent {
                            control_id: id.into(),
                            input
                        })
                        .collect::<Vec<_>>()
                );
            }
        }
        if fixture["python_normalized_states"].as_bool() != Some(true) {
            assert!(session
                .decode(&input(report_id, code, 255))
                .unwrap()
                .is_empty());
        }
    }
    assert!(session
        .decode(&input(report_id, 255, 1))
        .unwrap()
        .is_empty());
    let valid = input(report_id, 1, 1);
    for size in 0..valid.len() {
        let result = session.decode(&valid[..size]);
        assert!(
            result.is_err() || result.unwrap().is_empty(),
            "truncated prefix {size}"
        );
    }
    let mut foreign = valid.clone();
    foreign[if report_id == 0 { 0 } else { 1 }] = b'X';
    assert!(session.decode(&foreign).unwrap().is_empty());
    if report_id != 0 {
        assert!(session.decode(&input(0, 1, 1)).unwrap().is_empty());
    }
    let mut bad_ack = valid.clone();
    bad_ack[if report_id == 0 { 5 } else { 6 }] = b'X';
    if fixture["require_input_ok"].as_bool() == Some(false) {
        assert_eq!(
            session.decode(&bad_ack).unwrap(),
            session.decode(&valid).unwrap()
        );
    } else {
        assert!(session.decode(&bad_ack).unwrap().is_empty());
    }
    let init = session.initialize().unwrap();
    assert_eq!(init.len(), 3);
    let mode_first = fixture["mode_before_wake"].as_bool().unwrap_or(false);
    let wake_report = usize::from(mode_first);
    assert_eq!(&init[0].reports[wake_report][6..9], b"DIS");
    let after_wake = if let Some(brightness) = fixture["wake_brightness"].as_u64() {
        assert_eq!(
            &init[0].reports[wake_report + 1][6..12],
            &[b'L', b'I', b'G', 0, 0, brightness as u8]
        );
        wake_report + 2
    } else {
        wake_report + 1
    };
    assert_eq!(
        init[0].reports.len(),
        after_wake + usize::from(!fixture["mode"].is_null() && !mode_first)
    );
    if !fixture["mode"].is_null() {
        let mode_report = if mode_first { 0 } else { after_wake };
        assert_eq!(&init[0].reports[mode_report][6..9], b"MOD");
        assert_eq!(
            init[0].reports[mode_report][11],
            b'1' + fixture["mode"].as_u64().unwrap() as u8
        );
    }
    for (command, header) in [
        (DeviceCommand::Sleep, b"HAN".as_slice()),
        (DeviceCommand::Heartbeat, b"CONNECT".as_slice()),
        (DeviceCommand::Clear, b"CLE".as_slice()),
    ] {
        let tx = session.encode(command).unwrap();
        assert_eq!(tx.reports[0].len(), report_size);
        assert_eq!(tx.reports[0][0], report_id);
        assert_eq!(&tx.reports[0][6..6 + header.len()], header);
    }
    assert!(session.encode(DeviceCommand::Brightness(101)).is_err());
    for value in [0, 37, 100] {
        let tx = session.encode(DeviceCommand::Brightness(value)).unwrap();
        assert_eq!(&tx.reports[0][6..12], &[b'L', b'I', b'G', 0, 0, value]);
    }
    let channels = manifest.capabilities.lighting_channels.len();
    let colors: Vec<_> = (0..channels).map(|i| [i as u8, 0, 255 - i as u8]).collect();
    let lighting = session.encode(DeviceCommand::Lighting {
        brightness: 37,
        colors: colors.clone(),
    });
    if manifest.capabilities.lighting {
        let tx = lighting.unwrap();
        assert_eq!(tx.reports.len(), 2);
        assert_eq!(tx.coalesce_key.as_deref(), Some("lighting"));
        assert!(tx
            .reports
            .iter()
            .all(|r| r.len() == report_size && r[0] == report_id));
        assert_eq!(&tx.reports[0][6..12], b"LBLIG\x25");
        assert_eq!(&tx.reports[1][6..11], b"SETLB");
        assert_eq!(
            &tx.reports[1][11..11 + 3 * channels],
            colors.iter().flatten().copied().collect::<Vec<_>>()
        );
        assert!(session
            .encode(DeviceCommand::Lighting {
                brightness: 101,
                colors: colors.clone()
            })
            .is_err());
        assert!(session
            .encode(DeviceCommand::Lighting {
                brightness: 37,
                colors: colors[..channels - 1].to_vec()
            })
            .is_err());
    } else {
        assert!(lighting.is_err());
    }
    let image = hardware_core::render::encode_pixels(
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            7,
            5,
            image::Rgb([23, 79, 143]),
        )),
        "png",
    )
    .unwrap();
    for row in fixture["surfaces"].as_array().unwrap() {
        let id = row["control_id"].as_str().unwrap();
        let tx = session
            .encode(DeviceCommand::Image {
                control_id: id.into(),
                data: image.clone(),
            })
            .unwrap();
        assert_eq!(tx.coalesce_key.as_deref(), Some(id));
        assert!(tx
            .reports
            .iter()
            .all(|r| r.len() == report_size && r[0] == report_id));
        assert_eq!(&tx.reports[0][6..9], b"BAT");
        assert_eq!(u64::from(tx.reports[0][13]), row["slot"].as_u64().unwrap());
        assert_eq!(&tx.reports.last().unwrap()[6..9], b"STP");
        let length = u32::from_be_bytes(tx.reports[0][9..13].try_into().unwrap()) as usize;
        let bytes: Vec<_> = tx.reports[1..tx.reports.len() - 1]
            .iter()
            .flat_map(|r| r[1..1 + payload].iter().copied())
            .take(length)
            .collect();
        assert_eq!(bytes.len(), length);
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(
            serde_json::json!([decoded.width(), decoded.height()]),
            row["wire_size"]
        );
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            if row["encoding"] == "png" {
                image::ImageFormat::Png
            } else {
                image::ImageFormat::Jpeg
            }
        );
    }
    assert!(session
        .encode(DeviceCommand::Image {
            control_id: "unknown".into(),
            data: image.clone()
        })
        .is_err());
    assert!(session
        .encode(DeviceCommand::Image {
            control_id: fixture["surfaces"][0]["control_id"]
                .as_str()
                .unwrap()
                .into(),
            data: vec![0; 10]
        })
        .is_err());
    if let Some(logo) = &manifest.capabilities.boot_logo {
        assert!(session
            .encode(DeviceCommand::BootLogo {
                data: image.clone()
            })
            .is_err());
        let boot = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                u32::from(logo.size[0]),
                u32::from(logo.size[1]),
                image::Rgb([23, 79, 143]),
            )),
            "png",
        )
        .unwrap();
        let tx = session
            .encode(DeviceCommand::BootLogo { data: boot })
            .unwrap();
        assert_eq!(&tx.reports[0][6..9], b"LOG");
        assert_eq!(tx.coalesce_key, None);
        let length = u32::from_be_bytes(tx.reports[0][9..13].try_into().unwrap()) as usize;
        assert_eq!(tx.reports[0][13], 1);
        assert_eq!(tx.reports.len(), 1 + length.div_ceil(payload));
        assert!(tx
            .reports
            .iter()
            .all(|r| r.len() == report_size && r[0] == report_id));
        assert!(logo.size.iter().all(|side| *side > 0));
    } else {
        assert!(session
            .encode(DeviceCommand::BootLogo {
                data: image.clone()
            })
            .is_err());
    }
    if let Some(size) = manifest.capabilities.background_size {
        let tx = session
            .encode(DeviceCommand::Background {
                data: image.clone(),
                x: 0,
                y: 0,
                width: size[0],
                height: size[1],
            })
            .unwrap();
        assert_eq!(&tx.reports[0][6..11], b"BGPIC");
        assert_eq!(tx.coalesce_key.as_deref(), Some("background"));
        let length = u32::from_be_bytes(tx.reports[0][11..15].try_into().unwrap()) as usize;
        assert_eq!(&tx.reports[0][15..19], &[0; 4]);
        assert_eq!(&tx.reports[0][19..21], &size[0].to_be_bytes());
        assert_eq!(&tx.reports[0][21..23], &size[1].to_be_bytes());
        assert_eq!(tx.reports.len(), 1 + length.div_ceil(payload));
        assert!(tx
            .reports
            .iter()
            .all(|r| r.len() == report_size && r[0] == report_id));
        assert!(session
            .encode(DeviceCommand::Background {
                data: image,
                x: 1,
                y: 0,
                width: size[0],
                height: size[1]
            })
            .is_err());
    } else {
        assert!(session
            .encode(DeviceCommand::Background {
                data: image,
                x: 0,
                y: 0,
                width: 1,
                height: 1
            })
            .is_err());
    }
    // Reconnect creates a fresh session; no trajectory or report state leaks
    let commands: Vec<_> = manifest
        .controls
        .iter()
        .filter(|c| c.image_slot.is_some())
        .map(|control| DeviceCommand::Image {
            control_id: control.id.clone(),
            data: hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    u32::from(control.image_width.unwrap()),
                    u32::from(control.image_height.unwrap()),
                    image::Rgb([31, 87, 142]),
                )),
                "png",
            )
            .unwrap(),
        })
        .collect();
    let individual = session
        .encode_frame(commands.clone(), FrameEncoding::IndividualUpdates)
        .unwrap();
    let grouped = session
        .encode_frame(commands, FrameEncoding::GroupedKeyUpdates)
        .unwrap();
    for ((control, old), new) in manifest
        .controls
        .iter()
        .filter(|c| c.image_slot.is_some())
        .zip(individual)
        .zip(grouped)
    {
        if control.kind == "button" {
            assert_eq!(
                new.commit.as_ref().unwrap().reports,
                vec![{
                    let mut report = vec![0; report_size];
                    report[0] = report_id;
                    report[1..9].copy_from_slice(b"CRT\0\0STP");
                    report
                }]
            );
            assert_eq!(
                &old.transaction.reports[..old.transaction.reports.len() - 1],
                new.transaction.reports.as_slice()
            );
        } else {
            assert!(new.commit.is_none());
            assert_eq!(old.transaction.reports, new.transaction.reports);
        }
    }
    let mut reconnected = module.session();
    assert_eq!(
        session.initialize().unwrap()[0].reports,
        reconnected.initialize().unwrap()[0].reports
    );
}
