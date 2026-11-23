/** Coalesce only ephemeral preview projections, never saves, undo or recovery writes.
 * Owners cancel before loading another context and on destruction. The final callback
 * reads the latest complete draft; its existing parser keeps incomplete JSON isolated.
 */
export function editorPreviewWork() {
    let generation = 0;
    let pending: (() => void) | undefined;
    return {
        schedule(work: () => void) {
            const queued = Boolean(pending);
            pending = work;
            if (queued) return;
            const ticket = generation;
            queueMicrotask(() => {
                if (ticket !== generation) return;
                const run = pending;
                pending = undefined;
                run?.();
            });
        },
        cancel() {
            generation++;
            pending = undefined;
        },
    };
}
