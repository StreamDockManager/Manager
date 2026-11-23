//! Manager-owned installation state, separate from plugin settings and private data
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const LIMIT: usize = 16 * 1024 * 1024;
const MAX_ROWS: usize = 20_480;
const COLUMNS: &str = "enabled,sleep_when_hidden,retained_name,installation_uuid,provenance";

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct State {
    pub enabled: bool,
    pub sleep_when_hidden: Option<bool>,
    pub retained_name: Option<String>,
    pub installation_uuid: Option<String>,
    pub provenance: Option<Value>,
}

pub(crate) fn initialize(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "CREATE TABLE plugin_installation_state(
           plugin_id TEXT PRIMARY KEY NOT NULL CHECK(length(plugin_id) BETWEEN 1 AND 159
             AND plugin_id NOT IN ('.','..') AND plugin_id NOT GLOB '*[^A-Za-z0-9._-]*'),
           enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
           sleep_when_hidden INTEGER CHECK(sleep_when_hidden IN (0,1)),
           retained_name TEXT CHECK(length(CAST(retained_name AS BLOB)) <= 2048),
           installation_uuid TEXT CHECK(length(CAST(installation_uuid AS BLOB)) <= 45),
           provenance TEXT CHECK(json_valid(provenance) AND length(CAST(provenance AS BLOB)) <= 16777216),
           CHECK(enabled=1 OR sleep_when_hidden IS NOT NULL OR retained_name IS NOT NULL
             OR installation_uuid IS NOT NULL OR provenance IS NOT NULL)
         ) STRICT, WITHOUT ROWID;",
    )
    .map_err(|e| e.to_string())
}

fn validate(id: &str, state: &State) -> Result<(), String> {
    if id.is_empty()
        || id.len() >= 160
        || matches!(id, "." | "..")
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        || state
            .retained_name
            .as_ref()
            .is_some_and(|name| name.len() > 2048)
        || state
            .installation_uuid
            .as_deref()
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_err())
    {
        return Err("Invalid saved plugin installation state".into());
    }
    Ok(())
}

fn decode(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<State> {
    let raw: Option<String> = row.get(offset + 4)?;
    let provenance = raw
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 4,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    Ok(State {
        enabled: row.get(offset)?,
        sleep_when_hidden: row.get(offset + 1)?,
        retained_name: row.get(offset + 2)?,
        installation_uuid: row.get(offset + 3)?,
        provenance,
    })
}

pub fn get(db: &Connection, id: &str) -> Result<Option<State>, String> {
    let row = db
        .query_row(
            &format!("SELECT {COLUMNS} FROM plugin_installation_state WHERE plugin_id=?1"),
            [id],
            |row| decode(row, 0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some(state) = &row {
        validate(id, state)?;
    }
    Ok(row)
}

pub fn list(db: &Connection) -> Result<BTreeMap<String, State>, String> {
    // Probe the encoded aggregate before deserializing any large provenance row
    let (count, bytes): (usize, usize) = db
        .query_row(
            "SELECT count(*),coalesce(sum(length(CAST(plugin_id AS BLOB))
             +coalesce(length(CAST(retained_name AS BLOB)),0)
             +coalesce(length(CAST(installation_uuid AS BLOB)),0)
             +coalesce(length(CAST(provenance AS BLOB)),0)),0) FROM plugin_installation_state",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if count > MAX_ROWS || bytes > LIMIT {
        return Err("Saved plugin installation state exceeds bounds".into());
    }
    let mut statement = db
        .prepare(&format!(
            "SELECT plugin_id,{COLUMNS} FROM plugin_installation_state ORDER BY plugin_id"
        ))
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, decode(row, 1)?)))
        .map_err(|e| e.to_string())?;
    let mut result = BTreeMap::new();
    for row in rows {
        let (id, state) = row.map_err(|e| e.to_string())?;
        validate(&id, &state)?;
        result.insert(id, state);
    }
    Ok(result)
}

// Persist only one owner; an empty state needs no row
pub fn put(db: &Connection, id: &str, state: &State) -> Result<(), String> {
    validate(id, state)?;
    if *state == State::default() {
        return db
            .execute(
                "DELETE FROM plugin_installation_state WHERE plugin_id=?1",
                [id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string());
    }
    let provenance = state
        .provenance
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| e.to_string())?;
    db.execute(
        "INSERT INTO plugin_installation_state VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(plugin_id) DO UPDATE SET enabled=excluded.enabled,
         sleep_when_hidden=excluded.sleep_when_hidden, retained_name=excluded.retained_name,
         installation_uuid=excluded.installation_uuid, provenance=excluded.provenance
         WHERE enabled IS NOT excluded.enabled OR sleep_when_hidden IS NOT excluded.sleep_when_hidden
         OR retained_name IS NOT excluded.retained_name OR installation_uuid IS NOT excluded.installation_uuid
         OR provenance IS NOT excluded.provenance",
        params![id,state.enabled,state.sleep_when_hidden,state.retained_name,state.installation_uuid,provenance],
    ).map(|_| ()).map_err(|e| e.to_string())
}

pub fn replace_all(db: &Connection, next: BTreeMap<String, State>) -> Result<(), String> {
    let mut previous = list(db)?;
    for (id, state) in next {
        if previous.remove(&id).as_ref() != Some(&state) {
            put(db, &id, &state)?;
        }
    }
    for id in previous.keys() {
        put(db, id, &State::default())?;
    }
    Ok(())
}

pub fn clear_retained(db: &Connection, id: &str) -> Result<(), String> {
    if let Some(mut state) = get(db, id)? {
        if state.retained_name.take().is_some() {
            put(db, id, &state)?;
        }
    }
    Ok(())
}
