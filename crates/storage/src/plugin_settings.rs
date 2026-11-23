//! Split host settings out of Manager domain records at the common persistence boundary.
//! The public payload remains usable in a backup. Only a live record carries revision IDs.
use crate::{Domain, MAX_RECORD_BYTES};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::BTreeMap;

type Fields = BTreeMap<String, Value>;
type Revisions = BTreeMap<String, String>;
const VALUE_QUERY: &str = "SELECT value FROM settings_vault.revisions
    WHERE domain=?1 AND record_key=?2 AND pointer=?3 AND id=?4 AND length(CAST(value AS BLOB))<=?5";
const DAMAGED: &str = "Plugin settings revision is missing or damaged. Restore the matching keystore; data preserved.";

const EDITABLE: [Domain; 4] = [
    Domain::PluginGlobals,
    Domain::PluginDefaults,
    Domain::Profiles,
    Domain::SavedMacros,
];

/// Store only reference metadata in the main database, never plugin settings values
pub(crate) fn initialize(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE plugin_setting_refs(
            domain TEXT NOT NULL CHECK(domain IN ('plugin_globals','plugin_defaults','profiles',
                 'saved_macros','history_entries','history_plugin_sidecars','local_service_preferences')),
            record_key TEXT NOT NULL CHECK(length(CAST(record_key AS BLOB)) BETWEEN 1 AND 1024),
            pointer TEXT NOT NULL CHECK(pointer='' OR substr(pointer,1,1)='/'),
            revision_id TEXT NOT NULL UNIQUE,
            plugin_id TEXT,
            action_id TEXT,
            location TEXT NOT NULL DEFAULT '',
            CHECK((plugin_id IS NOT NULL)=(domain IN ('plugin_globals','plugin_defaults','profiles','saved_macros'))),
            CHECK((action_id IS NOT NULL)=(domain IN ('plugin_defaults','profiles','saved_macros'))),
            CHECK(domain<>'local_service_preferences' OR record_key='drafts'),
            PRIMARY KEY(domain,record_key,pointer)
        ) STRICT, WITHOUT ROWID;
        CREATE INDEX plugin_setting_refs_by_action ON plugin_setting_refs(plugin_id,action_id,domain,record_key,pointer,revision_id)
            WHERE plugin_id IS NOT NULL;",
    )
    .map_err(|e| e.to_string())
}

pub(crate) fn domains() -> [Domain; 6] {
    [
        Domain::PluginGlobals,
        Domain::PluginDefaults,
        Domain::Profiles,
        Domain::SavedMacros,
        Domain::History,
        Domain::HistoryPlugins,
    ]
}
pub(crate) fn supports(domain: Domain, key: &str) -> bool {
    domains().contains(&domain) || (domain == Domain::LocalPreferences && key == "drafts")
}
fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
}
fn field(fields: &mut Fields, path: String, value: &mut Value, public: Value) {
    fields.insert(path, std::mem::replace(value, public));
}
/// Stop at a plugin action: its vendor JSON (including objects resembling actions) is opaque.
fn actions(value: &mut Value, path: &str, fields: &mut Fields, drafts: bool) {
    match value {
        Value::Object(map) => {
            if map.get("kind").and_then(Value::as_str) == Some("plugin")
                && map.contains_key("plugin_id")
                && map.contains_key("action_id")
            {
                if let Some(settings) = map
                    .get_mut("settings")
                    .filter(|settings| !settings.as_object().is_some_and(serde_json::Map::is_empty))
                {
                    field(fields, child(path, "settings"), settings, json!({}));
                }
                return;
            }
            if drafts && map.get("action").is_some_and(contains_plugin_action) {
                if let Some(raw) = map.get_mut("rawValue").filter(|v| v.is_string()) {
                    field(fields, child(path, "rawValue"), raw, Value::Null);
                }
            }
            for (key, v) in map {
                actions(v, &child(path, key), fields, drafts);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                actions(item, &format!("{path}/{i}"), fields, drafts);
            }
        }
        _ => {}
    }
}
pub fn contains_plugin_action(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("kind").and_then(Value::as_str) == Some("plugin")
                && map.contains_key("plugin_id")
                || map.values().any(contains_plugin_action)
        }
        Value::Array(a) => a.iter().any(contains_plugin_action),
        _ => false,
    }
}
fn extract(domain: Domain, value: &mut Value) -> Result<Fields, String> {
    let mut fields = Fields::new();
    match domain {
        Domain::PluginGlobals => field(&mut fields, String::new(), value, json!({})),
        Domain::PluginDefaults => {
            if let Some(map) = value.as_object_mut() {
                for (action, v) in map {
                    field(&mut fields, child("", action), v, json!({}));
                }
            }
        }
        Domain::HistoryPlugins => {
            if let Some(changes) = value.get_mut("changes") {
                field(&mut fields, "/changes".into(), changes, json!([]));
            }
        }
        _ => actions(value, "", &mut fields, domain == Domain::LocalPreferences),
    }
    if domain == Domain::History && !fields.is_empty() {
        for part in ["before", "after"] {
            if let Some(document) = value.get(part) {
                let public = json!(crate::json_digest::sha256(document)?);
                let key = format!("{part}_hash");
                if let Some(hash) = value.get_mut(&key) {
                    field(&mut fields, child("", &key), hash, public);
                }
            }
        }
    }
    Ok(fields)
}
fn previous(db: &Connection, domain: Domain, key: &str) -> Result<Revisions, String> {
    let mut query = db
        .prepare(
            "SELECT pointer,revision_id FROM plugin_setting_refs WHERE domain=?1 AND record_key=?2",
        )
        .map_err(|_| DAMAGED)?;
    let mut rows = query
        .query(params![domain.table(), key])
        .map_err(|_| DAMAGED)?;
    let mut refs = Revisions::new();
    let mut remaining = MAX_RECORD_BYTES;
    while let Some(row) = rows.next().map_err(|_| DAMAGED)? {
        let pointer: String = row.get(0).map_err(|_| DAMAGED)?;
        let id: String = row.get(1).map_err(|_| DAMAGED)?;
        remaining = remaining
            .checked_sub(pointer.len() + id.len())
            .ok_or(DAMAGED)?;
        refs.insert(pointer, id);
    }
    Ok(refs)
}
fn retire(db: &Connection, domain: Domain, key: &str, refs: Revisions) -> Result<(), String> {
    for (pointer, id) in refs {
        db.execute(
            "DELETE FROM settings_vault.revisions
             WHERE id=?1 AND domain=?2 AND record_key=?3 AND pointer=?4",
            params![id, domain.table(), key, pointer],
        )
        .map_err(|e| e.to_string())?;
        db.execute(
            "DELETE FROM plugin_setting_refs WHERE domain=?1 AND record_key=?2 AND pointer=?3 AND revision_id=?4",
            params![domain.table(), key, pointer, id],
        )
        .map_err(|_| DAMAGED)?;
    }
    Ok(())
}
pub(crate) fn remove(db: &Connection, domain: Domain, key: &str) -> Result<(), String> {
    if supports(domain, key) {
        retire(db, domain, key, previous(db, domain, key)?)?;
    }
    Ok(())
}
pub(crate) fn write(db: &Connection, domain: Domain, key: &str, input: &str) -> Result<(), String> {
    let mut public: Value = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let fields = extract(domain, &mut public)?;
    let mut old = previous(db, domain, key)?;
    let old_public = if !old.is_empty() && matches!(domain, Domain::Profiles | Domain::SavedMacros)
    {
        let text: String = db
            .query_row(
                &format!("SELECT payload FROM {} WHERE key=?1", domain.table()),
                [key],
                |r| r.get(0),
            )
            .map_err(|_| DAMAGED)?;
        Some(serde_json::from_str::<Value>(&text).map_err(|_| DAMAGED)?)
    } else {
        None
    };
    let mut refs = Revisions::new();
    for (pointer, value) in fields {
        let encoded = value.to_string();
        if let Some(id) = old.get(&pointer) {
            let same: bool = db
                .query_row(
                    "SELECT value=?5 FROM settings_vault.revisions
                 WHERE domain=?1 AND record_key=?2 AND pointer=?3 AND id=?4",
                    params![domain.table(), key, pointer, id, encoded],
                    |row| row.get(0),
                )
                .map_err(|_| DAMAGED)?;
            let same_action = old_public.as_ref().is_none_or(|prior| {
                let parent = pointer.strip_suffix("/settings").unwrap_or("");
                if let Some((page, _)) = parent.split_once("/bindings/") {
                    let page_id = format!("{page}/id");
                    if prior.pointer(&page_id) != public.pointer(&page_id) {
                        return false;
                    }
                }
                let old = prior.pointer(parent);
                let new = public.pointer(parent);
                ["plugin_id", "action_id"]
                    .iter()
                    .all(|key| old.and_then(|a| a.get(*key)) == new.and_then(|a| a.get(*key)))
            });
            if same && same_action {
                refs.insert(pointer.clone(), old.remove(&pointer).unwrap());
                continue;
            }
        }
        // Detached redacted snapshots can be reconciled without a live keystore.
        if db.is_readonly("settings_vault").is_err() && public.pointer(&pointer) == Some(&value) {
            continue;
        }
        let id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO settings_vault.revisions(domain,record_key,pointer,id,value)
             VALUES(?1,?2,?3,?4,?5)",
            params![domain.table(), key, pointer, id, encoded],
        )
        .map_err(|_| DAMAGED)?;
        refs.insert(pointer, id);
    }
    retire(db, domain, key, old)?;
    let sql = format!(
        "INSERT INTO {}(key,payload,revision) VALUES(?1,?2,1)
         ON CONFLICT(key) DO UPDATE SET payload=excluded.payload,revision=revision+1",
        domain.table(),
    );
    db.execute(&sql, params![key, public.to_string()])
        .map_err(|e| e.to_string())?;
    let mut stmt = db.prepare(
        "INSERT INTO plugin_setting_refs(domain,record_key,pointer,revision_id,plugin_id,action_id,location)
         VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(domain,record_key,pointer) DO UPDATE SET
             revision_id=excluded.revision_id,plugin_id=excluded.plugin_id,
             action_id=excluded.action_id,location=excluded.location
         WHERE revision_id IS NOT excluded.revision_id OR plugin_id IS NOT excluded.plugin_id
            OR action_id IS NOT excluded.action_id OR location IS NOT excluded.location",
    ).map_err(|_| DAMAGED)?;
    let mut remaining = MAX_RECORD_BYTES;
    for (pointer, id) in refs {
        remaining = remaining
            .checked_sub(pointer.len() + id.len())
            .ok_or(DAMAGED)?;
        let (owner, action, location) = metadata(domain, key, &pointer, &public)?;
        stmt.execute(params![
            domain.table(),
            key,
            pointer,
            id,
            owner,
            action,
            location
        ])
        .map_err(|_| DAMAGED)?;
    }
    Ok(())
}
pub(crate) fn hydrate(
    db: &Connection,
    domain: Domain,
    key: &str,
    public: String,
    limit: usize,
) -> Result<String, String> {
    if !supports(domain, key) {
        return Ok(public);
    }
    let refs = previous(db, domain, key)?;
    if refs.is_empty() {
        return if public.len() <= limit {
            Ok(public)
        } else {
            Err("Stored record exceeds size limit".into())
        };
    }
    let mut value: Value = serde_json::from_str(&public).map_err(|_| DAMAGED)?;
    let mut remaining = limit.min(MAX_RECORD_BYTES);
    for (pointer, id) in refs {
        let target = value.pointer_mut(&pointer).ok_or(DAMAGED)?;
        let data: String = db
            .query_row(
                VALUE_QUERY,
                params![domain.table(), key, pointer, id, remaining],
                |row| row.get(0),
            )
            .map_err(|_| DAMAGED)?;
        remaining -= data.len();
        *target = serde_json::from_str(&data).map_err(|_| DAMAGED)?;
    }
    crate::records::encode(&value, limit)
}
/// Check every live reference on a disposable main DB with the candidate
/// attached. Never send a decoded value to the caller or to diagnostics.
pub(crate) fn validate_recovery_refs(db: &Connection) -> Result<(), String> {
    for domain in domains().into_iter().chain([Domain::LocalPreferences]) {
        let sql = format!(
            "SELECT refs.record_key,length(CAST(refs.record_key AS BLOB)),
                    length(CAST(record.payload AS BLOB)),record.payload
             FROM (SELECT DISTINCT record_key FROM plugin_setting_refs WHERE domain=?1) refs
             LEFT JOIN {} record ON record.key=refs.record_key",
            domain.table(),
        );
        let mut query = db.prepare(&sql).map_err(|_| DAMAGED)?;
        let mut rows = query.query([domain.table()]).map_err(|_| DAMAGED)?;
        while let Some(row) = rows.next().map_err(|_| DAMAGED)? {
            let key_len: usize = row.get(1).map_err(|_| DAMAGED)?;
            let bytes: usize = row.get(2).map_err(|_| DAMAGED)?;
            if key_len > crate::MAX_KEY_BYTES || bytes > MAX_RECORD_BYTES {
                return Err(DAMAGED.into());
            }
            let key: String = row.get(0).map_err(|_| DAMAGED)?;
            if !supports(domain, &key) {
                return Err(DAMAGED.into());
            }
            let public: String = row.get(3).map_err(|_| DAMAGED)?;
            let _decoded =
                zeroize::Zeroizing::new(hydrate(db, domain, &key, public, MAX_RECORD_BYTES)?);
        }
    }
    Ok(())
}

pub(crate) fn clear_refs(db: &Connection) -> Result<(), String> {
    db.execute("DELETE FROM plugin_setting_refs", [])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub(crate) fn omit(db: &Connection) -> Result<(), String> {
    clear_refs(db)?;
    db.execute(
        "UPDATE database_metadata SET value='' WHERE key='settings_installation'",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// The revision is also the optimistic editor token. A caller can address only a
/// currently referenced, user-editable setting, never an undo entry or draft.
pub struct Setting {
    pub domain: Domain,
    pub key: String,
    pub pointer: String,
    pub id: String,
    pub owner: String,
    pub action: Option<String>,
    pub location: String,
}
/// Compute only display/identity metadata from the already-redacted public document
fn metadata(
    domain: Domain,
    key: &str,
    pointer: &str,
    doc: &Value,
) -> Result<(Option<String>, Option<String>, String), String> {
    let unescape = |text: &str| text.replace("~1", "/").replace("~0", "~");
    Ok(match domain {
        Domain::PluginGlobals => (Some(key.into()), None, String::new()),
        Domain::PluginDefaults => (
            Some(key.into()),
            Some(unescape(pointer.strip_prefix('/').ok_or(DAMAGED)?)),
            String::new(),
        ),
        Domain::Profiles | Domain::SavedMacros => {
            let path = pointer.strip_suffix("/settings").ok_or(DAMAGED)?;
            let action = doc.pointer(path).ok_or(DAMAGED)?;
            let owner = action["plugin_id"].as_str().ok_or(DAMAGED)?.to_owned();
            let action = action["action_id"].as_str().ok_or(DAMAGED)?.to_owned();
            let mut parts = vec![doc["name"].as_str().unwrap_or(key).to_owned()];
            if let Some(rest) = path.strip_prefix("/pages/") {
                if let Some((page, control)) = rest.split_once("/bindings/") {
                    if let Some(name) = doc
                        .pointer(&format!("/pages/{page}/name"))
                        .and_then(Value::as_str)
                    {
                        parts.push(name.into());
                    }
                    parts.push(unescape(control.trim_end_matches("/action")));
                }
            } else if path != "/action" {
                parts.push(path.trim_start_matches("/action/").into());
            }
            (Some(owner), Some(action), parts.join(" · "))
        }
        _ => (None, None, String::new()),
    })
}

pub fn settings(db: &Connection, id: Option<&str>) -> Result<Vec<Setting>, String> {
    let mut result = Vec::new();
    // Keep listing order stable and use the unique revision index for editor lookup
    for domain in EDITABLE {
        let sql = if id.is_some() {
            "SELECT record_key,pointer,revision_id,plugin_id,action_id,location
             FROM plugin_setting_refs WHERE domain=?1 AND revision_id=?2"
        } else {
            "SELECT record_key,pointer,revision_id,plugin_id,action_id,location
             FROM plugin_setting_refs WHERE domain=?1 AND ?2 IS NULL ORDER BY record_key,pointer"
        };
        let mut query = db.prepare(sql).map_err(|_| DAMAGED)?;
        let rows = query
            .query_map(params![domain.table(), id], |row| {
                Ok(Setting {
                    domain,
                    key: row.get(0)?,
                    pointer: row.get(1)?,
                    id: row.get(2)?,
                    owner: row.get(3)?,
                    action: row.get(4)?,
                    location: row.get(5)?,
                })
            })
            .map_err(|_| DAMAGED)?;
        for row in rows {
            result.push(row.map_err(|_| DAMAGED)?);
        }
    }
    Ok(result)
}
/// Fetch only the optimistic tokens needed to review or clear one plugin
pub fn owner_revisions(db: &Connection, owner: &str) -> Result<Vec<String>, String> {
    let mut query = db
        .prepare("SELECT revision_id FROM plugin_setting_refs WHERE plugin_id=?1")
        .map_err(|_| DAMAGED)?;
    let rows = query
        .query_map([owner], |row| row.get(0))
        .map_err(|_| DAMAGED)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|_| DAMAGED.into())
}

pub fn setting(db: &Connection, id: &str) -> Result<Setting, String> {
    let setting = settings(db, Some(id))?
        .pop()
        .ok_or("These settings changed. Refresh and review the current values.")?;
    // An editor must never authorize a write based on stale or corrupted identity metadata
    let sql = format!(
        "SELECT payload FROM {} WHERE key=?1 AND length(CAST(payload AS BLOB))<=?2",
        setting.domain.table(),
    );
    let public: String = db
        .query_row(&sql, params![setting.key, MAX_RECORD_BYTES], |row| {
            row.get(0)
        })
        .map_err(|_| DAMAGED)?;
    let doc: Value = serde_json::from_str(&public).map_err(|_| DAMAGED)?;
    let (owner, action, location) = metadata(setting.domain, &setting.key, &setting.pointer, &doc)?;
    if owner.as_deref() != Some(&setting.owner)
        || action != setting.action
        || location != setting.location
        || doc.pointer(&setting.pointer).is_none()
    {
        return Err(DAMAGED.into());
    }
    Ok(setting)
}
pub fn setting_value(db: &Connection, s: &Setting) -> Result<Value, String> {
    let text: String = db
        .query_row(
            VALUE_QUERY,
            params![s.domain.table(), s.key, s.pointer, s.id, MAX_RECORD_BYTES],
            |row| row.get(0),
        )
        .map_err(|_| DAMAGED)?;
    serde_json::from_str(&text).map_err(|_| DAMAGED.into())
}

#[cfg(test)]
mod relational_tests {
    use super::*;
    use crate::records;

    #[test]
    fn metadata_listing_is_independent_of_vault_and_editor_rejects_mismatched_identity() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        db.execute_batch(
            "CREATE TABLE profiles(key TEXT PRIMARY KEY,payload TEXT,revision INTEGER);
             ATTACH DATABASE ':memory:' AS settings_vault;
             CREATE TABLE settings_vault.revisions(domain TEXT,record_key TEXT,pointer TEXT,id TEXT PRIMARY KEY,value TEXT);",
        ).unwrap();
        let profile = |name: &str| {
            json!({
                "name": name,
                "pages": [{"id":"original","name":"Home","bindings":{"touch/1":{"action":{
                    "kind":"plugin","plugin_id":"example","action_id":"example.action","settings":{"token":"secret-value"}
                }}}}]
            })
        };
        records::write_json(&db, Domain::Profiles, "profile", &profile("First"), 8192).unwrap();
        let first = settings(&db, None).unwrap().pop().unwrap();
        assert_eq!(first.location, "First · Home · touch/1");
        records::write_json(&db, Domain::Profiles, "profile", &profile("Renamed"), 8192).unwrap();
        let renamed = setting(&db, &first.id).unwrap();
        assert_eq!(renamed.location, "Renamed · Home · touch/1");
        assert_eq!(
            setting_value(&db, &renamed).unwrap(),
            json!({"token":"secret-value"})
        );
        db.execute_batch("DETACH DATABASE settings_vault").unwrap();
        let listed = settings(&db, None).unwrap();
        assert_eq!(listed[0].owner, "example");
        assert_eq!(
            owner_revisions(&db, "example").unwrap().as_slice(),
            std::slice::from_ref(&first.id)
        );
        assert!(owner_revisions(&db, "different").unwrap().is_empty());
        let plan: String = db
            .query_row(
                "EXPLAIN QUERY PLAN SELECT revision_id FROM plugin_setting_refs WHERE plugin_id=?1",
                ["example"],
                |row| row.get(3),
            )
            .unwrap();
        assert!(
            plan.contains("COVERING INDEX plugin_setting_refs_by_action"),
            "{plan}"
        );
        assert_eq!(listed[0].action.as_deref(), Some("example.action"));
        assert!(!listed[0].location.contains("secret-value"));
        db.execute("UPDATE plugin_setting_refs SET plugin_id='wrong'", [])
            .unwrap();
        assert!(setting(&db, &first.id).is_err());
        clear_refs(&db).unwrap();
        assert!(settings(&db, None).unwrap().is_empty());
    }
}
