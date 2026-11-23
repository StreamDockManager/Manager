//! Encrypted, revision-bound preparation for a verified catalog upgrade.
use super::*;
use stream_dock_manager_plugin_host::permissions::ActionScope;

pub(crate) struct Continuity {
    pub prior: VaultInstallation,
    pub epoch: uuid::Uuid,
    pub revision: u64,
    pub slots: Vec<VaultSetupSlot>,
    pub writers: Vec<ActionScope>,
    pub references: Vec<(
        stream_dock_manager_plugin_host::vault_references::ReferenceScope,
        uuid::Uuid,
    )>,
}
impl Vault {
    pub(crate) fn review_continuity(
        &self,
        prior: VaultInstallation,
        slots: &[VaultSetupSlot],
        unchanged: &[ActionScope],
        permission_revision: Option<u64>,
    ) -> Result<Continuity, String> {
        self.operation(|session, data| {
            Ok(Continuity {
                slots: slots
                    .iter()
                    .filter(|slot| {
                        data.grants.iter().any(|g| {
                            !g.revoked
                                && g.installation == prior
                                && g.slot == **slot
                                && (slot.mode != "broker"
                                    || (g.broker_revision.is_some()
                                        && g.broker_revision == permission_revision))
                                && data.entries.iter().any(|e| {
                                    e.id == g.entry && e.visible_to(&prior) && eligible(e, slot)
                                })
                        })
                    })
                    .cloned()
                    .collect(),
                writers: data
                    .writers
                    .iter()
                    .filter(|w| {
                        w.owner == prior && !w.revoked && unchanged.contains(&w.declaration)
                    })
                    .map(|w| w.declaration.clone())
                    .collect(),
                references: data
                    .references
                    .iter()
                    .filter(|g| {
                        g.scope.owner == prior
                            && Some(g.scope.permission_revision) == permission_revision
                            && unchanged.iter().any(|i| {
                                i.action == g.scope.action && i.scope == g.scope.declaration
                            })
                    })
                    .map(|g| (g.scope.clone(), g.entry))
                    .collect(),
                prior,
                epoch: session.epoch,
                revision: data.revision,
            })
        })
    }
}
impl Entry {
    pub(super) fn owned_by(&self, id: &VaultInstallation) -> bool {
        self.owner.as_ref() == Some(id) || self.successor.as_ref() == Some(id)
    }
    pub(super) fn visible_to(&self, id: &VaultInstallation) -> bool {
        self.owner.is_none() || self.owned_by(id)
    }
}
impl Continuity {
    pub(super) fn transfer_references(
        &self,
        store: &Storage,
        d: &mut Document,
        next: &VaultInstallation,
    ) {
        if self.references.is_empty() {
            return;
        }
        let Ok(decisions) =
            store.permission_decisions(stream_dock_manager_storage::permissions::PermissionOwner {
                plugin: next.owner.clone(),
                installation: next.token.clone(),
                inventory: next.inventory_sha256.clone(),
                generation: next.storage_generation.clone(),
            })
        else {
            return;
        };
        for (old, entry) in &self.references {
            let key = (
                old.action.clone(),
                serde_json::to_string(&old.declaration).expect("serializable scope"),
            );
            if decisions.scopes.get(&key) == Some(&true) {
                let mut scope = old.clone();
                scope.owner = next.clone();
                scope.permission_revision = decisions.revision;
                d.references.push(references::ReferenceGrant {
                    scope,
                    entry: *entry,
                });
            }
        }
    }
    pub(super) fn prepare(
        &self,
        s: &Session,
        d: &mut Document,
        next: &VaultInstallation,
        selected: &std::collections::BTreeMap<(String, String), VaultAssignment>,
    ) -> Result<(), String> {
        if self.epoch != s.epoch
            || self.revision != d.revision
            || self.prior.owner != next.owner
            || self.prior.storage_generation != next.storage_generation
            || self.prior.token == next.token
        {
            return Err("vault_setup_expired".into());
        }
        let mut grants = Vec::new();
        for old in &d.grants {
            if !old.revoked
                && old.installation == self.prior
                && self.slots.contains(&old.slot)
                && !selected.contains_key(&(old.slot.action.clone(), old.slot.id.clone()))
            {
                grants.push(Grant {
                    broker_revision: None,
                    revoked: false,
                    last_read: old.last_read.clone(),
                    installation: next.clone(),
                    slot: old.slot.clone(),
                    entry: old.entry,
                })
            }
        }
        if d.grants.len() + grants.len() > MAX_GRANTS {
            return Err("vault_limit".into());
        }
        d.grants.extend(grants);
        for e in &mut d.entries {
            if e.owned_by(&self.prior) {
                e.owner = Some(self.prior.clone());
                e.successor = Some(next.clone());
            }
        }
        Ok(())
    }
}
