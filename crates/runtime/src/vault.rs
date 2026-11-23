//! Native-only Vault service. User values never form part of the workspace snapshot
mod continuity;
mod display_text;
mod format_text;
mod mode;
pub(crate) use continuity::Continuity;
mod plugin_settings;
mod reveal;
mod rows;
#[cfg(test)]
mod rows_tests;
pub use reveal::{VaultEntryReveal, VaultRevealLease};
use rows::{read_document, RowKind, Rows};
mod references;
pub use plugin_settings::{VaultSettingsDocument, VaultSettingsPlugin, VaultSettingsRow};
mod transfer;
pub use transfer::{
    VaultExportSelection, VaultImportResult, VaultImportReview, VaultImportRow, VaultImportStatus,
    VAULT_TRANSFER_MAX_FILE_BYTES, VAULT_TRANSFER_MAX_PASSWORD_BYTES,
};
mod typed;
mod writes;
use crate::credentials::CredentialProvider;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use stream_dock_manager_plugin_host::vault_rpc::{VaultInstallation, VaultRead, VaultSetupSlot};
use stream_dock_manager_storage::{
    vault::{self, EncryptedVault},
    Domain, JsonRecord, Storage,
};
use typed::TypedReference;
pub(crate) fn is_typed_reference(text: &str) -> bool {
    TypedReference::parse(text).is_ok()
}
pub(crate) fn is_whole_reference(text: &str) -> bool {
    TypedReference::parse(text).is_ok_and(|r| r.field.is_none())
}
use zeroize::{Zeroize, Zeroizing};

const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_VALUE: usize = 64 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_GRANTS: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum VaultState {
    Uninitialized,
    Locked,
    Unlocked,
    Unavailable,
    RecoveryRequired,
}

#[derive(Clone, Copy, Serialize, ts_rs::TS)]
pub struct VaultStatus {
    pub state: VaultState,
    pub mode: &'static str,
}

// Opaque, session-scoped cache fence without entry IDs or decrypted values
#[derive(Serialize, ts_rs::TS)]
pub struct VaultMetadataRevision {
    pub state: VaultState,
    pub mode: &'static str,
    pub entries_revision: Option<String>,
}

#[derive(Serialize, ts_rs::TS)]
pub struct VaultPersonalEntries {
    pub revision: String,
    pub entries: Vec<VaultEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
pub enum VaultKind {
    ApiKey,
    BearerToken,
    Credential,
    Text,
    Integer,
    Boolean,
    Json,
    File,
    Folder,
    Location,
}

/// Replace local reference tokens in a document crossing a portable transfer boundary.
pub(crate) fn detach_portable_references(document: &mut Value) -> bool {
    static TOKEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        let uuid = "[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}";
        regex::Regex::new(&format!(
            r"\$\{{vault\.{uuid}\.{uuid}(?:\.(?:latitude|longitude|name))?\}}"
        ))
        .expect("Vault token syntax")
    });
    match document {
        Value::String(text) if text.contains("${vault.") => {
            if let std::borrow::Cow::Owned(replaced) =
                TOKEN.replace_all(text, regex::NoExpand("${vault.unlinked}"))
            {
                *text = replaced;
                true
            } else {
                false
            }
        }
        Value::Array(values) => detach_children(values.iter_mut()),
        Value::Object(fields) => detach_children(fields.values_mut()),
        _ => false,
    }
}

fn detach_children<'a>(children: impl Iterator<Item = &'a mut Value>) -> bool {
    let mut changed = false;
    for child in children {
        changed |= detach_portable_references(child);
    }
    changed
}

impl VaultKind {
    // Preserve the legacy default for transfer records without an explicit choice
    fn default_protected(self) -> bool {
        matches!(
            self,
            Self::ApiKey
                | Self::BearerToken
                | Self::Credential
                | Self::File
                | Self::Folder
                | Self::Location
        )
    }

    // Validate the exact logical type without probing a path or invoking any service
    fn validate(self, value: &Value) -> Result<(), String> {
        let bytes = Zeroizing::new(serde_json::to_vec(value).map_err(|_| "invalid_value")?);
        if bytes.len() > MAX_VALUE {
            return Err("invalid_value".into());
        }
        let valid = match self {
            Self::ApiKey | Self::BearerToken | Self::Credential => value
                .as_str()
                .is_some_and(|s| !s.is_empty() && !s.chars().any(char::is_control)),
            Self::Text => value.is_string(),
            Self::Integer => value
                .as_i64()
                .is_some_and(|n| n.unsigned_abs() <= 9_007_199_254_740_991),
            Self::Boolean => value.is_boolean(),
            Self::Json => true,
            Self::File | Self::Folder => value
                .as_str()
                .is_some_and(|s| !s.contains('\0') && std::path::Path::new(s).is_absolute()),
            Self::Location => value.as_object().is_some_and(|m| {
                m.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "name" | "latitude" | "longitude" | "accuracy_m" | "source"
                    )
                }) && m
                    .get("accuracy_m")
                    .is_none_or(|value| value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0))
                    && m.get("source")
                        .is_none_or(|value| value.as_str() == Some("os"))
                    && m.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(valid_label)
                    && m.get("latitude")
                        .and_then(Value::as_f64)
                        .is_some_and(|n| (-90.0..=90.0).contains(&n))
                    && m.get("longitude")
                        .and_then(Value::as_f64)
                        .is_some_and(|n| (-180.0..=180.0).contains(&n))
            }),
        };
        if !valid {
            return Err("invalid_value".into());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, ts_rs::TS)]
pub struct VaultEntry {
    pub id: String,
    pub label: String,
    pub kind: VaultKind,
    pub protected: bool,
    pub revision: String,
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub owner_verified: Option<bool>,
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct VaultEntryInput {
    pub label: String,
    pub kind: VaultKind,
    pub protected: bool,
    pub value: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    private_slot: Option<writes::PrivateSlot>,
    id: uuid::Uuid,
    label: String,
    kind: VaultKind,
    protected: bool,
    revision: u64,
    #[serde(default)]
    owner: Option<VaultInstallation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    successor: Option<VaultInstallation>,
    value: Value,
}

impl Entry {
    fn is_personal(&self) -> bool {
        self.owner.is_none() && self.private_slot.is_none() && self.successor.is_none()
    }
}

impl Drop for Entry {
    // Clear decrypted entry data when the operation ends
    fn drop(&mut self) {
        self.label.zeroize();
        clear_value(&mut self.value);
    }
}

// Best-effort zeroing of the owned value buffers
fn clear_value(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(items) => items.iter_mut().for_each(clear_value),
        Value::Object(map) => {
            for (mut key, mut value) in std::mem::take(map) {
                key.zeroize();
                clear_value(&mut value);
            }
        }
        _ => {}
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    writers: Vec<writes::WriterGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    references: Vec<references::ReferenceGrant>,
    entries: Vec<Entry>,
    #[serde(default)]
    grants: Vec<Grant>,
    #[serde(default)]
    revision: u64,
}

struct InstalledReview {
    owner: VaultInstallation,
    slot: VaultSetupSlot,
    editor: u64,
    revision: u64,
    epoch: uuid::Uuid,
    deadline: Instant,
}

#[derive(Serialize, ts_rs::TS)]
pub struct InstalledVaultReview {
    pub id: String,
    pub slot: VaultSetupSlot,
    pub entries: Vec<VaultEntry>,
    pub inventory_sha256: String,
}

pub(crate) struct BuiltinReferenceLease {
    epoch: uuid::Uuid,
    revision: u64,
}

struct LocationRequest {
    id: uuid::Uuid,
    editor: u64,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

struct Session {
    mode: &'static str,
    location: std::cell::RefCell<Option<LocationRequest>>,
    entry_import: std::cell::RefCell<Option<transfer::StagedImport>>,
    entry_import_pending: std::cell::Cell<Option<(u64, uuid::Uuid)>>,
    last_activity: Instant,
    last_wall_activity: std::time::SystemTime,
    vault: EncryptedVault,
    id: uuid::Uuid,
    metadata_write: std::cell::Cell<uuid::Uuid>,
    change_wake: Arc<crate::sampler::Signal>,
    display_text: std::cell::RefCell<display_text::DisplayTextCache>,
    epoch: uuid::Uuid,
}

impl Session {
    fn entries_revision(&self) -> String {
        hex::encode(
            Sha256::new()
                .chain_update(self.epoch.as_bytes())
                .chain_update(self.metadata_write.get().as_bytes())
                .finalize(),
        )
    }

    // Advance the cache fence only after ciphertext was committed
    fn changed(&self) {
        self.cancel_location();
        self.entry_import.take();
        self.entry_import_pending.set(None);
        self.display_text.borrow_mut().clear();
        self.metadata_write.set(uuid::Uuid::new_v4());
        self.change_wake.notify();
    }

    fn mutate<T>(&self, mutate: impl FnOnce(&Rows<'_>) -> Result<T, String>) -> Result<T, String> {
        let value = self.vault.rows(|rows| {
            let result = mutate(rows)?;
            rows::bump(rows)?;
            Ok(result)
        })?;
        self.changed();
        Ok(value)
    }

    fn cancel_location(&self) {
        if let Some(request) = self.location.take() {
            request
                .cancel
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel_location();
    }
}

struct ResetReview {
    token: String,
    editor: u64,
    identity: uuid::Uuid,
    created: Instant,
}

#[derive(Default)]
struct State {
    reset_review: Option<ResetReview>,
    locked_event_pending: bool,
    reviews: std::collections::HashMap<String, InstalledReview>,
    session: Option<Session>,
    failure: Option<VaultState>,
    retry_after: Option<Instant>,
    failed_passwords: u8,
    reveal_attempt_pending: bool,
}

impl State {
    fn password_throttled(&self) -> bool {
        self.reveal_attempt_pending || self.retry_after.is_some_and(|at| at > Instant::now())
    }

    fn password_failure(&mut self) {
        self.failed_passwords = self.failed_passwords.saturating_add(1).min(6);
        self.retry_after =
            Some(Instant::now() + Duration::from_secs(1 << (self.failed_passwords - 1)));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingRevocations {
    revision: u64,
    // Only the public plugin/action names appear outside the encrypted ledger.
    direct_reads: BTreeSet<(String, String)>,
    #[serde(default)]
    private_writes: BTreeSet<(String, String)>,
}

const REVOCATIONS: JsonRecord<PendingRevocations> = JsonRecord {
    domain: Domain::ManagerPreferences,
    key: "vault-direct-read-revocations-v1",
    max_bytes: 1024 * 1024,
    decode: |text| serde_json::from_str(text).map_err(|_| "vault_recovery_required".into()),
};

pub struct Vault {
    wake: Arc<crate::sampler::Signal>,
    store: Storage,
    root: PathBuf,
    credentials: Arc<dyn CredentialProvider>,
    state: Mutex<State>,
    entry_transfer_worker: Mutex<()>,
}

impl Vault {
    // Make the service without opening a keystore or touching the keyring
    pub(crate) fn new(
        root: PathBuf,
        credentials: Arc<dyn CredentialProvider>,
        store: Storage,
    ) -> Self {
        Self {
            root,
            credentials,
            store,
            state: Mutex::new(State::default()),
            entry_transfer_worker: Mutex::new(()),
            wake: Default::default(),
        }
    }

    // Check at admission as well as on the worker. Background requests never renew a session.
    fn current_state(&self) -> std::sync::MutexGuard<'_, State> {
        let mut state = self.state.lock().unwrap();
        self.expire_session(&mut state);
        state
    }
    fn expire_session(&self, state: &mut State) {
        if let Some(session) = &state.session {
            session.expire_entry_import();
        }
        if state.session.as_ref().is_some_and(|session| {
            session.last_activity.elapsed() >= IDLE_TIMEOUT
                || session
                    .last_wall_activity
                    .elapsed()
                    .map_or(true, |elapsed| elapsed >= IDLE_TIMEOUT)
        }) {
            state.session = None;
            state.reviews.clear();
            state.failure = None;
            state.locked_event_pending = true;
            self.wake.notify();
        }
    }

    pub(crate) fn session_epoch(&self) -> Result<uuid::Uuid, String> {
        self.current_state()
            .session
            .as_ref()
            .map(|s| s.epoch)
            .ok_or_else(|| "vault_locked".into())
    }

    /// Admit at most one explicit OS location request from this unlocked editor.
    pub fn begin_location(
        &self,
        id: uuid::Uuid,
        editor: u64,
    ) -> Result<Arc<std::sync::atomic::AtomicBool>, String> {
        let mut state = self.current_state();
        let session = state.session.as_mut().ok_or("vault_locked")?;
        let pending = session.location.get_mut();
        if pending.is_some() {
            return Err("location_pending".into());
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *pending = Some(LocationRequest {
            id,
            editor,
            cancel: cancel.clone(),
        });
        Ok(cancel)
    }

    pub fn cancel_location(&self, id: uuid::Uuid, editor: u64) {
        let mut state = self.current_state();
        if let Some(session) = state.session.as_mut() {
            if session
                .location
                .get_mut()
                .as_ref()
                .is_some_and(|r| r.id == id && r.editor == editor)
            {
                session.cancel_location();
            }
        }
    }

    pub fn finish_location(
        &self,
        cancel: &Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        let mut state = self.current_state();
        let pending = state
            .session
            .as_mut()
            .ok_or("vault_locked")?
            .location
            .get_mut();
        if !pending
            .as_ref()
            .is_some_and(|r| Arc::ptr_eq(&r.cancel, cancel))
        {
            return Err("cancelled".into());
        }
        pending.take();
        Ok(())
    }

    pub fn user_activity(&self) {
        if let Some(session) = self.current_state().session.as_mut() {
            session.last_activity = Instant::now();
            session.last_wall_activity = std::time::SystemTime::now();
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    pub(crate) fn is_unlocked(&self) -> bool {
        self.current_state().session.is_some()
    }

    pub(crate) fn start_idle_monitor(runtime: &Arc<crate::Runtime>) {
        let wake = runtime.vault.wake.clone();
        let weak = Arc::downgrade(runtime);
        std::thread::spawn(move || {
            let mut revision = 0;
            let mut last_presentation = None;
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            let mut observing_power = false;
            loop {
                let Some(runtime) = weak.upgrade() else { break };
                let (notify, remaining, presentation) = {
                    let mut state = runtime.vault.current_state();
                    (
                        std::mem::take(&mut state.locked_event_pending),
                        state.session.as_ref().map(|session| {
                            let idle = IDLE_TIMEOUT.saturating_sub(session.last_activity.elapsed());
                            session
                                .entry_import_remaining()
                                .map_or(idle, |time| idle.min(time))
                        }),
                        state
                            .session
                            .as_ref()
                            .map(|s| (s.epoch, s.metadata_write.get())),
                    )
                };
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                let remaining = {
                    let active = remaining.is_some();
                    if active != observing_power {
                        observing_power = active;
                        runtime.vendor_signal.notify();
                    }
                    if active && crate::power_policy::session_locked() == Some(true) {
                        runtime.vault.end_session();
                    }
                    remaining.map(|interval| interval.min(Duration::from_secs(5)))
                };
                if notify {
                    let mut host = runtime.plugins.lock().unwrap();
                    let locked = runtime.vault.status().state != VaultState::Unlocked;
                    host.set_vault_locked(locked);
                    drop(host);
                    runtime.emit("vault-session-ended", None, None, None);
                }
                if last_presentation != presentation {
                    last_presentation = presentation;
                    runtime
                        .vault_surface_revision
                        .fetch_add(1, std::sync::atomic::Ordering::Release);
                    runtime.emit("vault-presentation-changed", None, None, None);
                    if !runtime
                        .device_wake_pending
                        .swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        let _ = runtime.device_commands.send(crate::devices::Request::Wake);
                    }
                }
                drop(runtime);
                if !wake.wait(&mut revision, remaining) {
                    break;
                }
            }
        });
    }

    pub(crate) fn stop(&self) {
        self.end_session();
        self.wake.stop();
    }

    // A corrupt or unreadable path is still an existing Vault for startup gating
    pub fn is_configured(&self) -> bool {
        !matches!(vault::reset_pending(&self.root), Ok(false))
            || !matches!(vault::checked_path(&self.root), Ok(None))
    }

    /// Called once while constructing the runtime, before admitting plugins.
    /// Reopening the editor or refreshing Vault status must never undo a later lock.
    pub(crate) fn startup_locked(&self, unlock_os: bool) -> bool {
        if !self.is_configured() {
            return false;
        }
        if unlock_os {
            let status = self.status();
            if status.state == VaultState::Locked && status.mode == "os" {
                #[cfg(any(target_os = "windows", target_os = "linux"))]
                if crate::power_policy::session_locked() == Some(true) {
                    return true;
                }
                return self.unlock().is_err();
            }
            return status.state != VaultState::Unlocked;
        }
        true
    }

    // Report only public lock state
    pub fn status(&self) -> VaultStatus {
        self.status_for(&self.current_state())
    }

    fn status_for(&self, state: &State) -> VaultStatus {
        let (value, mode) = if let Some(session) = &state.session {
            (VaultState::Unlocked, session.mode)
        } else if self.recover_mode().is_err()
            || !matches!(vault::reset_pending(&self.root), Ok(false))
        {
            (VaultState::RecoveryRequired, "unknown")
        } else {
            match vault::checked_path(&self.root) {
                Ok(None) => (state.failure.unwrap_or(VaultState::Uninitialized), "os"),
                Ok(Some(path)) => match vault::password::envelope(&path) {
                    Ok(envelope) => (
                        state.failure.unwrap_or(VaultState::Locked),
                        if envelope.is_some() { "password" } else { "os" },
                    ),
                    Err(code) => (error_state(&code), "unknown"),
                },
                Err(code) => (error_state(&code), "unknown"),
            }
        };
        VaultStatus { state: value, mode }
    }

    // Nonsecret fence for text-bearing scenes. Reading it does not refresh session activity.
    pub(crate) fn presentation_revision(&self) -> Option<(uuid::Uuid, uuid::Uuid)> {
        self.current_state()
            .session
            .as_ref()
            .map(|s| (s.epoch, s.metadata_write.get()))
    }

    // Read the live session fence without decrypting the document or scanning plugins
    pub fn metadata_revision(&self) -> VaultMetadataRevision {
        let state = self.current_state();
        let status = self.status_for(&state);
        VaultMetadataRevision {
            state: status.state,
            mode: status.mode,
            entries_revision: state.session.as_ref().map(Session::entries_revision),
        }
    }

    pub fn initialize(&self) -> Result<VaultStatus, String> {
        self.create(None)
    }

    pub fn initialize_password(
        &self,
        password: &str,
        confirmation: &str,
    ) -> Result<VaultStatus, String> {
        validate_new_password(password, confirmation)?;
        self.create(Some(password))
    }

    fn create(&self, password: Option<&str>) -> Result<VaultStatus, String> {
        let mut state = self.current_state();
        self.recover_mode()?;
        if vault::reset_pending(&self.root)? {
            return Err("vault_recovery_required".into());
        }
        if vault::checked_path(&self.root)?.is_some() {
            return Err("vault_already_initialized".into());
        }
        let id = uuid::Uuid::new_v4();
        let key = vault::data_key()?;
        let staged =
            EncryptedVault::stage_rows(&vault::database_dir(&self.root)?, id, &key, |_| Ok(()))?;
        let mut staged_db = EncryptedVault::open(staged.path(), id, &key)?;
        let account = key_account(id);
        let mode = if let Some(password) = password {
            let envelope = vault::password::PasswordEnvelope::new(id, password, &key)?;
            staged_db.store_envelope(&envelope)?;
            drop(staged_db);
            let verified = vault::password::envelope(staged.path())?
                .ok_or("vault_recovery_required")?
                .unlock(id, password)?;
            read_document(&EncryptedVault::open(staged.path(), id, &verified)?)?;
            "password"
        } else {
            read_document(&staged_db)?;
            drop(staged_db);
            let encoded = Zeroizing::new(hex::encode(key.as_slice()));
            if self.credentials.write(&account, &encoded).is_err() {
                state.failure = Some(VaultState::Unavailable);
                return Err("vault_unavailable".into());
            }
            "os"
        };
        staged
            .as_file()
            .sync_all()
            .map_err(|_| "vault_unavailable")?;
        let path = vault::database_file(&self.root)?;
        if staged.persist_noclobber(&path).is_err() {
            if password.is_none() {
                let _ = self.credentials.delete(&account);
            }
            state.failure = Some(VaultState::Unavailable);
            return Err("vault_unavailable".into());
        }
        stream_dock_manager_storage::sync_directory(&vault::database_dir(&self.root)?)?;
        let db = EncryptedVault::open(&path, id, &key)?;
        state.session = Some(Session {
            mode,
            location: Default::default(),
            entry_import: Default::default(),
            entry_import_pending: Default::default(),
            metadata_write: std::cell::Cell::new(uuid::Uuid::new_v4()),
            change_wake: self.wake.clone(),
            display_text: Default::default(),
            last_activity: Instant::now(),
            last_wall_activity: std::time::SystemTime::now(),
            vault: db,
            id,
            epoch: uuid::Uuid::new_v4(),
        });
        state.failure = None;
        self.wake.notify();
        Ok(VaultStatus {
            state: VaultState::Unlocked,
            mode,
        })
    }

    // Unlock only an existing vault using its exact native keyring identity
    pub fn unlock(&self) -> Result<VaultStatus, String> {
        self.open_existing(None)
    }

    pub fn unlock_password(&self, password: &str) -> Result<VaultStatus, String> {
        self.open_existing(Some(password))
    }

    fn open_existing(&self, password: Option<&str>) -> Result<VaultStatus, String> {
        let mut state = self.current_state();
        if let Some(session) = &state.session {
            return Ok(VaultStatus {
                state: VaultState::Unlocked,
                mode: session.mode,
            });
        }
        if state.password_throttled() {
            return Err("rate_limited".into());
        }
        state.retry_after = Some(Instant::now() + Duration::from_secs(1));
        let opened = (|| {
            self.recover_mode()?;
            if vault::reset_pending(&self.root)? {
                return Err("vault_recovery_required".into());
            }
            let path = vault::checked_path(&self.root)?.ok_or("vault_not_initialized")?;
            let id = vault::identity(&path)?;
            let (key, mode) = match (vault::password::envelope(&path)?, password) {
                (Some(envelope), Some(password)) => (envelope.unlock(id, password)?, "password"),
                (Some(_), None) => return Err("password_required".into()),
                (None, Some(_)) => return Err("incorrect_unlock_mode".into()),
                (None, None) => {
                    let stored = Zeroizing::new(
                        self.credentials
                            .read(&key_account(id))
                            .map_err(|_| "vault_unavailable")?,
                    );
                    (decode_os_key(&stored, id, None)?, "os")
                }
            };
            let db = EncryptedVault::open(&path, id, &key)?;
            let session = Session {
                mode,
                location: Default::default(),
                entry_import: Default::default(),
                entry_import_pending: Default::default(),
                metadata_write: std::cell::Cell::new(uuid::Uuid::new_v4()),
                change_wake: self.wake.clone(),
                display_text: Default::default(),
                last_activity: Instant::now(),
                last_wall_activity: std::time::SystemTime::now(),
                vault: db,
                id,
                epoch: uuid::Uuid::new_v4(),
            };
            self.apply_revocations(&session)?;
            Ok::<_, String>(session)
        })();
        match opened {
            Ok(session) => {
                let mode = session.mode;
                state.failed_passwords = 0;
                state.retry_after = None;
                state.session = Some(session);
                self.wake.notify();
                state.failure = None;
                Ok(VaultStatus {
                    state: VaultState::Unlocked,
                    mode,
                })
            }
            Err(error) => {
                if error == "incorrect_password" {
                    state.password_failure();
                }
                state.failure = Some(error_state(&error));
                Err(error)
            }
        }
    }

    /// Rewrap the same data key under a fresh password and salt. All entries keep their identity.
    pub fn change_password(
        &self,
        current: &str,
        new_password: &str,
        confirmation: &str,
    ) -> Result<(), String> {
        validate_new_password(new_password, confirmation)?;
        let mut state = self.current_state();
        if state.password_throttled() {
            return Err("rate_limited".into());
        }
        state.retry_after = Some(Instant::now() + Duration::from_secs(1));
        let session = state.session.as_mut().ok_or("vault_locked")?;
        if session.mode != "password" {
            return Err("incorrect_unlock_mode".into());
        }
        let path = vault::checked_path(&self.root)?.ok_or("vault_recovery_required")?;
        let key = match vault::password::envelope(&path)?
            .ok_or("vault_recovery_required")?
            .unlock(session.id, current)
        {
            Ok(key) => key,
            Err(error) => {
                if error == "incorrect_password" {
                    state.password_failure();
                }
                return Err(error);
            }
        };
        let replacement = vault::password::PasswordEnvelope::new(session.id, new_password, &key)?;
        let verified = replacement.unlock(session.id, new_password)?;
        read_document(&EncryptedVault::open(&path, session.id, &verified)?)?;
        session.vault.store_envelope(&replacement)?;
        session.cancel_location();
        session.epoch = uuid::Uuid::new_v4();
        state.reviews.clear();
        state.reset_review = None;
        state.failed_passwords = 0;
        state.retry_after = None;
        Ok(())
    }

    pub fn review_keystore_reset(&self, editor: u64) -> Result<String, String> {
        let mut state = self.current_state();
        let path = vault::checked_path(&self.root)?.ok_or("vault_not_initialized")?;
        let identity = vault::identity(&path)?;
        let token = uuid::Uuid::new_v4().to_string();
        state.reset_review = Some(ResetReview {
            token: token.clone(),
            editor,
            identity,
            created: Instant::now(),
        });
        Ok(token)
    }

    pub fn cancel_keystore_reset(&self, editor: u64, token: &str) {
        let mut state = self.current_state();
        if state
            .reset_review
            .as_ref()
            .is_some_and(|r| r.token == token && r.editor == editor)
        {
            state.reset_review = None;
        }
    }

    /// Retire an exact generation. An interrupted reset stays locked until explicitly retried.
    pub fn delete_keystore(
        &self,
        editor: u64,
        token: &str,
        confirmation: &str,
    ) -> Result<VaultStatus, String> {
        use crate::credentials::CredentialState;
        if confirmation != "CLEAR VAULT ENTRIES" {
            return Err("confirmation_required".into());
        }
        let mut state = self.current_state();
        if !state
            .reset_review
            .as_ref()
            .is_some_and(|r| r.editor == editor && r.token == token)
        {
            return Err("reset_review_expired".into());
        }
        let review = state.reset_review.take().ok_or("reset_review_expired")?;
        let path = vault::checked_path(&self.root)?.ok_or("vault_not_initialized")?;
        if review.created.elapsed() > Duration::from_secs(300)
            || vault::identity(&path)? != review.identity
        {
            return Err("reset_review_expired".into());
        }
        self.recover_mode()?;
        let intent = match vault::ResetIntent::begin(&self.root) {
            Err(error) if matches!(vault::reset_pending(&self.root), Ok(false)) => {
                return Err(error);
            }
            result => result,
        };
        state.session = None;
        state.reviews.clear();
        state.retry_after = None;
        state.failure = Some(VaultState::RecoveryRequired);
        state.locked_event_pending = true;
        self.wake.notify();
        let intent = intent?;
        let live = intent.live_identity(&self.root)?;
        let next_account = key_account(intent.next);
        if live == intent.previous {
            // A prior attempt may already have registered the new key. Retain that key on retry.
            match self.credentials.read(&next_account).map(Zeroizing::new) {
                Ok(_) => {}
                Err(CredentialState::Missing) => {
                    let key = vault::data_key()?;
                    let raw = Zeroizing::new(hex::encode(key.as_slice()));
                    let encoded = Zeroizing::new(format!(
                        "reset-v1/{}/{}/{}",
                        intent.previous,
                        intent.next,
                        raw.as_str()
                    ));
                    self.credentials
                        .write(&next_account, &encoded)
                        .map_err(|_| "vault_unavailable")?;
                }
                Err(_) => return Err("vault_unavailable".into()),
            }
        }
        let encoded = Zeroizing::new(
            self.credentials
                .read(&next_account)
                .map_err(|_| "vault_unavailable")?,
        );
        let key = decode_os_key(&encoded, intent.next, Some(intent.previous))?;
        if live == intent.previous {
            intent.stage_empty_rows(&self.root, &key)?;
        }
        let previous_account = key_account(intent.previous);
        if live == intent.previous && !intent.previous_password {
            self.credentials
                .delete(&previous_account)
                .or_else(|error| match error {
                    CredentialState::Missing => Ok(()),
                    _ => Err("vault_unavailable"),
                })?;
        }
        if !intent.previous_password
            && !matches!(
                self.credentials.read(&previous_account).map(Zeroizing::new),
                Err(CredentialState::Missing)
            )
        {
            return Err("vault_recovery_required".into());
        }
        if live == intent.previous {
            intent.publish(&self.root)?;
        }
        let fresh = EncryptedVault::open(&vault::database_file(&self.root)?, intent.next, &key)?;
        let document = read_document(&fresh)?;
        if !document.entries.is_empty()
            || !document.grants.is_empty()
            || !document.references.is_empty()
            || !document.writers.is_empty()
            || document.revision != 0
        {
            return Err("vault_recovery_required".into());
        }
        drop(fresh);
        intent.finish(&self.root)?;
        state.failure = None;
        Ok(VaultStatus {
            state: VaultState::Locked,
            mode: "os",
        })
    }

    // Close the database and remove its live key from the session
    pub fn lock(&self) -> VaultStatus {
        self.end_session();
        self.status()
    }

    pub fn end_session(&self) {
        let mut state = self.current_state();
        if state.session.take().is_some() {
            state.reviews.clear();
            state.failure = None;
            state.locked_event_pending = true;
            self.wake.notify();
        }
    }

    pub fn builtin_choices(&self, path_field: bool) -> Result<Vec<VaultEntry>, String> {
        self.row_operation(|session, rows| {
            let mut entries: Vec<_> = rows::headers(rows)?
                .iter()
                .filter(|entry| entry.allows_builtin(path_field))
                .map(|entry| entry.metadata(session.id))
                .collect();
            entries.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
            Ok(entries)
        })
    }

    // Resolve exact rows under one lock and return the revision for the sink check
    pub(crate) fn with_builtin_references<T>(
        &self,
        path_field: bool,
        consume: impl FnOnce(&dyn Fn(&str) -> Result<String, String>) -> Result<T, String>,
    ) -> Result<(BuiltinReferenceLease, T), String> {
        self.with_builtin_reference_policy(path_field, path_field, consume)
    }

    // Path consumers other than the explicit Reveal file action never expand protected paths.
    pub(crate) fn with_unprotected_builtin_paths<T>(
        &self,
        consume: impl FnOnce(&dyn Fn(&str) -> Result<String, String>) -> Result<T, String>,
    ) -> Result<(BuiltinReferenceLease, T), String> {
        self.with_builtin_reference_policy(true, false, consume)
    }

    fn with_builtin_reference_policy<T>(
        &self,
        path_field: bool,
        allow_protected: bool,
        consume: impl FnOnce(&dyn Fn(&str) -> Result<String, String>) -> Result<T, String>,
    ) -> Result<(BuiltinReferenceLease, T), String> {
        self.row_operation(|session, rows| {
            let value = consume(&|reference| {
                let reference = TypedReference::parse(reference)?;
                if reference.vault != session.id {
                    return Err("unresolved_vault_reference".into());
                }
                let entry = rows::header(rows, reference.entry)?
                    .filter(|entry| entry.owner.is_none())
                    .ok_or("unresolved_vault_reference")?;
                if !entry.allows_builtin(path_field) || (entry.protected && !allow_protected) {
                    return Err("ineligible_vault_reference".into());
                }
                let value = rows::value(rows, entry.id)?;
                entry.kind.validate(&value.0)?;
                match (path_field, entry.kind, reference.field) {
                    (true, VaultKind::File | VaultKind::Folder, None)
                    | (false, VaultKind::Text | VaultKind::File | VaultKind::Folder, None) => value
                        .0
                        .as_str()
                        .map(str::to_owned)
                        .ok_or("invalid_value".into()),
                    (false, VaultKind::Integer | VaultKind::Boolean | VaultKind::Json, None) => {
                        Ok(value.0.to_string())
                    }
                    (false, VaultKind::Location, Some(field)) => {
                        let part = ValueBuffer(field.project(&value.0)?);
                        Ok(match &part.0 {
                            Value::String(text) => text.clone(),
                            part => part.to_string(),
                        })
                    }
                    (false, VaultKind::Location, None) => {
                        let lat = value.0["latitude"].as_f64().ok_or("invalid_value")?;
                        let lon = value.0["longitude"].as_f64().ok_or("invalid_value")?;
                        Ok(format!("{lat},{lon}"))
                    }
                    _ => Err("ineligible_vault_reference".into()),
                }
            })?;
            Ok((
                BuiltinReferenceLease {
                    epoch: session.epoch,
                    revision: rows.revision()?,
                },
                value,
            ))
        })
    }

    pub(crate) fn check_builtin_reference(
        &self,
        lease: BuiltinReferenceLease,
    ) -> Result<(), String> {
        self.row_operation(|session, rows| {
            if session.epoch == lease.epoch && rows.revision()? == lease.revision {
                Ok(())
            } else {
                Err("stale_vault_reference".into())
            }
        })
    }

    // Return only entry metadata to the trusted Manager view
    pub fn entries(&self, installed: &[VaultInstallation]) -> Result<Vec<VaultEntry>, String> {
        self.row_operation(|session, rows| {
            let mut result: Vec<_> = rows::headers(rows)?
                .iter()
                .map(|entry| {
                    let mut row = entry.metadata(session.id);
                    row.owner_verified = entry
                        .owner
                        .as_ref()
                        .map(|_| installed.iter().any(|i| entry.owned_by(i)));
                    row
                })
                .collect();
            result.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
            Ok(result)
        })
    }

    // Capture just personal rows and their revision within the same unlocked session
    pub fn personal_entries(&self) -> Result<VaultPersonalEntries, String> {
        self.row_operation(|session, rows| {
            let mut entries: Vec<_> = rows::headers(rows)?
                .iter()
                .filter(|entry| entry.is_personal())
                .map(|entry| entry.metadata(session.id))
                .collect();
            entries.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
            Ok(VaultPersonalEntries {
                revision: session.entries_revision(),
                entries,
            })
        })
    }

    // Create a user-owned entry with immutable kind and explicit protection
    pub fn create_entry(&self, input: VaultEntryInput) -> Result<VaultEntry, String> {
        let entry = Entry {
            private_slot: None,
            id: uuid::Uuid::new_v4(),
            label: input.label,
            kind: input.kind,
            protected: input.protected,
            revision: 1,
            owner: None,
            successor: None,
            value: input.value,
        };
        self.create_entry_record(entry, None)
    }

    // Validate and persist a newly admitted entry
    fn create_entry_record(
        &self,
        entry: Entry,
        epoch: Option<uuid::Uuid>,
    ) -> Result<VaultEntry, String> {
        if !valid_label(&entry.label) {
            return Err("invalid_label".into());
        }
        entry.kind.validate(&entry.value)?;
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        if epoch.is_some_and(|epoch| session.epoch != epoch) {
            return Err("vault_setup_expired".into());
        }
        session.mutate(|rows| {
            if rows.count(RowKind::Entry)? >= MAX_ENTRIES {
                return Err("vault_limit".into());
            }
            let metadata = rows::EntryHeader::from(&entry);
            rows::put_header(rows, &metadata)?;
            rows::put_value(rows, entry.id, &entry.value)?;
            Ok(metadata.metadata(session.id))
        })
    }

    // Replace an entry value without changing its type or identity
    pub fn replace_entry(
        &self,
        id: &str,
        revision: &str,
        value: Value,
    ) -> Result<VaultEntry, String> {
        let value = ValueBuffer(value);
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        session.mutate(|rows| {
            let mut entry = rows::current_header(rows, session.id, id, revision)?;
            entry.kind.validate(&value.0)?;
            entry.advance()?;
            rows::put_header(rows, &entry)?;
            rows::put_value(rows, entry.id, &value.0)?;
            Ok(entry.metadata(session.id))
        })
    }

    // Rename an entry without decrypting its value
    pub fn rename_entry(
        &self,
        id: &str,
        revision: &str,
        label: String,
    ) -> Result<VaultEntry, String> {
        if !valid_label(&label) {
            return Err("invalid_label".into());
        }
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        session.mutate(|rows| {
            let mut entry = rows::current_header(rows, session.id, id, revision)?;
            entry.advance()?;
            entry.label.zeroize();
            entry.label = label;
            rows::put_header(rows, &entry)?;
            Ok(entry.metadata(session.id))
        })
    }

    // Remove the entry and its grants in one transaction
    pub fn delete_entry(&self, id: &str, revision: &str) -> Result<(), String> {
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        session.mutate(|rows| {
            let entry = rows::current_header(rows, session.id, id, revision)?.id;
            for grant in rows::all_grants(rows)?.iter().filter(|g| g.entry == entry) {
                rows::delete_grant(rows, grant)?;
            }
            for grant in rows::all_refs(rows)?.iter().filter(|g| g.entry == entry) {
                rows::delete_ref(rows, grant)?;
            }
            rows.delete(RowKind::Entry, &entry.to_string())?;
            rows.delete(RowKind::EntryValue, &entry.to_string())?;
            Ok(())
        })
    }

    // Only the exact unlocked session can open an encrypted row transaction
    fn row_operation<T>(
        &self,
        run: impl FnOnce(&Session, &Rows<'_>) -> Result<T, String>,
    ) -> Result<T, String> {
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        session.vault.rows(|rows| run(session, rows))
    }

    // Serialize all operations with locking and avoid retaining decrypted metadata in memory
    fn operation<T>(
        &self,
        run: impl FnOnce(&Session, &mut Document) -> Result<T, String>,
    ) -> Result<T, String> {
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        let mut data = read_document(&session.vault)?;
        run(session, &mut data)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    broker_revision: Option<u64>,
    #[serde(default)]
    revoked: bool,
    #[serde(default)]
    last_read: Option<String>,
    installation: VaultInstallation,
    slot: VaultSetupSlot,
    entry: uuid::Uuid,
}

#[derive(Clone, Serialize, ts_rs::TS)]
pub struct VaultSetupSelection {
    pub action: String,
    pub slot_id: String,
    pub entry: VaultEntry,
}

pub(crate) struct VaultAssignment {
    pub entry_id: String,
    pub revision: String,
    pub epoch: uuid::Uuid,
    pub make_private: bool,
}

#[derive(Clone, Serialize, ts_rs::TS)]
pub struct VaultAccess {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    pub plugin_id: String,
    pub slot_id: String,
    pub action: String,
    pub field: String,
    pub purpose: String,
    pub direct_secret: bool,
    pub last_read: Option<String>,
    pub entry: VaultEntry,
}

#[derive(Clone, Serialize, ts_rs::TS)]
pub struct VaultPermissionActivity {
    pub plugin_id: String,
    pub action: String,
    pub field: String,
    pub entry_kind: String,
    pub purpose: String,
    pub revoked: bool,
    pub last_read: Option<String>,
    pub family: String,
    pub last_write: Option<String>,
}

// Match declared type and disclosure class without coercion
fn eligible(entry: &Entry, slot: &VaultSetupSlot) -> bool {
    eligible_kind(entry.kind, entry.protected, slot)
        && (entry.kind == VaultKind::Location
            || entry
                .value
                .as_str()
                .is_some_and(|value| value.len() <= MAX_VALUE))
}

fn eligible_kind(kind: VaultKind, protected: bool, slot: &VaultSetupSlot) -> bool {
    let (kind, class) = match kind {
        VaultKind::ApiKey => ("api-key", "secret"),
        VaultKind::BearerToken => ("bearer-token", "secret"),
        VaultKind::Credential => ("credential", "secret"),
        VaultKind::Text if !protected => ("text", "value"),
        VaultKind::Location => {
            return slot.mode == "direct"
                && slot.kind == "location"
                && slot.entry_kind == "location";
        }
        _ => return false,
    };
    kind == slot.entry_kind && class == slot.kind
}

// Prevent an older plaintext approval from becoming active again after a later downgrade
fn revoke_plaintext_grants(data: &mut Document, entry: uuid::Uuid) {
    for grant in &mut data.grants {
        if grant.entry == entry && grant.slot.kind == "value" {
            grant.revoked = true;
        }
    }
}

// Advance a document-wide fence so callers can compare separately read slots
fn write_document(session: &Session, data: &mut Document) -> Result<(), String> {
    data.revision = data.revision.checked_add(1).ok_or("vault_limit")?;
    session
        .vault
        .rows(|rows| rows::publish_document(rows, data))?;
    session.changed();
    Ok(())
}

impl Vault {
    // List metadata scoped to a trusted setup slot
    pub(crate) fn setup_entries(
        &self,
        slot: &VaultSetupSlot,
    ) -> Result<(uuid::Uuid, Vec<VaultEntry>), String> {
        self.row_operation(|session, rows| {
            let mut entries: Vec<_> = rows::headers(rows)?
                .iter()
                .filter(|entry| {
                    entry.owner.is_none() && eligible_kind(entry.kind, entry.protected, slot)
                })
                .map(|entry| entry.metadata(session.id))
                .collect();
            entries.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
            Ok((session.epoch, entries))
        })
    }

    // Create a user-owned entry only when its type matches the reviewed slot
    pub(crate) fn setup_create(
        &self,
        slot: &VaultSetupSlot,
        epoch: uuid::Uuid,
        input: VaultEntryInput,
    ) -> Result<VaultEntry, String> {
        let entry = Entry {
            private_slot: None,
            id: uuid::Uuid::new_v4(),
            label: input.label,
            kind: input.kind,
            protected: input.protected,
            revision: 1,
            owner: None,
            successor: None,
            value: input.value,
        };
        if !eligible(&entry, slot) {
            return Err("ineligible_entry".into());
        }
        self.create_entry_record(entry, Some(epoch))
    }

    // Capture entry metadata and revision only after exact explicit selection
    pub(crate) fn setup_selection(
        &self,
        slot: &VaultSetupSlot,
        id: &str,
        revision: &str,
        allow_direct_read: bool,
    ) -> Result<(VaultSetupSelection, VaultAssignment), String> {
        if slot.requires_direct_approval() != allow_direct_read {
            return Err("explicit_direct_read_approval_required".into());
        }
        self.row_operation(|session, rows| {
            let entry = rows::current_header(rows, session.id, id, revision)?;
            if entry.owner.is_some()
                || !eligible_kind(entry.kind, entry.protected, slot)
                || !rows::value_exists(rows, entry.id)?
            {
                return Err("ineligible_entry".into());
            }
            let entry = entry.metadata(session.id);
            let approval = VaultAssignment {
                entry_id: entry.id.clone(),
                revision: entry.revision.clone(),
                epoch: session.epoch,
                make_private: false,
            };
            Ok((
                VaultSetupSelection {
                    action: slot.action.clone(),
                    slot_id: slot.id.clone(),
                    entry,
                },
                approval,
            ))
        })
    }

    // Record prospective grants; they cannot match a live installation until package commit
    #[cfg(test)]
    pub(crate) fn grant_setup<T>(
        &self,
        owner: &VaultInstallation,
        slots: &[VaultSetupSlot],
        selected: &std::collections::BTreeMap<(String, String), VaultAssignment>,
        commit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.grant_setup_with_writers(owner, slots, selected, &[], None, None, commit)
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "Keep grant scope, revision fence and exact commit callback explicit"
    )]
    pub(crate) fn grant_setup_with_writers<T>(
        &self,
        owner: &VaultInstallation,
        slots: &[VaultSetupSlot],
        selected: &std::collections::BTreeMap<(String, String), VaultAssignment>,
        writers: &[stream_dock_manager_plugin_host::permissions::ActionScope],
        epoch: Option<uuid::Uuid>,
        continuity: Option<&Continuity>,
        commit: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        self.operation(|session, data| {
            if epoch.is_some_and(|e| e != session.epoch) {
                return Err("vault_setup_expired".into());
            }
            if let Some(c) = continuity {
                c.prepare(session, data, owner, selected)?;
            }
            if data.writers.len() + writers.len() > MAX_GRANTS {
                return Err("vault_limit".into());
            }
            data.writers
                .extend(writers.iter().map(|decl| writes::WriterGrant {
                    owner: owner.clone(),
                    declaration: decl.clone(),
                    revoked: false,
                    last_write: None,
                }));
            let mut grants = Vec::new();
            for slot in slots {
                let Some(selection) = selected.get(&(slot.action.clone(), slot.id.clone())) else {
                    if slot.required && !continuity.is_some_and(|c| c.slots.contains(slot)) {
                        return Err("needs_setup".into());
                    }
                    continue;
                };
                if selection.epoch != session.epoch {
                    return Err("vault_setup_expired".into());
                }
                let entry_id =
                    find_entry(session.id, data, &selection.entry_id, &selection.revision)?.id;
                if selection.make_private
                    && data
                        .grants
                        .iter()
                        .any(|grant| grant.entry == entry_id && grant.installation != *owner)
                {
                    return Err("entry_already_shared".into());
                }
                let entry = find_entry(session.id, data, &selection.entry_id, &selection.revision)?;
                if !entry.visible_to(owner) || !eligible(entry, slot) {
                    return Err("ineligible_entry".into());
                }
                if selection.make_private {
                    if slot.kind != "value" {
                        return Err("ineligible_entry".into());
                    }
                    entry.owner = Some(owner.clone());
                }
                grants.push(Grant {
                    broker_revision: None,
                    revoked: false,
                    last_read: None,
                    installation: owner.clone(),
                    slot: slot.clone(),
                    entry: entry.id,
                });
            }
            if data.grants.len() + grants.len() > MAX_GRANTS {
                return Err("vault_limit".into());
            }
            data.grants.extend(grants);
            write_document(session, data)?;
            match commit() {
                Ok(outcome) => {
                    if data
                        .grants
                        .iter()
                        .any(|g| g.installation == *owner && g.slot.mode == "broker")
                    {
                        if let Ok(decisions) =
                            self.store.permission_decisions(owner.permission_owner())
                        {
                            for grant in data.grants.iter_mut().filter(|g| g.installation == *owner)
                            {
                                if let Some(scope) = grant.slot.broker_scope() {
                                    let key = (
                                        grant.slot.action.clone(),
                                        serde_json::to_string(&scope)
                                            .expect("serializable permission"),
                                    );
                                    grant.broker_revision = decisions
                                        .scopes
                                        .get(&key)
                                        .filter(|v| **v)
                                        .map(|_| decisions.revision);
                                }
                            }
                        }
                    }
                    for e in &mut data.entries {
                        if e.successor.as_ref() == Some(owner) {
                            e.owner = e.successor.take();
                        }
                    }
                    data.references
                        .retain(|g| g.scope.owner.owner != owner.owner || g.scope.owner == *owner);
                    if let Some(continuity) = continuity {
                        continuity.transfer_references(&self.store, data, owner);
                    }
                    // Never reactivate retired grants after replacing an installation
                    data.writers
                        .retain(|g| g.owner.owner != owner.owner || g.owner == *owner);
                    data.grants.retain(|g| {
                        g.installation.owner != owner.owner || g.installation == *owner
                    });
                    let _ = write_document(session, data);
                    Ok(outcome)
                }
                Err(error) => {
                    // Restore newly entered values to the user's Entries after an ordinary failure
                    // A crash can leave only an orphan bound to the unused installation identity
                    data.writers.retain(|g| g.owner != *owner);
                    data.grants.retain(|g| g.installation != *owner);
                    for entry in &mut data.entries {
                        if entry.successor.as_ref() == Some(owner) {
                            entry.successor = None;
                        }
                        if entry.owner.as_ref() == Some(owner) {
                            entry.owner = None;
                        }
                    }
                    let _ = write_document(session, data);
                    Err(error)
                }
            }
        })
    }

    // The public barrier is written first. Clearing it follows the encrypted commit, so a
    // crash between stores can only repeat the denial. A failed reconciliation stays locked.
    fn apply_revocations(&self, session: &Session) -> Result<(), String> {
        let mut pending = REVOCATIONS.read(&self.store)?.unwrap_or_default();
        if pending.direct_reads.is_empty() && pending.private_writes.is_empty() {
            return Ok(());
        }
        session.mutate(|rows| {
            if !pending.direct_reads.is_empty() {
                for mut grant in rows::all_grants(rows)? {
                    if !grant.revoked
                        && grant.slot.requires_direct_approval()
                        && pending.direct_reads.contains(&(
                            grant.installation.owner.clone(),
                            grant.slot.action.clone(),
                        ))
                    {
                        grant.revoked = true;
                        rows::put_grant(rows, &grant)?;
                    }
                }
            }
            if !pending.private_writes.is_empty() {
                for mut grant in rows::all_writers(rows)? {
                    if !grant.revoked
                        && pending.private_writes.contains(&(
                            grant.owner.owner.clone(),
                            grant.declaration.action.clone(),
                        ))
                    {
                        grant.revoked = true;
                        rows::put_writer(rows, &grant)?;
                    }
                }
            }
            Ok(())
        })?;
        pending.direct_reads.clear();
        pending.private_writes.clear();
        REVOCATIONS.write(&self.store, &pending)
    }

    pub fn revoke_direct_reads(&self, plugin: &str, action: &str) -> Result<(), String> {
        self.revoke_action(plugin, action, false)
    }

    pub fn revoke_private_writes(&self, plugin: &str, action: &str) -> Result<(), String> {
        self.revoke_action(plugin, action, true)
    }

    fn revoke_action(&self, plugin: &str, action: &str, writes: bool) -> Result<(), String> {
        let mut state = self.current_state();
        let scope = (plugin.to_owned(), action.to_owned());
        self.store.transaction(move |tx| {
            let mut pending = REVOCATIONS.read_from(tx)?.unwrap_or_default();
            if writes {
                pending.private_writes.insert(scope);
            } else {
                pending.direct_reads.insert(scope);
            }
            pending.revision = pending.revision.checked_add(1).ok_or("vault_limit")?;
            REVOCATIONS.write_to(tx, &pending)
        })?;
        if let Some(session) = &mut state.session {
            session.cancel_location();
            session.epoch = uuid::Uuid::new_v4();
            if let Err(error) = self.apply_revocations(session) {
                state.session = None;
                state.failure = Some(VaultState::RecoveryRequired);
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn permission_activity(
        &self,
        live: &[VaultInstallation],
    ) -> Result<Vec<VaultPermissionActivity>, String> {
        self.row_operation(|_, rows| {
            let writers = rows::all_writers(rows)?;
            Ok(rows::all_grants(rows)?
                .iter()
                .filter(|g| g.slot.requires_direct_approval() && live.contains(&g.installation))
                .map(|g| VaultPermissionActivity {
                    plugin_id: g.installation.owner.clone(),
                    action: g.slot.action.clone(),
                    field: g.slot.field.clone(),
                    purpose: g.slot.purpose.clone(),
                    entry_kind: g.slot.entry_kind.clone(),
                    revoked: g.revoked,
                    last_read: g.last_read.clone(),
                    family: if g.slot.kind == "location" {"vault.read-location"} else {"vault.read-secret"}.into(),
                    last_write: None,
                })
                .chain(writers.iter().filter(|w| live.contains(&w.owner)).filter_map(|w| {
                    let stream_dock_manager_plugin_host::permissions::PermissionScope::WriteSecret { field, entry_kind, purpose } = &w.declaration.scope else { return None; };
                    Some(VaultPermissionActivity {
                        plugin_id: w.owner.owner.clone(), action: w.declaration.action.clone(),
                        family: "vault.write-secret".into(), field: field.clone(), entry_kind: entry_kind.clone(), purpose: purpose.clone(),
                        revoked: w.revoked, last_read: None, last_write: w.last_write.clone(),
                    })
                }))
                .collect())
        })
    }

    pub fn review_installed(
        &self,
        owner: VaultInstallation,
        slot: VaultSetupSlot,
        editor: u64,
    ) -> Result<InstalledVaultReview, String> {
        let mut state = self.current_state();
        let now = Instant::now();
        state.reviews.retain(|_, r| {
            r.deadline > now && !(r.editor == editor && r.slot == slot && r.owner == owner)
        });
        if state.reviews.len() >= 8 {
            return Err("rate_limited".into());
        }
        let session = state.session.as_ref().ok_or("vault_locked")?;
        let (revision, entries) = session.vault.rows(|rows| {
            let mut entries: Vec<_> = rows::headers(rows)?
                .iter()
                .filter(|entry| {
                    eligible_kind(entry.kind, entry.protected, &slot) && entry.visible_to(&owner)
                })
                .map(|entry| entry.metadata(session.id))
                .collect();
            entries.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
            Ok((rows.revision()?, entries))
        })?;
        let id = crate::review_store::new_id();
        let public = InstalledVaultReview {
            id: id.clone(),
            slot: slot.clone(),
            entries,
            inventory_sha256: owner.inventory_sha256.clone(),
        };
        let review = InstalledReview {
            owner,
            slot,
            editor,
            epoch: session.epoch,
            revision,
            deadline: now + Duration::from_secs(300),
        };
        state.reviews.insert(id, review);
        Ok(public)
    }

    pub fn cancel_installed_review(&self, id: &str, editor: u64) {
        let mut state = self.current_state();
        if state.reviews.get(id).is_some_and(|r| r.editor == editor) {
            state.reviews.remove(id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn approve_installed(
        &self,
        review_id: &str,
        editor: u64,
        owner: &VaultInstallation,
        slot: &VaultSetupSlot,
        entry_id: &str,
        entry_revision: &str,
        package_opt_in: bool,
        activate: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut state = self.current_state();
        let review = state.reviews.remove(review_id).ok_or("review_expired")?;
        let session = state.session.as_mut().ok_or("vault_locked")?;
        if !package_opt_in
            || !slot.requires_direct_approval()
            || review.editor != editor
            || &review.owner != owner
            || &review.slot != slot
            || review.epoch != session.epoch
            || Instant::now() >= review.deadline
        {
            return Err("permission_denied".into());
        }
        session.mutate(|rows| {
            if rows.revision()? != review.revision {
                return Err("stale_revision".into());
            }
            let entry = rows::current_header(rows, session.id, entry_id, entry_revision)?;
            if !eligible_kind(entry.kind, entry.protected, slot)
                || !entry.visible_to(owner)
                || !rows::value_exists(rows, entry.id)?
            {
                return Err("ineligible_entry".into());
            }
            if rows::grant(rows, owner, slot)?.is_none()
                && rows.count(RowKind::Grant)? >= MAX_GRANTS
            {
                return Err("vault_limit".into());
            }
            let grant = Grant {
                broker_revision: None,
                installation: owner.clone(),
                slot: slot.clone(),
                entry: entry.id,
                revoked: false,
                last_read: None,
            };
            // Mark the mode before issuing the grant; a crash only leaves setup incomplete
            activate()?;
            rows::put_grant(rows, &grant)
        })?;
        session.epoch = uuid::Uuid::new_v4();
        state.reviews.clear();
        Ok(())
    }

    // Report granted access by exact live installation without exposing any values
    pub fn access(&self, live: &[VaultInstallation]) -> Result<Vec<VaultAccess>, String> {
        self.row_operation(|session, rows| {
            let entries: std::collections::BTreeMap<_, _> = rows::headers(rows)?
                .into_iter().map(|entry| (entry.id, entry)).collect();
            let references = rows::all_refs(rows)?;
            Ok(rows::all_grants(rows)?
                .iter()
                .filter(|grant| !grant.revoked && live.contains(&grant.installation))
                .filter_map(|grant| {
                    let entry = entries.get(&grant.entry)?;
                    Some(VaultAccess {
                        description:grant.slot.origin.as_ref().map(|origin|format!("{} · {origin}",grant.slot.operation.as_deref().unwrap_or("use-entry"))),
                        plugin_id: grant.installation.owner.clone(),
                        slot_id: grant.slot.id.clone(),
                        action: grant.slot.action.clone(),
                        field: grant.slot.field.clone(),
                        purpose: grant.slot.purpose.clone(),
                        direct_secret: grant.slot.is_direct_secret(),
                        last_read: grant.last_read.clone(),
                        entry: entry.metadata(session.id),
                    })
                })
                .chain(references.iter().filter(|g|live.contains(&g.scope.owner)).filter_map(|g|{
                    let entry=entries.get(&g.entry)?;
                    let stream_dock_manager_plugin_host::permissions::PermissionScope::Access{mode,field,origin,operation,..}=&g.scope.declaration else {return None;};
                    Some(VaultAccess{plugin_id:g.scope.owner.owner.clone(),slot_id:g.selector(),action:g.scope.action.clone(),field:field.clone(),purpose:g.scope.purpose.clone(),direct_secret:false,last_read:None,entry:entry.metadata(session.id),description:Some(format!("{mode} · {operation} · {origin}"))})
                }))
                .collect())
        })
    }

    // Revoke a single exact slot assignment without disclosing or deleting the entry
    pub fn revoke(
        &self,
        owner: &VaultInstallation,
        slot_id: &str,
        action: &str,
        entry_id: &str,
        revision: &str,
    ) -> Result<(), String> {
        let mut state = self.current_state();
        let session = state.session.as_mut().ok_or("vault_locked")?;
        session.mutate(|rows| {
            let entry = rows::current_header(rows, session.id, entry_id, revision)?.id;
            if slot_id.starts_with("reference/") {
                let mut found = false;
                for grant in rows::references_for(rows, owner, action)?
                    .iter()
                    .filter(|grant| grant.entry == entry && grant.selector() == slot_id)
                {
                    rows::delete_ref(rows, grant)?;
                    found = true;
                }
                if !found {
                    return Err("permission_denied".into());
                }
            } else {
                let mut grant = rows::grants_for(rows, owner, action)?
                    .into_iter()
                    .find(|grant| {
                        !grant.revoked && grant.slot.id == slot_id && grant.entry == entry
                    })
                    .ok_or("permission_denied")?;
                grant.revoked = true;
                rows::put_grant(rows, &grant)?;
            }
            Ok(())
        })?;
        session.epoch = uuid::Uuid::new_v4();
        state.reviews.clear();
        Ok(())
    }
}

impl VaultRead for Vault {
    fn format_text_and_publish(
        &self,
        owner: &VaultInstallation,
        slots: &[&VaultSetupSlot],
        text: &str,
        publish: &mut dyn FnMut(&str) -> bool,
    ) -> Result<(), &'static str> {
        self.format_granted_text(owner, slots, text, publish)
    }

    fn writer_ready(
        &self,
        owner: &VaultInstallation,
        action: &str,
        scope: &stream_dock_manager_plugin_host::permissions::PermissionScope,
    ) -> bool {
        self.row_operation(|_, rows| {
            Ok(rows::writers_for(rows, owner, action)?.iter().any(|g| {
                !g.revoked
                    && &g.owner == owner
                    && g.declaration.action == action
                    && &g.declaration.scope == scope
            }))
        })
        .unwrap_or(false)
    }
    fn write_private(
        &self,
        owner: &VaultInstallation,
        scope: &stream_dock_manager_plugin_host::permissions::ActionScope,
        request: &stream_dock_manager_plugin_host::vault_writes::SecretWrite,
        alive: &std::sync::atomic::AtomicBool,
    ) -> Result<(String, String), &'static str> {
        self.private_write(owner, scope, request, alive)
    }
    fn required_ready(&self, owner: &VaultInstallation, slots: &[&VaultSetupSlot]) -> bool {
        self.row_operation(|_, rows| {
            for slot in slots {
                let Some(grant) = rows::grant(rows, owner, slot)?.filter(|grant| !grant.revoked)
                else {
                    return Ok(false);
                };
                let Some(entry) = rows::header(rows, grant.entry)? else {
                    return Ok(false);
                };
                if !eligible_kind(entry.kind, entry.protected, slot)
                    || !entry.visible_to(owner)
                    || !rows::value_exists(rows, entry.id)?
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })
        .unwrap_or(false)
    }

    // Resolve and publish one exact grant and value within the same lock epoch
    fn read_and_publish(
        &self,
        installation: &VaultInstallation,
        slot: &VaultSetupSlot,
        publish: &mut dyn FnMut(&Value, &str) -> bool,
    ) -> Result<(), &'static str> {
        if slot.mode != "direct" {
            return Err("permission_denied");
        }
        let state = self.current_state();
        let session = state.session.as_ref().ok_or("vault_locked")?;
        let mut grant = session.vault.rows(|rows| {
            let grant = rows::grant(rows, installation, slot)?
                .filter(|grant| !grant.revoked)
                .ok_or("needs_setup")?;
            let entry = rows::header(rows, grant.entry)?
                .filter(|entry| eligible_kind(entry.kind, entry.protected, slot)
                    && entry.visible_to(installation))
                .ok_or("needs_setup")?;
            let value = rows::value(rows, entry.id)?;
            entry.kind.validate(&value.0)?;
            let coordinates;
            let result = if entry.kind == VaultKind::Location {
                coordinates = serde_json::json!({"latitude":value.0["latitude"], "longitude":value.0["longitude"]});
                &coordinates
            } else {
                &value.0
            };
            if !publish(result, &rows.revision()?.to_string()) {
                return Err("cancelled".into());
            }
            Ok(grant)
        })
        .map_err(|error| match error.as_str() {
            "needs_setup" => "needs_setup",
            "cancelled" => "cancelled",
            _ => "operation_unavailable",
        })?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs().to_string());
        if timestamp.is_some() && grant.last_read != timestamp {
            grant.last_read = timestamp;
            // Usage audit changes no personal rows or grant revision
            let _ = session.vault.rows(|rows| rows::put_grant(rows, &grant));
        }
        Ok(())
    }
}

struct ValueBuffer(Value);
impl Drop for ValueBuffer {
    // Clear transient cleartext after validation or replacement
    fn drop(&mut self) {
        clear_value(&mut self.0);
    }
}

// Use a vault-specific, opaque account in the existing native credential store
// A reset generation's protected key record binds the journal to both Vault identities.
// Existing OS-backed Vaults retain their original raw hexadecimal key representation.
fn decode_os_key(
    stored: &str,
    id: uuid::Uuid,
    reset_from: Option<uuid::Uuid>,
) -> Result<Zeroizing<[u8; 32]>, String> {
    let hex_key = if let Some(wrapper) = stored.strip_prefix("reset-v1/") {
        let mut parts = wrapper.split('/');
        let previous = parts
            .next()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or("vault_recovery_required")?;
        let next = parts
            .next()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or("vault_recovery_required")?;
        let key = parts.next().ok_or("vault_recovery_required")?;
        if parts.next().is_some()
            || next != id
            || previous == next
            || reset_from.is_some_and(|expected| expected != previous)
        {
            return Err("vault_recovery_required".into());
        }
        key
    } else if reset_from.is_none() {
        stored
    } else {
        return Err("vault_recovery_required".into());
    };
    let mut key = Zeroizing::new([0; 32]);
    hex::decode_to_slice(hex_key, key.as_mut()).map_err(|_| "vault_recovery_required")?;
    Ok(key)
}

fn key_account(id: uuid::Uuid) -> String {
    format!("vault/dek/{id}")
}

// Keep unsupported files distinct from unavailable operating-system credentials
fn validate_new_password(password: &str, confirmation: &str) -> Result<(), String> {
    if password.is_empty()
        || password.len() > vault::password::MAX_PASSWORD_BYTES
        || password != confirmation
    {
        return Err("invalid_password".into());
    }
    Ok(())
}

fn error_state(code: &str) -> VaultState {
    match code {
        "vault_recovery_required" | "vault_unsupported_format" => VaultState::RecoveryRequired,
        "vault_not_initialized" => VaultState::Uninitialized,
        "invalid_password"
        | "incorrect_password"
        | "password_required"
        | "incorrect_unlock_mode" => VaultState::Locked,
        _ => VaultState::Unavailable,
    }
}

// Bound UI labels and reject hidden control characters
fn valid_label(label: &str) -> bool {
    !label.trim().is_empty() && label.len() <= 128 && !label.chars().any(char::is_control)
}

// Describe a local entry without returning its decrypted value
fn meta(vault: uuid::Uuid, entry: &Entry) -> VaultEntry {
    VaultEntry {
        id: format!("{vault}.{}", entry.id),
        label: entry.label.clone(),
        kind: entry.kind,
        protected: entry.protected,
        revision: entry.revision.to_string(),
        owner: entry.owner.as_ref().map(|owner| owner.owner.clone()),
        owner_verified: None,
    }
}

// Match the exact vault-bound entry and revision without substituting a later value
fn find_entry<'a>(
    vault: uuid::Uuid,
    document: &'a mut Document,
    id: &str,
    revision: &str,
) -> Result<&'a mut Entry, String> {
    let (vault_id, entry_id) = id.split_once('.').ok_or("entry_missing")?;
    let entry_id = uuid::Uuid::parse_str(entry_id).map_err(|_| "entry_missing")?;
    if vault_id != vault.to_string() {
        return Err("entry_missing".into());
    }
    let entry = document
        .entries
        .iter_mut()
        .find(|e| e.id == entry_id)
        .ok_or("entry_missing")?;
    if revision != entry.revision.to_string() {
        return Err("stale_entry_revision".into());
    }
    Ok(entry)
}

// Advance an entry revision on every successful mutation
fn advance(entry: &mut Entry) -> Result<(), String> {
    entry.revision = entry.revision.checked_add(1).ok_or("vault_limit")?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::credentials::CredentialState;
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap};

    #[derive(Default)]
    pub(crate) struct TestKeys(Mutex<HashMap<String, String>>);

    #[test]
    fn startup_unlocks_only_existing_os_vaults_and_preserves_manual_locks() {
        let root = tempfile::tempdir().unwrap();
        let keys = Arc::new(TestKeys::default());
        let storage = Storage::open(root.path().join("manager-store")).unwrap();
        let open = || Vault::new(root.path().into(), keys.clone(), storage.clone());
        let vault = open();
        assert!(!vault.startup_locked(true));
        assert_eq!(vault.status().state, VaultState::Uninitialized);
        vault.initialize().unwrap();
        drop(vault);
        let vault = open();
        assert!(vault.startup_locked(false));
        assert_eq!(vault.status().state, VaultState::Locked);
        assert!(!vault.startup_locked(true));
        assert_eq!(vault.status().state, VaultState::Unlocked);
        vault.lock();
        assert_eq!(vault.status().state, VaultState::Locked);
        drop(vault);
        let path = vault::database_file(root.path()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let stored = std::mem::take(&mut *keys.0.lock().unwrap());
        let vault = open();
        assert!(vault.startup_locked(true));
        assert_eq!(vault.status().state, VaultState::Unavailable);
        assert_eq!(std::fs::read(path).unwrap(), before);
        *keys.0.lock().unwrap() = stored;
        vault.state.lock().unwrap().retry_after = None;
        assert_eq!(vault.unlock().unwrap().state, VaultState::Unlocked);
    }

    #[test]
    fn encrypted_vault_and_envelope_relocate_together_without_changing_entry_identity() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("fixture-storage")).unwrap();
        let keys = Arc::new(TestKeys::default());
        let open = || Vault::new(root.path().into(), keys.clone(), store.clone());
        let password = "fixture master password for relocation";
        let vault = open();
        vault.initialize_password(password, password).unwrap();
        let expected = entry(&vault, VaultKind::Text, "Relocated entry", "fixture text");
        vault.lock();
        drop(vault);
        let databases = vault::database_dir(root.path()).unwrap();
        let nested = databases.join(vault::FILE_NAME);
        let envelope = std::fs::read_dir(&databases)
            .unwrap()
            .map(|item| item.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".keystore-key-")
            })
            .unwrap();
        let flat = root.path().join(vault::FILE_NAME);
        let flat_envelope = root.path().join(envelope.file_name().unwrap());
        let db_bytes = std::fs::read(&nested).unwrap();
        let envelope_bytes = std::fs::read(&envelope).unwrap();
        std::fs::rename(&nested, &flat).unwrap();
        std::fs::rename(&envelope, &flat_envelope).unwrap();

        let vault = open();
        assert_eq!(vault.status().state, VaultState::RecoveryRequired);
        assert!(vault.initialize().is_err());
        assert!(!nested.exists());
        stream_dock_manager_storage::prepare_database_layout(root.path()).unwrap();
        assert!(!flat.exists());
        assert!(!flat_envelope.exists());
        assert_eq!(std::fs::read(&nested).unwrap(), db_bytes);
        assert_eq!(std::fs::read(&envelope).unwrap(), envelope_bytes);
        vault.unlock_password(password).unwrap();
        let row = vault.personal_entries().unwrap().entries.remove(0);
        assert_eq!(row.id, expected.id);
        assert_eq!(row.revision, expected.revision);
        assert_eq!(
            vault
                .reveal_entry(
                    vault.begin_entry_reveal().unwrap(),
                    &row.id,
                    &row.revision,
                    None
                )
                .unwrap()
                .value,
            json!("fixture text"),
        );
    }

    #[test]
    // Verify plugin settings and the locked personal Vault use separate current files
    fn current_database_names_keep_plugin_settings_and_personal_vault_separate() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path();
        let keys = Arc::new(TestKeys::default());
        let open = || {
            crate::Runtime::build_credentials(
                config.into(),
                false,
                crate::credentials::Credentials::with_provider(keys.clone()),
            )
            .unwrap()
        };
        let password = "fixture storage password";
        let runtime = open();
        runtime
            .vault
            .initialize_password(password, password)
            .unwrap();
        let original = entry(
            &runtime.vault,
            VaultKind::Text,
            "Preserved location",
            "fixture",
        );
        let plugin_value = json!({"protected-setting": "fixture SDK settings"});
        runtime
            .profiles
            .storage()
            .write_json(
                Domain::PluginGlobals,
                "fixture.settings",
                &plugin_value,
                4096,
            )
            .unwrap();
        runtime.shutdown();
        drop(runtime);

        let databases = stream_dock_manager_storage::database_directory(config);
        let runtime = open();
        assert!(runtime.vault.startup_locked(false));
        assert_eq!(runtime.vault.status().state, VaultState::Locked);
        assert_eq!(
            runtime
                .profiles
                .storage()
                .read_json::<Value>(Domain::PluginGlobals, "fixture.settings", 4096,)
                .unwrap(),
            Some(plugin_value)
        );
        runtime.vault.unlock_password(password).unwrap();
        let restored = runtime.vault.personal_entries().unwrap().entries.remove(0);
        assert_eq!(restored.id, original.id);
        assert_eq!(restored.revision, original.revision);
        assert_eq!(
            runtime
                .vault
                .reveal_entry(
                    runtime.vault.begin_entry_reveal().unwrap(),
                    &restored.id,
                    &restored.revision,
                    None,
                )
                .unwrap()
                .value,
            json!("fixture")
        );
        assert!(databases.join("keystore.sqlite3").is_file());
        assert!(databases.join("datastore.sqlite3").is_file());
        runtime.shutdown();
    }

    impl CredentialProvider for TestKeys {
        // Return an isolated synthetic keyring fixture
        fn read(&self, account: &str) -> Result<String, CredentialState> {
            self.0
                .lock()
                .unwrap()
                .get(account)
                .cloned()
                .ok_or(CredentialState::Missing)
        }
        // Save only to the in-memory fixture
        fn write(&self, account: &str, value: &str) -> Result<(), CredentialState> {
            self.0.lock().unwrap().insert(account.into(), value.into());
            Ok(())
        }
        // Remove only the fixture item
        fn delete(&self, account: &str) -> Result<(), CredentialState> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    // Use consistent scope and artifact identities across vault fixture calls
    fn owner(suffix: &str) -> VaultInstallation {
        VaultInstallation {
            owner: "fixture".into(),
            token: format!("token-{suffix}"),
            inventory_sha256: format!("hash-{suffix}"),
            storage_generation: "fixture-epoch".into(),
        }
    }

    // Create a single action slot with the requested data class
    fn slot(kind: &str, entry_kind: &str) -> VaultSetupSlot {
        VaultSetupSlot {
            id: format!("fixture-{kind}"),
            kind: kind.into(),
            entry_kind: entry_kind.into(),
            mode: "direct".into(),
            required: true,
            purpose: "Synthetic test scope".into(),
            field: "account".into(),
            action: "fixture.action".into(),
            ..Default::default()
        }
    }

    // Create a disposable typed entry and return its metadata only
    fn entry(vault: &Vault, kind: VaultKind, label: &str, value: &str) -> VaultEntry {
        vault
            .create_entry(VaultEntryInput {
                label: label.into(),
                kind,
                protected: false,
                value: json!(value),
            })
            .unwrap()
    }

    #[test]
    fn personal_metadata_snapshot_tracks_durable_mutations_and_lock_epoch() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let vault = Vault::new(root.path().into(), Arc::new(TestKeys::default()), store);
        let before = vault.metadata_revision();
        assert_eq!(before.state, VaultState::Uninitialized);
        assert_eq!(before.entries_revision, None);
        assert!(vault.personal_entries().is_err());

        vault.initialize().unwrap();
        let snapshot = vault.personal_entries().unwrap();
        assert_eq!(
            Some(snapshot.revision.clone()),
            vault.metadata_revision().entries_revision
        );
        assert!(snapshot.entries.is_empty());
        let second = entry(&vault, VaultKind::Text, "Zulu", "fixture confidential");
        let first = entry(&vault, VaultKind::Text, "Alpha", "fixture personal");
        let fresh = vault.personal_entries().unwrap();
        assert_ne!(snapshot.revision, fresh.revision);
        assert_eq!(
            fresh
                .entries
                .iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ["Alpha", "Zulu"]
        );
        assert!(!serde_json::to_string(&fresh)
            .unwrap()
            .contains("fixture confidential"));
        let renamed = vault
            .rename_entry(&first.id, &first.revision, "Beta".into())
            .unwrap();
        let revised = vault.personal_entries().unwrap();
        assert_ne!(fresh.revision, revised.revision);
        assert_eq!(revised.entries[0].label, "Beta");
        assert!(vault
            .replace_entry(&first.id, &first.revision, json!("invalid stale"))
            .is_err());
        assert_eq!(
            Some(revised.revision),
            vault.metadata_revision().entries_revision
        );

        let install = owner("private");
        let slot = slot("value", "text");
        let (_, mut assignment) = vault
            .setup_selection(&slot, &second.id, &second.revision, false)
            .unwrap();
        assignment.make_private = true;
        let before_owned = vault.metadata_revision().entries_revision;
        vault
            .grant_setup(
                &install,
                std::slice::from_ref(&slot),
                &BTreeMap::from([((slot.action.clone(), slot.id.clone()), assignment)]),
                || Ok(()),
            )
            .unwrap();
        let personal = vault.personal_entries().unwrap();
        assert_ne!(before_owned, Some(personal.revision));
        assert_eq!(personal.entries.len(), 1);
        assert_eq!(personal.entries[0].id, renamed.id);
        assert_eq!(vault.entries(&[install]).unwrap().len(), 2);

        let before_lock = vault.metadata_revision().entries_revision;
        vault.lock();
        let locked = vault.metadata_revision();
        assert_eq!(locked.state, VaultState::Locked);
        assert_eq!(locked.entries_revision, None);
        assert!(vault.personal_entries().is_err());
        vault.unlock().unwrap();
        let reopened = vault.metadata_revision();
        assert_eq!(reopened.state, VaultState::Unlocked);
        assert_ne!(before_lock, reopened.entries_revision);
        assert_eq!(
            reopened.entries_revision,
            Some(vault.personal_entries().unwrap().revision)
        );

        vault
            .state
            .lock()
            .unwrap()
            .session
            .as_mut()
            .unwrap()
            .last_activity = Instant::now() - IDLE_TIMEOUT;
        assert_eq!(vault.metadata_revision().entries_revision, None);
        assert!(vault.personal_entries().is_err());
    }

    #[test]
    fn private_writes_bind_owner_revision_and_locked_revocation() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use stream_dock_manager_plugin_host::{
            permissions::{ActionScope, PermissionScope},
            vault_writes::SecretWrite,
        };
        let root = tempfile::tempdir().unwrap();
        let keys = Arc::new(TestKeys::default());
        let store = Storage::open(root.path().join("state")).unwrap();
        let open = || Vault::new(root.path().into(), keys.clone(), store.clone());
        let vault = open();
        vault.initialize().unwrap();
        let first = owner("first");
        let second = VaultInstallation {
            owner: "second".into(),
            ..owner("second")
        };
        let scope = ActionScope {
            action: "fixture.store".into(),
            required: true,
            scope: PermissionScope::WriteSecret {
                field: "credential".into(),
                entry_kind: "api-key".into(),
                purpose: "Store credential".into(),
            },
        };
        let mut request = SecretWrite {
            operation: "create".into(),
            slot: "account".into(),
            field: "credential".into(),
            entry_kind: "api-key".into(),
            action: Some(scope.action.clone()),
            expected_revision: None,
            value: "synthetic-private".into(),
        };
        let alive = AtomicBool::new(true);
        for actor in [&first, &second] {
            assert_eq!(
                vault.private_write(actor, &scope, &request, &alive),
                Err("permission_denied")
            );
            vault
                .grant_setup_with_writers(
                    actor,
                    &[],
                    &BTreeMap::new(),
                    std::slice::from_ref(&scope),
                    None,
                    None,
                    || Ok(()),
                )
                .unwrap();
        }
        let before_create = vault.metadata_revision().entries_revision;
        let a = vault
            .private_write(&first, &scope, &request, &alive)
            .unwrap();
        assert_ne!(before_create, vault.metadata_revision().entries_revision);
        assert!(vault.personal_entries().unwrap().entries.is_empty());
        let b = vault
            .private_write(&second, &scope, &request, &alive)
            .unwrap();
        assert_ne!(a.0, b.0);
        assert_eq!(a.1, "1");
        assert_eq!(
            vault.private_write(&first, &scope, &request, &alive),
            Err("slot_conflict")
        );
        request.operation = "rotate".into();
        request.expected_revision = Some("1".into());
        alive.store(false, Ordering::Release);
        assert_eq!(
            vault.private_write(&first, &scope, &request, &alive),
            Err("cancelled")
        );
        alive.store(true, Ordering::Release);
        assert_eq!(
            vault
                .private_write(&first, &scope, &request, &alive)
                .unwrap(),
            (a.0, "2".into())
        );
        assert_eq!(
            vault.private_write(&first, &scope, &request, &alive),
            Err("stale_revision")
        );
        vault.lock();
        assert_eq!(
            vault.private_write(&second, &scope, &request, &alive),
            Err("vault_locked")
        );
        vault
            .revoke_private_writes(&first.owner, &scope.action)
            .unwrap();
        drop(vault);
        let vault = open();
        vault.unlock().unwrap();
        assert_eq!(
            vault.private_write(&first, &scope, &request, &alive),
            Err("permission_denied")
        );
        assert_eq!(
            vault.private_write(&owner("reinstalled"), &scope, &request, &alive),
            Err("permission_denied")
        );
        assert_eq!(
            vault
                .private_write(&second, &scope, &request, &alive)
                .unwrap(),
            (b.0, "2".into())
        );
        let next = VaultInstallation {
            token: "next".into(),
            inventory_sha256: "next-inventory".into(),
            ..second.clone()
        };
        let same_scope = std::slice::from_ref(&scope);
        let review = || {
            vault
                .review_continuity(second.clone(), &[], same_scope, None)
                .unwrap()
        };
        let old_review = review();
        assert_eq!(old_review.writers.len(), 1);
        assert!(vault
            .grant_setup_with_writers(
                &next,
                &[],
                &Default::default(),
                same_scope,
                None,
                Some(&old_review),
                || Err::<(), _>("test rollback".into())
            )
            .is_err());
        request.expected_revision = Some("2".into());
        assert_eq!(
            vault.private_write(&next, &scope, &request, &alive),
            Err("permission_denied")
        );
        vault
            .grant_setup_with_writers(
                &next,
                &[],
                &Default::default(),
                same_scope,
                None,
                Some(&review()),
                || Ok(()),
            )
            .unwrap();
        assert!(vault
            .private_write(&second, &scope, &request, &alive)
            .is_err());
        let rotated = vault
            .private_write(&next, &scope, &request, &alive)
            .unwrap();
        assert_eq!(rotated.1, "3");
        let activity = vault.permission_activity(&[first, next]).unwrap();
        assert_eq!(activity.len(), 2);
        assert!(activity
            .iter()
            .all(|row| row.last_write.is_some() && row.last_read.is_none()));
        assert!(
            activity
                .iter()
                .find(|r| r.plugin_id == "fixture")
                .unwrap()
                .revoked
        );
        assert!(!serde_json::to_string(&activity)
            .unwrap()
            .contains("synthetic-private"));
    }

    // Verify encrypted approvals survive restart and never authorize another installed artifact
    #[test]
    fn password_vault_creation_rotation_restart_and_forgotten_password_reset() {
        let root = tempfile::tempdir().unwrap();
        let keys = Arc::new(TestKeys::default());
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let open = || Vault::new(root.path().into(), keys.clone(), store.clone());
        let vault = open();
        let original = "first synthetic password";
        let replacement = "second synthetic password";
        let path = vault::database_file(root.path()).unwrap();
        assert!(vault.initialize_password(original, "mismatch").is_err());
        assert!(vault
            .initialize_password(&"a".repeat(1025), &"a".repeat(1025))
            .is_err());
        assert!(!path.exists());
        assert_eq!(
            vault.initialize_password(original, original).unwrap().mode,
            "password"
        );
        let entry = entry(
            &vault,
            VaultKind::Text,
            "Fixture name",
            "fixture-private-value",
        );
        let id = vault::identity(&path).unwrap();
        assert!(keys.0.lock().unwrap().is_empty());
        vault.lock();
        drop(vault);
        let vault = open();
        assert_eq!(vault.status().mode, "password");
        assert!(vault.startup_locked(true));
        assert_eq!(vault.status().state, VaultState::Locked);
        assert_eq!(vault.unlock().err().as_deref(), Some("password_required"));
        vault.state.lock().unwrap().retry_after = None;
        assert_eq!(
            vault.unlock_password("wrong").err().as_deref(),
            Some("incorrect_password")
        );
        assert_eq!(
            vault.unlock_password(original).err().as_deref(),
            Some("rate_limited")
        );
        vault.state.lock().unwrap().retry_after = None;
        vault.unlock_password(original).unwrap();
        assert!(vault
            .change_password(original, replacement, original)
            .is_err());
        assert!(vault
            .change_password("wrong", replacement, replacement)
            .is_err());
        assert_eq!(vault.entries(&[]).unwrap()[0].id, entry.id);
        vault.state.lock().unwrap().retry_after = None;
        vault
            .change_password(original, replacement, replacement)
            .unwrap();
        vault.lock();
        assert_eq!(
            vault.unlock_password(original).err().as_deref(),
            Some("incorrect_password")
        );
        drop(vault);
        let vault = open();
        vault.unlock_password(replacement).unwrap();
        assert_eq!(vault.entries(&[]).unwrap()[0].id, entry.id);
        let bytes = std::fs::read(&path).unwrap();
        for private in [
            original,
            replacement,
            "Fixture name",
            "fixture-private-value",
        ] {
            assert!(!bytes
                .windows(private.len())
                .any(|w| w == private.as_bytes()));
        }
        assert!(keys.0.lock().unwrap().is_empty());
        // A password generation does not own an OS wrapper, even at a matching synthetic name.
        keys.write(&key_account(id), "unrelated fixture").unwrap();
        vault.lock();
        let review = vault.review_keystore_reset(1).unwrap();
        vault
            .delete_keystore(1, &review, "CLEAR VAULT ENTRIES")
            .unwrap();
        assert_eq!(keys.read(&key_account(id)).unwrap(), "unrelated fixture");
        assert_eq!(vault.status().mode, "os");
        vault.unlock().unwrap();
        assert!(vault.entries(&[]).unwrap().is_empty());
        assert_ne!(vault::identity(&path).unwrap(), id);
    }

    #[test]
    fn direct_reads_fence_owner_approval_lock_rotation_and_revoke() {
        let root = tempfile::tempdir().unwrap();
        let keys = Arc::new(TestKeys::default());
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let vault = Vault::new(root.path().into(), keys.clone(), store.clone());
        assert_eq!(vault.status().state, VaultState::Uninitialized);
        assert_eq!(vault.initialize().unwrap().state, VaultState::Unlocked);
        let secret = entry(
            &vault,
            VaultKind::ApiKey,
            "Synthetic protected label",
            "synthetic-secret-value",
        );
        let username = entry(
            &vault,
            VaultKind::Text,
            "Synthetic owner label",
            "synthetic-username",
        );
        let secret_slot = slot("secret", "api-key");
        let text_slot = slot("value", "text");
        let install = owner("original");
        assert!(vault
            .setup_selection(&secret_slot, &secret.id, &secret.revision, false)
            .is_err());
        let (_, secret_approval) = vault
            .setup_selection(&secret_slot, &secret.id, &secret.revision, true)
            .unwrap();
        let (_, mut user_approval) = vault
            .setup_selection(&text_slot, &username.id, &username.revision, false)
            .unwrap();
        user_approval.make_private = true;
        let approvals = BTreeMap::from([
            (
                (secret_slot.action.clone(), secret_slot.id.clone()),
                secret_approval,
            ),
            (
                (text_slot.action.clone(), text_slot.id.clone()),
                user_approval,
            ),
        ]);
        vault
            .grant_setup(
                &install,
                &[secret_slot.clone(), text_slot.clone()],
                &approvals,
                || Ok(()),
            )
            .unwrap();
        let mut value = String::new();
        let mut version = String::new();
        let read = |vault: &Vault, owner: &VaultInstallation, slot: &VaultSetupSlot| {
            let mut result = None;
            vault
                .read_and_publish(owner, slot, &mut |value, rev| {
                    result = Some((value.to_owned(), rev.to_owned()));
                    true
                })
                .map(|_| result.unwrap())
        };
        let before_read_usage = vault.metadata_revision().entries_revision;
        vault
            .read_and_publish(&install, &secret_slot, &mut |v, r| {
                value = v.as_str().unwrap().into();
                version = r.into();
                true
            })
            .unwrap();
        assert_eq!(value, "synthetic-secret-value");
        assert_eq!(
            before_read_usage,
            vault.metadata_revision().entries_revision
        );
        assert_eq!(
            read(&vault, &install, &text_slot).unwrap(),
            ("synthetic-username".into(), version)
        );
        assert!(vault.setup_entries(&text_slot).unwrap().1.is_empty());
        assert_eq!(
            vault
                .entries(&[])
                .unwrap()
                .iter()
                .find(|e| e.id == username.id)
                .unwrap()
                .owner
                .as_deref(),
            Some("fixture")
        );
        for (verified, current) in [
            (true, Some(install.clone())),
            (false, Some(owner("replacement"))),
            (
                false,
                Some(VaultInstallation {
                    inventory_sha256: "changed".into(),
                    ..install.clone()
                }),
            ),
            (
                false,
                Some(VaultInstallation {
                    storage_generation: "restored".into(),
                    ..install.clone()
                }),
            ),
            (false, None),
        ] {
            let rows = vault.entries(current.as_slice()).unwrap();
            let private = rows.iter().find(|row| row.id == username.id).unwrap();
            assert_eq!(private.owner_verified, Some(verified));
            assert!(rows
                .iter()
                .find(|row| row.id == secret.id)
                .unwrap()
                .owner_verified
                .is_none());
            assert!(!serde_json::to_string(private)
                .unwrap()
                .contains(&install.token));
        }
        assert_eq!(
            read(&vault, &owner("replacement"), &secret_slot),
            Err("needs_setup")
        );
        vault.lock();
        assert!(vault.entries(&[]).is_err());
        assert_eq!(read(&vault, &install, &secret_slot), Err("vault_locked"));
        drop(vault);
        let vault = Vault::new(root.path().into(), keys, store);
        assert_eq!(vault.status().state, VaultState::Locked);
        vault.unlock().unwrap();
        let replaced = vault
            .replace_entry(
                &secret.id,
                &secret.revision,
                json!("synthetic-rotated-secret"),
            )
            .unwrap();
        assert_eq!(
            read(&vault, &install, &secret_slot).unwrap().0,
            "synthetic-rotated-secret"
        );
        vault
            .revoke(
                &install,
                &secret_slot.id,
                &secret_slot.action,
                &secret.id,
                &replaced.revision,
            )
            .unwrap();
        assert_eq!(read(&vault, &install, &secret_slot), Err("needs_setup"));
        assert_eq!(vault.access(&[install]).unwrap().len(), 1);
        for file in std::fs::read_dir(root.path()).unwrap() {
            let file = file.unwrap().path();
            if !file.is_file() {
                continue;
            }
            let bytes = std::fs::read(file).unwrap();
            for secret in [
                "synthetic-secret-value",
                "synthetic-rotated-secret",
                "Synthetic protected label",
                "synthetic-username",
                "token-original",
                "fixture-secret",
            ] {
                assert!(!bytes
                    .windows(secret.len())
                    .any(|window| window == secret.as_bytes()));
            }
        }
    }

    #[test]
    fn coarse_revoke_while_locked_fences_restart_and_only_the_approved_action() {
        let root = tempfile::tempdir().unwrap();
        let keys = Arc::new(TestKeys::default());
        let storage = Storage::open(root.path().join("manager-store")).unwrap();
        let vault = Vault::new(root.path().into(), keys.clone(), storage.clone());
        vault.initialize().unwrap();
        let owner = owner("stable");
        let secret = entry(
            &vault,
            VaultKind::ApiKey,
            "Private metadata",
            "synthetic test",
        );
        let first = slot("secret", "api-key");
        let second = VaultSetupSlot {
            action: "fixture.other".into(),
            required: false,
            ..first.clone()
        };
        let slots = [first.clone(), second.clone()];
        let (_, approval) = vault
            .setup_selection(&first, &secret.id, &secret.revision, true)
            .unwrap();
        vault
            .grant_setup(
                &owner,
                &slots,
                &BTreeMap::from([((first.action.clone(), first.id.clone()), approval)]),
                || Ok(()),
            )
            .unwrap();
        assert!(vault.required_ready(&owner, &[&first]));
        assert!(!vault.required_ready(&owner, &[&second]));
        vault.lock();
        vault
            .revoke_direct_reads(&owner.owner, &first.action)
            .unwrap();
        assert!(vault
            .permission_activity(std::slice::from_ref(&owner))
            .is_err());
        drop(vault);
        let vault = Vault::new(root.path().into(), keys, storage);
        vault.unlock().unwrap();
        assert!(!vault.required_ready(&owner, &[&first]));
        let activity = vault
            .permission_activity(std::slice::from_ref(&owner))
            .unwrap();
        assert_eq!(activity.len(), 1);
        assert!(activity[0].revoked);
        assert!(activity[0].last_read.is_none());
        assert!(vault
            .read_and_publish(&owner, &first, &mut |_, _| panic!("revoked"))
            .is_err());
    }

    fn confirm_reset(vault: &Vault, phrase: &str) -> Result<VaultStatus, String> {
        let review = vault.review_keystore_reset(7)?;
        vault.delete_keystore(7, &review, phrase)
    }

    #[test]
    fn confirmed_reset_recovers_across_key_and_publication_boundaries() {
        for step in 0..=5 {
            let root = tempfile::tempdir().unwrap();
            let store = Storage::open(root.path().join("manager-store")).unwrap();
            let keys = Arc::new(TestKeys::default());
            let vault = Vault::new(root.path().into(), keys.clone(), store.clone());
            vault.initialize().unwrap();
            let original = entry(&vault, VaultKind::Text, "Entry", "sensitive fixture");
            let live = vault::database_file(root.path()).unwrap();
            let old = vault::identity(&live).unwrap();
            keys.write("unrelated-profile-key", "unrelated").unwrap();
            assert!(confirm_reset(&vault, "delete keystore").is_err());
            assert_eq!(vault.entries(&[]).unwrap().len(), 1);
            vault.lock();
            let cancelled = vault.review_keystore_reset(7).unwrap();
            assert!(vault
                .delete_keystore(8, &cancelled, "CLEAR VAULT ENTRIES")
                .is_err());
            vault.cancel_keystore_reset(7, &cancelled);
            assert!(vault
                .delete_keystore(7, &cancelled, "CLEAR VAULT ENTRIES")
                .is_err());
            let intent = vault::ResetIntent::begin(root.path()).unwrap();
            let next = intent.next;
            let key = vault::data_key().unwrap();
            if step >= 1 {
                keys.write(
                    &key_account(next),
                    &format!("reset-v1/{old}/{next}/{}", hex::encode(key.as_slice())),
                )
                .unwrap();
            }
            if step >= 2 {
                intent.stage_empty_rows(root.path(), &key).unwrap();
            }
            if step >= 3 {
                keys.delete(&key_account(old)).unwrap();
            }
            if step >= 4 {
                intent.publish(root.path()).unwrap();
            }
            let obstacle = vault::database_dir(root.path())
                .unwrap()
                .join(".keystore-stage-unrecognized");
            if step == 5 {
                std::fs::write(&obstacle, b"unrelated file").unwrap();
            }
            drop(vault);
            let restarted = Vault::new(root.path().into(), keys.clone(), store);
            assert_eq!(restarted.status().state, VaultState::RecoveryRequired);
            assert!(restarted.initialize().is_err());
            assert!(restarted.unlock().is_err());
            if step == 5 {
                assert!(confirm_reset(&restarted, "CLEAR VAULT ENTRIES").is_err());
                assert!(vault::reset_pending(root.path()).unwrap());
                assert_eq!(std::fs::read(&obstacle).unwrap(), b"unrelated file");
                std::fs::rename(&obstacle, root.path().join("unrelated")).unwrap();
            }
            assert_eq!(
                confirm_reset(&restarted, "CLEAR VAULT ENTRIES")
                    .unwrap()
                    .state,
                VaultState::Locked
            );
            assert_eq!(vault::identity(&live).unwrap(), next);
            assert!(!vault::reset_pending(root.path()).unwrap());
            assert_eq!(keys.read(&key_account(old)), Err(CredentialState::Missing));
            assert_eq!(keys.read("unrelated-profile-key").unwrap(), "unrelated");
            restarted.unlock().unwrap();
            assert!(restarted.entries(&[]).unwrap().is_empty());
            assert!(restarted
                .with_builtin_references(false, |get| get(&original.id))
                .is_err());
        }
    }

    #[test]
    fn reset_preserves_unrelated_key_and_rejects_an_unexpected_link() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let keys = Arc::new(TestKeys::default());
        let vault = Vault::new(root.path().into(), keys.clone(), store);
        vault.initialize().unwrap();
        vault.lock();
        let intent = vault::ResetIntent::begin(root.path()).unwrap();
        keys.write(&key_account(intent.next), &"f".repeat(64))
            .unwrap();
        assert!(confirm_reset(&vault, "CLEAR VAULT ENTRIES").is_err());
        assert_eq!(
            keys.read(&key_account(intent.next)).unwrap(),
            "f".repeat(64)
        );
        assert!(keys.read(&key_account(intent.previous)).is_ok());
        keys.delete(&key_account(intent.next)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let unrelated = root.path().join("unrelated");
            std::fs::write(&unrelated, b"preserve").unwrap();
            symlink(
                &unrelated,
                vault::database_dir(root.path())
                    .unwrap()
                    .join(format!(".keystore-reset-{}.sqlite3", intent.next)),
            )
            .unwrap();
            assert!(confirm_reset(&vault, "CLEAR VAULT ENTRIES").is_err());
            assert_eq!(std::fs::read(unrelated).unwrap(), b"preserve");
            assert!(keys.read(&key_account(intent.previous)).is_ok());
        }
    }

    #[test]
    fn location_request_tracks_editor_and_cancels_on_mutation_or_lock() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let vault = Vault::new(root.path().into(), Arc::new(TestKeys::default()), store);
        let id = uuid::Uuid::new_v4();
        assert!(vault.begin_location(id, 10).is_err());
        vault.initialize().unwrap();
        let first = vault.begin_location(id, 10).unwrap();
        assert!(vault.begin_location(uuid::Uuid::new_v4(), 10).is_err());
        vault.cancel_location(id, 11);
        assert!(!first.load(std::sync::atomic::Ordering::SeqCst));
        vault.cancel_location(id, 10);
        let second = vault.begin_location(id, 11).unwrap();
        assert!(first.load(std::sync::atomic::Ordering::SeqCst));
        assert!(vault.finish_location(&first).is_err());
        vault.finish_location(&second).unwrap();
        assert!(vault.finish_location(&second).is_err());
        let pending = vault.begin_location(id, 11).unwrap();
        let fix = serde_json::json!({"name":"Fixture","latitude":0,"longitude":-180,"accuracy_m":21,"source":"os"});
        vault
            .create_entry(VaultEntryInput {
                label: "Home".into(),
                kind: VaultKind::Location,
                protected: true,
                value: fix.clone(),
            })
            .unwrap();
        assert!(pending.load(std::sync::atomic::Ordering::SeqCst));
        let mut bad = fix;
        bad["accuracy_m"] = serde_json::json!(-1);
        assert!(VaultKind::Location.validate(&bad).is_err());
        let last = vault.begin_location(id, 11).unwrap();
        vault.lock();
        assert!(last.load(std::sync::atomic::Ordering::SeqCst));
        assert!(vault.finish_location(&last).is_err());
    }

    #[test]
    fn builtin_refs_share_picker_types_and_cannot_resolve_a_secret_or_foreign_id() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let vault = Vault::new(root.path().into(), Arc::new(TestKeys::default()), store);
        vault.initialize().unwrap();
        let text = entry(&vault, VaultKind::Text, "Template", "${run.fixture}");
        let secret = entry(&vault, VaultKind::ApiKey, "Secret", "fixture-private");
        let path = entry(
            &vault,
            VaultKind::File,
            "Path",
            root.path().join("does-not-exist").to_str().unwrap(),
        );
        let (epoch, result) = vault
            .with_builtin_references(false, |get| get(&text.id))
            .unwrap();
        assert_eq!(result, "${run.fixture}");
        assert_eq!(vault.builtin_choices(false).unwrap().len(), 2);
        assert_eq!(vault.builtin_choices(true).unwrap()[0].id, path.id);
        assert!(vault
            .with_builtin_references(false, |get| get(&secret.id))
            .is_err());
        assert_eq!(
            vault
                .with_builtin_references(false, |get| get(&path.id))
                .unwrap()
                .1,
            root.path().join("does-not-exist").to_str().unwrap()
        );
        assert!(vault
            .with_builtin_references(true, |get| get(&text.id))
            .is_err());
        let protected_path = vault
            .create_entry(VaultEntryInput {
                kind: VaultKind::Folder,
                label: "Protected folder".into(),
                protected: true,
                value: serde_json::json!(root.path().to_str().unwrap()),
            })
            .unwrap();
        assert!(vault
            .with_builtin_references(true, |get| get(&protected_path.id))
            .is_ok());
        assert!(vault
            .with_unprotected_builtin_paths(|get| get(&protected_path.id))
            .is_err());
        assert!(vault
            .with_unprotected_builtin_paths(|get| get(&path.id))
            .is_ok());
        let foreign = format!(
            "{}.{}",
            uuid::Uuid::new_v4(),
            text.id.split_once('.').unwrap().1
        );
        assert!(vault
            .with_builtin_references(false, |get| get(&foreign))
            .is_err());
        vault.lock();
        assert!(vault.check_builtin_reference(epoch).is_err());
        assert!(vault.builtin_choices(false).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn vault_presentation_wakes_subscribers_on_unlock_edit_and_lock() {
        let root = tempfile::tempdir().unwrap();
        let keys = crate::credentials::Credentials::with_provider(Arc::new(TestKeys::default()));
        let runtime =
            crate::Runtime::build_credentials(root.path().join("app"), false, keys).unwrap();
        let events = runtime.subscribe();
        Vault::start_idle_monitor(&runtime);
        let next_presentation = || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let event = events
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("Vault presentation change was not published");
                if event.kind == "vault-presentation-changed" {
                    assert!(event.message.is_none());
                    break;
                }
            }
        };
        runtime.vault.initialize().unwrap();
        next_presentation();
        let before = runtime.vault.presentation_revision();
        assert!(before.is_some());
        entry(&runtime.vault, VaultKind::Text, "Label", "Fixture");
        next_presentation();
        assert_ne!(before, runtime.vault.presentation_revision());
        runtime.vault.lock();
        next_presentation();
        assert!(runtime.vault.presentation_revision().is_none());
        runtime.vault.stop();
    }

    #[test]
    fn inactivity_expires_at_admission_and_user_activity_cannot_reopen_it() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let vault = Vault::new(root.path().into(), Arc::new(TestKeys::default()), store);
        vault.initialize().unwrap();
        vault.user_activity();
        let owner = owner("idle");
        let slot = slot("secret", "api-key");
        let review = vault
            .review_installed(owner.clone(), slot.clone(), 1)
            .unwrap();
        vault
            .state
            .lock()
            .unwrap()
            .session
            .as_mut()
            .unwrap()
            .last_activity -= IDLE_TIMEOUT;
        assert_eq!(
            vault.read_and_publish(&owner, &slot, &mut |_, _| panic!("expired")),
            Err("vault_locked")
        );
        vault.user_activity();
        assert_eq!(vault.status().state, VaultState::Locked);
        assert!(!vault.state.lock().unwrap().reviews.contains_key(&review.id));
        vault.unlock().unwrap();
        vault
            .state
            .lock()
            .unwrap()
            .session
            .as_mut()
            .unwrap()
            .last_wall_activity -= IDLE_TIMEOUT;
        assert!(vault.entries(&[]).is_err());
        assert_eq!(vault.status().state, VaultState::Locked);
    }

    #[test]
    fn installed_review_is_one_use_and_rechecks_the_editor_entry_and_epoch() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path().join("manager-store")).unwrap();
        let vault = Vault::new(root.path().into(), Arc::new(TestKeys::default()), store);
        vault.initialize().unwrap();
        let owner = owner("installed");
        let slot = slot("secret", "api-key");
        let entry = entry(&vault, VaultKind::ApiKey, "Reviewed key", "fixture-only");
        let review = || {
            vault
                .review_installed(owner.clone(), slot.clone(), 7)
                .unwrap()
                .id
        };
        let approve = |id: &str, editor: u64, opt_in: bool| {
            vault.approve_installed(
                id,
                editor,
                &owner,
                &slot,
                &entry.id,
                &entry.revision,
                opt_in,
                || Ok(()),
            )
        };
        assert!(approve(&review(), 7, false).is_err());
        assert!(approve(&review(), 8, true).is_err());
        let cancelled = review();
        vault.cancel_installed_review(&cancelled, 7);
        assert!(approve(&cancelled, 7, true).is_err());
        let good = review();
        approve(&good, 7, true).unwrap();
        assert!(approve(&good, 7, true).is_err());
        assert!(vault.required_ready(&owner, &[&slot]));
        let stale = review();
        vault
            .revoke(&owner, &slot.id, &slot.action, &entry.id, &entry.revision)
            .unwrap();
        assert!(approve(&stale, 7, true).is_err());
        let locked = review();
        vault.lock();
        vault.unlock().unwrap();
        assert!(approve(&locked, 7, true).is_err());
        assert!(!vault.required_ready(&owner, &[&slot]));
    }

    // An approval from a prior unlock cannot be committed after locking and reopening
    #[test]
    fn stale_setup_does_not_reopen_grants() {
        let root = tempfile::tempdir().unwrap();
        let vault = Vault::new(
            root.path().into(),
            Arc::new(TestKeys::default()),
            Storage::open(root.path().join("manager-store")).unwrap(),
        );
        vault.initialize().unwrap();
        let text = entry(&vault, VaultKind::Text, "Fixture", "fixture-value");
        let slot = slot("value", "text");
        let (_, approval) = vault
            .setup_selection(&slot, &text.id, &text.revision, false)
            .unwrap();
        let id = (slot.action.clone(), slot.id.clone());
        vault.lock();
        vault.unlock().unwrap();
        assert_eq!(
            vault
                .grant_setup(
                    &owner("original"),
                    &[slot],
                    &BTreeMap::from([(id, approval)]),
                    || Ok(())
                )
                .unwrap_err(),
            "vault_setup_expired"
        );
    }

    // Stage without execution and activate grants only for the exact committed installation
    #[test]
    fn staged_install_binds_verified_package_and_preserves_cancelled_entries() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("manager");
        let runtime = crate::Runtime::build_credentials(
            config.clone(),
            false,
            crate::credentials::Credentials::with_provider(Arc::new(TestKeys::default())),
        )
        .unwrap();
        runtime.vault.initialize().unwrap();
        let source = root.path().join("org.fixture.vault.sdPlugin");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("fixture.mjs"), "throw Error('must not run');").unwrap();
        let text_slot = VaultSetupSlot {
            id: "account".into(),
            kind: "value".into(),
            entry_kind: "text".into(),
            mode: "direct".into(),
            required: true,
            purpose: "Set a synthetic account".into(),
            field: "username".into(),
            action: "org.fixture.vault.action".into(),
            ..Default::default()
        };
        crate::test_plugin::write_manifest(
            &source,
            json!({
                "UUID":"org.fixture.vault","Name":"Vault fixture","Version":"0.1.0","CodePath":"fixture.mjs",
                "StreamDockManager":{"Runtime":{"Kind":"nodejs","Version":"24"},"SetupSlots":{"version":1,"slots":[text_slot]}},
                "Actions":[{"UUID":"org.fixture.vault.action","Name":"Fixture","Controllers":["Keypad"]}]
            }),
        );
        let input = |label: &str| VaultEntryInput {
            label: label.into(),
            kind: VaultKind::Text,
            protected: false,
            value: json!("synthetic fixture account"),
        };
        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert!(review.can_import, "{:?}", review.compatibility.checks);
        assert!(!config.join("plugins/org.fixture.vault").exists());
        let ticket = runtime
            .vault_setup_slot_entries(&review.id, "account", &text_slot.action, 99)
            .unwrap()
            .ticket;
        let cancel_entry = runtime
            .vault_setup_create_entry(
                &review.id,
                "account",
                &text_slot.action,
                99,
                &ticket,
                input("Cancelled entry"),
            )
            .unwrap();
        runtime.cancel_plugin_package(&review.id);
        assert!(runtime
            .vault_setup_slot_entries(&review.id, "account", &text_slot.action, 99)
            .is_err());
        assert!(runtime
            .vault
            .entries(&[])
            .unwrap()
            .iter()
            .any(|e| e.id == cancel_entry.id && e.owner.is_none()));

        let review = runtime.prepare_plugin_package(&source).unwrap();
        assert!(runtime
            .commit_plugin_package_from_editor(&review.id, false, false, Some(7))
            .is_err());
        let ticket = runtime
            .vault_setup_slot_entries(&review.id, "account", &text_slot.action, 7)
            .unwrap()
            .ticket;
        let chosen = runtime
            .vault_setup_create_entry(
                &review.id,
                "account",
                &text_slot.action,
                7,
                &ticket,
                input("Selected entry"),
            )
            .unwrap();
        assert!(runtime
            .vault_setup_select(
                &review.id,
                "account",
                &text_slot.action,
                8,
                &ticket,
                &chosen.id,
                &chosen.revision,
                false
            )
            .is_err());
        runtime
            .vault_setup_select(
                &review.id,
                "account",
                &text_slot.action,
                7,
                &ticket,
                &chosen.id,
                &chosen.revision,
                false,
            )
            .unwrap();
        assert!(runtime
            .commit_plugin_package_from_editor(&review.id, false, false, Some(8))
            .is_err());
        assert!(runtime
            .vault_setup_select(
                &review.id,
                "account",
                &text_slot.action,
                7,
                &ticket,
                &chosen.id,
                &chosen.revision,
                false
            )
            .is_err());
        let snapshot = runtime
            .commit_plugin_package_from_editor(&review.id, false, false, Some(7))
            .unwrap();
        let installed = snapshot
            .plugins
            .iter()
            .find(|p| p.id == "org.fixture.vault")
            .unwrap();
        assert!(!installed.enabled);
        let owner = runtime
            .plugins
            .lock()
            .unwrap()
            .verified_vault_installation(&installed.id)
            .unwrap();
        let slot = review.manifest.setup_slots.unwrap().remove(0);
        let mut value = None;
        runtime
            .vault
            .read_and_publish(&owner, &slot, &mut |v, _| {
                value = Some(v.as_str().unwrap().to_owned());
                true
            })
            .unwrap();
        assert_eq!(value.as_deref(), Some("synthetic fixture account"));
        assert!(runtime
            .vault
            .entries(&[])
            .unwrap()
            .iter()
            .any(|e| e.id == chosen.id && e.owner.as_deref() == Some("org.fixture.vault")));
        std::fs::write(
            config.join("plugins/org.fixture.vault/fixture.mjs"),
            "changed inventory",
        )
        .unwrap();
        let changed = runtime
            .plugins
            .lock()
            .unwrap()
            .verified_vault_installation(&installed.id)
            .unwrap();
        assert_eq!(
            runtime
                .vault
                .read_and_publish(&changed, &slot, &mut |_, _| unreachable!()),
            Err("needs_setup")
        );
        runtime.shutdown();
    }
}
