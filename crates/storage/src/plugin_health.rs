//! Normalized, bounded plugin-health records in the primary workspace database
use rusqlite::{params, Connection, Transaction};
use std::collections::{BTreeMap, HashSet};

pub const MAX_EVENTS: usize = 100;
const MAX_PLUGINS: usize = 4096;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DETAILS: usize = 20;
const MAX_TEXT: usize = 16 * 1024;
const MAX_DETAIL_JSON: usize = 128 * 1024;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub crashes: u64,
    pub failed_starts: u64,
    pub automatic_restarts: u64,
    pub issues: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Health {
    pub totals: Counts,
    pub session: Counts,
    pub events: Vec<Event>,
    pub last_registration_ms: Option<u64>,
    pub last_unexpected_exit_ms: Option<u64>,
    pub last_issue_ms: Option<u64>,
    pub last_exit: Option<String>,
    pub last_exit_code: Option<i32>,
    pub last_exit_signal: Option<i32>,
    pub blocked_reason: Option<String>,
    pub last_run_peak_resident_bytes: Option<u64>,
    pub restart_attempts: u8,
    pub recent_failures: Vec<String>,
    pub missing_assets: Vec<String>,
    pub unsupported_apis: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub timestamp_ms: u64,
    pub version: String,
    pub run_id: Option<String>,
    pub category: String,
    pub severity: String,
    pub message: String,
    pub occurrences: u64,
}

const SUMMARY_COLUMNS: &str =
    "owner,total_crashes,total_failed_starts,total_restarts,total_issues,session_crashes,
     session_failed_starts,session_restarts,session_issues,last_registration_ms,
     last_unexpected_exit_ms,last_issue_ms,last_exit,last_exit_code,last_exit_signal,
     blocked_reason,last_peak_bytes,restart_attempts,recent_failures,missing_assets,
     unsupported_apis,retained_bytes";

// Register both tables in the canonical schema
pub(crate) fn create(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch(
        "CREATE TABLE plugin_health_summary(
            owner TEXT PRIMARY KEY NOT NULL CHECK(length(owner) BETWEEN 1 AND 159 AND owner NOT IN ('.','..') AND owner NOT GLOB '*[^a-zA-Z0-9._-]*'),
            total_crashes INTEGER NOT NULL CHECK(total_crashes>=0),
            total_failed_starts INTEGER NOT NULL CHECK(total_failed_starts>=0),
            total_restarts INTEGER NOT NULL CHECK(total_restarts>=0),
            total_issues INTEGER NOT NULL CHECK(total_issues>=0),
            session_crashes INTEGER NOT NULL CHECK(session_crashes>=0),
            session_failed_starts INTEGER NOT NULL CHECK(session_failed_starts>=0),
            session_restarts INTEGER NOT NULL CHECK(session_restarts>=0),
            session_issues INTEGER NOT NULL CHECK(session_issues>=0),
            last_registration_ms INTEGER CHECK(last_registration_ms>=0),
            last_unexpected_exit_ms INTEGER CHECK(last_unexpected_exit_ms>=0),
            last_issue_ms INTEGER CHECK(last_issue_ms>=0),
            last_exit TEXT CHECK(length(CAST(last_exit AS BLOB))<=16384),
            last_exit_code INTEGER,
            last_exit_signal INTEGER,
            blocked_reason TEXT CHECK(length(CAST(blocked_reason AS BLOB))<=16384),
            last_peak_bytes INTEGER CHECK(last_peak_bytes>=0),
            restart_attempts INTEGER NOT NULL CHECK(restart_attempts BETWEEN 0 AND 255),
            recent_failures TEXT NOT NULL CHECK(length(CAST(recent_failures AS BLOB))<=131072 AND json_type(recent_failures)='array' AND json_array_length(recent_failures)<=20),
            missing_assets TEXT NOT NULL CHECK(length(CAST(missing_assets AS BLOB))<=131072 AND json_type(missing_assets)='array' AND json_array_length(missing_assets)<=20),
            unsupported_apis TEXT NOT NULL CHECK(length(CAST(unsupported_apis AS BLOB))<=131072 AND json_type(unsupported_apis)='array' AND json_array_length(unsupported_apis)<=20),
            retained_bytes INTEGER NOT NULL CHECK(retained_bytes BETWEEN 1 AND 16777216)
        ) STRICT;
        CREATE TABLE plugin_health_events(
            ordinal INTEGER PRIMARY KEY,
            owner TEXT NOT NULL REFERENCES plugin_health_summary(owner) ON DELETE CASCADE,
            event_id TEXT NOT NULL CHECK(length(event_id)=36),
            timestamp_ms INTEGER NOT NULL CHECK(timestamp_ms>=0),
            version TEXT NOT NULL CHECK(length(CAST(version AS BLOB))<=16384),
            run_id TEXT CHECK(length(CAST(run_id AS BLOB))<=16384),
            category TEXT NOT NULL CHECK(length(CAST(category AS BLOB))<=16384),
            severity TEXT NOT NULL CHECK(severity IN ('error','warning','info')),
            message TEXT NOT NULL CHECK(length(CAST(message AS BLOB))<=4096),
            occurrences INTEGER NOT NULL CHECK(occurrences>0),
            UNIQUE(owner,event_id)
        ) STRICT;
        CREATE INDEX plugin_health_event_owner_order ON plugin_health_events(owner,ordinal);",
    )
    .map_err(|e| e.to_string())
}

// Check the aggregate budget without opening unrelated event or summary payloads
fn check_limits(connection: &Connection) -> Result<usize, String> {
    let (count, bytes): (usize, usize) = connection
        .query_row(
            "SELECT count(*),coalesce(sum(retained_bytes),0) FROM plugin_health_summary",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    if count > MAX_PLUGINS || bytes > MAX_BYTES {
        return Err("Plugin health exceeds retention bounds".into());
    }
    Ok(bytes)
}

// Measure only variable strings and reserve fixed overhead; this is not a disk-space limit
fn event_cost(event: &Event) -> Result<usize, String> {
    if uuid::Uuid::parse_str(&event.id).is_err()
        || event.occurrences == 0
        || event.version.len() > MAX_TEXT
        || event.run_id.as_ref().is_some_and(|v| v.len() > MAX_TEXT)
        || event.category.len() > MAX_TEXT
        || event.message.chars().count() > 1024
        || !matches!(event.severity.as_str(), "error" | "warning" | "info")
    {
        return Err("Invalid plugin health event".into());
    }
    Ok(256
        + event.version.len()
        + event.run_id.as_ref().map_or(0, String::len)
        + event.category.len()
        + event.message.len())
}

// Prepare bounded compatibility/detail fields without encoding the health record
fn encode_details(value: &[String]) -> Result<String, String> {
    if value.len() > MAX_DETAILS || value.iter().any(|v| v.len() > MAX_TEXT) {
        return Err("Plugin health details exceed bounds".into());
    }
    crate::records::encode(value, MAX_DETAIL_JSON)
}

// Decode only a known bounded detail column
fn decode_details(value: String) -> Result<Vec<String>, String> {
    if value.len() > MAX_DETAIL_JSON {
        return Err("Plugin health details exceed bounds".into());
    }
    let decoded: Vec<String> =
        serde_json::from_str(&value).map_err(|_| "Invalid plugin health details")?;
    if decoded.len() > MAX_DETAILS || decoded.iter().any(|v| v.len() > MAX_TEXT) {
        return Err("Plugin health details exceed bounds".into());
    }
    Ok(decoded)
}

// Check bounds and derive the retained-byte accounting for one summary and its events
fn retained_bytes(owner: &str, health: &Health, detail_bytes: usize) -> Result<usize, String> {
    if owner.is_empty()
        || owner.len() >= 160
        || owner == "."
        || owner == ".."
        || !owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || health
            .last_exit
            .as_ref()
            .is_some_and(|v| v.len() > MAX_TEXT)
        || health
            .blocked_reason
            .as_ref()
            .is_some_and(|v| v.len() > MAX_TEXT)
        || health.events.len() > MAX_EVENTS
    {
        return Err("Plugin health summary exceeds bounds".into());
    }
    let mut bytes = 512
        + owner.len()
        + detail_bytes
        + health.last_exit.as_ref().map_or(0, String::len)
        + health.blocked_reason.as_ref().map_or(0, String::len);
    let mut ids = HashSet::new();
    for event in &health.events {
        if !ids.insert(&event.id) {
            return Err("Duplicate plugin health event".into());
        }
        bytes += event_cost(event)?;
    }
    if bytes > MAX_BYTES {
        return Err("Plugin health exceeds retention bounds".into());
    }
    Ok(bytes)
}

// Return the cost of admitting this batch to the storage worker
pub fn bounded_cost(records: &[(String, Health)]) -> Result<usize, String> {
    records.iter().try_fold(0, |sum, (owner, health)| {
        let bytes = encode_details(&health.recent_failures)?.len()
            + encode_details(&health.missing_assets)?.len()
            + encode_details(&health.unsupported_apis)?.len();
        let total = sum + retained_bytes(owner, health, bytes)?;
        if total > MAX_BYTES || records.len() > MAX_PLUGINS {
            return Err("Plugin health exceeds retention bounds".into());
        }
        Ok(total)
    })
}

// Upsert only dirty owners; deduplicated event rows preserve order and unchanged rows
pub fn write_dirty(tx: &Transaction<'_>, records: &[(String, Health)]) -> Result<(), String> {
    for (owner, h) in records {
        let recent = encode_details(&h.recent_failures)?;
        let assets = encode_details(&h.missing_assets)?;
        let apis = encode_details(&h.unsupported_apis)?;
        let size = retained_bytes(owner, h, recent.len() + assets.len() + apis.len())?;
        tx.execute(&format!("INSERT INTO plugin_health_summary({SUMMARY_COLUMNS})
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)
            ON CONFLICT(owner) DO UPDATE SET
            total_crashes=excluded.total_crashes,total_failed_starts=excluded.total_failed_starts,
            total_restarts=excluded.total_restarts,total_issues=excluded.total_issues,
            session_crashes=excluded.session_crashes,session_failed_starts=excluded.session_failed_starts,
            session_restarts=excluded.session_restarts,session_issues=excluded.session_issues,
            last_registration_ms=excluded.last_registration_ms,last_unexpected_exit_ms=excluded.last_unexpected_exit_ms,
            last_issue_ms=excluded.last_issue_ms,last_exit=excluded.last_exit,last_exit_code=excluded.last_exit_code,
            last_exit_signal=excluded.last_exit_signal,blocked_reason=excluded.blocked_reason,
            last_peak_bytes=excluded.last_peak_bytes,restart_attempts=excluded.restart_attempts,
            recent_failures=excluded.recent_failures,missing_assets=excluded.missing_assets,
            unsupported_apis=excluded.unsupported_apis,retained_bytes=excluded.retained_bytes"),
            params![owner,h.totals.crashes,h.totals.failed_starts,h.totals.automatic_restarts,h.totals.issues,
            h.session.crashes,h.session.failed_starts,h.session.automatic_restarts,h.session.issues,
            h.last_registration_ms,h.last_unexpected_exit_ms,h.last_issue_ms,h.last_exit,h.last_exit_code,
            h.last_exit_signal,h.blocked_reason,h.last_run_peak_resident_bytes,h.restart_attempts,recent,assets,apis,size])
            .map_err(|e| e.to_string())?;

        let ids = h
            .events
            .iter()
            .map(|event| &event.id)
            .collect::<HashSet<_>>();
        let mut pending = tx
            .prepare("SELECT event_id FROM plugin_health_events WHERE owner=?1")
            .map_err(|e| e.to_string())?;
        let previous = pending
            .query_map([owner], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let previous = previous
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for id in previous {
            if !ids.contains(&id) {
                tx.execute(
                    "DELETE FROM plugin_health_events WHERE owner=?1 AND event_id=?2",
                    params![owner, id],
                )
                .map_err(|e| e.to_string())?;
            }
        }

        let mut stmt = tx.prepare(
            "INSERT INTO plugin_health_events(owner,event_id,timestamp_ms,version,run_id,category,severity,message,occurrences)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(owner,event_id) DO UPDATE SET timestamp_ms=excluded.timestamp_ms,
                version=excluded.version,run_id=excluded.run_id,category=excluded.category,
                severity=excluded.severity,message=excluded.message,occurrences=excluded.occurrences
             WHERE timestamp_ms IS NOT excluded.timestamp_ms OR version IS NOT excluded.version
                OR run_id IS NOT excluded.run_id OR category IS NOT excluded.category
                OR severity IS NOT excluded.severity OR message IS NOT excluded.message
                OR occurrences IS NOT excluded.occurrences",
        ).map_err(|e| e.to_string())?;
        for event in &h.events {
            stmt.execute(params![
                owner,
                event.id,
                event.timestamp_ms,
                event.version,
                event.run_id,
                event.category,
                event.severity,
                event.message,
                event.occurrences
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    check_limits(tx)?;
    Ok(())
}

// Remove only the selected owner and its child rows
pub fn delete(tx: &Transaction<'_>, owner: &str) -> Result<(), String> {
    tx.execute("DELETE FROM plugin_health_summary WHERE owner=?1", [owner])
        .map_err(|e| e.to_string())?;
    Ok(())
}

// Load only validated current-schema records; history order is insertion order, not wall time
pub fn read(connection: &Connection) -> Result<BTreeMap<String, Health>, String> {
    let bytes = check_limits(connection)?;
    let mut owners = BTreeMap::new();
    let mut stmt = connection
        .prepare(&format!(
            "SELECT {SUMMARY_COLUMNS} FROM plugin_health_summary ORDER BY owner"
        ))
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    let mut seen_bytes = 0;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let owner: String = get(row, 0)?;
        let recent: String = get(row, 18)?;
        let assets: String = get(row, 19)?;
        let apis: String = get(row, 20)?;
        let details = recent.len() + assets.len() + apis.len();
        let stored: usize = get(row, 21)?;
        let health = Health {
            totals: Counts {
                crashes: get(row, 1)?,
                failed_starts: get(row, 2)?,
                automatic_restarts: get(row, 3)?,
                issues: get(row, 4)?,
            },
            session: Counts {
                crashes: get(row, 5)?,
                failed_starts: get(row, 6)?,
                automatic_restarts: get(row, 7)?,
                issues: get(row, 8)?,
            },
            last_registration_ms: get(row, 9)?,
            last_unexpected_exit_ms: get(row, 10)?,
            last_issue_ms: get(row, 11)?,
            last_exit: get(row, 12)?,
            last_exit_code: get(row, 13)?,
            last_exit_signal: get(row, 14)?,
            blocked_reason: get(row, 15)?,
            last_run_peak_resident_bytes: get(row, 16)?,
            restart_attempts: get(row, 17)?,
            recent_failures: decode_details(recent)?,
            missing_assets: decode_details(assets)?,
            unsupported_apis: decode_details(apis)?,
            ..Health::default()
        };
        seen_bytes += retained_bytes(&owner, &health, details)?;
        if seen_bytes > bytes {
            return Err("Invalid plugin health size accounting".into());
        }
        owners.insert(owner, (health, details, stored));
    }
    let mut stmt = connection.prepare(
        "SELECT owner,event_id,timestamp_ms,version,run_id,category,severity,message,occurrences
         FROM plugin_health_events ORDER BY owner,ordinal"
    ).map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let owner: String = get(row, 0)?;
        let Some((health, _, _)) = owners.get_mut(&owner) else {
            return Err("Plugin health event owner is missing".into());
        };
        if health.events.len() >= MAX_EVENTS {
            return Err("Plugin health event count exceeds bounds".into());
        }
        let event = Event {
            id: get(row, 1)?,
            timestamp_ms: get(row, 2)?,
            version: get(row, 3)?,
            run_id: get(row, 4)?,
            category: get(row, 5)?,
            severity: get(row, 6)?,
            message: get(row, 7)?,
            occurrences: get(row, 8)?,
        };
        seen_bytes += event_cost(&event)?;
        if seen_bytes > bytes {
            return Err("Invalid plugin health size accounting".into());
        }
        health.events.push(event);
    }
    owners
        .into_iter()
        .map(|(owner, (health, details, stored))| {
            if retained_bytes(&owner, &health, details)? != stored {
                return Err("Invalid plugin health size accounting".into());
            }
            Ok((owner, health))
        })
        .collect()
}

// Convert a typed column without echoing corrupt health details
fn get<T: rusqlite::types::FromSql>(row: &rusqlite::Row<'_>, index: usize) -> Result<T, String> {
    row.get(index)
        .map_err(|_| "Invalid persisted plugin health column".into())
}

// Reject corrupted plugin-health data in candidate backups as well as at startup
pub(crate) fn validate(connection: &Connection) -> Result<(), String> {
    read(connection).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use only a disposable relational database with the real schema and cascading references
    fn db() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let tx = connection.transaction().unwrap();
        create(&tx).unwrap();
        tx.commit().unwrap();
        connection
    }

    // Give records synthetic identifiers and exercise all kinds of detail
    fn fixture(timestamp_ms: u64) -> Event {
        Event {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp_ms,
            version: "0.1.1".into(),
            run_id: Some("fixture-run".into()),
            category: "crash".into(),
            severity: "error".into(),
            message: "Synthetic crash".into(),
            occurrences: 1,
        }
    }

    #[test]
    fn health_roundtrips_and_only_updates_dirty_owner_and_changed_events() {
        let mut connection = db();
        let mut entries = vec![
            (
                "fixture.one".into(),
                Health {
                    session: Counts {
                        issues: 9,
                        ..Counts::default()
                    },
                    totals: Counts {
                        crashes: 1,
                        issues: 7,
                        ..Counts::default()
                    },
                    last_registration_ms: Some(100),
                    last_unexpected_exit_ms: Some(110),
                    last_issue_ms: Some(120),
                    last_exit: Some("fixture exit".into()),
                    last_exit_code: Some(-15),
                    last_exit_signal: Some(4),
                    blocked_reason: Some("fixture blocked".into()),
                    last_run_peak_resident_bytes: Some(1000),
                    restart_attempts: 1,
                    recent_failures: vec!["fixture".into()],
                    missing_assets: vec!["missing".into()],
                    unsupported_apis: vec!["unsupported".into()],
                    events: vec![fixture(100), fixture(90)],
                },
            ),
            (
                "fixture.two".into(),
                Health {
                    events: vec![fixture(1)],
                    ..Health::default()
                },
            ),
        ];
        let tx = connection.transaction().unwrap();
        write_dirty(&tx, &entries).unwrap();
        tx.commit().unwrap();
        let listed = read(&connection).unwrap();
        for (owner, health) in &entries {
            assert_eq!(listed.get(owner), Some(health));
        }
        assert_eq!(
            listed["fixture.one"]
                .events
                .iter()
                .map(|e| e.timestamp_ms)
                .collect::<Vec<_>>(),
            [100, 90]
        );

        // Protect another owner's summary/history and an unchanged event against physical writes
        connection.execute_batch(&format!("
            CREATE TEMP TRIGGER no_other_update BEFORE UPDATE ON plugin_health_summary WHEN old.owner='fixture.two'
                BEGIN SELECT raise(ABORT,'unexpected summary update'); END;
            CREATE TEMP TRIGGER no_other_delete BEFORE DELETE ON plugin_health_summary WHEN old.owner='fixture.two'
                BEGIN SELECT raise(ABORT,'unexpected summary delete'); END;
            CREATE TEMP TRIGGER no_event_update BEFORE UPDATE ON plugin_health_events
                WHEN old.owner='fixture.two' OR old.event_id='{}'
                BEGIN SELECT raise(ABORT,'unexpected event update'); END;
            CREATE TEMP TRIGGER no_event_delete BEFORE DELETE ON plugin_health_events
                WHEN old.owner='fixture.two' OR old.event_id='{}'
                BEGIN SELECT raise(ABORT,'unexpected event delete'); END;",
            entries[0].1.events[0].id, entries[0].1.events[0].id
        )).unwrap();
        let other = entries.pop().unwrap();
        entries[0].1.events[1].occurrences = 2;
        entries[0].1.events.push(fixture(80));
        entries[0].1.totals.crashes += 1;
        let tx = connection.transaction().unwrap();
        write_dirty(&tx, &entries).unwrap();
        tx.commit().unwrap();

        let listed = read(&connection).unwrap();
        assert_eq!(listed.get(&entries[0].0), Some(&entries[0].1));
        assert_eq!(listed.get(&other.0), Some(&other.1));
    }

    #[test]
    fn health_caps_pruned_events_and_cascades_owner_removal() {
        let mut connection = db();
        let mut entries = vec![(
            "fixture".into(),
            Health {
                events: (0..MAX_EVENTS as u64).map(fixture).collect(),
                ..Health::default()
            },
        )];
        let tx = connection.transaction().unwrap();
        write_dirty(&tx, &entries).unwrap();
        tx.commit().unwrap();

        entries[0].1.events.push(fixture(MAX_EVENTS as u64));
        assert!(bounded_cost(&entries).is_err());
        let pruned = entries[0].1.events.remove(0);
        let tx = connection.transaction().unwrap();
        write_dirty(&tx, &entries).unwrap();
        tx.commit().unwrap();
        let loaded = read(&connection).unwrap();
        assert_eq!(loaded["fixture"], entries[0].1);
        assert!(loaded["fixture"].events.iter().all(|e| e.id != pruned.id));

        let tx = connection.transaction().unwrap();
        delete(&tx, "fixture").unwrap();
        tx.commit().unwrap();
        assert!(read(&connection).unwrap().is_empty());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM plugin_health_events", [], |r| r
                    .get::<_, usize>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn health_refuses_corrupt_accounting_or_detail_types() {
        let mut connection = db();
        let tx = connection.transaction().unwrap();
        write_dirty(
            &tx,
            &[(
                "fixture".into(),
                Health {
                    events: vec![fixture(1)],
                    ..Health::default()
                },
            )],
        )
        .unwrap();
        tx.commit().unwrap();
        connection
            .execute("UPDATE plugin_health_summary SET retained_bytes=1", [])
            .unwrap();
        assert!(validate(&connection)
            .unwrap_err()
            .contains("size accounting"));
        connection
            .execute("UPDATE plugin_health_summary SET recent_failures='[1]'", [])
            .unwrap();
        assert!(validate(&connection).unwrap_err().contains("details"));
    }
}
