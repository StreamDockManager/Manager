/** Two active thumbnail jobs, including jobs finishing from a replaced page. */
export function viewportThumbnails<T>(options: {
    run: (entry: T) => Promise<void>;
    busy: (busy: boolean) => void;
    allowed: () => boolean;
}) {
    let observer: IntersectionObserver | undefined;
    let queue = new Map<Element, T>();
    let visible = new Set<Element>();
    let active = 0;
    let generation = 0;
    // Start visible thumbnail jobs while respecting permission and the two-job concurrency limit
    const pump = () => {
        if (!options.allowed()) return;
        for (const element of visible) {
            if (active >= 2) break;
            const entry = queue.get(element);
            if (!entry) continue;
            queue.delete(element);
            observer?.unobserve(element);
            visible.delete(element);
            active++;
            options.busy(true);
            void options
                .run(entry)
                .catch(() => {})
                // Release transient viewport thumbnails state after the operation settles
                .finally(() => {
                    active--;
                    options.busy(active > 0);
                    pump();
                });
        }
    };
    // Stop the current viewport thumbnails operation
    const stop = () => {
        generation++;
        observer?.disconnect();
        observer = undefined;
        queue.clear();
        visible.clear();
        options.busy(active > 0);
    };
    return {
        stop,
        pump,
        // Replace thumbnail observation and reject callbacks from earlier observation generations
        observe(entries: Array<[Element, T]>, root: Element) {
            stop();
            queue = new Map(entries);
            if (typeof IntersectionObserver === 'undefined') {
                visible = new Set(queue.keys());
                pump();
                return;
            }
            const revision = generation;
            observer = new IntersectionObserver(
                // Complete the deferred viewport thumbnails work using the current operation state
                (changes) => {
                    if (revision !== generation) return;
                    for (const change of changes) {
                        if (change.isIntersecting) visible.add(change.target);
                        else visible.delete(change.target);
                    }
                    pump();
                },
                { root, rootMargin: '96px 0px' },
            );
            for (const element of queue.keys()) observer.observe(element);
        },
    };
}
