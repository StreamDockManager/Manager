// GPL-3.0-or-later. Protocol fixture for independent native runtime qualification
const args = process.argv.slice(2);
const options = Object.fromEntries(
    Array.from({ length: args.length / 2 }, (_, i) => [args[i * 2], args[i * 2 + 1]]),
);
const socket = new WebSocket(`ws://127.0.0.1:${Number(options['-port'])}`);
const contexts = new Map();
socket.addEventListener('open', () =>
    socket.send(
        JSON.stringify({
            event: options['-registerEvent'],
            uuid: options['-pluginUUID'],
            payload: { capabilities },
        }),
    ),
);
// Respond to message events in the main workflow
socket.addEventListener('message', ({ data }) => {
    const m = JSON.parse(data);
    if (m.event === 'shutdown') {
        contexts.clear();
        socket.close();
        return;
    }
    if (!m.context) return;
    if (m.event === 'willDisappear') {
        contexts.delete(m.context);
        return;
    }
    if (m.event === 'willAppear') {
        contexts.set(m.context, 0);
        socket.send(
            JSON.stringify({
                event: 'setTitle',
                context: m.context,
                payload: { title: '0', target: 0 },
            }),
        );
        return;
    }
    if (m.event !== 'keyUp' || !contexts.has(m.context)) return;
    const count = contexts.get(m.context) + 1;
    contexts.set(m.context, count);
    socket.send(
        JSON.stringify({
            event: 'setTitle',
            context: m.context,
            payload: { title: String(count), target: 0 },
        }),
    );
    if (m.streamDockManager?.request_id)
        socket.send(
            JSON.stringify({
                event: 'actionResult',
                context: m.context,
                payload: { request_id: m.streamDockManager.request_id, success: true },
            }),
        );
});
socket.addEventListener('close', () => {
    contexts.clear();
    process.exit(0);
});
