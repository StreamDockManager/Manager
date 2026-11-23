import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { CommandName, CommandParameters, CommandResult } from '../generated/commands';
import type { ConfigurationRecoveryItem } from '../generated/runtime';
import { ConfigurationRecoveryController } from './config-recovery-controller';
import {
    ConfigurationRecoveryKeystore,
    keystoreBackupLabel,
    keystoreRecoveryAdvice,
    type ConfigurationRecoveryKeystoreApi,
} from './config-recovery-keystore';
import { applyTheme, readAppearance } from './theme-style';
import { applyAccessibility, readAccessibility } from './accessibility-preferences';
import { RecoveryToasts } from './recovery-toasts';
import { RecoveryVersionViewer } from './recovery-versions';
import { RecoveryJsonEditor } from './recovery-json-editor';
import { locateDiagnostics } from './recovery-json-locations';
import './config-recovery.css';

// This shell has no running workspace or editor-generation lease. Only the explicitly
// scoped recovery commands may be used; importing the regular bridge would start it
const command = <C extends CommandName>(name: C, ...args: CommandParameters<C>) =>
    invoke<CommandResult<C>>(name, args[0]);
// Resolve a typed element from the recovery document
const element = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const editor = element<HTMLDialogElement>('recovery-editor');
const confirmation = element<HTMLDialogElement>('recovery-confirm');
const leave = element<HTMLDialogElement>('recovery-leave');
const input = element<HTMLTextAreaElement>('recovery-json');
const jsonEditor = new RecoveryJsonEditor(
    input,
    element('recovery-json-highlights'),
    element<HTMLInputElement>('recovery-json-search'),
    element('recovery-json-count'),
    element<HTMLButtonElement>('recovery-json-previous'),
    element<HTMLButtonElement>('recovery-json-next'),
);
const toasts = new RecoveryToasts();
window.addEventListener('pagehide', () => toasts.destroy(), { once: true });
const controller = new ConfigurationRecoveryController(
    {
        // Read the current configuration recovery status
        status: () => command('configuration_recovery_status'),
        // Read the available configuration backups
        backups: () => command('configuration_recovery_backups'),
        // Validate a full-backup restore against the selected inventory revision
        previewRestore: (backupId, revision) =>
            command('configuration_recovery_preview_restore', { backupId, revision }),
        // Restore only the backup identified by its validated review token
        restoreBackup: (backupId, revision, reviewToken) =>
            command('configuration_recovery_restore_backup', { backupId, revision, reviewToken }),
        // Read profiles from the selected backup revision
        backupProfiles: (backupId, revision) =>
            command('configuration_recovery_backup_profiles', { backupId, revision }),
        // Transfer selected backup profiles into the current database
        transferProfiles: (backupId, revision, profileIds) =>
            command('configuration_recovery_transfer_profiles', { backupId, revision, profileIds }),
        // Read the selected saved document for recovery editing
        read: (id) => command('configuration_recovery_document', { id }),
        // Validate edited JSON against the saved document revision
        validate: (id, revision, json) =>
            command('configuration_recovery_validate', { id, revision, json }),
        // Save the validated recovery document through the native backend
        save: (id, revision, json) =>
            command('configuration_recovery_save', { id, revision, json }),
        // Delete the reviewed saved document revision
        delete: (id, revision) => command('configuration_recovery_delete', { id, revision }),
        // Open the continuation flow after recovery becomes ready
        retry: async () => {
            setTimeout(() => void requestLeave(), 0);
        },
    },
    render,
    (message, level) => toasts.notify(message, level),
);
// Recovery is a separate window with no live editor, plugin, or Vault IPC.
const keystoreApi: ConfigurationRecoveryKeystoreApi = {
    status: () => command('configuration_recovery_keystore_status'),
    retry: (revision) => command('configuration_recovery_retry_keystore', { revision }),
    review: (revision, operation, backupId) =>
        command('configuration_recovery_review_keystore', { revision, operation, backupId }),
    apply: (revision, reviewToken) =>
        command('configuration_recovery_apply_keystore', { revision, reviewToken }),
};
const keystore = new ConfigurationRecoveryKeystore(keystoreApi, {
    blocked: () =>
        controller.busy || controller.dirty || !!controller.report?.incompatible_data_folder,
    perform: (operation) => controller.perform(operation),
    confirm,
    invalidateRecovery: () => {
        controller.report = null;
    },
    refreshRecovery: () => controller.refresh(),
});
// Refresh ordinary and keystore evidence without starting Manager.
async function refreshRecovery() {
    if (controller.busy) return;
    await controller.refresh();
    if (controller.report && !controller.report.incompatible_data_folder) await keystore.refresh();
}
const versionViewer = new RecoveryVersionViewer(
    {
        // List saved versions of the current recovery document
        list: (id, revision) => command('configuration_recovery_versions', { id, revision }),
        // Read one historical version of the recovery document
        read: (id, revision, version_id) =>
            command('configuration_recovery_version', { id, revision, versionId: version_id }),
    },
    confirm,
    // Load selected historical JSON into the recovery editor draft
    (json) => {
        controller.edit(json);
        input.value = controller.text;
        input.focus();
    },
    (message) => toasts.notify(message, 'error'),
);
const appearance = readAppearance();
const systemTheme = matchMedia('(prefers-color-scheme: dark)');
// Apply appearance settings to the recovery window
const theme = () => applyTheme(appearance.preference, systemTheme.matches, appearance.style);
systemTheme.addEventListener('change', theme);
theme();
applyAccessibility(
    readAccessibility(),
    matchMedia('(prefers-contrast: more)').matches,
    matchMedia('(prefers-reduced-motion: reduce)').matches,
);
let confirmResult: ((accepted: boolean) => void) | null = null;
// Ask for explicit confirmation of a recovery operation
function confirm(
    title: string,
    message: string,
    action: string,
    warnings: string[] = [],
): Promise<boolean> {
    if (confirmResult) return Promise.resolve(false);
    element('recovery-confirm-heading').textContent = title;
    element('recovery-confirm-message').textContent = message;
    element('recovery-confirm-accept').textContent = action;
    list('recovery-confirm-warnings', warnings);
    element('recovery-confirm-warnings').hidden = !warnings.length;
    // Wait for the user’s response to the confirmation dialog
    const result = new Promise<boolean>((resolve) => {
        confirmResult = resolve;
    });
    confirmation.showModal();
    element('recovery-confirm-cancel').focus();
    return result;
}
// Close the confirmation dialog and resolve its pending decision
function finishConfirm(accepted: boolean) {
    confirmation.close();
    const resolve = confirmResult;
    confirmResult = null;
    resolve?.(accepted);
}
// Set a recovery text label and optionally hide empty content
function text(id: string, value: string, hideEmpty = false) {
    const target = element(id);
    target.textContent = value;
    if (hideEmpty) target.hidden = !value;
}
// Render a list of recovery messages as text
function list(id: string, values: string[]) {
    element(id).replaceChildren(
        // Create a text-only list entry for a recovery message
        ...values.map((value) => {
            const item = document.createElement('li');
            item.textContent = value;
            return item;
        }),
    );
}
let renderedItems = '';
// Render saved items and their recovery actions
function renderItems(items: ConfigurationRecoveryItem[]) {
    const signature = JSON.stringify(items);
    if (signature !== renderedItems) {
        renderedItems = signature;
        element('recovery-items').replaceChildren(
            // Build a recovery row for one saved item
            ...items.map((item) => {
                const row = document.createElement('article');
                row.className = 'card configuration-recovery-item';
                const info = document.createElement('div');
                info.className = 'configuration-recovery-item-info';
                const title = document.createElement('h3');
                title.textContent = item.name;
                const kind = document.createElement('small');
                kind.textContent = `${item.kind} · ${item.issues.length ? 'Needs repair' : 'Valid'}`;
                const issues = document.createElement('ol');
                issues.className = 'recovery-issue-list';
                for (const issue of item.issues) {
                    const line = document.createElement('li');
                    line.textContent = issue;
                    issues.append(line);
                }
                info.append(title, kind, issues);
                const actions = document.createElement('div');
                actions.className = 'button-row';
                if (item.editable) {
                    const edit = document.createElement('button');
                    edit.className = 'button';
                    edit.textContent = 'Edit JSON';
                    edit.setAttribute('aria-label', `Edit ${item.name} as JSON`);
                    edit.addEventListener('click', () => void openEditor(item.id));
                    actions.append(edit);
                }
                if (item.deletable) {
                    const remove = document.createElement('button');
                    remove.className = 'button danger';
                    remove.textContent = item.kind === 'setting' ? 'Reset to default' : 'Delete';
                    remove.setAttribute(
                        'aria-label',
                        `${item.kind === 'setting' ? 'Reset' : 'Delete'} ${item.name}`,
                    );
                    remove.addEventListener('click', () => void removeItem(item));
                    actions.append(remove);
                }
                row.append(info, actions);
                return row;
            }),
        );
    }
    element('recovery-items')
        .querySelectorAll('button')
        // Connect each saved-item action to its recovery handler
        .forEach((button) => {
            button.disabled = controller.busy;
        });
}
let renderedKeystoreBackups = '';
// Render current diagnostics and encrypted-backup metadata without paths or secrets.
function renderKeystore() {
    const report = keystore.status;
    const section = element('recovery-keystore');
    section.hidden =
        !!controller.report?.incompatible_data_folder ||
        (!controller.report && !report && !keystore.error);
    section.setAttribute('aria-busy', String(controller.busy));
    const status =
        report?.state === 'ready'
            ? 'The plugin settings keystore is ready.'
            : report?.state === 'unavailable'
              ? 'The plugin settings keystore is unavailable.'
              : report?.state === 'missing'
                ? keystore.creationInterrupted
                    ? 'Plugin settings keystore creation was interrupted.'
                    : 'The plugin settings keystore is missing.'
                : 'The plugin settings keystore status is unknown.';
    text('recovery-keystore-status', status);
    text('recovery-keystore-details', keystoreRecoveryAdvice(report?.reason), true);
    text('recovery-keystore-notice', keystore.notice, true);
    text('recovery-keystore-error', keystore.error, true);

    const select = element<HTMLSelectElement>('recovery-keystore-backup-select');
    const backups = report?.backups ?? [];
    const signature = JSON.stringify(backups);
    if (signature !== renderedKeystoreBackups) {
        renderedKeystoreBackups = signature;
        select.replaceChildren(
            new Option('Choose an encrypted backup…', ''),
            ...backups.map(
                (backup, index) => new Option(keystoreBackupLabel(backup, index), backup.id),
            ),
        );
    }
    select.value = keystore.backupId;
    select.disabled = !keystore.canRetry;
    element('recovery-keystore-backup-choice').hidden = !backups.length;
    element('recovery-keystore-backups-empty').hidden = !report || !!backups.length;
    const backup = keystore.backup;
    text(
        'recovery-keystore-backup-details',
        backup
            ? `${new Intl.NumberFormat().format(backup.bytes)} bytes · For this installation only`
            : '',
        true,
    );
    element<HTMLButtonElement>('recovery-keystore-retry').disabled = !keystore.canRetry;
    const create = element<HTMLButtonElement>('recovery-keystore-create');
    const reset = element<HTMLButtonElement>('recovery-keystore-reset');
    create.textContent = keystore.creationInterrupted ? 'Resume creation…' : 'Review create…';
    create.hidden = report?.state !== 'missing';
    create.disabled = !keystore.canCreate;
    reset.hidden = report?.state === 'missing';
    reset.disabled = !keystore.canReset;
    element<HTMLButtonElement>('recovery-keystore-restore').disabled = !keystore.canRestore;
}
let renderedBackups = '';
let renderedBackupProfiles = '';
// Render backup selection, restore review and transferable profiles
function renderBackups() {
    const catalog = controller.backupCatalog;
    const select = element<HTMLSelectElement>('recovery-backup-select');
    element('recovery-backups').hidden = !catalog?.backups.length;
    element('recovery-backups').setAttribute('aria-busy', String(controller.busy));
    const signature = JSON.stringify(catalog?.backups || []);
    if (signature !== renderedBackups) {
        renderedBackups = signature;
        select.replaceChildren(
            new Option('Choose a backup…', ''),
            ...(catalog?.backups || []).map(
                (backup) =>
                    new Option(
                        `${backup.label}${backup.timestamp_ms === null ? '' : ' · ' + new Date(backup.timestamp_ms).toLocaleString()}`,
                        backup.id,
                    ),
            ),
        );
    }
    select.value = controller.backupId;
    select.disabled = controller.busy || controller.dirty || controller.backupNeedsRefresh;
    element<HTMLButtonElement>('recovery-preview-restore').disabled = !controller.canPreviewRestore;
    element<HTMLButtonElement>('recovery-restore-backup').disabled = !controller.canRestoreBackup;
    const review = controller.restorePreview;
    element('recovery-restore-review').hidden = !review;
    text(
        'recovery-restore-summary',
        review
            ? `${review.valid ? 'Validated and ready to restore' : 'Cannot restore this backup'} · ${review.profile_count} profiles.`
            : '',
    );
    list('recovery-restore-issues', review?.issues || []);
    list('recovery-restore-warnings', review?.warnings || []);
    text('recovery-restore-error', controller.restoreError, true);
    element('recovery-transfer-unavailable').hidden = controller.report?.primary_readable !== false;
    const profiles = controller.backupProfiles;
    const profileSignature = JSON.stringify(profiles);
    if (profileSignature !== renderedBackupProfiles) {
        renderedBackupProfiles = profileSignature;
        element('recovery-backup-profiles').replaceChildren(
            // Build a selection row for one profile found in the backup
            ...(profiles?.profiles || []).map((profile) => {
                const row = document.createElement('article');
                row.className = 'configuration-recovery-backup-profile';
                const label = document.createElement('label');
                const checkbox = document.createElement('input');
                checkbox.type = 'checkbox';
                checkbox.dataset.profileId = profile.id;
                checkbox.addEventListener('change', () =>
                    controller.toggleBackupProfile(profile.id, checkbox.checked),
                );
                const name = document.createElement('span');
                name.textContent = `${profile.name} · ${profile.model_id}${profile.valid ? '' : ' · Unavailable'}`;
                label.append(checkbox, name);
                const issues = document.createElement('ul');
                issues.className = 'recovery-issue-list';
                for (const issue of profile.issues) {
                    const item = document.createElement('li');
                    item.textContent = issue;
                    issues.append(item);
                }
                row.append(label, issues);
                return row;
            }),
        );
    }
    element('recovery-backup-profiles')
        .querySelectorAll<HTMLInputElement>('input')
        // Connect backup profile checkboxes to the controller selection
        .forEach((checkbox) => {
            const id = checkbox.dataset.profileId!;
            checkbox.checked = controller.selectedProfiles.includes(id);
            checkbox.disabled =
                controller.busy ||
                controller.dirty ||
                controller.backupNeedsRefresh ||
                !profiles?.profiles.find((profile) => profile.id === id)?.valid;
        });
    list('recovery-backup-warnings', [...(catalog?.warnings || []), ...(profiles?.warnings || [])]);
    text(
        'recovery-backup-status',
        controller.backupNotice ||
            (controller.busy
                ? 'Working…'
                : profiles
                  ? profiles.profiles.length
                      ? `${controller.selectedProfiles.length} of ${profiles.profiles.length} profiles selected.`
                      : 'This backup contains no available profiles.'
                  : 'Select a backup to see its profiles.'),
    );
    element('recovery-backup-conflict').hidden = !controller.backupNeedsRefresh;
    text('recovery-backup-error', controller.backupNeedsRefresh ? controller.error : '', true);
    element<HTMLButtonElement>('recovery-backup-transfer').disabled =
        !controller.canTransferProfiles;
}
// Confirm the exact full-backup review before restoring it
async function restoreBackup() {
    if (!controller.canRestoreBackup) return;
    const preview = controller.restorePreview!;
    const backup = controller.backupCatalog?.backups.find(
        (backup) => backup.id === preview.backup_id,
    );
    const accepted = await confirm(
        'Replace the current database?',
        `Restore the validated backup “${backup?.label || 'Selected backup'}” containing ${preview.profile_count} profiles? This replaces the entire current database, not just selected profiles. Its previous contents will be preserved separately, and the backup remains unchanged. External files are not restored by replacing the database. Devices and plugins stay stopped until you choose Continue to Manager.`,
        'Restore database',
    );
    if (accepted && preview === controller.restorePreview && controller.canRestoreBackup)
        await controller.restoreBackup();
}
// Confirm the selected backup profiles before transferring copies
async function transferBackupProfiles() {
    if (!controller.canTransferProfiles) return;
    const selected = controller.backupProfiles!.profiles.filter((profile) =>
        controller.selectedProfiles.includes(profile.id),
    );
    const signature = JSON.stringify([controller.backupProfiles, controller.selectedProfiles]);
    const accepted = await confirm(
        'Transfer selected profiles?',
        `Copy ${selected.length} ${selected.length === 1 ? 'profile' : 'profiles'} into the current database: ${selected.map((profile) => profile.name).join(', ')}. The backup and existing profiles will not be changed.`,
        'Transfer profiles',
    );
    if (
        accepted &&
        signature === JSON.stringify([controller.backupProfiles, controller.selectedProfiles])
    )
        await controller.transferBackupProfiles();
}
// Refresh recovery controls from the controller’s current state
function render(editorOnly = false) {
    if (editorOnly) return renderEditor();
    renderBackups();
    renderKeystore();
    const report = controller.report;
    const folder = report?.incompatible_data_folder;
    document.body.classList.toggle('incompatible-format', Boolean(folder));
    element('incompatible-data').hidden = !folder;
    text('incompatible-path', folder || '');
    editor.setAttribute('aria-busy', String(controller.busy));
    element('recovery-leave-blocker').hidden = controller.report?.ready === true;
    for (const id of ['recovery-leave-paused', 'recovery-leave-normal'])
        element<HTMLButtonElement>(id).disabled = controller.busy || !controller.report?.ready;
    element<HTMLButtonElement>('recovery-leave-cancel').disabled = controller.busy;
    element<HTMLButtonElement>('recovery-leave-quit').disabled = controller.busy || !isTauri();
    element<HTMLButtonElement>('recovery-open-data').disabled = controller.busy;
    text('recovery-startup-error', report?.startup_error || '');
    element('recovery-startup-details').hidden = !report?.startup_error;
    list('recovery-global-list', report?.issues || []);
    element('recovery-global').hidden = !report?.issues.length;
    text('recovery-backup', report?.last_backup ? `Backup: ${report.last_backup}` : '', true);
    renderItems(report?.items || []);
    element('recovery-empty').hidden = !report || report.items.length > 0 || report.ready;
    element<HTMLButtonElement>('recovery-refresh').disabled = controller.busy;
    element<HTMLButtonElement>('recovery-retry').disabled = controller.busy || !report?.ready;
    renderEditor();
}

function renderEditor() {
    input.readOnly = controller.busy;
    for (const id of [
        'recovery-validate',
        'recovery-save',
        'recovery-editor-close',
        'recovery-editor-cancel',
        'recovery-editor-reload',
        'recovery-compare',
    ]) {
        element<HTMLButtonElement>(id).disabled = controller.busy;
    }
    const validation = controller.validation;
    const issues =
        validation?.issues || (!controller.dirty ? controller.document?.issues : []) || [];
    text(
        'recovery-validation-status',
        validation
            ? validation.valid
                ? 'Valid. Ready to save.'
                : 'Fix these issues before saving.'
            : controller.dirty
              ? 'Changes have not been validated.'
              : 'Review the issues and edit the JSON below.',
    );
    const source = validation || (!controller.dirty ? controller.document : null);
    const diagnostics = source?.diagnostics || [];
    const locations = locateDiagnostics(controller.text, diagnostics);
    jsonEditor.update(
        controller.text,
        locations.filter((location) => location !== null),
    );
    const messageCounts = new Map<string, number>();
    for (const diagnostic of diagnostics)
        messageCounts.set(diagnostic.message, (messageCounts.get(diagnostic.message) || 0) + 1);
    element('recovery-validation-issues').replaceChildren(
        ...[
            ...diagnostics.map((diagnostic, index) => ({
                issue:
                    diagnostic.pointer &&
                    diagnostic.message.includes('Unsupported saved binding draft fields:')
                        ? 'Unsupported draft field'
                        : diagnostic.message,
                details: diagnostic.message,
                path:
                    messageCounts.get(diagnostic.message)! > 1 ||
                    diagnostic.message.includes('Unsupported saved binding draft fields:')
                        ? diagnostic.pointer
                        : null,
                location: locations[index],
            })),
            ...issues
                .filter((issue) => !diagnostics.some((diagnostic) => diagnostic.message === issue))
                .map((issue) => ({ issue, details: issue, path: null, location: null })),
            // Render validation findings with their affected document locations
        ].map(({ issue, details, path, location }) => {
            const li = document.createElement('li');
            if (location) {
                const button = document.createElement('button');
                button.className = 'recovery-issue-link';
                const label = document.createElement('span');
                label.className = 'recovery-issue-message';
                label.textContent = issue + (location.anchor ? ' (containing location)' : '');
                button.append(label);
                button.title = details;
                if (path) {
                    const code = document.createElement('code');
                    code.className = 'recovery-issue-path';
                    code.textContent = path;
                    button.append(code);
                }
                button.addEventListener('click', () => jsonEditor.reveal(location));
                li.append(button);
            } else li.textContent = issue;
            return li;
        }),
    );
    if (!controller.document && editor.open) editor.close();
}
// Open a saved document after resolving unsaved editor changes
async function openEditor(id: string) {
    await controller.open(id);
    if (controller.error || !controller.document || controller.document.id !== id) return;
    input.value = controller.text;
    const item = controller.report?.items.find((entry) => entry.id === id);
    text('recovery-editor-heading', `Edit ${item?.name || 'saved data'}`);
    text('recovery-editor-kind', item?.kind || '');
    editor.showModal();
    render();
    input.focus();
}
// Close the recovery editor after confirming unsaved changes
async function closeEditor() {
    if (controller.busy) return;
    if (
        controller.dirty &&
        !(await confirm(
            'Discard unsaved changes?',
            'Your JSON edits have not been saved. The original saved data will remain unchanged.',
            'Discard changes',
        ))
    )
        return;
    controller.discard();
}
// Reload saved JSON after confirming replacement of the editor draft
async function reloadEditor() {
    if (controller.busy || !controller.document) return;
    if (
        controller.dirty &&
        !(await confirm(
            'Reload saved version?',
            'This discards your unsaved JSON edits and reads the latest saved data.',
            'Reload saved version',
        ))
    )
        return;
    await controller.reloadSaved();
    input.value = controller.text;
}
// Confirm deletion of the exact saved item before removing it
async function removeItem(item: ConfigurationRecoveryItem) {
    // Read a revision before confirmation so a concurrent edit cannot be deleted
    await controller.open(item.id);
    const document = controller.document;
    if (controller.error || !document || document.id !== item.id) return;
    const accepted = await confirm(
        `${item.kind === 'setting' ? 'Reset' : 'Delete'} ${item.name}?`,
        `${item.kind === 'history' ? 'This removes this undo/redo entry, not the live profile.' : item.kind === 'setting' ? 'This removes the saved setting so its default will be used.' : item.kind === 'macro' ? 'This removes the saved macro. Referenced macros cannot be deleted until their callers are repaired.' : `This removes the saved ${item.kind}.`}${item.kind === 'profile' ? ' Its automatic rules, parameter overrides, and session navigation references will also be removed.' : ''} A backup of the original data will be kept. Other invalid items may still need attention.`,
        item.kind === 'setting' ? 'Reset to default' : 'Delete',
    );
    if (accepted) await controller.remove(document);
    else controller.discard();
}
// Route recovery-editor keyboard actions through guarded handlers
editor.addEventListener('keydown', (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'f') {
        event.preventDefault();
        jsonEditor.focusSearch();
    }
});
let leavePending = false;
// Ask how to leave recovery after resolving unsaved changes
async function requestLeave() {
    if (controller.busy || leavePending || leave.open || confirmation.open || versionViewer.open)
        return;
    leavePending = true;
    try {
        await controller.refresh();
        if (controller.error) return;
        leave.showModal();
        element('recovery-leave-cancel').focus();
    } finally {
        leavePending = false;
    }
}
// Continue to Manager with the selected recovery startup mode
async function finishRecovery(recovery: boolean) {
    if (controller.busy || confirmation.open) return;
    if (
        controller.dirty &&
        !(await confirm(
            'Discard unsaved changes?',
            'Your JSON edits have not been saved. Continue without saving them?',
            'Discard changes',
        ))
    )
        return;
    // Submit the reviewed recovery exit choice through the controller
    await controller.perform(async () => {
        await command('configuration_recovery_leave', { recovery });
    });
}
element('incompatible-open').addEventListener('click', async () => {
    text('incompatible-error', '', true);
    try {
        await command('configuration_recovery_open_data');
    } catch (error) {
        text('incompatible-error', String(error), true);
    }
});
element('recovery-open-data').addEventListener(
    'click',
    () =>
        // Apply the selected recovery continuation action
        void controller.perform(async () => {
            await command('configuration_recovery_open_data');
        }),
);
element('recovery-leave-cancel').addEventListener('click', () => leave.close());
element('recovery-leave-paused').addEventListener('click', () => void finishRecovery(true));
element('recovery-leave-normal').addEventListener('click', () => void finishRecovery(false));
element('recovery-leave-quit').addEventListener('click', () => void requestQuit());
// Prevent native dialog cancellation from bypassing the recovery exit flow
leave.addEventListener('cancel', (event) => {
    if (controller.busy) event.preventDefault();
});
let quitPending = false;
// Confirm unsaved recovery edits before quitting Manager
async function requestQuit() {
    if (!isTauri() || controller.busy || quitPending || confirmation.open) return;
    quitPending = true;
    try {
        if (
            controller.dirty &&
            !(await confirm(
                'Discard changes and quit?',
                'Your JSON edits have not been saved. Quit without saving them?',
                'Discard and quit',
            ))
        )
            return;
        // Submit the confirmed application quit request
        await controller.perform(async () => {
            await command('configuration_recovery_quit');
        });
    } finally {
        quitPending = false;
    }
}
if (isTauri()) {
    for (const [event, handler] of [
        ['configuration-recovery-close-requested', requestLeave],
        ['configuration-recovery-quit-requested', requestQuit],
    ] as const)
        // Report failures to attach native recovery-window event handlers
        void listen(event, () => void handler()).catch((error) => {
            controller.error = String(error);
            toasts.notify(controller.error, 'error');
            render();
        });
}
input.addEventListener('input', () => controller.edit(input.value));
element('recovery-backup-select').addEventListener(
    'change',
    (event) => void controller.selectBackup((event.currentTarget as HTMLSelectElement).value),
);
element('recovery-preview-restore').addEventListener(
    'click',
    () => void controller.previewBackup(),
);
element('recovery-restore-backup').addEventListener('click', () => void restoreBackup());
element('recovery-backup-transfer').addEventListener('click', () => void transferBackupProfiles());
element('recovery-keystore-backup-select').addEventListener('change', (event) => {
    keystore.select((event.currentTarget as HTMLSelectElement).value);
    renderKeystore();
});
element('recovery-keystore-retry').addEventListener('click', () => void keystore.retry());
element('recovery-keystore-create').addEventListener('click', () => void keystore.change('create'));
element('recovery-keystore-reset').addEventListener('click', () => void keystore.change('reset'));
element('recovery-keystore-restore').addEventListener(
    'click',
    () => void keystore.change('restore'),
);
element('recovery-refresh').addEventListener('click', () => void refreshRecovery());
element('recovery-retry').addEventListener('click', () => void controller.retry());
// Open comparison of the edited document with its saved history
element('recovery-compare').addEventListener('click', () => {
    if (controller.document && !controller.busy)
        void versionViewer.show(
            controller.document,
            controller.text,
            controller.report?.items.find((item) => item.id === controller.document!.id)?.kind ===
                'profile',
        );
});
element('recovery-editor-reload').addEventListener('click', () => void reloadEditor());
element('recovery-validate').addEventListener('click', () => void controller.validate());
element('recovery-save').addEventListener('click', () => void controller.save());
element('recovery-editor-close').addEventListener('click', () => void closeEditor());
element('recovery-editor-cancel').addEventListener('click', () => void closeEditor());
// Route Escape through the editor’s unsaved-change handling
editor.addEventListener('cancel', (event) => {
    event.preventDefault();
    void closeEditor();
});
element('recovery-confirm-cancel').addEventListener('click', () => finishConfirm(false));
element('recovery-confirm-accept').addEventListener('click', () => finishConfirm(true));
// Resolve cancelled confirmation dialogs as declined
confirmation.addEventListener('cancel', (event) => {
    event.preventDefault();
    finishConfirm(false);
});
// Warn before unloading a recovery document with unsaved edits
window.addEventListener('beforeunload', (event) => {
    if (controller.dirty || controller.busy) {
        event.preventDefault();
        event.returnValue = '';
    }
});
void refreshRecovery();
