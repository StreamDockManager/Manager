//! PNG is the UI default. Photographic JPEG is opt-in or must save 20% at quality 90.
#[derive(Default, Debug, Clone, Copy, serde::Serialize)]
pub struct EncodingWork {
    pub decodes: u64,
    pub alpha_scans: u64,
    pub jpeg_encodes: u64,
}
static DECODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ALPHA_SCANS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static JPEG_ENCODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Process-cumulative operation counts; bounded cardinality and no asset identities
pub fn work_stats() -> EncodingWork {
    use std::sync::atomic::Ordering::Relaxed;
    EncodingWork {
        decodes: DECODES.load(Relaxed),
        alpha_scans: ALPHA_SCANS.load(Relaxed),
        jpeg_encodes: JPEG_ENCODES.load(Relaxed),
    }
}
// Choose recorded
fn choose_recorded(
    png: crate::media::SharedBytes,
    supports_jpeg: bool,
    policy: &str,
    photographic: bool,
    validated_png: bool,
) -> Result<crate::media::SharedBytes, String> {
    choose_shared_quality(png, supports_jpeg, policy, photographic, validated_png, 90)
}
/// Encode with an explicit quality and whether the PNG has already passed validation
pub(crate) fn choose_shared_quality(
    png: crate::media::SharedBytes,
    supports_jpeg: bool,
    policy: &str,
    photographic: bool,
    validated_png: bool,
    quality: u8,
) -> Result<crate::media::SharedBytes, String> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut work = EncodingWork::default();
    let result = choose_inner_quality(
        png,
        supports_jpeg,
        policy,
        photographic,
        validated_png,
        &mut work,
        quality,
    );
    DECODES.fetch_add(work.decodes, Relaxed);
    ALPHA_SCANS.fetch_add(work.alpha_scans, Relaxed);
    JPEG_ENCODES.fetch_add(work.jpeg_encodes, Relaxed);
    result
}
#[cfg(test)]
// Choose inner
fn choose_inner(
    png: Vec<u8>,
    supports_jpeg: bool,
    policy: &str,
    photographic: bool,
    validated_png: bool,
    work: &mut EncodingWork,
) -> Result<Vec<u8>, String> {
    choose_inner_quality(
        crate::media::shared_bytes(png),
        supports_jpeg,
        policy,
        photographic,
        validated_png,
        work,
        90,
    )
    .map(|bytes| bytes.to_vec())
}
// Choose inner quality
fn choose_inner_quality(
    png: crate::media::SharedBytes,
    supports_jpeg: bool,
    policy: &str,
    photographic: bool,
    validated_png: bool,
    work: &mut EncodingWork,
    quality: u8,
) -> Result<crate::media::SharedBytes, String> {
    if !["auto", "png", "jpeg"].contains(&policy) {
        return Err("Unknown image encoding policy".into());
    }
    if png.len() > crate::media::MAX_ASSET {
        return Err("Image exceeds encoded size limit".into());
    }
    let retain_png = policy == "png" || policy == "auto" && (!photographic || !supports_jpeg);
    // Only crate-internal renderer output may use this boundary; never imported/plugin bytes
    if validated_png && retain_png && png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(png);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(&**png))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    work.decodes += 1;
    let image = reader.decode().map_err(|e| e.to_string())?;
    if retain_png {
        return Ok(png);
    }
    // Preserve transparent PNG fallback even for an explicit unsupported JPEG request
    if image.color().has_alpha() {
        work.alpha_scans += 1;
        let transparent = if let Some(rgba) = image.as_rgba8() {
            rgba.pixels().any(|pixel| pixel[3] < 255)
        } else {
            conversion_budget(&image, 4)?;
            image.to_rgba8().pixels().any(|pixel| pixel[3] < 255)
        };
        if transparent {
            return Ok(png);
        }
    }
    if !supports_jpeg {
        return Err("This device does not support JPEG output".into());
    }
    if image.as_rgb8().is_none() {
        conversion_budget(&image, 3)?;
    }
    let image = image.into_rgb8();
    work.jpeg_encodes += 1;
    let jpeg = hardware_core::render::encode_pixels_quality(
        image::DynamicImage::ImageRgb8(image),
        "jpeg",
        quality,
    )
    .map_err(|e| e.to_string())?;
    Ok(if policy == "jpeg" || jpeg.len() * 5 <= png.len() * 4 {
        crate::media::shared_bytes(jpeg)
    } else {
        png
    })
}
#[cfg(test)]
// Choose quality
pub(crate) fn choose_quality(
    png: Vec<u8>,
    supports_jpeg: bool,
    policy: &str,
    photographic: bool,
    validated_png: bool,
    quality: u8,
) -> Result<Vec<u8>, String> {
    choose_shared_quality(
        crate::media::shared_bytes(png),
        supports_jpeg,
        policy,
        photographic,
        validated_png,
        quality,
    )
    .map(|bytes| bytes.to_vec())
}
// Reject image conversion whose source and destination allocations exceed the working budget
fn conversion_budget(image: &image::DynamicImage, channels: usize) -> Result<(), String> {
    let additional = (image.width() as usize)
        .checked_mul(image.height() as usize)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or("Image conversion size overflow")?;
    if image
        .as_bytes()
        .len()
        .checked_add(additional)
        .is_none_or(|bytes| bytes > 128 * 1024 * 1024)
    {
        return Err("Image conversion exceeds decoded allocation budget".into());
    }
    Ok(())
}
// Recognize JPEG by its signature and otherwise label encoded image bytes as PNG
pub fn mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xff, 0xd8]) {
        "image/jpeg"
    } else {
        "image/png"
    }
}
/// Only the qualified full-screen wallpaper path changes chroma sampling.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnimationTarget {
    Standard,
    N4Wallpaper,
}
impl AnimationTarget {
    pub(crate) fn for_wallpaper(model: &str, variant: &str) -> Self {
        if model == "n4-pro" && variant == "vsdinside" {
            Self::N4Wallpaper
        } else {
            Self::Standard
        }
    }

    fn subsample(
        self,
        options: &crate::media::MediaOptions,
        size: [u16; 2],
        frames: usize,
    ) -> bool {
        self == Self::N4Wallpaper
            && size == [800, 480]
            && frames > 1
            && options.encoding == "auto"
            && options.jpeg_quality.is_none()
    }
}

/// Encode one interleaved baseline scan. Luma keeps the panel's full resolution.
pub(crate) fn encode_wallpaper_420(
    pixels: image::DynamicImage,
    quality: u8,
) -> Result<Vec<u8>, String> {
    let rgb = pixels.into_rgb8();
    let width = u16::try_from(rgb.width()).map_err(|_| "Wallpaper width exceeds JPEG limits")?;
    let height = u16::try_from(rgb.height()).map_err(|_| "Wallpaper height exceeds JPEG limits")?;
    let mut bytes = Vec::new();
    let mut encoder = jpeg_encoder::Encoder::new(&mut bytes, quality);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    encoder.set_chroma_subsampling_method(jpeg_encoder::ChromaSubsamplingMethod::Average);
    encoder
        .encode(rgb.as_raw(), width, height, jpeg_encoder::ColorType::Rgb)
        .map_err(|e| e.to_string())?;
    Ok(bytes)
}

impl crate::Runtime {
    /// Shared internal media output for backgrounds and their previews. Only the
    /// bounded media renderer supplies these PNGs; imported/plugin bytes continue
    /// through the validating public encoding boundary below
    pub(crate) fn render_media_frame_shared(
        &self,
        asset: &str,
        elapsed: std::time::Duration,
        size: [u16; 2],
        rotation: u16,
        options: &crate::media::MediaOptions,
        formats: &[String],
    ) -> Result<crate::media::SharedBytes, String> {
        self.render_media_frame_in_cycle(
            asset,
            elapsed,
            size,
            rotation,
            options,
            formats,
            AnimationTarget::Standard,
            &Default::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_media_frame_in_cycle(
        &self,
        asset: &str,
        elapsed: std::time::Duration,
        size: [u16; 2],
        rotation: u16,
        options: &crate::media::MediaOptions,
        formats: &[String],
        target: AnimationTarget,
        clips: &crate::profiles::ClipCycle,
    ) -> Result<crate::media::SharedBytes, String> {
        options.validate()?;
        if let Some(clip) = clips.get(&self.profiles, asset)? {
            return self.render_animation_frame_in_cycle(
                &clip,
                options.frame_index(&clip, elapsed),
                size,
                rotation,
                options,
                formats,
                None,
                target,
                clips,
            );
        }
        let png = self.profiles.render_media_shared_in_cycle(
            asset,
            elapsed,
            size[0],
            size[1],
            rotation,
            "png",
            options,
            Some(clips),
        )?;
        let policy = options.encoding.as_str();
        let supported = formats.iter().any(|format| format == "jpeg");
        let photographic =
            policy == "auto" && supported && self.profiles.asset_is_photographic(asset)?;
        if png.len() > crate::media::MAX_ASSET {
            return Err("Image exceeds encoded size limit".into());
        }
        if policy == "png" || policy == "auto" && (!photographic || !supported) {
            // Preserve the existing transform allocation. No copy, PNG decode,
            // content hash, or second cache entry is needed for mandatory PNG
            return Ok(png);
        }
        use sha2::{Digest, Sha256};
        let quality = options.quality(false);
        let key = format!(
            "encoding-v2:{}:{supported}:{policy}:{photographic}:true:{quality}",
            hex::encode(Sha256::digest(&**png))
        );
        if let Some(bytes) = self.profiles.render_cache.lock().unwrap().get_shared(&key) {
            return Ok(bytes);
        }
        let bytes = choose_shared_quality(png, supported, policy, photographic, true, quality)?;
        // Conversion releases its input lease; retained PNG keeps the same
        // allocation, which cache admission charges only once
        self.profiles
            .render_cache
            .lock()
            .unwrap()
            .insert_frame(key, bytes)
    }
    #[allow(clippy::too_many_arguments)]
    // Render animation frame shared
    pub(crate) fn render_animation_frame_shared(
        &self,
        clip: &crate::media::Clip,
        index: usize,
        size: [u16; 2],
        rotation: u16,
        options: &crate::media::MediaOptions,
        formats: &[String],
        background: Option<[u8; 4]>,
    ) -> Result<crate::media::SharedBytes, String> {
        self.render_animation_frame_in_cycle(
            clip,
            index,
            size,
            rotation,
            options,
            formats,
            background,
            AnimationTarget::Standard,
            &Default::default(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_animation_frame_in_cycle(
        &self,
        clip: &crate::media::Clip,
        index: usize,
        size: [u16; 2],
        rotation: u16,
        options: &crate::media::MediaOptions,
        formats: &[String],
        background: Option<[u8; 4]>,
        target: AnimationTarget,
        cycle: &crate::profiles::ClipCycle,
    ) -> Result<crate::media::SharedBytes, String> {
        options.validate()?;
        if options.trim_start_ms >= clip.duration_ms
            || options
                .trim_end_ms
                .is_some_and(|end| end > clip.duration_ms)
        {
            return Err("Trim boundaries exceed the clip duration".into());
        }
        let jpeg = options.encoding != "png" && formats.iter().any(|format| format == "jpeg");
        let subsample = jpeg && target.subsample(options, size, clip.frames.len());
        let background = background.filter(|color| color[3] != 0);
        let key = format!(
            "animation-output-v3:{}:{size:?}:{rotation}:{}:{}:{jpeg}:{background:?}:{subsample}",
            hex::encode(clip.frame_digests[index]),
            options.pixel_key(),
            options.quality(true)
        );
        if let Some(bytes) = self.profiles.render_cache.lock().unwrap().get_shared(&key) {
            return Ok(bytes);
        }
        let pixels = cycle.with_decoded(self, clip, index, |source| {
            hardware_core::render::edited_decoded_pixels(
                source,
                size[0],
                size[1],
                rotation,
                options.crop,
                options.fit == "fit",
                Some(options.detail_bounds()),
            )
        })?;
        let pixels = if options.letterbox.as_deref() == Some("black") {
            hardware_core::render::black_matte(pixels)
        } else {
            pixels
        };
        // Opaque video and black-matted frames already cover the binding color.
        let pixels = match background {
            Some(color) if !jpeg || pixels.color().has_alpha() => {
                hardware_core::render::over_color_pixels(pixels, color)
            }
            _ => pixels,
        };
        let format = if jpeg && !hardware_core::render::has_transparency(&pixels) {
            "jpeg"
        } else {
            "png"
        };
        let bytes = if subsample && format == "jpeg" {
            encode_wallpaper_420(pixels, options.quality(true))?
        } else {
            hardware_core::render::encode_pixels_quality(pixels, format, options.quality(true))
                .map_err(|e| e.to_string())?
        };
        if format == "jpeg" {
            JPEG_ENCODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.profiles
            .render_cache
            .lock()
            .unwrap()
            .insert_shared(key, bytes)
    }
    // Render media frame
    pub(crate) fn render_media_frame(
        &self,
        asset: &str,
        elapsed: std::time::Duration,
        size: [u16; 2],
        rotation: u16,
        options: &crate::media::MediaOptions,
        formats: &[String],
    ) -> Result<Vec<u8>, String> {
        let bytes =
            self.render_media_frame_shared(asset, elapsed, size, rotation, options, formats)?;
        Ok(self.owned_render_bytes(bytes))
    }
    // Encode media
    pub fn encode_media(
        &self,
        bytes: Vec<u8>,
        asset: &str,
        options: &crate::media::MediaOptions,
        formats: &[String],
    ) -> Result<Vec<u8>, String> {
        self.encode_media_shared(crate::media::shared_bytes(bytes), asset, options, formats)
            .map(|bytes| self.owned_render_bytes(bytes))
    }
    // Encode media shared
    pub(crate) fn encode_media_shared(
        &self,
        bytes: crate::media::SharedBytes,
        asset: &str,
        options: &crate::media::MediaOptions,
        formats: &[String],
    ) -> Result<crate::media::SharedBytes, String> {
        let binding = crate::Binding {
            live_label: Vec::new(),
            dial_response: None,
            title: String::new(),
            color: "#000000".into(),
            custom_background: false,
            background_source: None,
            action: crate::actions::Action::None,
            asset: Some(asset.into()),
            media: options.clone(),
            plugin_appearance: None,
            composition: None,
            appearance: None,
            feedback: None,
            activation_policy: Default::default(),
            disabled: false,
        };
        self.encode_surface_shared(bytes, Some(&binding), formats, false, false)
    }
    // Encode surface
    pub fn encode_surface(
        &self,
        bytes: Vec<u8>,
        binding: Option<&crate::Binding>,
        formats: &[String],
        has_feedback: bool,
    ) -> Result<Vec<u8>, String> {
        self.encode_surface_shared(
            crate::media::shared_bytes(bytes),
            binding,
            formats,
            has_feedback,
            false,
        )
        .map(|bytes| self.owned_render_bytes(bytes))
    }
    // Encode surface shared
    pub(crate) fn encode_surface_shared(
        &self,
        bytes: crate::media::SharedBytes,
        binding: Option<&crate::Binding>,
        formats: &[String],
        has_feedback: bool,
        validated_png: bool,
    ) -> Result<crate::media::SharedBytes, String> {
        self.encode_surface_with_cache(bytes, binding, formats, has_feedback, validated_png, true)
    }

    pub(crate) fn encode_surface_with_cache(
        &self,
        bytes: crate::media::SharedBytes,
        binding: Option<&crate::Binding>,
        formats: &[String],
        has_feedback: bool,
        validated_png: bool,
        cache_intermediates: bool,
    ) -> Result<crate::media::SharedBytes, String> {
        let Some(binding) = binding else {
            return choose_recorded(bytes, false, "png", false, validated_png);
        };
        let policy = binding.media.encoding.as_str();
        if !["auto", "png", "jpeg"].contains(&policy) {
            return Err("Unknown image encoding policy".into());
        }
        let supported = formats.iter().any(|f| f == "jpeg");
        // Text and plugin/UI feedback retain sharp PNG edges under Auto
        let photographic = policy == "auto"
            && supported
            && binding.appearance.is_none()
            && binding.title.is_empty()
            && binding.composition.is_none()
            && !has_feedback
            && binding
                .asset
                .as_ref()
                .is_some_and(|id| self.profiles.asset_is_photographic(id).unwrap_or(false));
        // Mandatory validated PNG is already final: no hash, duplicate cache entry, scan or decode
        if validated_png && (policy == "png" || policy == "auto" && (!photographic || !supported)) {
            return choose_recorded(bytes, supported, policy, photographic, true);
        }
        use sha2::{Digest, Sha256};
        let video = binding
            .asset
            .as_deref()
            .map(|asset| self.profiles.clip(asset))
            .transpose()?
            .flatten()
            .is_some();
        let quality = binding.media.quality(video);
        if !cache_intermediates {
            return choose_shared_quality(
                bytes,
                supported,
                policy,
                photographic,
                validated_png,
                quality,
            );
        }
        let key = format!(
            "encoding-v2:{}:{supported}:{policy}:{photographic}:{validated_png}:{quality}",
            hex::encode(Sha256::digest(&**bytes))
        );
        self.cached_render(crate::media::RenderKey::from_name(&key), || {
            choose_shared_quality(
                bytes,
                supported,
                policy,
                photographic,
                validated_png,
                quality,
            )
        })
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn automatic_n4_wallpaper_preserves_geometry_and_separates_cached_encodings() {
        use super::AnimationTarget;
        use crate::media::{Clip, MediaOptions};
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let red = [220, 25, 15];
        let blue = [20, 30, 211];
        let pixels =
            image::RgbImage::from_fn(80, 48, |x, _| image::Rgb(if x < 40 { red } else { blue }));
        let png = hardware_core::render::encode_pixels(pixels.into(), "png").unwrap();
        let clip = Clip::new("gif", vec![png.clone(), png], vec![40, 40]).unwrap();
        let options = MediaOptions::default();
        let target = AnimationTarget::for_wallpaper("n4-pro", "vsdinside");
        let render = |target, options: &MediaOptions| {
            runtime
                .render_animation_frame_in_cycle(
                    &clip,
                    0,
                    [800, 480],
                    180,
                    options,
                    &["jpeg".into(), "png".into()],
                    None,
                    target,
                    &Default::default(),
                )
                .unwrap()
        };
        let reference = render(AnimationTarget::Standard, &options);
        let reduced = render(target, &options);
        // These are locally generated baseline JPEGs. The first component in SOF0 is luma.
        let luma_sampling = |bytes: &[u8]| {
            let sof = bytes
                .windows(2)
                .position(|mark| mark == [0xff, 0xc0])
                .unwrap();
            bytes[sof + 11]
        };
        assert_eq!(luma_sampling(&reference), 0x11);
        assert_eq!(luma_sampling(&reduced), 0x22);
        assert!(std::sync::Arc::ptr_eq(&reduced, &render(target, &options)));
        let decoded = image::load_from_memory(&reduced).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (800, 480));
        assert!(decoded.get_pixel(20, 240)[2] > 190 && decoded.get_pixel(780, 240)[0] > 190);
        assert_eq!(
            render(
                target,
                &MediaOptions {
                    jpeg_quality: Some(75),
                    ..options.clone()
                }
            ),
            reference
        );
        assert_eq!(
            render(
                target,
                &MediaOptions {
                    encoding: "jpeg".into(),
                    ..options.clone()
                }
            ),
            reference
        );
        assert!(!target.subsample(&options, [112, 112], 2));
        assert!(!target.subsample(&options, [800, 480], 1));
        assert!(
            !AnimationTarget::for_wallpaper("n4-pro", "mirabox").subsample(&options, [800, 480], 2)
        );
        let transparent = image::RgbaImage::from_pixel(80, 48, image::Rgba([8, 16, 32, 128]));
        let frame = hardware_core::render::encode_pixels(transparent.into(), "png").unwrap();
        let alpha_clip = Clip::new("gif", vec![frame.clone(), frame], vec![40, 40]).unwrap();
        let alpha = runtime
            .render_animation_frame_in_cycle(
                &alpha_clip,
                0,
                [800, 480],
                0,
                &options,
                &["png".into(), "jpeg".into()],
                None,
                target,
                &Default::default(),
            )
            .unwrap();
        assert!(alpha.starts_with(b"\x89PNG"));
        assert_eq!(
            image::load_from_memory(&alpha)
                .unwrap()
                .to_rgba8()
                .get_pixel(400, 240)[3],
            128
        );
        runtime.shutdown();
    }

    #[test]
    fn decoded_frame_is_shared_across_crops_and_released_with_the_cycle() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let clip = crate::media::prepare(include_bytes!(
            "../../../tests/fixtures/media/source-25.gif"
        ))
        .unwrap();
        let reserved = || {
            runtime.profiles.media_budget.stats()["reserved_preparation_bytes"]
                .as_u64()
                .unwrap()
        };
        let before = reserved();
        {
            let cycle = crate::profiles::ClipCycle::default();
            let bytes = cycle
                .with_decoded(
                    &runtime,
                    &clip,
                    0,
                    |image| Ok(image.as_bytes().len() as u64),
                )
                .unwrap();
            for (crop, rotation, fit) in [
                ([0., 0., 1., 1.], 0, false),
                ([0.1, 0.2, 0.8, 0.5], 90, true),
            ] {
                let actual = cycle
                    .with_decoded(&runtime, &clip, 0, |shared| {
                        hardware_core::render::edited_decoded_pixels(
                            shared,
                            90,
                            64,
                            rotation,
                            crop,
                            fit,
                            Some([16, 512]),
                        )
                    })
                    .unwrap();
                let old = hardware_core::render::edited_pixels(
                    &clip.frames[0],
                    90,
                    64,
                    rotation,
                    crop,
                    fit,
                    Some([16, 512]),
                )
                .unwrap();
                assert_eq!(actual, old);
            }
            assert_eq!(reserved() - before, bytes);
            assert_eq!(
                runtime
                    .metrics
                    .snapshot()
                    .into_iter()
                    .find(|m| m.name == "animation_source_decode_hits")
                    .unwrap()
                    .count,
                2
            );
            assert_ne!(clip.frame_digests[0], clip.frame_digests[1]);
            cycle.with_decoded(&runtime, &clip, 1, |_| Ok(())).unwrap();
        }
        assert_eq!(reserved(), before);
        runtime.shutdown();
    }

    use super::*;
    use std::{sync::Arc, time::Duration};

    #[test]
    // Verify that animation quality changes invalidate final cache and keep native geometry and transparency
    fn animation_quality_changes_invalidate_final_cache_and_keep_native_geometry_and_transparency()
    {
        for source_format in ["mp4", "gif"] {
            let root = tempfile::tempdir().unwrap();
            let runtime = crate::Runtime::build(root.path().join("state"), false).unwrap();
            let pixels = image::RgbImage::from_fn(128, 72, |x, y| {
                image::Rgb([
                    ((x * 19 + y * 7) % 256) as u8,
                    ((x * 5 + y * 31) % 256) as u8,
                    ((x * 13 + y * 11) % 256) as u8,
                ])
            });
            let mut frame = vec![];
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut frame, 92)
                .encode_image(&pixels)
                .unwrap();
            let clip = crate::media::Clip::with_timing(
                source_format,
                vec![frame],
                vec![1_000_000],
                2,
                None,
            )
            .unwrap();
            let (asset, _lease) = runtime
                .profiles
                .publish_prepared_clip_shared(
                    std::sync::Arc::new(clip),
                    if source_format == "gif" {
                        include_bytes!("../../../tests/fixtures/media/source-25.gif").as_slice()
                    } else {
                        include_bytes!("../../../tests/fixtures/media/red-blue.mp4").as_slice()
                    },
                    &Default::default(),
                )
                .unwrap();
            let formats = vec!["png".into(), "jpeg".into()];
            let mut options = crate::media::MediaOptions {
                fit: "fit".into(),
                letterbox: Some("black".into()),
                jpeg_quality: Some(90),
                ..Default::default()
            };
            let high = runtime
                .render_media_frame_shared(&asset, Duration::ZERO, [96, 96], 0, &options, &formats)
                .unwrap();
            options.jpeg_quality = Some(50);
            let low = runtime
                .render_media_frame_shared(&asset, Duration::ZERO, [96, 96], 0, &options, &formats)
                .unwrap();
            assert!(low.len() < high.len());
            assert!(low.starts_with(&[0xff, 0xd8]));
            assert!(!std::sync::Arc::ptr_eq(&high, &low));
            assert!(std::sync::Arc::ptr_eq(
                &low,
                &runtime
                    .render_media_frame_shared(
                        &asset,
                        Duration::ZERO,
                        [96, 96],
                        0,
                        &options,
                        &formats
                    )
                    .unwrap()
            ));
            for (size, rotation, expected) in [
                ([96, 96], 0, (96, 96)),
                ([360, 60], 90, (60, 360)),
                ([800, 480], 180, (800, 480)),
            ] {
                let output = runtime
                    .render_media_frame_shared(
                        &asset,
                        Duration::ZERO,
                        size,
                        rotation,
                        &options,
                        &formats,
                    )
                    .unwrap();
                use image::GenericImageView;
                assert_eq!(
                    image::load_from_memory(&output).unwrap().dimensions(),
                    expected
                );
            }
            options.letterbox = Some("transparent".into());
            let output = runtime
                .render_media_frame_shared(&asset, Duration::ZERO, [96, 96], 0, &options, &formats)
                .unwrap();
            assert!(output.starts_with(b"\x89PNG"));
            assert_eq!(
                image::load_from_memory(&output)
                    .unwrap()
                    .to_rgba8()
                    .get_pixel(0, 0)[3],
                0
            );
            runtime.shutdown();
        }
    }
    #[test]
    // Verify that background PNG reuses the transform allocation until output
    fn background_png_reuses_the_transform_allocation_until_output() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("background.png");
        let pixels = image::RgbaImage::from_fn(800, 480, |x, y| {
            image::Rgba([x as u8, y as u8, 127, (x % 256) as u8])
        });
        pixels.save(&source).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let options = crate::media::MediaOptions::default();
        let formats = vec!["png".into(), "jpeg".into()];
        let first = runtime
            .render_media_frame_shared(&asset, Duration::ZERO, [800, 480], 0, &options, &formats)
            .unwrap();
        let transformed = runtime
            .profiles
            .render_media_shared(&asset, Duration::ZERO, 800, 480, 0, "png", &options)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &transformed));
        let before = runtime.cache_stats();
        for tick in 0..20 {
            let frame = runtime
                .render_media_frame_shared(
                    &asset,
                    Duration::from_millis(tick * 50),
                    [800, 480],
                    0,
                    &options,
                    &formats,
                )
                .unwrap();
            assert!(Arc::ptr_eq(&first, &frame));
        }
        let after = runtime.cache_stats();
        assert_eq!(after["bytes"], before["bytes"]);
        assert_eq!(after["owned_copy_bytes"], before["owned_copy_bytes"]);
        #[cfg(unix)]
        assert_eq!(after["source_work"], before["source_work"]);
        let owned = runtime
            .render_media_frame(&asset, Duration::ZERO, [800, 480], 0, &options, &formats)
            .unwrap();
        assert_eq!(owned.as_slice(), &**first);
        assert_eq!(
            runtime.cache_stats()["owned_copy_bytes"].as_u64().unwrap()
                - after["owned_copy_bytes"].as_u64().unwrap(),
            owned.len() as u64
        );
        assert_eq!(image::load_from_memory(&owned).unwrap().to_rgba8(), pixels);
        runtime.shutdown();
    }

    #[test]
    // Verify that background frames preserve crop fit rotation alpha and encoding
    fn background_frames_preserve_crop_fit_rotation_alpha_and_encoding() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        for source_format in ["png", "jpeg"] {
            let source = root.path().join(format!("source.{source_format}"));
            let pixels = image::RgbaImage::from_fn(96, 64, |x, y| {
                image::Rgba([(x * 2) as u8, (y * 3) as u8, 127, (x * 2) as u8])
            });
            if source_format == "jpeg" {
                image::DynamicImage::ImageRgba8(pixels)
                    .to_rgb8()
                    .save(&source)
                    .unwrap();
            } else {
                pixels.save(&source).unwrap();
            }
            let asset = runtime.profiles.import_asset(&source).unwrap();
            for rotation in [0, 90, 180, 270] {
                for policy in ["png", "auto", "jpeg"] {
                    let options = crate::media::MediaOptions {
                        encoding: policy.into(),
                        fit: if rotation % 180 == 0 { "fit" } else { "fill" }.into(),
                        crop: [0.125, 0.125, 0.75, 0.75],
                        ..Default::default()
                    };
                    for formats in [vec!["png".into()], vec!["png".into(), "jpeg".into()]] {
                        let legacy_png = runtime
                            .profiles
                            .render_media(
                                &asset,
                                Duration::ZERO,
                                120,
                                72,
                                rotation,
                                "png",
                                &options,
                            )
                            .unwrap();
                        let expected = runtime.encode_media(legacy_png, &asset, &options, &formats);
                        let actual = runtime.render_media_frame_shared(
                            &asset,
                            Duration::ZERO,
                            [120, 72],
                            rotation,
                            &options,
                            &formats,
                        );
                        match (actual, expected) {
                            (Ok(actual), Ok(expected)) => assert_eq!(
                                &**actual, expected,
                                "{source_format}/{rotation}/{policy}/{formats:?}"
                            ),
                            (Err(actual), Err(expected)) => assert_eq!(actual, expected),
                            (actual, expected) => {
                                panic!("encoding behavior changed: {actual:?} vs {expected:?}")
                            }
                        }
                    }
                }
            }
        }
        runtime.shutdown();
    }

    #[test]
    // Verify that background frames follow animation and source replacement
    fn background_frames_follow_animation_and_source_replacement() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let source = root.path().join("colors.gif");
        std::fs::write(&source, crate::media::tests::sample_gif()).unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let options = crate::media::MediaOptions::default();
        let formats = vec!["png".into()];
        let frame = |asset: &str, millis| {
            runtime
                .render_media_frame_shared(
                    asset,
                    Duration::from_millis(millis),
                    [32, 16],
                    0,
                    &options,
                    &formats,
                )
                .unwrap()
        };
        let red = frame(&asset, 0);
        let blue = frame(&asset, 100);
        assert_ne!(red, blue);
        assert!(Arc::ptr_eq(&red, &frame(&asset, 200)));
        assert_eq!(
            image::load_from_memory(&red)
                .unwrap()
                .to_rgb8()
                .get_pixel(0, 0)
                .0,
            [240, 0, 0]
        );
        assert_eq!(
            image::load_from_memory(&blue)
                .unwrap()
                .to_rgb8()
                .get_pixel(0, 0)
                .0,
            [0, 0, 240]
        );

        let source = root.path().join("static.png");
        std::fs::write(
            &source,
            hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    32,
                    16,
                    image::Rgb([10, 20, 30]),
                )),
                "png",
            )
            .unwrap(),
        )
        .unwrap();
        let asset = runtime.profiles.import_asset(&source).unwrap();
        let before = frame(&asset, 0);
        let replacement = root.path().join("replacement.png");
        std::fs::write(
            &replacement,
            hardware_core::render::encode_pixels(
                image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                    32,
                    16,
                    image::Rgb([40, 50, 60]),
                )),
                "png",
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::rename(
            &replacement,
            root.path()
                .join("app/profiles/assets")
                .join(format!("{asset}.png")),
        )
        .unwrap();
        let after = frame(&asset, 0);
        assert_ne!(before, after);
        assert_eq!(
            image::load_from_memory(&before)
                .unwrap()
                .to_rgb8()
                .get_pixel(0, 0)
                .0,
            [10, 20, 30]
        );
        assert_eq!(
            image::load_from_memory(&after)
                .unwrap()
                .to_rgb8()
                .get_pixel(0, 0)
                .0,
            [40, 50, 60]
        );
        runtime.shutdown();
    }

    #[test]
    // Verify that JPEG bytes match legacy double decode at quality ninety
    fn jpeg_bytes_match_legacy_double_decode_at_quality_ninety() {
        for alpha_channel in [false, true] {
            let rgba = image::RgbaImage::from_fn(32, 32, |x, y| {
                image::Rgba([(x * 7) as u8, (y * 5) as u8, ((x + y) * 3) as u8, 255])
            });
            let image = image::DynamicImage::ImageRgba8(rgba);
            let image = if alpha_channel {
                image
            } else {
                image::DynamicImage::ImageRgb8(image.to_rgb8())
            };
            let mut png = std::io::Cursor::new(Vec::new());
            image.write_to(&mut png, image::ImageFormat::Png).unwrap();
            let png = png.into_inner();
            assert!(!image::load_from_memory(&png)
                .unwrap()
                .to_rgba8()
                .pixels()
                .any(|pixel| pixel[3] < 255));
            let legacy_rgb = image::load_from_memory(&png).unwrap().to_rgb8();
            let mut legacy = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut legacy, 90)
                .encode_image(&legacy_rgb)
                .unwrap();
            assert_eq!(
                choose_quality(png, true, "jpeg", true, false, 90).unwrap(),
                legacy
            );
        }
    }
    #[test]
    // Verify that low channel decode cannot expand beyond conversion budget
    fn low_channel_decode_cannot_expand_beyond_conversion_budget() {
        // A highly compressed luma image fits the decoder budget but RGB conversion would not
        let image = image::DynamicImage::ImageLuma8(image::GrayImage::new(8192, 8192));
        assert!(conversion_budget(&image, 3).is_err());
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        drop(image);
        assert!(
            choose_quality(png.into_inner(), true, "jpeg", true, false, 90)
                .unwrap_err()
                .contains("conversion")
        );
    }
    #[test]
    // Verify that validated PNG fast path and single decode are structural
    fn validated_png_fast_path_and_single_decode_are_structural() {
        let png =
            hardware_core::render::tile_rgba(64, 32, [30, 40, 50, 255], "sharp", 0, "png").unwrap();
        let mut work = EncodingWork::default();
        assert_eq!(
            choose_inner(png.clone(), true, "png", false, true, &mut work).unwrap(),
            png
        );
        assert_eq!(
            (work.decodes, work.alpha_scans, work.jpeg_encodes),
            (0, 0, 0)
        );
        assert_eq!(
            choose_inner(png.clone(), true, "png", false, false, &mut work).unwrap(),
            png
        );
        assert_eq!((work.decodes, work.alpha_scans), (1, 0));
        let mut work = EncodingWork::default();
        let jpeg = choose_inner(png, true, "jpeg", true, true, &mut work).unwrap();
        assert_eq!(mime(&jpeg), "image/jpeg");
        assert_eq!(
            (work.decodes, work.alpha_scans, work.jpeg_encodes),
            (1, 1, 1)
        );
    }
    #[test]
    // Verify that untrusted PNG policy still rejects corrupt and oversized images
    fn untrusted_png_policy_still_rejects_corrupt_and_oversized_images() {
        for bytes in [
            b"not an image".to_vec(),
            b"\x89PNG\r\n\x1a\n".to_vec(),
            vec![0; crate::media::MAX_ASSET + 1],
        ] {
            assert!(choose_quality(bytes, true, "png", false, false, 90).is_err());
        }
        let mut work = EncodingWork::default();
        assert!(choose_inner(vec![], true, "bad-policy", false, true, &mut work).is_err());
        assert_eq!(work.decodes, 0);
        let png = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                8193,
                1,
                image::Rgb([0, 0, 0]),
            )),
            "png",
        )
        .unwrap();
        assert!(choose_quality(png, true, "png", false, false, 90).is_err());
    }
    #[test]
    // Verify that lossless policy preserves encoded bytes and alpha edges
    fn lossless_policy_preserves_encoded_bytes_and_alpha_edges() {
        for (width, height) in [(1, 1), (96, 96), (256, 64), (800, 480)] {
            let pixels = image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([x as u8, y as u8, 127, ((x + y) % 256) as u8])
            });
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(pixels.clone())
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            for policy in ["png", "auto", "jpeg"] {
                let output =
                    choose_quality(png.get_ref().clone(), true, policy, false, false, 90).unwrap();
                assert_eq!(output, *png.get_ref());
                assert_eq!(image::load_from_memory(&output).unwrap().to_rgba8(), pixels);
            }
        }
    }
    #[test]
    // Verify that auto requires at least twenty percent savings
    fn auto_requires_at_least_twenty_percent_savings() {
        use image::ImageEncoder;
        let pixels = image::RgbImage::from_pixel(16, 16, image::Rgb([40, 80, 120]));
        let png_with_padding = |padding: usize| {
            // A valid empty TIFF directory plus unused metadata bytes changes
            // the PNG size without changing any pixels or the resulting JPEG
            let mut exif = b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
            exif.resize(exif.len() + padding, 0);
            let mut png = vec![];
            let mut encoder = image::codecs::png::PngEncoder::new(&mut png);
            encoder.set_exif_metadata(exif).unwrap();
            encoder
                .write_image(&pixels, 16, 16, image::ExtendedColorType::Rgb8)
                .unwrap();
            png
        };
        let smallest = png_with_padding(0);
        let jpeg = choose_quality(smallest.clone(), true, "jpeg", true, false, 90).unwrap();
        let first_qualifying_size = (jpeg.len() * 5).div_ceil(4);
        assert!(first_qualifying_size > smallest.len());
        let qualifying = png_with_padding(first_qualifying_size - smallest.len());
        let below = png_with_padding(first_qualifying_size - smallest.len() - 1);
        assert_eq!(qualifying.len(), first_qualifying_size);
        assert_eq!(below.len() + 1, qualifying.len());
        assert_eq!(
            choose_quality(below.clone(), true, "auto", true, false, 90).unwrap(),
            below
        );
        assert_eq!(
            choose_quality(qualifying.clone(), true, "auto", true, false, 90).unwrap(),
            jpeg
        );
        // The same savings never override a PNG policy, UI classification, or
        // device capabilities; these inputs also share identical pixel data
        for (supported, policy, photographic) in [
            (true, "png", true),
            (true, "auto", false),
            (false, "auto", true),
        ] {
            assert_eq!(
                choose_quality(
                    qualifying.clone(),
                    supported,
                    policy,
                    photographic,
                    false,
                    90
                )
                .unwrap(),
                qualifying
            );
        }
    }
    #[test]
    // Verify that transparent surfaces keep PNG even when JPEG is requested
    fn transparent_surfaces_keep_png_even_when_jpeg_is_requested() {
        let bytes =
            hardware_core::render::tile_rgba(64, 32, [30, 40, 50, 0], "Test", 0, "png").unwrap();
        for policy in ["auto", "png", "jpeg"] {
            assert_eq!(
                choose_quality(bytes.clone(), true, policy, true, false, 90).unwrap(),
                bytes
            );
        }
    }
    #[test]
    // Verify that UI stays PNG and unsupported JPEG is rejected
    fn ui_stays_png_and_unsupported_jpeg_is_rejected() {
        let png = hardware_core::render::encode_pixels(
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                64,
                64,
                image::Rgb([100, 80, 50]),
            )),
            "png",
        )
        .unwrap();
        assert_eq!(
            choose_quality(png.clone(), true, "auto", false, false, 90).unwrap(),
            png
        );
        assert!(choose_quality(png, false, "jpeg", true, false, 90).is_err());
    }
}
