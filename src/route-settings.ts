import { nextTab, tabNavigationKey } from './tab-navigation';
import Alpine from '@alpinejs/csp';
import { createCacheSettings } from './cache-settings';
import { createDesktopSettings } from './desktop-settings';
import { native } from './bridge';
import { jumpToSection } from './visibility';
import type { registerTheme } from './theme';
import { paletteColors, type ThemeMode } from './theme-style';
import { ownWindowMethods } from './window-work';
import { windowCloseOwners } from './window-close-owners';

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
    // Resolve the current settings appearance
    const theme = () => Alpine.store('theme') as ReturnType<typeof registerTheme>;
    Alpine.data('paletteBrowser', () => ({
        paletteQuery: '',
        paletteTab: 'light' as ThemeMode,
        pendingPalette: null as { mode: ThemeMode; id: string } | null,
        previewOwner: Symbol('Palette browser'),
        unregisterClose: undefined as (() => void) | undefined,
        // Initialize settings state and its required lifecycle hooks
        init() {
            this.unregisterClose = windowCloseOwners.register({
                name: 'Palette browser',
                // Return a snapshot of the current settings state
                state: () => 'clean',
                // Discard pending settings edits without saving them
                discard: () => this.closePaletteBrowser(),
                // Release resources retained by the settings operation
                release: () => this.closePaletteBrowser(),
            });
        },
        // Release settings state and associated lifecycle resources
        destroy() {
            this.unregisterClose?.();
            theme().clearPalettePreview(this.previewOwner);
        },
        // Derive selected palettes from the current settings state
        get selectedPalettes() {
            return theme().paletteGroups.map(({ name, mode, options }) => ({
                label: mode === 'light' ? 'Light' : 'Dark',
                mode,
                selected: options.find((option) => option.id === theme().style[name]) ?? options[0],
                active: theme().isActivePalette(mode, theme().style[name]),
            }));
        },
        // Derive matching palettes from the current settings state
        get matchingPalettes() {
            const query = this.paletteQuery.trim().toLowerCase();
            return theme()
                .paletteGroups.find((group) => group.mode === this.paletteTab)!
                .options.filter((option) => option.label.toLowerCase().includes(query));
        },
        // Derive saved palette id from the current settings state
        get savedPaletteId() {
            return this.paletteTab === 'light'
                ? theme().style.lightPalette
                : theme().style.darkPalette;
        },
        // Derive preview palette from the current settings state
        get previewPalette() {
            const mode = this.pendingPalette?.mode ?? this.paletteTab;
            const group = theme().paletteGroups.find((group) => group.mode === mode)!;
            const id = this.pendingPalette?.id ?? theme().style[group.name];
            const option = group.options.find((option) => option.id === id) ?? group.options[0];
            const colors = {
                ...paletteColors(mode, option.id),
                ...theme().customPalettes.find(
                    (palette) => palette.id === option.id && palette.mode === mode,
                )?.colors,
            };
            return {
                label: option.label,
                modeLabel: mode === 'light' ? 'Light preview' : 'Dark preview',
                style: Object.fromEntries(
                    ['canvas', 'surface', 'ink', 'muted', 'accent', 'border'].map((key) => [
                        '--preview-' + key,
                        colors[key],
                    ]),
                ),
            };
        },
        // Derive can confirm palette from the current settings state
        get canConfirmPalette() {
            const pending = this.pendingPalette;
            if (!pending) return false;
            const { mode, id } = pending;
            const themeState = theme();
            const key = mode === 'light' ? 'lightPalette' : 'darkPalette';
            return (
                id !== themeState.style[key] ||
                (!themeState.followsSystem && mode !== themeState.resolved)
            );
        },
        // Open the palette browser
        openPaletteBrowser() {
            this.paletteQuery = '';
            this.paletteTab = theme().resolved;
            this.pendingPalette = null;
            (this.$refs.paletteBrowser as HTMLDialogElement).showModal();
            this.resetPaletteResults();
        },
        // Reset the palette results
        resetPaletteResults() {
            (this.$refs.paletteResults as HTMLElement).scrollTop = 0;
        },
        // Show the palette tab
        showPaletteTab(mode: ThemeMode) {
            this.paletteTab = mode;
            this.resetPaletteResults();
        },
        // Navigate light and dark palette tabs with supported keyboard shortcuts
        navigatePaletteTabs(event: KeyboardEvent) {
            if (!tabNavigationKey(event.key)) return;
            event.preventDefault();
            this.showPaletteTab(nextTab(['light', 'dark'] as const, this.paletteTab, event.key)!);
            const tab =
                this.paletteTab === 'light'
                    ? this.$refs.lightPaletteTab
                    : this.$refs.darkPaletteTab;
            (tab as HTMLButtonElement).focus();
        },
        // Close the palette browser
        closePaletteBrowser() {
            this.pendingPalette = null;
            theme().clearPalettePreview(this.previewOwner);
            const dialog = this.$refs.paletteBrowser as HTMLDialogElement;
            if (!dialog?.open) return;
            dialog.close();
            (this.$refs.browsePalettes as HTMLButtonElement).focus({ preventScroll: true });
        },
        // Stage the selected palette and publish its owner-scoped theme preview
        previewChoice(mode: ThemeMode, id: string) {
            this.pendingPalette = { mode, id };
            theme().showPalettePreview(this.previewOwner, mode, id);
        },
        // Confirm the palette
        confirmPalette() {
            if (!this.pendingPalette || !this.canConfirmPalette) return;
            theme().selectPalette(this.pendingPalette.mode, this.pendingPalette.id);
            this.closePaletteBrowser();
        },
    }));
    Alpine.data('settings', () => ({
        jumpToSection,
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
