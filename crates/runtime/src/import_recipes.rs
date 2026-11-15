//! Local, explicitly remembered import substitutions. Suggestions never mutate a staged import
use crate::{
    importing::{ImportIssue, ImportRepair, StagedImport},
    Runtime,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_FILE: usize = 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RememberImportRepair {
    pub key: String,
    /// `exact` for any supported identity, or `component_prefix` for paths only.
    pub match_mode: String,
    #[serde(default)]
    pub from_prefix: String,
    #[serde(default)]
    pub to_prefix: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ImportRepairSuggestion {
    pub id: String,
    pub key: String,
    pub label: String,
    pub replacement: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    id: String,
    kind: String,
    context: String,
    source: String,
    target: String,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipes {
    schema_version: u32,
    entries: Vec<Recipe>,
}
// Report the text condition for import recipes
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}
// Report the identity condition for import recipes
fn identity(value: &str) -> bool {
    text(value) && value.len() <= 512 && !value.contains(['/', '\\', ':'])
}
// Report the plugin condition for import recipes
fn plugin(value: &str) -> bool {
    value
        .split_once(':')
        .is_some_and(|(p, a)| identity(p) && identity(a))
}
// Return normalized path
fn normalized_path(value: &str) -> Result<String, String> {
    if !text(value) {
        return Err("Use a nonempty path of at most 4,096 bytes".into());
    }
    let path = value.replace('\\', "/");
    if path.split('/').any(|part| part == "." || part == "..") {
        return Err("Remembered path prefixes cannot contain . or .. components".into());
    }
    Ok(if path == "/" {
        path
    } else {
        path.trim_end_matches('/').into()
    })
}
// Replace only a normalized path prefix that matches on a component boundary
fn prefix_replace(value: &str, from: &str, to: &str) -> Option<String> {
    let value = normalized_path(value).ok()?;
    let from = normalized_path(from).ok()?;
    let to_normal = normalized_path(to).ok()?;
    let suffix = if value == from {
        ""
    } else {
        value.strip_prefix(&format!("{}/", from.trim_end_matches('/')))?
    };
    let result = if suffix.is_empty() {
        to_normal
    } else {
        format!("{}/{}", to_normal.trim_end_matches('/'), suffix)
    };
    Some(if to.contains('\\') && !to.contains('/') {
        result.replace('/', "\\")
    } else {
        result
    })
}
impl Recipe {
    // Validate the supplied import recipes value
    fn validate(&self) -> Result<(), String> {
        if uuid::Uuid::parse_str(&self.id)
            .map_err(|_| "Invalid repair recipe ID")?
            .to_string()
            != self.id
        {
            return Err("Noncanonical repair recipe ID".into());
        }
        let valid = match self.kind.as_str() {
            "path_exact" => text(&self.source) && text(&self.target) && self.context.is_empty(),
            "path_prefix" => {
                normalized_path(&self.source).is_ok()
                    && normalized_path(&self.target).is_ok()
                    && self.context.is_empty()
            }
            "plugin" => plugin(&self.source) && plugin(&self.target) && self.context.is_empty(),
            "model" => identity(&self.source) && identity(&self.target) && self.context.is_empty(),
            "control" => {
                identity(&self.source) && identity(&self.target) && identity(&self.context)
            }
            _ => false,
        };
        if !valid || self.source == self.target {
            return Err("Invalid remembered repair; original file preserved".into());
        }
        Ok(())
    }
    // Apply a repair recipe only to matching issue kinds, values and model context
    fn replacement(&self, issue: &ImportIssue, model: &str) -> Option<String> {
        match (self.kind.as_str(), issue.kind.as_str()) {
            ("path_exact", "path") if self.source == issue.value => Some(self.target.clone()),
            ("path_prefix", "path") => prefix_replace(&issue.value, &self.source, &self.target),
            ("plugin", "plugin") | ("model", "model") if self.source == issue.value => {
                Some(self.target.clone())
            }
            ("control", "control") if self.context == model && self.source == issue.value => {
                Some(self.target.clone())
            }
            _ => None,
        }
    }
}
// Find the model belonging to the imported profile that owns the issue
fn model<'a>(stage: &'a StagedImport, issue: &ImportIssue) -> &'a str {
    stage
        .data
        .profiles
        .iter()
        .find(|p| p.id == issue.profile_id)
        .map(|p| p.model_id.as_str())
        .unwrap_or("")
}

// Validate stored
fn validate_stored(recipes: Recipes) -> Result<Recipes, String> {
    if recipes.schema_version != 1 || recipes.entries.len() > 512 {
        return Err("Unsupported or oversized repair recipes; original file preserved".into());
    }
    let mut ids = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for recipe in &recipes.entries {
        recipe.validate()?;
        if !ids.insert(&recipe.id) || !keys.insert((&recipe.kind, &recipe.context, &recipe.source))
        {
            return Err("Duplicate repair recipes; original file preserved".into());
        }
    }
    Ok(recipes)
}
// Validate storage snapshot
pub(crate) fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    let Some(recipes): Option<Recipes> =
        crate::profile_database::LocalPreference::ImportRepairRecipes
            .snapshot_record()
            .read_from(connection)?
    else {
        return Ok(());
    };
    validate_stored(recipes).map(|_| ())
}
impl Runtime {
    // Read import recipes
    fn read_import_recipes(&self) -> Result<Recipes, String> {
        let recipes: Recipes = self
            .profiles
            .read_preference(crate::profile_database::LocalPreference::ImportRepairRecipes)?
            .unwrap_or(Recipes {
                schema_version: 1,
                entries: vec![],
            });
        validate_stored(recipes)
    }
    // Write import recipes
    fn write_import_recipes(&self, recipes: &Recipes) -> Result<(), String> {
        if recipes.entries.len() > 512
            || serde_json::to_vec(recipes)
                .map_err(|e| e.to_string())?
                .len()
                > MAX_FILE
        {
            return Err("Remember at most 512 repairs / 1 MiB".into());
        }
        self.profiles.write_preference(
            crate::profile_database::LocalPreference::ImportRepairRecipes,
            recipes,
        )
    }
    // Suggest import repairs using the current import recipes state
    pub(crate) fn suggest_import_repairs(
        &self,
        stage: &StagedImport,
        issues: &[ImportIssue],
    ) -> (Vec<ImportRepairSuggestion>, Option<String>) {
        let _guard = self.repair_preferences.lock().unwrap();
        let recipes = match self.read_import_recipes() {
            Ok(recipes) => recipes,
            Err(error) => return (vec![], Some(error)),
        };
        let mut out = vec![];
        for issue in issues {
            let mut matches: Vec<_> = recipes
                .entries
                .iter()
                .filter_map(|r| {
                    r.replacement(issue, model(stage, issue))
                        .map(|value| (r, value))
                })
                .collect();
            matches.sort_by(|(a, _), (b, _)| {
                (a.kind == "path_prefix")
                    .cmp(&(b.kind == "path_prefix"))
                    .then(b.source.len().cmp(&a.source.len()))
                    .then(a.id.cmp(&b.id))
            });
            for (r, replacement) in matches.into_iter().take(8) {
                out.push(ImportRepairSuggestion {
                    id: r.id.clone(),
                    key: issue.key.clone(),
                    label: if r.kind == "path_prefix" {
                        format!("Path prefix: {} → {}", r.source, r.target)
                    } else {
                        format!("Exact match: {} → {}", r.source, r.target)
                    },
                    replacement,
                });
                if out.len() >= 512 {
                    return (out, None);
                }
            }
        }
        (out, None)
    }
    /// Called only after the isolated candidate repair succeeds. Never inspects opaque settings
    pub(crate) fn remember_import_repair(
        &self,
        stage: &StagedImport,
        issue: &ImportIssue,
        repair: &ImportRepair,
        request: &RememberImportRepair,
    ) -> Result<(), String> {
        if request.key != issue.key
            || repair.disable
            || !["path", "plugin", "model", "control"].contains(&issue.kind.as_str())
        {
            return Err("This repair cannot be remembered".into());
        }
        let prefix = request.match_mode == "component_prefix";
        if !["exact", "component_prefix"].contains(&request.match_mode.as_str())
            || (prefix && issue.kind != "path")
        {
            return Err("Unknown repair match mode".into());
        }
        let recipe = Recipe {
            id: uuid::Uuid::new_v4().to_string(),
            kind: if issue.kind == "path" {
                if prefix { "path_prefix" } else { "path_exact" }.into()
            } else {
                issue.kind.clone()
            },
            context: if issue.kind == "control" {
                model(stage, issue).into()
            } else {
                String::new()
            },
            source: if prefix {
                request.from_prefix.clone()
            } else {
                issue.value.clone()
            },
            target: if prefix {
                request.to_prefix.clone()
            } else {
                repair.replacement.clone()
            },
        };
        recipe.validate()?;
        if recipe.replacement(issue, model(stage, issue)).as_deref()
            != Some(repair.replacement.as_str())
        {
            return Err(
                "The component-prefix substitution must exactly produce the reviewed replacement"
                    .into(),
            );
        }
        let _guard = self.repair_preferences.lock().unwrap();
        let mut recipes = self.read_import_recipes()?;
        recipes.entries.retain(|r| {
            (r.kind.as_str(), r.context.as_str(), r.source.as_str())
                != (
                    recipe.kind.as_str(),
                    recipe.context.as_str(),
                    recipe.source.as_str(),
                )
        });
        recipes.entries.push(recipe);
        self.write_import_recipes(&recipes)
    }
    // Discard the selected import recipe state
    pub fn forget_import_recipe(&self, id: &str) -> Result<(), String> {
        let _guard = self.repair_preferences.lock().unwrap();
        let mut recipes = self.read_import_recipes()?;
        recipes.entries.retain(|r| r.id != id);
        self.write_import_recipes(&recipes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that prefixes match components across archive platforms
    fn prefixes_match_components_across_archive_platforms() {
        assert_eq!(
            prefix_replace("/old/folder/a.txt", "/old/folder", "/new"),
            Some("/new/a.txt".into())
        );
        assert_eq!(
            prefix_replace("/old/folder2/a.txt", "/old/folder", "/new"),
            None
        );
        assert_eq!(
            prefix_replace("C:\\Old\\folder\\a.txt", "C:\\Old", "/new"),
            Some("/new/folder/a.txt".into())
        );
        assert_eq!(
            prefix_replace("/old/folder/a.txt", "/old", r"D:\New"),
            Some(r"D:\New\folder\a.txt".into())
        );
        assert_eq!(prefix_replace("/old/../private", "/old", "/new"), None);
        assert_eq!(
            prefix_replace("/file", "/", "/home"),
            Some("/home/file".into())
        );
    }
    // Build a repair-issue fixture with a fixed profile, page and control context
    fn issue(kind: &str, value: &str) -> ImportIssue {
        ImportIssue {
            key: "review-key".into(),
            kind: kind.into(),
            profile_id: "profile".into(),
            page_id: "page".into(),
            control_id: "key-1".into(),
            action_path: vec![],
            choices: vec![],
            message: String::new(),
            value: value.into(),
        }
    }
    #[test]
    // Verify that identity recipes are exact and controls are model scoped
    fn identity_recipes_are_exact_and_controls_are_model_scoped() {
        let mut recipe = Recipe {
            id: uuid::Uuid::new_v4().to_string(),
            kind: "control".into(),
            context: "n4-pro".into(),
            source: "old-key".into(),
            target: "key-1".into(),
        };
        recipe.validate().unwrap();
        assert_eq!(
            recipe.replacement(&issue("control", "old-key"), "mini"),
            None
        );
        assert_eq!(
            recipe.replacement(&issue("control", "old-key"), "n4-pro"),
            Some("key-1".into())
        );
        assert_eq!(
            recipe.replacement(&issue("control", "old-key-2"), "n4-pro"),
            None
        );
        recipe.kind = "plugin".into();
        recipe.context.clear();
        recipe.source = "old.plugin:action".into();
        recipe.target = "new.plugin:action".into();
        recipe.validate().unwrap();
        assert_eq!(
            recipe.replacement(&issue("plugin", "old.plugin:action"), ""),
            Some("new.plugin:action".into())
        );
        assert_eq!(
            recipe.replacement(&issue("plugin", "old.plugin:action.extra"), ""),
            None
        );
        recipe.target = "new.plugin:action:extra".into();
        assert!(recipe.validate().is_err());
    }
    // Construct the import recipes archive
    fn archive(path: &std::path::Path, data: &crate::ProfileData) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        zip.start_file("profiles.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(data).unwrap()).unwrap();
        zip.finish().unwrap();
    }
    #[test]
    #[ignore = "Requires disposable loopback host; no imported action executes"]
    // Verify that remembered paths persist but only suggest on later imports and are not exported
    fn remembered_paths_persist_but_only_suggest_on_later_imports_and_are_not_exported() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let revision = runtime.snapshot().configuration_revision;
        let mut data = runtime.profiles.load().unwrap();
        let old = root.path().join("missing-original");
        let new = root.path().join("replacement");
        std::fs::create_dir(&new).unwrap();
        std::fs::write(new.join("one.txt"), "one").unwrap();
        std::fs::write(new.join("two.txt"), "two").unwrap();
        let binding = data.profiles[0].pages[0].bindings.get_mut("key-1").unwrap();
        binding.action = crate::actions::Action::OpenPath {
            path: old.join("one.txt").to_string_lossy().into(),
        };
        let source = root.path().join("source.zip");
        archive(&source, &data);
        let preview = runtime.prepare_profile_import(&source).unwrap();
        let issue = preview.issues.iter().find(|i| i.kind == "path").unwrap();
        let request = RememberImportRepair {
            key: issue.key.clone(),
            match_mode: "component_prefix".into(),
            from_prefix: old.to_string_lossy().into(),
            to_prefix: new.to_string_lossy().into(),
        };
        let repaired = runtime
            .update_profile_import_remembered(
                &preview.id,
                "merge",
                vec![ImportRepair {
                    key: issue.key.clone(),
                    replacement: new.join("one.txt").to_string_lossy().into(),
                    disable: false,
                }],
                Some(request),
            )
            .unwrap();
        assert!(repaired.issues.iter().all(|i| i.key != issue.key));
        runtime.cancel_profile_import(&preview.id);
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action = crate::actions::Action::OpenPath {
            path: old.join("two.txt").to_string_lossy().into(),
        };
        archive(&source, &data);
        let later = runtime.prepare_profile_import(&source).unwrap();
        assert_eq!(later.repair_suggestions.len(), 1);
        assert!(later.issues.iter().any(|i| i.kind == "path"));
        assert_eq!(
            later.repair_suggestions[0].replacement,
            new.join("two.txt").to_string_lossy()
        );
        let stage = runtime.imports.lock().unwrap();
        let action = &stage[&later.id].data.profiles[0].pages[0].bindings["key-1"].action;
        assert!(
            matches!(action,crate::actions::Action::OpenPath{path} if path.contains("missing-original"))
        );
        drop(stage);
        assert_eq!(runtime.snapshot().configuration_revision, revision);
        let exported = root.path().join("export.zip");
        runtime.export_profiles(&exported).unwrap();
        let zip = zip::ZipArchive::new(std::fs::File::open(exported).unwrap()).unwrap();
        assert!(!zip.file_names().any(|n| n.unwrap().contains("recipe")));
        runtime.shutdown();
        drop(runtime);
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let preview = runtime.prepare_profile_import(&source).unwrap();
        assert_eq!(preview.repair_suggestions.len(), 1);
        runtime
            .forget_import_recipe(&preview.repair_suggestions[0].id)
            .unwrap();
        let preview = runtime
            .update_profile_import(&preview.id, "merge", vec![])
            .unwrap();
        assert!(preview.repair_suggestions.is_empty());
        assert!(preview.issues.iter().any(|i| i.kind == "path"));
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable loopback host; malformed local recipes are preserved"]
    // Verify that invalid recipe storage or prefix does not apply or overwrite a review
    fn invalid_recipe_storage_or_prefix_does_not_apply_or_overwrite_a_review() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let mut data = runtime.profiles.load().unwrap();
        data.profiles[0].pages[0]
            .bindings
            .get_mut("key-1")
            .unwrap()
            .action = crate::actions::Action::OpenPath {
            path: "/missing-original/a.txt".into(),
        };
        let source = root.path().join("source.zip");
        archive(&source, &data);
        let destination = root.path().join("replacement.txt");
        std::fs::write(&destination, "file").unwrap();
        let preview = runtime.prepare_profile_import(&source).unwrap();
        let issue = preview.issues.iter().find(|i| i.kind == "path").unwrap();
        let repair = ImportRepair {
            key: issue.key.clone(),
            replacement: destination.to_string_lossy().into(),
            disable: false,
        };
        let request = RememberImportRepair {
            key: issue.key.clone(),
            match_mode: "component_prefix".into(),
            from_prefix: "/missing-original".into(),
            to_prefix: "/different".into(),
        };
        assert!(runtime
            .update_profile_import_remembered(
                &preview.id,
                "merge",
                vec![repair.clone()],
                Some(request)
            )
            .unwrap_err()
            .contains("exactly produce"));
        runtime
            .profiles
            .write_preference(
                crate::profile_database::LocalPreference::ImportRepairRecipes,
                &serde_json::json!(["invalid original"]),
            )
            .unwrap();
        let rechecked = runtime
            .update_profile_import(&preview.id, "merge", vec![])
            .unwrap();
        assert!(rechecked.recipe_warning.is_some());
        assert!(rechecked.repair_suggestions.is_empty());
        let request = RememberImportRepair {
            key: issue.key.clone(),
            match_mode: "exact".into(),
            from_prefix: String::new(),
            to_prefix: String::new(),
        };
        assert!(runtime
            .update_profile_import_remembered(
                &preview.id,
                "merge",
                vec![repair.clone()],
                Some(request)
            )
            .is_err());
        assert_eq!(
            runtime
                .profiles
                .read_preference::<serde_json::Value>(
                    crate::profile_database::LocalPreference::ImportRepairRecipes
                )
                .unwrap()
                .unwrap(),
            serde_json::json!(["invalid original"])
        );
        assert!(runtime
            .update_profile_import(&preview.id, "merge", vec![])
            .unwrap()
            .issues
            .iter()
            .any(|i| i.key == issue.key));
        // An ordinary reviewed repair remains usable when local recipe suggestions are unavailable
        assert!(!runtime
            .update_profile_import(&preview.id, "merge", vec![repair])
            .unwrap()
            .issues
            .iter()
            .any(|i| i.key == issue.key));
        runtime.shutdown();
    }
    #[test]
    #[ignore = "Requires disposable loopback host; model/control repairs remain staged"]
    // Verify that model then control recipes require separate review and preserve current content
    fn model_then_control_recipes_require_separate_review_and_preserve_current_content() {
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let revision = runtime.snapshot().configuration_revision;
        let mut data = runtime.profiles.load().unwrap();
        let profile = &mut data.profiles[0];
        profile.model_id = "unknown-model".into();
        let mut binding = profile.pages[0].bindings["key-1"].clone();
        binding.action = crate::actions::Action::None;
        profile.pages[0].bindings.clear();
        profile.pages[0].bindings.insert("old-key".into(), binding);
        let source = root.path().join("source.zip");
        archive(&source, &data);
        let preview = runtime.prepare_profile_import(&source).unwrap();
        let repair_one = |preview: &crate::importing::ImportPreview,
                          kind: &str,
                          replacement: &str,
                          remember: bool| {
            let issue = preview.issues.iter().find(|i| i.kind == kind).unwrap();
            runtime
                .update_profile_import_remembered(
                    &preview.id,
                    "merge",
                    vec![ImportRepair {
                        key: issue.key.clone(),
                        replacement: replacement.into(),
                        disable: false,
                    }],
                    remember.then(|| RememberImportRepair {
                        key: issue.key.clone(),
                        match_mode: "exact".into(),
                        from_prefix: String::new(),
                        to_prefix: String::new(),
                    }),
                )
                .unwrap()
        };
        let preview = repair_one(&preview, "model", "n4-pro", true);
        let preview = repair_one(&preview, "control", "key-1", true);
        assert!(preview.issues.is_empty());
        runtime.cancel_profile_import(&preview.id);
        let later = runtime.prepare_profile_import(&source).unwrap();
        assert_eq!(later.repair_suggestions.len(), 1);
        assert_eq!(later.repair_suggestions[0].replacement, "n4-pro");
        assert!(later.issues.iter().any(|i| i.kind == "model"));
        let later = repair_one(&later, "model", "n4-pro", false);
        assert_eq!(later.repair_suggestions.len(), 1);
        assert_eq!(later.repair_suggestions[0].replacement, "key-1");
        assert!(later.issues.iter().any(|i| i.kind == "control"));
        assert_eq!(runtime.snapshot().configuration_revision, revision);
        assert_eq!(runtime.snapshot().profiles[0].model_id, "n4-pro");
        runtime.shutdown();
    }
}
