import type { ManagerResourceReport, ManagerResourceSample } from '../generated/runtime';

export interface ResourceGraph {
    id: string;
    name: string;
    value: string;
    note: string;
    scale: string;
    description: string;
    path: string;
    lastX: number;
    lastY: number;
    hasPoint: boolean;
}
const WINDOW = 300_000;
const mib = 1024 * 1024;
const definitions = [
    { id: 'cpu', name: 'CPU', field: 'cpu_percent', unit: '%' },
    { id: 'gpu', name: 'GPU', field: 'gpu_percent', unit: '%' },
    { id: 'ram', name: 'RAM', field: 'resident_bytes', unit: ' MiB' },
] as const;

/** Fixed time axis, honest gaps, and no invented zeroes for unavailable counters. */
export function resourceGraphs(
    report: ManagerResourceReport | undefined,
    now: number,
    failed = false,
): ResourceGraph[] {
    const samples = (report?.samples || [])
        .filter(
            (sample) =>
                Number.isFinite(sample.timestamp_ms) &&
                sample.timestamp_ms >= now - WINDOW &&
                sample.timestamp_ms <= now,
        )
        .slice(-300);
    // Convert each entry into the representation required by resource graphs
    return definitions.map(({ id, name, field, unit }) => {
        // Reject invalid resource samples and convert resident bytes to MiB for graphing
        const number = (sample: ManagerResourceSample): number | null => {
            const value = sample[field];
            if (
                value === null ||
                !Number.isFinite(value) ||
                value < 0 ||
                (id !== 'ram' && value > 100)
            )
                return null;
            return id === 'ram' ? value / mib : value;
        };
        const values = samples.map(number);
        const measured = values.filter((value): value is number => value !== null);
        const ceiling =
            id === 'ram'
                ? Math.max(64, 2 ** Math.ceil(Math.log2(Math.max(1, ...measured) * 1.1)))
                : 100;
        const latest = values.at(-1) ?? null;
        const stale = failed || !samples.length || now - samples.at(-1)!.timestamp_ms > 3500;
        const state =
            id === 'cpu' ? report?.cpu_state : id === 'gpu' ? report?.gpu_state : 'unavailable';
        const note = failed
            ? 'Could not refresh measurements.'
            : stale && report
              ? 'Waiting for a recent sample…'
              : latest !== null
                ? id === 'ram'
                    ? 'Resident memory'
                    : id === 'cpu'
                      ? 'Total logical CPU capacity'
                      : 'Process GPU engine activity'
                : state === 'warming_up'
                  ? 'Collecting a second sample…'
                  : id === 'gpu' && report
                    ? report.gpu_note
                    : 'No native measurement available.';
        // Format a resource-axis label with the metric's precision and unit
        const label = (value: number) => `${value.toFixed(id === 'ram' ? 0 : 1)}${unit}`;
        let path = '',
            lastX = 0,
            lastY = 0,
            previousAt: number | null = null;
        // Apply the resource graphs update to each selected entry
        samples.forEach((sample, i) => {
            const value = values[i];
            if (value === null || value === undefined) {
                previousAt = null;
                return;
            }
            const x = 6 + ((sample.timestamp_ms - (now - WINDOW)) / WINDOW) * 588;
            const y = 138 - (value / ceiling) * 132;
            const connected =
                previousAt !== null &&
                sample.timestamp_ms > previousAt &&
                sample.timestamp_ms - previousAt <= 2500;
            path += `${connected ? 'L' : 'M'}${x.toFixed(2)},${y.toFixed(2)} `;
            lastX = x;
            lastY = y;
            previousAt = sample.timestamp_ms;
        });
        const value = !stale && latest !== null ? label(latest) : '—';
        const range = measured.length
            ? ` Recorded range ${label(Math.min(...measured))} to ${label(Math.max(...measured))}.`
            : '';
        return {
            id,
            name,
            value,
            note,
            scale: `0–${ceiling}${unit}`,
            description: `${name}: ${value}. ${note}${range} Last five minutes.`,
            path: path.trim(),
            lastX,
            lastY,
            hasPoint: !stale && latest !== null,
        };
    });
}
