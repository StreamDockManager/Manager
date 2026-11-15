use crate::{Error, Result};
use image::{imageops::FilterType, DynamicImage, ImageFormat};
use std::io::Cursor;

// Parse a static SVG after rejecting unsupported external image sources
fn static_svg_tree(source: &[u8]) -> Result<resvg::usvg::Tree> {
    let svg = std::str::from_utf8(source)
        .map_err(|_| Error::Protocol("SVG must be UTF-8".into()))?
        .trim_start_matches('\u{feff}');
    crate::appearance::validate_static_svg(svg).map_err(Error::Protocol)?;
    let mut options = resvg::usvg::Options::default();
    // Disable both external and embedded image resolution in this static SVG path
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
    let tree = resvg::usvg::Tree::from_str(svg, &options)
        .map_err(|error| Error::Protocol(error.to_string()))?;
    crate::appearance::check_svg_dimensions(&tree).map_err(Error::Protocol)?;
    Ok(tree)
}

// Read the dimensions of validated SVG media
pub fn svg_media_dimensions(source: &[u8]) -> Result<(u32, u32)> {
    let tree = static_svg_tree(source)?;
    Ok((
        tree.size().width().ceil() as u32,
        tree.size().height().ceil() as u32,
    ))
}

// Rasterize validated SVG media within the requested edge length
fn svg_media_pixels(source: &[u8], edge: u32) -> Result<DynamicImage> {
    let tree = static_svg_tree(source)?;
    let size = tree.size();
    let scale = edge as f32 / size.width().max(size.height());
    let width = (size.width() * scale).ceil().clamp(1.0, edge as f32) as u32;
    let height = (size.height() * scale).ceil().clamp(1.0, edge as f32) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| Error::Protocol("SVG output allocation failed".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let bytes = pixmap
        .encode_png()
        .map_err(|error| Error::Protocol(error.to_string()))?;
    image::load_from_memory_with_format(&bytes, ImageFormat::Png)
        .map_err(|error| Error::Protocol(error.to_string()))
}

// Check whether the value is SVG media
pub fn is_svg_media(source: &[u8]) -> bool {
    source
        .strip_prefix(&[0xef, 0xbb, 0xbf])
        .unwrap_or(source)
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        == Some(&b'<')
}

// Render for the current operation
pub fn render(
    source: &[u8],
    width: u16,
    height: u16,
    rotation: u16,
    format: &str,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(Error::Protocol("invalid image dimensions".into()));
    }
    if source.len() > 16 * 1024 * 1024 {
        return Err(Error::Protocol("asset exceeds 16 MiB".into()));
    }
    let raster;
    let source = if source.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<') {
        let mut options = resvg::usvg::Options::default();
        options.image_href_resolver.resolve_string = Box::new(|_, _| None);
        options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
        let tree = resvg::usvg::Tree::from_data(source, &options)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(width.into(), height.into())
            .ok_or_else(|| Error::Protocol("SVG output allocation failed".into()))?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(
                f32::from(width) / tree.size().width(),
                f32::from(height) / tree.size().height(),
            ),
            &mut pixmap.as_mut(),
        );
        raster = pixmap
            .encode_png()
            .map_err(|e| Error::Protocol(e.to_string()))?;
        raster.as_slice()
    } else {
        source
    };
    let mut reader = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| Error::Protocol(e.to_string()))?
        .resize_to_fill(width.into(), height.into(), FilterType::Lanczos3);
    let image = match rotation {
        0 => image,
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => return Err(Error::Protocol("invalid rotation".into())),
    };
    encode_pixels(image, format)
}
// Create a solid RGBA image
pub fn solid_rgba(width: u16, height: u16, color: [u8; 4]) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(Error::Protocol("invalid image dimensions".into()));
    }
    encode_pixels(
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width.into(),
            height.into(),
            image::Rgba(color),
        )),
        "png",
    )
}
/// Decode a bounded internal render result once for a layered composition
/// Imported/plugin sources must first pass through their existing validated renderer
pub fn layer_pixels(source: &[u8], width: u16, height: u16) -> Result<image::RgbaImage> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(Error::Protocol("invalid image dimensions".into()));
    }
    // Internal PNGs can be larger than their compressed source, but are still bounded
    if source.len() > 128 * 1024 * 1024 {
        return Err(Error::Protocol("rendered layer exceeds size limit".into()));
    }
    let mut reader = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(width.into());
    limits.max_image_height = Some(height.into());
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let pixels = reader
        .decode()
        .map_err(|e| Error::Protocol(e.to_string()))?
        .into_rgba8();
    if pixels.dimensions() != (width.into(), height.into()) {
        return Err(Error::Protocol("Display layer dimensions differ".into()));
    }
    Ok(pixels)
}
/// Exact RGBA compositing, including transparent RGB and the existing alpha rounding
pub fn over_color_rgba(image: image::RgbaImage, color: [u8; 4]) -> image::RgbaImage {
    let mut canvas =
        image::RgbaImage::from_pixel(image.width(), image.height(), image::Rgba(color));
    image::imageops::overlay(&mut canvas, &image, 0, 0);
    canvas
}
// Composite an RGBA foreground over the background image
pub fn over_image_rgba(
    mut background: image::RgbaImage,
    foreground: &image::RgbaImage,
) -> Result<image::RgbaImage> {
    if background.dimensions() != foreground.dimensions() {
        return Err(Error::Protocol("Display layer dimensions differ".into()));
    }
    image::imageops::overlay(&mut background, foreground, 0, 0);
    Ok(background)
}
/// Composite alpha before rotating; PNG retains the remaining transparency
pub fn over_color(source: &[u8], color: [u8; 4]) -> Result<Vec<u8>> {
    let image = image::load_from_memory(source)
        .map_err(|e| Error::Protocol(e.to_string()))?
        .to_rgba8();
    let mut canvas =
        image::RgbaImage::from_pixel(image.width(), image.height(), image::Rgba(color));
    image::imageops::overlay(&mut canvas, &image, 0, 0);
    encode_pixels(DynamicImage::ImageRgba8(canvas), "png")
}
/// Composite a decoded animation without a PNG encode/decode round trip
pub fn over_color_pixels(image: DynamicImage, color: [u8; 4]) -> DynamicImage {
    if color[3] == 0 {
        return image;
    }
    if color == [0, 0, 0, 255] {
        return black_matte(image);
    }
    let foreground = image.into_rgba8();
    let mut canvas =
        image::RgbaImage::from_pixel(foreground.width(), foreground.height(), image::Rgba(color));
    image::imageops::overlay(&mut canvas, &foreground, 0, 0);
    DynamicImage::ImageRgba8(canvas)
}
/// Inspect alpha without cloning the usual decoded RGBA buffer
pub fn has_transparency(image: &DynamicImage) -> bool {
    if !image.color().has_alpha() {
        return false;
    }
    if let Some(rgba) = image.as_rgba8() {
        return rgba.pixels().any(|pixel| pixel[3] < 255);
    }
    image.to_rgba8().pixels().any(|pixel| pixel[3] < 255)
}
#[cfg(test)]
/// Blend display content over a media frame in logical orientation
fn over_image(background: &[u8], foreground: &[u8]) -> Result<Vec<u8>> {
    let mut canvas = image::load_from_memory(background)
        .map_err(|e| Error::Protocol(e.to_string()))?
        .to_rgba8();
    let foreground = image::load_from_memory(foreground)
        .map_err(|e| Error::Protocol(e.to_string()))?
        .to_rgba8();
    if canvas.dimensions() != foreground.dimensions() {
        return Err(Error::Protocol("Display layer dimensions differ".into()));
    }
    image::imageops::overlay(&mut canvas, &foreground, 0, 0);
    encode_pixels(DynamicImage::ImageRgba8(canvas), "png")
}
/// Crop in source coordinates, fit/fill in display coordinates, then apply hardware rotation
pub fn edited_pixels(
    source: &[u8],
    width: u16,
    height: u16,
    rotation: u16,
    crop: [f64; 4],
    fit: bool,
    detail: Option<[u16; 2]>,
) -> Result<DynamicImage> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 || source.len() > 16 * 1024 * 1024
    {
        return Err(Error::Protocol(
            "Invalid media dimensions or payload".into(),
        ));
    }
    let [x, y, w, h] = crop;
    if crop.iter().any(|v| !v.is_finite())
        || x < 0.0
        || y < 0.0
        || w <= 0.0
        || h <= 0.0
        || x + w > 1.000001
        || y + h > 1.000001
    {
        return Err(Error::Protocol("Invalid crop".into()));
    }
    let mut image = if is_svg_media(source) {
        let target_edge = f64::from(width.max(height));
        let edge = (target_edge / w.min(h)).ceil().clamp(1.0, 4096.0) as u32;
        let edge = detail.map_or(edge, |bounds| edge.min(u32::from(bounds[1]).clamp(1, 1024)));
        svg_media_pixels(source, edge)?
    } else {
        let mut reader = image::ImageReader::new(Cursor::new(source))
            .with_guessed_format()
            .map_err(|e| Error::Protocol(e.to_string()))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        reader
            .decode()
            .map_err(|e| Error::Protocol(e.to_string()))?
    };
    let sx = ((x * f64::from(image.width())) as u32).min(image.width() - 1);
    let sy = ((y * f64::from(image.height())) as u32).min(image.height() - 1);
    let sw = ((w * f64::from(image.width())).round() as u32)
        .max(1)
        .min(image.width() - sx);
    let sh = ((h * f64::from(image.height())).round() as u32)
        .max(1)
        .min(image.height() - sy);
    if sx != 0 || sy != 0 || sw != image.width() || sh != image.height() {
        image = image.crop_imm(sx, sy, sw, sh);
    }
    // Prepared video is sampled repeatedly on a deadline. Bilinear filtering
    // avoids the six-tap still-image filter on every delivered frame.
    let filter = if detail.is_some() {
        FilterType::Triangle
    } else {
        FilterType::Lanczos3
    };
    if let Some([minimum, maximum]) = detail {
        if minimum == 0 || maximum < minimum || maximum > 1024 {
            return Err(Error::Protocol("Invalid detail bounds".into()));
        }
        let edge = image.width().max(image.height());
        let wanted = edge.clamp(u32::from(minimum), u32::from(maximum));
        if wanted != edge {
            let scale = f64::from(wanted) / f64::from(edge);
            image = image.resize_exact(
                (f64::from(image.width()) * scale).round().max(1.0) as u32,
                (f64::from(image.height()) * scale).round().max(1.0) as u32,
                filter,
            );
        }
    }

    image = if image.width() == u32::from(width) && image.height() == u32::from(height) {
        image
    } else if fit {
        let resized = image.resize(width.into(), height.into(), filter);
        let mut canvas = DynamicImage::new_rgba8(width.into(), height.into());
        image::imageops::overlay(
            &mut canvas,
            &resized,
            i64::from((u32::from(width) - resized.width()) / 2),
            i64::from((u32::from(height) - resized.height()) / 2),
        );
        canvas
    } else {
        image.resize_to_fill(width.into(), height.into(), filter)
    };
    image = match rotation {
        0 => image,
        90 => image.rotate90(),
        180 => image.rotate180(),
        270 => image.rotate270(),
        _ => return Err(Error::Protocol("Invalid rotation".into())),
    };
    Ok(image)
}
/// Canonical tile renderer used by device output and native editor previews
pub fn tile(
    width: u16,
    height: u16,
    color: [u8; 3],
    title: &str,
    rotation: u16,
    format: &str,
) -> Result<Vec<u8>> {
    tile_rgba(
        width,
        height,
        [color[0], color[1], color[2], 255],
        title,
        rotation,
        format,
    )
}
// Repeat the source pixels across an RGBA output image
pub fn tile_rgba(
    width: u16,
    height: u16,
    color: [u8; 4],
    title: &str,
    rotation: u16,
    format: &str,
) -> Result<Vec<u8>> {
    use font8x8::UnicodeFonts;
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err(Error::Protocol("invalid surface dimensions".into()));
    }
    let mut image = image::RgbaImage::new(width.into(), height.into());
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let shade = 0.50 + 0.35 * (1.0 - (x + y) as f32 / (f32::from(width) + f32::from(height)));
        *pixel = image::Rgba([
            (f32::from(color[0]) * shade) as u8,
            (f32::from(color[1]) * shade) as u8,
            (f32::from(color[2]) * shade) as u8,
            color[3],
        ]);
    }
    let scale = if width >= 112 { 2u32 } else { 1 };
    let columns = (u32::from(width).saturating_sub(12) / (8 * scale)).max(1) as usize;
    let chars: Vec<char> = title.chars().take(columns * 3).collect();
    let lines: Vec<_> = chars.chunks(columns).collect();
    let top = (u32::from(height).saturating_sub(lines.len() as u32 * 10 * scale)) / 2;
    for (line_index, line) in lines.iter().enumerate() {
        let left = (u32::from(width).saturating_sub(line.len() as u32 * 8 * scale)) / 2;
        for (index, ch) in line.iter().enumerate() {
            let glyph = font8x8::BASIC_FONTS
                .get(*ch)
                .or_else(|| font8x8::BASIC_FONTS.get('?'))
                .unwrap();
            for (y, bits) in glyph.iter().enumerate() {
                for x in 0..8 {
                    if bits & (1 << x) != 0 {
                        for dy in 0..scale {
                            for dx in 0..scale {
                                let px = left + index as u32 * 8 * scale + x * scale + dx;
                                let py =
                                    top + line_index as u32 * 10 * scale + y as u32 * scale + dy;
                                if px < u32::from(width) && py < u32::from(height) {
                                    image.put_pixel(px, py, image::Rgba([235, 240, 250, 255]));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let image = DynamicImage::ImageRgba8(image);
    let image = match rotation {
        0 => image,
        180 => image.rotate180(),
        90 => image.rotate90(),
        270 => image.rotate270(),
        _ => return Err(Error::Protocol("invalid rotation".into())),
    };
    encode_pixels(image, format)
}
/// Flatten video letterboxing explicitly; legacy transparent media stays unchanged
pub fn black_matte(image: DynamicImage) -> DynamicImage {
    if !image.color().has_alpha() {
        return DynamicImage::ImageRgb8(image.into_rgb8());
    }
    let foreground = image.into_rgba8();
    let mut rgb =
        Vec::with_capacity(foreground.width() as usize * foreground.height() as usize * 3);
    for pixel in foreground.pixels() {
        for channel in &pixel.0[..3] {
            rgb.push((u16::from(*channel) * u16::from(pixel[3]) / 255) as u8);
        }
    }
    DynamicImage::ImageRgb8(
        image::RgbImage::from_raw(foreground.width(), foreground.height(), rgb).unwrap(),
    )
}
// Encode pixels
pub fn encode_pixels(image: DynamicImage, format: &str) -> Result<Vec<u8>> {
    encode_pixels_quality(image, format, 90)
}
// Encode pixels quality
pub fn encode_pixels_quality(image: DynamicImage, format: &str, quality: u8) -> Result<Vec<u8>> {
    if format == "jpeg" {
        let mut bytes = vec![];
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(&image.into_rgb8())
            .map_err(|e| Error::Protocol(e.to_string()))?;
        return Ok(bytes);
    }
    let format = match format {
        "png" => ImageFormat::Png,
        "jpeg" => ImageFormat::Jpeg,
        _ => return Err(Error::Unsupported(format.into())),
    };
    let mut buffer = Cursor::new(Vec::new());
    image
        .write_to(&mut buffer, format)
        .map_err(|e| Error::Protocol(e.to_string()))?;
    Ok(buffer.into_inner())
}
// Render the base layer for this visual
fn visual_base(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    rotation: u16,
) -> Result<Vec<u8>> {
    use base64::Engine;
    if value["layout"].is_object() {
        if let Some(background) = value["encoderBackground"].as_str() {
            let base = visual_base(
                &serde_json::json!({"image":background,"title":""}),
                width,
                height,
                0,
            )?;
            return crate::feedback::draw_on(
                &value["layout"],
                &value["feedback"],
                width,
                height,
                rotation,
                Some(&base),
            );
        }
        return crate::feedback::draw(
            &value["layout"],
            &value["feedback"],
            width,
            height,
            rotation,
        );
    }
    let title = value["title"].as_str().unwrap_or("");
    if value["titleParameters"].is_object() {
        // Reuse the normal image/background path, then draw the vendor state title
        // Binding-owned appearance layers are applied later by the runtime
        let mut base = value.clone();
        base.as_object_mut().unwrap().remove("titleParameters");
        base["title"] = serde_json::json!("");
        let bytes = visual_base(&base, width, height, 0)?;
        let mut pixels = image::load_from_memory(&bytes)
            .map_err(|error| Error::Protocol(error.to_string()))?
            .into_rgba8();
        let title = crate::vendor_title::draw(title, &value["titleParameters"], width, height)?;
        image::imageops::overlay(&mut pixels, &title, 0, 0);
        return encode_pixels(DynamicImage::ImageRgba8(pixels), "png");
    }
    let Some(uri) = value["image"].as_str() else {
        return tile_rgba(
            width,
            height,
            crate::appearance::background_color(value["background"].as_str().unwrap_or("#415a82"))
                .unwrap_or([0; 4]),
            title,
            rotation,
            "png",
        );
    };
    let encoded = uri
        .split_once(";base64,")
        .ok_or_else(|| Error::Unsupported("image data URI".into()))?
        .1;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let bytes = render(&bytes, width, height, 0, "png")?;
    let mut image = image::load_from_memory(&bytes)
        .map_err(|e| Error::Protocol(e.to_string()))?
        .to_rgba8();
    use font8x8::UnicodeFonts;
    let scale = 2u32;
    let max = (u32::from(width) / (8 * scale)).max(1) as usize;
    let chars: Vec<_> = title.chars().take(max).collect();
    let left = (u32::from(width).saturating_sub(chars.len() as u32 * 8 * scale)) / 2;
    let top = (u32::from(height).saturating_sub(8 * scale)) / 2;
    for (index, ch) in chars.iter().enumerate() {
        if let Some(glyph) = font8x8::BASIC_FONTS.get(*ch) {
            for (y, bits) in glyph.iter().enumerate() {
                for x in 0..8 {
                    if bits & (1 << x) != 0 {
                        for dy in 0..scale {
                            for dx in 0..scale {
                                let px = left + index as u32 * 8 * scale + x * scale + dx;
                                let py = top + y as u32 * scale + dy;
                                if px < image.width() && py < image.height() {
                                    image.put_pixel(px, py, image::Rgba([240, 245, 255, 255]));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let image = DynamicImage::ImageRgba8(image);
    let image = match rotation {
        0 => image,
        180 => image.rotate180(),
        90 => image.rotate90(),
        270 => image.rotate270(),
        _ => return Err(Error::Protocol("invalid rotation".into())),
    };
    encode_pixels(image, "png")
}
// Resolve the visual value for this operation
pub fn visual(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    rotation: u16,
) -> Result<Vec<u8>> {
    let bytes = visual_base(value, width, height, 0)?;
    let bytes = if let Some(badge) = value["badge"].as_str() {
        let mut canvas = image::load_from_memory(&bytes)
            .map_err(|e| Error::Protocol(e.to_string()))?
            .to_rgba8();
        let badge = tile(
            40.min(width),
            24.min(height),
            if badge == "OK" {
                [40, 170, 95]
            } else {
                [210, 65, 65]
            },
            badge,
            0,
            "png",
        )?;
        let badge = image::load_from_memory(&badge)
            .map_err(|e| Error::Protocol(e.to_string()))?
            .to_rgba8();
        image::imageops::overlay(&mut canvas, &badge, i64::from(width.saturating_sub(40)), 0);
        encode_pixels(DynamicImage::ImageRgba8(canvas), "png")?
    } else {
        bytes
    };
    if rotation == 0 {
        Ok(bytes)
    } else {
        render(&bytes, width, height, rotation, "png")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify layered pixels match encoded composition at alpha edges and enforce bounds
    fn layered_pixels_match_encoded_composition_at_alpha_edges_and_enforce_bounds() {
        let pixels = image::RgbaImage::from_fn(256, 3, |x, y| {
            image::Rgba([x as u8, (x / 2) as u8, (y * 60) as u8, x as u8])
        });
        let encoded = encode_pixels(DynamicImage::ImageRgba8(pixels.clone()), "png").unwrap();
        assert_eq!(layer_pixels(&encoded, 256, 3).unwrap(), pixels);
        for color in [
            [0, 0, 0, 0],
            [44, 55, 66, 0],
            [0, 0, 0, 255],
            [31, 63, 95, 128],
        ] {
            let legacy = image::load_from_memory(&over_color(&encoded, color).unwrap())
                .unwrap()
                .into_rgba8();
            assert_eq!(over_color_rgba(pixels.clone(), color), legacy);
            let legacy = image::load_from_memory(&over_image(&encoded, &encoded).unwrap())
                .unwrap()
                .into_rgba8();
            assert_eq!(over_image_rgba(pixels.clone(), &pixels).unwrap(), legacy);
        }
        assert!(layer_pixels(&encoded, 255, 3).is_err());
        assert!(layer_pixels(&encoded, 4097, 3).is_err());
        assert!(layer_pixels(&encoded, 0, 3).is_err());
        assert!(layer_pixels(b"not a PNG", 256, 3).is_err());
        assert!(over_image_rgba(pixels, &image::RgbaImage::new(1, 1)).is_err());
    }

    #[test]
    // Verify transparent tiles and images keep alpha through scaling and rotation
    fn transparent_tiles_and_images_keep_alpha_through_scaling_and_rotation() {
        let empty = tile_rgba(64, 32, [20, 50, 80, 0], "", 90, "png").unwrap();
        let image = image::load_from_memory(&empty).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (32, 64));
        assert!(image.pixels().all(|pixel| pixel[3] == 0));
        let title = tile_rgba(64, 32, [20, 50, 80, 0], "Test", 0, "png").unwrap();
        let image = image::load_from_memory(&title).unwrap().to_rgba8();
        assert_eq!(image.get_pixel(0, 0)[3], 0);
        assert!(image.pixels().any(|pixel| pixel[3] == 255));
        let resized = render(&title, 128, 64, 180, "png").unwrap();
        assert_eq!(
            image::load_from_memory(&resized)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            0
        );
        let visual = serde_json::json!({"image": format!("data:image/png;base64,{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &title)), "title":""});
        let rendered = self::visual(&visual, 64, 32, 180).unwrap();
        assert_eq!(
            image::load_from_memory(&rendered)
                .unwrap()
                .to_rgba8()
                .get_pixel(0, 0)[3],
            0
        );
    }
    #[test]
    // Verify both image formats round trip
    fn both_image_formats_round_trip() {
        for f in ["png", "jpeg"] {
            let b = encode_pixels(
                DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    112,
                    112,
                    image::Rgb([25, 60, 80]),
                )),
                f,
            )
            .unwrap();
            let rendered = render(&b, 176, 112, 180, f).unwrap();
            let image = image::load_from_memory(&rendered).unwrap();
            assert_eq!((image.width(), image.height()), (176, 112));
        }
    }
}

#[cfg(test)]
#[test]
// Verify vendor encoder background sits beneath layout and yields to authored background
fn vendor_encoder_background_sits_beneath_layout_and_yields_to_authored_background() {
    use base64::Engine;
    let uri = |color| {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(solid_rgba(20, 10, color).unwrap())
        )
    };
    let mut value = serde_json::json!({
        "encoderBackground":uri([255,0,0,255]),
        "layout":{"background":"#00000000","items":[{"key":"icon","type":"pixmap","rect":[0,0,100,100]}]},
        "feedback":{"icon":uri([0,255,0,255])}
    });
    let decoded = |value: &serde_json::Value| {
        image::load_from_memory(&visual(value, 200, 100, 0).unwrap())
            .unwrap()
            .into_rgba8()
    };
    let frame = decoded(&value);
    assert_eq!(frame.get_pixel(25, 50).0, [0, 255, 0, 255]);
    assert_eq!(frame.get_pixel(175, 50).0, [255, 0, 0, 255]);
    crate::plugin_display::remove_background_layers(&mut value);
    assert!(value.get("encoderBackground").is_none());
    let frame = decoded(&value);
    assert_eq!(frame.get_pixel(25, 50).0, [0, 255, 0, 255]);
    assert_eq!(frame.get_pixel(175, 50)[3], 0);
}
