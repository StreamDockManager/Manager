// Only requests a native chooser. No JS-supplied path is ever granted
(() => {
    if (window !== window.top) return;
    const descriptor = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
    let pending;
    Object.defineProperty(window, '__streamDockFileResult', {
        // Return the current vendor file input portable value
        value(id, paths) {
            if (!pending || pending.id !== id) return false;
            const input = pending.input;
            pending = undefined;
            if (!input.isConnected || input.disabled || input.type !== 'file' || !paths.length)
                return false;
            let selected = `C:\\fakepath\\${encodeURIComponent(paths[0])}`;
            Object.defineProperty(input, 'value', {
                configurable: true,
                // Read the requested vendor file input portable value
                get: () => selected,
                // Replace the requested vendor file input portable value
                set(next) {
                    descriptor.set.call(this, next);
                    selected = descriptor.get.call(this);
                },
            });
            input.dispatchEvent(new Event('input', { bubbles: true }));
            input.dispatchEvent(new Event('change', { bubbles: true }));
            return true;
        },
    });
    document.addEventListener(
        'click',
        // Respond to click events in the vendor file input portable workflow
        (event) => {
            const input = event
                .composedPath()
                .find((node) => node instanceof HTMLInputElement && node.type === 'file');
            if (!input || input.disabled) return;
            event.preventDefault();
            if (!event.isTrusted || pending || input.webkitdirectory) return;
            const id = crypto.randomUUID();
            pending = { input, id };
            window.ipc.postMessage(JSON.stringify({ id, multiple: input.multiple }));
        },
        true,
    );
})();
