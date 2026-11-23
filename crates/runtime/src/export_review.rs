//! Immutable portable export candidates, edited and written without reloading the live workspace
use crate::{
    actions::Action,
    asset_leases::AssetLease,
    builtins::BuiltinAction,
    macro_inputs::{MacroArgument, MacroInput},
    parameters::ProfileParameter,
    profiles::{self, ProfileData, ProfileExportOptions, ProfileStore},
    Runtime,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};
const LIFETIME: Duration = Duration::from_secs(3600);
const MAX_ARCHIVE: u64 = 80 * 1024 * 1024;
type Contracts = BTreeMap<(String, String), BTreeSet<String>>;
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportReviewRequest {
    pub kind: String,
    pub ids: Vec<String>,
    pub include_plugin_settings: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ExportField {
    pub key: String,
    pub location: String,
    pub label: String,
    pub value: Value,
    pub value_type: String,
    pub can_parameter: bool,
    pub can_input: bool,
    pub can_require: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct ExportReview {
    pub id: String,
    pub kind: String,
    #[ts(type = "number")]
    pub revision: u64,
    #[ts(type = "number")]
    pub source_revision: u64,
    pub names: Vec<String>,
    pub include_plugin_settings: bool,
    pub fields: Vec<ExportField>,
    pub warnings: Vec<String>,
    pub archive_sha256: String,
    pub document_sha256: String,
    pub candidate_json: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportFieldEdit {
    pub key: String,
    /// replace | parameter | input | required
    pub operation: String,
    pub replacement: Value,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct RehearsalFinding {
    pub group: String,
    pub message: String,
    pub profile_id: String,
    pub page_id: String,
    pub control_id: String,
}

pub(crate) struct StagedExport {
    store: ProfileStore,
    data: ProfileData,
    contracts: Contracts,
    report: ExportReview,
    created: Instant,
    _assets: AssetLease,
    directory: tempfile::TempDir,
}
impl crate::review_store::ReviewSession for StagedExport {
    const LIMIT: usize = 8;
    const FULL: &'static str = "Close an export review first";
    const LIFETIME: Option<std::time::Duration> = Some(LIFETIME);
    // Read the review creation time used to enforce its expiration
    fn created(&self) -> std::time::Instant {
        self.created
    }
}
#[derive(Clone)]
enum Scope {
    Profile(String),
    Macro(String),
}
#[derive(Clone)]
enum Kind {
    Text(bool),
    Plugin(bool),
    Argument,
    ProfileDefault(String),
    MacroDefault(String),
}
#[derive(Clone)]
struct Context {
    scope: Scope,
    location: String,
    label: String,
    kind: Kind,
}
struct Walker<'a, F> {
    index: usize,
    visit: &'a mut F,
}
impl<F: FnMut(String, &Context, &mut Value) -> Result<(), String>> Walker<'_, F> {
    // Deliver the export review event to its consumer
    fn emit(&mut self, context: &Context, value: &mut Value) -> Result<(), String> {
        if self.index >= 2048 {
            return Err("Export review exceeds 2,048 fields; select less content".into());
        }
        let key = format!("field-{}", self.index);
        self.index += 1;
        (self.visit)(key, context, value)
    }
    // Visit exportable action fields and rewrite their reviewed literals and macro arguments
    fn action(
        &mut self,
        action: &mut Action,
        context: &Context,
        contracts: &Contracts,
    ) -> Result<(), String> {
        let shared = RefCell::new(&mut *self);
        action.try_visit_fields_mut::<String>(
            &mut |role, value| {
                if matches!(
                    role,
                    crate::action_fields::FieldRole::Profile
                        | crate::action_fields::FieldRole::Page
                        | crate::action_fields::FieldRole::Device
                        | crate::action_fields::FieldRole::Control
                        | crate::action_fields::FieldRole::Macro
                ) {
                    return Ok(());
                }
                let mut context = context.clone();
                context.kind = Kind::Text(role.supports_parameter());
                context.label = format!("{role:?}");
                let mut editable = Value::String(value.clone());
                shared.borrow_mut().emit(&context, &mut editable)?;
                *value = editable
                    .as_str()
                    .ok_or("This action field requires text")?
                    .into();
                Ok(())
            },
            &mut |plugin, action, settings| {
                let allowed = contracts.get(&(plugin.into(), action.into()));
                if let Some(map) = settings.as_object_mut() {
                    for (key, value) in map {
                        let mut context = context.clone();
                        context.kind = Kind::Plugin(allowed.is_some_and(|a| a.contains(key)));
                        context.label = crate::macro_debug::bounded(
                            format!("Plugin {plugin} / {action} / {key}"),
                            1024,
                        );
                        shared.borrow_mut().emit(&context, value)?;
                    }
                } else if !settings.is_null() {
                    let mut context = context.clone();
                    context.kind = Kind::Plugin(false);
                    context.label = format!("Plugin {plugin} / {action} settings");
                    shared.borrow_mut().emit(&context, settings)?;
                }
                Ok(())
            },
        )?;
        self.arguments(action, context)
    }
    // Process saved-macro arguments through the export review’s parameter rules
    fn arguments(&mut self, action: &mut Action, context: &Context) -> Result<(), String> {
        if let Action::Builtin { action } = action {
            if let BuiltinAction::SavedMacro {
                macro_id,
                arguments,
            } = &mut **action
            {
                for (name, argument) in arguments {
                    if let MacroArgument::Literal { value } = argument {
                        let mut context = context.clone();
                        context.kind = Kind::Argument;
                        context.label = format!("Macro {macro_id} argument {name}");
                        self.emit(&context, value)?;
                        if let Some(name) = value
                            .as_object()
                            .filter(|m| m.len() == 1)
                            .and_then(|m| m.get("$parameter"))
                            .and_then(Value::as_str)
                        {
                            *argument = MacroArgument::Parameter { name: name.into() };
                        } else if let Some(name) = value
                            .as_object()
                            .filter(|m| m.len() == 1)
                            .and_then(|m| m.get("$input"))
                            .and_then(Value::as_str)
                        {
                            *argument = MacroArgument::Input { name: name.into() };
                        }
                    }
                }
            }
        }
        action.try_for_each_child_mut(|child| self.arguments(child, context))?;
        Ok(())
    }
}
// Walk the export review structure
fn walk(
    data: &mut ProfileData,
    contracts: &Contracts,
    visit: &mut impl FnMut(String, &Context, &mut Value) -> Result<(), String>,
) -> Result<(), String> {
    let mut walker = Walker { index: 0, visit };
    for profile in &mut data.profiles {
        let mut context = Context {
            scope: Scope::Profile(profile.id.clone()),
            location: profile.name.clone(),
            label: String::new(),
            kind: Kind::Text(false),
        };
        for parameter in &mut profile.parameters {
            if let Some(value) = &mut parameter.default {
                context.kind = Kind::ProfileDefault(parameter.name.clone());
                context.label = format!("Parameter {} default", parameter.label);
                walker.emit(&context, value)?;
            }
        }
        for page in &mut profile.pages {
            for (control, binding) in &mut page.bindings {
                context.location = crate::macro_debug::bounded(
                    format!("{} / {} / {control}", profile.name, page.name),
                    640,
                );
                walker.action(&mut binding.action, &context, contracts)?;
            }
        }
    }
    for macro_ in &mut data.saved_macros {
        let mut context = Context {
            scope: Scope::Macro(macro_.id.clone()),
            location: format!("Macro {}", macro_.name),
            label: String::new(),
            kind: Kind::Text(false),
        };
        for input in &mut macro_.inputs {
            if let Some(value) = &mut input.default {
                context.kind = Kind::MacroDefault(input.name.clone());
                context.label = format!("Input {} default", input.label);
                walker.emit(&context, value)?;
            }
        }
        walker.action(&mut macro_.action, &context, contracts)?;
    }
    Ok(())
}
// Classify JSON literals for the export parameter type contract
fn value_type(value: &Value) -> &'static str {
    if value.is_string() {
        "string"
    } else if value.is_boolean() {
        "boolean"
    } else if value.is_number() {
        "number"
    } else {
        "json"
    }
}
// Report the reference condition for export review
fn reference(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        crate::parameters::reference(s).is_some() || crate::macro_inputs::reference(s).is_some()
    }) || value
        .as_object()
        .is_some_and(|m| m.len() == 1 && (m.contains_key("$parameter") || m.contains_key("$input")))
}
// Collect reviewable literal fields while enforcing the aggregate review-byte limit
fn fields(data: &mut ProfileData, contracts: &Contracts) -> Result<Vec<ExportField>, String> {
    let mut out = vec![];
    let mut total = 0usize;
    walk(data, contracts, &mut |key, c, value| {
        total = total.saturating_add(serde_json::to_vec(value).map_err(|e| e.to_string())?.len());
        if total > 4 * 1024 * 1024 {
            return Err("Literal review exceeds 4 MiB; select less content".into());
        }
        let convertible = matches!(
            c.kind,
            Kind::Text(true) | Kind::Plugin(true) | Kind::Argument
        ) && value_type(value) != "json"
            && !reference(value);
        out.push(ExportField {
            key,
            location: crate::macro_debug::bounded(c.location.clone(), 640),
            label: crate::macro_debug::bounded(c.label.clone(), 1024),
            value: value.clone(),
            value_type: value_type(value).into(),
            can_parameter: convertible && matches!(c.scope, Scope::Profile(_)),
            can_input: convertible && matches!(c.scope, Scope::Macro(_)),
            can_require: matches!(c.kind, Kind::ProfileDefault(_) | Kind::MacroDefault(_)),
        });
        Ok(())
    })?;
    Ok(out)
}
// Visit mut
fn visit_mut(action: &mut Action, f: &mut impl FnMut(&mut Action)) {
    action.visit_mut(f);
}
// Append a profile parameter only when its name does not conflict with an existing definition
fn add_parameter(profile: &mut crate::Profile, name: &str, kind: &str) -> Result<(), String> {
    if profile.parameters.iter().any(|p| p.name == name) {
        return Err(format!(
            "Profile {} already defines {name}; choose a different name",
            profile.name
        ));
    }
    profile.parameters.push(ProfileParameter {
        name: name.into(),
        label: name.into(),
        r#type: kind.into(),
        required: true,
        default: None,
        sensitive: kind == "string",
    });
    Ok(())
}
/// Forward new required input contracts through included callers, ending at profile parameters
/// Existing explicit argument values remain visible for separate literal review
fn require_input(
    data: &mut ProfileData,
    target: &str,
    name: &str,
    kind: &str,
) -> Result<(), String> {
    let mut needed = BTreeSet::from([target.to_string()]);
    let mut processed = BTreeSet::new();
    let mut introduced = BTreeSet::from([target.to_string()]);
    let mut profile_parameters = BTreeSet::new();
    while let Some(target) = needed.pop_first() {
        if !processed.insert(target.clone()) {
            continue;
        }
        for caller in &mut data.saved_macros {
            let mut used = false;
            visit_mut(&mut caller.action, &mut |action| {
                if let Action::Builtin { action } = action {
                    if let BuiltinAction::SavedMacro {
                        macro_id,
                        arguments,
                    } = &mut **action
                    {
                        if *macro_id == target && !arguments.contains_key(name) {
                            arguments
                                .insert(name.into(), MacroArgument::Input { name: name.into() });
                            used = true;
                        }
                    }
                }
            });
            if used && !introduced.contains(&caller.id) {
                if caller.inputs.iter().any(|i| i.name == name) {
                    return Err(format!(
                        "Macro {} already defines {name}; choose a different name",
                        caller.name
                    ));
                }
                caller.inputs.push(MacroInput {
                    name: name.into(),
                    label: name.into(),
                    r#type: kind.into(),
                    required: true,
                    default: None,
                });
                introduced.insert(caller.id.clone());
                needed.insert(caller.id.clone());
            }
        }
        for profile in &mut data.profiles {
            let mut used = false;
            for binding in profile
                .pages
                .iter_mut()
                .flat_map(|p| p.bindings.values_mut())
            {
                visit_mut(&mut binding.action, &mut |action| {
                    if let Action::Builtin { action } = action {
                        if let BuiltinAction::SavedMacro {
                            macro_id,
                            arguments,
                        } = &mut **action
                        {
                            if *macro_id == target && !arguments.contains_key(name) {
                                arguments.insert(
                                    name.into(),
                                    MacroArgument::Parameter { name: name.into() },
                                );
                                used = true;
                            }
                        }
                    }
                });
            }
            if used && profile_parameters.insert(profile.id.clone()) {
                add_parameter(profile, name, kind)?;
            }
        }
    }
    Ok(())
}
// Report the credential marker condition for export review
fn credential_marker(value: &Value) -> bool {
    match value {
        Value::Object(m) => m.contains_key("$credential") || m.values().any(credential_marker),
        Value::Array(a) => a.iter().any(credential_marker),
        _ => false,
    }
}
// Edit the selected export review value
fn edit(
    data: &mut ProfileData,
    contracts: &Contracts,
    change: &ExportFieldEdit,
) -> Result<(), String> {
    let mut selected = None;
    walk(data, contracts, &mut |key, c, value| {
        if key == change.key {
            selected = Some((c.clone(), value.clone()));
        }
        Ok(())
    })?;
    let (context, value) = selected.ok_or("Field changed; reload the candidate")?;
    let kind = value_type(&value);
    let is_convertible = matches!(
        context.kind,
        Kind::Text(true) | Kind::Plugin(true) | Kind::Argument
    ) && kind != "json"
        && !reference(&value);
    if change.operation == "replace" {
        if credential_marker(&change.replacement) {
            return Err("Local credential references cannot be exported".into());
        }
        walk(data, contracts, &mut |key, _, value| {
            if key == change.key {
                *value = change.replacement.clone();
            }
            Ok(())
        })?;
    } else if change.operation == "required" {
        match (&context.scope, &context.kind) {
            (Scope::Profile(id), Kind::ProfileDefault(name)) => {
                let p = data
                    .profiles
                    .iter_mut()
                    .find(|p| p.id == *id)
                    .unwrap()
                    .parameters
                    .iter_mut()
                    .find(|p| p.name == *name)
                    .unwrap();
                p.default = None;
                p.required = true;
                p.sensitive = p.r#type == "string";
            }
            (Scope::Macro(id), Kind::MacroDefault(name)) => {
                let input = data
                    .saved_macros
                    .iter_mut()
                    .find(|m| m.id == *id)
                    .unwrap()
                    .inputs
                    .iter_mut()
                    .find(|i| i.name == *name)
                    .unwrap();
                let kind = input.r#type.clone();
                input.default = None;
                input.required = true;
                require_input(data, id, name, &kind)?;
            }
            _ => return Err("Only portable defaults can become required values".into()),
        }
    } else if ["parameter", "input"].contains(&change.operation.as_str()) && is_convertible {
        if !crate::macro_inputs::valid_name(&change.name) {
            return Err("Use a unique name of 1–80 letters, digits or underscores".into());
        }
        let marker = match (&context.scope, change.operation.as_str()) {
            (Scope::Profile(id), "parameter") => {
                add_parameter(
                    data.profiles.iter_mut().find(|p| p.id == *id).unwrap(),
                    &change.name,
                    kind,
                )?;
                "parameter"
            }
            (Scope::Macro(id), "input") => {
                let m = data.saved_macros.iter_mut().find(|m| m.id == *id).unwrap();
                if m.inputs.iter().any(|i| i.name == change.name) {
                    return Err("This macro already defines that input".into());
                }
                m.inputs.push(MacroInput {
                    name: change.name.clone(),
                    label: change.name.clone(),
                    r#type: kind.into(),
                    required: true,
                    default: None,
                });
                "input"
            }
            _ => {
                return Err(
                    "Choose a profile parameter or a saved-macro input in its owning scope".into(),
                )
            }
        };
        walk(data, contracts, &mut |key, c, value| {
            if key == change.key {
                *value = if matches!(c.kind, Kind::Argument)
                    || (!value.is_string() && matches!(c.kind, Kind::Plugin(true)))
                {
                    json!({format!("${marker}"):change.name})
                } else {
                    Value::String(format!("${{{marker}.{}}}", change.name))
                };
            }
            Ok(())
        })?;
        if let Scope::Macro(id) = &context.scope {
            require_input(data, id, &change.name, kind)?;
        }
    } else {
        return Err("This field does not support that conversion".into());
    }
    data.update_requirements();
    profiles::validate_data(data)?;
    if serde_json::to_vec(data).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Err("Candidate exceeds 16 MiB".into());
    }
    Ok(())
}
// Hash a bounded regular candidate archive while rejecting symlinks
fn archive_digest(path: &Path) -> Result<String, String> {
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_ARCHIVE + 256 {
        return Err("Candidate archive is not a bounded regular file".into());
    }
    let mut file = std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_ARCHIVE + 257);
    let mut buffer = [0; 65536];
    let mut hash = Sha256::new();
    let mut total = 0;
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hash.update(&buffer[..n]);
    }
    if total > MAX_ARCHIVE + 256 {
        return Err("Candidate archive exceeds 80 MiB".into());
    }
    Ok(hex::encode(hash.finalize()))
}
// Build archive
fn build_archive(
    stage: &StagedExport,
    data: ProfileData,
) -> Result<(String, String, String), String> {
    let path = stage.directory.path().join("candidate.zip");
    stage
        .store
        .export_archive(&path, data, stage.report.include_plugin_settings)?;
    let (_, document) = profiles::open_archive(&path, "profiles.json")?;
    Ok((
        archive_digest(&path)?,
        hex::encode(Sha256::digest(document.as_bytes())),
        document,
    ))
}
impl Runtime {
    pub fn rehearse_export(
        &self,
        id: &str,
        revision: u64,
        digest: &str,
        model: &str,
    ) -> Result<Vec<RehearsalFinding>, String> {
        let models = hardware_registry::manifests();
        let recipient = models
            .iter()
            .find(|m| m.model_id == model)
            .ok_or("Choose a recipient device model")?;
        let stages = self.exports.lock().unwrap();
        let stage = stages.active(id, "Export review expired", "Review the candidate again")?;
        if stage.report.kind != "profiles"
            || stage.report.revision != revision
            || stage.report.archive_sha256 != digest
            || self.state.lock().unwrap().configuration_revision != stage.report.source_revision
        {
            return Err("Workspace or candidate changed. Prepare a new export review.".into());
        }
        let path = stage.directory.path().join("candidate.zip");
        if archive_digest(&path)? != digest {
            return Err("Candidate integrity check failed".into());
        }
        let mut findings = vec![];
        let mut add = |group: &str, message: &str, profile: &str, page: &str, control: &str| {
            findings.push(RehearsalFinding {
                group: group.into(),
                message: message.into(),
                profile_id: profile.into(),
                page_id: page.into(),
                control_id: control.into(),
            });
        };
        let imported = match crate::importing::stage_portable(&path, 0) {
            Ok(stage) => stage,
            Err(_) => {
                add(
                    "Blockers",
                    "The portable importer rejected the archive or an embedded asset.",
                    "",
                    "",
                    "",
                );
                return Ok(findings);
            }
        };
        if profiles::validate_data(&imported.data).is_err() {
            add(
                "Blockers",
                "The imported workspace failed Manager validation.",
                "",
                "",
                "",
            );
        }
        let mut local_paths = std::collections::HashSet::new();
        for issue in crate::importing::inspect_recipient(&imported, &[], &[], false) {
            let plugin_message;
            let (group, message) = match issue.kind.as_str() {
                "plugin"
                    if issue.value.len() <= 240
                        && issue
                            .value
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c)) =>
                {
                    plugin_message = format!(
                        "Install the plugin providing {} and its declared runtime.",
                        issue.value
                    );
                    ("Recipient prerequisites", plugin_message.as_str())
                }
                "plugin" => (
                    "Recipient prerequisites",
                    "Install the required plugin and its declared runtime.",
                ),
                "path" => {
                    if !local_paths.insert((
                        issue.profile_id.clone(),
                        issue.page_id.clone(),
                        issue.control_id.clone(),
                    )) {
                        continue;
                    }
                    (
                        "Recipient prerequisites",
                        "Configure the recipient’s local application, executable or file.",
                    )
                }
                _ => ("Blockers", issue.message.as_str()),
            };
            add(
                group,
                message,
                &issue.profile_id,
                &issue.page_id,
                &issue.control_id,
            );
        }
        for profile in &imported.data.profiles {
            if profile.model_id != model {
                add(
                    "Blockers",
                    "Convert this profile to the selected recipient model.",
                    &profile.id,
                    "",
                    "",
                );
            }
            for param in &profile.parameters {
                if param.required && param.default.is_none() {
                    add(
                        "Recipient prerequisites",
                        &format!("Set required parameter {}.", param.name),
                        &profile.id,
                        "",
                        "",
                    );
                }
            }
            for page in &profile.pages {
                for (control, binding) in &page.bindings {
                    if profile.model_id != model {
                        add(
                            "Blockers",
                            if recipient.control(control).is_err() {
                                "Control is unavailable on the selected recipient model."
                            } else {
                                "Review this assignment when converting between device models."
                            },
                            &profile.id,
                            &page.id,
                            control,
                        );
                    }
                    let mut local = false;
                    let mut parameters = std::collections::BTreeSet::new();
                    for segment in &binding.live_label {
                        if let crate::live_label::LabelSegment::Parameter { name, .. } = segment {
                            parameters.insert(name.clone());
                        }
                    }
                    binding.action.try_visit_fields(
                        &mut |role, value| -> Result<(), String> {
                            if let Some(name) = crate::parameters::reference(value) {
                                parameters.insert(name.to_owned());
                            } else {
                                local |= role.is_local_path() && !value.is_empty();
                            }
                            Ok(())
                        },
                        &mut |_, _, _| Ok(()),
                    )?;
                    for name in parameters {
                        if profile
                            .parameters
                            .iter()
                            .any(|p| p.name == name && p.required && p.default.is_none())
                        {
                            add(
                                "Recipient prerequisites",
                                &format!("This assignment uses required parameter {name}."),
                                &profile.id,
                                &page.id,
                                control,
                            );
                        }
                    }
                    if local
                        && !local_paths.contains(&(
                            profile.id.clone(),
                            page.id.clone(),
                            control.clone(),
                        ))
                    {
                        add(
                            "Recipient prerequisites",
                            "Review machine-local paths on the recipient installation.",
                            &profile.id,
                            &page.id,
                            control,
                        );
                    }
                }
            }
        }
        add("Verify on a real installation", "Check device output, operating-system permissions and installed actions on the recipient device.", "", "", "");
        Ok(findings)
    }

    // Prepare export review
    pub fn prepare_export_review(
        &self,
        request: ExportReviewRequest,
    ) -> Result<ExportReview, String> {
        if request.ids.is_empty()
            || request.ids.len() > 1000
            || !["profiles", "macros"].contains(&request.kind.as_str())
        {
            return Err("Choose 1–1000 profiles or macros to export".into());
        }
        let mut stages = self.exports.lock().unwrap();
        stages.check_capacity()?;
        let snapshot = self.snapshot();
        let mut contracts = Contracts::new();
        for plugin in snapshot.plugins {
            if let Some(c) = plugin.capabilities {
                for (id, action) in c.actions {
                    contracts.insert(
                        (plugin.id.clone(), id),
                        action.parameter_fields.into_iter().collect(),
                    );
                }
            }
        }
        let transaction = self.profiles.transaction.lock().unwrap();
        let mut data = self.profiles.load()?;
        let source_revision = data.configuration_revision;
        data = match request.kind.as_str() {
            "profiles" => profiles::select_profile_export(
                data,
                &ProfileExportOptions {
                    profile_ids: Some(request.ids.clone()),
                    include_plugin_settings: request.include_plugin_settings,
                },
            )?,
            "macros" => crate::macro_sharing::select_macro_export(data, &request.ids)?,
            _ => return Err("Choose profiles or macros to export".into()),
        };
        let names = if request.kind == "profiles" {
            data.profiles.iter().map(|p| p.name.clone()).collect()
        } else {
            data.saved_macros.iter().map(|m| m.name.clone()).collect()
        };
        profiles::prepare_portable_export(&mut data, request.include_plugin_settings)?;
        profiles::validate_data(&data)?;
        if serde_json::to_vec(&data).map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
            return Err("Candidate exceeds 16 MiB".into());
        }
        let fields = fields(&mut data, &contracts)?;
        let assets = profiles::referenced_assets(&data);
        let lease = self
            .profiles
            .asset_leases
            .pin(assets.clone(), "Reviewed export candidate")?;
        let media = self.profiles.media_gate.lock()?;
        let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
        let store = ProfileStore::staging(&directory)?;
        let mut total = 0usize;
        for id in assets {
            if let Ok(ext) = self.profiles.asset_extension(&id) {
                let bytes = crate::media_library::read_regular(
                    &self
                        .profiles
                        .root
                        .join("assets")
                        .join(format!("{id}.{ext}")),
                    crate::media::MAX_ASSET,
                )?;
                total = total.saturating_add(bytes.len());
                if total > 256 * 1024 * 1024 {
                    return Err("Candidate media exceeds 256 MiB".into());
                }
                std::fs::write(store.root.join("assets").join(format!("{id}.{ext}")), bytes)
                    .map_err(|e| e.to_string())?;
            }
        }
        drop(media);
        drop(transaction);
        let warnings = vec![
            "Review every literal and the complete candidate document before sharing. Local overrides, credentials, rules, history, favorites and repair recipes are excluded.".into(),
            "Required macro inputs are forwarded through included callers; profile callers receive required parameters. Configure required values after import.".into(),
        ];
        let report = ExportReview {
            id: crate::review_store::new_id(),
            kind: request.kind,
            revision: 1,
            source_revision,
            names,
            include_plugin_settings: request.include_plugin_settings,
            fields,
            warnings,
            archive_sha256: String::new(),
            document_sha256: String::new(),
            candidate_json: String::new(),
        };
        let mut stage = StagedExport {
            store,
            data,
            contracts,
            report,
            created: Instant::now(),
            _assets: lease,
            directory,
        };
        let (archive, document, json) = build_archive(&stage, stage.data.clone())?;
        stage.report.archive_sha256 = archive;
        stage.report.document_sha256 = document;
        stage.report.candidate_json = json;
        let report = stage.report.clone();
        stages.insert(report.id.clone(), stage)?;
        Ok(report)
    }
    // Update export review
    pub fn update_export_review(
        &self,
        id: &str,
        expected: u64,
        change: ExportFieldEdit,
    ) -> Result<ExportReview, String> {
        let mut stages = self.exports.lock().unwrap();
        let stage = stages.active_mut(
            id,
            "Export review expired",
            "Export candidate changed or expired; reload the review",
        )?;
        if stage.report.revision != expected {
            return Err("Export candidate changed or expired; reload the review".into());
        }
        if change.key.len() > 64
            || change.name.len() > 80
            || serde_json::to_vec(&change.replacement)
                .map_err(|e| e.to_string())?
                .len()
                > 4 * 1024 * 1024
        {
            return Err("Export field edit exceeds its bounds".into());
        }
        let revision = stage
            .report
            .revision
            .checked_add(1)
            .filter(|r| *r <= 9_007_199_254_740_991)
            .ok_or("Candidate revision exhausted")?;
        let mut candidate = stage.data.clone();
        edit(&mut candidate, &stage.contracts, &change)?;
        let fields = fields(&mut candidate, &stage.contracts)?;
        let (archive, document, json) = build_archive(stage, candidate.clone())?;
        stage.data = candidate;
        stage.report.fields = fields;
        stage.report.revision = revision;
        stage.report.archive_sha256 = archive;
        stage.report.document_sha256 = document;
        stage.report.candidate_json = json;
        Ok(stage.report.clone())
    }
    // Discard the selected export review state
    pub fn discard_export_review(&self, id: &str) {
        self.exports.lock().unwrap().remove(id);
    }
    // Write export review
    pub fn write_export_review(
        &self,
        id: &str,
        expected: u64,
        sha256: &str,
        path: &Path,
    ) -> Result<(), String> {
        self.write_export_transfer(id, expected, sha256, path, None)
            .map(|_| ())
    }
    pub fn write_export_transfer(
        &self,
        id: &str,
        expected: u64,
        sha256: &str,
        path: &Path,
        password: Option<&str>,
    ) -> Result<String, String> {
        let mut stages = self.exports.lock().unwrap();
        let stage = stages.active(
            id,
            "Export review expired",
            "Export candidate changed or expired; review it again",
        )?;
        if stage.report.revision != expected || stage.report.archive_sha256 != sha256 {
            return Err("Export candidate changed or expired; review it again".into());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = parent.canonicalize().map_err(|e| e.to_string())?;
        stream_dock_manager_storage::vault::check_transfer_path(
            self.profiles.storage().root(),
            path,
        )?;
        if parent.starts_with(
            self.profiles
                .root
                .canonicalize()
                .map_err(|e| e.to_string())?,
        ) || parent.starts_with(stage.directory.path())
        {
            return Err(
                "Choose an export destination outside managed configuration and staging".into(),
            );
        }
        let source = stage.directory.path().join("candidate.zip");
        if archive_digest(&source)? != sha256 {
            return Err("Frozen candidate integrity failed; start a new review".into());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&parent).map_err(|e| e.to_string())?;
        let mut data = zeroize::Zeroizing::new(Vec::new());
        std::fs::File::open(source)
            .map_err(|e| e.to_string())?
            .take(MAX_ARCHIVE + 1)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;
        if data.len() as u64 > MAX_ARCHIVE || hex::encode(Sha256::digest(data.as_slice())) != sha256
        {
            return Err("Frozen candidate changed during write; destination preserved".into());
        }
        if stage.report.include_plugin_settings {
            use stream_dock_manager_storage::vault::transfer::{self, Kind};
            transfer::write(
                if stage.report.kind == "macros" {
                    Kind::Macros
                } else {
                    Kind::Profiles
                },
                temporary.as_file_mut(),
                &mut data,
                password
                    .filter(|s| !s.is_empty())
                    .ok_or("Enter an export password to include plugin settings.")?,
            )?;
        } else {
            temporary.write_all(&data).map_err(|e| e.to_string())?;
        }
        temporary.as_file().sync_all().map_err(|e| e.to_string())?;
        let written = if stage.report.include_plugin_settings {
            archive_digest(temporary.path())?
        } else {
            sha256.to_owned()
        };
        profiles::atomic_replace(temporary.path(), path)?;
        stages.remove(id);
        Ok(written)
    }
}

#[cfg(test)]
mod tests;
