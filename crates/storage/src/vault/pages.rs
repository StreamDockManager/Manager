//! Page encryption uses SQLCipher. Its public random file salt locates a small
//! key envelope. Replacing the database selects the matching envelope atomically.
use super::*;
use ring::hmac;
use std::io::{Read, Write};

const PREFIX: &str = ".keystore-key-";
const SQLITE: &[u8; 16] = b"SQLite format 3\0";

#[derive(Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Header {
    version: u8,
    pub id: uuid::Uuid,
    pub password: bool,
    pub wrapped: Option<String>,
}
fn locator(path: &Path) -> Result<Option<PathBuf>, String> {
    if !owned_regular(path)? {
        return Err(RECOVERY.into());
    }
    let mut signature = [0; 16];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut signature))
        .map_err(|_| RECOVERY)?;
    Ok((&signature != SQLITE)
        .then(|| path.with_file_name(format!("{PREFIX}{}.json", hex::encode(signature)))))
}
fn load(path: &Path) -> Result<Header, String> {
    if !owned_regular(path)? {
        return Err(RECOVERY.into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(1025).read_to_end(&mut bytes))
        .map_err(|_| RECOVERY)?;
    let h: Header = serde_json::from_slice(&bytes).map_err(|_| RECOVERY)?;
    if h.version != 1 || bytes.len() > 1024 {
        return Err(RECOVERY.into());
    }
    Ok(h)
}
pub(super) fn lookup(path: &Path) -> Result<Option<(PathBuf, Header)>, String> {
    locator(path)?.map(|p| load(&p).map(|h| (p, h))).transpose()
}
impl Header {
    pub fn write(&self, path: &Path, creating: bool) -> Result<(), String> {
        let root = path.parent().ok_or(RECOVERY)?;
        let mut file = tempfile::Builder::new()
            .prefix(".keystore-envelope-stage-")
            .tempfile_in(root)
            .map_err(|_| RECOVERY)?;
        serde_json::to_writer(file.as_file_mut(), self).map_err(|_| RECOVERY)?;
        file.flush()
            .and_then(|_| file.as_file().sync_all())
            .map_err(|_| RECOVERY)?;
        if creating {
            file.persist_noclobber(path).map_err(|_| RECOVERY)?;
        } else {
            if !owned_regular(path)? {
                return Err(RECOVERY.into());
            }
            file.persist(path).map_err(|_| RECOVERY)?;
        }
        crate::sync_directory(root)
    }
    pub fn create(database: &Path, id: uuid::Uuid) -> Result<(), String> {
        let path = locator(database)?.ok_or(RECOVERY)?;
        Self {
            version: 1,
            id,
            password: false,
            wrapped: None,
        }
        .write(&path, true)
    }
    pub fn replace_password(
        path: &Path,
        id: uuid::Uuid,
        envelope: &password::PasswordEnvelope,
    ) -> Result<(), String> {
        let mut h = load(path)?;
        if h.id != id {
            return Err(RECOVERY.into());
        }
        h.password = true;
        h.wrapped = Some(envelope.encoded());
        h.write(path, false)
    }
    pub fn password(&self) -> Result<Option<password::PasswordEnvelope>, String> {
        match (self.password, &self.wrapped) {
            (false, None) => Ok(None),
            (true, Some(s)) => password::PasswordEnvelope::decode(s).map(Some),
            _ => Err(RECOVERY.into()),
        }
    }
}

/// Derive an independent SQLCipher key from the randomly generated Vault DEK.
/// Supply the raw key in memory and verify the codec before creating any table.
pub(super) fn key_database(db: &Connection, key: &[u8; 32]) -> Result<(), String> {
    let derived = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, key),
        b"app.streamdockmanager.vault/sqlcipher-pages/v1",
    );
    let mut encoded = Zeroizing::new([0u8; 64]);
    hex::encode_to_slice(derived.as_ref(), encoded.as_mut_slice()).map_err(|_| RECOVERY)?;
    let raw = std::str::from_utf8(encoded.as_slice()).map_err(|_| RECOVERY)?;
    let sql = Zeroizing::new(format!("PRAGMA key=\"x'{raw}'\";"));
    db.execute_batch(&sql).map_err(|_| RECOVERY)?;
    let version: String = db
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .map_err(|_| RECOVERY)?;
    if !version.starts_with("4.") {
        return Err(RECOVERY.into());
    }
    db.execute_batch("PRAGMA cipher_memory_security=ON; PRAGMA cipher_plaintext_header_size=0;")
        .map_err(|_| RECOVERY.into())
}

pub(super) fn clean_retired(root: &Path, ids: &[uuid::Uuid]) -> Result<(), String> {
    let live = locator(&root.join(FILE_NAME))?;
    for f in std::fs::read_dir(root).map_err(|_| RECOVERY)? {
        let f = f.map_err(|_| RECOVERY)?;
        let name = f.file_name();
        let Some(name) = name.to_str() else { continue };
        let listed = name
            .strip_prefix(PREFIX)
            .and_then(|s| s.strip_suffix(".json"))
            .is_some_and(|s| s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()));
        if listed
            && !live.as_ref().is_some_and(|path| path == &f.path())
            && ids.contains(&load(&f.path())?.id)
        {
            std::fs::remove_file(f.path()).map_err(|_| RECOVERY)?;
        }
    }
    crate::sync_directory(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_store_encrypts_schema_and_rollback_pages() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(FILE_NAME);
        let key = data_key().unwrap();
        let id = uuid::Uuid::new_v4();
        let sample = "synthetic protected document";
        EncryptedVault::stage_rows(root.path(), id, &key, |rows| {
            rows.put(rows::RowKind::EntryValue, "fixture", "", &sample)
        })
        .unwrap()
        .persist_noclobber(&path)
        .unwrap();
        let vault = EncryptedVault::open(&path, id, &key).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_ne!(&bytes[..16], SQLITE);
        assert!(
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap()
                .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r
                    .get::<_, i64>(0))
                .is_err()
        );
        let marker = "probe_for_plaintext_in_database_and_rollback_journal";
        vault.db.execute_batch("PRAGMA cache_size=1; CREATE TABLE probe_for_plaintext_in_database_and_rollback_journal(value BLOB); INSERT INTO probe_for_plaintext_in_database_and_rollback_journal VALUES(zeroblob(65536)); BEGIN IMMEDIATE; DROP TABLE probe_for_plaintext_in_database_and_rollback_journal;").unwrap();
        for name in [FILE_NAME, "keystore.sqlite3-journal"] {
            let data = std::fs::read(root.path().join(name)).unwrap();
            for needle in [marker.as_bytes(), sample.as_bytes(), b"CREATE TABLE vault"] {
                assert!(!data.windows(needle.len()).any(|s| s == needle), "{name}");
            }
        }
        assert_eq!(
            vault
                .db
                .query_row("PRAGMA temp_store", [], |r| r.get::<_, u8>(0))
                .unwrap(),
            2
        );
        assert!(!root.path().join("keystore.sqlite3-wal").exists());
        vault.db.execute_batch("ROLLBACK").unwrap();
        vault
            .db
            .execute_batch("DROP TABLE probe_for_plaintext_in_database_and_rollback_journal;")
            .unwrap();
        drop(vault);
        let header = locator(&path).unwrap().unwrap();
        assert!(check_transfer_path(root.path(), &header).is_err());
        assert_eq!(
            EncryptedVault::open(&path, id, &key)
                .unwrap()
                .rows(|rows| rows.get::<String>(rows::RowKind::EntryValue, "fixture"))
                .unwrap(),
            Some(sample.into()),
        );
    }
}
