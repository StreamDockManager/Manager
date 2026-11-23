//! Owned, bounded row store. SQLCipher encrypts the table and its indexes and journal.
use super::*;
use rusqlite::OptionalExtension;

const VERSION: i64 = 1;
const APPLICATION_ID: i64 = 0x53445652;
const MAX_ROW: usize = 128 * 1024;
const MAX_IDENTIFIER: usize = 4096;
const MAX_ROWS: i64 = 8192;
const LIMIT: &str = "vault_limit";

const SCHEMA: &str = "
CREATE TABLE vault_rows_meta(
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    identity TEXT NOT NULL,
    revision BLOB NOT NULL CHECK(length(revision)=8),
    used_bytes INTEGER NOT NULL CHECK(used_bytes BETWEEN 0 AND 4194304),
    used_rows INTEGER NOT NULL CHECK(used_rows BETWEEN 0 AND 8192)
) STRICT;
CREATE TABLE vault_rows(
    kind INTEGER NOT NULL CHECK(kind BETWEEN 1 AND 5),
    key TEXT NOT NULL CHECK(length(CAST(key AS BLOB)) BETWEEN 1 AND 4096),
    scope TEXT NOT NULL CHECK(length(CAST(scope AS BLOB)) <= 4096),
    value BLOB NOT NULL CHECK(length(value) BETWEEN 1 AND 131072),
    PRIMARY KEY(kind,key)
) STRICT, WITHOUT ROWID;
CREATE INDEX vault_rows_scope ON vault_rows(kind,scope,key);
CREATE TRIGGER vault_rows_admit_insert AFTER INSERT ON vault_rows BEGIN
    UPDATE vault_rows_meta SET used_bytes=used_bytes+length(CAST(new.key AS BLOB))+length(CAST(new.scope AS BLOB))+length(new.value),
        used_rows=used_rows+1 WHERE singleton=1
        AND used_rows < 8192
        AND used_bytes+length(CAST(new.key AS BLOB))+length(CAST(new.scope AS BLOB))+length(new.value) <= 4194304;
    SELECT CASE WHEN changes() <> 1 THEN RAISE(ABORT,'vault_limit') END;
END;
CREATE TRIGGER vault_rows_admit_update AFTER UPDATE ON vault_rows BEGIN
    UPDATE vault_rows_meta SET used_bytes=used_bytes
        +length(CAST(new.key AS BLOB))+length(CAST(new.scope AS BLOB))+length(new.value)
        -length(CAST(old.key AS BLOB))-length(CAST(old.scope AS BLOB))-length(old.value)
        WHERE singleton=1 AND used_bytes
        +length(CAST(new.key AS BLOB))+length(CAST(new.scope AS BLOB))+length(new.value)
        -length(CAST(old.key AS BLOB))-length(CAST(old.scope AS BLOB))-length(old.value) <= 4194304;
    SELECT CASE WHEN changes() <> 1 THEN RAISE(ABORT,'vault_limit') END;
END;
CREATE TRIGGER vault_rows_account_delete AFTER DELETE ON vault_rows BEGIN
    UPDATE vault_rows_meta SET used_bytes=used_bytes-length(CAST(old.key AS BLOB))-length(CAST(old.scope AS BLOB))-length(old.value),
        used_rows=used_rows-1 WHERE singleton=1;
    SELECT CASE WHEN changes() <> 1 THEN RAISE(ABORT,'vault_recovery_required') END;
END;";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum RowKind {
    Entry = 1,
    EntryValue = 2,
    Grant = 3,
    Reference = 4,
    Writer = 5,
}

/// Access within one owned SQL transaction; no plaintext escapes into a second store
pub struct Rows<'a> {
    tx: &'a Connection,
}

impl<'a> Rows<'a> {
    pub(super) fn new(tx: &'a Connection) -> Self {
        Self { tx }
    }

    pub fn revision(&self) -> Result<u64, String> {
        self.tx
            .query_row(
                "SELECT revision FROM vault_rows_meta WHERE singleton=1",
                [],
                |row| row.get::<_, [u8; 8]>(0).map(u64::from_be_bytes),
            )
            .map_err(|_| RECOVERY.into())
    }

    pub fn set_revision(&self, revision: u64) -> Result<(), String> {
        let count = self
            .tx
            .execute(
                "UPDATE vault_rows_meta SET revision=?1 WHERE singleton=1",
                [revision.to_be_bytes()],
            )
            .map_err(|_| RECOVERY)?;
        if count != 1 {
            return Err(RECOVERY.into());
        }
        Ok(())
    }

    pub fn count(&self, kind: RowKind) -> Result<usize, String> {
        self.tx
            .query_row(
                "SELECT count(*) FROM vault_rows WHERE kind=?1",
                [kind as i64],
                |row| row.get(0),
            )
            .map_err(|_| RECOVERY.into())
    }

    pub fn contains(&self, kind: RowKind, key: &str) -> Result<bool, String> {
        check_identifier(key, false)?;
        self.tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM vault_rows WHERE kind=?1 AND key=?2)",
                params![kind as i64, key],
                |row| row.get(0),
            )
            .map_err(|_| RECOVERY.into())
    }

    pub fn get<T: DeserializeOwned>(&self, kind: RowKind, key: &str) -> Result<Option<T>, String> {
        check_identifier(key, false)?;
        let mut statement = self
            .tx
            .prepare("SELECT value FROM vault_rows WHERE kind=?1 AND key=?2")
            .map_err(|_| RECOVERY)?;
        let mut rows = statement
            .query(params![kind as i64, key])
            .map_err(|_| RECOVERY)?;
        rows.next()
            .map_err(|_| RECOVERY)?
            .map(|row| decode(row, 0))
            .transpose()
    }

    pub fn list<T: DeserializeOwned>(
        &self,
        kind: RowKind,
        scope: Option<&str>,
    ) -> Result<Vec<(String, T)>, String> {
        let sql = match scope {
            Some(scope) => {
                check_identifier(scope, true)?;
                "SELECT key,value FROM vault_rows WHERE kind=?1 AND scope=?2 ORDER BY key"
            }
            None => "SELECT key,value FROM vault_rows WHERE kind=?1 ORDER BY key",
        };
        let mut statement = self.tx.prepare(sql).map_err(|_| RECOVERY)?;
        let mut rows = match scope {
            Some(scope) => statement.query(params![kind as i64, scope]),
            None => statement.query([kind as i64]),
        }
        .map_err(|_| RECOVERY)?;
        let mut output = Vec::new();
        let mut bytes = 0;
        while let Some(row) = rows.next().map_err(|_| RECOVERY)? {
            if output.len() >= MAX_ROWS as usize {
                return Err(RECOVERY.into());
            }
            let key: String = row.get(0).map_err(|_| RECOVERY)?;
            check_identifier(&key, false).map_err(|_| RECOVERY)?;
            let payload = payload(row, 1)?;
            bytes += key.len() + payload.len();
            if bytes > MAX_PLAINTEXT {
                return Err(RECOVERY.into());
            }
            let value = serde_json::from_slice(payload).map_err(|_| RECOVERY)?;
            output.push((key, value));
        }
        Ok(output)
    }

    pub fn put<T: Serialize>(
        &self,
        kind: RowKind,
        key: &str,
        scope: &str,
        value: &T,
    ) -> Result<(), String> {
        check_identifier(key, false)?;
        check_identifier(scope, true)?;
        let value = encode(value)?;
        self.tx
            .execute(
                "INSERT INTO vault_rows(kind,key,scope,value) VALUES(?1,?2,?3,?4)
             ON CONFLICT(kind,key) DO UPDATE SET scope=excluded.scope,value=excluded.value",
                params![kind as i64, key, scope, value.as_slice()],
            )
            .map_err(write_error)?;
        Ok(())
    }

    pub fn delete(&self, kind: RowKind, key: &str) -> Result<(), String> {
        check_identifier(key, false)?;
        self.tx
            .execute(
                "DELETE FROM vault_rows WHERE kind=?1 AND key=?2",
                params![kind as i64, key],
            )
            .map_err(write_error)?;
        Ok(())
    }

    pub fn clear(&self, kind: RowKind) -> Result<(), String> {
        self.tx
            .execute("DELETE FROM vault_rows WHERE kind=?1", [kind as i64])
            .map_err(write_error)?;
        Ok(())
    }
}

fn write_error(error: rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(_, Some(message)) if message == LIMIT => LIMIT.into(),
        _ => RECOVERY.into(),
    }
}

fn check_identifier(value: &str, empty: bool) -> Result<(), String> {
    if value.len() > MAX_IDENTIFIER || (!empty && value.is_empty()) || value.contains('\0') {
        return Err(LIMIT.into());
    }
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<Zeroizing<Vec<u8>>, String> {
    use std::io::{self, Write};
    struct Buffer(Zeroizing<Vec<u8>>);
    impl Write for Buffer {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            if data.len() > MAX_ROW - self.0.len() {
                return Err(io::Error::other(LIMIT));
            }
            self.0.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer(Zeroizing::new(Vec::new()));
    serde_json::to_writer(&mut buffer, value).map_err(|err| {
        if err.is_io() {
            LIMIT
        } else {
            "vault_failure"
        }
    })?;
    Ok(buffer.0)
}

fn payload<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> Result<&'a [u8], String> {
    let bytes = row
        .get_ref(index)
        .map_err(|_| RECOVERY)?
        .as_blob()
        .map_err(|_| RECOVERY)?;
    if bytes.is_empty() || bytes.len() > MAX_ROW {
        return Err(RECOVERY.into());
    }
    Ok(bytes)
}

fn decode<T: DeserializeOwned>(row: &rusqlite::Row<'_>, index: usize) -> Result<T, String> {
    serde_json::from_slice(payload(row, index)?).map_err(|_| RECOVERY.into())
}

pub(super) fn initialize(db: &Connection, identity: uuid::Uuid) -> Result<(), String> {
    let tx = db.unchecked_transaction().map_err(|_| RECOVERY)?;
    tx.execute_batch(SCHEMA).map_err(|_| RECOVERY)?;
    tx.pragma_update(None, "application_id", APPLICATION_ID)
        .map_err(|_| RECOVERY)?;
    tx.pragma_update(None, "user_version", VERSION)
        .map_err(|_| RECOVERY)?;
    tx.execute(
        "INSERT INTO vault_rows_meta VALUES(1,?1,?2,0,0)",
        params![identity.to_string(), 0u64.to_be_bytes()],
    )
    .map_err(|_| RECOVERY)?;
    tx.commit().map_err(|_| RECOVERY.into())
}

/// Reject malformed and unsupported layouts before serving even a single row
pub(super) fn validate(db: &Connection, expected: uuid::Uuid) -> Result<(), String> {
    use std::sync::OnceLock;
    type Object = (String, String, String, String);
    fn objects(db: &Connection) -> Result<Vec<Object>, String> {
        let mut st = db.prepare("SELECT type,name,tbl_name,coalesce(sql,'') FROM sqlite_schema ORDER BY type,name LIMIT 8").map_err(|_| RECOVERY)?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|_| RECOVERY)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| RECOVERY.into())
    }
    let application_id: i64 = db
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|_| RECOVERY)?;
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|_| RECOVERY)?;
    if application_id != APPLICATION_ID || version != VERSION {
        let legacy = application_id == 0
            && version == 0
            && db
                .query_row(
                    "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='vault'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .map_err(|_| RECOVERY)?
                .is_some();
        return Err(if legacy || application_id == APPLICATION_ID {
            UNSUPPORTED
        } else {
            RECOVERY
        }
        .into());
    }
    static EXPECTED: OnceLock<Result<Vec<Object>, String>> = OnceLock::new();
    let schema = EXPECTED.get_or_init(|| {
        let schema = Connection::open_in_memory().map_err(|_| RECOVERY)?;
        schema.execute_batch(SCHEMA).map_err(|_| RECOVERY)?;
        objects(&schema)
    });
    if objects(db)? != *schema.as_ref().map_err(|_| RECOVERY)? {
        return Err(RECOVERY.into());
    }
    let (identity, _revision, used_bytes, used_rows): (String, [u8; 8], i64, i64) = db
        .query_row(
            "SELECT identity,revision,used_bytes,used_rows FROM vault_rows_meta WHERE singleton=1
         AND used_bytes BETWEEN 0 AND ?1 AND used_rows BETWEEN 0 AND ?2",
            params![MAX_PLAINTEXT, MAX_ROWS],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|_| RECOVERY)?;
    if identity != expected.to_string() || expected.is_nil() {
        return Err(RECOVERY.into());
    }
    let measured: (i64, i64) = db
        .query_row(
            "SELECT coalesce(sum(bytes),0),count(*) FROM
         (SELECT length(CAST(key AS BLOB))+length(CAST(scope AS BLOB))+length(value) AS bytes
          FROM vault_rows LIMIT ?1)",
            [MAX_ROWS + 1],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| RECOVERY)?;
    if measured != (used_bytes, used_rows) {
        return Err(RECOVERY.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_errors_are_statement_atomic_and_release_capacity_after_delete() {
        let directory = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let key = data_key().unwrap();
        let stage = EncryptedVault::stage_rows(directory.path(), id, &key, |_| Ok(())).unwrap();
        let vault = EncryptedVault::open(stage.path(), id, &key).unwrap();
        let large = "x".repeat(120 * 1024);
        let too_large = "y".repeat(MAX_ROW);
        vault
            .rows(|rows| {
                assert_eq!(
                    rows.put(RowKind::EntryValue, "too-large", "", &too_large)
                        .unwrap_err(),
                    LIMIT
                );
                for index in 0..34 {
                    rows.put(RowKind::EntryValue, &format!("row-{index}"), "", &large)?;
                }
                assert_eq!(
                    rows.put(RowKind::EntryValue, "overflow", "", &large)
                        .unwrap_err(),
                    LIMIT
                );
                assert_eq!(rows.get::<String>(RowKind::EntryValue, "overflow")?, None);
                assert!(!rows.contains(RowKind::EntryValue, "overflow")?);
                assert!(rows.contains(RowKind::EntryValue, "row-13")?);
                rows.put(RowKind::Entry, "tiny", "", &true)?;
                rows.delete(RowKind::EntryValue, "row-13")?;
                rows.put(RowKind::EntryValue, "replacement", "", &large)?;
                assert_eq!(rows.count(RowKind::EntryValue)?, 34);
                Ok(())
            })
            .unwrap();
        let reopened = EncryptedVault::open(stage.path(), id, &key).unwrap();
        reopened
            .rows(|rows| {
                assert_eq!(rows.count(RowKind::EntryValue)?, 34);
                rows.clear(RowKind::EntryValue)?;
                rows.put(RowKind::EntryValue, "after-clear", "", &large)?;
                assert_eq!(rows.count(RowKind::EntryValue)?, 1);
                assert_eq!(rows.get::<bool>(RowKind::Entry, "tiny")?, Some(true));
                Ok(())
            })
            .unwrap();
    }
}
