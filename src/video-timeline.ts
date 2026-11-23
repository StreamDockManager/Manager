export type TimelineMode = 'in' | 'out' | 'window' | 'playhead';
export function timelinePosition(
    x: number,
    left: number,
    width: number,
    start: number,
    end: number,
) {
    return Math.round(
        start + Math.max(0, Math.min(1, (x - left) / Math.max(1, width))) * (end - start),
    );
}
/** A window move preserves duration; only edge drags change it. Values are microseconds. */
export function timelineRange(
    mode: TimelineMode,
    delta: number,
    start: number,
    end: number,
    minimum: number,
    maximum: number,
    minimumSpan: number,
): [number, number] {
    const span = Math.max(1, Math.min(minimumSpan, maximum - minimum));
    if (mode === 'in') return [Math.max(minimum, Math.min(end - span, start + delta)), end];
    if (mode === 'out') return [start, Math.min(maximum, Math.max(start + span, end + delta))];
    const shift = Math.max(minimum - start, Math.min(maximum - end, delta));
    return [start + shift, end + shift];
}
