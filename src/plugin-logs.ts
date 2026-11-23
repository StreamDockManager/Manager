import { cloneJson } from './json-clone';
import * as fileDialogs from './window-dialogs';
import type AlpineType from '@alpinejs/csp';
import type { Bridge } from './bridge';
import type { AppState } from './state';
import { eventTime } from './event-time';
import { ownWindowMethods, windowWork } from './window-work';
import type {
    LogCaptureStatus,
    LogCursor,
    LogEntry,
    LogNetworkBody,
    LogNetworkHeaders,
    LogNetworkDetail,
    LogPreferences,
    LogReaderLease,
} from '../generated/runtime';

interface Reading {
    tab: string;
    entries: LogEntry[];
    earlier: LogCursor | null;
    next: LogCursor | null;
    follow: boolean;
    historical: boolean;
    scroll: number;
}
export const LOG_READER_BYTES = 4 * 1024 * 1024;
// Account retained strings and entry/cursor metadata without serializing the page
const entryBytes = (entry: LogEntry) =>
    512 +
    2 *
        (entry.message.length +
            entry.stream.length +
            entry.source.length +
            entry.run_id.length +
            (entry.network_request_id?.length || 0) +
            (entry.cursor?.capture_id.length || 0));
export interface LogContext {
    snapshot: { plugins: { id: string; name: string }[] };
    notify: AppState['notify'];
}
interface LogWindowOptions {
    detached?: boolean;
    closeWindow?: () => Promise<void>;
    popOut?: (id: string, stream: string) => Promise<void>;
}
export class PluginLogs {
    captures: LogCaptureStatus[] = [];
    preferences: LogPreferences = { enabled: true, plugins: {} };
    visible = false;
    selected = '';
    requestedPlugin = '';
    busy = false;
    error = '';
    confirmingClear = false;
    lease: LogReaderLease | null = null;
    readings: Record<string, Reading> = {};
    epoch = 0;
    metadataEpoch = 0;
    private preferenceEpoch = 0;
    private windowStarted = false;
    timer = 0;
    pollTimer = 0;
    trigger: HTMLElement | null = null;
    position: Record<string, string> = {};
    seenErrors = new Map<string, string>();
    windowVisible = true;
    readerStarting = false;
    networkSequence = 0;
    networkDetail: LogNetworkDetail | null = null;
    networkLoading = false;
    networkError = '';
    networkEpoch = 0;
    private retiredLeases = new Set<string>();
    private releaseTasks = new Set<Promise<void>>();
    // Initialize plugin logs state and its required lifecycle hooks
    constructor(
        readonly bridge: Pick<Bridge, 'command'>,
        readonly app: () => LogContext,
        readonly options: LogWindowOptions = {},
    ) {}
    // Derive detached from the current plugin logs state
    get detached() {
        return !!this.options.detached;
    }
    // Derive active count from the current plugin logs state
    get activeCount() {
        return this.captures.filter((c) => c.capturing).length;
    }
    // Derive indicator from the current plugin logs state
    get indicator() {
        return this.activeCount
            ? `Logs capturing · ${this.activeCount}`
            : `Logs · ${this.captures.length}`;
    }
    // Derive compact indicator from the current plugin logs state
    get compactIndicator() {
        return '≡ ' + (this.activeCount || this.captures.length);
    }
    // Derive requested name from the current plugin logs state
    get requestedName() {
        return (
            this.capture?.name ||
            this.app().snapshot.plugins.find((p) => p.id === this.requestedPlugin)?.name ||
            this.requestedPlugin
        );
    }
    // Derive can start from the current plugin logs state
    get canStart() {
        return (
            !this.busy &&
            this.preferences.enabled &&
            this.pluginPreferences.enabled &&
            this.app().snapshot.plugins.some((p) => p.id === this.requestedPlugin)
        );
    }
    // Derive capture from the current plugin logs state
    get capture() {
        return this.captures.find((c) => c.id === this.selected);
    }
    // Derive reading from the current plugin logs state
    get reading(): Reading {
        return (this.readings[this.selected] ||= {
            tab: 'stderr',
            entries: [],
            earlier: null,
            next: null,
            follow: true,
            historical: false,
            scroll: 0,
        });
    }
    // Derive entries from the current plugin logs state
    get entries() {
        return this.reading.entries
            .filter((e) => e.stream === this.reading.tab)
            .map((entry) => ({ ...entry, time: eventTime(entry.timestamp_ms) }));
    }
    // Derive retained reader bytes from the current plugin logs state
    get retainedReaderBytes() {
        return Object.values(this.readings).reduce(
            (total, reading) =>
                total + reading.entries.reduce((sum, entry) => sum + entryBytes(entry), 0),
            0,
        );
    }
    // Derive title from the current plugin logs state
    get title() {
        return this.capture ? `${this.capture.name} logs` : 'Plugin captures';
    }
    // Derive status from the current plugin logs state
    get status() {
        const c = this.capture;
        return (
            c?.error ||
            (c?.capturing
                ? c.waiting
                    ? 'Capturing · waiting for plugin output'
                    : 'Capturing'
                : 'Capture stopped')
        );
    }
    // Derive plugin preferences from the current plugin logs state
    get pluginPreferences() {
        return (
            this.preferences.plugins[this.capture?.plugin_id || this.requestedPlugin] || {
                enabled: true,
                stdout: true,
                stderr: true,
                debug_network: false,
            }
        );
    }
    // Check whether a log capture is active for the specified plugin
    capturing(id: string) {
        return this.captures.some((c) => c.plugin_id === id && c.capturing);
    }
    // Check whether open is valid for the current plugin logs state
    isOpen(id: string) {
        return this.visible && this.capture?.plugin_id === id;
    }
    // Start the window
    startWindow() {
        if (this.windowStarted) return;
        this.windowStarted = true;
        this.onNavigate = () => {
            if (!this.detached) this.close(false);
        };
        this.onBlur = () => {
            if (!this.detached) this.close(false);
        };
        this.onResize = () => {
            if (this.visible) this.place();
        };
        // Complete the deferred plugin logs work using the current operation state
        this.onVisibility = () => {
            if (this.detached) this.updateReaderVisibility();
            else if (document.hidden) this.close(false);
            if (!document.hidden) {
                void this.refresh();
                if (this.visible) void this.refreshPreferences();
            }
            this.scheduleMetadata();
        };
        void this.refresh();
        this.scheduleMetadata();
        window.addEventListener('hashchange', this.onNavigate);
        window.addEventListener('blur', this.onBlur);
        window.addEventListener('resize', this.onResize);
        document.addEventListener('visibilitychange', this.onVisibility);
    }
    private scheduleMetadata() {
        clearTimeout(this.timer);
        this.timer = 0;
        if (!this.windowStarted || !this.windowVisible || document.hidden) return;
        this.timer = window.setTimeout(
            () => {
                void this.refresh();
                if (this.visible) void this.refreshPreferences();
                this.scheduleMetadata();
            },
            this.visible ? 2000 : 15000,
        );
    }
    // Stop the window
    stopWindow() {
        this.windowStarted = false;
        this.close(false);
        clearTimeout(this.timer);
        this.timer = 0;
        window.removeEventListener('hashchange', this.onNavigate);
        window.removeEventListener('blur', this.onBlur);
        window.removeEventListener('resize', this.onResize);
        document.removeEventListener('visibilitychange', this.onVisibility);
    }
    // Drain pending log work and release retired leases before closing the window
    async retireWindow() {
        this.stopWindow();
        // Complete late subscriptions and their compensating releases before taking
        // the final lease inventory. Main-window methods own their whole continuations
        await windowWork.settled();
        await Promise.all([...this.releaseTasks]);
        for (const id of [...this.retiredLeases]) await this.retireLease(id);
        if (this.retiredLeases.size) throw Error('Plugin log readers could not be released.');
    }
    // Track a log-reader release until the backend confirms its removal
    private retireLease(id: string): Promise<void> {
        this.retiredLeases.add(id);
        const task = this.bridge
            .command('unsubscribe_plugin_logs', { id })
            .then(() => {
                this.retiredLeases.delete(id);
            })
            .catch((error) => {
                this.error = String(error);
            })
            .finally(() => {
                this.releaseTasks.delete(task);
            });
        this.releaseTasks.add(task);
        return task;
    }
    // Respond to resize in the plugin logs workflow
    onResize = () => {
        if (this.visible) this.place();
    };
    // Respond to blur in the plugin logs workflow
    onBlur = () => this.close(false);
    // Respond to navigate in the plugin logs workflow
    onNavigate = () => this.close(false);
    // Respond to visibility in the plugin logs workflow
    onVisibility = () => {
        if (document.hidden) this.close(false);
    };
    // Refresh the preferences
    async refreshPreferences() {
        const epoch = ++this.preferenceEpoch;
        try {
            const preferences = await this.bridge.command('plugin_log_preferences');
            if (epoch === this.preferenceEpoch) this.preferences = preferences;
        } catch (error) {
            if (epoch === this.preferenceEpoch) this.error = String(error);
        }
    }
    // Set the window visible
    setWindowVisible(visible: boolean) {
        this.windowVisible = visible;
        this.scheduleMetadata();
        if (visible && this.visible) void this.refreshPreferences();
        this.updateReaderVisibility();
    }
    // Update the reader visibility
    updateReaderVisibility() {
        if (!this.windowVisible || (typeof document !== 'undefined' && document.hidden)) {
            if (this.visible && this.selected) this.reading.scroll = this.body()?.scrollTop || 0;
            this.release();
        } else if (this.visible && this.selected && !this.lease && !this.readerStarting) {
            void this.view(this.selected);
        }
    }
    // Show the captures
    showCaptures() {
        void this.refreshPreferences();
        this.release();
        this.selected = '';
        this.requestedPlugin = '';
        this.visible = true;
        this.scheduleMetadata();
        this.confirmingClear = false;
    }
    // Refresh plugin logs data while keeping the view synchronized
    async refresh() {
        const epoch = ++this.metadataEpoch;
        try {
            const captures = await this.bridge.command('plugin_log_captures');
            if (epoch !== this.metadataEpoch) return;
            this.captures = captures;
            if (this.selected && !captures.some((capture) => capture.id === this.selected)) {
                const id = this.selected;
                this.release();
                this.selected = '';
                delete this.readings[id];
                this.confirmingClear = false;
            }
            const present = new Set(captures.map((capture) => capture.id));
            for (const id of Object.keys(this.readings))
                if (!present.has(id)) delete this.readings[id];
            for (const id of this.seenErrors.keys())
                if (!present.has(id)) this.seenErrors.delete(id);
            for (const c of this.captures)
                if (c.error && this.seenErrors.get(c.id) !== c.error) {
                    this.seenErrors.set(c.id, c.error);
                    this.app().notify(`${c.name}: ${c.error}`, 'error');
                }
        } catch (e) {
            this.error = String(e);
        }
    }
    // Remember the triggering element and position the log popover beside it
    anchor(event: Event) {
        this.trigger = event.currentTarget as HTMLElement;
        this.place();
    }
    // Fit the attached log popover within the current viewport
    place() {
        if (this.detached) {
            this.position = {};
            return;
        }
        const r = this.trigger?.isConnected
            ? this.trigger.getBoundingClientRect()
            : { left: 12, bottom: 12 };
        const width = Math.min(680, window.innerWidth - 24);
        const left = Math.max(12, Math.min(r.left, window.innerWidth - width - 12));
        const top =
            window.innerWidth <= 600 || window.innerHeight <= 600
                ? 12
                : Math.max(
                      12,
                      Math.min(
                          r.bottom + 8,
                          window.innerHeight - Math.min(440, window.innerHeight - 24) - 12,
                      ),
                  );
        this.position = {
            left: `${left}px`,
            top: `${top}px`,
            width: `${width}px`,
            maxHeight: `${window.innerHeight - top - 12}px`,
        };
    }
    // Open the for
    async openFor(event: Event) {
        const id = (event.currentTarget as HTMLElement).dataset.id;
        if (!id) return;
        this.close(false);
        this.requestedPlugin = id;
        this.anchor(event);
        const epoch = this.epoch;
        this.busy = true;
        this.error = '';
        try {
            const existing = this.captures.find((c) => c.plugin_id === id);
            const capture =
                existing ||
                (await this.bridge.command('start_plugin_log_capture', { pluginId: id }));
            await this.refresh();
            if (epoch !== this.epoch) return;
            await this.view(capture.id);
        } catch (e) {
            if (epoch === this.epoch) {
                this.visible = true;
                this.scheduleMetadata();
                this.error = String(e);
                this.focus();
            }
        } finally {
            this.busy = false;
        }
    }
    // Open the capture overview and clear the previous log selection
    overview(event: Event) {
        void this.refreshPreferences();
        this.close(false);
        this.requestedPlugin = '';
        this.anchor(event);
        this.selected = '';
        this.visible = true;
        this.scheduleMetadata();
        this.focus();
    }
    // Open the capture identified by the clicked element
    async viewEvent(event: Event) {
        const id = (event.currentTarget as HTMLElement).dataset.id;
        if (id) await this.view(id);
    }
    // Subscribe to the selected capture and release leases returned after the view changes
    async view(id: string) {
        void this.refreshPreferences();
        this.release();
        const epoch = this.epoch;
        this.selected = id;
        this.requestedPlugin = this.capture?.plugin_id || this.requestedPlugin;
        this.visible = true;
        this.scheduleMetadata();
        this.error = '';
        this.confirmingClear = false;
        if (!this.windowVisible || (typeof document !== 'undefined' && document.hidden)) return;
        this.readerStarting = true;
        try {
            const lease = await this.bridge.command('subscribe_plugin_logs', { id });
            if (epoch !== this.epoch || !this.visible) {
                await this.retireLease(lease.id);
                return;
            }
            this.lease = lease;
            if (this.reading.next && this.reading.next.generation !== lease.generation) {
                this.reading.next = null;
                this.reading.earlier = null;
                this.reading.follow = true;
                this.reading.historical = false;
                this.reading.scroll = 0;
            }
            if (!this.reading.entries.length)
                await this.load(this.reading.next && !this.reading.follow ? 'resume' : 'recent');
            else this.restoreScroll();
            this.schedule();
            this.focus();
        } catch (e) {
            if (epoch === this.epoch) this.error = String(e);
        } finally {
            if (epoch === this.epoch) this.readerStarting = false;
        }
    }
    // Clear the network
    clearNetwork() {
        ++this.networkEpoch;
        this.networkSequence = 0;
        this.networkDetail = null;
        this.networkLoading = false;
        this.networkError = '';
    }
    // Load expanded network details or clear them when their entry collapses
    networkToggled(event: Event) {
        const element = event.currentTarget as HTMLDetailsElement;
        const sequence = Number(element.dataset.sequence);
        if (!element.open) {
            if (this.networkSequence === sequence) this.clearNetwork();
            return;
        }
        this.networkSequence = sequence;
        this.reading.follow = false;
        void this.loadNetwork();
    }
    // Load network for the plugin logs workflow
    async loadNetwork() {
        const epoch = ++this.networkEpoch;
        const entry = this.entries.find((entry) => entry.sequence === this.networkSequence);
        this.networkDetail = null;
        this.networkError = '';
        if (!this.lease || !entry?.cursor) {
            this.networkError = 'Reopen the reader to load these request details.';
            return;
        }
        this.networkLoading = true;
        try {
            const detail = await this.bridge.command('plugin_log_network_detail', {
                lease: this.lease,
                cursor: entry.cursor,
            });
            if (epoch === this.networkEpoch && this.visible) this.networkDetail = detail;
        } catch (error) {
            if (epoch === this.networkEpoch) this.networkError = String(error);
        } finally {
            if (epoch === this.networkEpoch) this.networkLoading = false;
        }
    }
    // Render captured header values and indicate capture-limit omissions
    headersText(headers?: LogNetworkHeaders) {
        if (!headers) return 'Headers were not captured for this entry.';
        const lines = headers.entries.flatMap((field) =>
            field.values.length
                ? field.values.map((value) => `${field.name}: ${value}`)
                : [`${field.name}:`],
        );
        if (headers.truncated) lines.push('[Additional headers omitted at the capture limit]');
        return lines.join('\n') || '(no headers)';
    }
    // Summarize captured body type, byte count, encoding and completeness
    bodyDescription(body: LogNetworkBody) {
        return [
            body.content_type || 'Unspecified content type',
            `${body.bytes} bytes read`,
            body.encoding === 'base64' ? 'Base64' : 'Text',
            !body.complete ? 'Partly read' : '',
            body.truncated ? 'Omitted: exceeds capture limit' : '',
        ]
            .filter(Boolean)
            .join(' · ');
    }
    // Show captured body text or explain why no body content is available
    bodyText(body: LogNetworkBody) {
        if (body.truncated) return 'Body omitted because it exceeds the 64 KiB capture limit.';
        if (!body.text && !body.complete)
            return 'No body bytes were read before the request ended.';
        return body.text || '(empty body)';
    }
    // Release resources retained by the plugin logs operation
    release() {
        this.clearNetwork();
        ++this.epoch;
        clearTimeout(this.pollTimer);
        const lease = this.lease;
        this.lease = null;
        this.readerStarting = false;
        // Cursor/tab/scroll state can be restored from the capture file. Closed
        // readers must not retain one decoded output page per visited capture
        for (const reading of Object.values(this.readings)) reading.entries = [];
        if (lease) void this.retireLease(lease.id);
    }
    // Release plugin logs state and associated lifecycle resources
    close(returnFocus = true) {
        if (this.visible && this.selected) this.reading.scroll = this.body()?.scrollTop || 0;
        this.visible = false;
        this.scheduleMetadata();
        this.release();
        this.confirmingClear = false;
        if (this.detached && returnFocus && this.options.closeWindow) {
            // Handle a failed plugin logs operation without retaining stale work
            void this.options.closeWindow().catch((error) => {
                this.visible = true;
                this.scheduleMetadata();
                this.error = String(error);
                this.app().notify(String(error), 'error');
            });
        }
        if (returnFocus && this.trigger?.isConnected) this.trigger.focus();
    }
    // Dismiss the plugin logs operation
    dismiss(event: Event) {
        if (this.detached || !this.visible) return;
        if (
            !event
                .composedPath()
                .some(
                    (node) =>
                        node instanceof Element &&
                        node.matches('.plugin-log-panel, [data-log-trigger]'),
                )
        )
            this.close(false);
    }
    // Escape text before it is used by plugin logs
    escape(event: KeyboardEvent) {
        if (this.visible && event.key === 'Escape') {
            event.preventDefault();
            event.stopPropagation();
            this.close();
        }
    }
    // Find the currently mounted log output element
    body() {
        return document.querySelector<HTMLElement>('.plugin-log-output');
    }
    // Focus the plugin logs operation
    focus() {
        requestAnimationFrame(() =>
            document.querySelector<HTMLElement>('.plugin-log-close')?.focus(),
        );
    }
    // Restore the scroll
    restoreScroll() {
        // Complete the deferred plugin logs work using the current operation state
        requestAnimationFrame(() => {
            const body = this.body();
            if (body)
                body.scrollTop = this.reading.follow ? body.scrollHeight : this.reading.scroll;
        });
    }
    // Remember scroll position and follow live output only near the bottom
    scrolled(event: Event) {
        const el = event.currentTarget as HTMLElement;
        this.reading.scroll = el.scrollTop;
        this.reading.follow =
            !this.reading.historical && el.scrollHeight - el.scrollTop - el.clientHeight < 8;
    }
    // Select the requested output stream and restore its scroll position
    tab(event: Event) {
        this.reading.tab = (event.currentTarget as HTMLElement).dataset.tab || 'stderr';
        this.restoreScroll();
    }
    // Switch and focus stdout or stderr tabs with arrow keys
    tabKey(event: KeyboardEvent) {
        if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
            event.preventDefault();
            this.reading.tab = this.reading.tab === 'stderr' ? 'stdout' : 'stderr';
            document
                .querySelector<HTMLElement>(`.plugin-log-tabs [data-tab="${this.reading.tab}"]`)
                ?.focus();
            this.restoreScroll();
        }
    }
    // Schedule the next plugin logs update
    schedule() {
        clearTimeout(this.pollTimer);
        // Run the deferred plugin logs update after its scheduled delay
        this.pollTimer = window.setTimeout(async () => {
            if (this.visible && this.lease && !document.hidden && this.windowVisible) {
                if (this.reading.follow && !this.reading.historical && !this.busy)
                    await this.load('live');
                this.schedule();
            }
        }, 500);
    }
    // Load the data required by the plugin logs workflow
    async load(mode = 'recent') {
        const lease = this.lease;
        if (
            !lease ||
            !this.visible ||
            !this.windowVisible ||
            (typeof document !== 'undefined' && document.hidden)
        )
            return;
        const epoch = this.epoch;
        const metadataEpoch = this.metadataEpoch;
        const reading = this.reading;
        const cursor =
            mode === 'earlier'
                ? reading.earlier
                : mode === 'live' || mode === 'resume'
                  ? reading.next
                  : null;
        try {
            const page = await this.bridge.command('read_plugin_logs', {
                lease,
                cursor,
                forward: mode === 'live',
            });
            if (epoch !== this.epoch || !this.visible) return;
            if (mode === 'live' && (!reading.follow || reading.historical)) return;
            if (mode === 'live') {
                if (!page.entries.length) return;
                reading.entries = [...reading.entries, ...page.entries].slice(-200);
                const first = reading.entries[0]?.cursor;
                reading.earlier = first && first.offset > 0 ? first : null;
            } else if (mode === 'resume') {
                reading.entries = page.entries;
                reading.earlier = page.earlier;
            } else {
                reading.entries = page.entries;
                reading.follow = mode !== 'earlier';
                reading.historical = mode === 'earlier';
                reading.scroll = 0;
                reading.earlier = page.earlier;
            }
            let bytes = 0;
            let first = reading.entries.length;
            while (first > 0 && reading.entries.length - first < 200) {
                const size = entryBytes(reading.entries[first - 1]);
                if (bytes + size > LOG_READER_BYTES) break;
                bytes += size;
                --first;
            }
            if (first) {
                if (first === reading.entries.length) {
                    reading.entries = [];
                    throw Error(
                        'This log entry exceeds the reader limit. Download the capture to inspect it.',
                    );
                }
                reading.entries = reading.entries.slice(first);
                const cursor = reading.entries[0]?.cursor;
                reading.earlier = cursor && cursor.offset > 0 ? cursor : null;
            }
            reading.next = page.next;
            const index = this.captures.findIndex((c) => c.id === page.status.id);
            if (index >= 0 && metadataEpoch === this.metadataEpoch)
                this.captures.splice(index, 1, page.status);
            this.restoreScroll();
        } catch (e) {
            if (epoch === this.epoch) this.error = String(e);
        }
    }
    // Load an older page of captured log entries
    earlier() {
        return this.load('earlier');
    }
    // Load the most recent captured log entries
    latest() {
        return this.load();
    }
    // Start the requested
    async startRequested() {
        if (!this.canStart) return;
        const epoch = this.epoch;
        this.busy = true;
        this.error = '';
        try {
            const capture = await this.bridge.command('start_plugin_log_capture', {
                pluginId: this.requestedPlugin,
            });
            await this.refresh();
            if (epoch === this.epoch) await this.view(capture.id);
        } catch (error) {
            if (epoch === this.epoch) this.error = String(error);
        } finally {
            this.busy = false;
        }
    }
    // Stop, resume or clear capture and refresh the affected log view
    async operate(operation: 'stop' | 'resume' | 'clear', id = this.selected) {
        if (!id || this.busy) return;
        this.busy = true;
        this.error = '';
        try {
            if (operation === 'stop') await this.bridge.command('stop_plugin_log_capture', { id });
            else if (operation === 'resume')
                await this.bridge.command('resume_plugin_log_capture', { id });
            else await this.bridge.command('clear_plugin_log_capture', { id });
            await this.refresh();
            if (operation === 'clear') {
                delete this.readings[id];
                if (this.selected === id) {
                    this.release();
                    this.selected = '';
                    this.confirmingClear = false;
                }
            }
        } catch (e) {
            this.error = String(e);
        } finally {
            this.busy = false;
        }
    }
    // Stop the current plugin logs operation
    stop() {
        return this.operate('stop');
    }
    // Resume the paused plugin logs operation
    resume() {
        return this.operate('resume');
    }
    // Clear the retained plugin logs value
    clear() {
        return this.operate('clear');
    }
    // Stop the event
    stopEvent(event: Event) {
        return this.operate('stop', (event.currentTarget as HTMLElement).dataset.id);
    }
    // Open the selected capture in a detached window and close its popover
    async popOut() {
        const id = this.selected;
        if (!id || this.busy || this.detached) return;
        const stream = this.reading.tab;
        this.busy = true;
        try {
            if (this.options.popOut) await this.options.popOut(id, stream);
            else await this.bridge.command('open_plugin_log_window', { id, stream });
            this.close(false);
        } catch (error) {
            this.error = String(error);
            this.app().notify('Could not open the log window: ' + String(error), 'error');
        } finally {
            this.busy = false;
        }
    }
    // Export the selected capture to a user-chosen JSONL file
    async download() {
        const id = this.selected;
        if (!id) return;
        this.busy = true;
        this.error = '';
        try {
            const { save } = fileDialogs;
            const path = await save({
                defaultPath: 'plugin-capture.jsonl',
                filters: [{ name: 'Structured logs', extensions: ['jsonl'] }],
            });
            if (path) await this.bridge.command('export_plugin_log_capture', { id, path });
        } catch (e) {
            if (this.selected === id) this.error = String(e);
            this.app().notify('Could not download captured logs: ' + String(e), 'error');
        } finally {
            this.busy = false;
        }
    }
    // Change the preference
    async changePreference(event: Event) {
        const input = event.currentTarget as HTMLInputElement;
        const key = input.dataset.preference;
        const value = structuredClone(cloneJson(this.preferences)) as LogPreferences;
        const pluginId = this.capture?.plugin_id || this.requestedPlugin;
        if (key === 'global') value.enabled = input.checked;
        else if (
            pluginId &&
            (key === 'enabled' || key === 'stdout' || key === 'stderr' || key === 'debug_network')
        ) {
            const prefs = (value.plugins[pluginId] ||= {
                enabled: true,
                stdout: true,
                stderr: true,
                debug_network: false,
            });
            prefs[key] = input.checked;
        }
        try {
            await this.bridge.command('save_plugin_log_preferences', { value });
            this.preferenceEpoch++;
            this.preferences = value;
            await this.refresh();
        } catch (e) {
            this.error = String(e);
        }
    }
}
// Register the plugin logs
export function registerPluginLogs(
    Alpine: typeof AlpineType,
    bridge: Pick<Bridge, 'command'>,
    app: () => LogContext,
    options: LogWindowOptions = {},
) {
    const logs = new PluginLogs(bridge, app, options);
    if (!options.detached)
        ownWindowMethods(logs, 'plugin logs', [
            'refreshPreferences',
            'refresh',
            'openFor',
            'view',
            'loadNetwork',
            'load',
            'startRequested',
            'operate',
            'popOut',
            'download',
            'changePreference',
        ]);
    Alpine.store('pluginLogs', logs);
    Alpine.data('pluginLogReader', () => ({
        displayedEntries: [] as PluginLogs['entries'],
        updateEntries() {
            this.displayedEntries =
                this.logs.visible && this.logs.selected ? this.logs.entries : [];
        },
        // Derive logs from the current plugin logs state
        get logs() {
            return Alpine.store('pluginLogs') as PluginLogs;
        },
    }));
}
