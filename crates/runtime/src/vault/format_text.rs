//! The formatter reads only existing direct, nonsecret entry grants for one installed action.
use super::*;
use std::collections::HashMap;
use stream_dock_manager_plugin_host::{text_references::expand, vault_text};

struct Candidate {
    grant: Grant,
    value: Option<Option<(VaultKind, ValueBuffer)>>,
    used: bool,
}

impl Vault {
    pub(super) fn format_granted_text(
        &self,
        owner: &VaultInstallation,
        slots: &[&VaultSetupSlot],
        text: &str,
        publish: &mut dyn FnMut(&str) -> bool,
    ) -> Result<(), &'static str> {
        fn unavailable(text: &str) -> Result<String, &'static str> {
            expand(text, vault_text::MAX_OUTPUT_BYTES, |name| {
                Ok(name.starts_with("vault.").then(String::new))
            })
            .map_err(|_| "quota")
        }
        if slots.is_empty() || !text.contains("${vault.") {
            return publish(&unavailable(text)?)
                .then_some(())
                .ok_or("cancelled");
        }
        let state = self.current_state();
        let Some(session) = state.session.as_ref() else {
            return publish(&unavailable(text)?)
                .then_some(())
                .ok_or("cancelled");
        };
        let rendered = session.vault.rows(|rows| {
            let mut candidates = HashMap::new();
            for slot in slots.iter().filter(|slot| vault_text::eligible_slot(slot)) {
                if let Some(grant) = rows::grant(rows, owner, slot)?.filter(|grant| !grant.revoked)
                {
                    candidates.entry(grant.entry).or_insert(Candidate {
                        grant,
                        value: None,
                        used: false,
                    });
                }
            }
            let output = expand(text, vault_text::MAX_OUTPUT_BYTES, |name| {
                let Some(selector) = name.strip_prefix("vault.") else {
                    return Ok(None);
                };
                let result = (|| {
                    let reference = TypedReference::parse(selector).ok()?;
                    if reference.vault != session.id {
                        return None;
                    }
                    // Never query an entry from an unapproved caller-supplied UUID.
                    let candidate = candidates.get_mut(&reference.entry)?;
                    let (kind, value) = candidate
                        .value
                        .get_or_insert_with(|| {
                            let entry = rows::header(rows, reference.entry).ok().flatten()?;
                            if !entry.visible_to(owner)
                                || !eligible_kind(
                                    entry.kind,
                                    entry.protected,
                                    &candidate.grant.slot,
                                )
                            {
                                return None;
                            }
                            let value = rows::value(rows, entry.id).ok()?;
                            entry.kind.validate(&value.0).ok()?;
                            Some((entry.kind, value))
                        })
                        .as_ref()?;
                    let formatted = match (kind, reference.field) {
                        (VaultKind::Text, None) => value.0.as_str()?.to_owned(),
                        (VaultKind::Location, Some(typed::LocationField::Latitude)) => {
                            value.0["latitude"].as_number()?.to_string()
                        }
                        (VaultKind::Location, Some(typed::LocationField::Longitude)) => {
                            value.0["longitude"].as_number()?.to_string()
                        }
                        (VaultKind::Location, None) => format!(
                            "{},{}",
                            value.0["latitude"].as_number()?,
                            value.0["longitude"].as_number()?
                        ),
                        // The direct location grant exposes coordinates, never its name.
                        _ => return None,
                    };
                    candidate.used = true;
                    Some(formatted)
                })();
                Ok(Some(result.unwrap_or_default()))
            });
            Ok(output.map(|output| (output, candidates)))
        });
        let (output, candidates) = match rendered {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => return Err("quota"),
            // A broken or unavailable Vault must look like an unresolved template,
            // not report which entry exists to the caller.
            Err(_) => (unavailable(text)?, HashMap::new()),
        };
        // The state lock spans reply publication, fencing lock, rotation and revocation.
        if !publish(&zeroize::Zeroizing::new(output)) {
            return Err("cancelled");
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs().to_string());
        if now.is_some() {
            // Audit only entries successfully returned; like direct reads, this does
            // not increment the Vault revision or change the grant identity.
            let _ = session.vault.rows(|rows| {
                for mut candidate in candidates.into_values().filter(|candidate| candidate.used) {
                    if candidate.grant.last_read != now {
                        candidate.grant.last_read = now.clone();
                        rows::put_grant(rows, &candidate.grant)?;
                    }
                }
                Ok(())
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn owner(name: &str) -> VaultInstallation {
        VaultInstallation {
            owner: name.into(),
            token: format!("{name}-install"),
            inventory_sha256: "inventory".into(),
            storage_generation: "epoch".into(),
        }
    }

    fn slot(id: &str, kind: &str, entry_kind: &str) -> VaultSetupSlot {
        VaultSetupSlot {
            id: id.into(),
            field: id.into(),
            action: "fixture.format".into(),
            mode: "direct".into(),
            kind: kind.into(),
            entry_kind: entry_kind.into(),
            purpose: "format a fixture".into(),
            ..VaultSetupSlot::default()
        }
    }

    #[test]
    fn only_current_exact_nonsecret_grants_can_format_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = stream_dock_manager_storage::Storage::open(dir.path().join("manager")).unwrap();
        let vault = Vault::new(
            dir.path().into(),
            Arc::new(super::super::tests::TestKeys::default()),
            store,
        );
        vault.initialize().unwrap();
        let text_slot = slot("text", "value", "text");
        let location_slot = slot("location", "location", "location");
        let protected_slot = slot("protected", "value", "text");
        let foreign_slot = slot("foreign", "value", "text");
        let secret_slot = slot("secret", "secret", "api-key");
        let caller = owner("fixture");
        let other = owner("other");
        let create = |label: &str, kind, protected, value| {
            vault
                .create_entry(VaultEntryInput {
                    label: label.into(),
                    kind,
                    protected,
                    value,
                })
                .unwrap()
        };
        let text = create("Allowed text", VaultKind::Text, false, json!("demo"));
        let location = create(
            "Allowed location",
            VaultKind::Location,
            false,
            json!({"name":"private label","latitude":12.3456,"longitude":-78.9}),
        );
        let unassigned = create("Unassigned", VaultKind::Text, false, json!("unavailable"));
        let protected = create(
            "Protected",
            VaultKind::Text,
            true,
            json!("must never appear"),
        );
        let foreign = create(
            "Foreign",
            VaultKind::Text,
            false,
            json!("another installation"),
        );
        let secret = create(
            "Secret",
            VaultKind::ApiKey,
            true,
            json!("must never appear"),
        );
        vault
            .row_operation(|session, rows| {
                let entry = rows::parse_id(session.id, &foreign.id)?;
                let mut header = rows::header(rows, entry)?.unwrap();
                header.owner = Some(other.clone());
                rows::put_header(rows, &header)?;
                // Include stale/ineligible grants deliberately to test authorization
                // is checked at read time, not inferred from a record's existence.
                for (slot, entry) in [
                    (&text_slot, &text),
                    (&location_slot, &location),
                    (&protected_slot, &protected),
                    (&foreign_slot, &foreign),
                    (&secret_slot, &secret),
                ] {
                    rows::put_grant(
                        rows,
                        &Grant {
                            installation: caller.clone(),
                            slot: slot.clone(),
                            entry: rows::parse_id(session.id, &entry.id)?,
                            revoked: false,
                            last_read: None,
                            broker_revision: None,
                        },
                    )?;
                }
                Ok(())
            })
            .unwrap();
        let slots = [
            &text_slot,
            &location_slot,
            &secret_slot,
            &protected_slot,
            &foreign_slot,
        ];
        let render =
            |installation: &VaultInstallation, slot_list: &[&VaultSetupSlot], source: &str| {
                let mut result = None;
                vault
                    .format_granted_text(installation, slot_list, source, &mut |value| {
                        result = Some(value.to_owned());
                        true
                    })
                    .unwrap();
                result.unwrap()
            };
        let t = |entry: &VaultEntry| format!("${{vault.{}}}", entry.id);
        assert_eq!(
            render(
                &caller,
                &slots,
                &format!("Hi {} ${{unrelated.value}}", t(&text))
            ),
            "Hi demo ${unrelated.value}"
        );
        assert_eq!(render(&caller, &slots, &t(&location)), "12.3456,-78.9");
        assert_eq!(
            render(
                &caller,
                &slots,
                &format!(
                    "x ${{vault.{}.latitude}}/${{vault.{}.longitude}}/${{vault.{}.name}}",
                    location.id, location.id, location.id
                )
            ),
            "x 12.3456/-78.9/"
        );
        for denied in [&unassigned, &protected, &foreign, &secret] {
            assert_eq!(render(&caller, &slots, &format!("[{}]", t(denied))), "[]");
        }
        assert_eq!(render(&caller, &[], &t(&text)), "");
        assert_eq!(render(&other, &slots, &t(&text)), "");
        assert_eq!(
            render(&caller, &slots, "${vault.unlinked}/${vault.malformed}"),
            "/"
        );
        vault
            .row_operation(|_, rows| {
                let mut grant = rows::grant(rows, &caller, &text_slot)?.unwrap();
                assert!(grant.last_read.is_some());
                grant.revoked = true;
                rows::put_grant(rows, &grant)
            })
            .unwrap();
        assert_eq!(render(&caller, &slots, &t(&text)), "");
        vault.lock();
        assert_eq!(
            render(&caller, &slots, &format!("[{}]", t(&location))),
            "[]"
        );
    }
}
