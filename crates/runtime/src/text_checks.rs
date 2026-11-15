//! Advisory only: use native glyph layout and the same source-frame clock as the preview
use crate::{Binding, DeviceInfo, Profile, Runtime};
use hardware_core::Control;
use std::time::Duration;
impl Runtime {
    #[allow(clippy::too_many_arguments)]
    // Render binding text-check context and collect warnings for the selected display surface
    pub(crate) fn binding_text_checks(
        &self,
        device: &DeviceInfo,
        profile: Option<&Profile>,
        control: &Control,
        binding: &Binding,
        feedback: &str,
        visual: Option<&serde_json::Value>,
        elapsed: Duration,
    ) -> Result<Vec<String>, String> {
        let width = control.image_width.ok_or("No display surface")?;
        let height = control.image_height.ok_or("No display surface")?;
        let mut scene = self.render_scene_for_target(&device.id, true)?;
        scene.media_elapsed = elapsed;
        let animated = binding
            .asset
            .as_ref()
            .and_then(|asset| self.profiles.clip(asset).ok().flatten())
            .is_some_and(|clip| clip.frames.len() > 1)
            || scene
                .background_media(profile)
                .and_then(|(asset, _)| self.profiles.clip(asset).ok().flatten())
                .is_some_and(|clip| clip.frames.len() > 1);
        // Hardware alpha reveals the wallpaper behind this descriptor rectangle
        let backdrop = if device.manifest.capabilities.background {
            let size = [device.manifest.layout.width, device.manifest.layout.height];
            let bytes = self.render_scene_wallpaper(&scene, profile, size, 0, &["png".into()])?;
            let wallpaper = image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())?
                .to_rgba8();
            let crop = image::imageops::crop_imm(
                &wallpaper,
                control.x.into(),
                control.y.into(),
                control.width.into(),
                control.height.into(),
            )
            .to_image();
            image::imageops::resize(
                &crop,
                width.into(),
                height.into(),
                image::imageops::FilterType::Triangle,
            )
        } else {
            image::RgbaImage::from_pixel(width.into(), height.into(), image::Rgba([0, 0, 0, 255]))
        };
        let background = |draft: &Binding| -> Result<image::RgbaImage, String> {
            let bytes =
                self.render_binding_at(control, Some(draft), 0, feedback, visual, elapsed, true)?;
            let pixels = image::load_from_memory(&bytes)
                .map_err(|e| e.to_string())?
                .to_rgba8();
            hardware_core::render::over_image_rgba(backdrop.clone(), &pixels)
                .map_err(|e| e.to_string())
        };
        if let Some(composition) = &binding.composition {
            let sources =
                self.display_sources(control, binding, feedback, visual, elapsed, false)?;
            let mut warnings = Vec::new();
            for (index, layer) in composition
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| l.style.visible && l.style.kind == "text")
            {
                let value = if layer.source.kind == "text" {
                    layer.text.clone()
                } else {
                    sources
                        .iter()
                        .find(|s| s.available && s.source == layer.source)
                        .and_then(|s| s.value.as_str())
                        .unwrap_or("")
                        .to_owned()
                };
                if value.is_empty() {
                    continue;
                }
                let style = &layer.style;
                let x = (style.rect[0] * f64::from(width) / 100.) as u32;
                let y = (style.rect[1] * f64::from(height) / 100.) as u32;
                let w = ((style.rect[2] * f64::from(width) / 100.) as u32)
                    .min(u32::from(width).saturating_sub(x));
                let h = ((style.rect[3] * f64::from(height) / 100.) as u32)
                    .min(u32::from(height).saturating_sub(y));
                if w == 0 || h == 0 {
                    warnings.push(format!(
                        "{}: the label is outside this surface.",
                        layer.label
                    ));
                    continue;
                }
                let mut draft = binding.clone();
                // Retain the original layer's empty placeholder so an explicit media
                // layer later in the stack does not become an implicit background
                let stack = &mut draft.composition.as_mut().unwrap().layers;
                for later in &mut stack[index..] {
                    later.style.visible = false;
                }
                let pixels = background(&draft)?;
                let under = image::imageops::crop_imm(&pixels, x, y, w, h).to_image();
                warnings.extend(
                    hardware_core::plugin_display::text_checks(
                        &under, &value, style, height, animated,
                    )
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|warning| format!("{}: {warning}", layer.label)),
                );
            }
            return Ok(warnings);
        }
        if binding.title.is_empty() {
            return Ok(vec![]);
        }
        let mut draft = binding.clone();
        draft.title.clear();
        let under = background(&draft)?;
        if binding.appearance.is_some()
            || binding.asset.is_some() && visual.is_none()
            || !feedback.is_empty()
        {
            return hardware_core::appearance::text_warnings(
                &under,
                &binding.title,
                binding.appearance.as_deref().unwrap_or(&Default::default()),
                feedback,
                animated,
            )
            .map_err(|e| e.to_string());
        }
        if visual.is_some() {
            return Ok(vec![]);
        } // This path renders only the plugin's display
        let mask =
            hardware_core::render::tile_rgba(width, height, [0, 0, 0, 0], &binding.title, 0, "png")
                .map_err(|e| e.to_string())?;
        let mask = image::load_from_memory(&mask)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        let scale = if width >= 112 { 2 } else { 1 };
        let columns = (u32::from(width).saturating_sub(12) / (8 * scale)).max(1) as usize;
        let count = binding.title.chars().count();
        let mut warnings = Vec::new();
        if count > columns * 3
            || count.div_ceil(columns).min(3) as u32 * 10 * scale > u32::from(height)
        {
            warnings.push("Some title glyphs are clipped at the edge of this surface.".into());
        }
        if binding.title.contains(['\n', '\r']) {
            warnings.push("Line breaks are not supported by this title renderer.".into());
        }
        if hardware_core::appearance::low_contrast(&under, mask.pixels().map(|p| p[3]), "#ebf0fa") {
            warnings.push(hardware_core::appearance::contrast_warning(animated));
        }
        Ok(warnings)
    }
}
