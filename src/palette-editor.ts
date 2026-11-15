import type AlpineType from '@alpinejs/csp';
import type { CustomPalette, MediaOptions } from '../generated/runtime';
import { native } from './bridge';
import { registerCloseDraft } from './close-releases';
import { chooseMediaInput, droppedMedia, mediaFormats, selectMediaInput } from './media-input';
import { releaseMediaOwner } from './media-leases';
import { openMediaLibrary } from './media-library';
import { suggestPaletteColors } from './palette-suggestions';
import type { AppState } from './state';
import { paletteColors, palettesFor, type ThemeMode } from './theme-style';
import type { registerTheme } from './theme';
import { ownWindowMethods } from './window-work';

const primaryLabels: Record<string, string> = {
    canvas: 'Canvas',
    surface: 'Surface',
    ink: 'Text',
    muted: 'Muted',
    border: 'Border',
};

// Register the palette editor
export function registerPaletteEditor(Alpine: typeof AlpineType, app: () => AppState) {
    // Resolve the current palette editor appearance
    const theme = () => Alpine.store('theme') as ReturnType<typeof registerTheme>;
    Alpine.data('paletteEditor', () => ({
        mode: 'dark' as ThemeMode,
        selection: 'default',
        draft: null as CustomPalette | null,
        error: '',
        busy: false,
        importing: false,
        openState: false,
        epoch: 0,
        owner: 'palette:' + crypto.randomUUID(),
        previewOwner: Symbol('Palette editor'),
        unregister: undefined as (() => void) | undefined,
        focusReturn: null as HTMLElement | null,
        wallpaperPreview: '',
        // Derive options from the current palette editor state
        get options() {
            return palettesFor(this.mode, theme().customPalettes);
        },
        // Derive custom from the current palette editor state
        get custom() {
            return theme().customPalettes.find((palette) => palette.id === this.selection);
        },
        // Derive fields from the current palette editor state
        get fields() {
            return Object.entries(this.draft?.colors || {}).map(([key, value]) => ({
                key,
                value,
                label:
                    primaryLabels[key] ||
                    key.replaceAll('-', ' ').replace(/^./, (letter) => letter.toUpperCase()),
            }));
        },
        // Derive primary fields from the current palette editor state
        get primaryFields() {
            return Object.keys(primaryLabels).flatMap((key) =>
                this.fields.filter((field) => field.key === key),
            );
        },
        // Derive advanced fields from the current palette editor state
        get advancedFields() {
            return this.fields.filter(({ key }) => key !== 'accent' && !(key in primaryLabels));
        },
        // Derive preview name from the current palette editor state
        get previewName() {
            return this.draft
                ? this.draft.name.trim() || 'Untitled palette'
                : this.options.find((option) => option.id === this.selection)?.label || 'Default';
        },
        // Derive preview style from the current palette editor state
        get previewStyle() {
            const colors = this.draft?.colors || {
                ...paletteColors(this.mode, this.selection),
                ...this.custom?.colors,
            };
            return Object.fromEntries(
                ['canvas', 'surface', 'ink', 'muted', 'accent', 'accent-ink', 'border'].map(
                    (role) => ['--preview-' + role, colors[role]],
                ),
            );
        },
        // Derive suggestions from the current palette editor state
        get suggestions() {
            const accent = this.draft?.colors.accent;
            if (!accent) return [];
            // Convert each entry into the representation required by palette editor
            return ['Tonal', 'Complementary'].map((label) => {
                const colors = suggestPaletteColors(accent, this.mode, label === 'Complementary');
                return {
                    label,
                    colors,
                    swatches: [colors.canvas, colors.surface, colors.ink, colors['accent-ink']],
                };
            });
        },
        // Apply the suggestion
        applySuggestion(colors: Record<string, string>) {
            if (this.draft && !this.busy && !this.importing) {
                Object.assign(this.draft.colors, colors);
                theme().applyColors();
            }
        },
        // Derive wallpaper asset from the current palette editor state
        get wallpaperAsset() {
            return this.draft?.wallpaper?.asset_id || '';
        },
        // Derive native from the current palette editor state
        get native() {
            return native;
        },
        // Initialize palette editor state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'palette editor', ['selectInput', 'save', 'deletePalette']);
            this.$watch('wallpaperAsset', () => void this.previewWallpaper());
            this.unregister = registerCloseDraft(this, ['save', 'deletePalette'], {
                name: 'Palette editor',
                // Return a snapshot of the current palette editor state
                state: () => (this.draft ? 'dirty' : 'clean'),
                // Discard pending palette editor edits without saving them
                discard: () => this.close(),
                // Release resources retained by the palette editor operation
                release: () => this.close(),
            });
        },
        // Release palette editor state and associated lifecycle resources
        destroy() {
            this.unregister?.();
            theme().clearPalettePreview(this.previewOwner);
            this.close();
        },
        // Open the current palette editor target
        open() {
            this.mode = theme().resolved;
            this.selection =
                this.mode === 'dark' ? theme().style.darkPalette : theme().style.lightPalette;
            this.openState = true;
            this.focusReturn =
                document.activeElement instanceof HTMLElement ? document.activeElement : null;
            (this.$refs.dialog as HTMLDialogElement).showModal();
        },
        // Release palette editor state and associated lifecycle resources
        close() {
            if (this.busy) return;
            this.cancelEdit();
            this.openState = false;
            (this.$refs.dialog as HTMLDialogElement)?.close();
            this.focusReturn?.focus();
        },
        // Change the mode
        changeMode(event: Event) {
            this.mode = (event.currentTarget as HTMLSelectElement).value as ThemeMode;
            this.selection = 'default';
            this.cancelEdit();
        },
        // Change the selection
        changeSelection(event: Event) {
            this.selection = (event.currentTarget as HTMLSelectElement).value;
            this.cancelEdit();
        },
        // Open the selected palette editor item for editing
        edit(duplicate = false, create = false) {
            if (this.busy || this.importing) return;
            this.cancelEdit();
            const original = create ? undefined : this.custom;
            const source = create ? 'default' : this.selection;
            this.draft = {
                id: !duplicate && original ? original.id : '',
                name: create
                    ? ''
                    : original?.name ||
                      this.options.find((entry) => entry.id === source)?.label ||
                      '',
                mode: this.mode,
                colors: { ...paletteColors(this.mode, source), ...original?.colors },
                wallpaper: original?.wallpaper
                    ? structuredClone(Alpine.raw(original.wallpaper))
                    : null,
            };
            if (duplicate) this.draft.name += ' copy';
            theme().showPalettePreview(this.previewOwner, this.mode, 'preview', this.draft);
            this.$nextTick(() => (this.$refs.name as HTMLInputElement)?.focus());
        },
        // Add the palette
        addPalette() {
            this.edit(false, true);
        },
        // Open the palette editor in duplicate mode
        duplicate() {
            this.edit(true);
        },
        // Cancel the edit
        cancelEdit() {
            ++this.epoch;
            this.importing = false;
            this.draft = null;
            theme().clearPalettePreview(this.previewOwner);
            this.error = '';
            const owner = this.owner;
            this.owner = 'palette:' + crypto.randomUUID();
            this.wallpaperPreview = '';
            if (native) void releaseMediaOwner(app(), owner).catch(() => {});
        },
        // Cancel palette editing and restore focus to the palette selector
        backToPalettes() {
            this.cancelEdit();
            this.$nextTick(() => (this.$refs.paletteSelect as HTMLSelectElement)?.focus());
        },
        // Set the color
        setColor(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            if (this.draft) {
                this.draft.colors[input.name] = input.value;
                theme().applyColors();
            }
        },
        // Load the draft wallpaper thumbnail without applying stale responses to a newer draft
        async previewWallpaper() {
            this.wallpaperPreview = '';
            const asset = this.draft?.wallpaper?.asset_id;
            if (!asset) return;
            const epoch = this.epoch;
            try {
                const image = await app().bridge.command('media_thumbnail', { asset });
                if (epoch === this.epoch && this.draft?.wallpaper?.asset_id === asset)
                    this.wallpaperPreview = image;
            } catch {
                if (epoch === this.epoch && this.draft?.wallpaper?.asset_id === asset)
                    this.error =
                        'Wallpaper preview is unavailable. Choose a replacement or remove the wallpaper.';
            }
        },
        // Clear the wallpaper
        clearWallpaper() {
            if (this.draft) this.draft.wallpaper = null;
        },
        // Set the wallpaper
        setWallpaper(asset: string, options?: MediaOptions) {
            if (this.draft) {
                this.draft.wallpaper = {
                    asset_id: asset,
                    options: options || {},
                    format: 'png',
                    animated: false,
                };
            }
        },
        // Browse available items for palette editor
        browse() {
            if (!this.draft || this.busy || this.importing) return;
            const epoch = this.epoch;
            openMediaLibrary({
                owner: this.owner,
                formats: mediaFormats,
                // Return the currently selected palette editor value
                current: () => this.openState && epoch === this.epoch,
                // Choose the active item for the palette editor workflow
                choose: (asset, options) => this.setWallpaper(asset, options),
            });
        },
        // Choose the file
        chooseFile() {
            return this.selectInput();
        },
        // Drop the media
        dropMedia(event: CustomEvent<File[]>) {
            return this.selectInput(event);
        },
        // Select the input
        async selectInput(event?: CustomEvent<File[]>) {
            if (!this.draft || this.busy || this.importing) return;
            const epoch = this.epoch;
            // Return the currently selected palette editor value
            const current = () => this.openState && epoch === this.epoch;
            this.importing = true;
            this.error = '';
            try {
                const input = event ? droppedMedia(event) : await chooseMediaInput(mediaFormats);
                if (!input || !current()) return;
                await selectMediaInput(app(), input, {
                    owner: this.owner,
                    formats: mediaFormats,
                    current,
                    // Apply the pending palette editor changes
                    apply: (asset, options) => this.setWallpaper(asset, options),
                });
            } catch (error) {
                if (current()) this.error = String(error);
            } finally {
                if (current()) this.importing = false;
            }
        },
        // Save pending changes through the palette editor workflow
        async save() {
            if (!this.draft?.name.trim() || this.busy || this.importing) return;
            this.busy = true;
            this.error = '';
            try {
                const palette = structuredClone(Alpine.raw(this.draft));
                const updated = await app().bridge.command('update_theme_palette', { palette });
                theme().acceptPalettes(updated);
                this.selection = palette.id || updated.custom_palettes.at(-1)?.id || 'default';
                this.backToPalettes();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Confirm the delete
        confirmDelete() {
            if (this.custom) (this.$refs.confirm as HTMLDialogElement).showModal();
        },
        // Cancel the delete
        cancelDelete() {
            (this.$refs.confirm as HTMLDialogElement).close();
        },
        // Delete the palette
        async deletePalette() {
            if (!this.custom || this.busy) return;
            this.busy = true;
            this.error = '';
            try {
                const updated = await app().bridge.command('update_theme_palette', {
                    delete: this.selection,
                });
                theme().acceptPalettes(updated);
                this.selection = 'default';
                this.cancelEdit();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
                this.cancelDelete();
            }
        },
    }));
}
