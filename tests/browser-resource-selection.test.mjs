import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import vm from 'node:vm';

const source = readFileSync(
    new URL('../src-tauri/src/vendor_file_input.js', import.meta.url),
    'utf8',
);
const vendorSource = readFileSync(
    new URL('fixtures/plugins/vendor/elgato/sdpi-components/src/core/file.ts', import.meta.url),
    'utf8',
);
const { sanitize } = await import(
    `data:text/javascript;base64,${Buffer.from(stripTypeScriptTypes(vendorSource)).toString('base64')}`
);
// Create isolated input for the browser resource selection tests checks
function fixture() {
    let capture;
    class Input {
        // Initialize browser resource selection tests state and its required lifecycle hooks
        constructor() {
            this.type = 'file';
            this.disabled = false;
            this.isConnected = true;
            this.current = '';
        }
        // Derive value from the current browser resource selection tests state
        get value() {
            return this.current;
        }
        // Return the current browser resource selection tests value
        set value(next) {
            if (next !== '') throw new Error('InvalidStateError');
            this.current = next;
        }
    }
    const window = {};
    const context = vm.createContext({
        window,
        HTMLInputElement: Input,
        document: {
            // Add the event listener
            addEventListener(name, callback, capturing) {
                assert.equal(name, 'click');
                assert.equal(capturing, true);
                capture = callback;
            },
        },
    });
    vm.runInContext(source, context);
    return {
        window,
        context,
        Input,
        // Handle the browser resource selection tests click interaction
        click(input) {
            // Simulate a composed event path from the input through its ancestors to the window
            capture({ composedPath: () => [input, {}, window] });
        },
    };
}
// Verify native selection projects exact encoded path accepted by the pinned genuine vendor sanitizer
test('native selection projects exact encoded path accepted by the pinned genuine vendor sanitizer', () => {
    for (const path of ['/tmp/File %20 and # /test ü.txt', 'C:\\temp\\File %.wav']) {
        const { window, Input, click } = fixture();
        const input = new Input();
        click(input);
        assert.equal(window.__streamDockSelectedFile([path]), true);
        assert.equal(sanitize(input.value), path);
        assert.equal(window.__streamDockSelectedFile(['/unrequested']), false);
        input.value = '';
        assert.equal(input.value, '');
        // Verify native selection projects exact encoded path accepted by the pinned genuine vendor sanitizer
        assert.throws(() => {
            input.value = '/unselected';
        }, /InvalidStateError/);
    }
});
// Verify selection is bound to last clicked file control, including a composed shadow path
test('selection is bound to last clicked file control, including a composed shadow path', () => {
    const { window, Input, click } = fixture();
    const first = new Input();
    const second = new Input();
    click(first);
    click(second);
    window.__streamDockSelectedFile(['/selected']);
    assert.equal(first.value, '');
    assert.equal(sanitize(second.value), '/selected');
});
// Verify missing, disabled, detached and changed controls fail closed
test('missing, disabled, detached and changed controls fail closed', () => {
    for (const change of [
        // Verify missing, disabled, detached and changed controls fail closed
        (i) => {
            i.disabled = true;
        },
        // Verify missing, disabled, detached and changed controls fail closed
        (i) => {
            i.isConnected = false;
        },
        // Verify missing, disabled, detached and changed controls fail closed
        (i) => {
            i.type = 'text';
        },
    ]) {
        const { window, Input, click } = fixture();
        const input = new Input();
        click(input);
        change(input);
        assert.equal(window.__streamDockSelectedFile(['/selected']), false);
        assert.equal(input.value, '');
    }
    assert.equal(fixture().window.__streamDockSelectedFile(['/selected']), false);
});
// Verify repeated initializer leaves the same narrow projection callback and no native IPC
test('repeated initializer leaves the same narrow projection callback and no native IPC', () => {
    const { window, context } = fixture();
    const original = window.__streamDockSelectedFile;
    vm.runInContext(source, context);
    assert.equal(window.__streamDockSelectedFile, original);
    assert.deepEqual(Object.getOwnPropertyNames(window), ['__streamDockSelectedFile']);
});
