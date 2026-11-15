import { formatBinarySize } from './binary-size';
import type { PluginHealth, PluginRuntime, PluginRuntimeSnapshot } from '../generated/runtime';

// Create a disabled runtime snapshot with unavailable resource measurements
export function emptyPluginRuntime(): PluginRuntime {
    return {
        state: 'disabled',
        reason: 'disabled',
        demand: [],
        run_id: null,
        started_at_ms: null,
        last_wake_reason: null,
        last_wake_at_ms: null,
        uptime_ms: null,
        resources: {
            state: 'not_running',
            source: 'unavailable',
            cpu_percent: null,
            resident_bytes: null,
            peak_resident_bytes: 0,
            peak_cpu_percent: null,
            process_count: null,
            root_pid: null,
            sampled_at_ms: null,
            history: [],
        },
        network: {
            state: 'unavailable',
            source: 'unavailable',
            coverage: 'unavailable',
            sent_bytes: null,
            received_bytes: null,
            upload_bytes_per_second: null,
            download_bytes_per_second: null,
            sampled_at_ms: null,
        },
    };
}
// Create an empty health snapshot with zero counters and no recorded failures
export function emptyPluginHealth(): PluginHealth {
    return {
        restart_attempts: 0,
        recent_failures: [],
        missing_assets: [],
        unsupported_apis: [],
        totals: { crashes: 0, failed_starts: 0, automatic_restarts: 0, issues: 0 },
        session: { crashes: 0, failed_starts: 0, automatic_restarts: 0, issues: 0 },
        events: [],
        last_registration_ms: null,
        last_unexpected_exit_ms: null,
        last_issue_ms: null,
        last_exit: null,
        last_exit_code: null,
        last_exit_signal: null,
        blocked_reason: null,
        last_run_peak_resident_bytes: null,
    };
}
// Format the bytes
export function formatBytes(value: number | null | undefined): string {
    return formatBinarySize(value);
}
// Format the uptime
export function formatUptime(value: number | null | undefined): string {
    if (value == null) return 'Not running';
    const seconds = Math.max(0, Math.floor(value / 1000));
    return seconds < 60
        ? `${seconds}s`
        : seconds < 3600
          ? `${Math.floor(seconds / 60)}m ${seconds % 60}s`
          : `${Math.floor(seconds / 3600)}h ${Math.floor(seconds / 60) % 60}m`;
}

// Format tracked network rates and totals without implying complete network coverage
export function pluginNetworkSummary(network: PluginRuntime['network']) {
    const coverage =
        network.state === 'unavailable'
            ? 'Network: unavailable'
            : 'Tracked network payload · Partial coverage';
    if (network.state === 'unavailable') return { coverage, rates: '', totals: '' };
    if (network.state === 'stale') return { coverage, rates: 'Stale', totals: '' };
    return {
        coverage,
        rates:
            network.state === 'warming_up'
                ? 'Rates warming up'
                : `Upload ${formatBytes(network.upload_bytes_per_second)}/s · Download ${formatBytes(network.download_bytes_per_second)}/s`,
        totals: `Sent ${formatBytes(network.sent_bytes)} · Received ${formatBytes(network.received_bytes)}`,
    };
}

// The event contains a full snapshot; replacing the index releases deleted plugins
export function indexPluginRuntime(
    snapshots: PluginRuntimeSnapshot[],
): Record<string, PluginRuntimeSnapshot> {
    return Object.fromEntries(snapshots.map((snapshot) => [snapshot.id, snapshot]));
}

/** Missing readings break the line; sample spacing reflects real elapsed time. */
export function pluginResourceTrend(
    history: PluginRuntime['resources']['history'],
    metric: string,
): string {
    const samples = history.slice(-301);
    // Return the current plugin runtime value
    const value = (sample: (typeof samples)[number]) =>
        metric === 'cpu' ? sample.cpu_percent : sample.resident_bytes;
    // Report whether the current plugin runtime value is valid
    const valid = (n: number | null): n is number => n != null && Number.isFinite(n) && n >= 0;
    const peak = Math.max(1, ...samples.map(value).filter(valid));
    const end = samples.at(-1)?.timestamp_ms ?? 0;
    const start = end - 300_000;
    let connected = false;
    let previous = 0;
    return (
        samples
            // Complete the deferred plugin runtime work using the current operation state
            .flatMap((sample) => {
                const n = value(sample);
                if (
                    !valid(n) ||
                    sample.timestamp_ms < start ||
                    !Number.isFinite(sample.timestamp_ms)
                ) {
                    connected = false;
                    return [];
                }
                // Sampling is 1s visible / 5s hidden. Longer gaps are not observations
                if (sample.timestamp_ms - previous > 10_000 || sample.timestamp_ms < previous)
                    connected = false;
                const command = connected ? 'L' : 'M';
                connected = true;
                previous = sample.timestamp_ms;
                return [
                    `${command}${(((sample.timestamp_ms - start) / 300_000) * 280).toFixed(2)},${(50 - (n / peak) * 45).toFixed(2)}`,
                ];
            })
            .join(' ')
    );
}

// Describe the last wake reason and time or state that no start was recorded
export function pluginWakeLabel(runtime: PluginRuntime): string {
    if (runtime.last_wake_at_ms == null || runtime.last_wake_reason == null)
        return 'No start recorded this session';
    const reasons: Record<string, string> = {
        required_by_active_profile: 'active profile demand',
        manual_action: 'manual action',
        configuration: 'configuration',
        unexpected_exit: 'retry after an unexpected exit',
        registration_timeout: 'retry after registration timed out',
        user_restart: 'requested restart',
    };
    return `Last start attempt: ${new Date(runtime.last_wake_at_ms).toLocaleString()} · ${reasons[runtime.last_wake_reason] ?? runtime.last_wake_reason.replaceAll('_', ' ')}`;
}
