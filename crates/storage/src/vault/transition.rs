//! Durable boundary for a change of unlock mode. The Vault UUID stays stable across DEK rotation.
use super::*;
use std::io::Read;
const MARKER: &str = ".keystore-switch-v1.json";

#[derive(Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub vault: uuid::Uuid,
    operation: uuid::Uuid,
    pub to_password: bool,
    // A verifier for the one OS wrapper owned by this operation. It never authorizes unlock.
    pub wrapper_sha256: String,
}

impl Transition {
    pub fn pending(root: &Path) -> Result<Option<Self>, String> {
        let marker = root.join(MARKER);
        if !owned_regular(&marker)? {
            return Ok(None);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(marker)
            .and_then(|f| f.take(513).read_to_end(&mut bytes))
            .map_err(|_| RECOVERY)?;
        let intent: Self = serde_json::from_slice(&bytes).map_err(|_| RECOVERY)?;
        if bytes.len() > 512
            || intent.wrapper_sha256.len() != 64
            || !intent.wrapper_sha256.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(RECOVERY.into());
        }
        Ok(Some(intent))
    }
    pub fn record(
        root: &Path,
        vault: uuid::Uuid,
        to_password: bool,
        wrapper_sha256: String,
    ) -> Result<Self, String> {
        let intent = Self {
            vault,
            operation: uuid::Uuid::new_v4(),
            to_password,
            wrapper_sha256,
        };
        let mut file = tempfile::NamedTempFile::new_in(root).map_err(|_| RECOVERY)?;
        serde_json::to_writer(file.as_file_mut(), &intent).map_err(|_| RECOVERY)?;
        file.as_file().sync_all().map_err(|_| RECOVERY)?;
        file.persist_noclobber(root.join(MARKER))
            .map_err(|_| RECOVERY)?;
        crate::sync_directory(root)?;
        Ok(intent)
    }
    fn stage(&self, directory: &Path) -> PathBuf {
        directory.join(format!(".keystore-switch-{}.sqlite3", self.operation))
    }
    pub fn committed(&self, root: &Path) -> Result<bool, String> {
        let path = checked_path(root)?.ok_or(RECOVERY)?;
        if !owned_regular(&path)? || identity(&path)? != self.vault {
            return Err(RECOVERY.into());
        }
        Ok(password::envelope(&path)?.is_some() == self.to_password)
    }
    pub fn publish(&self, root: &Path, file: tempfile::NamedTempFile) -> Result<(), String> {
        if self.committed(root)? {
            return Err(RECOVERY.into());
        }
        let directory = database_dir(root)?;
        let path = self.stage(&directory);
        file.as_file().sync_all().map_err(|_| RECOVERY)?;
        let file = file.persist_noclobber(&path).map_err(|_| RECOVERY)?;
        drop(file);
        crate::sync_directory(&directory)?;
        let live = directory.join(FILE_NAME);
        for suffix in ["-journal", "-wal", "-shm"] {
            if owned_regular(&directory.join(format!("{FILE_NAME}{suffix}")))? {
                return Err(RECOVERY.into());
            }
        }
        crate::replace_file(&path, &live)?;
        crate::sync_directory(&directory)
    }
    pub fn finish(&self, root: &Path) -> Result<(), String> {
        let directory = database_dir(root)?;
        let stage = self.stage(&directory);
        if owned_regular(&stage)? {
            if identity(&stage)? != self.vault {
                return Err(RECOVERY.into());
            }
            std::fs::remove_file(&stage).map_err(|_| RECOVERY)?;
        }
        // Remove abandoned encrypted staging images of this Vault. No directory recursion.
        for entry in std::fs::read_dir(&directory).map_err(|_| RECOVERY)? {
            let entry = entry.map_err(|_| RECOVERY)?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".keystore-stage-")
                && owned_regular(&entry.path())?
                && identity(&entry.path())? == self.vault
            {
                std::fs::remove_file(entry.path()).map_err(|_| RECOVERY)?;
            }
        }
        crate::sync_directory(&directory)?;
        pages::clean_retired(&directory, &[self.vault])?;
        std::fs::remove_file(root.join(MARKER)).map_err(|_| RECOVERY)?;
        crate::sync_directory(root)
    }
}
