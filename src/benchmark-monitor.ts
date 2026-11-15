import type AlpineType from '@alpinejs/csp';
import type { BenchmarkProgressStore } from './benchmark-progress';

// Format a benchmark status identifier as a readable label
export function benchmarkLabel(value: string) {
    const label = value.replaceAll('_', ' ');
    return label ? label[0].toUpperCase() + label.slice(1) : 'Not started';
}

// Explain the backend’s output-restoration status without claiming optical verification
export function benchmarkRestoration(status: string) {
    const labels: Record<string, string> = {
        not_required: 'No device output restoration required.',
        not_finalized: 'Restoration will be checked when the run stops.',
        cleanup_requested: 'Stopping safely and requesting output restoration.',
        restore_requested_not_optically_verified:
            'Output restoration requested. Verify the device display.',
        pending_or_failed_reconnect_required:
            'Output restoration needs attention. Reconnect the device before continuing.',
        unknown: 'Output restoration could not be confirmed. Check the device.',
    };
    return labels[status] || benchmarkLabel(status);
}

// Create the shared benchmark progress-dialog model
export function createBenchmarkMonitor(read: () => BenchmarkProgressStore) {
    return {
        presentedRun: '',
        actionError: '',
        stopping: false,
        // Read the latest progress from the shared benchmark store
        get progress() {
            return read().progress;
        },
        // Summarize measured queue, transfer and throughput metrics
        get liveSummary() {
            const metrics = this.progress.live_metrics;
            if (!metrics || !this.running) return '';
            // Format a measured latency or mark it unavailable
            const ms = (value: number | null) =>
                value == null ? 'Unavailable' : `${value.toFixed(1)} ms`;
            const rate = metrics.completed_updates_per_second;
            return `Pending: ${metrics.pending} (${metrics.pending_bytes} bytes) · in flight: ${metrics.in_flight} · oldest pending: ${ms(metrics.oldest_pending_ms)} · dropped/replaced: ${metrics.dropped} · failed: ${metrics.failed} · host surface updates/sec: ${rate == null ? 'Warming up' : rate.toFixed(1)} · queue p95: ${ms(metrics.queue_p95_ms)} · transfer p95: ${ms(metrics.transfer_p95_ms)} · lost diagnostic outcomes: ${metrics.outcomes_lost}`;
        },
        // Report whether the benchmark is still running
        get running() {
            return this.progress.running;
        },
        // Choose a title from cancellation, running and terminal status
        get title() {
            if (this.progress.cancelling) return 'Stopping benchmark';
            if (this.running) return 'Benchmark running';
            const status = read().report?.status || this.progress.phase;
            if (status === 'completed') return 'Benchmark complete';
            if (status === 'cancelled') return 'Benchmark cancelled';
            if (status === 'failed') return 'Benchmark failed';
            return 'Benchmark status';
        },
        // Format the current benchmark phase
        get phase() {
            return benchmarkLabel(this.progress.phase);
        },
        // Clamp reported completion to a finite fraction
        get completion() {
            const value = this.progress.progress;
            return Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0;
        },
        // Format completion as a whole percentage
        get percentage() {
            return `${Math.round(this.completion * 100)}%`;
        },
        // Explain the current output-restoration status
        get restoration() {
            return benchmarkRestoration(this.progress.restoration_status);
        },
        // Identify restoration states requiring user attention
        get needsAttention() {
            return ['unknown', 'pending_or_failed_reconnect_required'].includes(
                this.progress.restoration_status,
            );
        },
        // Combine distinct operation and progress errors
        get error() {
            return [
                ...new Set(
                    [this.actionError, this.progress.error_summary, read().error].filter(Boolean),
                ),
            ].join(' · ');
        },
        // Allow cancellation only while an available run can still stop
        get canStop() {
            return read().available && this.running && !this.progress.cancelling && !this.stopping;
        },
        // Allow retry when the result is unavailable or failed
        get canRetryResult() {
            return ['failed', 'not_ready'].includes(read().resultState);
        },
        // Report whether the final benchmark report is loading
        get loadingResult() {
            return read().resultState === 'loading';
        },
        // Present each newly running benchmark once
        reconcile(dialog: HTMLDialogElement) {
            if (
                !this.running ||
                !this.progress.run_id ||
                this.progress.run_id === this.presentedRun
            )
                return;
            this.presentedRun = this.progress.run_id;
            this.actionError = '';
            this.open(dialog);
        },
        // Open the progress dialog if it is not already open
        open(dialog: HTMLDialogElement) {
            if (!dialog.open) dialog.showModal();
        },
        // Close the progress dialog without stopping its run
        close(dialog: HTMLDialogElement) {
            dialog.close();
        },
        // Request cancellation and keep a visible operation error
        async stop() {
            if (!this.canStop) return;
            this.stopping = true;
            this.actionError = '';
            try {
                await read().cancel();
            } catch (error) {
                this.actionError = String(error);
            } finally {
                this.stopping = false;
            }
        },
        // Retry loading the final benchmark report
        async retryResult() {
            await read().retryResult();
        },
    };
}

// Register the benchmark progress dialog controller
export function registerBenchmarkMonitor(
    Alpine: typeof AlpineType,
    read: () => BenchmarkProgressStore,
) {
    Alpine.data('benchmarkMonitor', () => ({
        monitor: createBenchmarkMonitor(read),
        // Synchronize the dialog with shared progress changes
        init() {
            this.$watch('$store.benchmarkProgress.progress', () => this.sync());
            this.$nextTick(() => this.sync());
        },
        // Reconcile the mounted dialog with the current run
        sync() {
            const dialog = this.$refs.progress as HTMLDialogElement | undefined;
            if (dialog) this.monitor.reconcile(dialog);
        },
        // Show the benchmark progress dialog
        show() {
            this.monitor.open(this.$refs.progress as HTMLDialogElement);
        },
        // Hide the progress dialog if it is mounted
        hide() {
            const dialog = this.$refs.progress as HTMLDialogElement | undefined;
            if (dialog) this.monitor.close(dialog);
        },
        // Forward cancellation to the monitor model
        async stop() {
            await this.monitor.stop();
        },
        // Retry the report through the monitor model
        async retryResult() {
            await this.monitor.retryResult();
        },
        // Open diagnostics to view the benchmark results
        viewResults() {
            this.hide();
            location.hash = '/diagnostics';
        },
        // Close the progress dialog during controller disposal
        destroy() {
            this.hide();
        },
    }));
}
