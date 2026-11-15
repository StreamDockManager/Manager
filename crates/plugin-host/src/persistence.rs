//! SQLite persistence for host-owned settings. Runtime tokens and inspector drafts stay in memory
use super::*;
use stream_dock_manager_storage::{Domain, Storage};

const LIMIT: usize = 16 * 1024 * 1024;
const MAX_ROWS: usize = 65_536;

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
    pub bytes: usize,
}
impl Snapshot {
    // Write for the current operation
    pub fn write(
        self,
        tx: &stream_dock_manager_storage::rusqlite::Transaction<'_>,
    ) -> Result<(), String> {
        replace_records(tx, Domain::PluginGlobals, self.globals)?;
        replace_records(tx, Domain::PluginDefaults, self.defaults)?;
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

impl Hub {
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
        let result = storage.transaction_with_cost(snapshot.bytes, move |tx| {
            snapshot.write(tx)?;
            extra(tx)
        })?;
        self.refresh_log_secrets();
        Ok(result)
    }
    // Collect plugin settings and metadata for a durable storage transaction
    pub(super) fn snapshot(&self) -> Result<Snapshot, String> {
        let globals = self.globals.clone();
        let defaults = self.action_defaults.clone();
        let metadata = vec![
            ("enabled", json!(self.enabled)),
            ("sleep_when_hidden", json!(self.sleep_when_hidden)),
            ("retained", json!(self.retained)),
            ("provenance", json!(self.provenance)),
            ("backup_receipts", json!(self.backup_receipts)),
            ("installations", json!(self.installations)),
            ("appearance", json!(self.appearance)),
            ("ui_appearance", json!(self.ui_appearance)),
            ("ui_theme_style", json!(self.ui_theme_style)),
            ("custom_palettes", json!(self.custom_palettes)),
            ("device_theme", json!(self.device_theme)),
            ("system_dark", json!(self.system_dark)),
        ];
        let encoded_bytes = serde_json::to_vec(&(&globals, &defaults, &metadata))
            .map_err(|e| e.to_string())?
            .len();
        if encoded_bytes > LIMIT {
            return Err("plugin settings exceed 16 MiB".into());
        }
        Ok(Snapshot {
            globals,
            defaults,
            metadata,
            bytes: encoded_bytes,
        })
    }

    // Load for the current operation
    pub(super) fn load(storage: &Storage) -> Result<Self, String> {
        let mut hub = storage.run(|connection| Self::load_connection(connection))?;
        hub.store = Some(storage.clone());
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
        hub.retained =
            serde_json::from_value(metadata("retained", json!({}))?).map_err(|e| e.to_string())?;
        hub.enabled =
            serde_json::from_value(metadata("enabled", json!([]))?).map_err(|e| e.to_string())?;
        hub.provenance = serde_json::from_value(metadata("provenance", json!({}))?)
            .map_err(|e| e.to_string())?;
        hub.backup_receipts = serde_json::from_value(metadata("backup_receipts", json!({}))?)
            .map_err(|e| e.to_string())?;
        hub.installations = serde_json::from_value(metadata("installations", json!({}))?)
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
        hub.health =
            stream_dock_manager_storage::list_json(connection, Domain::PluginHealth, 4096, LIMIT)?
                .into_iter()
                .collect();
        // Older stores recorded only true IDs. An absent ID could mean an
        // explicit false choice, so preserve the effective value for every
        // known plugin rather than silently changing existing background work.
        // New stores record bool overrides; an unconfigured plugin defaults true
        let sleep = metadata("sleep_when_hidden", json!([]))?;
        hub.sleep_when_hidden = if sleep.is_array() {
            let enabled: std::collections::HashSet<String> =
                serde_json::from_value(sleep).map_err(|e| e.to_string())?;
            if enabled.len() > 4096 || enabled.iter().any(|id| !safe_id(id)) {
                return Err("Invalid plugin sleep preferences".into());
            }
            let mut overrides = std::collections::HashMap::new();
            for id in hub
                .enabled
                .iter()
                .chain(hub.installations.keys())
                .chain(hub.globals.keys())
                .chain(hub.action_defaults.keys())
                .chain(hub.retained.keys())
                .chain(hub.provenance.keys())
                .chain(hub.appearance.keys())
                .chain(hub.health.keys())
                .chain(enabled.iter())
            {
                overrides.insert(id.clone(), enabled.contains(id));
            }
            overrides
        } else {
            serde_json::from_value(sleep).map_err(|e| e.to_string())?
        };
        if hub.sleep_when_hidden.len() > 4096 || hub.sleep_when_hidden.keys().any(|id| !safe_id(id))
        {
            return Err("Invalid plugin sleep preferences".into());
        }
        for health in hub.health.values_mut() {
            health.session = Default::default();
            health.events.truncate(100);
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
            .strip_prefix("storage-generation:")
            .or_else(|| key.strip_prefix("storage-updated:"))
            .or_else(|| key.strip_prefix("storage-removed:"))
        {
            if !safe_id(id) || value.as_u64().is_none() {
                return Err("Invalid durable plugin generation".into());
            }
        } else if ![
            "enabled",
            "sleep_when_hidden",
            "retained",
            "provenance",
            "backup_receipts",
            "installations",
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
