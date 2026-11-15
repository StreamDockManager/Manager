import type { PluginHealth } from '../generated/runtime';
/** Health events use Unix milliseconds, not the monotonic uptime clock. */
export function pluginFailureSummary(health: PluginHealth, now = Date.now()) {
    const events = health.events.filter((event) =>
        ['crash', 'failed_start'].includes(event.category),
    );
    const recent = events
        .filter((event) => now >= event.timestamp_ms && now - event.timestamp_ms <= 600000)
        .reduce((sum, event) => sum + event.occurrences, 0);
    const latest = [...events].sort((a, b) => b.timestamp_ms - a.timestamp_ms)[0];
    return `${recent} recorded failures in the last 10 minutes. ${latest ? `Last failure: ${new Date(latest.timestamp_ms).toLocaleString()} · ${latest.message}` : health.last_exit ? `Last exit: ${health.last_exit}` : 'No failure recorded.'}`;
}
