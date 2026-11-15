// GPL-3.0-or-later. Project-owned fixture
const capabilities = {
    version: 1,
    id: 'app.streamdockmanager.plugins.fixture.browser',
    plugin_version: '1.0.0',
    theme_protocol: 0,
    settings_protocol: 1,
    input_protocol: 0,
    icon: { light: 'icon.svg', dark: 'icon.svg' },
    actions: {
        'app.streamdockmanager.plugins.fixture.browser.action': {
            control_types: ['button'],
            settings: 'inspector.html',
            icon: { light: 'icon.svg', dark: 'icon.svg' },
            states: [{ light: 'icon.svg', dark: 'icon.svg' }],
        },
    },
};
// Complete the deferred plugin work using the current operation state
window.connectSocket = window.connectElgatoStreamDeckSocket = (port, uuid, registerEvent) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}`);
    ws.addEventListener('open', () => {
        ws.send(JSON.stringify({ event: registerEvent, uuid, payload: { capabilities } }));
    });
    // Respond to message events in the plugin workflow
    ws.addEventListener('message', ({ data }) => {
        const message = JSON.parse(data);
        if (message.event === 'didReceiveGlobalSettings')
            document.querySelector('#status').textContent = 'Connected';
        if (message.event === 'sendToPlugin')
            ws.send(
                JSON.stringify({
                    event: 'sendToPropertyInspector',
                    context: message.context,
                    payload: { reply: 'received' },
                }),
            );
    });
};
