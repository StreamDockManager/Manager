// GPL-3.0-or-later. Synthetic dual-role package, not vendor code
window.connectSocket = window.connectElgatoStreamDeckSocket = (...args) => {
    const [port, uuid, event, _info, actionInfo] = args;
    const status = document.querySelector('#status');
    const timer = document.querySelector('#timer');
    document.querySelector('#arg-count').textContent = String(args.length);
    const ws = new WebSocket(`ws://127.0.0.1:${port}`);
    const send = (message) => ws.send(JSON.stringify(message));
    ws.addEventListener('open', () => {
        send({ event, uuid });
        if (!actionInfo) status.textContent = 'Connected';
    });

    // The vendor starter selects its inspector from the fifth argument
    if (actionInfo) {
        document.body.dataset.role = 'inspector';
        document.querySelector('#settings').hidden = false;
        const { action, context, payload } = JSON.parse(actionInfo);
        if (!action.endsWith('.action')) throw Error('Missing action identity');
        const field = document.querySelector('#title');
        field.value = payload.settings.title;
        document.body.dataset.initial = JSON.stringify(payload.settings);
        let pending = false;
        ws.addEventListener('message', ({ data }) => {
            const message = JSON.parse(data);
            if (message.event === 'didReceiveSettings') {
                field.value = message.payload.settings.title;
                status.textContent = pending ? 'Saved' : 'Connected';
                pending = false;
            }
            if (message.event === 'sendToPropertyInspector')
                document.querySelector('#reply').textContent = message.payload.reply;
        });
        document.querySelector('#save').addEventListener('click', () => {
            pending = true;
            send({ event: 'setSettings', context, payload: { title: field.value } });
            send({ event: 'getSettings', context });
        });
        document
            .querySelector('#message')
            .addEventListener('click', () =>
                send({ event: 'sendToPlugin', context, payload: { hello: true } }),
            );
        return;
    }

    document.body.dataset.role = 'runtime';
    const worker = new Worker('./interval.js');
    let context;
    let titleParameters;
    ws.addEventListener('message', ({ data }) => {
        const message = JSON.parse(data);
        if (message.event === 'willAppear') {
            context = message.context;
            const settings = message.payload.settings;
            if (
                settings.theme !== 'theme1' ||
                settings.zone !== 'system' ||
                settings.amplify !== false
            )
                throw Error('Missing legacy VSD defaults or explicit setting lost');
            document.body.dataset.initial = JSON.stringify(settings);
        }
        if (message.event === 'titleParametersDidChange') {
            if (context !== message.context)
                throw Error('Title parameters arrived before appearance');
            titleParameters = message.payload.titleParameters;
            document.body.dataset.titleParameters = JSON.stringify(titleParameters);
        }
        if (message.event === 'sendToPlugin')
            send({
                event: 'sendToPropertyInspector',
                context: message.context,
                payload: { reply: 'received' },
            });
    });
    worker.addEventListener('message', () => {
        if (!context || !titleParameters) return;
        const canvas = document.createElement('canvas');
        canvas.width = canvas.height = 16;
        const ctx = canvas.getContext('2d');
        ctx.fillStyle = '#33cc88';
        ctx.fillRect(0, 0, 16, 16);
        send({
            event: 'setImage',
            context,
            payload: { target: 0, image: canvas.toDataURL('image/png') },
        });
        send({ event: 'setTitle', context, payload: { title: 'Worker tick' } });
        timer.textContent = 'Worker tick';
        worker.terminate();
    });
};
