import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';

const modules = new Map();
const toUrl = (code) => 'data:text/javascript;base64,' + Buffer.from(code).toString('base64');
modules.set('builtin-actions', toUrl('export const actionChildren = () => [];'));
function sourceModule(name) {
    if (modules.has(name)) return modules.get(name);
    const code = stripTypeScriptTypes(
        readFileSync(new URL(`../src/${name}.ts`, import.meta.url), 'utf8'),
    ).replace(/from '\.\/([\w-]+)'/g, (_, dependency) => `from '${sourceModule(dependency)}'`);
    const url = toUrl(code);
    modules.set(name, url);
    return url;
}
const { controlEventRows, convertControlEvents, standardControlAction } = await import(
    sourceModule('control-events')
);
const { ActionDraftTree } = await import(sourceModule('action-draft-tree'));
const { InspectorActionDraft } = await import(sourceModule('inspector-action-draft'));
const manifests = JSON.parse(readFileSync(new URL('../public/hardware.json', import.meta.url)));
const manifest = (model) => manifests.find((value) => value.model_id === model);
const rows = (model, id) =>
    controlEventRows(
        model,
        model.controls.find((value) => value.id === id),
    );
const offered = (value) => value.filter((row) => !row.reason).map((row) => row.index);
const action = (id) => ({
    kind: 'plugin',
    plugin_id: 'fixture',
    action_id: id,
    settings: { id, value: { enabled: false } },
    resources: { icon: id },
});

test('returning to standard bindings keeps normal triggers and identifies dropped actions', () => {
    const n4pro = manifest('n4-pro');
    const dialRows = rows(n4pro, 'dial-1');
    const original = {
        kind: 'dial',
        actions: [action('left'), action('right'), action('press')],
        linked_turns: false,
    };
    const expanded = convertControlEvents(original, dialRows);
    expanded.actions[3] = action('double');
    const dial = standardControlAction(expanded, dialRows, true, 3);
    assert.deepEqual(dial.action, original);
    assert.deepEqual(dial.removed, ['Double press']);
    assert.notEqual(dial.action.actions[2], expanded.actions[2]);
    const keyRows = rows(n4pro, 'key-1');
    const key = convertControlEvents(action('press'), keyRows);
    key.actions[0] = action('down');
    key.actions[4] = action('triple');
    assert.deepEqual(standardControlAction(key, keyRows, false, 4), {
        action: action('press'),
        kept: ['Single press'],
        removed: ['Key down', 'Triple press'],
        sharedSwipe: false,
    });
    const stripRows = rows(n4pro, n4pro.capabilities.touch_panel.gesture_control);
    const strip = convertControlEvents(action('original-swipe'), stripRows);
    assert.deepEqual(standardControlAction(strip, stripRows, false, 10).removed, []);
    strip.actions[11] = action('chosen-swipe');
    const standard = standardControlAction(strip, stripRows, false, 11);
    assert.deepEqual(standard.action, action('chosen-swipe'));
    assert.deepEqual(standard.removed, ['Swipe left', 'Swipe up', 'Swipe down']);
    assert.equal(standard.sharedSwipe, true);
});

test('hardware event choices use qualified descriptor flags without phantom N4 Pro edges', () => {
    const n4pro = manifest('n4-pro');
    for (let i = 1; i <= 4; i++) {
        const dial = rows(n4pro, 'dial-' + i);
        assert.deepEqual(offered(dial), [2, 3, 4, 6, 7]);
        assert.match(dial[1].reason, /press pulse/);
    }
    for (const id of n4pro.capabilities.touch_panel.button_controls) {
        const touch = rows(n4pro, id);
        assert.deepEqual(offered(touch), [2, 3, 4]);
        assert.equal(touch[2].name, 'Tap');
    }
    assert.deepEqual(
        offered(rows(n4pro, n4pro.capabilities.touch_panel.gesture_control)),
        [10, 11, 12, 13],
    );
    const n4 = manifest('n4');
    const legacyStrip = n4.controls.find((control) => control.semantics.includes('swipe'));
    assert.deepEqual(offered(rows(n4, legacyStrip.id)), [10, 11]);
    const capability = structuredClone(n4);
    const control = capability.controls.find((entry) => entry.kind === 'encoder');
    assert.deepEqual(offered(rows(capability, control.id)), [0, 1, 2, 3, 4, 5, 6, 7]);
    control.semantics.push('rotate_held');
    assert.deepEqual(offered(rows(capability, control.id)), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
});

test('conversion retains separate legacy actions, and each event owns settings/resources and undo data', () => {
    const n4pro = manifest('n4-pro');
    const base = {
        kind: 'dial',
        linked_turns: false,
        actions: [action('left'), action('right'), action('click')],
    };
    const events = convertControlEvents(base, rows(n4pro, 'dial-1'));
    assert.equal(events.actions.length, 14);
    assert.equal(events.multi_press_ms, 300);
    assert.equal(events.hold_ms, 600);
    assert.deepEqual([events.actions[6], events.actions[7], events.actions[2]], base.actions);
    assert.notEqual(events.actions[6].settings, base.actions[0].settings);

    const tree = new ActionDraftTree(events);
    tree.eventLane = 4;
    tree.active = new InspectorActionDraft(action('triple'));
    tree.eventLane = 3;
    tree.active = new InspectorActionDraft(action('double'));
    tree.eventLane = 8; // Preserve an imported action on an unsupported Event slot.
    tree.active = new InspectorActionDraft(action('held-left'));
    tree.multiPressMs = 455;
    tree.holdMs = 900;
    const restored = ActionDraftTree.restore(tree.recovery());
    assert.equal(restored.eventLane, 8);
    assert.deepEqual(restored.commit(), tree.commit());
    restored.eventLane = 3;
    restored.active = new InspectorActionDraft();
    const committed = restored.commit();
    assert.equal(committed.actions[3].kind, 'none');
    assert.deepEqual(committed.actions[4], action('triple'));
    assert.deepEqual(committed.actions[8], action('held-left'));
    assert.deepEqual(committed.actions[6], action('left'));
    assert.equal(committed.multi_press_ms, 455);
    assert.equal(committed.hold_ms, 900);
    assert.equal(tree.commit().actions[3].action_id, 'double');
});

test('normal action and swipe conversions do not mutate originals or invent supported inputs', () => {
    const n4pro = manifest('n4-pro');
    const legacy = { kind: 'mute' };
    const button = n4pro.controls.find((control) => control.kind === 'button');
    const converted = convertControlEvents(legacy, rows(n4pro, button.id));
    assert.deepEqual(converted.actions[2], legacy);
    assert.equal(converted.actions.filter((entry) => entry.kind !== 'none').length, 1);
    const restore = { kind: 'swipe_navigation', target: 'profile', wrap: false, reverse: true };
    const swipes = convertControlEvents(restore, rows(n4pro, 'touch-strip'));
    for (const index of [10, 11, 12, 13]) assert.deepEqual(swipes.actions[index], restore);
    assert.equal(swipes.actions[2].kind, 'none');
    assert.equal(new ActionDraftTree(legacy).recovery().eventActions, undefined);
    assert.throws(
        () =>
            ActionDraftTree.restore({
                ...new ActionDraftTree(converted).recovery(),
                eventHoldMs: 249,
            }),
        /Invalid event binding draft/,
    );
});
