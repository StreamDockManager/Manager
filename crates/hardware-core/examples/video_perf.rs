//! Matched prepared-video render/queue probe. Simulated transport; never opens HID
//! args: quality [75|90], editor-preview [open|closed]. Five one-second repetitions.
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use stream_dock_manager_hardware_core::{
    render, DeviceTransaction, OutputPolicy, OutputPriority, OutputStatus, OutputWorker,
    SurfaceSubmission, Transport,
};
struct Pipe(Arc<AtomicU64>);
impl Transport for Pipe {
    // Write for the current operation
    fn write(&mut self, bytes: &[u8]) -> stream_dock_manager_hardware_core::Result<()> {
        // A declared synthetic byte budget, independent of a USB device or firmware
        std::thread::sleep(Duration::from_secs_f64(
            bytes.len() as f64 / (16.0 * 1024.0 * 1024.0),
        ));
        self.0.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Ok(())
    }
}
// Run this executable entry point
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let quality = args.get(1).and_then(|q| q.parse::<u8>().ok()).unwrap_or(75);
    assert!((1..=100).contains(&quality));
    let editor = args.get(2).is_some_and(|arg| arg == "open");
    let pixels = image::RgbImage::from_fn(512, 288, |x, y| {
        image::Rgb([
            ((x * 19 + y * 7) % 256) as u8,
            ((x * 5 + y * 31) % 256) as u8,
            ((x * 13 + y * 11) % 256) as u8,
        ])
    });
    let mut source = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut source, 92)
        .encode_image(&pixels)
        .unwrap();
    let render = |size: [u16; 2], quality| {
        let pixels = render::edited_pixels(
            &source,
            size[0],
            size[1],
            0,
            [0., 0., 1., 1.],
            true,
            Some([64, 512]),
        )
        .unwrap();
        render::encode_pixels_quality(render::black_matte(pixels), "jpeg", quality).unwrap()
    };
    let mut repetitions = vec![];
    for _ in 0..5 {
        let bytes = Arc::new(AtomicU64::new(0));
        let worker = OutputWorker::spawn_with_policy(
            Box::new(Pipe(bytes.clone())),
            20,
            OutputPolicy::default(),
        )
        .unwrap();
        let start = Instant::now();
        let mut previous = None;
        let mut submitted = 0;
        let mut skipped = 0;
        let mut rendering_us = 0;
        let mut preview_slot = 0;
        while start.elapsed() < Duration::from_secs(1) {
            let elapsed = start.elapsed();
            let slot = (elapsed.as_secs_f64() * 120.).floor() as u64;
            if previous == Some(slot) {
                std::thread::sleep(Duration::from_micros(100));
                continue;
            }
            if let Some(last) = previous {
                skipped += slot.saturating_sub(last + 1);
            }
            previous = Some(slot);
            let rendering = Instant::now();
            let frame = render([800, 480], quality);
            if editor && slot / 2 > preview_slot {
                std::hint::black_box(render([512, 288], 75));
                preview_slot = slot / 2;
            }
            rendering_us += rendering.elapsed().as_micros() as u64;
            worker
                .submit_frame(
                    worker.generation(),
                    worker.scene_revision(),
                    vec![SurfaceSubmission {
                        surface_id: "background".into(),
                        content_revision: slot,
                        priority: OutputPriority::Animation,
                        upload: DeviceTransaction {
                            reports: vec![frame],
                            coalesce_key: Some("background".into()),
                        }
                        .into(),
                    }],
                )
                .unwrap();
            submitted += 1;
        }
        assert!(worker.wait_idle(Duration::from_secs(3)));
        let outcomes = worker.drain_outcomes();
        let mut ages: Vec<_> = outcomes
            .iter()
            .filter(|outcome| outcome.status == OutputStatus::Completed)
            .map(|outcome| outcome.queue_ms)
            .collect();
        ages.sort_by(f64::total_cmp);
        repetitions.push(serde_json::json!({"submitted":submitted,"source_opportunities_skipped":skipped,"completed":ages.len(),"replaced":outcomes.iter().filter(|o|o.status==OutputStatus::Replaced).count(),"render_and_preview_us":rendering_us,"output_bytes":bytes.load(Ordering::Relaxed),"queue_age_p95_ms":ages.get(ages.len()*95/100),"queue_age_max_ms":ages.last()}));
    }
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"build_profile":if cfg!(debug_assertions) { "dev" } else { "release" },"scope":"prepared-JPEG render and bounded output queue; simulated 16 MiB/s transport, no optical FPS qualification","source_fps":120,"destination":[800,480],"working_edge":[64,512],"jpeg_quality":quality,"editor_preview":editor,"cache":"cold per frame; excludes recording preparation","repetitions":repetitions})).unwrap());
}
