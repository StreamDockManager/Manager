export interface RateChoice {
    value: string;
    label: string;
}
// Offer five-FPS steps and retain the saved rate even when it exceeds the source rate
export function fpsChoices(source: number, saved?: number | null): RateChoice[] {
    if (!Number.isFinite(source) || source <= 0) return [{ value: 'source', label: 'Source rate' }];
    const endpoint = source;
    const rates: number[] = [];
    for (let rate = 5; rate < endpoint && rates.length < 8192; rate += 5) rates.push(rate);
    if (endpoint >= 5) rates.push(endpoint);
    if (saved && Number.isFinite(saved) && !rates.some((rate) => Math.abs(rate - saved) < 0.0005))
        rates.push(saved);
    rates.sort((a, b) => a - b);
    return [
        { value: 'source', label: `Source rate (${Number(endpoint.toFixed(3))} FPS)` },
        ...rates.map((rate) => ({
            value: String(rate),
            label: `${Number(rate.toFixed(3))}${rate > endpoint ? ' (saved)' : ''}`,
        })),
    ];
}
// Format microseconds as a nonnegative hours, minutes, seconds and milliseconds timecode
export function timecode(us: number): string {
    const ms = Math.max(0, Math.round(us / 1000));
    return `${String(Math.floor(ms / 3600000)).padStart(2, '0')}:${String(Math.floor(ms / 60000) % 60).padStart(2, '0')}:${String(Math.floor(ms / 1000) % 60).padStart(2, '0')}.${String(ms % 1000).padStart(3, '0')}`;
}
// Parse up to three time components and reject out-of-range minute or second fields
export function parseTimecode(value: string): number {
    const parts = value.trim().split(':');
    if (!parts.length || parts.length > 3 || parts.some((part) => !/^\d+(?:\.\d+)?$/.test(part)))
        throw Error('Enter a time such as 00:01:23.450');
    let seconds = 0;
    for (const [index, part] of parts.entries()) {
        const number = Number(part);
        if (!Number.isFinite(number) || (index > 0 && number >= 60))
            throw Error('Minutes and seconds must be below 60');
        seconds = seconds * 60 + number;
    }
    return Math.round(seconds * 1_000_000);
}
// Validate clip bounds, duration, frame-rate limits and output dimensions before encoding
export function clipValidation(
    start: number,
    end: number,
    duration: number,
    minimum: number,
    maximum: number | null | undefined,
    minEdge: number,
    maxEdge: number,
): string {
    if (
        ![start, end, duration].every(Number.isFinite) ||
        start < 0 ||
        end <= start ||
        end > duration
    )
        return 'Choose in and out points within the recording.';
    if (end - start > 30_000_000)
        return 'Clips can be up to 30 seconds. Move the in or out point closer.';
    if (
        !Number.isFinite(minimum) ||
        minimum <= 0 ||
        (maximum != null && (!Number.isFinite(maximum) || maximum <= 0 || minimum > maximum))
    )
        return 'Minimum FPS must be positive and no higher than maximum FPS.';
    if (
        ![minEdge, maxEdge].every((edge) => Number.isInteger(edge) && edge >= 64 && edge <= 1024) ||
        minEdge > maxEdge
    )
        return 'Detail bounds must be between 64 and 1024 pixels, with minimum no higher than maximum.';
    return '';
}
// Fit the timeline to the whole duration or a padded selection clamped to its bounds
export function timelineWindow(
    duration: number,
    start: number,
    end: number,
    selection: boolean,
): [number, number] {
    if (!selection) return [0, duration];
    const padding = Math.max(1_000_000, (end - start) / 8);
    return [Math.max(0, start - padding), Math.min(duration, end + padding)];
}
