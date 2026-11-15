import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';
const source = readFileSync(
    new URL('../src-tauri/src/vendor_file_input_portable.js', import.meta.url),
    'utf8',
);
// Create isolated input for the browser resource selection portable tests checks
function fixture() {
    let click;
    const sent = [];
    class Input {
        type = 'file';
        disabled = false;
        isConnected = true;
        multiple = false;
        webkitdirectory = false;
        events = [];
        // Derive value from the current browser resource selection portable tests state
        get value() {
            return '';
        }
        // Return the current browser resource selection portable tests value
        set value(value) {
            if (value !== '') throw Error('InvalidStateError');
        }
        // Record dispatched event types on the simulated resource input
        dispatchEvent(event) {
            this.events.push(event.type);
        }
    }
    // Decode and capture outbound resource-selection messages
    const window = { ipc: { postMessage: (body) => sent.push(JSON.parse(body)) } };
    window.top = window;
    vm.runInNewContext(source, {
        window,
        HTMLInputElement: Input,
        Event: class {
            // Initialize browser resource selection portable tests state and its required lifecycle hooks
            constructor(type) {
                this.type = type;
            }
        },
        // Supply a stable request identifier for deterministic protocol assertions
        crypto: { randomUUID: () => 'request-id' },
        document: {
            // Add the event listener
            addEventListener: (_, cb) => {
                click = cb;
            },
        },
    });
    return {
        window,
        sent,
        Input,
        // Handle the browser resource selection portable tests click interaction
        click(input, isTrusted = true) {
            // Expose the resource input as the synthetic event path
            click({ isTrusted, composedPath: () => [input], preventDefault() {} });
        },
    };
}
// Verify portable chooser sends no path and correlates one native completion
test('portable chooser sends no path and correlates one native completion', () => {
    const f = fixture();
    const input = new f.Input();
    f.click(input);
    assert.deepEqual(f.sent, [{ id: 'request-id', multiple: false }]);
    assert.equal(f.window.__streamDockFileResult('wrong', ['/tmp/a']), false);
    assert.equal(f.window.__streamDockFileResult('request-id', ['/tmp/a #%.png']), true);
    assert.equal(decodeURIComponent(input.value.replace('C:\\fakepath\\', '')), '/tmp/a #%.png');
    assert.deepEqual(input.events, ['input', 'change']);
    assert.equal(f.window.__streamDockFileResult('request-id', ['/tmp/b']), false);
    input.value = '';
    assert.equal(input.value, '');
    // Verify portable chooser sends no path and correlates one native completion
    assert.throws(() => {
        input.value = '/unselected';
    });
});
// Verify cancel permits retry and ignores detached or changed input
test('cancel permits retry and ignores detached or changed input', () => {
    const f = fixture();
    const input = new f.Input();
    f.click(input);
    f.click(input);
    assert.equal(f.sent.length, 1);
    assert.equal(f.window.__streamDockFileResult('request-id', []), false);
    f.click(input);
    assert.equal(f.sent.length, 2);
    input.isConnected = false;
    assert.equal(f.window.__streamDockFileResult('request-id', ['/tmp/a']), false);
    assert.deepEqual(input.events, []);
});
// Verify untrusted clicks, directories and disabled inputs do not request native picker
test('untrusted clicks, directories and disabled inputs do not request native picker', () => {
    const f = fixture();
    const input = new f.Input();
    f.click(input, false);
    input.webkitdirectory = true;
    f.click(input);
    input.webkitdirectory = false;
    input.disabled = true;
    f.click(input);
    assert.deepEqual(f.sent, []);
});
