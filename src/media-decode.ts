/** One shared budget for explicit browser decodes. Running decodes cannot be cancelled. */
export type MediaDecodePriority = 'preview' | 'thumbnail';
type Batch = {
    urls: string[];
    next: number;
    active: number;
    current: () => boolean;
    decode: (url: string) => Promise<void>;
    priority: MediaDecodePriority;
    failed: boolean;
    error?: unknown;
    resolve: (current: boolean) => void;
    reject: (error: unknown) => void;
};

// Create the shared bounded queue for preview and thumbnail image decoding
export function createMediaDecodeQueue(concurrency = 2) {
    if (!Number.isInteger(concurrency) || concurrency < 1) throw Error('Invalid decode budget');
    const batches: Batch[] = [];
    let active = 0;
    // Admit queued decodes by priority and settle finished or obsolete batches
    const pump = () => {
        // Drop queued stale work, but let already-running work settle before its owner releases URLs
        for (const batch of [...batches]) {
            const current = batch.current();
            if (!current || batch.failed) batch.next = batch.urls.length;
            if (batch.next === batch.urls.length && batch.active === 0) {
                batches.splice(batches.indexOf(batch), 1);
                if (batch.failed) batch.reject(batch.error);
                else batch.resolve(current);
            }
        }
        while (active < concurrency) {
            // Check whether the decode batch still has images waiting to start
            const available = (batch: Batch) => batch.next < batch.urls.length;
            const batch =
                batches.find((item) => available(item) && item.priority === 'preview') ??
                batches.find(available);
            if (!batch) break;
            const url = batch.urls[batch.next++];
            active++;
            batch.active++;
            // Invoke immediately, preserving callers' existing synchronous decode-start behavior
            let result: Promise<void>;
            try {
                result = batch.decode(url);
            } catch (error) {
                result = Promise.reject(error);
            }
            void result
                .then(
                    // Leave successful completion to the shared finalization path
                    () => {},
                    // Retain the decode failure so the batch rejects after active work settles
                    (error) => {
                        batch.failed = true;
                        batch.error = error;
                    },
                )
                // Release completed work accounting and allow the next queued operation
                .finally(() => {
                    active--;
                    batch.active--;
                    pump();
                });
        }
    };
    return (
        urls: string[],
        current: () => boolean,
        decode: (url: string) => Promise<void>,
        priority: MediaDecodePriority = 'preview',
    ): Promise<boolean> =>
        // Track the queued media decode request until it resolves or rejects
        new Promise((resolve, reject) => {
            batches.push({
                urls: [...new Set(urls)],
                next: 0,
                active: 0,
                current,
                decode,
                priority,
                failed: false,
                resolve,
                reject,
            });
            pump();
        });
}

const queue = createMediaDecodeQueue();
// Queue explicit browser image decodes under the shared concurrency budget
export function decodeMediaImages(
    urls: string[],
    current: () => boolean,
    // Decode the image before making the preview available
    decode: (url: string) => Promise<void> = async (url) => {
        const image = new Image();
        image.decoding = 'async';
        image.src = url;
        await image.decode();
    },
    priority: MediaDecodePriority = 'preview',
) {
    return queue(urls, current, decode, priority);
}
