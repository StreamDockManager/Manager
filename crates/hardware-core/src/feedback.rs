//! Bounded raster rendering for the supported Stream Deck feedback layout primitives
use crate::{render, Error, Result};
#[cfg(test)]
use base64::Engine;
use font8x8::UnicodeFonts;
use image::{DynamicImage, Pixel, Rgba, RgbaImage};
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
// Render the selected built-in feedback appearance
pub fn builtin(name: &str) -> Result<Value> {
    // Item geometry and names come from the official Dials guide, not inferred
    // hardware placement. Font/optical parity remains separately qualified.
    static FACTS: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let facts = FACTS.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../tests/fixtures/plugins/vendor/elgato/builtin-layout-facts.json"
        ))
        .expect("pinned builtin layout facts")
    });
    facts["layouts"]
        .get(name)
        .cloned()
        .ok_or_else(|| Error::Unsupported(format!("feedback layout {name}")))
}

// Draw for the current operation
pub fn draw(
    layout: &Value,
    values: &Value,
    width: u16,
    height: u16,
    rotation: u16,
) -> Result<Vec<u8>> {
    draw_on(layout, values, width, height, rotation, None)
}
// Draw on
pub fn draw_on(
    layout: &Value,
    values: &Value,
    width: u16,
    height: u16,
    rotation: u16,
    base: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let canvas = draw_pixels(layout, values, width, height, &|_| Ok(None), || {
        base.map(|base| {
            image::load_from_memory(base)
                .map(|image| {
                    image
                        .resize_exact(
                            width.into(),
                            height.into(),
                            image::imageops::FilterType::Lanczos3,
                        )
                        .into_rgba8()
                })
                .map_err(|e| Error::Protocol(e.to_string()))
        })
        .transpose()
    })?;
    render::encode_pixels(
        render::rotate_pixels(DynamicImage::ImageRgba8(canvas), rotation)?,
        "png",
    )
}

pub(crate) fn draw_pixels(
    layout: &Value,
    values: &Value,
    width: u16,
    height: u16,
    resolve: &render::ArtworkResolver<'_>,
    base: impl FnOnce() -> Result<Option<RgbaImage>>,
) -> Result<RgbaImage> {
    let items = layout["items"]
        .as_array()
        .ok_or_else(|| Error::Protocol("layout items missing".into()))?;
    if items.len() > 64 || width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(Error::Protocol("feedback layout exceeds limits".into()));
    }
    let background = color(layout["background"].as_str().unwrap_or("#0f141d"), 0.0);
    let mut canvas = base()?
        .unwrap_or_else(|| RgbaImage::from_pixel(width.into(), height.into(), Rgba(background)));
    let mut items = items.clone();
    items.sort_by_key(|i| i["zOrder"].as_i64().unwrap_or(0));
    for mut item in items {
        let key = item["key"].as_str().unwrap_or("").to_string();
        if let Some(value) = values.get(&key) {
            if let Some(properties) = value.as_object() {
                for (name, value) in properties {
                    item[name] = value.clone();
                }
            } else {
                item["value"] = value.clone();
            }
        }
        let rect = item["rect"]
            .as_array()
            .ok_or_else(|| Error::Protocol("layout rectangle missing".into()))?;
        if rect.len() != 4 {
            return Err(Error::Protocol("invalid layout rectangle".into()));
        }
        let read = |i: usize, scale: f64| -> u32 {
            (rect[i].as_f64().unwrap_or(0.0).clamp(0.0, 2000.0) * scale) as u32
        };
        let x = read(0, f64::from(width) / 200.0);
        let y = read(1, f64::from(height) / 100.0);
        let w = read(2, f64::from(width) / 200.0).min(u32::from(width).saturating_sub(x));
        let h = read(3, f64::from(height) / 100.0).min(u32::from(height).saturating_sub(y));
        if w == 0 || h == 0 {
            continue;
        }
        match item["type"].as_str().unwrap_or("") {
            "text" => {
                if let Some(style) = item.get("__manager_style") {
                    let style: crate::plugin_display::ElementStyle =
                        serde_json::from_value(style.clone())
                            .map_err(|e| Error::Protocol(e.to_string()))?;
                    let value = item["value"].as_str().map(String::from).unwrap_or_else(|| {
                        if item["value"].is_null() {
                            String::new()
                        } else {
                            item["value"].to_string()
                        }
                    });
                    let layer = crate::plugin_display::text_image(&value, &style, w, h, height)?;
                    image::imageops::overlay(&mut canvas, &layer, i64::from(x), i64::from(y));
                    continue;
                }
                let value = match &item["value"] {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                if item["__vendor_title_parameters"].is_object() {
                    if let Some(layer) = crate::vendor_title::draw(
                        &value,
                        &item["__vendor_title_parameters"],
                        w as u16,
                        h as u16,
                    )? {
                        image::imageops::overlay(&mut canvas, &layer, i64::from(x), i64::from(y));
                    }
                    continue;
                }
                if item["font"]["fit"] == true {
                    let style = crate::plugin_display::ElementStyle {
                        layout: layout["id"].as_str().unwrap_or("plugin-display").into(),
                        key: key.clone(),
                        kind: "text".into(),
                        size: item["font"]["size"].as_f64().unwrap_or(16.).clamp(1., 100.),
                        foreground: item["color"].as_str().unwrap_or("#edf2fc").into(),
                        align: item["alignment"].as_str().unwrap_or("center").into(),
                        wrap: false,
                        ..Default::default()
                    };
                    let layer = crate::plugin_display::text_image_fitted(
                        &value, &style, w, h, height, true,
                    )?;
                    image::imageops::overlay(&mut canvas, &layer, i64::from(x), i64::from(y));
                    continue;
                }
                let scale = if item["__manager_original_title"] == true {
                    2
                } else {
                    ((item["font"]["size"].as_f64().unwrap_or(16.0) * f64::from(height)
                        / 100.0
                        / 8.0) as u32)
                        .clamp(1, 6)
                };
                let max = (w / (8 * scale)).max(1) as usize;
                let chars: Vec<_> = value.chars().take(max).collect();
                let remaining = w.saturating_sub(chars.len() as u32 * 8 * scale);
                let left = x + match item["alignment"].as_str() {
                    Some("left") => 0,
                    Some("right") => remaining,
                    _ => remaining / 2,
                };
                let top = y + (h.saturating_sub(8 * scale)) / 2;
                let color = color(item["color"].as_str().unwrap_or("#edf2fc"), 0.0);
                for (index, ch) in chars.into_iter().enumerate() {
                    if let Some(glyph) = font8x8::BASIC_FONTS
                        .get(ch)
                        .or_else(|| font8x8::LATIN_FONTS.get(ch))
                    {
                        for (gy, bits) in glyph.iter().enumerate() {
                            for gx in 0..8 {
                                if bits & (1 << gx) != 0 {
                                    for dy in 0..scale {
                                        for dx in 0..scale {
                                            let px =
                                                left + index as u32 * 8 * scale + gx * scale + dx;
                                            let py = top + gy as u32 * scale + dy;
                                            if px < x + w && py < y + h {
                                                put(&mut canvas, px, py, color);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "bar" | "gbar" => {
                let min = item["range"]["min"].as_f64().unwrap_or(0.0);
                let max = item["range"]["max"].as_f64().unwrap_or(100.0);
                let value = item["value"]
                    .as_f64()
                    .or_else(|| item["value"].as_str().and_then(|s| s.parse().ok()))
                    .unwrap_or(0.0);
                let fraction = ((value - min) / (max - min).max(0.001)).clamp(0.0, 1.0);
                if item["type"] == "gbar" {
                    // The published layout schema defines bar_h as the height
                    // of a moving triangle below a fixed bar, not a fill level.
                    // The bounded raster shape is not reference-host optics
                    let indicator = item["bar_h"].as_f64().unwrap_or(10.).clamp(0., 2000.);
                    let indicator_h = ((indicator * f64::from(height) / 100.) as u32).min(h);
                    let bar_h = h - indicator_h;
                    let background = item["bar_bg_c"].as_str().unwrap_or("#303b4d");
                    for dx in 0..w {
                        let paint = color(
                            background,
                            f64::from(dx) / f64::from(w.saturating_sub(1).max(1)),
                        );
                        for dy in 0..bar_h {
                            put(&mut canvas, x + dx, y + dy, paint);
                        }
                    }
                    let indicator_color =
                        color(item["bar_fill_c"].as_str().unwrap_or("white"), fraction);
                    let center = (fraction * f64::from(w - 1)).round() as i64;
                    let half_width = (indicator * f64::from(width) / 200.).min(f64::from(w));
                    for dy in 0..indicator_h {
                        let half = (half_width * f64::from(dy)
                            / f64::from(indicator_h.saturating_sub(1).max(1)))
                        .round() as i64;
                        for dx in (center - half).max(0)..=(center + half).min(i64::from(w - 1)) {
                            put(&mut canvas, x + dx as u32, y + bar_h + dy, indicator_color);
                        }
                    }
                    continue;
                }
                let foreground = item["bar_fill_c"].as_str().unwrap_or("#82aef0");
                let background = item["bar_bg_c"].as_str().unwrap_or("#303b4d");
                for dx in 0..w {
                    let position = f64::from(dx) / f64::from(w);
                    let paint = color(
                        if position < fraction {
                            foreground
                        } else {
                            background
                        },
                        position,
                    );
                    for dy in 0..h {
                        put(&mut canvas, x + dx, y + dy, paint);
                    }
                }
            }
            "gauge" => {
                let min = item["range"]["min"].as_f64().unwrap_or(0.0);
                let max = item["range"]["max"].as_f64().unwrap_or(100.0);
                let fraction = ((item["value"].as_f64().unwrap_or(min) - min)
                    / (max - min).max(0.001))
                .clamp(0.0, 1.0);
                let radius = f64::from(w.min(h)) / 2.0;
                for dy in 0..h {
                    for dx in 0..w {
                        let px = f64::from(dx) - f64::from(w) / 2.0;
                        let py = f64::from(dy) - f64::from(h) / 2.0;
                        let distance = px.hypot(py);
                        let angle = (px.atan2(-py).to_degrees() + 135.0) / 270.0;
                        if distance >= radius * 0.72
                            && distance <= radius
                            && (0.0..=1.0).contains(&angle)
                        {
                            put(
                                &mut canvas,
                                x + dx,
                                y + dy,
                                if angle <= fraction {
                                    [130, 174, 240, 255]
                                } else {
                                    [48, 59, 77, 255]
                                },
                            );
                        }
                    }
                }
            }
            "sparkline" => {
                let values = item["value"]
                    .as_array()
                    .ok_or_else(|| Error::Protocol("sparkline points missing".into()))?;
                if values.len() > 120 {
                    return Err(Error::Protocol("sparkline exceeds 120 points".into()));
                }
                let min = item["range"]["min"].as_f64().unwrap_or(0.0);
                let max = item["range"]["max"].as_f64().unwrap_or(100.0);
                let points: Vec<_> = values
                    .iter()
                    .enumerate()
                    .filter_map(|(i, v)| {
                        v.as_f64().filter(|v| v.is_finite()).map(|v| {
                            (
                                i as f64 * f64::from(w.saturating_sub(1))
                                    / values.len().saturating_sub(1).max(1) as f64,
                                (1.0 - ((v - min) / (max - min).max(0.001)).clamp(0.0, 1.0))
                                    * f64::from(h.saturating_sub(1)),
                            )
                        })
                    })
                    .collect();
                for pair in points.windows(2) {
                    let (a, b) = (pair[0], pair[1]);
                    let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) as u32).max(1);
                    for step in 0..=steps {
                        let t = f64::from(step) / f64::from(steps);
                        put(
                            &mut canvas,
                            x + (a.0 + (b.0 - a.0) * t) as u32,
                            y + (a.1 + (b.1 - a.1) * t) as u32,
                            [130, 174, 240, 255],
                        );
                    }
                }
            }
            "pixmap" => {
                if let Some(uri) = item["value"].as_str() {
                    if let Some(bytes) = render::image_bytes(uri, resolve)? {
                        let image = if item.get("__manager_style").is_some() {
                            render::contained_pixels(&bytes, w as u16, h as u16)?
                        } else {
                            render::render_pixels(&bytes, w as u16, h as u16)?.into_rgba8()
                        };
                        image::imageops::overlay(&mut canvas, &image, i64::from(x), i64::from(y));
                    }
                }
            }
            other => return Err(Error::Unsupported(format!("layout item {other}"))),
        }
    }
    Ok(canvas)
}
// Write the value into its destination
fn put(image: &mut RgbaImage, x: u32, y: u32, color: [u8; 4]) {
    if x < image.width() && y < image.height() {
        image.get_pixel_mut(x, y).blend(&Rgba(color));
    }
}
// Resolve the requested color value
fn color(source: &str, position: f64) -> [u8; 4] {
    let source = if source.contains(':') {
        source
            .split(',')
            .filter_map(|s| s.split_once(':'))
            .filter_map(|(stop, color)| stop.parse::<f64>().ok().map(|n| (n, color)))
            .rfind(|(stop, _)| *stop <= position)
            .map(|(_, c)| c)
            .unwrap_or("#82aef0")
    } else {
        source
    };
    match source {
        "red" => [230, 70, 70, 255],
        "green" => [80, 200, 130, 255],
        "yellow" => [240, 205, 70, 255],
        "white" => [255; 4],
        "black" => [0, 0, 0, 255],
        "transparent" => [0; 4],
        other => crate::appearance::background_color(other).unwrap_or([130, 174, 240, 255]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Read the pixel representation used by the renderer
    fn pixels(layout: &Value, values: &Value) -> RgbaImage {
        image::load_from_memory(&draw(layout, values, 200, 100, 0).unwrap())
            .unwrap()
            .to_rgba8()
    }

    // Find the bounds of white pixels in the test image
    fn white_bounds(image: &RgbaImage) -> (u32, u32, u32, u32) {
        let points: Vec<_> = image
            .enumerate_pixels()
            .filter(|(_, _, pixel)| pixel.0 == [255; 4])
            .map(|(x, y, _)| (x, y))
            .collect();
        (
            points.iter().map(|p| p.0).min().unwrap(),
            points.iter().map(|p| p.1).min().unwrap(),
            points.iter().map(|p| p.0).max().unwrap(),
            points.iter().map(|p| p.1).max().unwrap(),
        )
    }

    #[test]
    // Verify text alignment moves raster glyphs and clips to the item
    fn text_alignment_moves_raster_glyphs_and_clips_to_the_item() {
        let mut layout = json!({"items":[{"key":"label","type":"text","rect":[20,30,160,40],"color":"#ffffff","font":{"size":8},"value":"F"}]});
        let center = white_bounds(&pixels(&layout, &json!({})));
        layout["items"][0]["alignment"] = json!("left");
        let left = white_bounds(&pixels(&layout, &json!({})));
        layout["items"][0]["alignment"] = json!("right");
        let right = white_bounds(&pixels(&layout, &json!({})));
        assert_eq!(center.0 - left.0, 76);
        assert_eq!(right.0 - left.0, 152);
        assert_eq!((left.1, left.3), (right.1, right.3));
        assert_eq!(left.2 - left.0, right.2 - right.0);
        let updated = pixels(&layout, &json!({"label":{"alignment":"left"}}));
        assert_eq!(white_bounds(&updated), left);
        // Fitting is a Manager extension, but does not discard the item's alignment
        layout["items"][0]["font"]["fit"] = json!(true);
        layout["items"][0]["font"]["size"] = json!(20);
        let fitted_left = white_bounds(&pixels(&layout, &json!({"label":{"alignment":"left"}})));
        let fitted_center =
            white_bounds(&pixels(&layout, &json!({"label":{"alignment":"center"}})));
        let fitted_right = white_bounds(&pixels(&layout, &json!({"label":{"alignment":"right"}})));
        assert!(fitted_left.0 < fitted_center.0 && fitted_center.0 < fitted_right.0);
        // Fractional font positioning can change the solid-white antialiased
        // interior by one pixel without changing the glyph or its alignment.
        assert!((fitted_left.2 - fitted_left.0).abs_diff(fitted_right.2 - fitted_right.0) <= 1);
        assert_eq!(
            (fitted_left.1, fitted_left.3),
            (fitted_right.1, fitted_right.3)
        );
        layout["items"][0]["font"]["fit"] = json!(false);
        layout["items"][0]["rect"] = json!([20, 30, 3, 2]);
        let clipped = pixels(&layout, &json!({}));
        assert!(clipped.enumerate_pixels().all(|(x, y, pixel)| {
            pixel.0 != [255; 4] || ((20..23).contains(&x) && (30..32).contains(&y))
        }));

        // The declared builtins consume these fields, rather than only custom layouts
        let mut builtin = builtin("$A1").unwrap();
        for item in builtin["items"].as_array_mut().unwrap() {
            if item["type"] == "text" {
                item["color"] = json!("#ffffff");
            }
        }
        let title = white_bounds(&pixels(&builtin, &json!({"title":"F"})));
        let value = white_bounds(&pixels(&builtin, &json!({"value":"F"})));
        assert_eq!(title.0, 16);
        assert_eq!(value.0, 160);
    }

    #[test]
    // Verify builtin gbar keeps its bar and moves a bounded lower triangle
    fn builtin_gbar_keeps_its_bar_and_moves_a_bounded_lower_triangle() {
        let layout = builtin("$B2").unwrap();
        let render = |value, height: Option<u32>| {
            let mut indicator = json!({"value":value,"bar_fill_c":"#ffffff"});
            if let Some(height) = height {
                indicator["bar_h"] = json!(height);
            }
            pixels(&layout, &json!({"indicator":indicator}))
        };
        let images = [render(0, None), render(50, None), render(100, None)];
        for y in 74..82 {
            for x in 76..184 {
                assert_eq!(images[0].get_pixel(x, y), images[1].get_pixel(x, y));
                assert_eq!(images[0].get_pixel(x, y), images[2].get_pixel(x, y));
            }
        }
        assert_eq!(images[0].get_pixel(76, 74).0, [255, 0, 0, 255]);
        assert_eq!(images[0].get_pixel(183, 74).0, [0, 255, 0, 255]);
        for (index, x) in [76, 130, 183].into_iter().enumerate() {
            assert_eq!(images[index].get_pixel(x, 82).0, [255; 4]);
            let bounds = white_bounds(&images[index]);
            assert_eq!((bounds.1, bounds.3), (82, 93)); // declared 12-pixel indicator
            assert!(bounds.0 >= 76 && bounds.2 < 184);
            assert_ne!(images[index].get_pixel(x, 94).0, [255; 4]);
        }
        let smaller = white_bounds(&render(50, Some(4)));
        assert_eq!((smaller.1, smaller.3), (90, 93));
        assert_eq!(render(-50, None), images[0]);
        assert_eq!(render(150, None), images[2]);
        let huge = white_bounds(&render(50, Some(u32::MAX)));
        assert_eq!((huge.1, huge.3), (74, 93));
        assert!(huge.0 >= 76 && huge.2 < 184);
        let mut default = layout;
        default["items"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|item| item["type"] == "gbar")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("bar_h");
        let default = pixels(
            &default,
            &json!({"indicator":{"value":50,"bar_fill_c":"#ffffff"}}),
        );
        let bounds = white_bounds(&default);
        assert_eq!((bounds.1, bounds.3), (84, 93)); // schema default is 10
    }

    #[test]
    // Verify layout and pixmap alpha survive without black rectangles
    fn layout_and_pixmap_alpha_survive_without_black_rectangles() {
        let icon = br##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><circle cx="16" cy="16" r="8" fill="#ff0000"/></svg>"##;
        let icon = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(icon)
        );
        let layout = json!({"background":"#00000000", "items":[{"key":"icon","type":"pixmap","rect":[0,0,200,100]}, {"key":"bar","type":"bar","rect":[0,90,200,10], "bar_bg_c":"#00000000","bar_fill_c":"#00ff00"}]});
        let bytes = draw(&layout, &json!({"icon":icon,"bar":50}), 32, 32, 0).unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        assert_eq!(image.get_pixel(16, 16).0, [255, 0, 0, 255]);
        assert_eq!(image.get_pixel(0, 29).0, [0, 255, 0, 255]);
        assert_eq!(image.get_pixel(31, 29)[3], 0);
    }
    #[test]
    // Verify feedback is bounded and renders values
    fn feedback_is_bounded_and_renders_values() {
        let layout = builtin("$B1").unwrap();
        let bytes = draw(
            &layout,
            &json!({"title":"Volume","value":"50%","indicator":50}),
            176,
            112,
            180,
        )
        .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (176, 112));
        assert!(draw(
            &json!({"items":vec![json!({});65]}),
            &json!({}),
            176,
            112,
            0
        )
        .is_err());
    }
}
#[test]
// Verify semantic text uses bundled metrics on small and wide surfaces
fn semantic_text_uses_bundled_metrics_on_small_and_wide_surfaces() {
    let layout = json!({"id":"plugin-display","background":"#00000000","items":[{"key":"line0","type":"text","rect":[6,28,188,40],"color":"#ffffff","font":{"size":30,"fit":true}}]});
    for (width, height) in [(64, 64), (176, 112)] {
        for text in ["123456789012345", "WWW iii café"] {
            let bytes = draw(&layout, &json!({"line0":text}), width, height, 0).unwrap();
            let pixels = image::load_from_memory(&bytes).unwrap().to_rgba8();
            assert!(pixels.pixels().any(|p| p[3] > 0));
            assert_eq!(pixels.get_pixel(0, 0)[3], 0);
        }
    }
}
