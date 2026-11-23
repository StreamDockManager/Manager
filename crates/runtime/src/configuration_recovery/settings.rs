//! Closed, non-secret preference recovery surface. Never instantiate live services
use super::Record;
use serde_json::Value;
use stream_dock_manager_storage::{rusqlite::Connection, Domain};

type Validator = fn(&Connection) -> Result<(), String>;
struct Setting {
    domain: Domain,
    key: &'static str,
    limit: usize,
    object: bool,
    validate: Validator,
}
// Define the exact allowlist of preferences editable through configuration recovery
fn settings() -> [Setting; 8] {
    use crate::{application_data as app, desktop, profile_database::LocalPreference as Local};
    macro_rules! manager {
        ($record:expr, $object:expr) => {
            Setting {
                domain: $record.domain,
                key: $record.key,
                limit: $record.max_bytes,
                object: $object,
                validate: |conn| $record.read_from(conn).map(|_| ()),
            }
        };
    }
    macro_rules! local {
        ($name:ident, $validate:path) => {{
            let record = Local::$name.snapshot_record::<Value>();
            Setting {
                domain: record.domain,
                key: record.key,
                limit: record.max_bytes,
                object: true,
                validate: $validate,
            }
        }};
    }
    [
        manager!(desktop::SETTINGS_RECORD, true),
        manager!(app::ACCESSIBILITY_RECORD, true),
        manager!(app::OFFLINE_ICONS_RECORD, false),
        manager!(app::DIAGNOSTICS_RECORD, false),
        local!(DevicePower, crate::power_policy::validate_storage_snapshot),
        local!(
            ResourceAlerts,
            crate::resource_alerts::validate_storage_snapshot
        ),
        local!(
            LibraryOrganization,
            crate::library_organization::validate_storage_snapshot
        ),
        manager!(app::FIRMWARE_TEST_CATALOG_RECORD, false),
    ]
}
impl Setting {
    // Encode a recovery setting identity from its table and key
    fn id(&self) -> String {
        format!("setting:{}:{}", self.domain.table(), self.key)
    }
}
// Resolve the supplied settings request
fn resolve(record: &Record) -> Result<Setting, String> {
    settings()
        .into_iter()
        .find(|setting| {
            record.kind == "setting" && record.key == setting.key && record.id == setting.id()
        })
        .ok_or_else(|| "Setting is not available for recovery".into())
}
// Read existing allowlisted settings as raw JSON so invalid typed values remain repairable
pub(super) fn inventory(conn: &Connection) -> Result<Vec<Record>, String> {
    let mut records = Vec::new();
    for setting in settings() {
        // Read as JSON, not the valid domain type: invalid typed values must remain editable
        if let Some(value) = stream_dock_manager_storage::read_json::<Value>(
            conn,
            setting.domain,
            setting.key,
            super::LIMIT,
        )? {
            records.push(Record {
                id: setting.id(),
                key: setting.key.into(),
                kind: "setting",
                value,
            });
        }
    }
    Ok(records)
}
// Validate the supplied settings value
fn validate(setting: &Setting, value: Option<&Value>) -> Result<(), String> {
    if setting.object && value.is_some_and(|value| !value.is_object()) {
        return Err("Saved preference must be a JSON object".into());
    }
    // The production record readers and semantic validators are authoritative
    // Isolate the candidate so unrelated damaged records cannot block its repair
    let conn = Connection::open_in_memory().map_err(|error| error.to_string())?;
    conn.execute_batch(&format!(
        "CREATE TABLE {} (key TEXT PRIMARY KEY, payload TEXT NOT NULL, revision INTEGER NOT NULL)",
        setting.domain.table(),
    ))
    .map_err(|error| error.to_string())?;
    if let Some(value) = value {
        stream_dock_manager_storage::write_json(
            &conn,
            setting.domain,
            setting.key,
            value,
            setting.limit,
        )?;
    }
    (setting.validate)(&conn)
}
// Validate a proposed setting with its allowlisted authoritative decoder
pub(super) fn issues(record: &Record, value: &Value) -> Vec<String> {
    resolve(record)
        .and_then(|setting| validate(&setting, Some(value)))
        .err()
        .into_iter()
        .collect()
}
// Apply the reviewed settings change
pub(super) fn apply(
    conn: &Connection,
    record: &Record,
    value: Option<&Value>,
) -> Result<(), String> {
    let setting = resolve(record)?;
    validate(&setting, value)?;
    if let Some(value) = value {
        stream_dock_manager_storage::write_json(
            conn,
            setting.domain,
            setting.key,
            value,
            setting.limit,
        )
    } else {
        stream_dock_manager_storage::delete(conn, setting.domain, setting.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    // Create in-memory preference tables for recovery setting tests
    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        for domain in [Domain::ManagerPreferences, Domain::LocalPreferences] {
            conn.execute_batch(&format!("CREATE TABLE {} (key TEXT PRIMARY KEY, payload TEXT NOT NULL, revision INTEGER NOT NULL)", domain.table())).unwrap();
        }
        conn
    }
    // Record the new settings observation
    fn record(setting: &Setting) -> Record {
        Record {
            id: setting.id(),
            key: setting.key.into(),
            kind: "setting",
            value: Value::Null,
        }
    }
    #[test]
    // Verify that inventory is allowlisted and keeps invalid typed values
    fn inventory_is_allowlisted_and_keeps_invalid_typed_values() {
        let conn = database();
        for (domain, key, value) in [
            (
                Domain::ManagerPreferences,
                "offline_icons",
                json!("invalid"),
            ),
            (Domain::ManagerPreferences, "credentials", json!("private")),
            (
                Domain::LocalPreferences,
                "restart-recovery-once",
                json!(true),
            ),
            (
                Domain::LocalPreferences,
                "plugin-update-policies",
                json!({}),
            ),
        ] {
            stream_dock_manager_storage::write_json(&conn, domain, key, &value, 4096).unwrap();
        }
        let items = inventory(&conn).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "setting:manager_preferences:offline_icons");
        assert!(!issues(&items[0], &items[0].value).is_empty());
    }
    #[test]
    // Verify that invalid edit does not change database and reset is safe
    fn invalid_edit_does_not_change_database_and_reset_is_safe() {
        let conn = database();
        let item = record(&settings()[2]);
        apply(&conn, &item, Some(&json!(true))).unwrap();
        assert!(apply(&conn, &item, Some(&json!("false"))).is_err());
        assert_eq!(inventory(&conn).unwrap()[0].value, json!(true));
        apply(&conn, &item, None).unwrap();
        assert!(inventory(&conn).unwrap().is_empty());
        for setting in settings() {
            apply(&conn, &record(&setting), None).unwrap();
        }
    }
    #[test]
    // Verify that production semantic and object validation is used
    fn production_semantic_and_object_validation_is_used() {
        for setting in settings().into_iter().filter(|setting| setting.object) {
            let item = record(&setting);
            assert!(!issues(&item, &json!([])).is_empty());
        }
        let item = record(&settings()[5]);
        assert!(!issues(
            &item,
            &json!({"schema_version":999,"revision":0,"entries":{}})
        )
        .is_empty());
        let accessibility =
            serde_json::to_value(crate::application_data::AccessibilityPreferences::default())
                .unwrap();
        assert!(issues(&record(&settings()[1]), &accessibility).is_empty());
    }
    #[test]
    // Verify that local settings accept current records but reject future versions
    fn local_settings_accept_current_records_but_reject_future_versions() {
        let conn = database();
        for (index, mut value) in [
            (4, json!({"schema_version":1,"revision":0,"entries":{}})),
            (5, json!({"schema_version":1,"revision":0,"entries":{}})),
            (
                6,
                json!({"schema_version":1,"organization":{"revision":0,"entries":[]}}),
            ),
        ] {
            let item = record(&settings()[index]);
            assert!(issues(&item, &value).is_empty());
            apply(&conn, &item, Some(&value)).unwrap();
            value["schema_version"] = json!(999);
            assert!(!issues(&item, &value).is_empty());
            assert!(apply(&conn, &item, Some(&value)).is_err());
        }
        assert_eq!(inventory(&conn).unwrap().len(), 3);
    }
    #[test]
    // Verify that identity cannot select other domains or keys
    fn identity_cannot_select_other_domains_or_keys() {
        let conn = database();
        let mut item = record(&settings()[2]);
        item.id = "setting:local_service_preferences:restart-recovery-once".into();
        item.key = "restart-recovery-once".into();
        assert!(apply(&conn, &item, Some(&json!(true))).is_err());
        assert!(apply(&conn, &item, None).is_err());
    }
}
