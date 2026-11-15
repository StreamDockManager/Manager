const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;

// Format a valid event timestamp with a safe unavailable fallback
export function eventTime(timestamp: number | null | undefined) {
    const date = new Date(timestamp ?? NaN);
    if (!Number.isFinite(date.getTime()))
        return { label: 'Time unavailable', iso: '', title: 'Timestamp unavailable' };
    // Pad a timestamp component to its fixed display width
    const pad = (value: number, width = 2) => String(value).padStart(width, '0');
    const label = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`;
    return { label, iso: date.toISOString(), title: `${label} (${localZone})` };
}
