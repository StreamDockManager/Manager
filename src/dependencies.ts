import type { Snapshot } from './types';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
export interface Reference {
    source: string;
    target: string;
    profile?: string;
    page?: string;
    control?: string;
    path: string;
}
export interface Finding {
    code: string;
    message: string;
    profile?: string;
    page?: string;
    control?: string;
}
export class ReferenceIndex {
    readonly revision: number;
    readonly edges: Reference[] = [];
    readonly findings: Finding[] = [];
    private outgoing = new Map<string, Reference[]>();
    private incoming = new Map<string, Reference[]>();
    // Build a dependency index from the current workspace snapshot
    constructor(snapshot: Snapshot) {
        this.revision = snapshot.configuration_revision;
        // Add a discovered reference to both dependency directions
        const edge = (reference: Reference) => {
            this.edges.push(reference);
            this.outgoing.set(reference.source, [
                ...(this.outgoing.get(reference.source) || []),
                reference,
            ]);
            this.incoming.set(reference.target, [
                ...(this.incoming.get(reference.target) || []),
                reference,
            ]);
        };
        // Scan saved values for entity references in their source location
        const scan = (value: unknown, location: Omit<Reference, 'target'>) => {
            if (typeof value === 'string') {
                for (const match of value.matchAll(/\$\{parameter\.([A-Za-z0-9_]+)\}/g))
                    edge({
                        ...location,
                        target: `parameter:${location.profile || '*'}:${match[1]}`,
                    });
            }
            if (Array.isArray(value)) {
                value.forEach((v, i) => scan(v, { ...location, path: location.path + '/' + i }));
                return;
            }
            if (value && typeof value === 'object') {
                for (const [key, v] of Object.entries(value)) {
                    const prefix: Record<string, string> = {
                        profile_id: 'profile',
                        page_id: 'page',
                        plugin_id: 'plugin',
                        macro_id: 'macro',
                        asset: 'media',
                        background_asset: 'media',
                        $parameter: `parameter:${location.profile || '*'}`,
                    };
                    if (prefix[key] && typeof v === 'string' && v)
                        edge({
                            ...location,
                            target: prefix[key] + ':' + v,
                            path: location.path + '/' + key,
                        });
                    scan(v, { ...location, path: location.path + '/' + key });
                }
            }
        };
        // Inspect each profile’s pages and bindings for dependencies
        snapshot.profiles.forEach((profile) => {
            // Inspect the bindings and navigation within one profile page
            profile.pages.forEach((page) => {
                edge({
                    source: 'profile:' + profile.id,
                    target: 'page:' + page.id,
                    profile: profile.id,
                    page: page.id,
                    path: 'pages',
                });
                for (const [control, binding] of Object.entries(page.bindings)) {
                    if (!binding) continue;
                    const source = `control:${profile.id}:${page.id}:${control}`;
                    edge({
                        source: 'page:' + page.id,
                        target: source,
                        profile: profile.id,
                        page: page.id,
                        control,
                        path: 'bindings',
                    });
                    scan(binding, {
                        source,
                        profile: profile.id,
                        page: page.id,
                        control,
                        path: 'binding',
                    });
                }
            });
            for (const parameter of profile.parameters || [])
                edge({
                    source: 'profile:' + profile.id,
                    target: `parameter:${profile.id}:${parameter.name}`,
                    profile: profile.id,
                    path: 'parameters',
                });
            if (profile.background_asset)
                edge({
                    source: 'profile:' + profile.id,
                    target: 'media:' + profile.background_asset,
                    profile: profile.id,
                    path: 'background_asset',
                });
        });
        snapshot.saved_macros.forEach((macro) =>
            scan(macro.action, { source: 'macro:' + macro.id, path: 'action' }),
        );
        snapshot.rules.forEach((rule) => scan(rule, { source: 'rule:' + rule.id, path: 'rule' }));
        // Recognize navigation that cannot be resolved statically
        const navigationUncertain = (value: unknown) =>
            /swipe_navigation|page_step|page_back|momentary_page|binding_layer|trigger_binding|"kind":"profile"/.test(
                JSON.stringify(value),
            );
        const macroActions = new Map(
            snapshot.saved_macros.map((macro) => ['macro:' + macro.id, macro.action]),
        );
        const uncertainProfiles = new Set<string>();
        // Bind portable macro references to the calling profile and page. Page
        // navigation through a macro participates in the same reachability graph
        for (const call of [...this.edges].filter(
            (e) => e.profile && e.target.startsWith('macro:'),
        )) {
            const visited = new Set<string>(),
                pending = [call.target];
            while (pending.length) {
                const macro = pending.pop()!;
                if (visited.has(macro)) continue;
                visited.add(macro);
                if (navigationUncertain(macroActions.get(macro)))
                    uncertainProfiles.add(call.profile!);
                for (const reference of this.references(macro, 'outgoing')) {
                    if (
                        reference.target.startsWith('parameter:*:') ||
                        reference.target.startsWith('page:')
                    )
                        edge({
                            ...call,
                            target: reference.target.replace(
                                'parameter:*:',
                                `parameter:${call.profile}:`,
                            ),
                            path: call.path + '/' + reference.path,
                        });
                    if (reference.target.startsWith('macro:')) pending.push(reference.target);
                }
            }
        }
        // Only identical or unconditional earlier rules prove shadowing
        snapshot.rules.forEach((rule, i) => {
            const shadow = snapshot.rules
                .slice(0, i)
                .find(
                    (earlier) =>
                        earlier.device_key === rule.device_key &&
                        earlier.application === rule.application &&
                        (!earlier.title ||
                            JSON.stringify(earlier.title) === JSON.stringify(rule.title)) &&
                        (!earlier.schedule ||
                            JSON.stringify(earlier.schedule) === JSON.stringify(rule.schedule)),
                );
            if (shadow)
                this.findings.push({
                    code: 'shadowed_rule',
                    message: `Rule ${rule.id} is shadowed by ${shadow.id}`,
                    profile: rule.profile_id,
                    page: rule.page_id || undefined,
                });
        });
        for (const profile of snapshot.profiles) {
            const reached = new Set([profile.active_page]);
            for (const rule of snapshot.rules)
                if (rule.profile_id === profile.id && rule.page_id) reached.add(rule.page_id);
            let changed = true;
            while (changed) {
                changed = false;
                for (const page of profile.pages)
                    if (reached.has(page.id)) {
                        const references = this.edges.filter(
                            (e) =>
                                e.profile === profile.id &&
                                e.page === page.id &&
                                e.target.startsWith('page:'),
                        );
                        for (const reference of references) {
                            const id = reference.target.slice(5);
                            if (!reached.has(id)) {
                                reached.add(id);
                                changed = true;
                            }
                        }
                    }
            }
            // Swipe, page-step, menu, and external profile navigation make reachability uncertain
            const uncertain = navigationUncertain(profile) || uncertainProfiles.has(profile.id);
            if (!uncertain) {
                for (const page of profile.pages)
                    if (!reached.has(page.id))
                        this.findings.push({
                            code: 'unreachable_page',
                            message: `Page “${page.name}” has no path from this profile’s initial page`,
                            profile: profile.id,
                            page: page.id,
                        });
                const graph = new Map(
                    profile.pages.map((page) => [
                        page.id,
                        new Set(
                            this.edges
                                .filter(
                                    (e) =>
                                        e.source.startsWith('control:') &&
                                        e.profile === profile.id &&
                                        e.page === page.id &&
                                        e.target.startsWith('page:'),
                                )
                                .map((e) => e.target.slice(5)),
                        ),
                    ]),
                );
                // Find entities reachable from a dependency graph node
                const reach = (start: string) => {
                    const seen = new Set([start]),
                        queue = [start];
                    while (queue.length)
                        for (const next of graph.get(queue.pop()!) || [])
                            if (!seen.has(next)) {
                                seen.add(next);
                                queue.push(next);
                            }
                    return seen;
                };
                const reported = new Set<string>();
                for (const page of profile.pages) {
                    const outbound = reach(page.id);
                    const cycle = [...outbound].filter((id) => reach(id).has(page.id));
                    if (
                        cycle.length > 1 &&
                        !cycle.includes(profile.active_page) &&
                        !cycle.some((id) =>
                            [...(graph.get(id) || [])].some((next) => !cycle.includes(next)),
                        ) &&
                        !reported.has(page.id)
                    ) {
                        cycle.forEach((id) => reported.add(id));
                        this.findings.push({
                            code: 'closed_navigation',
                            message: `Navigation cycle has no explicit exit: ${cycle.map((id) => profile.pages.find((p) => p.id === id)?.name || id).join(', ')}`,
                            profile: profile.id,
                            page: page.id,
                        });
                    }
                }
            }
            for (const page of profile.pages) {
                const shortcuts = new Map<string, string>();
                for (const [control, binding] of Object.entries(page.bindings)) {
                    if (!binding) continue;
                    if (binding.disabled || binding.action.kind !== 'hotkey') continue;
                    const chord = binding.action.keys
                        .map((key) => key.toLowerCase())
                        .sort()
                        .join('+');
                    const previous = shortcuts.get(chord);
                    if (previous)
                        this.findings.push({
                            code: 'repeated_shortcut',
                            message: `${previous} and ${control} emit the same shortcut (${chord}); review whether this is intentional`,
                            profile: profile.id,
                            page: page.id,
                            control,
                        });
                    else shortcuts.set(chord, control);
                }
            }
        }
    }
    // Return incoming or outgoing references for an entity
    references(entity: string, direction: 'incoming' | 'outgoing') {
        return (direction === 'incoming' ? this.incoming : this.outgoing).get(entity) || [];
    }
    // Collect entities affected by a set of dependency changes
    affected(entities: Iterable<string>) {
        const seen = new Set(entities);
        const pending = [...seen];
        while (pending.length) {
            for (const reference of this.references(pending.pop()!, 'incoming'))
                if (!seen.has(reference.source)) {
                    seen.add(reference.source);
                    pending.push(reference.source);
                }
        }
        return seen;
    }
    // Check whether the index still describes the supplied snapshot
    isCurrent(snapshot: Snapshot) {
        return snapshot.configuration_revision === this.revision;
    }
}
// Register dependency inspection and navigation controls
export function registerDependencies(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('dependencies', () => ({
        query: '',
        direction: 'incoming' as 'incoming' | 'outgoing',
        entity: '',
        index: null as ReferenceIndex | null,
        // Reuse the index only while it matches the current snapshot
        get current() {
            if (!this.index?.isCurrent(app().snapshot))
                this.index = new ReferenceIndex(app().snapshot);
            return this.index;
        },
        // Expose references for the selected dependency target
        get references() {
            return (
                this.entity
                    ? this.current.references(this.entity, this.direction)
                    : this.current.edges
            ).filter((e) =>
                [e.source, e.target].join(' ').toLowerCase().includes(this.query.toLowerCase()),
            );
        },
        // Expose dependency findings from the current index
        get findings() {
            return this.current.findings;
        },
        // Navigate to the source of the selected dependency reference
        async navigate(event: Event) {
            const el = event.currentTarget as HTMLElement;
            await app().flushDrafts?.();
            if (el.dataset.profile)
                app().apply(
                    await app().command('activate_profile', {
                        id: el.dataset.profile,
                        deviceId: app().deviceId,
                    }),
                );
            if (el.dataset.page)
                app().apply(
                    await app().command('select_page', {
                        pageId: el.dataset.page,
                        deviceId: app().deviceId,
                    }),
                );
            if (el.dataset.control) app().controlId = el.dataset.control;
            location.hash = '#/editor';
        },
    }));
}
