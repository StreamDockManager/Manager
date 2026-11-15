import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

// Remove parameter-property modifiers only from the unused constructor so Node can strip types
const stateSource = readFileSync(new URL('../src/state.ts', import.meta.url), 'utf8');
assert.match(stateSource, /public bridge: Bridge/);
assert.match(stateSource, /public nativeRendering = bridge.surfaceRendering/);
// Exercise unchanged prototype methods without constructing browser services or native subscriptions
const source = stripTypeScriptTypes(
    stateSource
        .replace('public bridge: Bridge', 'bridge: Bridge')
        .replace('public nativeRendering =', 'nativeRendering ='),
)
    .replace(/^import [\s\S]*?;\n/gm, '')
    .replace(/^export /gm, '');
const State = runInNewContext(source + '\nAppState', { ToastState: class {} });

// Retain the chosen device and original revision while an inferred-target assignment waits in the queue
test('empty-profile assignment captures its device and never rebases onto a changed workspace', async () => {
    let release;
    const blocked = new Promise((resolve) => {
        release = resolve;
    });
    let request;
    const state = {
        deviceId: 'chosen',
        configurationRevision: 4,
        snapshot: { profiles: [], plugins: [] },
        bindingWrites: blocked,
        command: async (_name, args) => {
            request = args;
            return {};
        },
        receiveWorkspace: () => {},
        withControls: (_ids, work) => work(),
    };
    const saving = State.prototype.edit.call(
        state,
        [{ kind: 'bindings', profile_id: '', page_id: '', bindings: { key: null } }],
        'Assign',
    );
    state.deviceId = 'other';
    state.configurationRevision = 5;
    release();
    await saving;
    assert.equal(request.deviceId, 'chosen');
    assert.equal(request.expectedRevision, 4);
});

// Send an actual paste as an inferred-target edit rather than silently dropping it when profiles are absent
test('paste into a connected device without a profile requests transactional target resolution', async () => {
    let edits;
    const state = {
        selectedIds: ['key'],
        sortedIds: (ids) => ids,
        clipboard: [{ title: 'Action' }],
        device: { connected: true },
        profile: undefined,
        page: undefined,
        isProtectedControl: () => false,
        edit: async (value) => {
            edits = value;
        },
    };
    await State.prototype.pasteBindings.call(state);
    assert.equal(edits[0].profile_id, '');
    assert.equal(edits[0].page_id, '');
    assert.equal(edits[0].bindings.key.title, 'Action');
});

// Exercise the guided setup's actual assignment method with an empty connected workspace
test('guided setup can assign its first binding without requiring a pre-existing profile', async () => {
    let factory, edits;
    const code = stripTypeScriptTypes(
        readFileSync(new URL('../src/setup-workflow.ts', import.meta.url), 'utf8'),
    )
        .replace(/^import .*?;\n/gm, '')
        .replace('export function', 'function');
    const state = {
        device: { connected: true },
        edit: async (value) => {
            edits = value;
        },
    };
    runInNewContext(code + '\nregisterSetupWorkflow(Alpine, app, {});', {
        Alpine: {
            data: (_name, value) => {
                factory = value;
            },
        },
        app: () => state,
        createSetupCapabilities: () => [],
        DEFAULT_BACKGROUND: '#000000',
        defaultMedia: () => ({}),
        suggestedFeedback: () => ({}),
    });
    const setup = factory();
    setup.run = (work) => work();
    setup.saveProgress = async () => {};
    setup.setupControl = 'key';
    setup.setupKind = 'mute';
    await setup.createSetupBinding();
    assert.equal(edits[0].profile_id, '');
    assert.equal(edits[0].page_id, '');
    assert.equal(edits[0].bindings.key.action.kind, 'mute');
});

// Keep explicit profile edits independent of connected-device resolution for offline editing
test('explicit profile assignment does not acquire a device constraint', async () => {
    let request;
    const state = {
        deviceId: 'unrelated-disconnected',
        configurationRevision: 4,
        snapshot: { profiles: [], plugins: [] },
        bindingWrites: Promise.resolve(),
        command: async (_name, args) => {
            request = args;
            return {};
        },
        receiveWorkspace: () => {},
        withControls: (_ids, work) => work(),
    };
    await State.prototype.edit.call(
        state,
        [
            {
                kind: 'bindings',
                profile_id: 'explicit-profile',
                page_id: 'explicit-page',
                bindings: { key: null },
            },
        ],
        'Assign',
    );
    assert.equal(request.deviceId, undefined);
});
