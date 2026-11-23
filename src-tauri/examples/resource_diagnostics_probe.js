// This initialization script runs only in the opt-in disposable WebKit probe.
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
let failures = 0;
let stage = 'editor';
window.addEventListener('manager-diagnostic-preview', (event) => {
    if (event.detail.event === 'resource_error') {
        failures++;
        window.ipc.postMessage(JSON.stringify(event.detail));
    }
});
async function waitFor(test) {
    for (let attempt = 0; attempt < 160; attempt++) {
        if (test()) return;
        await pause(100);
    }
    throw Error('Fixture timed out');
}
window.addEventListener('load', async () => {
    try {
        const surface = () =>
            document.querySelector(
                '.hardware-control[data-id="key-1"] img[data-resource-role="control-surface"]',
            );
        await waitFor(surface);
        for (let i = 0; i < 2; i++) {
            stage = 'settings';
            location.hash = '#/settings';
            await waitFor(
                () => !surface() && document.querySelector('[data-acl-component="SD-SETTINGS" i]'),
            );
            stage = 'editor';
            location.hash = '#/editor';
            await waitFor(surface);
        }
        await pause(250);
        stage = 'empty-media';
        if (failures) throw Error('Unexpected optional-media error');
        stage = 'broken-image';
        surface().src = 'data:image/png;base64,private-fixture';
        await waitFor(() => failures > 0);
        window.ipc.postMessage('finish');
    } catch {
        window.ipc.postMessage('failure-' + stage);
    }
});
