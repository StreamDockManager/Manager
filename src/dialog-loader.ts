import { componentReady, RetryableTasks } from './route-loading';

let mount: ((key: string) => Promise<void>) | undefined;

export function ensureDialog(key: string) {
    if (!mount) return Promise.reject(Error('The dialog loader is not ready.'));
    return mount(key);
}

export function installDialogLoader(
    load: (key: string) => Promise<void>,
    run: (work: () => Promise<void>) => Promise<void>,
    signal: AbortSignal,
) {
    const mounted = new Set<string>();
    const mounts = new RetryableTasks();
    const loader = (key: string) =>
        mounts.run(key, async () => {
            await load(key);
            if (signal.aborted) throw Error('Window closed.');
            const outlet = document.getElementById('optional-dialogs');
            if (!outlet) throw Error('Dialog outlet unavailable.');
            const element = document.createElement('sd-' + key);
            const ready = componentReady(element, signal);
            outlet.append(element);
            try {
                await ready;
                mounted.add(key);
            } catch (error) {
                element.remove();
                throw error;
            }
        });
    mount = loader;
    signal.addEventListener(
        'abort',
        () => {
            if (mount === loader) mount = undefined;
        },
        { once: true },
    );
    const events: Record<string, string> = {
        'open-video-editor': 'video-editor',
        'open-background-editor': 'background-editor',
        'profile-versions': 'profile-versions',
        'convert-profile': 'profile-conversion',
        'open-macro-export': 'macro-sharing',
        'open-macro-import': 'macro-sharing',
        'open-export-review': 'export-review',
        'open-profile-export': 'profile-sharing-dialog',
        'open-plugin-management': 'plugin-management',
        'open-media-library': 'media-library',
        'open-library-organization': 'library-organization',
        'open-device-output': 'device-output-dialog',
        'open-plugin-catalog-dialog': 'plugin-catalog-dialog',
        'open-runtime': 'runtime-dialog',
        'open-runtime-removal': 'runtime-dialog',
        'open-application-data': 'application-data',
        'open-plugin-import': 'plugin-folder-import',
        'open-plugin-preflight': 'plugin-preflight',
    };
    const pending = new Set<string>();
    for (const [name, key] of Object.entries(events)) {
        window.addEventListener(
            name,
            (event) => {
                if (mounted.has(key)) return;
                event.stopImmediatePropagation();
                if (pending.has(name)) return;
                pending.add(name);
                const target = event.target;
                const hash = location.hash;
                void run(async () => {
                    await ensureDialog(key);
                    if (
                        signal.aborted ||
                        hash !== location.hash ||
                        (target instanceof Node && !target.isConnected)
                    )
                        return;
                    target?.dispatchEvent(
                        new CustomEvent(name, {
                            detail: (event as CustomEvent).detail,
                            bubbles: event.bubbles,
                            composed: event.composed,
                        }),
                    );
                }).finally(() => pending.delete(name));
            },
            { capture: true, signal },
        );
    }
}
