export interface BinarySizeOptions {
    maxUnit?: number;
    precision?: number | readonly number[];
    fallback?: string;
}
/** Binary arithmetic with caller-owned display precision and maximum unit */
export function formatBinarySize(
    value: number | null | undefined,
    options: BinarySizeOptions = {},
): string {
    if (value == null || !Number.isFinite(value)) return options.fallback ?? 'Unavailable';
    const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
    const max = Math.max(0, Math.min(4, options.maxUnit ?? 4));
    let unit = 0;
    while (value >= 1024 && unit < max) {
        value /= 1024;
        unit++;
    }
    const precision = options.precision ?? 1;
    const digits = typeof precision === 'number' ? precision : (precision[unit] ?? 1);
    return `${unit === 0 ? value : value.toFixed(digits)} ${units[unit]}`;
}
