//! The desktop webview applies EXIF orientation during display. Keep the hardware
//! encoder's compressed pixels intact and add only the inverse display transform.
//! Native snapshot consumers still receive physically upright pixel buffers.
pub(super) enum Container {
    Jpeg,
    Png,
}

impl Container {
    pub(super) fn detect(bytes: &[u8]) -> Option<Self> {
        // Recognize the simple, static PNG and baseline JFIF layouts produced by
        // Manager. Existing orientation, animation, and unfamiliar metadata use the
        // ordinary decoded path. Skip payloads by length, never search inside them.
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR") {
            let mut rest = bytes.get(33..)?;
            let mut pixels = false;
            while rest.len() >= 12 {
                let size = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
                match &rest[4..8] {
                    b"IDAT" => pixels = true,
                    b"IEND" if pixels && size == 0 && rest.len() == 12 => return Some(Self::Png),
                    _ => return None,
                }
                rest = rest.get(12usize.checked_add(size)?..)?;
            }
        } else if bytes.starts_with(b"\xff\xd8\xff\xe0\x00\x10JFIF\0") {
            let mut rest = bytes.get(20..)?;
            while rest.len() >= 4 && rest[0] == 0xff {
                match rest[1] {
                    0xda => return Some(Self::Jpeg),
                    0xc0 | 0xc4 | 0xdb | 0xdd => {
                        let size = usize::from(u16::from_be_bytes([rest[2], rest[3]]));
                        if size < 2 {
                            return None;
                        }
                        rest = rest.get(2 + size..)?;
                    }
                    _ => return None,
                }
            }
        }
        None
    }

    pub(super) fn upright(self, bytes: &[u8], rotation: u16) -> Result<Vec<u8>, String> {
        // One big-endian TIFF IFD entry: Orientation (0x0112), SHORT, count 1.
        let mut tiff = *b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\0\0\0\0\0\0\0";
        tiff[19] = match rotation {
            90 => 8,
            180 => 3,
            270 => 6,
            _ => return Err("Invalid preview rotation".into()),
        };
        let mut result = Vec::with_capacity(bytes.len() + 38);
        match self {
            Self::Jpeg => {
                let (header, image) = bytes.split_at(20);
                result.extend_from_slice(header);
                result.extend_from_slice(b"\xff\xe1\0\x22Exif\0\0");
                result.extend_from_slice(&tiff);
                result.extend_from_slice(image);
            }
            Self::Png => {
                let (header, image) = bytes.split_at(33);
                result.extend_from_slice(header);
                result.extend_from_slice(&(tiff.len() as u32).to_be_bytes());
                let chunk = result.len();
                result.extend_from_slice(b"eXIf");
                result.extend_from_slice(&tiff);
                result.extend_from_slice(&crc32fast::hash(&result[chunk..]).to_be_bytes());
                result.extend_from_slice(image);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageDecoder, ImageReader, Rgba, RgbaImage};
    use std::io::Cursor;

    #[test]
    fn browser_images_decode_to_the_exact_upright_hardware_pixels() {
        let source = DynamicImage::ImageRgba8(RgbaImage::from_fn(19, 11, |x, y| {
            Rgba([
                (11 * x) as u8,
                (18 * y) as u8,
                (x * y) as u8,
                (x * 9 + y) as u8,
            ])
        }));
        for format in ["jpeg", "png"] {
            let bytes = hardware_core::render::encode_pixels(source.clone(), format).unwrap();
            let encoded = image::load_from_memory(&bytes).unwrap();
            for (rotation, upright) in [
                (90, encoded.rotate270()),
                (180, encoded.rotate180()),
                (270, encoded.rotate90()),
            ] {
                let display = Container::detect(&bytes)
                    .unwrap()
                    .upright(&bytes, rotation)
                    .unwrap();
                assert!(Container::detect(&display).is_none());
                let mut decoder = ImageReader::new(Cursor::new(&display))
                    .with_guessed_format()
                    .unwrap()
                    .into_decoder()
                    .unwrap();
                let orientation = decoder.orientation().unwrap();
                let mut pixels = DynamicImage::from_decoder(decoder).unwrap();
                pixels.apply_orientation(orientation);
                assert_eq!(pixels.to_rgba8(), upright.to_rgba8());
            }
            assert!(Container::detect(&bytes[..26]).is_none());
        }
    }
}
