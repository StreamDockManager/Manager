//! Demand-paced wallpaper frames. An unchanged source/render identity never copies
//! encoded pixels across IPC, while each response schedules the next real boundary
use crate::{media, Runtime};
use sha2::{Digest, Sha256};
use std::time::Duration;

const MIN_FRAME_MS: u64 = 67; // Preserve the existing ~15 fps wallpaper ceiling

// Round the next wallpaper frame delay up to milliseconds while enforcing the minimum refresh interval
fn next_delay(clip: &media::Clip, options: &media::MediaOptions, elapsed: Duration) -> u64 {
    options
        .next_frame_delay(clip, elapsed)
        .as_micros()
        .div_ceil(1000)
        .min(u128::from(u32::MAX))
        .max(u128::from(MIN_FRAME_MS)) as u64
}

impl Runtime {
    // Render a wallpaper frame using the current media clock
    pub fn theme_wallpaper_frame(
        &self,
        asset: &str,
        size: [u16; 2],
        options: &media::MediaOptions,
        still: bool,
        previous: Option<&str>,
    ) -> Result<Vec<u8>, String> {
        self.theme_wallpaper_frame_at(asset, size, options, still, previous, self.media_elapsed())
    }

    // Validate wallpaper dimensions and frame tokens before rendering or reusing the requested frame
    fn theme_wallpaper_frame_at(
        &self,
        asset: &str,
        size: [u16; 2],
        options: &media::MediaOptions,
        still: bool,
        previous: Option<&str>,
        elapsed: Duration,
    ) -> Result<Vec<u8>, String> {
        if size.iter().any(|edge| !(1..=1280).contains(edge)) {
            return Err("Wallpaper preview size is outside its bounds".into());
        }
        options.validate()?;
        if previous
            .is_some_and(|token| token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err("Invalid wallpaper frame token".into());
        }
        let elapsed = if still { Duration::ZERO } else { elapsed };
        let clip = self.profiles.clip(asset)?;
        let index = clip.as_ref().map(|clip| options.frame_index(clip, elapsed));
        if let Some(clip) = &clip {
            if options.trim_start_ms >= clip.duration_ms
                || options
                    .trim_end_ms
                    .is_some_and(|end| end > clip.duration_ms)
            {
                return Err("Trim boundaries exceed the clip duration".into());
            }
        }
        // Imported asset IDs are immutable; prepared frame digests already exist
        // Include every rendering option and size to invalidate resize/draft edits
        let identity = serde_json::to_vec(&(
            asset,
            size,
            options,
            clip.as_ref()
                .zip(index)
                .map(|(clip, index)| &clip.frame_digests[index]),
        ))
        .map_err(|error| error.to_string())?;
        let token = hex::encode(Sha256::digest(identity));
        let delay = clip
            .as_ref()
            .filter(|clip| !still && clip.frames.len() > 1)
            .map(|clip| next_delay(clip, options, elapsed));
        let bytes = if previous == Some(token.as_str()) {
            None
        } else if let Some((clip, index)) = clip.as_ref().zip(index) {
            Some(self.render_animation_frame_shared(
                clip,
                index,
                size,
                0,
                options,
                &["png".into(), "jpeg".into()],
                None,
            )?)
        } else {
            Some(self.render_media_frame_shared(
                asset,
                elapsed,
                size,
                0,
                options,
                &["png".into(), "jpeg".into()],
            )?)
        };
        let mime = bytes.as_ref().map(|bytes| {
            if bytes.first() == Some(&0xff) {
                "image/jpeg"
            } else {
                "image/png"
            }
        });
        let metadata = serde_json::to_vec(&serde_json::json!({
            "token": token, "next_ms": delay, "mime": mime,
        }))
        .map_err(|error| error.to_string())?;
        let mut response =
            Vec::with_capacity(4 + metadata.len() + bytes.as_ref().map_or(0, |bytes| bytes.len()));
        response.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
        response.extend_from_slice(&metadata);
        if let Some(bytes) = bytes {
            response.extend_from_slice(&bytes);
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Delay, Frame, Rgba, RgbaImage};

    // Unpack the selected theme wallpaper content
    fn unpack(bytes: &[u8]) -> (serde_json::Value, &[u8]) {
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        (
            serde_json::from_slice(&bytes[4..4 + length]).unwrap(),
            &bytes[4 + length..],
        )
    }

    // Construct an isolated theme wallpaper fixture
    fn fixture() -> (tempfile::TempDir, std::sync::Arc<Runtime>, String) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("wallpaper.gif");
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .unwrap();
            for (color, delay) in [
                ([180, 0, 0, 255], 500),
                ([180, 0, 0, 255], 500),
                ([0, 0, 180, 255], 1000),
            ] {
                encoder
                    .encode_frame(Frame::from_parts(
                        RgbaImage::from_pixel(64, 64, Rgba(color)),
                        0,
                        0,
                        Delay::from_numer_denom_ms(delay, 1),
                    ))
                    .unwrap();
            }
        }
        std::fs::write(&path, bytes).unwrap();
        let runtime = Runtime::build(root.path().join("config"), false).unwrap();
        let asset = runtime.profiles.import_asset(&path).unwrap();
        (root, runtime, asset)
    }

    #[test]
    // Verify that wallpaper timing suppresses duplicate pixels and invalidates render changes
    fn wallpaper_timing_suppresses_duplicate_pixels_and_invalidates_render_changes() {
        let (_root, runtime, asset) = fixture();
        let options = media::MediaOptions::default();
        let first = runtime
            .theme_wallpaper_frame_at(&asset, [128, 64], &options, false, None, Duration::ZERO)
            .unwrap();
        let (metadata, pixels) = unpack(&first);
        assert!(!pixels.is_empty());
        assert_eq!(metadata["next_ms"], 500);
        let token = metadata["token"].as_str().unwrap();
        for time in [100, 500, 900] {
            let next = runtime
                .theme_wallpaper_frame_at(
                    &asset,
                    [128, 64],
                    &options,
                    false,
                    Some(token),
                    Duration::from_millis(time),
                )
                .unwrap();
            let (next_metadata, pixels) = unpack(&next);
            assert!(
                pixels.is_empty(),
                "identical prepared pixels must not cross IPC"
            );
            assert_eq!(next_metadata["token"], token);
            assert!(next.len() < 150);
        }
        let changed = runtime
            .theme_wallpaper_frame_at(
                &asset,
                [128, 64],
                &options,
                false,
                Some(token),
                Duration::from_millis(1000),
            )
            .unwrap();
        assert!(!unpack(&changed).1.is_empty());
        let resized = runtime
            .theme_wallpaper_frame_at(
                &asset,
                [64, 64],
                &options,
                false,
                Some(token),
                Duration::ZERO,
            )
            .unwrap();
        assert!(!unpack(&resized).1.is_empty());
        let edited = media::MediaOptions {
            fit: "fit".into(),
            ..options.clone()
        };
        let edited = runtime
            .theme_wallpaper_frame_at(
                &asset,
                [128, 64],
                &edited,
                false,
                Some(token),
                Duration::ZERO,
            )
            .unwrap();
        assert!(!unpack(&edited).1.is_empty());
        let still = runtime
            .theme_wallpaper_frame_at(
                &asset,
                [128, 64],
                &options,
                true,
                Some(token),
                Duration::from_millis(1000),
            )
            .unwrap();
        assert!(unpack(&still).0["next_ms"].is_null());
        assert!(unpack(&still).1.is_empty());
    }

    #[test]
    // Verify that wallpaper schedule honors variable timing speed trim and existing ceiling
    fn wallpaper_schedule_honors_variable_timing_speed_trim_and_existing_ceiling() {
        let clip =
            media::Clip::new("gif", vec![vec![1], vec![2], vec![3]], vec![10, 490, 1000]).unwrap();
        let options = media::MediaOptions::default();
        assert_eq!(next_delay(&clip, &options, Duration::ZERO), 67);
        assert_eq!(next_delay(&clip, &options, Duration::from_millis(10)), 490);
        assert_eq!(
            next_delay(&clip, &options, Duration::from_millis(500)),
            1000
        );
        let options = media::MediaOptions {
            speed: 2.0,
            trim_start_ms: 500,
            trim_end_ms: Some(1200),
            ..options
        };
        assert_eq!(next_delay(&clip, &options, Duration::ZERO), 350);
        assert_eq!(next_delay(&clip, &options, Duration::from_millis(300)), 67);
    }

    #[test]
    // Verify that wallpaper retains input validation even when a frame is unchanged
    fn wallpaper_retains_input_validation_even_when_a_frame_is_unchanged() {
        let (_root, runtime, asset) = fixture();
        let options = media::MediaOptions::default();
        assert!(runtime
            .theme_wallpaper_frame(&asset, [1281, 10], &options, false, None)
            .is_err());
        assert!(runtime
            .theme_wallpaper_frame(&asset, [10, 10], &options, false, Some("invalid"))
            .is_err());
        let invalid = media::MediaOptions {
            trim_start_ms: 5000,
            ..options
        };
        assert!(runtime
            .theme_wallpaper_frame(&asset, [10, 10], &invalid, false, None)
            .is_err());
    }
}
