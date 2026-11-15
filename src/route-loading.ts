/** Cache successful initialization, share pending work, and allow failed work to retry. */
export class RetryableTasks {
    private tasks = new Map<string, Promise<void>>();
    // Share in-flight initialization and evict rejected work so later attempts can retry
    run(key: string, initialize: () => Promise<void>): Promise<void> {
        const previous = this.tasks.get(key);
        if (previous) return previous;
        const pending = Promise.resolve()
            .then(initialize)
            // Handle a failed loading operation without retaining stale work
            .catch((error) => {
                if (this.tasks.get(key) === pending) this.tasks.delete(key);
                throw error;
            });
        this.tasks.set(key, pending);
        return pending;
    }
}

// Wait for the component and nested ACL content while supporting cancellation and timeout
export function componentReady(element: Element, signal?: AbortSignal) {
    // Resolve the loading operation when its asynchronous work finishes
    return new Promise<void>((resolve, reject) => {
        // Remove load listeners and cancel the timeout before settling component readiness
        const cleanup = () => {
            clearTimeout(timeout);
            element.removeEventListener('loaded', loaded);
            element.removeEventListener('acl:error', failed);
            signal?.removeEventListener('abort', aborted);
        };
        let ownLoaded = false;
        // Wait for the component and its nested content before reporting route readiness
        const loaded = (event: Event) => {
            if (event.target === element) ownLoaded = true;
            // ACL reports parent readiness before nested components finish. Keep local
            // error ownership until the entire eagerly mounted subtree is ready
            if (!ownLoaded || element.querySelector?.('[data-acl-component][aria-busy="true"]'))
                return;
            cleanup();
            resolve();
        };
        // Contain a route load failure without discarding the surrounding shell or drafts
        const failed = (event?: Event) => {
            // Route failures are handled locally; the outer boundary must keep the shell and drafts
            event?.stopPropagation();
            cleanup();
            reject(Error('Component failed to load. Please retry.'));
        };
        // Stop waiting and reject readiness when route loading is cancelled
        const aborted = () => {
            cleanup();
            reject(Error('Component loading was cancelled.'));
        };
        const timeout = setTimeout(failed, 30_000);
        element.addEventListener('loaded', loaded);
        element.addEventListener('acl:error', failed);
        signal?.addEventListener('abort', aborted, { once: true });
        if (signal?.aborted) aborted();
    });
}
