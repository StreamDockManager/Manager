//! A short visual-only lease. It never enters persisted profiles or plugin demand
use crate::{Binding, Runtime};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackgroundDraft {
    pub asset: Option<String>,
    pub media: crate::media::MediaOptions,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaletteDraft {
    pub mode: String,
    pub id: String,
    #[serde(default)]
    #[ts(optional)]
    pub custom: Option<serde_json::Value>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HardwareDraft {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display_test: Option<DisplayTest>,
    #[serde(default)]
    #[ts(optional)]
    pub palette: Option<PaletteDraft>,
    pub bindings: BTreeMap<String, Binding>,
    pub background: Option<BackgroundDraft>,
    #[serde(default)]
    #[ts(optional)]
    pub scope: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<BTreeMap<String, u32>>")]
    pub preview_states: BTreeMap<String, u32>,
}
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Pattern {
    Red,
    Green,
    Blue,
    Black,
    White,
    Alignment,
    Positions,
    Marker,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DisplayTest {
    pub pattern: Pattern,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub background_color: Option<[u8; 3]>,
    pub frame: u8,
    pub previous_token: Option<String>,
}
pub(crate) struct Lease {
    started: Instant,
    pub token: String,
    pub epoch: u64,
    pub revision: u64,
    pub scope: String,
    pub until: Instant,
    pub draft: HardwareDraft,
    _assets: crate::asset_leases::AssetLease,
}
#[derive(Default)]
pub(crate) struct Previews {
    leases: HashMap<String, Lease>,
    expiry_worker: bool,
}
impl Runtime {
    pub(super) fn test_pixels(
        &self,
        test: &DisplayTest,
        [w, h]: [u16; 2],
        rotation: u16,
        surface: Option<&hardware_core::Control>,
        device: &str,
        formats: &[String],
    ) -> Result<crate::media::SharedBytes, String> {
        let state = self.state.lock().unwrap();
        let model = &state
            .devices
            .iter()
            .find(|d| d.id == device)
            .ok_or("Device disconnected")?
            .manifest;
        let surfaces: Vec<_> = model
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some())
            .collect();
        let index = surface.and_then(|c| surfaces.iter().position(|s| s.id == c.id));
        let touch: Vec<_> = surfaces.iter().filter(|c| c.kind == "touch").collect();
        let active = touch
            .get(usize::from(test.frame) / 8 % touch.len().max(1))
            .is_some_and(|c| surface.is_some_and(|s| s.id == c.id));
        drop(state);
        let fmt = if formats.iter().any(|f| f == "png") {
            "png"
        } else {
            "jpeg"
        };
        let background_color = if surface.is_none() {
            test.background_color
        } else {
            None
        };
        self.cached_render(
            crate::media::RenderKey::from_inputs(&(
                "display test",
                test.pattern,
                background_color,
                test.frame,
                index,
                active,
                w,
                h,
                rotation,
                fmt,
            ))?,
            || {
                let color = background_color.unwrap_or(match test.pattern {
                    Pattern::Red => [255, 0, 0],
                    Pattern::Green => [0, 255, 0],
                    Pattern::Blue => [0, 0, 255],
                    Pattern::White => [255; 3],
                    _ => [0; 3],
                });
                let bytes = if background_color.is_none()
                    && matches!(test.pattern, Pattern::Positions)
                {
                    let title = index.map(|i| (i + 1).to_string()).unwrap_or_default();
                    hardware_core::render::tile(w, h, color, &title, rotation, fmt)
                } else {
                    let pixels = image::RgbaImage::from_fn(w.into(), h.into(), |x, y| {
                        let on = if background_color.is_some() {
                            false
                        } else if matches!(test.pattern, Pattern::Alignment) {
                            x == 0
                                || y == 0
                                || x + 1 == u32::from(w)
                                || y + 1 == u32::from(h)
                                || x == u32::from(w) / 2
                                || y == u32::from(h) / 2
                        } else {
                            matches!(test.pattern, Pattern::Marker)
                                && active
                                && x.abs_diff(
                                    u32::from(test.frame % 8) * u32::from(w.saturating_sub(1)) / 7,
                                ) < 3
                        };
                        image::Rgba(if on {
                            [255; 4]
                        } else {
                            [color[0], color[1], color[2], 255]
                        })
                    });
                    let img = image::DynamicImage::ImageRgba8(pixels);
                    let img = match rotation {
                        90 => img.rotate90(),
                        180 => img.rotate180(),
                        270 => img.rotate270(),
                        _ => img,
                    };
                    hardware_core::render::encode_pixels(img, fmt)
                }
                .map_err(|e| e.to_string())?;
                Ok(crate::media::shared_bytes(bytes))
            },
        )
    }

    // Resolve an unsaved visual candidate without mutating plugin theme or persisted state
    pub(crate) fn resolve_preview_palette(
        &self,
        palette: &PaletteDraft,
    ) -> Result<
        (
            serde_json::Value,
            Option<stream_dock_manager_plugin_host::PaletteWallpaper>,
        ),
        String,
    > {
        let custom = palette
            .custom
            .as_ref()
            .map(|value| {
                serde_json::from_value::<stream_dock_manager_plugin_host::CustomPalette>(
                    value.clone(),
                )
                .map_err(|_| "Invalid custom palette draft".to_string())
            })
            .transpose()?;
        self.plugins.lock().unwrap().preview_device_palette(
            &palette.mode,
            &palette.id,
            custom.as_ref(),
        )
    }
    // Preview on dock using the current hardware preview state
    pub fn preview_on_dock(
        self: &Arc<Self>,
        device_id: &str,
        expected: u64,
        draft: HardwareDraft,
    ) -> Result<String, String> {
        let palette = draft
            .palette
            .as_ref()
            .map(|palette| self.resolve_preview_palette(palette))
            .transpose()?;
        let palette_asset = palette
            .as_ref()
            .and_then(|(_, wallpaper)| wallpaper.as_ref())
            .map(|wallpaper| wallpaper.asset_id.clone());
        if let Some((_, Some(wallpaper))) = &palette {
            self.profiles.asset_extension(&wallpaper.asset_id)?;
            let media: crate::media::MediaOptions =
                serde_json::from_value(wallpaper.options.clone())
                    .map_err(|_| "Invalid palette wallpaper media options")?;
            media.validate()?;
        }
        let epochs = self.connection_epochs.lock().unwrap();
        let epoch = *epochs.get(device_id).ok_or("Device disconnected")?;
        let state = self.state.lock().unwrap();
        if state.configuration_revision != expected {
            return Err("Configuration changed. Reopen the draft before previewing.".into());
        }
        let session = state.session(device_id);
        let scope = format!("{}:{}", session.profile_id, session.page_id);
        if draft
            .scope
            .as_ref()
            .is_some_and(|expected| expected != &scope)
        {
            return Err("Preview target changed. Reopen the draft before previewing.".into());
        }
        let device = state
            .devices
            .iter()
            .find(|d| d.id == device_id && d.connected)
            .ok_or("Device disconnected")?;
        if device.simulated && draft.display_test.is_none() {
            return Err("Preview on dock requires a physical device".into());
        }
        if self
            .power
            .effective(device_id, epoch, &state.session(device_id).output)
            .sleeping
        {
            return Err("Wake the dock before previewing".into());
        }
        if draft.display_test.is_some()
            && (!device
                .manifest
                .controls
                .iter()
                .any(|c| c.image_slot.is_some())
                && device.manifest.capabilities.background_size.is_none()
                || !draft.bindings.is_empty()
                || draft.palette.is_some()
                || draft.background.is_some())
        {
            return Err("Choose a device with a display to test.".into());
        }
        if draft.bindings.len() > 100
            || draft.bindings.is_empty()
                && draft.background.is_none()
                && draft.palette.is_none()
                && draft.display_test.is_none()
        {
            return Err("Choose draft surfaces to preview".into());
        }
        if draft
            .preview_states
            .keys()
            .any(|id| !draft.bindings.contains_key(id))
        {
            return Err("Preview states must belong to draft bindings".into());
        }
        for (id, binding) in &draft.bindings {
            let source = device.manifest.control(id).map_err(|e| e.to_string())?;
            let surface = device
                .manifest
                .control(source.feedback_surface.as_deref().unwrap_or(id))
                .map_err(|e| e.to_string())?;
            if surface.image_slot.is_none() {
                return Err("This control has no live output surface".into());
            }
            binding.validate_visual_draft()?;
            self.validate_binding(binding, &device.manifest, source)?;
        }
        if let Some(background) = &draft.background {
            if !device.manifest.capabilities.background {
                return Err("This device has no live background output".into());
            }
            background.media.validate()?;
            if let Some(asset) = &background.asset {
                self.profiles.asset_extension(asset)?;
                if let Some(clip) = self.profiles.clip(asset)? {
                    if background.media.trim_start_ms >= clip.duration_ms
                        || background
                            .media
                            .trim_end_ms
                            .is_some_and(|end| end > clip.duration_ms)
                    {
                        return Err("Background trim exceeds clip duration".into());
                    }
                }
            }
        }
        let assets = self.profiles.asset_leases.pin(
            draft
                .bindings
                .values()
                .filter_map(|b| b.asset.clone())
                .chain(draft.background.iter().filter_map(|b| b.asset.clone()))
                .chain(palette_asset),
            "Temporary hardware preview",
        )?;
        let token = uuid::Uuid::new_v4().to_string();
        let mut previews = self.hardware_previews.lock().unwrap();
        let now = Instant::now();
        let started = if let Some(previous) = draft
            .display_test
            .as_ref()
            .and_then(|p| p.previous_token.as_ref())
        {
            let lease = previews
                .leases
                .get_mut(device_id)
                .filter(|l| {
                    &l.token == previous
                        && l.epoch == epoch
                        && l.revision == expected
                        && l.scope == scope
                        && l.until > now
                        && l.started.elapsed() < Duration::from_secs(30)
                })
                .ok_or("Display test expired or another preview took ownership.")?;
            if lease
                .draft
                .display_test
                .as_ref()
                .zip(draft.display_test.as_ref())
                .is_some_and(|(a, b)| {
                    a.pattern == b.pattern
                        && a.frame == b.frame
                        && a.background_color == b.background_color
                })
            {
                lease.until =
                    (now + Duration::from_secs(5)).min(lease.started + Duration::from_secs(30));
                return Ok(lease.token.clone());
            }
            lease.started
        } else {
            now
        };
        previews.leases.insert(
            device_id.into(),
            Lease {
                started,
                token: token.clone(),
                epoch,
                revision: expected,
                scope,
                until: (now + Duration::from_secs(5)).min(started + Duration::from_secs(30)),
                draft,
                _assets: assets,
            },
        );
        let start_expiry_worker = !previews.expiry_worker;
        previews.expiry_worker = true;
        drop(previews);
        drop(state);
        drop(epochs);
        self.preview_subscriptions
            .scene_changed("display-theme", Some(device_id));
        let _ = self.device_commands.send(crate::devices::Request::Wake);
        if start_expiry_worker {
            let weak = Arc::downgrade(self);
            // One expiry worker owns all leases. Live edits refresh deadlines,
            // rather than creating a sleeping thread for every keystroke
            std::thread::spawn(move || loop {
                let Some(runtime) = weak.upgrade() else {
                    return;
                };
                let wait = runtime.expire_hardware_previews(Instant::now());
                drop(runtime);
                let Some(wait) = wait else { return };
                std::thread::sleep(wait);
            });
        }
        Ok(token)
    }
    // Signal visible streams only after lease changes become authoritative and locks are released
    fn hardware_preview_changed(&self, devices: Vec<String>) {
        for device in devices {
            self.preview_subscriptions
                .scene_changed("display-theme", Some(&device));
        }
        let _ = self.device_commands.send(crate::devices::Request::Wake);
    }
    // Retire expired leases and wake both physical output and parked software preview subscribers
    fn expire_hardware_previews(&self, now: Instant) -> Option<Duration> {
        let mut previews = self.hardware_previews.lock().unwrap();
        let removed: Vec<_> = previews
            .leases
            .iter()
            .filter(|(_, lease)| lease.until <= now)
            .map(|(id, _)| id.clone())
            .collect();
        previews.leases.retain(|_, lease| lease.until > now);
        let wait = previews
            .leases
            .values()
            .map(|lease| lease.until.saturating_duration_since(now))
            .min();
        if wait.is_none() {
            previews.expiry_worker = false;
        }
        drop(previews);
        if !removed.is_empty() {
            self.hardware_preview_changed(removed);
        }
        wait
    }
    // Cancel only the current matching token without waking unrelated device streams
    pub fn cancel_hardware_preview(&self, token: Option<&str>) {
        let mut previews = self.hardware_previews.lock().unwrap();
        let removed = previews
            .leases
            .iter()
            .filter(|(_, lease)| token.is_none_or(|token| token == lease.token))
            .map(|(id, _)| id.clone())
            .collect();
        previews
            .leases
            .retain(|_, lease| token.is_some_and(|token| token != lease.token));
        drop(previews);
        self.hardware_preview_changed(removed);
    }
    // Invalidate hardware preview and publish the restored software scene after retirement
    pub(crate) fn invalidate_hardware_preview(&self, device: Option<&str>) {
        let mut previews = self.hardware_previews.lock().unwrap();
        let removed = previews
            .leases
            .keys()
            .filter(|id| device.is_none_or(|device| device == id.as_str()))
            .cloned()
            .collect();
        previews
            .leases
            .retain(|id, _| device.is_some_and(|device| device != id));
        drop(previews);
        self.hardware_preview_changed(removed);
    }
    // Read a hardware preview only while its connection, page scope and lease remain current
    pub(crate) fn hardware_preview(&self, device: &str) -> Option<(String, HardwareDraft)> {
        let epoch = self.connection_generation(device)?;
        let state = self.state.lock().unwrap();
        let session = state.session(device);
        let scope = format!("{}:{}", session.profile_id, session.page_id);
        let previews = self.hardware_previews.lock().unwrap();
        let local = previews
            .leases
            .get(device)
            .filter(|lease| {
                lease.epoch == epoch
                    && lease.revision == state.configuration_revision
                    && lease.scope == scope
                    && lease.until > Instant::now()
            })
            .map(|lease| (lease.token.clone(), lease.draft.clone()));
        drop(previews);
        drop(state);
        if let Some((global_token, palette)) = self.device_palette_preview() {
            let (local_token, mut draft) = local.unwrap_or_default();
            draft.palette = Some(palette);
            return Some((format!("{global_token}:{local_token}"), draft));
        }
        local
    }
    // Read the token for the currently valid hardware preview lease
    pub(crate) fn hardware_preview_token(&self, device: &str) -> Option<String> {
        self.hardware_preview(device).map(|value| value.0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::Action;
    // Verify a parked subscription wakes without relying on animation or another runtime event
    fn assert_preview_wake(runtime: &Arc<Runtime>, device: &str, edit: impl FnOnce()) {
        let id = uuid::Uuid::new_v4().to_string();
        let subscription = runtime
            .preview_subscriptions
            .register_device(id.clone(), Some(device.into()));
        assert!(subscription.wait_for_frame(Instant::now()));
        edit();
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            send.send(subscription.wait_for_frame(Instant::now()))
                .unwrap()
        });
        let result = receive.recv_timeout(Duration::from_secs(1));
        if result.is_err() {
            runtime.preview_subscriptions.cancel(&id);
        }
        thread.join().unwrap();
        assert!(
            result.unwrap(),
            "Preview lease change did not wake its software stream"
        );
    }

    #[test]
    // Static LCD streams apply and restore temporary palettes even when no other event occurs
    fn display_test_renews_only_its_lease_and_preserves_the_saved_scene() {
        let root = tempfile::tempdir().unwrap();
        let rt = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:display-pattern";
        let state = rt.simulate_device(true, id).unwrap();
        let rev = state.configuration_revision;
        let draft = |pattern, background_color, previous_token| HardwareDraft {
            display_test: Some(DisplayTest {
                pattern,
                background_color,
                frame: 0,
                previous_token,
            }),
            ..Default::default()
        };
        let token = rt
            .preview_on_dock(id, rev, draft(Pattern::Red, None, None))
            .unwrap();
        assert_eq!(
            rt.preview_on_dock(id, rev, draft(Pattern::Red, None, Some(token.clone())))
                .unwrap(),
            token
        );
        let device = state.devices.iter().find(|d| d.id == id).unwrap();
        let surface = device
            .manifest
            .controls
            .iter()
            .find(|c| c.image_slot.is_some())
            .unwrap();
        let data = rt
            .test_pixels(
                &DisplayTest {
                    pattern: Pattern::Red,
                    background_color: Some([16, 96, 128]),
                    frame: 0,
                    previous_token: None,
                },
                [8, 4],
                90,
                Some(surface),
                id,
                &["png".into()],
            )
            .unwrap();
        let image = image::load_from_memory(&data).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (4, 8));
        assert_eq!(image.get_pixel(0, 0).0, [255, 0, 0]);
        let custom = [16, 96, 128];
        let updated = rt
            .preview_on_dock(
                id,
                rev,
                draft(Pattern::Red, Some(custom), Some(token.clone())),
            )
            .unwrap();
        assert_ne!(updated, token);
        let (_, lease) = rt.hardware_preview(id).unwrap();
        let test = lease.display_test.as_ref().unwrap();
        for (surface, expected) in [(None, custom), (Some(surface), [255, 0, 0])] {
            let pixels = rt
                .test_pixels(test, [8, 4], 0, surface, id, &["png".into()])
                .unwrap();
            assert!(image::load_from_memory(&pixels)
                .unwrap()
                .to_rgb8()
                .pixels()
                .all(|pixel| pixel.0 == expected));
        }
        let next = rt
            .preview_on_dock(id, rev, draft(Pattern::Blue, None, None))
            .unwrap();
        assert!(rt
            .preview_on_dock(id, rev, draft(Pattern::Red, None, Some(token.clone())))
            .is_err());
        rt.cancel_hardware_preview(Some(&token));
        assert_eq!(rt.hardware_preview_token(id), Some(next.clone()));
        rt.cancel_hardware_preview(Some(&next));
        assert!(rt.hardware_preview(id).is_none());
        assert_eq!(rev, rt.snapshot().configuration_revision);
        rt.shutdown();
    }
    #[test]
    fn palette_lease_wakes_static_stream_on_start_cancel_and_expiry() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:palette-stream";
        runtime.simulate_device(true, id).unwrap();
        runtime.state.lock().unwrap().devices[0].simulated = false;
        let revision = runtime.snapshot().configuration_revision;
        let light = PaletteDraft {
            mode: "light".into(),
            id: "default".into(),
            custom: None,
        };
        let mode = if runtime.render_scene(id).unwrap().device_surface
            == runtime.resolve_preview_palette(&light).unwrap().0["surface"]
                .as_str()
                .unwrap()
        {
            "dark"
        } else {
            "light"
        };
        let draft = HardwareDraft {
            palette: Some(PaletteDraft {
                mode: mode.into(),
                id: "default".into(),
                custom: None,
            }),
            ..Default::default()
        };
        let saved = runtime.preview_surfaces_binary(id).unwrap().0;
        let mut token = String::new();
        assert_preview_wake(&runtime, id, || {
            token = runtime
                .preview_on_dock(id, revision, draft.clone())
                .unwrap();
        });
        let candidate = runtime.preview_surfaces_binary(id).unwrap().0;
        assert!(
            saved.surfaces != candidate.surfaces || saved.background != candidate.background,
            "Candidate must change actual encoded LCD/background frames"
        );
        assert_preview_wake(&runtime, id, || {
            runtime.cancel_hardware_preview(Some(&token))
        });
        let restored = runtime.preview_surfaces_binary(id).unwrap().0;
        assert_eq!(restored.surfaces, saved.surfaces);
        assert_eq!(restored.background, saved.background);
        runtime.preview_on_dock(id, revision, draft).unwrap();
        assert_preview_wake(&runtime, id, || {
            runtime.expire_hardware_previews(Instant::now() + Duration::from_secs(6));
        });
        assert!(runtime.hardware_preview(id).is_none());
    }

    #[test]
    // Candidate colors are isolated to one live lease and never replace saved configuration
    fn palette_preview_is_scoped_visual_only_and_restores_latest_saved_state() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:palette-candidate";
        runtime.simulate_device(true, id).unwrap();
        let other = "simulator:n4-pro:palette-other";
        runtime.simulate_device(true, other).unwrap();
        let other_before = runtime.render_scene(other).unwrap().device_surface;
        let before = runtime.snapshot();
        let draft = HardwareDraft {
            palette: Some(PaletteDraft {
                mode: "light".into(),
                id: "default".into(),
                custom: None,
            }),
            scope: Some(format!(
                "{}:{}",
                before.session(id).profile_id,
                before.session(id).page_id
            )),
            ..Default::default()
        };
        assert!(runtime
            .preview_on_dock(id, before.configuration_revision, draft.clone())
            .unwrap_err()
            .contains("physical"));
        runtime
            .state
            .lock()
            .unwrap()
            .devices
            .iter_mut()
            .find(|d| d.id == id)
            .unwrap()
            .simulated = false;
        let saved = serde_json::to_value(&runtime.snapshot().profiles).unwrap();
        let original = runtime.render_scene(id).unwrap();
        let token = runtime
            .preview_on_dock(id, before.configuration_revision, draft.clone())
            .unwrap();
        let preview = runtime.render_scene(id).unwrap();
        let mut saved_wallpaper = before.profiles[0].clone();
        saved_wallpaper.background_asset = Some(uuid::Uuid::new_v4().to_string());
        assert!(original.background_media(Some(&saved_wallpaper)).is_some());
        assert!(
            preview.background_media(Some(&saved_wallpaper)).is_some(),
            "A custom background must take precedence over the candidate palette"
        );
        assert!(runtime.hardware_preview(other).is_none());
        assert_eq!(
            runtime.render_scene(other).unwrap().device_surface,
            other_before
        );
        let expected = runtime
            .resolve_preview_palette(draft.palette.as_ref().unwrap())
            .unwrap()
            .0;
        assert_eq!(
            preview.device_surface,
            expected["surface"].as_str().unwrap()
        );
        assert_eq!(preview.wallpaper, expected["wallpaper"].as_str().unwrap());
        assert_eq!(
            serde_json::to_value(&runtime.snapshot().profiles).unwrap(),
            saved
        );
        assert_eq!(
            runtime.snapshot().configuration_revision,
            before.configuration_revision
        );
        let newer = runtime
            .preview_on_dock(id, before.configuration_revision, draft.clone())
            .unwrap();
        runtime.cancel_hardware_preview(Some(&token));
        assert_eq!(
            runtime.hardware_preview_token(id).as_deref(),
            Some(newer.as_str())
        );
        runtime.cancel_hardware_preview(Some(&newer));
        assert_eq!(
            runtime.render_scene(id).unwrap().device_surface,
            original.device_surface
        );
        let token = runtime
            .preview_on_dock(id, before.configuration_revision, draft.clone())
            .unwrap();
        runtime
            .hardware_previews
            .lock()
            .unwrap()
            .leases
            .get_mut(id)
            .unwrap()
            .until = Instant::now();
        assert!(runtime.hardware_preview_token(id).is_none());
        runtime.cancel_hardware_preview(Some(&token));
        runtime
            .preview_on_dock(id, before.configuration_revision, draft.clone())
            .unwrap();
        runtime.state.lock().unwrap().configuration_revision += 1;
        assert!(
            runtime.hardware_preview(id).is_none(),
            "A save must not restore old draft state"
        );
        assert!(runtime
            .preview_on_dock(id, before.configuration_revision, draft)
            .is_err());
    }

    #[test]
    // Invalid candidates cannot replace an active valid visual lease
    fn invalid_palette_candidate_preserves_current_lease() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:palette-invalid";
        runtime.simulate_device(true, id).unwrap();
        runtime.state.lock().unwrap().devices[0].simulated = false;
        let revision = runtime.snapshot().configuration_revision;
        let mut draft = HardwareDraft {
            palette: Some(PaletteDraft {
                mode: "dark".into(),
                id: "default".into(),
                custom: None,
            }),
            ..Default::default()
        };
        let token = runtime
            .preview_on_dock(id, revision, draft.clone())
            .unwrap();
        draft.palette.as_mut().unwrap().mode = "invalid".into();
        assert!(runtime.preview_on_dock(id, revision, draft).is_err());
        assert_eq!(
            runtime.hardware_preview_token(id).as_deref(),
            Some(token.as_str())
        );
    }

    #[test]
    // Verify that empty keyboard draft and repeated live updates are visual only
    fn empty_keyboard_draft_and_repeated_live_updates_are_visual_only() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:live-draft";
        runtime.simulate_device(true, id).unwrap();
        runtime.state.lock().unwrap().devices[0].simulated = false;
        let before = runtime.snapshot();
        let mut binding = before.profiles[0].pages[0].bindings["key-1"].clone();
        binding.action = Action::Builtin {
            action: Box::new(crate::builtins::BuiltinAction::KeyboardMacro {
                recording: Default::default(),
                playback: Default::default(),
            }),
        };
        assert!(binding.validate().is_err());
        let draft = HardwareDraft {
            bindings: BTreeMap::from([("key-1".into(), binding)]),
            ..Default::default()
        };
        let first = runtime
            .preview_on_dock(id, before.configuration_revision, draft.clone())
            .unwrap();
        let mut stale = draft.clone();
        stale.scope = Some("different:page".into());
        assert!(runtime
            .preview_on_dock(id, before.configuration_revision, stale)
            .unwrap_err()
            .contains("target changed"));
        assert_eq!(runtime.hardware_preview_token(id), Some(first.clone()));
        let latest = runtime
            .preview_on_dock(id, before.configuration_revision, draft)
            .unwrap();
        assert_ne!(first, latest);
        assert!(runtime.hardware_previews.lock().unwrap().expiry_worker);
        assert_eq!(runtime.hardware_previews.lock().unwrap().leases.len(), 1);
        runtime.cancel_hardware_preview(Some(&first));
        assert_eq!(runtime.hardware_preview_token(id), Some(latest.clone()));
        assert_eq!(
            runtime.snapshot().configuration_revision,
            before.configuration_revision
        );
        assert_eq!(
            serde_json::to_value(runtime.snapshot().profiles).unwrap(),
            serde_json::to_value(before.profiles).unwrap()
        );
        runtime.cancel_hardware_preview(Some(&latest));
        assert!(runtime.hardware_preview(id).is_none());
        runtime.shutdown();
    }

    #[test]
    // Verify that visual preview allows incomplete actions but rejects invalid appearance
    fn visual_preview_allows_incomplete_actions_but_rejects_invalid_appearance() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:incomplete";
        runtime.simulate_device(true, id).unwrap();
        runtime.state.lock().unwrap().devices[0].simulated = false;
        let before = runtime.snapshot();
        let mut binding = before.profiles[0].pages[0].bindings["key-1"].clone();
        for action in [
            Action::OpenUrl { url: String::new() },
            Action::Hotkey { keys: vec![] },
        ] {
            binding.action = action;
            assert!(binding.validate().is_err());
            assert!(runtime
                .preview_binding_details(id, "key-1", &binding, None)
                .is_ok());
            assert!(runtime
                .preview_on_dock(
                    id,
                    before.configuration_revision,
                    HardwareDraft {
                        bindings: BTreeMap::from([("key-1".into(), binding.clone())]),
                        ..Default::default()
                    }
                )
                .is_ok());
        }
        binding.color = "invalid".into();
        assert!(runtime
            .preview_binding_details(id, "key-1", &binding, None)
            .is_err());
        assert!(runtime
            .preview_on_dock(
                id,
                before.configuration_revision,
                HardwareDraft {
                    bindings: BTreeMap::from([("key-1".into(), binding)]),
                    ..Default::default()
                }
            )
            .is_err());
        runtime.shutdown();
    }

    #[test]
    // Verify that draft plugin uses its own artwork without demand or saved action pixels
    fn draft_plugin_uses_its_own_artwork_without_demand_or_saved_action_pixels() {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/fixture.preview.sdPlugin");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("runtime.html"),
            "<!doctype html><title>Fixture</title>",
        )
        .unwrap();
        for (icon, color) in [("red", "red"), ("blue", "blue")] {
            std::fs::write(package.join(format!("{icon}.svg")), format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="72" height="72"><rect width="72" height="72" fill="{color}"/></svg>"#)).unwrap();
        }
        std::fs::write(package.join("manifest.json"), json!({
            "UUID":"fixture.preview", "Name":"Preview", "Version":"0.1.0.0", "SDKVersion":2,
            "Author":"Manager test suite", "Description":"Disposable visual fixture", "Icon":"red",
            "OS":[{"Platform":"mac","MinimumVersion":"12"},{"Platform":"windows","MinimumVersion":"10"}],
            "Software":{"MinimumVersion":"6.9"}, "CodePath":"runtime.html",
            "Actions":[
                {"UUID":"fixture.preview.red", "Name":"Red", "Icon":"red", "Controllers":["Keypad"], "States":[{"Image":"red"}]},
                {"UUID":"fixture.preview.blue", "Name":"Blue", "Icon":"blue", "Controllers":["Keypad"], "States":[{"Image":"blue"}]}
            ]
        }).to_string()).unwrap();
        crate::test_plugin::modernize(&package);
        // Current host metadata uses explicit themed icon paths. Keep the
        // embedded browser descriptor identical to the manifest capabilities
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
        for color in ["red", "blue"] {
            manifest["StreamDockManager"]["Capabilities"]["actions"]
                [format!("fixture.preview.{color}")]["icon"] =
                json!({"light":format!("{color}.svg"),"dark":format!("{color}.svg")});
            manifest["StreamDockManager"]["Capabilities"]["actions"]
                [format!("fixture.preview.{color}")]["states"] =
                json!([{ "light":format!("{color}.svg"),"dark":format!("{color}.svg") }]);
        }
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&manifest["StreamDockManager"]["Capabilities"]).unwrap());
        std::fs::write(
            package.join("runtime.html"),
            format!(
                "<!doctype html><title>Fixture</title><!-- SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1 -->",
                encoded.len()
            ),
        )
        .unwrap();
        std::fs::write(package.join("manifest.json"), manifest.to_string()).unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:plugin-draft";
        runtime.simulate_device(true, id).unwrap();
        runtime.state.lock().unwrap().devices[0].simulated = false;
        let before = runtime.snapshot();
        let mut binding = before.profiles[0].pages[0].bindings["key-1"].clone();
        binding.action = Action::Plugin {
            plugin_id: "fixture.preview".into(),
            action_id: "fixture.preview.blue".into(),
            settings: json!({}),
            resources: Default::default(),
            user_desired_state: None,
        };
        let session = before.session(id);
        let mut saved = binding.clone();
        if let Action::Plugin { action_id, .. } = &mut saved.action {
            *action_id = "fixture.preview.red".into();
        }
        {
            let mut state = runtime.state.lock().unwrap();
            let page = state
                .profiles
                .iter_mut()
                .find(|profile| profile.id == session.profile_id)
                .unwrap()
                .pages
                .iter_mut()
                .find(|page| page.id == session.page_id)
                .unwrap();
            page.bindings.insert("key-1".into(), saved);
        }
        let red_image = runtime
            .plugins
            .lock()
            .unwrap()
            .action_icon("fixture.preview", "fixture.preview.red")
            .unwrap()["image"]
            .clone();
        let details = runtime
            .preview_binding_details(id, "key-1", &binding, None)
            .unwrap();
        assert!(
            details.placeholder,
            "installed inactive action must have declared artwork"
        );
        let token = runtime
            .preview_on_dock(
                id,
                before.configuration_revision,
                HardwareDraft {
                    bindings: BTreeMap::from([("key-1".into(), binding)]),
                    ..Default::default()
                },
            )
            .unwrap();
        let scene = runtime.render_scene(id).unwrap();
        assert!(!scene.visual_contexts.contains_key("key-1"));
        let visual = scene
            .visuals
            .get("key-1")
            .map(AsRef::as_ref)
            .or_else(|| scene.fallbacks.get("key-1"))
            .unwrap();
        assert!(visual["image"].is_string());
        assert_ne!(
            visual["image"], red_image,
            "draft must not borrow the saved action icon"
        );
        assert!(runtime
            .plugins
            .lock()
            .unwrap()
            .browser_view("fixture.preview")
            .unwrap()
            .is_none());
        assert_eq!(
            runtime.snapshot().configuration_revision,
            before.configuration_revision
        );
        runtime.cancel_hardware_preview(Some(&token));
        assert!(runtime.render_scene(id).unwrap().draft.is_none());
        runtime.shutdown();
    }

    #[test]
    // Verify that editing a dial never steals its independent saved touch surface
    fn feedback_draft_preserves_independent_saved_surface() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:feedback-draft";
        runtime.simulate_device(true, id).unwrap();
        let snapshot = runtime.snapshot();
        let manifest = &snapshot
            .devices
            .iter()
            .find(|device| device.id == id)
            .unwrap()
            .manifest;
        let encoder = manifest
            .controls
            .iter()
            .find(|control| control.feedback_surface.is_some())
            .unwrap();
        let surface = encoder.feedback_surface.as_deref().unwrap();
        let mut scene = runtime.render_scene(id).unwrap();
        let mut saved = snapshot.profiles[0].pages[0].bindings["key-1"].clone();
        saved.action = Action::None;
        saved.title = "Saved LCD".into();
        scene
            .page
            .as_mut()
            .unwrap()
            .bindings
            .insert(surface.into(), saved.clone());
        assert_eq!(scene.surface_source(manifest, surface), surface);
        let mut draft = saved.clone();
        draft.title = "Draft dial".into();
        scene.draft = Some(HardwareDraft {
            bindings: BTreeMap::from([(encoder.id.clone(), draft)]),
            ..Default::default()
        });
        assert_eq!(scene.surface_source(manifest, surface), surface);
        assert!(!scene.draft_owns_surface(manifest, surface));
        scene.page.as_mut().unwrap().bindings.remove(surface);
        assert_eq!(scene.surface_source(manifest, surface), surface);
        // A genuine inherited display still follows its encoder draft.
        let mut inherited = manifest.clone();
        inherited.capabilities.touch_panel = None;
        assert_eq!(scene.surface_source(&inherited, surface), encoder.id);
        assert!(scene.draft_owns_surface(&inherited, surface));
        scene
            .page
            .as_mut()
            .unwrap()
            .bindings
            .insert(surface.into(), saved.clone());
        scene.draft = None;
        assert_eq!(scene.surface_source(manifest, surface), surface);
        assert!(!scene.draft_owns_surface(manifest, surface));
        assert_eq!(
            scene.page.as_ref().unwrap().bindings[surface].title,
            "Saved LCD"
        );
        runtime.shutdown();
    }

    #[test]
    // Verify that lease is visual only expires and restores latest saved scene
    fn lease_is_visual_only_expires_and_restores_latest_saved_scene() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let id = "simulator:n4-pro:preview";
        runtime.simulate_device(true, id).unwrap();
        let original = runtime.snapshot();
        let session = original.session(id);
        let mut binding: Binding=serde_json::from_value(serde_json::json!({"title":"Saved","color":"#000000","action":{"kind":"none"},"media":crate::media::MediaOptions::default()})).unwrap();
        runtime
            .apply_edits(
                original.configuration_revision,
                "Saved binding",
                vec![crate::workspace::Edit::Bindings {
                    profile_id: session.profile_id.clone(),
                    page_id: session.page_id.clone(),
                    bindings: BTreeMap::from([("key-1".into(), Some(binding.clone()))]),
                }],
            )
            .unwrap();
        let saved = runtime.snapshot();
        let revision = saved.configuration_revision;
        binding.title = "Draft only".into();
        binding.action = Action::Mute;
        let draft = HardwareDraft {
            display_test: None,
            bindings: BTreeMap::from([("key-1".into(), binding.clone())]),
            background: None,
            scope: None,
            preview_states: Default::default(),
            palette: None,
        };
        assert!(runtime
            .preview_on_dock(id, revision, draft.clone())
            .unwrap_err()
            .contains("physical"));
        runtime.state.lock().unwrap().devices[0].simulated = false; // transport-free physical descriptor fixture
        let token = runtime
            .preview_on_dock(id, revision, draft.clone())
            .unwrap();
        let scene = runtime.render_scene(id).unwrap();
        assert_eq!(scene.draft.as_ref().unwrap().bindings.len(), 1);
        assert_eq!(runtime.snapshot().configuration_revision, revision);
        assert_eq!(
            runtime
                .snapshot()
                .profiles
                .iter()
                .find(|p| p.id == session.profile_id)
                .unwrap()
                .pages[0]
                .bindings["key-1"]
                .title,
            "Saved"
        );
        let profile = saved
            .profiles
            .iter()
            .find(|p| p.id == session.profile_id)
            .unwrap();
        let control = saved.devices[0].manifest.control("key-1").unwrap();
        let render = |scene: &crate::render_scene::RenderSceneContext| {
            runtime
                .render_bound_surface_in_scene(
                    control,
                    profile.pages[0].bindings.get("key-1"),
                    0,
                    &scene.scope,
                    "key-1",
                    scene,
                )
                .unwrap()
        };
        let preview_pixels = render(&scene);
        let next = runtime.preview_on_dock(id, revision, draft).unwrap();
        runtime.cancel_hardware_preview(Some(&token));
        assert_eq!(runtime.hardware_preview_token(id), Some(next.clone()));
        assert!(!scene.current(&runtime));
        let mut edited = profile.clone();
        edited.pages[0].bindings.get_mut("key-1").unwrap().title = "Latest saved".into();
        runtime.save_profile(edited, revision).unwrap();
        // Deterministic deadline test; production expiration uses the identical Instant check
        runtime
            .hardware_previews
            .lock()
            .unwrap()
            .leases
            .get_mut(id)
            .unwrap()
            .until = Instant::now() - Duration::from_millis(1);
        assert!(runtime.hardware_preview(id).is_none());
        let restored = runtime.render_scene(id).unwrap();
        assert_eq!(
            restored.page.as_ref().unwrap().bindings["key-1"].title,
            "Latest saved"
        );
        assert!(restored.draft.is_none());
        assert_ne!(render(&restored), preview_pixels);
        runtime.cancel_hardware_preview(Some(&next));
        let token = runtime
            .preview_on_dock(
                id,
                runtime.snapshot().configuration_revision,
                HardwareDraft {
                    display_test: None,
                    bindings: BTreeMap::from([("key-1".into(), binding)]),
                    background: None,
                    scope: None,
                    preview_states: Default::default(),
                    palette: None,
                },
            )
            .unwrap();
        runtime
            .select_device_profile(id, &session.profile_id, Some(&session.page_id), false)
            .unwrap();
        assert!(runtime.hardware_preview_token(id).is_none());
        runtime.cancel_hardware_preview(Some(&token));
        runtime.shutdown();
    }
}
