//! Portable macro archives and isolated, reviewable imports. Never execute imported actions
use crate::{
    actions::Action,
    builtins::{BuiltinAction, SavedMacro},
    profiles::{self, ProfileData},
    Runtime, Snapshot,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
    time::{Duration, Instant},
};

const MAX_MACROS: usize = 1000;
const LIFETIME: Duration = Duration::from_secs(3600);
#[cfg(test)]
mod tests;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MacroImportItem {
    pub name: String,
    pub imported_name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct MacroImportPreview {
    pub id: String,
    #[ts(type = "number")]
    pub expected_revision: u64,
    pub macros: Vec<MacroImportItem>,
    pub includes_plugin_settings: bool,
    pub warnings: Vec<String>,
}
pub(crate) struct StagedMacroImport {
    settings_omitted: bool,
    data: ProfileData,
    resources: crate::vendor_resources::PreparedResourceImport,
    revision: u64,
    created: Instant,
}
impl crate::review_store::ReviewSession for StagedMacroImport {
    const LIMIT: usize = 8;
    const FULL: &'static str = "Close an existing macro import before opening another";
    const LIFETIME: Option<std::time::Duration> = Some(LIFETIME);
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
// Strip profile-specific content and reset history for a macro-only export
fn macro_only(mut data: ProfileData) -> ProfileData {
    data.profiles.clear();
    data.active_profile.clear();
    data.styles.clear();
    data.rules.clear();
    data.history = Default::default();
    data.configuration_revision = 1;
    data
}
// Collect saved-macro IDs referenced anywhere in the action tree
fn referenced_macros(action: &Action) -> HashSet<String> {
    let mut ids = HashSet::new();
    action.visit(&mut |action| {
        if let Action::Builtin { action } = action {
            if let BuiltinAction::SavedMacro { macro_id, .. } = &**action {
                ids.insert(macro_id.clone());
            }
        }
    });
    ids
}
// Validate macros
fn validate_macros(data: &ProfileData) -> Result<(), String> {
    profiles::validate_data(data)?;
    if data.saved_macros.is_empty() || data.saved_macros.len() > MAX_MACROS {
        return Err("Choose an archive containing 1–1000 saved macros".into());
    }
    let ids: HashSet<_> = data.saved_macros.iter().map(|m| m.id.as_str()).collect();
    for macro_ in &data.saved_macros {
        if macro_.id.len() > 128 || macro_.name.len() > 640 {
            return Err("Macro identity or name exceeds its size limit".into());
        }
        if referenced_macros(&macro_.action)
            .iter()
            .any(|id| !ids.contains(id.as_str()))
        {
            return Err(format!(
                "“{}” calls a missing saved macro. Include all referenced macros before sharing.",
                macro_.name
            ));
        }
    }
    crate::builtins::validate_macro_graph(&data.saved_macros)
}
// Choose collision-free imported macro names against existing and newly proposed names
fn names(macros: &[SavedMacro], existing: &[SavedMacro]) -> Vec<MacroImportItem> {
    let mut used: HashSet<_> = existing.iter().map(|m| m.name.clone()).collect();
    macros
        .iter()
        .map(|macro_| {
            let mut name = macro_.name.clone();
            let mut suffix = 1;
            while used.contains(&name) {
                name = format!(
                    "{} (imported{})",
                    macro_.name.chars().take(140).collect::<String>(),
                    if suffix == 1 {
                        String::new()
                    } else {
                        format!(" {suffix}")
                    }
                );
                suffix += 1;
            }
            used.insert(name.clone());
            MacroImportItem {
                name: macro_.name.clone(),
                imported_name: name,
            }
        })
        .collect()
}
// Select macro export
pub(crate) fn select_macro_export(
    data: ProfileData,
    ids: &[String],
) -> Result<ProfileData, String> {
    let mut data = macro_only(data);
    let mut needed: HashSet<_> = ids.iter().cloned().collect();
    if needed.is_empty()
        || needed.len() != ids.len()
        || needed
            .iter()
            .any(|id| !data.saved_macros.iter().any(|m| m.id == *id))
    {
        return Err("Choose existing macros to export; the selection may have changed".into());
    }
    loop {
        let before = needed.len();
        for macro_ in &data.saved_macros {
            if needed.contains(&macro_.id) {
                needed.extend(referenced_macros(&macro_.action));
            }
        }
        if needed.len() == before {
            break;
        }
    }
    data.saved_macros.retain(|m| needed.contains(&m.id));
    validate_macros(&data)?;
    Ok(data)
}
impl Runtime {
    // Prepare macro import
    pub fn prepare_macro_import(&self, path: &Path) -> Result<MacroImportPreview, String> {
        // Reuse the bounded, path-checked profile archive reader; only macro data is retained
        let (mut archive, document) = profiles::open_archive(path, "profiles.json")?;
        let mut document = serde_json::from_str(&document).map_err(|e| e.to_string())?;
        crate::vault::detach_portable_references(&mut document);
        let data = macro_only(profiles::parse_data(document)?);
        validate_macros(&data)?;
        let resources = crate::vendor_resources::prepare_import(&data, &mut archive)?;
        let stage = StagedMacroImport {
            settings_omitted: profiles::settings_omitted(&mut archive)?,
            resources,
            data,
            revision: self.snapshot().configuration_revision,
            created: Instant::now(),
        };
        let id = crate::review_store::new_id();
        let preview = self.macro_import_preview(&id, &stage);
        let mut imports = self.macro_imports.lock().unwrap();
        imports.check_capacity()?;
        imports.insert(id, stage)?;
        Ok(preview)
    }
    // Summarize staged macros, settings exposure and compatibility warnings for import review
    fn macro_import_preview(&self, id: &str, stage: &StagedMacroImport) -> MacroImportPreview {
        let snapshot = self.snapshot();
        let mut includes_plugin_settings = false;
        let mut warnings = BTreeSet::new();
        if stage.settings_omitted {
            warnings.extend(profiles::omitted_settings_warnings(
                &stage.data.profiles,
                &stage.data.saved_macros,
            ));
        }
        for macro_ in &stage.data.saved_macros {
            macro_.action.visit(&mut |action| match action {
                Action::Plugin { plugin_id, action_id, settings, resources, ..} => {
                    includes_plugin_settings |= !resources.is_empty() || (!settings.is_null() && settings != &serde_json::json!({}));
                    if !snapshot.plugins.iter().any(|p| p.id == *plugin_id && p.actions.iter().any(|a| a.id == *action_id)) {
                        warnings.insert(format!("Required plugin action {plugin_id} / {action_id} is missing. Configure it before using these macros."));
                    }
                }
                Action::Builtin { action } if matches!(action.as_ref(), BuiltinAction::KeyboardMacro { .. }) => {
                    warnings.insert("Contains recorded keystrokes. Review every keyboard macro before use; playback sends input to another application and recordings may contain sensitive information.".into());
                }
                Action::Profile { profile_id } if !snapshot.profiles.iter().any(|p| p.id == *profile_id) => { warnings.insert("A macro refers to a profile not present on this computer. Review its target before use.".into()); }
                Action::Page { page_id } if !snapshot.profiles.iter().any(|p| p.pages.iter().any(|page| page.id == *page_id)) => { warnings.insert("A macro refers to a page not present on this computer. Review its target before use.".into()); }
                _ => {}
            });
        }
        MacroImportPreview {
            id: id.into(),
            expected_revision: stage.revision,
            macros: names(&stage.data.saved_macros, &snapshot.saved_macros),
            includes_plugin_settings,
            warnings: warnings.into_iter().collect(),
        }
    }
    // Rebase an active macro import review onto the current workspace revision
    pub fn recheck_macro_import(&self, id: &str) -> Result<MacroImportPreview, String> {
        let mut imports = self.macro_imports.lock().unwrap();
        let stage = imports.active_mut(
            id,
            "Macro import session expired",
            "Macro import session expired",
        )?;
        stage.revision = self.snapshot().configuration_revision;
        Ok(self.macro_import_preview(id, stage))
    }
    // Cancel macro import
    pub fn cancel_macro_import(&self, id: &str) {
        self.macro_imports.lock().unwrap().remove(id);
    }
    // Commit macro import
    pub fn commit_macro_import(
        &self,
        id: &str,
        expected: u64,
        include_plugin_settings: bool,
    ) -> Result<Snapshot, String> {
        let mut imports = self.macro_imports.lock().unwrap();
        let stage = imports.active(
            id,
            "Macro import session expired",
            "Macro import session expired",
        )?;
        let mut state = self.state.lock().unwrap();
        if state.configuration_revision != expected || stage.revision != expected {
            return Err("Workspace changed. Recheck this import before committing; your selection has been retained.".into());
        }
        let mut imported = stage.data.clone();
        // Fresh IDs prevent an archive from changing any existing calls, even dangling ones
        let mut used: HashSet<_> = state.saved_macros.iter().map(|m| m.id.clone()).collect();
        let map: HashMap<_, _> = imported
            .saved_macros
            .iter()
            .map(|m| {
                let mut id = uuid::Uuid::new_v4().to_string();
                while !used.insert(id.clone()) {
                    id = uuid::Uuid::new_v4().to_string();
                }
                (m.id.clone(), id)
            })
            .collect();
        let imported_names = names(&imported.saved_macros, &state.saved_macros);
        crate::builtins::remap_macros(&mut imported, &map);
        for (macro_, item) in imported.saved_macros.iter_mut().zip(imported_names) {
            macro_.name = item.imported_name;
            if !include_plugin_settings {
                profiles::redact_plugin_settings(&mut macro_.action);
            }
        }
        validate_macros(&imported)?;
        if include_plugin_settings {
            crate::vendor_resources::publish_import(&self.profiles.root, &stage.resources)?;
        }
        let data = self.profiles.edit(expected, "Import macros", |data| {
            data.saved_macros.extend(imported.saved_macros);
            Ok(())
        })?;
        Self::apply_data(&mut state, data);
        drop(state);
        imports.remove(id);
        drop(imports);
        self.emit("profiles", None, None, None);
        Ok(self.snapshot())
    }
}
