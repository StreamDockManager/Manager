import Alpine from '@alpinejs/csp';
import { nextTab, tabNavigationKey } from './tab-navigation';
import type { registerTheme } from './theme';
import { paletteColors, type ThemeMode } from './theme-style';
import { windowCloseOwners } from './window-close-owners';

// Shell-owned picker: route teardown must not end an explicit palette preview.
export function registerPaletteBrowser() {
    // Resolve the current settings appearance
    const theme = () => Alpine.store('theme') as ReturnType<typeof registerTheme>;
    Alpine.data('paletteBrowser', () => ({
        launcher: null as HTMLElement | null,
        drag: null as { pointer: number; x: number; y: number } | null,
        moveStart(event: PointerEvent) {
            if (event.button !== 0 || (event.target as HTMLElement).closest('button')) return;
            event.preventDefault();
            const panel = this.$refs.paletteBrowser as HTMLDialogElement;
            const rect = panel.getBoundingClientRect();
            this.drag = {
                pointer: event.pointerId,
                x: event.clientX - rect.left,
                y: event.clientY - rect.top,
            };
            (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        },
        movePanel(event: PointerEvent) {
            if (!this.drag || this.drag.pointer !== event.pointerId) return;
            const panel = this.$refs.paletteBrowser as HTMLDialogElement;
            const x = Math.max(
                8,
                Math.min(innerWidth - panel.offsetWidth - 8, event.clientX - this.drag.x),
            );
            const y = Math.max(
                8,
                Math.min(innerHeight - panel.offsetHeight - 8, event.clientY - this.drag.y),
            );
            Object.assign(panel.style, {
                left: `${x}px`,
                top: `${y}px`,
                right: 'auto',
                bottom: 'auto',
            });
        },
        endMove() {
            this.drag = null;
        },
        resetPosition() {
            const panel = this.$refs.paletteBrowser as HTMLDialogElement;
            for (const property of ['left', 'top', 'right', 'bottom'])
                panel.style.removeProperty(property);
            this.drag = null;
        },
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
        openPaletteBrowser(event: CustomEvent<HTMLElement>) {
            const dialog = this.$refs.paletteBrowser as HTMLDialogElement;
            if (dialog.open) {
                this.$refs.paletteSearch.focus();
                return;
            }
            this.launcher = event.detail;
            this.resetPosition();
            this.paletteQuery = '';
            this.paletteTab = theme().resolved;
            this.pendingPalette = null;
            dialog.show();
            this.$refs.paletteSearch.focus();
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
            const hadFocus = dialog.contains(document.activeElement);
            dialog.close();
            if (hadFocus && this.launcher?.isConnected)
                this.launcher.focus({ preventScroll: true });
        },
        escapePalette(event: KeyboardEvent) {
            if (event.key !== 'Escape' || event.defaultPrevented) return;
            event.preventDefault();
            event.stopPropagation();
            this.closePaletteBrowser();
        },
        // Stage the selected palette and publish its owner-scoped theme preview
        previewChoice(mode: ThemeMode, id: string) {
            this.pendingPalette = { mode, id };
            theme().showPalettePreview(this.previewOwner, mode, id, undefined, true);
        },
        // Confirm the palette
        confirmPalette() {
            if (!this.pendingPalette || !this.canConfirmPalette) return;
            theme().selectPalette(this.pendingPalette.mode, this.pendingPalette.id);
            this.closePaletteBrowser();
        },
    }));
}
