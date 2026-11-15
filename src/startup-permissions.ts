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
            return this.completed && this.ready && !this.busy && !this.opening && !this.error;
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
        // Derive permissions from the current startup permissions state
        get permissions() {
            return this.report?.permissions || [];
        },
        // Refresh startup permissions data while keeping the view synchronized
        async refresh(initial = false) {
            if (disposed) return;
            if (pending) return pending;
            this.busy = true;
            this.error = '';
            // Complete the deferred startup permissions work using the current operation state
            pending = (async () => {
                await Promise.resolve();
                try {
                    const report = await bridge.command('startup_permissions');
                    if (disposed) return;
                    this.report = report;
                    if (!report.ready || !report.device_connections_enabled) this.completed = false;
                    if (initial && report.ready) {
                        this.report = await bridge.command('complete_startup_permissions');
                        if (disposed) return;
                        if (this.report.ready && this.report.device_connections_enabled) {
                            this.completed = true;
                            this.visible = false;
                            return;
                        }
                    }
                    this.visible = this.reviewing || !this.completed || !report.ready;
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
            if (disposed || this.busy || this.opening || !this.ready) return;
            this.busy = true;
            this.error = '';
            try {
                this.report = await bridge.command('complete_startup_permissions');
                if (disposed) return;
                if (this.report.ready && this.report.device_connections_enabled) {
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
        // Derive status message from the current startup permissions state
        get statusMessage() {
            if (current().busy) return 'Checking permissions…';
            if (current().error)
                return 'Unable to verify permissions. Check again before continuing.';
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
            if (this.initialized && !document.hidden) void current().refresh();
        },
        // Initialize startup permissions state and its required lifecycle hooks
        init() {
            this.$watch('permissions.visible', () => this.syncDialog());
            // Refresh startup permissions state on the scheduled interval
            this.timer = window.setInterval(() => {
                if (this.initialized && current().visible && !document.hidden)
                    void current().refresh();
            }, 2000);
        },
        // Release startup permissions state and associated lifecycle resources
        destroy() {
            window.clearInterval(this.timer);
            current().dispose();
        },
    }));
}
