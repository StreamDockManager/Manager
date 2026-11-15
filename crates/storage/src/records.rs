//! Bounded JSON records in explicitly owned domain tables
use rusqlite::{Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};

pub const MAX_RECORD_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_KEY_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Workspace,
    Profiles,
    SavedMacros,
    AppearancePresets,
    ProfileRules,
    History,
    HistoryPlugins,
    LocalPreferences,
    ManagerPreferences,
    MediaTombstones,
    PluginGlobals,
    PluginDefaults,
    PluginMetadata,
    PluginHealth,
    LoggingPreferences,
    RuntimeState,
}

impl Domain {
    pub const ALL: [Self; 16] = [
        Self::Workspace,
        Self::Profiles,
        Self::SavedMacros,
        Self::AppearancePresets,
        Self::ProfileRules,
        Self::History,
        Self::HistoryPlugins,
        Self::LocalPreferences,
        Self::ManagerPreferences,
        Self::MediaTombstones,
        Self::PluginGlobals,
        Self::PluginDefaults,
        Self::PluginMetadata,
        Self::PluginHealth,
        Self::LoggingPreferences,
        Self::RuntimeState,
    ];

    // Resolve the database table for this record type
    pub const fn table(self) -> &'static str {
        match self {
            Self::Workspace => "workspace_state",
            Self::Profiles => "profiles",
            Self::SavedMacros => "saved_macros",
            Self::AppearancePresets => "appearance_presets",
            Self::ProfileRules => "profile_rules",
            Self::History => "history_entries",
            Self::HistoryPlugins => "history_plugin_sidecars",
            Self::LocalPreferences => "local_service_preferences",
            Self::ManagerPreferences => "manager_preferences",
            Self::MediaTombstones => "media_tombstones",
            Self::PluginGlobals => "plugin_globals",
            Self::PluginDefaults => "plugin_defaults",
            Self::PluginMetadata => "plugin_metadata",
            Self::PluginHealth => "plugin_health",
            Self::LoggingPreferences => "logging_preferences",
            Self::RuntimeState => "runtime_state",
        }
    }
}

// Check key
fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.contains('\0') {
        return Err("Storage record key is empty, invalid, or too long".into());
    }
    Ok(())
}

// Encode for the current operation
pub(crate) fn encode<T: Serialize + ?Sized>(value: &T, max_bytes: usize) -> Result<String, String> {
    // Stop serialization at the limit instead of first allocating an unbounded value
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        // Write for the current operation
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("Storage record exceeds size limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        // Flush for the current operation
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        limit: max_bytes.min(MAX_RECORD_BYTES),
    };
    serde_json::to_writer(&mut output, value)
        .map_err(|_| "Storage record could not be encoded within its size limit".to_string())?;
    String::from_utf8(output.bytes).map_err(|_| "Storage JSON was not UTF-8".into())
}

// Read text
pub(crate) fn read_text(
    conn: &Connection,
    domain: Domain,
    key: &str,
    max_bytes: usize,
) -> Result<Option<String>, String> {
    check_key(key)?;
    let sql = format!(
        "SELECT length(CAST(payload AS BLOB)), payload FROM {} WHERE key=?1",
        domain.table()
    );
    let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut rows = statement.query([key]).map_err(|e| e.to_string())?;
    let Some(row) = rows.next().map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let bytes: i64 = row.get(0).map_err(|e| e.to_string())?;
    if bytes < 0 || bytes as u64 > max_bytes.min(MAX_RECORD_BYTES) as u64 {
        return Err("Stored record exceeds size limit".into());
    }
    row.get(1).map(Some).map_err(|e| e.to_string())
}

// Read JSON
pub fn read_json<T: DeserializeOwned>(
    conn: &Connection,
    domain: Domain,
    key: &str,
    max_bytes: usize,
) -> Result<Option<T>, String> {
    read_text(conn, domain, key, max_bytes)?
        .map(|text| {
            serde_json::from_str(&text).map_err(|_| "Stored record has invalid domain data".into())
        })
        .transpose()
}

// Write text
pub(crate) fn write_text(
    conn: &Connection,
    domain: Domain,
    key: &str,
    payload: &str,
) -> Result<(), String> {
    check_key(key)?;
    if payload.len() > MAX_RECORD_BYTES {
        return Err("Storage record exceeds size limit".into());
    }
    let sql = format!("INSERT INTO {}(key,payload,revision) VALUES(?1,?2,1) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload,revision=revision+1", domain.table());
    conn.execute(&sql, (key, payload))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Validated, bounded JSON bytes prepared once before transaction admission
#[derive(Debug)]
pub struct EncodedJson(String);
impl EncodedJson {
    // Construct the value and initialize its state
    pub fn new(value: &(impl Serialize + ?Sized), limit: usize) -> Result<Self, String> {
        encode(value, limit).map(Self)
    }
    // Return the number of entries
    pub fn len(&self) -> usize {
        self.0.len()
    }
    // Check whether the value is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    // Write for the current operation
    pub fn write(&self, conn: &Connection, domain: Domain, key: &str) -> Result<(), String> {
        write_text(conn, domain, key, &self.0)
    }
    // Write changed
    pub fn write_changed(
        &self,
        conn: &Connection,
        domain: Domain,
        key: &str,
    ) -> Result<(), String> {
        check_key(key)?;
        let unchanged: Option<bool> = conn
            .query_row(
                &format!("SELECT payload=?2 FROM {} WHERE key=?1", domain.table()),
                (key, &self.0),
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if unchanged != Some(true) {
            self.write(conn, domain, key)?;
        }
        Ok(())
    }
}

// Write JSON
pub fn write_json<T: Serialize + ?Sized>(
    conn: &Connection,
    domain: Domain,
    key: &str,
    value: &T,
    max_bytes: usize,
) -> Result<(), String> {
    write_text(conn, domain, key, &encode(value, max_bytes)?)
}

// Delete for the current operation
pub fn delete(conn: &Connection, domain: Domain, key: &str) -> Result<(), String> {
    check_key(key)?;
    conn.execute(
        &format!("DELETE FROM {} WHERE key=?1", domain.table()),
        [key],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

// Return the current revision identifier
pub fn revision(conn: &Connection, domain: Domain, key: &str) -> Result<Option<u64>, String> {
    check_key(key)?;
    conn.query_row(
        &format!("SELECT revision FROM {} WHERE key=?1", domain.table()),
        [key],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

// Read the selected textual database records
pub(crate) fn list_text(
    conn: &Connection,
    domain: Domain,
    max_rows: usize,
    max_bytes: usize,
) -> Result<Vec<(String, String)>, String> {
    if max_rows > 1_000_000 {
        return Err("Storage row limit is too large".into());
    }
    let mut statement = conn.prepare(&format!("SELECT length(CAST(key AS BLOB)),length(CAST(payload AS BLOB)),key,payload FROM {} ORDER BY key LIMIT ?1",domain.table())).map_err(|e|e.to_string())?;
    let mut rows = statement
        .query([max_rows as i64 + 1])
        .map_err(|e| e.to_string())?;
    let mut result = Vec::new();
    let mut remaining = max_bytes.min(crate::MAX_ADMITTED_BYTES);
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        if result.len() == max_rows {
            return Err("Stored collection exceeds row limit".into());
        }
        let key_bytes: usize = row.get(0).map_err(|e| e.to_string())?;
        let value_bytes: usize = row.get(1).map_err(|e| e.to_string())?;
        let size = key_bytes
            .checked_add(value_bytes)
            .ok_or("Stored collection exceeds size limit")?;
        if size > remaining || value_bytes > MAX_RECORD_BYTES || key_bytes > MAX_KEY_BYTES {
            return Err("Stored collection exceeds size limit".into());
        }
        remaining -= size;
        result.push((
            row.get(2).map_err(|e| e.to_string())?,
            row.get(3).map_err(|e| e.to_string())?,
        ));
    }
    Ok(result)
}

// Read and decode the selected JSON records
pub fn list_json<T: DeserializeOwned>(
    conn: &Connection,
    domain: Domain,
    max_rows: usize,
    max_bytes: usize,
) -> Result<Vec<(String, T)>, String> {
    list_text(conn, domain, max_rows, max_bytes)?
        .into_iter()
        .map(|(key, text)| {
            serde_json::from_str(&text)
                .map(|value| (key, value))
                .map_err(|_| "Stored record has invalid domain data".into())
        })
        .collect()
}

/// A typed record's identity, bound and pure decoder, shared by service I/O and
/// snapshot validation. It does not own a transaction or construct runtime state.
pub struct JsonRecord<T> {
    pub domain: Domain,
    pub key: &'static str,
    pub max_bytes: usize,
    pub decode: fn(&str) -> Result<T, String>,
}
impl<T> JsonRecord<T> {
    // Read for the current operation
    pub fn read(&self, storage: &crate::Storage) -> Result<Option<T>, String> {
        let (domain, key, limit) = (self.domain, self.key, self.max_bytes);
        storage
            .run(move |connection| read_text(connection, domain, key, limit))?
            .map(|text| (self.decode)(&text))
            .transpose()
    }
    // Read from
    pub fn read_from(&self, connection: &Connection) -> Result<Option<T>, String> {
        read_text(connection, self.domain, self.key, self.max_bytes)?
            .map(|text| (self.decode)(&text))
            .transpose()
    }
    // Write to
    pub fn write_to(&self, connection: &Connection, value: &T) -> Result<(), String>
    where
        T: Serialize,
    {
        write_json(connection, self.domain, self.key, value, self.max_bytes)
    }
    // Write for the current operation
    pub fn write(&self, storage: &crate::Storage, value: &T) -> Result<(), String>
    where
        T: Serialize,
    {
        storage.write_json(self.domain, self.key, value, self.max_bytes)
    }
}

#[cfg(test)]
mod prepared_tests {
    use super::*;
    #[test]
    // Verify encoding is bounded and reused without reserialization
    fn encoding_is_bounded_and_reused_without_reserialization() {
        struct Counted(std::cell::Cell<usize>);
        impl Serialize for Counted {
            // Serialize the value for storage
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.set(self.0.get() + 1);
                serializer.serialize_str("é")
            }
        }
        let value = Counted(std::cell::Cell::new(0));
        let encoded = EncodedJson::new(&value, 4).unwrap();
        assert_eq!(value.0.get(), 1);
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE profiles(key TEXT PRIMARY KEY,payload TEXT,revision INTEGER)",
        )
        .unwrap();
        encoded
            .write_changed(&conn, Domain::Profiles, "id")
            .unwrap();
        encoded
            .write_changed(&conn, Domain::Profiles, "id")
            .unwrap();
        assert_eq!(value.0.get(), 1);
        assert_eq!(revision(&conn, Domain::Profiles, "id").unwrap(), Some(1));
        assert!(encoded.write(&conn, Domain::Profiles, "").is_err());
        assert!(EncodedJson::new(&value, 3).is_err());
    }
}
