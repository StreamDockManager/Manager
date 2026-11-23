//! A full transfer authenticates the normal redacted SQLite backup together with
//! its confidential records. Restore rekeys those records for the destination.
use crate::{maintenance, plugin_settings, records, rusqlite::Connection, Domain, Storage};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const LIMIT: usize = 256 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    table: String,
    key: String,
    document: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum TransferRow {
    Setting(Record),
    OwnerGeneration(String, i64),
    Namespace(String, String, String, [u8; 8]),
    Entry(String, String, String, String, i64),
}

const SDK_TABLES: [(&str, &str); 4] = [
    ("plugin_storage_generations", "?1,?2"),
    ("plugin_storage_namespaces", "?1,?2,?3,?4"),
    ("plugin_storage_records", "?1,?2,?3,?4,?5"),
    ("plugin_storage_receipts", "?1,?2,?3,?4,?5,?6,?7"),
];

pub(crate) fn copy_sdk_for_preview(db: &Connection, source: &Connection) -> Result<(), String> {
    let epoch: String = source
        .query_row(
            "SELECT epoch FROM settings_vault.plugin_storage_epoch WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    db.execute(
        "UPDATE settings_vault.plugin_storage_epoch SET epoch=?1 WHERE singleton=1",
        [&epoch],
    )
    .map_err(|e| e.to_string())?;
    for (table, parameters) in SDK_TABLES {
        let mut select = source
            .prepare(&format!("SELECT * FROM settings_vault.{table}"))
            .map_err(|e| e.to_string())?;
        let columns = select.column_count();
        let mut input = select.query([]).map_err(|e| e.to_string())?;
        let mut insert = db
            .prepare(&format!(
                "INSERT INTO settings_vault.{table} VALUES({parameters})"
            ))
            .map_err(|e| e.to_string())?;
        while let Some(row) = input.next().map_err(|e| e.to_string())? {
            let values = (0..columns)
                .map(|index| row.get::<_, rusqlite::types::Value>(index))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            insert
                .execute(rusqlite::params_from_iter(values))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn export(db: &Connection, output: &mut Vec<u8>) -> Result<(), String> {
    use serde::ser::{SerializeSeq, Serializer};
    struct Bounded<'a>(&'a mut Vec<u8>);
    impl Write for Bounded<'_> {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            if input.len() > LIMIT.saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("Settings transfer exceeds 256 MiB"));
            }
            self.0.extend_from_slice(input);
            Ok(input.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut serializer = serde_json::Serializer::new(Bounded(output));
    let mut seq = serializer.serialize_seq(None).map_err(|e| e.to_string())?;
    for domain in plugin_settings::domains()
        .into_iter()
        .chain([Domain::LocalPreferences])
    {
        let mut q = db
            .prepare("SELECT DISTINCT record_key FROM plugin_setting_refs WHERE domain=?1 ORDER BY record_key")
            .map_err(|e| e.to_string())?;
        let mut rows = q.query([domain.table()]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let key: String = row.get(0).map_err(|e| e.to_string())?;
            let document = records::read_text(db, domain, &key, crate::MAX_RECORD_BYTES)?
                .ok_or("Missing backup record")?;
            seq.serialize_element(&TransferRow::Setting(Record {
                table: domain.table().into(),
                key,
                document: serde_json::from_str(&document).map_err(|e| e.to_string())?,
            }))
            .map_err(|e| e.to_string())?;
        }
    }
    for (index, (table, _)) in SDK_TABLES.into_iter().take(3).enumerate() {
        let ordering = [
            "plugin_id",
            "plugin_id,namespace",
            "plugin_id,namespace,key",
        ][index];
        let mut query = db
            .prepare(&format!(
                "SELECT * FROM settings_vault.{table} ORDER BY {ordering}"
            ))
            .map_err(|e| e.to_string())?;
        let mut rows = query.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let decode = || -> rusqlite::Result<TransferRow> {
                let plugin = row.get(0)?;
                if index == 0 {
                    return Ok(TransferRow::OwnerGeneration(plugin, row.get(1)?));
                }
                let namespace = row.get(1)?;
                if index == 1 {
                    return Ok(TransferRow::Namespace(
                        plugin,
                        namespace,
                        row.get(2)?,
                        row.get(3)?,
                    ));
                }
                Ok(TransferRow::Entry(
                    plugin,
                    namespace,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            };
            let row = decode().map_err(|e| e.to_string())?;
            seq.serialize_element(&row).map_err(|e| e.to_string())?;
        }
    }
    seq.end().map_err(|e| e.to_string())
}
impl Storage {
    pub fn backup_with_settings(&self, destination: &Path, password: &str) -> Result<(), String> {
        let root = self.root().to_owned();
        crate::vault::check_transfer_path(&root, destination)?;
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        crate::installation_key::check_backup_directory(parent)?;
        if parent
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&root)
        {
            return Err(
                "Choose a full transfer destination outside Manager's data directory".into(),
            );
        }
        let validators = self.inner.validators.clone();
        let mut data = self.run(move |db| {
            let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
            let snapshot = maintenance::backup_connection(
                db,
                &root,
                &dir.path().join("transfer.sqlite3"),
                &validators,
            )?;
            if snapshot.bytes > LIMIT as u64 {
                return Err("Transfer exceeds 256 MiB".into());
            }
            let mut body = Zeroizing::new(Vec::from(snapshot.bytes.to_le_bytes()));
            let mut file = std::fs::File::open(snapshot.path).map_err(|e| e.to_string())?;
            std::io::copy(&mut file, &mut *body).map_err(|e| e.to_string())?;
            export(db, &mut body)?;
            Ok(body)
        })?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        crate::vault::transfer::write(
            crate::vault::transfer::Kind::Backup,
            file.as_file_mut(),
            &mut data,
            password,
        )?;
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(destination).map_err(|e| e.to_string())?;
        crate::sync_directory(parent)
    }

    pub fn recover_with_settings<F, G>(
        root: PathBuf,
        path: &Path,
        password: &str,
        validate: F,
        reconcile: G,
    ) -> Result<crate::RecoveryReport, String>
    where
        F: Fn(&Connection) -> Result<(), String>,
        G: FnOnce(&Connection, &[crate::RecoverySource]) -> Result<Vec<String>, String>,
    {
        let data = crate::vault::transfer::open(path, Some(password))?
            .ok_or("Choose a password-encrypted Manager backup")?;
        let length = data.get(..8).ok_or("Invalid transfer header")?;
        let split = usize::try_from(u64::from_le_bytes(length.try_into().unwrap()))
            .ok()
            .and_then(|n| 8usize.checked_add(n))
            .filter(|n| *n < data.len())
            .ok_or("Invalid backup size")?;
        let rows: Vec<TransferRow> = serde_json::from_slice(&data[split..])
            .map_err(|_| "Invalid settings transfer records")?;
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        file.write_all(&data[8..split]).map_err(|e| e.to_string())?;
        drop(data);
        let (root, _owner) = crate::own_root(&root)?;
        crate::database_layout::prepare_owned(&root)?;
        let live = crate::database_path(&root, crate::DATABASE_NAME);
        if live.exists() {
            // Refuse replacement when the supported destination has lost its key.
            crate::schema::live_snapshot_copy(&root)?;
        }
        let apply = |db: &Connection| -> Result<(), String> {
            let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
            let mut seen = std::collections::BTreeSet::new();
            tx.execute_batch(
                "DELETE FROM settings_vault.plugin_storage_receipts;
                DELETE FROM settings_vault.plugin_storage_records;
                DELETE FROM settings_vault.plugin_storage_namespaces;
                DELETE FROM settings_vault.plugin_storage_generations;",
            )
            .map_err(|e| e.to_string())?;
            for r in &rows {
                use rusqlite::params;
                let r = match r {
                    TransferRow::OwnerGeneration(owner, generation) => {
                        tx.execute(
                            "INSERT INTO settings_vault.plugin_storage_generations VALUES(?1,?2)",
                            params![owner, generation],
                        )
                        .map_err(|e| e.to_string())?;
                        continue;
                    }
                    TransferRow::Namespace(owner, namespace, generation, revision) => {
                        tx.execute("INSERT INTO settings_vault.plugin_storage_namespaces VALUES(?1,?2,?3,?4)", params![owner, namespace, generation, &revision[..]])
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                    TransferRow::Entry(owner, namespace, key, value, bytes) => {
                        tx.execute("INSERT INTO settings_vault.plugin_storage_records VALUES(?1,?2,?3,?4,?5)", params![owner, namespace, key, value, bytes])
                            .map_err(|e| e.to_string())?;
                        continue;
                    }
                    TransferRow::Setting(record) => record,
                };
                let domain = Domain::ALL
                    .into_iter()
                    .find(|d| d.table() == r.table && plugin_settings::supports(*d, &r.key))
                    .ok_or("Invalid settings transfer domain")?;
                if !seen.insert((&r.table, &r.key))
                    || crate::revision(&tx, domain, &r.key)?.is_none()
                {
                    return Err("Duplicate or missing settings transfer target".into());
                }
                crate::write_json(&tx, domain, &r.key, &r.document, crate::MAX_RECORD_BYTES)?;
            }
            crate::installation_key::rotate_storage_epoch(&tx)?;
            validate(&tx)?;
            tx.commit().map_err(|e| e.to_string())
        };
        // Authenticate and validate all domain values on a disposable copy first.
        let preview = crate::schema::snapshot_copy(file.path())?;
        crate::installation_key::attach_empty(&preview)?;
        apply(&preview)?;
        drop(preview);
        crate::recovery::recover_locked_inner(
            &root,
            file.path().into(),
            None,
            &validate,
            true,
            |db, sources| {
                let staged = crate::installation_key::stage_transfer(db, &root)?;
                apply(db)?;
                Ok((reconcile(db, sources)?, Some(staged)))
            },
        )
    }
}
