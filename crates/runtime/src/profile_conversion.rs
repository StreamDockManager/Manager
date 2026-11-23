//! Frozen, reviewed cross-model copies. No assignment or source mutation occurs during staging
use crate::{
    actions::Action,
    builtins::{BuiltinAction, SavedMacro},
    importing::ImportChoice,
    profiles::{Profile, ProfileData},
    workspace::Edit,
    Binding, Runtime, Snapshot,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
const OMIT: &str = "@omit";
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ConversionRow {
    pub source_control: String,
    pub label: String,
    pub binding_count: usize,
    pub target: String,
    pub choices: Vec<ImportChoice>,
    pub problem: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ConversionPreview {
    pub id: String,
    #[ts(type = "number")]
    pub expected_revision: u64,
    pub source_name: String,
    pub target_model: String,
    pub profile_id: String,
    pub name: String,
    pub rows: Vec<ConversionRow>,
    pub background_present: bool,
    pub keep_background: bool,
    pub cloned_macros: Vec<String>,
    pub issues: Vec<String>,
    pub ready: bool,
}
pub(crate) struct StagedConversion {
    data: ProfileData,
    source: Profile,
    target: hardware_core::ModelManifest,
    profile_id: String,
    pages: BTreeMap<String, String>,
    macro_ids: BTreeMap<String, String>,
    mappings: BTreeMap<String, String>,
    name: String,
    keep_background: bool,
    created: std::time::Instant,
    _assets: crate::asset_leases::AssetLease,
}
impl crate::review_store::ReviewSession for StagedConversion {
    const LIMIT: usize = 8;
    const FULL: &'static str = "Close another conversion review first";
    const LIFETIME: Option<std::time::Duration> = Some(std::time::Duration::from_secs(3600));
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
// Collect the saved macros transitively referenced by the converted profile
fn macro_closure(root: &Profile, data: &ProfileData) -> Result<BTreeSet<String>, String> {
    let mut pending = vec![];
    let calls = |action: &Action, pending: &mut Vec<String>| {
        action.visit(&mut |a| {
            if let Action::Builtin { action } = a {
                if let BuiltinAction::SavedMacro { macro_id, .. } = &**action {
                    pending.push(macro_id.clone());
                }
            }
        })
    };
    for binding in root.pages.iter().flat_map(|p| p.bindings.values()) {
        calls(&binding.action, &mut pending);
    }
    let mut found = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !found.insert(id.clone()) {
            continue;
        }
        let Some(saved) = data.saved_macros.iter().find(|m| m.id == id) else {
            continue;
        };
        calls(&saved.action, &mut pending);
    }
    Ok(found)
}
// Remap action
fn remap_action(action: &mut Action, stage: &StagedConversion, issues: &mut Vec<String>) {
    let page = |id: &mut String| {
        if let Some(new) = stage.pages.get(id) {
            *id = new.clone();
        }
    };
    let control = |id: &mut String, issues: &mut Vec<String>| {
        match stage.mappings.get(id) {
            Some(new) if !new.is_empty() && new != OMIT => *id = new.clone(),
            _ => issues.push(format!("An internal action refers to unresolved or omitted control {id}. Assign it or omit the calling binding.")),
        }
    };
    // Navigation fields need their action's scope: explicit external device/profile targets
    // remain external. The shared field visitor is used separately for macro identities
    match action {
        Action::Profile { profile_id } if *profile_id == stage.source.id => {
            *profile_id = stage.profile_id.clone()
        }
        Action::Page { page_id } => page(page_id),
        Action::Builtin { action } => match &mut **action {
            BuiltinAction::MomentaryPage { page_id } => page(page_id),
            BuiltinAction::BindingLayer {
                page_id, controls, ..
            } => {
                if stage.pages.contains_key(page_id) {
                    page(page_id);
                    for id in controls {
                        control(id, issues);
                    }
                }
            }
            BuiltinAction::TriggerBinding {
                device_id,
                profile_id,
                page_id,
                control_id,
            }
            | BuiltinAction::EditBinding {
                device_id,
                profile_id,
                page_id,
                control_id,
            } if device_id.is_empty()
                && (profile_id.is_empty() || *profile_id == stage.source.id) =>
            {
                if !profile_id.is_empty() {
                    *profile_id = stage.profile_id.clone();
                }
                page(page_id);
                control(control_id, issues);
            }
            _ => {}
        },
        _ => {}
    }
    action.for_each_child_mut(|child| remap_action(child, stage, issues));
}
// Collect controls referenced by bindings and relevant nested or saved actions
fn referenced_controls(
    profile: &Profile,
    data: &ProfileData,
    closure: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut ids: BTreeSet<_> = profile
        .pages
        .iter()
        .flat_map(|p| p.bindings.keys().cloned())
        .collect();
    for action in profile
        .pages
        .iter()
        .flat_map(|p| p.bindings.values())
        .map(|b| &b.action)
        .chain(
            data.saved_macros
                .iter()
                .filter(|m| closure.contains(&m.id))
                .map(|m| &m.action),
        )
    {
        action.visit(&mut |a| {
            if let Action::Builtin { action } = a {
                match &**action {
                    BuiltinAction::BindingLayer {
                        page_id, controls, ..
                    } if profile.pages.iter().any(|p| p.id == *page_id) => {
                        ids.extend(controls.iter().cloned())
                    }
                    BuiltinAction::TriggerBinding {
                        device_id,
                        profile_id,
                        control_id,
                        ..
                    }
                    | BuiltinAction::EditBinding {
                        device_id,
                        profile_id,
                        control_id,
                        ..
                    } if device_id.is_empty()
                        && (profile_id.is_empty() || *profile_id == profile.id) =>
                    {
                        ids.insert(control_id.clone());
                    }
                    _ => {}
                }
            }
        });
    }
    ids
}
// Check action
pub(crate) fn check_action(
    action: &Action,
    control: &hardware_core::Control,
    macros: &[SavedMacro],
    plugins: &[stream_dock_manager_plugin_host::PluginInfo],
    depth: usize,
    model: &hardware_core::ModelManifest,
) -> Result<(), String> {
    check_action_inner(
        action,
        control,
        macros,
        plugins,
        depth,
        model,
        &mut HashSet::new(),
        &mut 10_000,
    )
}
#[allow(clippy::too_many_arguments)]
// Check action inner
fn check_action_inner(
    action: &Action,
    control: &hardware_core::Control,
    macros: &[SavedMacro],
    plugins: &[stream_dock_manager_plugin_host::PluginInfo],
    depth: usize,
    model: &hardware_core::ModelManifest,
    expanded: &mut HashSet<String>,
    remaining: &mut usize,
) -> Result<(), String> {
    if *remaining == 0 {
        return Err("Conversion action validation exceeds 10,000 boundaries".into());
    }
    *remaining -= 1;
    if depth > 16 {
        return Err("Saved macro/reference nesting is too deep".into());
    }
    match action {
        Action::Sleep if !model.capabilities.sleep => {
            return Err("Target has no sleep capability".into())
        }
        Action::Brightness { .. } if !model.capabilities.brightness => {
            return Err("Target has no brightness capability".into())
        }
        Action::Lighting { colors, .. }
            if !model.capabilities.lighting
                || colors.len() != model.capabilities.lighting_channels.len() =>
        {
            return Err("Target lighting channels do not match the action".into())
        }
        Action::Plugin {
            plugin_id,
            action_id,
            ..
        } => {
            let plugin = plugins
                .iter()
                .find(|p| p.id == *plugin_id)
                .ok_or("Required plugin is missing")?;
            let a = plugin
                .actions
                .iter()
                .find(|a| a.id == *action_id)
                .ok_or("Required plugin action is missing")?;
            if !a.control_types.contains(&control.kind) {
                return Err("Plugin action does not support this controller type".into());
            }
        }
        Action::SwipeNavigation { .. } if !control.semantics.iter().any(|s| s == "swipe") => {
            return Err("Action requires swipe events".into())
        }
        Action::Builtin { action } => {
            if matches!(&**action, BuiltinAction::BrightnessStep { .. })
                && !model.capabilities.brightness
            {
                return Err("Target has no brightness capability".into());
            }
            if crate::builtins::requires_release(action)
                && !control.semantics.iter().any(|s| s == "release")
            {
                return Err("Action requires release events".into());
            }
            if let BuiltinAction::SavedMacro { macro_id, .. } = &**action {
                let saved = macros
                    .iter()
                    .find(|m| m.id == *macro_id)
                    .ok_or("Required saved macro is missing")?;
                if expanded.insert(macro_id.clone()) {
                    check_action_inner(
                        &saved.action,
                        control,
                        macros,
                        plugins,
                        depth + 1,
                        model,
                        expanded,
                        remaining,
                    )?;
                }
            }
        }
        _ => {}
    }
    action.try_for_each_child(|child| {
        check_action_inner(
            child,
            control,
            macros,
            plugins,
            depth + 1,
            model,
            expanded,
            remaining,
        )
    })?;
    Ok(())
}
impl Runtime {
    // Validate controller semantics, action support and media compatibility for a converted binding
    fn conversion_binding(
        &self,
        stage: &StagedConversion,
        source: &str,
        target: &hardware_core::Control,
        binding: &Binding,
        plugins: &[stream_dock_manager_plugin_host::PluginInfo],
    ) -> Result<(), String> {
        let models = hardware_registry::manifests();
        let source_model = models
            .iter()
            .find(|m| m.model_id == stage.source.model_id)
            .ok_or("Source model is unavailable")?;
        let old = source_model.control(source).map_err(|e| e.to_string())?;
        if old.kind != target.kind {
            return Err("Choose the same controller type to preserve input semantics".into());
        }
        check_action(
            &binding.action,
            target,
            &stage.data.saved_macros,
            plugins,
            0,
            &stage.target,
        )?;
        let surface = stage
            .target
            .control(target.feedback_surface.as_deref().unwrap_or(&target.id))
            .map_err(|e| e.to_string())?;
        if (binding.background_source == Some(crate::profiles::BackgroundSource::Palette)
            || binding.asset.is_some()
            || binding.appearance.is_some()
            || binding.plugin_appearance.is_some()
            || binding.composition.is_some())
            && surface.image_width.is_none()
        {
            return Err("This control has no image surface".into());
        }
        if let Some(asset) = &binding.asset {
            self.conversion_media(stage, asset, &binding.media)?;
        }
        Ok(())
    }
    // Require the target hardware to support the saved media format and output encoding
    fn conversion_media(
        &self,
        stage: &StagedConversion,
        asset: &str,
        media: &crate::media::MediaOptions,
    ) -> Result<(), String> {
        let clip = self.profiles.clip(asset)?;
        let format = clip
            .as_ref()
            .map(|c| c.source_format.as_str())
            .unwrap_or("png");
        if !stage
            .target
            .capabilities
            .media_formats
            .iter()
            .any(|f| f == format)
        {
            return Err(format!("Target does not support {format} media"));
        }
        if media.encoding != "auto"
            && !stage
                .target
                .capabilities
                .image_formats
                .contains(&media.encoding)
        {
            return Err("Target does not support the saved output encoding".into());
        }
        Ok(())
    }
    // Build a revision-checked conversion proposal with remapped controls, copied macros and issues
    fn conversion_candidate(
        &self,
        id: &str,
        stage: &StagedConversion,
    ) -> Result<(ConversionPreview, Profile, Vec<SavedMacro>), String> {
        if crate::review_store::ReviewSession::expired(stage) {
            return Err("Conversion review expired. Start again.".into());
        }
        if self.snapshot().configuration_revision != stage.data.configuration_revision {
            return Err("Workspace changed. Start a new conversion review.".into());
        }
        let plugins = self.plugins.lock().unwrap().list();
        let models = hardware_registry::manifests();
        let source_model = models
            .iter()
            .find(|m| m.model_id == stage.source.model_id)
            .ok_or("Source model is unavailable")?;
        let mut rows = vec![];
        let mut issues = vec![];
        if stage.name.trim().is_empty() || stage.name.trim().len() > 160 {
            issues.push("Use a profile name of 1–160 bytes".into());
        }
        let mut assigned = HashSet::new();
        for (source, selected) in &stage.mappings {
            let bindings: Vec<_> = stage
                .source
                .pages
                .iter()
                .filter_map(|p| p.bindings.get(source))
                .collect();
            let compatible = |control: &hardware_core::Control| -> Result<(), String> {
                if source_model
                    .control(source)
                    .map_err(|e| e.to_string())?
                    .kind
                    != control.kind
                {
                    return Err("Different controller type".into());
                }
                for binding in &bindings {
                    self.conversion_binding(stage, source, control, binding, &plugins)?;
                }
                Ok(())
            };
            let choices: Vec<ImportChoice> = stage
                .target
                .controls
                .iter()
                .filter(|c| compatible(c).is_ok())
                .map(|c| ImportChoice {
                    value: c.id.clone(),
                    label: c.label.clone(),
                })
                .collect();
            let problem = if selected == OMIT {
                None
            } else if selected.is_empty() {
                if choices.is_empty() {
                    let reason = stage
                        .target
                        .controls
                        .iter()
                        .find(|c| {
                            source_model
                                .control(source)
                                .is_ok_and(|old| old.kind == c.kind)
                        })
                        .and_then(|control| compatible(control).err())
                        .unwrap_or_else(|| "Target has no matching controller type".into());
                    Some(format!(
                        "{reason}. Repair the source or explicitly omit this control."
                    ))
                } else {
                    Some("Choose a compatible destination or explicitly omit this control".into())
                }
            } else if !assigned.insert(selected.clone()) {
                Some("Destination is already assigned to another source control".into())
            } else {
                stage
                    .target
                    .control(selected)
                    .map_err(|e| e.to_string())
                    .and_then(compatible)
                    .err()
            };
            if let Some(problem) = &problem {
                issues.push(format!("{source}: {problem}"));
            }
            rows.push(ConversionRow {
                source_control: source.clone(),
                label: source_model
                    .control(source)
                    .map(|c| c.label.clone())
                    .unwrap_or_else(|_| source.clone()),
                binding_count: bindings.len(),
                target: selected.clone(),
                choices,
                problem,
            });
        }
        let mut profile = stage.source.clone();
        profile.id = stage.profile_id.clone();
        profile.name = stage.name.trim().into();
        profile.model_id = stage.target.model_id.clone();
        profile.active_page = stage.pages[&profile.active_page].clone();
        for page in &mut profile.pages {
            page.id = stage.pages[&page.id].clone();
            page.bindings = std::mem::take(&mut page.bindings)
                .into_iter()
                .filter_map(|(source, mut binding)| {
                    let target = stage.mappings.get(&source)?;
                    if target.is_empty() || target == OMIT {
                        return None;
                    }
                    remap_action(&mut binding.action, stage, &mut issues);
                    Some((target.clone(), binding))
                })
                .collect();
        }
        if !stage.keep_background {
            profile.background_asset = None;
        }
        if let Some(asset) = &profile.background_asset {
            if !stage.target.capabilities.background {
                issues.push(
                    "Target has no background display. Explicitly omit the background.".into(),
                );
            }
            if let Err(error) = self.conversion_media(stage, asset, &profile.background_media) {
                issues.push(format!("Background: {error}"));
            }
        }
        let closure = macro_closure(&stage.source, &stage.data)?;
        let mut changed = BTreeSet::new();
        let mut macros: Vec<_> = stage
            .data
            .saved_macros
            .iter()
            .filter(|m| closure.contains(&m.id))
            .cloned()
            .collect();
        // Only macros reachable from retained bindings are needed. Omitted callers must not
        // force repairs to unused shared definitions
        let used = macro_closure(&profile, &stage.data)?;
        macros.retain(|m| used.contains(&m.id));
        for saved in &mut macros {
            let before = serde_json::to_value(&saved.action).unwrap();
            remap_action(&mut saved.action, stage, &mut issues);
            if before != serde_json::to_value(&saved.action).unwrap() {
                changed.insert(saved.id.clone());
            }
        }
        loop {
            let mut added = false;
            for saved in &macros {
                let mut child_changed = false;
                let _: Result<(), ()> = saved.action.try_visit_fields(
                    &mut |role, value| {
                        if role == crate::action_fields::FieldRole::Macro && changed.contains(value)
                        {
                            child_changed = true;
                        }
                        Ok(())
                    },
                    &mut |_, _, _| Ok(()),
                );
                if child_changed && changed.insert(saved.id.clone()) {
                    added = true;
                }
            }
            if !added {
                break;
            }
        }
        let rewrite_calls = |action: &mut Action| {
            let _: Result<(), ()> = action.try_visit_fields_mut(
                &mut |role, value| {
                    if role == crate::action_fields::FieldRole::Macro && changed.contains(value) {
                        *value = stage.macro_ids[value].clone();
                    }
                    Ok(())
                },
                &mut |_, _, _| Ok(()),
            );
        };
        for binding in profile
            .pages
            .iter_mut()
            .flat_map(|p| p.bindings.values_mut())
        {
            rewrite_calls(&mut binding.action);
        }
        macros.retain(|m| changed.contains(&m.id));
        for saved in &mut macros {
            saved.id = stage.macro_ids[&saved.id].clone();
            saved.name = format!(
                "{} (converted)",
                crate::macro_debug::bounded(saved.name.clone(), 140)
            );
            rewrite_calls(&mut saved.action);
        }
        if macros.len() > 99 {
            issues.push(
                "Conversion needs more than 99 copied macros. Split the source before converting."
                    .into(),
            );
        }
        issues.sort();
        issues.dedup();
        if issues.len() > 256 {
            issues.truncate(256);
            issues.push("Further issues omitted; repair these and review again.".into());
        }
        let report = ConversionPreview {
            id: id.into(),
            expected_revision: stage.data.configuration_revision,
            source_name: stage.source.name.clone(),
            target_model: stage.target.model_id.clone(),
            profile_id: profile.id.clone(),
            name: profile.name.clone(),
            rows,
            background_present: stage.source.background_asset.is_some(),
            keep_background: stage.keep_background,
            cloned_macros: macros.iter().map(|m| m.name.clone()).collect(),
            ready: issues.is_empty(),
            issues,
        };
        Ok((report, profile, macros))
    }
    // Prepare profile conversion
    pub fn prepare_profile_conversion(
        &self,
        expected_revision: u64,
        profile_id: &str,
        target_model: &str,
    ) -> Result<ConversionPreview, String> {
        let (data, assets) = {
            let _transaction = self.profiles.transaction.lock().unwrap();
            let data = self.profiles.load()?;
            let source = data
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .ok_or("Source profile is missing")?;
            if !source.linked_controls.is_empty() {
                return Err("This conversion cannot preserve linked controls. Detach the groups before converting.".into());
            }
            let ids = source
                .pages
                .iter()
                .flat_map(|p| p.bindings.values())
                .filter_map(|b| b.asset.clone())
                .chain(source.background_asset.clone());
            let assets = self
                .profiles
                .asset_leases
                .pin(ids, "Staged profile conversion")?;
            (data, assets)
        };
        if data.configuration_revision != expected_revision {
            return Err("Workspace changed. Refresh before converting.".into());
        }
        if serde_json::to_vec(&data).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
            return Err(
                "Conversion review exceeds 16 MiB; split the workspace before converting".into(),
            );
        }
        let source = data
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or("Source profile is missing")?
            .clone();
        let target = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == target_model)
            .ok_or("Target model is unavailable")?;
        if source.model_id == target.model_id {
            return Err(
                "Choose a different target model; same-model copies use Add profile".into(),
            );
        }
        let closure = macro_closure(&source, &data)?;
        let controls = referenced_controls(&source, &data, &closure);
        if controls.len() > 256 {
            return Err("Conversion exceeds 256 source controls".into());
        }
        let name = format!(
            "{} ({})",
            source.name.chars().take(90).collect::<String>(),
            target.name
        );
        let mut stage = StagedConversion {
            profile_id: uuid::Uuid::new_v4().to_string(),
            pages: source
                .pages
                .iter()
                .map(|p| (p.id.clone(), uuid::Uuid::new_v4().to_string()))
                .collect(),
            macro_ids: closure
                .iter()
                .map(|id| (id.clone(), uuid::Uuid::new_v4().to_string()))
                .collect(),
            mappings: controls.into_iter().map(|id| (id, String::new())).collect(),
            name,
            keep_background: true,
            created: std::time::Instant::now(),
            _assets: assets,
            source,
            target,
            data,
        };
        let id = crate::review_store::new_id();
        let (first, _, _) = self.conversion_candidate(&id, &stage)?;
        let mut taken = HashSet::new();
        for row in first.rows {
            let choice = row
                .choices
                .iter()
                .find(|c| c.value == row.source_control && !taken.contains(&c.value))
                .or_else(|| row.choices.iter().find(|c| !taken.contains(&c.value)));
            if let Some(choice) = choice {
                taken.insert(choice.value.clone());
                stage
                    .mappings
                    .insert(row.source_control, choice.value.clone());
            }
        }
        let (report, _, _) = self.conversion_candidate(&id, &stage)?;
        let mut stages = self.conversions.lock().unwrap();
        stages.check_capacity()?;
        stages.insert(id, stage)?;
        Ok(report)
    }
    // Update profile conversion
    pub fn update_profile_conversion(
        &self,
        id: &str,
        name: String,
        mappings: BTreeMap<String, String>,
        keep_background: bool,
    ) -> Result<ConversionPreview, String> {
        let mut stages = self.conversions.lock().unwrap();
        let stage = stages.get_mut(id).ok_or("Conversion review expired")?;
        if mappings.len() != stage.mappings.len()
            || mappings.keys().any(|key| !stage.mappings.contains_key(key))
        {
            return Err("Conversion mapping identities changed".into());
        }
        if name.len() > 1024 || mappings.values().any(|s| s.len() > 512) {
            return Err("Conversion review exceeds bounds".into());
        }
        stage.name = name;
        stage.mappings = mappings;
        stage.keep_background = keep_background;
        Ok(self.conversion_candidate(id, stage)?.0)
    }
    // Discard the selected profile conversion state
    pub fn discard_profile_conversion(&self, id: &str) {
        self.conversions.lock().unwrap().remove(id);
    }
    // Commit profile conversion
    pub fn commit_profile_conversion(&self, id: &str) -> Result<Snapshot, String> {
        let mut stages = self.conversions.lock().unwrap();
        let stage = stages.get(id).ok_or("Conversion review expired")?;
        let (report, profile, macros) = self.conversion_candidate(id, stage)?;
        if !report.ready {
            return Err("Resolve every conversion issue before creating the profile".into());
        }
        let mut edits: Vec<_> = macros
            .into_iter()
            .map(|value| Edit::SaveMacro { value })
            .collect();
        edits.push(Edit::SaveProfile { profile });
        let result = self.apply_edits(
            report.expected_revision,
            "Convert profile to another model",
            edits,
        )?;
        stages.remove(id);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Disposable loopback host and temporary marker path only; no HID or native input"]
    // Verify that retired controls are rejected without saving or actuation
    fn retired_controls_are_rejected_without_saving_or_actuation() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().join("app"), false).unwrap();
        let before = runtime.snapshot();
        for (model, id) in [
            ("n3", "key-10"),
            ("n1", "key-18"),
            ("n4", "touch-1"),
            ("293s", "key-16"),
            ("293s-v3", "key-18"),
        ] {
            let mut source = before.profiles[0].clone();
            source.model_id = model.into();
            let mut binding = source.pages[0].bindings["key-1"].clone();
            binding.action = Action::Page {
                page_id: source.pages[0].id.clone(),
            };
            for page in &mut source.pages {
                page.bindings.clear();
            }
            source.pages[0].bindings.insert(id.into(), binding);
            let error = runtime
                .save_profile(source, before.configuration_revision)
                .unwrap_err();
            assert!(error.contains(model) && error.contains(id), "{error}");
            assert_eq!(
                runtime.snapshot().configuration_revision,
                before.configuration_revision
            );
            assert_eq!(
                serde_json::to_value(runtime.snapshot().profiles).unwrap(),
                serde_json::to_value(&before.profiles).unwrap()
            );
        }
        let source = &before.profiles[0];
        assert!(runtime
            .prepare_profile_conversion(before.configuration_revision, &source.id, &source.model_id)
            .unwrap_err()
            .contains("different target"));
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable loopback host; no effects"]
    // Verify that media is rechecked at commit and external binding targets stay external
    fn media_is_rechecked_at_commit_and_external_binding_targets_stay_external() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let png = hardware_core::render::tile(4, 4, [10, 20, 30], "", 0, "png").unwrap();
        let file = root.path().join("sample.png");
        std::fs::write(&file, png).unwrap();
        let asset = runtime.profiles.import_asset(&file).unwrap();
        let mut source = runtime.snapshot().profiles[0].clone();
        let external = builtin(
            serde_json::json!({"kind":"trigger_binding","device_id":"other-device","profile_id":source.id,"page_id":source.pages[0].id,"control_id":"key-2"}),
        );
        let binding = source.pages[0].bindings.get_mut("key-1").unwrap();
        binding.asset = Some(asset.clone());
        binding.action = external.clone();
        runtime
            .save_profile(source.clone(), runtime.snapshot().configuration_revision)
            .unwrap();
        let report = runtime
            .prepare_profile_conversion(
                runtime.snapshot().configuration_revision,
                &source.id,
                "mini",
            )
            .unwrap();
        assert!(report.ready, "{:?}", report.issues);
        std::fs::remove_file(
            runtime
                .profiles
                .root
                .join("assets")
                .join(format!("{asset}.png")),
        )
        .unwrap();
        assert!(runtime.commit_profile_conversion(&report.id).is_err());
        assert_eq!(runtime.snapshot().profiles.len(), 1);
        let mut map = mappings(&report);
        map.insert("key-1".into(), OMIT.into());
        let review = runtime
            .update_profile_conversion(&report.id, "No media".into(), map, false)
            .unwrap();
        assert!(review.ready, "{:?}", review.issues);
        {
            let stages = runtime.conversions.lock().unwrap();
            let stage = &stages[&report.id];
            let mut remapped = external.clone();
            let mut issues = vec![];
            remap_action(&mut remapped, stage, &mut issues);
            assert_eq!(
                serde_json::to_value(remapped).unwrap(),
                serde_json::to_value(external).unwrap()
            );
            assert!(issues.is_empty());
        }
        runtime.shutdown();
    }
    // Decode a built-in action from the test’s JSON payload
    fn builtin(value: serde_json::Value) -> Action {
        serde_json::from_value(serde_json::json!({"kind":"builtin","action":value})).unwrap()
    }
    // Extract source-to-target control mappings from a conversion preview
    fn mappings(report: &ConversionPreview) -> BTreeMap<String, String> {
        report
            .rows
            .iter()
            .map(|r| (r.source_control.clone(), r.target.clone()))
            .collect()
    }
    #[test]
    #[ignore = "Requires disposable loopback host; staged conversion executes no actions"]
    // Verify that conversion clones affected macro closure remaps internal targets and is one edit
    fn conversion_clones_affected_macro_closure_remaps_internal_targets_and_is_one_edit() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let mut source = runtime.snapshot().profiles[0].clone();
        let source_id = source.id.clone();
        let page = source.pages[0].id.clone();
        let leaf = SavedMacro {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Shared page".into(),
            inputs: vec![],
            action: Action::Page {
                page_id: page.clone(),
            },
        };
        let parent = SavedMacro {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Shared parent".into(),
            inputs: vec![],
            action: builtin(serde_json::json!({"kind":"saved_macro","macro_id":leaf.id})),
        };
        source.pages[0].bindings.get_mut("key-1").unwrap().action = Action::Macro {
            actions: vec![
                Action::Profile {
                    profile_id: source.id.clone(),
                },
                Action::Page {
                    page_id: page.clone(),
                },
                builtin(serde_json::json!({"kind":"saved_macro","macro_id":parent.id})),
                builtin(
                    serde_json::json!({"kind":"trigger_binding","device_id":"","profile_id":source.id,"page_id":page,"control_id":"key-2"}),
                ),
            ],
        };
        let current = runtime
            .apply_edits(
                runtime.snapshot().configuration_revision,
                "Prepare",
                vec![
                    Edit::SaveMacro {
                        value: leaf.clone(),
                    },
                    Edit::SaveMacro {
                        value: parent.clone(),
                    },
                    Edit::SaveProfile {
                        profile: source.clone(),
                    },
                ],
            )
            .unwrap();
        let sessions = serde_json::to_value(&current.sessions).unwrap();
        let before = serde_json::to_vec(&runtime.profiles.load().unwrap()).unwrap();
        let report = runtime
            .prepare_profile_conversion(current.configuration_revision, &source_id, "mini")
            .unwrap();
        assert!(report.ready, "{:?}", report.issues);
        assert_eq!(report.cloned_macros.len(), 2);
        assert_eq!(
            serde_json::to_vec(&runtime.profiles.load().unwrap()).unwrap(),
            before
        );
        let mut map = mappings(&report);
        map.insert("key-2".into(), "key-6".into());
        let review = runtime
            .update_profile_conversion(&report.id, "Converted".into(), map, true)
            .unwrap();
        assert!(review.ready, "{:?}", review.issues);
        let committed = runtime.commit_profile_conversion(&report.id).unwrap();
        assert_eq!(
            committed.configuration_revision,
            current.configuration_revision + 1
        );
        assert_eq!(committed.history.undo.len(), current.history.undo.len() + 1);
        assert_eq!(serde_json::to_value(&committed.sessions).unwrap(), sessions);
        assert_eq!(
            serde_json::to_value(
                committed
                    .profiles
                    .iter()
                    .find(|p| p.id == source_id)
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(&source).unwrap()
        );
        let copy = committed
            .profiles
            .iter()
            .find(|p| p.id == report.profile_id)
            .unwrap();
        assert_eq!(copy.model_id, "mini");
        assert!(copy.pages[0].bindings.contains_key("key-6"));
        assert_ne!(copy.pages[0].id, page);
        let copied = serde_json::to_value(&copy.pages[0].bindings["key-1"].action).unwrap();
        assert_eq!(copied["actions"][0]["profile_id"], copy.id);
        assert_eq!(copied["actions"][1]["page_id"], copy.pages[0].id);
        assert_eq!(copied["actions"][3]["action"]["control_id"], "key-6");
        assert_eq!(copied["actions"][3]["action"]["page_id"], copy.pages[0].id);
        let parent_id = copied["actions"][2]["action"]["macro_id"].as_str().unwrap();
        assert_ne!(parent_id, parent.id);
        let parent_copy = committed
            .saved_macros
            .iter()
            .find(|m| m.id == parent_id)
            .unwrap();
        let leaf_id = serde_json::to_value(&parent_copy.action).unwrap()["action"]["macro_id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(leaf_id, leaf.id);
        assert!(
            matches!(&committed.saved_macros.iter().find(|m|m.id==leaf_id).unwrap().action,Action::Page{page_id} if *page_id==copy.pages[0].id)
        );
        assert_eq!(
            serde_json::to_value(
                committed
                    .saved_macros
                    .iter()
                    .find(|m| m.id == leaf.id)
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(&leaf).unwrap()
        );
        let undone = runtime
            .history_action("undo", committed.configuration_revision)
            .unwrap();
        assert_eq!(undone.profiles.len(), 1);
        assert_eq!(undone.saved_macros.len(), 2);
        let redone = runtime
            .history_action("redo", undone.configuration_revision)
            .unwrap();
        assert!(redone.profiles.iter().any(|p| p.id == report.profile_id));
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable loopback host; no effects"]
    // Verify that unresolved bindings require omission duplicate targets and stale reviews are rejected
    fn unresolved_bindings_require_omission_duplicate_targets_and_stale_reviews_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let mut source = runtime.snapshot().profiles[0].clone();
        source.pages[0].bindings.get_mut("key-1").unwrap().action = Action::Plugin {
            user_desired_state: None,
            plugin_id: "missing".into(),
            action_id: "missing".into(),
            settings: serde_json::json!({}),
            resources: Default::default(),
        };
        let mut swipe = source.pages[0].bindings["key-2"].clone();
        swipe.action = Action::SwipeNavigation {
            target: "page".into(),
            reverse: false,
            wrap: true,
        };
        source.pages[0].bindings.insert("touch-strip".into(), swipe);
        runtime
            .save_profile(source.clone(), runtime.snapshot().configuration_revision)
            .unwrap();
        let revision = runtime.snapshot().configuration_revision;
        let review = runtime
            .prepare_profile_conversion(revision, &source.id, "mini")
            .unwrap();
        assert!(!review.ready);
        assert!(runtime.commit_profile_conversion(&review.id).is_err());
        let mut map = mappings(&review);
        map.insert("key-1".into(), OMIT.into());
        map.insert("touch-strip".into(), OMIT.into());
        let ready = runtime
            .update_profile_conversion(&review.id, "Converted".into(), map.clone(), false)
            .unwrap();
        assert!(ready.ready, "{:?}", ready.issues);
        map.insert("key-2".into(), map["key-3"].clone());
        let invalid = runtime
            .update_profile_conversion(&review.id, "Converted".into(), map, false)
            .unwrap();
        assert!(!invalid.ready);
        assert!(runtime.commit_profile_conversion(&review.id).is_err());
        let other = runtime
            .prepare_profile_conversion(revision, &source.id, "mini")
            .unwrap();
        source.name = "Edited elsewhere".into();
        runtime.save_profile(source, revision).unwrap();
        assert!(runtime
            .commit_profile_conversion(&other.id)
            .unwrap_err()
            .contains("changed"));
        runtime.discard_profile_conversion(&other.id);
        assert!(!runtime.conversions.lock().unwrap().contains_key(&other.id));
        runtime.shutdown();
    }
    #[test]
    // Verify that transitive release and swipe requirements are checked without execution
    fn transitive_release_and_swipe_requirements_are_checked_without_execution() {
        let manifest = hardware_registry::manifests()
            .into_iter()
            .find(|m| m.model_id == "mini")
            .unwrap();
        let mut control = manifest.controls[0].clone();
        control.semantics = vec!["press".into()];
        let saved = SavedMacro {
            id: "held".into(),
            name: "Held".into(),
            inputs: vec![],
            action: builtin(serde_json::json!({"kind":"wait_release","timeout_ms":1000})),
        };
        let call = builtin(serde_json::json!({"kind":"saved_macro","macro_id":"held"}));
        assert!(check_action(&call, &control, &[saved], &[], 0, &manifest)
            .unwrap_err()
            .contains("release"));
        assert!(check_action(
            &Action::SwipeNavigation {
                target: "page".into(),
                reverse: false,
                wrap: true
            },
            &control,
            &[],
            &[],
            0,
            &manifest
        )
        .unwrap_err()
        .contains("swipe"));
    }
}
