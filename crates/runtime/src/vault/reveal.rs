//! First-party view-only reads. No plugin RPC, reusable credential proof or
//! persistent entry value is exposed through this path.
use super::*;

#[derive(Clone, Copy)]
pub struct VaultRevealLease(uuid::Uuid);

#[derive(Serialize, ts_rs::TS)]
pub struct VaultEntryReveal {
    pub kind: VaultKind,
    pub value: Value,
}
impl Drop for VaultEntryReveal {
    fn drop(&mut self) {
        clear_value(&mut self.value);
    }
}

pub(super) fn session_for_reveal(
    state: &State,
    lease: VaultRevealLease,
) -> Result<&Session, String> {
    let session = state.session.as_ref().ok_or("vault_locked")?;
    if session.epoch != lease.0 {
        return Err("vault_reveal_expired".into());
    }
    Ok(session)
}

fn user_entry(
    session: &Session,
    rows: &Rows<'_>,
    id: &str,
    revision: &str,
) -> Result<rows::EntryHeader, String> {
    if id.len() != 73 || revision.len() > 20 {
        return Err("entry_missing".into());
    }
    let entry = rows::current_header(rows, session.id, id, revision)?;
    if id.split_once('.').unwrap().1 != entry.id.hyphenated().to_string() {
        return Err("entry_missing".into());
    }
    // Metadata can be visible in the editor without authorizing an owner's value
    if !entry.is_personal() {
        return Err("permission_denied".into());
    }
    Ok(entry)
}

fn apply_to_entry<T>(
    session: &Session,
    entry: rows::EntryHeader,
    apply: impl FnOnce(&Session, &Rows<'_>, rows::EntryHeader) -> Result<(T, bool), String>,
) -> Result<T, String> {
    let (result, changed) = session.vault.rows(|rows| apply(session, rows, entry))?;
    if changed {
        session.changed();
    }
    Ok(result)
}

impl Vault {
    /// Capture the unlocked session before dispatch onto a blocking worker.
    pub fn begin_entry_reveal(&self) -> Result<VaultRevealLease, String> {
        self.session_epoch().map(VaultRevealLease)
    }

    /// Do not give a late native result to an editor after this Vault locks or rotates.
    pub fn check_entry_reveal(&self, lease: VaultRevealLease) -> Result<(), String> {
        session_for_reveal(&self.current_state(), lease)?;
        Ok(())
    }

    pub fn reveal_entry(
        &self,
        lease: VaultRevealLease,
        id: &str,
        revision: &str,
        password: Option<&str>,
    ) -> Result<VaultEntryReveal, String> {
        self.with_personal_entry(
            lease,
            id,
            revision,
            password,
            |entry| entry.protected,
            |_, rows, entry| {
                let mut value = rows::value(rows, entry.id)?;
                entry.kind.validate(&value.0)?;
                Ok((
                    VaultEntryReveal {
                        kind: entry.kind,
                        value: value.0.take(),
                    },
                    false,
                ))
            },
        )
    }

    // Change only personal reveal protection, fencing stale edits and re-verifying downgrades
    #[allow(clippy::too_many_arguments)]
    pub fn set_entry_protection<A>(
        &self,
        lease: VaultRevealLease,
        id: &str,
        revision: &str,
        protected: bool,
        password: Option<&str>,
        admit: impl FnOnce() -> Result<A, String>,
    ) -> Result<VaultEntry, String> {
        let mut admission = None;
        let result = self.with_personal_entry(
            lease,
            id,
            revision,
            password,
            |entry| entry.protected && !protected,
            |session, rows, mut entry| {
                if session.mode != "password" {
                    return Err("master_password_mode_required".into());
                }
                if entry.protected == protected {
                    return Ok((entry.metadata(session.id), false));
                }
                entry.advance()?;
                entry.protected = protected;
                admission = Some(admit()?);
                if entry.kind == VaultKind::Text {
                    for mut grant in rows::all_grants(rows)?.into_iter().filter(|grant| {
                        grant.entry == entry.id && grant.slot.kind == "value" && !grant.revoked
                    }) {
                        grant.revoked = true;
                        rows::put_grant(rows, &grant)?;
                    }
                }
                rows::put_header(rows, &entry)?;
                rows::bump(rows)?;
                Ok((entry.metadata(session.id), true))
            },
        );
        drop(admission);
        result
    }

    // Bind one password proof to the original lease, exact personal entry and revision
    fn with_personal_entry<T>(
        &self,
        lease: VaultRevealLease,
        id: &str,
        revision: &str,
        password: Option<&str>,
        needs_password: impl Fn(&rows::EntryHeader) -> bool,
        apply: impl FnOnce(&Session, &Rows<'_>, rows::EntryHeader) -> Result<(T, bool), String>,
    ) -> Result<T, String> {
        let (vault_id, wrapper, password) = {
            let mut state = self.current_state();
            let session = session_for_reveal(&state, lease)?;
            let entry = session
                .vault
                .rows(|rows| user_entry(session, rows, id, revision))?;
            if !needs_password(&entry) {
                return apply_to_entry(session, entry, apply);
            }
            if session.mode != "password" {
                return Err("master_password_mode_required".into());
            }
            let password = password
                .filter(|password| !password.is_empty())
                .ok_or("password_required")?;
            if password.len() > vault::password::MAX_PASSWORD_BYTES {
                return Err("invalid_password".into());
            }
            if state.password_throttled() {
                return Err("rate_limited".into());
            }
            let path = vault::checked_path(&self.root)?.ok_or("vault_recovery_required")?;
            let wrapper = vault::password::envelope(&path)?.ok_or("vault_recovery_required")?;
            let vault_id = session.id;
            state.retry_after = Some(Instant::now() + Duration::from_secs(1));
            state.reveal_attempt_pending = true;
            (vault_id, wrapper, password)
        };

        // Run Argon2 outside the mutex so lock, expiry and unrelated edits can take effect
        let proof = wrapper.unlock(vault_id, password);
        let mut state = self.current_state();
        state.reveal_attempt_pending = false;
        session_for_reveal(&state, lease)?;
        let key = match proof {
            Ok(key) => key,
            Err(error) => {
                if error == "incorrect_password" {
                    state.password_failure();
                }
                return Err(error);
            }
        };
        let session = session_for_reveal(&state, lease)?;
        if !session.vault.matches_data_key(&key)? {
            return Err("vault_recovery_required".into());
        }
        let entry = session
            .vault
            .rows(|rows| user_entry(session, rows, id, revision))?;
        if !needs_password(&entry) {
            return Err("stale_entry_revision".into());
        }
        let result = apply_to_entry(session, entry, apply);
        state.failed_passwords = 0;
        state.retry_after = None;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn open() -> (tempfile::TempDir, Vault) {
        let root = tempfile::tempdir().unwrap();
        let storage = Storage::open_staging(root.path().join("store")).unwrap();
        let vault = Vault::new(
            root.path().into(),
            Arc::new(super::super::tests::TestKeys::default()),
            storage,
        );
        (root, vault)
    }

    #[test]
    fn reveal_only_exact_user_entry_from_current_unlocked_session() {
        let (_root, vault) = open();
        vault.initialize().unwrap();
        let user = vault
            .create_entry(VaultEntryInput {
                label: "Visible note".into(),
                kind: VaultKind::Text,
                protected: false,
                value: json!("fixture value"),
            })
            .unwrap();
        let protected = vault
            .create_entry(VaultEntryInput {
                label: "Protected token".into(),
                kind: VaultKind::ApiKey,
                protected: true,
                value: json!("fixture token"),
            })
            .unwrap();
        let private = vault
            .create_entry_record(
                Entry {
                    private_slot: None,
                    id: uuid::Uuid::new_v4(),
                    label: "Private".into(),
                    kind: VaultKind::Text,
                    protected: false,
                    revision: 1,
                    successor: None,
                    owner: Some(VaultInstallation {
                        owner: "package.fixture".into(),
                        token: "test install".into(),
                        inventory_sha256: "verified fixture".into(),
                        storage_generation: "test".into(),
                    }),
                    value: json!("private fixture value"),
                },
                None,
            )
            .unwrap();
        let lease = vault.begin_entry_reveal().unwrap();
        let reveal = |entry: &VaultEntry, password: Option<&str>| {
            vault.reveal_entry(lease, &entry.id, &entry.revision, password)
        };
        let result = reveal(&user, None).unwrap();
        assert_eq!(result.kind, VaultKind::Text);
        assert_eq!(result.value, json!("fixture value"));
        assert_eq!(
            reveal(&protected, Some("fixture password")).err().unwrap(),
            "master_password_mode_required"
        );
        assert_eq!(reveal(&private, None).err().unwrap(), "permission_denied");
        assert_eq!(
            vault
                .reveal_entry(
                    lease,
                    &format!(
                        "{}.{}",
                        uuid::Uuid::new_v4(),
                        user.id.split_once('.').unwrap().1
                    ),
                    &user.revision,
                    None,
                )
                .err()
                .unwrap(),
            "entry_missing"
        );
        vault
            .replace_entry(&user.id, &user.revision, json!("new"))
            .unwrap();
        assert_eq!(reveal(&user, None).err().unwrap(), "stale_entry_revision");
        vault.lock();
        assert_eq!(reveal(&user, None).err().unwrap(), "vault_locked");
        vault.unlock().unwrap();
        assert_eq!(reveal(&user, None).err().unwrap(), "vault_reveal_expired");
    }

    #[test]
    fn every_kind_can_be_created_with_explicit_protection_on_or_off() {
        let (_root, vault) = open();
        vault.initialize().unwrap();
        let lease = vault.begin_entry_reveal().unwrap();
        let path = std::env::temp_dir().to_string_lossy().into_owned();
        for (kind, value) in [
            (VaultKind::ApiKey, json!("fixture key")),
            (VaultKind::BearerToken, json!("fixture token")),
            (VaultKind::Credential, json!("fixture credential")),
            (VaultKind::Text, json!("fixture text")),
            (VaultKind::Integer, json!(42)),
            (VaultKind::Boolean, json!(true)),
            (VaultKind::Json, json!({"test":1})),
            (VaultKind::File, json!(path)),
            (VaultKind::Folder, json!(path)),
            (
                VaultKind::Location,
                json!({"name":"Fixture","latitude":1,"longitude":2}),
            ),
        ] {
            for protected in [true, false] {
                let entry = vault
                    .create_entry(VaultEntryInput {
                        label: format!("{kind:?}"),
                        kind,
                        protected,
                        value: value.clone(),
                    })
                    .unwrap();
                assert_eq!(entry.protected, protected);
                let reveal = vault.reveal_entry(lease, &entry.id, &entry.revision, None);
                if protected {
                    assert_eq!(reveal.err().unwrap(), "master_password_mode_required");
                } else {
                    assert_eq!(reveal.unwrap().value, value);
                }
                assert_eq!(
                    vault
                        .set_entry_protection(
                            lease,
                            &entry.id,
                            &entry.revision,
                            !protected,
                            None,
                            || Ok(())
                        )
                        .err()
                        .unwrap(),
                    "master_password_mode_required"
                );
            }
        }
        vault.lock();
        vault.unlock().unwrap();
        assert_eq!(
            vault
                .entries(&[])
                .unwrap()
                .iter()
                .filter(|e| e.protected)
                .count(),
            10
        );
    }

    #[test]
    fn protection_change_rechecks_password_revision_owner_and_editor_admission() {
        let (_root, vault) = open();
        let password = "synthetic fixture master password";
        vault.initialize_password(password, password).unwrap();
        let create = |kind, protected| {
            vault
                .create_entry(VaultEntryInput {
                    label: "Fixture".into(),
                    kind,
                    protected,
                    value: json!("fixture value"),
                })
                .unwrap()
        };
        let text = create(VaultKind::Text, false);
        let secret = create(VaultKind::ApiKey, false);
        let owner = VaultInstallation {
            owner: "fixture.plugin".into(),
            token: "fixture g1".into(),
            inventory_sha256: "fixture digest".into(),
            storage_generation: "fixture generation".into(),
        };
        let slot = |kind: &str, entry_kind: &str| {
            serde_json::from_value::<VaultSetupSlot>(json!({
                "id":"value","field":"value","action":"fixture.plugin.action","required":true,
                "purpose":"Fixture","kind":kind,"entry_kind":entry_kind,"mode":"direct",
            }))
            .unwrap()
        };
        let plaintext = slot("value", "text");
        let secret_slot = slot("secret", "api-key");
        assert!(vault
            .setup_entries(&slot("value", "api-key"))
            .unwrap()
            .1
            .is_empty());
        assert_eq!(
            vault.setup_entries(&secret_slot).unwrap().1[0].id,
            secret.id
        );
        assert_eq!(vault.setup_entries(&plaintext).unwrap().1[0].id, text.id);
        let (_, assignment) = vault
            .setup_selection(&plaintext, &text.id, &text.revision, false)
            .unwrap();
        vault
            .grant_setup(
                &owner,
                std::slice::from_ref(&plaintext),
                &std::collections::BTreeMap::from([(
                    (plaintext.action.clone(), plaintext.id.clone()),
                    assignment,
                )]),
                || Ok(()),
            )
            .unwrap();
        let lease = vault.begin_entry_reveal().unwrap();
        assert!(vault.required_ready(&owner, &[&plaintext]));
        let set = |entry: &VaultEntry, protected, password| {
            vault.set_entry_protection(
                lease,
                &entry.id,
                &entry.revision,
                protected,
                password,
                || Ok(()),
            )
        };
        assert_eq!(
            vault
                .set_entry_protection(
                    lease,
                    &text.id,
                    &text.revision,
                    true,
                    None,
                    || Err::<(), _>("editor closed".into())
                )
                .err()
                .unwrap(),
            "editor closed"
        );
        assert!(vault.required_ready(&owner, &[&plaintext]));
        let protected = set(&text, true, None).unwrap();
        assert!(protected.protected);
        assert_eq!(protected.kind, text.kind);
        assert_eq!(protected.id, text.id);
        assert_ne!(protected.revision, text.revision);
        assert!(!serde_json::to_value(&protected)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("value"));
        assert!(!vault.required_ready(&owner, &[&plaintext]));
        assert_eq!(
            set(&text, false, Some(password)).err().unwrap(),
            "stale_entry_revision"
        );
        assert_eq!(
            set(&protected, false, None).err().unwrap(),
            "password_required"
        );
        assert_eq!(
            set(&protected, false, Some("incorrect")).err().unwrap(),
            "incorrect_password"
        );
        assert_eq!(
            set(&protected, false, Some(password)).err().unwrap(),
            "rate_limited"
        );
        vault.state.lock().unwrap().retry_after = None;
        let unprotected = set(&protected, false, Some(password)).unwrap();
        assert!(!unprotected.protected);
        assert_eq!(
            vault
                .reveal_entry(lease, &unprotected.id, &unprotected.revision, None)
                .unwrap()
                .value,
            json!("fixture value")
        );
        assert!(!vault.required_ready(&owner, &[&plaintext]));
        assert_eq!(vault.access(std::slice::from_ref(&owner)).unwrap().len(), 0);

        let provider_secret = set(&secret, true, None).unwrap();
        assert!(provider_secret.protected);
        assert_eq!(
            vault.setup_entries(&secret_slot).unwrap().1[0].id,
            secret.id
        );
        let private = vault
            .create_entry_record(
                Entry {
                    private_slot: None,
                    id: uuid::Uuid::new_v4(),
                    label: "Private fixture".into(),
                    kind: VaultKind::Text,
                    protected: false,
                    revision: 1,
                    owner: Some(owner),
                    successor: None,
                    value: json!("fixture only"),
                },
                None,
            )
            .unwrap();
        assert_eq!(
            set(&private, true, None).err().unwrap(),
            "permission_denied"
        );
        vault.lock();
        assert_eq!(set(&unprotected, true, None).err().unwrap(), "vault_locked");
    }

    #[test]
    fn protected_reveal_reverifies_uses_backoff_and_does_not_persist_value() {
        let (root, vault) = open();
        let password = "synthetic original master password";
        vault.initialize_password(password, password).unwrap();
        let entry = vault
            .create_entry(VaultEntryInput {
                label: "Fixture secret".into(),
                kind: VaultKind::Credential,
                protected: true,
                value: json!("fixture protected value"),
            })
            .unwrap();
        let handle = vault::database_file(root.path()).unwrap();
        let bytes = std::fs::read(&handle).unwrap();
        let lease = vault.begin_entry_reveal().unwrap();
        let reveal = |password: Option<&str>| {
            vault.reveal_entry(lease, &entry.id, &entry.revision, password)
        };
        assert_eq!(reveal(None).err().unwrap(), "password_required");
        assert_eq!(
            reveal(Some("incorrect")).err().unwrap(),
            "incorrect_password"
        );
        assert_eq!(reveal(Some(password)).err().unwrap(), "rate_limited");
        assert_eq!(vault.status().state, VaultState::Unlocked);
        vault.state.lock().unwrap().retry_after = None;
        assert_eq!(
            reveal(Some(password)).unwrap().value,
            json!("fixture protected value")
        );
        assert_eq!(reveal(None).err().unwrap(), "password_required");
        assert_eq!(std::fs::read(&handle).unwrap(), bytes);

        // Even an authentic wrapper for this UUID must unwrap the active key.
        let different_key = vault::data_key().unwrap();
        let forged_password = "different wrapper fixture password";
        {
            let mut state = vault.state.lock().unwrap();
            let session = state.session.as_mut().unwrap();
            let other =
                vault::password::PasswordEnvelope::new(session.id, forged_password, &different_key)
                    .unwrap();
            session.vault.store_envelope(&other).unwrap();
        }
        assert_eq!(
            reveal(Some(forged_password)).err().unwrap(),
            "vault_recovery_required"
        );
    }
}
