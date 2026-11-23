import * as fileDialogs from './window-dialogs';
import Alpine from '@alpinejs/csp';
import type { BenchmarkProgressStore } from './benchmark-progress';
import { app } from './route-context';
import type { BenchmarkConfig } from '../generated/runtime';
import { benchmarkLabel, benchmarkRestoration } from './benchmark-monitor';
import { benchmarkSurfaceCounts } from './benchmark-surfaces';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
import { windowCloseOwners } from './window-close-owners';

// Create the default benchmark configuration
export const benchmarkDefaults = (): BenchmarkConfig => ({
    target: 'manager',
    pattern: 'numbered',
    device_id: null,
    workload: 'one_key',
    mode: 'individual_updates',
    target_fps: 10,
    warmup_seconds: 2,
    duration_seconds: 10,
    repetitions: 1,
    acknowledge_hardware: false,
});
// Read the shared benchmark progress store
const shared = () => Alpine.store('benchmarkProgress') as BenchmarkProgressStore;
// Register benchmark configuration and result controls
export function registerBenchmark() {
    Alpine.data('benchmark', () => ({
        config: benchmarkDefaults(),
        configBaseline: '',
        closeOwnerStop: undefined as (() => void) | undefined,
        // Expose the shared progress and report view
        get view() {
            return shared().view;
        },
        busy: false,
        actionError: '',
        // Expose the benchmark operation error
        get error() {
            return this.actionError;
        },
        // Expose the final-report loading error
        get resultError() {
            return shared().error || this.view.error || '';
        },
        // Report whether benchmark results are available
        get hasResults() {
            return !this.view.running && Boolean(shared().progress.run_id);
        },
        // Report whether final results are loading
        get loadingResult() {
            return shared().resultState === 'loading';
        },
        // Report whether result retrieval can be retried
        get canRetryResult() {
            return ['failed', 'not_ready'].includes(shared().resultState);
        },
        // Retry retrieving the final benchmark report
        async retryResult() {
            await shared().retryResult();
        },
        // Report whether the benchmark backend is available
        get available() {
            return shared().available;
        },
        // List devices eligible for benchmark selection
        get devices() {
            return app().snapshot?.devices.filter((d) => d.connected) || [];
        },
        // Resolve the selected benchmark device
        get selectedDevice() {
            return this.devices.find((device) => device.id === this.config.device_id);
        },
        // Count sections relevant to the selected benchmark target
        get sectionCount() {
            return benchmarkSurfaceCounts(this.selectedDevice?.manifest).sections;
        },
        // Describe the selected device’s key surfaces
        get allKeysLabel() {
            if (this.isPlugins) return 'All fixture keys (10)';
            const count = benchmarkSurfaceCounts(this.selectedDevice?.manifest).keys;
            return count ? `All LCD keys (${count})` : 'All LCD keys';
        },
        // Describe the selected device’s screen surfaces
        get allScreensLabel() {
            if (this.isPlugins) return 'All LCD sections (device benchmarks)';
            return this.sectionCount
                ? `All LCD sections (${this.sectionCount})`
                : 'All LCD sections';
        },
        // Describe the selected device’s mixed surface workload
        get mixedLabel() {
            return this.isPlugins
                ? 'Keys + touch fixture (14 sections)'
                : 'All LCD sections + background (when available)';
        },
        // Report whether the selected device has a background surface
        get hasBackground() {
            return this.selectedDevice?.manifest.capabilities.background_size != null;
        },
        // Reset target-dependent benchmark settings after selection changes
        targetChanged() {
            if (
                this.isPlugins &&
                !['one_key', 'two_keys', 'all_keys', 'mixed'].includes(this.config.workload)
            )
                this.config.workload = 'all_keys';
        },
        // Identify the preset matching the current benchmark configuration
        get preset() {
            const c = this.config;
            if (
                Number(c.duration_seconds) === 3 &&
                Number(c.warmup_seconds) === 0 &&
                Number(c.repetitions) === 1 &&
                c.mode === 'individual_updates'
            )
                return 'smoke';
            if (
                Number(c.duration_seconds) === 60 &&
                Number(c.warmup_seconds) === 10 &&
                Number(c.repetitions) === 3 &&
                c.mode === 'compare'
            )
                return 'qualification';
            if (
                Number(c.duration_seconds) === 300 &&
                Number(c.warmup_seconds) === 0 &&
                Number(c.repetitions) === 2 &&
                c.mode === 'compare'
            )
                return 'soak';
            return '';
        },
        // Expose individual benchmark run results
        get runs() {
            return this.view.report?.runs || [];
        },
        // Expose qualification limits reported by the benchmark
        get limitations() {
            return this.view.report?.limitations || [];
        },
        // Expose plugin-specific benchmark results
        get pluginResult() {
            return this.view.report?.plugin_fixture
                ? JSON.stringify(this.view.report.plugin_fixture, null, 2)
                : '';
        },
        // Summarize the completed benchmark report
        get reportSummary() {
            return this.view.report
                ? `${benchmarkLabel(this.view.report.measurement_scope)} · ${benchmarkLabel(this.view.report.status)}. ${benchmarkRestoration(this.view.report.restoration)}`
                : '';
        },
        // Report whether the configuration targets hardware
        get isHardware() {
            return this.config.target === 'hardware';
        },
        // Report whether the configuration targets plugins
        get isPlugins() {
            return this.config.target === 'plugins';
        },
        // Check whether the selected benchmark can be started
        get canStart() {
            return (
                this.available &&
                !this.busy &&
                !this.view.running &&
                (this.isPlugins || Boolean(this.config.device_id))
            );
        },
        // Check whether the current report can be exported
        get canExport() {
            return this.available && !this.view.running && Boolean(this.view.report);
        },
        // Prevent configuration changes during an active run
        get locked() {
            return this.busy || this.view.running;
        },
        // Format a benchmark number or an unavailable label
        format(value: number | null) {
            return value === null ? 'Not measured' : value.toFixed(2);
        },
        // Load benchmark configuration and connect close protection
        async init() {
            ownWindowMethods(this, 'benchmark configuration', ['start', 'export', 'retryResult']);
            if (!this.config.device_id && this.devices.length)
                this.config.device_id =
                    this.devices.find((device) => device.id === app().deviceId)?.id ||
                    this.devices[0].id;
            this.configBaseline = JSON.stringify(this.config);
            this.closeOwnerStop = registerCloseDraft(this, ['start', 'export'], {
                name: 'Benchmark configuration',
                // Report whether benchmark configuration has unsaved edits
                state: () =>
                    JSON.stringify(this.config) === this.configBaseline ? 'clean' : 'dirty',
                // Discard configuration edits when close protection requests it
                discard: () => {
                    this.config = JSON.parse(this.configBaseline) as BenchmarkConfig;
                    this.actionError = '';
                },
                retainOnUnmount: true,
            });
        },
        // Release benchmark controller registrations
        destroy() {
            this.closeOwnerStop?.();
            this.closeOwnerStop = undefined;
        },
        // Select the short smoke-test configuration
        smoke() {
            this.config.duration_seconds = 3;
            this.config.warmup_seconds = 0;
            this.config.repetitions = 1;
            this.config.mode = 'individual_updates';
        },
        // Select the qualification benchmark configuration
        qualification() {
            this.config.duration_seconds = 60;
            this.config.warmup_seconds = 10;
            this.config.repetitions = 3;
            this.config.mode = 'compare';
        },
        // Select the extended soak-test configuration
        soak() {
            this.config.duration_seconds = 300;
            this.config.warmup_seconds = 0;
            this.config.repetitions = 2;
            this.config.mode = 'compare';
        },
        // Start the configured benchmark and show its progress
        async start() {
            if (!this.canStart) return;
            this.busy = true;
            this.actionError = '';
            try {
                const config: BenchmarkConfig = {
                    ...this.config,
                    target_fps: Number(this.config.target_fps),
                    warmup_seconds: Number(this.config.warmup_seconds),
                    duration_seconds: Number(this.config.duration_seconds),
                    repetitions: Number(this.config.repetitions),
                };
                await shared().start(config);
                this.configBaseline = JSON.stringify(this.config);
                this.showProgress();
            } catch (error) {
                this.actionError = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Open the shared progress dialog
        showProgress(event?: Event) {
            if (windowCloseOwners.closing) return;
            if (event?.currentTarget instanceof HTMLButtonElement)
                event.currentTarget.focus({ preventScroll: true });
            window.dispatchEvent(new CustomEvent('open-benchmark-progress'));
        },
        // Export the current report as JSON
        async exportJson() {
            await this.export('json');
        },
        // Export the current report as Markdown
        async exportMarkdown() {
            await this.export('md');
        },
        // Export the current report as CSV
        async exportCsv() {
            await this.export('csv');
        },
        // Choose a destination and export the reviewed report
        async export(format: string) {
            if (!this.canExport) return;
            const { run_id: runId, report_revision: reportRevision } = shared().progress;
            if (reportRevision === null) return;
            const { save } = fileDialogs;
            const path = await save({
                defaultPath: `stream-dock-manager-benchmark.${format}`,
                filters: [{ name: format.toUpperCase(), extensions: [format] }],
            });
            if (!path) return;
            try {
                await shared().exportResult(path, format, runId, reportRevision);
                app().notify('Benchmark report exported', 'success');
            } catch (error) {
                this.actionError = String(error);
            }
        },
    }));
}
