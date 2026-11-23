import { readTransfer } from './transfer-password';
import * as fileDialogs from './window-dialogs';
import { registerArchiveReviewer } from './archive-import';
import { openExportReview } from './export-review';
import Alpine from '@alpinejs/csp';
import { app } from './route-context';
import { native } from './bridge';
import type { MacroImportPreview } from '../generated/runtime';
import { registerCloseDraft, trackCloseRelease } from './close-releases';

// Register the macro archive import and export dialog controller
export function registerMacroSharing() {
    Alpine.data('macroSharing', () => ({
        selected: [] as string[],
        exporting: false,
        importing: false,
        imported: null as MacroImportPreview | null,
        error: '',
        remaining: 3,
        readyAt: 0,
        generation: 0,
        unregisterArchiveReviewer: undefined as (() => void) | undefined,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        timer: undefined as ReturnType<typeof setInterval> | undefined,
        // Return macros for the current macro sharing view
        get macros() {
            return [...app().snapshot.saved_macros].sort((a, b) => a.name.localeCompare(b.name));
        },
        // Check all selected for the current macro sharing state
        get allSelected() {
            return this.selected.length === this.macros.length;
        },
        // Check whether exported macros can include plugin settings
        get canIncludeExport() {
            return this.remaining === 0 && this.selected.length > 0 && !this.exporting;
        },
        // Check whether the staged archive offers plugin settings for import
        get canIncludeImport() {
            return this.remaining === 0 && Boolean(this.imported) && !this.importing;
        },
        // Return import items for the current macro sharing view
        get importItems() {
            return this.imported?.macros || [];
        },
        // Return import warnings for the current macro sharing view
        get importWarnings() {
            return this.imported?.warnings || [];
        },
        // Return imported settings for the current macro sharing view
        get importedSettings() {
            return Boolean(this.imported?.includes_plugin_settings);
        },
        // Attach macro sharing listeners and register owned draft resources
        init() {
            this.unregisterCloseOwner = registerCloseDraft(this, ['commitImport', 'export'], {
                name: 'Macro import review',
                // Report whether the controller retains unsaved or active work
                state: () => (this.imported ? 'dirty' : 'clean'),
                // Discard the staged media or editor state owned by the current dialog
                discard: () => this.releaseForClose(),
                // Release resources retained by the current controller
                release: () => this.releaseForClose(),
            });
            this.unregisterArchiveReviewer = registerArchiveReviewer('macros', (path) =>
                this.openImport(path),
            );
            this.$watch('$store.app.route', () => this.close());
        },
        // Release the prepared import session before closing its dialog
        async releaseForClose() {
            const id = this.imported?.id;
            if (id)
                await trackCloseRelease('macro-import:' + id, () =>
                    app().bridge.command('cancel_macro_import', { id }),
                );
            this.imported = null;
            this.close();
        },
        // Refresh the remaining lifetime of the staged macro import
        countdown() {
            clearInterval(this.timer);
            this.remaining = 3;
            this.readyAt = performance.now() + 3000;
            // Resume scheduled macro sharing work while its owner remains active
            this.timer = setInterval(() => {
                this.remaining = Math.max(0, Math.ceil((this.readyAt - performance.now()) / 1000));
                if (!this.remaining) clearInterval(this.timer);
            }, 100);
        },
        // Close the dialog and release its retained resources
        close() {
            this.generation++;
            this.exporting = false;
            clearInterval(this.timer);
            (this.$refs.exportDialog as HTMLDialogElement)?.close();
            (this.$refs.importDialog as HTMLDialogElement)?.close();
            const id = this.imported?.id;
            this.imported = null;
            if (id)
                void trackCloseRelease(
                    'macro-import:' + id,
                    () => app().bridge.command('cancel_macro_import', { id }),
                    // Report the asynchronous macro sharing failure without applying stale results
                ).catch(() => {});
        },
        // Remove macro sharing listeners and release owned resources
        destroy() {
            this.unregisterArchiveReviewer?.();
            this.unregisterCloseOwner?.();
            this.close();
        },
        // Open export in the current macro sharing draft
        openExport(event: CustomEvent<{ id?: string }>) {
            if (this.exporting || this.importing) return;
            this.close();
            this.error = '';
            this.selected = event.detail?.id ? [event.detail.id] : this.macros.map((m) => m.id);
            this.countdown();
            (this.$refs.exportDialog as HTMLDialogElement).showModal();
            (this.$refs.exportDecline as HTMLButtonElement).focus();
        },
        // Select all in the current macro sharing draft
        selectAll() {
            this.selected = this.allSelected ? [] : this.macros.map((m) => m.id);
        },
        // Cancel export in the current macro sharing draft
        cancelExport() {
            if (!this.exporting) this.close();
        },
        // Export selected macros without plugin settings
        exportWithoutSettings() {
            return this.export(false);
        },
        // Export selected macros with the explicitly selected plugin settings
        exportWithSettings() {
            if (!this.canIncludeExport || performance.now() < this.readyAt) return;
            return this.export(true);
        },
        // Export the selected macros with the requested plugin-settings policy
        async export(include: boolean) {
            if (this.exporting) return;
            if (!this.selected.length) {
                this.error = 'Select at least one macro.';
                return;
            }
            if (!native) {
                this.error = 'Macro export requires the desktop application.';
                return;
            }
            const generation = this.generation;
            const ids = [...this.selected];
            this.exporting = true;
            this.error = '';
            try {
                await app().flushDrafts?.();
                if (generation !== this.generation) return;
                this.exporting = false;
                this.close();
                openExportReview({ kind: 'macros', ids, include_plugin_settings: include });
            } catch (error) {
                if (generation === this.generation) this.error = String(error);
            } finally {
                if (generation === this.generation) this.exporting = false;
            }
        },
        // Open import in the current macro sharing draft
        async openImport(dropped?: string | Event) {
            if (this.importing || this.exporting) return;
            this.close();
            const generation = this.generation;
            this.importing = true;
            this.error = '';
            try {
                let path: string | null = typeof dropped === 'string' ? dropped : 'sample';
                if (native && typeof dropped !== 'string') {
                    const { open } = fileDialogs;
                    path = await open({
                        multiple: false,
                        filters: [{ name: 'Stream Dock Manager macros', extensions: ['zip'] }],
                    });
                }
                if (!path || generation !== this.generation) return;
                const imported = await readTransfer((password) =>
                    app().bridge.command('prepare_macro_import', { path: path!, password }),
                );
                if (generation !== this.generation) {
                    void trackCloseRelease(
                        'macro-import:' + imported.id,
                        () => app().bridge.command('cancel_macro_import', { id: imported.id }),
                        // Report the asynchronous macro sharing failure without applying stale results
                    ).catch(() => {});
                    return;
                }
                this.imported = imported;
                this.countdown();
                (this.$refs.importDialog as HTMLDialogElement).showModal();
                this.$nextTick(() => (this.$refs.importDecline as HTMLButtonElement).focus());
            } catch (error) {
                if (generation === this.generation) {
                    this.error = String(error);
                    (this.$refs.importDialog as HTMLDialogElement).showModal();
                }
            } finally {
                this.importing = false;
            }
        },
        // Cancel import in the current macro sharing draft
        cancelImport() {
            if (!this.importing) this.close();
        },
        // Refresh the import review after a workspace revision change
        async recheckImport() {
            if (!this.imported || this.importing) return;
            const generation = this.generation;
            this.importing = true;
            this.error = '';
            try {
                const imported = await app().command('recheck_macro_import', {
                    id: this.imported.id,
                });
                if (generation === this.generation) {
                    this.imported = imported;
                    this.countdown();
                }
            } catch (error) {
                if (generation === this.generation) this.error = String(error);
            } finally {
                this.importing = false;
            }
        },
        // Commit the reviewed import without plugin settings
        importWithoutSettings() {
            return this.commitImport(false);
        },
        // Commit the reviewed import with the explicitly selected plugin settings
        importWithSettings() {
            if (!this.canIncludeImport || performance.now() < this.readyAt) return;
            return this.commitImport(true);
        },
        // Commit the prepared import using its reviewed revision and settings choice
        async commitImport(include: boolean) {
            if (!this.imported || this.importing) return;
            this.importing = true;
            this.error = '';
            try {
                app().apply(
                    await app().command('commit_macro_import', {
                        id: this.imported.id,
                        expectedRevision: this.imported.expected_revision,
                        includePluginSettings: include,
                    }),
                );
                this.imported = null;
                this.close();
                app().notify(
                    'Macros imported. Use Undo to restore the previous library.',
                    'success',
                );
            } catch (error) {
                this.error = String(error);
            } finally {
                this.importing = false;
            }
        },
    }));
}
