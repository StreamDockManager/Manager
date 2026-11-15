import { PHYSICAL_KEYS, KEYBOARD_MACRO_LIMITS } from '../generated/keyboard-macro';
import type { BuiltinAction, KeyboardMacroEvent, PhysicalKey } from '../generated/runtime';
export type KeyboardMacroAction = Extract<BuiltinAction, { kind: 'keyboard_macro' }>;
export { PHYSICAL_KEYS, KEYBOARD_MACRO_LIMITS };
// Recognize supported physical keyboard codes
export const isPhysicalKey = (key: string): key is PhysicalKey =>
    (PHYSICAL_KEYS as readonly string[]).includes(key);
// Summarize the keyboard macro timeline for display
export function keyboardMacroSummary(action: KeyboardMacroAction) {
    const events = action.recording.events;
    const mode = action.playback.repeat_mode === 'system' ? ' · system repeat' : '';
    const countLabel = mode ? 'recorded events' : 'events';
    return `${events.length} ${countLabel} · ${(events.reduce((n, event) => n + event.delay_ms, 0) / 1000).toFixed(2)} s · ${action.playback.speed}×${mode}`;
}
/** Immediate feedback only; the native validator is authoritative at every persistence ingress */
export function validateKeyboardMacro(action: KeyboardMacroAction): string {
    return validateRecording(action, false);
}
/** Only for non-executable local draft recovery and appearance previews */
export function validateKeyboardMacroDraft(action: KeyboardMacroAction): string {
    return validateRecording(action, true);
}
// Reject recorded keyboard input that cannot safely form a macro
function validateRecording(action: KeyboardMacroAction, allowEmpty: boolean): string {
    const { recording, playback } = action;
    if (recording.version !== KEYBOARD_MACRO_LIMITS.version || recording.key_mode !== 'physical')
        return 'This keyboard recording format is not supported.';
    if (!['macos', 'windows', 'linux', 'unknown'].includes(recording.platform))
        return 'Choose a supported recording platform.';
    if (
        !Number.isFinite(playback.speed) ||
        playback.speed < KEYBOARD_MACRO_LIMITS.minSpeed ||
        playback.speed > KEYBOARD_MACRO_LIMITS.maxSpeed
    )
        return 'Playback speed must be between 0.25× and 4×.';
    if (!['stay_on_target', 'allow_focus_changes'].includes(playback.focus_policy))
        return 'Choose a supported focus policy.';
    if (
        !['exact', 'system'].includes(
            playback.repeat_mode === undefined ? 'exact' : playback.repeat_mode,
        )
    )
        return 'Choose a supported repeat mode.';
    if (
        recording.layout != null &&
        (typeof recording.layout !== 'string' ||
            !recording.layout ||
            new TextEncoder().encode(recording.layout).byteLength > 256 ||
            /[\x00-\x1f\x7f-\x9f]/.test(recording.layout))
    )
        return 'Recording layout must be a nonempty identifier of at most 256 bytes without control characters.';
    if (!allowEmpty && !recording.events.length) return 'Add or record at least one key.';
    if (recording.events.length > KEYBOARD_MACRO_LIMITS.maxEvents)
        return 'A recording supports up to 10,000 events.';
    if (
        new TextEncoder().encode(JSON.stringify(action)).byteLength >
        KEYBOARD_MACRO_LIMITS.maxSerializedBytes
    )
        return 'The recording must fit within 1 MiB.';
    let duration = 0;
    const held = new Set<PhysicalKey>();
    for (const [index, event] of recording.events.entries()) {
        const prefix = `Event ${index + 1}: `;
        if (!isPhysicalKey(event.key)) return prefix + 'Unsupported physical key.';
        if (!Number.isSafeInteger(event.delay_ms) || event.delay_ms < 0)
            return prefix + 'Delay must be a nonnegative whole number.';
        duration += event.delay_ms;
        if (duration > KEYBOARD_MACRO_LIMITS.maxDurationMs)
            return 'A recording cannot exceed five minutes.';
        if (event.kind === 'key_down') {
            if (held.has(event.key)) return prefix + 'Key is already held; use Repeat.';
            held.add(event.key);
        } else if (event.kind === 'key_up') {
            if (!held.delete(event.key)) return prefix + 'Release has no matching press.';
        } else if (event.kind === 'repeat') {
            if (!held.has(event.key)) return prefix + 'Repeat has no matching press.';
        } else return prefix + 'Unsupported event type.';
    }
    return held.size ? 'Every pressed key must have a matching release.' : '';
}
/** All-released boundaries preserve overlapping chords instead of flattening them into taps */
export function keyboardMacroGroups(events: KeyboardMacroEvent[]) {
    const groups: { start: number; end: number; label: string; duration: number }[] = [];
    const held = new Set<string>();
    let start = 0;
    let labels: string[] = [];
    let duration = 0;
    // Close an event group when its held keys are balanced or the timeline ends
    events.forEach((event, index) => {
        duration += event.delay_ms;
        if (event.kind === 'key_down') {
            held.add(event.key);
            if (!labels.includes(event.key)) labels.push(event.key);
        } else if (event.kind === 'key_up') held.delete(event.key);
        if (!held.size || index === events.length - 1) {
            groups.push({
                start,
                end: index + 1,
                label: labels.join(' + ') || event.key,
                duration,
            });
            start = index + 1;
            labels = [];
            duration = 0;
        }
    });
    return groups;
}
// Append a balanced keyboard combination to the draft timeline
export function addKeyboardCombo(
    action: KeyboardMacroAction,
    keys: PhysicalKey[],
    delay: number,
    hold: number,
) {
    if (!keys.length || new Set(keys).size !== keys.length || !keys.every(isPhysicalKey))
        throw Error('Choose distinct supported physical keys.');
    if (
        ![delay, hold].every(
            (value) =>
                Number.isSafeInteger(value) &&
                value >= 0 &&
                value <= KEYBOARD_MACRO_LIMITS.maxDurationMs,
        )
    )
        throw Error('Delays must be whole milliseconds between 0 and 300,000.');
    if (action.recording.events.length + keys.length * 2 > KEYBOARD_MACRO_LIMITS.maxEvents)
        throw Error('A recording supports up to 10,000 events.');
    action.recording.events.push(
        ...keys.map((key, index): KeyboardMacroEvent => ({
            kind: 'key_down',
            key,
            delay_ms: index ? 0 : delay,
        })),
        ...[...keys].reverse().map((key, index): KeyboardMacroEvent => ({
            kind: 'key_up',
            key,
            delay_ms: index ? 0 : hold,
        })),
    );
}
