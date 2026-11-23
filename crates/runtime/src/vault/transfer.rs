//! Explicit native import and export for personal entries, separate from configuration backups
use super::*;
use std::{
    fs::File,
    io::{Cursor, Read, Seek, Write},
    path::Path,
    time::SystemTime,
};
use vault::transfer::{self as format, Kind};

const MAX_FILE: usize = MAX_ENTRIES * (MAX_VALUE + 1024);
pub const VAULT_TRANSFER_MAX_FILE_BYTES: usize = MAX_FILE + 1024;
pub const VAULT_TRANSFER_MAX_PASSWORD_BYTES: usize = vault::password::MAX_PASSWORD_BYTES;
const REVIEW_TTL: Duration = Duration::from_secs(5 * 60);
const EXPIRED: &str = "vault_import_expired";
const INVALID: &str = "vault_transfer_invalid";

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct VaultExportSelection {
    pub id: String,
    pub revision: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VaultImportStatus {
    New,
    Conflict,
    Ambiguous,
}

#[derive(Clone, Serialize, ts_rs::TS)]
pub struct VaultImportRow {
    pub row: u32,
    pub label: String,
    pub kind: VaultKind,
    pub protected: bool,
    pub status: VaultImportStatus,
    pub existing: Option<VaultEntry>,
}

#[derive(Serialize, ts_rs::TS)]
pub struct VaultImportReview {
    pub id: String,
    pub encrypted: bool,
    pub rows: Vec<VaultImportRow>,
}

#[derive(Default, Serialize, ts_rs::TS)]
pub struct VaultImportResult {
    pub created: u32,
    pub overwritten: u32,
    pub skipped: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transfer {
    format: String,
    version: u32,
    #[serde(deserialize_with = "bounded_entries")]
    entries: Vec<TransferEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(from = "TransferEntryWire")]
struct TransferEntry {
    label: String,
    kind: VaultKind,
    protected: bool,
    value: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferEntryWire {
    label: String,
    kind: VaultKind,
    #[serde(default, deserialize_with = "read_protection")]
    protected: Option<bool>,
    value: Value,
}

// Distinguish a missing legacy choice from an explicit boolean, without accepting null
fn read_protection<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Option<bool>, D::Error> {
    bool::deserialize(decoder).map(Some)
}

impl From<TransferEntryWire> for TransferEntry {
    fn from(wire: TransferEntryWire) -> Self {
        Self {
            label: wire.label,
            kind: wire.kind,
            protected: wire
                .protected
                .unwrap_or_else(|| wire.kind.default_protected()),
            value: wire.value,
        }
    }
}

// Bound row allocation while decoding rather than after an adversarial array is read
fn bounded_entries<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<Vec<TransferEntry>, D::Error> {
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = Vec<TransferEntry>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a bounded list of personal entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut entries = Vec::new();
            while let Some(entry) = sequence.next_element()? {
                if entries.len() == MAX_ENTRIES {
                    return Err(serde::de::Error::custom(INVALID));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    decoder.deserialize_seq(Entries)
}

impl Drop for TransferEntry {
    fn drop(&mut self) {
        self.label.zeroize();
        clear_value(&mut self.value);
    }
}

pub(super) struct StagedImport {
    id: String,
    editor: u64,
    revision: u64,
    started: Instant,
    wall_started: SystemTime,
    entries: Vec<TransferEntry>,
    rows: Vec<VaultImportRow>,
}

impl StagedImport {
    fn remaining(&self) -> Duration {
        let wall = self.wall_started.elapsed().unwrap_or(REVIEW_TTL);
        REVIEW_TTL.saturating_sub(self.started.elapsed().max(wall))
    }
}

impl Session {
    pub(super) fn expire_entry_import(&self) {
        if self
            .entry_import_remaining()
            .is_some_and(|time| time.is_zero())
        {
            self.entry_import.take();
        }
    }

    pub(super) fn entry_import_remaining(&self) -> Option<Duration> {
        self.entry_import
            .borrow()
            .as_ref()
            .map(StagedImport::remaining)
    }
}

fn personal(entry: &Entry) -> bool {
    entry.is_personal()
}

fn label_key(label: &str) -> String {
    label.trim().to_lowercase()
}

fn require_external(root: &Path, path: &Path) -> Result<(), String> {
    vault::check_transfer_path(root, path)?;
    let root = root.canonicalize().map_err(|_| "vault_unavailable")?;
    if path.starts_with(root) {
        return Err("vault_transfer_inside_app_data".into());
    }
    Ok(())
}

fn export_entries_document(
    session: &Session,
    selected: &[VaultExportSelection],
) -> Result<Transfer, String> {
    session.vault.rows(|rows| {
        let mut ids = BTreeSet::new();
        let mut entries = Vec::new();
        for selection in selected {
            if selection.id.len() != 73 || selection.revision.len() > 20 {
                return Err("entry_missing".into());
            }
            let entry = rows::current_header(rows, session.id, &selection.id, &selection.revision)?;
            if !entry.is_personal() || !ids.insert(entry.id) {
                return Err("permission_denied".into());
            }
            let mut value = rows::value(rows, entry.id)?;
            entry.kind.validate(&value.0)?;
            entries.push(TransferEntry {
                label: entry.label.clone(),
                kind: entry.kind,
                protected: entry.protected,
                value: value.0.take(),
            });
        }
        Ok(Transfer {
            format: "sdm-vault-entries".into(),
            version: 1,
            entries,
        })
    })
}

// Compare open file identity where the platform exposes it without unsafe calls
fn same_identity(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == after.dev() && before.ino() == after.ino()
    }
    #[cfg(not(unix))]
    {
        before.created().ok() == after.created().ok()
    }
}

// Open through a regular-file descriptor without following a swapped final symlink on Unix
fn open_input(path: &Path) -> Result<File, String> {
    #[cfg(unix)]
    let file = {
        use rustix::fs::{open, Mode, OFlags};
        let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW;
        File::from(open(path, flags, Mode::empty()).map_err(|_| INVALID)?)
    };
    #[cfg(not(unix))]
    let file = File::open(path).map_err(|_| INVALID)?;
    if !file
        .metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() <= VAULT_TRANSFER_MAX_FILE_BYTES as u64)
    {
        return Err(INVALID.into());
    }
    Ok(file)
}

fn read_transfer(
    file: &mut (impl Read + Seek),
    password: Option<&str>,
) -> Result<(Transfer, bool), String> {
    let kind = format::read_kind(file).map_err(|_| INVALID)?;
    let encrypted = kind.is_some();
    let bytes = match kind {
        Some(Kind::VaultEntries) => {
            let password = password
                .filter(|p| !p.is_empty())
                .ok_or("vault_transfer_password_required")?;
            if password.len() > vault::password::MAX_PASSWORD_BYTES {
                return Err("invalid_password".into());
            }
            format::open_payload(Kind::VaultEntries, file, Some(password), MAX_FILE)?
        }
        Some(_) => return Err(INVALID.into()),
        None => {
            file.rewind().map_err(|_| INVALID)?;
            let mut bytes = Zeroizing::new(Vec::new());
            file.take((MAX_FILE + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| INVALID)?;
            if bytes.len() > MAX_FILE {
                return Err(INVALID.into());
            }
            bytes
        }
    };
    let document: Transfer = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if document.format != "sdm-vault-entries"
        || document.version != 1
        || document.entries.is_empty()
        || document.entries.len() > MAX_ENTRIES
    {
        return Err(INVALID.into());
    }
    for entry in &document.entries {
        if !valid_label(&entry.label) || entry.kind.validate(&entry.value).is_err() {
            return Err(INVALID.into());
        }
    }
    Ok((document, encrypted))
}

fn classify(session: &Session, data: &Document, entries: &[TransferEntry]) -> Vec<VaultImportRow> {
    use std::collections::BTreeMap;
    let mut incoming = BTreeMap::new();
    let mut existing = BTreeMap::<String, Vec<_>>::new();
    for entry in entries {
        *incoming.entry(label_key(&entry.label)).or_insert(0u32) += 1;
    }
    for entry in data.entries.iter().filter(|entry| personal(entry)) {
        existing
            .entry(label_key(&entry.label))
            .or_default()
            .push(entry);
    }
    entries
        .iter()
        .enumerate()
        .map(|(row, entry)| {
            let name = label_key(&entry.label);
            let (status, existing) = if incoming[&name] != 1 {
                (VaultImportStatus::Ambiguous, None)
            } else {
                match existing.get(&name).map(Vec::as_slice) {
                    None => (VaultImportStatus::New, None),
                    Some([target]) if target.kind == entry.kind => {
                        (VaultImportStatus::Conflict, Some(meta(session.id, target)))
                    }
                    _ => (VaultImportStatus::Ambiguous, None),
                }
            };
            VaultImportRow {
                row: row as u32,
                label: entry.label.clone(),
                kind: entry.kind,
                protected: entry.protected,
                status,
                existing,
            }
        })
        .collect()
}

impl Vault {
    // Export explicitly selected revisions without replacing any existing destination
    pub fn export_entries(
        &self,
        lease: VaultRevealLease,
        path: &Path,
        selected: &[VaultExportSelection],
        password: Option<&str>,
        allow_plaintext: bool,
        master_password: Option<&str>,
    ) -> Result<u32, String> {
        self.export_entries_with_admission(
            lease,
            path,
            selected,
            password,
            allow_plaintext,
            master_password,
            || Ok(()),
        )
    }

    // Hold editor admission through publication to fence a dismissed window
    #[allow(clippy::too_many_arguments)]
    pub fn export_entries_with_admission<A>(
        &self,
        lease: VaultRevealLease,
        path: &Path,
        selected: &[VaultExportSelection],
        password: Option<&str>,
        allow_plaintext: bool,
        master_password: Option<&str>,
        admit: impl FnOnce() -> Result<A, String>,
    ) -> Result<u32, String> {
        if selected.is_empty() || selected.len() > MAX_ENTRIES {
            return Err("vault_export_selection_invalid".into());
        }
        match password {
            Some(p) if p.is_empty() || p.len() > vault::password::MAX_PASSWORD_BYTES => {
                return Err("invalid_password".into())
            }
            None if !allow_plaintext => return Err("vault_plaintext_confirmation_required".into()),
            _ => {}
        };
        let _worker = self
            .entry_transfer_worker
            .try_lock()
            .map_err(|_| "vault_transfer_busy")?;
        let name = path.file_name().ok_or("vault_transfer_invalid_path")?;
        let parent = path
            .parent()
            .ok_or("vault_transfer_invalid_path")?
            .canonicalize()
            .map_err(|_| "vault_transfer_invalid_path")?;
        let parent_identity = parent
            .metadata()
            .map_err(|_| "vault_transfer_invalid_path")?;
        let path = parent.join(name);
        require_external(&self.root, &path)?;
        if path.symlink_metadata().is_ok() {
            return Err("vault_transfer_destination_exists".into());
        }

        let (document, proof) = {
            let mut state = self.current_state();
            let session = reveal::session_for_reveal(&state, lease)?;
            let document = export_entries_document(session, selected)?;
            let proof = if document.entries.iter().any(|entry| entry.protected) {
                if session.mode != "password" {
                    return Err("master_password_mode_required".into());
                }
                let password = master_password
                    .filter(|p| !p.is_empty())
                    .ok_or("password_required")?;
                if password.len() > vault::password::MAX_PASSWORD_BYTES {
                    return Err("invalid_password".into());
                }
                if state.password_throttled() {
                    return Err("rate_limited".into());
                }
                let path = vault::checked_path(&self.root)?.ok_or("vault_recovery_required")?;
                let wrapper = vault::password::envelope(&path)?.ok_or("vault_recovery_required")?;
                let id = session.id;
                state.reveal_attempt_pending = true;
                state.retry_after = Some(Instant::now() + Duration::from_secs(1));
                Some((id, wrapper, password))
            } else {
                None
            };
            (document, proof)
        };
        if let Some((id, wrapper, password)) = proof {
            let key = wrapper.unlock(id, password);
            let mut state = self.current_state();
            state.reveal_attempt_pending = false;
            let session = reveal::session_for_reveal(&state, lease)?;
            let key = match key {
                Ok(key) => key,
                Err(error) => {
                    if error == "incorrect_password" {
                        state.password_failure();
                    }
                    return Err(error);
                }
            };
            if !session.vault.matches_data_key(&key)? {
                return Err("vault_recovery_required".into());
            }
            state.failed_passwords = 0;
            state.retry_after = None;
        }

        let mut bytes = Zeroizing::new(serde_json::to_vec(&document).map_err(|_| INVALID)?);
        drop(document);
        if bytes.len() > MAX_FILE {
            return Err(INVALID.into());
        }
        let make_temp =
            || tempfile::NamedTempFile::new_in(&parent).map_err(|_| "vault_transfer_write_failed");
        let sync_temp = |file: &tempfile::NamedTempFile| {
            file.as_file()
                .sync_all()
                .map_err(|_| "vault_transfer_write_failed")
        };
        let encrypted = if let Some(password) = password {
            let mut file = make_temp()?;
            format::write(Kind::VaultEntries, file.as_file_mut(), &mut bytes, password)?;
            sync_temp(&file)?;
            Some(file)
        } else {
            None
        };
        let state = self.current_state();
        let session = reveal::session_for_reveal(&state, lease)?;
        // Every selected entry must still be the same revision just before external publication
        drop(export_entries_document(session, selected)?);
        let checked_parent = || {
            if parent.canonicalize().ok().as_deref() != Some(parent.as_path())
                || !parent
                    .metadata()
                    .is_ok_and(|after| same_identity(&parent_identity, &after))
            {
                return Err("vault_transfer_invalid_path".into());
            }
            require_external(&self.root, &path)
        };
        checked_parent()?;
        let _admission = admit()?;
        let file = match encrypted {
            Some(file) => file,
            None => {
                let mut file = make_temp()?;
                file.write_all(&bytes)
                    .map_err(|_| "vault_transfer_write_failed")?;
                sync_temp(&file)?;
                file
            }
        };
        checked_parent()?;
        let temp_identity = file
            .as_file()
            .metadata()
            .map_err(|_| "vault_transfer_write_failed")?;
        if !file
            .path()
            .symlink_metadata()
            .is_ok_and(|after| after.is_file() && same_identity(&temp_identity, &after))
        {
            return Err("vault_transfer_write_failed".into());
        }
        file.persist_noclobber(&path).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                "vault_transfer_destination_exists"
            } else {
                "vault_transfer_write_failed"
            }
        })?;
        Ok(selected.len() as u32)
    }

    // Stage file data under the same unlock that admitted the native picker
    pub fn prepare_entry_import(
        &self,
        lease: VaultRevealLease,
        editor: u64,
        path: &Path,
        password: Option<&str>,
    ) -> Result<VaultImportReview, String> {
        self.stage_entry_import(lease, editor, || {
            let path = path
                .canonicalize()
                .map_err(|_| "vault_transfer_invalid_path")?;
            require_external(&self.root, &path)?;
            let metadata = path.metadata().map_err(|_| INVALID)?;
            if !metadata.is_file() || metadata.len() > VAULT_TRANSFER_MAX_FILE_BYTES as u64 {
                return Err(INVALID.into());
            }
            let mut file = open_input(&path)?;
            if path.canonicalize().ok().as_deref() != Some(path.as_path())
                || !file
                    .metadata()
                    .is_ok_and(|after| same_identity(&metadata, &after))
            {
                return Err(INVALID.into());
            }
            read_transfer(&mut file, password)
        })
    }

    // Review an explicitly dropped file in memory without creating a plaintext temporary file
    pub fn prepare_entry_import_bytes(
        &self,
        lease: VaultRevealLease,
        editor: u64,
        bytes: &[u8],
        password: Option<&str>,
    ) -> Result<VaultImportReview, String> {
        if bytes.len() > VAULT_TRANSFER_MAX_FILE_BYTES {
            return Err(INVALID.into());
        }
        self.stage_entry_import(lease, editor, || {
            read_transfer(&mut Cursor::new(bytes), password)
        })
    }

    fn stage_entry_import(
        &self,
        lease: VaultRevealLease,
        editor: u64,
        load: impl FnOnce() -> Result<(Transfer, bool), String>,
    ) -> Result<VaultImportReview, String> {
        let _loading = self
            .entry_transfer_worker
            .try_lock()
            .map_err(|_| "vault_import_busy")?;
        let ticket = Some((editor, uuid::Uuid::new_v4()));
        {
            let state = self.current_state();
            let session = reveal::session_for_reveal(&state, lease)?;
            session.entry_import.take();
            session.entry_import_pending.set(ticket);
        }
        let loaded = load();
        let state = self.current_state();
        let session = reveal::session_for_reveal(&state, lease)?;
        if session.entry_import_pending.replace(None) != ticket {
            return Err(EXPIRED.into());
        }
        let (document, encrypted) = loaded?;
        let data = read_document(&session.vault)?;
        let rows = classify(session, &data, &document.entries);
        let id = uuid::Uuid::new_v4().to_string();
        session.entry_import.replace(Some(StagedImport {
            id: id.clone(),
            editor,
            revision: data.revision,
            started: Instant::now(),
            wall_started: SystemTime::now(),
            entries: document.entries,
            rows: rows.clone(),
        }));
        self.wake.notify();
        Ok(VaultImportReview {
            id,
            encrypted,
            rows,
        })
    }

    // Fence a late IPC response after timeout, cancellation or a replacement review
    pub fn check_entry_import_review(&self, editor: u64, id: &str) -> Result<(), String> {
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        if session
            .entry_import
            .borrow()
            .as_ref()
            .is_some_and(|s| s.editor == editor && s.id == id)
        {
            Ok(())
        } else {
            Err(EXPIRED.into())
        }
    }

    // Reveal only one incoming conflict value, never an existing personal entry
    pub fn reveal_entry_import(
        &self,
        editor: u64,
        id: &str,
        row: u32,
    ) -> Result<VaultEntryReveal, String> {
        self.row_operation(|session, rows| {
            let revision = rows.revision()?;
            let staged = session.entry_import.borrow();
            let staged = staged
                .as_ref()
                .filter(|s| s.editor == editor && s.id == id && s.revision == revision)
                .ok_or(EXPIRED)?;
            let row = row as usize;
            if !staged
                .rows
                .get(row)
                .is_some_and(|r| r.status == VaultImportStatus::Conflict)
            {
                return Err("vault_import_not_a_conflict".into());
            }
            let incoming = &staged.entries[row];
            Ok(VaultEntryReveal {
                kind: incoming.kind,
                value: incoming.value.clone(),
            })
        })
    }

    // Apply one reviewed batch atomically, keeping existing conflicts unless explicitly selected
    pub fn apply_entry_import(
        &self,
        editor: u64,
        id: &str,
        overwrite: &[u32],
    ) -> Result<VaultImportResult, String> {
        self.apply_entry_import_with_admission(editor, id, overwrite, || Ok(()))
    }

    // Hold editor admission through committing the selected entry updates
    pub fn apply_entry_import_with_admission<A>(
        &self,
        editor: u64,
        id: &str,
        overwrite: &[u32],
        admit: impl FnOnce() -> Result<A, String>,
    ) -> Result<VaultImportResult, String> {
        if overwrite.len() > MAX_ENTRIES {
            return Err(INVALID.into());
        }
        self.operation(|session, data| {
            let mut staging = session.entry_import.borrow_mut();
            let review = staging
                .as_ref()
                .filter(|s| s.editor == editor && s.id == id && s.revision == data.revision)
                .ok_or(EXPIRED)?;
            let accepted: BTreeSet<_> = overwrite.iter().copied().collect();
            if accepted.len() != overwrite.len()
                || accepted.iter().any(|&id| {
                    !review
                        .rows
                        .get(id as usize)
                        .is_some_and(|row| row.status == VaultImportStatus::Conflict)
                })
            {
                return Err(INVALID.into());
            }

            let new_count = review
                .rows
                .iter()
                .filter(|row| row.status == VaultImportStatus::New)
                .count();
            if data.entries.len() + new_count > MAX_ENTRIES {
                return Err("vault_limit".into());
            }

            let review = staging.take().unwrap();
            drop(staging);
            let mut result = VaultImportResult::default();
            for (mut incoming, row) in review.entries.into_iter().zip(review.rows) {
                match row.status {
                    VaultImportStatus::New => {
                        data.entries.push(Entry {
                            id: uuid::Uuid::new_v4(),
                            label: std::mem::take(&mut incoming.label),
                            kind: incoming.kind,
                            protected: incoming.protected,
                            revision: 1,
                            value: std::mem::take(&mut incoming.value),
                            owner: None,
                            successor: None,
                            private_slot: None,
                        });
                        result.created += 1;
                    }
                    VaultImportStatus::Conflict if accepted.contains(&row.row) => {
                        let existing = row.existing.ok_or(EXPIRED)?;
                        let target =
                            find_entry(session.id, data, &existing.id, &existing.revision)?;
                        if !personal(target) || target.kind != incoming.kind {
                            return Err(EXPIRED.into());
                        }
                        advance(target)?;
                        target.protected |= incoming.protected;
                        let target_id = target.id;
                        let protect_text = target.protected && target.kind == VaultKind::Text;
                        std::mem::swap(&mut target.value, &mut incoming.value);
                        if protect_text {
                            revoke_plaintext_grants(data, target_id);
                        }
                        result.overwritten += 1;
                    }
                    _ => result.skipped += 1,
                }
            }
            let _admission = admit()?;
            if result.created != 0 || result.overwritten != 0 {
                write_document(session, data)?;
            }
            Ok(result)
        })
    }

    // Closing an editor invalidates both staged data and an in-flight file read
    pub fn cancel_editor_entry_import(&self, editor: u64) {
        let state = self.current_state();
        if let Some(session) = &state.session {
            let mut staged = session.entry_import.borrow_mut();
            if staged.as_ref().is_some_and(|s| s.editor == editor) {
                *staged = None;
            }
            if session
                .entry_import_pending
                .get()
                .is_some_and(|(owner, _)| owner == editor)
            {
                session.entry_import_pending.set(None);
            }
        }
    }

    pub fn cancel_entry_import(&self, editor: u64, id: &str) {
        let state = self.current_state();
        if let Some(session) = &state.session {
            let mut staged = session.entry_import.borrow_mut();
            if staged
                .as_ref()
                .is_some_and(|s| s.editor == editor && s.id == id)
            {
                *staged = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(root: &Path, password: Option<&str>) -> Vault {
        let vault = Vault::new(
            root.into(),
            Arc::new(super::super::tests::TestKeys::default()),
            Storage::open(root.join("manager-store")).unwrap(),
        );
        if let Some(password) = password {
            vault.initialize_password(password, password).unwrap();
        } else {
            vault.initialize().unwrap();
        }
        vault
    }

    fn entry(vault: &Vault, label: &str, value: &str, protected: bool) -> VaultEntry {
        vault
            .create_entry(VaultEntryInput {
                label: label.into(),
                kind: VaultKind::Text,
                protected,
                value: json!(value),
            })
            .unwrap()
    }

    fn selection(entry: &VaultEntry) -> VaultExportSelection {
        VaultExportSelection {
            id: entry.id.clone(),
            revision: entry.revision.clone(),
        }
    }

    fn incoming(rows: Vec<Value>) -> Vec<u8> {
        serde_json::to_vec(&json!({"format":"sdm-vault-entries","version":1,"entries":rows}))
            .unwrap()
    }

    fn item(label: &str, value: &str) -> Value {
        json!({"label":label,"value":value,"protected":false,"kind":"text"})
    }

    #[test]
    fn transfer_preserves_explicit_protection_and_defaults_only_legacy_omissions() {
        let source_root = tempfile::tempdir().unwrap();
        let target_root = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let source = fixture(source_root.path(), None);
        let target = fixture(target_root.path(), None);
        let secret = source
            .create_entry(VaultEntryInput {
                label: "Unprotected fixture".into(),
                kind: VaultKind::ApiKey,
                protected: false,
                value: json!("fixture key"),
            })
            .unwrap();
        let path = output.path().join("entries.sdmvault");
        source
            .export_entries(
                source.begin_entry_reveal().unwrap(),
                &path,
                &[selection(&secret)],
                None,
                true,
                None,
            )
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let target_lease = target.begin_entry_reveal().unwrap();
        let staged = target
            .prepare_entry_import_bytes(target_lease, 7, &bytes, None)
            .unwrap();
        assert!(!staged.rows[0].protected);
        target.apply_entry_import(7, &staged.id, &[]).unwrap();
        let loaded = target.entries(&[]).unwrap().remove(0);
        assert_eq!(loaded.kind, VaultKind::ApiKey);
        assert_eq!(
            target
                .reveal_entry(target_lease, &loaded.id, &loaded.revision, None)
                .unwrap()
                .value,
            json!("fixture key")
        );

        let bytes = incoming(vec![
            json!({"label":"Legacy key","kind":"api-key","value":"fixture"}),
            json!({"label":"Legacy text","kind":"text","value":"fixture"}),
        ]);
        let staged = target
            .prepare_entry_import_bytes(target_lease, 7, &bytes, None)
            .unwrap();
        assert!(staged.rows[0].protected);
        assert!(!staged.rows[1].protected);
        target.apply_entry_import(7, &staged.id, &[]).unwrap();
        let bytes = incoming(vec![
            json!({"label":"Legacy key","kind":"api-key","protected":false,"value":"replacement"}),
        ]);
        let staged = target
            .prepare_entry_import_bytes(target_lease, 7, &bytes, None)
            .unwrap();
        assert!(!staged.rows[0].protected);
        target.apply_entry_import(7, &staged.id, &[0]).unwrap();
        assert!(
            target
                .entries(&[])
                .unwrap()
                .iter()
                .find(|e| e.label == "Legacy key")
                .unwrap()
                .protected
        );

        let bytes = incoming(vec![
            json!({"label":"Invalid","kind":"api-key","protected":null,"value":"fixture"}),
        ]);
        assert!(target
            .prepare_entry_import_bytes(target_lease, 7, &bytes, None)
            .is_err());
    }

    #[test]
    fn personal_transfer_round_trips_every_entry_at_the_existing_vault_limit() {
        let source_root = tempfile::tempdir().unwrap();
        let target_root = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let source = fixture(source_root.path(), None);
        let target = fixture(target_root.path(), None);
        let source_lease = source.begin_entry_reveal().unwrap();
        let records = (1..=MAX_ENTRIES)
            .map(|number| item(&format!("Entry {number:03}"), "synthetic value"))
            .collect();
        let stage = source
            .prepare_entry_import_bytes(source_lease, 7, &incoming(records), None)
            .unwrap();
        assert_eq!(
            source
                .apply_entry_import(7, &stage.id, &[])
                .unwrap()
                .created,
            MAX_ENTRIES as u32
        );
        let selected: Vec<_> = source.entries(&[]).unwrap().iter().map(selection).collect();
        let path = output.path().join("full-vault.sdmvault");
        assert_eq!(
            source
                .export_entries(source_lease, &path, &selected, None, true, None)
                .unwrap(),
            MAX_ENTRIES as u32
        );
        let review = target
            .prepare_entry_import(target.begin_entry_reveal().unwrap(), 9, &path, None)
            .unwrap();
        assert_eq!(review.rows.len(), MAX_ENTRIES);
        assert_eq!(review.rows[0].label, "Entry 001");
        assert_eq!(
            review.rows.last().unwrap().label,
            format!("Entry {MAX_ENTRIES:03}")
        );
        assert_eq!(
            target
                .apply_entry_import(9, &review.id, &[])
                .unwrap()
                .created,
            MAX_ENTRIES as u32
        );
    }

    #[test]
    fn personal_transfer_preview_is_redacted_and_default_keeps_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let vault = fixture(root.path(), None);
        let existing = entry(&vault, "Kept", "old-value", true);
        let exported = entry(&vault, "Exportable", "export-fixture", false);
        let path = out.path().join("entries.sdmvault");
        let lease = vault.begin_entry_reveal().unwrap();
        assert!(vault
            .export_entries(lease, &path, &[selection(&existing)], None, true, None)
            .is_err());
        assert!(vault
            .export_entries(lease, &path, &[selection(&exported)], None, false, None)
            .is_err());
        assert!(vault
            .export_entries(
                lease,
                &root.path().join("blocked.sdmvault"),
                &[selection(&exported)],
                None,
                true,
                None
            )
            .is_err());
        vault
            .export_entries(lease, &path, &[selection(&exported)], None, true, None)
            .unwrap();
        let output = std::fs::read(&path).unwrap();
        assert!(String::from_utf8_lossy(&output).contains("export-fixture"));
        assert!(!String::from_utf8_lossy(&output).contains("old-value"));
        assert!(vault
            .export_entries(lease, &path, &[selection(&exported)], None, true, None)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), output);
        let cancelled = out.path().join("cancelled.sdmvault");
        assert!(vault
            .export_entries_with_admission(
                lease,
                &cancelled,
                &[selection(&exported)],
                None,
                true,
                None,
                || Err::<(), _>("editor closed".into()),
            )
            .is_err());
        assert!(!cancelled.exists());

        let bytes = incoming(vec![
            item(" KePT ", "incoming-fixture"),
            item("New", "new-value"),
            item("Duplicate", "a"),
            item("duplicate", "b"),
        ]);
        let review = vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .unwrap();
        assert!(!serde_json::to_string(&review)
            .unwrap()
            .contains("incoming-fixture"));
        assert_eq!(vault.entries(&[]).unwrap().len(), 2);
        assert!(matches!(review.rows[0].status, VaultImportStatus::Conflict));
        assert!(matches!(review.rows[1].status, VaultImportStatus::New));
        assert!(matches!(
            review.rows[2].status,
            VaultImportStatus::Ambiguous
        ));
        assert!(vault.reveal_entry_import(8, &review.id, 0).is_err());
        assert!(vault.reveal_entry_import(7, &review.id, 1).is_err());
        assert_eq!(
            vault.reveal_entry_import(7, &review.id, 0).unwrap().value,
            json!("incoming-fixture")
        );
        assert!(vault.apply_entry_import(7, &review.id, &[2]).is_err());
        let result = vault.apply_entry_import(7, &review.id, &[]).unwrap();
        assert_eq!(
            (result.created, result.overwritten, result.skipped),
            (1, 0, 3)
        );
        assert!(vault.apply_entry_import(7, &review.id, &[]).is_err());
        let bytes = incoming(vec![item("KEPT", "replacement")]);
        let closed = vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .unwrap();
        assert!(vault
            .apply_entry_import_with_admission(7, &closed.id, &[0], || Err::<(), _>(
                "editor closed".into()
            ))
            .is_err());
        assert!(vault.check_entry_import_review(7, &closed.id).is_err());
        assert_eq!(
            vault
                .entries(&[])
                .unwrap()
                .into_iter()
                .find(|e| e.id == existing.id)
                .unwrap()
                .revision,
            existing.revision
        );

        let review = vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .unwrap();
        assert_eq!(
            vault
                .apply_entry_import(7, &review.id, &[0])
                .unwrap()
                .overwritten,
            1
        );
        let updated = vault
            .entries(&[])
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == existing.id)
            .unwrap();
        assert_eq!(updated.label, "Kept");
        assert!(updated.protected);
        assert_ne!(updated.revision, existing.revision);
        vault
            .operation(|session, data| {
                assert_eq!(
                    find_entry(session.id, data, &updated.id, &updated.revision)?.value,
                    json!("replacement")
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn personal_transfer_rejects_stale_closed_wrong_kind_and_malformed_imports() {
        let root = tempfile::tempdir().unwrap();
        let vault = fixture(root.path(), None);
        let lease = vault.begin_entry_reveal().unwrap();
        let bytes = incoming(vec![item("Fixture", "fixture")]);
        let review = vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .unwrap();
        entry(&vault, "Mutation", "change", false);
        assert!(vault.apply_entry_import(7, &review.id, &[]).is_err());
        let review = vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .unwrap();
        vault.cancel_editor_entry_import(7);
        assert!(vault.apply_entry_import(7, &review.id, &[]).is_err());
        let review = vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .unwrap();
        {
            let state = vault.current_state();
            state
                .session
                .as_ref()
                .unwrap()
                .entry_import
                .borrow_mut()
                .as_mut()
                .unwrap()
                .wall_started -= REVIEW_TTL;
        }
        assert!(vault.apply_entry_import(7, &review.id, &[]).is_err());
        let review = vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .unwrap();
        vault.lock();
        vault.unlock().unwrap();
        assert!(vault.apply_entry_import(7, &review.id, &[]).is_err());
        assert!(vault
            .prepare_entry_import_bytes(lease, 7, &bytes, None)
            .is_err());
        let lease = vault.begin_entry_reveal().unwrap();
        assert!(vault
            .prepare_entry_import_bytes(lease, 7, b"SDMPROF1tail", Some("pass"))
            .is_err());
        assert!(vault
            .prepare_entry_import_bytes(
                lease,
                7,
                &incoming(vec![
                    json!({"label":"x","kind":"text","protected":false,"owner":null,"value":"x"})
                ]),
                None
            )
            .is_err());
    }

    #[test]
    fn personal_transfer_encrypts_with_an_independent_exact_password() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let master = "Fixture master password";
        let transfer_password = " Fixture transfer password ";
        let vault = fixture(root.path(), Some(master));
        let entry = entry(&vault, "Protected", "encrypted fixture value", true);
        let lease = vault.begin_entry_reveal().unwrap();
        let path = out.path().join("entries.sdmvault");
        assert!(vault
            .export_entries(
                lease,
                &path,
                &[selection(&entry)],
                Some(transfer_password),
                false,
                None
            )
            .is_err());
        vault
            .export_entries(
                lease,
                &path,
                &[selection(&entry)],
                Some(transfer_password),
                false,
                Some(master),
            )
            .unwrap();
        assert!(!String::from_utf8_lossy(&std::fs::read(&path).unwrap())
            .contains("encrypted fixture value"));
        assert_eq!(
            vault
                .prepare_entry_import(lease, 7, &path, None)
                .err()
                .as_deref(),
            Some("vault_transfer_password_required")
        );
        assert!(vault
            .prepare_entry_import(lease, 7, &path, Some(transfer_password.trim()))
            .is_err());
        assert!(
            vault
                .prepare_entry_import(lease, 7, &path, Some(transfer_password))
                .unwrap()
                .encrypted
        );
    }
}
