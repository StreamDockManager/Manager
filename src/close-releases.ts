import type { OperationName } from './window-work';
import { windowCloseOwners } from './window-close-owners';
import type { WindowCloseOwner } from './window-close-owners';

/** Do not discard values underneath an already-committing user operation */
export function trackCloseOperations<T extends object>(
    target: T,
    methods: readonly OperationName<T>[],
) {
    const pending = new Set<Promise<unknown>>();
    for (const name of methods) {
        const original = target[name];
        if (typeof original !== 'function') throw Error(`Missing draft operation ${name}`);
        Object.defineProperty(target, name, {
            configurable: true,
            writable: true,
            enumerable: true,
            // Track asynchronous releases started by the wrapped owner method
            value: function (this: T, ...args: unknown[]) {
                const task = Promise.resolve(original.apply(this, args));
                pending.add(task);
                // Observe release cleanup failures after removing the pending task
                void task.finally(() => pending.delete(task)).catch(() => {});
                return task;
            },
        });
    }
    // Restore the original method and wait for tracked releases
    return async () => {
        while (pending.size) await Promise.allSettled([...pending]);
    };
}

// Register a draft owner whose cleanup may outlive dismissal
export function registerCloseDraft<T extends object>(
    target: T,
    methods: readonly OperationName<T>[],
    owner: WindowCloseOwner,
) {
    const settle = trackCloseOperations(target, methods);
    return windowCloseOwners.register({
        ...owner,
        // Discard the draft while retaining its cleanup promise
        discard: async () => {
            await settle();
            await owner.discard();
        },
        // Release the owner while retaining its cleanup promise
        release: async () => {
            await settle();
            await owner.release?.();
        },
    });
}

// A view may unmount before native cancellation is acknowledged. Keep those
// releases owned by the document, including failures that close must retry
const releases = new Map<string, { run: () => Promise<unknown>; pending?: Promise<void> }>();
// Wait until all tracked close releases have settled
export async function drainCloseReleases() {
    while (releases.size) {
        await Promise.all([...releases].map(([id, entry]) => trackCloseRelease(id, entry.run)));
    }
}

windowCloseOwners.register({
    name: 'Pending native releases',
    // Treat the cleanup-only owner as having no unsaved draft
    state: () => 'clean',
    // Leave draft discard empty for a cleanup-only owner
    discard: () => {},
    release: drainCloseReleases,
});

// Track one keyed release until its promise settles
export function trackCloseRelease(key: string, run: () => Promise<unknown>): Promise<void> {
    const current = releases.get(key);
    if (current?.pending) return current.pending;
    const entry = current || { run };
    releases.set(key, entry);
    let operation: Promise<unknown>;
    try {
        operation = entry.run();
    } catch (error) {
        operation = Promise.reject(error);
    }
    const pending = Promise.resolve(operation)
        // Resolve successful release work without exposing its return value
        .then(() => {
            if (releases.get(key) === entry) releases.delete(key);
        })
        // Remove the release from the pending registry when settled
        .finally(() => {
            if (entry.pending === pending) entry.pending = undefined;
        });
    entry.pending = pending;
    return pending;
}
