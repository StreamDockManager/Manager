//! Contact sheets of already-rendered native fixtures, at their actual pixel sizes
use std::{collections::BTreeMap, io::BufRead};
// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("Fixture directory required")?,
    );
    let mut fixtures = BTreeMap::<(String, String, u64), String>::new();
    for line in std::io::stdin().lock().lines() {
        let value: serde_json::Value = serde_json::from_str(&line?)?;
        let width = value["width"].as_u64().unwrap_or(0);
        if value["unavailable"] == true || ![64, 176].contains(&width) {
            continue;
        }
        let name = value["name"].as_str().unwrap();
        let theme = if name.contains("-light-") {
            "light"
        } else {
            "dark"
        };
        fixtures
            .entry((
                theme.into(),
                value["plugin"].as_str().unwrap().into(),
                width,
            ))
            .or_insert(name.into());
    }
    for theme in ["light", "dark"] {
        let plugins = fixtures
            .keys()
            .filter(|(t, _, w)| t == theme && *w == 64)
            .map(|(_, p, _)| p.clone())
            .collect::<Vec<_>>();
        let background = if theme == "light" {
            [244, 245, 247, 255]
        } else {
            [8, 8, 8, 255]
        };
        let foreground = if theme == "light" {
            "#181b24"
        } else {
            "#fafafa"
        };
        let mut sheet = image::RgbaImage::from_pixel(
            1216,
            plugins.len().div_ceil(4) as u32 * 160,
            image::Rgba(background),
        );
        for (i, plugin) in plugins.iter().enumerate() {
            let x = i as u32 % 4 * 304;
            let y = i as u32 / 4 * 160;
            let label = stream_dock_manager_hardware_core::feedback::draw(
                &serde_json::json!({"id":"review","background":"#00000000","items":[{"key":"label","type":"text","rect":[0,0,200,100],"color":foreground,"font":{"size":60,"fit":true}}]}),
                &serde_json::json!({"label":plugin}),
                296,
                24,
                0,
            )?;
            image::imageops::overlay(
                &mut sheet,
                &image::load_from_memory(&label)?.to_rgba8(),
                i64::from(x),
                i64::from(y),
            );
            for (width, dx, dy) in [(64, 8, 52), (176, 88, 28)] {
                let name = &fixtures[&(theme.into(), plugin.clone(), width)];
                let image = image::open(directory.join(format!("{name}.png")))?.to_rgba8();
                image::imageops::overlay(&mut sheet, &image, i64::from(x + dx), i64::from(y + dy));
            }
        }
        sheet.save(directory.join(format!("contact-sheet-{theme}.png")))?;
    }
    Ok(())
}
