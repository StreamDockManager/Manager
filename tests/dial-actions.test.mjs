import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';

// Encode JavaScript source as an importable data URL
const codeUrl = (source) => `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`;
// Isolate draft persistence from browser/native services and unrelated key-logic UI
const urls = new Map([
    ['builtin-actions', codeUrl('export const actionChildren = () => [];')],
    ['key-logic', codeUrl('export const keyLogicActionReason = () => "";')],
    [
        'bridge',
        codeUrl(
            'export const defaultMedia = () => ({fit:"fill",speed:1,trim_start_ms:0,trim_end_ms:null,crop:[0,0,1,1]});',
        ),
    ],
    ['features', codeUrl('export const defaultAppearance = () => ({});')],
    ['background-color', codeUrl('export const DEFAULT_BACKGROUND = "#19202c";')],
]);
// Transpile and cache source modules while recursively rewriting their local imports
function sourceModule(name) {
    if (urls.has(name)) return urls.get(name);
    const source = stripTypeScriptTypes(
        readFileSync(new URL(`../src/${name}.ts`, import.meta.url), 'utf8'),
    ).replace(/from '\.\/([\w-]+)'/g, (_, dependency) => `from '${sourceModule(dependency)}'`);
    const url = codeUrl(source);
    urls.set(name, url);
    return url;
}
const { dialActionPresentation, selectedDial } = await import(sourceModule('dial-actions'));
const { actionPluginsEnabled } = await import(sourceModule('plugin-actions'));
// Create a fixture plugin action with its lane ID in settings
const lane = (id) => ({
    kind: 'plugin',
    plugin_id: 'fixture',
    action_id: id,
    settings: { id },
    resources: {},
});
// Create an unlinked dial binding with distinct left, right and press fixture actions
const dial = (id) => ({
    kind: 'dial',
    linked_turns: false,
    actions: [lane(id + '-left'), lane(id + '-right'), lane(id + '-press')],
});

// Verify lane presentation uses Encoder.Icon without inferring a whole-dial owner
test('lane presentation uses Encoder.Icon without inferring a whole-dial owner', () => {
    const plugins = [
        {
            id: 'fixture',
            dialect: 'streamdeck',
            actions: [
                { id: 'a-left', icon: 'list', encoder_icon: 'dial-left' },
                { id: 'a-right', icon: 'other-list', encoder_icon: 'dial-right' },
            ],
        },
    ];
    assert.equal(dialActionPresentation(lane('a-left'), plugins).icon, 'dial-left');
    assert.equal(dialActionPresentation(lane('a-right'), plugins).icon, 'dial-right');
    assert.equal(dialActionPresentation(dial('a'), plugins).icon, '');
    plugins[0].dialect = 'vsd';
    assert.equal(dialActionPresentation(lane('a-left'), plugins).icon, '');
});

// Verify all assigned lanes retain their plugin availability checks
test('all assigned lanes retain their plugin availability checks', () => {
    const action = dial('a');
    assert.equal(actionPluginsEnabled(action, [{ id: 'fixture', enabled: true }]), true);
    action.actions[1].plugin_id = 'disabled';
    assert.equal(actionPluginsEnabled(action, [{ id: 'fixture', enabled: true }]), false);
    action.actions[1] = { kind: 'none' };
    assert.equal(actionPluginsEnabled(action, [{ id: 'fixture', enabled: true }]), true);
    assert.equal(selectedDial(action), action);
    assert.equal(selectedDial({ kind: 'none' }), undefined);
});
