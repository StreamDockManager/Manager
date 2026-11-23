//! Transient vendor GIF timelines. No package image becomes a persisted user asset
use crate::{media, media_budget::ProtectedClip, profiles::ProfileStore};
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(crate) struct Animation {
    pub asset: String,
    pub started: Duration,
    pub clip: Arc<media::Clip>,
}
#[derive(Debug)]
pub(crate) struct PreparationFailure {
    pub message: String,
    pub retryable: bool,
}
impl PreparationFailure {
    // Construct an animation failure that callers must not retry
    pub fn permanent(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            retryable: false,
        }
    }
    // Construct an animation failure eligible for retry
    pub fn transient(message: impl ToString) -> Self {
        Self {
            message: message.to_string(),
            retryable: true,
        }
    }
}
struct Entry {
    source: Weak<Value>,
    fingerprint: String,
    size: [u16; 2],
    touched: Instant,
    prepared: Instant,
    control: String,
    animation: Result<Animation, String>,
    retryable: bool,
    _protected: Option<ProtectedClip>,
}
#[derive(Default)]
pub(crate) struct Registry {
    entries: HashMap<(String, bool, String), Entry>,
}
impl Registry {
    // Prune obsolete vendor animation entries
    pub fn prune(&mut self) {
        self.entries.retain(|(_, software, _), entry| {
            !*software || entry.touched.elapsed() < Duration::from_secs(30)
        });
    }
    // Retry due
    pub fn retry_due(&self, device: &str, software: bool) -> bool {
        self.entries.iter().any(|((id, target, _), entry)| {
            id == device
                && *target == software
                && entry.retryable
                && entry.prepared.elapsed() >= Duration::from_secs(1)
        })
    }
    // Clear retained vendor animation state
    pub fn clear(&mut self, device: &str, software: bool) {
        self.entries
            .retain(|(id, target, _), _| id != device || *target != software);
    }
    // Report the active vendor animation state
    pub fn active(&self, device: &str, software: bool) -> HashMap<String, Animation> {
        self.entries
            .iter()
            .filter(|((id, target, _), _)| id == device && *target == software)
            .filter_map(|(_, entry)| {
                entry
                    .animation
                    .as_ref()
                    .ok()
                    .filter(|a| a.clip.frames.len() > 1)
                    .map(|a| (entry.control.clone(), a.clone()))
            })
            .collect()
    }
    // Report the prepare condition for vendor animation
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        profiles: &ProfileStore,
        target: (&str, bool),
        contexts: &HashMap<String, String>,
        visuals: &mut HashMap<String, Arc<Value>>,
        artwork: &HashMap<String, stream_dock_manager_plugin_host::artwork_cache::ArtworkFiles>,
        sizes: &HashMap<String, [u16; 2]>,
        elapsed: Duration,
        cancelled: &dyn Fn() -> bool,
    ) -> Vec<(String, String)> {
        let (device, software) = target;
        let mut errors = Vec::new();
        let mut live = HashSet::new();
        for (control, visual) in visuals.iter_mut() {
            let Some(image) = visual["image"]
                .as_str()
                .filter(|s| s.starts_with("data:image/gif;base64,"))
            else {
                continue;
            };
            let key = (
                device.to_owned(),
                software,
                contexts
                    .get(control)
                    .cloned()
                    .unwrap_or_else(|| control.clone()),
            );
            live.insert(key.clone());
            let size = sizes.get(control).copied().unwrap_or([512, 512]);
            let same_pointer = self
                .entries
                .get(&key)
                .and_then(|entry| entry.source.upgrade())
                .is_some_and(|source| Arc::ptr_eq(&source, visual));
            let image_digest = || {
                artwork
                    .get(control)
                    .and_then(|files| files.stored_digest(image))
                    .unwrap_or_else(|| Sha256::digest(image.as_bytes()).into())
            };
            let fingerprint = if same_pointer {
                self.entries[&key].fingerprint.clone()
            } else {
                format!("{}:{}", hex::encode(image_digest()), visual["state"])
            };
            let unchanged = self.entries.get(&key).is_some_and(|entry| {
                entry.fingerprint == fingerprint
                    && entry.size == size
                    && (!entry.retryable || entry.prepared.elapsed() < Duration::from_secs(1))
            });
            if !unchanged {
                if self.entries.len() >= 512 && !self.entries.contains_key(&key) {
                    let mut rejected = visual.as_ref().clone();
                    rejected["image"] = Value::Null;
                    rejected["vendorAnimationError"] =
                        Value::String("Too many active vendor animations".into());
                    *visual = Arc::new(rejected);
                    continue;
                }
                let result: Result<(Animation, ProtectedClip), PreparationFailure> = (|| {
                    if image.len() > 24 * 1024 * 1024 {
                        return Err(PreparationFailure::permanent(
                            "Vendor GIF exceeds source limit",
                        ));
                    }
                    let asset = format!(
                        "vendor-gif:{}:{}x{}",
                        hex::encode(image_digest()),
                        size[0],
                        size[1]
                    );
                    let clip = self
                        .entries
                        .values()
                        .filter_map(|entry| entry.animation.as_ref().ok())
                        .find(|animation| animation.asset == asset)
                        .map(|animation| animation.clip.clone())
                        .map(Ok)
                        .unwrap_or_else(|| {
                            profiles.vendor_gif(
                                &asset,
                                image,
                                artwork.get(control),
                                size,
                                cancelled,
                            )
                        })?;
                    let protected = profiles
                        .media_budget
                        .protect(clip.clone())
                        .map_err(PreparationFailure::transient)?;
                    Ok((
                        Animation {
                            asset,
                            started: elapsed,
                            clip,
                        },
                        protected,
                    ))
                })(
                );
                if cancelled() {
                    return errors;
                }
                let (animation, protected, retryable) = match result {
                    Ok((a, p)) => (Ok(a), Some(p), false),
                    Err(e) => (Err(e.message), None, e.retryable),
                };
                if let Err(error) = &animation {
                    if self
                        .entries
                        .get(&key)
                        .and_then(|entry| entry.animation.as_ref().err())
                        != Some(error)
                    {
                        errors.push((control.clone(), error.clone()));
                    }
                }
                self.entries.insert(
                    key.clone(),
                    Entry {
                        source: Arc::downgrade(visual),
                        fingerprint,
                        size,
                        touched: Instant::now(),
                        prepared: Instant::now(),
                        control: control.clone(),
                        animation,
                        retryable,
                        _protected: protected,
                    },
                );
            }
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.source = Arc::downgrade(visual);
                entry.touched = Instant::now();
                let mut frame = Value::Object(
                    visual
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(key, _)| key.as_str() != "image")
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                );
                match &entry.animation {
                    Ok(animation) => {
                        let index = animation
                            .clip
                            .frame_index(elapsed.saturating_sub(animation.started));
                        frame["image"] = Value::String(format!(
                            "data:image/png;base64,{}",
                            base64::engine::general_purpose::STANDARD
                                .encode(&**animation.clip.frames[index])
                        ));
                    }
                    // Invalid content stays memoized; temporary capacity failures retry
                    // at a bounded cadence without repeated identical error notices
                    Err(error) => {
                        frame["image"] = Value::Null;
                        frame["vendorAnimationError"] = Value::String(error.clone());
                    }
                }
                *visual = Arc::new(frame);
            }
        }
        self.entries
            .retain(|key, _| key.0 != device || key.1 != software || live.contains(key));
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    // Create an animated GIF visual fixture encoded as a data URI
    fn visual() -> Arc<Value> {
        Arc::new(
            json!({"state":0,"image":format!("data:image/gif;base64,{}",base64::engine::general_purpose::STANDARD.encode(crate::media::tests::sample_gif()))}),
        )
    }
    // Collect a vendor animation sample
    fn sample(
        registry: &mut Registry,
        store: &ProfileStore,
        source: Arc<Value>,
        software: bool,
        at: u64,
    ) -> Value {
        let mut visuals = HashMap::from([("key".into(), source)]);
        registry.prepare(
            store,
            ("device", software),
            &HashMap::from([("key".into(), "context".into())]),
            &mut visuals,
            &HashMap::new(),
            &HashMap::new(),
            Duration::from_millis(at),
            &|| false,
        );
        visuals["key"].as_ref().clone()
    }
    #[test]
    // Verify that frames follow source delays with independent target clocks and shared clip
    fn frames_follow_source_delays_with_independent_target_clocks_and_shared_clip() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let mut registry = Registry::default();
        let source = visual();
        let first = sample(&mut registry, &store, source.clone(), false, 5000);
        assert_eq!(
            first,
            sample(&mut registry, &store, source.clone(), false, 5099)
        );
        let second = sample(&mut registry, &store, source.clone(), false, 5100);
        assert_ne!(first["image"], second["image"]);
        let software = sample(&mut registry, &store, source.clone(), true, 5100);
        assert_eq!(first["image"], software["image"]);
        let hardware = registry.active("device", false)["key"].clone();
        let software = registry.active("device", true)["key"].clone();
        assert!(Arc::ptr_eq(&hardware.clip, &software.clip));
        assert_ne!(hardware.started, software.started);
        assert_eq!(
            first["image"],
            sample(&mut registry, &store, source.clone(), false, 5200)["image"]
        );
        registry.clear("device", true);
        assert_eq!(
            registry.active("device", false)["key"].started,
            hardware.started
        );
        registry.clear("device", false);
        assert_eq!(
            first["image"],
            sample(&mut registry, &store, source, false, 5300)["image"]
        );
        assert_eq!(
            registry.active("device", false)["key"].started,
            Duration::from_millis(5300)
        );
    }
    #[test]
    // Verify that unchanged source retries transient capacity failure but not invalid content
    fn unchanged_source_retries_transient_capacity_failure_but_not_invalid_content() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let mut registry = Registry::default();
        let source = visual();
        let first = store.media_gate.lock().unwrap();
        let second = store.media_gate.lock().unwrap();
        assert!(
            sample(&mut registry, &store, source.clone(), false, 0)["vendorAnimationError"]
                .is_string()
        );
        assert!(!registry.retry_due("device", false));
        drop((first, second));
        for entry in registry.entries.values_mut() {
            entry.prepared = Instant::now() - Duration::from_secs(2);
        }
        assert!(registry.retry_due("device", false));
        assert!(sample(&mut registry, &store, source, false, 2000)["image"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png"));
        assert!(!registry.retry_due("device", false));
        let invalid = Arc::new(json!({"image":"data:image/gif;base64,R0lGODlh"}));
        sample(&mut registry, &store, invalid.clone(), false, 3000);
        for entry in registry.entries.values_mut() {
            entry.prepared = Instant::now() - Duration::from_secs(2);
        }
        let prepared = registry.entries.values().next().unwrap().prepared;
        assert!(!registry.retry_due("device", false));
        sample(&mut registry, &store, invalid, false, 5000);
        assert_eq!(registry.entries.values().next().unwrap().prepared, prepared);
    }
    #[test]
    // Verify that source replacement retirement errors and static images are bounded
    fn source_replacement_retirement_errors_and_static_images_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let mut registry = Registry::default();
        let source = visual();
        sample(&mut registry, &store, source.clone(), false, 0);
        let old = registry.active("device", false)["key"].clip.clone();
        let mut changed = source.as_ref().clone();
        changed["state"] = json!(1);
        sample(&mut registry, &store, Arc::new(changed), false, 150);
        assert_eq!(
            registry.active("device", false)["key"].started,
            Duration::from_millis(150)
        );
        let invalid = Arc::new(json!({"image":"data:image/gif;base64,R0lGODlh"}));
        assert!(
            sample(&mut registry, &store, invalid.clone(), false, 200)["vendorAnimationError"]
                .is_string()
        );
        assert!(registry.active("device", false).is_empty());
        assert!(
            sample(&mut registry, &store, invalid, false, 300)["vendorAnimationError"].is_string()
        );
        let static_image = Arc::new(json!({"image":null,"title":"static"}));
        assert_eq!(
            sample(&mut registry, &store, static_image.clone(), false, 400),
            *static_image
        );
        assert!(registry.entries.is_empty());
        drop(old);
        store.media_budget.trim_pressure();
        assert!(
            root.path()
                .join("profiles/assets")
                .read_dir()
                .unwrap()
                .next()
                .is_none(),
            "transient GIF must not persist assets"
        );
    }
}

#[cfg(test)]
mod limits_tests {
    use super::*;
    #[test]
    fn a_gif_prepares_distinct_frames_for_each_screen_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(dir.path().join("profiles")).unwrap();
        let mut gif = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
            for color in [[225, 16, 64, 255], [20, 180, 32, 255]] {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(256, 256, image::Rgba(color)),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(100, 1),
                    ))
                    .unwrap();
            }
        }
        let source = Arc::new(
            serde_json::json!({"image": format!("data:image/gif;base64,{}", base64::engine::general_purpose::STANDARD.encode(gif))}),
        );
        let mut registry = Registry::default();
        let mut asset = String::new();
        for size in [[64, 64], [112, 96]] {
            let sizes = HashMap::from([("key".into(), size)]);
            let mut visuals = HashMap::from([("key".into(), source.clone())]);
            assert!(registry
                .prepare(
                    &store,
                    ("device", false),
                    &HashMap::new(),
                    &mut visuals,
                    &HashMap::new(),
                    &sizes,
                    Duration::ZERO,
                    &|| false
                )
                .is_empty());
            let animation = registry.active("device", false).remove("key").unwrap();
            assert_ne!(asset, animation.asset);
            asset = animation.asset;
            let frame = image::load_from_memory(&animation.clip.frames[0]).unwrap();
            assert_eq!(
                (frame.width(), frame.height()),
                (u32::from(size[0]), u32::from(size[0]))
            );
            assert_eq!(animation.clip.delays_ms, [100, 100]);
        }
    }

    #[test]
    // Verify that vendor decode cancels and does not wait behind other preparations
    fn vendor_decode_cancels_and_does_not_wait_behind_other_preparations() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::open(root.path().join("profiles")).unwrap();
        let uri = format!(
            "data:image/gif;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(crate::media::tests::sample_gif())
        );
        assert!(store
            .vendor_gif("vendor-gif:cancelled", &uri, None, [112, 112], &|| true)
            .err()
            .unwrap()
            .message
            .contains("changed"));
        let first = store.media_gate.lock().unwrap();
        let second = store.media_gate.lock().unwrap();
        assert!(store
            .vendor_gif("vendor-gif:busy", &uri, None, [112, 112], &|| false)
            .err()
            .unwrap()
            .message
            .contains("busy"));
        drop((first, second));
        assert_eq!(
            store
                .vendor_gif("vendor-gif:ready", &uri, None, [112, 112], &|| false)
                .ok()
                .unwrap()
                .frames
                .len(),
            2
        );
    }
}
