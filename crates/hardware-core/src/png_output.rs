//! Lossless PNG compression at the final hardware boundary, not between render layers
use crate::{Error, Result};
use image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    DynamicImage,
};
use std::{borrow::Cow, io::Cursor};

/// Encode already transformed device pixels without changing color depth or alpha
pub fn encode(pixels: &DynamicImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pixels
        .write_with_encoder(PngEncoder::new_with_quality(
            &mut bytes,
            CompressionType::Default,
            FilterType::Adaptive,
        ))
        .map_err(|error| Error::Protocol(error.to_string()))?;
    Ok(bytes)
}

/// Compact a rendered PNG when the model sends it without another pixel transform
/// Other wire formats pass through. Keep the existing PNG if recompression grows it
pub fn compact(source: &[u8]) -> Result<Cow<'_, [u8]>> {
    if !source.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(Cow::Borrowed(source));
    }
    if source.len() > 16 * 1024 * 1024 {
        return Err(Error::Protocol("device PNG exceeds 16 MiB".into()));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(source), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let pixels = reader
        .decode()
        .map_err(|error| Error::Protocol(error.to_string()))?;
    let bytes = encode(&pixels)?;
    Ok(if bytes.len() < source.len() {
        Cow::Owned(bytes)
    } else {
        Cow::Borrowed(source)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify device compression preserves pixels alpha and sixteen bit samples
    fn device_compression_preserves_pixels_alpha_and_sixteen_bit_samples() {
        let rgba = image::RgbaImage::from_fn(112, 112, |x, y| {
            image::Rgba([x as u8, y as u8, 137, (x + y) as u8])
        });
        let gray = image::ImageBuffer::from_fn(112, 112, |x, y| {
            image::Luma([((x * 173 + y * 257) % 65536) as u16])
        });
        for pixels in [
            DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba.clone()).into_rgb8()),
            DynamicImage::ImageRgba8(rgba),
            DynamicImage::ImageLuma16(gray),
        ] {
            let fast = crate::render::encode_pixels(pixels.clone(), "png").unwrap();
            let compacted = compact(&fast).unwrap();
            let decoded = image::load_from_memory(&compacted).unwrap();
            assert_eq!(decoded.color(), pixels.color());
            assert_eq!(decoded.as_bytes(), pixels.as_bytes());
            assert_eq!((decoded.width(), decoded.height()), (112, 112));
            assert!(compacted.len() <= fast.len());
        }
    }

    #[test]
    // Verify labeled key uses fewer packets without changing the fast renderer
    fn labeled_key_uses_fewer_packets_without_changing_the_fast_renderer() {
        let fast = crate::render::tile(112, 112, [36, 87, 178], "A key-1", 180, "png").unwrap();
        let compacted = compact(&fast).unwrap();
        assert!(compacted.len().div_ceil(1024) * 3 < fast.len().div_ceil(1024));
        assert_eq!(
            image::load_from_memory(&compacted).unwrap(),
            image::load_from_memory(&fast).unwrap()
        );
        let again = compact(&compacted).unwrap();
        assert!(
            matches!(again, Cow::Borrowed(_)),
            "An already compact PNG is reused"
        );
    }

    #[test]
    // Verify JPEG is borrowed and invalid or oversized PNG is rejected
    fn jpeg_is_borrowed_and_invalid_or_oversized_png_is_rejected() {
        let jpeg = crate::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                112,
                112,
                image::Rgb([20, 40, 60]),
            )),
            "jpeg",
        )
        .unwrap();
        let unchanged = compact(&jpeg).unwrap();
        assert!(matches!(unchanged, Cow::Borrowed(_)));
        assert_eq!(&*unchanged, jpeg);
        assert!(compact(b"\x89PNG\r\n\x1a\ntruncated").is_err());
        let wide = crate::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                4097,
                1,
                image::Rgb([0; 3]),
            )),
            "png",
        )
        .unwrap();
        assert!(compact(&wide).is_err());
    }
}
