import { cloneJson } from './json-clone';
import * as fileDialogs from './window-dialogs';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { native } from './bridge';
import { reconcile } from './reconcile';
import { PollingReader } from './polling-reader';
import { windowCloseOwners } from './window-close-owners';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { ownWindowMethods } from './window-work';
import type {
    RuntimeStatus,
    RuntimeKind,
    RuntimeInstallation,
    RuntimeSelection,
    RuntimeUsage,
    RuntimeRelease,
    RuntimeRequirement,
    RuntimeJob,
} from '../generated/runtime';
const titles = { nodejs: 'Node.js' };
// Register the runtimes
export function registerRuntimes(Alpine: typeof AlpineType, app: () => AppState) {
    const reader = new PollingReader(
        // Complete the deferred runtimes work using the current operation state
        async () => {
            const [status, usage] = await Promise.all([
                app().command('runtime_status', undefined, false),
                app().command('runtime_usage', undefined, false),
            ]);
            return { status, usage };
        },
        ({ status }) =>
            status.jobs.some((job) => !['complete', 'failed', 'cancelled'].includes(job.state)),
    );
    Alpine.data('runtimes', () => ({
        status: null as RuntimeStatus | null,
        usage: [] as RuntimeUsage[],
        candidates: [] as RuntimeInstallation[],
        kind: 'nodejs' as RuntimeKind,
        line: '',
        version: '',
        pluginId: null as string | null,
        pluginName: '',
        requirement: null as RuntimeRequirement | null,
        source: 'private',
        acknowledged: false,
        error: '',
        busy: false,
        saved: false,
        removal: null as RuntimeInstallation | null,
        removing: false,
        removalError: '',
        unsubscribe: null as (() => void) | null,
        destroyed: false,
        installJobId: '',
        handledJobId: '',
        viewEpoch: 0,
        installEpoch: -1,
        installOwner: '',
        closeBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        resumeListener: null as (() => void) | null,
        // Close the value
        closeValue() {
            return JSON.stringify([this.line, this.version, this.source, this.acknowledged]);
        },
        // Derive current owner from the current runtimes state
        get currentOwner() {
            return JSON.stringify([this.kind, this.line, this.version, this.pluginId, this.source]);
        },
        // Derive cards from the current runtimes state
        get cards() {
            return (['nodejs'] as RuntimeKind[]).map((kind) => ({
                kind,
                title: titles[kind],
                channel: 'LTS',
                selected: this.status?.defaults[kind]?.installation_id || '',
                missing:
                    !!this.status?.defaults[kind]?.installation_id &&
                    !this.status?.installations.some(
                        (i) => i.id === this.status?.defaults[kind]?.installation_id,
                    ),
                releases: this.activeReleases,
                lines: this.lines(kind),
                installed:
                    this.status?.installations
                        .filter((i) => i.kind === kind)
                        .map((i) => ({
                            ...i,
                            support: this.lines(kind).some(
                                (l) => l.channel === i.channel && l.line === i.line,
                            )
                                ? 'Supported'
                                : this.status?.lines.some(
                                        (l) => l.kind === kind && l.line === i.line,
                                    )
                                  ? 'EOL / unsupported channel'
                                  : 'Unknown support',
                        })) || [],
                source: this.status?.defaults[kind]?.external
                    ? 'Existing installation'
                    : 'Private installations',
                external: this.status?.defaults[kind]?.external || '',
                latest:
                    this.lines(kind)
                        .map(
                            (line) =>
                                this.activeReleases.find((release) => release.line === line.line)
                                    ?.version,
                        )
                        .filter(Boolean)
                        .join(', ') || 'Check for releases',
            }));
        },
        // Derive automatic from the current runtimes state
        get automatic() {
            return this.status?.automatic ?? true;
        },
        // Derive metadata from the current runtimes state
        get metadata() {
            return this.status?.last_check
                ? `Last successful check: ${new Date(Number(this.status.last_check) * 1000).toLocaleString()}`
                : 'No successful release check yet';
        },
        // Derive jobs from the current runtimes state
        get jobs() {
            return this.status?.jobs || [];
        },
        // Derive title from the current runtimes state
        get title() {
            return `${this.pluginName || titles[this.kind]} · LTS runtime`;
        },
        // Derive selected lines from the current runtimes state
        get selectedLines() {
            return this.lines(this.kind);
        },
        // Derive active releases from the current runtimes state
        get activeReleases(): RuntimeRelease[] {
            const lines = new Set(this.selectedLines.map((line) => line.line));
            return (this.status?.releases || [])
                .filter((r) => r.kind === this.kind && lines.has(r.line))
                .sort((a, b) => b.version.localeCompare(a.version, undefined, { numeric: true }));
        },
        // Derive selected releases from the current runtimes state
        get selectedReleases(): RuntimeRelease[] {
            return this.activeReleases.filter((r) => r.line === this.line);
        },
        // Derive release from the current runtimes state
        get release(): RuntimeRelease | undefined {
            return this.selectedReleases.find((r) => r.version === this.version);
        },
        // Derive can install from the current runtimes state
        get canInstall() {
            return (
                !!this.release && !this.status?.installations.some((i) => i.id === this.release?.id)
            );
        },
        // Choose the first release in the new runtime line and reset unsaved source selection
        lineChange() {
            this.version = this.selectedReleases[0]?.version || '';
            this.source = 'private';
            this.saved = false;
        },
        // Treat the newly selected runtime version as an unsaved private installation choice
        versionChange() {
            this.source = 'private';
            this.saved = false;
        },
        // Derive installed from the current runtimes state
        get installed() {
            return (
                this.status?.installations.filter(
                    (i) => i.kind === this.kind && i.line === this.line && i.source === 'private',
                ) || []
            );
        },
        // Derive install label from the current runtimes state
        get installLabel() {
            return this.release && !this.canInstall ? 'Version installed' : 'Install runtime';
        },
        // Derive affected from the current runtimes state
        get affected() {
            return this.usage.filter((u) => u.pending_restart);
        },
        // Derive waiting from the current runtimes state
        get waiting() {
            return this.usage.filter((u) => u.enabled_preference && u.selected.state !== 'ready');
        },
        // List runtime lines that are active today in numeric version order
        lines(kind: RuntimeKind) {
            const today = new Date().toISOString().slice(0, 10);
            return (this.status?.lines || [])
                .filter((l) => l.kind === kind && l.start <= today && (!l.end || today < l.end))
                .sort((a, b) => a.line.localeCompare(b.line, undefined, { numeric: true }));
        },
        // Initialize runtimes state and its required lifecycle hooks
        async init() {
            ownWindowMethods(this, 'runtime management', [
                'open',
                'check',
                'automaticChange',
                'startInstall',
                'pick',
                'save',
                'restart',
                'confirmRemoval',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                ['save', 'restart', 'startInstall'],
                {
                    name: 'Runtime selection',
                    retainOnUnmount: true,
                    // Return a snapshot of the current runtimes state
                    state: () =>
                        (this.$refs.runtimeDialog as HTMLDialogElement | undefined)?.open &&
                        this.closeBaseline &&
                        this.closeValue() !== this.closeBaseline
                            ? 'dirty'
                            : 'clean',
                    // Discard pending runtimes edits without saving them
                    discard: () => this.close(),
                    // Release resources retained by the runtimes operation
                    release: async () => {
                        this.stopReader();
                        const jobId = this.installJobId;
                        if (
                            jobId &&
                            this.status?.jobs.some(
                                (job) =>
                                    job.id === jobId && ['queued', 'running'].includes(job.state),
                            )
                        )
                            await trackCloseRelease(`runtime-install:${jobId}`, () =>
                                app().bridge.command('runtime_cancel', { jobId }),
                            );
                        this.close();
                    },
                },
            );
            this.resumeListener = () => {
                void this.resumeReader();
            };
            window.addEventListener('editor-window-resumed', this.resumeListener);
            await this.resumeReader();
        },
        // Resume the reader
        async resumeReader() {
            this.destroyed = false;
            this.unsubscribe = reader.subscribe(
                ({ status, usage }) => void this.acceptStatus(status, usage),
                (error) => {
                    this.error = String(error);
                },
            );
            await this.load();
        },
        // Release runtimes state and associated lifecycle resources
        destroy() {
            if (this.resumeListener)
                window.removeEventListener('editor-window-resumed', this.resumeListener);
            this.unregisterCloseOwner?.();
            this.stopReader();
        },
        // Stop the reader
        stopReader() {
            ++this.viewEpoch;
            this.destroyed = true;
            this.unsubscribe?.();
            this.unsubscribe = null;
        },
        // Load the data required by the runtimes workflow
        async load(fresh = false) {
            try {
                const { status, usage } = await reader.refresh(fresh === true);
                if (!this.destroyed) await this.acceptStatus(status, usage);
            } catch (error) {
                if (!this.destroyed) this.error = String(error);
            }
        },
        // Accept the status
        async acceptStatus(status: RuntimeStatus, usage: RuntimeUsage[]) {
            if (this.destroyed || windowCloseOwners.closing) return;
            try {
                if (this.status) reconcile(this.status, cloneJson(status));
                else this.status = cloneJson(status);
                reconcile(this.usage, cloneJson(usage));
                this.error = '';
                if (!this.version) this.version = this.selectedReleases[0]?.version || '';
                const completed = status.jobs.find(
                    (j) => j.id === this.installJobId && j.state === 'complete',
                );
                if (
                    this.pluginId &&
                    this.installEpoch === this.viewEpoch &&
                    this.installOwner === this.currentOwner &&
                    completed &&
                    this.handledJobId !== completed.id &&
                    !this.source.startsWith('external-')
                ) {
                    this.handledJobId = completed.id;
                    await this.save();
                    if (this.saved && !this.affected.length) this.close();
                }
            } catch (e) {
                this.error = String(e);
            }
        },
        // Explain which running plugins prevent removal of the selected runtime installation
        removalBlock(installation: RuntimeInstallation) {
            const running = this.usage.filter(
                (u) =>
                    u.running?.id === installation.id ||
                    u.running?.executable === installation.executable,
            );
            return running.length
                ? `In use by ${running.map((u) => u.name).join(', ')}. Disable these plugins before deleting this runtime.`
                : '';
        },
        // Derive removable selection from the current runtimes state
        get removableSelection() {
            const id = this.source === 'private' ? this.release?.id : this.source;
            return this.status?.installations.find((i) => i.id === id && i.source === 'private');
        },
        // Derive removal impact from the current runtimes state
        get removalImpact() {
            if (!this.removal) return '';
            const id = this.removal.id;
            const defaultSelected = Object.values(this.status?.defaults || {}).some(
                (s) => s?.installation_id === id,
            );
            const plugins = new Set([
                ...Object.entries(this.status?.plugins || {})
                    .filter(([, s]) => s?.installation_id === id)
                    .map(([id]) => id),
                ...this.usage.filter((u) => u.selected.selected?.id === id).map((u) => u.plugin_id),
            ]);
            const names = [...plugins].map(
                (id) => this.usage.find((u) => u.plugin_id === id)?.name || id,
            );
            return [
                defaultSelected ? 'This is the selected default runtime.' : '',
                names.length ? `Selected by plugins: ${names.join(', ')}.` : '',
                'Saved selections are kept. Plugins that require this runtime will report it missing until you install it again or choose a replacement.',
            ]
                .filter(Boolean)
                .join(' ');
        },
        // Request the removal
        requestRemoval(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (id)
                window.dispatchEvent(new CustomEvent('open-runtime-removal', { detail: { id } }));
        },
        // Open the removal
        async openRemoval(event: CustomEvent<{ id: string }>) {
            if (this.removing || this.removal) return;
            await this.load(true);
            const installation = this.status?.installations.find((i) => i.id === event.detail.id);
            if (!installation || installation.source !== 'private' || this.destroyed) return;
            this.removal = { ...installation };
            this.removalError = '';
            // Complete the deferred runtimes work using the current operation state
            this.$nextTick(() => {
                const dialog = this.$refs.runtimeRemoval as HTMLDialogElement;
                if (this.removal && !dialog.open) dialog.showModal();
            });
        },
        // Cancel the removal
        cancelRemoval() {
            if (this.removing) return;
            (this.$refs.runtimeRemoval as HTMLDialogElement | undefined)?.close();
            this.removal = null;
            this.removalError = '';
        },
        // Confirm the removal
        async confirmRemoval() {
            const installation = this.removal;
            if (
                !installation ||
                installation.source !== 'private' ||
                this.removing ||
                this.removalBlock(installation)
            )
                return;
            this.removing = true;
            this.removalError = '';
            try {
                await app().command('runtime_remove', { installationId: installation.id });
                if (this.source === installation.id) this.source = 'private';
                this.saved = false;
                await this.load(true);
                window.dispatchEvent(new CustomEvent('runtime-changed'));
                this.removing = false;
                this.cancelRemoval();
                app().notify(`Deleted private Node.js ${installation.version}.`, 'success');
            } catch (error) {
                await this.load(true);
                this.removalError = String(error);
            } finally {
                this.removing = false;
            }
        },
        // Check the current runtimes preconditions
        async check() {
            this.busy = true;
            this.error = '';
            try {
                await app().command('runtime_check');
                await this.load(true);
            } catch (e) {
                this.error = String(e);
            } finally {
                this.busy = false;
            }
        },
        // Save automatic runtime-check preference and refresh the runtime status
        async automaticChange(event: Event) {
            try {
                await app().command('runtime_automatic', {
                    enabled: (event.target as HTMLInputElement).checked,
                });
                await this.load(true);
            } catch (e) {
                this.error = String(e);
            }
        },
        // Choose the active item for the runtimes workflow
        choose(event: Event) {
            const el = event.currentTarget as HTMLSelectElement | HTMLButtonElement;
            const kind = el.dataset.kind as RuntimeKind;
            window.dispatchEvent(
                new CustomEvent('open-runtime', {
                    detail: { kind, releaseId: el.value || undefined },
                }),
            );
            // The dialog owns the draft; the table continues to show the saved choice
            if (el.tagName === 'SELECT')
                el.value = this.status?.defaults[kind]?.installation_id || '';
        },
        // Open the current runtimes target
        async open(
            event: CustomEvent<{
                kind: RuntimeKind;
                line?: string;
                releaseId?: string;
                pluginId?: string;
                pluginName?: string;
                requirement?: RuntimeRequirement;
            }>,
        ) {
            const epoch = ++this.viewEpoch;
            this.installJobId = '';
            this.handledJobId = '';
            this.kind = event.detail.kind;
            this.pluginId = event.detail.pluginId || null;
            this.pluginName = event.detail.pluginName || '';
            this.requirement = event.detail.requirement || null;
            this.source = 'private';
            this.acknowledged = false;
            this.saved = false;
            this.error = '';
            this.candidates = [];
            await this.load();
            if (epoch !== this.viewEpoch) return;
            const remembered =
                this.status?.plugins[this.pluginId || ''] || this.status?.defaults[this.kind];
            const requested = this.status?.releases.find((r) => r.id === event.detail.releaseId);
            const installed = this.status?.installations.find(
                (i) => i.id === remembered?.installation_id,
            );
            this.line =
                requested?.line ||
                event.detail.line ||
                remembered?.line ||
                this.selectedLines.at(-1)?.line ||
                '';
            this.version =
                requested?.version ||
                (installed?.line === this.line ? installed.version : '') ||
                this.selectedReleases[0]?.version ||
                '';
            if (!requested && installed?.line === this.line) this.source = installed.id;
            const dialog = this.$refs.runtimeDialog as HTMLDialogElement;
            if (!dialog.open) dialog.showModal();
            try {
                const candidates = await app().command('runtime_candidates', {
                    kind: this.kind,
                });
                if (epoch !== this.viewEpoch) return;
                this.candidates = candidates;
                const existing = this.candidates.find((i) => i.executable === remembered?.external);
                if (existing && !requested) {
                    this.source = existing.id;
                    this.version = existing.version;
                    this.line = existing.line;
                }
            } catch (e) {
                if (epoch !== this.viewEpoch) return;
                this.error = String(e);
            }
            this.closeBaseline = this.closeValue();
        },
        // Release runtimes state and associated lifecycle resources
        close() {
            ++this.viewEpoch;
            (this.$refs.runtimeDialog as HTMLDialogElement | undefined)?.close();
        },
        // Install the selected runtimes item
        async install() {
            if (!this.release) return;
            return this.startInstall(this.release.id);
        },
        // Retry the runtimes operation
        async retry(event: Event) {
            const data = (event.currentTarget as HTMLElement).dataset;
            if (data.release && data.id) return this.startInstall(data.release, data.id);
        },
        // Label a runtime job using its release kind and exact version when available
        runtimeJobTitle(job: RuntimeJob) {
            const release = this.status?.releases.find((release) => release.id === job.release_id);
            const kind = job.kind || release?.kind;
            return kind
                ? `${titles[kind]} ${job.version || release?.version || ''}`.trim()
                : job.release_id;
        },
        // Start the install
        async startInstall(releaseId: string, retryJobId?: string) {
            const epoch = this.viewEpoch;
            const owner = this.currentOwner;
            const prior = retryJobId
                ? this.status?.jobs.find((job) => job.id === retryJobId)
                : undefined;
            const release = this.status?.releases.find((release) => release.id === releaseId);
            const matching =
                !retryJobId ||
                ((prior?.kind || release?.kind) === this.kind &&
                    (prior?.line || release?.line) === this.line &&
                    (prior?.version || release?.version) === this.version);
            this.error = '';
            try {
                const request: { releaseId: string; retryJobId?: string } = {
                    releaseId,
                };
                if (retryJobId) request.retryJobId = retryJobId;
                const job = await app().command('runtime_install', request);
                if (epoch !== this.viewEpoch) return;
                this.installEpoch = epoch;
                this.installOwner = matching ? owner : '';
                this.installJobId = job;
                this.handledJobId = '';
                await this.load(true);
            } catch (e) {
                if (epoch !== this.viewEpoch) return;
                this.error = String(e);
            }
        },
        // Cancel the active runtimes operation
        async cancel(event: Event) {
            const jobId = (event.currentTarget as HTMLElement).dataset.id;
            if (!jobId) return;
            try {
                await app().command('runtime_cancel', {
                    jobId,
                });
                await this.load(true);
            } catch (e) {
                this.error = String(e);
            }
        },
        // Open a native file picker for a runtime executable only outside browser preview
        async pick() {
            if (!native) return;
            try {
                const { open } = fileDialogs;
                const path = await open({
                    multiple: false,
                    directory: false,
                    title: `Choose ${titles[this.kind]} executable`,
                });
                if (typeof path !== 'string') return;
                const candidate = await app().command('runtime_validate_external', {
                    kind: this.kind,
                    path,
                });
                this.candidates = [
                    ...this.candidates.filter((c) => c.id !== candidate.id),
                    candidate,
                ];
                this.source = candidate.id;
                this.line = candidate.line;
                this.version = candidate.version;
            } catch (e) {
                this.error = String(e);
            }
        },
        // Copy line and version metadata from the selected runtime source
        sourceChange() {
            const chosen =
                this.candidates.find((i) => i.id === this.source) ||
                this.status?.installations.find((i) => i.id === this.source);
            if (chosen) {
                this.line = chosen.line;
                this.version = chosen.version;
            }
        },
        // Save pending changes through the runtimes workflow
        async save() {
            this.error = '';
            try {
                const external = this.candidates.find((c) => c.id === this.source);
                const installed = this.status?.installations.find(
                    (i) => i.id === (this.source === 'private' ? this.release?.id : this.source),
                );
                if (!external && !installed)
                    throw Error('Install the selected private release first.');
                const choice: RuntimeSelection = {
                    kind: this.kind,
                    channel: 'lts',
                    line: external?.line || this.line,
                    installation_id: installed?.id || null,
                    external: external?.executable || null,
                    acknowledge_override: this.acknowledged,
                };
                await app().command('runtime_select', { pluginId: this.pluginId, choice });
                this.closeBaseline = this.closeValue();
                this.saved = true;
                await this.load(true);
                window.dispatchEvent(new CustomEvent('runtime-changed'));
            } catch (e) {
                this.error = String(e);
            }
        },
        // Apply the selected runtime to affected plugins and refresh status before closing
        async restart() {
            try {
                app().apply(
                    await app().command('runtime_apply', {
                        pluginIds: this.affected.map((u) => u.plugin_id),
                    }),
                );
                await this.load(true);
                this.close();
            } catch (e) {
                this.error = String(e);
            }
        },
    }));
}
