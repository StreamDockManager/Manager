//! Reviewable profile imports. Inspection and repairs live in an isolated temporary workspace
use crate::{actions::Action, Binding, ProfileData, ProfileStore, Runtime, Snapshot};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ImportChoice {
    pub value: String,
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ImportIssue {
    pub key: String,
    pub kind: String,
    pub profile_id: String,
    pub page_id: String,
    pub control_id: String,
    pub action_path: Vec<usize>,
    pub choices: Vec<ImportChoice>,
    pub message: String,
    pub value: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ImportRepair {
    pub key: String,
    pub replacement: String,
    pub disable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ImportPreview {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plugin: Option<crate::plugin_profiles::PluginProfilePreview>,
    pub id: String,
    #[ts(type = "number")]
    pub expected_revision: u64,
    pub mode: String,
    pub profiles: Vec<String>,
    pub style_count: usize,
    pub issues: Vec<ImportIssue>,
    pub impact: Vec<String>,
    pub repair_suggestions: Vec<crate::import_recipes::ImportRepairSuggestion>,
    pub recipe_warning: Option<String>,
}
pub(crate) struct StagedImport {
    pub store: ProfileStore,
    pub data: ProfileData,
    pub revision: u64,
    pub mode: String,
    pub created: std::time::Instant,
    _directory: tempfile::TempDir,
}
impl crate::review_store::ReviewSession for StagedImport {
    const LIMIT: usize = 8;
    const FULL: &'static str = "Close an existing import before opening another";
    const LIFETIME: Option<std::time::Duration> = Some(std::time::Duration::from_secs(3600));
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
#[allow(clippy::too_many_arguments)]
// Collect action import issues with their nested action path and owning workspace context
fn action_issues(
    action: &Action,
    path: Vec<usize>,
    profile: &str,
    page: &str,
    control: &str,
    data: &ProfileData,
    plugins: &[stream_dock_manager_plugin_host::PluginInfo],
    kind: &str,
    out: &mut Vec<ImportIssue>,
) {
    if path.len() > 8 {
        return;
    }
    let issue = |kind: &str, value: String, message: String, out: &mut Vec<ImportIssue>| {
        out.push(ImportIssue {
            choices: vec![],
            key: format!("{profile}/{page}/{control}/{path:?}/{kind}"),
            kind: kind.into(),
            profile_id: profile.into(),
            page_id: page.into(),
            control_id: control.into(),
            action_path: path.clone(),
            message,
            value,
        })
    };
    match action {
        Action::Plugin {
            plugin_id,
            action_id,
            ..
        } => {
            match plugins
                .iter()
                .find(|p| p.id == *plugin_id)
                .and_then(|p| p.actions.iter().find(|a| a.id == *action_id))
            {
                None => issue(
                    "plugin",
                    format!("{plugin_id}:{action_id}"),
                    "Required plugin or action is missing".into(),
                    out,
                ),
                Some(a) if !kind.is_empty() && !a.control_types.iter().any(|k| k == kind) => issue(
                    "plugin",
                    format!("{plugin_id}:{action_id}"),
                    "Plugin action is incompatible with this control".into(),
                    out,
                ),
                _ => {}
            }
        }
        Action::Profile { profile_id } if !data.profiles.iter().any(|p| p.id == *profile_id) => {
            issue(
                "profile",
                profile_id.clone(),
                "Target profile is missing".into(),
                out,
            )
        }
        Action::Page { page_id }
            if !data
                .profiles
                .iter()
                .any(|p| p.pages.iter().any(|page| page.id == *page_id)) =>
        {
            issue(
                "page",
                page_id.clone(),
                "Target page is missing".into(),
                out,
            )
        }
        Action::Application { path } | Action::OpenPath { path } if !Path::new(path).exists() => {
            issue(
                "path",
                path.clone(),
                "Application or file path does not exist on this computer".into(),
                out,
            )
        }
        Action::Command { program, .. } if !program_exists(program) => issue(
            "path",
            program.clone(),
            "Command executable cannot be found".into(),
            out,
        ),
        Action::Builtin { action } => {
            use crate::builtins::BuiltinAction;
            match &**action {
                BuiltinAction::SavedMacro { macro_id, .. } => {
                    if let Some(macro_) = data.saved_macros.iter().find(|m| m.id == *macro_id) {
                        let mut next = path.clone();
                        next.push(0);
                        action_issues(
                            &macro_.action,
                            next,
                            profile,
                            page,
                            control,
                            data,
                            plugins,
                            kind,
                            out,
                        );
                    } else {
                        issue(
                            "macro",
                            macro_id.clone(),
                            "Referenced saved macro is missing".into(),
                            out,
                        );
                    }
                }
                BuiltinAction::MomentaryPage { page_id }
                | BuiltinAction::BindingLayer { page_id, .. }
                    if !data
                        .profiles
                        .iter()
                        .any(|p| p.pages.iter().any(|page| page.id == *page_id)) =>
                {
                    issue(
                        "page",
                        page_id.clone(),
                        "Referenced page is missing".into(),
                        out,
                    )
                }
                _ => {}
            }
            for (i, child) in action.children().iter().enumerate() {
                let mut next = path.clone();
                next.push(i);
                action_issues(
                    child, next, profile, page, control, data, plugins, kind, out,
                );
            }
        }
        Action::Macro { actions } | Action::Dial { actions, .. } => {
            for (i, a) in actions.iter().enumerate() {
                let mut next = path.clone();
                next.push(i);
                action_issues(a, next, profile, page, control, data, plugins, kind, out);
            }
        }
        Action::Toggle { on, off } => {
            for (i, a) in [on, off].into_iter().enumerate() {
                let mut next = path.clone();
                next.push(i);
                action_issues(a, next, profile, page, control, data, plugins, kind, out);
            }
        }
        _ => {}
    }
}
// Report the program exists condition for importing
fn program_exists(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    if Path::new(program).components().count() > 1 {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|p| {
            p.join(program).is_file()
                || (cfg!(windows) && p.join(format!("{program}.exe")).is_file())
        })
    })
}
// Inspect the selected importing resource
fn inspect(stage: &StagedImport, snapshot: &Snapshot) -> Vec<ImportIssue> {
    let plugins = &snapshot.plugins;
    let mut references = stage.data.clone();
    if stage.mode == "merge" {
        for profile in &snapshot.profiles {
            if !references.profiles.iter().any(|p| p.id == profile.id) {
                references.profiles.push(profile.clone());
            }
        }
    }
    let mut out = vec![];
    let manifests = hardware_registry::manifests();
    for profile in &stage.data.profiles {
        let manifest = manifests.iter().find(|m| m.model_id == profile.model_id);
        if manifest.is_none() {
            out.push(ImportIssue {
                choices: vec![],
                key: format!("{}/model", profile.id),
                kind: "model".into(),
                profile_id: profile.id.clone(),
                page_id: String::new(),
                control_id: String::new(),
                action_path: vec![],
                message: "Profile uses an unsupported device model".into(),
                value: profile.model_id.clone(),
            });
        }
        if !profile
            .pages
            .iter()
            .any(|page| page.id == profile.active_page)
        {
            out.push(ImportIssue {
                choices: vec![],
                key: format!("{}/active_page", profile.id),
                kind: "active_page".into(),
                profile_id: profile.id.clone(),
                page_id: String::new(),
                control_id: String::new(),
                action_path: vec![],
                message: "Profile's active page is missing".into(),
                value: profile.active_page.clone(),
            });
        }
        if let Some(asset) = &profile.background_asset {
            if stage.store.asset(asset).is_err() {
                out.push(ImportIssue {
                    choices: vec![],
                    key: format!("{}/background", profile.id),
                    kind: "background".into(),
                    profile_id: profile.id.clone(),
                    page_id: String::new(),
                    control_id: String::new(),
                    action_path: vec![],
                    message: "Background media is missing".into(),
                    value: asset.clone(),
                });
            }
        }
        for page in &profile.pages {
            for (control, binding) in &page.bindings {
                let target = manifest.and_then(|m| m.control(control).ok());
                if target.is_none() {
                    out.push(ImportIssue {
                        choices: vec![],
                        key: format!("{}/{}/{control}/control", profile.id, page.id),
                        kind: "control".into(),
                        profile_id: profile.id.clone(),
                        page_id: page.id.clone(),
                        control_id: control.clone(),
                        action_path: vec![],
                        message: "Control does not exist on this device model".into(),
                        value: control.clone(),
                    });
                }
                binding_issues(
                    binding,
                    &profile.id,
                    &page.id,
                    control,
                    &references,
                    &stage.store,
                    plugins,
                    target.map(|c| c.kind.as_str()).unwrap_or(""),
                    &mut out,
                );
            }
        }
    }
    for issue in &mut out {
        let choice = |value: String, label: String| ImportChoice { value, label };
        issue.choices = match issue.kind.as_str() {
            "macro" => references
                .saved_macros
                .iter()
                .map(|m| choice(m.id.clone(), m.name.clone()))
                .collect(),
            "profile" => references
                .profiles
                .iter()
                .map(|p| choice(p.id.clone(), p.name.clone()))
                .collect(),
            "page" => references
                .profiles
                .iter()
                .flat_map(|p| {
                    p.pages
                        .iter()
                        .map(|page| choice(page.id.clone(), format!("{} / {}", p.name, page.name)))
                })
                .collect(),
            "active_page" => stage
                .data
                .profiles
                .iter()
                .filter(|p| p.id == issue.profile_id)
                .flat_map(|p| {
                    p.pages
                        .iter()
                        .map(|page| choice(page.id.clone(), page.name.clone()))
                })
                .collect(),
            "model" => manifests
                .iter()
                .map(|m| choice(m.model_id.clone(), m.name.clone()))
                .collect(),
            "control" => stage
                .data
                .profiles
                .iter()
                .find(|p| p.id == issue.profile_id)
                .and_then(|p| manifests.iter().find(|m| m.model_id == p.model_id))
                .map(|m| {
                    m.controls
                        .iter()
                        .map(|c| choice(c.id.clone(), c.label.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            "plugin" => snapshot
                .plugins
                .iter()
                .flat_map(|p| {
                    p.actions.iter().map(|a| {
                        choice(
                            format!("{}:{}", p.id, a.id),
                            format!("{} / {}", p.name, a.name),
                        )
                    })
                })
                .collect(),
            _ => vec![],
        };
    }
    out
}
#[allow(clippy::too_many_arguments)]
// Collect binding media and action issues before an imported binding is accepted
fn binding_issues(
    binding: &Binding,
    profile: &str,
    page: &str,
    control: &str,
    data: &ProfileData,
    store: &ProfileStore,
    plugins: &[stream_dock_manager_plugin_host::PluginInfo],
    kind: &str,
    out: &mut Vec<ImportIssue>,
) {
    if binding.disabled {
        return;
    }
    if let Some(asset) = &binding.asset {
        if store.asset(asset).is_err() {
            out.push(ImportIssue {
                choices: vec![],
                key: format!("{profile}/{page}/{control}/media"),
                kind: "media".into(),
                profile_id: profile.into(),
                page_id: page.into(),
                control_id: control.into(),
                action_path: vec![],
                message: "Assigned media is missing".into(),
                value: asset.clone(),
            });
        }
    }
    action_issues(
        &binding.action,
        vec![],
        profile,
        page,
        control,
        data,
        plugins,
        kind,
        out,
    );
}
// Borrow mutable binding state for importing
fn binding_mut<'a>(
    data: &'a mut ProfileData,
    issue: &ImportIssue,
) -> Result<&'a mut Binding, String> {
    data.profiles
        .iter_mut()
        .find(|p| p.id == issue.profile_id)
        .and_then(|p| p.pages.iter_mut().find(|p| p.id == issue.page_id))
        .and_then(|p| p.bindings.get_mut(&issue.control_id))
        .ok_or("Binding missing".into())
}
// Borrow mutable action state for importing
fn action_mut<'a>(
    data: &'a mut ProfileData,
    issue: &ImportIssue,
) -> Result<&'a mut Action, String> {
    let root = {
        &data
            .profiles
            .iter()
            .find(|p| p.id == issue.profile_id)
            .and_then(|p| p.pages.iter().find(|p| p.id == issue.page_id))
            .and_then(|p| p.bindings.get(&issue.control_id))
            .ok_or("Binding missing")?
            .action
    };
    let (owner, path) =
        crate::builtins::resolve_reference_path(root, &issue.action_path, &data.saved_macros)?;
    if let Some(id) = owner {
        crate::workspace::action_at_mut(
            &mut data
                .saved_macros
                .iter_mut()
                .find(|m| m.id == id)
                .ok_or("Saved macro missing")?
                .action,
            &path,
        )
    } else {
        crate::workspace::action_at_mut(&mut binding_mut(data, issue)?.action, &path)
    }
}
impl Runtime {
    // Prepare profile import
    pub fn prepare_profile_import(&self, path: &Path) -> Result<ImportPreview, String> {
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let store = ProfileStore::open(directory.path().join("profiles"))?;
        let data = store.import_for_repair(path)?;
        let mut ids = HashSet::new();
        for profile in &data.profiles {
            if profile.id.is_empty() || !ids.insert(profile.id.clone()) {
                return Err("Archive contains duplicate or empty profile IDs".into());
            }
            let mut pages = HashSet::new();
            if profile.pages.is_empty()
                || profile
                    .pages
                    .iter()
                    .any(|p| p.id.is_empty() || !pages.insert(p.id.clone()))
            {
                return Err("Archive contains duplicate, empty, or missing page IDs".into());
            }
        }

        let stage = StagedImport {
            store,
            data,
            revision: self.snapshot().configuration_revision,
            mode: "merge".into(),
            created: std::time::Instant::now(),
            _directory: directory,
        };
        let id = crate::review_store::new_id();
        let report = self.import_preview(&id, &stage);
        let mut imports = self.imports.lock().unwrap();
        imports.check_capacity()?;
        imports.insert(id, stage)?;
        Ok(report)
    }
    // Import preview
    fn import_preview(&self, id: &str, stage: &StagedImport) -> ImportPreview {
        let current = self.snapshot();
        let mut impact = vec![];
        if stage.mode == "replace" {
            impact.push(format!(
                "Replace {} existing profiles",
                current.profiles.len()
            ));
            for rule in &current.rules {
                if !stage.data.profiles.iter().any(|p| {
                    p.id == rule.profile_id
                        && rule
                            .page_id
                            .as_ref()
                            .is_none_or(|id| p.pages.iter().any(|page| page.id == *id))
                }) {
                    impact.push(format!(
                        "Remove local automation rule {} because its target is removed",
                        rule.id
                    ));
                }
            }
        } else {
            impact.push("Add imported profiles; existing device selections and rules stay in place. Conflicting IDs are remapped.".into());
        }
        let mut includes_settings = false;
        for action in stage
            .data
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
            .map(|b| &b.action)
            .chain(stage.data.saved_macros.iter().map(|m| &m.action))
        {
            action.visit(&mut |a| {
                if let Action::Plugin { settings, .. } = a {
                    if settings.as_object().is_some_and(|s| !s.is_empty()) {
                        includes_settings = true;
                    }
                }
            });
        }
        if includes_settings {
            impact.push("This archive includes plugin action settings, which may contain credentials or private information. Review them before using or sharing it.".into());
        }
        let disabled = stage
            .data
            .profiles
            .iter()
            .flat_map(|p| &p.pages)
            .flat_map(|p| p.bindings.values())
            .filter(|b| b.disabled)
            .count();
        if disabled > 0 {
            impact.push(format!(
                "{disabled} imported bindings remain disabled for later repair"
            ));
        }
        let issues = inspect(stage, &current);
        let (repair_suggestions, recipe_warning) = self.suggest_import_repairs(stage, &issues);
        ImportPreview {
            plugin: None,
            id: id.into(),
            expected_revision: stage.revision,
            mode: stage.mode.clone(),
            profiles: stage.data.profiles.iter().map(|p| p.name.clone()).collect(),
            style_count: stage.data.styles.len(),
            issues,
            impact,
            repair_suggestions,
            recipe_warning,
        }
    }
    // Update profile import
    pub fn update_profile_import(
        &self,
        id: &str,
        mode: &str,
        repairs: Vec<ImportRepair>,
    ) -> Result<ImportPreview, String> {
        self.update_profile_import_remembered(id, mode, repairs, None)
    }
    // Update profile import remembered
    pub fn update_profile_import_remembered(
        &self,
        id: &str,
        mode: &str,
        repairs: Vec<ImportRepair>,
        remember: Option<crate::import_recipes::RememberImportRepair>,
    ) -> Result<ImportPreview, String> {
        if !["merge", "replace"].contains(&mode) {
            return Err("Unknown import mode".into());
        }
        let snapshot = self.snapshot();
        let mut imports = self.imports.lock().unwrap();
        let stage = imports.get_mut(id).ok_or("Import session expired")?;
        let issues = inspect(stage, &snapshot);
        if remember.is_some() && repairs.len() != 1 {
            return Err("Remember one explicitly reviewed repair at a time".into());
        }
        let mut candidate = stage.data.clone();
        let remembered = remember
            .as_ref()
            .map(|request| {
                let issue = issues
                    .iter()
                    .find(|i| i.key == request.key)
                    .ok_or("Issue changed; recheck the import")?;
                let repair = repairs
                    .first()
                    .filter(|r| r.key == issue.key)
                    .ok_or("Repair does not match the remembered issue")?;
                Ok::<_, String>((issue.clone(), repair.clone(), request))
            })
            .transpose()?;
        for repair in repairs {
            let issue = issues
                .iter()
                .find(|i| i.key == repair.key)
                .ok_or("Issue changed; recheck the import")?;
            if repair.disable {
                binding_mut(&mut candidate, issue)?.disabled = true;
                continue;
            }
            match issue.kind.as_str() {
                "media" | "background" => {
                    let asset = if repair.replacement.is_empty() {
                        None
                    } else {
                        Some(stage.store.import_asset(Path::new(&repair.replacement))?)
                    };
                    if issue.kind == "background" {
                        candidate
                            .profiles
                            .iter_mut()
                            .find(|p| p.id == issue.profile_id)
                            .ok_or("Profile missing")?
                            .background_asset = asset;
                        candidate
                            .profiles
                            .iter_mut()
                            .find(|p| p.id == issue.profile_id)
                            .unwrap()
                            .background_media = Default::default();
                    } else {
                        let binding = binding_mut(&mut candidate, issue)?;
                        binding.asset = asset;
                        binding.media = Default::default();
                    }
                }
                "active_page" => {
                    candidate
                        .profiles
                        .iter_mut()
                        .find(|p| p.id == issue.profile_id)
                        .ok_or("Profile missing")?
                        .active_page = repair.replacement;
                }
                "model" => {
                    let profile = candidate
                        .profiles
                        .iter_mut()
                        .find(|p| p.id == issue.profile_id)
                        .ok_or("Profile missing")?;
                    profile.model_id = repair.replacement;
                }
                "control" => {
                    let page = candidate
                        .profiles
                        .iter_mut()
                        .find(|p| p.id == issue.profile_id)
                        .and_then(|p| p.pages.iter_mut().find(|p| p.id == issue.page_id))
                        .ok_or("Page missing")?;
                    if page.bindings.contains_key(&repair.replacement) {
                        return Err("Destination control is occupied".into());
                    }
                    let binding = page
                        .bindings
                        .remove(&issue.control_id)
                        .ok_or("Binding missing")?;
                    if !repair.replacement.is_empty() {
                        page.bindings.insert(repair.replacement, binding);
                    }
                }
                _ => {
                    let action = action_mut(&mut candidate, issue)?;
                    match action {
                        Action::Builtin { action } => {
                            use crate::builtins::BuiltinAction;
                            match &mut **action {
                                BuiltinAction::SavedMacro { macro_id, .. } => {
                                    *macro_id = repair.replacement
                                }
                                BuiltinAction::MomentaryPage { page_id }
                                | BuiltinAction::BindingLayer { page_id, .. } => {
                                    *page_id = repair.replacement
                                }
                                _ => return Err("Unsupported repair".into()),
                            }
                        }
                        Action::Application { path } | Action::OpenPath { path } => {
                            *path = repair.replacement
                        }
                        Action::Command { program, .. } => *program = repair.replacement,
                        Action::Profile { profile_id } => *profile_id = repair.replacement,
                        Action::Page { page_id } => *page_id = repair.replacement,
                        Action::Plugin {
                            plugin_id,
                            action_id,
                            ..
                        } => {
                            let (plugin, action) = repair
                                .replacement
                                .split_once(':')
                                .ok_or("Use plugin-id:action-id")?;
                            *plugin_id = plugin.into();
                            *action_id = action.into();
                        }
                        _ => return Err("Unsupported repair".into()),
                    }
                }
            }
        }
        if let Some((issue, repair, request)) = remembered {
            // Inspect the candidate without publishing it. A remaining issue at this location must
            // be reviewed rather than saving a recipe that did not fix its dependency
            std::mem::swap(&mut stage.data, &mut candidate);
            let unresolved = inspect(stage, &snapshot).iter().any(|i| i.key == issue.key);
            std::mem::swap(&mut stage.data, &mut candidate);
            if unresolved {
                return Err("The repair remains unresolved and cannot be remembered".into());
            }
            self.remember_import_repair(stage, &issue, &repair, request)?;
        }
        stage.data = candidate;
        stage.mode = mode.into();
        stage.revision = self.snapshot().configuration_revision;
        Ok(self.import_preview(id, stage))
    }
    // Cancel profile import
    pub fn cancel_profile_import(&self, id: &str) {
        self.plugin_profile_imports.lock().unwrap().remove(id);
        self.imports.lock().unwrap().remove(id);
    }
    // Commit profile import
    pub fn commit_profile_import(&self, id: &str, expected: u64) -> Result<Snapshot, String> {
        if self.plugin_profile_imports.lock().unwrap().contains_key(id) {
            return self.commit_plugin_profile_import(id, expected);
        }
        let mut imports = self.imports.lock().unwrap();
        let stage = imports.get_mut(id).ok_or("Import session expired")?;
        if !inspect(stage, &self.snapshot()).is_empty() {
            return Err("Resolve remaining import issues or disable their bindings".into());
        }
        let mut state = self.state.lock().unwrap();

        if state.configuration_revision != expected || stage.revision != expected {
            return Err("Workspace changed. Recheck this import before committing; repairs have been retained.".into());
        }
        let _transaction = self.profiles.transaction.lock().unwrap();
        let _gate = self.profiles.media_gate.lock().unwrap();
        let mut imported = stage.data.clone();
        crate::keyboard_navigation::normalize(&mut imported)?;
        let mut created = vec![];
        let result = (|| {
            for asset in crate::profiles::referenced_assets(&imported) {
                let Ok(bytes) = stage.store.asset(&asset) else {
                    continue;
                };
                let destination_id = if self.profiles.asset(&asset).is_ok_and(|old| old != bytes) {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    asset.clone()
                };
                if destination_id != asset {
                    remap_asset(&mut imported, &asset, &destination_id);
                }
                let extension = stage.store.asset_extension(&asset)?;
                let destination = self
                    .profiles
                    .root
                    .join("assets")
                    .join(format!("{destination_id}.{extension}"));
                if !destination.exists() {
                    let mut temp = tempfile::NamedTempFile::new_in(destination.parent().unwrap())
                        .map_err(|e| e.to_string())?;
                    use std::io::Write;
                    temp.write_all(&bytes).map_err(|e| e.to_string())?;
                    temp.as_file().sync_all().map_err(|e| e.to_string())?;
                    crate::profiles::atomic_replace(temp.path(), &destination)?;
                    created.push(destination);
                }
            }
            let prepared = self.profiles.prepare_import_transaction(
                expected,
                "Import profiles",
                None,
                0,
                |data| {
                    if stage.mode == "merge" {
                        merge(data, imported);
                    } else {
                        data.profiles = imported.profiles;

                        data.saved_macros = imported.saved_macros;

                        data.styles = imported.styles;
                        data.active_profile = imported.active_profile;
                        data.rules.retain(|r| {
                            data.profiles.iter().any(|p| {
                                p.id == r.profile_id
                                    && r.page_id
                                        .as_ref()
                                        .is_none_or(|id| p.pages.iter().any(|page| page.id == *id))
                            })
                        });
                    }
                    self.validate_workspace(data)
                },
            )?;
            let replace = stage.mode != "merge";
            self.profiles
                .storage
                .transaction_with_cost(prepared.retained_bytes()?, move |tx| {
                    // Reused external IDs are not authority for a bundled vendor profile
                    if replace {
                        crate::vendor_profiles::forget_all(tx)?;
                    }
                    prepared.commit(tx)
                })
        })();
        match result {
            Ok(data) => Self::apply_data(&mut state, data),
            Err(error) => {
                for path in created {
                    if let Some(id) = path.file_stem().and_then(|s| s.to_str()) {
                        self.profiles.forget_asset(id);
                    }
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
        }

        drop(_transaction);
        drop(_gate);
        drop(state);

        imports.remove(id);
        drop(imports);
        self.finish_workspace_commit();
        Ok(self.snapshot())
    }
}
// Remap asset
fn remap_asset(data: &mut ProfileData, old: &str, new: &str) {
    for p in &mut data.profiles {
        if p.background_asset.as_deref() == Some(old) {
            p.background_asset = Some(new.into());
        }
        for b in p.pages.iter_mut().flat_map(|p| p.bindings.values_mut()) {
            if b.asset.as_deref() == Some(old) {
                b.asset = Some(new.into());
            }
        }
    }
}
// Merge the supplied importing values
fn merge(target: &mut ProfileData, mut source: ProfileData) {
    let existing_profiles: HashSet<_> = target.profiles.iter().map(|p| p.id.clone()).collect();
    let existing_pages: HashSet<_> = target
        .profiles
        .iter()
        .flat_map(|p| p.pages.iter().map(|page| page.id.clone()))
        .collect();
    let mut profiles = HashMap::new();
    let mut pages = HashMap::new();
    // Shared IDs must have one order-independent mapping, including converted
    // non-keyboard pages which intentionally retain this profile-local ID
    if source.profiles.iter().any(|profile| {
        crate::keyboard_navigation::supported(&profile.model_id)
            && profile.pages.iter().any(|page| {
                page.id == crate::keyboard_navigation::PAGE_ID
                    && crate::keyboard_navigation::canonical(page)
            })
    }) {
        pages.insert(
            crate::keyboard_navigation::PAGE_ID.into(),
            crate::keyboard_navigation::PAGE_ID.into(),
        );
    }
    for profile in &mut source.profiles {
        let old = profile.id.clone();
        if existing_profiles.contains(&old) {
            profile.id = uuid::Uuid::new_v4().to_string();
            profile.name = format!(
                "{} (imported)",
                profile.name.chars().take(140).collect::<String>()
            );
        }
        profiles.insert(old, profile.id.clone());
        // Shared page IDs stay shared across imported profiles, preserving cross-profile macros
        for page in &mut profile.pages {
            // Generated navigation identity is profile-local and shared intentionally
            // Renaming it would create an editable duplicate on the next normalization
            if crate::keyboard_navigation::supported(&profile.model_id)
                && page.id == crate::keyboard_navigation::PAGE_ID
                && crate::keyboard_navigation::canonical(page)
            {
                pages.insert(page.id.clone(), page.id.clone());
                continue;
            }
            let replacement = pages.entry(page.id.clone()).or_insert_with(|| {
                if existing_pages.contains(&page.id) {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    page.id.clone()
                }
            });
            page.id = replacement.clone();
        }
        if let Some(page) = pages.get(&profile.active_page) {
            profile.active_page = page.clone();
        }
    }
    // Apply the importing correction
    fn fix(
        action: &mut Action,
        profiles: &HashMap<String, String>,
        pages: &HashMap<String, String>,
    ) {
        let result: Result<(), std::convert::Infallible> = action.try_visit_fields_mut(
            &mut |role, value| {
                let replacement = match role {
                    crate::action_fields::FieldRole::Profile => profiles.get(value),
                    crate::action_fields::FieldRole::Page => pages.get(value),
                    _ => None,
                };
                if let Some(id) = replacement {
                    *value = id.clone();
                }
                Ok(())
            },
            &mut |_, _, _| Ok(()),
        );
        result.unwrap_or_else(|never| match never {});
    }
    for binding in source
        .profiles
        .iter_mut()
        .flat_map(|p| &mut p.pages)
        .flat_map(|p| p.bindings.values_mut())
    {
        fix(&mut binding.action, &profiles, &pages);
    }
    for style in &mut source.styles {
        if target.styles.iter().any(|s| s.id == style.id) {
            style.id = uuid::Uuid::new_v4().to_string();
        }
    }
    let remap = source
        .saved_macros
        .iter()
        .filter(|m| target.saved_macros.iter().any(|t| t.id == m.id))
        .map(|m| (m.id.clone(), uuid::Uuid::new_v4().to_string()))
        .collect();
    crate::builtins::remap_macros(&mut source, &remap);
    for macro_ in &mut source.saved_macros {
        fix(&mut macro_.action, &profiles, &pages);
    }
    target.saved_macros.extend(source.saved_macros);
    if target.profiles.is_empty() {
        target.active_profile = profiles
            .get(&source.active_profile)
            .cloned()
            .unwrap_or_default();
    }
    target.profiles.extend(source.profiles);

    target.styles.extend(source.styles);
}

#[cfg(test)]
mod keyboard_navigation_import_tests {
    #[test]
    // Verify that mixed model shared navigation IDs remap independently of import order
    fn mixed_model_shared_navigation_ids_remap_independently_of_import_order() {
        for reverse in [false, true] {
            let mut target = crate::ProfileData::default();
            target.profiles[0].model_id = "k1-pro".into();
            crate::keyboard_navigation::normalize(&mut target).unwrap();
            let mut source = target.clone();
            let mut converted = source.profiles[0].clone();
            converted.id = "converted".into();
            converted.model_id = "n4-pro".into();
            converted.active_page = crate::keyboard_navigation::PAGE_ID.into();
            converted.pages[0].bindings.get_mut("key-1").unwrap().action =
                crate::actions::Action::Page {
                    page_id: crate::keyboard_navigation::PAGE_ID.into(),
                };
            source.profiles.push(converted);
            if reverse {
                source.profiles.reverse();
            }
            super::merge(&mut target, source);
            crate::keyboard_navigation::normalize(&mut target).unwrap();
            for profile in &target.profiles {
                profile.validate().unwrap();
                if profile.id == "converted" {
                    assert_eq!(profile.active_page, crate::keyboard_navigation::PAGE_ID);
                    assert!(
                        matches!(&profile.pages[0].bindings["key-1"].action, crate::actions::Action::Page { page_id } if page_id == &profile.pages[0].id)
                    );
                }
            }
        }
    }
    #[test]
    // Verify that repeated merge preserves one protected navigation page per profile
    fn repeated_merge_preserves_one_protected_navigation_page_per_profile() {
        let mut target = crate::ProfileData::default();
        target.profiles[0].model_id = "k1-pro".into();
        crate::keyboard_navigation::normalize(&mut target).unwrap();
        let source = target.clone();
        for _ in 0..2 {
            super::merge(&mut target, source.clone());
            crate::keyboard_navigation::normalize(&mut target).unwrap();
        }
        assert_eq!(target.profiles.len(), 3);
        for p in &target.profiles {
            assert_eq!(p.pages.len(), 2);
            assert_eq!(p.pages[0].id, crate::keyboard_navigation::PAGE_ID);
            assert!(crate::keyboard_navigation::canonical(&p.pages[0]));
        }
    }
}
