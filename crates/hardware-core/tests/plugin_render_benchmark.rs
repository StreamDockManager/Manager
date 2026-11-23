//! cargo test --release -p stream-dock-manager-hardware-core --test plugin_render_benchmark -- --ignored --nocapture
use base64::Engine;
use serde_json::{json, Value};
use std::{
    hash::{DefaultHasher, Hasher},
    time::Instant,
};
use stream_dock_manager_hardware_core::render;

fn uri(mime: &str, bytes: &[u8]) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn fixture(method: &str, frame: u32) -> Value {
    let x = frame % 50;
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="112" height="80"><g transform="translate({x} 0)"><path d="M0 40L22 0L52 35L20 72Z" fill="#ba2031"/><circle cx="24" cy="35" r="18" fill="#efcd90" opacity=".6"/></g></svg>"##
    );
    let image = match method {
        "png" | "jpeg" => {
            let pixels = image::RgbImage::from_fn(112, 112, |x, y| {
                image::Rgb([(x + frame) as u8, (y * 2) as u8, (x + y + frame) as u8])
            });
            uri(
                &format!("image/{method}"),
                &render::encode_pixels(image::DynamicImage::ImageRgb8(pixels), method).unwrap(),
            )
        }
        _ => uri("image/svg+xml", svg.as_bytes()),
    };
    match method {
        "styled_svg" => {
            json!({"layout":{"items":[{"type":"pixmap","key":"icon","rect":[0,0,200,100],"value":image,"__manager_style":{}}]}})
        }
        "feedback" => json!({"layout":{"items":[
            {"type":"bar","key":"value","rect":[0,0,200,72],"bar_fill_c":"0:#225544,0.4:#55cc88,0.8:#ffff00"},
            {"type":"gbar","key":"peak","rect":[0,72,200,28],"bar_bg_c":"0:red,0.5:yellow,0.8:green"}]},"feedback":{"value":frame%101,"peak":frame%101}}),
        _ => json!({"image":image,"title":format!("{frame}")}),
    }
}

#[test]
#[ignore = "Release throughput probe for eight independent plugin key visuals"]
fn eight_plugin_keys() {
    for method in ["svg", "styled_svg", "png", "jpeg", "feedback"] {
        let frames: Vec<_> = (0..192).map(|n| fixture(method, n)).collect();
        let mut digest = DefaultHasher::new();
        let mut times = Vec::new();
        for group in frames.as_chunks::<8>().0 {
            let start = Instant::now();
            for frame in group {
                let bytes = render::visual(frame, 112, 112, 180).unwrap();
                digest.write(&bytes);
            }
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
        times.sort_by(f64::total_cmp);
        eprintln!(
            "{method} eight_keys_p50_ms={:.3} eight_keys_p95_ms={:.3} digest={:016x}",
            times[times.len() / 2],
            times[(times.len() - 1) * 95 / 100],
            digest.finish()
        );
    }
}
