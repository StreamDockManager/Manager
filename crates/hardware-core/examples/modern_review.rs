//! Render deterministic desired plugin views with the actual native compositor
//! stdin is JSONL from Plugins' tools/modernreview. No device access.
use std::io::BufRead;

// Normalize images
fn normalize_images(value: &mut serde_json::Value) -> Result<(), Box<dyn std::error::Error>> {
    use base64::Engine;
    match value {
        serde_json::Value::String(source) if source.starts_with("data:image/svg+xml;base64,") => {
            let data = base64::engine::general_purpose::STANDARD
                .decode(source.split_once(',').unwrap().1)?;
            *source = format!(
                "data:image/svg+xml;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(
                    stream_dock_manager_hardware_core::icons::transparent_svg(&data)
                )
            );
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                normalize_images(value)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_images(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// Count pixels with visible alpha coverage
fn visible_pixels(bytes: &[u8]) -> Result<usize, image::ImageError> {
    Ok(image::load_from_memory(bytes)?
        .to_rgba8()
        .pixels()
        .filter(|pixel| pixel[3] > 0)
        .count())
}

// Check editor
fn check_editor(value: &serde_json::Value) -> Result<usize, Box<dyn std::error::Error>> {
    use stream_dock_manager_hardware_core::plugin_display::{self, PluginAppearance};
    let visual = serde_json::json!({"plugin_id":value["plugin"],"action_id":value["action"],"title":value["title"],"state":0,"layout":value["layout"],"feedback":value["feedback"]});
    let mut checked = 0;
    for kind in ["text", "pixmap"] {
        let Some(element) = plugin_display::elements(&visual)
            .into_iter()
            .find(|element| {
                element.style.kind == kind && element.style.visible && !element.value.is_empty()
            })
        else {
            continue;
        };
        let mut style = element.style;
        style.rect = [5., 5., 90., 90.];
        style.size = 10.;
        style.font = "mono".into();
        style.foreground = "#fa8040".into();
        let appearance = PluginAppearance {
            vendor_profile: None,
            plugin_id: value["plugin"].as_str().unwrap().into(),
            action_id: value["action"].as_str().unwrap().into(),
            elements: vec![style.clone()],
        };
        let mut live = visual.clone();
        if kind == "text" {
            live["feedback"][&style.key] = serde_json::json!("Updated 12345");
        }
        let styled = plugin_display::styled_visual(&live, &appearance)?;
        let rendered = plugin_display::render_items(&styled);
        let item = rendered
            .iter()
            .find(|item| item["key"] == style.key)
            .ok_or("customized field disappeared")?;
        if item["__manager_style"] != serde_json::to_value(&style)? {
            return Err("live feedback replaced a saved editor style".into());
        }
        let pixels = stream_dock_manager_hardware_core::feedback::draw(
            &styled["layout"],
            &styled["feedback"],
            value["width"].as_u64().unwrap() as u16,
            value["height"].as_u64().unwrap() as u16,
            0,
        )?;
        if visible_pixels(&pixels)? == 0 {
            return Err("customized display is blank".into());
        }
        checked += 1;
    }
    Ok(checked)
}

// Run this executable entry point
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory =
        std::path::PathBuf::from(std::env::args().nth(1).ok_or("Output directory required")?);
    std::fs::create_dir_all(&directory)?;
    let mut count = 0;
    let mut checked = 0;
    let mut blank = Vec::new();
    let mut suffix_checks = 0;
    let mut clipped = Vec::new();
    let mut editor_checks = 0;
    for line in std::io::stdin().lock().lines() {
        let mut value: serde_json::Value = serde_json::from_str(&line?)?;
        let name = value["name"]
            .as_str()
            .ok_or("Missing fixture name")?
            .to_owned();
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err("Unsafe fixture name".into());
        }
        if value["normalize_icons"] == true {
            normalize_images(&mut value["layout"])?;
            normalize_images(&mut value["feedback"])?;
        }
        let image = stream_dock_manager_hardware_core::feedback::draw(
            &value["layout"],
            &value["feedback"],
            value["width"].as_u64().ok_or("Missing width")? as u16,
            value["height"].as_u64().ok_or("Missing height")? as u16,
            0,
        )?;
        if value["expect_visible"] == true {
            checked += 1;
            if visible_pixels(&image)? == 0 {
                blank.push(serde_json::json!({"name":name,"plugin":value["plugin"],"action":value["action"],"asset":value["asset"]}));
            }
        }
        if value["expect_suffix"] == true {
            suffix_checks += 1;
            let mut prefix = value["layout"].clone();
            let text = prefix["items"][0]["value"]
                .as_str()
                .ok_or("Text probe value missing")?;
            let mut text = text.to_owned();
            text.pop();
            prefix["items"][0]["value"] = serde_json::json!(text);
            let without_suffix = stream_dock_manager_hardware_core::feedback::draw(
                &prefix,
                &serde_json::json!({}),
                value["width"].as_u64().unwrap() as u16,
                value["height"].as_u64().unwrap() as u16,
                0,
            )?;
            if image == without_suffix {
                clipped.push(serde_json::json!({"name":name,"plugin":value["plugin"],"action":value["action"],"key":value["layout"]["items"][0]["key"],"value":value["layout"]["items"][0]["value"]}));
            }
        }
        if value["editor_check"] == true {
            editor_checks += check_editor(&value).map_err(|error| format!("{name}: {error}"))?;
        }
        std::fs::write(directory.join(format!("{name}.png")), image)?;
        count += 1;
    }
    if checked > 0 {
        std::fs::write(
            directory.join("audit.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"checked":checked,"blank":blank,"suffix_checks":suffix_checks,"clipped":clipped,"editor_checks":editor_checks}),
            )?,
        )?;
        if !blank.is_empty() || !clipped.is_empty() {
            return Err(format!(
                "{} blank and {} clipped native fixtures; see audit.json",
                blank.len(),
                clipped.len()
            )
            .into());
        }
    }
    eprintln!("Rendered {count} native compositor fixtures; no native-device qualification.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    // Verify pixel audit detects the embedded raster regression
    fn pixel_audit_detects_the_embedded_raster_regression() {
        let png = stream_dock_manager_hardware_core::render::solid_rgba(8, 8, [240, 40, 10, 255])
            .unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><image width="8" height="8" href="data:image/png;base64,{}"/></svg>"#,
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let blank =
            stream_dock_manager_hardware_core::render::render(svg.as_bytes(), 64, 64, 0, "png")
                .unwrap();
        assert_eq!(visible_pixels(&blank).unwrap(), 0);
        let visible =
            stream_dock_manager_hardware_core::render::render(&png, 64, 64, 0, "png").unwrap();
        assert!(visible_pixels(&visible).unwrap() > 0);
    }

    #[test]
    // Verify fitted defaults keep the final digit on small and wide fields
    fn fitted_defaults_keep_the_final_digit_on_small_and_wide_fields() {
        for (width, height, rect, size, text) in [
            (176, 112, [78, 34, 120, 32], 18, "80/2376"),
            (64, 64, [8, 34, 184, 24], 10, "45s / 214s"),
        ] {
            let layout = |fit| serde_json::json!({"id":"fixture","background":"#00000000","items":[{"key":"value","type":"text","rect":rect,"font":{"size":size,"fit":fit}}]});
            let prefix = &text[..text.len() - 1];
            let draw = |fit, value| {
                stream_dock_manager_hardware_core::feedback::draw(
                    &layout(fit),
                    &serde_json::json!({"value":value}),
                    width,
                    height,
                    0,
                )
                .unwrap()
            };
            assert_eq!(
                draw(false, text),
                draw(false, prefix),
                "fixture must reproduce the clipping bug"
            );
            assert_ne!(
                draw(true, text),
                draw(true, prefix),
                "the final digit must remain visible"
            );
        }
    }

    #[test]
    // Verify audit uses the hosts icon backplate normalization
    fn audit_uses_the_hosts_icon_backplate_normalization() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="#000000"/><path d="M0 0"/></svg>"##;
        let mut value = serde_json::json!({"icon":format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg))});
        normalize_images(&mut value).unwrap();
        let data = base64::engine::general_purpose::STANDARD
            .decode(value["icon"].as_str().unwrap().split_once(',').unwrap().1)
            .unwrap();
        assert!(!String::from_utf8(data).unwrap().contains("<rect"));
    }
}
