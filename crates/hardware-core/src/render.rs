#[path = "gradient_wallpaper.rs"]
mod gradient_wallpaper;

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
    pixmap_rgba(pixmap).map(DynamicImage::ImageRgba8)
}

/// Convert tiny-skia's premultiplied buffer to the straight RGBA used by image.
pub(crate) fn pixmap_rgba(pixmap: resvg::tiny_skia::Pixmap) -> Result<image::RgbaImage> {
    let (width, height) = (pixmap.width(), pixmap.height());
    let mut bytes = pixmap.take();
    for pixel in bytes.as_chunks_mut::<4>().0 {
        // Fully transparent and opaque premultiplied pixels are already straight RGBA.
        if (1..255).contains(&pixel[3]) {
            let alpha = f64::from(pixel[3]) / 255.;
            for channel in &mut pixel[..3] {
                *channel = (f64::from(*channel) / alpha + 0.5) as u8;
            }
        }
    }
    image::RgbaImage::from_raw(width, height, bytes)
        .ok_or_else(|| Error::Protocol("Invalid pixmap dimensions".into()))
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
    encode_pixels(
        rotate_pixels(render_pixels(source, width, height)?, rotation)?,
        format,
    )
}

pub(crate) fn render_pixels(source: &[u8], width: u16, height: u16) -> Result<DynamicImage> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err(Error::Protocol("invalid image dimensions".into()));
    }
    if source.len() > 16 * 1024 * 1024 {
        return Err(Error::Protocol("asset exceeds 16 MiB".into()));
    }
    if is_svg_media(source) {
        return draw_svg(&plugin_svg_tree(source)?, width, height);
    }
    Ok(resize_raster(decode_raster(source)?, width, height))
}

fn plugin_svg_tree(source: &[u8]) -> Result<resvg::usvg::Tree> {
    let mut options = resvg::usvg::Options::default();
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    options.image_href_resolver.resolve_data = Box::new(|_, _, _| None);
    resvg::usvg::Tree::from_data(source, &options).map_err(|e| Error::Protocol(e.to_string()))
}

fn draw_svg(tree: &resvg::usvg::Tree, width: u16, height: u16) -> Result<DynamicImage> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width.into(), height.into())
        .ok_or_else(|| Error::Protocol("SVG output allocation failed".into()))?;
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_scale(
            f32::from(width) / tree.size().width(),
            f32::from(height) / tree.size().height(),
        ),
        &mut pixmap.as_mut(),
    );
    pixmap_rgba(pixmap).map(DynamicImage::ImageRgba8)
}

fn decode_raster(source: &[u8]) -> Result<DynamicImage> {
    let mut reader = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|e| Error::Protocol(e.to_string()))
}

fn resize_raster(image: DynamicImage, width: u16, height: u16) -> DynamicImage {
    if image.width() == u32::from(width) && image.height() == u32::from(height) {
        image
    } else {
        image.resize_to_fill(width.into(), height.into(), FilterType::Lanczos3)
    }
}

/// Fit styled plugin artwork without reopening the raster or parsing the SVG a second time.
pub(crate) fn contained_pixels(source: &[u8], width: u16, height: u16) -> Result<image::RgbaImage> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 || source.len() > 16 * 1024 * 1024
    {
        return Err(Error::Protocol(
            "Invalid artwork dimensions or payload".into(),
        ));
    }
    let fit = |w: f32, h: f32| {
        let scale = (f32::from(width) / w).min(f32::from(height) / h);
        (
            (w * scale).round().max(1.) as u16,
            (h * scale).round().max(1.) as u16,
        )
    };
    let pixels = if is_svg_media(source) {
        let tree = plugin_svg_tree(source)?;
        let (w, h) = fit(tree.size().width(), tree.size().height());
        draw_svg(&tree, w, h)?
    } else {
        let image = decode_raster(source)?;
        let (w, h) = fit(image.width() as f32, image.height() as f32);
        resize_raster(image, w, h)
    };
    let x = (u32::from(width) - pixels.width()) / 2;
    let y = (u32::from(height) - pixels.height()) / 2;
    let mut canvas = image::RgbaImage::new(width.into(), height.into());
    image::imageops::overlay(&mut canvas, &pixels.into_rgba8(), x.into(), y.into());
    Ok(canvas)
}

pub(crate) fn rotate_pixels(image: DynamicImage, rotation: u16) -> Result<DynamicImage> {
    match rotation {
        0 => Ok(image),
        90 => Ok(image.rotate90()),
        180 => Ok(image.rotate180()),
        270 => Ok(image.rotate270()),
        _ => Err(Error::Protocol("invalid rotation".into())),
    }
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
pub fn over_color_rgba(mut image: image::RgbaImage, color: [u8; 4]) -> image::RgbaImage {
    use image::Pixel;
    for pixel in image.pixels_mut() {
        match pixel[3] {
            255 => {}
            0 => pixel.0 = color,
            _ => {
                let mut background = image::Rgba(color);
                background.blend(pixel);
                *pixel = background;
            }
        }
    }
    image
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
    validate_edit(width, height, crop)?;
    if source.len() > 16 * 1024 * 1024 {
        return Err(Error::Protocol("Invalid media payload".into()));
    }
    let pixels = if is_svg_media(source) {
        let target_edge = f64::from(width.max(height));
        let edge = (target_edge / crop[2].min(crop[3]))
            .ceil()
            .clamp(1.0, 4096.0) as u32;
        let edge = detail.map_or(edge, |bounds| edge.min(u32::from(bounds[1]).clamp(1, 1024)));
        svg_media_pixels(source, edge)?
    } else {
        decode_raster_pixels(source)?
    };
    transform_pixels(
        std::borrow::Cow::Owned(pixels),
        width,
        height,
        rotation,
        crop,
        fit,
        detail,
    )
}

pub fn decode_raster_pixels(source: &[u8]) -> Result<DynamicImage> {
    if source.len() > 16 * 1024 * 1024 {
        return Err(Error::Protocol("Invalid media payload".into()));
    }
    let mut reader = image::ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|e| Error::Protocol(e.to_string()))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|e| Error::Protocol(e.to_string()))
}

fn validate_edit(width: u16, height: u16, crop: [f64; 4]) -> Result<()> {
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
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
    Ok(())
}

/// Transform a validated prepared raster without decoding it again for each crop.
pub fn edited_decoded_pixels(
    source: &DynamicImage,
    width: u16,
    height: u16,
    rotation: u16,
    crop: [f64; 4],
    fit: bool,
    detail: Option<[u16; 2]>,
) -> Result<DynamicImage> {
    validate_edit(width, height, crop)?;
    if source.width() == 0 || source.height() == 0 {
        return Err(Error::Protocol("Empty raster".into()));
    }
    transform_pixels(
        std::borrow::Cow::Borrowed(source),
        width,
        height,
        rotation,
        crop,
        fit,
        detail,
    )
}
fn transform_pixels(
    mut image: std::borrow::Cow<'_, DynamicImage>,
    width: u16,
    height: u16,
    rotation: u16,
    crop: [f64; 4],
    fit: bool,
    detail: Option<[u16; 2]>,
) -> Result<DynamicImage> {
    let [x, y, w, h] = crop;
    let sx = ((x * f64::from(image.width())) as u32).min(image.width() - 1);
    let sy = ((y * f64::from(image.height())) as u32).min(image.height() - 1);
    let sw = ((w * f64::from(image.width())).round() as u32)
        .max(1)
        .min(image.width() - sx);
    let sh = ((h * f64::from(image.height())).round() as u32)
        .max(1)
        .min(image.height() - sy);
    if sx != 0 || sy != 0 || sw != image.width() || sh != image.height() {
        image = std::borrow::Cow::Owned(image.crop_imm(sx, sy, sw, sh));
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
            image = std::borrow::Cow::Owned(image.resize_exact(
                (f64::from(image.width()) * scale).round().max(1.0) as u32,
                (f64::from(image.height()) * scale).round().max(1.0) as u32,
                filter,
            ));
        }
    }

    let image = if image.width() == u32::from(width) && image.height() == u32::from(height) {
        image.into_owned()
    } else if fit {
        let resized = image.resize(width.into(), height.into(), filter);
        let x = (u32::from(width) - resized.width()) / 2;
        let y = (u32::from(height) - resized.height()) / 2;
        let mut canvas = image::RgbaImage::new(width.into(), height.into());
        let foreground = resized.into_rgba8();
        if foreground.pixels().all(|pixel| pixel[3] == 255) {
            // A fitted opaque frame can be copied one scanline at a time. Going
            // through DynamicImage::blend_pixel converts every pixel twice.
            let stride = usize::from(width) * 4;
            let row_bytes = foreground.width() as usize * 4;
            for (row, source) in foreground.as_raw().chunks_exact(row_bytes).enumerate() {
                let start = (y as usize + row) * stride + x as usize * 4;
                canvas.as_mut()[start..start + row_bytes].copy_from_slice(source);
            }
        } else {
            image::imageops::overlay(&mut canvas, &foreground, x.into(), y.into());
        }
        DynamicImage::ImageRgba8(canvas)
    } else {
        image.resize_to_fill(width.into(), height.into(), filter)
    };
    let image = match rotation {
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
    let pixels = tile_pixels(width, height, color, title)?;
    encode_pixels(
        rotate_pixels(DynamicImage::ImageRgba8(pixels), rotation)?,
        format,
    )
}

fn tile_pixels(width: u16, height: u16, color: [u8; 4], title: &str) -> Result<image::RgbaImage> {
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
    Ok(image)
}
/// Flatten video letterboxing explicitly; legacy transparent media stays unchanged
pub fn black_matte(image: DynamicImage) -> DynamicImage {
    if !image.color().has_alpha() {
        return DynamicImage::ImageRgb8(image.into_rgb8());
    }
    let foreground = image.into_rgba8();
    let mut rgb = image::RgbImage::new(foreground.width(), foreground.height());
    for (destination, source) in rgb.pixels_mut().zip(foreground.pixels()) {
        match source[3] {
            255 => destination.0.copy_from_slice(&source.0[..3]),
            0 => {}
            alpha => {
                for (out, &channel) in destination.0.iter_mut().zip(&source.0) {
                    *out = (u16::from(channel) * u16::from(alpha) / 255) as u8;
                }
            }
        }
    }
    DynamicImage::ImageRgb8(rgb)
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
pub type ArtworkResolver<'a> = dyn Fn(&str) -> Result<Option<Vec<u8>>> + 'a;

pub(crate) fn image_bytes(uri: &str, resolve: &ArtworkResolver<'_>) -> Result<Option<Vec<u8>>> {
    use base64::Engine;
    let Some((_, encoded)) = uri.split_once(";base64,") else {
        return Ok(None);
    };
    if let Some(bytes) = resolve(uri)? {
        return Ok(Some(bytes));
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map(Some)
        .map_err(|e| Error::Protocol(e.to_string()))
}

fn visual_base_image(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    resolve: &ArtworkResolver<'_>,
) -> Result<DynamicImage> {
    if value["layout"].is_object() {
        return crate::feedback::draw_pixels(
            &value["layout"],
            &value["feedback"],
            width,
            height,
            resolve,
            || {
                value["encoderBackground"]
                    .as_str()
                    .map(|background| {
                        visual_base_image(
                            &serde_json::json!({"image":background}),
                            width,
                            height,
                            resolve,
                        )
                        .map(DynamicImage::into_rgba8)
                    })
                    .transpose()
            },
        )
        .map(DynamicImage::ImageRgba8);
    }
    let title = value["title"].as_str().unwrap_or("");
    let parameters = &value["titleParameters"];
    let image = plugin_image_or_tile(
        value,
        width,
        height,
        resolve,
        if parameters.is_object() { "" } else { title },
    )?;
    if parameters.is_object() {
        if let Some(layer) = crate::vendor_title::draw(title, parameters, width, height)? {
            let mut pixels = image.into_rgba8();
            image::imageops::overlay(&mut pixels, &layer, 0, 0);
            return Ok(DynamicImage::ImageRgba8(pixels));
        }
    }
    Ok(image)
}

fn plugin_image_or_tile(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    resolve: &ArtworkResolver<'_>,
    title: &str,
) -> Result<DynamicImage> {
    let Some(uri) = value["image"].as_str() else {
        return tile_pixels(
            width,
            height,
            crate::appearance::background_color(value["background"].as_str().unwrap_or("#415a82"))
                .unwrap_or([0; 4]),
            title,
        )
        .map(DynamicImage::ImageRgba8);
    };
    let bytes =
        image_bytes(uri, resolve)?.ok_or_else(|| Error::Unsupported("image data URI".into()))?;
    let image = render_pixels(&bytes, width, height)?;
    // Keep opaque decoder output in RGB while painting its title. Any other
    // source type still follows the existing eight-bit RGBA conversion.
    if let DynamicImage::ImageRgb8(mut pixels) = image {
        draw_image_title(&mut pixels, title, image::Rgb([240, 245, 255]));
        Ok(DynamicImage::ImageRgb8(pixels))
    } else {
        let mut pixels = image.into_rgba8();
        draw_image_title(&mut pixels, title, image::Rgba([240, 245, 255, 255]));
        Ok(DynamicImage::ImageRgba8(pixels))
    }
}

fn draw_image_title<P: image::Pixel<Subpixel = u8>>(
    image: &mut image::ImageBuffer<P, Vec<u8>>,
    title: &str,
    ink: P,
) {
    if title.is_empty() {
        return;
    }
    let (width, height) = image.dimensions();
    use font8x8::UnicodeFonts;
    let scale = 2u32;
    let max = (width / (8 * scale)).max(1) as usize;
    let chars: Vec<_> = title.chars().take(max).collect();
    let left = (width.saturating_sub(chars.len() as u32 * 8 * scale)) / 2;
    let top = (height.saturating_sub(8 * scale)) / 2;
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
                                    image.put_pixel(px, py, ink);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
// Resolve the visual value for this operation
pub fn visual(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    rotation: u16,
) -> Result<Vec<u8>> {
    visual_resolved(value, width, height, rotation, &|_| Ok(None))
}

pub fn visual_resolved(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    rotation: u16,
    resolve: &ArtworkResolver<'_>,
) -> Result<Vec<u8>> {
    visual_surface_resolved(value, width, height, rotation, resolve, None)
}

/// Paint a plugin output directly in its final color model. An opaque image
/// covers the binding fill, so neither target needs an intermediate alpha plane.
pub fn visual_surface_resolved(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    rotation: u16,
    resolve: &ArtworkResolver<'_>,
    background: Option<[u8; 4]>,
) -> Result<Vec<u8>> {
    encode_plugin_surface(
        plugin_surface_pixels(value, width, height, resolve, background)?,
        rotation,
    )
}

fn plugin_surface_pixels(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    resolve: &ArtworkResolver<'_>,
    background: Option<[u8; 4]>,
) -> Result<DynamicImage> {
    let image = visual_image_resolved(value, width, height, resolve)?;
    Ok(match (image, background) {
        (DynamicImage::ImageRgba8(pixels), Some(color)) => {
            DynamicImage::ImageRgba8(over_color_rgba(pixels, color))
        }
        (image, _) => image,
    })
}

/// Final lossless payload for the small N4 Pro key. The fast Paeth alternative
/// only needs comparison when a different encoder could save a payload report.
pub fn compact_plugin_key(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    rotation: u16,
    resolve: &ArtworkResolver<'_>,
    background: [u8; 4],
) -> Result<Vec<u8>> {
    let pixels = plugin_surface_pixels(value, width, height, resolve, Some(background))?;
    let pixels = oriented_plugin_pixels(pixels, rotation)?;
    let compact = crate::png_output::encode_key(&pixels)?;
    if compact.len() <= 1024 {
        return Ok(compact);
    }
    let fast = fast_plugin_png(&pixels)?;
    Ok(if compact.len() <= fast.len() {
        compact
    } else {
        fast
    })
}

fn encode_plugin_surface(image: DynamicImage, rotation: u16) -> Result<Vec<u8>> {
    fast_plugin_png(&oriented_plugin_pixels(image, rotation)?)
}

fn oriented_plugin_pixels(image: DynamicImage, rotation: u16) -> Result<DynamicImage> {
    let mut pixels = match image {
        DynamicImage::ImageRgba8(ref rgba)
            if rgba.as_raw().as_chunks::<4>().0.iter().all(|p| p[3] == 255) =>
        {
            DynamicImage::ImageRgb8(image.into_rgb8())
        }
        _ => image,
    };
    match (&mut pixels, rotation) {
        (DynamicImage::ImageRgb8(rgb), 180) => rgb.as_mut().as_chunks_mut::<3>().0.reverse(),
        (DynamicImage::ImageRgba8(rgba), 180) => rgba.as_mut().as_chunks_mut::<4>().0.reverse(),
        _ => pixels = rotate_pixels(pixels, rotation)?,
    }
    Ok(pixels)
}

fn fast_plugin_png(pixels: &DynamicImage) -> Result<Vec<u8>> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let mut bytes = Vec::new();
    PngEncoder::new_with_quality(&mut bytes, CompressionType::Fast, FilterType::Paeth)
        .write_image(
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            pixels.color().into(),
        )
        .map_err(|e| Error::Protocol(e.to_string()))?;
    Ok(bytes)
}

/// Rasterize one logical plugin surface for further composition before wire encoding.
pub fn visual_pixels_resolved(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    resolve: &ArtworkResolver<'_>,
) -> Result<image::RgbaImage> {
    visual_image_resolved(value, width, height, resolve).map(DynamicImage::into_rgba8)
}

fn visual_image_resolved(
    value: &serde_json::Value,
    width: u16,
    height: u16,
    resolve: &ArtworkResolver<'_>,
) -> Result<DynamicImage> {
    let canvas = visual_base_image(value, width, height, resolve)?;
    if let Some(badge) = value["badge"].as_str() {
        let color = if badge == "OK" {
            [40, 170, 95, 255]
        } else {
            [210, 65, 65, 255]
        };
        let mut canvas = canvas.into_rgba8();
        let badge = tile_pixels(40.min(width), 24.min(height), color, badge)?;
        image::imageops::overlay(&mut canvas, &badge, i64::from(width.saturating_sub(40)), 0);
        return Ok(DynamicImage::ImageRgba8(canvas));
    }
    Ok(canvas)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plugin_png_preserves_pixels_for_each_rotation_and_alpha() {
        for alpha in [0, 73, 254, 255] {
            let pixels = image::RgbaImage::from_fn(9, 5, |x, y| {
                image::Rgba([
                    x as u8,
                    (x + 20 * y) as u8,
                    17,
                    if x == 3 { alpha } else { 255 },
                ])
            });
            for rotation in [0, 90, 180, 270] {
                let expected =
                    rotate_pixels(DynamicImage::ImageRgba8(pixels.clone()), rotation).unwrap();
                let encoded =
                    encode_plugin_surface(DynamicImage::ImageRgba8(pixels.clone()), rotation)
                        .unwrap();
                let decoded = image::load_from_memory(&encoded).unwrap();
                assert_eq!(decoded.color().has_alpha(), alpha != 255);
                assert_eq!(decoded.into_rgba8(), expected.into_rgba8());
            }
        }
    }

    #[test]
    fn svg_alpha_conversion_matches_tiny_skia_for_all_premultiplied_channels() {
        use resvg::tiny_skia::{Pixmap, PremultipliedColorU8};
        let mut pixmap = Pixmap::new(256, 256).unwrap();
        for alpha in 0..=u8::MAX {
            for value in 0..=alpha {
                let channel =
                    PremultipliedColorU8::from_rgba(value, alpha - value, alpha, alpha).unwrap();
                pixmap.pixels_mut()[usize::from(alpha) * 256 + usize::from(value)] = channel;
            }
        }
        let expected = pixmap.clone().take_demultiplied();
        assert_eq!(pixmap_rgba(pixmap).unwrap().as_raw(), &expected);
    }

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

/// Render the generated palette wash with stable sub-byte gradient quantization.
pub fn render_palette_wallpaper(
    source: &str,
    width: u16,
    height: u16,
    rotation: u16,
) -> Result<Vec<u8>> {
    gradient_wallpaper::render(source, width, height, rotation)
}

/// Generated gradient provenance only selects a fidelity policy, not trust or validation.
pub fn is_generated_palette_wallpaper(source: &[u8]) -> bool {
    gradient_wallpaper::is_generated(source)
}

#[test]
fn final_plugin_payload_preserves_pixels_and_padded_reports() {
    use base64::Engine;
    let mut kept_original = false;
    for style in 0..6 {
        let rgba = image::RgbaImage::from_fn(112, 112, |x, y| {
            let n = (x.wrapping_mul(19) ^ y.wrapping_mul(373)).wrapping_mul(2654435761);
            match style {
                0 => image::Rgba([x as u8, y as u8, (x + y) as u8, 255]),
                1 => image::Rgba([n as u8, (n >> 8) as u8, (n >> 16) as u8, 255]),
                2 => image::Rgba([30, 50, 90, (x + y) as u8]),
                3 => {
                    let v = (x.wrapping_mul(2654435761) >> 16).wrapping_add(y * (x % 3 + 1)) as u8;
                    image::Rgba([
                        v,
                        v.wrapping_add((x * 7) as u8),
                        v.wrapping_add(x as u8),
                        255,
                    ])
                }
                4 => {
                    // Independent vertical random walks have inexpensive Paeth residuals
                    // and poorly correlated neighbors in the horizontal Sub filter.
                    let mut state = (x + 1).wrapping_mul(2654435761);
                    let mut rgb = [state as u8, (state >> 8) as u8, (state >> 16) as u8];
                    for _ in 0..y {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        for (channel, value) in rgb.iter_mut().enumerate() {
                            *value = value
                                .wrapping_add(((state >> (channel * 8)) % 3) as u8)
                                .wrapping_sub(1);
                        }
                    }
                    image::Rgba([rgb[0], rgb[1], rgb[2], 255])
                }
                _ => image::Rgba([(y * 2) as u8, (y * 2) as u8, (y * 2) as u8, 255]),
            }
        });
        let input = encode_pixels(DynamicImage::ImageRgba8(rgba), "png").unwrap();
        let visual = serde_json::json!({"image":format!("data:image/png;base64,{}",base64::engine::general_purpose::STANDARD.encode(input))});
        for rotation in [0, 90, 180, 270] {
            let fill = [0, 0, 0, 0];
            let old =
                visual_surface_resolved(&visual, 112, 112, rotation, &|_| Ok(None), Some(fill))
                    .unwrap();
            let old_payload = crate::png_output::compact(&old).unwrap();
            kept_original |= matches!(old_payload, std::borrow::Cow::Borrowed(_));
            let direct =
                compact_plugin_key(&visual, 112, 112, rotation, &|_| Ok(None), fill).unwrap();
            assert_eq!(
                image::load_from_memory(&old_payload).unwrap(),
                image::load_from_memory(&direct).unwrap()
            );
            assert!(direct.len().div_ceil(1024) <= old_payload.len().div_ceil(1024));
        }
    }
    assert!(kept_original);
}
