import { nextTab } from './tab-navigation';
import { openPluginPermissions } from './plugin-permissions';
import { cloneJson } from './json-clone';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { ownWindowMethods } from './window-work';
import type {
    PluginBackups,
    PluginBackup,
    PluginRollbackReview,
    PluginUpdatePolicy,
} from '../generated/runtime';
// Open the plugin management
export function openPluginManagement(id: string) {
    window.dispatchEvent(new CustomEvent('open-plugin-management', { detail: { id } }));
}
// Register the plugin management
export function registerPluginManagement(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('pluginManagement', () => ({
        opened: false,
        busy: false,
        preparing: false,
        writing: false,
        pendingClose: false,
        epoch: 0,
        reviewEpoch: 0,
        pluginId: '',
        openPermissions() {
            if (
                this.writing ||
                this.preparing ||
                (this.policyCloseValue() !== this.policyBaseline && this.policy)
            )
                return;
            const id = this.pluginId;
            this.close();
            openPluginPermissions(id);
        },
        tab: 'backups',
        page: 1,
        backups: null as PluginBackups | null,
        backupDeletion: null as { id: string | null; label: string } | null,
        report: null as PluginRollbackReview | null,
        policy: null as PluginUpdatePolicy | null,
        held: false,
        skipped: '',
        acknowledgeSettings: false,
        allowUnverified: false,
        error: '',
        reviewError: '',
        policyBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Serialize held and skipped update policies to detect unsaved changes
        policyCloseValue() {
            return JSON.stringify([this.held, this.skipped]);
        },
        // Derive name from the current plugin management state
        get name() {
            return (
                app().snapshot.plugins.find((p) => p.id === this.pluginId)?.name || this.pluginId
            );
        },
        // Derive can restore from the current plugin management state
        get canRestore() {
            return Boolean(
                this.report?.can_restore &&
                this.acknowledgeSettings &&
                (this.report.package.backup.trusted_origin || this.allowUnverified) &&
                !this.preparing &&
                !this.writing &&
                !this.reviewError,
            );
        },
        // Derive can save policy from the current plugin management state
        get canSavePolicy() {
            return Boolean(this.policy?.managed && !this.busy && !this.writing);
        },
        // Derive pages from the current plugin management state
        get pages() {
            return Math.max(1, Math.ceil((this.backups?.total || 0) / 12));
        },
        // Derive backup deletion title from the current plugin management state
        get backupDeletionTitle() {
            return this.backupDeletion?.id
                ? `Delete backup ${this.backupDeletion.label}?`
                : 'Delete all backups?';
        },
        // Derive backup deletion description from the current plugin management state
        get backupDeletionDescription() {
            const scope = this.backupDeletion?.id
                ? 'the selected backup'
                : 'all retained package backups';
            return `This permanently removes ${scope} for ${this.name}. The installed plugin and its settings are kept.`;
        },
        // Derive plugin present from the current plugin management state
        get pluginPresent() {
            return !this.opened || app().snapshot.plugins.some((p) => p.id === this.pluginId);
        },
        // Initialize plugin management state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'plugin management', [
                'open',
                'load',
                'reviewBackup',
                'confirmBackupDeletion',
                'restore',
                'savePolicy',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                ['reviewBackup', 'confirmBackupDeletion', 'restore', 'savePolicy'],
                {
                    name: 'Plugin version and update preferences',
                    retainOnUnmount: true,
                    // Return a snapshot of the current plugin management state
                    state: () =>
                        this.opened &&
                        ((this.policy && this.policyCloseValue() !== this.policyBaseline) ||
                            (this.report && (this.acknowledgeSettings || this.allowUnverified)))
                            ? 'dirty'
                            : 'clean',
                    // Discard pending plugin management edits without saving them
                    discard: () => this.close(),
                    // Release resources retained by the plugin management operation
                    release: () => this.close(),
                },
            );
            this.$watch('$store.app.route', () => this.close());
            this.$watch('pluginPresent', () => {
                if (!this.pluginPresent) this.close();
            });
        },
        // Release resources retained by the plugin management operation
        async release(id: string) {
            await trackCloseRelease(`plugin-rollback:${id}`, () =>
                app().bridge.command('cancel_plugin_rollback', { id }),
            );
        },
        // Release plugin management state and associated lifecycle resources
        close() {
            if (this.writing) {
                this.pendingClose = true;
                return;
            }
            ++this.epoch;
            this.opened = false;
            this.busy = false;
            this.pendingClose = false;
            this.cancelReview();
            this.cancelBackupDeletion();
            this.policy = null;
            this.backups = null;
            (this.$refs.dialog as HTMLDialogElement)?.close();
        },
        // Release plugin management state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.close();
        },
        // Open the current plugin management target
        async open(event: CustomEvent<{ id: string }>) {
            if (this.writing) return;
            this.close();
            this.pluginId = event.detail.id;
            if (!app().snapshot.plugins.some((p) => p.id === this.pluginId)) return;
            this.opened = true;
            this.tab = 'backups';
            this.page = 1;
            (this.$refs.dialog as HTMLDialogElement).showModal();
            await this.load();
        },
        // Load the data required by the plugin management workflow
        async load() {
            if (!this.opened || this.writing) return;
            const epoch = ++this.epoch;
            this.cancelReview();
            this.busy = true;
            this.error = '';
            const results = await Promise.allSettled([
                app().bridge.command('plugin_backups', {
                    id: this.pluginId,
                    page: this.page,
                }),
                app().bridge.command('plugin_update_policy', {
                    id: this.pluginId,
                }),
            ]);
            if (epoch !== this.epoch || !this.opened) return;
            const [backups, policy] = results;
            if (backups.status === 'fulfilled') {
                this.backups = backups.value;
                this.page = backups.value.page;
            } else {
                this.backups = null;
                this.error = String(backups.reason);
            }
            if (policy.status === 'fulfilled') {
                this.policy = policy.value;
                this.held = policy.value.held;
                this.skipped = policy.value.skipped_versions.join('\n');
                this.policyBaseline = this.policyCloseValue();
            } else {
                this.policy = null;
                this.error = [this.error, String(policy.reason)].filter(Boolean).join('\n');
            }
            this.busy = false;
        },
        versionTabKey(event: KeyboardEvent) {
            if (this.writing) return;
            const next = nextTab(['backups', 'policy'], this.tab, event.key);
            if (!next) return;
            event.preventDefault();
            this.tab = next;
            document.getElementById('version-tab-' + next)?.focus();
        },
        // Show the backups
        showBackups() {
            this.tab = 'backups';
        },
        // Show the policy
        showPolicy() {
            this.tab = 'policy';
        },
        // Select the previous plugin management item
        previous() {
            this.page = Math.max(1, this.page - 1);
            void this.load();
        },
        // Select the next plugin management item
        next() {
            this.page = Math.min(this.pages, this.page + 1);
            void this.load();
        },
        // Format the plugin management timestamp for display
        date(backup: PluginBackup) {
            return backup.created
                ? new Date(backup.created * 1000).toLocaleString()
                : 'Not recorded';
        },
        // Distinguish unavailable backups from recorded and unverified provenance
        trust(backup: PluginBackup) {
            return backup.error
                ? 'Unavailable'
                : backup.trusted_origin
                  ? 'Recorded catalog origin'
                  : backup.recorded
                    ? 'Recorded; unverified origin'
                    : 'Unverified provenance';
        },
        // Check whether review backup is valid for the current plugin management state
        canReviewBackup(backup: PluginBackup) {
            return !this.busy && !this.preparing && !this.writing && !backup.error;
        },
        // Request the backup deletion
        requestBackupDeletion(event: Event) {
            if (this.busy || this.preparing || this.writing || !this.backups?.total) return;
            const trigger = event.currentTarget as HTMLButtonElement;
            const id = trigger.dataset.backup;
            const entry = this.backups.entries.find((backup) => backup.id === id);
            if (id && !entry) return;
            this.backupDeletion = { id: entry?.id ?? null, label: entry?.version || id || '' };
            trigger.focus({ preventScroll: true });
            (this.$refs.backupConfirmation as HTMLDialogElement).showModal();
        },
        // Cancel the backup deletion
        cancelBackupDeletion() {
            if (this.writing) return;
            (this.$refs.backupConfirmation as HTMLDialogElement)?.close();
            this.backupDeletion = null;
        },
        // Confirm the backup deletion
        async confirmBackupDeletion() {
            if (!this.backupDeletion || this.writing) return;
            const backup = this.backupDeletion.id;
            const id = this.pluginId;
            this.writing = true;
            this.error = '';
            try {
                const removed = await app().bridge.command('delete_plugin_backups', {
                    id,
                    backup,
                    all: backup === null,
                });
                app().notify(
                    removed === 1 ? 'Deleted one backup.' : `Deleted ${removed} backups.`,
                    'success',
                );
            } catch (error) {
                this.error = String(error);
            }
            // Refresh even on failure: some files may already have been deleted
            try {
                this.backups = await app().bridge.command('plugin_backups', {
                    id,
                    page: this.page,
                });
                this.page = this.backups.page;
            } catch (error) {
                this.backups = null;
                this.error = [this.error, String(error)].filter(Boolean).join('\n');
            } finally {
                this.writing = false;
                this.cancelBackupDeletion();
                if (this.pendingClose) this.close();
                else this.$nextTick(() => (this.$refs.refresh as HTMLButtonElement).focus());
            }
        },
        // Prepare a backup review only while its plugin and review generations remain current
        async reviewBackup(event: Event) {
            if (this.busy || this.preparing || this.writing) return;
            const id = (event.currentTarget as HTMLElement).dataset.backup;
            if (!id) return;
            this.cancelReview();
            const epoch = this.epoch;
            const reviewEpoch = this.reviewEpoch;
            this.preparing = true;
            this.reviewError = '';
            this.acknowledgeSettings = false;
            this.allowUnverified = false;
            (this.$refs.review as HTMLDialogElement).showModal();
            try {
                const report = await app().bridge.command('prepare_plugin_rollback', {
                    id: this.pluginId,
                    backup: id,
                });
                if (epoch !== this.epoch || reviewEpoch !== this.reviewEpoch || !this.opened) {
                    void this.release(report.id).catch(() => {});
                    return;
                }
                this.report = report;
                this.$nextTick(() => (this.$refs.cancelRollback as HTMLButtonElement).focus());
            } catch (error) {
                if (epoch === this.epoch && reviewEpoch === this.reviewEpoch)
                    this.reviewError = String(error);
            } finally {
                if (epoch === this.epoch && reviewEpoch === this.reviewEpoch)
                    this.preparing = false;
            }
        },
        // Cancel the review
        cancelReview() {
            if (this.writing) return;
            ++this.reviewEpoch;
            this.preparing = false;
            if (this.report) void this.release(this.report.id).catch(() => {});
            this.report = null;
            this.reviewError = '';
            (this.$refs.review as HTMLDialogElement)?.close();
        },
        // Restore the retained plugin management state
        async restore() {
            if (!this.canRestore || !this.report) return;
            this.writing = true;
            this.reviewError = '';
            try {
                const snapshot = await app().bridge.command('commit_plugin_rollback', {
                    id: this.report.id,
                    acknowledgeSettings: this.acknowledgeSettings,
                    allowUnverified: this.allowUnverified,
                });
                this.report = null;
                app().apply(snapshot);
                this.writing = false;
                this.cancelReview();
                app().notify('Plugin restored and left disabled.', 'success');
                await this.load();
                this.$nextTick(() => (this.$refs.refresh as HTMLButtonElement).focus());
            } catch (error) {
                this.reviewError = String(error);
            } finally {
                this.writing = false;
                if (this.pendingClose) this.close();
            }
        },
        // Save the policy
        async savePolicy() {
            if (!this.canSavePolicy || !this.policy) return;
            this.writing = true;
            this.error = '';
            try {
                const value = {
                    ...cloneJson(this.policy),
                    held: this.held,
                    skipped_versions: this.skipped
                        .split(/[\n,]+/)
                        .map((v) => v.trim())
                        .filter(Boolean),
                };
                this.policy = await app().bridge.command('save_plugin_update_policy', { value });
                this.held = this.policy.held;
                this.skipped = this.policy.skipped_versions.join('\n');
                this.policyBaseline = this.policyCloseValue();
                app().notify('Plugin update preferences saved.', 'success');
                void (
                    Alpine.store('updates') as { refresh(): Promise<void> } | undefined
                )?.refresh();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.writing = false;
                if (this.pendingClose) this.close();
            }
        },
    }));
}
