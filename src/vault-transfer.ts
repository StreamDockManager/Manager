import Alpine from '@alpinejs/csp';
import type {
    JsonValue,
    VaultEntry,
    VaultExportSelection,
    VaultImportResult,
    VaultImportReview,
    VaultImportRow,
} from '../generated/runtime';
import { bridge, native } from './bridge';
import * as fileDialogs from './window-dialogs';
import { windowCloseOwners } from './window-close-owners';
import { trackCloseRelease } from './close-releases';
import { windowInvoke } from './window-native';
import { app } from './route-context';
import { copyVaultValueText } from './vault-value-clipboard';

export type TransferSelection = VaultExportSelection;
export type TransferRow = VaultImportRow;
export type TransferReview = VaultImportReview;
export type TransferResult = VaultImportResult;
export interface VaultTransferService {
    exportFile(
        path: string,
        entries: TransferSelection[],
        password: string | null,
        allowPlaintext: boolean,
        master: string | null,
    ): Promise<number>;
    reviewFile(path: string, password: string | null): Promise<TransferReview>;
    reviewDrop(file: File, password: string | null): Promise<TransferReview>;
    reveal(id: string, row: number): Promise<{ value: JsonValue }>;
    apply(id: string, overwrite: number[]): Promise<TransferResult>;
    cancel(id: string): Promise<void>;
    openPath(): Promise<string | null>;
    savePath(): Promise<string | null>;
}

type TransferStage = '' | 'export' | 'choose' | 'password' | 'review' | 'complete';
type TransferOpen = CustomEvent<{
    mode: 'export' | 'import';
    entries: VaultEntry[];
    passwordMode: boolean;
}>;
const clearError = 'Manager could not complete the Vault transfer. Try again.';
const fileFilters = [{ name: 'Vault transfer', extensions: ['sdmvault'] }];
const fileLimit = 17_040_384;
const passwordLimit = 1024;
const exportPageSize = 6;

// Show only fixed, actionable messages, never raw native errors or file data
function transferError(error: unknown) {
    switch (typeof error === 'string' ? error : error instanceof Error ? error.message : '') {
        case 'vault_locked':
            return 'Unlock Vault before continuing.';
        case 'vault_transfer_password_required':
            return 'Enter the transfer password for this file.';
        case 'archive_authentication_failed':
        case 'vault_transfer_authentication_failed':
            return 'The transfer password did not unlock this file.';
        case 'vault_import_expired':
        case 'vault_transfer_stale':
        case 'stale_revision':
            return 'Vault changed or the review expired. Choose the file again to review the latest entries.';
        case 'password_required':
        case 'vault_master_password_required':
            return 'Verify your current Vault master password to export protected entries.';
        case 'master_password_mode_required':
            return 'Use a Vault master password before exporting protected entries.';
        case 'incorrect_password':
            return 'That Vault master password is incorrect.';
        case 'The transfer password is incorrect or the file is damaged.':
            return 'The transfer password is incorrect or the file is damaged.';
        case 'invalid_password':
            return 'Enter a nonempty password up to 1024 UTF-8 bytes.';
        case 'rate_limited':
            return 'Wait a moment before trying another password.';
        case 'vault_transfer_destination_exists':
            return 'Choose a new file name. Existing files will not be replaced.';
        case 'vault_transfer_inside_app_data':
            return 'Choose a folder outside Manager app data.';
        case 'vault_transfer_busy':
        case 'vault_import_busy':
            return 'Another Vault transfer is in progress. Wait for it to finish and try again.';
        case 'vault_limit':
            return 'The selected entries exceed the Vault limits. Select fewer entries or reduce the import.';
        case 'vault_transfer_invalid_path':
            return 'Choose a valid file in a folder you can access.';
        case 'vault_transfer_write_failed':
            return 'Manager could not write the file. Choose a writable folder and try again.';
        case 'vault_transfer_invalid':
            return 'This is not a supported Vault transfer file, or it is damaged.';
        case 'vault_transfer_too_large':
            return 'Choose a Vault transfer file up to 16 MiB and a password up to 1024 UTF-8 bytes.';
        default:
            return clearError;
    }
}

// Build the editor-owned transfer dialog, with no credential fields in reactive state
export function vaultTransferController(service: VaultTransferService | null) {
    let dialog: HTMLDialogElement | undefined;
    return {
        stage: '' as TransferStage,
        selection: [] as TransferSelection[],
        exportPage: 1,
        entries: [] as VaultEntry[],
        passwordMode: false,
        encrypted: true,
        acknowledge: false,
        busy: false,
        committing: false,
        error: '',
        completion: '',
        exportPath: '',
        importPath: '',
        dropFile: null as File | null,
        review: null as TransferReview | null,
        overwrites: [] as number[],
        revealRow: null as number | null,
        version: 0,
        revealVersion: 0,
        unregister: undefined as (() => void) | undefined,
        get reviewRows() {
            return this.review?.rows ?? [];
        },
        get exportPageCount() {
            return Math.max(1, Math.ceil(this.entries.length / exportPageSize));
        },
        get exportPageRows() {
            const start = (this.exportPage - 1) * exportPageSize;
            return this.entries.slice(start, start + exportPageSize);
        },
        get exportSelectionLabel() {
            return `${this.selection.length} of ${this.entries.length} selected`;
        },
        get allExportEntriesSelected() {
            return this.entries.length > 0 && this.selection.length === this.entries.length;
        },
        get needsMaster() {
            return this.entries.some(
                (entry) => entry.protected && this.selection.some((item) => item.id === entry.id),
            );
        },
        get canExport() {
            return (
                !this.busy &&
                !!this.exportPath &&
                this.selection.length > 0 &&
                (!this.needsMaster || this.passwordMode) &&
                (this.encrypted || this.acknowledge)
            );
        },
        get readyToImport() {
            return (
                !this.busy &&
                !!this.review &&
                this.review.rows.some(
                    (row) => row.status === 'new' || this.overwrites.includes(row.row),
                )
            );
        },
        get title() {
            if (this.stage === 'complete') return 'Vault transfer complete';
            return this.stage === 'export'
                ? 'Export Vault entries'
                : this.stage === 'review'
                  ? 'Review Vault import'
                  : 'Import Vault entries';
        },
        // Resolve the dialog without copying it into Alpine's reactive state
        dialog() {
            return dialog;
        },
        init() {
            dialog = (this as unknown as { $el?: HTMLDialogElement }).$el;
            this.unregister = windowCloseOwners.register({
                name: 'Vault transfer dialog',
                state: () => 'clean',
                discard: () => this.close(),
                release: () => this.close(),
            });
        },
        destroy() {
            this.unregister?.();
            this.close();
            dialog = undefined;
        },
        dialogClosed() {
            if (!this.dialog()?.open) this.close();
        },
        constrainTransfer(unlocked: boolean) {
            if (!unlocked && this.stage) this.close();
        },
        visibilityChanged() {
            if (document.hidden && this.stage) this.close();
        },
        // Open the selected transfer from the personal entries panel
        openTransfer(event: TransferOpen) {
            if (!service || this.stage) return;
            this.passwordMode = event.detail.passwordMode;
            if (event.detail.mode === 'export') {
                this.stage = 'export';
                this.entries = event.detail.entries
                    .filter((entry) => entry.owner == null)
                    .map((entry) => ({ ...entry }));
            } else this.stage = 'choose';
            this.dialog()?.showModal();
        },
        dismiss() {
            if (!this.committing) this.close();
        },
        close() {
            if (!this.stage) return;
            const review = this.review?.id;
            ++this.version;
            this.clearReveal();
            this.clearPasswords();
            this.stage = '';
            this.exportPage = 1;
            this.review = null;
            this.entries = [];
            this.selection = [];
            this.overwrites = [];
            this.importPath = '';
            this.exportPath = '';
            this.dropFile = null;
            this.error = '';
            this.completion = '';
            this.busy = false;
            this.committing = false;
            this.acknowledge = false;
            this.encrypted = true;
            this.dialog()?.close();
            if (review) void this.cancelReview(review);
        },
        cancelReview(id: string) {
            return trackCloseRelease(
                `vault-transfer:${id}`,
                () => service?.cancel(id) ?? Promise.resolve(),
            ).catch(() => {});
        },
        clearPasswords() {
            for (const input of this.dialog()?.querySelectorAll<HTMLInputElement>(
                'input[type=password]',
            ) ?? [])
                input.value = '';
        },
        clearReveal() {
            ++this.revealVersion;
            this.revealRow = null;
            const text = this.dialog()?.querySelector('[data-transfer-value]');
            if (text) text.textContent = '';
        },
        isSelected(id: string) {
            return this.selection.some((entry) => entry.id === id);
        },
        selectEntry(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const id = input.dataset.entryId;
            const entry = this.entries.find((entry) => entry.id === id);
            if (!entry || this.busy) return;
            this.selection = this.selection.filter((item) => item.id !== id);
            if (input.checked) this.selection.push({ id: entry.id, revision: entry.revision });
            if (!this.needsMaster) {
                const password = this.dialog()?.querySelector<HTMLInputElement>('[name=master]');
                if (password) password.value = '';
            }
        },
        // Keep selection independent of which page is visible
        selectAll() {
            if (this.busy) return;
            this.selection = this.allExportEntriesSelected
                ? []
                : this.entries.map(({ id, revision }) => ({ id, revision }));
            if (!this.selection.length) this.clearPasswords();
        },
        // Reset the list scroll while leaving the selection and dialog position intact
        navigateExport(step: number) {
            if (this.busy) return;
            this.exportPage = Math.max(1, Math.min(this.exportPageCount, this.exportPage + step));
            const list = this.dialog()?.querySelector<HTMLElement>('[data-vault-export-list]');
            if (list) list.scrollTop = 0;
        },
        changeEncryption(event: Event) {
            this.encrypted = (event.currentTarget as HTMLInputElement).checked;
            this.acknowledge = false;
            this.clearPasswords();
        },
        async chooseExportPath() {
            if (!service || this.busy || this.stage !== 'export') return;
            const generation = this.version;
            this.busy = true;
            try {
                const path = await service.savePath();
                if (path && generation === this.version) this.exportPath = path;
            } catch (error) {
                if (generation === this.version) this.error = transferError(error);
            } finally {
                if (generation === this.version) this.busy = false;
            }
        },
        async submitExport(event: SubmitEvent) {
            if (!service || !this.canExport) return;
            const form = new FormData(event.currentTarget as HTMLFormElement);
            let password = this.encrypted ? String(form.get('exportPassword') ?? '') : null;
            let master = this.needsMaster ? String(form.get('master') ?? '') : null;
            const equal = !this.encrypted || password === form.get('confirmPassword');
            for (const key of ['exportPassword', 'confirmPassword', 'master']) form.delete(key);
            this.clearPasswords();
            if (!equal || password === '' || master === '') {
                this.error = !equal
                    ? 'Transfer passwords must match.'
                    : 'Enter the required password.';
                return;
            }
            const generation = this.version;
            this.error = '';
            this.busy = true;
            try {
                this.committing = true;
                const count = await service.exportFile(
                    this.exportPath,
                    this.selection.map((entry) => ({ ...entry })),
                    password,
                    !this.encrypted && this.acknowledge,
                    master,
                );
                if (generation === this.version) {
                    this.selection = [];
                    this.entries = [];
                    this.exportPath = '';
                    this.completion = `Exported ${count} Vault ${count === 1 ? 'entry' : 'entries'}.`;
                    this.stage = 'complete';
                }
            } catch (error) {
                if (generation === this.version) this.error = transferError(error);
            } finally {
                password = master = null;
                if (generation === this.version) this.busy = this.committing = false;
            }
        },
        async chooseFile() {
            if (!service || this.busy) return;
            const generation = this.version;
            this.busy = true;
            this.error = '';
            try {
                const path = await service.openPath();
                if (generation !== this.version || !path) return;
                this.dropFile = null;
                this.importPath = path;
                await this.readFile(null, generation);
            } catch (error) {
                if (generation === this.version) this.error = transferError(error);
            } finally {
                if (generation === this.version) this.busy = false;
            }
        },
        async acceptDrop(event: CustomEvent<File>) {
            const file = event.detail;
            if (
                !service ||
                (this.stage && this.stage !== 'choose') ||
                this.busy ||
                !file ||
                !/\.sdmvault$/i.test(file.name)
            )
                return;
            if (!this.stage) {
                this.stage = 'choose';
                this.dialog()?.showModal();
            }
            if (file.size < 1 || file.size > fileLimit) {
                this.error = 'Choose a Vault transfer file up to 16 MiB.';
                return;
            }
            const generation = this.version;
            this.busy = true;
            this.error = '';
            this.importPath = '';
            this.dropFile = file;
            try {
                await this.readFile(null, generation);
            } finally {
                if (generation === this.version) this.busy = false;
            }
        },
        async submitPassword(event: SubmitEvent) {
            if (!service || this.busy || this.stage !== 'password') return;
            const form = new FormData(event.currentTarget as HTMLFormElement);
            const password = String(form.get('importPassword') ?? '');
            form.delete('importPassword');
            this.clearPasswords();
            if (!password) return;
            const generation = this.version;
            this.busy = true;
            this.error = '';
            try {
                await this.readFile(password, generation);
            } finally {
                if (generation === this.version) this.busy = false;
            }
        },
        async readFile(password: string | null, generation: number) {
            if (!service || (!this.importPath && !this.dropFile)) return;
            try {
                const review = this.dropFile
                    ? await service.reviewDrop(this.dropFile, password)
                    : await service.reviewFile(this.importPath, password);
                if (generation !== this.version) {
                    await this.cancelReview(review.id);
                    return;
                }
                this.importPath = '';
                this.dropFile = null;
                this.review = review;
                this.overwrites = [];
                this.stage = 'review';
            } catch (error) {
                if (generation !== this.version) return;
                if (
                    error === 'vault_transfer_password_required' ||
                    (error instanceof Error && error.message === 'vault_transfer_password_required')
                ) {
                    this.stage = 'password';
                } else {
                    this.error = transferError(error);
                    if (this.stage !== 'password') {
                        this.dropFile = null;
                        this.importPath = '';
                    }
                }
            }
        },
        rowIsOverwrite(row: number) {
            return this.overwrites.includes(row);
        },
        chooseOverwrite(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const row = Number(input.dataset.transferRow);
            if (
                this.busy ||
                !this.review?.rows.some((item) => item.row === row && item.status === 'conflict')
            )
                return;
            this.overwrites = this.overwrites.filter((value) => value !== row);
            if (input.checked) this.overwrites.push(row);
        },
        async showIncoming(event: Event) {
            const row = Number((event.currentTarget as HTMLElement).dataset.transferRow);
            const review = this.review;
            const hide = this.revealRow === row;
            this.clearReveal();
            if (
                !service ||
                hide ||
                this.busy ||
                !review?.rows.some((item) => item.row === row && item.status === 'conflict')
            )
                return;
            const generation = this.version;
            const revision = this.revealVersion;
            this.error = '';
            try {
                const result = await service.reveal(review.id, row);
                if (
                    generation !== this.version ||
                    revision !== this.revealVersion ||
                    this.review?.id !== review.id
                )
                    return;
                const text = this.dialog()?.querySelector('[data-transfer-value]');
                if (text) {
                    text.textContent =
                        typeof result.value === 'string'
                            ? result.value
                            : JSON.stringify(result.value, null, 2);
                    this.revealRow = row;
                }
            } catch (error) {
                if (generation === this.version && revision === this.revealVersion)
                    this.error = transferError(error);
            }
        },
        // Copy only the incoming value already visible in this open review
        async copyIncomingValue() {
            const dialog = this.dialog();
            if (!dialog?.open || this.stage !== 'review' || this.revealRow === null || this.busy)
                return;
            const value = dialog.querySelector('[data-transfer-value]')?.textContent;
            if (value == null) return;
            const revision = this.revealVersion;
            try {
                await copyVaultValueText(value);
                if (revision === this.revealVersion) app().notify('Value copied', 'success');
            } catch {
                if (revision === this.revealVersion) app().notify('Could not copy value', 'error');
            }
        },
        rowStatus(row: TransferRow) {
            return row.status === 'new'
                ? 'New entry'
                : row.status === 'conflict'
                  ? 'Existing entry'
                  : 'Ambiguous match · skipped';
        },
        async applyImport() {
            if (!service || !this.readyToImport || !this.review) return;
            this.clearReveal();
            const id = this.review.id;
            const generation = this.version;
            this.committing = this.busy = true;
            this.error = '';
            try {
                const result = await service.apply(id, [...this.overwrites]);
                if (generation === this.version) {
                    this.review = null;
                    this.overwrites = [];
                    this.completion = `Added: ${result.created}. Replaced: ${result.overwritten}. Skipped: ${result.skipped}.`;
                    this.stage = 'complete';
                    this.dialog()?.dispatchEvent(
                        new CustomEvent('vault-personal-transfer-applied', { bubbles: true }),
                    );
                }
            } catch (error) {
                if (generation === this.version) this.error = transferError(error);
            } finally {
                if (generation === this.version) this.busy = this.committing = false;
            }
        },
    };
}

// Keep the picker native; no transfer can read or write files in a browser preview
export const chooseVaultTransferFile = () =>
    fileDialogs.open({ multiple: false, filters: fileFilters });
export const saveVaultTransferFile = () =>
    fileDialogs.save({ defaultPath: 'vault-entries.sdmvault', filters: fileFilters });

export function registerVaultTransfer(service: VaultTransferService | null) {
    Alpine.data('vaultTransfer', () => vaultTransferController(native ? service : null));
}

// Send one bounded raw File without reading its plaintext into a JS string
export async function reviewDroppedVaultFile(
    file: File,
    password: string | null,
    invoke = windowInvoke,
): Promise<TransferReview> {
    const key = new TextEncoder().encode(password ?? '');
    let data: ArrayBuffer | undefined;
    let payload: Uint8Array<ArrayBuffer> | undefined;
    try {
        if (file.size < 1 || file.size > fileLimit || key.length > passwordLimit)
            throw Error('vault_transfer_too_large');
        data = await file.arrayBuffer();
        payload = new Uint8Array(4 + key.length + data.byteLength);
        new DataView(payload.buffer).setUint32(0, key.length, true);
        payload.set(key, 4);
        payload.set(new Uint8Array(data), 4 + key.length);
        new Uint8Array(data).fill(0);
        return await invoke<TransferReview>('vault_prepare_dropped_entry_import', payload.buffer);
    } finally {
        key.fill(0);
        if (data) new Uint8Array(data).fill(0);
        payload?.fill(0);
    }
}

export function desktopVaultTransferService(): VaultTransferService {
    return {
        exportFile: (path, selected, password, allowPlaintext, masterPassword) =>
            bridge.command('vault_export_entries', {
                path,
                selected,
                password,
                allowPlaintext,
                masterPassword,
            }),
        reviewFile: (path, password) =>
            bridge.command('vault_prepare_entry_import', { path, password }),
        reviewDrop: reviewDroppedVaultFile,
        reveal: (reviewId, row) => bridge.command('vault_reveal_entry_import', { reviewId, row }),
        apply: (reviewId, overwrite) =>
            bridge.command('vault_apply_entry_import', { reviewId, overwrite }),
        cancel: (reviewId) => bridge.command('vault_cancel_entry_import', { reviewId }),
        openPath: chooseVaultTransferFile,
        savePath: saveVaultTransferFile,
    };
}
