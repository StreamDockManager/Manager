//! Compact serde JSON digest. No domain normalization or validation is performed
use serde::Serialize;
use sha2::{Digest, Sha256};
struct HashWriter(Sha256);
impl std::io::Write for HashWriter {
    // Write for the current operation
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    // Flush for the current operation
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
// Hash the canonical JSON representation using SHA-256
pub fn sha256(value: &(impl Serialize + ?Sized)) -> Result<String, String> {
    sha256_bytes(value).map(hex::encode)
}

/// Stream compact JSON directly into the digest without retaining a serialized copy.
pub fn sha256_bytes(value: &(impl Serialize + ?Sized)) -> Result<[u8; 32], String> {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value).map_err(|e| e.to_string())?;
    Ok(writer.0.finalize().into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify matches compact bytes including unicode and key order
    fn matches_compact_bytes_including_unicode_and_key_order() {
        for value in [
            serde_json::json!(null),
            serde_json::json!({"z":"é😀\n", "a":[1,true,1.5]}),
        ] {
            assert_eq!(
                sha256(&value).unwrap(),
                hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap()))
            );
        }
    }
    #[test]
    // Verify serialization errors are not a digest
    fn serialization_errors_are_not_a_digest() {
        let invalid = std::collections::BTreeMap::from([(vec![1, 2], "value")]);
        assert!(sha256(&invalid).is_err());
    }
}
