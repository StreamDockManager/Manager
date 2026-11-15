import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
const root = new URL('./fixtures/plugins/browser/', import.meta.url);
const manifest = JSON.parse(fs.readFileSync(new URL('manifest.json', root)));
// Load the data required by the browser fixture tests workflow
function load(name) {
    const nodes = new Map();
    const handlers = {};
    const messages = [];
    const posted = [];
    let socket;
    class WebSocket {
        // Initialize browser fixture tests state and its required lifecycle hooks
        constructor() {
            socket = this;
            this.handlers = {};
        }
        // Add the event listener
        addEventListener(name, callback) {
            this.handlers[name] = callback;
        }
        // Send the browser fixture tests operation
        send(value) {
            messages.push(JSON.parse(value));
        }
        // Release browser fixture tests state and associated lifecycle resources
        close() {
            this.closed = true;
        }
    }
    const window = {
        // Capture posted browser messages for assertions
        parent: { postMessage: (value) => posted.push(value) },
        // Add the event listener
        addEventListener: (name, callback) => {
            handlers[name] = callback;
        },
    };
    const document = {
        // Reuse selector-keyed element stubs and retain their registered event handlers
        querySelector: (key) => {
            if (!nodes.has(key))
                nodes.set(key, {
                    value: '',
                    textContent: '',
                    // Add the event listener
                    addEventListener(name, callback) {
                        this[name] = callback;
                    },
                });
            return nodes.get(key);
        },
    };
    vm.runInNewContext(fs.readFileSync(new URL(name, root), 'utf8'), {
        window,
        document,
        WebSocket,
        structuredClone,
    });
    return {
        window,
        nodes,
        handlers,
        messages,
        posted,
        // Derive socket from the current browser fixture tests state
        get socket() {
            return socket;
        },
    };
}
// Verify browser fixture registers its exact current descriptor on both bootstrap names
test('browser fixture registers its exact current descriptor on both bootstrap names', () => {
    const page = load('plugin.js');
    assert.equal(page.window.connectSocket, page.window.connectElgatoStreamDeckSocket);
    page.window.connectSocket(1234, 'token', 'registerPlugin');
    page.socket.handlers.open();
    assert.deepEqual(
        page.messages[0].payload.capabilities,
        manifest.StreamDockManager.Capabilities,
    );
});
// Verify managed inspector preserves unknown settings, acknowledges save and cancels through Manager
test('managed inspector preserves unknown settings, acknowledges save and cancels through Manager', () => {
    const page = load('inspector.js');
    const context = 'settings-draft:token';
    page.window.connectSocket(
        1234,
        'token',
        'registerPropertyInspector',
        '{}',
        JSON.stringify({ context, payload: { settings: { title: 'Original', unknown: 42 } } }),
    );
    page.socket.handlers.open();
    page.socket.handlers.message({
        data: JSON.stringify({
            event: 'didReceiveSettings',
            payload: { settings: { title: 'Original', unknown: 42 } },
        }),
    });
    page.nodes.get('#title').value = 'Changed';
    page.handlers.message({
        source: page.window.parent,
        data: { event: 'stream-dock-manager-save-settings' },
    });
    // Verify managed inspector preserves unknown settings, acknowledges save and cancels through Manager
    const saved = page.messages.find((message) => message.event === 'setSettings');
    assert.deepEqual(saved.payload, { title: 'Changed', unknown: 42 });
    assert.equal(page.posted.length, 0);
    page.socket.handlers.message({
        data: JSON.stringify({ event: 'settingsDraftSaved', context }),
    });
    assert.equal(page.posted[0].event, 'stream-dock-manager-settings-saved');
    assert.equal(page.posted[0].context, context);
    page.nodes.get('#cancel').click();
    assert.equal(page.posted[1].event, 'stream-dock-manager-cancel-settings');
    page.handlers.pagehide();
    assert.equal(page.socket.closed, true);
});
