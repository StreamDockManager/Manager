import * as fileDialogs from './window-dialogs';
import Alpine from '@alpinejs/csp';
import { app } from './route-context';
import { paginate } from './pagination';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { ownWindowMethods } from './window-work';

// Register installed-package review and export controls
export function registerInstalledPackageExport() {
    Alpine.data('installedPackageExport', () => ({
        plugin: '',
        label: '',
        review: '',
        receipt: '',
        files: [] as string[],
        selected: [] as string[],
        query: '',
        page: 1,
        busy: false,
        dragSupported: false,
        error: '',
        opener: null as HTMLElement | null,
        unregister: undefined as (() => void) | undefined,
        // Paginate the files included in the staged package export
        get filePage() {
            const query = this.query.trim().toLowerCase();
            return paginate(
                this.files.filter((file) => file.toLowerCase().includes(query)),
                this.page,
                24,
            );
        },
        // Attach installed-package export listeners and window-close coordination
        init() {
            const methods: Array<'open' | 'build' | 'save' | 'drag'> = [
                'open',
                'build',
                'save',
                'drag',
            ];
            ownWindowMethods(this, 'installed plugin export', methods);
            this.unregister = registerCloseDraft(this, methods, {
                name: 'Installed plugin export',
                // Report unsaved installed-package export work to window-close coordination
                state: () =>
                    this.review && this.selected.length && !this.receipt ? 'dirty' : 'clean',
                // Discard the current installed-package export draft and release temporary state
                discard: () => this.close(),
                // Release temporary installed-package export state during close coordination
                release: () => this.close(),
            });
            this.$watch('query', () => {
                this.page = 1;
            });
        },
        // Detach installed-package export listeners and release owned state
        destroy() {
            this.unregister?.();
            this.close();
        },
        // Move to the previous installed-package export result page
        previous() {
            this.page = Math.max(1, this.filePage.page - 1);
        },
        // Move to the next installed-package export result page
        next() {
            this.page = Math.min(this.filePage.pages, this.filePage.page + 1);
        },
        // Release temporary installed-package export state during close coordination
        release(id: string) {
            if (id)
                void trackCloseRelease('archive:' + id, () =>
                    app().bridge.command('release_archive_export', { id }),
                ).catch(() => {});
        },
        // Close installed-package export and invalidate stale asynchronous work
        close() {
            this.release(this.review);
            this.release(this.receipt);
            this.review = this.receipt = this.plugin = '';
            (this.$refs.dialog as HTMLDialogElement).close();
            if (this.opener?.isConnected && this.opener.getClientRects().length)
                this.opener.focus();
            this.opener = null;
        },
        // Open installed-package export for the requested item
        async open(event: CustomEvent<{ id: string; label: string; opener: HTMLElement }>) {
            if (this.busy) return;
            this.close();
            this.plugin = event.detail.id;
            this.label = event.detail.label;
            this.opener = event.detail.opener;
            this.error = '';
            this.files = [];
            this.selected = [];
            this.query = '';
            this.page = 1;
            this.busy = true;
            (this.$refs.dialog as HTMLDialogElement).showModal();
            try {
                const [id, files] = await app().bridge.command('review_installed_package_export', {
                    pluginId: event.detail.id,
                });
                if (!this.plugin) {
                    this.release(id);
                    return;
                }
                this.review = id;
                this.files = files;
                this.dragSupported = await app().bridge.command('archive_drag_supported');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Prepare a validated installed-package export for review
        async build() {
            if (this.busy || !this.review || this.receipt) return;
            this.busy = true;
            this.error = '';
            const reviewId = this.review;
            try {
                const receipt = await app().bridge.command('build_installed_package_export', {
                    reviewId,
                    selected: [...this.selected],
                });
                if (this.review !== reviewId) this.release(receipt);
                else {
                    this.receipt = receipt;
                    this.$nextTick(() => (this.$refs.save as HTMLButtonElement)?.focus());
                }
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Save the validated installed-package export draft through the application bridge
        async save() {
            if (this.busy || !this.receipt) return;
            this.busy = true;
            this.error = '';
            const receipt = this.receipt;
            try {
                const { save } = fileDialogs;
                const path = await save({
                    defaultPath: this.plugin + '.zip',
                    filters: [{ name: 'Plugin archive', extensions: ['zip'] }],
                });
                if (path && this.receipt === receipt) {
                    await app().bridge.command('save_installed_package_export', { receipt, path });
                    app().notify('Plugin ZIP saved.', 'success');
                }
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Start a native drag of the prepared package export
        async drag(event: MouseEvent) {
            if (event.button !== 0) return;
            event.preventDefault();
            if (!this.receipt || !this.dragSupported) return;
            try {
                await app().bridge.command('start_archive_drag', { receipt: this.receipt });
            } catch (error) {
                this.error = String(error);
            }
        },
    }));
}
