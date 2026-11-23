import type AlpineType from '@alpinejs/csp';
import type { Bridge } from './bridge';
import type { StartupPermissionReport } from '../generated/runtime';

/** Completed once per document, before the first-run setup can open another modal. */
let settleStartup!: () => void;
export const startupPermissionsReady = new Promise<void>((resolve) => {
    settleStartup = resolve;
});

// Create the startup permissions
export function createStartupPermissions(bridge: Pick<Bridge, 'command'>) {
    let pending: Promise<void> | undefined;
    let disposed = false;
    return {
        report: null as StartupPermissionReport | null,
        busy: false,
        opening: '',
        error: '',
        visible: false,
        completed: false,
        reviewing: false,
        // Derive can close from the current startup permissions state
        get canClose() {
            return this.completed && this.admitted && !this.busy && !this.opening && !this.error;
        },
        // Review the pending startup permissions changes
        async review() {
            if (disposed) return;
            this.reviewing = true;
            this.visible = true;
            await this.refresh();
        },
        // Close the review
        closeReview() {
            if (!this.canClose) return;
            this.reviewing = false;
            this.visible = false;
        },
        // Derive is mac os from the current startup permissions state
        get isMacOS() {
            return this.report?.platform === 'macos';
        },
        // Derive ready from the current startup permissions state
        get ready() {
            return this.report?.ready === true;
        },
        // The native report is authoritative; skips never change real OS status.
        get canSkip() {
            return this.report?.development_mode === true;
        },
        get skippedPermissions() {
            return this.report?.skipped_permissions || [];
        },
        get developmentWarning() {
            if (!this.canSkip || this.ready || !this.skippedPermissions.length) return '';
            const inputMissing =
                this.skippedPermissions.includes('input_monitoring') &&
                this.permissions.some(
                    (permission) =>
                        permission.id === 'input_monitoring' &&
                        permission.status !== 'granted' &&
                        permission.status !== 'not_required',
                );
            if (this.report?.device_connections_enabled)
                return inputMissing
                    ? 'Input Monitoring is not enabled. Device connections are allowed for this development session, but device input may not work.'
                    : 'Permissions were skipped for this development session. Features that require them may not work.';
            return inputMissing
                ? 'Input Monitoring is not enabled; device input may not work. Device connections are still paused by remaining requirements.'
                : 'Skipped for this development session. Device connections are paused by remaining requirements.';
        },
        get canContinue() {
            return this.ready || (this.canSkip && this.report?.setup_ready === true);
        },
        get canRequestContinue() {
            return (
                this.canContinue ||
                (this.canSkip &&
                    this.permissions.some(
                        (permission) =>
                            permission.required &&
                            permission.status !== 'granted' &&
                            permission.status !== 'not_required',
                    ))
            );
        },
        get admitted() {
            return (
                this.canContinue &&
                (this.report?.device_connections_enabled === true ||
                    (!this.ready && this.canSkip && this.report?.setup_ready === true))
            );
        },
        // Derive permissions from the current startup permissions state
        get permissions() {
            return this.report?.permissions || [];
        },
        // Refresh startup permissions data while keeping the view synchronized
        async refresh(initial = false) {
            if (disposed) return;
            if (pending) return pending;
            if (this.busy) return;
            this.busy = true;
            this.error = '';
            // Complete the deferred startup permissions work using the current operation state
            pending = (async () => {
                await Promise.resolve();
                try {
                    const report = await bridge.command('startup_permissions');
                    if (disposed) return;
                    this.report = report;
                    if (!this.admitted) this.completed = false;
                    if (initial && this.canContinue) {
                        this.report = await bridge.command('complete_startup_permissions');
                        if (disposed) return;
                        if (this.admitted) {
                            this.completed = true;
                            this.visible = false;
                            return;
                        }
                    }
                    this.visible = this.reviewing || !this.completed || !this.canContinue;
                } catch (error) {
                    if (!disposed) {
                        this.error = String(error);
                        this.completed = false;
                        this.visible = true;
                    }
                } finally {
                    this.busy = false;
                    pending = undefined;
                }
            })();
            return pending;
        },
        // Complete permission setup and keep the prompt visible if requirements remain unmet
        async continueSetup() {
            if (disposed || this.busy || this.opening || !this.canRequestContinue) return;
            this.busy = true;
            this.error = '';
            try {
                // This explicit development action acknowledges remaining requirements without
                // granting OS permission. Native code validates every ID and remains authoritative.
                if (!this.canContinue && this.canSkip) {
                    const remaining = this.permissions
                        .filter(
                            (permission) =>
                                permission.required &&
                                permission.status !== 'granted' &&
                                permission.status !== 'not_required' &&
                                !this.skippedPermissions.includes(permission.id),
                        )
                        .map((permission) => permission.id);
                    for (const id of remaining) {
                        if (disposed) return;
                        if (this.canContinue) break;
                        if (!this.canSkip)
                            throw Error('Development permission bypass is no longer available.');
                        const current = this.permissions.find((permission) => permission.id === id);
                        if (
                            !current?.required ||
                            current.status === 'granted' ||
                            current.status === 'not_required' ||
                            this.skippedPermissions.includes(id)
                        )
                            continue;
                        this.report = await bridge.command('complete_startup_permissions', {
                            skipPermission: id,
                        });
                        if (disposed) return;
                    }
                }
                if (!this.canContinue) {
                    this.error = 'Permission requirements changed. Check again before continuing.';
                    this.visible = true;
                    return;
                }
                this.report = await bridge.command('complete_startup_permissions');
                if (disposed) return;
                if (this.admitted) {
                    this.completed = true;
                    this.reviewing = false;
                    this.visible = false;
                } else {
                    this.visible = true;
                    this.error =
                        'A required permission is still unavailable. Check the settings and try again.';
                }
            } catch (error) {
                if (!disposed) this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Skip one startup requirement for this native development process only.
        // Completion stays explicit; only the returned native report determines device admission.
        async skipPermission(id: string) {
            if (disposed || !this.canSkip || this.busy || this.opening) return;
            const permission = this.permissions.find((permission) => permission.id === id);
            if (
                !permission ||
                !permission.required ||
                permission.status === 'granted' ||
                permission.status === 'not_required' ||
                this.skippedPermissions.includes(id)
            )
                return;
            this.busy = true;
            this.error = '';
            try {
                const report = await bridge.command('complete_startup_permissions', {
                    skipPermission: id,
                });
                if (!disposed) this.report = report;
            } catch (error) {
                if (!disposed) this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Open the settings
        async openSettings(id: string) {
            if (disposed || this.opening) return;
            this.opening = id;
            this.error = '';
            try {
                await bridge.command('open_startup_permission_settings', { id });
            } catch (error) {
                if (!disposed) this.error = String(error);
            } finally {
                this.opening = '';
            }
        },
        // Release startup permissions state and associated lifecycle resources
        dispose() {
            disposed = true;
        },
    };
}

// Register the startup permissions
export function registerStartupPermissions(Alpine: typeof AlpineType, bridge: Bridge) {
    Alpine.store('startupPermissions', createStartupPermissions(bridge));
    // Return the currently selected startup permissions value
    const current = () =>
        Alpine.store('startupPermissions') as ReturnType<typeof createStartupPermissions>;
    Alpine.data('startupPermissionDialog', () => ({
        // Derive permissions from the current startup permissions state
        get permissions() {
            return current();
        },
        timer: 0,
        initialized: false,
        // Close the review
        closeReview() {
            current().closeReview();
        },
        // Refresh the active permission report
        recheck() {
            return current().refresh();
        },
        // Delegate permission completion to the active setup controller
        continueSetup() {
            return current().continueSetup();
        },
        // Open the settings
        openSettings(event: Event) {
            return current().openSettings(
                (event.currentTarget as HTMLElement).dataset.permission || '',
            );
        },
        skipPermission(event: Event) {
            return current().skipPermission(
                (event.currentTarget as HTMLElement).dataset.permission || '',
            );
        },
        // Derive status message from the current startup permissions state
        get statusMessage() {
            if (current().busy) return 'Checking permissions…';
            if (current().error)
                return 'Unable to verify permissions. Check again before continuing.';
            if (current().developmentWarning) return current().developmentWarning;
            if (!current().ready && current().canSkip)
                return 'Continue in development mode skips remaining requirements for this session. Device input and permission-dependent features may not work.';
            if (current().completed && current().ready) {
                return current().permissions.length
                    ? 'Required permissions are enabled.'
                    : 'No startup permissions are required on this platform.';
            }
            return current().ready
                ? 'Required permissions are enabled. You can connect your devices.'
                : 'Device connections are paused until required permissions are enabled.';
        },
        // Sync the dialog
        syncDialog() {
            this.syncTimer();
            if (!this.initialized) return;
            const dialog = this.$refs.dialog as HTMLDialogElement;
            if (current().visible && !dialog.open) dialog.showModal();
            else if (!current().visible && dialog.open) dialog.close();
            if (current().completed && !current().visible) settleStartup();
        },
        // Begin the startup permissions operation
        async begin() {
            if (this.initialized) return;
            this.initialized = true;
            await current().refresh(true);
            this.syncDialog();
        },
        // Refresh the visible
        refreshVisible() {
            this.syncTimer();
            if (this.initialized && !document.hidden) void current().refresh();
        },
        // Initialize startup permissions state and its required lifecycle hooks
        init() {
            this.$watch('permissions.visible', () => this.syncDialog());
            this.syncTimer();
        },
        syncTimer() {
            const active = this.initialized && current().visible && !document.hidden;
            if (active && !this.timer)
                this.timer = window.setInterval(() => void current().refresh(), 2000);
            if (!active && this.timer) {
                window.clearInterval(this.timer);
                this.timer = 0;
            }
        },
        // Release startup permissions state and associated lifecycle resources
        destroy() {
            window.clearInterval(this.timer);
            current().dispose();
        },
    }));
}
