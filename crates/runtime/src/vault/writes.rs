use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use stream_dock_manager_plugin_host::{
    permissions::{ActionScope, PermissionScope},
    vault_writes::SecretWrite,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WriterGrant {
    pub owner: VaultInstallation,
    pub declaration: ActionScope,
    pub revoked: bool,
    pub last_write: Option<String>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrivateSlot {
    pub name: String,
    pub field: String,
    pub action: String,
}

impl Vault {
    pub(super) fn private_write(
        &self,
        owner: &VaultInstallation,
        scope: &ActionScope,
        request: &SecretWrite,
        alive: &AtomicBool,
    ) -> Result<(String, String), &'static str> {
        let run = || {
            let state = self.current_state();
            let session = state.session.as_ref().ok_or("vault_locked")?;
            session.mutate(|rows| {
                let mut writer = rows::writer(rows, owner, scope)?
                    .filter(|grant| !grant.revoked)
                    .ok_or("permission_denied")?;
                let PermissionScope::WriteSecret {
                    field, entry_kind, ..
                } = &scope.scope
                else {
                    return Err("not_declared".into());
                };
                let kind = match entry_kind.as_str() {
                    "api-key" => VaultKind::ApiKey,
                    "bearer-token" => VaultKind::BearerToken,
                    "credential" => VaultKind::Credential,
                    _ => return Err("not_declared".into()),
                };
                if request.field != *field || &request.entry_kind != entry_kind {
                    return Err("not_declared".into());
                }
                let value = ValueBuffer(Value::String(request.value.clone()));
                kind.validate(&value.0)?;
                let existing = rows::headers(rows)?.into_iter().find(|e| {
                    e.owned_by(owner)
                        && e.private_slot
                            .as_ref()
                            .is_some_and(|s| s.name == request.slot)
                });
                let entry = match (request.operation.as_str(), existing) {
                    ("create", None) if request.expected_revision.is_none() => {
                        if rows.count(RowKind::Entry)? >= MAX_ENTRIES {
                            return Err("vault_limit".into());
                        }
                        rows::EntryHeader {
                            id: uuid::Uuid::new_v4(),
                            owner: Some(owner.clone()),
                            successor: None,
                            private_slot: Some(PrivateSlot {
                                name: request.slot.clone(),
                                field: field.clone(),
                                action: scope.action.clone(),
                            }),
                            kind,
                            protected: true,
                            label: request.slot.clone(),
                            revision: 1,
                        }
                    }
                    ("create", Some(_)) => return Err("slot_conflict".into()),
                    ("rotate", Some(mut entry))
                        if entry.kind == kind
                            && entry
                                .private_slot
                                .as_ref()
                                .is_some_and(|s| s.action == scope.action && &s.field == field) =>
                    {
                        if request.expected_revision.as_deref() != Some(&entry.revision.to_string())
                        {
                            return Err("stale_revision".into());
                        }
                        entry.advance()?;
                        entry
                    }
                    _ => return Err("permission_denied".into()),
                };
                if !alive.load(Ordering::Acquire) {
                    return Err("cancelled".into());
                }
                writer.last_write = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                        .to_string(),
                );
                rows::put_header(rows, &entry)?;
                rows::put_value(rows, entry.id, &value.0)?;
                rows::put_writer(rows, &writer)?;
                Ok((
                    format!("{}.{}", session.id, entry.id),
                    entry.revision.to_string(),
                ))
            })
        };
        run().map_err(|e| match e.as_str() {
            "vault_locked" => "vault_locked",
            "not_declared" => "not_declared",
            "permission_denied" => "permission_denied",
            "stale_revision" => "stale_revision",
            "slot_conflict" => "slot_conflict",
            "cancelled" => "cancelled",
            "vault_limit" => "quota",
            "invalid_value" => "invalid_request",
            _ => "operation_unavailable",
        })
    }
}
