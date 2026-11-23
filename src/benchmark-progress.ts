import type {
    BenchmarkConfig,
    BenchmarkProgress,
    BenchmarkReport,
    BenchmarkView,
} from '../generated/runtime';

import type { Bridge } from './bridge';
export type BenchmarkBridge = Pick<Bridge, 'command'>;
// Create an idle benchmark progress snapshot
export const emptyProgress = (): BenchmarkProgress => ({
    live_metrics: null,
    run_id: '',
    revision: -1,
    running: false,
    cancelling: false,
    phase: 'Not started',
    progress: 0,
    error_summary: null,
    report_revision: null,
    restoration_status: 'not_required',
});

/** One instance per window. Poll snapshots recover missed transitions without an event stream */
export class BenchmarkProgressStore {
    progress = emptyProgress();
    report: BenchmarkReport | null = null;
    progressError = '';
    resultError = '';
    resultState: 'none' | 'loading' | 'ready' | 'not_ready' | 'expired' | 'failed' = 'none';
    readonly counts = {
        progressRequests: 0,
        resultRequests: 0,
        staleResponses: 0,
        unchangedSnapshots: 0,
    };
    private active = false;
    private generation = 0;
    private timer: ReturnType<typeof setTimeout> | undefined;
    private inFlight: Promise<void> | null = null;
    private queued = false;
    private resultKey = '';
    private resultAttempt = 0;
    private visibilityListener: (() => void) | undefined;
    // Initialize benchmark monitoring with backend availability and document visibility
    constructor(
        private bridge: BenchmarkBridge,
        readonly available: boolean,
        private visibility?: Pick<Document, 'hidden' | 'addEventListener' | 'removeEventListener'>,
    ) {}
    // Combine progress-fetch and final-report errors
    get error() {
        return [this.progressError, this.resultError].filter(Boolean).join(' · ');
    }
    // Project progress and report state for benchmark views
    get view(): BenchmarkView {
        const p = this.progress;
        return {
            live_metrics: p.live_metrics,
            run_id: p.run_id,
            running: p.running,
            cancelling: p.cancelling,
            progress: p.progress,
            phase: p.phase,
            error: p.error_summary,
            report: this.report,
        };
    }
    // Choose polling cadence from the current benchmark state
    private idleRepeats = 0;
    get cadence() {
        return this.progress.running
            ? this.visibility?.hidden
                ? 1000
                : 500
            : Math.min(15000, (this.visibility?.hidden ? 5000 : 2000) * 2 ** this.idleRepeats);
    }
    // Start progress monitoring for an active editor window
    startWindow() {
        if (this.active || !this.available) return;
        this.active = true;
        this.generation++;
        // Capture the Alpine reactive receiver, not the raw constructor instance
        this.visibilityListener = () => {
            void this.refresh();
        };
        this.visibility?.addEventListener('visibilitychange', this.visibilityListener);
        void this.refresh();
    }
    // Stop window polling without cancelling the backend run
    stopWindow() {
        this.active = false;
        this.generation++;
        this.queued = false;
        if (this.resultState === 'loading') {
            this.resultKey = '';
            this.resultState = 'none';
            this.resultAttempt++;
        }
        if (this.timer) clearTimeout(this.timer);
        this.timer = undefined;
        if (this.visibilityListener)
            this.visibility?.removeEventListener('visibilitychange', this.visibilityListener);
        this.visibilityListener = undefined;
    }
    // Schedule the next progress refresh
    private schedule(delay: number) {
        if (this.timer) clearTimeout(this.timer);
        if (!this.active) return;
        // Clear the elapsed timer before refreshing progress
        this.timer = setTimeout(() => {
            this.timer = undefined;
            void this.refresh();
        }, delay);
    }
    // Coalesce concurrent progress refresh requests
    refresh(): Promise<void> {
        if (!this.available || !this.active) return Promise.resolve();
        if (this.inFlight) {
            this.queued = true;
            return this.inFlight;
        }
        if (this.timer) clearTimeout(this.timer);
        this.timer = undefined;
        const generation = this.generation;
        this.counts.progressRequests++;
        // Read current progress and reconcile final-report loading
        this.inFlight = Promise.resolve().then(async () => {
            try {
                const next = await this.bridge.command('benchmark_progress');
                if (generation !== this.generation || !this.active) return;
                this.progressError = '';
                this.idleRepeats =
                    !next.running && next.revision === this.progress.revision
                        ? Math.min(3, this.idleRepeats + 1)
                        : 0;
                this.accept(next);
            } catch (error) {
                if (generation === this.generation && this.active)
                    this.progressError = String(error);
            } finally {
                this.inFlight = null;
                if (generation === this.generation && this.active) {
                    const again = this.queued;
                    this.queued = false;
                    this.schedule(again ? 0 : this.cadence);
                } else if (this.active) {
                    this.queued = false;
                    this.schedule(0);
                }
            }
        });
        return this.inFlight;
    }
    /** Never adopt an out-of-order snapshot, including mutation responses from another window */
    accept(next: BenchmarkProgress) {
        if (next.revision <= this.progress.revision) {
            if (next.revision < this.progress.revision || next.run_id !== this.progress.run_id)
                this.counts.staleResponses++;
            else this.counts.unchangedSnapshots++;
            if (
                next.revision === this.progress.revision &&
                next.run_id === this.progress.run_id &&
                next.report_revision !== null
            )
                void this.fetchResult();
            return;
        }
        const changedRun =
            next.run_id !== this.progress.run_id ||
            next.report_revision !== this.progress.report_revision;
        this.progress = next;
        if (changedRun) {
            this.report = null;
            this.resultError = '';
            this.resultState = 'none';
            this.resultKey = '';
            this.resultAttempt++;
        }
        if (next.report_revision !== null) void this.fetchResult();
    }
    // Load the final report while guarding against stale runs
    private async fetchResult(force = false) {
        const { run_id, report_revision } = this.progress;
        if (report_revision === null || !this.available || !this.active) return;
        const key = JSON.stringify([run_id, report_revision]);
        if (!force && this.resultKey === key) return;
        this.resultKey = key;
        const attempt = ++this.resultAttempt;
        const generation = this.generation;
        this.resultState = 'loading';
        this.resultError = '';
        this.counts.resultRequests++;
        try {
            const result = await this.bridge.command('benchmark_result', {
                runId: run_id,
                reportRevision: report_revision,
            });
            if (
                generation !== this.generation ||
                !this.active ||
                attempt !== this.resultAttempt ||
                this.progress.run_id !== run_id ||
                this.progress.report_revision !== report_revision
            ) {
                this.counts.staleResponses++;
                return;
            }
            if (
                result.status === 'ready' &&
                result.run_id === run_id &&
                result.report_revision === report_revision
            ) {
                this.report = result.report;
                this.resultState = 'ready';
            } else {
                this.report = null;
                this.resultState = result.status === 'not_ready' ? 'not_ready' : 'expired';
                this.resultError =
                    result.status === 'not_ready'
                        ? 'Benchmark result is not ready. Retry to fetch it.'
                        : 'Benchmark result expired. Start another run or review the current progress.';
            }
        } catch (error) {
            if (generation === this.generation && this.active && attempt === this.resultAttempt) {
                this.resultState = 'failed';
                this.resultError = `Result retrieval failed: ${String(error)}`;
            }
        }
    }
    // Retry a failed or not-yet-ready report request
    retryResult() {
        if (this.resultState === 'loading') return Promise.resolve();
        return this.fetchResult(true);
    }
    // Start a benchmark and refresh the shared progress state
    async start(config: BenchmarkConfig) {
        if (!this.available) throw Error('Benchmarks require the native desktop application.');
        const generation = this.generation;
        const next = await this.bridge.command('benchmark_start_progress', {
            config,
        });
        if (generation !== this.generation || !this.active) return;
        this.progressError = '';
        this.accept(next);
        this.schedule(this.cadence);
    }
    // Cancel the active benchmark and refresh its progress
    async cancel() {
        if (!this.available) return;
        const generation = this.generation;
        const next = await this.bridge.command('benchmark_cancel_progress');
        if (generation !== this.generation || !this.active) return;
        this.progressError = '';
        this.accept(next);
        this.schedule(this.cadence);
    }
    // Export the exact reviewed run and report revision
    async exportResult(path: string, format: string, runId: string, reportRevision: number) {
        if (!this.available)
            throw Error('Benchmark export requires the native desktop application.');
        await this.bridge.command('benchmark_export', { path, format, runId, reportRevision });
    }
}
