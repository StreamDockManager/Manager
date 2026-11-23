//! Trusted editor commands; never install these into a plugin webview capability
use crate::editor_lifecycle::{self, EditorWork};
use std::{path::Path, sync::Arc};
use stream_dock_manager_runtime::{
    vault::{
        VaultEntry, VaultEntryInput, VaultEntryReveal, VaultExportSelection, VaultImportResult,
        VaultImportReview, VaultMetadataRevision, VaultPersonalEntries, VaultRevealLease,
        VaultState, VaultStatus, VAULT_TRANSFER_MAX_FILE_BYTES, VAULT_TRANSFER_MAX_PASSWORD_BYTES,
    },
    Runtime,
};
use zeroize::Zeroizing;

// Verify that Vault operations belong to the current trusted editor and not recovery
fn admit(work: &EditorWork, runtime: &Runtime) -> Result<(), String> {
    work.ensure_editable()?;
    if runtime.recovery_status().active {
        return Err("vault_unavailable".into());
    }
    Ok(())
}

// Only trusted native editor activity can extend the current unlocked session.
#[tauri::command]
pub fn vault_user_activity(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    runtime.vault.user_activity();
    Ok(())
}

// Query coarse Vault state without opening an OS credential prompt
#[tauri::command]
pub async fn vault_status(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<VaultStatus, String> {
    admit(&work, &runtime)?;
    Ok(runtime.vault.status())
}

// Publish lock policy to runtime demand only outside the encrypted store lock
fn synchronize(runtime: &Runtime) -> VaultStatus {
    let mut host = runtime.plugins.lock().unwrap();
    let status = runtime.vault.status();
    host.set_vault_locked(runtime.vault.is_configured() && status.state != VaultState::Unlocked);
    status
}

// Initialize a fresh Vault only after a first-party user choice
#[tauri::command]
pub async fn vault_initialize(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<VaultStatus, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault.initialize().map(|_| synchronize(&runtime))
    })
    .await
}

// Unlock an existing Vault using the native credential provider
#[tauri::command]
pub async fn vault_unlock(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<VaultStatus, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault.unlock().map(|_| synchronize(&runtime))
    })
    .await
}

/// Password arguments exist only on the trusted editor command and its blocking native operation.
#[tauri::command]
pub async fn vault_create_with_password(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    password: String,
    confirmation: String,
) -> Result<VaultStatus, String> {
    let password = zeroize::Zeroizing::new(password);
    let confirmation = zeroize::Zeroizing::new(confirmation);
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime
            .vault
            .initialize_password(&password, &confirmation)
            .map(|_| synchronize(&runtime))
    })
    .await
}

#[tauri::command]
pub async fn vault_unlock_password(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    password: String,
) -> Result<VaultStatus, String> {
    let password = zeroize::Zeroizing::new(password);
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime
            .vault
            .unlock_password(&password)
            .map(|_| synchronize(&runtime))
    })
    .await
}

#[tauri::command]
pub async fn vault_change_password(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    current: String,
    password: String,
    confirmation: String,
) -> Result<VaultStatus, String> {
    let current = zeroize::Zeroizing::new(current);
    let password = zeroize::Zeroizing::new(password);
    let confirmation = zeroize::Zeroizing::new(confirmation);
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime
            .vault
            .change_password(&current, &password, &confirmation)
            .map(|_| synchronize(&runtime))
    })
    .await
}

#[tauri::command]
pub async fn vault_switch_unlock_mode(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    to_password: bool,
    current: String,
    password: String,
    confirmation: String,
) -> Result<VaultStatus, String> {
    let current = zeroize::Zeroizing::new(current);
    let password = zeroize::Zeroizing::new(password);
    let confirmation = zeroize::Zeroizing::new(confirmation);
    admit(&work, &runtime)?;
    let rt = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        rt.vault
            .switch_mode(to_password, &current, &password, &confirmation)
            .map(|_| synchronize(&rt))
    })
    .await
}

#[tauri::command]
pub async fn vault_review_reset(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<String, String> {
    admit(&work, &runtime)?;
    let editor = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || runtime.vault.review_keystore_reset(editor))
        .await
}

#[tauri::command]
pub fn vault_cancel_reset(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    token: String,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    runtime
        .vault
        .cancel_keystore_reset(work.generation()?, &token);
    Ok(())
}

/// Retire the keystore only after an explicit first-party destructive confirmation.
#[tauri::command]
pub async fn vault_delete_keystore(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    confirmation: String,
    token: String,
) -> Result<VaultStatus, String> {
    admit(&work, &runtime)?;
    let editor = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        let result = runtime.vault.delete_keystore(editor, &token, &confirmation);
        synchronize(&runtime);
        result
    })
    .await
}

// End the Vault session and retire new dependent operations
#[tauri::command]
pub async fn vault_lock(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<VaultStatus, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault.lock();
        Ok(synchronize(&runtime))
    })
    .await
}

// A built-in selector returns metadata only for entries eligible for the native field.
#[tauri::command]
pub async fn vault_builtin_choices(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path_field: bool,
    json_field: Option<bool>,
) -> Result<Vec<VaultEntry>, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        if json_field == Some(true) {
            runtime.vault.json_choices()
        } else {
            runtime.vault.builtin_choices(path_field)
        }
    })
    .await
}

// Probe the cache fence before using pending personal metadata
#[tauri::command]
pub async fn vault_metadata_revision(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<VaultMetadataRevision, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || Ok(runtime.vault.metadata_revision())).await
}

// Read ordinary personal rows without verifying unrelated plugin installations
#[tauri::command]
pub async fn vault_personal_entries(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<VaultPersonalEntries, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || runtime.vault.personal_entries()).await
}

// Read entry metadata only while the Vault is unlocked
#[tauri::command]
pub async fn vault_entries(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<Vec<VaultEntry>, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        if runtime.vault.status().state != VaultState::Unlocked {
            return Err("vault_locked".into());
        }
        let host = runtime.plugins.lock().unwrap();
        runtime.vault.entries(&host.verified_vault_installations())
    })
    .await
}

/// Reveal one still-current personal entry in the admitted first-party document.
#[tauri::command]
pub async fn vault_reveal_entry(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    revision: String,
    password: Option<String>,
) -> Result<VaultEntryReveal, String> {
    let password = password.map(zeroize::Zeroizing::new);
    admit(&work, &runtime)?;
    if runtime.is_stopping() {
        return Err("vault_unavailable".into());
    }
    let generation = work.generation()?;
    let lease = runtime.vault.begin_entry_reveal()?;
    let on_worker = runtime.inner().clone();
    let worker = work.worker();
    let revealed = editor_lifecycle::blocking_result(&work, move || {
        admit(&worker, &on_worker)?;
        if worker.generation()? != generation || on_worker.is_stopping() {
            return Err("vault_reveal_expired".into());
        }
        let value = on_worker.vault.reveal_entry(
            lease,
            &id,
            &revision,
            password.as_ref().map(|p| p.as_str()),
        )?;
        admit(&worker, &on_worker)?;
        Ok(value)
    })
    .await?;
    admit(&work, &runtime)?;
    if work.generation()? != generation || runtime.is_stopping() {
        return Err("vault_reveal_expired".into());
    }
    runtime.vault.check_entry_reveal(lease)?;
    Ok(revealed)
}

// Check that a transfer still belongs to the admitted first-party editor
fn transfer_generation(work: &EditorWork, runtime: &Runtime) -> Result<u64, String> {
    admit(work, runtime)?;
    if runtime.is_stopping() {
        return Err("vault_unavailable".into());
    }
    work.generation()
}

// Export only explicitly selected and still-current personal entries
#[tauri::command]
pub async fn vault_export_entries(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
    selected: Vec<VaultExportSelection>,
    password: Option<String>,
    allow_plaintext: bool,
    master_password: Option<String>,
) -> Result<u32, String> {
    let password = password.map(Zeroizing::new);
    let master_password = master_password.map(Zeroizing::new);
    let editor = transfer_generation(&work, &runtime)?;
    let lease = runtime.vault.begin_entry_reveal()?;
    let runtime = runtime.inner().clone();
    let worker = work.worker();
    editor_lifecycle::blocking_result(&work, move || {
        if transfer_generation(&worker, &runtime)? != editor {
            return Err("vault_import_expired".into());
        }
        runtime.vault.export_entries_with_admission(
            lease,
            Path::new(&path),
            &selected,
            password.as_ref().map(|p| p.as_str()),
            allow_plaintext,
            master_password.as_ref().map(|p| p.as_str()),
            || {
                if runtime.is_stopping() {
                    return Err("vault_unavailable".into());
                }
                worker.commit_for_generation(editor)
            },
        )
    })
    .await
}

// Stage a review without changing the Vault and discard it if the editor closes
async fn stage_import(
    work: &EditorWork,
    runtime: Arc<Runtime>,
    prepare: impl FnOnce(&Runtime, VaultRevealLease, u64) -> Result<VaultImportReview, String>
        + Send
        + 'static,
) -> Result<VaultImportReview, String> {
    let editor = transfer_generation(work, &runtime)?;
    let lease = runtime.vault.begin_entry_reveal()?;
    let worker = work.worker();
    let on_worker = runtime.clone();
    let review = editor_lifecycle::blocking_result(work, move || {
        if transfer_generation(&worker, &on_worker)? != editor {
            return Err("vault_import_expired".into());
        }
        let review = prepare(&on_worker, lease, editor)?;
        if transfer_generation(&worker, &on_worker) != Ok(editor) {
            on_worker.vault.cancel_entry_import(editor, &review.id);
            return Err("vault_import_expired".into());
        }
        Ok(review)
    })
    .await?;
    if transfer_generation(work, &runtime) != Ok(editor)
        || runtime.vault.check_entry_reveal(lease).is_err()
    {
        runtime.vault.cancel_entry_import(editor, &review.id);
        return Err("vault_import_expired".into());
    }
    Ok(review)
}

// Read a picked Vault export directly in the native worker
#[tauri::command]
pub async fn vault_prepare_entry_import(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    path: String,
    password: Option<String>,
) -> Result<VaultImportReview, String> {
    let password = password.map(Zeroizing::new);
    stage_import(
        &work,
        runtime.inner().clone(),
        move |runtime, lease, editor| {
            runtime.vault.prepare_entry_import(
                lease,
                editor,
                Path::new(&path),
                password.as_ref().map(|p| p.as_str()),
            )
        },
    )
    .await
}

// Split a bounded drag payload without writing either the export or password to disk
fn dropped_transfer(body: &[u8]) -> Result<(&[u8], Option<&str>), String> {
    let invalid = || "vault_transfer_invalid".to_owned();
    let (header, body) = body.split_at_checked(4).ok_or_else(invalid)?;
    let password_len = u32::from_le_bytes(header.try_into().unwrap()) as usize;
    if password_len > VAULT_TRANSFER_MAX_PASSWORD_BYTES {
        return Err(invalid());
    }
    let (password, file) = body.split_at_checked(password_len).ok_or_else(invalid)?;
    if file.is_empty() || file.len() > VAULT_TRANSFER_MAX_FILE_BYTES {
        return Err(invalid());
    }
    let password = if password.is_empty() {
        None
    } else {
        Some(std::str::from_utf8(password).map_err(|_| invalid())?)
    };
    Ok((file, password))
}

// Reuse the native file picker review pipeline for a browser File dropped into Vault
#[tauri::command]
pub async fn vault_prepare_dropped_entry_import(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    request: tauri::ipc::Request<'_>,
) -> Result<VaultImportReview, String> {
    transfer_generation(&work, &runtime)?;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("vault_transfer_invalid".into());
    };
    let (file, password) = dropped_transfer(bytes)?;
    let file = Zeroizing::new(file.to_vec());
    let password = password.map(|value| Zeroizing::new(value.to_owned()));
    stage_import(
        &work,
        runtime.inner().clone(),
        move |runtime, lease, editor| {
            runtime.vault.prepare_entry_import_bytes(
                lease,
                editor,
                &file,
                password.as_ref().map(|p| p.as_str()),
            )
        },
    )
    .await
}

// Return only the selected incoming conflict value to the admitted first-party editor
#[tauri::command]
pub async fn vault_reveal_entry_import(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    review_id: String,
    row: u32,
) -> Result<VaultEntryReveal, String> {
    let editor = transfer_generation(&work, &runtime)?;
    let lease = runtime.vault.begin_entry_reveal()?;
    let on_worker = runtime.inner().clone();
    let worker = work.worker();
    let worker_review_id = review_id.clone();
    let value = editor_lifecycle::blocking_result(&work, move || {
        if transfer_generation(&worker, &on_worker)? != editor {
            return Err("vault_import_expired".into());
        }
        on_worker
            .vault
            .reveal_entry_import(editor, &worker_review_id, row)
    })
    .await?;
    if transfer_generation(&work, &runtime)? != editor {
        return Err("vault_import_expired".into());
    }
    runtime.vault.check_entry_reveal(lease)?;
    runtime
        .vault
        .check_entry_import_review(editor, &review_id)?;
    Ok(value)
}

// Commit the review with only the explicitly approved overwrite rows
#[tauri::command]
pub async fn vault_apply_entry_import(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    review_id: String,
    overwrite: Vec<u32>,
) -> Result<VaultImportResult, String> {
    let editor = transfer_generation(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    let worker = work.worker();
    editor_lifecycle::blocking_result(&work, move || {
        if transfer_generation(&worker, &runtime)? != editor {
            return Err("vault_import_expired".into());
        }
        runtime
            .vault
            .apply_entry_import(editor, &review_id, &overwrite)
    })
    .await
}

// Allow the trusted editor to retire staged values while the close drain is running
#[tauri::command]
pub fn vault_cancel_entry_import(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    review_id: String,
) -> Result<(), String> {
    runtime
        .vault
        .cancel_entry_import(work.generation()?, &review_id);
    Ok(())
}

// Create a typed entry without echoing the supplied value
#[tauri::command]
pub async fn vault_create_entry(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    input: VaultEntryInput,
) -> Result<VaultEntry, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || runtime.vault.create_entry(input)).await
}

// Replace an entry value while preserving its identity and type
#[tauri::command]
pub async fn vault_replace_entry(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    revision: String,
    value: serde_json::Value,
) -> Result<VaultEntry, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault.replace_entry(&id, &revision, value)
    })
    .await
}

// Change only the entry label
#[tauri::command]
pub async fn vault_rename_entry(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    revision: String,
    label: String,
) -> Result<VaultEntry, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault.rename_entry(&id, &revision, label)
    })
    .await
}

// Change the personal reveal requirement without returning the saved value
#[tauri::command]
pub async fn vault_set_entry_protection(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    revision: String,
    protected: bool,
    password: Option<String>,
) -> Result<VaultEntry, String> {
    let password = password.map(Zeroizing::new);
    let generation = transfer_generation(&work, &runtime)?;
    let lease = runtime.vault.begin_entry_reveal()?;
    let worker = work.worker();
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        if transfer_generation(&worker, &runtime)? != generation {
            return Err("vault_reveal_expired".into());
        }
        let entry = runtime.vault.set_entry_protection(
            lease,
            &id,
            &revision,
            protected,
            password.as_deref().map(String::as_str),
            || {
                worker
                    .commit_for_generation(generation)
                    .map_err(|_| "vault_reveal_expired".into())
            },
        )?;
        synchronize(&runtime);
        Ok(entry)
    })
    .await
}

// Delete an entry from the live encrypted ledger
#[tauri::command]
pub async fn vault_delete_entry(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    revision: String,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault.delete_entry(&id, &revision)?;
        synchronize(&runtime);
        Ok(())
    })
    .await
}

/// Record the separate package opt-in before approving any particular entry.
#[tauri::command]
pub async fn vault_setup_permission(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    slot_id: String,
    action: String,
    allow: bool,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    let owner = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault_setup_permission(&id, &slot_id, &action, owner, allow)
    })
    .await
}

// Return only entry metadata that fits the exact reviewed staged slot
#[tauri::command]
pub async fn vault_setup_slot_entries(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    slot_id: String,
    action: String,
) -> Result<stream_dock_manager_runtime::plugin_packages::VaultSetupChoices, String> {
    admit(&work, &runtime)?;
    let owner = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault_setup_slot_entries(&id, &slot_id, &action, owner)
    })
    .await
}

// Record explicit entry approval in the native staged review
#[expect(
    clippy::too_many_arguments,
    reason = "Preserve the existing Vault setup IPC argument names"
)]
#[tauri::command]
pub async fn vault_setup_select(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    slot_id: String,
    action: String,
    ticket: String,
    entry_id: String,
    revision: String,
    allow_direct_read: bool,
) -> Result<stream_dock_manager_runtime::vault::VaultSetupSelection, String> {
    admit(&work, &runtime)?;
    let owner = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault_setup_select(
            &id,
            &slot_id,
            &action,
            owner,
            &ticket,
            &entry_id,
            &revision,
            allow_direct_read,
        )
    })
    .await
}

// Create a user entry for this slot without selecting or approving it
#[tauri::command]
pub async fn vault_setup_create_entry(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    slot_id: String,
    action: String,
    ticket: String,
    input: VaultEntryInput,
) -> Result<VaultEntry, String> {
    admit(&work, &runtime)?;
    let owner = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault_setup_create_entry(&id, &slot_id, &action, owner, &ticket, input)
    })
    .await
}

// List grants for currently installed matching package byte inventories
#[tauri::command]
pub async fn vault_access(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<Vec<stream_dock_manager_runtime::vault::VaultAccess>, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        if runtime.vault.status().state != VaultState::Unlocked {
            return Err("vault_locked".into());
        }
        let host = runtime.plugins.lock().unwrap();
        runtime.vault.access(&host.verified_vault_installations())
    })
    .await
}

// Revoke only the exact currently displayed access grant
#[tauri::command]
pub async fn vault_revoke(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
    slot_id: String,
    action: String,
    entry_id: String,
    revision: String,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        if runtime.vault.status().state != VaultState::Unlocked {
            return Err("vault_locked".into());
        }
        let host = runtime.plugins.lock().unwrap();
        let owner = host
            .reviewed_vault_owner(&plugin_id)
            .ok_or("permission_denied")?;
        runtime
            .vault
            .revoke(&owner, &slot_id, &action, &entry_id, &revision)?;
        drop(host);
        synchronize(&runtime);
        Ok(())
    })
    .await
}

// Deny an action's direct reads even while the entry ledger is locked.
#[tauri::command]
pub async fn vault_revoke_direct_reads(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
    action: String,
) -> Result<(), String> {
    revoke_action_access(work, runtime, plugin_id, action, false).await
}

#[tauri::command]
pub async fn vault_revoke_private_writes(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
    action: String,
) -> Result<(), String> {
    revoke_action_access(work, runtime, plugin_id, action, true).await
}

async fn revoke_action_access(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
    action: String,
    writes: bool,
) -> Result<(), String> {
    use stream_dock_manager_plugin_host::permissions::PermissionScope;
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        let host = runtime.plugins.lock().unwrap();
        let declared = host
            .list()
            .into_iter()
            .find(|plugin| plugin.id == plugin_id)
            .and_then(|p| p.permissions)
            .and_then(|mut d| d.actions.remove(&action))
            .is_some_and(|g| {
                g.required.iter().chain(&g.optional).any(|scope| {
                    matches!(
                        (writes, scope),
                        (
                            false,
                            PermissionScope::ReadSecret { .. }
                                | PermissionScope::ReadLocation { .. }
                        ) | (true, PermissionScope::WriteSecret { .. })
                    )
                })
            });
        if !declared {
            return Err("not_declared".into());
        }
        if writes {
            runtime.vault.revoke_private_writes(&plugin_id, &action)?;
        } else {
            runtime.vault.revoke_direct_reads(&plugin_id, &action)?;
        }
        drop(host);
        synchronize(&runtime);
        Ok(())
    })
    .await
}

// Expose public declaration selectors and encrypted usage status only while unlocked.
#[tauri::command]
pub async fn vault_permission_activity(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<Vec<stream_dock_manager_runtime::vault::VaultPermissionActivity>, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        if runtime.vault.status().state != VaultState::Unlocked {
            return Err("vault_locked".into());
        }
        let host = runtime.plugins.lock().unwrap();
        let owners: Vec<_> = host
            .list()
            .iter()
            .filter(|p| p.permissions.is_some())
            .filter_map(|p| host.verified_vault_installation(&p.id))
            .collect();
        runtime.vault.permission_activity(&owners)
    })
    .await
}

#[tauri::command]
pub async fn vault_installed_review(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
    action: String,
    field: String,
) -> Result<stream_dock_manager_runtime::vault::InstalledVaultReview, String> {
    admit(&work, &runtime)?;
    let editor = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        let host = runtime.plugins.lock().unwrap();
        let (owner, slot) = host.installed_setup_slot(&plugin_id, &action, &field)?;
        runtime.vault.review_installed(owner, slot, editor)
    })
    .await
}

#[tauri::command]
pub async fn vault_cancel_installed_review(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    runtime
        .vault
        .cancel_installed_review(&id, work.generation()?);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Preserve the existing Vault approval IPC argument names"
)]
#[tauri::command]
pub async fn vault_allow_installed_read(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    plugin_id: String,
    action: String,
    field: String,
    id: String,
    entry_id: String,
    revision: String,
    package_opt_in: bool,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    let editor = work.generation()?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        let mut host = runtime.plugins.lock().unwrap();
        let (owner, slot) = host.installed_setup_slot(&plugin_id, &action, &field)?;
        runtime.vault.approve_installed(
            &id,
            editor,
            &owner,
            &slot,
            &entry_id,
            &revision,
            package_opt_in,
            || host.select_managed_vault(&plugin_id),
        )?;
        host.set_vault_locked(false);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn vault_cancel_picker(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    ticket: String,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    runtime.cancel_vault_picker(&id, work.generation()?, &ticket);
    Ok(())
}

#[tauri::command]
pub async fn vault_settings_plugins(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
) -> Result<Vec<stream_dock_manager_runtime::vault::VaultSettingsPlugin>, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || runtime.vault_settings_plugins()).await
}

#[tauri::command]
pub async fn vault_settings_document(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    reveal: bool,
) -> Result<stream_dock_manager_runtime::vault::VaultSettingsDocument, String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || runtime.vault_settings_document(id, reveal))
        .await
}

#[tauri::command]
pub async fn vault_delete_settings(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || runtime.vault_delete_settings(id)).await
}

#[tauri::command]
pub async fn vault_delete_plugin_settings(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    review: String,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || {
        runtime.vault_delete_plugin_settings(id, review)
    })
    .await
}

#[tauri::command]
pub async fn vault_save_settings(
    work: EditorWork,
    runtime: tauri::State<'_, Arc<Runtime>>,
    id: String,
    value: serde_json::Value,
) -> Result<(), String> {
    admit(&work, &runtime)?;
    let runtime = runtime.inner().clone();
    editor_lifecycle::blocking_result(&work, move || runtime.vault_save_settings(id, value)).await
}

#[cfg(test)]
mod transfer_command_tests {
    use super::*;

    #[test]
    fn dropped_transfer_has_a_bounded_password_prefix_and_nonempty_file() {
        assert_eq!(
            dropped_transfer(b"\0\0\0\0file").unwrap(),
            (&b"file"[..], None)
        );
        let password = "clé";
        let mut valid = (password.len() as u32).to_le_bytes().to_vec();
        valid.extend_from_slice(password.as_bytes());
        valid.extend_from_slice(b"test");
        assert_eq!(
            dropped_transfer(&valid).unwrap(),
            (&b"test"[..], Some(password))
        );
        for bad in [
            b"".as_slice(),
            b"\0\0\0",
            b"\0\0\0\0",
            b"\x01\0\0\0\xfffile",
            b"\xff\xff\xff\xfffile",
            b"\x07\0\0\0short",
        ] {
            assert!(dropped_transfer(bad).is_err());
        }
        assert!(dropped_transfer(&vec![0; 5 + VAULT_TRANSFER_MAX_FILE_BYTES]).is_err());
    }
}
