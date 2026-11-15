import type { Snapshot, WorkspaceChanges } from '../generated/runtime';
/** Validate a complete batch before touching reactive state. Unchanged entities retain identity. */
export function workspaceSnapshot(current: Snapshot, changes: Partial<WorkspaceChanges>): Snapshot {
    // Apply a validated entity delta and optional order to the current workspace list
    function entities<T extends { id: string }>(
        values: T[],
        updates: T[],
        removed: string[],
        order: string[] | null | undefined,
    ): T[] {
        const map = new Map(values.map((value) => [value.id, value]));
        if (
            new Set(updates.map((value) => value.id)).size !== updates.length ||
            new Set(removed).size !== removed.length ||
            updates.some((value) => removed.includes(value.id))
        )
            throw Error('Invalid workspace entity batch');
        for (const id of removed) {
            if (!map.delete(id)) throw Error('Unknown removed workspace entity');
        }
        for (const value of updates) map.set(value.id, value);
        if (order) {
            if (
                new Set(order).size !== order.length ||
                order.length !== map.size ||
                order.some((id) => !map.has(id))
            )
                throw Error('Invalid workspace ordering');
            return order.map((id) => map.get(id)!);
        }
        return [...map.values()];
    }
    const next = { ...current };
    next.profiles = entities(
        current.profiles,
        changes.profiles || [],
        changes.removed_profiles || [],
        changes.profile_order,
    );
    next.saved_macros = entities(
        current.saved_macros,
        changes.macros || [],
        changes.removed_macros || [],
        changes.macro_order,
    );
    if (
        current.styles ||
        changes.styles?.length ||
        changes.removed_styles?.length ||
        changes.style_order
    )
        next.styles = entities(
            current.styles || [],
            changes.styles || [],
            changes.removed_styles || [],
            changes.style_order,
        );
    for (const key of [
        'devices',
        'plugins',
        'sessions',
        'rules',
        'history',
        'active_profile',
        'binding_overrides',
    ] as const) {
        const value = changes[key];
        if (value !== null && value !== undefined) Object.assign(next, { [key]: value });
    }
    return next;
}
/** Browser fixtures use the same wire contract; native code captures only touched entities. */
export function workspaceDifference(before: Snapshot, after: Snapshot): WorkspaceChanges {
    // Compare workspace values through their JSON representations
    const equal = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
    // Compute changed entities, removed IDs and any new ordering
    const diff = <T extends { id: string }>(old: T[], next: T[]) => ({
        updates: next.filter(
            (value) =>
                !equal(
                    old.find((v) => v.id === value.id),
                    value,
                ),
        ),
        removed: old.filter((value) => !next.some((v) => v.id === value.id)).map((v) => v.id),
        order: equal(
            old.map((v) => v.id),
            next.map((v) => v.id),
        )
            ? null
            : next.map((v) => v.id),
    });
    const p = diff(before.profiles, after.profiles),
        m = diff(before.saved_macros, after.saved_macros),
        s = diff(before.styles || [], after.styles || []);
    return {
        profiles: p.updates,
        removed_profiles: p.removed,
        profile_order: p.order,
        macros: m.updates,
        removed_macros: m.removed,
        macro_order: m.order,
        styles: s.updates,
        removed_styles: s.removed,
        style_order: s.order,
        devices: equal(before.devices, after.devices) ? null : after.devices,
        plugins: equal(before.plugins, after.plugins) ? null : after.plugins,
        sessions: equal(before.sessions, after.sessions) ? null : after.sessions,
        rules: equal(before.rules, after.rules) ? null : after.rules,
        history: equal(before.history, after.history) ? null : after.history,
        active_profile:
            before.active_profile === after.active_profile ? null : after.active_profile,
        binding_overrides: equal(before.binding_overrides, after.binding_overrides)
            ? null
            : after.binding_overrides || {},
    };
}
