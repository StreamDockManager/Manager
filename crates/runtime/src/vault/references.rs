use super::{rows::*, *};
use stream_dock_manager_plugin_host::{permissions::PermissionScope, vault_references::*};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReferenceGrant {
    pub(super) scope: ReferenceScope,
    pub(super) entry: uuid::Uuid,
}

fn matches_reference(scope: &ReferenceScope, entry: &EntryHeader) -> bool {
    if !entry.visible_to(&scope.owner) {
        return false;
    }
    match &scope.declaration {
        PermissionScope::Access {
            recipe, operation, ..
        } if recipe == "clipboard-summary" && operation == "authorize-request" => {
            matches!(entry.kind, VaultKind::ApiKey | VaultKind::BearerToken)
        }
        _ => false,
    }
}
fn selected_name(session: &Session, entry: &EntryHeader) -> EntryName {
    EntryName {
        id: format!("{}.{}", session.id, entry.id),
        name: entry.label.clone(),
    }
}

impl Vault {
    // Never wait for a package install that may be retiring a host holding its own fence
    fn reference_operation<T>(
        &self,
        changed: bool,
        f: impl FnOnce(&Session, &Rows<'_>) -> Result<T, String>,
    ) -> Result<T, &'static str> {
        let mut state = self.state.try_lock().map_err(|_| "busy")?;
        self.expire_session(&mut state);
        let session = state.session.as_ref().ok_or("vault_locked")?;
        let result = session
            .vault
            .rows(|rows| f(session, rows))
            .map_err(|error| match error.as_str() {
                "stale_revision" => "stale_revision",
                "invalid_request" => "invalid_request",
                "permission_denied" => "permission_denied",
                "cancelled" => "cancelled",
                "quota" => "quota",
                _ => "operation_unavailable",
            })?;
        if changed {
            session.changed();
        }
        Ok(result)
    }
}
impl References for Vault {
    fn approve(
        &self,
        expected: ReviewVersion,
        commit: &mut dyn FnMut() -> Result<u64, String>,
    ) -> Result<u64, String> {
        self.reference_operation(false, |s, rows| {
            if s.epoch != expected.epoch || rows.revision()? != expected.revision {
                return Err("stale_revision".into());
            }
            commit().map_err(|e| {
                if e == "stale_revision" {
                    e
                } else {
                    "operation_unavailable".into()
                }
            })
        })
        .map_err(str::to_owned)
    }

    fn secret_grant(&self, scope: &ReferenceScope) -> (bool, bool) {
        self.reference_operation(false, |_, rows| {
            let allowed = match &scope.declaration {
                PermissionScope::ReadSecret { .. } | PermissionScope::ReadLocation { .. } => {
                    for grant in grants_for(rows, &scope.owner, &scope.action)? {
                        if !grant.revoked
                            && grant.installation == scope.owner
                            && grant.slot.action == scope.action
                            && scope.declaration.matches_direct_slot(&grant.slot)
                            && header(rows, grant.entry)?.is_some_and(|e| {
                                e.visible_to(&scope.owner)
                                    && eligible_kind(e.kind, e.protected, &grant.slot)
                            })
                            && rows.contains(RowKind::EntryValue, &grant.entry.to_string())?
                        {
                            return Ok((true, true));
                        }
                    }
                    false
                }
                PermissionScope::WriteSecret { .. } => {
                    writers_for(rows, &scope.owner, &scope.action)?
                        .iter()
                        .any(|w| {
                            !w.revoked
                                && w.owner == scope.owner
                                && w.declaration.action == scope.action
                                && w.declaration.scope == scope.declaration
                        })
                }
                _ => false,
            };
            Ok((allowed, true))
        })
        .unwrap_or((false, false))
    }

    fn resolve(&self, ticket: &RecipeEntry) -> Result<RecipeValue, &'static str> {
        self.reference_operation(false, |s, rows| {
            let scope = &ticket.scope;
            if s.epoch != ticket.version.epoch || rows.revision()? != ticket.version.revision {
                return Err("stale_revision".into());
            }
            if !matches!(&scope.declaration, PermissionScope::Access{mode,..} if mode=="use-entry")
            {
                return Err("permission_denied".into());
            }
            let id = if let Some(slot) = ticket.id.strip_prefix("slot:") {
                grants_for(rows, &scope.owner, &scope.action)?
                    .iter()
                    .find(|g| {
                        !g.revoked
                            && g.installation == scope.owner
                            && g.slot.id == slot
                            && g.slot.action == scope.action
                            && g.slot.purpose == scope.purpose
                            && g.slot.broker_scope().as_ref() == Some(&scope.declaration)
                            && g.broker_revision == Some(scope.permission_revision)
                    })
                    .map(|g| g.entry)
                    .ok_or("permission_denied")?
            } else {
                let reference = TypedReference::parse(&ticket.id).map_err(|_| "invalid_request")?;
                if reference.field.is_some()
                    || reference.vault != s.id
                    || !reference_exists(rows, scope, reference.entry)?
                {
                    return Err("permission_denied".into());
                }
                reference.entry
            };
            let entry = header(rows, id)?
                .filter(|entry| matches_reference(scope, entry))
                .ok_or("permission_denied")?;
            let secret = value(rows, entry.id)?;
            let value = secret
                .0
                .as_str()
                .filter(|v| {
                    v.len() <= 4096 && !v.is_empty() && v.bytes().all(|b| (33..=126).contains(&b))
                })
                .ok_or("invalid_request")?;
            Ok(RecipeValue::Authorization(Zeroizing::new(value.into())))
        })
    }

    fn version(&self) -> Result<ReviewVersion, &'static str> {
        self.reference_operation(false, |s, rows| {
            Ok(ReviewVersion {
                epoch: s.epoch,
                revision: rows.revision()?,
            })
        })
    }

    fn entries(
        &self,
        scope: &ReferenceScope,
        approved_only: bool,
        start: usize,
        version: Option<ReviewVersion>,
    ) -> Result<EntryPage, &'static str> {
        if start > MAX_ENTRIES {
            return Err("invalid_request");
        }
        self.reference_operation(false, |s, rows| {
            let revision = rows.revision()?;
            if version.is_some_and(|v| v.epoch != s.epoch || v.revision != revision) {
                return Err("stale_revision".into());
            }
            let allowed = if approved_only {
                let ids: std::collections::HashSet<_> =
                    references_for(rows, &scope.owner, &scope.action)?
                        .iter()
                        .filter(|g| g.scope == *scope)
                        .map(|g| g.entry)
                        .collect();
                let mut matching = Vec::new();
                for id in ids {
                    if let Some(row) = header(rows, id)? {
                        matching.push(row);
                    }
                }
                matching.sort_unstable_by_key(|row| row.id);
                matching
            } else {
                let mut matching = headers(rows)?;
                matching.sort_unstable_by_key(|row| row.id);
                matching
            };
            let mut entries: Vec<_> = allowed
                .iter()
                .filter(|e| matches_reference(scope, e))
                .skip(start)
                .take(33)
                .map(|e| selected_name(s, e))
                .collect();
            let next = (entries.len() > 32).then_some(start + 32);
            entries.truncate(32);
            Ok(EntryPage { entries, next })
        })
    }

    fn select(
        &self,
        scope: &ReferenceScope,
        id: &str,
        version: ReviewVersion,
        live: &dyn Fn() -> bool,
    ) -> Result<EntryName, &'static str> {
        self.reference_operation(true, |s, rows| {
            if !live() {
                return Err("cancelled".into());
            }
            if s.epoch != version.epoch || rows.revision()? != version.revision {
                return Err("stale_revision".into());
            }
            let reference = TypedReference::parse(id).map_err(|_| "permission_denied")?;
            if reference.vault != s.id || reference.field.is_some() {
                return Err("permission_denied".into());
            }
            let entry = header(rows, reference.entry)?
                .filter(|e| matches_reference(scope, e) && format!("{}.{}", s.id, e.id) == id)
                .ok_or("permission_denied")?;
            let chosen = selected_name(s, &entry);
            for grant in references_for(rows, &scope.owner, &scope.action)? {
                if grant.scope.owner == scope.owner
                    && grant.scope.action == scope.action
                    && grant.scope.declaration == scope.declaration
                    && (grant.scope.permission_revision != scope.permission_revision
                        || grant.entry == entry.id)
                {
                    delete_ref(rows, &grant)?;
                }
            }
            if rows.count(RowKind::Reference)? >= MAX_GRANTS {
                return Err("quota".into());
            }
            put_ref(
                rows,
                &ReferenceGrant {
                    scope: scope.clone(),
                    entry: entry.id,
                },
            )?;
            if !live() {
                return Err("cancelled".into());
            }
            bump(rows)?;
            Ok(chosen)
        })
    }
}

impl ReferenceGrant {
    pub(super) fn selector(&self) -> String {
        // A slash cannot occur in the existing setup-slot identifier grammar.
        format!(
            "reference/{}",
            serde_json::to_string(&self.scope.declaration).expect("serializable declaration")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn installed_broker_slot_and_reference_follow_only_verified_reviewed_updates() {
        use stream_dock_manager_plugin_host::permissions::ActionScope;
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("state")).unwrap();
        let vault = Vault::new(
            root.path().into(),
            Arc::new(super::super::tests::TestKeys::default()),
            store.clone(),
        );
        vault.initialize().unwrap();
        let old = VaultInstallation {
            owner: "fixture".into(),
            token: "first".into(),
            inventory_sha256: "first-hash".into(),
            storage_generation: store.stats().unwrap().storage_generation,
        };
        let next = VaultInstallation {
            token: "second".into(),
            inventory_sha256: "next-hash".into(),
            ..old.clone()
        };
        let key = vault
            .create_entry(VaultEntryInput {
                label: "Fixture".into(),
                kind: VaultKind::ApiKey,
                protected: true,
                value: json!("fixture-credential"),
            })
            .unwrap();
        let slot:VaultSetupSlot=serde_json::from_value(json!({"id":"key","kind":"secret","entry_kind":"api-key","mode":"broker","required":true,"purpose":"Authorize fixture","field":"key","action":"fixture.action","origin":"https://api.example.test","recipe":"clipboard-summary","operation":"authorize-request"})).unwrap();
        let scope = slot.broker_scope().unwrap();
        let change = vec![(
            slot.action.clone(),
            serde_json::to_string(&scope).unwrap(),
            true,
        )];
        let old_revision = store
            .set_permission_decisions(old.permission_owner(), 0, change.clone())
            .unwrap();
        let declared = ActionScope {
            action: slot.action.clone(),
            scope: scope.clone(),
            required: true,
        };
        let selector = |owner: &VaultInstallation, revision| ReferenceScope {
            owner: owner.clone(),
            purpose: slot.purpose.clone(),
            action: slot.action.clone(),
            declaration: scope.clone(),
            permission_revision: revision,
        };
        let (_, assignment) = vault
            .setup_selection(&slot, &key.id, &key.revision, false)
            .unwrap();
        let selected = [((slot.action.clone(), slot.id.clone()), assignment)].into();
        vault
            .grant_setup(&old, std::slice::from_ref(&slot), &selected, || Ok(()))
            .unwrap();
        let resolve = |owner: &VaultInstallation, revision, id: &str| {
            vault.resolve(&RecipeEntry {
                scope: selector(owner, revision),
                id: id.into(),
                version: vault.version().unwrap(),
            })
        };
        assert!(matches!(
            resolve(&old, old_revision, "slot:key"),
            Ok(RecipeValue::Authorization(_))
        ));
        assert!(resolve(&old, old_revision, &key.id).is_err());
        assert!(vault
            .read_and_publish(&old, &slot, &mut |_, _| panic!(
                "disclosed a brokered credential"
            ))
            .is_err());
        vault
            .select(
                &selector(&old, old_revision),
                &key.id,
                vault.version().unwrap(),
                &|| true,
            )
            .unwrap();
        let review = || {
            vault
                .review_continuity(
                    old.clone(),
                    std::slice::from_ref(&slot),
                    std::slice::from_ref(&declared),
                    Some(old_revision),
                )
                .unwrap()
        };
        let upgrade = |review: &Continuity,
                       commit: Box<dyn FnOnce() -> Result<(), String> + '_>| {
            vault.grant_setup_with_writers(
                &next,
                std::slice::from_ref(&slot),
                &Default::default(),
                &[],
                None,
                Some(review),
                commit,
            )
        };
        assert!(upgrade(&review(), Box::new(|| Err("fixture rollback".into()))).is_err());
        assert!(resolve(&old, old_revision, "slot:key").is_ok());
        assert!(resolve(&next, old_revision, "slot:key").is_err());
        upgrade(
            &review(),
            Box::new(|| {
                store
                    .set_permission_decisions(next.permission_owner(), 0, change.clone())
                    .map(|_| ())
            }),
        )
        .unwrap();
        let rev = store
            .permission_decisions(next.permission_owner())
            .unwrap()
            .revision;
        assert!(resolve(&old, old_revision, &key.id).is_err());
        assert!(resolve(&next, rev, &key.id).is_ok());
        assert!(resolve(&next, rev, "slot:key").is_ok());
        let changed = store
            .set_permission_decisions(next.permission_owner(), rev, change)
            .unwrap();
        assert!(resolve(&next, changed, "slot:key").is_err());
    }

    #[test]
    fn selection_separates_metadata_use_rotation_and_revocation() {
        let root = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            root.path().into(),
            Arc::new(super::super::tests::TestKeys::default()),
            Storage::open(root.path().join("state")).unwrap(),
        );
        vault.initialize().unwrap();
        let location = vault
            .create_entry(VaultEntryInput {
                label: "Synthetic credential".into(),
                kind: VaultKind::ApiKey,
                protected: true,
                value: json!("synthetic-key"),
            })
            .unwrap();
        let owner = VaultInstallation {
            owner: "fixture".into(),
            token: "one".into(),
            inventory_sha256: "hash".into(),
            storage_generation: "epoch".into(),
        };
        let scope = |mode: &str| ReferenceScope {
            owner: owner.clone(),
            action: "fixture.summary".into(),
            purpose: "Summarize".into(),
            permission_revision: 3,
            declaration: PermissionScope::Access {
                mode: mode.into(),
                field: "credential".into(),
                recipe: "clipboard-summary".into(),
                operation: "authorize-request".into(),
                origin: "https://api.example.test".into(),
            },
        };
        let earlier = vault.version().unwrap();
        vault.lock();
        assert!(vault
            .approve(earlier, &mut || panic!(
                "published an approval while locked"
            ))
            .is_err());
        vault.unlock().unwrap();
        assert!(vault
            .approve(earlier, &mut || panic!(
                "published approval from the previous session"
            ))
            .is_err());
        assert_eq!(
            vault
                .approve(vault.version().unwrap(), &mut || Ok(7))
                .unwrap(),
            7
        );
        let browse = scope("browse-metadata");
        let use_entry = scope("use-entry");
        let version = || vault.version().unwrap();
        let list = |s: &ReferenceScope| {
            References::entries(&vault, s, true, 0, None)
                .unwrap()
                .entries
        };
        assert!(list(&browse).is_empty());
        let old = version();
        vault.select(&browse, &location.id, old, &|| true).unwrap();
        assert_eq!(list(&browse)[0].name, "Synthetic credential");
        assert!(list(&use_entry).is_empty());
        assert!(vault
            .select(&use_entry, &location.id, old, &|| true)
            .is_err());
        let ticket = |scope: ReferenceScope| RecipeEntry {
            scope,
            id: location.id.clone(),
            version: version(),
        };
        assert!(vault.resolve(&ticket(browse.clone())).is_err());
        assert!(vault.resolve(&ticket(use_entry.clone())).is_err());
        vault
            .select(&use_entry, &location.id, version(), &|| true)
            .unwrap();
        let authorized = ticket(use_entry.clone());
        let RecipeValue::Authorization(key) = vault.resolve(&authorized).unwrap();
        assert_eq!(key.as_str(), "synthetic-key");
        let mut foreign = use_entry;
        foreign.owner.token = "reinstalled".into();
        assert!(vault.resolve(&ticket(foreign)).is_err());
        let access = vault.access(std::slice::from_ref(&owner)).unwrap();
        let g = access
            .iter()
            .find(|g| g.description.as_ref().unwrap().contains("use-entry"))
            .unwrap();
        vault
            .revoke(
                &owner,
                &g.slot_id,
                &g.action,
                &g.entry.id,
                &g.entry.revision,
            )
            .unwrap();
        assert!(vault.resolve(&authorized).is_err());
        vault.lock();
        assert!(References::entries(&vault, &browse, true, 0, None).is_err());
    }
}
