// SDMCAP1:0000020c:eyJ2ZXJzaW9uIjoxLCJpZCI6ImFwcC5zdHJlYW1kb2NrbWFuYWdlci5wbHVnaW5zLmZpeHR1cmUuY291bnRlciIsInBsdWdpbl92ZXJzaW9uIjoiMS4wLjAiLCJ0aGVtZV9wcm90b2NvbCI6MSwic2V0dGluZ3NfcHJvdG9jb2wiOjEsImlucHV0X3Byb3RvY29sIjowLCJpY29uIjp7ImxpZ2h0IjoiaWNvbi5zdmciLCJkYXJrIjoiaWNvbi5zdmcifSwiYWN0aW9ucyI6eyJhcHAuc3RyZWFtZG9ja21hbmFnZXIucGx1Z2lucy5maXh0dXJlLmNvdW50ZXIuaW5jcmVtZW50Ijp7ImNvbnRyb2xfdHlwZXMiOlsiYnV0dG9uIl0sImljb24iOnsibGlnaHQiOiJpY29uLnN2ZyIsImRhcmsiOiJpY29uLnN2ZyJ9LCJzdGF0ZXMiOlt7ImxpZ2h0IjoiaWNvbi5zdmciLCJkYXJrIjoiaWNvbi5zdmcifV19fX0=:ENDSDMCAP1
const capabilities = {
    version: 1,
    id: 'app.streamdockmanager.plugins.fixture.counter',
    plugin_version: '1.0.0',
    theme_protocol: 1,
    settings_protocol: 1,
    input_protocol: 0,
    icon: { light: 'icon.svg', dark: 'icon.svg' },
    actions: {
        'app.streamdockmanager.plugins.fixture.counter.increment': {
            control_types: ['button'],
            icon: { light: 'icon.svg', dark: 'icon.svg' },
            states: [{ light: 'icon.svg', dark: 'icon.svg' }],
        },
    },
};
// GPL-3.0-or-later. Project-owned protocol fixture; no third-party plugin code
const argument = (name) => process.argv[process.argv.indexOf(name) + 1];
const socket = new WebSocket(`ws://127.0.0.1:${argument('-port')}`);
let count = 0;
// Respond to open events in the counter workflow
socket.addEventListener('open', () => {
    socket.send(
        JSON.stringify({
            event: argument('-registerEvent'),
            uuid: argument('-pluginUUID'),
            payload: { capabilities },
        }),
    );
});
// Respond to message events in the counter workflow
socket.addEventListener('message', ({ data }) => {
    const message = JSON.parse(data);
    if (message.event === 'keyDown') {
        socket.send(
            JSON.stringify({
                event: 'setTitle',
                context: message.context,
                payload: { title: String(++count), target: 0 },
            }),
        );
    }
});
socket.addEventListener('close', () => process.exit(0));
