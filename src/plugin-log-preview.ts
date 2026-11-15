import type { PreviewHandlers } from './preview-handlers';
import type {
    LogCaptureStatus,
    LogCursor,
    LogEntry,
    LogNetworkDetail,
    LogPreferences,
    LogReaderLease,
    Snapshot,
} from '../generated/runtime';

// Browser-only captures: deterministic fixtures, no process execution or filesystem access
export function createPluginLogPreview() {
    let preferences: LogPreferences = { enabled: true, plugins: {} };
    const captures = new Map<string, { status: LogCaptureStatus; entries: LogEntry[] }>();
    const readers = new Map<string, LogReaderLease>();
    const networkDetails = new Map<string, LogNetworkDetail>();
    let serial = 0;
    // Append the new entry to the plugin log preview collection
    const append = (
        capture: { status: LogCaptureStatus; entries: LogEntry[] },
        stream: string,
        message: string,
        source = 'rpc',
    ) => {
        const p = preferences.plugins[capture.status.plugin_id] || {
            enabled: true,
            stdout: true,
            stderr: true,
            debug_network: false,
        };
        if (
            !capture.status.capturing ||
            !preferences.enabled ||
            !p.enabled ||
            !(stream === 'stderr' ? p.stderr : p.stdout)
        )
            return;
        capture.entries.push({
            sequence: ++capture.status.sequence,
            timestamp_ms: 1_790_000_000_000 + capture.status.sequence,
            stream,
            source,
            run_id: 'fixture-run',
            message,
            continuation: false,
        });
        capture.status.bytes += message.length + 160;
        capture.status.waiting = false;
    };
    // Read the requested plugin log preview value
    const get = (id: string) => {
        const capture = [...captures.values()].find((c) => c.status.id === id);
        if (!capture) throw Error('Capture no longer exists');
        return capture;
    };
    return {
        // Respond to input in the plugin log preview workflow
        onInput() {
            for (const c of captures.values()) {
                append(c, 'stdout', 'Fixture action completed in 12 ms');
                append(c, 'stderr', 'Fixture action failed: dependency unavailable');
                if (preferences.plugins[c.status.plugin_id]?.debug_network) {
                    append(
                        c,
                        'stdout',
                        'HTTP GET api.example.test/status?token=[redacted] · HTTP 200 · 12 ms',
                        'network',
                    );
                    const entry = c.entries.at(-1)!;
                    entry.network_request_id = String(entry.sequence);
                    networkDetails.set(`${c.status.id}:${entry.sequence - 1}`, {
                        request_id: entry.network_request_id,
                        request_headers: {
                            entries: [
                                { name: 'Authorization', values: ['[redacted]'] },
                                { name: 'Content-Type', values: ['application/json'] },
                                { name: 'X-Trace', values: ['request-one', 'request-two'] },
                            ],
                            truncated: false,
                        },
                        response_headers: {
                            entries: [
                                { name: 'Content-Type', values: ['application/json'] },
                                { name: 'Set-Cookie', values: ['[redacted]', '[redacted]'] },
                                { name: 'X-Trace', values: ['response-fixture'] },
                                { name: 'X-Debug', values: ['<img src=x onerror=alert(1)>'] },
                            ],
                            truncated: false,
                        },
                        request: {
                            text: '{"operation":"fixture","token":"[redacted]"}',
                            encoding: 'utf8',
                            content_type: 'application/json',
                            bytes: 48,
                            complete: true,
                            truncated: false,
                        },
                        response: {
                            text: '{"ok":true,"result":"<script>fixture</script>"}',
                            encoding: 'utf8',
                            content_type: 'application/json',
                            bytes: 52,
                            complete: true,
                            truncated: false,
                        },
                    });
                }
            }
        },
        // Respond to delete in the plugin log preview workflow
        onDelete(id: string) {
            const capture = captures.get(id);
            if (capture) capture.status.capturing = false;
        },
        // Build browser-only log commands backed by the current sample snapshot and reader leases
        handlers(snapshot: Snapshot) {
            return {
                // Simulate plugin log network detail in the browser without calling native services
                plugin_log_network_detail: (args) => {
                    const lease = args.lease as LogReaderLease;
                    const cursor = args.cursor as LogCursor;
                    if (
                        !readers.has(lease.id) ||
                        lease.capture_id !== cursor.capture_id ||
                        lease.generation !== cursor.generation
                    )
                        throw Error('Log reader is closed');
                    const detail = networkDetails.get(`${cursor.capture_id}:${cursor.offset}`);
                    if (!detail) throw Error('This entry has no captured network bodies');
                    return structuredClone(detail);
                },
                // Simulate plugin log preferences in the browser without calling native services
                plugin_log_preferences: (args) => {
                    return structuredClone(preferences);
                },
                // Simulate plugin log plugins in the browser without calling native services
                plugin_log_plugins: (args) => {
                    return snapshot.plugins.map(({ id, name }) => ({ id, name }));
                },
                // Simulate save plugin log preferences in the browser without calling native services
                save_plugin_log_preferences: (args) => {
                    preferences = structuredClone(args.value as LogPreferences);
                    for (const c of captures.values())
                        if (
                            !preferences.enabled ||
                            preferences.plugins[c.status.plugin_id]?.enabled === false
                        )
                            c.status.capturing = false;
                    return undefined;
                },
                // Simulate plugin log captures in the browser without calling native services
                plugin_log_captures: (args) => {
                    return structuredClone([...captures.values()].map((c) => c.status));
                },
                // Simulate start plugin log capture in the browser without calling native services
                start_plugin_log_capture: (args) => {
                    const id = String(args.pluginId);
                    if (!preferences.enabled || preferences.plugins[id]?.enabled === false)
                        throw Error('Plugin logging is disabled in preferences.');
                    let c = captures.get(id);
                    if (!c) {
                        c = {
                            status: {
                                id: `capture-${++serial}`,
                                plugin_id: id,
                                name: snapshot.plugins.find((p) => p.id === id)?.name || id,
                                capturing: true,
                                waiting: true,
                                incomplete: false,
                                error: null,
                                generation: 1,
                                bytes: 0,
                                sequence: 0,
                            },
                            entries: [],
                        };
                        captures.set(id, c);
                        const fixture = new URLSearchParams(location.search).get('pluginLogs');
                        if (fixture && fixture !== 'waiting') {
                            for (let i = 1; i <= 620; i++)
                                append(
                                    c,
                                    i % 3 === 0 ? 'stderr' : 'stdout',
                                    i % 3 === 0
                                        ? `Action ${i} failed: dependency unavailable`
                                        : `Action ${i} completed in 12 ms`,
                                );
                            if (
                                fixture === 'stopped' ||
                                fixture === 'failure' ||
                                fixture === 'limit'
                            )
                                c.status.capturing = false;
                            if (fixture === 'failure' || fixture === 'limit') {
                                c.status.incomplete = true;
                                c.status.error =
                                    fixture === 'limit'
                                        ? 'Plugin capture reached its 256 MiB limit.'
                                        : 'Cannot write captured logs; check free disk space.';
                            }
                        }
                    }
                    return structuredClone(c.status);
                },
                // Simulate stop plugin log capture in the browser without calling native services
                stop_plugin_log_capture: (args) => {
                    get(args.id).status.capturing = false;
                    return undefined;
                },
                // Simulate resume plugin log capture in the browser without calling native services
                resume_plugin_log_capture: (args) => {
                    const c = get(args.id);
                    if (
                        !preferences.enabled ||
                        preferences.plugins[c.status.plugin_id]?.enabled === false ||
                        c.status.incomplete
                    )
                        throw Error('Capture cannot resume');
                    c.status.capturing = true;
                    append(c, 'stdout', 'Capture resumed');
                    return undefined;
                },
                // Simulate clear plugin log capture in the browser without calling native services
                clear_plugin_log_capture: (args) => {
                    const c = get(args.id);
                    if (c.status.capturing)
                        throw Error('Stop capture before clearing captured logs.');
                    captures.delete(c.status.plugin_id);
                    return undefined;
                },
                // Simulate subscribe plugin logs in the browser without calling native services
                subscribe_plugin_logs: (args) => {
                    const c = get(args.id);
                    const lease = {
                        id: `reader-${++serial}`,
                        capture_id: c.status.id,
                        generation: 1,
                    };
                    readers.set(lease.id, lease);
                    return lease;
                },
                // Simulate unsubscribe plugin logs in the browser without calling native services
                unsubscribe_plugin_logs: (args) => {
                    readers.delete(String(args.id));
                    return undefined;
                },
                // Simulate read plugin logs in the browser without calling native services
                read_plugin_logs: (args) => {
                    const lease = args.lease as LogReaderLease;
                    if (!readers.has(lease.id)) throw Error('Log reader is closed');
                    const c = [...captures.values()].find((c) => c.status.id === lease.capture_id)!;
                    const cursor = args.cursor as LogCursor | null;
                    const boundary = cursor?.offset ?? c.entries.length;
                    const start = args.forward ? boundary : Math.max(0, boundary - 200);
                    const end = args.forward ? Math.min(c.entries.length, start + 200) : boundary;
                    // Read the plugin log preview item at the requested position
                    const at = (offset: number): LogCursor => ({
                        capture_id: c.status.id,
                        generation: 1,
                        offset,
                    });
                    return structuredClone({
                        status: c.status,
                        entries: c.entries
                            .slice(start, end)
                            .map((e, i) => ({ ...e, cursor: at(start + i) })),
                        earlier: start > 0 ? at(start) : null,
                        next: at(end),
                    });
                },
                // Simulate export plugin log capture in the browser without calling native services
                export_plugin_log_capture: (args) => {
                    return undefined;
                },
            } satisfies PreviewHandlers;
        },
    };
}
