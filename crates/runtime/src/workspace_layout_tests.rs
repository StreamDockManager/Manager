//! Disposable validation parity and operation-count fixtures, never opens HID.
use super::*;
use std::{cell::Cell, time::Instant};
thread_local! { static LAYOUTS: Cell<usize> = const { Cell::new(0) }; }
fn counted_layouts() -> Vec<hardware_core::ModelManifest> {
    let layouts = hardware_registry::manifests();
    LAYOUTS.with(|count| count.set(count.get() + layouts.len()));
    layouts
}
// Frozen pre-optimization validator for error-order parity and paired timing.
impl Runtime {
    fn legacy_validate_workspace(&self, data: &ProfileData) -> Result<(), String> {
        let plugins = self.plugins.lock().unwrap().list();
        for action in data.saved_macros.iter().map(|saved| &saved.action).chain(
            data.profiles
                .iter()
                .flat_map(|profile| &profile.pages)
                .flat_map(|page| page.bindings.values())
                .filter(|binding| !binding.disabled)
                .map(|binding| &binding.action),
        ) {
            builtins::validate_key_logic_plugins(
                action,
                |id| {
                    data.saved_macros
                        .iter()
                        .find(|saved| saved.id == id)
                        .map(|saved| &saved.action)
                },
                &plugins,
            )?;
        }
        for profile in &data.profiles {
            let manifests = counted_layouts();
            let manifest = manifests
                .iter()
                .find(|m| m.model_id == profile.model_id)
                .ok_or("Unsupported model")?;
            for binding in profile.pages.iter().flat_map(|p| p.bindings.iter()) {
                if binding.1.disabled {
                    continue;
                }
                let control = manifest.control(binding.0).map_err(|error| {
                    format!(
                        "Unsupported profile control {}/{}: {error}",
                        profile.model_id, binding.0
                    )
                })?;
                self.validate_binding(binding.1, manifest, control)
                    .map_err(|error| {
                        format!(
                            "Invalid profile control {}/{}: {error}",
                            profile.model_id, binding.0
                        )
                    })?;
                if let Some(id) = &binding.1.asset {
                    if let Some(clip) = self.profiles.clip(id)? {
                        if binding.1.media.trim_start_ms >= clip.duration_ms
                            || binding
                                .1
                                .media
                                .trim_end_ms
                                .is_some_and(|end| end > clip.duration_ms)
                        {
                            return Err("Trim boundaries exceed the clip duration".into());
                        }
                    }
                }
            }
            if let Some(id) = &profile.background_asset {
                if let Some(clip) = self.profiles.clip(id)? {
                    if profile.background_media.trim_start_ms >= clip.duration_ms
                        || profile
                            .background_media
                            .trim_end_ms
                            .is_some_and(|end| end > clip.duration_ms)
                    {
                        return Err("Background trim exceeds clip duration".into());
                    }
                }
            }
            for id in profile
                .pages
                .iter()
                .flat_map(|p| p.bindings.values())
                .filter(|b| !b.disabled)
                .filter_map(|b| b.asset.as_ref())
                .chain(profile.background_asset.as_ref())
            {
                let clip = self.profiles.clip(id)?;
                let format = clip
                    .as_ref()
                    .map(|c| c.source_format.as_str())
                    .unwrap_or("png");
                if !manifest
                    .capabilities
                    .media_formats
                    .iter()
                    .any(|f| f == format)
                {
                    return Err("Media format unsupported by this model".into());
                }
            }
        }

        Ok(())
    }
}
fn fixture(count: usize) -> ProfileData {
    let mut data = ProfileData::default();
    let profile = data.profiles[0].clone();
    data.profiles = (0..count)
        .map(|i| {
            let mut p = profile.clone();
            p.id = format!("profile-{i}");
            p
        })
        .collect();
    data
}
fn compare(runtime: &Runtime, data: &ProfileData) -> Result<(), String> {
    let old = runtime.legacy_validate_workspace(data);
    let new = runtime.validate_workspace_with_layouts(data, counted_layouts);
    assert_eq!(old, new);
    new
}
#[test]
fn workspace_layout_parity_counts_and_paired_timing() {
    let root = tempfile::tempdir().unwrap();
    let runtime = Runtime::build(root.path().into(), false).unwrap();
    assert_eq!(hardware_registry::modules().len(), 16);
    for count in [0, 1, 200] {
        let data = fixture(count);
        LAYOUTS.with(|n| n.set(0));
        runtime.legacy_validate_workspace(&data).unwrap();
        assert_eq!(LAYOUTS.with(Cell::get), count * 16);
        LAYOUTS.with(|n| n.set(0));
        runtime
            .validate_workspace_with_layouts(&data, counted_layouts)
            .unwrap();
        assert_eq!(LAYOUTS.with(Cell::get), if count == 0 { 0 } else { 16 });
    }
    let mut invalid = fixture(2);
    invalid.profiles[0].model_id = "missing-first".into();
    invalid.profiles[1].model_id = "missing-second".into();
    assert_eq!(compare(&runtime, &invalid), Err("Unsupported model".into()));
    let mut invalid = fixture(2);
    let binding = invalid.profiles[0].pages[0]
        .bindings
        .values()
        .next()
        .unwrap()
        .clone();
    invalid.profiles[0].pages[0]
        .bindings
        .insert("invalid-first".into(), binding.clone());
    invalid.profiles[1].model_id = "missing-later".into();
    assert!(compare(&runtime, &invalid)
        .unwrap_err()
        .contains("invalid-first"));
    invalid.profiles[0].pages[0]
        .bindings
        .get_mut("invalid-first")
        .unwrap()
        .disabled = true;
    assert_eq!(compare(&runtime, &invalid), Err("Unsupported model".into()));
    let mut invalid = fixture(1);
    let mut bad = binding;
    bad.action = actions::Action::SwipeNavigation {
        target: "page".into(),
        wrap: true,
        reverse: false,
    };
    invalid.profiles[0].pages[0]
        .bindings
        .insert("key-1".into(), bad);
    assert!(compare(&runtime, &invalid)
        .unwrap_err()
        .contains("Touch bar navigation"));
    let mut early = fixture(1);
    early.profiles[0].pages[0]
        .bindings
        .get_mut("key-1")
        .unwrap()
        .action = actions::Action::Macro {
        actions: vec![
            actions::Action::Macro {
                actions: vec![actions::Action::None; 100]
            };
            100
        ],
    };
    LAYOUTS.with(|n| n.set(0));
    assert!(compare(&runtime, &early).unwrap_err().contains("10000"));
    assert_eq!(
        LAYOUTS.with(Cell::get),
        0,
        "early validation must stay lazy"
    );
    let mut all_models = fixture(16);
    for (profile, manifest) in all_models
        .profiles
        .iter_mut()
        .zip(hardware_registry::manifests())
    {
        profile.model_id = manifest.model_id;
        for page in &mut profile.pages {
            page.bindings.clear();
        }
    }
    compare(&runtime, &all_models).unwrap();
    let before = fixture(2);
    let mut duplicate = before.clone();
    duplicate.profiles[1].id = duplicate.profiles[0].id.clone();
    assert_eq!(
        crate::validation::validate_edit(&before, &duplicate)
            .err()
            .as_deref(),
        Some("Duplicate workspace identity")
    );

    let data = fixture(200);
    compare(&runtime, &data).unwrap();
    let mut legacy = Vec::new();
    let mut reused = Vec::new();
    for run in 0..10 {
        for old in if run % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let start = Instant::now();
            if old {
                runtime.legacy_validate_workspace(&data).unwrap();
            } else {
                runtime.validate_workspace(&data).unwrap();
            }
            if old {
                legacy.push(start.elapsed());
            } else {
                reused.push(start.elapsed());
            }
        }
    }
    eprintln!("workspace validation 200 profiles legacy={legacy:?} reused={reused:?}");
    runtime.shutdown();
}
