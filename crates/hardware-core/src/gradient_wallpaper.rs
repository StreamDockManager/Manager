//! High-precision sampling for the generated palette wash only. Imported SVGs,
//! photos and solid fills continue through the general renderer unchanged.
use super::*;

// Standard ancillary PNG text records origin for the downstream JPEG quality policy.
// Re-encoded/imported media without this marker retains its existing policy.
const MARKER: &[u8] = &[
    0, 0, 0, 25, 116, 69, 88, 116, 115, 100, 109, 45, 103, 101, 110, 101, 114, 97, 116, 101, 100,
    45, 103, 114, 97, 100, 105, 101, 110, 116, 0, 118, 49, 245, 137, 205, 168,
];
pub(super) fn is_generated(source: &[u8]) -> bool {
    source.starts_with(b"\x89PNG\r\n\x1a\n") && source.get(33..33 + MARKER.len()) == Some(MARKER)
}

fn colors(svg: &str) -> Option<[[u8; 3]; 3]> {
    let doc = roxmltree::Document::parse(svg).ok()?;
    let values: Vec<_> = doc
        .descendants()
        .filter_map(|node| {
            node.attribute("stop-color")
                .or_else(|| node.attribute("fill").filter(|v| v.starts_with('#')))
        })
        .collect();
    if values.len() != 3 {
        return None;
    }
    let expected = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 100" preserveAspectRatio="none"><defs><linearGradient id="wash" x2="100%" y2="100%"><stop stop-color="{}"/><stop offset="1" stop-color="{}"/></linearGradient></defs><path fill="url(#wash)" d="M0 0h200v100H0z"/><path fill="{}" opacity=".08" d="M122 100 200 22v78z"/></svg>"##,
        values[0], values[1], values[2]
    );
    if svg != expected {
        return None;
    }
    let mut result = [[0; 3]; 3];
    for (out, value) in result.iter_mut().zip(values) {
        if value.len() != 7 || !value.is_ascii() {
            return None;
        }
        for (channel, offset) in out.iter_mut().zip([1, 3, 5]) {
            *channel = u8::from_str_radix(&value[offset..offset + 2], 16).ok()?;
        }
    }
    Some(result)
}

// Stable spatial stochastic rounding avoids coherent contour lines and does not
// shimmer between frames. One shared threshold per pixel avoids colored speckle.
fn threshold(x: u32, y: u32) -> f32 {
    let mut hash = x.wrapping_mul(0x9e3779b9) ^ y.wrapping_mul(0x85ebca6b);
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7feb352d);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846ca68b);
    hash ^= hash >> 16;
    (hash & 0xffff) as f32 / 65536.0
}

pub(super) fn render(svg: &str, width: u16, height: u16, rotation: u16) -> Result<Vec<u8>> {
    let Some([start, end, _]) = colors(svg) else {
        return super::render(svg.as_bytes(), width, height, rotation, "png");
    };
    if start == end {
        return super::render(svg.as_bytes(), width, height, rotation, "png");
    }
    // Keep the existing SVG rasterizer's exact accent geometry/antialiasing.
    // The gradient itself stays floating point until final quantization.
    let overlay = super::render(
        svg.replace("url(#wash)", "none").as_bytes(),
        width,
        height,
        0,
        "png",
    )?;
    let mut pixels = image::load_from_memory(&overlay)
        .map_err(|e| Error::Protocol(e.to_string()))?
        .to_rgba8();
    for (x, y, pixel) in pixels.enumerate_pixels_mut() {
        let t = ((x as f32 + 0.5) / f32::from(width) + (y as f32 + 0.5) / f32::from(height)) * 0.5;
        let alpha = f32::from(pixel[3]) / 255.0;
        let noise = threshold(x, y);
        for c in 0..3 {
            let wash = f32::from(start[c]) + (f32::from(end[c]) - f32::from(start[c])) * t;
            let value = wash * (1.0 - alpha) + f32::from(pixel[c]) * alpha;
            pixel[c] = (value + noise).floor().clamp(0.0, 255.0) as u8;
        }
        pixel[3] = 255;
    }
    let pixels = DynamicImage::ImageRgba8(pixels);
    let pixels = match rotation {
        0 => pixels,
        90 => pixels.rotate90(),
        180 => pixels.rotate180(),
        270 => pixels.rotate270(),
        _ => return Err(Error::Protocol("invalid rotation".into())),
    };
    let mut encoded = encode_pixels(pixels, "png")?;
    encoded.splice(33..33, MARKER.iter().copied());
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Standard ancillary PNG text records origin for the downstream JPEG quality policy.
    // Re-encoded/imported media without this marker retains its existing policy.
    const MARKER: &[u8] = &[
        0, 0, 0, 25, 116, 69, 88, 116, 115, 100, 109, 45, 103, 101, 110, 101, 114, 97, 116, 101,
        100, 45, 103, 114, 97, 100, 105, 101, 110, 116, 0, 118, 49, 245, 137, 205, 168,
    ];
    pub(super) fn is_generated(source: &[u8]) -> bool {
        source.starts_with(b"\x89PNG\r\n\x1a\n")
            && source.get(33..33 + MARKER.len()) == Some(MARKER)
    }

    fn wallpaper() -> String {
        let palettes: serde_json::Value =
            serde_json::from_str(include_str!("../../../src/device-palettes.v1.json")).unwrap();
        palettes["palettes"]["dark"]["default"]["wallpaper"]
            .as_str()
            .unwrap()
            .into()
    }
    #[test]
    fn generated_gradient_quantization_is_bounded_unbiased_and_repeatable() {
        let svg = wallpaper();
        let encoded = render(&svg, 512, 256, 0).unwrap();
        assert!(is_generated(&encoded));
        assert_eq!(encoded, render(&svg, 512, 256, 0).unwrap());
        let pixels = image::load_from_memory(&encoded).unwrap().to_rgba8();
        let mut error = 0.0f64;
        let mut count = 0;
        let mut fractional = std::collections::HashSet::new();
        for y in 0..128 {
            for x in 0..256 {
                let exact =
                    8.0 + 18.0 * ((x as f64 + 0.5) / 512.0 + (y as f64 + 0.5) / 256.0) * 0.5;
                let p = pixels.get_pixel(x, y);
                let delta = f64::from(p[0]) - exact;
                assert!(delta.abs() < 1.0);
                assert_eq!(p[0], p[1]);
                assert_eq!(p[1], p[2]);
                assert_eq!(p[3], 255);
                error += delta;
                count += 1;
                // Constant continuous-tone diagonal now uses adjacent quantized levels,
                // rather than a single flat band.
                if x + 2 * y == 200 {
                    fractional.insert(p[0]);
                }
            }
        }
        assert!((error / count as f64).abs() < 0.02);
        assert_eq!(fractional.len(), 2);
        let rotated = image::load_from_memory(&render(&svg, 512, 256, 90).unwrap()).unwrap();
        assert_eq!(
            rotated.to_rgba8(),
            image::DynamicImage::ImageRgba8(pixels)
                .rotate90()
                .to_rgba8()
        );
    }
    #[test]
    fn all_generated_palettes_use_the_bounded_renderer() {
        let palettes: serde_json::Value =
            serde_json::from_str(include_str!("../../../src/device-palettes.v1.json")).unwrap();
        for mode in palettes["palettes"].as_object().unwrap().values() {
            for palette in mode.as_object().unwrap().values() {
                let svg = palette["wallpaper"].as_str().unwrap();
                assert!(colors(svg).is_some());
                let image = image::load_from_memory(&render(svg, 72, 48, 0).unwrap()).unwrap();
                assert_eq!(image.to_rgba8().dimensions(), (72, 48));
            }
        }
    }
    #[test]
    fn imported_art_and_solid_fills_are_not_dithered() {
        for svg in [r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="#123456"/></svg>"##.to_string(), wallpaper().replace("#1a1a1a","#080808")] {
            assert_eq!(render(&svg,112,112,0).unwrap(),super::super::render(svg.as_bytes(),112,112,0,"png").unwrap());
        }
    }
}
