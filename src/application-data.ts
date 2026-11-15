import { paginate } from './pagination';
import { formatBinarySize } from './binary-size';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { ApplicationData, StoredPluginData } from '../generated/runtime';
import { ownWindowMethods } from './window-work';

// Register the application data review and maintenance controller
export function registerApplicationData(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('applicationData', () => ({
        data: null as ApplicationData | null,
        pending: [] as StoredPluginData[],
        selected: [] as string[],
        usageTarget: null as StoredPluginData | null,
        filter: 'all',
        sort: 'name',
        page: 1,
        busy: false,
        error: '',
        // Connect pagination resets to filter and sort changes
        init() {
            ownWindowMethods(this, 'application data', [
                'load',
                'createBackup',
                'compact',
                'purge',
                'restore',
            ]);
            // Reset pagination when the data filter changes
            this.$watch('filter', () => {
                this.page = 1;
            });
            // Reset pagination when the data sort changes
            this.$watch('sort', () => {
                this.page = 1;
            });
        },
        // Open application data review and load current usage
        async open() {
            const dialog = this.$refs.dialog as HTMLDialogElement;
            if (!dialog.open) dialog.showModal();
            await this.load();
        },
        // Close application data review
        close() {
            if (this.busy) return;
            (this.$refs.dialog as HTMLDialogElement).close();
            this.pending = [];
        },
        // Format stored byte counts for display
        bytes(value: number) {
            return formatBinarySize(value, { maxUnit: 2, precision: [0, 1, 2] });
        },
        // Format a stored timestamp or its missing-value label
        date(value: number | null) {
            return value ? new Date(value).toLocaleDateString() : 'Not recorded';
        },
        // Filter and sort the stored plugin data inventory
        get filtered(): StoredPluginData[] {
            return (this.data?.plugins || [])
                .filter(
                    (plugin) =>
                        this.filter === 'all' ||
                        (this.filter === 'not-installed'
                            ? !plugin.installed
                            : plugin.cache_bytes > 0),
                )
                .sort((left, right) =>
                    this.sort === 'size'
                        ? right.logical_bytes - left.logical_bytes
                        : this.sort === 'age'
                          ? (left.removed_ms || left.updated_ms || 0) -
                            (right.removed_ms || right.updated_ms || 0)
                          : left.name.localeCompare(right.name),
                );
        },
        // Create the paginated stored-data view
        get dataPage() {
            return paginate(this.filtered, this.page, 12);
        },
        // Expose the number of stored-data pages
        get pages() {
            return this.dataPage.pages;
        },
        // Expose the current page of stored-data rows
        get rows() {
            return this.dataPage.items;
        },
        // Describe the current stored-data page
        get pageLabel() {
            return `Page ${this.dataPage.page} of ${this.pages}`;
        },
        // Move to the previous stored-data page
        previous() {
            this.page = Math.max(1, this.dataPage.page - 1);
        },
        // Move to the next stored-data page
        next() {
            this.page = Math.min(this.pages, this.dataPage.page + 1);
        },
        // Toggle selection of a stored-data row
        select(event: Event) {
            const target = event.currentTarget as HTMLInputElement;
            const id = target.dataset.id!;
            if (target.checked) {
                if (this.selected.length < 64) this.selected = [...this.selected, id];
                else target.checked = false;
            } else this.selected = this.selected.filter((value) => value !== id);
        },
        // Check whether a stored-data row is selected
        isSelected(id: string) {
            return this.selected.includes(id);
        },
        // Review the selected stored-data entries before removal
        reviewSelected() {
            this.pending = (this.data?.plugins || []).filter((plugin) =>
                this.selected.includes(plugin.plugin_id),
            );
        },
        // Open dependency information for the selected data entry
        usage(event: Event) {
            this.usageTarget =
                this.data?.plugins.find(
                    (plugin) =>
                        plugin.plugin_id === (event.currentTarget as HTMLElement).dataset.id,
                ) || null;
        },
        // Summarize references to a stored-data entry
        usageText(id: string) {
            const value = this.data?.usage[id];
            return `${value?.bindings || 0} bindings · ${value?.saved_macros || 0} saved macros`;
        },
        // Describe the data entries selected for removal review
        get reviewTitle() {
            return this.pending.length === 1
                ? `Delete stored data for “${this.pending[0].name}”?`
                : `Delete stored data for ${this.pending.length} plugins?`;
        },
        // Load the current stored-data inventory and usage
        async load() {
            this.busy = true;
            this.error = '';
            this.pending = [];
            this.selected = [];
            this.usageTarget = null;
            try {
                this.data = await app().bridge.command('application_data');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Open removal review for one stored-data entry
        review(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            this.pending = this.data?.plugins.filter((plugin) => plugin.plugin_id === id) || [];
        },
        // Cancel the pending data-removal review
        cancelReview() {
            this.pending = [];
        },
        // Restore a recoverable stored-data entry
        async restore(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            const target = this.data?.plugins.find((plugin) => plugin.plugin_id === id);
            if (!target || this.busy) return;
            this.busy = true;
            this.error = '';
            try {
                this.data = await app().bridge.command('restore_plugin_data', {
                    id: target.plugin_id,
                    reviewToken: target.review_token,
                });
                app().notify('Previous data restored. You can now enable the plugin.', 'success');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Remove the reviewed data entries through the backend
        async purge() {
            if (!this.pending.length || this.busy) return;
            const target = this.pending;
            this.busy = true;
            this.error = '';
            try {
                this.data =
                    target.length === 1
                        ? await app().bridge.command('purge_plugin_data', {
                              id: target[0].plugin_id,
                              reviewToken: target[0].review_token,
                          })
                        : await app().bridge.command('purge_plugin_data_batch', {
                              reviews: target.map((plugin) => ({
                                  id: plugin.plugin_id,
                                  review_token: plugin.review_token,
                              })),
                          });
                this.selected = [];
                this.usageTarget = null;
                this.pending = [];
                app().notify(
                    `Removed stored data for ${target.length} plugin${target.length === 1 ? '' : 's'}.`,
                    'success',
                );
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Create a configuration backup before maintenance
        async createBackup() {
            this.busy = true;
            this.error = '';
            try {
                this.data = await app().bridge.command('create_recovery_snapshot');
                app().notify('Recovery snapshot saved.', 'success');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Request database compaction and refresh usage
        async compact() {
            this.busy = true;
            this.error = '';
            try {
                this.data = await app().bridge.command('compact_application_data');
                app().notify('Unused database space reclaimed.', 'success');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
    }));
}
