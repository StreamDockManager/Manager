//! Explicit, non-destructive still-image copies. Preview never creates a managed asset
use crate::Runtime;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MediaOptimizationPreview {
    pub source_id: String,
    pub max_edge: u16,
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    #[ts(type = "number")]
    pub original_bytes: u64,
    #[ts(type = "number")]
    pub output_bytes: u64,
    pub image: String,
    pub token: String,
    pub warning: String,
}

impl Runtime {
    // Prepare media optimization
    fn prepare_media_optimization(
        &self,
        asset: &str,
        max_edge: u16,
    ) -> Result<(MediaOptimizationPreview, Vec<u8>), String> {
        if ![512, 1024].contains(&max_edge) {
            return Err("Choose a 512 or 1024 pixel copy".into());
        }
        let _pin = self
            .profiles
            .asset_leases
            .pin([asset.to_owned()], "Optimize media preview")?;
        let extension = self.profiles.asset_extension(asset)?;
        if extension == "sdmedia" {
            return Err("Use the video editor to make an optimized animation copy".into());
        }
        let _gate = self.profiles.media_gate.lock()?;
        let _budget = self
            .profiles
            .media_budget
            .reserve(crate::media_budget::PREPARATION_BYTES)?;
        let path = self
            .profiles
            .root
            .join("assets")
            .join(format!("{asset}.{extension}"));
        let source = crate::media_library::read_regular(&path, crate::media::MAX_SOURCE)?;
        let format = crate::media::detect(&source)?;
        if !["png", "jpeg", "svg"].contains(&format) {
            return Err(
                "Choose a still image; animations cannot be optimized as PNG copies".into(),
            );
        }
        if format == "png"
            && image::codecs::png::PngDecoder::new(std::io::Cursor::new(&source))
                .map_err(|e| e.to_string())?
                .is_apng()
                .map_err(|e| e.to_string())?
        {
            return Err("Animated PNG cannot be optimized as a still-image copy".into());
        }
        let (source_width, source_height) = if format == "svg" {
            hardware_core::render::svg_media_dimensions(&source).map_err(|e| e.to_string())?
        } else {
            image::ImageReader::new(std::io::Cursor::new(&source))
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?
        };
        if source_width == 0 || source_height == 0 {
            return Err("Invalid source dimensions".into());
        }
        let scale = (f64::from(max_edge) / f64::from(source_width.max(source_height))).min(1.0);
        let edge = |value| (f64::from(value) * scale).round().max(1.0) as u16;
        let (width, height) = (edge(source_width), edge(source_height));
        let pixels = hardware_core::render::edited_pixels(
            &source,
            width,
            height,
            0,
            [0.0, 0.0, 1.0, 1.0],
            false,
            None,
        )
        .map_err(|e| e.to_string())?;
        let output =
            hardware_core::render::encode_pixels(pixels, "png").map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        hash.update(b"media-optimization-v1");
        hash.update(asset.as_bytes());
        hash.update(max_edge.to_le_bytes());
        hash.update(Sha256::digest(&source));
        hash.update(Sha256::digest(&output));
        let warning = if format == "svg" {
            "Creates a PNG raster copy; vector scalability is lost. Transparency is preserved."
        } else if scale < 1.0 {
            "Reduces image resolution; fine details may be lost. PNG preserves transparency."
        } else {
            "Keeps the original dimensions and encodes a PNG copy. File size may increase."
        };
        let preview = MediaOptimizationPreview {
            source_id: asset.into(),
            max_edge,
            source_width,
            source_height,
            width: u32::from(width),
            height: u32::from(height),
            original_bytes: source.len() as u64,
            output_bytes: output.len() as u64,
            image: format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&output)
            ),
            token: hex::encode(hash.finalize()),
            warning: warning.into(),
        };
        Ok((preview, output))
    }

    // Preview media optimization using the current media optimization state
    pub fn preview_media_optimization(
        &self,
        asset: &str,
        max_edge: u16,
    ) -> Result<MediaOptimizationPreview, String> {
        self.prepare_media_optimization(asset, max_edge)
            .map(|(preview, _)| preview)
    }

    // Create optimized media copy
    pub fn create_optimized_media_copy(
        &self,
        asset: &str,
        max_edge: u16,
        token: &str,
    ) -> Result<String, String> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid optimization preview token".into());
        }
        let (preview, output) = self.prepare_media_optimization(asset, max_edge)?;
        if preview.token != token {
            return Err(
                "Media or copy settings changed. Preview the copy again before saving.".into(),
            );
        }
        // Staging exists only during the explicit save and is always removed afterwards
        let mut staged = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        staged.write_all(&output).map_err(|e| e.to_string())?;
        self.profiles.import_asset(staged.path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgba, RgbaImage};

    // Count asset-directory entries for optimization fixture assertions
    fn asset_count(runtime: &Runtime) -> usize {
        std::fs::read_dir(runtime.profiles.root.join("assets"))
            .unwrap()
            .count()
    }

    #[test]
    // Verify that optimized copy is previewed measured and never replaces original
    fn optimized_copy_is_previewed_measured_and_never_replaces_original() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        let source = root.path().join("source.png");
        RgbaImage::from_pixel(1200, 600, Rgba([60, 30, 10, 100]))
            .save(&source)
            .unwrap();
        let original_bytes = std::fs::read(&source).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let count = asset_count(&runtime);
        let preview = runtime.preview_media_optimization(&asset, 512).unwrap();
        assert_eq!(
            asset_count(&runtime),
            count,
            "preview must not create managed files"
        );
        assert_eq!((preview.width, preview.height), (512, 256));
        assert_eq!((preview.source_width, preview.source_height), (1200, 600));
        assert_eq!(preview.original_bytes, original_bytes.len() as u64);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(preview.image.split_once(',').unwrap().1)
            .unwrap();
        assert_eq!(preview.output_bytes, bytes.len() as u64);
        assert_eq!(
            image::load_from_memory(&bytes).unwrap().get_pixel(0, 0).0[3],
            100
        );
        assert!(runtime
            .create_optimized_media_copy(&asset, 1024, &preview.token)
            .is_err());
        let copied = runtime
            .create_optimized_media_copy(&asset, 512, &preview.token)
            .unwrap();
        assert_ne!(copied, asset);
        assert_eq!(runtime.profiles.asset(&copied).unwrap(), bytes);
        assert_eq!(runtime.profiles.asset(&asset).unwrap(), original_bytes);
        assert_eq!(asset_count(&runtime), count + 1);
    }

    #[test]
    // Verify that no upscale SVG warning and validation
    fn no_upscale_svg_warning_and_validation() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        let source = root.path().join("source.svg");
        std::fs::write(&source, r#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="40"><rect width="20" height="20" fill="red"/></svg>"#).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let preview = runtime.preview_media_optimization(&asset, 1024).unwrap();
        assert_eq!((preview.width, preview.height), (80, 40));
        assert!(preview.warning.contains("vector"));
        assert!(runtime
            .preview_media_optimization("../../etc/passwd", 512)
            .is_err());
        assert!(runtime.preview_media_optimization(&asset, 4096).is_err());
        assert!(runtime
            .create_optimized_media_copy(&asset, 1024, "invalid")
            .is_err());
        let source = runtime
            .profiles
            .root
            .join("assets")
            .join(format!("{asset}.svg"));
        std::fs::write(
            source,
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"/>"#,
        )
        .unwrap();
        assert!(runtime
            .create_optimized_media_copy(&asset, 1024, &preview.token)
            .is_err());
    }

    #[test]
    // Verify that animations are not silently flattened
    fn animations_are_not_silently_flattened() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        let source = root.path().join("source.gif");
        let file = std::fs::File::create(&source).unwrap();
        let mut encoder = image::codecs::gif::GifEncoder::new(file);
        encoder
            .encode_frame(image::Frame::new(RgbaImage::from_pixel(
                16,
                16,
                Rgba([255, 0, 0, 255]),
            )))
            .unwrap();
        drop(encoder);
        let asset = runtime.profiles.import_asset(&source).unwrap();
        assert!(runtime
            .preview_media_optimization(&asset, 512)
            .unwrap_err()
            .contains("video editor"));
    }
}
