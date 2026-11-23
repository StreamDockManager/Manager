//! Runtime's encrypted row schema. Values are never needed for metadata-only operations.
use super::*;
use stream_dock_manager_plugin_host::vault_references::ReferenceScope;
pub(super) use stream_dock_manager_storage::vault::rows::{RowKind, Rows};
use writes::{PrivateSlot, WriterGrant};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EntryHeader {
    pub id: uuid::Uuid,
    pub label: String,
    pub kind: VaultKind,
    pub protected: bool,
    pub revision: u64,
    pub owner: Option<VaultInstallation>,
    pub successor: Option<VaultInstallation>,
    pub private_slot: Option<PrivateSlot>,
}

impl Drop for EntryHeader {
    fn drop(&mut self) {
        self.label.zeroize();
    }
}

impl From<&Entry> for EntryHeader {
    fn from(entry: &Entry) -> Self {
        Self {
            id: entry.id,
            label: entry.label.clone(),
            kind: entry.kind,
            protected: entry.protected,
            revision: entry.revision,
            owner: entry.owner.clone(),
            successor: entry.successor.clone(),
            private_slot: entry.private_slot.clone(),
        }
    }
}

impl EntryHeader {
    fn into_entry(mut self, mut value: ValueBuffer) -> Entry {
        Entry {
            id: self.id,
            label: std::mem::take(&mut self.label),
            kind: self.kind,
            protected: self.protected,
            revision: self.revision,
            owner: self.owner.take(),
            successor: self.successor.take(),
            private_slot: self.private_slot.take(),
            value: value.0.take(),
        }
    }

    pub fn owned_by(&self, id: &VaultInstallation) -> bool {
        self.owner.as_ref() == Some(id) || self.successor.as_ref() == Some(id)
    }

    pub fn visible_to(&self, id: &VaultInstallation) -> bool {
        self.owner.is_none() || self.owned_by(id)
    }

    pub fn is_personal(&self) -> bool {
        self.owner.is_none() && self.private_slot.is_none() && self.successor.is_none()
    }

    pub fn allows_builtin(&self, path_field: bool) -> bool {
        self.owner.is_none()
            && if path_field {
                matches!(self.kind, VaultKind::File | VaultKind::Folder)
            } else {
                !self.protected
                    && matches!(
                        self.kind,
                        VaultKind::Text
                            | VaultKind::Boolean
                            | VaultKind::Integer
                            | VaultKind::Json
                            | VaultKind::Location
                            | VaultKind::File
                            | VaultKind::Folder
                    )
            }
    }

    pub fn metadata(&self, vault: uuid::Uuid) -> VaultEntry {
        VaultEntry {
            id: format!("{vault}.{}", self.id),
            label: self.label.clone(),
            kind: self.kind,
            protected: self.protected,
            revision: self.revision.to_string(),
            owner: self.owner.as_ref().map(|owner| owner.owner.clone()),
            owner_verified: None,
        }
    }

    pub fn advance(&mut self) -> Result<(), String> {
        self.revision = self.revision.checked_add(1).ok_or("vault_limit")?;
        Ok(())
    }
}

fn key(value: impl Serialize) -> String {
    let bytes = serde_json::to_vec(&value).expect("serializable fixed identity");
    hex::encode(Sha256::digest(&bytes))
}

fn owner_scope(owner: &VaultInstallation, action: &str) -> String {
    key((owner, action))
}

pub(super) fn parse_id(vault: uuid::Uuid, id: &str) -> Result<uuid::Uuid, String> {
    let (vault_id, entry_id) = id.split_once('.').ok_or("entry_missing")?;
    if vault_id != vault.hyphenated().to_string() {
        return Err("entry_missing".into());
    }
    uuid::Uuid::parse_str(entry_id).map_err(|_| "entry_missing".into())
}

pub(super) fn current_header(
    rows: &Rows<'_>,
    vault: uuid::Uuid,
    id: &str,
    revision: &str,
) -> Result<EntryHeader, String> {
    let row = header(rows, parse_id(vault, id)?)?.ok_or("entry_missing")?;
    if row.revision.to_string() != revision {
        return Err("stale_entry_revision".into());
    }
    Ok(row)
}

pub(super) fn header(rows: &Rows<'_>, id: uuid::Uuid) -> Result<Option<EntryHeader>, String> {
    let row = rows.get::<EntryHeader>(RowKind::Entry, &id.to_string())?;
    if row.as_ref().is_some_and(|row| row.id != id) {
        return Err("vault_recovery_required".into());
    }
    Ok(row)
}

pub(super) fn headers(rows: &Rows<'_>) -> Result<Vec<EntryHeader>, String> {
    rows.list::<EntryHeader>(RowKind::Entry, None)?
        .into_iter()
        .map(|(key, row)| {
            if key != row.id.to_string() {
                return Err("vault_recovery_required".into());
            }
            Ok(row)
        })
        .collect()
}

pub(super) fn value_exists(rows: &Rows<'_>, id: uuid::Uuid) -> Result<bool, String> {
    rows.contains(RowKind::EntryValue, &id.to_string())
}

pub(super) fn value(rows: &Rows<'_>, id: uuid::Uuid) -> Result<ValueBuffer, String> {
    rows.get(RowKind::EntryValue, &id.to_string())?
        .map(ValueBuffer)
        .ok_or_else(|| "vault_recovery_required".into())
}

pub(super) fn put_header(rows: &Rows<'_>, entry: &EntryHeader) -> Result<(), String> {
    rows.put(RowKind::Entry, &entry.id.to_string(), "", entry)
}

pub(super) fn put_value(rows: &Rows<'_>, id: uuid::Uuid, value: &Value) -> Result<(), String> {
    rows.put(RowKind::EntryValue, &id.to_string(), "", value)
}

pub(super) fn grant(
    rows: &Rows<'_>,
    owner: &VaultInstallation,
    slot: &VaultSetupSlot,
) -> Result<Option<Grant>, String> {
    let row = rows.get::<Grant>(RowKind::Grant, &key((owner, slot)))?;
    if row
        .as_ref()
        .is_some_and(|row| &row.installation != owner || &row.slot != slot)
    {
        return Err("vault_recovery_required".into());
    }
    Ok(row)
}

pub(super) fn grants_for(
    rows: &Rows<'_>,
    owner: &VaultInstallation,
    action: &str,
) -> Result<Vec<Grant>, String> {
    rows.list::<Grant>(RowKind::Grant, Some(&owner_scope(owner, action)))?
        .into_iter()
        .map(|(id, row)| {
            if &row.installation != owner || row.slot.action != action || id != grant_key(&row) {
                return Err("vault_recovery_required".into());
            }
            Ok(row)
        })
        .collect()
}

fn grant_key(row: &Grant) -> String {
    key((&row.installation, &row.slot))
}

pub(super) fn put_grant(rows: &Rows<'_>, row: &Grant) -> Result<(), String> {
    rows.put(
        RowKind::Grant,
        &grant_key(row),
        &owner_scope(&row.installation, &row.slot.action),
        row,
    )
}

pub(super) fn delete_grant(rows: &Rows<'_>, row: &Grant) -> Result<(), String> {
    rows.delete(RowKind::Grant, &grant_key(row)).map(|_| ())
}

fn writer_key(row: &WriterGrant) -> String {
    key((&row.owner, &row.declaration))
}

pub(super) fn writers_for(
    rows: &Rows<'_>,
    owner: &VaultInstallation,
    action: &str,
) -> Result<Vec<WriterGrant>, String> {
    rows.list::<WriterGrant>(RowKind::Writer, Some(&owner_scope(owner, action)))?
        .into_iter()
        .map(|(id, row)| {
            if &row.owner != owner || row.declaration.action != action || id != writer_key(&row) {
                return Err("vault_recovery_required".into());
            }
            Ok(row)
        })
        .collect()
}

pub(super) fn writer(
    rows: &Rows<'_>,
    owner: &VaultInstallation,
    scope: &stream_dock_manager_plugin_host::permissions::ActionScope,
) -> Result<Option<WriterGrant>, String> {
    let row = rows.get::<WriterGrant>(RowKind::Writer, &key((owner, scope)))?;
    if row
        .as_ref()
        .is_some_and(|row| &row.owner != owner || &row.declaration != scope)
    {
        return Err("vault_recovery_required".into());
    }
    Ok(row)
}

pub(super) fn put_writer(rows: &Rows<'_>, row: &WriterGrant) -> Result<(), String> {
    rows.put(
        RowKind::Writer,
        &writer_key(row),
        &owner_scope(&row.owner, &row.declaration.action),
        row,
    )
}

fn reference_key(row: &references::ReferenceGrant) -> String {
    key((&row.scope, row.entry))
}

pub(super) fn references_for(
    rows: &Rows<'_>,
    owner: &VaultInstallation,
    action: &str,
) -> Result<Vec<references::ReferenceGrant>, String> {
    rows.list::<references::ReferenceGrant>(RowKind::Reference, Some(&owner_scope(owner, action)))?
        .into_iter()
        .map(|(id, row)| {
            if &row.scope.owner != owner || row.scope.action != action || id != reference_key(&row)
            {
                return Err("vault_recovery_required".into());
            }
            Ok(row)
        })
        .collect()
}

pub(super) fn reference_exists(
    rows: &Rows<'_>,
    scope: &ReferenceScope,
    entry: uuid::Uuid,
) -> Result<bool, String> {
    match rows.get::<references::ReferenceGrant>(RowKind::Reference, &key((scope, entry)))? {
        Some(row) if &row.scope == scope && row.entry == entry => Ok(true),
        Some(_) => Err("vault_recovery_required".into()),
        None => Ok(false),
    }
}

pub(super) fn put_ref(rows: &Rows<'_>, row: &references::ReferenceGrant) -> Result<(), String> {
    rows.put(
        RowKind::Reference,
        &reference_key(row),
        &owner_scope(&row.scope.owner, &row.scope.action),
        row,
    )
}

pub(super) fn delete_ref(rows: &Rows<'_>, row: &references::ReferenceGrant) -> Result<(), String> {
    rows.delete(RowKind::Reference, &reference_key(row))
        .map(|_| ())
}

pub(super) fn bump(rows: &Rows<'_>) -> Result<(), String> {
    rows.set_revision(rows.revision()?.checked_add(1).ok_or("vault_limit")?)
}

// Complex ownership transitions and transfers get a bounded typed view
pub(super) fn read_document(db: &EncryptedVault) -> Result<Document, String> {
    db.rows(|rows| {
        let entry_rows = headers(rows)?;
        if entry_rows.len() != rows.count(RowKind::EntryValue)? {
            return Err("vault_recovery_required".into());
        }
        let entries = entry_rows
            .into_iter()
            .map(|row| {
                let value = value(rows, row.id)?;
                row.kind
                    .validate(&value.0)
                    .map_err(|_| "vault_recovery_required")?;
                Ok(row.into_entry(value))
            })
            .collect::<Result<_, String>>()?;
        Ok(Document {
            entries,
            grants: all(rows, RowKind::Grant, grant_key)?,
            references: all(rows, RowKind::Reference, reference_key)?,
            writers: all(rows, RowKind::Writer, writer_key)?,
            revision: rows.revision()?,
        })
    })
}

pub(super) fn all<T: serde::de::DeserializeOwned>(
    rows: &Rows<'_>,
    kind: RowKind,
    key_for: impl Fn(&T) -> String,
) -> Result<Vec<T>, String> {
    rows.list::<T>(kind, None)?
        .into_iter()
        .map(|(id, row)| {
            if id != key_for(&row) {
                return Err("vault_recovery_required".into());
            }
            Ok(row)
        })
        .collect()
}

pub(super) fn all_grants(rows: &Rows<'_>) -> Result<Vec<Grant>, String> {
    all(rows, RowKind::Grant, grant_key)
}

pub(super) fn all_writers(rows: &Rows<'_>) -> Result<Vec<WriterGrant>, String> {
    all(rows, RowKind::Writer, writer_key)
}

pub(super) fn all_refs(rows: &Rows<'_>) -> Result<Vec<references::ReferenceGrant>, String> {
    all(rows, RowKind::Reference, reference_key)
}

// Bulk publication preserves unchanged row ciphertext and commits every change together
pub(super) fn publish_document(rows: &Rows<'_>, data: &Document) -> Result<(), String> {
    let ids: BTreeSet<_> = data.entries.iter().map(|e| e.id).collect();
    let previous = headers(rows)?;
    for row in &previous {
        if !ids.contains(&row.id) {
            rows.delete(RowKind::Entry, &row.id.to_string())?;
            rows.delete(RowKind::EntryValue, &row.id.to_string())?;
        }
    }
    let previous: std::collections::BTreeMap<_, _> =
        previous.into_iter().map(|row| (row.id, row)).collect();
    for entry in &data.entries {
        let current = EntryHeader::from(entry);
        let original = previous.get(&entry.id);
        if original != Some(&current) {
            put_header(rows, &current)?;
        }
        if original.is_none()
            || original.is_some_and(|before| before.revision != entry.revision)
                && value(rows, entry.id)?.0 != entry.value
        {
            put_value(rows, entry.id, &entry.value)?;
        }
    }
    publish_group(rows, RowKind::Grant, &data.grants, grant_key, put_grant)?;
    publish_group(rows, RowKind::Writer, &data.writers, writer_key, put_writer)?;
    publish_group(
        rows,
        RowKind::Reference,
        &data.references,
        reference_key,
        put_ref,
    )?;
    rows.set_revision(data.revision)
}

fn publish_group<T: serde::de::DeserializeOwned + Serialize>(
    rows: &Rows<'_>,
    kind: RowKind,
    current: &[T],
    key_for: impl Fn(&T) -> String,
    put: impl Fn(&Rows<'_>, &T) -> Result<(), String>,
) -> Result<(), String> {
    let old: std::collections::BTreeMap<_, _> = rows.list::<T>(kind, None)?.into_iter().collect();
    let mut next = std::collections::BTreeMap::new();
    for item in current {
        // Setup replay replaces the same logical grant rather than adding an indistinguishable copy
        next.insert(key_for(item), item);
    }
    for id in old.keys().filter(|id| !next.contains_key(*id)) {
        rows.delete(kind, id)?;
    }
    for (key, item) in next {
        let unchanged = if let Some(previous) = old.get(&key) {
            serde_json::to_vec(previous).map_err(|_| "vault_failure")?
                == serde_json::to_vec(item).map_err(|_| "vault_failure")?
        } else {
            false
        };
        if !unchanged {
            put(rows, item)?;
        }
    }
    Ok(())
}
