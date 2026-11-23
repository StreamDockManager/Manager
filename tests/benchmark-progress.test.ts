import { afterEach, expect, it, vi } from 'vitest';
import { createBenchmarkMonitor } from '../src/benchmark-monitor';
import {
    BenchmarkProgressStore,
    emptyProgress,
    type BenchmarkBridge,
} from '../src/benchmark-progress';
import type { BenchmarkProgress, BenchmarkReport, BenchmarkResult } from '../generated/runtime';
// Create benchmark progress fixtures with an explicit revision and optional field overrides
const progress = (
    revision: number,
    fields: Partial<BenchmarkProgress> = {},
): BenchmarkProgress => ({ ...emptyProgress(), revision, ...fields });
const report = { restoration: 'pending_or_failed_reconnect_required' } as BenchmarkReport;
// Expose promise settlement controls for deterministic benchmark race tests
const deferred = <T>() => {
    let resolve!: (v: T) => void;
    let reject!: (e: unknown) => void;
    const promise = new Promise<T>((a, b) => {
        resolve = a;
        reject = b;
    });
    return { promise, resolve, reject };
};
// Adapt a supplied command handler to the typed benchmark bridge interface
const bridge = (
    fn: (name: string, args?: Record<string, unknown>) => Promise<unknown>,
): BenchmarkBridge => ({
    // Dispatch the requested benchmark progress tests command
    command: <T>(n: string, a?: Record<string, unknown>) => fn(n, a) as Promise<T>,
});
// Flush pending benchmark progress tests work to its consumer
const flush = async () => {
    for (let i = 0; i < 8; i++) await Promise.resolve();
};
afterEach(() => vi.useRealTimers());
// Verify shell and page share one nonoverlapping stream; unchanged completed result is fetched once
it('shell and page share one nonoverlapping stream; unchanged completed result is fetched once', async () => {
    vi.useFakeTimers();
    const pending = deferred<BenchmarkProgress>();
    let calls = 0,
        results = 0;
    const store = new BenchmarkProgressStore(
        // Verify shell and page share one nonoverlapping stream; unchanged completed result is fetched once
        bridge(async (n) => {
            if (n === 'benchmark_progress') {
                calls++;
                return calls === 1
                    ? pending.promise
                    : progress(2, { run_id: 'one', report_revision: 2 });
            }
            results++;
            return { status: 'ready', run_id: 'one', report_revision: 2, report };
        }),
        true,
    );
    store.startWindow();
    store.startWindow();
    await flush();
    void store.refresh();
    void store.refresh();
    expect(calls).toBe(1);
    pending.resolve(progress(2, { run_id: 'one', report_revision: 2 }));
    await flush();
    await vi.advanceTimersByTimeAsync(10000);
    expect(results).toBe(1);
    expect(store.report).toBe(report);
    expect(store.counts.resultRequests).toBe(1);
    expect(calls).toBeLessThan(9);
    store.stopWindow();
    await vi.advanceTimersByTimeAsync(10000);
    expect(results).toBe(1);
});
// Verify out-of-order progress and delayed old results cannot resurrect a finished run after a new start
it('out-of-order progress and delayed old results cannot resurrect a finished run after a new start', async () => {
    vi.useFakeTimers();
    const old = deferred<BenchmarkResult>();
    let resultCalls = 0;
    const store = new BenchmarkProgressStore(
        // Verify out-of-order progress and delayed old results cannot resurrect a finished run after a new start
        bridge(async (n) =>
            n === 'benchmark_progress'
                ? progress(1)
                : n === 'benchmark_result'
                  ? (resultCalls++, old.promise)
                  : progress(5, { run_id: 'new', running: true }),
        ),
        true,
    );
    store.startWindow();
    await flush();
    store.accept(progress(3, { run_id: 'old', report_revision: 3 }));
    await flush();
    store.accept(progress(5, { run_id: 'new', running: true }));
    store.accept(progress(4, { run_id: 'old', running: false, report_revision: 3 }));
    old.resolve({ status: 'ready', run_id: 'old', report_revision: 3, report });
    await flush();
    expect(store.progress.run_id).toBe('new');
    expect(store.progress.running).toBe(true);
    expect(store.report).toBeNull();
    expect(resultCalls).toBe(1);
    store.stopWindow();
});
// Verify hidden active window keeps Stop state, failures preserve restoration, explicit retry recovers once
it('hidden active window keeps Stop state, failures preserve restoration, explicit retry recovers once', async () => {
    vi.useFakeTimers();
    let resultCalls = 0;
    const visibility = { hidden: true, addEventListener: vi.fn(), removeEventListener: vi.fn() };
    const store = new BenchmarkProgressStore(
        // Verify hidden active window keeps Stop state, failures preserve restoration, explicit retry recovers once
        bridge(async (n) => {
            if (n === 'benchmark_progress') return progress(1, { run_id: 'active', running: true });
            if (++resultCalls === 1) throw Error('temporary IPC failure');
            return { status: 'ready', run_id: 'active', report_revision: 3, report };
        }),
        true,
        visibility,
    );
    store.startWindow();
    await flush();
    expect(store.cadence).toBe(1000);
    store.accept(
        progress(3, {
            run_id: 'active',
            report_revision: 3,
            restoration_status: 'pending_or_failed_reconnect_required',
        }),
    );
    await flush();
    expect(store.error).toContain('temporary IPC failure');
    expect(store.progress.restoration_status).toBe('pending_or_failed_reconnect_required');
    store.accept(progress(3, { run_id: 'active', report_revision: 3 }));
    await flush();
    expect(resultCalls).toBe(1);
    await store.retryResult();
    expect(store.report).toBe(report);
    store.stopWindow();
    expect(visibility.removeEventListener).toHaveBeenCalledOnce();
});
// Verify unavailable/expired results are explicit and preview never sends native commands
it('unavailable/expired results are explicit and preview never sends native commands', async () => {
    // Verify unavailable/expired results are explicit and preview never sends native commands
    const command = vi.fn(async () => progress(0));
    const preview = new BenchmarkProgressStore(bridge(command), false);
    preview.startWindow();
    await preview.refresh();
    await preview.cancel();
    expect(command).not.toHaveBeenCalled();
    vi.useFakeTimers();
    const store = new BenchmarkProgressStore(
        // Verify unavailable/expired results are explicit and preview never sends native commands
        bridge(async (n) => (n === 'benchmark_progress' ? progress(1) : { status: 'expired' })),
        true,
    );
    store.startWindow();
    await flush();
    store.accept(progress(2, { run_id: 'old', report_revision: 2 }));
    await flush();
    expect(store.resultState).toBe('expired');
    expect(store.report).toBeNull();
    store.stopWindow();
});
// Verify mutation snapshots outrank pending polling and export uses captured run/revision
it('mutation snapshots outrank pending polling and export uses captured run/revision', async () => {
    vi.useFakeTimers();
    const pending = deferred<BenchmarkProgress>();
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const store = new BenchmarkProgressStore(
        // Verify mutation snapshots outrank pending polling and export uses captured run/revision
        bridge(async (n, a) => {
            calls.push([n, a]);
            if (n === 'benchmark_progress') return pending.promise;
            if (n === 'benchmark_cancel_progress')
                return progress(8, { run_id: 'run', running: true, cancelling: true });
            return null;
        }),
        true,
    );
    store.startWindow();
    await flush();
    await store.cancel();
    pending.resolve(progress(7, { run_id: 'run', running: false }));
    await flush();
    expect(store.progress.running).toBe(true);
    expect(store.progress.cancelling).toBe(true);
    await store.exportResult('/test/report.json', 'json', 'old', 3);
    expect(calls.at(-1)).toEqual([
        'benchmark_export',
        { path: '/test/report.json', format: 'json', runId: 'old', reportRevision: 3 },
    ]);
    store.stopWindow();
});
// Verify window disposal ignores late progress and results; reopening recovers a pending result
it('window disposal ignores late progress and results; reopening recovers a pending result', async () => {
    vi.useFakeTimers();
    const late = deferred<BenchmarkResult>();
    let results = 0;
    const store = new BenchmarkProgressStore(
        // Verify window disposal ignores late progress and results; reopening recovers a pending result
        bridge(async (n) =>
            n === 'benchmark_progress'
                ? progress(2, { run_id: 'one', report_revision: 2 })
                : ++results === 1
                  ? late.promise
                  : { status: 'ready', run_id: 'one', report_revision: 2, report },
        ),
        true,
    );
    store.startWindow();
    await flush();
    expect(store.resultState).toBe('loading');
    store.stopWindow();
    late.resolve({ status: 'ready', run_id: 'one', report_revision: 2, report });
    await flush();
    expect(store.report).toBeNull();
    store.startWindow();
    await flush();
    expect(store.report).toBe(report);
    expect(results).toBe(2);
    store.stopWindow();
});
// Verify visibility recovery keeps the reactive receiver rather than mutating the raw instance
it('visibility recovery keeps the reactive receiver rather than mutating the raw instance', async () => {
    vi.useFakeTimers();
    let revision = 0;
    const visibility = { hidden: false, addEventListener: vi.fn(), removeEventListener: vi.fn() };
    const raw = new BenchmarkProgressStore(
        // Verify visibility recovery keeps the reactive receiver rather than mutating the raw instance
        bridge(async () => progress(++revision)),
        true,
        visibility,
    );
    const writes: string[] = [];
    const store = new Proxy(raw, {
        // Verify visibility recovery keeps the reactive receiver rather than mutating the raw instance
        set(target, key, value) {
            writes.push(String(key));
            return Reflect.set(target, key, value);
        },
    });
    store.startWindow();
    await flush();
    // Verify visibility recovery keeps the reactive receiver rather than mutating the raw instance
    expect(writes.filter((k) => k === 'progress')).toHaveLength(1);
    const listener = visibility.addEventListener.mock.calls[0][1] as () => void;
    listener();
    await flush();
    // Verify visibility recovery keeps the reactive receiver rather than mutating the raw instance
    expect(writes.filter((k) => k === 'progress')).toHaveLength(2);
    store.stopWindow();
    expect(visibility.removeEventListener).toHaveBeenCalledWith('visibilitychange', listener);
});
// Verify independent windows each own one stream and observe the same cancellation revision
it('independent windows each own one stream and observe the same cancellation revision', async () => {
    vi.useFakeTimers();
    let current = progress(1, { run_id: 'shared', running: true });
    let calls = 0;
    // Verify independent windows each own one stream and observe the same cancellation revision
    const backend = bridge(async (n) => {
        if (n === 'benchmark_cancel_progress') {
            current = progress(2, { run_id: 'shared', running: true, cancelling: true });
            return current;
        }
        calls++;
        return current;
    });
    const a = new BenchmarkProgressStore(backend, true),
        b = new BenchmarkProgressStore(backend, true);
    a.startWindow();
    b.startWindow();
    await flush();
    expect(calls).toBe(2);
    await a.cancel();
    await b.refresh();
    expect(a.progress.cancelling).toBe(true);
    expect(b.progress.cancelling).toBe(true);
    a.stopWindow();
    b.stopWindow();
});

// Verify live benchmark metrics are projected without another request or history
it('live benchmark metrics are projected without another request or history', () => {
    const store = new BenchmarkProgressStore(
        // Verify live benchmark metrics are projected without another request or history
        bridge(async () => null),
        true,
    );
    const live_metrics = {
        pending: 2,
        in_flight: 1,
        pending_bytes: 300,
        oldest_pending_ms: 4,
        dropped: 5,
        failed: 0,
        outcomes_lost: 0,
        completed_updates_per_second: 12,
        queue_p95_ms: null,
        transfer_p95_ms: 3,
    };
    store.accept(progress(1, { running: true, live_metrics }));
    expect(store.view.live_metrics).toEqual(live_metrics);
    store.accept(progress(2, { running: true, live_metrics: null }));
    expect(store.view.live_metrics).toBeNull();
    expect(store.counts.progressRequests).toBe(0);
});

// Verify live monitor labels unavailable latency and never claims display FPS
it('live monitor labels unavailable latency and never claims display FPS', () => {
    const store = new BenchmarkProgressStore(
        // Verify live monitor labels unavailable latency and never claims display FPS
        bridge(async () => null),
        true,
    );
    // Verify live monitor labels unavailable latency and never claims display FPS
    const monitor = createBenchmarkMonitor(() => store);
    expect(monitor.liveSummary).toBe('');
    store.accept(
        progress(1, {
            running: true,
            live_metrics: {
                pending: 2,
                in_flight: 1,
                pending_bytes: 300,
                oldest_pending_ms: 4,
                dropped: 5,
                failed: 0,
                outcomes_lost: 0,
                completed_updates_per_second: null,
                queue_p95_ms: null,
                transfer_p95_ms: 3,
            },
        }),
    );
    expect(monitor.liveSummary).toContain('Warming up');
    expect(monitor.liveSummary).toContain('queue p95: Unavailable');
    expect(monitor.liveSummary).toContain('dropped/replaced: 5');
    expect(monitor.liveSummary).toContain('host surface updates/sec:');
    expect(monitor.liveSummary).not.toContain('FPS');
    store.accept(progress(2));
    expect(monitor.liveSummary).toBe('');
});

it('bounds idle polls and discovers an externally started run on the next fallback', async () => {
    vi.useFakeTimers();
    let running = false;
    const store = new BenchmarkProgressStore(
        bridge(async () =>
            running ? progress(2, { run_id: 'external', running: true }) : progress(1),
        ),
        true,
    );
    store.startWindow();
    await flush();
    await vi.advanceTimersByTimeAsync(4250);
    expect(store.counts.progressRequests).toBe(2);
    await vi.advanceTimersByTimeAsync(30000);
    expect(store.cadence).toBe(15000);
    running = true;
    await vi.advanceTimersByTimeAsync(15000);
    expect(store.progress.run_id).toBe('external');
    expect(store.cadence).toBe(500);
    store.stopWindow();
});
