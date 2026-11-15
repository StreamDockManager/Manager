//! Per-device immutable inputs for a bounded render cycle
//!
//! Lock order: runtime state -> builtin layers (read only). Both are released
//! before plugin host/hub capture. Plugin pixels and the short feedback leaf
//! snapshot share a hub-lock point; feedback producers never acquire the hub
//! No state or feedback lock is retained during image I/O/render/output. Revisions are rechecked
//! after capture and before enqueue, so racing changes discard CPU work
use crate::{builtins::EffectiveBinding, feedback::RenderFeedback, Page, Runtime};
use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

pub(crate) struct RenderSceneContext {
    pub draft: Option<crate::hardware_preview::HardwareDraft>,
    pub preview_token: Option<String>,
    pub(crate) clips: crate::profiles::ClipCycle,
    pub device: String,
    pub scope: String,
    pub keyboard_navigation: bool,
    pub wallpaper: String,
    pub theme_wallpaper: Option<(String, crate::media::MediaOptions)>,
    pub device_theme: stream_dock_manager_plugin_host::DeviceThemePreferences,
    pub device_surface: String,
    pub device_ink: String,
    pub plugin_surfaces: HashMap<String, String>,
    pub page: Option<Page>,
    pub media_elapsed: Duration,
    pub formats: Vec<String>,
    pub contract_identity: String,
    pub overrides: BTreeMap<String, EffectiveBinding>,
    pub visuals: HashMap<String, std::sync::Arc<serde_json::Value>>,
    pub fallbacks: HashMap<String, serde_json::Value>,
    pub visual_contexts: HashMap<String, String>,
    pub feedback: RenderFeedback,
    state_revision: u64,
    configuration_revision: u64,
    layer_revision: u64,
    plugin_generation: u64,
    font_generation: u64,
}
impl RenderSceneContext {
    /// Temporary visual bindings own their output surface while the lease lives
    /// Saved touch/display precedence is restored as soon as the draft expires
    pub(crate) fn surface_source<'a>(
        &self,
        manifest: &'a hardware_core::ModelManifest,
        surface: &'a str,
    ) -> &'a str {
        if let Some(draft) = &self.draft {
            if draft.bindings.contains_key(surface) {
                return surface;
            }
            if let Some(source) = manifest.controls.iter().find(|source| {
                source.feedback_surface.as_deref() == Some(surface)
                    && draft.bindings.contains_key(&source.id)
            }) {
                return &source.id;
            }
        }
        self.page
            .as_ref()
            .map(|page| page.surface_source(manifest, surface))
            .unwrap_or(surface)
    }
    // Report the draft owns surface condition for render scene
    pub(crate) fn draft_owns_surface(
        &self,
        manifest: &hardware_core::ModelManifest,
        surface: &str,
    ) -> bool {
        let source = self.surface_source(manifest, surface);
        self.draft
            .as_ref()
            .is_some_and(|draft| draft.bindings.contains_key(source))
    }

    // Resolve draft or saved background media with theme wallpaper as the fallback
    pub(crate) fn background_media<'a>(
        &'a self,
        profile: Option<&'a crate::Profile>,
    ) -> Option<(&'a str, &'a crate::media::MediaOptions)> {
        if let Some(background) = self
            .draft
            .as_ref()
            .and_then(|draft| draft.background.as_ref())
        {
            return background
                .asset
                .as_deref()
                .map(|asset| (asset, &background.media))
                .or_else(|| {
                    self.theme_wallpaper
                        .as_ref()
                        .map(|(asset, media)| (asset.as_str(), media))
                });
        }
        profile
            .and_then(|profile| {
                Some((
                    profile.background_asset.as_deref()?,
                    &profile.background_media,
                ))
            })
            .or_else(|| {
                self.theme_wallpaper
                    .as_ref()
                    .map(|(id, options)| (id.as_str(), options))
            })
    }

    // Check for matching snapshot
    pub(crate) fn matches_snapshot(&self, snapshot: &crate::Snapshot) -> bool {
        let session = snapshot.session(&self.device);
        self.contract_identity
            == snapshot
                .devices
                .iter()
                .find(|d| d.id == self.device)
                .and_then(|d| crate::device_contract::resolve(d).ok())
                .map(|c| c.identity())
                .unwrap_or_default()
            && self.state_revision == snapshot.revision
            && self.configuration_revision == snapshot.configuration_revision
            && self.scope == format!("{}:{}", session.profile_id, session.page_id)
    }
    // Check permission for transient
    pub(crate) fn permits_transient(&self, source: &str, context: &str, generation: u64) -> bool {
        self.plugin_generation == generation
            && self
                .visual_contexts
                .get(source)
                .is_some_and(|value| value == context)
    }
    // Return the current render scene value
    pub(crate) fn current(&self, runtime: &Runtime) -> bool {
        let valid_state = {
            let state = runtime.state.lock().unwrap();
            let session = state.session(&self.device);
            self.contract_identity
                == state
                    .devices
                    .iter()
                    .find(|d| d.id == self.device)
                    .and_then(|d| crate::device_contract::resolve(d).ok())
                    .map(|c| c.identity())
                    .unwrap_or_default()
                && state.revision == self.state_revision
                && state.configuration_revision == self.configuration_revision
                && format!("{}:{}", session.profile_id, session.page_id) == self.scope
        };
        let layer_revision = runtime.builtin_state.layer_revision();
        let plugin_generation = runtime.plugins.lock().unwrap().render_generation();
        valid_state
            && runtime.hardware_preview_token(&self.device) == self.preview_token
            && layer_revision == self.layer_revision
            && plugin_generation == self.plugin_generation
            && hardware_core::fonts::generation() == self.font_generation
            && self.feedback.connected(runtime, &self.device)
    }
}
impl Runtime {
    // Render theme wallpaper
    pub(crate) fn render_theme_wallpaper(
        &self,
        [width, height]: [u16; 2],
        rotation: u16,
        wallpaper: &str,
    ) -> Result<crate::media::SharedBytes, String> {
        self.cached_render(
            format!("wallpaper:{width}:{height}:{rotation}:{wallpaper}"),
            || {
                hardware_core::render::render(wallpaper.as_bytes(), width, height, rotation, "png")
                    .map(crate::media::shared_bytes)
                    .map_err(|error| error.to_string())
            },
        )
    }

    // Render scene wallpaper
    pub(crate) fn render_scene_wallpaper(
        &self,
        scene: &RenderSceneContext,
        profile: Option<&crate::Profile>,
        size: [u16; 2],
        rotation: u16,
        formats: &[String],
    ) -> Result<crate::media::SharedBytes, String> {
        if let Some((asset, options)) = scene.background_media(profile) {
            if let Ok(data) = self.render_media_frame_shared(
                asset,
                scene.media_elapsed,
                size,
                rotation,
                options,
                formats,
            ) {
                return Ok(data);
            }
        }
        self.render_theme_wallpaper(size, rotation, &scene.wallpaper)
    }

    // Render scene
    pub(crate) fn render_scene(&self, device: &str) -> Result<RenderSceneContext, String> {
        self.render_scene_for_target(device, false)
    }
    /// Software previews consume the vendor software target; device output always
    /// uses `render_scene`, whose target is the physical display
    pub(crate) fn render_scene_for_target(
        &self,
        device: &str,
        software: bool,
    ) -> Result<RenderSceneContext, String> {
        let started = std::time::Instant::now();
        for _ in 0..2 {
            let (
                state_revision,
                configuration_revision,
                scope,
                keyboard_navigation,
                page,
                formats,
                contract_identity,
                layer_revision,
                overrides,
            ) = {
                let state = self.state.lock().unwrap();
                let session = state.session(device);
                let profile = state.profiles.iter().find(|p| p.id == session.profile_id);
                let page = profile
                    .and_then(|p| p.pages.iter().find(|p| p.id == session.page_id))
                    .cloned();
                let formats = state
                    .devices
                    .iter()
                    .find(|d| d.id == device)
                    .map(|d| d.manifest.capabilities.image_formats.clone())
                    .unwrap_or_else(|| vec!["png".into()]);
                let contract_identity = state
                    .devices
                    .iter()
                    .find(|d| d.id == device)
                    .and_then(|d| crate::device_contract::resolve(d).ok())
                    .map(|c| c.identity())
                    .unwrap_or_default();
                let (layer_revision, overrides) =
                    self.builtin_state.device_overrides(&state, device);
                (
                    state.revision,
                    state.configuration_revision,
                    format!("{}:{}", session.profile_id, session.page_id),
                    profile.is_some_and(|p| crate::keyboard_navigation::supported(&p.model_id))
                        && session.page_id == crate::keyboard_navigation::PAGE_ID,
                    page,
                    formats,
                    contract_identity,
                    layer_revision,
                    overrides,
                )
            };
            let preview = self.hardware_preview(device);
            let draft = preview.as_ref().map(|value| &value.1);
            let mut requests: BTreeMap<String, String> = page
                .as_ref()
                .map(|p| {
                    p.bindings
                        .keys()
                        .map(|k| (k.clone(), scope.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for (source, value) in &overrides {
                requests.insert(source.clone(), value.scope.clone());
            }
            for source in draft.into_iter().flat_map(|draft| draft.bindings.keys()) {
                requests.insert(source.clone(), scope.clone());
            }
            let requests: Vec<_> = requests.into_iter().collect();
            let fallback_requests: Vec<_> = requests
                .iter()
                .filter_map(|(source, _)| {
                    let binding = draft
                        .and_then(|draft| draft.bindings.get(source))
                        .or_else(|| overrides.get(source).map(|v| &v.binding))
                        .or_else(|| page.as_ref().and_then(|p| p.bindings.get(source)))?;
                    if binding.disabled {
                        return None;
                    }
                    if let crate::actions::Action::Plugin {
                        plugin_id,
                        action_id,
                        ..
                    } = &binding.action
                    {
                        Some((source.clone(), plugin_id.clone(), action_id.clone()))
                    } else {
                        None
                    }
                })
                .collect();
            // Load a saved system font before capturing the generation. First
            // playback after restart must not invalidate its own initial frame
            for binding in page
                .iter()
                .flat_map(|p| p.bindings.values())
                .chain(overrides.values().map(|v| &v.binding))
                .chain(draft.into_iter().flat_map(|draft| draft.bindings.values()))
            {
                let mut fonts = binding
                    .appearance
                    .iter()
                    .map(|a| a.font.as_str())
                    .chain(
                        binding
                            .plugin_appearance
                            .iter()
                            .flat_map(|a| &a.elements)
                            .map(|e| e.font.as_str()),
                    )
                    .chain(
                        binding
                            .composition
                            .iter()
                            .flat_map(|c| &c.layers)
                            .map(|l| l.style.font.as_str()),
                    );
                if let Some(font) = fonts.find(|font| font.starts_with("system:")) {
                    hardware_core::fonts::database_for(font);
                    break;
                }
            }
            let font_generation = hardware_core::fonts::generation();
            let metadata = RenderFeedback::metadata(self, device);
            let live_requests: Vec<_> = requests
                .iter()
                .filter(|(source, _)| {
                    draft.is_none_or(|draft| !draft.bindings.contains_key(source))
                })
                .cloned()
                .collect();
            let (mut plugin_snapshot, feedback) =
                self.plugins.lock().unwrap().render_surfaces_for_target(
                    device,
                    &live_requests,
                    &fallback_requests,
                    software,
                    || RenderFeedback::capture(self, device, metadata),
                );
            // A visual draft must never borrow the saved action's plugin pixels
            // or register a new executable plugin context. Reuse only an exact
            // existing action/settings match; otherwise show its declared artwork
            for (source, binding) in draft.into_iter().flat_map(|draft| &draft.bindings) {
                plugin_snapshot.values.remove(source);
                plugin_snapshot.contexts.remove(source);
                if binding.disabled {
                    continue;
                }
                let resolved = if matches!(binding.action, crate::actions::Action::Plugin { .. }) {
                    self.resolve_parameters(&binding.action, device, &scope)?
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
                    let wide = {
                        let state = self.state.lock().unwrap();
                        state
                            .devices
                            .iter()
                            .find(|entry| entry.id == device)
                            .and_then(|entry| {
                                let source = entry.manifest.control(source).ok()?;
                                let control = entry
                                    .manifest
                                    .control(
                                        source.feedback_surface.as_deref().unwrap_or(&source.id),
                                    )
                                    .ok()?;
                                Some(
                                    u32::from(control.image_width.unwrap_or(112)) * 10
                                        > u32::from(control.image_height.unwrap_or(112)) * 13,
                                )
                            })
                            .unwrap_or(false)
                    };
                    let host = self.plugins.lock().unwrap();
                    let visual = host
                        .matching_surface_for_target(
                            device, source, &scope, plugin_id, action_id, settings, software,
                        )
                        .or_else(|| host.declared_surface(device, plugin_id, action_id, wide));
                    if let Some(mut visual) = visual {
                        if let Some(state) =
                            draft.and_then(|draft| draft.preview_states.get(source))
                        {
                            if visual["states"].as_array().is_some_and(|states| {
                                states
                                    .iter()
                                    .any(|entry| entry["index"].as_u64() == Some(u64::from(*state)))
                            }) {
                                visual["state"] = serde_json::json!(state);
                            }
                        }
                        plugin_snapshot
                            .values
                            .insert(source.clone(), std::sync::Arc::new(visual));
                    }
                }
            }
            let media_elapsed = self.media_elapsed();
            let animation_errors = self.vendor_animations.lock().unwrap().prepare(
                &self.profiles,
                (device, software),
                &plugin_snapshot.contexts,
                &mut plugin_snapshot.values,
                media_elapsed,
                &|| {
                    self.state.lock().unwrap().revision != state_revision
                        || self.plugins.lock().unwrap().render_generation()
                            != plugin_snapshot.generation
                },
            );
            for (control, error) in animation_errors {
                self.emit(
                    "media-error",
                    Some(device.into()),
                    None,
                    Some(format!("{control}: {error}")),
                );
            }
            let context = RenderSceneContext {
                preview_token: preview.as_ref().map(|value| value.0.clone()),
                draft: preview.map(|value| value.1),
                clips: Default::default(),
                device: device.into(),
                wallpaper: plugin_snapshot.device_palette["wallpaper"]
                    .as_str()
                    .expect("theme wallpaper")
                    .into(),
                theme_wallpaper: plugin_snapshot.wallpaper.and_then(|wallpaper| {
                    let options = serde_json::from_value(wallpaper.options).ok()?;
                    self.profiles.asset_extension(&wallpaper.asset_id).ok()?;
                    Some((wallpaper.asset_id, options))
                }),
                device_theme: plugin_snapshot.device_theme,
                plugin_surfaces: plugin_snapshot.plugin_surfaces,
                device_ink: plugin_snapshot.device_palette["ink"]
                    .as_str()
                    .expect("theme ink")
                    .into(),
                device_surface: plugin_snapshot.device_palette["surface"]
                    .as_str()
                    .expect("theme surface")
                    .into(),
                scope,
                keyboard_navigation,
                page,
                formats,
                contract_identity,
                overrides,
                visuals: plugin_snapshot.values,
                fallbacks: plugin_snapshot
                    .fallbacks
                    .into_iter()
                    .filter_map(|(source, fallback)| {
                        fallback.resolve().map(|visual| (source, visual))
                    })
                    .collect(),
                visual_contexts: plugin_snapshot.contexts,
                feedback,
                media_elapsed,
                state_revision,
                configuration_revision,
                layer_revision,
                plugin_generation: plugin_snapshot.generation,
                font_generation,
            };
            // Plugin visuals (including themed fallback icons) were copied under
            // one hub lock. Soft revisions need no retry: a newer immutable visual
            // or feedback sample is rendered next cycle, avoiding producer starvation
            if context.current(self) && context.feedback.metadata_current(self, device) {
                self.metrics.record("scene_context_builds", 1.0);
                self.metrics.record(
                    "scene_context_bindings",
                    context.page.as_ref().map_or(0, |p| p.bindings.len()) as f64
                        + context.overrides.len() as f64,
                );
                self.metrics
                    .record("scene_context_ms", started.elapsed().as_secs_f64() * 1000.0);
                return Ok(context);
            }
        }
        self.metrics.record("obsolete_render_contexts", 1.0);
        Err("Render scene changed during capture".into())
    }
}

#[cfg(test)]
// Install burst fixture
pub(crate) fn install_burst_fixture(runtime: &Runtime) {
    let mut state = runtime.state.lock().unwrap();
    let mut profile = state.profiles[0].clone();
    profile.id = "fixture-profile".into();
    profile.model_id = "n4-pro".into();
    profile.active_page = "fixture-page".into();
    let bindings = (1..=10)
        .map(|index| {
            (
                format!("key-{index}"),
                crate::Binding {
                    dial_response: None,
                    activation_policy: Default::default(),
                    title: format!("Fixture key-{index}"),
                    color: "#336699".into(),
                    custom_background: false,
                    background_source: None,
                    action: crate::actions::Action::None,
                    asset: None,
                    media: Default::default(),
                    appearance: None,
                    plugin_appearance: None,
                    composition: None,
                    feedback: None,
                    disabled: false,
                },
            )
        })
        .collect();
    profile.pages = vec![crate::Page {
        id: "fixture-page".into(),
        name: "Fixture".into(),
        bindings,
    }];
    state.active_profile = profile.id.clone();
    state.profiles = vec![profile];
    state.sessions.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    // Create a disposable runtime for render-scene tests
    fn runtime() -> (tempfile::TempDir, std::sync::Arc<Runtime>) {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        (root, runtime)
    }
    #[test]
    // Verify that cached render shares misses hits and preview allocations
    fn cached_render_shares_misses_hits_and_preview_allocations() {
        let (_root, runtime) = runtime();
        let visual = serde_json::json!({"background": "#ff1200"});
        let first = runtime.render_visual(&visual, 32, 32, 0).unwrap();
        let hit = runtime.render_visual(&visual, 32, 32, 0).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &hit));
        let preview = runtime.preview_image(hit, 0).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &preview));
        assert_eq!(
            runtime.profiles.render_cache.lock().unwrap().copied_bytes,
            0
        );
        let owned = runtime.owned_render_bytes(preview);
        assert_eq!(owned.as_slice(), &**first);
        assert_eq!(
            runtime.profiles.render_cache.lock().unwrap().copied_bytes,
            owned.len() as u64
        );
        runtime.profiles.render_cache.lock().unwrap().clear();
        assert!(
            runtime
                .profiles
                .render_cache
                .lock()
                .unwrap()
                .in_flight_bytes()
                >= first.len()
        );
        assert_eq!(&**first, owned.as_slice());
        drop(first);
        assert_eq!(
            runtime
                .profiles
                .render_cache
                .lock()
                .unwrap()
                .resident_bytes(),
            0
        );
        runtime.shutdown();
    }
    #[test]
    // Verify that context rejects configuration layers feedback and reconnect changes
    fn context_rejects_configuration_layers_feedback_and_reconnect_changes() {
        let (_root, runtime) = runtime();
        let context = runtime.render_scene("fixture").unwrap();
        assert!(context.current(&runtime));
        runtime.state.lock().unwrap().configuration_revision += 1;
        assert!(!context.current(&runtime));
        let context = runtime.render_scene("fixture").unwrap();
        runtime.builtin_state.cleanup(Some("fixture"));
        assert!(!context.current(&runtime));
        let context = runtime.render_scene("fixture").unwrap();
        runtime.feedback_state.lock().unwrap().revision += 1;
        assert!(
            context.current(&runtime),
            "Soft feedback updates do not discard a coherent captured frame"
        );
        assert!(!context.feedback.current(&runtime, "fixture"));
        let context = runtime.render_scene("fixture").unwrap();
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert("fixture".into(), 42);
        assert!(!context.current(&runtime));
        runtime.shutdown();
    }
    #[test]
    // Verify that sustained soft feedback updates do not starve capture or submission
    fn sustained_soft_feedback_updates_do_not_starve_capture_or_submission() {
        let (_root, runtime) = runtime();
        let producer = runtime.clone();
        let worker = std::thread::spawn(move || {
            for i in 0..10_000 {
                let mut feedback = producer.feedback_state.lock().unwrap();
                feedback.revision += 1;
                feedback.audio.volume = Some((i % 101) as u8);
                drop(feedback);
                std::thread::yield_now();
            }
        });
        for _ in 0..100 {
            let context = runtime.render_scene("fixture").unwrap();
            assert!(context.current(&runtime));
        }
        worker.join().unwrap();
        runtime.shutdown();
    }
    #[test]
    // Verify that stale transients cannot cross context or generation
    fn stale_transients_cannot_cross_context_or_generation() {
        let (_root, runtime) = runtime();
        let mut context = runtime.render_scene("fixture").unwrap();
        context
            .visual_contexts
            .insert("key-1".into(), "new-context".into());
        assert!(context.permits_transient("key-1", "new-context", context.plugin_generation));
        assert!(!context.permits_transient("key-1", "old-context", context.plugin_generation));
        assert!(!context.permits_transient(
            "key-1",
            "new-context",
            context.plugin_generation.wrapping_add(1)
        ));
        assert!(!context.permits_transient("key-2", "new-context", context.plugin_generation));
        runtime.shutdown();
    }
    #[test]
    // Verify that context copies active page only and renders identical stable pixels
    fn context_copies_active_page_only_and_renders_identical_stable_pixels() {
        let (_root, runtime) = runtime();
        install_burst_fixture(&runtime);
        {
            let mut state = runtime.state.lock().unwrap();
            let base = state.profiles[0].clone();
            for i in 1..1000 {
                let mut profile = base.clone();
                profile.id = format!("unused-{i}");
                state.profiles.push(profile);
            }
        }
        let context = runtime.render_scene("fixture").unwrap();
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "n4-pro")
            .unwrap();
        let page = context.page.as_ref().unwrap();
        for control in manifest
            .controls
            .iter()
            .filter(|c| c.image_slot.is_some())
            .take(10)
        {
            let binding = page.bindings.get(&control.id);
            let expected = runtime
                .render_binding(control, binding, manifest.layout.rotation, "", None)
                .unwrap();
            let actual = runtime
                .render_bound_surface_in_scene(
                    control,
                    binding,
                    manifest.layout.rotation,
                    &context.scope,
                    &control.id,
                    &context,
                )
                .unwrap();
            assert_eq!(
                image::load_from_memory(&expected).unwrap().to_rgba8(),
                image::load_from_memory(&actual).unwrap().to_rgba8()
            );
        }
        let count = runtime
            .metrics
            .snapshot()
            .into_iter()
            .find(|m| m.name == "scene_context_builds")
            .unwrap();
        assert_eq!(count.sum, 1.0);
        assert!(context.current(&runtime));
        runtime.shutdown();
    }
    #[test]
    // Verify that navigation render defaults are centered themed and customizable
    fn navigation_render_defaults_are_centered_themed_and_customizable() {
        let (_root, runtime) = runtime();
        install_burst_fixture(&runtime);
        let mut scene = runtime.render_scene("fixture").unwrap();
        scene.keyboard_navigation = true;
        scene.scope = "fixture-profile:__keyboard_navigation".into();
        scene.device_surface = "#112233".into();
        scene.device_ink = "#ffffff".into();
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "k1-pro")
            .unwrap();
        let control = manifest.control("key-1").unwrap();
        let page = crate::keyboard_navigation::page();
        let render = |binding: &crate::Binding, scene: &RenderSceneContext| {
            let bytes = runtime
                .render_bound_surface_in_scene(
                    control,
                    Some(binding),
                    0,
                    &scene.scope,
                    "key-1",
                    scene,
                )
                .unwrap();
            image::load_from_memory(&bytes).unwrap().to_rgba8()
        };
        let default = render(&page.bindings["key-1"], &scene);
        let background = crate::navigation_background(
            default.width() as u16,
            default.height() as u16,
            [17, 34, 51, 255],
            "#ffffff",
        );
        assert_eq!(default.get_pixel(0, 0), background.get_pixel(0, 0));
        assert_eq!(background.get_pixel(0, 0).0, [17, 34, 51, 255]);
        assert_eq!(background.get_pixel(4, 4).0, [17, 34, 51, 255]);
        assert_ne!(
            background.get_pixel(default.width() / 2, 4).0,
            [17, 34, 51, 255]
        );
        // All label pixels are in the middle third, not the former bottom label area
        let text_rows: Vec<_> = (0..default.height())
            .filter(|&y| {
                (0..default.width()).any(|x| default.get_pixel(x, y) != background.get_pixel(x, y))
            })
            .collect();
        assert!(!text_rows.is_empty());
        assert!(*text_rows.first().unwrap() > default.height() / 3);
        assert!(*text_rows.last().unwrap() < default.height() * 2 / 3);
        let mut composed = page.bindings["key-1"].clone();
        composed.composition = Some(Box::new(Default::default()));
        assert_eq!(render(&composed, &scene), background);
        let mut custom = page.bindings["key-1"].clone();
        custom.background_source = Some(crate::profiles::BackgroundSource::Custom);
        custom.color = "#123456".into();
        custom.custom_background = true;
        custom.appearance = Some(Box::new(hardware_core::appearance::Appearance {
            position: "bottom".into(),
            foreground: "#ff0000".into(),
            ..Default::default()
        }));
        let customized = render(&custom, &scene);
        assert_eq!(customized.get_pixel(0, 0).0, [18, 52, 86, 255]);
        scene.device_surface = "#eeddbb".into();
        scene.device_ink = "#000000".into();
        assert_ne!(default, render(&page.bindings["key-1"], &scene));
        assert_eq!(customized, render(&custom, &scene));
        if let Ok(path) = std::env::var("NAVIGATION_VISUAL_OUTPUT") {
            let mut grid = image::RgbaImage::new(default.width() * 3, default.height() * 4);
            for (palette, (surface, ink)) in [("#eeddbb", "#000000"), ("#112233", "#ffffff")]
                .into_iter()
                .enumerate()
            {
                scene.device_surface = surface.into();
                scene.device_ink = ink.into();
                for (index, key) in (1..=6).enumerate() {
                    let tile = render(&page.bindings[&format!("key-{key}")], &scene);
                    image::imageops::replace(
                        &mut grid,
                        &tile,
                        (index % 3) as i64 * i64::from(default.width()),
                        (palette * 2 + index / 3) as i64 * i64::from(default.height()),
                    );
                }
            }
            grid.save(path).unwrap();
        }
        runtime.shutdown();
    }
    #[test]
    // Verify that hardware and software visuals do not share the wrong rendered frame
    fn hardware_and_software_visuals_do_not_share_the_wrong_rendered_frame() {
        let (_root, runtime) = runtime();
        install_burst_fixture(&runtime);
        let mut hardware = runtime.render_scene("fixture").unwrap();
        let mut software = runtime.render_scene_for_target("fixture", true).unwrap();
        hardware.visuals.insert(
            "key-1".into(),
            std::sync::Arc::new(serde_json::json!({"title": "Hardware", "background": "#ff0000"})),
        );
        software.visuals.insert(
            "key-1".into(),
            std::sync::Arc::new(serde_json::json!({"title": "Software", "background": "#0000ff"})),
        );
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "n4-pro")
            .unwrap();
        let control = manifest.control("key-1").unwrap();
        let render = |scene: &RenderSceneContext| {
            runtime
                .render_bound_surface_in_scene(
                    control,
                    scene.page.as_ref().unwrap().bindings.get("key-1"),
                    0,
                    &scene.scope,
                    "key-1",
                    scene,
                )
                .unwrap()
        };
        let first_hardware = render(&hardware);
        let first_software = render(&software);
        assert_ne!(first_hardware, first_software);
        assert_eq!(first_hardware, render(&hardware));
        assert_eq!(first_software, render(&software));
        runtime.shutdown();
    }
    #[test]
    // Verify that different preview scope cannot borrow active plugin pixels
    fn different_preview_scope_cannot_borrow_active_plugin_pixels() {
        let (_root, runtime) = runtime();
        install_burst_fixture(&runtime);
        let mut context = runtime.render_scene("fixture").unwrap();
        context.visuals.insert(
            "key-1".into(),
            std::sync::Arc::new(
                serde_json::json!({"title":"Wrong active plugin", "background":"#ff0000"}),
            ),
        );
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "n4-pro")
            .unwrap();
        let control = manifest.control("key-1").unwrap();
        let binding = context.page.as_ref().unwrap().bindings.get("key-1");
        let expected = runtime
            .render_binding(control, binding, 0, "", None)
            .unwrap();
        let actual = runtime
            .render_bound_surface_in_scene(control, binding, 0, "other:page", "key-1", &context)
            .unwrap();
        assert_eq!(
            image::load_from_memory(&expected).unwrap().to_rgba8(),
            image::load_from_memory(&actual).unwrap().to_rgba8()
        );
        runtime.shutdown();
    }
    #[test]
    #[ignore = "release-only deterministic source performance fixture; no HID or plugin actions"]
    // Verify that scene snapshot candidate
    fn scene_snapshot_candidate() {
        let (_root, runtime) = runtime();
        {
            let mut state = runtime.state.lock().unwrap();
            let base = state.profiles[0].clone();
            for index in 1..1000 {
                let mut profile = base.clone();
                profile.id = format!("fixture-{index}");
                state.profiles.push(profile);
            }
        }
        for repetition in 0..6 {
            let started = std::time::Instant::now();
            for _cycle in 0..100 {
                let context = runtime.render_scene("fixture-device").unwrap();
                for _surface in 0..10 {
                    std::hint::black_box(&context);
                }
            }
            if repetition > 0 {
                println!(
                    "FOLLOWUP_SCENE {{\"variant\":\"candidate\",\"repetition\":{repetition},\"cycles\":100,\"surfaces\":10,\"profiles\":1000,\"context_calls\":100,\"elapsed_ms\":{}}}",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        runtime.shutdown();
    }
    #[test]
    #[ignore = "release-only five-field host visual burst fixture; no plugin process or HID"]
    // Verify that plugin render burst candidate
    fn plugin_render_burst_candidate() {
        let (_root, runtime) = runtime();
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "n4-pro")
            .unwrap();
        install_burst_fixture(&runtime);
        for repetition in 0..6 {
            let started = std::time::Instant::now();
            let mut renders = 0;
            let mut generation_valid = true;
            for cycle in 0..10 {
                let context = runtime.render_scene("fixture-device").unwrap();
                let page = context.page.as_ref().unwrap();
                for control in manifest
                    .controls
                    .iter()
                    .filter(|c| c.image_slot.is_some())
                    .take(10)
                {
                    let mut owned = page.bindings.get(&control.id).unwrap().clone();
                    owned.title = format!("Fixture {cycle} {}", control.id);
                    owned.color = "#336699".into();
                    let binding = Some(&owned);
                    std::hint::black_box(
                        runtime
                            .render_bound_surface_in_scene(
                                control,
                                binding,
                                manifest.layout.rotation,
                                &context.scope,
                                &control.id,
                                &context,
                            )
                            .unwrap(),
                    );
                    generation_valid &= std::hint::black_box(context.current(&runtime));
                    renders += 1;
                }
            }
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            assert!(generation_valid, "Fixture generation changed during render");
            if repetition > 0 {
                println!(
                    "FOLLOWUP_BURST {{\"variant\":\"candidate\",\"repetition\":{repetition},\"cycles\":10,\"surfaces\":10,\"visual_effects\":500,\"render_calls\":{renders},\"elapsed_ms\":{}}}",
                    elapsed_ms
                );
            }
        }
        runtime.shutdown();
    }
}
