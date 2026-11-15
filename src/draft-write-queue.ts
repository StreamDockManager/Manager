/** Serial persistence with one replaceable, unsent snapshot per context and barrier */
export class DraftWriteQueue<T, Context = undefined> {
    value: Promise<void> = Promise.resolve();
    readonly pending = new Map<string, T>();
    private unsent = new Map<string, { draft: T; context?: Context }>();

    // Initialize ordered draft persistence with admission tracking
    constructor(
        private write: (key: string, draft: T | null, context?: Context) => Promise<unknown>,
        private failed: (error: unknown) => void,
        // Use a no-op admission release when no tracker is supplied
        private admit: (context?: Context) => () => void = () => () => {},
    ) {}

    // Persist or delete one keyed draft through the supplied writer
    private persist(key: string, draft: T | null, context?: Context) {
        return context === undefined ? this.write(key, draft) : this.write(key, draft, context);
    }

    // Queue the latest draft value behind pending writes
    enqueue(key: string, draft: T, context?: Context) {
        const release = this.admit(context);
        this.pending.set(key, draft);
        const previous = this.unsent.get(key);
        if (previous) {
            previous.draft = draft;
            previous.context = context;
            release();
            return this.value;
        }
        const slot = { draft, context };
        this.unsent.set(key, slot);
        this.value = this.value
            // Allow later writes to proceed after an earlier write failed
            .catch(() => {})
            // Persist the queued draft while retaining its admission lifetime
            .then(async () => {
                if (this.unsent.get(key) === slot) this.unsent.delete(key);
                const snapshot = slot.draft;
                try {
                    await this.persist(key, snapshot, slot.context);
                    if (this.pending.get(key) === snapshot) this.pending.delete(key);
                } catch (error) {
                    this.failed(error);
                } finally {
                    release();
                }
            });
        return this.value;
    }

    // Serialize an operation after pending draft writes
    barrier(operation: () => Promise<void>, context?: Context) {
        const release = this.admit(context);
        // Later edits must not replace a snapshot ordered before a clear/save
        this.unsent.clear();
        const completion = this.value
            // Allow a barrier to proceed after an earlier write failed
            .catch(() => {})
            .then(operation)
            .finally(release);
        // Keep the queue usable after a barrier failure
        this.value = completion.catch(() => {});
        return completion;
    }

    // Remove a keyed draft after earlier writes finish
    clear(key: string, context?: Context) {
        const previous = this.pending.get(key);
        // Delete the draft within the ordered persistence barrier
        return this.barrier(async () => {
            await this.persist(key, null, context);
            if (this.pending.get(key) === previous) this.pending.delete(key);
        }, context);
    }

    // Flush pending draft writes before continuing
    async flush(context?: Context) {
        const release = this.admit(context);
        try {
            await this.value;
            for (const [key, draft] of this.pending) this.enqueue(key, draft, context);
            await this.value;
            if (this.pending.size)
                throw Error('Draft writes remain unsaved; retry before closing.');
        } finally {
            release();
        }
    }
}
