import type {
    HistoryComparison,
    HistoryControlAddress,
    HistoryFieldChange,
    HistoryItem,
    Snapshot,
} from '../generated/runtime';
// In-memory browser fixtures only. Native comparisons use integrity-checked Rust history
type Content = Pick<Snapshot, 'active_profile' | 'profiles' | 'saved_macros' | 'rules' | 'styles'>;
type Entity = {
    kind: string;
    label: string;
    value: unknown;
    opaque: unknown[];
    control: HistoryControlAddress | null;
};
// Replace opaque plugin settings with a redacted comparison marker
function portable(value: unknown, opaque: unknown[]): unknown {
    if (Array.isArray(value)) return value.map((item) => portable(item, opaque));
    if (!value || typeof value !== 'object') return value;
    const object = value as Record<string, unknown>;
    if (object.kind === 'plugin') {
        opaque.push(object.settings);
        return { ...object, settings: 'Plugin settings omitted from comparison' };
    }
    return Object.fromEntries(
        Object.entries(object).map(([key, value]) => [key, portable(value, opaque)]),
    );
}
// Index profile entities for identity-based history comparison
function entities(data: Content) {
    const result = new Map<string, Entity>();
    // Add an entity and its source location to the comparison index
    const add = (
        id: string,
        kind: string,
        label: string,
        value: unknown,
        control: HistoryControlAddress | null = null,
    ) => {
        const opaque: unknown[] = [];
        result.set(id, { kind, label, value: portable(value, opaque), opaque, control });
    };
    add('workspace', 'workspace', 'Workspace', {
        default_profile: data.active_profile,
        profile_order: data.profiles.map((p) => p.id),
        macro_order: data.saved_macros.map((m) => m.id),
    });
    for (const profile of data.profiles) {
        const { pages, ...metadata } = profile;
        add(`profile:${profile.id}`, 'profile', profile.name, {
            ...metadata,
            page_order: pages.map((p) => p.id),
        });
        for (const page of pages) {
            add(
                `page:${JSON.stringify([profile.id, page.id])}`,
                'page',
                `${profile.name} / ${page.name}`,
                { name: page.name },
            );
            for (const [control, binding] of Object.entries(page.bindings)) {
                add(
                    `control:${JSON.stringify([profile.id, page.id, control])}`,
                    'control',
                    `${profile.name} / ${page.name} / ${control}`,
                    binding,
                    { profile_id: profile.id, page_id: page.id, control_id: control },
                );
            }
        }
    }
    for (const macro of data.saved_macros) add(`macro:${macro.id}`, 'macro', macro.name, macro);
    for (const rule of data.rules) add(`rule:${rule.id}`, 'rule', `Rule ${rule.id}`, rule);
    for (const style of data.styles || []) add(`style:${style.id}`, 'style', style.name, style);
    return result;
}
// Normalize nested values for deterministic history comparison
const canonical = (value: unknown): unknown =>
    Array.isArray(value)
        ? value.map(canonical)
        : value && typeof value === 'object'
          ? Object.fromEntries(
                Object.keys(value)
                    .sort()
                    .map((key) => [key, canonical((value as Record<string, unknown>)[key])]),
            )
          : value;
// Compare canonical values without depending on object property order
const same = (a: unknown, b: unknown) =>
    JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));
// Produce a bounded display snippet for a changed history value
const snippet = (value: unknown) =>
    value === undefined
        ? null
        : (typeof value === 'string' ? value : JSON.stringify(value)).slice(0, 2048);
// Build an in-memory history comparison for browser fixtures
export function compareHistoryFixture(
    item: HistoryItem,
    old: Content,
    next: Content,
): HistoryComparison {
    const before = entities(old),
        after = entities(next);
    const changes: HistoryComparison['changes'] = [];
    let truncated = false;
    let budget = 512 * 1024;
    for (const id of new Set([...before.keys(), ...after.keys()])) {
        const a = before.get(id),
            b = after.get(id);
        if (same(a?.value, b?.value) && same(a?.opaque, b?.opaque)) continue;
        if (changes.length >= 250) {
            truncated = true;
            break;
        }
        const fields: HistoryFieldChange[] = [];
        // Record a changed history field with its before and after values
        const diff = (a: unknown, b: unknown, path = '', depth = 0) => {
            if (same(a, b)) return;
            if (fields.length >= 100 || budget <= 0) {
                truncated = true;
                return;
            }
            // Compare the properties of a history object
            const object = (value: unknown) =>
                value !== null && typeof value === 'object' && !Array.isArray(value);
            const sameContainer =
                ((a === undefined || object(a)) && (b === undefined || object(b))) ||
                ((a === undefined || Array.isArray(a)) && (b === undefined || Array.isArray(b)));
            if (depth < 12 && sameContainer) {
                const keys = new Set([...Object.keys(a || {}), ...Object.keys(b || {})]);
                for (const key of keys)
                    diff(
                        (a as Record<string, unknown> | undefined)?.[key],
                        (b as Record<string, unknown> | undefined)?.[key],
                        path
                            ? `${path} / ${Array.isArray(a) || Array.isArray(b) ? Number(key) + 1 : key}`
                            : key,
                        depth + 1,
                    );
            } else {
                const field = {
                    field: path.replaceAll('_', ' '),
                    before: snippet(a),
                    after: snippet(b),
                };
                budget -=
                    field.field.length + (field.before?.length || 0) + (field.after?.length || 0);
                if (budget >= 0) fields.push(field);
                else truncated = true;
            }
        };
        diff(a?.value, b?.value);
        if (!same(a?.opaque, b?.opaque) && (a?.opaque.length || b?.opaque.length))
            fields.push({
                field: 'Action / plugin settings',
                before: a ? 'Saved values omitted' : null,
                after: b ? 'Changed values omitted' : null,
            });
        const entity = b || a!;
        changes.push({
            id,
            kind: entity.kind,
            label: entity.label,
            operation: !a ? 'added' : !b ? 'removed' : 'changed',
            fields,
            control: entity.control,
        });
    }
    return { item, changes, truncated };
}
