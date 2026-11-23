import type {
    ConfigurationRecoveryKeystoreBackup as KeystoreBackup,
    ConfigurationRecoveryKeystoreReview,
    ConfigurationRecoveryKeystoreStatus as KeystoreStatus,
} from '../generated/runtime';

type KeystoreOperation = 'create' | 'reset' | 'restore';

const interruptedCreation =
    'An interrupted plugin settings keystore creation must be resumed in Recovery. Original files are preserved.';

export interface ConfigurationRecoveryKeystoreApi {
    status(): Promise<KeystoreStatus>;
    retry(revision: string): Promise<KeystoreStatus>;
    review(
        revision: string,
        operation: KeystoreOperation,
        backupId: string | null,
    ): Promise<ConfigurationRecoveryKeystoreReview>;
    apply(revision: string, reviewToken: string): Promise<KeystoreStatus>;
}

interface KeystoreRecoveryView {
    blocked(): boolean;
    perform(operation: () => Promise<void>): Promise<void>;
    confirm(title: string, message: string, action: string, warnings: string[]): Promise<boolean>;
    invalidateRecovery(): void;
    refreshRecovery(): Promise<void>;
}

const dateFormat = new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
});

// Keep backup IDs as opaque selectors, not filenames or paths presented as trustworthy labels
export function keystoreBackupLabel(backup: KeystoreBackup, index: number) {
    const date = new Date(backup.modified_ms);
    const dateLabel = Number.isFinite(date.getTime()) ? ` · ${dateFormat.format(date)}` : '';
    return `Encrypted backup ${index + 1}${dateLabel}`;
}

// Only recognize exact native reason messages; never display an arbitrary IPC error or path.
export function keystoreRecoveryAdvice(error: unknown, fallback = '') {
    const message = typeof error === 'string' ? error : error instanceof Error ? error.message : '';
    switch (message) {
        case interruptedCreation:
            return 'An earlier creation did not finish. Resume it here. Original files are preserved.';
        case 'Manager cannot open its plugin settings keystore. Restore the keystore and its installation key, or unlock the OS credential store and restart. Your data has been preserved.':
        case 'The OS installation credential store is locked or unavailable. Unlock it and retry; no installation credentials were deleted.':
        case 'The installation key file is unavailable':
            return 'Unlock the operating-system credential store or make the original installation key available, then retry. An encrypted backup cannot replace the key.';
        case 'The backup does not belong to the current plugin settings installation':
        case 'Plugin settings revision is missing or damaged. Restore the matching keystore; data preserved.':
            return 'The encrypted keystore must match this configuration database. Choose a matching backup; the originals were preserved.';
        case 'Configuration changed. Reload recovery before saving; originals preserved.':
        case 'Plugin settings backup selection changed; review again':
        case 'Encrypted plugin settings backup changed; review it again':
        case 'Encrypted plugin settings backup changed during validation':
        case 'Review the keystore operation again before applying it':
            return 'The saved data or selected backup changed. Choose Check again and review the operation again.';
        case 'Remove the explicit installation key file setting before resetting the OS installation keystore. The key file was not changed.':
            return 'An explicit installation key is configured. Remove that setting before resetting the OS keystore. The key file was not changed.';
        case 'A database recovery operation is unfinished; resolve it before restoring plugin settings':
            return 'Finish the pending database recovery before restoring the plugin settings keystore.';
        case 'Encrypted plugin settings backup failed integrity validation':
            return 'The encrypted backup did not pass integrity checks. Choose another backup.';
        default:
            return fallback;
    }
}

export class ConfigurationRecoveryKeystore {
    status: KeystoreStatus | null = null;
    backupId = '';
    error = '';
    notice = '';

    constructor(
        private readonly api: ConfigurationRecoveryKeystoreApi,
        private readonly view: KeystoreRecoveryView,
    ) {}

    get backup() {
        return this.status?.backups.find((item) => item.id === this.backupId);
    }

    get canRetry() {
        return !this.view.blocked() && !!this.status?.revision;
    }

    get canReset() {
        return this.canRetry && this.status?.state === 'unavailable';
    }

    get canCreate() {
        return this.canRetry && this.status?.state === 'missing';
    }

    get creationInterrupted() {
        return this.status?.state === 'missing' && this.status.reason === interruptedCreation;
    }

    get canRestore() {
        return (
            this.canRetry &&
            ['unavailable', 'missing'].includes(this.status!.state) &&
            !!this.backup
        );
    }

    select(id: string) {
        if (!this.canRetry) return;
        this.backupId = this.status?.backups.some((item) => item.id === id) ? id : '';
    }

    private receive(status: KeystoreStatus | null) {
        if (status?.revision !== this.status?.revision) this.backupId = '';
        this.status = status;
        if (!this.backup) this.backupId = '';
    }

    // Failed reads do not leave a stale revision enabled for writes.
    refresh() {
        return this.view.perform(async () => {
            this.error = '';
            try {
                this.receive(await this.api.status());
            } catch {
                this.receive(null);
                this.error = 'Could not check the plugin settings keystore. Choose Check again.';
            }
        });
    }

    async retry() {
        if (!this.canRetry) return;
        const revision = this.status!.revision;
        let attempted = false;
        await this.view.perform(async () => {
            attempted = true;
            this.error = '';
            this.notice = '';
            this.view.invalidateRecovery();
            try {
                const status = await this.api.retry(revision);
                this.receive(status);
                if (status.state === 'ready')
                    this.notice = 'The plugin settings keystore is ready.';
            } catch (error) {
                this.receive(null);
                this.error = keystoreRecoveryAdvice(
                    error,
                    'Could not retry the keystore. Choose Check again before trying again.',
                );
            }
        });
        if (attempted) await this.view.refreshRecovery();
    }

    async change(operation: KeystoreOperation) {
        const allowed = { create: this.canCreate, reset: this.canReset, restore: this.canRestore };
        if (!allowed[operation]) return;
        const status = this.status!;
        const backup = operation === 'restore' ? this.backup! : null;
        let applied = false;
        await this.view.perform(async () => {
            this.error = '';
            this.notice = '';
            try {
                const review = await this.api.review(
                    status.revision,
                    operation,
                    backup?.id ?? null,
                );
                if (!review.review_token) throw Error('Missing keystore review');
                const backupName = backup
                    ? keystoreBackupLabel(backup, status.backups.indexOf(backup))
                    : '';
                const notice = {
                    create: {
                        action: this.creationInterrupted ? 'Resume creation' : 'Create keystore',
                        title: this.creationInterrupted
                            ? 'Resume plugin settings keystore creation?'
                            : 'Create the missing plugin settings keystore?',
                        message: this.creationInterrupted
                            ? 'Manager will contact the operating-system credential store to finish creating the keystore. Original files are preserved. The new datastore starts without saved plugin settings or private SDK data. Your existing configuration, profiles, and personal Vault are kept.'
                            : 'Manager will contact the operating-system credential store to create a fresh keystore. The new datastore starts without saved plugin settings or private SDK data. Your existing configuration, profiles, and personal Vault are kept.',
                        success:
                            'Keystore created. Reconfigure affected plugins before using them.',
                    },
                    reset: {
                        action: 'Reset keystore',
                        title: 'Reset the plugin settings keystore?',
                        message:
                            'This clears saved plugin settings and private SDK data from the active datastore. The current datastore and configuration database are preserved separately. Your profiles and personal Vault are not reset.',
                        success:
                            'Keystore reset. The previous datastore was preserved. Reconfigure affected plugins before using them.',
                    },
                    restore: {
                        action: 'Restore keystore',
                        title: 'Restore the plugin settings keystore?',
                        message: `Restore “${backupName}” for this installation? Saved plugin settings and private SDK data will return to the state in this backup. Existing files will be preserved separately, and the backup and your personal Vault will not be changed.`,
                        success: 'Keystore restored. Existing files were preserved separately.',
                    },
                }[operation];
                const accepted = await this.view.confirm(
                    notice.title,
                    notice.message,
                    notice.action,
                    review.warnings,
                );
                if (!accepted || status !== this.status || (backup && backup !== this.backup))
                    return;
                applied = true;
                this.view.invalidateRecovery();
                this.receive(await this.api.apply(status.revision, review.review_token));
                this.backupId = '';
                this.notice = this.status?.state === 'ready' ? notice.success : '';
            } catch (error) {
                this.receive(null);
                this.error = keystoreRecoveryAdvice(
                    error,
                    'Could not complete keystore recovery. Choose Check again and review the current state before retrying.',
                );
            }
        });
        if (applied) await this.view.refreshRecovery();
    }
}
