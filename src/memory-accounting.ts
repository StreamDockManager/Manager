import type { MemoryAccounting } from '../generated/runtime';

const mib = 1024 * 1024;
const labels: Record<string, string> = {
    manager: 'Manager process',
    webkit: 'Verified WebKit helpers',
    native_plugins: 'Native plugins',
    other: 'Other verified helpers',
};
const reasons: Record<string, string> = {
    unsupported: 'This measurement is not supported on this platform.',
    permission_denied: 'Permission to measure this process was denied.',
    process_exited: 'The process exited before it could be measured.',
    identity_mismatch: 'The process identity changed while sampling.',
    partial_read: 'Only part of this process’s memory data could be read.',
    sample_unavailable: 'This process measurement is unavailable.',
};
function memoryValue(bytes: number | null | undefined) {
    return bytes == null || !Number.isFinite(bytes) || bytes < 0
        ? 'Unknown'
        : `${(bytes / mib).toLocaleString('en-US', { maximumFractionDigits: 1 })} MiB`;
}

/** Presentation only: no sampling, aggregation across metrics, or history/alert changes. */
export function memoryAccountingView(
    accounting: MemoryAccounting | undefined,
    scope: 'manager' | 'combined',
    now: number,
    failed = false,
) {
    if (!accounting?.timestamp_ms || !accounting.platform) accounting = undefined;
    const footprint = accounting?.primary_metric === 'physical_footprint';
    const residentLabel =
        accounting?.platform === 'windows' ? 'Working set' : 'Resident memory (RSS)';
    const metric = footprint ? 'Physical footprint' : residentLabel;
    const age = accounting ? Math.max(0, Math.floor((now - accounting.timestamp_ms) / 1000)) : null;
    const freshness = !accounting
        ? 'Memory breakdown unavailable. No attributed process sample.'
        : `${failed ? 'Refresh failed. ' : ''}${age! > 3 ? 'Last sample' : 'Sampled'} ${age}s ago · ${new Date(accounting.timestamp_ms).toLocaleTimeString()}${age! > 3 ? ' · Stale' : ''}`;
    const groups = (accounting?.groups || [])
        .filter((group) => scope === 'combined' || group.kind !== 'native_plugins')
        .map((group) => {
            const value = memoryValue(footprint ? group.footprint_bytes : group.resident_bytes);
            return {
                key: group.kind,
                name: labels[group.kind] || 'Other verified helpers',
                value,
                status: group.complete ? 'Complete' : 'Partial',
                note:
                    group.note ||
                    (group.complete
                        ? 'All attributed processes measured.'
                        : 'Some processes or measurements are unavailable.'),
                metric: `${metric}${!group.complete && value !== 'Unknown' ? ' · Known subtotal' : ''}`,
                identity:
                    group.kind === 'manager' && group.rows.length === 1
                        ? `PID ${group.rows[0].pid}`
                        : '',
                hasRows: group.rows.length > 0,
                empty: group.complete && group.rows.length === 0,
                details: `Process details (${group.rows.length})`,
                rows: group.rows.map((row) => ({
                    key: `${row.pid}:${row.created_seconds}`,
                    name: row.role,
                    identity: `PID ${row.pid} · Started ${new Date(row.created_seconds * 1000).toLocaleString()}`,
                    primary: memoryValue(footprint ? row.footprint_bytes : row.resident_bytes),
                    resident: memoryValue(row.resident_bytes),
                    note:
                        !footprint && row.resident_bytes != null
                            ? ''
                            : reasons[row.note] || row.note,
                })),
            };
        });
    return {
        metric,
        residentLabel,
        footprint,
        freshness,
        groups,
        explanation: footprint
            ? 'Compare each PID’s physical footprint with the corresponding Activity Monitor process, allowing for sample timing. Process sums are not unique physical RAM and may differ from grouped or system-wide usage.'
            : accounting?.platform === 'windows'
              ? 'Windows working-set measurements are not macOS physical footprint. Process sums are not unique physical RAM.'
              : 'Resident memory (RSS) is not macOS physical footprint. Process sums are not unique physical RAM.',
    };
}
