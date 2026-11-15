//! Bundled suggestions use the profile importer, never execute plugin code, and remain drafts
use crate::plugin_profile_transactions as transaction;
use crate::{
    actions::Action,
    importing::{ImportChoice, ImportPreview},
    profiles::{ProfileData, ProfileStore},
    Runtime, Snapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io::Write,
    path::Path,
};
use stream_dock_manager_plugin_host::{PluginConfiguration, PluginInfo};
const OMIT: &str = "@omit";
const NEW: &str = "@new";
/// Schema for package-authored JSON; import-only fields never enter ProfileData
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
pub struct PluginProfileDocument {
    #[serde(flatten)]
    pub profile: ProfileData,
    #[serde(default)]
    pub plugin_configuration: Option<PluginConfiguration>,
    #[serde(default)]
    pub unavailable_actions: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginProfileSuggestion {
    pub key: String,
    pub name: String,
    pub model_id: String,
    pub page_count: usize,
    pub binding_count: usize,
    pub unavailable_actions: Vec<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginProfileCatalog {
    pub plugin_id: String,
    pub name: String,
    pub fingerprint: String,
    pub can_configure: bool,
    pub suggestions: Vec<PluginProfileSuggestion>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginPageDestination {
    pub source_page: String,
    pub target_page: String,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginBindingMapping {
    pub source_page: String,
    pub source_control: String,
    pub target_control: String,
    pub replace: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginProfilePlacement {
    pub target_profile: String,
    pub target_model: String,
    pub profile_name: String,
    pub pages: Vec<PluginPageDestination>,
    pub bindings: Vec<PluginBindingMapping>,
    pub configuration_choices: Vec<Vec<String>>,
    pub include_configuration: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginImportRow {
    pub source_page: String,
    pub source_control: String,
    pub title: String,
    pub target_control: String,
    pub replace: bool,
    pub occupied: bool,
    pub choices: Vec<ImportChoice>,
    pub problem: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginConfigurationField {
    pub path: Vec<String>,
    pub current: Option<Value>,
    pub suggested: Value,
    pub use_suggested: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct PluginProfilePreview {
    pub plugin_id: String,
    pub placement: PluginProfilePlacement,
    pub rows: Vec<PluginImportRow>,
    pub configuration: Vec<PluginConfigurationField>,
    pub ready: bool,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ConfigurationChange {
    path: Vec<String>,
    before: Option<Value>,
    after: Option<Value>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct ConfigurationHistory {
    plugin_id: String,
    generation: u64,
    changes: Vec<ConfigurationChange>,
}
pub(crate) struct StagedPluginProfile {
    source: ProfileData,
    store: ProfileStore,
    catalog: PluginProfileCatalog,
    installation: Option<String>,
    placement: PluginProfilePlacement,
    target_device: Option<String>,
    configuration: Option<PluginConfiguration>,
    base: PluginConfiguration,
    revision: u64,
    profile_id: String,
    page_ids: BTreeMap<String, String>,
    macro_ids: BTreeMap<String, String>,
    style_ids: BTreeMap<String, String>,
    created: std::time::Instant,
    _directory: tempfile::TempDir,
}
impl crate::review_store::ReviewSession for StagedPluginProfile {
    const LIMIT: usize = 8;
    const FULL: &'static str = "Close an existing import first";
    const LIFETIME: Option<std::time::Duration> = Some(std::time::Duration::from_secs(3600));
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
// Reject symlinks and non-directories before reading suggested profile files
fn regular_directory(path: &Path) -> Result<(), String> {
    let m = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err("Suggested profiles require a regular package directory".into());
    }
    Ok(())
}
// Read bundled profile media with validated names and bounded file count and total bytes
fn media(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut out = BTreeMap::new();
    let base = root.join("profiles");
    if !base.exists() {
        return Ok(out);
    }
    regular_directory(&base)?;
    let aliases = base.join("asset-aliases.json");
    if aliases.exists() {
        out.insert(
            "asset-aliases.json".into(),
            transaction::read(&aliases, 65536)?,
        );
    }
    let assets = base.join("assets");
    if assets.exists() {
        regular_directory(&assets)?;
        let mut total = 0usize;
        for entry in std::fs::read_dir(assets).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let name = path
                .file_name()
                .and_then(|v| v.to_str())
                .ok_or("Invalid profile asset name")?;
            let (id, ext) = name.rsplit_once('.').ok_or("Invalid profile asset")?;
            crate::profiles::validate_asset_id(id)?;
            if !crate::media::ASSET_EXTENSIONS.contains(&ext) {
                return Err("Unsupported bundled profile asset".into());
            }
            let bytes = transaction::read(&path, crate::media::MAX_ASSET)?;
            total += bytes.len();
            if total > 80 * 1024 * 1024 || out.len() >= 250 {
                return Err("Bundled profile assets exceed import limits".into());
            }
            out.insert(format!("assets/{name}"), bytes);
        }
    }
    Ok(out)
}
// Read a bounded suggested-profile file and validate its decoded document
fn document(
    root: &Path,
    key: &str,
    plugin: &PluginInfo,
) -> Result<(ProfileData, Option<PluginConfiguration>, Vec<String>), String> {
    document_bytes(
        &transaction::read(&root.join(key), 16 * 1024 * 1024)?,
        key,
        plugin,
    )
}
// Validate one suggested profile, its hardware bindings and ownership of plugin actions
fn document_bytes(
    bytes: &[u8],
    key: &str,
    plugin: &PluginInfo,
) -> Result<(ProfileData, Option<PluginConfiguration>, Vec<String>), String> {
    let mut value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or("Profile document must be an object")?;
    let configuration: Option<PluginConfiguration> = object
        .remove("plugin_configuration")
        .filter(|value| !value.is_null())
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| e.to_string())?;
    let unavailable = object
        .remove("unavailable_actions")
        .map(serde_json::from_value::<Vec<String>>)
        .transpose()
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    let mut data = crate::profiles::parse_data(value)?;
    if data.schema_version != 1 || data.profiles.len() != 1 {
        return Err("A suggested layout must contain exactly one schema-v1 profile".into());
    }
    data.history = Default::default();
    data.rules.clear();
    crate::profiles::validate_data(&data)?;
    let profile = &data.profiles[0];
    if key != format!("profiles/{}.json", profile.model_id) {
        return Err("Profile filename must match its hardware model ID".into());
    }
    let model = hardware_registry::manifests()
        .into_iter()
        .find(|m| m.model_id == profile.model_id)
        .ok_or("Unsupported suggested hardware model")?;
    let mut ownership = Ok(());
    for action in profile
        .pages
        .iter()
        .flat_map(|p| p.bindings.values())
        .map(|b| &b.action)
        .chain(data.saved_macros.iter().map(|m| &m.action))
    {
        action.visit(&mut |a| {
            if let Action::Plugin {
                plugin_id,
                action_id,
                ..
            } = a
            {
                if plugin_id != &plugin.id || !plugin.actions.iter().any(|a| a.id == *action_id) {
                    ownership = Err("Suggested actions must belong to this plugin".to_string());
                }
            }
        });
    }
    ownership?;
    for page in &profile.pages {
        for (control, binding) in &page.bindings {
            let control = model.control(control).map_err(|e| e.to_string())?;
            crate::profile_conversion::check_action(
                &binding.action,
                control,
                &data.saved_macros,
                std::slice::from_ref(plugin),
                0,
                &model,
            )?;
        }
    }
    if let Some(config) = &configuration {
        if serde_json::to_vec(config).map_err(|e| e.to_string())?.len() > 256 * 1024
            || config
                .action_defaults
                .iter()
                .any(|(id, v)| !v.is_object() || !plugin.actions.iter().any(|a| a.id == *id))
        {
            return Err("Invalid suggested plugin configuration".into());
        }
    }
    Ok((data, configuration, unavailable))
}
// Collect a bounded set of suggested-profile JSON paths while excluding asset aliases
fn suggestion_keys(names: impl Iterator<Item = String>) -> BTreeMap<String, ()> {
    names
        .filter(|name| name.ends_with(".json") && name != "asset-aliases.json")
        .take(65)
        .map(|name| (format!("profiles/{name}"), ()))
        .collect()
}
// Discover available plugin profiles candidates
pub(crate) fn discover(root: &Path, plugin: &PluginInfo) -> PluginProfileCatalog {
    let mut catalog = PluginProfileCatalog {
        plugin_id: plugin.id.clone(),
        name: plugin.name.clone(),
        fingerprint: String::new(),
        can_configure: plugin
            .capabilities
            .as_ref()
            .is_some_and(|c| c.plugin_settings.is_some()),
        suggestions: vec![],
        warnings: vec![],
    };
    let mut files = BTreeMap::new();
    let profiles = root.join("profiles");
    if profiles.exists() {
        match regular_directory(&profiles)
            .and_then(|_| std::fs::read_dir(&profiles).map_err(|e| e.to_string()))
        {
            Ok(entries) => {
                files.extend(suggestion_keys(entries.filter_map(|entry| match entry {
                    Ok(entry) => Some(entry.file_name().to_string_lossy().to_string()),
                    Err(error) => {
                        catalog.warnings.push(error.to_string());
                        None
                    }
                })));
            }
            Err(e) => catalog.warnings.push(e),
        }
    }
    if files.len() > 64 {
        catalog.warnings.push("Too many suggested profiles".into());
        files.clear();
    }
    let assets = media(root);
    let mut digest = Sha256::new();
    match &assets {
        Ok(assets) => {
            for (name, bytes) in assets {
                digest.update(name.as_bytes());
                digest.update(Sha256::digest(bytes));
            }
        }
        Err(e) => {
            digest.update(e.as_bytes());
            catalog.warnings.push(e.clone());
        }
    }
    for key in files.keys() {
        digest.update(key.as_bytes());
        let bytes = transaction::read(&root.join(key), 16 * 1024 * 1024);
        match &bytes {
            Ok(bytes) => digest.update(Sha256::digest(bytes)),
            Err(e) => digest.update(e.as_bytes()),
        }
        let mut suggestion = PluginProfileSuggestion {
            key: key.clone(),
            name: key.clone(),
            model_id: String::new(),
            page_count: 0,
            binding_count: 0,
            unavailable_actions: vec![],
            error: None,
        };
        match bytes
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|bytes| document_bytes(bytes, key, plugin))
            .and_then(|document| {
                let available = assets.as_ref().map_err(Clone::clone)?;
                let aliases: BTreeMap<String, String> = available
                    .get("asset-aliases.json")
                    .map(|b| serde_json::from_slice(b))
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .unwrap_or_default();
                for id in crate::profiles::referenced_assets(&document.0) {
                    let name = aliases.get(&id).cloned().unwrap_or_else(|| {
                        if available.contains_key(&format!("assets/{id}.sdmedia")) {
                            format!("assets/{id}.sdmedia")
                        } else {
                            format!("assets/{id}.png")
                        }
                    });
                    if !available.contains_key(&name) {
                        return Err(format!("Bundled profile asset {id} is missing"));
                    }
                }
                Ok(document)
            }) {
            Ok((data, _, unavailable)) => {
                let p = &data.profiles[0];
                suggestion.name = p.name.clone();
                suggestion.model_id = p.model_id.clone();
                suggestion.page_count = p.pages.len();
                suggestion.binding_count = p.pages.iter().map(|p| p.bindings.len()).sum();
                suggestion.unavailable_actions = unavailable;
            }
            Err(e) => {
                catalog.warnings.push(format!("{key}: {e}"));
                suggestion.error = Some(e);
            }
        }
        catalog.suggestions.push(suggestion);
    }
    catalog.suggestions.sort_by_key(|s| s.key.clone());
    catalog.fingerprint = hex::encode(digest.finalize());
    catalog
}
// Follow the supplied object-key path into a JSON value
fn at<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(value, |v, k| v.get(k))
}
// Store the selected plugin profiles value
fn put(value: &mut Value, path: &[String], replacement: Option<Value>) -> Result<(), String> {
    let (key, parents) = path.split_last().ok_or("Empty configuration path")?;
    let mut node = value;
    for parent in parents {
        node = node
            .as_object_mut()
            .ok_or("Invalid configuration path")?
            .entry(parent.clone())
            .or_insert_with(|| json!({}));
    }
    let object = node.as_object_mut().ok_or("Invalid configuration object")?;
    if let Some(value) = replacement {
        object.insert(key.clone(), value);
    } else {
        object.remove(key);
    }
    Ok(())
}
// Collect changed suggested fields recursively and default new fields to selected
fn diff(
    current: Option<&Value>,
    suggested: &Value,
    path: Vec<String>,
    out: &mut Vec<PluginConfigurationField>,
) {
    if let (Some(Value::Object(old)), Value::Object(new)) = (current, suggested) {
        for (key, value) in new {
            let mut child = path.clone();
            child.push(key.clone());
            diff(old.get(key), value, child, out);
        }
    } else if current != Some(suggested) {
        out.push(PluginConfigurationField {
            path,
            current: current.cloned(),
            suggested: suggested.clone(),
            use_suggested: current.is_none(),
        });
    }
}
// Build configuration review rows with the user’s current field selections
fn fields(stage: &StagedPluginProfile) -> Vec<PluginConfigurationField> {
    let mut out = vec![];
    if let Some(config) = &stage.configuration {
        let base = serde_json::to_value(&stage.base).unwrap();
        let proposed = serde_json::to_value(config).unwrap();
        diff(Some(&base), &proposed, vec![], &mut out);
        for row in &mut out {
            row.use_suggested = stage.placement.configuration_choices.contains(&row.path);
        }
    }
    out
}
// Apply reviewed configuration changes for undo or redo only when expected values still match
fn configuration_after(
    current: &PluginConfiguration,
    changes: &[ConfigurationChange],
    redo: bool,
) -> Result<PluginConfiguration, String> {
    let mut value = serde_json::to_value(current).map_err(|e| e.to_string())?;
    for change in changes {
        let (expected, next) = if redo {
            (&change.before, &change.after)
        } else {
            (&change.after, &change.before)
        };
        if at(&value, &change.path) != expected.as_ref() {
            return Err("Plugin settings changed after review. Review the import or resolve settings changes before Undo/Redo.".into());
        }
        put(&mut value, &change.path, next.clone())?;
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}
// Remap identifiers in plugin profiles
fn remap(
    action: &mut Action,
    source_page: &str,
    stage: &StagedPluginProfile,
    pages: &BTreeMap<String, String>,
    retained: &HashSet<(String, String)>,
) -> Result<(), String> {
    use crate::builtins::BuiltinAction as B;
    let control = |page: &str, id: &mut String| -> Result<(), String> {
        let targets: HashSet<_> = stage
            .placement
            .bindings
            .iter()
            .filter(|m| m.source_control == *id && (page.is_empty() || m.source_page == page))
            .map(|m| {
                if retained.contains(&(m.source_page.clone(), m.source_control.clone())) {
                    m.target_control.as_str()
                } else {
                    OMIT
                }
            })
            .collect();
        if targets.len() != 1 || targets.contains(OMIT) || targets.contains("") {
            return Err(format!(
                "Internal reference to {id} needs one retained control mapping"
            ));
        }
        *id = targets.into_iter().next().unwrap().into();
        Ok(())
    };
    match action {
        Action::Profile { profile_id } if *profile_id == stage.source.profiles[0].id => {
            *profile_id = stage.profile_id.clone()
        }
        Action::Page { page_id } => {
            if let Some(id) = pages.get(page_id) {
                *page_id = id.clone();
            }
        }
        Action::Builtin { action } => match &mut **action {
            B::MomentaryPage { page_id } => {
                if let Some(id) = pages.get(page_id) {
                    *page_id = id.clone();
                }
            }
            B::BindingLayer {
                page_id, controls, ..
            } => {
                if let Some(id) = pages.get(page_id) {
                    for c in controls {
                        control(page_id, c)?;
                    }
                    *page_id = id.clone();
                }
            }
            B::TriggerBinding {
                device_id,
                profile_id,
                page_id,
                control_id,
            }
            | B::EditBinding {
                device_id,
                profile_id,
                page_id,
                control_id,
            } if device_id.is_empty()
                && (profile_id.is_empty() || *profile_id == stage.source.profiles[0].id) =>
            {
                control(
                    if page_id.is_empty() {
                        source_page
                    } else {
                        page_id
                    },
                    control_id,
                )?;
                if !profile_id.is_empty() {
                    *profile_id = stage.profile_id.clone();
                }
                if let Some(id) = pages.get(page_id) {
                    *page_id = id.clone();
                }
            }
            B::SavedMacro { macro_id, .. } => {
                if let Some(id) = stage.macro_ids.get(macro_id) {
                    *macro_id = id.clone();
                }
            }
            _ => {}
        },
        _ => {}
    }
    if let Action::Toggle { on, off } = action {
        remap(on, source_page, stage, pages, retained)?;
        remap(off, source_page, stage, pages, retained)?;
    } else {
        for child in action.children_mut() {
            remap(child, source_page, stage, pages, retained)?;
        }
    }
    Ok(())
}
impl Runtime {
    // List plugin profiles
    pub fn list_plugin_profiles(&self, id: &str) -> Result<PluginProfileCatalog, String> {
        let (directory, info) = {
            let host = self.plugins.lock().unwrap();
            let info = host
                .list()
                .into_iter()
                .find(|p| p.id == id)
                .ok_or("Plugin is no longer installed")?;
            (host.plugin_directory(id)?, info)
        };
        // Package I/O and profile/asset validation must not block plugin dispatch
        Ok(discover(&directory, &info))
    }
    // Persist acknowledgement only if the suggested-profile catalog fingerprint is unchanged
    pub fn acknowledge_plugin_profile_setup(
        &self,
        id: &str,
        fingerprint: &str,
    ) -> Result<(), String> {
        let _guard = self.plugin_preferences.lock().unwrap();
        let catalog = self.list_plugin_profiles(id)?;
        if catalog.fingerprint != fingerprint {
            return Err("Suggested profiles changed; reopen setup".into());
        }
        let mut receipts: BTreeMap<String, String> = self
            .profiles
            .read_preference(crate::profile_database::LocalPreference::PluginProfilePrompts)?
            .unwrap_or_default();
        receipts.insert(id.into(), fingerprint.into());
        self.profiles.write_preference(
            crate::profile_database::LocalPreference::PluginProfilePrompts,
            &receipts,
        )
    }
    // Clear plugin profile acknowledgement
    pub(crate) fn clear_plugin_profile_acknowledgement(&self, id: &str) -> Result<(), String> {
        let _guard = self.plugin_preferences.lock().unwrap();
        let mut receipts: BTreeMap<String, String> = self
            .profiles
            .read_preference(crate::profile_database::LocalPreference::PluginProfilePrompts)?
            .unwrap_or_default();
        if receipts.remove(id).is_some() {
            self.profiles.write_preference(
                crate::profile_database::LocalPreference::PluginProfilePrompts,
                &receipts,
            )?;
        }
        Ok(())
    }
    // List actionable unacknowledged setup catalogs while skipping removed and duplicate plugins
    pub fn pending_plugin_profile_setup(
        &self,
        ids: Vec<String>,
    ) -> Result<Vec<PluginProfileCatalog>, String> {
        if ids.len() > 100 {
            return Err("Too many setup candidates".into());
        }
        let receipts: BTreeMap<String, String> = self
            .profiles
            .read_preference(crate::profile_database::LocalPreference::PluginProfilePrompts)?
            .unwrap_or_default();
        let mut out = vec![];
        let mut seen = HashSet::new();
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            // A candidate can disappear after the frontend's installed snapshot
            let Ok(catalog) = self.list_plugin_profiles(&id) else {
                continue;
            };
            if (!catalog.suggestions.is_empty() || catalog.can_configure)
                && receipts.get(&id) != Some(&catalog.fingerprint)
            {
                out.push(catalog);
            }
        }
        Ok(out)
    }
    // Prepare plugin profile import
    pub fn prepare_plugin_profile_import(
        &self,
        id: &str,
        key: &str,
        target_profile: Option<String>,
        target_page: Option<String>,
    ) -> Result<ImportPreview, String> {
        // Preserve explicit offline import callers while device-aware UI requests resolve their chosen device
        let target_profile = target_profile.or_else(|| {
            let snapshot = self.snapshot();
            snapshot
                .profiles
                .iter()
                .find(|profile| profile.id == snapshot.active_profile)
                .map(|profile| profile.id.clone())
        });
        self.prepare_plugin_profile_import_for_device(id, key, target_profile, target_page, None)
    }
    // Stage suggestions for the explicitly selected device without persisting a new profile
    pub fn prepare_plugin_profile_import_for_device(
        &self,
        id: &str,
        key: &str,
        target_profile: Option<String>,
        target_page: Option<String>,
        target_device: Option<String>,
    ) -> Result<ImportPreview, String> {
        let snapshot = self.snapshot();
        let device = if target_device.is_some() || target_profile.is_none() {
            Some(crate::assignment_target::connected_device(
                &snapshot,
                target_device.as_deref(),
            )?)
        } else {
            None
        };
        let (root, info, base, installation) = {
            let host = self.plugins.lock().unwrap();
            (
                host.plugin_directory(id)?,
                host.list()
                    .into_iter()
                    .find(|p| p.id == id)
                    .ok_or("Plugin is no longer installed")?,
                host.configuration(id)?,
                host.installation_token(id),
            )
        };
        let catalog = discover(&root, &info);
        if !catalog
            .suggestions
            .iter()
            .any(|s| s.key == key && s.error.is_none())
        {
            return Err("Choose a valid bundled suggested profile".into());
        }
        let (source, configuration, _) = document(&root, key, &info)?;
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let archive = directory.path().join("profile.zip");
        let mut writer =
            zip::ZipWriter::new(std::fs::File::create(&archive).map_err(|e| e.to_string())?);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6));
        writer
            .start_file("profiles.json", options)
            .map_err(|e| e.to_string())?;
        writer
            .write_all(&serde_json::to_vec(&source).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        for (name, bytes) in media(&root)? {
            writer
                .start_file(name, options)
                .map_err(|e| e.to_string())?;
            writer.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        writer.finish().map_err(|e| e.to_string())?;
        let store = ProfileStore::open(directory.path().join("staged"))?;
        let mut source = store.import_for_repair(&archive)?;
        for asset in crate::profiles::referenced_assets(&source) {
            let bytes = store.asset(&asset)?;
            let ext = store.asset_extension(&asset)?;
            let fresh = uuid::Uuid::new_v4().to_string();
            std::fs::write(store.root.join(format!("assets/{fresh}.{ext}")), bytes)
                .map_err(|e| e.to_string())?;
            for profile in &mut source.profiles {
                if profile.background_asset.as_deref() == Some(&asset) {
                    profile.background_asset = Some(fresh.clone());
                }
                for binding in profile
                    .pages
                    .iter_mut()
                    .flat_map(|p| p.bindings.values_mut())
                {
                    if binding.asset.as_deref() == Some(&asset) {
                        binding.asset = Some(fresh.clone());
                    }
                }
            }
        }
        for asset in crate::profiles::referenced_assets(&source) {
            store.asset(&asset)?;
        }
        let target = if let Some(id) = target_profile.as_ref() {
            let profile = snapshot
                .profiles
                .iter()
                .find(|p| p.id == *id)
                .ok_or("Destination profile no longer exists")?;
            if device.is_some_and(|device| device.model_id != profile.model_id) {
                return Err(
                    "Destination profile does not belong to the selected device model".into(),
                );
            }
            Some(profile)
        } else if let Some(device) = device {
            let session = snapshot.session(&device.id);
            snapshot
                .profiles
                .iter()
                .find(|p| p.id == session.profile_id && p.model_id == device.model_id)
                .or_else(|| {
                    snapshot
                        .profiles
                        .iter()
                        .find(|p| p.model_id == device.model_id)
                })
        } else {
            snapshot
                .profiles
                .iter()
                .find(|p| p.id == snapshot.active_profile)
        };
        if let Some(page_id) = target_page.as_deref() {
            if !target.is_some_and(|profile| profile.pages.iter().any(|page| page.id == page_id)) {
                return Err("Destination page no longer exists".into());
            }
        }
        let profile = &source.profiles[0];
        let mut placement = PluginProfilePlacement {
            target_profile: target.map(|p| p.id.clone()).unwrap_or_else(|| NEW.into()),
            target_model: target.map(|p| p.model_id.clone()).unwrap_or_else(|| {
                device
                    .map(|d| d.model_id.clone())
                    .unwrap_or_else(|| profile.model_id.clone())
            }),
            profile_name: if target.is_none() {
                crate::profiles::DEFAULT_PROFILE_NAME.into()
            } else {
                profile.name.clone()
            },
            pages: vec![],
            bindings: vec![],
            configuration_choices: vec![],
            include_configuration: true,
        };
        for (i, page) in profile.pages.iter().enumerate() {
            placement.pages.push(PluginPageDestination {
                source_page: page.id.clone(),
                target_page: if i == 0 {
                    target
                        .and_then(|p| {
                            p.pages
                                .iter()
                                .find(|p| Some(&p.id) == target_page.as_ref())
                                .or_else(|| p.pages.iter().find(|page| page.id == p.active_page))
                        })
                        .map(|p| p.id.clone())
                        .unwrap_or_else(|| NEW.into())
                } else {
                    NEW.into()
                },
                name: page.name.clone(),
            });
            for control in page.bindings.keys() {
                placement.bindings.push(PluginBindingMapping {
                    source_page: page.id.clone(),
                    source_control: control.clone(),
                    target_control: control.clone(),
                    replace: false,
                });
            }
        }
        let mut stage = StagedPluginProfile {
            source,
            store,
            catalog,
            installation,
            placement,
            target_device: device.map(|device| device.id.clone()),
            configuration,
            base,
            revision: snapshot.configuration_revision,
            profile_id: target
                .map(|p| p.id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            page_ids: BTreeMap::new(),
            macro_ids: BTreeMap::new(),
            style_ids: BTreeMap::new(),
            created: std::time::Instant::now(),
            _directory: directory,
        };
        for page in &stage.source.profiles[0].pages {
            stage
                .page_ids
                .insert(page.id.clone(), uuid::Uuid::new_v4().to_string());
        }
        for m in &stage.source.saved_macros {
            stage
                .macro_ids
                .insert(m.id.clone(), uuid::Uuid::new_v4().to_string());
        }
        for s in &stage.source.styles {
            stage
                .style_ids
                .insert(s.id.clone(), uuid::Uuid::new_v4().to_string());
        }
        if let Some(config) = &stage.configuration {
            let mut rows = vec![];
            diff(
                Some(&serde_json::to_value(&stage.base).unwrap()),
                &serde_json::to_value(config).unwrap(),
                vec![],
                &mut rows,
            );
            stage.placement.configuration_choices = rows
                .into_iter()
                .filter(|r| r.current.is_none())
                .map(|r| r.path)
                .collect();
        }
        let session = crate::review_store::new_id();
        let report = self.plugin_profile_preview(&session, &stage)?;
        let mut imports = self.plugin_profile_imports.lock().unwrap();
        imports.check_capacity()?;
        imports.insert(session, stage)?;
        Ok(report)
    }
    // Build the proposed profile import and report placement, parameter and validation conflicts
    fn plugin_profile_candidate(
        &self,
        stage: &StagedPluginProfile,
    ) -> Result<(ProfileData, Vec<PluginImportRow>, Vec<String>), String> {
        let mut candidate = self.profiles.load()?;
        let snapshot = self.snapshot();
        let mut problems = vec![];
        let mut rows = vec![];
        let source = &stage.source.profiles[0];
        let placement = &stage.placement;
        if let Some(id) = stage.target_device.as_deref() {
            let device = crate::assignment_target::connected_device(&snapshot, Some(id))?;
            if placement.target_model != device.model_id {
                return Err("Import target model must match the selected device".into());
            }
            if placement.target_profile != NEW
                && candidate
                    .profiles
                    .iter()
                    .find(|profile| profile.id == placement.target_profile)
                    .is_some_and(|profile| profile.model_id != device.model_id)
            {
                return Err(
                    "Destination profile does not belong to the selected device model".into(),
                );
            }
        }
        let mut profile = if placement.target_profile == NEW {
            let mut p = source.clone();
            p.id = stage.profile_id.clone();
            p.name = placement.profile_name.trim().into();
            p.model_id = placement.target_model.clone();
            p.pages.clear();
            p
        } else {
            candidate
                .profiles
                .iter()
                .find(|p| p.id == placement.target_profile)
                .cloned()
                .ok_or("Destination profile no longer exists")?
        };
        let models = hardware_registry::manifests();
        let model = models
            .iter()
            .find(|m| m.model_id == profile.model_id)
            .ok_or("Choose a supported target hardware model")?;
        let mut pages = BTreeMap::new();
        if placement.pages.len() != source.pages.len()
            || placement.bindings.len()
                != source.pages.iter().map(|p| p.bindings.len()).sum::<usize>()
        {
            return Err("Import placement must include every source page and binding".into());
        }
        for page in &source.pages {
            let dest = placement
                .pages
                .iter()
                .filter(|p| p.source_page == page.id)
                .collect::<Vec<_>>();
            if dest.len() != 1 {
                return Err("Duplicate or missing source page mapping".into());
            }
            let dest = dest[0];
            let id = if dest.target_page == NEW {
                let id = stage.page_ids[&page.id].clone();
                profile.pages.push(crate::Page {
                    id: id.clone(),
                    name: dest.name.trim().into(),
                    bindings: BTreeMap::new(),
                });
                id
            } else {
                profile
                    .pages
                    .iter()
                    .find(|p| p.id == dest.target_page)
                    .ok_or("Choose a page in the destination profile")?
                    .id
                    .clone()
            };
            pages.insert(page.id.clone(), id);
        }
        if placement.target_profile == NEW {
            profile.active_page = pages
                .get(&source.active_page)
                .cloned()
                .ok_or("Source active page missing")?;
        }
        // References may only target retained imported bindings, never an unrelated
        // local action kept at an overlapping control
        let retained = placement
            .bindings
            .iter()
            .filter(|mapping| {
                mapping.target_control != OMIT
                    && pages
                        .get(&mapping.source_page)
                        .and_then(|id| profile.pages.iter().find(|page| page.id == *id))
                        .is_some_and(|page| {
                            mapping.replace || !page.bindings.contains_key(&mapping.target_control)
                        })
            })
            .map(|mapping| (mapping.source_page.clone(), mapping.source_control.clone()))
            .collect::<HashSet<_>>();
        let mut imported = stage.source.clone();
        crate::builtins::remap_macros(
            &mut imported,
            &stage
                .macro_ids
                .iter()
                .map(|(a, b)| (a.clone(), b.clone()))
                .collect(),
        );
        for m in &mut imported.saved_macros {
            if let Err(e) = remap(&mut m.action, "", stage, &pages, &retained) {
                problems.push(e);
            }
        }
        let mut used = HashSet::new();
        for source_page in &source.pages {
            for (control, binding) in &source_page.bindings {
                let mappings: Vec<_> = placement
                    .bindings
                    .iter()
                    .filter(|m| m.source_page == source_page.id && m.source_control == *control)
                    .collect();
                if mappings.len() != 1 {
                    return Err("Duplicate or missing binding mapping".into());
                }
                let mapping = mappings[0];
                let choices = model
                    .controls
                    .iter()
                    .filter(|c| {
                        crate::profile_conversion::check_action(
                            &binding.action,
                            c,
                            &stage.source.saved_macros,
                            &snapshot.plugins,
                            0,
                            model,
                        )
                        .is_ok()
                    })
                    .map(|c| ImportChoice {
                        value: c.id.clone(),
                        label: c.label.clone(),
                    })
                    .collect::<Vec<_>>();
                let destination = profile
                    .pages
                    .iter_mut()
                    .find(|p| p.id == pages[&source_page.id])
                    .unwrap();
                let occupied = destination.bindings.contains_key(&mapping.target_control);
                let mut row = PluginImportRow {
                    source_page: source_page.id.clone(),
                    source_control: control.clone(),
                    title: binding.title.clone(),
                    target_control: mapping.target_control.clone(),
                    replace: mapping.replace,
                    occupied,
                    choices,
                    problem: None,
                };
                if mapping.target_control != OMIT {
                    if !row
                        .choices
                        .iter()
                        .any(|c| c.value == mapping.target_control)
                    {
                        row.problem =
                            Some("Choose a compatible destination or skip this binding".into());
                    } else if !used.insert((destination.id.clone(), mapping.target_control.clone()))
                    {
                        row.problem = Some("Two suggested bindings use this destination".into());
                    } else if occupied && !mapping.replace { /* Existing bindings are kept unless replacement is explicit. */
                    } else {
                        let mut binding = binding.clone();
                        if let Err(e) = remap(
                            &mut binding.action,
                            &source_page.id,
                            stage,
                            &pages,
                            &retained,
                        ) {
                            row.problem = Some(e);
                        } else {
                            destination
                                .bindings
                                .insert(mapping.target_control.clone(), binding);
                        }
                    }
                }
                if let Some(problem) = &row.problem {
                    problems.push(format!("{} / {}: {problem}", source_page.name, control));
                }
                rows.push(row);
            }
        }
        for parameter in &source.parameters {
            if let Some(old) = profile.parameters.iter().find(|p| p.name == parameter.name) {
                if serde_json::to_value(old).unwrap() != serde_json::to_value(parameter).unwrap() {
                    problems.push(format!(
                        "Profile parameter {} conflicts; choose a new profile",
                        parameter.name
                    ));
                }
            } else {
                profile.parameters.push(parameter.clone());
            }
        }
        if let Some(index) = candidate.profiles.iter().position(|p| p.id == profile.id) {
            candidate.profiles[index] = profile;
        } else {
            if candidate.profiles.is_empty() {
                candidate.active_profile = profile.id.clone();
            }
            candidate.profiles.push(profile);
        }
        candidate.saved_macros.extend(imported.saved_macros);
        for mut style in imported.styles {
            style.id = stage.style_ids[&style.id].clone();
            candidate.styles.push(style);
        }
        if let Err(e) = crate::profiles::validate_data(&candidate) {
            problems.push(e);
        }
        Ok((candidate, rows, problems))
    }
    // Preview plugin profile changes before applying them
    fn plugin_profile_preview(
        &self,
        id: &str,
        stage: &StagedPluginProfile,
    ) -> Result<ImportPreview, String> {
        let (_, rows, problems) = self.plugin_profile_candidate(stage)?;
        let overlaps = rows
            .iter()
            .filter(|r| r.occupied && r.target_control != OMIT)
            .count();
        let mut impact = vec![format!(
            "Import suggested pages and configuration for {}. The complete import can be undone.",
            stage.catalog.name
        )];
        if overlaps > 0 {
            impact.push(format!("{overlaps} controls overlap. Existing bindings are kept unless replacement is selected. Consider creating a new page."));
        }
        impact.extend(problems.iter().cloned());
        Ok(ImportPreview {
            plugin: Some(PluginProfilePreview {
                plugin_id: stage.catalog.plugin_id.clone(),
                placement: stage.placement.clone(),
                rows,
                configuration: fields(stage),
                ready: problems.is_empty(),
            }),
            id: id.into(),
            expected_revision: stage.revision,
            mode: "pages".into(),
            profiles: vec![stage.source.profiles[0].name.clone()],
            style_count: stage.source.styles.len(),
            issues: vec![],
            impact,
            repair_suggestions: vec![],
            recipe_warning: None,
        })
    }
    // Update plugin profile import
    pub fn update_plugin_profile_import(
        &self,
        id: &str,
        mut placement: PluginProfilePlacement,
    ) -> Result<ImportPreview, String> {
        let mut imports = self.plugin_profile_imports.lock().unwrap();
        let stage = imports
            .get_mut(id)
            .ok_or("Suggested profile import expired")?;
        let catalog = self.list_plugin_profiles(&stage.catalog.plugin_id)?;
        if catalog.fingerprint != stage.catalog.fingerprint {
            return Err("Plugin suggestions changed. Start a new import review.".into());
        }
        if placement.target_profile != NEW {
            let snapshot = self.snapshot();
            let target = snapshot
                .profiles
                .iter()
                .find(|p| p.id == placement.target_profile)
                .ok_or("Choose a destination profile")?;
            placement.target_model = target.model_id.clone();
            stage.profile_id = target.id.clone();
        } else if stage.placement.target_profile != NEW {
            stage.profile_id = uuid::Uuid::new_v4().to_string();
        }
        let base = self
            .plugins
            .lock()
            .unwrap()
            .configuration(&stage.catalog.plugin_id)?;
        let previous = serde_json::to_value(&stage.base).map_err(|e| e.to_string())?;
        let current = serde_json::to_value(&base).map_err(|e| e.to_string())?;
        placement
            .configuration_choices
            .retain(|path| at(&previous, path) == at(&current, path));
        stage.base = base;
        stage.placement = placement;
        stage.revision = self.snapshot().configuration_revision;
        self.plugin_profile_preview(id, stage)
    }
    // Commit plugin profile import
    pub(crate) fn commit_plugin_profile_import(
        &self,
        id: &str,
        expected: u64,
    ) -> Result<Snapshot, String> {
        let mut imports = self.plugin_profile_imports.lock().unwrap();
        let stage = imports
            .get_mut(id)
            .ok_or("Suggested profile import expired")?;
        let (candidate, _, problems) = self.plugin_profile_candidate(stage)?;
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        let changes: Vec<_> = fields(stage)
            .into_iter()
            .filter(|r| stage.placement.include_configuration && r.use_suggested)
            .map(|r| ConfigurationChange {
                path: r.path,
                before: r.current,
                after: Some(r.suggested),
            })
            .collect();
        let mut state = self.state.lock().unwrap();
        // Recheck the chosen device under the commit guard so disconnects cannot retarget an import
        if let Some(id) = stage.target_device.as_deref() {
            let device = crate::assignment_target::connected_device(&state, Some(id))?;
            if device.model_id != stage.placement.target_model {
                return Err("Import target model must match the selected device".into());
            }
        }
        if state.configuration_revision != expected || stage.revision != expected {
            return Err(
                "Workspace changed. Recheck the import; your selections have been retained.".into(),
            );
        }
        let _transaction = self.profiles.transaction.lock().unwrap();
        let _media = self.profiles.media_gate.lock().unwrap();
        let mut host = self.plugins.lock().unwrap();
        let plugin_id = &stage.catalog.plugin_id;
        let info = host
            .list()
            .into_iter()
            .find(|p| p.id == *plugin_id)
            .ok_or("Plugin is no longer installed")?;
        if host.installation_token(plugin_id) != stage.installation {
            return Err("Plugin installation changed. Start a new review.".into());
        }
        if discover(&host.plugin_directory(plugin_id)?, &info).fingerprint
            != stage.catalog.fingerprint
        {
            return Err("Plugin suggestions changed. Start a new review.".into());
        }
        let current = host.configuration(plugin_id)?;
        let after = configuration_after(&current, &changes, true)?;
        let history_id = uuid::Uuid::new_v4().to_string();
        let history = ConfigurationHistory {
            plugin_id: plugin_id.clone(),
            generation: host.configuration_generation(plugin_id)?,
            changes,
        };
        // Publish immutable assets first. A failed database transaction leaves only
        // collectible unreferenced files, never committed references to missing files
        for asset in crate::profiles::referenced_assets(&stage.source) {
            let ext = stage.store.asset_extension(&asset)?;
            let destination = self.profiles.root.join(format!("assets/{asset}.{ext}"));
            let bytes = stage.store.asset(&asset)?;
            if destination.exists() {
                if transaction::read(&destination, crate::media::MAX_ASSET)? != bytes {
                    return Err("A staged asset changed. Start a new review.".into());
                }
            } else {
                let mut file = tempfile::NamedTempFile::new_in(self.profiles.root.join("assets"))
                    .map_err(|e| e.to_string())?;
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                file.as_file().sync_all().map_err(|e| e.to_string())?;
                crate::profiles::atomic_replace(file.path(), &destination)?;
            }
        }
        let mut prepared = self.profiles.prepare_import_transaction(
            expected,
            "Import suggested plugin profile",
            Some(&history_id),
            serde_json::to_vec(&history)
                .map_err(|e| e.to_string())?
                .len(),
            |data| {
                *data = candidate;
                Ok(())
            },
        )?;
        if !history.changes.is_empty() {
            prepared.set_sidecar((
                history_id,
                serde_json::to_value(history).map_err(|e| e.to_string())?,
            ));
        }
        let data = host.configuration_transaction_sql(
            plugin_id,
            &current,
            &after,
            prepared.retained_bytes()?,
            move |tx| prepared.commit(tx),
        )?;
        Self::apply_data(&mut state, data);
        host.configuration_changed(plugin_id);
        drop(host);
        drop(_media);
        drop(_transaction);
        drop(state);
        imports.remove(id);
        drop(imports);
        self.finish_workspace_commit();
        Ok(self.snapshot())
    }
    // Read stored versions for the selected plugin profile
    pub(crate) fn plugin_profile_history(
        &self,
        operation: &str,
        expected: u64,
    ) -> Result<Option<Snapshot>, String> {
        if !["undo", "redo"].contains(&operation) {
            return Ok(None);
        }
        let snapshot = self.snapshot();
        let item = if operation == "redo" {
            snapshot.history.redo.last()
        } else {
            snapshot.history.undo.last()
        };
        let Some(item) = item else { return Ok(None) };
        let Some(history): Option<ConfigurationHistory> = self.profiles.storage.read_json(
            stream_dock_manager_storage::Domain::HistoryPlugins,
            &item.id,
            32 * 1024 * 1024,
        )?
        else {
            return Ok(None);
        };
        let mut state = self.state.lock().unwrap();
        if state.configuration_revision != expected || snapshot.configuration_revision != expected {
            return Err("Workspace changed. Review history again.".into());
        }
        let _transaction = self.profiles.transaction.lock().unwrap();
        let mut host = self.plugins.lock().unwrap();
        if host.configuration_generation(&history.plugin_id)? != history.generation {
            return Err("Plugin data was cleared after this history entry. Undo/Redo cannot restore deleted settings.".into());
        }
        let current = host.configuration(&history.plugin_id)?;
        let after = configuration_after(&current, &history.changes, operation == "redo")?;
        let prepared = self.profiles.prepare_history(expected, operation)?;
        let data = host.configuration_transaction_sql(
            &history.plugin_id,
            &current,
            &after,
            prepared.retained_bytes()?,
            move |tx| prepared.commit(tx),
        )?;
        Self::apply_data(&mut state, data);
        host.configuration_changed(&history.plugin_id);
        drop(host);
        drop(_transaction);
        drop(state);
        self.finish_workspace_commit();
        Ok(Some(self.snapshot()))
    }
}

#[cfg(test)]
#[path = "plugin_profiles_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "plugin_profiles_generated_tests.rs"]
mod generated_corpus_tests;
