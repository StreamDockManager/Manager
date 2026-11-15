//! Render packaged SVG/state assets through the real hardware feedback renderer
use base64::Engine;
use serde_json::json;
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let source = std::path::Path::new(args.get(1).ok_or("UI source directory required")?);
    let output = std::path::Path::new(args.get(2).ok_or("output directory required")?);
    let transparent = args.get(3).is_some_and(|value| value == "--transparent");
    std::fs::create_dir_all(output)?;
    let mut count = 0;
    for entry in std::fs::read_dir(source)? {
        let path = entry?.path();
        let cap: stream_dock_manager_plugin_host::Capabilities =
            serde_json::from_slice(&std::fs::read(path.join("descriptor.json"))?)?;
        for (action, capability) in cap.actions {
            for (state, asset) in capability.states.iter().enumerate() {
                for (theme, image) in [("light", &asset.light), ("dark", &asset.dark)] {
                    for (wide, width, height) in [(false, 112, 112), (true, 200, 60)] {
                        let mut layout: serde_json::Value =
                            serde_json::from_slice(&std::fs::read(path.join(format!(
                                "display/{}-{theme}.json",
                                if wide { "wide" } else { "key" }
                            )))?)?;
                        if transparent {
                            layout["background"] = json!("#00000000");
                            if let Some(items) = layout["items"].as_array_mut() {
                                for item in items {
                                    item["bar_bg_c"] = json!("#00000000");
                                }
                            }
                        }
                        let bytes = std::fs::read(path.join(image))?;
                        let value = json!({"icon":format!("data:image/svg+xml;base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes)),"line0":action.rsplit('.').next(),"line1":format!("State {state}"),"line2":theme});
                        let png = hardware_core::feedback::draw(&layout, &value, width, height, 0)?;
                        if transparent {
                            let image = image::load_from_memory(&png)?.to_rgba8();
                            assert_eq!(
                                image.get_pixel(0, 0)[3],
                                0,
                                "{action} paints an empty corner"
                            );
                            assert!(
                                image.pixels().any(|pixel| pixel[3] > 0),
                                "{action} lost its foreground"
                            );
                            let rect = &layout["items"][0]["rect"];
                            let x = (rect[0].as_f64().unwrap() * f64::from(width) / 200.0) as u32;
                            let y = (rect[1].as_f64().unwrap() * f64::from(height) / 100.0) as u32;
                            assert_eq!(
                                image.get_pixel(x, y)[3],
                                0,
                                "{action} paints an icon backplate"
                            );
                        }
                        std::fs::write(
                            output.join(format!(
                                "{action}-{state}-{theme}-{}.png",
                                if wide { "wide" } else { "key" }
                            )),
                            png,
                        )?;
                        count += 1;
                    }
                }
            }
        }
    }
    println!("Rendered {count} state/theme/surface snapshots");
    Ok(())
}
