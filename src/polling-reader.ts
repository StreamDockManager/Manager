const readers = new Set<{ pause(): void; resume(): void }>();
let paused = false;
// Pause the window readers
export function pauseWindowReaders() {
    if (paused) return;
    paused = true;
    for (const reader of readers) reader.pause();
}
// Resume the window readers
export function resumeWindowReaders() {
    if (!paused) return;
    paused = false;
    for (const reader of readers) reader.resume();
}

/** A window-owned read stream. Consumers keep their own editable drafts and job ownership. */
export class PollingReader<T> {
    private listeners = new Set<{ value: (value: T) => void; error: (error: unknown) => void }>();
    private pending: Promise<T> | undefined;
    private afterPending: Promise<T> | undefined;
    private timer: ReturnType<typeof setTimeout> | undefined;
    private current: T | undefined;
    private generation = 0;
    // Reset polling cadence and refresh immediately when visibility returns
    private visibilityChanged = () => {
        this.clearTimer();
        if (!this.visibility?.hidden) this.poll();
        else this.schedule();
    };

    // Store the read function and visibility source without starting polling before subscription
    constructor(
        private read: () => Promise<T>,
        private active: (value: T) => boolean,
        private visibility:
            | Pick<Document, 'hidden' | 'addEventListener' | 'removeEventListener'>
            | undefined = typeof document === 'undefined' ? undefined : document,
    ) {}

    // Start polling for the first subscriber and return a listener cleanup function
    subscribe(value: (value: T) => void, error: (error: unknown) => void = () => {}) {
        const listener = { value, error };
        this.listeners.add(listener);
        if (this.listeners.size === 1) {
            readers.add(this);
            this.visibility?.addEventListener('visibilitychange', this.visibilityChanged);
            this.poll();
        } else if (this.current !== undefined && !paused) value(this.current);
        // Remove this subscriber and retire polling when no listeners remain
        return () => {
            this.listeners.delete(listener);
            if (!this.listeners.size) {
                readers.delete(this);
                ++this.generation;
                this.clearTimer();
                this.visibility?.removeEventListener('visibilitychange', this.visibilityChanged);
            }
        };
    }

    // Refresh polling reader data while keeping the view synchronized
    refresh(fresh = false): Promise<T> {
        if (this.pending) {
            if (!fresh) return this.pending;
            if (!this.afterPending) {
                this.afterPending = this.pending
                    .catch(() => {})
                    // Process the settled polling reader operation result
                    .then(() => {
                        this.afterPending = undefined;
                        // This was explicitly requested by a mutation/cleanup caller
                        // It must complete even while passive delivery is paused
                        return this.refresh();
                    });
            }
            return this.afterPending;
        }
        this.clearTimer();
        const generation = this.generation;
        this.pending = this.read()
            .then(
                // Process the settled polling reader operation result
                (value) => {
                    if (generation === this.generation && !paused) {
                        this.current = value;
                        for (const listener of this.listeners) listener.value(value);
                    }
                    return value;
                },
                // Process the settled polling reader operation result
                (error) => {
                    if (generation === this.generation && !paused)
                        for (const listener of this.listeners) listener.error(error);
                    throw error;
                },
            )
            // Release transient polling reader state after the operation settles
            .finally(() => {
                this.pending = undefined;
                // A new subscriber may have joined while the previous window was torn down
                if (generation !== this.generation && !this.afterPending) this.poll();
                else this.schedule();
            });
        return this.pending;
    }

    // Clear the timer
    private clearTimer() {
        if (this.timer !== undefined) clearTimeout(this.timer);
        this.timer = undefined;
    }

    // Refresh only while polling is active and at least one subscriber remains
    private poll() {
        if (!paused && this.listeners.size) void this.refresh().catch(() => {});
    }

    // Pause the current polling reader operation
    pause() {
        ++this.generation;
        this.clearTimer();
    }
    // Resume the paused polling reader operation
    resume() {
        // A request started before resumption is not a fresh window snapshot
        ++this.generation;
        this.poll();
    }

    // Schedule the next polling reader update
    private schedule() {
        this.clearTimer();
        if (paused || !this.listeners.size || this.pending) return;
        const busy = this.current !== undefined && this.active(this.current);
        if (this.visibility?.hidden && !busy) return;
        this.timer = setTimeout(
            () => this.poll(),
            busy ? (this.visibility?.hidden ? 5_000 : 2_000) : 30_000,
        );
    }
}
