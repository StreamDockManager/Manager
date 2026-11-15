use crate::SurfacePreview;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

impl crate::Runtime {
    /// Preview the final encoded hardware image in its upright display orientation
    /// Rotating before JPEG encoding can change pixels, so never render a second,
    /// unrotated approximation for the editor. The inverse transform is lossless
    pub(crate) fn preview_image(
        &self,
        bytes: crate::media::SharedBytes,
        rotation: u16,
    ) -> Result<crate::media::SharedBytes, String> {
        if rotation == 0 {
            return Ok(bytes);
        }
        use sha2::{Digest, Sha256};
        let key = format!(
            "preview-upright:{rotation}:{}",
            hex::encode(Sha256::digest(&**bytes))
        );
        self.cached_render(key, || {
            let pixels = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
            let upright = match rotation {
                90 => pixels.rotate270(),
                180 => pixels.rotate180(),
                270 => pixels.rotate90(),
                _ => return Err("Invalid preview rotation".into()),
            };
            hardware_core::render::encode_pixels(upright, "png")
                .map(crate::media::shared_bytes)
                .map_err(|e| e.to_string())
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PreviewDelta {
    #[serde(default)]
    #[ts(optional, as = "Option<f64>")]
    pub configuration_revision: u64,
    pub generation: String,
    #[ts(type = "number")]
    pub sequence: u64,
    pub reset: bool,
    pub surfaces: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub trigger_descriptions: BTreeMap<String, BTreeMap<String, String>>,
    pub background_changed: bool,
    pub background: Option<String>,
}
// Compute changed or removed preview surfaces and background while omitting unchanged updates
pub fn difference(
    previous: Option<&SurfacePreview>,
    current: &SurfacePreview,
    generation: &str,
    sequence: u64,
) -> Option<PreviewDelta> {
    let full = previous.is_none_or(|p| p.configuration_revision != current.configuration_revision);
    let mut surfaces = BTreeMap::new();
    for (key, value) in &current.surfaces {
        if full || previous.and_then(|p| p.surfaces.get(key)) != Some(value) {
            surfaces.insert(key.clone(), Some(value.clone()));
        }
    }
    if let Some(previous) = previous {
        for key in previous.surfaces.keys() {
            if !current.surfaces.contains_key(key) {
                surfaces.insert(key.clone(), None);
            }
        }
    }
    let background_changed = full || previous.is_none_or(|p| p.background != current.background);
    if previous.is_some()
        && surfaces.is_empty()
        && !background_changed
        && previous.is_some_and(|p| p.trigger_descriptions == current.trigger_descriptions)
    {
        return None;
    }
    Some(PreviewDelta {
        configuration_revision: current.configuration_revision,
        generation: generation.into(),
        sequence,
        reset: previous.is_none(),
        surfaces,
        trigger_descriptions: current.trigger_descriptions.clone(),
        background_changed,
        background: if background_changed {
            current.background.clone()
        } else {
            None
        },
    })
}

/// Tracks successful frames while transient render failures retry with bounded backoff
pub struct SurfacePreviewStream {
    generation: String,
    scene: String,
    previous: Option<SurfacePreview>,
    sequence: u64,
    failures: u8,
    origin: std::time::Instant,
    sample_slots: BTreeMap<String, u128>,
}
impl SurfacePreviewStream {
    // Initialize preview state from the supplied inputs
    pub fn new(generation: String) -> Self {
        Self {
            generation,
            scene: String::new(),
            previous: None,
            sequence: 0,
            failures: 0,
            origin: std::time::Instant::now(),
            sample_slots: BTreeMap::new(),
        }
    }
    // Apply the new preview state
    pub fn update(
        &mut self,
        scene: &str,
        current: Result<SurfacePreview, String>,
    ) -> Option<PreviewDelta> {
        self.update_at(scene, current, &BTreeMap::new(), std::time::Instant::now())
    }
    // Update at
    pub fn update_at(
        &mut self,
        scene: &str,
        current: Result<SurfacePreview, String>,
        rates: &BTreeMap<String, u8>,
        now: std::time::Instant,
    ) -> Option<PreviewDelta> {
        let mut current = match current {
            Ok(current) => current,
            Err(_) => {
                self.failures = self.failures.saturating_add(1);
                return None;
            }
        };
        self.failures = 0;
        if self.scene != scene {
            self.previous = None;
            self.sequence = 0;
            self.scene = scene.into();
            self.sample_slots.clear();
        }
        let full = self
            .previous
            .as_ref()
            .is_none_or(|p| p.configuration_revision != current.configuration_revision);
        if full {
            self.sample_slots.clear();
        }
        for (surface, fps) in rates {
            let slot = now.saturating_duration_since(self.origin).as_nanos() * u128::from(*fps)
                / 1_000_000_000;
            if !full && self.sample_slots.get(surface) == Some(&slot) {
                if let Some(previous) = self.previous.as_ref() {
                    if surface == "@background" {
                        current.background = previous.background.clone();
                    } else if let Some(value) = previous.surfaces.get(surface) {
                        current.surfaces.insert(surface.clone(), value.clone());
                    }
                }
            } else {
                self.sample_slots.insert(surface.clone(), slot);
            }
        }
        let delta = difference(
            self.previous.as_ref(),
            &current,
            &self.generation,
            self.sequence + 1,
        );
        if delta.is_some() {
            self.sequence += 1;
        }
        self.previous = Some(current);
        delta
    }
    // Report the need for refresh
    pub fn needs_refresh(&self) -> bool {
        self.failures > 0 || self.previous.as_ref().is_none_or(|p| p.animated)
    }
    // Calculate the preview retry interval with bounded exponential backoff
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_nanos(
            ((1_000_000_000u64 / 60) << self.failures.min(4)).min(1_000_000_000),
        )
    }
}

impl crate::Runtime {
    // Preview frame rates using the current preview state
    pub fn preview_frame_rates(&self, device_id: &str) -> BTreeMap<String, u8> {
        let state = self.snapshot();
        let session = state.session(device_id);
        let Some(device) = state.devices.iter().find(|d| d.id == device_id) else {
            return BTreeMap::new();
        };
        let Some(profile) = state.profiles.iter().find(|p| p.id == session.profile_id) else {
            return BTreeMap::new();
        };
        let Some(page) = profile.pages.iter().find(|p| p.id == session.page_id) else {
            return BTreeMap::new();
        };
        let mut rates = BTreeMap::new();
        if profile.background_asset.is_some() {
            rates.insert(
                "@background".into(),
                profile
                    .background_asset
                    .as_ref()
                    .and_then(|asset| self.profiles.clip(asset).ok().flatten())
                    .map_or(24, |clip| {
                        profile
                            .background_media
                            .requested_fps(&clip)
                            .clamp(1.0, 60.0)
                            .ceil() as u8
                    }),
            );
        }
        for control in &device.manifest.controls {
            let source = page.surface_source(&device.manifest, &control.id);
            if let Some(binding) = page.bindings.get(source).filter(|b| {
                b.asset.is_some()
                    && b.feedback.as_deref().unwrap_or("off") == "off"
                    && !matches!(b.action, crate::actions::Action::Plugin { .. })
            }) {
                rates.insert(
                    control.id.clone(),
                    binding
                        .asset
                        .as_ref()
                        .and_then(|asset| self.profiles.clip(asset).ok().flatten())
                        .map_or(24, |clip| {
                            binding.media.requested_fps(&clip).clamp(1.0, 60.0).ceil() as u8
                        }),
                );
            }
        }
        for (source, animation) in self
            .vendor_animations
            .lock()
            .unwrap()
            .active(device_id, true)
        {
            if let Ok(control) = device.manifest.control(&source) {
                rates.insert(
                    control
                        .feedback_surface
                        .as_deref()
                        .unwrap_or(&source)
                        .into(),
                    animation.clip.prepared_fps().clamp(1.0, 60.0).ceil() as u8,
                );
            }
        }
        rates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that trigger description changes publish even when pixels do not change
    fn trigger_description_changes_publish_even_when_pixels_do_not_change() {
        let mut current = SurfacePreview {
            configuration_revision: 1,
            surfaces: BTreeMap::new(),
            trigger_descriptions: BTreeMap::new(),
            background: None,
            animated: false,
        };
        let previous = current.clone();
        current.trigger_descriptions.insert(
            "dial-1".into(),
            BTreeMap::from([("rotate".into(), "Adjust volume".into())]),
        );
        let delta = difference(Some(&previous), &current, "test", 2).unwrap();
        assert!(delta.surfaces.is_empty());
        assert_eq!(
            delta.trigger_descriptions["dial-1"]["rotate"],
            "Adjust volume"
        );
        assert!(difference(Some(&current), &current, "test", 3).is_none());
        let cleared = difference(Some(&current), &previous, "test", 4).unwrap();
        assert!(cleared.trigger_descriptions.is_empty());
    }
    #[test]
    // Verify that explicit preview rates keep old pixels until due but scene changes reset
    fn explicit_preview_rates_keep_old_pixels_until_due_but_scene_changes_reset() {
        let mut stream = SurfacePreviewStream::new("timed".into());
        let start = stream.origin;
        let frame = |pixel: &str, revision| SurfacePreview {
            trigger_descriptions: Default::default(),
            configuration_revision: revision,
            surfaces: BTreeMap::from([("key".into(), pixel.into())]),
            background: Some(pixel.into()),
            animated: true,
        };
        let rates = BTreeMap::from([("key".into(), 5), ("@background".into(), 5)]);
        assert!(
            stream
                .update_at("profile:first", Ok(frame("first", 1)), &rates, start)
                .unwrap()
                .reset
        );
        assert!(stream
            .update_at(
                "profile:first",
                Ok(frame("early", 1)),
                &rates,
                start + std::time::Duration::from_millis(40)
            )
            .is_none());
        let next = stream
            .update_at(
                "profile:first",
                Ok(frame("due", 1)),
                &rates,
                start + std::time::Duration::from_millis(200),
            )
            .unwrap();
        assert_eq!(next.surfaces["key"].as_deref(), Some("due"));
        assert_eq!(next.background.as_deref(), Some("due"));
        let profile = stream
            .update_at(
                "profile:second",
                Ok(frame("new-profile", 1)),
                &rates,
                start + std::time::Duration::from_millis(210),
            )
            .unwrap();
        assert!(profile.reset);
        assert_eq!(profile.surfaces["key"].as_deref(), Some("new-profile"));
        let edit = stream
            .update_at(
                "profile:second",
                Ok(frame("edited", 2)),
                &rates,
                start + std::time::Duration::from_millis(220),
            )
            .unwrap();
        assert_eq!(edit.configuration_revision, 2);
        assert_eq!(edit.surfaces["key"].as_deref(), Some("edited"));
    }
    #[test]
    // Verify that deltas send changes removals and resets without repeating static scenes
    fn deltas_send_changes_removals_and_resets_without_repeating_static_scenes() {
        let first = SurfacePreview {
            trigger_descriptions: Default::default(),
            configuration_revision: 1,
            surfaces: BTreeMap::from([
                ("key-1".into(), "one".into()),
                ("key-2".into(), "two".into()),
            ]),
            background: Some("background".into()),
            animated: false,
        };
        assert!(difference(None, &first, "a", 1).unwrap().reset);
        assert!(difference(Some(&first), &first, "a", 2).is_none());
        let next = SurfacePreview {
            trigger_descriptions: Default::default(),
            configuration_revision: 1,
            surfaces: BTreeMap::from([("key-1".into(), "changed".into())]),
            background: None,
            animated: false,
        };
        let delta = difference(Some(&first), &next, "a", 2).unwrap();
        assert_eq!(delta.surfaces["key-1"], Some("changed".into()));
        assert_eq!(delta.surfaces["key-2"], None);
        assert!(delta.background_changed);
        assert!(delta.background.is_none());
        let updated = SurfacePreview {
            trigger_descriptions: Default::default(),
            configuration_revision: 2,
            ..next.clone()
        };
        let delta = difference(Some(&next), &updated, "a", 3).unwrap();
        assert_eq!(delta.configuration_revision, 2);
        assert_eq!(delta.surfaces["key-1"], Some("changed".into()));
        assert!(delta.background_changed);
    }
    #[test]
    // Verify that preview stream recovers after render errors and delivers saved display changes
    fn preview_stream_recovers_after_render_errors_and_delivers_saved_display_changes() {
        let frame = |revision, image: &str| SurfacePreview {
            trigger_descriptions: Default::default(),
            configuration_revision: revision,
            surfaces: BTreeMap::from([("key-1".into(), image.into())]),
            background: None,
            animated: false,
        };
        let mut stream = SurfacePreviewStream::new("preview-test".into());
        let first = stream
            .update("profile:page", Ok(frame(1, "white")))
            .unwrap();
        assert!(first.reset);
        assert_eq!(first.sequence, 1);
        assert!(!stream.needs_refresh());
        for _ in 0..40 {
            assert!(stream
                .update(
                    "profile:page",
                    Err("Render scene changed during capture".into())
                )
                .is_none());
            assert!(stream.needs_refresh());
            assert!(stream.interval() <= std::time::Duration::from_secs(1));
        }
        let saved = stream
            .update("profile:page", Ok(frame(2, "purple")))
            .unwrap();
        assert!(!saved.reset);
        assert_eq!(saved.sequence, 2);
        assert_eq!(saved.configuration_revision, 2);
        assert_eq!(saved.surfaces["key-1"].as_deref(), Some("purple"));
        assert!(!stream.needs_refresh());
        assert_eq!(
            stream.interval(),
            std::time::Duration::from_nanos(1_000_000_000 / 60)
        );
        assert!(stream
            .update("profile:page", Ok(frame(2, "purple")))
            .is_none());
        let switched = stream
            .update("profile:other", Ok(frame(2, "other-page")))
            .unwrap();
        assert!(switched.reset);
        assert_eq!(switched.sequence, 1);
    }
    #[test]
    // Verify that initial render failure keeps the preview subscription ready for its first frame
    fn initial_render_failure_keeps_the_preview_subscription_ready_for_its_first_frame() {
        let mut stream = SurfacePreviewStream::new("preview-start".into());
        assert!(stream
            .update("profile:page", Err("Scene is changing".into()))
            .is_none());
        assert!(stream.needs_refresh());
        let first = stream
            .update(
                "profile:page",
                Ok(SurfacePreview {
                    trigger_descriptions: Default::default(),
                    configuration_revision: 4,
                    surfaces: BTreeMap::new(),
                    background: None,
                    animated: false,
                }),
            )
            .unwrap();
        assert!(first.reset);
        assert_eq!(first.sequence, 1);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct BindingPreview {
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub text_warnings: Vec<String>,
    pub image: String,
    /// off, live, waiting, inactive or image_only
    pub availability: String,
    pub elements: Vec<hardware_core::plugin_display::DisplayElement>,
    pub states: Vec<hardware_core::plugin_display::DisplayState>,
    pub current_state: Option<u32>,
    #[serde(default)]
    #[ts(
        optional,
        as = "Option<Vec<hardware_core::composition::DisplaySourceDescriptor>>"
    )]
    pub sources: Vec<hardware_core::composition::DisplaySourceDescriptor>,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub placeholder: bool,
}
impl crate::Runtime {
    /// Draft-only render; never registers plugin contexts or sends device output
    pub fn preview_binding_details(
        &self,
        device_id: &str,
        control_id: &str,
        binding: &crate::Binding,
        preview_state: Option<u32>,
    ) -> Result<BindingPreview, String> {
        use base64::Engine;
        binding.validate_visual_draft()?;
        let snapshot = self.snapshot();
        let device = snapshot
            .devices
            .iter()
            .find(|d| d.id == device_id)
            .ok_or("Device missing")?;
        let source = device
            .manifest
            .control(control_id)
            .map_err(|e| e.to_string())?;
        let control = device
            .manifest
            .control(source.feedback_surface.as_deref().unwrap_or(control_id))
            .map_err(|e| e.to_string())?;
        let session = snapshot.session(device_id);
        let scope = format!("{}:{}", session.profile_id, session.page_id);
        let mut visual = None;
        let mut availability = "off";
        let mut placeholder = false;
        let resolved = if matches!(binding.action, crate::actions::Action::Plugin { .. }) {
            self.resolve_parameters(&binding.action, device_id, &scope)?
        } else {
            binding.action.clone()
        };
        if let crate::actions::Action::Plugin {
            plugin_id,
            action_id,
            settings,
            ..
        } = &resolved
        {
            let host = self.plugins.lock().unwrap();
            visual = if binding.disabled {
                None
            } else {
                host.matching_surface_for_target(
                    device_id, control_id, &scope, plugin_id, action_id, settings, true,
                )
            };
            availability = if binding.disabled
                || !host.list().iter().any(|p| p.id == *plugin_id && p.enabled)
            {
                "inactive"
            } else if visual.is_some() {
                "live"
            } else {
                "waiting"
            };
        }
        if visual.is_none() {
            if let crate::actions::Action::Plugin {
                plugin_id,
                action_id,
                ..
            } = &resolved
            {
                let host = self.plugins.lock().unwrap();
                visual = host
                    .declared_surface(
                        device_id,
                        plugin_id,
                        action_id,
                        u32::from(control.image_width.unwrap_or(112)) * 10
                            > u32::from(control.image_height.unwrap_or(112)) * 13,
                    )
                    .or_else(|| {
                        let (mut snapshot, ()) = host.render_surfaces_for_target(
                            device_id,
                            &[],
                            &[(control_id.into(), plugin_id.clone(), action_id.clone())],
                            true,
                            || (),
                        );
                        snapshot
                            .fallbacks
                            .remove(control_id)
                            .and_then(|fallback| fallback.resolve())
                    });
                placeholder = visual.is_some();
            }
        }
        let elements = visual
            .as_ref()
            .map(hardware_core::plugin_display::elements)
            .unwrap_or_default();
        let states: Vec<hardware_core::plugin_display::DisplayState> = visual
            .as_ref()
            .and_then(|v| v["states"].as_array())
            .into_iter()
            .flatten()
            .filter_map(|s| serde_json::from_value(s.clone()).ok())
            .collect();
        let current_state = visual
            .as_ref()
            .and_then(|v| v["state"].as_u64())
            .map(|s| s as u32);
        if let Some(state) = preview_state {
            if states.iter().any(|s| s.index == state) {
                if let Some(visual) = &mut visual {
                    visual["state"] = serde_json::json!(state);
                }
            }
        }
        if visual.as_ref().is_some_and(|v| {
            !v["layout"].is_object()
                && v["image"].is_string()
                && v["title"].as_str().is_none_or(str::is_empty)
        }) {
            availability = "image_only";
        }
        let feedback = crate::feedback::text(
            self,
            binding.feedback.as_deref().unwrap_or("off"),
            device_id,
            &scope,
            control_id,
        );
        let rotation = device.manifest.layout.rotation;
        let plugin_id = match &resolved {
            crate::actions::Action::Plugin { plugin_id, .. }
                if binding.background_source
                    != Some(crate::profiles::BackgroundSource::Palette) =>
            {
                Some(plugin_id.as_str())
            }
            _ => None,
        };
        let (theme, colors) = self
            .plugins
            .lock()
            .unwrap()
            .device_display_colors(device_id, plugin_id);
        let navigation = crate::keyboard_navigation::protected_control(
            &device.manifest.model_id,
            &session.page_id,
            control_id,
        );
        let navigation_default = navigation.then(|| {
            let mut b = binding.clone();
            if b.appearance.is_none() && b.composition.is_none() {
                b.appearance = Some(Box::new(hardware_core::appearance::Appearance {
                    position: "center".into(),
                    foreground: colors["ink"].as_str().expect("theme ink").into(),
                    ..Default::default()
                }));
            }
            b
        });
        let binding = navigation_default.as_ref().unwrap_or(binding);
        let navigation_ink = (navigation
            && binding.background_source == Some(crate::profiles::BackgroundSource::Palette))
        .then(|| colors["ink"].as_str().expect("theme ink"));
        let themed_binding = Self::themed_button_binding(
            control,
            binding,
            theme,
            colors["surface"].as_str().expect("theme surface"),
            colors["ink"].as_str().expect("theme ink"),
        );
        let binding = themed_binding.as_ref().unwrap_or(binding);
        let sources = self.display_sources(
            control,
            binding,
            &feedback,
            visual.as_ref(),
            self.media_elapsed(),
            placeholder,
        )?;

        let elapsed = self.media_elapsed();
        let text_warnings = self.binding_text_checks(
            device,
            snapshot
                .profiles
                .iter()
                .find(|p| p.id == session.profile_id),
            control,
            binding,
            &feedback,
            visual.as_ref(),
            elapsed,
        )?;
        let bytes = self.render_binding_in_cycle(
            control,
            Some(binding),
            rotation,
            &feedback,
            visual.as_ref(),
            elapsed,
            true,
            None,
            navigation_ink,
        )?;
        let bytes = self.encode_surface_shared(
            bytes,
            Some(binding),
            &device.manifest.capabilities.image_formats,
            visual.is_some() || !feedback.is_empty(),
            false,
        )?;
        let bytes = self.preview_image(bytes, rotation)?;
        Ok(BindingPreview {
            text_warnings,
            image: format!(
                "data:{};base64,{}",
                crate::encoding::mime(&bytes),
                base64::engine::general_purpose::STANDARD.encode(&**bytes)
            ),
            availability: availability.into(),
            elements,
            states,
            current_state,
            sources,
            placeholder,
        })
    }
}

#[cfg(test)]
mod binding_preview_tests {
    #[test]
    // Verify that upright preview preserves every final hardware pixel including JPEG
    fn upright_preview_preserves_every_final_hardware_pixel_including_jpeg() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(85, 53, |x, y| {
            image::Rgb([
                (x * 71 + y * 13) as u8,
                (x * 7 + y * 43) as u8,
                (x * y) as u8,
            ])
        }));
        let png = hardware_core::render::encode_pixels(source, "png").unwrap();
        let mut exposed_old_mismatch = false;
        for format in ["png", "jpeg"] {
            for rotation in [0, 90, 180, 270] {
                let output = hardware_core::render::render(&png, 85, 53, rotation, format).unwrap();
                let hardware = image::load_from_memory(&output).unwrap();
                let upright = match rotation {
                    90 => hardware.rotate270(),
                    180 => hardware.rotate180(),
                    270 => hardware.rotate90(),
                    _ => hardware,
                }
                .to_rgba8();
                let preview = runtime
                    .preview_image(crate::media::shared_bytes(output.clone()), rotation)
                    .unwrap();
                assert_eq!(
                    image::load_from_memory(&preview).unwrap().to_rgba8(),
                    upright
                );
                assert_eq!(
                    runtime
                        .preview_image(crate::media::shared_bytes(output), rotation)
                        .unwrap(),
                    preview,
                    "cached preview changed"
                );
                let old = hardware_core::render::render(&png, 85, 53, 0, format).unwrap();
                exposed_old_mismatch |=
                    image::load_from_memory(&old).unwrap().to_rgba8() != upright;
            }
        }
        assert!(
            exposed_old_mismatch,
            "fixture must reproduce the independently encoded preview bug"
        );
        runtime.shutdown();
    }

    #[test]
    // Verify that preview is read only and never starts an inactive draft plugin
    fn preview_is_read_only_and_never_starts_an_inactive_draft_plugin() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        runtime
            .simulate_device(true, "simulator:preview-draft")
            .unwrap();
        let before = runtime.snapshot();
        let mut binding = before.profiles[0].pages[0].bindings["key-1"].clone();
        binding.action = crate::actions::Action::Plugin {
            user_desired_state: None,
            plugin_id: "fixture".into(),
            action_id: "new-action".into(),
            settings: serde_json::json!({"count":99}),
            resources: Default::default(),
        };
        let preview = runtime
            .preview_binding_details("simulator:preview-draft", "key-1", &binding, None)
            .unwrap();
        assert_eq!(preview.availability, "inactive");
        assert!(preview.elements.is_empty());
        assert!(preview.image.starts_with("data:image/"));
        let after = runtime.snapshot();
        assert_eq!(before.configuration_revision, after.configuration_revision);
        assert_eq!(
            serde_json::to_value(before.profiles).unwrap(),
            serde_json::to_value(after.profiles).unwrap()
        );
        assert!(runtime.plugins.lock().unwrap().list().is_empty());
        runtime.shutdown();
    }
}

impl crate::Runtime {
    // List system fonts and refresh device output when the font generation changes
    pub fn system_fonts(&self, refresh: bool) -> Vec<String> {
        let before = hardware_core::fonts::generation();
        let families = hardware_core::fonts::families(refresh);
        if hardware_core::fonts::generation() != before {
            self.emit("fonts-updated", None, None, None);
            let _ = self.device_commands.send(crate::devices::Request::Restore);
        }
        families
    }
    // Compose media, text and icon display sources with the binding’s appearance overrides
    pub(crate) fn display_sources(
        &self,
        control: &hardware_core::Control,
        binding: &crate::Binding,
        feedback: &str,
        visual: Option<&serde_json::Value>,
        elapsed: std::time::Duration,
        placeholder: bool,
    ) -> Result<Vec<hardware_core::composition::DisplaySourceDescriptor>, String> {
        use base64::Engine;
        let icon = binding
            .appearance
            .as_ref()
            .and_then(|a| hardware_core::appearance::appearance_icon_uri(a));
        let media = if let Some(asset) = binding
            .asset
            .as_ref()
            .filter(|asset| !binding.disabled || self.profiles.asset(asset).is_ok())
        {
            // Composition paints the binding color once, below the media frame
            let frame = self.profiles.render_media_shared(
                asset,
                elapsed,
                control.image_width.ok_or("No display surface")?,
                control.image_height.ok_or("No display surface")?,
                0,
                "png",
                &binding.media,
            )?;
            Some(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&**frame)
            ))
        } else {
            None
        };
        let mut sources = hardware_core::composition::sources(
            &binding.title,
            icon.as_deref(),
            media.as_deref(),
            feedback,
            visual,
            placeholder,
        );
        for source in &mut sources {
            if source.source.kind == "title" {
                source.style.visible =
                    visual.is_none() || binding.appearance.as_ref().is_some_and(|a| a.show_title);
                if let Some(a) = &binding.appearance {
                    source.style.font = a.font.clone();
                    source.style.size = a.size as f64;
                    source.style.foreground = a.foreground.clone();
                    source.style.align = a.align.clone();
                    source.style.rect[1] = match a.position.as_str() {
                        "top" => 2.,
                        "center" => 38.,
                        _ => 74.,
                    };
                }
            }
            if source.source.kind == "icon" {
                if let Some(a) = &binding.appearance {
                    let size = a.icon_size as f64;
                    source.style.rect = [
                        (100. - size) / 2.,
                        match a.icon_position.as_str() {
                            "top" => 2.,
                            "bottom" => 98. - size,
                            _ => (100. - size) / 2.,
                        },
                        size,
                        size,
                    ];
                }
            }
        }
        if let (Some(style), Some(visual)) = (&binding.plugin_appearance, visual) {
            if style.plugin_id == visual["plugin_id"] && style.action_id == visual["action_id"] {
                for source in &mut sources {
                    if source.source.kind == "plugin" {
                        if let Some(s) = style.elements.iter().find(|s| {
                            s.layout == source.style.layout
                                && s.key == source.style.key
                                && s.kind == source.style.kind
                        }) {
                            source.style = s.clone();
                        }
                    }
                }
            }
        }
        Ok(sources)
    }
}

#[cfg(test)]
mod composition_runtime_tests {
    #[test]
    #[ignore = "Uses an isolated loopback host and simulator; never HID"]
    // Verify that composition preview matches saved simulator output
    fn composition_preview_matches_saved_simulator_output() {
        use std::collections::BTreeMap;
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:composition";
        runtime.simulate_device(true, id).unwrap();
        let before = runtime.snapshot();
        let session = before.session(id);
        let profile = before
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap();
        let page = profile
            .pages
            .iter()
            .find(|p| p.id == session.page_id)
            .unwrap();
        let mut binding = page.bindings["key-1"].clone();
        binding.action = crate::actions::Action::None;
        binding.appearance = None;
        binding.plugin_appearance = None;
        binding.asset = None;
        binding.feedback = None;
        binding.composition = Some(Box::new(hardware_core::composition::DisplayComposition {
            layers: vec![hardware_core::composition::DisplayLayer {
                id: "value".into(),
                label: "Value".into(),
                source: hardware_core::composition::DisplaySource {
                    kind: "text".into(),
                    ..Default::default()
                },
                style: hardware_core::plugin_display::ElementStyle {
                    layout: "$composition".into(),
                    key: "value".into(),
                    size: 24.,
                    ..Default::default()
                },
                text: "54.5 °F".into(),
            }],
        }));
        let preview = runtime
            .preview_binding_details(id, "key-1", &binding, None)
            .unwrap();
        if std::env::var_os("STREAM_DOCK_MANAGER_CAPTURE_COMPOSITION").is_some() {
            use base64::Engine;
            let artifacts =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            let data = base64::engine::general_purpose::STANDARD
                .decode(preview.image.split_once(',').unwrap().1)
                .unwrap();
            std::fs::write(artifacts.join("composition-simulator.png"), data).unwrap();
        }
        runtime
            .apply_edits(
                before.configuration_revision,
                "Compose simulator",
                vec![crate::workspace::Edit::Bindings {
                    profile_id: profile.id.clone(),
                    page_id: page.id.clone(),
                    bindings: BTreeMap::from([("key-1".into(), Some(binding.clone()))]),
                }],
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let surfaces = runtime.preview_surfaces(Some(id)).unwrap();
            if surfaces.surfaces.get("key-1") == Some(&preview.image) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "preview and saved simulator composition differ"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        binding.composition.as_mut().unwrap().layers[0]
            .style
            .foreground = "#4d22b3".into();
        let recolored = runtime
            .preview_binding_details(id, "key-1", &binding, None)
            .unwrap();
        assert_ne!(
            recolored.image, preview.image,
            "Color edits must change native pixels"
        );
        let saved = runtime.snapshot();
        runtime
            .apply_edits(
                saved.configuration_revision,
                "Save appearance color",
                vec![crate::workspace::Edit::Bindings {
                    profile_id: profile.id.clone(),
                    page_id: page.id.clone(),
                    bindings: BTreeMap::from([("key-1".into(), Some(binding))]),
                }],
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let surfaces = runtime.preview_surfaces(Some(id)).unwrap();
            if surfaces.surfaces.get("key-1") == Some(&recolored.image) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Saved color did not replace native simulator pixels"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        runtime.shutdown();
    }
}

#[cfg(test)]
mod system_font_tests {
    #[test]
    #[ignore = "Requires installed host fonts and loopback sockets; never opens HID"]
    // Verify that saved system fonts match native simulator and survive portable export
    fn saved_system_fonts_match_native_simulator_and_survive_portable_export() {
        use crate::actions::Action;
        use hardware_core::composition::{DisplayComposition, DisplayLayer, DisplaySource};
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::Runtime::build(root.path().join("app"), false).unwrap();
        let device = "simulator:n4-pro:fonts";
        runtime.simulate_device(true, device).unwrap();
        let names = runtime.system_fonts(false);
        let family = [
            "Georgia",
            "Times New Roman",
            "Arial",
            "Menlo",
            "DejaVu Serif",
            "Liberation Serif",
        ]
        .iter()
        .find_map(|wanted| names.iter().find(|name| name.eq_ignore_ascii_case(wanted)))
        .unwrap();
        let font = format!("system:{family}");
        let mut profile = runtime.snapshot().profiles[0].clone();
        let mut binding = profile.pages[0].bindings["key-1"].clone();
        binding.action = Action::None;
        binding.title = "Wi WWW 123".into();
        binding.asset = None;
        binding.appearance = Some(Box::default());
        let noto = runtime
            .preview_binding_details(device, "key-1", &binding, None)
            .unwrap();
        binding.appearance.as_mut().unwrap().font = font.clone();
        let selected = runtime
            .preview_binding_details(device, "key-1", &binding, None)
            .unwrap();
        assert_ne!(selected.image, noto.image);
        binding.composition = Some(Box::new(DisplayComposition {
            layers: vec![DisplayLayer {
                id: "title".into(),
                label: "Title".into(),
                source: DisplaySource {
                    kind: "title".into(),
                    ..Default::default()
                },
                style: hardware_core::plugin_display::ElementStyle {
                    layout: "$composition".into(),
                    key: "title".into(),
                    font: font.clone(),
                    rect: [5., 20., 90., 60.],
                    size: 26.,
                    ..Default::default()
                },
                text: String::new(),
            }],
        }));
        let canvas = runtime
            .preview_binding_details(device, "key-1", &binding, None)
            .unwrap();
        let mut default = binding.clone();
        default.composition.as_mut().unwrap().layers[0].style.font = "sans".into();
        assert_ne!(
            canvas.image,
            runtime
                .preview_binding_details(device, "key-1", &default, None)
                .unwrap()
                .image
        );
        profile.pages[0].bindings.insert("key-1".into(), binding);
        runtime
            .save_profile(profile, runtime.snapshot().configuration_revision)
            .unwrap();
        let surfaces = runtime.preview_surfaces(Some(device)).unwrap();
        assert_eq!(surfaces.surfaces["key-1"], canvas.image);
        let snapshot = runtime.snapshot();
        let context = runtime.render_scene(device).unwrap();
        runtime.system_fonts(true);
        assert!(
            !context.current(&runtime),
            "Refresh must invalidate renders from the previous font database"
        );
        assert_eq!(
            runtime.snapshot().configuration_revision,
            snapshot.configuration_revision
        );
        assert_eq!(
            runtime.preview_surfaces(Some(device)).unwrap().surfaces["key-1"],
            canvas.image
        );
        let archive = root.path().join("fonts.zip");
        runtime.export_profiles(&archive).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap()).unwrap();
        let mut json = String::new();
        use std::io::Read;
        zip.by_name("profiles.json")
            .unwrap()
            .read_to_string(&mut json)
            .unwrap();
        let data: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            data["profiles"][0]["pages"][0]["bindings"]["key-1"]["composition"]["layers"][0]
                ["style"]["font"],
            font
        );
        assert!(
            zip.file_names()
                .all(|name| name.is_ok_and(|name| ![".ttf", ".otf", ".ttc"]
                    .iter()
                    .any(|extension| name.ends_with(*extension)))),
            "System fonts are referenced, never copied into portable archives"
        );
        runtime.shutdown();
    }
}
