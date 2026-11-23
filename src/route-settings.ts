import Alpine from '@alpinejs/csp';
import { createCacheSettings } from './cache-settings';
import { createDesktopSettings } from './desktop-settings';
import { native } from './bridge';
import { permissionsController } from './plugin-permissions';
import { pageSections } from './tab-navigation';
import type { registerTheme } from './theme';
import { ownWindowMethods } from './window-work';

import { registerPaletteEditor } from './palette-editor';
import { registerParameters } from './parameters-ui';
import { registerProfilePalettes } from './profile-palettes';
import { app, swallow } from './route-context';
type CacheSettingsView = ReturnType<typeof createCacheSettings> & {
    $refs: Record<string, HTMLElement>;
    $nextTick(callback: () => void): void;
    cancelClear(): void;
};
// Register the settings handler with its owner
export function register() {
    Alpine.data('pluginPermissions', () => permissionsController(() => app().snapshot.plugins));
    Alpine.data('cacheSettings', () =>
        Object.assign(createCacheSettings(app), {
            // Initialize settings state and its required lifecycle hooks
            init(this: CacheSettingsView) {
                ownWindowMethods(this, 'download cache', ['refresh', 'clear']);
                void this.refresh();
            },
            // Request the clear
            requestClear(this: CacheSettingsView) {
                if (this.busy || this.confirming) return;
                this.confirming = true;
                (this.$refs.cacheConfirm as HTMLDialogElement).showModal();
            },
            // Cancel the clear
            cancelClear(this: CacheSettingsView) {
                if (this.clearing) return;
                this.confirming = false;
                (this.$refs.cacheConfirm as HTMLDialogElement).close();
                this.$nextTick(() => (this.$refs.deleteCache as HTMLButtonElement).focus());
            },
            // Confirm the clear
            async confirmClear(this: CacheSettingsView) {
                await this.clear();
                this.cancelClear();
            },
        }),
    );
    registerParameters(Alpine, app);
    registerProfilePalettes(Alpine, app);
    registerPaletteEditor(Alpine, app);
    const theme = () => Alpine.store('theme') as ReturnType<typeof registerTheme>;
    Alpine.data('paletteSummary', () => ({
        get selectedPalettes() {
            return theme().paletteGroups.map(({ name, mode, options }) => ({
                label: mode === 'light' ? 'Light' : 'Dark',
                mode,
                selected: options.find((option) => option.id === theme().style[name]) ?? options[0],
                active: theme().isActivePalette(mode, theme().style[name]),
            }));
        },
        openPaletteBrowser(event: Event) {
            window.dispatchEvent(
                new CustomEvent('open-palette-browser', { detail: event.currentTarget }),
            );
        },
    }));
    Alpine.data('settings', () => ({
        ...pageSections('settings', [
            { id: 'appearance', label: 'Appearance' },
            { id: 'profiles', label: 'Profiles' },
            { id: 'devices', label: 'Devices & startup' },
            { id: 'plugins', label: 'Plugins' },
            { id: 'permissions', label: 'Permissions' },
            { id: 'storage', label: 'Storage & updates' },
        ]),
        // Choose the background
        chooseBackground() {
            window.dispatchEvent(
                new CustomEvent('open-background-editor', { detail: { choose: true } }),
            );
        },
        // Edit the background
        editBackground() {
            window.dispatchEvent(new CustomEvent('open-background-editor'));
        },
        // Remove the background
        removeBackground() {
            swallow(app().clearBackground());
        },
        ...createDesktopSettings(app, native),
        // Derive startup preview from the current settings state
        get startupPreview() {
            return !native;
        },
        // Initialize settings state and its required lifecycle hooks
        async init() {
            ownWindowMethods(this, 'desktop preferences', [
                'loadDesktop',
                'saveDesktop',
                'setNotifications',
                'setAutostart',
                'setManagerShortcut',
                'setPluginLinkHandling',
            ]);
            void theme().loadFonts();
            await this.loadDesktop();
        },
        // Release settings state and associated lifecycle resources
        destroy() {
            this.releaseShortcutListener();
            this.closeDesktop();
        },
        // Request Wayland keyboard authorization and report a successful grant
        authorizeWayland() {
            swallow(
                app()
                    .command('authorize_wayland')
                    // Process the settled settings operation result
                    .then(() => {
                        app().notify('Wayland keyboard permission granted', 'success');
                    }),
            );
        },
        // Derive app from the current settings state
        get app() {
            return app();
        },
        // Export the profiles
        exportProfiles() {
            window.dispatchEvent(new CustomEvent('open-profile-export'));
        },
        // Import the profiles
        importProfiles() {
            window.dispatchEvent(new CustomEvent('open-profile-import'));
        },
        // Cancel the active settings operation
        cancel() {
            swallow(
                app()
                    .command('cancel_macros')
                    .then(() => app().notify('Pending macros cancelled', 'success')),
            );
        },
    }));
}
