//! Personal encrypted row store isolated from Manager backups and recovery
mod pages;
pub mod password;
pub mod rows;
pub mod transfer;
pub mod transition;

#[cfg(test)]
#[path = "vault/rows_tests.rs"]
mod rows_tests;

use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use rusqlite::{params, Connection, OpenFlags};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub const FILE_NAME: &str = "keystore.sqlite3";
const KEY_PROOF: &[u8] = b"app.streamdockmanager.vault/key-proof/v1";
const MAX_PLAINTEXT: usize = 4 * 1024 * 1024;
const FORMAT: i64 = 1;
const PASSWORD_FORMAT: i64 = 2;

pub struct EncryptedVault {
    page_header: PathBuf,
    db: Connection,
    id: uuid::Uuid,
    proof: hmac::Tag,
}

// Produce a fresh data encryption key without retaining a plaintext disk copy
pub fn data_key() -> Result<Zeroizing<[u8; 32]>, String> {
    let mut key = Zeroizing::new([0; 32]);
    SystemRandom::new()
        .fill(key.as_mut())
        .map_err(|_| "vault_unavailable")?;
    Ok(key)
}

// Keep encrypted database assets separate from the recovery markers at the data root
pub fn database_dir(root: &Path) -> Result<PathBuf, String> {
    crate::database_layout::ensure_directory(root).map_err(|_| RECOVERY.into())
}

pub fn database_file(root: &Path) -> Result<PathBuf, String> {
    Ok(database_dir(root)?.join(FILE_NAME))
}

// Refuse links and foreign SQLite sidecars before opening the owned keystore
pub fn checked_path(root: &Path) -> Result<Option<PathBuf>, String> {
    let directory = database_dir(root)?;
    let mut found = None;
    for name in [
        FILE_NAME,
        "keystore.sqlite3-journal",
        "keystore.sqlite3-wal",
        "keystore.sqlite3-shm",
    ] {
        if owned_regular(&root.join(name))? {
            return Err(RECOVERY.into());
        }
        let path = directory.join(name);
        if !owned_regular(&path)? {
            continue;
        }
        if name == FILE_NAME {
            found = Some(path);
        } else if found.is_none() || name.ends_with("-wal") || name.ends_with("-shm") {
            return Err("vault_recovery_required".into());
        }
    }
    Ok(found)
}

// Configure SQLite to keep temporary state off disk and use ciphertext-only rollback
fn configure(db: &Connection) -> Result<(), String> {
    db.execute_batch(
        "PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY; PRAGMA synchronous=FULL;",
    )
    .map_err(|_| "vault_recovery_required".into())
}

// Read the opaque identity without decrypting metadata or modifying the database
pub fn identity(path: &Path) -> Result<uuid::Uuid, String> {
    Ok(header(path)?.0)
}

fn header(path: &Path) -> Result<(uuid::Uuid, i64), String> {
    if let Some((_, h)) = pages::lookup(path)? {
        return Ok((h.id, if h.password { PASSWORD_FORMAT } else { FORMAT }));
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "vault_recovery_required")?;
    configure(&db)?;
    EncryptedVault::read_header(&db)
}

impl EncryptedVault {
    // Reject unsupported versions and ambiguous identities
    fn read_header(db: &Connection) -> Result<(uuid::Uuid, i64), String> {
        let (format, id): (i64, String) = db
            .query_row("SELECT format,id FROM vault WHERE singleton=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(|_| "vault_recovery_required")?;
        let id = uuid::Uuid::parse_str(&id).map_err(|_| "vault_recovery_required")?;
        if !matches!(format, FORMAT | PASSWORD_FORMAT) {
            return Err("vault_recovery_required".into());
        }
        Ok((id, format))
    }

    // Open the authenticated row format without exposing legacy document contents
    pub fn open(path: &Path, id: uuid::Uuid, key: &[u8; 32]) -> Result<Self, String> {
        let Some((page_header, header)) = pages::lookup(path)? else {
            // An old SQLite header is recognized using the old opaque identity only
            let (legacy_id, _) = self::header(path)?;
            return Err(if legacy_id == id {
                UNSUPPORTED
            } else {
                RECOVERY
            }
            .into());
        };
        if header.id != id {
            return Err(RECOVERY.into());
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| RECOVERY)?;
        pages::key_database(&db, key)?;
        configure(&db)?;
        rows::validate(&db, id)?;
        let mode: String = db
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(|_| RECOVERY)?;
        if mode != "delete" {
            return Err(RECOVERY.into());
        }
        let proof = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), KEY_PROOF);
        Ok(Self {
            page_header,
            db,
            id,
            proof,
        })
    }

    // Write an owned, authenticated encrypted database before publication
    pub fn stage_rows(
        root: &Path,
        id: uuid::Uuid,
        key: &[u8; 32],
        build: impl FnOnce(&rows::Rows<'_>) -> Result<(), String>,
    ) -> Result<tempfile::NamedTempFile, String> {
        let file = tempfile::Builder::new()
            .prefix(".keystore-stage-")
            .tempfile_in(root)
            .map_err(|_| "vault_unavailable")?;
        Self::initialize_rows(file.path(), id, key, build)?;
        file.as_file().sync_all().map_err(|_| "vault_unavailable")?;
        Ok(file)
    }

    // The staging file is created exclusively before this function is called
    fn initialize_rows(
        path: &Path,
        id: uuid::Uuid,
        key: &[u8; 32],
        build: impl FnOnce(&rows::Rows<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        if id.is_nil() {
            return Err(RECOVERY.into());
        }
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| RECOVERY)?;
        pages::key_database(&db, key)?;
        configure(&db)?;
        db.execute_batch("PRAGMA journal_mode=DELETE;")
            .map_err(|_| RECOVERY)?;
        rows::initialize(&db, id)?;
        Self::transaction(&db, build)?;
        db.close().map_err(|_| RECOVERY)?;
        pages::Header::create(path, id)
    }

    /// Prove a freshly unwrapped credential matches the live key without storing that key
    pub fn matches_data_key(&self, candidate: &[u8; 32]) -> Result<bool, String> {
        Ok(hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, candidate),
            KEY_PROOF,
            self.proof.as_ref(),
        )
        .is_ok())
    }

    /// Run a consistent row snapshot or atomically commit a batch
    pub fn rows<T>(
        &self,
        op: impl FnOnce(&rows::Rows<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        Self::transaction(&self.db, op)
    }

    fn transaction<T>(
        db: &Connection,
        op: impl FnOnce(&rows::Rows<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let tx = db.unchecked_transaction().map_err(|_| "vault_failure")?;
        let value = op(&rows::Rows::new(&tx))?;
        tx.commit().map_err(|_| "vault_failure")?;
        Ok(value)
    }
}

const RESET_INTENT: &str = ".keystore-reset-v1.json";
const RECOVERY: &str = "vault_recovery_required";
const UNSUPPORTED: &str = "vault_unsupported_format";

#[derive(Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetIntent {
    version: u8,
    pub previous: uuid::Uuid,
    #[serde(default)]
    pub previous_password: bool,
    pub next: uuid::Uuid,
}

pub(crate) fn owned_regular(path: &Path) -> Result<bool, String> {
    let meta = match path.symlink_metadata() {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(RECOVERY.into()),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(RECOVERY.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(RECOVERY.into());
        }
    }
    Ok(true)
}

pub fn reset_pending(root: &Path) -> Result<bool, String> {
    owned_regular(&root.join(RESET_INTENT))
}

/// Keep ordinary database and profile transfers away from live Vault files and recovery state.
pub fn check_transfer_path(root: &Path, candidate: &Path) -> Result<(), String> {
    if let Some(file) = std::env::var_os(crate::installation_key::KEY_FILE) {
        if let (Ok(key), Ok(target)) = (Path::new(&file).canonicalize(), candidate.canonicalize()) {
            if key == target {
                return Err(
                    "Choose a transfer destination outside the installation key file".into(),
                );
            }
        }
    }
    let Some(name) = candidate.file_name().and_then(|n| n.to_str()) else {
        return Ok(());
    };
    let name = name.to_ascii_lowercase();
    if !(name == ".stream-dock-manager-database-relocation-v1.json"
        || name == crate::DATABASE_NAME
        || name.starts_with("manager.sqlite3-")
        || name == "cache.sqlite3"
        || name.starts_with("cache.sqlite3-")
        || name == crate::installation_key::NAME
        || name.starts_with("datastore.sqlite3-")
        || name == FILE_NAME
        || name.starts_with("keystore.sqlite3-")
        || name.starts_with(".keystore-reset-")
        || name.starts_with(".keystore-key-")
        || name.starts_with(".keystore-envelope-stage-")
        || name.starts_with(".keystore-switch-")
        || name.starts_with(".keystore-stage-"))
    {
        return Ok(());
    }
    let parent = candidate
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|_| "Invalid transfer directory")?;
    let root = root
        .canonicalize()
        .map_err(|_| "Invalid application directory")?;
    let directory = crate::database_directory(&root);
    if parent == root
        || parent == directory
        || directory.canonicalize().is_ok_and(|path| parent == path)
    {
        return Err(
            "Choose a transfer path outside the application's database and Vault files".into(),
        );
    }
    Ok(())
}

impl ResetIntent {
    // Only a confirmed reset may create or resume this journal.
    pub fn begin(root: &Path) -> Result<Self, String> {
        use std::io::{Read, Write};
        if !root
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            return Err(RECOVERY.into());
        }
        let marker = root.join(RESET_INTENT);
        if reset_pending(root)? {
            let mut bytes = Vec::new();
            std::fs::File::open(&marker)
                .and_then(|f| f.take(513).read_to_end(&mut bytes))
                .map_err(|_| RECOVERY)?;
            let intent: Self = serde_json::from_slice(&bytes).map_err(|_| RECOVERY)?;
            if bytes.len() > 512
                || intent.version != 1
                || intent.previous == intent.next
                || intent.previous.is_nil()
                || intent.next.is_nil()
            {
                return Err(RECOVERY.into());
            }
            return Ok(intent);
        }
        let old = checked_path(root)?.ok_or("vault_not_initialized")?;
        owned_regular(&old)?;
        let (previous, format) = header(&old)?;
        let intent = Self {
            version: 1,
            previous,
            previous_password: format == PASSWORD_FORMAT,
            next: uuid::Uuid::new_v4(),
        };
        let mut file = tempfile::NamedTempFile::new_in(root).map_err(|_| RECOVERY)?;
        file.write_all(&serde_json::to_vec(&intent).map_err(|_| RECOVERY)?)
            .map_err(|_| RECOVERY)?;
        file.as_file().sync_all().map_err(|_| RECOVERY)?;
        file.persist_noclobber(&marker).map_err(|_| RECOVERY)?;
        crate::sync_directory(root)?;
        Ok(intent)
    }

    pub fn live_identity(&self, root: &Path) -> Result<uuid::Uuid, String> {
        let path = checked_path(root)?.ok_or(RECOVERY)?;
        owned_regular(&path)?;
        let (actual, format) = header(&path)?;
        if actual == self.previous {
            if (format == PASSWORD_FORMAT) != self.previous_password {
                return Err(RECOVERY.into());
            }
        } else if actual != self.next {
            return Err(RECOVERY.into());
        }
        Ok(actual)
    }

    fn stage_path(&self, directory: &Path) -> PathBuf {
        directory.join(format!(".keystore-reset-{}.sqlite3", self.next))
    }

    // The journal owns exactly one staging name. A previous partial stage is disposable.
    pub fn stage_empty_rows(&self, root: &Path, key: &[u8; 32]) -> Result<(), String> {
        let directory = database_dir(root)?;
        let path = self.stage_path(&directory);
        for suffix in ["-journal", "-wal", "-shm", ""] {
            let candidate =
                path.with_file_name(format!(".keystore-reset-{}.sqlite3{suffix}", self.next));
            if owned_regular(&candidate)? {
                std::fs::remove_file(candidate).map_err(|_| RECOVERY)?;
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|_| RECOVERY)?;
        EncryptedVault::initialize_rows(&path, self.next, key, |_| Ok(()))?;
        EncryptedVault::open(&path, self.next, key)?;
        file.sync_all().map_err(|_| RECOVERY)?;
        crate::sync_directory(&directory)
    }

    pub fn publish(&self, root: &Path) -> Result<(), String> {
        if self.live_identity(root)? != self.previous {
            return Err(RECOVERY.into());
        }
        let directory = database_dir(root)?;
        let stage = self.stage_path(&directory);
        owned_regular(&stage)?;
        if identity(&stage)? != self.next {
            return Err(RECOVERY.into());
        }
        // A surviving old rollback journal must never be applied to the new generation.
        let journal = directory.join("keystore.sqlite3-journal");
        if owned_regular(&journal)? {
            std::fs::remove_file(journal).map_err(|_| RECOVERY)?;
            crate::sync_directory(&directory)?;
        }
        crate::replace_file(&stage, &directory.join(FILE_NAME))?;
        crate::sync_directory(&directory)
    }

    // Remove orphaned initial-creation stages only after verifying their Vault identity.
    pub fn finish(&self, root: &Path) -> Result<(), String> {
        if self.live_identity(root)? != self.next {
            return Err(RECOVERY.into());
        }
        let directory = database_dir(root)?;
        for item in std::fs::read_dir(&directory).map_err(|_| RECOVERY)? {
            let item = item.map_err(|_| RECOVERY)?;
            let name = item.file_name();
            if !name.to_string_lossy().starts_with(".keystore-stage-") {
                continue;
            }
            let path = item.path();
            if !owned_regular(&path)? {
                continue;
            }
            let id = identity(&path)?;
            if id != self.previous && id != self.next {
                return Err(RECOVERY.into());
            }
            std::fs::remove_file(path).map_err(|_| RECOVERY)?;
        }
        crate::sync_directory(&directory)?;
        pages::clean_retired(&directory, &[self.previous, self.next])?;
        std::fs::remove_file(root.join(RESET_INTENT)).map_err(|_| RECOVERY)?;
        crate::sync_directory(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn damaged_password_envelope_preserves_files_until_explicit_reset() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let path = database_file(root.path()).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE vault(singleton INTEGER PRIMARY KEY, format INTEGER, id TEXT, payload BLOB);
            CREATE TABLE vault_password(singleton INTEGER PRIMARY KEY, envelope BLOB);
            INSERT INTO vault_password VALUES(1, x'ff');").unwrap();
        db.execute("INSERT INTO vault VALUES(1,2,?1,x'ff')", [id.to_string()])
            .unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        assert!(password::envelope(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let reset = ResetIntent::begin(root.path()).unwrap();
        assert!(reset.previous_password);
        assert_eq!(reset.live_identity(root.path()).unwrap(), id);
    }

    #[test]
    fn persistence_and_wrong_key() {
        use rows::RowKind;
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let key = data_key().unwrap();
        let original = json!({"label":"fixture-private-label","value":"fixture-private-value"});
        let file = EncryptedVault::stage_rows(root.path(), id, &key, |tx| {
            tx.put(RowKind::Entry, "fixture-entry", "", &original)
        })
        .unwrap();
        let path = root.path().join(FILE_NAME);
        file.persist_noclobber(&path).unwrap();
        assert_eq!(identity(&path).unwrap(), id);
        let db = EncryptedVault::open(&path, id, &key).unwrap();
        assert_eq!(
            db.rows(|tx| tx.get::<serde_json::Value>(RowKind::Entry, "fixture-entry"))
                .unwrap(),
            Some(original)
        );
        db.rows(|tx| {
            tx.put(
                RowKind::Grant,
                "fixture-grant",
                "",
                &json!("fixture-private-grant"),
            )
        })
        .unwrap();
        drop(db);
        let bytes = std::fs::read(&path).unwrap();
        for needle in [
            "fixture-private-label",
            "fixture-private-value",
            "fixture-private-grant",
        ] {
            assert!(!bytes.windows(needle.len()).any(|b| b == needle.as_bytes()));
        }
        assert!(EncryptedVault::open(&path, id, &data_key().unwrap()).is_err());
    }
}
