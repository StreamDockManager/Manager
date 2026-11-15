import * as fileDialogs from './window-dialogs';
import type { AppState } from './state';
import type { Diagnostic } from './types';
import type { ManagerResourceReport, Metric } from '../generated/runtime';
import { native } from './bridge';
import { resourceGraphs } from './resource-graphs';
import { eventTime } from './event-time';
import { runtimeEventDescription } from './runtime-events';

type Visibility = Pick<Document, 'hidden' | 'addEventListener' | 'removeEventListener'>;
type Preferences = Pick<Storage, 'getItem' | 'setItem'>;
const resourceUsageKey = 'stream-dock-manager.diagnostics.resource-usage';

// Create resource sampling and diagnostic-report controls
export function createDiagnostics(
    app: () => AppState,
    visibility: Visibility = document,
    preferences?: Preferences,
) {
    let resourceUsageEnabled = true;
    try {
        if (!native) preferences ??= globalThis.localStorage;
        resourceUsageEnabled = preferences?.getItem(resourceUsageKey) !== 'false';
    } catch {}
    return {
        report: { metrics: [], cache: { bytes: 0, hits: 0, misses: 0 } } as {
            metrics: Metric[];
            cache: { bytes: number; hits: number; misses: number };
            manager?: ManagerResourceReport;
            combined?: ManagerResourceReport;
        },
        timer: undefined as ReturnType<typeof setInterval> | undefined,
        visibilityListener: undefined as (() => void) | undefined,
        closingListener: undefined as (() => void) | undefined,
        resumedListener: undefined as (() => void) | undefined,
        windowClosing: false,
        active: true,
        initialized: false,
        sampling: false,
        sampleEpoch: 0,
        sampleFailed: false,
        sampledAt: Date.now(),
        resourceUsageEnabled,
        preferenceEpoch: 0,
        // Apply the resource sampling checkbox selection
        changeResourceUsage(event: Event) {
            this.setResourceUsage((event.currentTarget as HTMLInputElement).checked);
        },
        // Persist resource sampling preference and reconcile its polling lifecycle
        setResourceUsage(enabled: boolean) {
            if (this.resourceUsageEnabled === enabled) return;
            this.preferenceEpoch++;
            this.resourceUsageEnabled = enabled;
            try {
                if (native)
                    void app()
                        .bridge.command('save_client_preference', {
                            name: 'diagnostics_sampling',
                            value: enabled,
                        })
                        .catch((error) => app().notify(String(error), 'error'));
                else preferences?.setItem(resourceUsageKey, String(enabled));
            } catch {}
            this.stopSampling();
            if (enabled) {
                if (this.initialized) void this.startSampling();
            } else {
                this.report = { metrics: this.report.metrics, cache: this.report.cache };
                this.sampleFailed = false;
            }
        },
        // Stop scheduled resource sampling
        stopSampling() {
            if (this.timer !== undefined) clearInterval(this.timer);
            this.timer = undefined;
            this.sampleEpoch++;
        },
        // Start sampling only while the diagnostics view is eligible
        async startSampling() {
            if (
                !this.active ||
                this.windowClosing ||
                !this.resourceUsageEnabled ||
                visibility.hidden
            )
                return;
            if (this.timer === undefined) this.timer = setInterval(() => void this.sample(), 1000);
            await this.sample();
        },
        resourceScope: 'manager' as 'manager' | 'combined',
        // Show Manager-only resource accounting
        showManagerResources() {
            this.resourceScope = 'manager';
        },
        // Show combined Manager and plugin resource accounting
        showCombinedResources() {
            this.resourceScope = 'combined';
        },
        // Name the selected resource accounting scope
        get resourceScopeLabel() {
            return this.resourceScope === 'manager' ? 'Manager' : 'Manager + plugins';
        },
        // Select the resource histories shown by the graphs
        get resourceGraphs() {
            return resourceGraphs(
                this.report[this.resourceScope],
                this.sampledAt,
                this.sampleFailed,
            );
        },
        // Explain limitations of the selected resource accounting scope
        get resourceNote() {
            return this.resourceScope === 'manager'
                ? 'Manager, its child processes and identified WebKit helpers; plugin executables are excluded.'
                : 'Manager and all running plugin processes, including their child processes and identified WebKit helpers. Shared processes are counted once.';
        },
        // Describe process accounting for the selected resource scope
        get resourceProcessNote() {
            return this.report[this.resourceScope]?.process_note || '';
        },
        // Fetch resource samples while rejecting stale responses
        async sample() {
            if (
                visibility.hidden ||
                this.windowClosing ||
                !this.active ||
                !this.resourceUsageEnabled
            )
                return;
            if (this.sampling) {
                this.sampledAt = Date.now();
                return;
            }
            this.sampling = true;
            const epoch = this.sampleEpoch;
            try {
                const report = await app().bridge.command('performance');
                if (!this.active || !this.resourceUsageEnabled || epoch !== this.sampleEpoch)
                    return;
                this.report = report;
                this.sampleFailed = false;
            } catch {
                if (this.active && this.resourceUsageEnabled && epoch === this.sampleEpoch)
                    this.sampleFailed = true;
            } finally {
                if (epoch === this.sampleEpoch) this.sampledAt = Date.now();
                this.sampling = false;
                // A request already in flight cannot be cancelled. After re-enabling, discard
                // its old result and collect a fresh sample without overlapping requests
                if (
                    epoch !== this.sampleEpoch &&
                    this.active &&
                    !this.windowClosing &&
                    this.resourceUsageEnabled &&
                    !visibility.hidden
                )
                    void this.sample();
            }
        },
        // Format a diagnostic measurement or its unavailable state
        format(value: number | null) {
            return value === null ? '—' : value.toFixed(2);
        },
        exportPreview: '',
        exportId: '',
        exportBusy: false,
        exportEpoch: 0,
        $refs: {} as Record<string, HTMLElement>,
        // Prepare a redacted diagnostic report for user review
        async exportReport() {
            if (!native) {
                app().notify('Diagnostic export requires the desktop application.', 'warning');
                return;
            }
            if (this.exportBusy) return;
            const epoch = ++this.exportEpoch;
            this.exportBusy = true;
            try {
                const document = await app().command('preview_diagnostics');
                const id = JSON.parse(document).report_id as string;
                if (!this.active || epoch !== this.exportEpoch) {
                    void app()
                        .bridge.command('discard_diagnostics', { reportId: id })
                        // Observe cleanup failure for a report that is no longer current
                        .catch(() => {});
                    return;
                }
                this.exportPreview = document;
                this.exportId = id;
                (this.$refs.exportDialog as HTMLDialogElement).showModal();
            } catch {
            } finally {
                this.exportBusy = false;
            }
        },
        // Cancel the report review and release its prepared export
        cancelExport() {
            this.exportEpoch++;
            if (this.exportId)
                void app()
                    .bridge.command('discard_diagnostics', { reportId: this.exportId })
                    // Observe export cleanup failures after cancelling review
                    .catch(() => {});
            this.exportId = '';
            this.exportPreview = '';
            (this.$refs.exportDialog as HTMLDialogElement)?.close();
        },
        // Save the reviewed diagnostic report to the chosen destination
        async saveExport() {
            if (!this.exportId || this.exportBusy) return;
            const id = this.exportId;
            this.exportBusy = true;
            try {
                const path = await fileDialogs.save({
                    defaultPath: 'stream-dock-manager-diagnostics.json',
                    filters: [{ name: 'JSON', extensions: ['json'] }],
                });
                if (path && this.active && this.exportId === id) {
                    await app().command('export_diagnostics', { path, reportId: id });
                    this.cancelExport();
                    app().notify('Diagnostic report exported', 'success');
                }
            } catch {
            } finally {
                this.exportBusy = false;
            }
        },
        // Stop polling and release prepared report resources
        destroy() {
            this.active = false;
            this.cancelExport();
            this.stopSampling();
            if (this.visibilityListener)
                visibility.removeEventListener('visibilitychange', this.visibilityListener);
            if (typeof window !== 'undefined') {
                if (this.closingListener)
                    window.removeEventListener('editor-window-closing', this.closingListener);
                if (this.resumedListener)
                    window.removeEventListener('editor-window-resumed', this.resumedListener);
            }
        },
        rows: [] as Diagnostic[],
        // Read the current application state for diagnostics
        get app() {
            return app();
        },
        // Connect diagnostics sampling to window and visibility events
        async init() {
            if (native) {
                const epoch = this.preferenceEpoch;
                try {
                    const value = await app().bridge.command('client_preferences');
                    if (this.active && epoch === this.preferenceEpoch)
                        this.resourceUsageEnabled = value.diagnostics_sampling;
                } catch (error) {
                    app().notify(String(error), 'error');
                }
            }
            // Stop sampling while the editor window is closing
            this.closingListener = () => {
                this.windowClosing = true;
                this.stopSampling();
            };
            // Resume sampling after the editor window becomes active
            this.resumedListener = () => {
                this.windowClosing = false;
                void this.startSampling();
            };
            if (typeof window !== 'undefined') {
                window.addEventListener('editor-window-closing', this.closingListener);
                window.addEventListener('editor-window-resumed', this.resumedListener);
            }
            // Reconcile sampling when document visibility changes
            this.visibilityListener = () => {
                if (!this.initialized) return;
                if (visibility.hidden) this.stopSampling();
                else void this.startSampling();
            };
            visibility.addEventListener('visibilitychange', this.visibilityListener);
            await this.refresh();
            if (!this.active) return;
            this.initialized = true;
            await this.startSampling();
        },
        // Refresh diagnostic state and resource samples
        async refresh() {
            try {
                const rows = await app().command('diagnostics');
                if (this.active) this.rows = rows;
            } catch {}
        },
        // Expose runtime events matching the diagnostics view
        get events() {
            return app()
                .events.slice(0, 12)
                .map((event) => ({
                    ...event,
                    time: eventTime(event.timestamp_ms),
                    description: runtimeEventDescription(event),
                }));
        },
    };
}
