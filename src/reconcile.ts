type RecordValue = Record<string, unknown>;

// Record the reconcile operation
function record(value: unknown): value is RecordValue {
    return value !== null && typeof value === 'object' && !Array.isArray(value);
}

// Reconcile one own property while preserving reusable nested state where safe
function field(target: RecordValue, key: string, value: unknown): boolean {
    const previous = Object.hasOwn(target, key) ? target[key] : undefined;
    if (Object.hasOwn(target, key) && Object.is(previous, value)) return false;
    if (Array.isArray(previous) && Array.isArray(value)) return reconcile(previous, value);
    // Treat a JSON object with a prototype-named own property as an atomic value
    if (record(previous) && record(value) && !Object.hasOwn(value, '__proto__'))
        return reconcile(previous, value);
    if (key === '__proto__')
        Object.defineProperty(target, key, {
            value,
            enumerable: true,
            configurable: true,
            writable: true,
        });
    else target[key] = value;
    return true;
}

/** Apply complete JSON snapshots without notifying observers of unchanged fields. */
export function reconcile<T extends object>(
    target: T,
    source: T,
    onChange?: (key: string) => void,
): boolean {
    if (target === source) return false;
    let changed = false;
    if (Array.isArray(target) && Array.isArray(source)) {
        // Check whether id is valid for the current reconcile state
        const hasId = (value: unknown) => record(value) && typeof value.id === 'string';
        const keyed = target.every(hasId) && source.every(hasId);
        const byId = keyed ? new Map(target.map((value) => [value.id, value])) : undefined;
        const unique =
            byId?.size === target.length &&
            new Set(source.map((value) => value.id)).size === source.length;
        for (let index = 0; index < source.length; index++) {
            const incoming = source[index];
            const previous = unique ? byId!.get(incoming.id) : target[index];
            if (record(previous) && record(incoming) && !Object.hasOwn(incoming, '__proto__')) {
                if (reconcile(previous, incoming)) changed = true;
                if (target[index] !== previous) {
                    target[index] = previous;
                    changed = true;
                }
            } else if (Array.isArray(previous) && Array.isArray(incoming)) {
                if (reconcile(previous, incoming)) changed = true;
            } else if (index >= target.length || !Object.is(previous, incoming)) {
                target[index] = incoming;
                changed = true;
            }
        }
        if (target.length !== source.length) {
            target.length = source.length;
            changed = true;
        }
        return changed;
    }
    const current = target as RecordValue;
    const incoming = source as RecordValue;
    for (const key of Object.keys(current)) {
        if (!Object.hasOwn(incoming, key)) {
            delete current[key];
            changed = true;
            onChange?.(key);
        }
    }
    for (const key of Object.keys(incoming)) {
        if (field(current, key, incoming[key])) {
            changed = true;
            onChange?.(key);
        }
    }
    return changed;
}
