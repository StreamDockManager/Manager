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
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HardwareDraft {
    pub bindings: BTreeMap<String, Binding>,
    pub background: Option<BackgroundDraft>,
    #[serde(default)]
    #[ts(optional)]
    pub scope: Option<String>,
    #[serde(default)]
    #[ts(optional, as = "Option<BTreeMap<String, u32>>")]
    pub preview_states: BTreeMap<String, u32>,
}
pub(crate) struct Lease {
    pub token: String,
    pub epoch: u64,
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
    // Preview on dock using the current hardware preview state
    pub fn preview_on_dock(
        self: &Arc<Self>,
        device_id: &str,
        expected: u64,
        draft: HardwareDraft,
    ) -> Result<String, String> {
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
        if device.simulated {
            return Err("Preview on dock requires a physical device".into());
        }
        if self
            .power
            .effective(device_id, epoch, &state.session(device_id).output)
            .sleeping
        {
            return Err("Wake the dock before previewing".into());
        }
        if draft.bindings.len() > 100 || draft.bindings.is_empty() && draft.background.is_none() {
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
            self.validate_binding(binding, source)?;
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
                .chain(draft.background.iter().filter_map(|b| b.asset.clone())),
            "Temporary hardware preview",
        )?;
        let token = uuid::Uuid::new_v4().to_string();
        let mut previews = self.hardware_previews.lock().unwrap();
        previews.leases.insert(
            device_id.into(),
            Lease {
                token: token.clone(),
                epoch,
                scope,
                until: Instant::now() + Duration::from_secs(5),
                draft,
                _assets: assets,
            },
        );
        let start_expiry_worker = !previews.expiry_worker;
        previews.expiry_worker = true;
        drop(previews);
        drop(state);
        drop(epochs);
        let _ = self.device_commands.send(crate::devices::Request::Wake);
        if start_expiry_worker {
            let weak = Arc::downgrade(self);
            // One expiry worker owns all leases. Live edits refresh deadlines,
            // rather than creating a sleeping thread for every keystroke
            std::thread::spawn(move || loop {
                let Some(runtime) = weak.upgrade() else {
                    return;
                };
                let mut previews = runtime.hardware_previews.lock().unwrap();
                let now = Instant::now();
                let before = previews.leases.len();
                previews.leases.retain(|_, lease| lease.until > now);
                if before != previews.leases.len() {
                    let _ = runtime.device_commands.send(crate::devices::Request::Wake);
                }
                let wait = previews
                    .leases
                    .values()
                    .map(|lease| lease.until - now)
                    .min();
                if wait.is_none() {
                    previews.expiry_worker = false;
                }
                drop(previews);
                drop(runtime);
                let Some(wait) = wait else { return };
                std::thread::sleep(wait);
            });
        }
        Ok(token)
    }
    // Cancel hardware preview
    pub fn cancel_hardware_preview(&self, token: Option<&str>) {
        self.hardware_previews
            .lock()
            .unwrap()
            .leases
            .retain(|_, lease| token.is_some_and(|token| token != lease.token));
        let _ = self.device_commands.send(crate::devices::Request::Wake);
    }
    // Invalidate hardware preview
    pub(crate) fn invalidate_hardware_preview(&self, device: Option<&str>) {
        self.hardware_previews
            .lock()
            .unwrap()
            .leases
            .retain(|id, _| device.is_some_and(|device| device != id));
        let _ = self.device_commands.send(crate::devices::Request::Wake);
    }
    // Read a hardware preview only while its connection, page scope and lease remain current
    pub(crate) fn hardware_preview(&self, device: &str) -> Option<(String, HardwareDraft)> {
        let epoch = self.connection_generation(device)?;
        let state = self.state.lock().unwrap();
        let session = state.session(device);
        let scope = format!("{}:{}", session.profile_id, session.page_id);
        let previews = self.hardware_previews.lock().unwrap();
        let lease = previews.leases.get(device)?;
        (lease.epoch == epoch && lease.scope == scope && lease.until > Instant::now())
            .then(|| (lease.token.clone(), lease.draft.clone()))
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
    // Verify that feedback draft owns surface then restores explicit saved surface
    fn feedback_draft_owns_surface_then_restores_explicit_saved_surface() {
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
        let mut draft = saved;
        draft.title = "Draft dial".into();
        scene.draft = Some(HardwareDraft {
            bindings: BTreeMap::from([(encoder.id.clone(), draft)]),
            ..Default::default()
        });
        assert_eq!(scene.surface_source(manifest, surface), encoder.id);
        assert!(scene.draft_owns_surface(manifest, surface));
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
            bindings: BTreeMap::from([("key-1".into(), binding.clone())]),
            background: None,
            scope: None,
            preview_states: Default::default(),
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
                    bindings: BTreeMap::from([("key-1".into(), binding)]),
                    background: None,
                    scope: None,
                    preview_states: Default::default(),
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
