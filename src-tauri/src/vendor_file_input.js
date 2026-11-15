// Elgato's sdpi-components v4 reads the input value, strips C:\fakepath\,
// then URI-decodes the full selected path. No native operation is exposed here
(() => {
    const key = '__streamDockSelectedFile';
    if (Object.prototype.hasOwnProperty.call(window, key)) return;
    const value = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
    let pending;
    document.addEventListener(
        'click',
        // Respond to click events in the vendor file input workflow
        (event) => {
            const input = event
                .composedPath()
                .find((node) => node instanceof HTMLInputElement && node.type === 'file');
            if (input && !input.disabled) pending = input;
        },
        true,
    );
    Object.defineProperty(window, key, {
        configurable: false,
        writable: false,
        // Return the current vendor file input value
        value(paths) {
            const input = pending;
            pending = undefined;
            if (
                !input ||
                !input.isConnected ||
                input.type !== 'file' ||
                input.disabled ||
                !Array.isArray(paths) ||
                !paths.length
            )
                return false;
            let selected = `C:\\fakepath\\${encodeURIComponent(paths[0])}`;
            Object.defineProperty(input, 'value', {
                configurable: true,
                // Read the requested vendor file input value
                get() {
                    return selected;
                },
                // Replace the requested vendor file input value
                set(next) {
                    value.set.call(this, next);
                    selected = value.get.call(this);
                },
            });
            return true;
        },
    });
})();
