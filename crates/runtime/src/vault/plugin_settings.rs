use super::*;
use crate::Runtime;
use serde_json::json;
use std::collections::BTreeMap;
use stream_dock_manager_storage::plugin_settings as records;

#[derive(Serialize, ts_rs::TS)]
pub struct VaultSettingsRow {
    pub revision: String,
    pub group: String,
    pub action: String,
    pub location: String,
}
#[derive(Serialize, ts_rs::TS)]
pub struct VaultSettingsPlugin {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub dialect: Option<String>,
    pub native_package: bool,
    pub rows: Vec<VaultSettingsRow>,
    pub settings_review: Option<String>,
}
#[derive(Serialize, ts_rs::TS)]
pub struct VaultSettingsDocument {
    pub json_type: String,
    pub fields: BTreeMap<String, String>,
    pub json: Option<String>,
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
impl Vault {
    fn while_unlocked<T>(
        &self,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self.current_state();
        guard.session.as_ref().ok_or("vault_locked")?;
        operation()
    }
}
impl Runtime {
    pub fn vault_settings_plugins(&self) -> Result<Vec<VaultSettingsPlugin>, String> {
        let host = self.plugins.lock().unwrap();
        self.vault.while_unlocked(|| {
            let installed = host
                .list()
                .into_iter()
                .map(|p| (p.id.clone(), p))
                .collect::<BTreeMap<_, _>>();
            let mut plugins = BTreeMap::new();
            for entry in host.stored_data()? {
                let id = entry.plugin_id;
                let info = installed.get(&id);
                let family = match info {
                    Some(p) => Some((p.dialect.clone(), p.capabilities.is_some())),
                    None => self.profiles.storage.read_json::<(String, bool)>(
                        Domain::PluginMetadata,
                        &format!("retained-family:{id}"),
                        1024,
                    )?,
                };
                plugins.insert(
                    id.clone(),
                    VaultSettingsPlugin {
                        id,
                        name: entry.name,
                        installed: entry.installed,
                        native_package: family.as_ref().is_some_and(|(_, native)| *native),
                        dialect: family.map(|(dialect, _)| dialect),
                        rows: vec![],
                        settings_review: None,
                    },
                );
            }
            for s in host.storage().run(|db| records::settings(db, None))? {
                let plugin =
                    plugins
                        .entry(s.owner.clone())
                        .or_insert_with(|| VaultSettingsPlugin {
                            id: s.owner.clone(),
                            name: s.owner.clone(),
                            installed: false,
                            dialect: None,
                            native_package: false,
                            rows: vec![],
                            settings_review: None,
                        });
                let action = s
                    .action
                    .map(|id| {
                        installed
                            .get(&s.owner)
                            .and_then(|p| p.actions.iter().find(|a| a.id == id))
                            .map(|a| a.name.clone())
                            .unwrap_or(id)
                    })
                    .unwrap_or_default();
                let group = match s.domain {
                    Domain::PluginGlobals => "Global settings",
                    Domain::PluginDefaults => "Action defaults",
                    _ => "Action instances",
                };
                plugin.rows.push(VaultSettingsRow {
                    revision: s.id,
                    group: group.into(),
                    action,
                    location: s.location,
                });
            }
            host.storage().run(move |db| {
                for plugin in plugins.values_mut() {
                    plugin.settings_review = settings_review(
                        db,
                        &plugin.id,
                        plugin.rows.iter().map(|row| row.revision.clone()).collect(),
                    )?;
                }
                Ok(plugins.into_values().collect())
            })
        })
    }
    pub fn vault_settings_document(
        &self,
        id: String,
        reveal: bool,
    ) -> Result<VaultSettingsDocument, String> {
        self.vault.while_unlocked(|| {
            self.profiles.storage.run(move |db| {
                let s = records::setting(db, &id)?;
                let value = records::setting_value(db, &s)?;
                let fields = value
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, v)| (k.clone(), json_type(v).into()))
                    .collect();
                Ok(VaultSettingsDocument {
                    json_type: json_type(&value).into(),
                    fields,
                    json: if reveal {
                        Some(serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?)
                    } else {
                        None
                    },
                })
            })
        })
    }
    pub fn vault_save_settings(&self, id: String, value: Value) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        let gate = self.profiles.transaction.lock().unwrap();
        let host = self.plugins.lock().unwrap();
        let changed = self.vault.while_unlocked(|| {
            let revision = id.clone();
            let s = self
                .profiles
                .storage
                .run(move |db| records::setting(db, &revision))?;
            if matches!(s.domain, Domain::PluginGlobals | Domain::PluginDefaults) {
                host.edit_vault_setting(&id, value)?;
                return Ok(false);
            }
            let dialect = host
                .list()
                .into_iter()
                .find(|p| p.id == s.owner)
                .map(|p| p.dialect);
            let dialect = match dialect {
                Some(family) => Some(family),
                None => self
                    .profiles
                    .storage
                    .read_json::<(String, bool)>(
                        Domain::PluginMetadata,
                        &format!("retained-family:{}", s.owner),
                        1024,
                    )?
                    .map(|(family, _)| family),
            };
            if value.to_string().len() > 256 * 1024
                || (dialect.as_deref().is_some_and(|d| d != "openaction") && !value.is_object())
            {
                return Err(
                    "The settings must match the plugin’s JSON format and fit within 256 KiB."
                        .into(),
                );
            }
            let data = self.profiles.edit_under_transaction(
                state.configuration_revision,
                "Edit plugin action settings",
                |data| {
                    if s.domain == Domain::Profiles {
                        let p = data
                            .profiles
                            .iter_mut()
                            .find(|p| p.id == s.key)
                            .ok_or("Profile was removed")?;
                        let mut document = json!(p);
                        *document
                            .pointer_mut(&s.pointer)
                            .ok_or("Action was removed")? = value;
                        *p = serde_json::from_value(document).map_err(|e| e.to_string())?;
                        let parts: Vec<_> = s.pointer.split('/').collect();
                        if parts.get(1) == Some(&"pages") && parts.get(3) == Some(&"bindings") {
                            let page = p
                                .pages
                                .get(parts[2].parse::<usize>().map_err(|_| "Invalid page")?)
                                .ok_or("Page was removed")?
                                .id
                                .clone();
                            p.sync_link(&page, &parts[4].replace("~1", "/").replace("~0", "~"))?;
                        }
                    } else {
                        let m = data
                            .saved_macros
                            .iter_mut()
                            .find(|m| m.id == s.key)
                            .ok_or("Macro was removed")?;
                        let mut document = json!(m);
                        *document
                            .pointer_mut(&s.pointer)
                            .ok_or("Action was removed")? = value;
                        *m = serde_json::from_value(document).map_err(|e| e.to_string())?;
                    }
                    Ok(())
                },
            )?;
            Self::apply_data(&mut state, data);
            Ok(true)
        })?;
        drop(host);
        drop(gate);
        drop(state);
        if changed {
            self.finish_workspace_commit();
        }
        Ok(())
    }
}

// Review only opaque setting revisions and private-store namespace CAS versions
fn settings_review(
    db: &stream_dock_manager_storage::rusqlite::Connection,
    owner: &str,
    mut revisions: Vec<String>,
) -> Result<Option<String>, String> {
    use sha2::{Digest, Sha256};
    let mut query = db.prepare("SELECT namespace,generation,revision FROM settings_vault.plugin_storage_namespaces WHERE plugin_id=?1 ORDER BY namespace")
        .map_err(|error| error.to_string())?;
    let namespaces = query
        .query_map([owner], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, [u8; 8]>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if revisions.is_empty() && namespaces.is_empty() {
        return Ok(None);
    }
    revisions.sort_unstable();
    let generation = stream_dock_manager_plugin_host::plugin_storage_generation(db, owner)?;
    let encoded = serde_json::to_vec(&(owner, generation, revisions, namespaces))
        .map_err(|error| error.to_string())?;
    Ok(Some(hex::encode(Sha256::digest(encoded))))
}

fn draft_mentions_owner(value: &Value, owner: &str) -> bool {
    match value {
        Value::Object(map) => {
            if map.get("kind").and_then(Value::as_str) == Some("plugin") {
                return map.get("plugin_id").and_then(Value::as_str) == Some(owner);
            }
            map.iter().any(|(key, value)| {
                if key == "rawValue" {
                    return value.as_str().is_some_and(|raw| {
                        !raw.trim().is_empty()
                            && serde_json::from_str::<Value>(raw)
                                .map_or(true, |action| draft_mentions_owner(&action, owner))
                    });
                }
                draft_mentions_owner(value, owner)
            })
        }
        Value::Array(items) => items.iter().any(|item| draft_mentions_owner(item, owner)),
        _ => false,
    }
}

fn clear_owner_action(action: &mut crate::actions::Action, owner: &str) -> bool {
    let mut matched = false;
    action.visit_mut(&mut |action| {
        if let crate::actions::Action::Plugin {
            plugin_id,
            settings,
            ..
        } = action
        {
            if plugin_id == owner {
                *settings = json!({});
                matched = true;
            }
        }
    });
    matched
}

fn clear_owner_workspace(data: &mut crate::ProfileData, owner: &str) -> bool {
    let mut changed = false;
    for binding in data
        .profiles
        .iter_mut()
        .flat_map(|profile| &mut profile.pages)
        .flat_map(|page| page.bindings.values_mut())
    {
        changed |= clear_owner_action(&mut binding.action, owner);
    }
    for saved in &mut data.saved_macros {
        changed |= clear_owner_action(&mut saved.action, owner);
    }
    changed
}

fn clear_owner_data(
    tx: &stream_dock_manager_storage::rusqlite::Connection,
    data: &mut crate::ProfileData,
    owner: &str,
) -> Result<bool, String> {
    use stream_dock_manager_storage::{write_json, MAX_RECORD_BYTES};
    let mut changed = false;
    for profile in &mut data.profiles {
        let profile_changed = profile
            .pages
            .iter_mut()
            .flat_map(|page| page.bindings.values_mut())
            .fold(false, |changed, binding| {
                clear_owner_action(&mut binding.action, owner) | changed
            });
        if profile_changed {
            // Force retirement even if an older installation stored an encrypted empty object
            write_json(tx, Domain::Profiles, &profile.id, profile, MAX_RECORD_BYTES)?;
            changed = true;
        }
    }
    for saved in &mut data.saved_macros {
        if clear_owner_action(&mut saved.action, owner) {
            write_json(tx, Domain::SavedMacros, &saved.id, saved, MAX_RECORD_BYTES)?;
            changed = true;
        }
    }
    changed |= crate::workspace::rewrite_history(tx, &mut data.history, owner, true, |history| {
        clear_owner_workspace(history, owner)
    })?;
    Ok(changed)
}

fn clear_document(
    tx: &stream_dock_manager_storage::rusqlite::Connection,
    data: &mut crate::ProfileData,
    setting: &records::Setting,
) -> Result<(), String> {
    use stream_dock_manager_storage::{write_json, MAX_RECORD_BYTES};
    if setting.domain == Domain::Profiles {
        let profile = data
            .profiles
            .iter_mut()
            .find(|profile| profile.id == setting.key)
            .ok_or("Profile was removed")?;
        let mut document = json!(profile);
        *document
            .pointer_mut(&setting.pointer)
            .ok_or("Action was removed")? = json!({});
        *profile = serde_json::from_value(document).map_err(|error| error.to_string())?;
        let mut path = setting.pointer.split('/').skip(1);
        if let (Some("pages"), Some(page), Some("bindings"), Some(control)) =
            (path.next(), path.next(), path.next(), path.next())
        {
            let page = profile
                .pages
                .get(page.parse::<usize>().map_err(|_| "Invalid page")?)
                .ok_or("Page was removed")?
                .id
                .clone();
            profile.sync_link(&page, &control.replace("~1", "/").replace("~0", "~"))?;
        }
        write_json(tx, setting.domain, &setting.key, profile, MAX_RECORD_BYTES)?;
    } else if setting.domain == Domain::SavedMacros {
        let saved = data
            .saved_macros
            .iter_mut()
            .find(|saved| saved.id == setting.key)
            .ok_or("Macro was removed")?;
        let mut document = json!(saved);
        *document
            .pointer_mut(&setting.pointer)
            .ok_or("Action was removed")? = json!({});
        *saved = serde_json::from_value(document).map_err(|error| error.to_string())?;
        write_json(tx, setting.domain, &setting.key, saved, MAX_RECORD_BYTES)?;
    } else {
        return Err("Not a saved action setting".into());
    }
    Ok(())
}

enum DeleteSettings {
    Document(String),
    Plugin { owner: String, review: String },
}

impl Runtime {
    pub fn vault_delete_settings(&self, id: String) -> Result<(), String> {
        self.delete_vault_settings(DeleteSettings::Document(id))
    }

    pub fn vault_delete_plugin_settings(
        &self,
        owner: String,
        review: String,
    ) -> Result<(), String> {
        self.delete_vault_settings(DeleteSettings::Plugin { owner, review })
    }

    fn delete_vault_settings(&self, selection: DeleteSettings) -> Result<(), String> {
        use stream_dock_manager_plugin_host::VaultSettingsClear;
        let mut state = self.state.lock().unwrap();
        let gate = self.profiles.transaction.lock().unwrap();
        let drafts = self.drafts.lock().unwrap();
        let host = self.plugins.lock().unwrap();
        let changed = self.vault.while_unlocked(|| {
            let owner = match &selection {
                DeleteSettings::Document(revision) => {
                    uuid::Uuid::parse_str(revision).map_err(|_| "Invalid setting revision")?;
                    let revision = revision.clone();
                    self.profiles.storage.run(move |db| records::setting(db, &revision))?.owner
                }
                DeleteSettings::Plugin { owner, review } => {
                    if review.len() != 64 || !review.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err("Invalid settings review".into());
                    }
                    owner.clone()
                }
            };
            if drafts.values().any(|draft| {
                draft_mentions_owner(&draft.value, &owner)
                    || draft.base.as_ref().is_some_and(|base| {
                        let mut uses = false;
                        base.action.visit(&mut |action| {
                            uses |= matches!(action, crate::actions::Action::Plugin { plugin_id, .. } if *plugin_id == owner);
                        });
                        uses
                    })
            }) {
                return Err("Save or discard this plugin's binding drafts before deleting settings.".into());
            }
            let id = owner.clone();
            let revision = state.configuration_revision;
            let updated = host.clear_vault_settings(&owner, move |tx| {
                let (scope, setting) = match selection {
                    DeleteSettings::Document(token) => {
                        let setting = records::setting(tx, &token)?;
                        if setting.owner != id {
                            return Err("Settings owner changed".into());
                        }
                        let scope = match setting.domain {
                            Domain::PluginGlobals => VaultSettingsClear::Global,
                            Domain::PluginDefaults => VaultSettingsClear::Default(
                                setting.action.clone().ok_or("Missing action defaults")?,
                            ),
                            Domain::Profiles | Domain::SavedMacros => VaultSettingsClear::Binding,
                            _ => return Err("Unsupported setting".into()),
                        };
                        (scope, Some(setting))
                    }
                    DeleteSettings::Plugin { review, .. } => {
                        let revisions = records::owner_revisions(tx, &id)?;
                        if settings_review(tx, &id, revisions)?.as_deref() != Some(&review) {
                            return Err("Saved plugin data changed. Refresh and review before deleting.".into());
                        }
                        (VaultSettingsClear::All, None)
                    }
                };
                let mut updated = None;
                if matches!(scope, VaultSettingsClear::Binding | VaultSettingsClear::All) {
                    let mut data = crate::profile_database::load(tx)?.ok_or("Workspace unavailable")?;
                    if data.configuration_revision != revision {
                        return Err("Workspace changed. Refresh and review before deleting.".into());
                    }
                    let changed = if let Some(setting) = &setting {
                        clear_document(tx, &mut data, setting)?;
                        true
                    } else {
                        clear_owner_data(tx, &mut data, &id)?
                    };
                    if changed {
                        data.configuration_revision = revision.checked_add(1)
                            .ok_or("Workspace revision limit reached")?;
                        data.update_requirements();
                        crate::profiles::validate_data(&data)?;
                        crate::profile_database::save(tx, &data)?;
                        updated = Some(data);
                    }
                }
                Ok((updated, scope))
            })?;
            if let Some(data) = updated {
                Self::apply_data(&mut state, data);
                return Ok(true);
            }
            Ok(false)
        })?;
        drop(host);
        drop(drafts);
        drop(gate);
        drop(state);
        if changed {
            self.finish_workspace_commit();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{actions::Action, workspace::Edit};
    use stream_dock_manager_storage::{rusqlite::params, Storage};

    const OWNER: &str = "fixture.vault-settings";
    const FOREIGN: &str = "fixture.foreign-settings";

    fn fixture() -> (tempfile::TempDir, std::sync::Arc<Runtime>) {
        let root = tempfile::tempdir().unwrap();
        for owner in [OWNER, FOREIGN] {
            let package = root.path().join(format!("plugins/{owner}.sdPlugin"));
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(package.join("runtime.html"), "<title>Fixture</title>").unwrap();
            std::fs::write(package.join("manifest.json"), json!({
                "UUID": owner, "Name": "Fixture", "Version": "0.1.0", "CodePath": "runtime.html",
                "Actions": [{"UUID":format!("{owner}.press"),"Name":"Press"}]
            }).to_string()).unwrap();
            crate::test_plugin::modernize(&package);
        }
        let storage = Storage::open(root.path().to_owned()).unwrap();
        for owner in [OWNER, FOREIGN] {
            storage
                .write_json(
                    Domain::PluginGlobals,
                    owner,
                    &json!({"fixture":owner}),
                    4096,
                )
                .unwrap();
            storage
                .write_json(
                    Domain::PluginDefaults,
                    owner,
                    &json!({format!("{owner}.press"):{"default":true}}),
                    4096,
                )
                .unwrap();
        }
        storage
            .transaction(|tx| {
                for owner in [OWNER, FOREIGN] {
                    tx.execute(
                        "INSERT INTO settings_vault.plugin_storage_namespaces VALUES(?1,'fixture','0',X'0000000000000001')",
                        [owner],
                    )
                    .map_err(|error| error.to_string())?;
                    tx.execute(
                        "INSERT INTO settings_vault.plugin_storage_records VALUES(?1,'fixture','value','true',9)",
                        [owner],
                    )
                    .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
            .unwrap();
        drop(storage);
        let runtime = Runtime::build_credentials(
            root.path().into(),
            false,
            crate::credentials::Credentials::with_provider(Arc::new(
                crate::vault::tests::TestKeys::default(),
            )),
        )
        .unwrap();
        runtime.vault.initialize().unwrap();
        let snapshot = runtime.snapshot();
        let mut profile = snapshot.profiles[0].clone();
        let action = |owner: &str| Action::Plugin {
            plugin_id: owner.into(),
            action_id: format!("{owner}.press"),
            settings: json!({"binding":owner}),
            resources: Default::default(),
            user_desired_state: None,
        };
        let first = profile.pages[0].bindings.get_mut("key-1").unwrap();
        first.action = action(OWNER);
        first.custom_background = true;
        first.color = "#345678".into();
        profile.pages[0].bindings.get_mut("key-2").unwrap().action = action(FOREIGN);
        runtime
            .apply_edits(
                snapshot.configuration_revision,
                "Seed saved plugin settings",
                vec![
                    Edit::SaveProfile { profile },
                    Edit::SaveMacro {
                        value: crate::builtins::SavedMacro {
                            id: "fixture-settings-macro".into(),
                            name: "Fixture settings macro".into(),
                            inputs: vec![],
                            action: Action::Macro {
                                actions: vec![action(OWNER), action(FOREIGN)],
                            },
                        },
                    },
                ],
            )
            .unwrap();
        (root, runtime)
    }

    fn row(runtime: &Runtime, owner: &str) -> VaultSettingsPlugin {
        runtime
            .vault_settings_plugins()
            .unwrap()
            .into_iter()
            .find(|row| row.id == owner)
            .unwrap()
    }

    fn private_count(runtime: &Runtime, owner: &str) -> u64 {
        let owner = owner.to_owned();
        runtime
            .profiles
            .storage
            .run(move |db| {
                db.query_row(
                    "SELECT count(*) FROM settings_vault.plugin_storage_records WHERE plugin_id=?1",
                    [&owner],
                    |r| r.get(0),
                )
                .map_err(|error| error.to_string())
            })
            .unwrap()
    }

    #[test]
    fn document_and_group_clear_are_scoped_revision_fenced_and_keep_bindings() {
        let (_root, runtime) = fixture();
        let before = row(&runtime, OWNER);
        assert_eq!(before.rows.len(), 4);
        let review = before.settings_review.unwrap();
        let selected = before
            .rows
            .into_iter()
            .find(|setting| setting.location.contains("key-1"))
            .unwrap();
        runtime.vault.lock();
        assert_eq!(
            runtime
                .vault_delete_settings(selected.revision.clone())
                .unwrap_err(),
            "vault_locked"
        );
        runtime.vault.unlock().unwrap();
        runtime
            .vault_delete_settings(selected.revision.clone())
            .unwrap();
        assert!(runtime
            .vault_settings_document(selected.revision, true)
            .is_err());
        let state = runtime.snapshot();
        let binding = &state.profiles[0].pages[0].bindings["key-1"];
        assert!(
            matches!(&binding.action, Action::Plugin { plugin_id, settings, .. }
            if plugin_id == OWNER && *settings == json!({}))
        );
        assert_eq!(binding.color, "#345678");
        assert_eq!(row(&runtime, OWNER).rows.len(), 3);
        assert_eq!(private_count(&runtime, OWNER), 1);
        assert!(runtime
            .vault_delete_plugin_settings(OWNER.into(), review)
            .unwrap_err()
            .contains("changed"));

        let before = row(&runtime, OWNER);
        let review = before.settings_review.unwrap();
        runtime
            .profiles
            .storage
            .transaction(|tx| {
                tx.execute(
                    "UPDATE settings_vault.plugin_storage_namespaces SET revision=?1 WHERE plugin_id=?2",
                    params![2_u64.to_be_bytes(), OWNER],
                )
                .map_err(|error| error.to_string())?;
                Ok(())
            })
            .unwrap();
        assert!(runtime
            .vault_delete_plugin_settings(OWNER.into(), review)
            .unwrap_err()
            .contains("changed"));

        let selected = row(&runtime, OWNER)
            .rows
            .into_iter()
            .find(|setting| setting.group == "Action defaults")
            .unwrap();
        runtime.vault_delete_settings(selected.revision).unwrap();
        assert_eq!(row(&runtime, OWNER).rows.len(), 2);
        assert_eq!(private_count(&runtime, OWNER), 1);

        let review = row(&runtime, OWNER).settings_review.unwrap();
        runtime.plugins.lock().unwrap().enable(OWNER, true).unwrap();
        assert!(runtime
            .vault_delete_plugin_settings(OWNER.into(), review.clone())
            .unwrap_err()
            .contains("Disable"));
        runtime
            .plugins
            .lock()
            .unwrap()
            .enable(OWNER, false)
            .unwrap();
        runtime
            .save_draft(
                "fixture",
                Some(crate::workspace::Draft {
                    key: "fixture".into(),
                    base: Some(runtime.snapshot().profiles[0].pages[0].bindings["key-1"].clone()),
                    value: json!({}),
                }),
            )
            .unwrap();
        assert!(runtime
            .vault_delete_plugin_settings(OWNER.into(), review.clone())
            .unwrap_err()
            .contains("drafts"));
        runtime.save_draft("fixture", None).unwrap();
        runtime
            .vault_delete_plugin_settings(OWNER.into(), review)
            .unwrap();
        let cleared = row(&runtime, OWNER);
        assert!(cleared.installed && cleared.rows.is_empty() && cleared.settings_review.is_none());
        assert_eq!(private_count(&runtime, OWNER), 0);
        assert_eq!(private_count(&runtime, FOREIGN), 1);
        assert_eq!(row(&runtime, FOREIGN).rows.len(), 4);
        let saved = &runtime.snapshot().saved_macros[0].action;
        assert_eq!(json!(saved)["actions"][0]["settings"], json!({}));
        assert_eq!(
            json!(saved)["actions"][1]["settings"],
            json!({"binding":FOREIGN})
        );
        runtime.shutdown();
    }

    #[test]
    fn uninstall_keep_option_removes_or_retains_current_vault_settings() {
        for keep in [true, false] {
            let (_root, runtime) = fixture();
            runtime.delete_plugin(OWNER, keep).unwrap();
            let records = runtime.vault_settings_plugins().unwrap();
            let old = records.iter().find(|record| record.id == OWNER);
            if keep {
                let old = old.unwrap();
                assert!(!old.installed && old.rows.len() == 4 && old.settings_review.is_some());
            } else {
                assert!(old.is_none_or(|record| !record.installed
                    && record.rows.is_empty()
                    && record.settings_review.is_none()));
            }
            assert_eq!(private_count(&runtime, OWNER), u64::from(keep));
            assert_eq!(row(&runtime, FOREIGN).rows.len(), 4);
            runtime.shutdown();
        }
    }
}
