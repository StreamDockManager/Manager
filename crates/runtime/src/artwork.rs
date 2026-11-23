//! One image transform for a contiguous rectangular set of key surfaces
use crate::{Binding, Runtime, Snapshot};
use hardware_core::{Control, ModelManifest};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtworkSlice {
    pub device_id: String,
    pub profile_id: String,
    pub page_id: String,
    pub controls: Vec<String>,
    pub asset: String,
    pub media: crate::media::MediaOptions,
    /// Source image rectangle in descriptor layout coordinates, including gaps
    pub image_rect: [f64; 4],
}

// Validate a distinct contiguous selection of image-capable controls
pub fn selection<'a>(
    manifest: &'a ModelManifest,
    ids: &[String],
) -> Result<Vec<&'a Control>, String> {
    let unique: BTreeSet<_> = ids.iter().collect();
    if ids.len() < 2 || ids.len() > 100 || unique.len() != ids.len() {
        return Err("Select at least two distinct image keys in a row or rectangle".into());
    }
    let controls: Vec<_> = ids
        .iter()
        .map(|id| manifest.control(id).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let first = controls[0];
    if controls.iter().any(|c| {
        c.kind != "button"
            || c.preview_hidden
            || c.image_slot.is_none()
            || c.image_width.is_none()
            || c.image_height.is_none()
            || c.width != first.width
            || c.height != first.height
            || c.image_width != first.image_width
            || c.image_height != first.image_height
    }) {
        return Err("Select compatible image-capable keys".into());
    }
    let xs: BTreeSet<_> = controls.iter().map(|c| c.x).collect();
    let ys: BTreeSet<_> = controls.iter().map(|c| c.y).collect();
    if xs.len() * ys.len() != controls.len()
        || xs.iter().any(|x| {
            ys.iter()
                .any(|y| !controls.iter().any(|c| c.x == *x && c.y == *y))
        })
    {
        return Err("Select a contiguous row or rectangular block".into());
    }
    let left = *xs.first().unwrap();
    let right = *xs.last().unwrap();
    let top = *ys.first().unwrap();
    let bottom = *ys.last().unwrap();
    if manifest.controls.iter().any(|c| {
        c.kind == "button"
            && c.x >= left
            && c.x <= right
            && c.y >= top
            && c.y <= bottom
            && !unique.contains(&c.id)
    }) {
        return Err("The selection has a gap".into());
    }
    Ok(controls)
}
// Map each selected control to its crop within the source artwork
pub fn crops(
    manifest: &ModelManifest,
    request: &ArtworkSlice,
) -> Result<BTreeMap<String, [f64; 4]>, String> {
    let controls = selection(manifest, &request.controls)?;
    request.media.validate()?;
    let [x, y, width, height] = request.image_rect;
    if !request
        .image_rect
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 1_000_000.0)
        || width <= 0.0
        || height <= 0.0
    {
        return Err("Invalid artwork transform".into());
    }
    controls
        .into_iter()
        .map(|control| {
            let crop = [
                (f64::from(control.x) - x) / width,
                (f64::from(control.y) - y) / height,
                f64::from(control.width) / width,
                f64::from(control.height) / height,
            ];
            if crop[0] < -1e-9
                || crop[1] < -1e-9
                || crop[0] + crop[2] > 1.0 + 1e-9
                || crop[1] + crop[3] > 1.0 + 1e-9
            {
                return Err("Artwork must cover every selected key".into());
            }
            let mut crop = crop;
            crop[0] = crop[0].max(0.0);
            crop[1] = crop[1].max(0.0);
            crop[2] = crop[2].min(1.0 - crop[0]);
            crop[3] = crop[3].min(1.0 - crop[1]);
            Ok((control.id.clone(), crop))
        })
        .collect()
}
impl Runtime {
    // Prepare artwork bindings only for the reviewed workspace revision
    pub fn artwork_bindings(
        &self,
        request: &ArtworkSlice,
        expected: u64,
    ) -> Result<BTreeMap<String, Option<Binding>>, String> {
        let state = self.snapshot();
        if state.configuration_revision != expected {
            return Err("Configuration changed. Reopen Slice artwork.".into());
        }
        let device = state
            .devices
            .iter()
            .find(|d| d.id == request.device_id && d.connected)
            .ok_or("Device disconnected")?;
        let session = state.session(&request.device_id);
        if session.profile_id != request.profile_id || session.page_id != request.page_id {
            return Err("Device selection changed. Reopen Slice artwork.".into());
        }
        let profile = state
            .profiles
            .iter()
            .find(|p| p.id == request.profile_id)
            .ok_or("Profile missing")?;
        let page = profile
            .pages
            .iter()
            .find(|p| p.id == request.page_id)
            .ok_or("Page missing")?;
        let extension = self.profiles.asset_extension(&request.asset)?;
        let clip = self.profiles.clip(&request.asset)?;
        let format = clip
            .as_ref()
            .map(|c| c.source_format.as_str())
            .unwrap_or(extension);
        if !["png", "jpeg", "jpg", "gif", "mp4"].contains(&format)
            || !device
                .manifest
                .capabilities
                .media_formats
                .iter()
                .any(|f| f == format)
        {
            return Err("Unsupported artwork format".into());
        }
        if clip.as_ref().is_some_and(|clip| {
            request.media.trim_start_ms >= clip.duration_ms
                || request
                    .media
                    .trim_end_ms
                    .is_some_and(|end| end > clip.duration_ms)
        }) {
            return Err("Trim boundaries exceed the clip duration".into());
        }
        let crops = crops(&device.manifest, request)?;
        crops
            .into_iter()
            .map(|(id, crop)| {
                let mut binding = page.bindings.get(&id).cloned().unwrap_or_else(|| Binding {
                    live_label: Vec::new(),
                    dial_response: None,
                    activation_policy: Default::default(),
                    title: String::new(),
                    color: "#00000000".into(),
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
                });
                binding.asset = Some(request.asset.clone());
                binding.media = request.media.clone();
                binding.media.crop = crop;
                binding.media.fit = "fill".into();
                binding.validate()?;
                Ok((id, Some(binding)))
            })
            .collect()
    }
    // Apply sliced artwork bindings as one revision-checked workspace edit
    pub fn slice_artwork(&self, request: ArtworkSlice, expected: u64) -> Result<Snapshot, String> {
        let bindings = self.artwork_bindings(&request, expected)?;
        self.apply_workspace_edits_in_session(
            expected,
            "Slice artwork",
            vec![crate::workspace::Edit::Bindings {
                profile_id: request.profile_id.clone(),
                page_id: request.page_id.clone(),
                bindings,
            }],
            Some((&request.device_id, &request.profile_id, &request.page_id)),
        )?;
        Ok(self.snapshot())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that slices include gaps and reject holes or uncovered keys
    fn slices_include_gaps_and_reject_holes_or_uncovered_keys() {
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "n4-pro")
            .unwrap();
        let keys: Vec<_> = manifest
            .controls
            .iter()
            .filter(|c| c.kind == "button")
            .collect();
        let mut request = ArtworkSlice {
            device_id: String::new(),
            profile_id: String::new(),
            page_id: String::new(),
            controls: vec![keys[0].id.clone(), keys[1].id.clone()],
            asset: String::new(),
            media: Default::default(),
            image_rect: [
                0.,
                0.,
                f64::from(manifest.layout.width),
                f64::from(manifest.layout.height),
            ],
        };
        let crop = crops(&manifest, &request).unwrap();
        assert!(crop[&keys[0].id][0] + crop[&keys[0].id][2] < crop[&keys[1].id][0]);
        request.controls[1] = keys[2].id.clone();
        assert!(crops(&manifest, &request).is_err());
        request.controls[1] = keys[1].id.clone();
        request.image_rect[2] = 1.;
        assert!(crops(&manifest, &request).is_err());
    }
}
