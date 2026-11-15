import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { LibraryIdentity, LibraryMark, LibraryOrganization } from '../generated/runtime';
import { ownWindowMethods } from './window-work';
import { registerCloseDraft } from './close-releases';
// Encode a stable identity for a library organization entry
export function libraryKey(identity: LibraryIdentity) {
    return identity.kind === 'plugin'
        ? JSON.stringify(['plugin', identity.plugin_id, identity.action_id])
        : JSON.stringify([identity.kind, identity.id]);
}
// Summarize favorite status and visible library tags
export function librarySummary(favorite: boolean, tags: string[]) {
    return [
        favorite ? 'Favorite' : '',
        ...tags.slice(0, 2),
        ...(tags.length > 2 ? ['+' + (tags.length - 2) + ' tags'] : []),
    ]
        .filter(Boolean)
        .join(' · ');
}
export class LibraryOrganizationStore {
    data: LibraryOrganization = { revision: 0, entries: [] };
    index: Record<string, LibraryMark> = {};
    loaded = false;
    loading: Promise<void> | null = null;
    // Report whether library organization data is currently loading
    get busy() {
        return this.loading !== null;
    }
    error = '';
    // Retain access to the current application state for organization requests
    constructor(public app: () => AppState) {}
    // Apply a nonstale organization snapshot and rebuild its identity index
    apply(data: LibraryOrganization) {
        if (this.loaded && data.revision < this.data.revision) return;
        this.data = data;
        this.index = Object.fromEntries(
            data.entries.map((entry) => [libraryKey(entry.identity), entry]),
        );
        this.loaded = true;
        this.error = '';
    }
    // Read favorite status for the requested library entry
    favorite(identity: LibraryIdentity) {
        return this.index[libraryKey(identity)]?.favorite || false;
    }
    // Read the tags attached to the requested library entry
    tags(identity: LibraryIdentity) {
        return this.index[libraryKey(identity)]?.tags || [];
    }
    // Load library organization state while sharing concurrent requests
    async load(force = false) {
        if (this.loading) return this.loading;
        if (this.loaded && !force) return;
        const revision = this.data.revision;
        this.loading = this.app()
            .bridge.command('library_organization')
            .then((data) => this.apply(data))
            // Do not replace newer organization data with an older request failure
            .catch((error) => {
                if (this.data.revision !== revision) return;
                this.loaded = false;
                this.error = String(error);
            })
            .finally(() => {
                this.loading = null;
            });
        return this.loading;
    }
    // Save the requested favorite and tags against the expected organization revision
    async save(revision: number, mark: LibraryMark) {
        this.apply(
            await this.app().bridge.command('save_library_mark', {
                expectedRevision: revision,
                mark: {
                    identity: { ...mark.identity },
                    favorite: mark.favorite,
                    tags: [...mark.tags],
                },
            }),
        );
    }
}
// Resolve the shared Alpine library organization store
export const getLibraryOrganization = (Alpine: typeof AlpineType) =>
    Alpine.store('libraryOrganization') as LibraryOrganizationStore;
// Request tag and favorite editing for a library entry
export function openLibraryOrganization(identity: LibraryIdentity, name: string) {
    window.dispatchEvent(
        new CustomEvent('open-library-organization', { detail: { identity, name } }),
    );
}
// Register the shared organization store and entry editor
export function registerLibraryOrganization(Alpine: typeof AlpineType, app: () => AppState) {
    if (!Alpine.store('libraryOrganization'))
        Alpine.store('libraryOrganization', new LibraryOrganizationStore(app));
    Alpine.data('libraryMarkEditor', () => ({
        identity: null as LibraryIdentity | null,
        name: '',
        favorite: false,
        tagText: '',
        revision: 0,
        working: false,
        saving: false,
        error: '',
        epoch: 0,
        baseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Capture the organization editor draft for unsaved-change detection
        closeValue() {
            return JSON.stringify([this.favorite, this.tagText]);
        },
        // Attach library organization listeners and window-close coordination
        init() {
            ownWindowMethods(this, 'library organization', ['open', 'reload', 'save']);
            this.unregisterCloseOwner = registerCloseDraft(this, ['save'], {
                name: 'Favorites and tags',
                retainOnUnmount: true,
                // Report unsaved library organization work to window-close coordination
                state: () =>
                    this.identity && this.baseline && this.closeValue() !== this.baseline
                        ? 'dirty'
                        : 'clean',
                // Discard the current library organization draft and release temporary state
                discard: () => this.close(),
            });
        },
        // Resolve the shared library organization store
        get organization() {
            return getLibraryOrganization(Alpine);
        },
        // Open library organization for the requested item
        async open(event: CustomEvent<{ identity: LibraryIdentity; name: string }>) {
            if (this.saving) return;
            const epoch = ++this.epoch;
            this.identity = event.detail.identity;
            this.name = event.detail.name;
            this.favorite = false;
            this.tagText = '';
            this.baseline = '';
            this.working = true;
            this.error = '';
            const dialog = this.$refs.dialog as HTMLDialogElement;
            if (!dialog.open) dialog.showModal();
            await this.organization.load();
            if (epoch !== this.epoch) return;
            if (!this.organization.loaded) this.error = this.organization.error;
            this.favorite = this.organization.favorite(this.identity);
            this.tagText = this.organization.tags(this.identity).join(', ');
            this.revision = this.organization.data.revision;
            this.working = false;
            this.baseline = this.closeValue();
        },
        // Close library organization and invalidate stale asynchronous work
        close() {
            if (this.saving) return;
            ++this.epoch;
            (this.$refs.dialog as HTMLDialogElement).close();
            this.identity = null;
            this.working = false;
        },
        // Reload organization metadata without replacing a newer editor generation
        async reload() {
            if (!this.identity) return;
            const epoch = ++this.epoch;
            this.working = true;
            await this.organization.load(true);
            if (epoch !== this.epoch || !this.identity) return;
            this.working = false;
            if (this.organization.loaded) {
                this.favorite = this.organization.favorite(this.identity);
                this.tagText = this.organization.tags(this.identity).join(', ');
                this.baseline = this.closeValue();
                this.revision = this.organization.data.revision;
                this.error = '';
            } else this.error = this.organization.error;
        },
        // Save the requested favorite and tags against the expected organization revision
        async save() {
            if (!this.identity || this.working || !this.organization.loaded) return;
            this.working = true;
            this.error = '';
            this.saving = true;
            try {
                await this.organization.save(this.revision, {
                    identity: this.identity,
                    favorite: this.favorite,
                    tags: this.tagText.split(','),
                });
                this.working = false;
                this.saving = false;
                this.close();
                app().notify('Personal organization saved', 'success');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.saving = false;
                this.working = false;
            }
        },
        // Detach library organization listeners and release owned state
        destroy() {
            this.unregisterCloseOwner?.();
            ++this.epoch;
            (this.$refs.dialog as HTMLDialogElement)?.close();
        },
    }));
}
