import type { RuntimeEvent } from './types';

export interface RuntimeEventEntry extends RuntimeEvent {
    logId: number;
    occurrences: number;
    timestamp_ms: number;
}

// Routine visual updates can arrive for every frame. Group matching updates
// across intervening events so a busy display cannot evict actionable failures
export function recordRuntimeEvent(
    entries: RuntimeEventEntry[],
    event: RuntimeEvent,
    logId: number,
    receivedAt = Date.now(),
): RuntimeEventEntry[] {
    const index = ['plugin-feedback', 'live-feedback'].includes(event.kind)
        ? entries.findIndex(
              (entry) =>
                  entry.kind === event.kind &&
                  entry.device_id === event.device_id &&
                  entry.message === event.message &&
                  entry.control == null &&
                  event.control == null,
          )
        : -1;
    const previous = entries[index];
    const entry = {
        ...event,
        timestamp_ms: event.timestamp_ms ?? receivedAt,
        logId: previous?.logId ?? logId,
        occurrences: Math.min(Number.MAX_SAFE_INTEGER, (previous?.occurrences ?? 0) + 1),
    };
    return [entry, ...entries.filter((_, i) => i !== index)].slice(0, 100);
}

// Choose a concise runtime-event description without exposing unnecessary payload details
export function runtimeEventDescription(event: RuntimeEventEntry): string {
    const description =
        event.control?.control_id ||
        event.message ||
        (event.kind === 'plugin-feedback'
            ? 'Plugin display updated'
            : event.kind === 'plugin-health'
              ? 'Plugin health changed'
              : 'Runtime updated');
    return event.occurrences > 1 ? `${description} · ${event.occurrences} updates` : description;
}
