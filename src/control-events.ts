// Event indexes are persistent action paths; append-only if the wire contract expands.
import type { Action, Control, Manifest } from './types';
import { cloneJson } from './json-clone';

export const eventCount = 14;
export const defaultMultiPressMs = 300;
export const defaultHoldMs = 600;
export const multiPressBounds = [100, 750] as const;
export const holdBounds = [250, 2500] as const;

export type EventAction = Extract<Action, { kind: 'events' }>;
export type EventGroup = 'Press & release' | 'Gestures' | 'Rotation';
export type EventRow = { index: number; name: string; group: EventGroup; reason: string };

export function controlEventRows(manifest: Manifest | undefined, control: Control | undefined) {
    if (!manifest || !control) return [] as EventRow[];
    const has = (name: string) => control.semantics.includes(name);
    const { capabilities } = manifest;
    const knob = control.kind === 'encoder';
    const touch = control.kind === 'touch';
    const press = (knob || control.kind === 'button') && has('press');
    const pulse = (knob || control.kind === 'button') && has('pulse');
    const reliable = knob
        ? capabilities.reliable_encoder_release
        : capabilities.reliable_button_release;
    const edges = press && has('release') && reliable;
    const touchDown = touch && has('touch_down');
    const touchUp = touch && has('touch_up');
    const panel = capabilities.touch_panel;
    const strip = touch && panel?.gesture_control === control.id;
    const taps = edges || pulse || (touch && has('touch') && !strip);
    const rotates = knob && has('rotate');
    const swipe = (direction: string) =>
        touch &&
        has('swipe') &&
        (panel
            ? strip && panel.swipe_directions.includes(direction)
            : ['left', 'right'].includes(direction));
    const edgeReason = pulse
        ? 'This control reports a press pulse, not physical down and up.'
        : 'Physical contact edges have not been verified for this control.';
    const row = (
        index: number,
        name: string,
        group: EventGroup,
        supported: boolean,
        reason: string,
    ) => ({ index, name, group, reason: supported ? '' : reason });
    return [
        row(
            0,
            touch ? 'Touch down' : knob ? 'Dial down' : 'Key down',
            'Press & release',
            edges || touchDown,
            edgeReason,
        ),
        row(
            1,
            touch ? 'Touch up' : knob ? 'Dial up' : 'Key up',
            'Press & release',
            edges || touchUp,
            edgeReason,
        ),
        row(
            2,
            touch ? 'Tap' : 'Single press',
            'Gestures',
            taps || press,
            'A completed press or tap is not available.',
        ),
        row(
            3,
            touch ? 'Double tap' : 'Double press',
            'Gestures',
            taps,
            'Distinct presses or taps are not available.',
        ),
        row(
            4,
            touch ? 'Triple tap' : 'Triple press',
            'Gestures',
            taps,
            'Distinct presses or taps are not available.',
        ),
        row(
            5,
            'Long hold',
            'Gestures',
            edges || (touchDown && touchUp && has('touch_hold')),
            'A hold requires reliable physical down and up.',
        ),
        row(6, 'Turn left', 'Rotation', rotates, 'This control cannot report rotation.'),
        row(7, 'Turn right', 'Rotation', rotates, 'This control cannot report rotation.'),
        row(
            8,
            'Hold and turn left',
            'Rotation',
            rotates && edges && has('rotate_held'),
            'Held rotation requires reliable dial down and up.',
        ),
        row(
            9,
            'Hold and turn right',
            'Rotation',
            rotates && edges && has('rotate_held'),
            'Held rotation requires reliable dial down and up.',
        ),
        ...['left', 'right', 'up', 'down'].map((direction, index) =>
            row(
                10 + index,
                'Swipe ' + direction,
                'Gestures',
                Boolean(swipe(direction)),
                'This swipe direction is not available.',
            ),
        ),
    ];
}

export function eventTiming(value: unknown, [min, max]: readonly [number, number]) {
    return typeof value === 'number' && Number.isInteger(value) && value >= min && value <= max;
}

/** Explicit conversion. An existing dial's three resolved lanes are never collapsed. */
export function convertControlEvents(action: Action, rows: readonly EventRow[]): EventAction {
    const actions: Action[] = Array.from({ length: eventCount }, () => ({ kind: 'none' }));
    if (action.kind === 'dial') {
        if (action.actions.length !== 3) throw Error('The saved dial does not have three lanes.');
        [actions[6], actions[7], actions[2]] = cloneJson(action.actions);
    } else if (action.kind !== 'none') {
        const swipes = rows.filter((row) => row.index >= 10 && !row.reason);
        if (swipes.length) {
            for (const row of swipes) actions[row.index] = cloneJson(action);
        } else {
            actions[2] = cloneJson(action);
        }
    }
    return {
        kind: 'events',
        actions,
        multi_press_ms: defaultMultiPressMs,
        hold_ms: defaultHoldMs,
    };
}

// Keep only actions that can keep their trigger in the standard editor
export function standardControlAction(
    draft: EventAction,
    rows: readonly EventRow[],
    encoder: boolean,
    selected: number,
) {
    const assigned = rows.filter((row) => draft.actions[row.index]?.kind !== 'none');
    const supportedSwipes = rows.filter((row) => row.index >= 10 && !row.reason);
    const swipes = assigned.filter((row) => supportedSwipes.includes(row));
    const swipe = swipes.find((row) => row.index === selected) || swipes[0];
    const swipeAction = swipe && draft.actions[swipe.index];
    const retained = encoder
        ? [6, 7, 2]
        : supportedSwipes.length
          ? swipes
                .filter(
                    (row) =>
                        JSON.stringify(draft.actions[row.index]) === JSON.stringify(swipeAction),
                )
                .map((row) => row.index)
          : [2];
    const read = (index: number): Action => cloneJson(draft.actions[index] || { kind: 'none' });
    const action: Action = encoder
        ? { kind: 'dial', actions: retained.map(read), linked_turns: false }
        : supportedSwipes.length
          ? swipe
              ? read(swipe.index)
              : { kind: 'none' }
          : read(2);
    return {
        action,
        kept: assigned.filter((row) => retained.includes(row.index)).map((row) => row.name),
        removed: assigned.filter((row) => !retained.includes(row.index)).map((row) => row.name),
        sharedSwipe: Boolean(swipe),
    };
}

// The model designates the strip independently from tappable touch regions.
export function isSwipeStrip(manifest: Manifest | undefined, control: Control | undefined) {
    return (
        control?.kind === 'touch' &&
        control.semantics.includes('swipe') &&
        manifest?.capabilities.touch_panel?.gesture_control === control.id
    );
}

// Project an unsaved event map while preserving custom timing, legacy swipe settings and lane identity.
export function swipeStripEvents(
    action: Action | undefined,
    rows: readonly EventRow[],
): EventAction {
    if (action?.kind === 'events') return action;
    const events = convertControlEvents(action ?? { kind: 'none' }, rows);
    for (const row of rows) {
        if (row.reason || row.index < 10) continue;
        if (
            action &&
            action.kind !== 'none' &&
            (action.kind !== 'swipe_navigation' || row.index < 12)
        )
            continue;
        events.actions[row.index] = {
            kind: 'navigate',
            target: row.index < 12 ? 'page' : 'profile',
            direction: row.index === 10 || row.index === 12 ? 'next' : 'previous',
        };
    }
    return events;
}
