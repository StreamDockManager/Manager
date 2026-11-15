use hardware_core::*;
use stream_dock_manager_hardware_n3_vsd::Model;

#[test]
// Verify matches only the observed vendor interface
fn matches_only_the_observed_vendor_interface() {
    let identity = HidIdentity {
        vendor_id: 0x5548,
        product_id: 0x1001,
        interface: 0,
        usage_page: 0xffa0,
        usage: 1,
    };
    Model.manifest().validate().unwrap();
    assert!(Model.matches(&identity));
    for other in [
        HidIdentity {
            interface: 1,
            usage_page: 1,
            usage: 6,
            ..identity.clone()
        },
        HidIdentity {
            interface: 1,
            ..identity.clone()
        },
        HidIdentity {
            usage_page: 0xff00,
            ..identity.clone()
        },
        HidIdentity {
            usage: 2,
            ..identity.clone()
        },
        HidIdentity {
            product_id: 0x1002,
            ..identity.clone()
        },
        HidIdentity {
            vendor_id: 0x5500,
            ..identity
        },
    ] {
        assert!(!Model.matches(&other));
    }
}

#[test]
// Verify source fixture covers every control edge and rotation
fn source_fixture_covers_every_control_edge_and_rotation() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/protocol.synthetic.json")).unwrap();
    assert_eq!(fixture["method"], "simulated");
    let mut session = Model.session();
    for row in fixture["inputs"].as_array().unwrap() {
        for state in [0, 1] {
            // The reference reader only requires ACK, not an additional OK marker
            let mut report = vec![0; 512];
            report[..3].copy_from_slice(b"ACK");
            report[9] = row["code"].as_u64().unwrap() as u8;
            report[10] = state;
            let expected = vec![ControlEvent {
                control_id: row["control_id"].as_str().unwrap().into(),
                input: if row["kind"] == "button" {
                    Input::Button {
                        pressed: state == 1,
                    }
                } else {
                    Input::Rotate {
                        delta: row["delta"].as_i64().unwrap() as i32,
                    }
                },
            }];
            assert_eq!(session.decode(&report).unwrap(), expected);
            let mut numbered = vec![0];
            numbered.extend(&report);
            assert_eq!(session.decode(&numbered).unwrap(), expected);
            for length in 0..11 {
                assert!(session.decode(&report[..length]).unwrap().is_empty());
            }
            report[10] = 255;
            if row["kind"] == "button" {
                assert!(session.decode(&report).unwrap().is_empty());
            } else {
                // Turning does not use the state byte
                assert_eq!(session.decode(&report).unwrap(), expected);
                report[10] = 2;
                assert_eq!(session.decode(&report).unwrap(), expected);
            }
            report[10] = state;
            report[0] = b'X';
            assert!(session.decode(&report).unwrap().is_empty());
        }
    }
    let mut unknown = b"ACK\0\0\0\0\0\0\xff\x01".to_vec();
    assert!(session.decode(&unknown).unwrap().is_empty());
    unknown[9] = 0;
    assert!(session.decode(&unknown).unwrap().is_empty());
}

#[test]
// Verify initialization and wake enable input without the vendor app
fn initialization_and_wake_enable_input_without_the_vendor_app() {
    let mut session = Model.session();
    let init = session.initialize().unwrap();
    let reports: Vec<_> = init.iter().flat_map(|tx| &tx.reports).collect();
    assert_eq!(reports.len(), 4);
    assert_eq!(&reports[0][1..9], b"CRT\0\0DIS");
    assert_eq!(&reports[1][6..14], b"LIG\0\0\0\0\0");
    assert_eq!(&reports[2][6..13], b"CLE\0\0\0\xff");
    assert_eq!(&reports[3][6..9], b"STP");
    assert!(reports.iter().all(|r| r.len() == 1025 && r[0] == 0));
    assert_eq!(
        session.encode(DeviceCommand::Wake).unwrap().reports,
        init[0].reports
    );
    let wake_frame = session
        .encode_frame(vec![DeviceCommand::Wake], FrameEncoding::IndividualUpdates)
        .unwrap();
    assert_eq!(wake_frame[0].transaction.reports, init[0].reports);
    for (command, bytes) in [
        (DeviceCommand::Brightness(37), b"LIG\0\0\x25".as_slice()),
        (DeviceCommand::Heartbeat, b"CONNECT".as_slice()),
        (DeviceCommand::Sleep, b"HAN".as_slice()),
    ] {
        let tx = session.encode(command).unwrap();
        assert_eq!(&tx.reports[0][6..6 + bytes.len()], bytes);
    }
    assert!(session.encode(DeviceCommand::Brightness(101)).is_err());
}

#[test]
// Verify uploads six rotated jpegs and never writes to plain buttons
fn uploads_six_rotated_jpegs_and_never_writes_to_plain_buttons() {
    let mut session = Model.session();
    let mut pixels = image::RgbImage::new(64, 64);
    for (x, _, pixel) in pixels.enumerate_pixels_mut() {
        *pixel = if x < 32 {
            image::Rgb([240, 10, 10])
        } else {
            image::Rgb([10, 10, 240])
        };
    }
    let mut encoded = std::io::Cursor::new(vec![]);
    pixels
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let image = encoded.into_inner();
    for key in 1..=6 {
        let tx = session
            .encode(DeviceCommand::Image {
                control_id: format!("key-{key}"),
                data: image.clone(),
            })
            .unwrap();
        assert!(tx.reports.iter().all(|r| r.len() == 1025 && r[0] == 0));
        assert_eq!(&tx.reports[0][6..9], b"BAT");
        assert_eq!(tx.reports[0][13], key);
        assert_eq!(&tx.reports.last().unwrap()[6..9], b"STP");
        let length = u32::from_be_bytes(tx.reports[0][9..13].try_into().unwrap()) as usize;
        let bytes: Vec<_> = tx.reports[1..tx.reports.len() - 1]
            .iter()
            .flat_map(|r| r[1..].iter().copied())
            .take(length)
            .collect();
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (64, 64));
        assert!(decoded.get_pixel(32, 8)[0] > 200);
        assert!(decoded.get_pixel(32, 56)[2] > 200);
    }
    for id in ["key-7", "key-8", "key-9", "dial-1"] {
        assert!(Model.manifest().control(id).unwrap().image_slot.is_none());
        assert!(session
            .encode(DeviceCommand::Image {
                control_id: id.into(),
                data: image.clone()
            })
            .is_err());
    }
    let grouped = session
        .encode_frame(
            (1..=6)
                .map(|i| DeviceCommand::Image {
                    control_id: format!("key-{i}"),
                    data: image.clone(),
                })
                .collect(),
            FrameEncoding::GroupedKeyUpdates,
        )
        .unwrap();
    assert_eq!(grouped.len(), 6);
    for upload in grouped {
        assert_eq!(upload.embedded_commit_count, 0);
        assert_eq!(&upload.commit.unwrap().reports[0][6..9], b"STP");
        assert!(upload
            .transaction
            .reports
            .iter()
            .all(|r| &r[6..9] != b"STP"));
    }
    assert!(session
        .encode(DeviceCommand::BootLogo {
            data: image.clone()
        })
        .is_err());
    assert!(session
        .encode(DeviceCommand::Background {
            data: image,
            x: 0,
            y: 0,
            width: 64,
            height: 64
        })
        .is_err());
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
