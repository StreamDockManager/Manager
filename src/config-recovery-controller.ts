import type {
    ConfigurationRecoveryReport,
    ConfigurationRecoveryDocument,
    ConfigurationRecoveryValidation,
    ConfigurationRecoveryBackups,
    ConfigurationRecoveryBackupProfiles,
    ConfigurationRecoveryRestorePreview,
} from '../generated/runtime';

export interface ConfigurationRecoveryApi {
    status(): Promise<ConfigurationRecoveryReport>;
    read(id: string): Promise<ConfigurationRecoveryDocument>;
    validate(id: string, revision: string, json: string): Promise<ConfigurationRecoveryValidation>;
    save(id: string, revision: string, json: string): Promise<ConfigurationRecoveryReport>;
    delete(id: string, revision: string): Promise<ConfigurationRecoveryReport>;
    retry(): Promise<unknown>;
    previewRestore?(
        backupId: string,
        revision: string,
    ): Promise<ConfigurationRecoveryRestorePreview>;
    restoreBackup?(
        backupId: string,
        revision: string,
        reviewToken: string,
    ): Promise<ConfigurationRecoveryReport>;
    backups?(): Promise<ConfigurationRecoveryBackups>;
    backupProfiles?(
        backupId: string,
        revision: string,
    ): Promise<ConfigurationRecoveryBackupProfiles>;
    transferProfiles?(
        backupId: string,
        revision: string,
        profileIds: string[],
    ): Promise<ConfigurationRecoveryReport>;
}

/** Recovery edits never enter the normal workspace's draft or persistence queues */
export class ConfigurationRecoveryController {
    report: ConfigurationRecoveryReport | null = null;
    document: ConfigurationRecoveryDocument | null = null;
    text = '';
    busy = false;
    error = '';
    notice = '';
    backupCatalog: ConfigurationRecoveryBackups | null = null;
    backupProfiles: ConfigurationRecoveryBackupProfiles | null = null;
    backupId = '';
    selectedProfiles: string[] = [];
    backupNeedsRefresh = false;
    backupNotice = '';
    restorePreview: ConfigurationRecoveryRestorePreview | null = null;
    restoreError = '';
    // Allow backup review only when no operation or unsaved document blocks it
    get canPreviewRestore() {
        return (
            !this.busy &&
            !this.dirty &&
            !!this.backupCatalog?.backups.some((backup) => backup.id === this.backupId)
        );
    }
    // Allow restore only for the currently selected validated backup review
    get canRestoreBackup() {
        return (
            this.canPreviewRestore &&
            this.restorePreview?.valid === true &&
            this.restorePreview.backup_id === this.backupId
        );
    }
    // Request a validated restore preview for the selected backup
    previewBackup() {
        if (!this.canPreviewRestore || !this.api.previewRestore) return Promise.resolve();
        // Clear the previous restore review before validating the selected backup
        return this.run(async () => {
            this.restorePreview = null;
            this.restoreError = '';
            try {
                this.restorePreview = await this.api.previewRestore!(
                    this.backupId,
                    this.backupCatalog!.revision,
                );
            } catch (error) {
                this.restoreError = String(error);
                throw error;
            }
        });
    }
    /** Called only after explicit confirmation of this exact validated review */
    restoreBackup() {
        if (!this.canRestoreBackup || !this.api.restoreBackup) return Promise.resolve();
        // Restore the confirmed review and invalidate stale document and backup state
        return this.run(async () => {
            const preview = this.restorePreview!;
            this.restoreError = '';
            try {
                this.report = await this.api.restoreBackup!(
                    preview.backup_id,
                    preview.revision,
                    preview.review_token,
                );
                this.document = null;
                this.text = '';
                this.validation = null;
                this.restorePreview = null;
                this.backupProfiles = null;
                this.selectedProfiles = [];
                this.backupId = '';
                this.backupNotice =
                    'Database restored. The previous database was preserved. Review the saved items, then choose Continue to Manager when ready.';
                this.notify(this.backupNotice, 'success');
                await this.refreshBackups();
            } catch (error) {
                // Publication may have fenced or replaced the current database before
                // failing. Never leave an earlier ready report on screen
                this.report = null;
                this.restorePreview = null;
                this.backupProfiles = null;
                this.selectedProfiles = [];
                this.backupNeedsRefresh = true;
                try {
                    this.report = await this.api.status();
                    await this.refreshBackups();
                } catch {
                    // Unknown state stays non-ready; retain the original restore error
                    this.report = null;
                    this.backupNeedsRefresh = true;
                }
                this.restoreError = String(error);
                throw error;
            }
        });
    }
    // Allow selected-profile transfer only from a current readable backup view
    get canTransferProfiles() {
        return (
            !this.busy &&
            !this.dirty &&
            !this.backupNeedsRefresh &&
            this.report?.primary_readable !== false &&
            !!this.backupProfiles &&
            this.selectedProfiles.length > 0
        );
    }
    // Refresh the backup inventory and invalidate selections when its revision changes
    private async refreshBackups() {
        if (!this.api.backups || this.report?.incompatible_data_folder) return;
        const catalog = await this.api.backups();
        if (
            this.backupNeedsRefresh ||
            catalog.revision !== this.backupCatalog?.revision ||
            !catalog.backups.some((backup) => backup.id === this.backupId)
        ) {
            this.backupProfiles = null;
            this.restorePreview = null;
            this.restoreError = '';
            this.selectedProfiles = [];
            this.backupId = '';
        }
        this.backupCatalog = catalog;
        this.backupNeedsRefresh = false;
    }
    // Select a backup and load its transferable profiles
    selectBackup(id: string) {
        // Clear the old selection before reading the chosen backup
        return this.run(async () => {
            this.backupProfiles = null;
            this.selectedProfiles = [];
            this.backupNotice = '';
            this.restorePreview = null;
            this.restoreError = '';
            this.backupId = id;
            if (
                !id ||
                !this.api.backupProfiles ||
                this.report?.primary_readable === false ||
                !this.backupCatalog?.backups.some((backup) => backup.id === id)
            )
                return;
            try {
                this.backupProfiles = await this.api.backupProfiles(
                    id,
                    this.backupCatalog.revision,
                );
                this.backupNeedsRefresh = false;
            } catch (error) {
                this.backupNeedsRefresh = true;
                throw error;
            }
        });
    }
    // Toggle only valid profiles from the current backup inventory
    toggleBackupProfile(id: string, checked: boolean) {
        if (
            this.busy ||
            this.backupNeedsRefresh ||
            !this.backupProfiles?.profiles.some((profile) => profile.id === id && profile.valid)
        )
            return;
        this.selectedProfiles = checked
            ? [...new Set([...this.selectedProfiles, id])]
            : this.selectedProfiles.filter((value) => value !== id);
        this.changed();
    }
    /** Called only after the UI confirms this exact selection */
    transferBackupProfiles() {
        if (!this.canTransferProfiles) return Promise.resolve();
        // Transfer the confirmed valid profiles as new copies
        return this.run(async () => {
            const profiles = this.backupProfiles;
            if (!profiles || !this.api.transferProfiles) return;
            const ids = this.selectedProfiles.filter((id) =>
                profiles.profiles.some((profile) => profile.id === id && profile.valid),
            );
            if (!ids.length) return;
            try {
                this.report = await this.api.transferProfiles(
                    profiles.backup_id,
                    profiles.revision,
                    ids,
                );
                this.restorePreview = null;
                this.backupNotice = `Transferred ${ids.length} ${ids.length === 1 ? 'profile' : 'profiles'} as new copies. The backup and existing profiles are unchanged.`;
                this.selectedProfiles = [];
                this.backupProfiles = null;
                this.backupId = '';
                this.notify(this.backupNotice, 'success');
                await this.refreshBackups();
            } catch (error) {
                this.selectedProfiles = [];
                this.backupNeedsRefresh = true;
                throw error;
            }
        });
    }
    validation: ConfigurationRecoveryValidation | null = null;
    // Initialize recovery operations and their UI notification hooks
    constructor(
        readonly api: ConfigurationRecoveryApi,
        // Leave change notification inert when no view is attached
        readonly changed: (editorOnly?: boolean) => void = () => {},
        // Leave user notification inert when no notifier is supplied
        readonly notify: (message: string, level: 'info' | 'success' | 'error') => void = () => {},
    ) {}
    // Compare the edited JSON with its loaded saved document
    get dirty() {
        return this.document !== null && this.text !== this.document.json;
    }
    // Serialize recovery operations and report failures to the view
    private async run(operation: () => Promise<void>) {
        if (this.busy) return;
        this.busy = true;
        this.error = '';
        this.changed();
        try {
            await operation();
        } catch (error) {
            this.error = String(error);
            this.notify(this.error, 'error');
        } finally {
            this.busy = false;
            this.changed();
        }
    }
    // Run an external recovery operation through the busy-state guard
    perform(operation: () => Promise<void>) {
        return this.run(operation);
    }
    // Refresh recovery readiness and the available backups
    refresh() {
        // Notify when a fresh status report becomes ready to continue
        return this.run(async () => {
            const wasReady = this.report?.ready;
            this.report = await this.api.status();
            await this.refreshBackups();
            if (this.report.ready && wasReady !== true)
                this.notify('Saved data is valid. You can continue to Manager.', 'success');
        });
    }
    // Load a saved recovery document for editing
    open(id: string) {
        // Replace the editor with the requested saved document
        return this.run(async () => {
            const document = await this.api.read(id);
            this.document = document;
            this.text = document.json;
            this.validation = null;
            this.notice = '';
        });
    }
    /** Refresh the storage snapshot before rereading after a concurrent write */
    reloadSaved() {
        // Refresh status before rereading the current saved document
        return this.run(async () => {
            const id = this.document?.id;
            if (!id) return;
            this.report = await this.api.status();
            const document = await this.api.read(id);
            // Keep unsaved text/revision intact unless both reads succeed
            this.document = document;
            this.text = document.json;
            this.validation = null;
            this.notice = '';
        });
    }
    // Update editable JSON and invalidate its earlier validation
    edit(text: string) {
        if (this.busy) return;
        this.text = text;
        this.validation = null;
        this.error = '';
        this.notice = '';
        this.changed(true);
    }
    // Discard the loaded document and unsaved recovery text
    discard() {
        if (this.busy) return;
        this.document = null;
        this.text = '';
        this.validation = null;
        this.error = '';
        this.changed();
    }
    // Validate the edited JSON against its saved document revision
    validate() {
        // Submit the current document revision and text for validation
        return this.run(async () => {
            const document = this.document;
            if (!document) return;
            this.validation = await this.api.validate(document.id, document.revision, this.text);
        });
    }
    // Validate and save the exact current recovery document
    save() {
        // Save only after validating the exact submitted text
        return this.run(async () => {
            const document = this.document;
            if (!document) return;
            // Always validate the exact submitted text, even following a successful Validate
            this.validation = await this.api.validate(document.id, document.revision, this.text);
            if (!this.validation.valid) return;
            this.report = await this.api.save(document.id, document.revision, this.text);
            this.restorePreview = null;
            this.document = null;
            this.text = '';
            this.validation = null;
            this.notice = 'Saved. The original data was backed up.';
            this.notify(this.notice, 'success');
            this.changed();
        });
    }
    /** Called only after the UI confirms this exact document and revision */
    remove(document: ConfigurationRecoveryDocument) {
        // Delete the confirmed document revision and clear matching editor state
        return this.run(async () => {
            this.report = await this.api.delete(document.id, document.revision);
            this.restorePreview = null;
            if (this.document?.id === document.id) {
                this.document = null;
                this.text = '';
                this.validation = null;
            }
            this.notice = 'Deleted. The original data was backed up.';
            this.notify(this.notice, 'success');
            this.changed();
        });
    }
    // Recheck readiness before choosing how to continue to Manager
    retry() {
        // Keep recovery open until the latest status is ready
        return this.run(async () => {
            this.report = await this.api.status();
            if (!this.report.ready) {
                this.notice = 'Resolve the remaining issues before reloading Manager.';
                this.notify(this.notice, 'info');
                return;
            }
            this.notice = 'Choose how to continue to Manager.';
            this.changed();
            await this.api.retry();
        });
    }
}
