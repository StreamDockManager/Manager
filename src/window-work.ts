/** A main-window operation must own its complete async lifetime, including gaps between IPC. */
export class WindowQuiescingError extends Error {
    // Create the explicit error reported when new work arrives during editor shutdown
    constructor() {
        super('The editor is preparing to unload. Reopen it to continue.');
        this.name = 'WindowQuiescingError';
    }
}

export interface WindowFence {
    readonly generation: number;
    readonly requestId: number;
}

const cleanupCommands = new Set([
    'cancel_vendor_profile_review',
    'save_draft',
    'unsubscribe_workspace',
    'unsubscribe_surfaces',
    'unsubscribe_plugin_logs',
    'release_media',
    'release_archive_export',
    'discard_media_drop',
    'discard_archive_drop',
    'vault_cancel_entry_import',
    'finish_window_action',
    'finish_keyboard_preview',
    'abort_window_action',
]);

export class WindowWork {
    private operations = new Map<symbol, string>();
    private fence: WindowFence | undefined;
    private cleanupAdmission: WindowFence | undefined;
    private retirementFailures = new Set<string>();
    private fenceInvalidated = false;
    private idleWaiters = new Set<() => void>();

    // Derive pending from the current window work state
    get pending() {
        return this.operations.size;
    }
    // Derive quiescing from the current window work state
    get quiescing() {
        return this.fence !== undefined;
    }
    // Derive blocked retirements from the current window work state
    get blockedRetirements() {
        return [...this.retirementFailures];
    }
    // Derive owners from the current window work state
    get owners() {
        return [...this.operations.values()];
    }
    // Begin the window work operation
    begin(kind: string) {
        if (this.fence) {
            this.fenceInvalidated = true;
            throw new WindowQuiescingError();
        }
        return this.acquire(kind);
    }

    // Register operation ownership and return a release callback that wakes idle waiters
    private acquire(kind: string) {
        const id = Symbol(kind);
        this.operations.set(id, kind);
        // Complete the deferred window work work using the current operation state
        return () => {
            this.operations.delete(id);
            if (!this.pending) for (const ready of [...this.idleWaiters]) ready();
        };
    }

    // Run the requested window work operation
    async run<T>(kind: string, operation: () => Promise<T>): Promise<T> {
        const release = this.begin(kind);
        try {
            return await operation();
        } finally {
            release();
        }
    }

    /** Raw command ownership supplements, but does not replace, operation leases. */
    async command<T>(name: string, invoke: () => Promise<T>): Promise<T> {
        if (this.fence && (this.cleanupAdmission !== this.fence || !cleanupCommands.has(name))) {
            this.fenceInvalidated = true;
            throw new WindowQuiescingError();
        }
        const release = this.acquire('ipc:' + name);
        try {
            return await invoke();
        } finally {
            release();
        }
    }

    /** Close begins only after all operations and native retirements have settled. */
    tryCloseFence(generation: number, requestId: number): WindowFence | undefined {
        if (this.fence || this.pending || this.retirementFailures.size) return undefined;
        this.fenceInvalidated = false;
        this.fence = Object.freeze({ generation, requestId });
        return this.fence;
    }

    // Retire transient window work state after the operation settles
    async settled(timeoutMs = 5000): Promise<void> {
        if (!this.pending) return;
        // Resolve the window work operation when its asynchronous work finishes
        await new Promise<void>((resolve, reject) => {
            // Cancel the idle timeout and resolve the waiter after removing its registration
            const ready = () => {
                clearTimeout(timer);
                this.idleWaiters.delete(ready);
                resolve();
            };
            // Run the deferred window work update after its scheduled delay
            const timer = setTimeout(() => {
                this.idleWaiters.delete(ready);
                reject(Error('Editor operations are still finishing. Please try closing again.'));
            }, timeoutMs);
            this.idleWaiters.add(ready);
            if (!this.pending) ready();
        });
    }

    // Return the currently selected window work value
    current(fence: WindowFence) {
        return this.fence === fence;
    }

    /** The capability applies only to synchronous invocation, never across an await. */
    async cleanup<T>(fence: WindowFence, kind: string, start: () => Promise<T> | T): Promise<T> {
        if (!this.current(fence)) throw new WindowQuiescingError();
        const release = this.acquire('cleanup:' + kind);
        const previous = this.cleanupAdmission;
        let pending: Promise<T> | T;
        try {
            this.cleanupAdmission = fence;
            try {
                pending = start();
            } finally {
                this.cleanupAdmission = previous;
            }
            const value = await pending;
            if (!this.current(fence)) throw new WindowQuiescingError();
            return value;
        } finally {
            release();
        }
    }

    // Check whether commit is valid for the current window work state
    canCommit(fence: WindowFence) {
        return (
            this.current(fence) &&
            !this.fenceInvalidated &&
            !this.pending &&
            !this.retirementFailures.size
        );
    }

    // Resume the paused window work operation
    resume(fence?: WindowFence) {
        if (fence && !this.current(fence)) return false;
        this.fence = undefined;
        this.fenceInvalidated = false;
        return true;
    }

    // Mark the current unload fence stale without creating a new fence
    invalidate() {
        if (this.fence) this.fenceInvalidated = true;
    }

    // Retain each failed retirement category so unacknowledged cleanup vetoes unload
    retirementFailed(kind: string) {
        // Keep one reason per owner category; an unacknowledged release must veto unload
        this.retirementFailures.add(kind);
    }
}

export const windowWork = new WindowWork();

/** Only callable properties can be registered; each lifecycle retains its own fence. */
export type OperationName<T> = {
    [K in keyof T]-?: T[K] extends (...args: never[]) => unknown ? K : never;
}[keyof T] &
    string;

/** Explicit async-operation inventory; synchronous rendering helpers remain unwrapped. */
export function ownWindowMethods<T extends object>(
    target: T,
    owner: string,
    names: readonly OperationName<T>[],
    work = windowWork,
): T {
    const methods = target as Record<string, unknown>;
    for (const name of names) {
        const original = methods[name];
        if (typeof original !== 'function')
            throw Error(`Missing window operation ${owner}.${name}`);
        // Complete the deferred window work work using the current operation state
        methods[name] = function (this: T, ...args: unknown[]) {
            return work.run(`${owner}.${name}`, () => Promise.resolve(original.apply(this, args)));
        };
    }
    return target;
}
