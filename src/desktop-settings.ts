import { listen } from '@tauri-apps/api/event';
import type { AppState } from './state';
import type { DesktopSettings } from '../generated/runtime';

// Create desktop preference controls with separate native capability state
export function createDesktopSettings(app: () => AppState, native: boolean) {
    return {
        notifications: false,
        launchToTray: false,
        managerShortcut: false,
        pluginLinksEnabled: false,
        pluginLinksSavedEnabled: false,
        pluginLinksSupported: false,
        pluginLinksBusy: false,
        pluginLinksReason: '',
        pluginLinksError: '',
        openactionLinksEnabled: false,
        openactionLinksSavedEnabled: false,
        openactionLinksSupported: false,
        openactionLinksBusy: false,
        openactionLinksReason: '',
        openactionLinksError: '',

        shortcutSaving: false,
        shortcutStatus: '',
        shortcutListener: null as Promise<() => void> | null,
        autostart: false,
        autostartSupported: false,
        desktopLoading: true,
        desktopSaving: false,
        autostartSaving: false,
        desktopError: '',
        startupError: '',
        desktopEpoch: 0,
        savedDesktop: null as DesktopSettings | null,
        savedAutostart: false,
        // Mark startup settings as simulated in browser preview
        get startupPreview() {
            return !native;
        },
        // Load desktop preferences and independent platform integration status
        async loadDesktop() {
            const epoch = ++this.desktopEpoch;
            if (native) {
                this.shortcutListener ??= listen<string>(
                    'manager-shortcut-status',
                    // Display native shortcut registration status updates
                    ({ payload }) => {
                        this.shortcutStatus = payload;
                    },
                );
                void this.shortcutListener
                    // Read the shortcut status after its listener is attached
                    .then(async () => {
                        const status = await app().bridge.command('manager_shortcut_status');
                        if (epoch === this.desktopEpoch) this.shortcutStatus = status;
                    })
                    // Leave shortcut status unchanged if listener setup fails
                    .catch(() => {});
            }
            this.desktopLoading = true;
            this.desktopError = '';
            this.startupError = '';
            // Read independently: unavailable login registration must not hide the
            // saved window/notification preferences or prevent their editing
            await Promise.all([
                app()
                    .bridge.command('desktop_settings')
                    // Apply desktop settings only while the request epoch is current
                    .then((settings) => {
                        if (epoch !== this.desktopEpoch) return;
                        this.savedDesktop = settings;
                        this.notifications = settings.notifications;
                        this.launchToTray = settings.launch_to_tray || false;
                        this.managerShortcut = settings.show_manager_shortcut || false;
                    })
                    // Report desktop settings load failures for the active request
                    .catch((error) => {
                        if (epoch === this.desktopEpoch) this.desktopError = String(error);
                    }),
                app()
                    .bridge.command('plugin_link_status', { openaction: false })
                    // Apply Manager plugin-link status for the current request
                    .then((status) => {
                        if (epoch !== this.desktopEpoch) return;
                        this.pluginLinksEnabled = this.pluginLinksSavedEnabled = status.enabled;
                        this.pluginLinksSupported = status.supported;
                        this.pluginLinksReason = status.reason || '';
                    })
                    // Report Manager plugin-link status failures for the current request
                    .catch((error) => {
                        if (epoch === this.desktopEpoch) this.pluginLinksError = String(error);
                    }),
                app()
                    .bridge.command('plugin_link_status', { openaction: true })
                    // Apply OpenAction link status for the current request
                    .then((status) => {
                        if (epoch !== this.desktopEpoch) return;
                        this.openactionLinksEnabled = this.openactionLinksSavedEnabled =
                            status.enabled;
                        this.openactionLinksSupported = status.supported;
                        this.openactionLinksReason = status.reason || '';
                    })
                    // Report OpenAction link status failures for the current request
                    .catch((error) => {
                        if (epoch === this.desktopEpoch) this.openactionLinksError = String(error);
                    }),
                app()
                    .bridge.command('autostart_status')
                    // Apply autostart availability and state for the current request
                    .then((status) => {
                        if (epoch !== this.desktopEpoch) return;
                        this.autostartSupported = status.supported;
                        this.savedAutostart = this.autostart = status.enabled;
                    })
                    // Report autostart status failures independently of desktop preferences
                    .catch((error) => {
                        if (epoch === this.desktopEpoch) this.startupError = String(error);
                    }),
            ]);
            if (epoch === this.desktopEpoch) this.desktopLoading = false;
        },
        // Release the native shortcut-status listener
        releaseShortcutListener() {
            void this.shortcutListener?.then(
                (unlisten) => unlisten(),
                // Ignore a failed listener setup during cleanup
                () => {},
            );
            this.shortcutListener = null;
        },
        // Update the Manager shortcut and restore saved state on failure
        async setManagerShortcut() {
            if (this.shortcutSaving || !this.savedDesktop) return;
            this.shortcutSaving = true;
            const enabled = this.managerShortcut;
            try {
                this.shortcutStatus = await app().bridge.command('set_manager_shortcut', {
                    enabled,
                });
                this.savedDesktop.show_manager_shortcut = enabled;
            } catch (error) {
                this.managerShortcut = this.savedDesktop.show_manager_shortcut || false;
                this.shortcutStatus = String(error);
            } finally {
                this.shortcutSaving = false;
            }
        },
        // Update Manager plugin-link registration for the current settings session
        async setPluginLinkHandling() {
            if (this.pluginLinksBusy || !this.pluginLinksSupported || this.desktopLoading) return;
            const epoch = this.desktopEpoch;
            this.pluginLinksBusy = true;
            this.pluginLinksError = '';
            try {
                const status = await app().bridge.command('set_plugin_link_handling', {
                    enabled: this.pluginLinksEnabled,
                    openaction: false,
                });
                if (epoch !== this.desktopEpoch) return;
                this.pluginLinksEnabled = this.pluginLinksSavedEnabled = status.enabled;
                this.pluginLinksSupported = status.supported;
                this.pluginLinksReason = status.reason || '';
                if (this.savedDesktop) this.savedDesktop.plugin_links = status.enabled;
            } catch (error) {
                if (epoch === this.desktopEpoch) {
                    this.pluginLinksEnabled = this.pluginLinksSavedEnabled;
                    this.pluginLinksError = String(error);
                }
            } finally {
                if (epoch === this.desktopEpoch) this.pluginLinksBusy = false;
            }
        },
        // Update OpenAction link registration for the current settings session
        async setOpenactionLinkHandling() {
            if (this.openactionLinksBusy || !this.openactionLinksSupported || this.desktopLoading)
                return;
            const epoch = this.desktopEpoch;
            this.openactionLinksBusy = true;
            this.openactionLinksError = '';
            try {
                const status = await app().bridge.command('set_plugin_link_handling', {
                    enabled: this.openactionLinksEnabled,
                    openaction: true,
                });
                if (epoch !== this.desktopEpoch) return;
                this.openactionLinksEnabled = this.openactionLinksSavedEnabled = status.enabled;
                this.openactionLinksSupported = status.supported;
                this.openactionLinksReason = status.reason || '';
                if (this.savedDesktop) this.savedDesktop.openaction_links = status.enabled;
            } catch (error) {
                if (epoch === this.desktopEpoch) {
                    this.openactionLinksEnabled = this.openactionLinksSavedEnabled;
                    this.openactionLinksError = String(error);
                }
            } finally {
                if (epoch === this.desktopEpoch) this.openactionLinksBusy = false;
            }
        },
        // Save desktop preferences while guarding stale responses
        async saveDesktop() {
            if (this.desktopLoading || this.desktopSaving || !this.savedDesktop) return;
            const epoch = this.desktopEpoch;
            const before = this.savedDesktop;
            const candidate: DesktopSettings = {
                notifications: this.notifications,
                launch_to_tray: this.launchToTray,
                show_manager_shortcut: before.show_manager_shortcut,
                plugin_links: before.plugin_links,
                openaction_links: before.openaction_links,
            };
            this.desktopSaving = true;
            this.desktopError = '';
            try {
                await app().bridge.command('save_desktop_settings', { settings: candidate });
                if (epoch === this.desktopEpoch) this.savedDesktop = candidate;
            } catch (error) {
                if (epoch === this.desktopEpoch) {
                    this.desktopError = String(error);
                    this.notifications = before.notifications;
                    this.launchToTray = before.launch_to_tray || false;
                }
            } finally {
                if (epoch === this.desktopEpoch) this.desktopSaving = false;
            }
        },
        // Save notification preferences and reconcile the returned state
        async setNotifications() {
            if (this.desktopLoading || this.desktopSaving || !this.savedDesktop) return;
            const epoch = this.desktopEpoch;
            // Permission UI also owns the preference lane; another toggle must
            // not persist the pending notification value before permission returns
            this.desktopSaving = true;
            try {
                if (native && this.notifications) {
                    const { isPermissionGranted, requestPermission } =
                        await import('@tauri-apps/plugin-notification');
                    if (!(await isPermissionGranted()) && (await requestPermission()) !== 'granted')
                        throw Error('Notification permission was denied.');
                }
            } catch (error) {
                if (epoch === this.desktopEpoch) {
                    this.notifications = this.savedDesktop.notifications;
                    this.desktopError = String(error);
                }
                return;
            } finally {
                if (epoch === this.desktopEpoch) this.desktopSaving = false;
            }
            if (epoch === this.desktopEpoch) await this.saveDesktop();
        },
        // Update native autostart registration and restore saved state on failure
        async setAutostart() {
            if (this.desktopLoading || this.autostartSaving || !this.autostartSupported) return;
            const epoch = this.desktopEpoch;
            const enabled = this.autostart;
            this.autostartSaving = true;
            this.startupError = '';
            try {
                await app().bridge.command('set_autostart', { enabled });
                if (epoch === this.desktopEpoch) this.savedAutostart = enabled;
            } catch (error) {
                if (epoch === this.desktopEpoch) {
                    this.autostart = this.savedAutostart;
                    this.startupError = String(error);
                }
            } finally {
                if (epoch === this.desktopEpoch) this.autostartSaving = false;
            }
        },
        // Invalidate outstanding desktop settings requests on closure
        closeDesktop() {
            ++this.desktopEpoch;
        },
    };
}
