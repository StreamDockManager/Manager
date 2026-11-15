// GPL-3.0-or-later. Project-owned fixture
window.connectSocket = window.connectElgatoStreamDeckSocket = (
    port,
    uuid,
    registerEvent,
    info,
    actionInfo,
) => {
    const ws = new WebSocket(`ws://127.0.0.1:${port}`);
    const action = JSON.parse(actionInfo);
    let pendingTitle = null;
    let settings = structuredClone(action.payload.settings || {});
    ws.addEventListener('open', () => {
        ws.send(JSON.stringify({ event: registerEvent, uuid }));
    });
    // Respond to message events in the inspector workflow
    ws.addEventListener('message', ({ data }) => {
        const message = JSON.parse(data);
        if (message.event === 'settingsDraftSaved') {
            document.querySelector('#status').textContent = 'Saved';
            window.parent.postMessage(
                { event: 'stream-dock-manager-settings-saved', context: action.context },
                '*',
            );
        }
        if (message.event === 'didReceiveSettings') {
            settings = structuredClone(message.payload.settings);
            const title = message.payload.settings.title || '';
            document.querySelector('#title').value = title;
            document.querySelector('#status').textContent =
                pendingTitle === null
                    ? 'Connected'
                    : title === pendingTitle
                      ? 'Saved'
                      : 'Settings mismatch';
            pendingTitle = null;
        }
        if (message.event === 'sendToPropertyInspector')
            document.querySelector('#reply').textContent = message.payload.reply;
    });
    // Save pending changes through the inspector workflow
    const save = () => {
        pendingTitle = document.querySelector('#title').value;
        document.querySelector('#status').textContent = 'Saving';
        ws.send(
            JSON.stringify({
                event: 'setSettings',
                context: action.context,
                payload: { ...settings, title: pendingTitle },
            }),
        );
        // Ordered reads confirm the managed draft only; Manager owns durable commit
        ws.send(JSON.stringify({ event: 'getSettings', context: action.context }));
    };
    document.querySelector('#save').addEventListener('click', save);
    // Respond to message events in the inspector workflow
    window.addEventListener('message', (event) => {
        if (
            event.source === window.parent &&
            event.data?.event === 'stream-dock-manager-save-settings'
        )
            save();
    });
    // Respond to click events in the inspector workflow
    document.querySelector('#cancel').addEventListener('click', () => {
        document.querySelector('#title').value = settings.title || '';
        window.parent.postMessage(
            { event: 'stream-dock-manager-cancel-settings', context: action.context },
            '*',
        );
    });
    window.addEventListener('pagehide', () => ws.close(), { once: true });
    document.querySelector('#message').addEventListener('click', () =>
        ws.send(
            JSON.stringify({
                event: 'sendToPlugin',
                context: action.context,
                payload: { hello: true },
            }),
        ),
    );
};
