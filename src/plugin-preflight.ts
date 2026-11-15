import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
import { showFeatureDialog, closeFeatureDialog, runFeatureWork } from './feature-dialog-lifecycle';
import type { PluginPreflight } from '../generated/runtime';

// Register the plugin preflight
export function registerPluginPreflight(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('pluginPreflight', () => ({
        working: false,
        error: '',
        dialogEpoch: 0,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Derive app from the current plugin preflight state
        get app() {
            return app();
        },
        // Run the requested plugin preflight operation
        async run(work: () => Promise<void>) {
            await runFeatureWork(this, work);
        },
        // Show the selected plugin preflight content
        show(name: string) {
            showFeatureDialog(this, name);
        },
        // Release plugin preflight state and associated lifecycle resources
        close(name: string) {
            closeFeatureDialog(this, name);
        },
        preflight: null as PluginPreflight | null,
        preflightMode: 'install',
        preflightId: '',
        // Choose the runtime
        chooseRuntime() {
            if (!this.preflight?.runtime) return;
            window.dispatchEvent(
                new CustomEvent('open-runtime', {
                    detail: {
                        kind: this.preflight.runtime.requirement.kind,
                        line: this.preflight.runtime.recommended_line || undefined,
                        pluginId: this.preflightMode === 'enable' ? this.preflightId : undefined,
                        pluginName: this.preflight.name,
                        requirement: this.preflight.runtime.requirement,
                    },
                }),
            );
        },
        // Refresh the runtime
        async refreshRuntime() {
            if (!this.preflight) return;
            try {
                this.preflight = await app().command('preflight_plugin', {
                    id: this.preflightMode === 'enable' ? this.preflightId : undefined,
                    path: this.preflightMode === 'install' ? this.preflight.source : undefined,
                });
            } catch {}
        },
        // Initialize plugin preflight state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'plugin preflight', ['run', 'openPreflight', 'refreshRuntime']);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                ['run', 'openPreflight', 'refreshRuntime'],
                {
                    name: 'Plugin compatibility review',
                    retainOnUnmount: true,
                    // Return a snapshot of the current plugin preflight state
                    state: () => 'clean',
                    // Discard pending plugin preflight edits without saving them
                    discard: () => this.releaseForClose(),
                    // Release resources retained by the plugin preflight operation
                    release: () => this.releaseForClose(),
                },
            );
        },
        // Open the preflight
        async openPreflight(event: CustomEvent<{ path?: string; id?: string }>) {
            if (this.working) return;
            this.preflight = null;
            this.preflightMode = event.detail.id ? 'enable' : 'install';
            this.preflightId = event.detail.id || '';
            this.show('preflightDialog');
            // Complete the deferred plugin preflight work using the current operation state
            await this.run(async () => {
                this.preflight = await app().command('preflight_plugin', {
                    id: event.detail.id || null,
                    path: event.detail.path || null,
                });
                if (
                    this.preflightMode === 'enable' &&
                    this.preflight.runtime &&
                    this.preflight.runtime.state !== 'ready'
                )
                    this.$nextTick(() => this.chooseRuntime());
            });
        },
        // Confirm the preflight
        async confirmPreflight() {
            // Complete the deferred plugin preflight work using the current operation state
            await this.run(async () => {
                if (!this.preflight) return;
                if (this.preflightMode === 'enable') {
                    if (!this.preflight.can_install) return;
                    app().apply(
                        await app().command('enable_plugin', {
                            id: this.preflightId,
                            enabled: true,
                        }),
                    );
                } else {
                    if (!this.preflight.can_install) return;
                    const review = await app().command('prepare_plugin_package', {
                        path: this.preflight.source,
                    });
                    window.dispatchEvent(
                        new CustomEvent('review-plugin-imports', { detail: [review] }),
                    );
                }
                this.close('preflightDialog');
                app().notify(
                    this.preflightMode === 'enable'
                        ? 'Plugin enabled'
                        : 'Plugin installed and disabled',
                    'success',
                );
            });
        },
        // Close the preflight
        closePreflight() {
            if (this.working) return;
            this.close('preflightDialog');
        },
        // Release the for close
        releaseForClose() {
            this.close('preflightDialog');
        },
        // Release plugin preflight state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.releaseForClose();
        },
    }));
}
