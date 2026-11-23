/** One decoder consumer, with interactive frames ahead of queued filmstrip work. */
export function videoDecodeQueue() {
    type Job = { background: boolean; run: () => Promise<void>; cancel: () => void };
    const jobs: Job[] = [];
    let running = false;
    const pump = async () => {
        if (running) return;
        const index = jobs.findIndex((job) => !job.background);
        const job = jobs.splice(index < 0 ? 0 : index, 1)[0];
        if (!job) return;
        running = true;
        try {
            await job.run();
        } finally {
            running = false;
            queueMicrotask(pump);
        }
    };
    return {
        run<T>(background: boolean, work: () => Promise<T>): Promise<T> {
            return new Promise((resolve, reject) => {
                jobs.push({
                    background,
                    run: async () => {
                        try {
                            resolve(await work());
                        } catch (error) {
                            reject(error);
                        }
                    },
                    cancel: () => reject(Error('Superseded filmstrip request')),
                });
                queueMicrotask(pump);
            });
        },
        cancelBackground() {
            for (let i = jobs.length - 1; i >= 0; i--)
                if (jobs[i].background) jobs.splice(i, 1)[0].cancel();
        },
    };
}

/** Encoded-only LRU: no cache-owned object URLs or decoded image surfaces. */
export function filmstripCache(maxEntries = 24, maxBytes = 8 * 1024 * 1024) {
    const entries = new Map<string, ArrayBuffer>();
    const pending = new Map<string, Promise<ArrayBuffer>>();
    let bytes = 0,
        epoch = 0;
    return {
        get size() {
            return entries.size;
        },
        get bytes() {
            return bytes;
        },
        clear() {
            epoch++;
            entries.clear();
            pending.clear();
            bytes = 0;
        },
        get(key: string, decode: () => Promise<ArrayBuffer>): Promise<ArrayBuffer> {
            const hit = entries.get(key);
            if (hit) {
                entries.delete(key);
                entries.set(key, hit);
                return Promise.resolve(hit);
            }
            const flight = pending.get(key);
            if (flight)
                return flight.catch((error) => {
                    // A newer UI window can join a queued job cancelled just before its
                    // rejection microtask. Retry that cancellation, not decoder errors.
                    if (error instanceof Error && error.message === 'Superseded filmstrip request')
                        return this.get(key, decode);
                    throw error;
                });
            const generation = epoch;
            const promise = decode()
                .then((value) => {
                    if (generation === epoch && value.byteLength <= maxBytes) {
                        entries.set(key, value);
                        bytes += value.byteLength;
                        while (entries.size > maxEntries || bytes > maxBytes) {
                            const first = entries.entries().next().value!;
                            bytes -= first[1].byteLength;
                            entries.delete(first[0]);
                        }
                    }
                    return value;
                })
                .finally(() => {
                    if (pending.get(key) === promise) pending.delete(key);
                });
            pending.set(key, promise);
            return promise;
        },
    };
}
