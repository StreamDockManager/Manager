import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
import { showFeatureDialog, closeFeatureDialog, runFeatureWork } from './feature-dialog-lifecycle';
import * as fileDialogs from './window-dialogs';
import { native } from './bridge';
import type { PluginPreflight } from '../generated/runtime';

// Register the plugin folder import
export function registerPluginFolderImport(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('pluginFolderImport', () => ({
        working: false,
        error: '',
        dialogEpoch: 0,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Derive app from the current plugin folder import state
        get app() {
            return app();
        },
        // Run the requested plugin folder import operation
        async run(work: () => Promise<void>) {
            await runFeatureWork(this, work);
        },
        // Show the selected plugin folder import content
        show(name: string) {
            showFeatureDialog(this, name);
        },
        // Release plugin folder import state and associated lifecycle resources
        close(name: string) {
            closeFeatureDialog(this, name);
            this.candidates = [];
            this.selected = [];
            this.results = [];
        },
        candidates: [] as PluginPreflight[],
        selected: [] as string[],
        sourceRoot: '',
        source: 'vsd',
        // Change the source
        async changeSource() {
            this.sourceRoot = '';
            this.query = '';
            this.candidates = [];
            this.selected = [];
            this.results = [];
            await this.scanPlugins();
        },
        query: '',
        results: [] as { source: string; status: string; message: string }[],
        // Derive filtered candidates from the current plugin folder import state
        get filteredCandidates() {
            return this.candidates.filter((c) =>
                `${c.name} ${c.id} ${c.source}`.toLowerCase().includes(this.query.toLowerCase()),
            );
        },
        // Initialize plugin folder import state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'plugin folder import', [
                'run',
                'openPluginImport',
                'choosePluginFolder',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                ['run', 'openPluginImport', 'choosePluginFolder'],
                {
                    name: 'Plugin folder import selection',
                    retainOnUnmount: true,
                    // Return a snapshot of the current plugin folder import state
                    state: () => (this.selected.length ? 'dirty' : 'clean'),
                    // Discard pending plugin folder import edits without saving them
                    discard: () => this.releaseForClose(),
                    // Release resources retained by the plugin folder import operation
                    release: () => this.releaseForClose(),
                },
            );
        },
        // Open the plugin import
        async openPluginImport() {
            if (this.working) return;
            this.show('pluginImportDialog');
            this.results = [];
            if (!this.candidates.length) await this.scanPlugins();
        },
        // Scan the selected source roots and clear selections from the previous scan
        async scanPlugins() {
            // Complete the deferred plugin folder import work using the current operation state
            await this.run(async () => {
                this.candidates = await app().command('scan_plugin_sources', {
                    roots: this.sourceRoot ? [this.sourceRoot] : [],
                    source: this.source,
                });
                this.selected = [];
            });
        },
        // Choose the plugin folder
        async choosePluginFolder() {
            if (!native) {
                app().notify('Folder selection is available in the desktop app.', 'warning');
                return;
            }
            const { open } = fileDialogs;
            const path = await open({ directory: true, multiple: false });
            if (path) {
                this.sourceRoot = path;
                await this.scanPlugins();
            }
        },
        // Select the available
        selectAvailable() {
            const ids = new Set<string>();
            this.selected = this.candidates
                // Keep entries that satisfy the plugin folder import selection criteria
                .filter((c) => {
                    if (c.installed || !c.can_install || ids.has(c.id)) return false;
                    ids.add(c.id);
                    return true;
                })
                .map((c) => c.source)
                .slice(0, 4);
        },
        // Import the selected plugins
        async importSelectedPlugins() {
            // Complete the deferred plugin folder import work using the current operation state
            await this.run(async () => {
                const result = await app().command('import_plugin_selection', {
                    paths: this.selected,
                });
                if (result.reviews.length) {
                    this.close('pluginImportDialog');
                    window.dispatchEvent(
                        new CustomEvent('review-plugin-imports', { detail: result.reviews }),
                    );
                }
                if (!(this.$refs.pluginImportDialog as HTMLDialogElement).open) return;
                this.results = result.results;
                this.selected = [];
                this.candidates = await app().command('scan_plugin_sources', {
                    roots: this.sourceRoot ? [this.sourceRoot] : [],
                    source: this.source,
                });
            });
        },
        // Close the plugin import
        closePluginImport() {
            if (this.working) return;
            this.close('pluginImportDialog');
        },
        // Release the for close
        releaseForClose() {
            this.close('pluginImportDialog');
        },
        // Release plugin folder import state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.releaseForClose();
        },
    }));
}
