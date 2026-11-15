import { formatBinarySize } from './binary-size';
import type { AppState } from './state';
import type { CacheStatus } from '../generated/runtime';

// Format cache usage with binary size units
export function cacheBytes(value: number): string {
    return formatBinarySize(value, { maxUnit: 3 });
}

// Create cache usage and cleanup controls
export function createCacheSettings(app: () => AppState) {
    return {
        status: null as CacheStatus | null,
        loading: false,
        clearing: false,
        confirming: false,
        error: '',
        notice: '',
        // Report whether cache loading or clearing is in progress
        get busy() {
            return this.loading || this.clearing;
        },
        // Expose the known cache directories
        get folders() {
            return (this.status?.folders ?? []).map((folder) => ({
                ...folder,
                usage: `${cacheBytes(folder.bytes)} of ${cacheBytes(folder.max_bytes)}`,
                count: `${folder.entries} of ${folder.max_entries} entries`,
                age: `${folder.max_age_seconds / 86400} day${folder.max_age_seconds === 86400 ? '' : 's'}`,
                protected: `${folder.protected_entries} protected ${folder.protected_entries === 1 ? 'entry' : 'entries'}`,
            }));
        },
        // Expose warnings returned by cache inspection
        get warnings() {
            return this.status?.errors.join('\n') ?? '';
        },
        // Refresh cache usage and directory information
        async refresh() {
            if (this.busy || this.confirming) return;
            this.loading = true;
            this.error = '';
            try {
                this.status = await app().bridge.command('cache_status');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.loading = false;
            }
        },
        // Clear the selected cache through the backend and refresh usage
        async clear() {
            if (this.busy || !this.confirming) return;
            this.clearing = true;
            this.error = '';
            this.notice = '';
            try {
                const result = await app().bridge.command('clear_cache');
                this.status = result.status;
                this.error = result.errors.join('\n');
                this.notice = `Deleted ${result.removed_entries} cache entries (${cacheBytes(result.removed_bytes)}).`;
            } catch (error) {
                this.error = String(error);
            } finally {
                this.clearing = false;
                this.confirming = false;
            }
        },
    };
}
