//! Password-protected transfers carry an independent random key wrapped by the same
//! Argon2id envelope as a personal Vault. The entire bounded payload is authenticated
//! before callers can inspect it or change an installation.
use super::{data_key, password::PasswordEnvelope};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use std::{
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Profiles,
    Macros,
    Backup,
    VaultEntries,
}
impl Kind {
    fn signature(self) -> &'static [u8; 8] {
        match self {
            Self::Profiles => b"SDMPROF1",
            Self::Macros => b"SDMMACR1",
            Self::Backup => b"SDMBACK1",
            Self::VaultEntries => b"SDMVAUL1",
        }
    }
    fn associated_data(self, id: &[u8; 16]) -> [u8; 24] {
        let mut data = [0; 24];
        data[..8].copy_from_slice(self.signature());
        data[8..].copy_from_slice(id);
        data
    }
}
/// A public routing hint only. Its authenticity is verified when the transfer is opened.
pub fn read_kind(input: &mut impl Read) -> Result<Option<Kind>, String> {
    let mut signature = [0; 8];
    match input.read_exact(&mut signature) {
        Ok(()) => Ok([
            Kind::Profiles,
            Kind::Macros,
            Kind::Backup,
            Kind::VaultEntries,
        ]
        .into_iter()
        .find(|kind| kind.signature() == &signature)),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
const MAX: usize = 256 * 1024 * 1024;
const AUTH_ERROR: &str = "The transfer password is incorrect or the file is damaged.";
pub const PASSWORD_REQUIRED: &str = "This transfer requires its export password.";
fn cipher(key: &[u8; 32]) -> Result<LessSafeKey, String> {
    Ok(LessSafeKey::new(
        UnboundKey::new(&AES_256_GCM, key).map_err(|_| AUTH_ERROR)?,
    ))
}
pub fn write(
    kind: Kind,
    output: &mut impl Write,
    data: &mut Vec<u8>,
    password: &str,
) -> Result<(), String> {
    if data.len() > MAX {
        return Err("Transfer exceeds 256 MiB".into());
    }
    let key = data_key()?;
    let id = uuid::Uuid::new_v4();
    let wrapped = PasswordEnvelope::new(id, password, &key)?;
    // Each transfer gets a fresh one-use key. Bind its random identity to the ciphertext.
    cipher(&key)?
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key([0; 12]),
            Aad::from(kind.associated_data(id.as_bytes())),
            data,
        )
        .map_err(|_| AUTH_ERROR)?;
    output
        .write_all(kind.signature())
        .and_then(|_| output.write_all(id.as_bytes()))
        .and_then(|_| output.write_all(wrapped.encoded().as_bytes()))
        .and_then(|_| output.write_all(data))
        .map_err(|e| e.to_string())
}
pub fn open(path: &Path, password: Option<&str>) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let Some(kind) = read_kind(&mut file)? else {
        return Ok(None);
    };
    if kind == Kind::VaultEntries {
        return Err("Use the Vault entries import for this transfer.".into());
    }
    open_payload(kind, &mut file, password, MAX).map(Some)
}

// Open an already identified encrypted transfer through the same bounded reader
pub fn open_payload(
    kind: Kind,
    input: &mut impl Read,
    password: Option<&str>,
    max: usize,
) -> Result<Zeroizing<Vec<u8>>, String> {
    let max = max.min(MAX);
    let password = password.ok_or(PASSWORD_REQUIRED)?;
    let mut id = [0; 16];
    let mut envelope = [0; super::password::ENVELOPE_BYTES * 2];
    input
        .read_exact(&mut id)
        .and_then(|_| input.read_exact(&mut envelope))
        .map_err(|_| AUTH_ERROR)?;
    let wrapped = PasswordEnvelope::decode(std::str::from_utf8(&envelope).map_err(|_| AUTH_ERROR)?)
        .map_err(|_| AUTH_ERROR)?;
    let key = wrapped
        .unlock(uuid::Uuid::from_bytes(id), password)
        .map_err(|_| AUTH_ERROR)?;
    let mut bytes = Zeroizing::new(Vec::new());
    input
        .take((max + 17) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > max + 16 {
        return Err("Transfer exceeds its size limit".into());
    }
    let len = cipher(&key)?
        .open_in_place(
            Nonce::assume_unique_for_key([0; 12]),
            Aad::from(kind.associated_data(&id)),
            &mut bytes,
        )
        .map_err(|_| AUTH_ERROR)?
        .len();
    bytes.truncate(len);
    Ok(bytes)
}
