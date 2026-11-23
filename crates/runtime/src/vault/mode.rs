//! Rotate the DEK when changing wrappers, preserving entry IDs and exact grants.
use super::*;
use crate::credentials::CredentialState;
use sha2::{Digest, Sha256};
use vault::transition::Transition;

fn fingerprint(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

impl Vault {
    /// Recover only the exact app-owned wrapper recorded before changing modes. This never unlocks Vault.
    pub(super) fn recover_mode(&self) -> Result<(), String> {
        let Some(intent) = Transition::pending(&self.root)? else {
            return Ok(());
        };
        let committed = intent.committed(&self.root)?;
        let account = key_account(intent.vault);
        let retiring = (committed && intent.to_password) || (!committed && !intent.to_password);
        match self.credentials.read(&account).map(Zeroizing::new) {
            Ok(value) if fingerprint(&value) == intent.wrapper_sha256 => {
                if retiring {
                    self.credentials
                        .delete(&account)
                        .map_err(|_| "vault_unavailable")?;
                    if !matches!(
                        self.credentials.read(&account).map(Zeroizing::new),
                        Err(CredentialState::Missing)
                    ) {
                        return Err("vault_recovery_required".into());
                    }
                } else {
                    let key = decode_os_key(&value, intent.vault, None)?;
                    let path = vault::checked_path(&self.root)?.ok_or("vault_recovery_required")?;
                    read_document(&EncryptedVault::open(&path, intent.vault, &key)?)?;
                }
            }
            Err(CredentialState::Missing) if retiring => {}
            _ => return Err("vault_recovery_required".into()),
        }
        intent.finish(&self.root)
    }

    pub fn switch_mode(
        &self,
        to_password: bool,
        current: &str,
        password: &str,
        confirmation: &str,
    ) -> Result<(), String> {
        if to_password {
            validate_new_password(password, confirmation)?;
            if !current.is_empty() {
                return Err("invalid_request".into());
            }
        } else if current.is_empty() || !password.is_empty() || !confirmation.is_empty() {
            return Err("invalid_request".into());
        }
        let mut state = self.current_state();
        self.recover_mode()?;
        if vault::reset_pending(&self.root)? {
            return Err("vault_recovery_required".into());
        }
        if state.password_throttled() {
            return Err("rate_limited".into());
        }
        let session = state.session.as_ref().ok_or("vault_locked")?;
        if (session.mode == "password") == to_password {
            return Err("incorrect_unlock_mode".into());
        }
        let id = session.id;
        let path = vault::checked_path(&self.root)?.ok_or("vault_recovery_required")?;
        let account = key_account(id);
        let fresh = vault::data_key()?;
        let os_wrapper = if to_password {
            let old = Zeroizing::new(
                self.credentials
                    .read(&account)
                    .map_err(|_| "vault_unavailable")?,
            );
            let key = decode_os_key(&old, id, None)?;
            read_document(&EncryptedVault::open(&path, id, &key)?)?;
            old
        } else {
            let envelope = vault::password::envelope(&path)?.ok_or("vault_recovery_required")?;
            let previous = match envelope.unlock(id, current) {
                Ok(key) => key,
                Err(code) => {
                    if code == "incorrect_password" {
                        state.password_failure();
                    }
                    return Err(code);
                }
            };
            read_document(&EncryptedVault::open(&path, id, &previous)?)?;
            if !matches!(
                self.credentials.read(&account).map(Zeroizing::new),
                Err(CredentialState::Missing)
            ) {
                return Err("vault_unavailable".into());
            }
            Zeroizing::new(hex::encode(fresh.as_slice()))
        };
        let document = read_document(&session.vault)?;
        let staged =
            EncryptedVault::stage_rows(&vault::database_dir(&self.root)?, id, &fresh, |rows| {
                rows::publish_document(rows, &document)
            })?;
        if to_password {
            EncryptedVault::open(staged.path(), id, &fresh)?.store_envelope(
                &vault::password::PasswordEnvelope::new(id, password, &fresh)?,
            )?;
            let key = vault::password::envelope(staged.path())?
                .ok_or("vault_recovery_required")?
                .unlock(id, password)?;
            read_document(&EncryptedVault::open(staged.path(), id, &key)?)?;
        }
        let intent = Transition::record(&self.root, id, to_password, fingerprint(&os_wrapper))?;
        state.session = None;
        state.reviews.clear();
        state.reset_review = None;
        state.locked_event_pending = true;
        self.wake.notify();
        if !to_password {
            self.credentials
                .write(&account, &os_wrapper)
                .map_err(|_| "vault_unavailable")?;
            let encoded = Zeroizing::new(
                self.credentials
                    .read(&account)
                    .map_err(|_| "vault_unavailable")?,
            );
            let verified = decode_os_key(&encoded, id, None)?;
            read_document(&EncryptedVault::open(staged.path(), id, &verified)?)?;
        }
        intent.publish(&self.root, staged)?;
        self.recover_mode()?;
        state.failed_passwords = 0;
        state.retry_after = None;
        state.failure = None;
        // End all outstanding reviews. The user unlocks the verified new mode explicitly.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::TestKeys;
    use super::*;

    #[test]
    fn changing_unlock_modes_rotates_keys_and_preserves_exact_entry_references() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager")).unwrap();
        let keys = Arc::new(TestKeys::default());
        let open = || Vault::new(root.path().into(), keys.clone(), store.clone());
        let vault = open();
        vault.initialize().unwrap();
        let path = vault::database_file(root.path()).unwrap();
        let id = vault::identity(&path).unwrap();
        let original = decode_os_key(&keys.read(&key_account(id)).unwrap(), id, None).unwrap();
        let entry = vault
            .create_entry(VaultEntryInput {
                kind: VaultKind::Text,
                label: "private label".into(),
                protected: false,
                value: serde_json::json!("fixture value"),
            })
            .unwrap();
        let password = "new synthetic password";
        vault.switch_mode(true, "", password, password).unwrap();
        assert_eq!(vault.status().mode, "password");
        assert!(matches!(
            keys.read(&key_account(id)),
            Err(CredentialState::Missing)
        ));
        assert!(EncryptedVault::open(&path, id, &original)
            .and_then(|v| read_document(&v))
            .is_err());
        assert!(vault.unlock().is_err());
        drop(vault);
        let vault = open();
        vault.unlock_password(password).unwrap();
        assert_eq!(vault.entries(&[]).unwrap()[0].id, entry.id);
        let old_password_key = vault::password::envelope(&path)
            .unwrap()
            .unwrap()
            .unlock(id, password)
            .unwrap();
        assert!(vault.switch_mode(false, "incorrect", "", "").is_err());
        {
            let mut state = vault.state.lock().unwrap();
            state.retry_after = None;
        }
        vault.switch_mode(false, password, "", "").unwrap();
        assert_eq!(vault.status().mode, "os");
        assert!(vault::password::envelope(&path).unwrap().is_none());
        assert!(EncryptedVault::open(&path, id, &old_password_key)
            .and_then(|v| read_document(&v))
            .is_err());
        drop(vault);
        let vault = open();
        vault.unlock().unwrap();
        assert_eq!(vault.entries(&[]).unwrap()[0].id, entry.id);
    }
    #[test]
    fn interrupted_mode_switch_reconciles_only_its_os_wrapper() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager")).unwrap();
        let keys = Arc::new(TestKeys::default());
        let vault = Vault::new(root.path().into(), keys.clone(), store);
        let password = "fixture password";
        vault.initialize_password(password, password).unwrap();
        let path = vault::database_file(root.path()).unwrap();
        let id = vault::identity(&path).unwrap();
        let account = key_account(id);
        let fresh = vault::data_key().unwrap();
        let encoded = Zeroizing::new(hex::encode(fresh.as_slice()));
        // After recording intent but before registering the new OS key, retain password mode.
        Transition::record(root.path(), id, false, fingerprint(&encoded)).unwrap();
        vault.recover_mode().unwrap();
        // A registered key must be removed before abandoning a prepublication switch.
        Transition::record(root.path(), id, false, fingerprint(&encoded)).unwrap();
        keys.write(&account, &encoded).unwrap();
        vault.recover_mode().unwrap();
        assert!(matches!(keys.read(&account), Err(CredentialState::Missing)));
        // Never erase a different credential if another writer replaced the registered item.
        Transition::record(root.path(), id, false, fingerprint(&encoded)).unwrap();
        keys.write(&account, "unrelated").unwrap();
        assert!(vault.recover_mode().is_err());
        assert_eq!(keys.read(&account).unwrap(), "unrelated");
        keys.delete(&account).unwrap();
        vault.recover_mode().unwrap();
        // After publication, preserve the new key and finish using the committed mode.
        let intent = Transition::record(root.path(), id, false, fingerprint(&encoded)).unwrap();
        keys.write(&account, &encoded).unwrap();
        let stage = EncryptedVault::stage_rows(
            &vault::database_dir(root.path()).unwrap(),
            id,
            &fresh,
            |_| Ok(()),
        )
        .unwrap();
        vault.end_session();
        intent.publish(root.path(), stage).unwrap();
        vault.recover_mode().unwrap();
        vault.unlock().unwrap();
        assert_eq!(vault.status().mode, "os");
    }
}
