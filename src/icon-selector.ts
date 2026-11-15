import { ownWindowMethods, windowWork } from './window-work';
import { registerCloseDraft } from './close-releases';
import Alpine from '@alpinejs/csp';
import { native } from './bridge';
import { app } from './route-context';
import type { IconAsset, IconCollection } from '../generated/runtime';
import { iconImage } from './icon-svg';
export { iconImage } from './icon-svg';

type IconChoice = {
    id: string;
    name: string;
    image: string;
    asset?: IconAsset;
    license: string;
    error: string;
};
// Register icon search, selection and staged preview cleanup
export function registerIconSelector() {
    Alpine.data('iconSelector', () => ({
        query: '',
        prefix: '',
        start: 0,
        more: false,
        offline: native
            ? false
            : localStorage.getItem('stream-dock-manager.icons.offline') === 'true',
        connected: navigator.onLine,
        localOnly: true,
        collections: [] as IconCollection[],
        choices: [] as IconChoice[],
        selected: '',
        context: '',
        message: '',
        error: '',
        loading: false,
        saving: false,
        opened: false,
        generation: 0,
        preferenceEpoch: 0,
        timer: undefined as ReturnType<typeof setTimeout> | undefined,
        assets: new Map<string, IconAsset>(),
        networkListener: undefined as (() => void) | undefined,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Attach icon selection listeners and window-close coordination
        init() {
            ownWindowMethods(this, 'icon-selector', ['open', 'fetchIcons', 'choose']);
            const preferenceEpoch = this.preferenceEpoch;
            if (native)
                void app()
                    .bridge.command('client_preferences')
                    // Ignore a saved preference response after the user selects a newer preference
                    .then((value) => {
                        if (this.preferenceEpoch !== preferenceEpoch) return;
                        this.offline = value.offline_icons;
                        if (this.opened) this.schedule();
                    })
                    .catch((error) => {
                        this.error = String(error);
                    });
            this.unregisterCloseOwner = registerCloseDraft(this, ['choose'], {
                name: 'Icon selection',
                // Report unsaved icon selection work to window-close coordination
                state: () => (this.saving ? 'dirty' : 'clean'),
                // Discard the current icon selection draft and release temporary state
                discard: () => this.close(),
                // Release temporary icon selection state during close coordination
                release: () => this.close(),
            });
            this.$watch('query', () => this.schedule());
            this.$watch('prefix', () => this.schedule());
            // Refresh open icon searches when connectivity changes
            this.networkListener = () => {
                this.connected = navigator.onLine;
                if (this.opened) this.schedule();
            };
            window.addEventListener('online', this.networkListener);
            window.addEventListener('offline', this.networkListener);
            this.$watch('$store.app.route', () => this.close());
            this.$watch('$store.app.controlId', () => this.close());
        },
        // Determine whether icon lookup uses the offline collection
        get useOffline() {
            return this.offline || !this.connected;
        },
        // Describe the current icon result page
        get pageLabel() {
            return 'Page ' + (this.start / 64 + 1);
        },
        // Describe icon search progress, errors and availability
        get status() {
            return this.loading
                ? 'Loading icons…'
                : this.message ||
                      this.choices.length + ' icons. Select an icon to save it offline.';
        },
        // Resolve the currently selected icon result
        get selectedAsset() {
            return this.assets.get(this.selected);
        },
        // Open icon selection for the requested item
        async open(event: CustomEvent<{ icon: string; asset?: IconAsset; context: string }>) {
            this.assets.clear();
            this.generation++;
            this.context = event.detail.context;
            this.selected = event.detail.icon.includes(':')
                ? event.detail.icon
                : event.detail.icon
                  ? 'lucide:' + event.detail.icon
                  : '';
            this.query = '';
            this.prefix = '';
            this.start = 0;
            this.error = '';
            this.saving = false;
            if (event.detail.asset) this.assets.set(event.detail.asset.id, event.detail.asset);
            this.opened = true;
            (this.$refs.icons as HTMLDialogElement).showModal();
            await this.search();
        },
        // Close icon selection and invalidate stale asynchronous work
        close() {
            this.opened = false;
            this.generation++;
            clearTimeout(this.timer);
            this.assets.clear();
            this.choices = [];
            this.collections = [];
            this.loading = false;
            this.saving = false;
            (this.$refs.icons as HTMLDialogElement)?.close();
        },
        // Detach icon selection listeners and release owned state
        destroy() {
            this.unregisterCloseOwner?.();
            this.close();
            if (this.networkListener) {
                window.removeEventListener('online', this.networkListener);
                window.removeEventListener('offline', this.networkListener);
            }
        },
        // Switch the icon source and refresh the current search
        toggleOffline() {
            this.preferenceEpoch++;
            if (native)
                void app()
                    .bridge.command('save_client_preference', {
                        name: 'offline_icons',
                        value: this.offline,
                    })
                    .catch((error) => {
                        this.error = String(error);
                    });
            else localStorage.setItem('stream-dock-manager.icons.offline', String(this.offline));
            this.schedule();
        },
        // Debounce icon lookup for the current search and source settings
        schedule() {
            if (this.saving) return;
            this.generation++;
            clearTimeout(this.timer);
            this.start = 0;
            if (this.opened) this.timer = setTimeout(() => void this.search(), 300);
        },
        // Move to the previous icon selection result page
        previous() {
            this.start = Math.max(0, this.start - 64);
            void this.search();
        },
        // Move to the next icon selection result page
        next() {
            this.start += 64;
            void this.search();
        },
        // Reset pagination and schedule a search for the current query
        search() {
            if (this.saving || !this.opened || windowWork.quiescing) return;
            return this.fetchIcons(false);
        },
        // Fetch the requested icon page for the current search generation
        async fetchIcons(forceOffline: boolean): Promise<void> {
            clearTimeout(this.timer);
            const generation = ++this.generation;
            const offline = this.useOffline || forceOffline;
            this.loading = true;
            this.error = '';
            this.choices = [];
            const selected = this.selectedAsset;
            this.assets.clear();
            if (selected) this.assets.set(selected.id, selected);
            try {
                const result = await app().bridge.command('icon_search', {
                    query: this.query,
                    prefix: this.prefix.trim().toLowerCase(),
                    start: this.start,
                    offline,
                });
                if (!this.opened || generation !== this.generation) return;
                if (result.offline && this.start > 0 && result.icons.length === 0) {
                    // The offline subset can be shorter after a network fallback
                    // or a library change. Retry its first page once
                    this.start = 0;
                    return this.fetchIcons(true);
                }
                // Both native and preview searches return the requested page
                const icons = result.icons;
                this.more = result.more;
                // The existing footer hides pagination only for a single offline page
                this.localOnly = result.offline && this.start === 0 && !result.more;
                this.message = result.message;
                for (const collection of result.collections) {
                    if (!this.collections.some((c) => c.prefix === collection.prefix))
                        this.collections.push(collection);
                }
                this.collections.sort((a, b) => a.name.localeCompare(b.name));
                if (
                    result.offline &&
                    this.selectedAsset &&
                    !icons.includes(this.selected) &&
                    this.query
                        .trim()
                        .toLowerCase()
                        .split(/\s+/)
                        .every((word) => this.selected.includes(word)) &&
                    (!this.prefix.trim() ||
                        this.selected.startsWith(this.prefix.trim().toLowerCase() + ':'))
                )
                    icons.unshift(this.selected);
                this.choices = icons.map((id) => ({
                    id,
                    name: id.split(':')[1],
                    image: '',
                    error: '',
                    license: '',
                }));
                let cursor = 0;
                await Promise.all(
                    // Load icon previews with bounded concurrency while the query remains current
                    Array.from({ length: 6 }, async () => {
                        while (
                            cursor < icons.length &&
                            this.opened &&
                            generation === this.generation
                        ) {
                            const index = cursor++;
                            const id = icons[index];
                            try {
                                const asset =
                                    this.assets.get(id) ||
                                    (await app().bridge.command('icon_load', {
                                        id,
                                        offline: offline || result.offline,
                                    }));
                                if (generation !== this.generation || !this.opened) return;
                                this.assets.set(id, asset);
                                this.choices[index] = {
                                    id,
                                    name: id.split(':')[1],
                                    asset,
                                    image: iconImage(asset),
                                    error: '',
                                    license: asset.license,
                                };
                            } catch (error) {
                                if (generation === this.generation && this.opened)
                                    this.choices[index].error = String(error);
                            }
                        }
                    }),
                );
                if (
                    this.opened &&
                    generation === this.generation &&
                    !offline &&
                    !result.offline &&
                    this.choices.length &&
                    !this.choices.some((c) => c.asset)
                ) {
                    const fallback = this.fetchIcons(true);
                    const fallbackGeneration = this.generation;
                    await fallback;
                    if (!this.opened || fallbackGeneration !== this.generation) return;
                    this.message =
                        'These icons could not be loaded. Showing saved and bundled offline icons. Try another search or refresh when connected.';
                }
            } catch (error) {
                if (generation === this.generation) this.error = String(error);
            } finally {
                if (generation === this.generation) this.loading = false;
            }
        },
        // Select the requested result for the current preview owner
        async choose(event: Event) {
            if (this.saving) return;
            const id = (event.currentTarget as HTMLElement).dataset.icon || '';
            const asset = this.assets.get(id);
            if (!asset) return;
            clearTimeout(this.timer);
            const generation = ++this.generation;
            const context = this.context;
            this.loading = false;
            this.saving = true;
            this.error = '';
            try {
                await app().bridge.command('icon_save', { asset });
                if (generation !== this.generation || !this.opened) return;
                window.dispatchEvent(
                    new CustomEvent('icon-chosen', { detail: { asset, context } }),
                );
                this.close();
            } catch (error) {
                if (generation === this.generation)
                    this.error = 'Could not save icon offline: ' + String(error);
            } finally {
                if (generation === this.generation) this.saving = false;
            }
        },
    }));
}
