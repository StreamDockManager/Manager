//! Connection-global negotiated plugin theme preview with one bounded visual lease
use crate::{hardware_preview::PaletteDraft, Runtime};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

struct Lease {
    token: String,
    until: Instant,
    visual_token: String,
    palette: PaletteDraft,
    _assets: crate::asset_leases::AssetLease,
}
#[derive(Default)]
pub(crate) struct Previews {
    lease: Option<Lease>,
    worker: bool,
}
impl Runtime {
    // Serialize temporary plugin theme ownership without changing saved appearance or device scope
    pub fn preview_plugin_palette(
        self: &Arc<Self>,
        palette: PaletteDraft,
    ) -> Result<String, String> {
        let (_, wallpaper) = self.resolve_preview_palette(&palette)?;
        if let Some(wallpaper) = &wallpaper {
            self.profiles.asset_extension(&wallpaper.asset_id)?;
            let media: crate::media::MediaOptions =
                serde_json::from_value(wallpaper.options.clone())
                    .map_err(|_| "Invalid palette wallpaper media options")?;
            media.validate()?;
        }
        let assets = self.profiles.asset_leases.pin(
            wallpaper.iter().map(|wallpaper| wallpaper.asset_id.clone()),
            "Temporary plugin palette preview",
        )?;
        let custom = palette
            .custom
            .clone()
            .map(|value| {
                serde_json::from_value::<stream_dock_manager_plugin_host::CustomPalette>(value)
                    .map_err(|_| "Invalid custom palette draft".to_string())
            })
            .transpose()?;
        let token = uuid::Uuid::new_v4().to_string();
        // This leaf lease lock always precedes the plugin host lock and is never taken by the host
        let mut previews = self.plugin_palette_preview.lock().unwrap();
        if self.is_stopping() {
            return Err("Manager is stopping".into());
        }
        self.plugins.lock().unwrap().set_palette_theme_preview(
            &token,
            &palette.mode,
            &palette.id,
            custom.as_ref(),
        )?;
        let changed = previews
            .lease
            .as_ref()
            .is_none_or(|lease| lease.until <= Instant::now() || lease.palette != palette);
        let visual_token = if changed {
            token.clone()
        } else {
            previews.lease.as_ref().unwrap().visual_token.clone()
        };
        previews.lease = Some(Lease {
            token: token.clone(),
            visual_token,
            palette,
            until: Instant::now() + Duration::from_secs(5),
            _assets: assets,
        });
        let start = !previews.worker;
        previews.worker = true;
        drop(previews);
        if changed {
            self.palette_device_scenes_changed();
        }
        if start {
            let weak = Arc::downgrade(self);
            std::thread::spawn(move || loop {
                let Some(runtime) = weak.upgrade() else {
                    return;
                };
                let next = runtime.expire_plugin_palette_preview(Instant::now());
                drop(runtime);
                let Some(wait) = next else { return };
                std::thread::sleep(wait);
            });
        }
        Ok(token)
    }

    // Expose one immutable global candidate without changing any device profile or page
    pub(crate) fn device_palette_preview(&self) -> Option<(String, PaletteDraft)> {
        self.plugin_palette_preview
            .lock()
            .unwrap()
            .lease
            .as_ref()
            .filter(|lease| lease.until > Instant::now())
            .map(|lease| (lease.visual_token.clone(), lease.palette.clone()))
    }

    // Repaint connected awake outputs and software streams without waking a sleeping dock
    fn palette_device_scenes_changed(&self) {
        self.preview_subscriptions
            .scene_changed("display-theme", None);
        let _ = self.device_commands.send(crate::devices::Request::Wake);
    }

    // Retire only the matching preview so stale asynchronous cancellation cannot clear a newer owner
    pub fn cancel_plugin_palette_preview(&self, token: Option<&str>) {
        let mut previews = self.plugin_palette_preview.lock().unwrap();
        let mut changed = false;
        if previews
            .lease
            .as_ref()
            .is_some_and(|lease| token.is_none_or(|token| lease.token == token))
        {
            let lease = previews.lease.take().unwrap();
            changed = true;
            self.plugins
                .lock()
                .unwrap()
                .clear_palette_theme_preview(&lease.token);
        }
        drop(previews);
        if changed {
            self.palette_device_scenes_changed();
        }
    }

    // Restore the latest saved plugin theme at expiry without retaining the runtime while waiting
    fn expire_plugin_palette_preview(&self, now: Instant) -> Option<Duration> {
        let mut previews = self.plugin_palette_preview.lock().unwrap();
        let mut changed = false;
        if previews
            .lease
            .as_ref()
            .is_some_and(|lease| lease.until <= now)
        {
            let lease = previews.lease.take().unwrap();
            changed = true;
            self.plugins
                .lock()
                .unwrap()
                .clear_palette_theme_preview(&lease.token);
        }
        let wait = previews
            .lease
            .as_ref()
            .map(|lease| lease.until.saturating_duration_since(now));
        if wait.is_none() {
            previews.worker = false;
        }
        drop(previews);
        if changed {
            self.palette_device_scenes_changed();
        }
        wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Build a global palette candidate without a connected or simulated dock
    fn candidate(mode: &str) -> PaletteDraft {
        PaletteDraft {
            mode: mode.into(),
            id: "default".into(),
            custom: None,
        }
    }

    #[test]
    fn global_palette_preview_recolors_real_declared_plugin_layout_and_restores() {
        use serde_json::json;
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("plugins/fixture.palette.sdPlugin");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("runtime.html"), "<title>Fixture</title>").unwrap();
        crate::test_plugin::write_manifest(
            &package,
            json!({"UUID":"fixture.palette","Name":"Palette fixture","Version":"0.1.0","CodePath":"runtime.html","Actions":[{"UUID":"fixture.palette.action","Name":"Action"}]}),
        );
        let path = package.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest["StreamDockManager"]["Capabilities"]["actions"]["fixture.palette.action"]
            ["feedback_layout"] = json!("layout.json");
        std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&manifest["StreamDockManager"]["Capabilities"]).unwrap());
        std::fs::write(package.join("runtime.html"), format!("<title>Fixture</title>\n<!-- CURRENT_TEST_DESCRIPTOR SDMCAP1:{:08x}:{encoded}:ENDSDMCAP1 -->", encoded.len())).unwrap();
        std::fs::write(package.join("layout.json"), json!({"id":"fixture", "streamDockManager":{"displayPalette":true},"background":"#000000","items":[{"key":"value","type":"text","rect":[0,0,72,72],"color":"#ffffff"}]}).to_string()).unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime
            .plugins
            .lock()
            .unwrap()
            .set_ui_theme_style(Default::default(), Some("light"))
            .unwrap();
        let surface = || {
            runtime
                .plugins
                .lock()
                .unwrap()
                .declared_surface("", "fixture.palette", "fixture.palette.action", false)
                .expect("declared fixture layout")
        };
        let saved = surface();
        let mode = "light";
        let palette = PaletteDraft {
            mode: mode.into(),
            id: "sage".into(),
            custom: None,
        };
        let expected = runtime.resolve_preview_palette(&palette).unwrap().0;
        let token = runtime.preview_plugin_palette(palette).unwrap();
        let candidate = surface();
        assert_eq!(candidate["layout"]["background"], expected["surface"]);
        assert_eq!(candidate["layout"]["items"][0]["color"], expected["ink"]);
        assert_ne!(candidate["layout"], saved["layout"]);
        runtime.cancel_plugin_palette_preview(Some(&token));
        assert_eq!(surface()["layout"], saved["layout"]);
        runtime.shutdown();
    }

    #[test]
    // Global Settings preview works without hardware and stale cancellations preserve current ownership
    fn global_palette_lease_needs_no_device_and_never_persists() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let before = runtime.snapshot();
        assert!(before.devices.is_empty());
        let first = runtime.preview_plugin_palette(candidate("light")).unwrap();
        let next = runtime.preview_plugin_palette(candidate("dark")).unwrap();
        runtime.cancel_plugin_palette_preview(Some(&first));
        assert_eq!(
            runtime
                .plugin_palette_preview
                .lock()
                .unwrap()
                .lease
                .as_ref()
                .unwrap()
                .token,
            next
        );
        assert!(runtime
            .preview_plugin_palette(candidate("invalid"))
            .is_err());
        assert_eq!(
            runtime
                .plugin_palette_preview
                .lock()
                .unwrap()
                .lease
                .as_ref()
                .unwrap()
                .token,
            next
        );
        assert_eq!(
            runtime.snapshot().configuration_revision,
            before.configuration_revision
        );
        assert_eq!(
            serde_json::to_value(runtime.snapshot().profiles).unwrap(),
            serde_json::to_value(before.profiles).unwrap()
        );
        runtime.cancel_plugin_palette_preview(Some(&next));
        assert!(runtime
            .plugin_palette_preview
            .lock()
            .unwrap()
            .lease
            .is_none());
    }

    #[test]
    // Heartbeats replace the deadline and editor close or expiry retires the current owner
    fn renewed_palette_lease_expires_and_close_cancels() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        runtime.preview_plugin_palette(candidate("dark")).unwrap();
        assert!(runtime
            .expire_plugin_palette_preview(Instant::now())
            .is_some());
        runtime.preview_plugin_palette(candidate("dark")).unwrap();
        assert!(runtime
            .expire_plugin_palette_preview(Instant::now() + Duration::from_secs(6))
            .is_none());
        assert!(runtime
            .plugin_palette_preview
            .lock()
            .unwrap()
            .lease
            .is_none());
        runtime.preview_plugin_palette(candidate("light")).unwrap();
        runtime.cancel_plugin_palette_preview(None);
        assert!(runtime
            .plugin_palette_preview
            .lock()
            .unwrap()
            .lease
            .is_none());
    }
}
