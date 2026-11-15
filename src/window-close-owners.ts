export type WindowCloseState = 'clean' | 'dirty' | 'unknown';

export interface WindowCloseOwner {
    name: string;
    state(): WindowCloseState;
    discard(): void | Promise<void>;
    release?(): void | Promise<void>;
    /** Retain unfinished route forms after their component is removed. */
    retainOnUnmount?: boolean;
}

interface OwnerEntry {
    owner: WindowCloseOwner;
    retained?: WindowCloseState;
    detached: boolean;
}

/** User-editable state is separate from window work permits and presentation blockers. */
export class WindowCloseOwners {
    private entries = new Map<symbol, OwnerEntry>();
    private isClosing = false;

    // Derive closing from the current window close owners state
    get closing() {
        return this.isClosing;
    }

    // Set the closing
    setClosing(value: boolean) {
        this.isClosing = value;
    }

    // Return a snapshot of the current window close owners state
    private state(entry: OwnerEntry): WindowCloseState {
        if (entry.retained) return entry.retained;
        try {
            return entry.owner.state();
        } catch {
            // An unavailable inspector is not evidence that its changes are saved
            return 'unknown';
        }
    }

    // Register the window close owners handler with its owner
    register(owner: WindowCloseOwner) {
        const id = Symbol(owner.name);
        const entry: OwnerEntry = { owner, detached: false };
        this.entries.set(id, entry);
        // Complete the deferred window close owners work using the current operation state
        return () => {
            if (entry.detached) return;
            entry.detached = true;
            const state = this.state(entry);
            if (owner.retainOnUnmount && state !== 'clean') entry.retained = state;
            else this.entries.delete(id);
        };
    }

    // Return a detached snapshot of window close owners state
    snapshot() {
        return [...this.entries.values()].map((entry) => ({
            name: entry.owner.name,
            state: this.state(entry),
        }));
    }

    // Derive dirty from the current window close owners state
    get dirty() {
        return this.snapshot().some(({ state }) => state !== 'clean');
    }

    // Discard dirty owners in reverse registration order and collect retirement failures
    async discardAll() {
        const errors: unknown[] = [];
        // Nested editors usually register after their parent. Discard them first
        for (const entry of [...this.entries.values()].reverse()) {
            if (this.state(entry) === 'clean') continue;
            try {
                await entry.owner.discard();
                entry.retained = undefined;
            } catch (error) {
                errors.push(new Error(`${entry.owner.name}: ${String(error)}`, { cause: error }));
            }
        }
        if (errors.length)
            throw new AggregateError(
                errors,
                'Could not discard all unsaved changes. ' + errors.map(String).join('; '),
            );
    }

    // Release the all
    async releaseAll() {
        const errors: unknown[] = [];
        for (const [id, entry] of [...this.entries].reverse()) {
            try {
                await entry.owner.release?.();
                if (entry.detached) this.entries.delete(id);
            } catch (error) {
                errors.push(new Error(`${entry.owner.name}: ${String(error)}`, { cause: error }));
            }
        }
        if (errors.length)
            throw new AggregateError(
                errors,
                'Could not release all editor resources. ' + errors.map(String).join('; '),
            );
    }
}

export const windowCloseOwners = new WindowCloseOwners();
