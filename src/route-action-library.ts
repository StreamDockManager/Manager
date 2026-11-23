import { loadPluginAction } from './plugin-choice-behavior';
import { keyLogicPluginReason } from './key-logic';
import Alpine from '@alpinejs/csp';
import { builtinCatalog, type BuiltinDescriptor } from './builtin-actions';
import { controlGroups } from './plugin-actions';
import {
    getLibraryOrganization,
    openLibraryOrganization,
    librarySummary,
    libraryKey,
} from './library-organization';
import { paginate, adjacentPage } from './pagination';
import { app, swallow } from './route-context';
import { ownWindowMethods, windowWork } from './window-work';
import type { LibraryIdentity } from '../generated/runtime';
import { actionLibrarySelection, type LibrarySelection } from './action-library-selection';

interface LibraryAction {
    key: string;
    id: string;
    name: string;
    description: string;
    category: string;
    source: string;
    sourceId: string;
    icon: string;
    themedIcon: boolean;
    keywords: string;
    identity: LibraryIdentity;
    builtin?: BuiltinDescriptor;
    pluginId?: string;
    controlTypes?: string[];
    organizable?: boolean;
}
// Fingerprint the current device, profile, page and control selection
const selectionContext = () =>
    JSON.stringify([app().deviceId, app().profile?.id, app().page?.id, app().controlId]);

// Register the action library handler with its owner
export function register() {
    const filters = Alpine.reactive({
        favoritesOnly: false,
        search: '',
        category: 'all',
        source: 'all',
        page: 1,
        pageSize: '12',
    });
    Alpine.data('actions', () => ({
        // Derive selection from the current action library state
        get selection() {
            return actionLibrarySelection;
        },
        // Derive filters from the current action library state
        get filters() {
            return this.selection.choosing ? this.selection.filters : filters;
        },
        // Cancel the selection
        cancelSelection() {
            this.selection.cancel();
        },
        loadingKey: '',
        selectionGeneration: 0,
        // Derive selection summary from the current action library state
        get selectionSummary() {
            if (this.selection.choosing)
                return this.selection.valid
                    ? `Choose an action to return to ${this.selection.request!.label}. Your unsaved changes are kept.`
                    : 'The editing target changed. Return to your editor to continue.';
            const control = app().control;
            return control
                ? `Choose an action to configure ${control.label} on ${app().device?.name}.`
                : 'Browse all actions. Select a control in Device editor to configure one.';
        },
        // Derive organization from the current action library state
        get organization() {
            return getLibraryOrganization(Alpine);
        },
        // Check whether the action is marked as a library favorite
        itemFavorite(item: LibraryAction) {
            return this.organization.favorite(item.identity);
        },
        // Summarize the action's favorite status and organization tags
        itemTags(item: LibraryAction) {
            return librarySummary(this.itemFavorite(item), this.organization.tags(item.identity));
        },
        // Open organization settings for the action identified by the clicked element
        organize(event: Event) {
            const item = this.items.find(
                (item) => item.key === (event.currentTarget as HTMLElement).dataset.key,
            );
            if (item) openLibraryOrganization(item.identity, item.name);
        },
        // Derive sources from the current action library state
        get sources() {
            return [
                { id: 'builtin', name: 'Built-in' },
                ...app().snapshot.plugins.map((plugin) => ({
                    id: 'plugin:' + plugin.id,
                    name: plugin.name,
                })),
            ];
        },
        // Match the action against the selected source filter
        matchesSource(item: LibraryAction) {
            return (
                this.filters.source === 'all' ||
                this.filters.source === item.sourceId ||
                (this.filters.source === 'plugins' && Boolean(item.pluginId))
            );
        },
        // Derive categories from the current action library state
        get categories() {
            return [
                ...new Set(
                    this.items
                        .filter((item) => this.matchesSource(item))
                        .map((item) => item.category),
                ),
            ];
        },
        // These route-owned values are replaced by dependency-tracked derivations.
        // Alpine tracks fields inside reconciled objects, not just array identity/length.
        items: [] as LibraryAction[],
        searchIndex: [] as { item: LibraryAction; text: string; favorite: boolean }[],
        results: { filtered: [] as LibraryAction[] },
        releaseDerivations: [] as (() => void)[],
        // Project only when fields actually consumed by the catalog change.
        projectItems(): LibraryAction[] {
            const builtins: LibraryAction[] = builtinCatalog
                .filter((item) => item.id !== 'swipe_navigation')
                // Convert each entry into the representation required by action library
                .map((item) => {
                    const identity: LibraryIdentity = { kind: 'builtin', id: item.id };
                    return {
                        ...item,
                        key: libraryKey(identity),
                        identity,
                        source: 'Built-in',
                        sourceId: 'builtin',
                        icon: import.meta.env.BASE_URL + 'appearance/icons/' + item.icon + '.svg',
                        themedIcon: true,
                        builtin: item,
                    };
                });
            const plugins: LibraryAction[] = app().snapshot.plugins.flatMap((plugin) =>
                plugin.actions
                    .filter(
                        (action) =>
                            action.visible_in_actions_list !== false &&
                            !(
                                this.selection.choosing &&
                                this.selection.request?.multiAction &&
                                !this.selection.request?.keyLogic &&
                                action.supported_in_multi_actions === false
                            ),
                    )
                    // Convert each entry into the representation required by action library
                    .map((action) => {
                        const identity: LibraryIdentity = {
                            kind: 'plugin',
                            plugin_id: plugin.id,
                            action_id: action.id,
                        };
                        const types = [...(action.control_types ?? ['button'])];
                        const controls = controlGroups
                            .filter((group) => types.includes(group.id))
                            .map((group) => group.label)
                            .join(' · ');
                        return {
                            key: libraryKey(identity),
                            identity,
                            id: action.id,
                            name: action.name,
                            description: controls || 'No supported control type declared',
                            category: plugin.name,
                            source: plugin.name,
                            sourceId: 'plugin:' + plugin.id,
                            pluginId: plugin.id,
                            icon:
                                action.icon ||
                                plugin.icon ||
                                import.meta.env.BASE_URL + 'appearance/icons/settings.svg',
                            themedIcon: !action.icon && !plugin.icon,
                            keywords: [
                                plugin.id,
                                plugin.name,
                                action.id,
                                plugin.kind,
                                plugin.dialect,
                            ].join(' '),
                            controlTypes: types,
                        };
                    }),
            );
            const special: LibraryAction[] = this.selection.choosing
                ? [
                      {
                          id: 'none',
                          name: 'No action',
                          description: 'Leave this control or step unassigned.',
                      },
                      ...(this.selection.request?.advanced
                          ? [
                                {
                                    id: 'custom',
                                    name: 'Advanced action JSON',
                                    description: 'Edit the action payload as JSON.',
                                },
                            ]
                          : []),
                  ].map((item) => ({
                      ...item,
                      key: 'selection:' + item.id,
                      category: 'General',
                      source: 'Built-in',
                      sourceId: 'builtin',
                      icon: import.meta.env.BASE_URL + 'appearance/icons/settings.svg',
                      themedIcon: true,
                      keywords: 'clear empty custom advanced json',
                      identity: { kind: 'builtin', id: item.id },
                      organizable: false,
                  }))
                : [];
            return [...builtins, ...plugins, ...special];
        },
        get filtered() {
            return this.results.filtered;
        },
        actionPage: paginate<LibraryAction>([], 1, 12),
        updatePage() {
            this.actionPage = paginate(
                this.results.filtered,
                this.filters.page,
                Number(this.filters.pageSize),
            );
        },
        deriveResults() {
            const terms = this.filters.search
                .trim()
                .toLocaleLowerCase()
                .split(/\s+/)
                .filter(Boolean);
            const filtered = this.searchIndex
                .filter(
                    ({ item, text, favorite }) =>
                        this.matchesSource(item) &&
                        (this.filters.category === 'all' ||
                            this.filters.category === item.category) &&
                        (!this.filters.favoritesOnly || favorite) &&
                        terms.every((term) => text.includes(term)),
                )
                .map((entry) => entry.item);
            return {
                filtered,
            };
        },
        // Initialize action library state and its required lifecycle hooks
        init() {
            const derive = (work: () => void) => {
                let active = true;
                const effect = Alpine.effect(() => {
                    if (active) work();
                });
                this.releaseDerivations.push(() => {
                    active = false;
                    Alpine.release(effect);
                });
            };
            derive(() => {
                this.items = this.projectItems();
            });
            derive(() => {
                this.searchIndex = this.items.map((item) => ({
                    item,
                    favorite: this.organization.favorite(item.identity),
                    text: `${item.name} ${item.description} ${item.category} ${item.source} ${item.keywords} ${this.organization.tags(item.identity).join(' ')}`.toLocaleLowerCase(),
                }));
            });
            derive(() => this.updatePage());
            let resultQueued = false,
                active = true;
            this.releaseDerivations.push(() => {
                active = false;
            });
            derive(() => {
                // Alpine's scheduler can visit a downstream effect before a catalog
                // projection queued in the same turn. Publish once after that batch,
                // not a stale intermediate result which the scheduler deduplicates.
                const f = this.filters;
                void [this.searchIndex, f.search, f.source, f.category, f.favoritesOnly];
                if (resultQueued) return;
                resultQueued = true;
                queueMicrotask(() => {
                    resultQueued = false;
                    if (active) this.results = this.deriveResults();
                });
            });
            ownWindowMethods(this, 'action library', ['pick']);
            swallow(windowWork.run('action library startup', () => this.organization.load()));
            for (const key of ['favoritesOnly', 'search', 'category', 'pageSize'])
                this.$watch('filters.' + key, () => {
                    this.filters.page = 1;
                });
            // Complete the deferred action library work using the current operation state
            this.$watch('filters.source', () => {
                this.filters.page = 1;
                this.filters.category = 'all';
            });
        },
        // Release action library state and associated lifecycle resources
        destroy() {
            this.releaseDerivations.splice(0).forEach((release) => release());
            this.items = [];
            this.searchIndex = [];
            this.results = { filtered: [] };
            this.selectionGeneration++;
        },
        // Change the action page
        changeActionPage(event: Event) {
            this.filters.page = adjacentPage(
                this.actionPage,
                Number((event.currentTarget as HTMLElement).dataset.direction),
            );
        },
        // Explain why an action cannot be used with the current editing target
        unavailable(item: LibraryAction) {
            if (this.selection.choosing && !this.selection.valid)
                return 'The editing target changed';
            const standalone = this.selection.choosing && this.selection.request?.standalone;
            if (!standalone && !app().control) return 'Select a control to configure this action';
            if (item.organizable === false) return '';
            if (item.builtin) {
                if (
                    item.builtin.release &&
                    this.selection.choosing &&
                    this.selection.request?.independentEvent
                )
                    return 'Requires a continuous physical press and release, not independent events';
                if (
                    item.builtin.release &&
                    !standalone &&
                    !app().control?.semantics.includes('release')
                )
                    return 'Requires a button with press and release events';
                return app().builtinCapabilities[item.builtin.capability] || '';
            }
            const plugin = app().snapshot.plugins.find((plugin) => plugin.id === item.pluginId);
            if (!plugin?.enabled) return 'Enable this plugin in Plugins to use its actions';
            // Recheck the current catalog at use time, including the interval before
            // scheduled reactive derivations publish a newly reconciled catalog.
            const current = plugin.actions.find((action) => action.id === item.id);
            if (!current || current.visible_in_actions_list === false)
                return 'This action is no longer available';
            if (
                this.selection.choosing &&
                this.selection.request?.multiAction &&
                !this.selection.request?.keyLogic &&
                current.supported_in_multi_actions === false
            )
                return 'This action is not supported in multi-actions';
            if (this.selection.choosing && this.selection.request?.keyLogic) {
                const reason = keyLogicPluginReason(plugin, item.id);
                if (reason) return reason;
            }
            if (!standalone && !(current.control_types ?? ['button']).includes(app().control!.kind))
                return `Select a matching control: ${item.description}`;
            return '';
        },
        // Validate the selected action and editing target before applying the choice
        async pick(event: Event) {
            const item = this.items.find(
                (item) => item.key === (event.currentTarget as HTMLElement).dataset.key,
            );
            if (!item || this.loadingKey || (this.selection.request && !this.selection.choosing))
                return;
            if (this.selection.choosing && !this.selection.valid) return;
            if (!this.selection.request?.standalone && !app().control) {
                app().notify('Select a control first', 'warning');
                return;
            }
            const reason = this.unavailable(item);
            if (reason) {
                app().notify(reason, 'warning');
                if (item.pluginId) return;
            }
            if (!item.pluginId) {
                window.dispatchEvent(
                    new CustomEvent('configure-library-action', {
                        detail: { kind: item.id } satisfies LibrarySelection,
                    }),
                );
                return;
            }
            const generation = ++this.selectionGeneration;
            const context = selectionContext();
            const selectionId = this.selection.choosing ? this.selection.request!.id : null;
            this.loadingKey = item.key;
            try {
                const action = await loadPluginAction(
                    app(),
                    item.pluginId,
                    item.id,
                    () =>
                        !(
                            generation !== this.selectionGeneration ||
                            selectionId !==
                                (this.selection.choosing ? this.selection.request?.id : null) ||
                            (!this.selection.request?.standalone &&
                                context !== selectionContext()) ||
                            this.unavailable(item) ||
                            !this.items.some((current) => current.key === item.key)
                        ),
                );
                if (!action) return;
                window.dispatchEvent(
                    new CustomEvent('configure-library-action', {
                        detail: {
                            action,
                            name: item.name,
                        } satisfies LibrarySelection,
                    }),
                );
            } catch {
            } finally {
                if (generation === this.selectionGeneration) this.loadingKey = '';
            }
        },
    }));
}
