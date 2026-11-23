//! Explicit physical diagnostics. Uses only known image/refresh commands, never actions
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(serde::Serialize)]
struct WriteSample {
    start_ms: f64,
    wait_ms: f64,
    write_ms: f64,
    bytes: usize,
    command: String,
    slot: Option<u8>,
}
struct Probe {
    device: Arc<Mutex<hidapi::HidDevice>>,
    start: Instant,
    writes: Vec<WriteSample>,
}
impl Probe {
    // Assemble the n4 refresh probe report
    fn report(&mut self, report: &[u8]) {
        let start = self.start.elapsed().as_secs_f64() * 1000.0;
        let waiting = Instant::now();
        let device = self.device.lock().unwrap();
        let wait_ms = waiting.elapsed().as_secs_f64() * 1000.0;
        let writing = Instant::now();
        let written = device.write(report).unwrap();
        let write_ms = writing.elapsed().as_secs_f64() * 1000.0;
        drop(device);
        assert_eq!(written, report.len());
        let command = if report.get(1..6) == Some(b"CRT\0\0") {
            String::from_utf8_lossy(&report[6..9]).into_owned()
        } else {
            "payload".into()
        };
        let slot = (command == "BAT").then(|| report[13]);
        assert!(self.writes.len() < 10_000, "Bounded probe trace");
        self.writes.push(WriteSample {
            start_ms: start,
            wait_ms,
            write_ms,
            bytes: report.len(),
            command,
            slot,
        });
    }
    // Transmit probe frame reports with the requested delay and optional refresh commit
    fn uploads(&mut self, uploads: &[FrameUpload], commit: bool, delay: Duration) {
        for upload in uploads {
            for report in &upload.transaction.reports {
                self.report(report);
            }
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
        }
        if commit {
            if let Some(commit) = uploads.first().and_then(|u| u.commit.as_ref()) {
                for report in &commit.reports {
                    self.report(report);
                }
            }
        }
    }
    // Return the display label for n4 refresh probe
    fn label(&mut self, session: &mut dyn HardwareSession, text: &str) {
        let data = hardware_core::render::tile(176, 112, [32, 32, 32], text, 180, "png").unwrap();
        let command = session
            .encode(DeviceCommand::Image {
                control_id: "touch-1".into(),
                data,
            })
            .unwrap();
        for report in command.reports {
            self.report(&report);
        }
    }
}
struct Reader {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<usize>>,
}
impl Reader {
    // Start the n4 refresh probe operation
    fn start(device: Arc<Mutex<hidapi::HidDevice>>, blocking: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut inputs = 0;
            let mut report = [0u8; 1025];
            while !signal.load(Ordering::SeqCst) {
                let result = if blocking {
                    // Preserve the old shared-handle behavior for the A/B probe
                    device.lock().unwrap().read_timeout(&mut report, 5)
                } else {
                    read_hid_input(
                        &device,
                        true,
                        false,
                        |device, timeout| device.read_timeout(&mut report, timeout),
                        std::thread::sleep,
                    )
                };
                match result {
                    Ok(size) if size > 0 => inputs += 1,
                    Ok(_) => {}
                    Err(error) => panic!("Probe input failed: {error}"),
                }
            }
            inputs
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Reader {
    // Release this n4 refresh probe owner when it leaves scope
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            println!(
                "Discarded observed input reports: {}",
                thread.join().unwrap()
            );
        }
    }
}

#[test]
#[ignore = "Explicit N4 Pro optical/transport probe: STREAM_DOCK_MANAGER_N4_REFRESH_PROBE=RUN, Manager closed"]
// Verify that physical key refresh probe
fn physical_key_refresh_probe() {
    assert_eq!(
        std::env::var("STREAM_DOCK_MANAGER_N4_REFRESH_PROBE").as_deref(),
        Ok("RUN")
    );
    let output = std::env::var_os("STREAM_DOCK_MANAGER_N4_REFRESH_TRACE")
        .expect("Supply a fresh trace path");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    let api = hidapi::HidApi::new().unwrap();
    let supported: Vec<_> = api
        .device_list()
        .filter_map(|info| hardware_registry::find(&identity(info)).map(|module| (info, module)))
        .collect();
    assert_eq!(
        supported.len(),
        1,
        "Exactly one supported device is required"
    );
    let (info, module) = &supported[0];
    assert_eq!(module.manifest().model_id, "n4-pro");
    let device = Arc::new(Mutex::new(info.open_device(&api).unwrap()));
    let mut probe = Probe {
        device: device.clone(),
        start: Instant::now(),
        writes: Vec::new(),
    };
    let mut session = module.session();
    for tx in session.initialize().unwrap() {
        for report in tx.reports {
            probe.report(&report);
        }
    }
    let keys = |title: &str, color: [u8; 3]| {
        module
            .manifest()
            .controls
            .iter()
            .filter(|c| c.kind == "button" && c.image_slot.is_some())
            .map(|c| DeviceCommand::Image {
                control_id: c.id.clone(),
                data: hardware_core::render::tile(
                    c.image_width.unwrap(),
                    c.image_height.unwrap(),
                    color,
                    &format!("{title} {}", c.id),
                    180,
                    "png",
                )
                .unwrap(),
            })
            .collect::<Vec<_>>()
    };
    let blue = session
        .encode_frame(keys("A", [36, 87, 178]), FrameEncoding::GroupedKeyUpdates)
        .unwrap();
    let red = session
        .encode_frame(keys("B", [178, 57, 56]), FrameEncoding::GroupedKeyUpdates)
        .unwrap();
    let green = session
        .encode_frame(keys("C", [40, 124, 75]), FrameEncoding::GroupedKeyUpdates)
        .unwrap();
    assert_eq!(blue.len(), 10);
    for group in [&blue, &red, &green] {
        assert!(group
            .iter()
            .all(|u| u.commit == group[0].commit && u.embedded_commit_count == 0));
    }
    let mut phases = Vec::new();
    probe.label(session.as_mut(), "BASELINE");
    probe.uploads(&blue, true, Duration::ZERO);
    std::thread::sleep(Duration::from_secs(3));
    probe.label(session.as_mut(), "NO STP");
    let before = probe.writes.len();
    let started = probe.start.elapsed().as_secs_f64() * 1000.0;
    probe.uploads(&red, false, Duration::from_millis(200));
    println!("Red uploads complete WITHOUT refresh; holding for 3 seconds");
    std::thread::sleep(Duration::from_secs(3));
    let commit_at = probe.start.elapsed().as_secs_f64() * 1000.0;
    for report in &red[0].commit.as_ref().unwrap().reports {
        probe.report(report);
    }
    phases.push(serde_json::json!({"phase":"withheld_refresh","start_ms":started,"commit_ms":commit_at,"first_write":before,"end_write":probe.writes.len()}));
    std::thread::sleep(Duration::from_secs(2));
    for (name, mode) in [
        ("NO READER", None),
        ("BLOCK READ", Some(true)),
        ("POLL READ", Some(false)),
    ] {
        probe.label(session.as_mut(), name);
        let reader = mode.map(|blocking| Reader::start(device.clone(), blocking));
        for (title, group) in [("A", &blue), ("C", &green), ("A", &blue)] {
            let before = probe.writes.len();
            let started = Instant::now();
            probe.uploads(group, true, Duration::ZERO);
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            let writes = &probe.writes[before..];
            let wait_ms: f64 = writes.iter().map(|w| w.wait_ms).sum();
            let write_ms: f64 = writes.iter().map(|w| w.write_ms).sum();
            println!(
                "{name} {title}: {elapsed:.2} ms total, {wait_ms:.2} ms lock wait, {write_ms:.2} ms HID writes, {} reports",
                writes.len()
            );
            phases.push(serde_json::json!({"phase":name,"title":title,"elapsed_ms":elapsed,"wait_ms":wait_ms,"write_ms":write_ms,"first_write":before,"end_write":probe.writes.len()}));
            std::thread::sleep(Duration::from_millis(1200));
        }
        drop(reader);
    }
    probe.label(session.as_mut(), "DONE");
    serde_json::to_writer_pretty(&mut file,&serde_json::json!({"schema":1,"model":"n4-pro","note":"Host write timing only. Optical behavior needs recording. Input reports discarded; no actions executed.","phases":phases,"writes":probe.writes})).unwrap();
    println!("Probe complete; reopen Manager to restore the saved scene.");
}
