import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import test from 'node:test';

// Instantiate the actual import controller with UI and native dependencies left outside the test
function controller(state) {
    state.snapshot ??= {
        devices: state.deviceId
            ? [
                  {
                      id: state.deviceId,
                      persistence_key: state.deviceId,
                      connected: true,
                      model_id: 'mini',
                      name: 'Fixture',
                  },
              ]
            : [],
        profiles: [],
        sessions: {},
    };
    state.models ??= [];
    let factory;
    const source = stripTypeScriptTypes(
        readFileSync(new URL('../src/profile-archive-import.ts', import.meta.url), 'utf8'),
    )
        .replace(/^import .*?;\n/gm, '')
        .replace('export function', 'function');
    runInNewContext(source + '\nregisterProfileArchiveImport(Alpine, app, {});', {
        Alpine: {
            data: (_name, value) => {
                factory = value;
            },
        },
        app: () => state,
    });
    const value = factory();
    value.show = () => {};
    value.run = (work) => work();
    return value;
}

// Preserve the explicit selection even when flushing drafts changes the editor context
test('suggested import captures explicit device and does not follow a later active-device change', async () => {
    const calls = [];
    const state = {
        deviceId: 'selected-device',
        profile: undefined,
        page: undefined,
        flushDrafts: async () => {
            state.deviceId = 'other-device';
            state.profile = { id: 'other-profile' };
        },
        command: async (name, args) => {
            calls.push({ name, args });
            return {};
        },
    };
    await controller(state).reviewPluginSuggestion('plugin', 'suggestion');
    const request = calls.find((item) => item.name === 'prepare_plugin_profile_import').args;
    assert.equal(request.targetDevice, 'selected-device');
    assert.equal(request.targetProfile, undefined);
    assert.equal(request.targetPage, undefined);
});

// Leave active-device fallback to the runtime only when no device was explicitly selected
test('suggested import omits the target device only when no selection exists', async () => {
    let request;
    const state = {
        deviceId: '',
        command: async (_name, args) => {
            request = args;
            return {};
        },
    };
    await controller(state).reviewPluginSuggestion('plugin', 'suggestion');
    assert.equal(request.targetDevice, undefined);
});

// Use the chosen device session rather than the editor's unrelated active profile
test('explicit second device supplies its own profile and page', async () => {
    let request;
    const state = {
        deviceId: 'first',
        profile: { id: 'wrong' },
        page: { id: 'wrong-page' },
        snapshot: {
            devices: [
                {
                    id: 'second',
                    persistence_key: 'second-key',
                    connected: true,
                    model_id: 'mini',
                    name: 'Second',
                },
            ],
            profiles: [
                {
                    id: 'right',
                    model_id: 'mini',
                    active_page: 'main',
                    pages: [{ id: 'main' }, { id: 'remembered' }],
                },
            ],
            sessions: { 'second-key': { profile_id: 'right', page_id: 'remembered' } },
        },
        command: async (_name, args) => {
            request = args;
            return {};
        },
    };
    const review = controller(state);
    await review.reviewPluginSuggestion('plugin', 'suggestion', 'second');
    assert.equal(request.targetDevice, 'second');
    assert.equal(request.targetProfile, 'right');
    assert.equal(request.targetPage, 'remembered');
    assert.equal(review.pluginImportDevice.id, 'second');
});

// Keep two connected devices of the same model as distinct destinations in the chooser
test('suggestion chooser retains distinct same-model device IDs', async () => {
    let factory, handed;
    const state = {
        deviceId: 'first',
        device: { model_id: 'mini' },
        models: [{ model_id: 'mini', name: 'Mini' }],
        snapshot: {
            devices: [
                { id: 'first', connected: true, model_id: 'mini', name: 'Mini' },
                { id: 'second', connected: true, model_id: 'mini', name: 'Mini' },
                { id: 'offline', connected: false, model_id: 'mini', name: 'Mini' },
            ],
        },
    };
    const source = stripTypeScriptTypes(
        readFileSync(new URL('../src/plugin-profile-suggestions.ts', import.meta.url), 'utf8'),
    )
        .replace(/^import .*?;\n/gm, '')
        .replace('export function', 'function');
    runInNewContext(source + '\nregisterPluginProfileSuggestions(Alpine, app, handoffs);', {
        Alpine: {
            data: (_name, value) => {
                factory = value;
            },
        },
        app: () => state,
        profileFamily: (_models, model) => model,
        profileSuggestions: (values) => values,
        handoffs: {
            importSuggestion: async (...args) => {
                handed = args;
            },
        },
    });
    const chooser = factory();
    chooser.pluginSetup = {
        plugin_id: 'plugin',
        suggestions: [{ key: 'layout', source_key: 'source', family_id: 'mini' }],
    };
    chooser.pluginSuggestion = 'layout';
    chooser.chooseSuggestionDevice();
    assert.equal(chooser.pluginTargetDevice, 'first');
    assert.equal(chooser.pluginSuggestionDevices.length, 2);
    chooser.pluginTargetDevice = 'second';
    chooser.run = (work) => work();
    await chooser.importPluginSuggestion();
    assert.deepEqual(handed, ['plugin', 'source', 'second']);
});
