import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import { webcrypto } from 'node:crypto';
import test from 'node:test';

// Evaluate preview-only functions without browser or native service imports
function load(name, symbol, values = {}) {
    const code = stripTypeScriptTypes(
        readFileSync(new URL(`../src/${name}.ts`, import.meta.url), 'utf8'),
    )
        .replace(/^import [\s\S]*?;\n/gm, '')
        .replace(/^export /gm, '');
    return runInNewContext(code + '\n' + symbol, { structuredClone, crypto: webcrypto, ...values });
}
const resolve = load('preview-assignment-target', 'resolvePreviewAssignment', {
    defaultMedia: () => ({}),
    ensureKeyboardNavigation: () => {},
});
const handlers = load('preview-workspace-handlers', 'createPreviewWorkspaceHandlers', {
    resolvePreviewAssignment: resolve,
    assertKeyboardMacroEdits: () => {},
});
// Build a minimal connected preview snapshot with no profiles or retained device sessions
const fixture = () => ({
    profiles: [],
    sessions: {},
    active_profile: '',
    devices: [
        {
            id: 'chosen',
            connected: true,
            persistence_key: 'chosen',
            model_id: 'mini',
            manifest: { controls: [{ id: 'key' }] },
        },
    ],
});
const edit = () => ({
    kind: 'bindings',
    profile_id: '',
    page_id: '',
    bindings: { key: { title: 'Assigned' } },
});

// Resolve an inferred target to a new Default and reuse it for subsequent assignments
test('preview assignments create one Default for the explicit device and reuse it', () => {
    const s = fixture();
    s.devices.push({ ...s.devices[0], id: 'other', persistence_key: 'other', model_id: 'other' });
    const first = resolve(s, edit(), 'chosen');
    assert.equal(s.profiles.length, 1);
    assert.equal(s.profiles[0].name, 'Default');
    assert.equal(s.profiles[0].model_id, 'mini');
    assert.equal(resolve(s, edit(), 'chosen').profile_id, first.profile_id);
    const scoped = resolve(
        s,
        {
            kind: 'assign_control',
            profile_id: '',
            page_id: '',
            control_id: 'key',
            scope: 'page',
            binding: { title: 'Scoped' },
        },
        'chosen',
    );
    assert.equal(scoped.profile_id, first.profile_id);
    assert.equal(scoped.page_id, first.page_id);
});

// Reject ambiguous, disconnected and stale explicit targets rather than redirecting the assignment
test('preview resolution rejects invalid targets and never creates a profile for removals', () => {
    const s = fixture();
    assert.throws(() => resolve(s, edit(), 'missing'));
    assert.throws(() => resolve(s, { ...edit(), profile_id: 'missing' }));
    assert.throws(() => resolve(s, { ...edit(), page_id: 'missing' }, 'chosen'));
    assert.equal(s.profiles.length, 0);
    resolve(s, { ...edit(), bindings: { key: null } }, 'chosen');
    assert.equal(s.profiles.length, 0);
    s.devices.push({ ...s.devices[0], id: 'second' });
    assert.throws(() => resolve(s, edit()));
});

// Roll back the entire preview snapshot if a later edit fails after inferred profile creation
test('preview handler rolls back inferred profiles and earlier writes on failure', () => {
    const s = fixture();
    const before = structuredClone(s);
    let calls = 0,
        commits = 0;
    const controller = handlers(
        {
            s,
            guard: () => {},
            apply: () => {
                if (++calls === 2) throw Error('invalid later edit');
            },
            commit: () => {
                commits++;
            },
            markEdit: () => {},
        },
        {},
        new Map(),
        () => {},
    );
    assert.throws(
        () =>
            controller.apply_edits({
                expectedRevision: 1,
                deviceId: 'chosen',
                edits: [edit(), edit()],
            }),
        /invalid later edit/,
    );
    assert.deepEqual(s, before);
    assert.equal(commits, 0);
});
