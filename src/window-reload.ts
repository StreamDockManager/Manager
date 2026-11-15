/** Match Vite's HTML reload routing before retiring this document. */
export function developmentReloadApplies(path: string | undefined, pathname: string, base: string) {
    if (!path?.endsWith('.html')) return true;
    const current = decodeURI(pathname);
    const target = base + path.slice(1);
    return (
        current === target ||
        path === '/index.html' ||
        (current.endsWith('/') && current + 'index.html' === target)
    );
}

/** Vite full reloads must retire native owners before WebKit tears down IPC. */
export async function prepareDevelopmentReload(hooks: {
    flush(): Promise<void>;
    pause(): void;
    release(): Promise<void>;
    dispose(): void;
    settle(): Promise<void>;
}): Promise<void> {
    // Never release an editor underneath a draft write that is still committing
    await hooks.flush();
    hooks.pause();
    const errors: unknown[] = [];
    try {
        await hooks.release();
    } catch (error) {
        errors.push(error);
    }
    // Unmounting can itself enqueue native releases. Dispose before draining them
    try {
        hooks.dispose();
    } catch (error) {
        errors.push(error);
    }
    try {
        await hooks.settle();
    } catch (error) {
        errors.push(error);
    }
    if (errors.length) throw new AggregateError(errors, 'Development reload cleanup failed');
}
