import type { PreviewHandlers } from './preview-handlers';
import type {
    KeyboardMacroRecording,
    KeyboardMacroSession,
    PhysicalKey,
} from '../generated/runtime';
import { PHYSICAL_KEYS, KEYBOARD_MACRO_LIMITS } from '../generated/keyboard-macro';
import { validateKeyboardMacro } from './keyboard-macro-draft';

/** Browser-only synthetic sessions: no hooks, permission checks or native output */
export function createKeyboardMacroPreview(): PreviewHandlers {
    let active:
        | {
              session: KeyboardMacroSession;
              recording: KeyboardMacroRecording;
              held: PhysicalKey[];
              initiallyHeld: Set<PhysicalKey>;
              last: number;
              releaseDelay: number;
          }
        | undefined;
    // Require the requested preview recording session to remain active
    const requireSession = (id: string) => {
        if (!active || active.session.session_id !== id) throw Error('Recording session expired.');
        return active;
    };
    // Balance held keys and finalize the synthetic recording session
    const finish = (state: NonNullable<typeof active>) => {
        for (const [index, key] of [...state.held].reverse().entries())
            state.recording.events.push({
                kind: 'key_up',
                key,
                delay_ms: index === 0 ? state.releaseDelay : 0,
            });
        state.session.balanced_releases += state.held.length;
        state.held = [];
        state.releaseDelay = 0;
        state.session.state = 'stopped';
        state.session.event_count = state.recording.events.length;
        state.session.duration_ms = state.recording.events.reduce(
            (duration, event) => duration + event.delay_ms,
            0,
        );
    };
    return {
        // Advertise synthetic recording without native key hooks or playback
        keyboard_macro_capabilities: () => ({
            platform: 'unknown',
            native_warning: null,
            required_repeat_mode: null,
            new_action_repeat_mode: 'exact',
            supported_keys: [...PHYSICAL_KEYS],
            in_app_recording: true,
            global_recording: false,
            global_reason: 'Recording other applications is unavailable in browser preview.',
            native_playback: false,
            playback_reason:
                'Browser preview cannot send native keys. Use Simulate to review this draft.',
            test_playback: false,
            test_reason:
                'Browser preview cannot select a native scratch target or send keys. Use Simulate to review this draft.',
            emergency_shortcut: 'Ctrl+Shift+F12',
            global_emergency_shortcut: 'Unavailable in browser preview',
            max_events: KEYBOARD_MACRO_LIMITS.maxEvents,
            max_duration_ms: KEYBOARD_MACRO_LIMITS.maxDurationMs,
        }),
        // Start an isolated synthetic keyboard recording session
        start_keyboard_macro_recording: ({ mode, initiallyHeld }) => {
            if (mode !== 'in_app')
                throw Error('Global recording is unavailable in browser preview.');
            if (active && active.session.state !== 'stopped')
                throw Error('Another recording is active.');
            active = {
                session: {
                    session_id: crypto.randomUUID(),
                    state: initiallyHeld.length ? 'waiting_for_release' : 'recording',
                    event_count: 0,
                    duration_ms: 0,
                    balanced_releases: 0,
                    error: null,
                },
                recording: { version: 1, platform: 'unknown', key_mode: 'physical', events: [] },
                held: [],
                initiallyHeld: new Set(initiallyHeld),
                last: 0,
                releaseDelay: 0,
            };
            return structuredClone(active.session);
        },
        // Validate and append events to the active synthetic recording
        append_keyboard_macro_events: ({ sessionId, events }) => {
            const state = requireSession(sessionId);
            if (state.session.state === 'stopped') return structuredClone(state.session);
            if (!['recording', 'waiting_for_release'].includes(state.session.state))
                throw Error('Recording is stopped.');
            for (const event of events) {
                if (
                    !Number.isSafeInteger(event.elapsed_ms) ||
                    event.elapsed_ms < state.last ||
                    event.elapsed_ms > 300000 ||
                    !PHYSICAL_KEYS.includes(event.key)
                )
                    throw Error('Invalid synthetic event.');
                if (state.initiallyHeld.size) {
                    if (event.kind === 'key_up') state.initiallyHeld.delete(event.key);
                    state.last = event.elapsed_ms;
                    if (!state.initiallyHeld.size) state.session.state = 'recording';
                    continue;
                }
                if (event.kind === 'key_up' && !state.held.includes(event.key)) continue;
                if (
                    event.key === 'F12' &&
                    event.kind === 'key_down' &&
                    state.held.some((key) => key.startsWith('Control')) &&
                    state.held.some((key) => key.startsWith('Shift'))
                ) {
                    // Only unmatched modifier presses after the last committed event
                    // belong to this stop prefix. A completed release is history, even
                    // when another key is still held; never delete it or its matching press
                    const discardedPresses = new Set<PhysicalKey>();
                    state.releaseDelay = event.elapsed_ms - state.last;
                    while (state.recording.events.length) {
                        const tail = state.recording.events.at(-1)!;
                        if (!/^(Control|Shift)/.test(tail.key) || tail.kind === 'key_up') break;
                        state.recording.events.pop();
                        state.releaseDelay += tail.delay_ms;
                        if (tail.kind === 'key_down') discardedPresses.add(tail.key);
                    }
                    state.held = state.held.filter((key) => !discardedPresses.has(key));
                    state.last = event.elapsed_ms;
                    finish(state);
                    break;
                }
                const nextHeld =
                    state.held.length +
                    (event.kind === 'key_down' ? 1 : event.kind === 'key_up' ? -1 : 0);
                // Reserve one release for every owned press, just like the native service
                if (
                    state.recording.events.length + 1 + nextHeld >
                    KEYBOARD_MACRO_LIMITS.maxEvents
                ) {
                    state.releaseDelay = event.elapsed_ms - state.last;
                    state.last = event.elapsed_ms;
                    finish(state);
                    break;
                }
                if (event.kind === 'key_down') {
                    if (state.held.includes(event.key)) throw Error('Duplicate press.');
                    state.held.push(event.key);
                } else if (event.kind === 'repeat') {
                    if (!state.held.includes(event.key)) throw Error('Repeat without press.');
                } else if (event.kind === 'key_up')
                    state.held.splice(state.held.indexOf(event.key), 1);
                else throw Error('Unknown event kind.');
                state.recording.events.push({
                    kind: event.kind,
                    key: event.key,
                    delay_ms: event.elapsed_ms - state.last,
                });
                state.last = event.elapsed_ms;
            }
            state.session.event_count = state.recording.events.length;
            state.session.duration_ms = state.recording.events.reduce(
                (n, event) => n + event.delay_ms,
                0,
            );
            return structuredClone(state.session);
        },
        // Return the current synthetic recording session state
        heartbeat_keyboard_macro_recording: ({ sessionId }) =>
            structuredClone(requireSession(sessionId).session),
        // Balance and stop the synthetic keyboard recording
        stop_keyboard_macro_recording: ({ sessionId }) => {
            const state = requireSession(sessionId);
            finish(state);
            if (state.recording.events.length) {
                const error = validateKeyboardMacro({
                    kind: 'keyboard_macro',
                    recording: state.recording,
                    playback: { speed: 1, focus_policy: 'stay_on_target', repeat_mode: 'exact' },
                });
                if (error) throw Error(error);
            }
            const result = structuredClone({ session: state.session, recording: state.recording });
            return result;
        },
        // Discard the active synthetic recording session
        cancel_keyboard_macro_recording: ({ sessionId }) => {
            if (active && active.session.session_id !== sessionId)
                throw Error('Recording session expired.');
            active = undefined;
        },
        // Reject native macro testing in browser preview
        begin_keyboard_macro_test: () => {
            throw Error('Native scratch-target playback is unavailable in browser preview.');
        },
        // Reject native macro playback in browser preview
        start_keyboard_macro_test: () => {
            throw Error('Native scratch-target playback is unavailable in browser preview.');
        },
        // Reject status requests because browser preview has no native macro test
        keyboard_macro_test_status: () => {
            throw Error('No native playback test exists in browser preview.');
        },
        // Acknowledge cancellation without a native macro test
        cancel_keyboard_macro_test: () => null,
    };
}
