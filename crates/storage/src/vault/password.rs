//! Password envelope v1 uses Argon2id v1.3 (64 MiB, three passes, one lane)
//! and AES-256-GCM. The format chooses fixed KDF parameters before allocating.
use super::*;
use argon2::{Algorithm, Argon2, Block, Params, Version};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};

const MEMORY_KIB: u32 = 64 * 1024;
pub(super) const ENVELOPE_BYTES: usize = 1 + 16 + 12 + 32 + 16;
pub const MAX_PASSWORD_BYTES: usize = 1024;

fn wrapping_key(password: &str, salt: &[u8]) -> Result<LessSafeKey, String> {
    if password.is_empty() || password.len() > MAX_PASSWORD_BYTES {
        return Err("invalid_password".into());
    }
    let parameters = Params::new(MEMORY_KIB, 3, 1, Some(32)).map_err(|_| "vault_unavailable")?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters);
    let mut key = Zeroizing::new([0u8; 32]);
    let mut memory = Vec::new();
    memory
        .try_reserve_exact(MEMORY_KIB as usize)
        .map_err(|_| "vault_unavailable")?;
    memory.resize_with(MEMORY_KIB as usize, Block::default);
    let mut memory = Zeroizing::new(memory);
    argon
        .hash_password_into_with_memory(password.as_bytes(), salt, key.as_mut(), &mut *memory)
        .map_err(|_| "vault_unavailable")?;
    Ok(LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key.as_slice()).map_err(|_| "vault_unavailable")?,
    ))
}

pub struct PasswordEnvelope([u8; ENVELOPE_BYTES]);

impl PasswordEnvelope {
    pub(super) fn encoded(&self) -> String {
        hex::encode(self.0)
    }
    pub(super) fn decode(text: &str) -> Result<Self, String> {
        let mut bytes = [0; ENVELOPE_BYTES];
        hex::decode_to_slice(text, &mut bytes).map_err(|_| RECOVERY)?;
        if bytes[0] != 1 {
            return Err(RECOVERY.into());
        }
        Ok(Self(bytes))
    }
    pub fn new(id: uuid::Uuid, password: &str, key: &[u8; 32]) -> Result<Self, String> {
        let mut bytes = Zeroizing::new([0; ENVELOPE_BYTES]);
        bytes[0] = 1;
        SystemRandom::new()
            .fill(&mut bytes[1..29])
            .map_err(|_| "vault_unavailable")?;
        let wrapping = wrapping_key(password, &bytes[1..17])?;
        let nonce = Nonce::assume_unique_for_key(bytes[17..29].try_into().unwrap());
        bytes[29..61].copy_from_slice(key);
        let tag = wrapping
            .seal_in_place_separate_tag(nonce, Aad::from(id.as_bytes()), &mut bytes[29..61])
            .map_err(|_| "vault_unavailable")?;
        bytes[61..].copy_from_slice(tag.as_ref());
        Ok(Self(*bytes))
    }

    pub fn unlock(&self, id: uuid::Uuid, password: &str) -> Result<Zeroizing<[u8; 32]>, String> {
        let key = wrapping_key(password, &self.0[1..17])?;
        let nonce = Nonce::assume_unique_for_key(self.0[17..29].try_into().unwrap());
        let mut wrapped = Zeroizing::new(self.0[29..].to_vec());
        let plain = key
            .open_in_place(nonce, Aad::from(id.as_bytes()), &mut wrapped)
            .map_err(|_| "incorrect_password")?;
        let mut result = Zeroizing::new([0; 32]);
        result.copy_from_slice(plain);
        Ok(result)
    }
}

fn read(db: &Connection) -> Result<Option<PasswordEnvelope>, String> {
    let format: i64 = db
        .query_row("SELECT format FROM vault WHERE singleton=1", [], |r| {
            r.get(0)
        })
        .map_err(|_| RECOVERY)?;
    if format == FORMAT {
        return Ok(None);
    }
    if format != PASSWORD_FORMAT {
        return Err(RECOVERY.into());
    }
    let wrapped: Vec<u8> = db
        .query_row(
            "SELECT envelope FROM vault_password WHERE singleton=1 AND length(envelope)=?1",
            [ENVELOPE_BYTES],
            |r| r.get(0),
        )
        .map_err(|_| RECOVERY)?;
    let bytes: [u8; ENVELOPE_BYTES] = wrapped.try_into().map_err(|_| RECOVERY)?;
    if bytes[0] != 1 {
        return Err(RECOVERY.into());
    }
    Ok(Some(PasswordEnvelope(bytes)))
}

pub fn envelope(path: &Path) -> Result<Option<PasswordEnvelope>, String> {
    if let Some((_, h)) = pages::lookup(path)? {
        return h.password();
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| RECOVERY)?;
    configure(&db)?;
    read(&db)
}

impl EncryptedVault {
    /// Install or rotate the authenticated password envelope atomically
    pub fn store_envelope(&mut self, envelope: &PasswordEnvelope) -> Result<(), String> {
        pages::Header::replace_password(&self.page_header, self.id, envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_rotation_preserves_row_data_and_existing_key() {
        use rows::RowKind;
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let key = data_key().unwrap();
        let path = root.path().join(FILE_NAME);
        EncryptedVault::stage_rows(root.path(), id, &key, |tx| {
            tx.put(RowKind::Entry, "fixture", "", &"sample row")
        })
        .unwrap()
        .persist_noclobber(&path)
        .unwrap();
        let mut vault = EncryptedVault::open(&path, id, &key).unwrap();
        let old = PasswordEnvelope::new(id, "old fixture", &key).unwrap();
        let new = PasswordEnvelope::new(id, "new fixture", &key).unwrap();
        vault.store_envelope(&old).unwrap();
        assert!(vault
            .matches_data_key(&old.unlock(id, "old fixture").unwrap())
            .unwrap());
        vault.store_envelope(&new).unwrap();
        drop(vault);
        let committed = envelope(&path).unwrap().unwrap();
        assert_eq!(committed.0, new.0);
        let opened = committed.unlock(id, "new fixture").unwrap();
        assert_eq!(
            EncryptedVault::open(&path, id, &opened)
                .unwrap()
                .rows(|tx| tx.get::<String>(RowKind::Entry, "fixture"))
                .unwrap(),
            Some("sample row".into())
        );
        assert!(committed.unlock(id, "old fixture").is_err());
        assert!(committed
            .unlock(uuid::Uuid::new_v4(), "new fixture")
            .is_err());
    }
}
