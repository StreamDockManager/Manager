//! Explicit, isolated recovery of current-format profiles, drafts, macros and allowlisted settings
//! No Runtime, plugin, device or ordinary settings writer runs in this session
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::PathBuf, sync::Mutex};
use stream_dock_manager_storage::{rusqlite::Connection, Domain, RecoveryEditor};
const LIMIT: usize = 16 * 1024 * 1024;
mod full_restore;
pub use full_restore::ConfigurationRecoveryRestorePreview;
mod backups;
mod saved_items;
mod settings;
pub use backups::{
    has_recoverable_backups, ConfigurationRecoveryBackup, ConfigurationRecoveryBackupProfile,
    ConfigurationRecoveryBackupProfiles, ConfigurationRecoveryBackups,
};
mod versions;
pub use versions::{ConfigurationRecoveryVersion, ConfigurationRecoveryVersions};

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryItem {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub issues: Vec<String>,
    pub editable: bool,
    pub deletable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryReport {
    pub primary_readable: bool,
    pub startup_error: String,
    pub issues: Vec<String>,
    pub items: Vec<ConfigurationRecoveryItem>,
    pub ready: bool,
    pub last_backup: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryDiagnostic {
    pub message: String,
    /// True when the location anchors a containing object rather than an offending value
    pub anchor: bool,
    /// RFC 6901 pointer, when the validator identifies an exact JSON value
    pub pointer: Option<String>,
    /// One-based source line and UTF-8 byte column, as reported by serde_json
    pub line: Option<usize>,
    pub column: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryDocument {
    pub id: String,
    pub revision: String,
    pub json: String,
    pub issues: Vec<String>,
    #[serde(default)]
    #[ts(optional)]
    pub diagnostics: Option<Vec<ConfigurationRecoveryDiagnostic>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct ConfigurationRecoveryValidation {
    pub valid: bool,
    pub issues: Vec<String>,
    #[serde(default)]
    #[ts(optional)]
    pub diagnostics: Option<Vec<ConfigurationRecoveryDiagnostic>>,
}
pub struct ConfigurationRecovery {
    editor: RecoveryEditor,
    root: PathBuf,
    backups: Mutex<backups::BackupCache>,
    restore_review: Mutex<Option<full_restore::RestoreReview>>,
    startup_error: String,
    last_backup: Mutex<Option<String>>,
    versions: Mutex<versions::VersionCache>,
}
struct Record {
    id: String,
    key: String,
    kind: &'static str,
    value: Value,
}
/// Read-only startup routing. A missing primary offers recoverable backups before
/// starting normal initialization. A persisted explicit normal restart overrides a
/// command-line recovery flag; Continue bypasses the editor for one admitted run
pub fn startup_landing(root: &std::path::Path, cli_recovery: bool) -> Result<bool, String> {
    if !root
        .join(stream_dock_manager_storage::DATABASE_NAME)
        .exists()
    {
        return Ok(cli_recovery || backups::has_recoverable_backups(root)?);
    }
    let editor = RecoveryEditor::open(root.to_path_buf())?;
    editor.read(|conn, _| {
        let request = crate::profile_database::LocalPreference::RestartRecoveryOnce
            .record::<bool>()
            .read_from(conn)?;
        let bypass = crate::profile_database::LocalPreference::ConfigurationRecoveryBypass
            .record::<bool>()
            .read_from(conn)?;
        Ok(match request {
            Some(false) => false,
            Some(true) => bypass != Some(true),
            None => cli_recovery,
        })
    })
}

impl ConfigurationRecovery {
    /// Initialize a missing primary without importing or replacing preserved evidence
    /// Backup transfer is a separate explicit operation in this recovery session
    pub fn open_requested(root: PathBuf, message: String) -> Result<Self, String> {
        if !root
            .join(stream_dock_manager_storage::DATABASE_NAME)
            .exists()
        {
            let offer_backups = backups::has_recoverable_backups(&root)?;
            let storage = stream_dock_manager_storage::Storage::open(root.clone())?;
            if offer_backups {
                // Keep the chooser available after a close/crash until the user
                // explicitly continues into the newly initialized workspace
                storage.write_json(Domain::LocalPreferences, "restart-recovery-once", &true, 16)?;
            }
            drop(storage);
        }
        Self::open(root, message)
    }
    /// Called only after the user chooses Continue (true) or normal restart
    /// (false). The admission check and both intent keys are one transaction.
    pub fn request_restart(&self, recovery: bool) -> Result<(), String> {
        self.editor.refresh()?;
        let revision = self.editor.read(|conn, revision| {
            overall(conn)?;
            Ok(revision.to_owned())
        })?;
        let (_, backup) = self.editor.commit(&revision, |conn| {
            overall(conn)?;
            stream_dock_manager_storage::write_json(
                conn,
                Domain::LocalPreferences,
                "restart-recovery-once",
                &recovery,
                16,
            )?;
            if recovery {
                stream_dock_manager_storage::write_json(
                    conn,
                    Domain::LocalPreferences,
                    "configuration-recovery-bypass",
                    &true,
                    16,
                )?;
            } else {
                stream_dock_manager_storage::delete(
                    conn,
                    Domain::LocalPreferences,
                    "configuration-recovery-bypass",
                )?;
            }
            Ok(())
        })?;
        *self
            .last_backup
            .lock()
            .map_err(|_| "Recovery lock failed")? = Some(backup.display().to_string());
        Ok(())
    }

    // Open the requested configuration recovery resource
    pub fn open(root: PathBuf, startup_error: String) -> Result<Self, String> {
        Ok(Self {
            editor: RecoveryEditor::open_for_recovery(root.clone())?,
            root,
            backups: Mutex::new(Default::default()),
            restore_review: Mutex::new(None),
            startup_error,
            last_backup: Mutex::new(None),
            versions: Mutex::new(Default::default()),
        })
    }
    // Assemble the configuration recovery report
    pub fn report(&self) -> Result<ConfigurationRecoveryReport, String> {
        self.editor.refresh()?;
        if let Some(reason) = self.editor.unavailable_reason()? {
            return Ok(ConfigurationRecoveryReport {
                primary_readable: false,
                startup_error: self.startup_error.clone(),
                issues: vec![reason],
                items: vec![],
                ready: false,
                last_backup: self
                    .last_backup
                    .lock()
                    .map_err(|_| "Recovery lock failed")?
                    .clone(),
            });
        }
        self.editor.read(|conn, _| {
            let mut issues = overall(conn).err().into_iter().collect::<Vec<_>>();
            let records = records(conn)?;
            let items = records
                .into_iter()
                .map(|record| {
                    let mut local = record_issues(&record, &record.value);
                    if local.is_empty() && record.kind == "profile" {
                        local.extend(context_issues(conn, &record));
                    }
                    let name = record
                        .value
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&record.key)
                        .to_owned();
                    ConfigurationRecoveryItem {
                        id: record.id,
                        kind: record.kind.into(),
                        name,
                        issues: local,
                        editable: record.kind != "history",
                        deletable: true,
                    }
                })
                .collect::<Vec<_>>();
            for item in &items {
                for issue in &item.issues {
                    issues.push(format!("{}: {issue}", item.name));
                }
            }
            issues.sort();
            issues.dedup();
            Ok(ConfigurationRecoveryReport {
                primary_readable: true,
                startup_error: self.startup_error.clone(),
                ready: issues.is_empty(),
                issues,
                items,
                last_backup: self
                    .last_backup
                    .lock()
                    .map_err(|_| "Recovery lock failed")?
                    .clone(),
            })
        })
    }
    // Read a repairable record with its JSON, revision and validation diagnostics
    pub fn document(&self, id: &str) -> Result<ConfigurationRecoveryDocument, String> {
        self.editor.read(|conn, revision| {
            let record = find(conn, id)?;
            let json = serde_json::to_string_pretty(&record.value).map_err(|e| e.to_string())?;
            let mut issues = record_issues(&record, &record.value);
            if issues.is_empty() && record.kind == "profile" {
                issues.extend(context_issues(conn, &record));
            }
            let diagnostics = Some(diagnostics(&record.kind, &json, &issues));
            Ok(ConfigurationRecoveryDocument {
                id: id.into(),
                revision: revision.into(),
                json,
                issues,
                diagnostics,
            })
        })
    }
    // Validate a candidate in a revision-checked preview transaction without persisting it
    pub fn validate(
        &self,
        id: &str,
        revision: &str,
        json: &str,
    ) -> Result<ConfigurationRecoveryValidation, String> {
        let value = match parse(json) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ConfigurationRecoveryValidation {
                    valid: false,
                    diagnostics: Some(diagnostics("json", json, std::slice::from_ref(&e))),
                    issues: vec![e],
                })
            }
        };
        self.editor.preview(revision, |conn| {
            let record = find(conn, id)?;
            let mut issues = record_issues(&record, &value);
            if issues.is_empty() {
                apply(conn, &record, Some(&value))?;
                if record.kind == "profile" {
                    issues.extend(context_issues(conn, &record));
                }
            }
            Ok(ConfigurationRecoveryValidation {
                valid: issues.is_empty(),
                diagnostics: Some(diagnostics(record.kind, json, &issues)),
                issues,
            })
        })
    }
    // Revalidate and commit the repaired record with a backup at the reviewed revision
    pub fn save(
        &self,
        id: &str,
        revision: &str,
        json: &str,
    ) -> Result<ConfigurationRecoveryReport, String> {
        let validation = self.validate(id, revision, json)?;
        if !validation.valid {
            return Err(validation.issues.join("\n"));
        }
        let value = parse(json)?;
        let (_, backup) = self.editor.commit(revision, |conn| {
            let record = find(conn, id)?;
            let issues = record_issues(&record, &value);
            if !issues.is_empty() {
                return Err(issues.join("\n"));
            }
            apply(conn, &record, Some(&value))?;
            let issues = if record.kind == "profile" {
                context_issues(conn, &record)
            } else {
                Vec::new()
            };
            if !issues.is_empty() {
                return Err(issues.join("\n"));
            }
            Ok(())
        })?;
        *self
            .last_backup
            .lock()
            .map_err(|_| "Recovery lock failed")? = Some(backup.display().to_string());
        self.report()
    }
    // Preview and commit the selected record deletion with a revision-checked backup
    pub fn delete(&self, id: &str, revision: &str) -> Result<ConfigurationRecoveryReport, String> {
        self.editor.preview(revision, |conn| {
            let record = find(conn, id)?;
            apply(conn, &record, None)?;
            Ok(())
        })?;
        let (_, backup) = self.editor.commit(revision, |conn| {
            let record = find(conn, id)?;
            apply(conn, &record, None)
        })?;
        *self
            .last_backup
            .lock()
            .map_err(|_| "Recovery lock failed")? = Some(backup.display().to_string());
        self.report()
    }
}
// Build bounded validation diagnostics with JSON locations where available
fn diagnostics(kind: &str, json: &str, issues: &[String]) -> Vec<ConfigurationRecoveryDiagnostic> {
    if json.len() > LIMIT {
        return issues
            .iter()
            .map(|message| ConfigurationRecoveryDiagnostic {
                message: message.clone(),
                anchor: false,
                pointer: None,
                line: None,
                column: None,
            })
            .collect();
    }
    let mut result = Vec::new();
    let parsed = serde_json::from_str::<Value>(json).ok();
    for issue in issues {
        let mut pointers = Vec::new();
        if let Some(pointer) = issue_pointer(issue) {
            pointers.push(pointer);
        }
        // Draft validator emits a list of fully qualified unsupported field paths
        if issue.starts_with("$.value: Unsupported saved binding draft fields:") {
            pointers.clear();
            for (start, _) in issue.match_indices("$.value[") {
                if let Some(pointer) = issue_pointer(&issue[start..]) {
                    if !pointers.contains(&pointer) {
                        pointers.push(pointer);
                    }
                }
            }
        }
        if pointers.is_empty() {
            pointers.push(String::new());
        }
        for pointer in pointers {
            result.push(ConfigurationRecoveryDiagnostic {
                message: issue.clone(),
                anchor: parsed
                    .as_ref()
                    .and_then(|value| value.pointer(&pointer))
                    .is_some_and(|value| value.is_object() || value.is_array()),
                pointer: (!pointer.is_empty()).then_some(pointer),
                line: None,
                column: None,
            });
        }
    }
    let error = match kind {
        "profile" => serde_json::from_str::<crate::profiles::Profile>(json).err(),
        "draft" => serde_json::from_str::<crate::workspace::Draft>(json).err(),
        _ => serde_json::from_str::<Value>(json).err(),
    };
    if let Some(error) = error {
        result.push(ConfigurationRecoveryDiagnostic {
            message: error.to_string(),
            anchor: error.to_string().starts_with("missing field ")
                || error
                    .to_string()
                    .contains("did not match any variant of untagged enum"),
            pointer: None,
            line: (error.line() > 0).then_some(error.line()),
            column: (error.column() > 0).then_some(error.column()),
        });
    }
    result
}

// Only validator-emitted rooted paths are mapped. Never search the document for
// a field name, which could select an unrelated occurrence in another action
fn issue_pointer(source: &str) -> Option<String> {
    let mut rest = source.strip_prefix('$')?;
    let mut pointer = String::new();
    loop {
        let segment;
        if let Some(after) = rest.strip_prefix('.') {
            let end = after
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(after.len());
            if end == 0 {
                break;
            }
            segment = after[..end].to_owned();
            rest = &after[end..];
        } else if let Some(after) = rest.strip_prefix('[') {
            let mut stream = serde_json::Deserializer::from_str(after).into_iter::<Value>();
            let value = stream.next()?.ok()?;
            let end = stream.byte_offset();
            rest = after.get(end..)?.strip_prefix(']')?;
            segment = match value {
                Value::String(value) => value,
                Value::Number(value) if value.is_u64() => value.to_string(),
                _ => return None,
            };
        } else {
            break;
        }
        pointer.push('/');
        pointer.push_str(&segment.replace('~', "~0").replace('/', "~1"));
    }
    (!pointer.is_empty()).then_some(pointer)
}

// Parse the supplied configuration recovery representation
fn parse(json: &str) -> Result<Value, String> {
    if json.len() > LIMIT {
        return Err("JSON exceeds the 16 MiB record limit".into());
    }
    serde_json::from_str(json)
        .map_err(|e| format!("JSON line {}, column {}: {e}", e.line(), e.column()))
}
// Read the requested configuration recovery data
fn read(conn: &Connection, domain: Domain, key: &str) -> Result<Option<Value>, String> {
    stream_dock_manager_storage::read_json(conn, domain, key, LIMIT)
}
// Enumerate the saved records explicitly supported by configuration repair
fn records(conn: &Connection) -> Result<Vec<Record>, String> {
    let profiles =
        stream_dock_manager_storage::list_json::<Value>(conn, Domain::Profiles, 4096, LIMIT)?;
    let mut records = profiles
        .into_iter()
        .map(|(key, value)| Record {
            id: format!("profile:{key}"),
            key,
            kind: "profile",
            value,
        })
        .collect::<Vec<_>>();
    records.extend(saved_items::inventory(conn)?);
    records.extend(settings::inventory(conn)?);
    if let Some(value) = read(conn, Domain::LocalPreferences, "drafts")? {
        if let Some(map) = value.as_object() {
            records.extend(map.iter().map(|(key, value)| Record {
                id: format!("draft:{key}"),
                key: key.clone(),
                kind: "draft",
                value: value.clone(),
            }));
        } else {
            records.push(Record {
                id: "drafts".into(),
                key: "drafts".into(),
                kind: "drafts",
                value,
            });
        }
    }
    for (key, value) in stream_dock_manager_storage::list_json::<Value>(
        conn,
        Domain::History,
        4096,
        32 * 1024 * 1024,
    )? {
        if crate::workspace::validate_recovery_history(value.clone()).is_err() {
            records.push(Record {
                id: format!("history:{key}"),
                key,
                kind: "history",
                value,
            });
        }
    }
    Ok(records)
}
// Find the requested configuration recovery entry
fn find(conn: &Connection, id: &str) -> Result<Record, String> {
    records(conn)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| "Recovery item no longer exists. Refresh the list.".into())
}
// Record issues
fn record_issues(record: &Record, value: &Value) -> Vec<String> {
    if record.kind == "setting" {
        return settings::issues(record, value);
    }
    let mut issues = Vec::new();
    if !value.is_object() {
        return vec!["$: expected a JSON object".into()];
    }
    match record.kind {
        "macro" => issues.extend(saved_items::issues(record, value)),
        "profile" => {
            // Expose useful binding paths even when a nested enum fails to decode
            if let Some(pages) = value.get("pages").and_then(Value::as_array) {
                for (index, page) in pages.iter().enumerate() {
                    if let Some(bindings) = page.get("bindings").and_then(Value::as_object) {
                        for (key, binding) in bindings {
                            let result =
                                serde_json::from_value::<crate::profiles::Binding>(binding.clone())
                                    .map_err(|e| e.to_string())
                                    .and_then(|b| b.validate());
                            if let Err(e) = result {
                                issues.push(format!("$.pages[{index}].bindings[{key:?}]: {e}"));
                            }
                        }
                    }
                }
            }
            match serde_json::from_value::<crate::profiles::Profile>(value.clone()) {
                Ok(profile) => {
                    if profile.id != record.key {
                        issues.push("$.id: profile identity cannot be changed".into());
                    }
                    if let Err(e) = profile.validate() {
                        issues.push(format!("$: {e}"));
                    }
                }
                Err(e) => issues.push(format!("$: {e}")),
            }
        }
        "draft" => match serde_json::from_value::<crate::workspace::Draft>(value.clone()) {
            Ok(draft) => {
                if let Err(e) = draft.validate() {
                    issues.push(format!("$.value: {e}"));
                }
                // Map key is a stable recovery identity, Draft.key may be a control key
            }
            Err(e) => issues.push(format!("$: {e}")),
        },
        "history" => {
            if let Err(error) = crate::workspace::validate_recovery_history(value.clone()) {
                issues.push(error);
            }
        }
        "drafts" => {
            for (key, value) in value.as_object().unwrap() {
                let item = Record {
                    id: String::new(),
                    key: key.clone(),
                    kind: "draft",
                    value: value.clone(),
                };
                issues.extend(
                    record_issues(&item, value)
                        .into_iter()
                        .map(|e| format!("draft {key:?}: {e}")),
                );
            }
        }
        _ => issues.push("Unsupported recovery item".into()),
    }
    issues.sort();
    issues.dedup();
    issues
}
// Validate the full application snapshot and saved profile graph
fn overall(conn: &Connection) -> Result<(), String> {
    crate::application_data::validate_snapshot(conn)?;
    if let Some(data) = crate::profile_database::load(conn)? {
        crate::profiles::validate_data(&data)?;
    }
    Ok(())
}
// Validate an edited profile’s dependencies independently of other damaged profiles
fn context_issues(conn: &Connection, record: &Record) -> Vec<String> {
    // Validate the edited profile's semantic dependencies independently. Other
    // invalid profiles must not prevent sequential repair; full admission still
    // validates the entire workspace, preferences and history in report()
    let result = (|| -> Result<(), String> {
        let profile: crate::profiles::Profile = serde_json::from_value(
            read(conn, Domain::Profiles, &record.key)?.ok_or("Profile missing")?,
        )
        .map_err(|e| e.to_string())?;
        let macros = stream_dock_manager_storage::list_json::<crate::builtins::SavedMacro>(
            conn,
            Domain::SavedMacros,
            4096,
            LIMIT,
        )?
        .into_iter()
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
        if let Some(deleted) = crate::profile_database::DELETED_MEDIA_RECORD.read_from(conn)? {
            if profile
                .background_asset
                .as_ref()
                .is_some_and(|id| deleted.assets.contains(id))
            {
                return Err("$.background_asset: this media was permanently deleted; choose an available asset".into());
            }
            for (index, page) in profile.pages.iter().enumerate() {
                for (key, binding) in &page.bindings {
                    if binding
                        .asset
                        .as_ref()
                        .is_some_and(|id| deleted.assets.contains(id))
                    {
                        return Err(format!("$.pages[{index}].bindings[{key:?}].asset: this media was permanently deleted; choose an available asset"));
                    }
                }
            }
        }
        for (index, page) in profile.pages.iter().enumerate() {
            for (key, binding) in &page.bindings {
                let validate = || -> Result<(), String> {
                    crate::macro_inputs::validate_action_calls(&binding.action, &macros, &[])?;
                    crate::keyboard_macro::validation::expanded_event_count(
                        &binding.action,
                        |id| {
                            macros
                                .iter()
                                .find(|item| item.id == id)
                                .map(|item| &item.action)
                        },
                    )?;
                    Ok(())
                };
                validate().map_err(|e| format!("$.pages[{index}].bindings[{key:?}]: {e}"))?;
            }
        }
        Ok(())
    })();
    result.err().into_iter().collect()
}

// Write the supplied configuration recovery data
fn write(conn: &Connection, domain: Domain, key: &str, value: &Value) -> Result<(), String> {
    stream_dock_manager_storage::write_json(conn, domain, key, value, LIMIT)
}
// Apply a candidate replacement or deletion within the caller-owned recovery transaction
fn apply(conn: &Connection, record: &Record, value: Option<&Value>) -> Result<(), String> {
    match record.kind {
        "macro" => saved_items::apply(conn, record, value)?,
        "setting" => settings::apply(conn, record, value)?,
        "draft" => {
            let mut drafts =
                read(conn, Domain::LocalPreferences, "drafts")?.ok_or("Drafts disappeared")?;
            let map = drafts.as_object_mut().ok_or("Draft map is invalid")?;
            if let Some(value) = value {
                map.insert(record.key.clone(), value.clone());
            } else {
                map.remove(&record.key);
            }
            write(conn, Domain::LocalPreferences, "drafts", &drafts)?;
        }
        "drafts" => {
            if let Some(value) = value {
                write(conn, Domain::LocalPreferences, "drafts", value)?;
            } else {
                stream_dock_manager_storage::delete(conn, Domain::LocalPreferences, "drafts")?;
            }
        }
        "history" => {
            if value.is_some() {
                return Err("Recovery history is read-only. Delete this invalid history entry or restore a backup.".into());
            }
            let mut header =
                read(conn, Domain::Workspace, "current")?.ok_or("Workspace metadata is missing")?;
            for direction in ["undo", "redo"] {
                header["document"]["history"][direction]
                    .as_array_mut()
                    .ok_or("Invalid history index")?
                    .retain(|v| v["id"] != record.key);
            }
            write(conn, Domain::Workspace, "current", &header)?;
            stream_dock_manager_storage::delete(conn, Domain::History, &record.key)?;
            stream_dock_manager_storage::delete(conn, Domain::HistoryPlugins, &record.key)?;
        }
        "profile" => {
            if let Some(value) = value {
                write(conn, Domain::Profiles, &record.key, value)?;
            } else {
                delete_profile(conn, &record.key)?;
            }
            let mut header =
                read(conn, Domain::Workspace, "current")?.ok_or("Workspace metadata is missing")?;
            let revision = header["document"]["configuration_revision"]
                .as_u64()
                .ok_or("Invalid configuration revision")?;
            header["document"]["configuration_revision"] = Value::from(
                revision
                    .checked_add(1)
                    .ok_or("Configuration revision exhausted")?,
            );
            write(conn, Domain::Workspace, "current", &header)?;
        }
        _ => return Err("Unsupported recovery item".into()),
    }
    Ok(())
}
// Delete profile
fn delete_profile(conn: &Connection, id: &str) -> Result<(), String> {
    let mut header =
        read(conn, Domain::Workspace, "current")?.ok_or("Workspace metadata is missing")?;
    let ids = header
        .get_mut("profiles")
        .and_then(Value::as_array_mut)
        .ok_or("Invalid profile index")?;
    ids.retain(|v| v.as_str() != Some(id));
    let replacement = ids.first().and_then(Value::as_str).unwrap_or("").to_owned();
    if header["document"]["active_profile"] == id {
        header["document"]["active_profile"] = Value::String(replacement);
    }
    write(conn, Domain::Workspace, "current", &header)?;
    stream_dock_manager_storage::delete(conn, Domain::Profiles, id)?;
    if let Some(mut rules) = read(conn, Domain::ProfileRules, "current")? {
        rules
            .as_array_mut()
            .ok_or("Invalid profile rules")?
            .retain(|r| r["profile_id"] != id);
        write(conn, Domain::ProfileRules, "current", &rules)?;
    }
    if let Some(mut sessions) = read(conn, Domain::LocalPreferences, "sessions")? {
        for session in sessions
            .as_object_mut()
            .ok_or("Invalid device sessions")?
            .values_mut()
        {
            if session["profile_id"] == id {
                session["profile_id"] = Value::String(String::new());
                session["page_id"] = Value::String(String::new());
            }
            if session["manual_profile"] == id {
                session["manual_profile"] = Value::String(String::new());
            }
            if let Some(map) = session
                .get_mut("remembered_pages")
                .and_then(Value::as_object_mut)
            {
                map.remove(id);
            }
        }
        write(conn, Domain::LocalPreferences, "sessions", &sessions)?;
    }
    if let Some(mut overrides) = read(conn, Domain::LocalPreferences, "parameter-overrides")? {
        if let Some(map) = overrides
            .get_mut("installation")
            .and_then(Value::as_object_mut)
        {
            map.remove(id);
        }
        if let Some(devices) = overrides.get_mut("devices").and_then(Value::as_object_mut) {
            for profiles in devices.values_mut() {
                if let Some(map) = profiles.as_object_mut() {
                    map.remove(id);
                }
            }
        }
        write(
            conn,
            Domain::LocalPreferences,
            "parameter-overrides",
            &overrides,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use stream_dock_manager_storage::{Storage, DATABASE_NAME};
    // Construct an isolated configuration recovery fixture
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open(root.path().into()).unwrap();
        storage
            .transaction(|conn| {
                crate::profile_database::save(conn, &crate::profiles::ProfileData::default())
            })
            .unwrap();
        root
    }
    // Store the selected configuration recovery value
    fn put(root: &std::path::Path, domain: Domain, key: &str, value: Value) {
        let storage = Storage::open(root.into()).unwrap();
        storage.write_json(domain, key, &value, LIMIT).unwrap();
    }
    #[test]
    // Verify that drafts repair independently validation is nonmutating and backup preserves original
    fn drafts_repair_independently_validation_is_nonmutating_and_backup_preserves_original() {
        let root = fixture();
        let bad = serde_json::json!({"key":"button","base":null,"value":{"actionDraft":{},"dialStackActions":[]}});
        put(
            root.path(),
            Domain::LocalPreferences,
            "drafts",
            serde_json::json!({"a":bad,"b":bad,"c":bad}),
        );
        let before = std::fs::read(root.path().join(DATABASE_NAME)).unwrap();
        let recovery = ConfigurationRecovery::open(root.path().into(), "fixture".into()).unwrap();
        assert!(!recovery.report().unwrap().ready);
        let doc = recovery.document("draft:a").unwrap();
        assert!(
            !recovery
                .validate(&doc.id, &doc.revision, "{")
                .unwrap()
                .valid
        );
        let good = r#"{"key":"button","base":null,"value":{}}"#;
        assert!(
            recovery
                .validate(&doc.id, &doc.revision, good)
                .unwrap()
                .valid
        );
        assert_eq!(
            before,
            std::fs::read(root.path().join(DATABASE_NAME)).unwrap()
        );
        let report = recovery.save(&doc.id, &doc.revision, good).unwrap();
        assert!(!report.ready);
        assert!(recovery.save(&doc.id, &doc.revision, good).is_err());
        let backup = Connection::open(report.last_backup.unwrap()).unwrap();
        assert_eq!(
            read(&backup, Domain::LocalPreferences, "drafts")
                .unwrap()
                .unwrap()["a"],
            bad
        );
        for id in ["draft:b", "draft:c"] {
            let doc = recovery.document(id).unwrap();
            recovery.delete(id, &doc.revision).unwrap();
        }
        assert!(recovery.report().unwrap().ready);
    }
    #[test]
    // Verify that profiles repair sequentially preserving identity and last profile can be deleted
    fn profiles_repair_sequentially_preserving_identity_and_last_profile_can_be_deleted() {
        let root = fixture();
        let storage = Storage::open(root.path().into()).unwrap();
        let mut data = crate::profiles::ProfileData::default();
        let mut second = data.profiles[0].clone();
        second.id = "second".into();
        data.profiles.push(second);
        let to_save = data.clone();
        storage
            .transaction(move |c| crate::profile_database::save(c, &to_save))
            .unwrap();
        drop(storage);
        let originals = data
            .profiles
            .iter()
            .map(|p| serde_json::to_value(p).unwrap())
            .collect::<Vec<_>>();
        for original in &originals {
            let mut bad = original.clone();
            bad["pages"] = Value::String("invalid".into());
            put(
                root.path(),
                Domain::Profiles,
                original["id"].as_str().unwrap(),
                bad,
            );
        }
        let before = std::fs::read(root.path().join(DATABASE_NAME)).unwrap();
        let recovery =
            ConfigurationRecovery::open(root.path().into(), "bad profile".into()).unwrap();
        for original in &originals {
            let id = format!("profile:{}", original["id"].as_str().unwrap());
            let doc = recovery.document(&id).unwrap();
            let mut other = original.clone();
            other["id"] = Value::String("different".into());
            assert!(
                !recovery
                    .validate(&id, &doc.revision, &other.to_string())
                    .unwrap()
                    .valid
            );
            if original == &originals[0] {
                assert_eq!(
                    before,
                    std::fs::read(root.path().join(DATABASE_NAME)).unwrap()
                );
            }
            recovery
                .save(&id, &doc.revision, &original.to_string())
                .unwrap();
        }
        assert!(recovery.report().unwrap().ready);
        for original in originals {
            let id = format!("profile:{}", original["id"].as_str().unwrap());
            let doc = recovery.document(&id).unwrap();
            recovery.delete(&id, &doc.revision).unwrap();
        }
        assert!(recovery.report().unwrap().ready);
    }
    #[test]
    // Verify that invalid draft envelope is editable without destroying source
    fn invalid_draft_envelope_is_editable_without_destroying_source() {
        let root = fixture();
        put(
            root.path(),
            Domain::LocalPreferences,
            "drafts",
            serde_json::json!({"bad":{"value":{}}}),
        );
        let before = std::fs::read(root.path().join(DATABASE_NAME)).unwrap();
        let recovery = ConfigurationRecovery::open(root.path().into(), "fixture".into()).unwrap();
        let doc = recovery.document("draft:bad").unwrap();
        assert!(doc.issues.iter().any(|e| e.contains("key")));
        assert!(recovery.save(&doc.id, &doc.revision, &doc.json).is_err());
        assert_eq!(
            before,
            std::fs::read(root.path().join(DATABASE_NAME)).unwrap()
        );
    }
    #[test]
    // Verify that mixed invalid profile draft and history are explicit independent repairs
    fn mixed_invalid_profile_draft_and_history_are_explicit_independent_repairs() {
        let root = fixture();
        let data = crate::profiles::ProfileData::default();
        let original = serde_json::to_value(&data.profiles[0]).unwrap();
        let mut bad = original.clone();
        bad["pages"] = Value::Null;
        put(root.path(), Domain::Profiles, &data.profiles[0].id, bad);
        put(
            root.path(),
            Domain::LocalPreferences,
            "drafts",
            serde_json::json!({"a":{"key":"button","base":null,"value":{"actionDraft":{},"dialStackIndex":0}}}),
        );
        let recovery = ConfigurationRecovery::open(root.path().into(), "fixture".into()).unwrap();
        let draft = recovery.document("draft:a").unwrap();
        recovery
            .save(
                &draft.id,
                &draft.revision,
                r#"{"key":"button","base":null,"value":{}}"#,
            )
            .unwrap();
        assert!(!recovery.report().unwrap().ready);
        let id = format!("profile:{}", data.profiles[0].id);
        let profile = recovery.document(&id).unwrap();
        recovery
            .save(&id, &profile.revision, &original.to_string())
            .unwrap();
        assert!(recovery.report().unwrap().ready);
        drop(recovery);
        let storage = Storage::open(root.path().into()).unwrap();
        let history_id = uuid::Uuid::new_v4().to_string();
        let key = history_id.clone();
        storage.transaction(move |conn| {
            write(conn,Domain::History,&key,&serde_json::json!({}))?;
            let mut header=read(conn,Domain::Workspace,"current")?.unwrap();
            header["document"]["history"]["undo"]=serde_json::json!([{"id":key,"description":"Broken history","timestamp_ms":0,"bytes":2}]);
            write(conn,Domain::Workspace,"current",&header)
        }).unwrap();
        drop(storage);
        let recovery = ConfigurationRecovery::open(root.path().into(), "fixture".into()).unwrap();
        let report = recovery.report().unwrap();
        assert!(!report.ready);
        let id = format!("history:{history_id}");
        assert!(report
            .items
            .iter()
            .any(|item| item.id == id && !item.editable && item.deletable));
        let doc = recovery.document(&id).unwrap();
        assert!(recovery.delete(&id, &doc.revision).unwrap().ready);
    }
    #[test]
    // Verify that semantic faults in other profiles do not block individual repairs
    fn semantic_faults_in_other_profiles_do_not_block_individual_repairs() {
        let root = fixture();
        let storage = Storage::open(root.path().into()).unwrap();
        let mut data = crate::profiles::ProfileData::default();
        let mut second = data.profiles[0].clone();
        second.id = "second".into();
        data.profiles.push(second);
        for profile in &mut data.profiles {
            let action = crate::actions::Action::Builtin {
                action: Box::new(crate::builtins::BuiltinAction::TypeText {
                    text: "${input.undeclared}".into(),
                    template: false,
                }),
            };
            profile.pages[0].bindings.insert(
                "key-1".into(),
                serde_json::from_value(
                    serde_json::json!({"title":"Input","color":"#000000","action":action}),
                )
                .unwrap(),
            );
            assert!(profile.validate().is_ok());
        }
        assert!(crate::profiles::validate_data(&data).is_err());
        let original = data.clone();
        storage
            .transaction(move |conn| crate::profile_database::save(conn, &data))
            .unwrap();
        drop(storage);
        let recovery = ConfigurationRecovery::open(root.path().into(), "fixture".into()).unwrap();
        for mut profile in original.profiles {
            let id = format!("profile:{}", profile.id);
            let doc = recovery.document(&id).unwrap();
            assert!(
                !recovery
                    .validate(&id, &doc.revision, &doc.json)
                    .unwrap()
                    .valid
            );
            profile.pages[0].bindings.clear();
            recovery
                .save(
                    &id,
                    &doc.revision,
                    &serde_json::to_string(&profile).unwrap(),
                )
                .unwrap();
        }
        assert!(recovery.report().unwrap().ready);
    }
    #[test]
    // Verify that diagnostics map exact paths and keep unlocated messages unmapped
    fn diagnostics_map_exact_paths_and_keep_unlocated_messages_unmapped() {
        let issues=vec![r#"$.value: Unsupported saved binding draft fields: $.value["dialStackActions"], $.value["dialStackIndex"]"#.into()];
        let found = diagnostics(
            "draft",
            r#"{"key":"é","base":null,"value":{"dialStackActions":[],"dialStackIndex":0}}"#,
            &issues,
        );
        assert_eq!(
            found
                .iter()
                .filter_map(|d| d.pointer.as_deref())
                .collect::<Vec<_>>(),
            vec!["/value/dialStackActions", "/value/dialStackIndex"]
        );
        assert_eq!(
            issue_pointer(r#"$.pages[2].bindings["a/b~c"]: invalid"#).as_deref(),
            Some("/pages/2/bindings/a~1b~0c")
        );
        assert!(issue_pointer("unrelated $.name").is_none());
        let found = diagnostics(
            "json",
            "{}",
            &["A global error mentioning $.name is not a location".into()],
        );
        assert!(found
            .iter()
            .all(|d| d.pointer.is_none() && d.line.is_none()));
    }
    #[test]
    // Verify that serde diagnostic locations use exact source and missing field anchor
    fn serde_diagnostic_locations_use_exact_source_and_missing_field_anchor() {
        let source = "{\n  \"value\": {},\n  \"base\": null\n}";
        let error = serde_json::from_str::<crate::workspace::Draft>(source).unwrap_err();
        let found = diagnostics("draft", source, &[]);
        assert_eq!(found[0].line, Some(error.line()));
        assert_eq!(found[0].column, Some(error.column()));
        assert!(found[0].anchor);
        let source = r#"{"é":"😀","bad": }"#;
        let error = serde_json::from_str::<Value>(source).unwrap_err();
        let found = diagnostics("json", source, &[]);
        assert_eq!(found[0].column, Some(error.column()));
        assert!(!found[0].anchor);
    }
    #[test]
    // Verify that oversized diagnostics do not parse or claim locations
    fn oversized_diagnostics_do_not_parse_or_claim_locations() {
        let text = "x".repeat(LIMIT + 1);
        let found = diagnostics("draft", &text, &["JSON exceeds limit".into()]);
        assert_eq!(found.len(), 1);
        assert!(found[0].line.is_none());
        assert!(found[0].pointer.is_none());
    }
    #[test]
    // Verify that startup routing is read only and safe continue intent is consumed once
    fn startup_routing_is_read_only_and_safe_continue_intent_is_consumed_once() {
        let root = fixture();
        assert!(!startup_landing(root.path(), false).unwrap());
        assert!(startup_landing(root.path(), true).unwrap());
        put(
            root.path(),
            Domain::LocalPreferences,
            "restart-recovery-once",
            Value::Bool(true),
        );
        let before = std::fs::read(root.path().join(DATABASE_NAME)).unwrap();
        assert!(startup_landing(root.path(), false).unwrap());
        assert!(startup_landing(root.path(), false).unwrap());
        assert_eq!(
            before,
            std::fs::read(root.path().join(DATABASE_NAME)).unwrap()
        );
        let recovery = ConfigurationRecovery::open(root.path().into(), "explicit".into()).unwrap();
        recovery.request_restart(true).unwrap();
        drop(recovery);
        assert!(!startup_landing(root.path(), false).unwrap());
        assert!(!startup_landing(root.path(), true).unwrap());
        let storage = Storage::open(root.path().into()).unwrap();
        assert_eq!(
            crate::take_restart_recovery_request(&storage).unwrap(),
            Some(true)
        );
        assert_eq!(
            crate::take_restart_recovery_request(&storage).unwrap(),
            None
        );
        drop(storage);
        assert!(startup_landing(root.path(), true).unwrap());
        assert!(!startup_landing(root.path(), false).unwrap());
        let recovery = ConfigurationRecovery::open(root.path().into(), "explicit".into()).unwrap();
        recovery.request_restart(false).unwrap();
        drop(recovery);
        assert!(!startup_landing(root.path(), true).unwrap());
    }
    #[test]
    // Verify that invalid configuration cannot request either exit and routing preserves originals
    fn invalid_configuration_cannot_request_either_exit_and_routing_preserves_originals() {
        let root = fixture();
        put(
            root.path(),
            Domain::LocalPreferences,
            "drafts",
            serde_json::json!({"broken":{}}),
        );
        put(
            root.path(),
            Domain::LocalPreferences,
            "restart-recovery-once",
            Value::Bool(true),
        );
        let before = std::fs::read(root.path().join(DATABASE_NAME)).unwrap();
        assert!(startup_landing(root.path(), false).unwrap());
        let recovery = ConfigurationRecovery::open(root.path().into(), "bad".into()).unwrap();
        assert!(recovery.request_restart(true).is_err());
        assert!(recovery.request_restart(false).is_err());
        assert_eq!(
            before,
            std::fs::read(root.path().join(DATABASE_NAME)).unwrap()
        );
    }
    #[test]
    // Verify that fresh explicit recovery initializes schema and recreates missing primary
    fn fresh_explicit_recovery_initializes_schema_and_recreates_missing_primary() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("fresh");
        assert!(!startup_landing(&root, false).unwrap());
        assert!(startup_landing(&root, true).unwrap());
        assert!(!root.exists());
        let recovery =
            ConfigurationRecovery::open_requested(root.clone(), "requested".into()).unwrap();
        assert!(recovery.report().unwrap().ready);
        assert!(recovery.report().unwrap().items.is_empty());
        drop(recovery);
        std::fs::remove_file(root.join(DATABASE_NAME)).unwrap();
        assert!(!startup_landing(&root, false).unwrap());
        let recovery =
            ConfigurationRecovery::open_requested(root.clone(), "requested".into()).unwrap();
        assert!(recovery.report().unwrap().ready);
        assert!(root.join(DATABASE_NAME).exists());
    }
    #[test]
    // Verify that missing primary without backups starts normally and preserves legacy evidence
    fn missing_primary_without_backups_starts_normally_and_preserves_legacy_evidence() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("profiles")).unwrap();
        let legacy = root.path().join("profiles/profiles.json");
        std::fs::write(&legacy, b"legacy evidence, not a migration source").unwrap();
        std::fs::write(
            root.path().join(".stream-dock-manager-sqlite-initialized"),
            b"previous install",
        )
        .unwrap();
        assert!(!startup_landing(root.path(), false).unwrap());
        let runtime = crate::Runtime::build(root.path().into(), false).unwrap();
        assert!(root.path().join(DATABASE_NAME).exists());
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            b"legacy evidence, not a migration source"
        );
        drop(runtime);
    }

    #[test]
    // Verify that missing primary with backup retains chooser until explicit continue
    fn missing_primary_with_backup_retains_chooser_until_explicit_continue() {
        let root = fixture();
        let storage = Storage::open(root.path().into()).unwrap();
        let backup = storage.backup_recovery().unwrap();
        drop(storage);
        let before = std::fs::read(&backup.path).unwrap();
        std::fs::remove_file(root.path().join(DATABASE_NAME)).unwrap();
        assert!(startup_landing(root.path(), false).unwrap());
        let recovery =
            ConfigurationRecovery::open_requested(root.path().into(), String::new()).unwrap();
        assert!(recovery.report().unwrap().ready);
        assert!(recovery.report().unwrap().items.is_empty());
        assert_eq!(std::fs::read(&backup.path).unwrap(), before);
        drop(recovery);
        assert!(startup_landing(root.path(), false).unwrap());
        let recovery = ConfigurationRecovery::open(root.path().into(), String::new()).unwrap();
        recovery.request_restart(false).unwrap();
        drop(recovery);
        assert!(!startup_landing(root.path(), false).unwrap());
        assert_eq!(std::fs::read(&backup.path).unwrap(), before);
    }

    #[test]
    // Verify that failed runtime construction preserves restart intent
    fn failed_runtime_construction_preserves_restart_intent() {
        let root = fixture();
        put(
            root.path(),
            Domain::LocalPreferences,
            "restart-recovery-once",
            Value::Bool(true),
        );
        std::fs::create_dir_all(root.path().join("profiles")).unwrap();
        std::fs::write(root.path().join("profiles/assets"), "blocks initialization").unwrap();
        assert!(crate::Runtime::build(root.path().into(), false).is_err());
        assert!(startup_landing(root.path(), false).unwrap());
        let storage = Storage::open(root.path().into()).unwrap();
        assert_eq!(
            storage
                .read_json::<bool>(Domain::LocalPreferences, "restart-recovery-once", 16)
                .unwrap(),
            Some(true)
        );
    }
}
