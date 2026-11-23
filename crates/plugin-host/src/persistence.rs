//! SQLite persistence for host-owned settings. Runtime tokens and inspector drafts stay in memory
use super::*;
use std::time::Instant;
use stream_dock_manager_storage::{
    plugin_installations::{self, State},
    Domain, Storage,
};

const LIMIT: usize = 16 * 1024 * 1024;
const MAX_ROWS: usize = 65_536;

/// Exact encoded snapshot accounting, invalidated by any unknown/full snapshot operation.
#[derive(Default)]
pub(super) struct Sizes {
    rows: std::collections::BTreeMap<(u8, String), usize>,
    sums: [usize; 3],
    counts: [usize; 3],
    installation_bytes: usize,
    installation_rows: std::collections::BTreeMap<String, usize>,
}
impl Sizes {
    fn update(&mut self, bucket: u8, key: &str, size: Option<usize>) {
        if let Some(old) = self.rows.remove(&(bucket, key.into())) {
            self.sums[bucket as usize] -= old;
            self.counts[bucket as usize] -= 1;
        }
        if let Some(size) = size {
            self.rows.insert((bucket, key.into()), size);
            self.sums[bucket as usize] += size;
            self.counts[bucket as usize] += 1;
        }
    }
    fn bytes(&self) -> usize {
        // Three containers plus the enclosing tuple; each row includes its trailing comma.
        10 + self.sums.iter().sum::<usize>() - self.counts.iter().filter(|n| **n > 0).count()
            + self.installation_bytes
    }
}
/// Last successful scoped transaction. Durations cover synchronous persistence while
/// the caller owns the hub lock; counters never include setting values or identifiers.
#[derive(Clone, Default, Debug, Serialize)]
pub struct Work {
    pub records: usize,
    pub serialized_bytes: usize,
    pub read_bytes: usize,
    pub changed: usize,
    pub transaction_us: u128,
    pub held_us: u128,
}
/// Explicit bounded dirty keys. Missing values are intentional deletions, never a domain sweep.
pub(super) struct Scope<'a> {
    pub globals: &'a [&'a str],
    pub defaults: &'a [&'a str],
    pub metadata: &'a [&'a str],
}
struct Record {
    bucket: u8,
    key: String,
    value: Option<Value>,
    encoded: Option<stream_dock_manager_storage::EncodedJson>,
    size: Option<usize>,
}
fn record(bucket: u8, key: &str, value: Option<Value>) -> Result<Record, String> {
    let encoded = value
        .as_ref()
        .map(|v| stream_dock_manager_storage::EncodedJson::new(v, LIMIT))
        .transpose()?;
    let size = encoded
        .as_ref()
        .map(|encoded| -> Result<usize, String> {
            Ok(serde_json::to_vec(key).map_err(|e| e.to_string())?.len()
                + encoded.len()
                + if bucket == 2 { 4 } else { 2 })
        })
        .transpose()?;
    Ok(Record {
        bucket,
        key: key.into(),
        value,
        encoded,
        size,
    })
}
fn domain(bucket: u8) -> Domain {
    match bucket {
        0 => Domain::PluginGlobals,
        1 => Domain::PluginDefaults,
        _ => Domain::PluginMetadata,
    }
}

// Replace records
fn replace_records<T: Serialize>(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
    domain: Domain,
    records: HashMap<String, T>,
) -> Result<(), String> {
    let mut previous: HashMap<String, Value> =
        stream_dock_manager_storage::list_json(connection, domain, MAX_ROWS, LIMIT)?
            .into_iter()
            .collect();
    for (key, value) in records {
        let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
        if previous.remove(&key).as_ref() != Some(&value) {
            stream_dock_manager_storage::write_json(connection, domain, &key, &value, LIMIT)?;
            stored_data::mark_updated(connection, &key)?;
        }
    }
    for key in previous.keys() {
        stream_dock_manager_storage::delete(connection, domain, key)?;
        stored_data::mark_updated(connection, key)?;
    }
    Ok(())
}

pub(super) struct Snapshot {
    globals: HashMap<String, Value>,
    defaults: HashMap<String, HashMap<String, Value>>,
    metadata: Vec<(&'static str, Value)>,
    installation_rows: std::collections::BTreeMap<String, State>,
    pub bytes: usize,
}

fn installation_size(id: &str, state: &State) -> Result<usize, String> {
    Ok(serde_json::to_vec(id).map_err(|e| e.to_string())?.len()
        + serde_json::to_vec(state).map_err(|e| e.to_string())?.len()
        + 2)
}

impl Snapshot {
    fn sizes(&self) -> Result<Sizes, String> {
        let mut sizes = Sizes::default();
        for (key, value) in &self.globals {
            let row = record(0, key, Some(value.clone()))?;
            sizes.update(0, key, row.size);
        }
        for (key, value) in &self.defaults {
            let row = record(1, key, Some(json!(value)))?;
            sizes.update(1, key, row.size);
        }
        for (key, value) in &self.metadata {
            let row = record(2, key, Some(value.clone()))?;
            sizes.update(2, key, row.size);
        }
        for (id, state) in &self.installation_rows {
            let size = installation_size(id, state)?;
            sizes.installation_rows.insert(id.clone(), size);
            sizes.installation_bytes += size;
        }
        debug_assert_eq!(sizes.bytes(), self.bytes);
        Ok(sizes)
    }
    // Write for the current operation
    pub fn write(
        self,
        tx: &stream_dock_manager_storage::rusqlite::Transaction<'_>,
    ) -> Result<(), String> {
        replace_records(tx, Domain::PluginGlobals, self.globals)?;
        replace_records(tx, Domain::PluginDefaults, self.defaults)?;
        plugin_installations::replace_all(tx, self.installation_rows)?;
        for (key, value) in self.metadata {
            let previous: Option<Value> =
                stream_dock_manager_storage::read_json(tx, Domain::PluginMetadata, key, LIMIT)?;
            if previous.as_ref() != Some(&value) {
                stream_dock_manager_storage::write_json(
                    tx,
                    Domain::PluginMetadata,
                    key,
                    &value,
                    LIMIT,
                )?;
            }
        }
        Ok(())
    }
}

impl PluginHost {
    /// Read bounded, value-free work counters for the last scoped settings transaction.
    pub fn persistence_work(&self) -> PersistenceWork {
        self.hub
            .lock()
            .unwrap()
            .persistence_work
            .lock()
            .unwrap()
            .clone()
    }
}

impl Hub {
    /// Direct domain transactions must invalidate accounting after their commit.
    pub(super) fn invalidate_persistence_sizes(&self) {
        *self.persistence_sizes.lock().unwrap() = None;
    }
    // Persist for the current operation
    pub(super) fn persist(&self) -> Result<(), String> {
        if self.store.is_none() {
            return Ok(());
        }
        self.persist_with(|_| Ok(()))
    }
    // Persist with
    pub(super) fn persist_with<T: Send + 'static>(
        &self,
        extra: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<T, String>
            + Send
            + 'static,
    ) -> Result<T, String> {
        let storage = self.store.as_ref().ok_or("Storage is unavailable")?;
        let snapshot = self.snapshot()?;
        let sizes = snapshot.sizes()?;
        let result = storage.transaction_with_cost(snapshot.bytes, move |tx| {
            snapshot.write(tx)?;
            extra(tx)
        })?;
        *self.persistence_sizes.lock().unwrap() = Some(sizes);
        self.refresh_log_secrets();
        Ok(result)
    }
    fn installation_state(&self, id: &str) -> State {
        State {
            enabled: self.enabled.contains(id),
            sleep_when_hidden: self.sleep_when_hidden.get(id).copied(),
            retained_name: self.retained.get(id).cloned(),
            installation_uuid: self.installations.get(id).cloned(),
            provenance: self.provenance.get(id).cloned(),
        }
    }

    // Common UI toggles write exactly one installation row
    pub(super) fn persist_installation(&self, id: &str) -> Result<(), String> {
        let Some(storage) = &self.store else {
            return Ok(());
        };
        let row = self.installation_state(id);
        if self.persistence_sizes.lock().unwrap().is_none() {
            // Recheck the in-memory budget without persisting unrelated settings
            self.snapshot()?;
            let key = id.to_owned();
            let cost = installation_size(id, &row)?;
            return storage
                .transaction_with_cost(cost, move |tx| plugin_installations::put(tx, &key, &row));
        }
        let size = if row == State::default() {
            0
        } else {
            installation_size(id, &row)?
        };
        let mut accounting = self.persistence_sizes.lock().unwrap();
        let sizes = accounting.as_mut().unwrap();
        let old = sizes.installation_rows.get(id).copied().unwrap_or(0);
        if sizes.bytes() - old + size > LIMIT {
            return Err("plugin settings exceed 16 MiB".into());
        }
        let key = id.to_owned();
        storage.transaction_with_cost(old.max(size), move |tx| {
            plugin_installations::put(tx, &key, &row)
        })?;
        if size == 0 {
            sizes.installation_rows.remove(id);
        } else {
            sizes.installation_rows.insert(id.into(), size);
        }
        sizes.installation_bytes = sizes.installation_bytes - old + size;
        Ok(())
    }

    // Collect plugin settings and metadata for a durable storage transaction
    pub(super) fn snapshot(&self) -> Result<Snapshot, String> {
        self.invalidate_persistence_sizes();
        let globals = self.globals.clone();
        let defaults = self.action_defaults.clone();
        let metadata = vec![
            ("backup_receipts", json!(self.backup_receipts)),
            ("appearance", json!(self.appearance)),
            ("ui_appearance", json!(self.ui_appearance)),
            ("ui_theme_style", json!(self.ui_theme_style)),
            ("custom_palettes", json!(self.custom_palettes)),
            ("device_theme", json!(self.device_theme)),
            ("system_dark", json!(self.system_dark)),
        ];
        let installation_rows: std::collections::BTreeMap<_, _> = self
            .enabled
            .iter()
            .chain(self.sleep_when_hidden.keys())
            .chain(self.retained.keys())
            .chain(self.installations.keys())
            .chain(self.provenance.keys())
            .map(|id| (id.clone(), self.installation_state(id)))
            .collect();
        let installation_bytes = installation_rows
            .iter()
            .try_fold(0usize, |bytes, (id, row)| {
                Ok::<_, String>(bytes + installation_size(id, row)?)
            })?;
        let encoded_bytes = serde_json::to_vec(&(&globals, &defaults, &metadata))
            .map_err(|e| e.to_string())?
            .len()
            + installation_bytes;
        if encoded_bytes > LIMIT {
            return Err("plugin settings exceed 16 MiB".into());
        }
        Ok(Snapshot {
            globals,
            defaults,
            metadata,
            installation_rows,
            bytes: encoded_bytes,
        })
    }

    fn metadata_value(&self, key: &str) -> Result<Value, String> {
        Ok(match key {
            "backup_receipts" => json!(self.backup_receipts),
            "appearance" => json!(self.appearance),
            "ui_appearance" => json!(self.ui_appearance),
            "ui_theme_style" => json!(self.ui_theme_style),
            "custom_palettes" => json!(self.custom_palettes),
            "device_theme" => json!(self.device_theme),
            "system_dark" => json!(self.system_dark),
            _ => return Err("Unknown scoped plugin metadata record".into()),
        })
    }
    pub(super) fn persist_scoped(&self, scope: Scope<'_>) -> Result<(), String> {
        if self.store.is_none() {
            return Ok(());
        }
        self.persist_scoped_with(scope, |_| Ok(()))
    }
    pub(super) fn persist_scoped_with<T: Send + 'static>(
        &self,
        scope: Scope<'_>,
        extra: impl FnOnce(&stream_dock_manager_storage::rusqlite::Transaction<'_>) -> Result<T, String>
            + Send
            + 'static,
    ) -> Result<T, String> {
        let started = Instant::now();
        let storage = self.store.as_ref().ok_or("Storage is unavailable")?;
        if scope.globals.len() + scope.defaults.len() + scope.metadata.len() > 128 {
            return Err("Scoped plugin persistence exceeds record limit".into());
        }
        // First write or an import/restore of unknown scope re-establishes exact accounting.
        if self.persistence_sizes.lock().unwrap().is_none() {
            return self.persist_with(extra);
        }
        let mut records = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for (bucket, keys) in [(0, scope.globals), (1, scope.defaults), (2, scope.metadata)] {
            for key in keys {
                if !seen.insert((bucket, *key)) {
                    continue;
                }
                let value = match bucket {
                    0 => self.globals.get(*key).cloned(),
                    1 => self.action_defaults.get(*key).map(|v| json!(v)),
                    _ => Some(self.metadata_value(key)?),
                };
                records.push(record(bucket, key, value)?);
            }
        }
        let accounting_guard = self.persistence_sizes.lock().unwrap();
        let sizes = accounting_guard.as_ref().unwrap();
        let mut sums = sizes.sums;
        let mut counts = sizes.counts;
        for row in &records {
            let index = row.bucket as usize;
            if let Some(old) = sizes.rows.get(&(row.bucket, row.key.clone())) {
                sums[index] -= old;
                counts[index] -= 1;
            }
            if let Some(size) = row.size {
                sums[index] += size;
                counts[index] += 1;
            }
        }
        let bytes = 10 + sums.iter().sum::<usize>() - counts.iter().filter(|n| **n > 0).count()
            + sizes.installation_bytes;
        if bytes > LIMIT {
            return Err("plugin settings exceed 16 MiB".into());
        }
        // Account for the old row as well: deleting/replacing a large value still reads it.
        let cost: usize = records
            .iter()
            .map(|r| {
                r.size
                    .unwrap_or(0)
                    .max(*sizes.rows.get(&(r.bucket, r.key.clone())).unwrap_or(&0))
            })
            .sum();
        drop(accounting_guard);
        let updates: Vec<_> = records
            .iter()
            .map(|r| (r.bucket, r.key.clone(), r.size))
            .collect();
        let serialized_bytes = records
            .iter()
            .filter_map(|r| r.encoded.as_ref().map(|v| v.len()))
            .sum();
        let transaction = Instant::now();
        let (result, mut work) = storage.transaction_with_cost(cost, move |tx| {
            let mut work = Work {
                records: records.len(),
                serialized_bytes,
                ..Default::default()
            };
            for row in records {
                let previous: Option<Value> = stream_dock_manager_storage::read_json(
                    tx,
                    domain(row.bucket),
                    &row.key,
                    LIMIT,
                )?;
                work.read_bytes += previous.as_ref().map(|v| v.to_string().len()).unwrap_or(0);
                if previous == row.value {
                    continue;
                }
                if let Some(encoded) = row.encoded {
                    encoded.write(tx, domain(row.bucket), &row.key)?;
                } else {
                    stream_dock_manager_storage::delete(tx, domain(row.bucket), &row.key)?;
                }
                if row.bucket != 2 {
                    stored_data::mark_updated(tx, &row.key)?;
                }
                work.changed += 1;
            }
            Ok((extra(tx)?, work))
        })?;
        work.transaction_us = transaction.elapsed().as_micros();
        // Hub mutex remains held by every caller, so no stale publication or asynchronous ACK.
        let mut accounting = self.persistence_sizes.lock().unwrap();
        let accounting = accounting.as_mut().unwrap();
        for (bucket, key, size) in updates {
            accounting.update(bucket, &key, size);
        }
        self.refresh_log_secrets();
        work.held_us = started.elapsed().as_micros();
        *self.persistence_work.lock().unwrap() = work;
        Ok(result)
    }

    // Load for the current operation
    pub(super) fn load(storage: &Storage) -> Result<Self, String> {
        let mut hub = storage.run(|connection| Self::load_connection(connection))?;
        hub.store = Some(storage.clone());
        let sizes = hub.snapshot()?.sizes()?;
        *hub.persistence_sizes.lock().unwrap() = Some(sizes);
        Ok(hub)
    }
    #[allow(clippy::field_reassign_with_default)]
    // Load connection
    pub(super) fn load_connection(
        connection: &stream_dock_manager_storage::rusqlite::Connection,
    ) -> Result<Self, String> {
        let metadata = |key, default| -> Result<Value, String> {
            Ok(stream_dock_manager_storage::read_json(
                connection,
                Domain::PluginMetadata,
                key,
                LIMIT,
            )?
            .unwrap_or(default))
        };
        let mut hub = Self::default();
        hub.globals = stream_dock_manager_storage::list_json(
            connection,
            Domain::PluginGlobals,
            MAX_ROWS,
            LIMIT,
        )?
        .into_iter()
        .collect();
        hub.action_defaults = stream_dock_manager_storage::list_json(
            connection,
            Domain::PluginDefaults,
            MAX_ROWS,
            LIMIT,
        )?
        .into_iter()
        .collect();
        hub.appearance = serde_json::from_value(metadata("appearance", json!({}))?)
            .map_err(|e| e.to_string())?;
        hub.ui_appearance = serde_json::from_value(metadata("ui_appearance", json!(""))?)
            .map_err(|e| e.to_string())?;
        hub.ui_theme_style = stream_dock_manager_storage::read_json(
            connection,
            Domain::PluginMetadata,
            "ui_theme_style",
            4096,
        )?
        .unwrap_or_default();
        hub.custom_palettes = theme::load_custom_palettes(connection)?;
        hub.device_theme = stream_dock_manager_storage::read_json(
            connection,
            Domain::PluginMetadata,
            "device_theme",
            LIMIT,
        )?
        .unwrap_or_default();
        hub.ui_theme_style.validate(&hub.custom_palettes)?;
        hub.system_dark = serde_json::from_value(metadata("system_dark", json!(true))?)
            .map_err(|e| e.to_string())?;
        for (id, state) in plugin_installations::list(connection)? {
            if state.enabled {
                hub.enabled.insert(id.clone());
            }
            if let Some(value) = state.sleep_when_hidden {
                hub.sleep_when_hidden.insert(id.clone(), value);
            }
            if let Some(value) = state.retained_name {
                hub.retained.insert(id.clone(), value);
            }
            if let Some(value) = state.installation_uuid {
                hub.installations.insert(id.clone(), value);
            }
            if let Some(value) = state.provenance {
                hub.provenance.insert(id, value);
            }
        }
        hub.backup_receipts = serde_json::from_value(metadata("backup_receipts", json!({}))?)
            .map_err(|e| e.to_string())?;
        if hub.backup_receipts.len() > 4096 || hub.installations.len() > 4096 {
            return Err("Package identity index exceeds bounds".into());
        }
        for (id, digest) in &hub.backup_receipts {
            if uuid::Uuid::parse_str(id)
                .map_err(|_| "Invalid backup receipt identity")?
                .to_string()
                != *id
                || digest.len() != 64
                || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("Invalid backup receipt".into());
            }
        }
        for (id, token) in &hub.installations {
            if !safe_id(id) || uuid::Uuid::parse_str(token).is_err() {
                return Err("Invalid installation identity".into());
            }
        }
        hub.health = stream_dock_manager_storage::plugin_health::read(connection)?
            .into_iter()
            .map(|(id, health)| (id, monitor::from_stored_health(health)))
            .collect();
        if hub.sleep_when_hidden.len() > 4096 || hub.sleep_when_hidden.keys().any(|id| !safe_id(id))
        {
            return Err("Invalid plugin sleep preferences".into());
        }
        for health in hub.health.values_mut() {
            health.session = Default::default();
        }
        Ok(hub)
    }
}

/// Pure, bounded validation for cold restore and snapshot publication
pub fn validate_storage_snapshot(
    connection: &stream_dock_manager_storage::rusqlite::Connection,
) -> Result<(), String> {
    recovery::validate_snapshot(connection)?;
    let hub = Hub::load_connection(connection)?;
    hub.snapshot()?;
    for (owner, value) in &hub.globals {
        // OpenAction settings are opaque JSON, including scalars and arrays
        if !safe_id(owner) || value.to_string().len() > 256 * 1024 {
            return Err("Invalid saved plugin globals".into());
        }
    }
    for (owner, defaults) in &hub.action_defaults {
        if !safe_id(owner)
            || defaults.len() > 4096
            || defaults
                .iter()
                .any(|(action, value)| !safe_id(action) || value.to_string().len() > 256 * 1024)
        {
            return Err("Invalid saved plugin action defaults".into());
        }
    }
    if hub.retained.len() > 4096
        || hub
            .retained
            .iter()
            .any(|(id, name)| !safe_id(id) || name.len() > 2048)
    {
        return Err("Invalid retained plugin identity".into());
    }
    if let Some(value) = stream_dock_manager_storage::read_json::<Value>(
        connection,
        Domain::LoggingPreferences,
        "preferences",
        1024 * 1024,
    )? {
        if !value.is_object() {
            return Err("Invalid saved logging preferences".into());
        }
        let preferences: log_capture::LogPreferences =
            serde_json::from_value(value).map_err(|_| "Invalid saved logging preferences")?;
        if preferences.plugins.len() > 4096 || preferences.plugins.keys().any(|id| !safe_id(id)) {
            return Err("Logging preference index exceeds bounds".into());
        }
    }
    for (key, value) in stream_dock_manager_storage::list_json::<Value>(
        connection,
        Domain::PluginMetadata,
        16384,
        LIMIT,
    )? {
        if let Some(id) = key
            .strip_prefix("storage-updated:")
            .or_else(|| key.strip_prefix("storage-removed:"))
        {
            if !safe_id(id) || value.as_u64().is_none() {
                return Err("Invalid durable plugin generation".into());
            }
        } else if let Some(id) = key.strip_prefix("retained-family:") {
            if !safe_id(id) || serde_json::from_value::<(String, bool)>(value).is_err() {
                return Err("Invalid retained package family".into());
            }
        } else if ![
            "backup_receipts",
            "appearance",
            "ui_appearance",
            "ui_theme_style",
            "custom_palettes",
            "device_theme",
            "system_dark",
        ]
        .contains(&key.as_str())
        {
            return Err("Unsupported saved plugin metadata record; original preserved".into());
        }
    }
    storage_rpc::validate_snapshot(connection)
}

#[cfg(test)]
mod scoped_tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Hub) {
        let root = tempfile::tempdir().unwrap();
        let mut hub = Hub {
            store: Some(Storage::open(root.path().into()).unwrap()),
            ..Hub::default()
        };
        hub.globals.insert("fixture.one".into(), json!({"value":1}));
        hub.globals.insert(
            "fixture.other".into(),
            json!({"large":"x".repeat(128*1024)}),
        );
        hub.persist().unwrap();
        (root, hub)
    }
    fn one() -> Scope<'static> {
        Scope {
            globals: &["fixture.one"],
            defaults: &[],
            metadata: &[],
        }
    }
    #[test]
    fn exact_installation_writes_preserve_other_plugin_settings_and_row_fields() {
        let (_root, mut hub) = fixture();
        let shared = hub.store.as_ref().unwrap().clone();
        hub.enabled.insert("fixture.one".into());
        hub.sleep_when_hidden.insert("fixture.one".into(), false);
        hub.retained
            .insert("fixture.other".into(), "Retained".into());
        hub.installations
            .insert("fixture.one".into(), uuid::Uuid::new_v4().to_string());
        hub.provenance
            .insert("fixture.one".into(), json!({"source":"one"}));
        hub.persist().unwrap();
        let rows = || shared.run(|db| plugin_installations::list(db)).unwrap();
        let before = rows();
        hub.enabled.remove("fixture.one");
        hub.persist_installation("fixture.one").unwrap();
        let after = rows();
        assert_eq!(after["fixture.other"], before["fixture.other"]);
        assert!(!after["fixture.one"].enabled);
        assert_eq!(after["fixture.one"].sleep_when_hidden, Some(false));
        assert_eq!(
            after["fixture.one"].installation_uuid,
            before["fixture.one"].installation_uuid
        );
        assert_eq!(
            after["fixture.one"].provenance,
            before["fixture.one"].provenance
        );
        assert_eq!(Hub::load(&shared).unwrap().globals, hub.globals);
        for key in [
            "enabled",
            "sleep_when_hidden",
            "retained",
            "installations",
            "provenance",
        ] {
            assert!(shared
                .read_json::<Value>(Domain::PluginMetadata, key, 1024)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn scoped_update_reads_only_selected_row_and_preserves_noop_revision() {
        let (_root, mut hub) = fixture();
        hub.globals.insert("fixture.one".into(), json!({"value":2}));
        hub.persist_scoped(one()).unwrap();
        let work = hub.persistence_work.lock().unwrap();
        assert_eq!(work.records, 1);
        assert_eq!(work.changed, 1);
        assert!(work.read_bytes < 64);
        assert!(work.serialized_bytes < 64);
        drop(work);
        hub.persist_scoped(one()).unwrap();
        assert_eq!(hub.persistence_work.lock().unwrap().changed, 0);
        let reopened = Hub::load(hub.store.as_ref().unwrap()).unwrap();
        assert_eq!(reopened.globals, hub.globals);
    }
    #[test]
    fn scoped_extra_failure_rolls_back_rows_metadata_and_accounting() {
        let (_root, mut hub) = fixture();
        let bytes = hub
            .persistence_sizes
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .bytes();
        hub.globals
            .insert("fixture.one".into(), json!({"value":"replacement"}));
        let result: Result<(), String> = hub.persist_scoped_with(one(), |tx| {
            stream_dock_manager_storage::write_json(
                tx,
                Domain::PluginMetadata,
                "extra",
                &json!(true),
                128,
            )?;
            Err("injected extra failure".into())
        });
        assert!(result.is_err());
        assert_eq!(
            hub.persistence_sizes
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .bytes(),
            bytes
        );
        // The synchronous caller owns memory rollback, just as with full persistence.
        hub.globals.insert("fixture.one".into(), json!({"value":1}));
        let reopened = Hub::load(hub.store.as_ref().unwrap()).unwrap();
        assert_eq!(reopened.globals, hub.globals);
        assert!(hub
            .store
            .as_ref()
            .unwrap()
            .read_json::<Value>(Domain::PluginMetadata, "extra", 128)
            .unwrap()
            .is_none());
    }
    #[test]
    fn scoped_deletion_reinstall_defaults_theme_and_full_fallback() {
        let (_root, mut hub) = fixture();
        hub.globals.remove("fixture.one");
        hub.persist_scoped(one()).unwrap();
        assert!(!Hub::load(hub.store.as_ref().unwrap())
            .unwrap()
            .globals
            .contains_key("fixture.one"));
        hub.action_defaults.insert(
            "fixture.one".into(),
            HashMap::from([("action".into(), json!({"a":true}))]),
        );
        hub.ui_appearance = "light".into();
        hub.persist_scoped(Scope {
            globals: &[],
            defaults: &["fixture.one"],
            metadata: &["ui_appearance"],
        })
        .unwrap();
        hub.globals
            .insert("fixture.one".into(), json!({"new":true}));
        hub.persist_scoped(one()).unwrap();
        let loaded = Hub::load(hub.store.as_ref().unwrap()).unwrap();
        assert_eq!(loaded.action_defaults, hub.action_defaults);
        assert_eq!(loaded.ui_appearance, "light");
        assert_eq!(loaded.globals, hub.globals);
        let snapshot = hub.snapshot().unwrap();
        assert!(hub.persistence_sizes.lock().unwrap().is_none());
        assert_eq!(snapshot.sizes().unwrap().bytes(), snapshot.bytes);
        hub.persist_scoped(one()).unwrap();
        assert!(hub.persistence_sizes.lock().unwrap().is_some());
    }
    #[test]
    fn scoped_size_limit_is_aggregate_and_failed_admission_keeps_baseline() {
        let (_root, mut hub) = fixture();
        let before = hub
            .persistence_sizes
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .bytes();
        hub.globals
            .insert("fixture.one".into(), json!("x".repeat(LIMIT - 100)));
        assert!(hub.persist_scoped(one()).unwrap_err().contains("16 MiB"));
        assert_eq!(
            hub.persistence_sizes
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .bytes(),
            before
        );
    }
    #[test]
    fn scoped_commit_refreshes_log_redaction_before_returning() {
        let (_root, mut hub) = fixture();
        let logs =
            log_capture::LogCaptureService::with_storage(hub.store.as_ref().unwrap().clone())
                .unwrap();
        let capture = logs.start("fixture.one", "Fixture").unwrap();
        hub.logs = Some(logs.clone());
        hub.globals.insert(
            "fixture.one".into(),
            json!({"api_key":"scoped-fixture-credential"}),
        );
        hub.persist_scoped(one()).unwrap();
        logs.record(
            "fixture.one",
            "run",
            "stdout",
            "fixture",
            "value=scoped-fixture-credential",
        );
        logs.stop(&capture.id).unwrap();
        let lease = logs.subscribe(&capture.id).unwrap();
        let page = logs.read(&lease, None, false).unwrap();
        let text = serde_json::to_string(&page).unwrap();
        assert!(!text.contains("scoped-fixture-credential"));
        assert!(text.contains("credential"));
        logs.unsubscribe(&lease.id);
    }
    #[test]
    fn concurrent_scoped_writers_preserve_both_plugins() {
        let (_root, hub) = fixture();
        let shared = std::sync::Arc::new(Mutex::new(hub));
        let workers: Vec<_> = ["fixture.one", "fixture.other"]
            .into_iter()
            .map(|id| {
                let shared = shared.clone();
                std::thread::spawn(move || {
                    for n in 0..20 {
                        let mut hub = shared.lock().unwrap();
                        hub.globals.insert(id.into(), json!({"n":n}));
                        hub.persist_scoped(Scope {
                            globals: &[id],
                            defaults: &[],
                            metadata: &[],
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let hub = shared.lock().unwrap();
        let loaded = Hub::load(hub.store.as_ref().unwrap()).unwrap();
        assert_eq!(loaded.globals["fixture.one"], json!({"n":19}));
        assert_eq!(loaded.globals["fixture.other"], json!({"n":19}));
    }
    #[test]
    fn scoped_large_settings_benchmark_and_concurrent_message_wait() {
        let root = tempfile::tempdir().unwrap();
        let mut hub = Hub {
            store: Some(Storage::open(root.path().into()).unwrap()),
            ..Hub::default()
        };
        for n in 0..256 {
            hub.globals
                .insert(format!("fixture.{n}"), json!({"value":"x".repeat(4096)}));
        }
        hub.persist().unwrap();
        let shared = std::sync::Arc::new(Mutex::new(hub));
        for scoped in [false, true] {
            let mut times = Vec::new();
            let mut waits = Vec::new();
            for n in 0..30 {
                let mut hub = shared.lock().unwrap();
                hub.globals.insert("fixture.one".into(), json!({"value":n}));
                let other = shared.clone();
                let (ready, recv) = std::sync::mpsc::channel();
                let waiter = std::thread::spawn(move || {
                    let now = Instant::now();
                    ready.send(()).unwrap();
                    let _hub = other.lock().unwrap();
                    now.elapsed().as_micros()
                });
                recv.recv().unwrap();
                let start = Instant::now();
                if scoped {
                    hub.persist_scoped(one()).unwrap();
                } else {
                    hub.persist().unwrap();
                }
                times.push(start.elapsed().as_micros());
                drop(hub);
                waits.push(waiter.join().unwrap());
            }
            times.sort();
            waits.sort();
            eprintln!("settings scoped={scoped} payload=256x4096B samples=30 hold_p95_us={} queued_hub_message_p95_us={} last_work={:?}",times[28],waits[28],shared.lock().unwrap().persistence_work.lock().unwrap());
        }
        let hub = shared.lock().unwrap();
        assert_eq!(hub.persistence_work.lock().unwrap().records, 1);
        assert_eq!(
            Hub::load(hub.store.as_ref().unwrap()).unwrap().globals["fixture.one"],
            json!({"value":29})
        );
    }
}
