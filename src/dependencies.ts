import { actionChildren } from './builtin-actions';
import type { Action, Snapshot } from './types';
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
export interface NavigationEdge {
    profile: string;
    page: string;
    control: string;
    path: string;
    destination: string | null;
    targetProfile?: string;
    conditional: boolean;
}

function navigationOrigins(snapshot: Snapshot) {
    const macros = new Map(snapshot.saved_macros.map((m) => [m.id, m]));
    const edges: NavigationEdge[] = [];
    for (const profile of snapshot.profiles)
        for (const page of profile.pages) {
            for (const [control, binding] of Object.entries(page.bindings)) {
                if (!binding || binding.disabled) continue;
                let visited = 0;
                const walk = (
                    action: Action,
                    path: string[],
                    conditional: boolean,
                    calls: string[],
                ) => {
                    const record = (
                        destination: string | null,
                        detail?: string,
                        targetProfile?: string,
                    ) =>
                        edges.push({
                            profile: profile.id,
                            page: page.id,
                            control,
                            path: [...path, ...(detail ? [detail] : [])].join(' / '),
                            conditional,
                            destination,
                            targetProfile,
                        });
                    if (++visited > 2048) {
                        if (visited === 2049) record(null, 'Expansion limit');
                        return;
                    }
                    if (action.kind === 'page')
                        record(
                            profile.pages.some((p) => p.id === action.page_id)
                                ? action.page_id
                                : null,
                        );
                    else if (action.kind === 'profile')
                        record(null, 'Remembered page', action.profile_id);
                    else if (action.kind === 'navigate') {
                        const offset = action.direction === 'next' ? 1 : -1;
                        if (action.target === 'page') {
                            const pages = profile.pages;
                            record(
                                pages[(pages.indexOf(page) + pages.length + offset) % pages.length]
                                    .id,
                                action.direction + ' page',
                            );
                        } else {
                            const profiles = snapshot.profiles.filter(
                                (p) => p.model_id === profile.model_id,
                            );
                            const next =
                                profiles[
                                    (profiles.indexOf(profile) + profiles.length + offset) %
                                        profiles.length
                                ];
                            record(next.active_page, action.direction + ' profile', next.id);
                        }
                    } else if (action.kind === 'swipe_navigation') record(null, 'Swipe direction');
                    else if (action.kind === 'builtin') {
                        const a = action.action;
                        if (a.kind === 'momentary_page') record(a.page_id, 'While held');
                        if (
                            ['page_step', 'page_back', 'binding_layer', 'trigger_binding'].includes(
                                a.kind,
                            )
                        )
                            record(null, a.kind);
                        if (a.kind === 'saved_macro') {
                            const macro = macros.get(a.macro_id);
                            if (macro && !calls.includes(macro.id))
                                walk(macro.action, [...path, macro.name], conditional, [
                                    ...calls,
                                    macro.id,
                                ]);
                            else record(null, 'Unresolved macro');
                        }
                    }
                    const kind = action.kind === 'builtin' ? action.action.kind : action.kind;
                    const branches: Record<string, string[]> = {
                        gesture: ['Tap', 'Double tap', 'Hold'],
                        press_release: ['Press', 'Release'],
                        dial: ['Counterclockwise', 'Clockwise', 'Press'],
                        toggle: ['On', 'Off'],
                        conditional: ['If true', 'If false'],
                    };
                    actionChildren(action).forEach((child, i) =>
                        walk(
                            child,
                            [...path, branches[kind]?.[i] || 'Step ' + (i + 1)],
                            conditional ||
                                !!branches[kind] ||
                                [
                                    'action_menu',
                                    'random_action',
                                    'cycle_actions',
                                    'cooldown',
                                    'failure_policy',
                                ].includes(kind),
                            calls,
                        ),
                    );
                };
                walk(binding.action, [], false, []);
            }
        }
    return edges;
}

function sessionEntries(snapshot: Snapshot) {
    return JSON.stringify(
        Object.values(snapshot.sessions).map((s) => [
            s?.profile_id,
            s?.page_id,
            s?.remembered_pages,
        ]),
    );
}
export class ReferenceIndex {
    readonly revision: number;
    readonly navigation: NavigationEdge[];
    private readonly entries: string;
    readonly edges: Reference[] = [];
    readonly findings: Finding[] = [];
    private outgoing = new Map<string, Reference[]>();
    private incoming = new Map<string, Reference[]>();
    // Build a dependency index from the current workspace snapshot
    constructor(snapshot: Snapshot) {
        this.revision = snapshot.configuration_revision;
        this.navigation = navigationOrigins(snapshot);
        this.entries = sessionEntries(snapshot);
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
            for (const session of Object.values(snapshot.sessions)) {
                if (session?.profile_id === profile.id) reached.add(session.page_id);
                const remembered = session?.remembered_pages?.[profile.id];
                if (remembered) reached.add(remembered);
            }
            const edges = this.navigation.filter((e) => e.profile === profile.id);
            const graph = new Map(profile.pages.map((p) => [p.id, new Set<string>()]));
            for (const edge of edges)
                if (edge.destination && (!edge.targetProfile || edge.targetProfile === profile.id))
                    graph.get(edge.page)?.add(edge.destination);
            const pending = [...reached];
            while (pending.length) {
                for (const id of graph.get(pending.pop()!) || [])
                    if (!reached.has(id)) {
                        reached.add(id);
                        pending.push(id);
                    }
            }
            const uncertain =
                edges.some((e) => !e.destination) ||
                this.navigation.some((e) => e.targetProfile === profile.id);
            if (!uncertain) {
                for (const page of profile.pages)
                    if (!reached.has(page.id))
                        this.findings.push({
                            code: 'unreachable_page',
                            message:
                                'Page “' +
                                page.name +
                                '” has no known navigation path from an entry page',
                            profile: profile.id,
                            page: page.id,
                        });
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
                        (cycle.length > 1 || graph.get(page.id)?.has(page.id)) &&
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
        return (
            snapshot.configuration_revision === this.revision &&
            this.entries === sessionEntries(snapshot)
        );
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
