import { ApplicationUpdateDraft, CatalogUpdateDraft } from './update-preference-drafts';
import { nextTab, tabNavigationKey } from './tab-navigation';
import * as fileDialogs from './window-dialogs';
import { registerArchiveReviewer } from './archive-import';
import { listen } from '@tauri-apps/api/event';
import { version as managerVersion } from '../package.json';
import type AlpineType from '@alpinejs/csp';
import { bridge, native } from './bridge';
import type {
    UpdateStatus,
    PluginPackageReview,
    ReleaseAsset,
    ReleaseCatalog,
    CatalogPlugin,
    PluginDocumentationRequest,
    UpdateJob,
} from '../generated/runtime';
import type { AppState } from './state';
import { reconcile } from './reconcile';
import { PollingReader } from './polling-reader';
import { ownWindowMethods } from './window-work';
import { windowCloseOwners } from './window-close-owners';
import { registerCloseDraft, trackCloseRelease } from './close-releases';

// Build release links only from validated repository and source provenance
export function managerReleaseLinks(
    catalog:
        Pick<ReleaseCatalog, 'repository' | 'release_tag' | 'commit' | 'build'> | null | undefined,
) {
    if (
        !catalog ||
        !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(catalog.repository) ||
        catalog.repository.split('/').some((part) => part === '.' || part === '..')
    )
        return [];
    const root = `https://github.com/${catalog.repository}`;
    const links = [];
    if (/^[A-Za-z0-9][A-Za-z0-9._+-]*$/.test(catalog.release_tag))
        links.push({
            kind: 'release',
            label: 'GitHub release',
            url: `${root}/releases/tag/${catalog.release_tag}`,
        });
    if (/^(?:[a-fA-F0-9]{40}|[a-fA-F0-9]{64})$/.test(catalog.commit))
        links.push({
            kind: 'source',
            label: 'Source commit',
            url: `${root}/commit/${catalog.commit}`,
        });
    if (/^[1-9][0-9]*$/.test(catalog.build))
        links.push({
            kind: 'build',
            label: 'Build run',
            url: `${root}/actions/runs/${catalog.build}`,
        });
    return links;
}

export type InstallationReviewPhase =
    | { kind: 'idle' }
    | { kind: 'starting' }
    | { kind: 'downloading'; job: string }
    | { kind: 'preparing' }
    | {
          kind: 'reviewing' | 'committing';
          review: PluginPackageReview;
          allowChannelChange: boolean;
          replaceInstalled: boolean;
      };
export type InstallationReviewRequest =
    | { kind: 'local'; review: PluginPackageReview }
    | { kind: 'remote'; id: string; channel: string };
// Start package review with channel-change and replacement permissions unset
const reviewing = (review: PluginPackageReview): InstallationReviewPhase => ({
    kind: 'reviewing',
    review,
    allowChannelChange: false,
    replaceInstalled: false,
});

// Register the updates
export function registerUpdates(Alpine: typeof AlpineType, app: () => AppState) {
    const reader = new PollingReader(
        () => bridge.command('update_status'),
        (status) => status.jobs.some((job) => ['queued', 'running'].includes(job.state)),
    );
    const store = {
        applicationDraft: new ApplicationUpdateDraft(),
        catalogDraft: new CatalogUpdateDraft(),
        status: null as UpdateStatus | null,
        reviewPhase: { kind: 'idle' } as InstallationReviewPhase,
        reviewQueue: [] as InstallationReviewRequest[],
        // Derive review from the current updates state
        get review() {
            return this.reviewPhase.kind === 'reviewing' || this.reviewPhase.kind === 'committing'
                ? this.reviewPhase.review
                : null;
        },
        // Derive working from the current updates state
        get working() {
            return this.reviewPhase.kind === 'committing';
        },
        // Derive starting review from the current updates state
        get startingReview() {
            return this.reviewPhase.kind === 'starting';
        },
        // Derive preparing review from the current updates state
        get preparingReview() {
            return this.reviewPhase.kind === 'preparing';
        },
        // Derive pending review from the current updates state
        get pendingReview() {
            return this.reviewPhase.kind === 'downloading' ? this.reviewPhase.job : '';
        },
        // Derive allow channel change from the current updates state
        get allowChannelChange() {
            return (
                (this.reviewPhase.kind === 'reviewing' || this.reviewPhase.kind === 'committing') &&
                this.reviewPhase.allowChannelChange
            );
        },
        // Record channel-change consent only while a package review is active
        set allowChannelChange(value: boolean) {
            if (this.reviewPhase.kind === 'reviewing') this.reviewPhase.allowChannelChange = value;
        },
        // Derive replace installed from the current updates state
        get replaceInstalled() {
            return (
                (this.reviewPhase.kind === 'reviewing' || this.reviewPhase.kind === 'committing') &&
                this.reviewPhase.replaceInstalled
            );
        },
        // Replace the installed
        set replaceInstalled(value: boolean) {
            if (this.reviewPhase.kind === 'reviewing') this.reviewPhase.replaceInstalled = value;
        },
        // Clear the remote reviews
        clearRemoteReviews() {
            // Prepared local tokens are retained on errors until retry or explicit dismissal
            this.reviewQueue = this.reviewQueue.filter((request) => request.kind === 'local');
        },
        // Advance the review queue or offer setup after all installation reviews are complete
        async nextReview() {
            const next = this.reviewQueue.shift();
            if (next?.kind === 'local') this.reviewPhase = reviewing(next.review);
            else if (next) await this.startReview(next.id, next.channel);
            else this.offerPluginSetup();
        },
        pluginSetupCandidates: [] as string[],
        // Close the catalog and offer setup for newly installed plugins unless the window is closing
        offerPluginSetup() {
            if (windowCloseOwners.closing) return;
            if (!this.pluginSetupCandidates.length) return;
            this.catalogOpen = false;
            const ids = this.pluginSetupCandidates.splice(0);
            window.dispatchEvent(new CustomEvent('plugin-setup-imports', { detail: ids }));
        },
        catalogOpen: false,
        catalogOpener: null as HTMLElement | null,
        catalogTab: 'browse',
        packageVariants: {} as Record<string, string>,
        linkedPlugin: '',
        linkedChannel: '',
        linkCheck: '',
        dragSupported: false,
        dragJob: '',
        dragReceipt: '',
        preparingDrag: false,
        downloadOnlyJobs: new Set<string>(),
        // Derive browse channel from the current updates state
        get browseChannel() {
            return this.linkedChannel || this.channel;
        },
        reviewEpoch: 0,
        error: '',
        refreshing: false,
        refreshAgain: false,
        initialized: false,
        // Derive manager repository from the current updates state
        get managerRepository() {
            return this.applicationDraft.repository;
        },
        // Update the unsaved Manager release-repository preference
        set managerRepository(value: string) {
            this.applicationDraft.repository = value;
        },
        // Derive plugins repository from the current updates state
        get pluginsRepository() {
            return this.catalogDraft.repository;
        },
        // Update the unsaved plugin catalog repository preference
        set pluginsRepository(value: string) {
            this.catalogDraft.repository = value;
        },
        // Derive catalog url from the current updates state
        get catalogUrl() {
            return this.catalogDraft.url;
        },
        // Update the unsaved custom catalog URL
        set catalogUrl(value: string) {
            this.catalogDraft.url = value;
        },
        // Derive effective catalog url from the current updates state
        get effectiveCatalogUrl() {
            return this.status?.effective_plugins_catalog_url || '';
        },
        catalogSearch: '',
        // Derive catalog source from the current updates state
        get catalogSource() {
            return this.catalogDraft.source;
        },
        // Update the unsaved catalog source selection
        set catalogSource(value: string) {
            this.catalogDraft.source = value;
        },
        // Derive channel from the current updates state
        get channel() {
            return this.catalogDraft.channel;
        },
        // Update the unsaved plugin catalog channel
        set channel(value: string) {
            this.catalogDraft.channel = value;
        },
        // Derive automatic from the current updates state
        get automatic() {
            return this.applicationDraft.automatic;
        },
        // Update the unsaved automatic Manager-update preference
        set automatic(value: boolean) {
            this.applicationDraft.automatic = value;
        },
        unsubscribe: null as (() => void) | null,
        destroyed: false,
        // Derive review loading from the current updates state
        get reviewLoading() {
            return ['starting', 'downloading', 'preparing'].includes(this.reviewPhase.kind);
        },
        // Derive review active from the current updates state
        get reviewActive() {
            return this.reviewPhase.kind !== 'idle';
        },
        // Derive review download from the current updates state
        get reviewDownload() {
            return this.status?.jobs.find((job) => job.id === this.pendingReview);
        },
        // Derive review loading title from the current updates state
        get reviewLoadingTitle() {
            return this.pendingReview && !this.preparingReview
                ? 'Downloading package…'
                : 'Preparing review…';
        },
        // Derive manager error from the current updates state
        get managerError() {
            return this.status?.manager.error || '';
        },
        // Derive plugins error from the current updates state
        get pluginsError() {
            return this.status?.plugin_policy_error || this.status?.plugins.error || '';
        },
        // Derive manager last check from the current updates state
        get managerLastCheck() {
            return this.date(this.status?.manager.last_success);
        },
        // Derive plugins last check from the current updates state
        get pluginsLastCheck() {
            return this.date(this.status?.plugins.last_success);
        },
        // Derive manager notes from the current updates state
        get managerNotes() {
            return this.managerCatalog?.release_notes || '';
        },

        // Derive manager version from the current updates state
        get managerVersion() {
            return this.status?.installed_manager || managerVersion;
        },
        // Derive count from the current updates state
        get count() {
            return this.pluginUpdates.length;
        },
        // Derive manager state from the current updates state
        get managerState() {
            return this.status?.manager.state.replaceAll('_', ' ') || 'unavailable';
        },
        // Derive plugins state from the current updates state
        get pluginsState() {
            return this.status?.plugins.state.replaceAll('_', ' ') || 'unavailable';
        },
        // Derive manager catalog from the current updates state
        get managerCatalog() {
            return this.status?.manager.catalog;
        },
        // Derive manager links from the current updates state
        get managerLinks() {
            return managerReleaseLinks(this.managerCatalog);
        },
        // Derive plugin updates from the current updates state
        get pluginUpdates() {
            return this.status?.plugin_updates || [];
        },
        // Derive checking from the current updates state
        get checking() {
            return (
                this.status?.jobs.some(
                    (j) => j.operation === 'check' && ['queued', 'running'].includes(j.state),
                ) || false
            );
        },
        // Derive jobs from the current updates state
        get jobs() {
            return (
                this.status?.jobs.filter(
                    (j) => j.operation !== 'check' && j.product === 'plugins',
                ) || []
            );
        },
        // Derive manager jobs from the current updates state
        get managerJobs() {
            return (
                this.status?.jobs.filter(
                    (j) => j.operation !== 'check' && j.product === 'manager',
                ) || []
            );
        },
        // Derive installers from the current updates state
        get installers(): ReleaseAsset[] {
            return (
                this.managerCatalog?.assets.filter(
                    (a) =>
                        a.os === this.status?.host_os &&
                        (a.arch === this.status?.host_arch || a.arch === 'universal'),
                ) || []
            );
        },
        // Derive selected catalog from the current updates state
        get selectedCatalog() {
            return this.status?.available_catalogs?.find(
                (catalog) => catalog.channel === this.browseChannel,
            );
        },
        // Derive linked ready from the current updates state
        get linkedReady() {
            return Boolean(
                this.linkedPlugin &&
                this.selectedCatalog?.plugins.some((entry) => entry.id === this.linkedPlugin),
            );
        },
        // Derive available plugins from the current updates state
        get availablePlugins() {
            const query = this.catalogSearch.trim().toLowerCase();
            return (this.selectedCatalog?.plugins || [])
                .filter((p) =>
                    this.linkedPlugin
                        ? p.id === this.linkedPlugin
                        : [
                              p.name,
                              p.description,
                              ...p.tags,
                              ...p.actions,
                              ...p.dependencies.map((d) => d.name),
                          ]
                              .join(' ')
                              .toLowerCase()
                              .includes(query),
                )
                .sort((a, b) => a.name.localeCompare(b.name));
        },
        // Derive has catalog from the current updates state
        get hasCatalog() {
            return (
                this.status?.available_catalogs?.some((c) => c.channel === this.channel) || false
            );
        },
        // Derive can commit from the current updates state
        get canCommit() {
            return (
                Boolean(this.review?.compatible) &&
                !this.working &&
                (!this.review?.channel_change || this.allowChannelChange) &&
                (this.review?.trusted || !this.review?.installed_version || this.replaceInstalled)
            );
        },
        // Check whether the unified package supports this host OS and architecture
        unifiedAvailable(plugin: CatalogPlugin) {
            return Boolean(
                plugin.unified_download?.targets.some(
                    (target) =>
                        target.os === this.status?.host_os &&
                        target.architectures.includes(this.status?.host_arch || ''),
                ),
            );
        },
        // Read the selected package variant with platform packaging as the default
        packageVariant(id: string) {
            return this.packageVariants[id] === 'unified' ? 'unified' : 'platform';
        },
        // Select the package variant
        selectPackageVariant(event: Event) {
            const input = event.currentTarget as HTMLSelectElement;
            const id = input.dataset.id;
            if (id && !this.reviewActive && !this.working)
                this.packageVariants[id] = input.value === 'unified' ? 'unified' : 'platform';
        },
        // Select the requested universal archive or an artifact matching the host platform
        artifact(plugin: CatalogPlugin) {
            if (this.packageVariant(plugin.id) === 'unified') {
                if (!this.unifiedAvailable(plugin) || !plugin.unified_download) return undefined;
                return {
                    ...plugin.unified_download,
                    os: 'unified',
                    compatibility: { status: 'unverified' },
                };
            }
            return plugin.downloads
                .filter(
                    (a) =>
                        a.os === this.status?.host_os &&
                        a.architectures.includes(this.status?.host_arch || ''),
                )
                .sort((a, b) => a.architectures.length - b.architectures.length)[0];
        },
        // Label the selected artifact with its platform and download size
        downloadLabel(plugin: CatalogPlugin) {
            const artifact = this.artifact(plugin);
            if (!artifact) return 'Unavailable for this platform';
            const platforms: Record<string, string> = {
                macos: 'macOS',
                windows: 'Windows',
                linux: 'Linux',
                unified: 'All platforms',
            };
            return `${platforms[artifact.os] || artifact.os} · ${(artifact.size / 1048576).toFixed(1)} MiB`;
        },
        // Describe the selected artifact's reported VSD Craft compatibility
        vsdCraftLabel(plugin: CatalogPlugin) {
            const status = this.artifact(plugin)?.compatibility.status;
            if (!status) return '';
            if (status === 'verified') return 'VSD Craft: Compatible';
            if (status === 'incompatible') return 'VSD Craft: Not compatible';
            return 'VSD Craft: Not verified';
        },
        // Label an installed plugin's available update and compatibility
        badge(id: string) {
            const p = this.pluginUpdates.find((p) => p.id === id);
            return p
                ? `${p.available_version}${p.compatible ? ' available' : ' incompatible'}`
                : '';
        },
        // Format the updates timestamp for display
        date(value?: number | null) {
            return value ? new Date(value * 1000).toLocaleString() : 'Never';
        },
        // Open the catalog
        openCatalog(tab = 'browse', event?: Event) {
            if (!this.catalogOpen) {
                this.packageVariants = {};
                const opener = event?.currentTarget;
                this.catalogOpener = opener && opener instanceof HTMLElement ? opener : null;
            }
            this.catalogTab = tab;
            this.catalogOpen = true;
        },
        // Clear the linked plugin
        clearLinkedPlugin() {
            this.linkedPlugin = '';
            this.linkedChannel = '';
            this.linkCheck = '';
        },
        // Open the linked plugin
        async openLinkedPlugin(channel: string, id: string) {
            this.linkedPlugin = id;
            this.linkedChannel = channel;
            this.error = '';
            this.openCatalog();
            const status = await bridge.command('update_status');
            if (this.linkedPlugin !== id || !this.catalogOpen) return;
            await this.refresh(status);
            if (
                !status.available_catalogs?.some(
                    (catalog) =>
                        catalog.channel === channel &&
                        catalog.plugins.some((plugin) => plugin.id === id),
                )
            ) {
                const check = await bridge.command('check_updates', { product: 'plugins' });
                if (this.linkedPlugin !== id || !this.catalogOpen) return;
                this.linkCheck = check;
                await this.refresh();
            }
        },
        // Refresh updates data while keeping the view synchronized
        async refresh(snapshot?: UpdateStatus) {
            if (this.refreshing) {
                if (!snapshot) this.refreshAgain = true;
                return;
            }
            this.refreshing = true;
            try {
                const status = snapshot && 'jobs' in snapshot ? snapshot : await reader.refresh();
                if (this.destroyed) return;
                if (this.status) reconcile(this.status, status);
                else this.status = status;
                if (!this.initialized) {
                    const p = this.status.preferences;
                    this.applicationDraft.load(p);
                    this.catalogDraft.load(p);
                    this.initialized = true;
                }
                if (this.linkCheck) {
                    const job = this.status.jobs.find(
                        (candidate) => candidate.id === this.linkCheck,
                    );
                    if (job && !['queued', 'running'].includes(job.state)) {
                        this.linkCheck = '';
                        if (!this.availablePlugins.length)
                            this.error =
                                job.error ||
                                'This plugin is unavailable in the configured catalog.';
                    }
                }
                if (this.pendingReview) {
                    const job = this.status.jobs.find((j) => j.id === this.pendingReview);
                    if (job?.state === 'complete') {
                        const id = this.pendingReview;
                        const epoch = this.reviewEpoch;
                        this.reviewPhase = { kind: 'preparing' };
                        try {
                            const review = await bridge.command('prepare_catalog_download', { id });
                            if (
                                this.catalogOpen &&
                                epoch === this.reviewEpoch &&
                                !windowCloseOwners.closing
                            )
                                this.reviewPhase = reviewing(review);
                            else
                                await trackCloseRelease(`plugin-package:${review.id}`, () =>
                                    bridge.command('cancel_plugin_package', { id: review.id }),
                                );
                        } catch (error) {
                            if (epoch === this.reviewEpoch) {
                                this.error = String(error);
                                this.clearRemoteReviews();
                            }
                        } finally {
                            if (epoch === this.reviewEpoch && this.reviewPhase.kind === 'preparing')
                                this.reviewPhase = { kind: 'idle' };
                        }
                    } else if (job && ['failed', 'cancelled'].includes(job.state)) {
                        this.reviewPhase = { kind: 'idle' };
                        this.clearRemoteReviews();
                        this.error = job.error || job.state;
                    }
                }
            } catch (error) {
                this.error = String(error);
            } finally {
                this.refreshing = false;
                if (this.refreshAgain && !this.destroyed) {
                    this.refreshAgain = false;
                    await this.refresh();
                }
            }
        },
        // Initialize updates state and its required lifecycle hooks
        async init() {
            if (native)
                void bridge
                    .command('archive_drag_supported')
                    .then((supported) => {
                        this.dragSupported = supported;
                    })
                    .catch(() => {});
            this.destroyed = false;
            this.unsubscribe = reader.subscribe(
                (status) => void this.refresh(status),
                (error) => {
                    this.error = String(error);
                },
            );
            await this.refresh();
        },
        // Release updates state and associated lifecycle resources
        destroy() {
            this.destroyed = true;
            this.unsubscribe?.();
            this.unsubscribe = null;
        },
        // Save pending changes through the updates workflow
        async save(product?: string) {
            try {
                // Catalog checks must not commit the application settings draft
                const saved =
                    product === 'plugins'
                        ? (await bridge.command('update_status')).preferences
                        : undefined;
                await bridge.command('save_update_preferences', {
                    preferences: {
                        manager_repository:
                            saved?.manager_repository ?? this.managerRepository.trim(),
                        plugins_repository: this.pluginsRepository.trim(),
                        plugins_catalog_source: this.catalogSource,
                        plugins_catalog_url: this.catalogUrl.trim() || undefined,
                        automatic_checks: saved?.automatic_checks ?? this.automatic,
                        plugin_channel: this.channel,
                    },
                });
                this.error = '';
                await this.refresh();
                return true;
            } catch (error) {
                this.error = String(error);
                return false;
            }
        },
        // Check the current updates preconditions
        async check(product: string) {
            if (this.checking) return;
            if (!(await this.save(product))) return;
            try {
                await bridge.command('check_updates', { product });
                this.error = '';
                await this.refresh();
            } catch (error) {
                this.error = String(error);
            }
        },
        // Install the available
        async installAvailable(event: Event) {
            this.clearRemoteReviews();
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            await this.startReview(id, this.browseChannel, this.packageVariant(id) === 'unified');
        },
        // Open the updates catalog and begin reviewing the selected plugin
        reviewOne(event: Event) {
            this.openCatalog('updates');
            this.clearRemoteReviews();
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            const candidate = this.pluginUpdates.find((p) => p.id === id);
            return this.startReview(id, candidate?.channel || this.channel);
        },
        // Queue compatible plugin updates and start reviewing the first
        async reviewAll() {
            if (this.reviewActive || this.working) return;
            const remote = this.pluginUpdates.filter((p) => p.compatible);
            const first = remote.shift();
            this.clearRemoteReviews();
            this.reviewQueue.push(
                ...remote.map((p) => ({ kind: 'remote' as const, id: p.id, channel: p.channel })),
            );
            if (first) await this.startReview(first.id, first.channel);
        },
        // Start the review
        async startReview(id: string, channel: string, unified = false) {
            if (!id || this.reviewActive || this.working) return;
            this.error = '';
            const epoch = ++this.reviewEpoch;
            this.reviewPhase = { kind: 'starting' };
            try {
                const job = await bridge.command('download_release_asset', {
                    product: 'plugins',
                    name: id,
                    channel,
                    unified,
                });
                if (epoch !== this.reviewEpoch || !this.catalogOpen) {
                    await bridge.command('cancel_update_job', { id: job });
                    return;
                }
                this.reviewPhase = { kind: 'downloading', job };
                await this.refresh();
            } catch (error) {
                if (epoch !== this.reviewEpoch) return;
                this.error = String(error);
                this.clearRemoteReviews();
            } finally {
                if (epoch === this.reviewEpoch && this.reviewPhase.kind === 'starting')
                    this.reviewPhase = { kind: 'idle' };
            }
        },
        // Import the package
        async importPackage(folder = false) {
            if (!native) {
                app().notify('Import packages in the native desktop app.', 'warning');
                return;
            }
            const { open } = fileDialogs;
            const path = await open({
                multiple: false,
                directory: folder,
                title: folder
                    ? 'Import VSD Craft / Stream Deck .sdPlugin folder'
                    : 'Import VSD Craft / Stream Deck plugin',
                filters: [
                    {
                        name: 'VSD Craft / Stream Deck plugins (.streamDeckPlugin, .zip)',
                        extensions: [
                            'zip',
                            'ZIP',
                            'streamDeckPlugin',
                            'streamdeckplugin',
                            'STREAMDECKPLUGIN',
                            'sdPlugin',
                            'sdplugin',
                            'SDPLUGIN',
                        ],
                    },
                ],
            });
            if (path) await this.reviewLocalArchive(path);
        },
        // Prepare a local archive for review while rejecting stale dialog results
        async reviewLocalArchive(path: string) {
            if (this.working) return;
            this.openCatalog('updates');
            const closing = this.closeReview();
            const epoch = this.reviewEpoch;
            await closing;
            if (epoch !== this.reviewEpoch || !this.catalogOpen) return;
            this.reviewPhase = { kind: 'preparing' };
            this.error = '';
            try {
                const review = await bridge.command('prepare_plugin_package', {
                    path,
                });
                if (this.catalogOpen && epoch === this.reviewEpoch && !windowCloseOwners.closing)
                    this.reviewPhase = reviewing(review);
                else
                    await trackCloseRelease(`plugin-package:${review.id}`, () =>
                        bridge.command('cancel_plugin_package', { id: review.id }),
                    );
            } catch (error) {
                if (epoch === this.reviewEpoch) this.error = String(error);
            } finally {
                if (epoch === this.reviewEpoch && this.reviewPhase.kind === 'preparing')
                    this.reviewPhase = { kind: 'idle' };
            }
        },
        // Commit the reviewed updates changes
        async commit() {
            if (!this.canCommit || this.reviewPhase.kind !== 'reviewing') return;
            const phase = this.reviewPhase;
            const epoch = this.reviewEpoch;
            this.reviewPhase = { ...phase, kind: 'committing' };
            let succeeded = false;
            try {
                app().apply(
                    await bridge.command('commit_plugin_package', {
                        id: phase.review.id,
                        allowChannelChange: phase.allowChannelChange,
                        replace: phase.replaceInstalled,
                    }),
                );
                this.pluginSetupCandidates.push(phase.review.package.id);
                this.reviewPhase = { kind: 'idle' };
                this.error = '';
                succeeded = true;
                app().notify(
                    'Plugin imported. New plugins stay disabled; enabled plugins run only when an active device profile uses them.',
                    'success',
                );
            } catch (error) {
                // A failed commit retains its review token and exact draft for retry
                this.reviewPhase = phase;
                this.error = String(error);
                this.clearRemoteReviews();
            }
            await this.refresh();
            if (windowCloseOwners.closing || epoch !== this.reviewEpoch) return;
            if (succeeded) await this.nextReview();
        },
        // Close the review
        async closeReview() {
            // The UI already blocks close during commit; all other entry points obey it too
            if (this.working) return;
            ++this.reviewEpoch;
            const phase = this.reviewPhase;
            const queued = this.reviewQueue.splice(0);
            this.reviewPhase = { kind: 'idle' };
            const releases: Promise<unknown>[] = [];
            // Release updates state and associated lifecycle resources
            const dispose = (review: PluginPackageReview) =>
                releases.push(
                    trackCloseRelease(`plugin-package:${review.id}`, () =>
                        bridge.command('cancel_plugin_package', { id: review.id }),
                    ),
                );
            for (const request of queued) if (request.kind === 'local') dispose(request.review);
            if (phase.kind === 'downloading')
                releases.push(
                    trackCloseRelease(`package-download:${phase.job}`, () =>
                        bridge.command('cancel_update_job', { id: phase.job }),
                    ),
                );
            if (phase.kind === 'reviewing') dispose(phase.review);
            await Promise.all(releases);
        },
        // Accept the local reviews
        async acceptLocalReviews(reviews: PluginPackageReview[]) {
            if (this.working) {
                // Local imports have always taken precedence over queued remote downloads
                const locals = this.reviewQueue.filter((request) => request.kind === 'local');
                const remote = this.reviewQueue.filter((request) => request.kind === 'remote');
                this.reviewQueue = [
                    ...locals,
                    ...reviews.map((review) => ({ kind: 'local' as const, review })),
                    ...remote,
                ];
                return;
            }
            const closing = this.closeReview();
            const epoch = this.reviewEpoch;
            let closeFailed = false;
            try {
                await closing;
            } catch (error) {
                this.error = String(error);
                closeFailed = true;
            }
            if (closeFailed || epoch !== this.reviewEpoch || windowCloseOwners.closing) {
                await Promise.all(
                    reviews.map((review) =>
                        trackCloseRelease(`plugin-package:${review.id}`, () =>
                            bridge.command('cancel_plugin_package', { id: review.id }),
                        ),
                    ),
                );
                return;
            }
            const [first, ...remaining] = reviews;
            this.reviewPhase = first ? reviewing(first) : { kind: 'idle' };
            this.reviewQueue = remaining.map((review) => ({ kind: 'local', review }));
            this.openCatalog('updates');
        },
        // Open the manager link
        async openManagerLink(event: Event) {
            if (!native) return;
            event.preventDefault();
            const kind = (event.currentTarget as HTMLElement).dataset.kind;
            if (kind !== 'release' && kind !== 'source' && kind !== 'build') return;
            try {
                await bridge.command('open_manager_release_link', {
                    kind,
                });
            } catch (error) {
                this.error = String(error);
            }
        },
        // Open the plugin documentation
        async openPluginDocumentation(event: Event) {
            if (!native) return;
            event.preventDefault();
            const data = (event.currentTarget as HTMLElement).dataset;
            const request: PluginDocumentationRequest = {
                repository: data.repository || '',
                channel: data.channel || '',
                plugin_id: data.pluginId || '',
                plugin_version: data.pluginVersion || '',
                dependency: data.dependency ?? null,
            };
            try {
                await bridge.command('open_plugin_documentation', { request });
            } catch (error) {
                this.error = String(error);
            }
        },
        // Start a download-only plugin job for the chosen package variant
        async downloadPlugin(event: Event) {
            const name = (event.currentTarget as HTMLElement).dataset.id;
            if (!name) return;
            try {
                const id = await bridge.command('download_release_asset', {
                    product: 'plugins',
                    name,
                    channel: this.browseChannel,
                    unified: this.packageVariant(name) === 'unified',
                });
                this.downloadOnlyJobs.add(id);
                this.catalogTab = 'updates';
                await this.refresh();
            } catch (error) {
                this.error = String(error);
            }
        },
        // Release the drag
        releaseDrag() {
            const id = this.dragReceipt;
            this.dragReceipt = '';
            this.dragJob = '';
            if (id)
                void trackCloseRelease('archive:' + id, () =>
                    bridge.command('release_archive_export', { id }),
                ).catch(() => {});
        },
        // Prepare the drag
        async prepareDrag(event: Event) {
            const job = (event.currentTarget as HTMLElement).dataset.id;
            if (!job || !this.dragSupported || this.preparingDrag) return;
            this.releaseDrag();
            this.preparingDrag = true;
            try {
                const receipt = await bridge.command('prepare_catalog_archive_drag', { job });
                if (!this.catalogOpen || this.destroyed) {
                    await trackCloseRelease('archive:' + receipt, () =>
                        bridge.command('release_archive_export', { id: receipt }),
                    );
                    return;
                }
                this.dragJob = job;
                this.dragReceipt = receipt;
            } catch (error) {
                this.error = String(error);
            } finally {
                this.preparingDrag = false;
            }
        },
        // Start a native archive drag from the retained download receipt
        async dragPackage(event: MouseEvent) {
            if (event.button !== 0) return;
            event.preventDefault();
            if (!this.dragReceipt) return;
            try {
                await bridge.command('start_archive_drag', { receipt: this.dragReceipt });
            } catch (error) {
                this.error = String(error);
                this.releaseDrag();
            }
        },
        // Download the selected Manager release asset and refresh job status
        async download(event: Event) {
            const name = (event.currentTarget as HTMLElement).dataset.name;
            if (!name) return;
            try {
                await bridge.command('download_release_asset', {
                    product: 'manager',
                    name,
                });
                await this.refresh();
            } catch (error) {
                this.error = String(error);
            }
        },
        // Cancel the active updates operation
        async cancel(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (!id) return;
            try {
                await bridge.command('cancel_update_job', {
                    id,
                });
                await this.refresh();
            } catch (error) {
                this.error = String(error);
            }
        },
        // Check whether retry is valid for the current updates state
        canRetry(job: UpdateJob) {
            return Boolean(job.retry_available && job.state === 'failed');
        },
        // Format completed bytes and the total when the download size is known
        downloadProgress(job: UpdateJob) {
            return job.total > 0
                ? `${job.completed.toLocaleString()} of ${job.total.toLocaleString()} bytes`
                : `${job.completed.toLocaleString()} bytes`;
        },
        // Retry the download
        async retryDownload(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            const job = this.status?.jobs.find((job) => job.id === id);
            if (!id || !job || !this.canRetry(job)) return;
            const plugin = job.product === 'plugins' && !this.downloadOnlyJobs.has(id);
            if (plugin && (this.reviewActive || this.working)) return;
            const epoch = plugin ? ++this.reviewEpoch : this.reviewEpoch;
            if (plugin) this.reviewPhase = { kind: 'starting' };
            this.error = '';
            try {
                const next = await bridge.command('retry_update_job', { id });
                if (this.downloadOnlyJobs.delete(id)) this.downloadOnlyJobs.add(next);
                if (plugin) {
                    if (epoch !== this.reviewEpoch || !this.catalogOpen) {
                        await bridge.command('cancel_update_job', { id: next });
                        return;
                    }
                    this.reviewPhase = { kind: 'downloading', job: next };
                }
                await this.refresh();
            } catch (error) {
                if (!plugin || epoch === this.reviewEpoch) this.error = String(error);
            } finally {
                if (plugin && epoch === this.reviewEpoch && this.reviewPhase.kind === 'starting')
                    this.reviewPhase = { kind: 'idle' };
            }
        },
        // Show the selected updates content
        async show(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (!id) return;
            try {
                await bridge.command('show_update_download', {
                    id,
                });
            } catch (error) {
                this.error = String(error);
            }
        },
    };
    Alpine.store('updates', store);
    const reactive = Alpine.store('updates') as typeof store;
    for (const [name, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(store)))
        if (typeof descriptor.value === 'function')
            Reflect.set(reactive, name, descriptor.value.bind(reactive));
    ownWindowMethods(reactive, 'package operation', [
        'save',
        'check',
        'installAvailable',
        'downloadPlugin',
        'prepareDrag',
        'dragPackage',
        'reviewAll',
        'startReview',
        'importPackage',
        'reviewLocalArchive',
        'commit',
        'closeReview',
        'acceptLocalReviews',
        'openManagerLink',
        'openPluginDocumentation',
        'download',
        'cancel',
        'retryDownload',
        'show',
    ]);
    registerCloseDraft(reactive, ['save'], {
        name: 'Application update preferences',
        // Return a snapshot of the current updates state
        state: () =>
            reactive.status && reactive.applicationDraft.dirty(reactive.status.preferences)
                ? 'dirty'
                : 'clean',
        // Discard pending updates edits without saving them
        discard: () => {
            if (reactive.status) reactive.applicationDraft.load(reactive.status.preferences);
        },
    });
    registerCloseDraft(reactive, ['save'], {
        name: 'Plugin catalog preferences',
        // Return a snapshot of the current updates state
        state: () =>
            reactive.status && reactive.catalogDraft.dirty(reactive.status.preferences)
                ? 'dirty'
                : 'clean',
        // Discard pending updates edits without saving them
        discard: () => {
            if (reactive.status) reactive.catalogDraft.load(reactive.status.preferences);
        },
    });
    registerCloseDraft(
        reactive,
        ['commit', 'startReview', 'importPackage', 'reviewLocalArchive', 'reviewAll'],
        {
            name: 'Package review',
            // Return a snapshot of the current updates state
            state: () =>
                reactive.review && (reactive.allowChannelChange || reactive.replaceInstalled)
                    ? 'dirty'
                    : 'clean',
            // Discard pending updates edits without saving them
            discard: () => reactive.closeReview(),
            // Release resources retained by the updates operation
            release: async () => {
                reactive.destroy();
                reactive.catalogOpen = false;
                await reactive.closeReview();
                reactive.pluginSetupCandidates = [];
            },
        },
    );
    const unregisterArchiveReviewer = registerArchiveReviewer('plugin', (path) =>
        reactive.reviewLocalArchive(path),
    );
    window.addEventListener(
        'pagehide',
        () => {
            unregisterArchiveReviewer();
            reactive.destroy();
        },
        { once: true },
    );
    if (native && typeof document !== 'undefined') {
        let reading = false;
        let queued = false;
        // Drain pending catalog links only while the window is open and no modal blocks delivery
        const deliver = async () => {
            queued = true;
            if (reading || windowCloseOwners.closing || document.querySelector('dialog:modal'))
                return;
            reading = true;
            try {
                while (
                    queued &&
                    !windowCloseOwners.closing &&
                    !document.querySelector('dialog:modal')
                ) {
                    queued = false;
                    const pending = await bridge.command('manager_catalog_link');
                    if (
                        !pending ||
                        windowCloseOwners.closing ||
                        document.querySelector('dialog:modal')
                    )
                        return;
                    const [channel, id] = pending;
                    void reactive.openLinkedPlugin(channel, id).catch((error) => {
                        reactive.error = String(error);
                    });
                    await bridge.command('acknowledge_manager_catalog_link', {
                        channel,
                        pluginId: id,
                    });
                }
            } catch (error) {
                app().notify(String(error), 'warning');
            } finally {
                reading = false;
            }
        };
        // Resume the paused updates operation
        const resume = () => void deliver();
        document.addEventListener('close', resume, true);
        window.addEventListener('editor-window-resumed', resume);
        const subscription = listen('manager-catalog-link', resume).then((unlisten) => {
            resume();
            return unlisten;
        });
        void subscription.catch(() => {});
        window.addEventListener(
            'pagehide',
            // Respond to pagehide events in the updates workflow
            () => {
                document.removeEventListener('close', resume, true);
                window.removeEventListener('editor-window-resumed', resume);
                void subscription.then(
                    (unlisten) => unlisten(),
                    () => {},
                );
            },
            { once: true },
        );
    }
    window.addEventListener('import-plugin-package', () => void reactive.importPackage());
    window.addEventListener('import-plugin-folder', () => void reactive.importPackage(true));
    // Respond to review-plugin-imports events in the updates workflow
    window.addEventListener('review-plugin-imports', (event) => {
        void reactive
            .acceptLocalReviews((event as CustomEvent<PluginPackageReview[]>).detail)
            .catch((error) => {
                reactive.error = String(error);
            });
    });
    Alpine.data('pluginCatalog', () => ({
        settingsOpen: false,
        reviewTab: 'overview',
        reviewTabs: [
            { id: 'overview', label: 'Overview' },
            { id: 'compatibility', label: 'Compatibility' },
            { id: 'package', label: 'Package details' },
        ],
        reviewReturnFocus: null as HTMLElement | null,
        sourceRepository: '',
        sourceUrl: '',
        sourceChannel: 'stable',
        sourceKind: 'repository',
        sourceToken: '',
        tokenState: '',
        // Derive repository source from the current updates state
        get repositorySource() {
            return this.sourceKind === 'repository';
        },
        sourceError: '',
        sourceSaving: false,
        closeBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Close the value
        closeValue() {
            return JSON.stringify([
                this.sourceRepository,
                this.sourceUrl,
                this.sourceChannel,
                this.sourceKind,
            ]);
        },
        // Derive updates from the current updates state
        get updates() {
            return reactive;
        },
        // Install the from catalog
        installFromCatalog(event: Event) {
            // Capture the opener before entering review disables the catalog buttons
            this.reviewReturnFocus = event.currentTarget as HTMLElement;
            return reactive.installAvailable(event);
        },
        // Initialize updates state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'catalog preferences', [
                'saveSource',
                'saveToken',
                'clearToken',
                'loadTokenStatus',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                ['saveSource', 'saveToken', 'clearToken'],
                {
                    name: 'Plugin catalog source',
                    retainOnUnmount: true,
                    // Return a snapshot of the current updates state
                    state: () =>
                        this.settingsOpen &&
                        (this.sourceToken !== '' || this.closeValue() !== this.closeBaseline)
                            ? 'dirty'
                            : 'clean',
                    // Discard pending updates edits without saving them
                    discard: () => {
                        this.sourceToken = '';
                        this.settingsOpen = false;
                        this.sourceError = '';
                    },
                },
            );
            this.$watch('$store.updates.catalogOpen', () => this.sync());
            this.$watch('$store.updates.linkedReady', () => this.focusLinkedPlugin());
            // Complete the deferred updates work using the current operation state
            this.$watch('$store.updates.review', () => {
                this.reviewTab = 'overview';
                // Complete the deferred updates work using the current operation state
                this.$nextTick(() => {
                    if (reactive.catalogOpen && reactive.review && !this.settingsOpen) {
                        (this.$refs.catalogBody as HTMLElement).scrollTop = 0;
                        (this.$refs.reviewBody as HTMLElement).scrollTop = 0;
                        this.focusReview();
                    }
                });
            });
            // Complete the deferred updates work using the current operation state
            this.$watch('$store.updates.reviewActive', () => {
                if (reactive.reviewActive) {
                    const active = document.activeElement as HTMLElement | null;
                    if (
                        !this.reviewReturnFocus &&
                        (this.$refs.catalogDialog as HTMLElement).contains(active)
                    )
                        this.reviewReturnFocus = active;
                    // Complete the deferred updates work using the current operation state
                    this.$nextTick(() => {
                        (this.$refs.catalogBody as HTMLElement).scrollTop = 0;
                        this.focusReview();
                    });
                } else {
                    const target = this.reviewReturnFocus;
                    this.reviewReturnFocus = null;
                    // Complete the deferred updates work using the current operation state
                    this.$nextTick(() => {
                        // x-show reveals the listing on the next animation frame
                        requestAnimationFrame(() => {
                            if (
                                reactive.catalogOpen &&
                                !reactive.reviewActive &&
                                target?.isConnected
                            )
                                target.focus();
                        });
                    });
                }
            });
            this.$watch('$store.app.route', () => {
                if (reactive.catalogOpen) void this.close();
            });
        },
        // Release updates state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
        },
        // Focus the linked plugin
        focusLinkedPlugin() {
            if (
                !reactive.linkedPlugin ||
                !reactive.catalogOpen ||
                this.settingsOpen ||
                reactive.reviewActive
            )
                return;
            // Complete the deferred updates work using the current operation state
            this.$nextTick(() => {
                (this.$refs.catalogDialog as HTMLDialogElement)
                    .querySelector<HTMLElement>('[data-linked-catalog-card="true"]')
                    ?.focus();
            });
        },
        // Sync the updates operation
        sync() {
            const dialog = this.$refs.catalogDialog as HTMLDialogElement;
            if (reactive.catalogOpen)
                // Complete the deferred updates work using the current operation state
                this.$nextTick(() => {
                    if (!reactive.catalogOpen) return;
                    if (!dialog.open) dialog.showModal();
                    if (reactive.reviewActive) this.focusReview();
                    else
                        (dialog.querySelector('input[type="search"]') as HTMLInputElement)?.focus();
                    this.focusLinkedPlugin();
                });
            else {
                reactive.clearLinkedPlugin();
                reactive.releaseDrag();
                this.settingsOpen = false;
                this.sourceError = '';
                this.sourceToken = '';
                this.closeBaseline = this.closeValue();
                this.tokenState = '';
                const opener = reactive.catalogOpener;
                reactive.catalogOpener = null;
                if (dialog.open) {
                    dialog.close();
                    // Complete the deferred updates work using the current operation state
                    this.$nextTick(() => {
                        if (opener?.isConnected && opener.getClientRects().length) opener.focus();
                    });
                }
            }
        },
        // Toggle the settings
        toggleSettings() {
            if (this.sourceSaving || reactive.working || reactive.reviewActive) return;
            if (this.settingsOpen) this.backToCatalog();
            else {
                this.sourceRepository = reactive.pluginsRepository;
                this.sourceUrl = reactive.catalogUrl;
                this.sourceChannel = reactive.channel;
                this.sourceKind = reactive.catalogSource;
                this.sourceToken = '';
                this.closeBaseline = this.closeValue();
                this.tokenState = '';
                if (this.repositorySource) void this.loadTokenStatus();
                this.sourceError = '';
                this.settingsOpen = true;
                this.$nextTick(() =>
                    // Complete the deferred updates work using the current operation state
                    requestAnimationFrame(() => {
                        if (this.settingsOpen)
                            (this.$refs.sourceRepository as HTMLInputElement).focus();
                    }),
                );
            }
        },
        // Load token status for the updates workflow
        async loadTokenStatus() {
            this.sourceToken = '';
            this.tokenState = '';
            const repository = this.sourceRepository.trim();
            if (!this.repositorySource || !/^[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+$/.test(repository))
                return;
            try {
                const state = await bridge.command('plugin_github_credential_status', {
                    repository,
                });
                if (repository === this.sourceRepository.trim() && this.repositorySource)
                    this.tokenState = state;
            } catch (error) {
                this.sourceError = String(error);
            }
        },
        // Save the token
        async saveToken() {
            if (this.sourceSaving) return;
            const repository = this.sourceRepository.trim();
            const token = this.sourceToken;
            this.sourceToken = '';
            this.sourceSaving = true;
            this.sourceError = '';
            try {
                await bridge.command('save_plugin_github_token', { repository, token });
                this.tokenState = 'configured';
            } catch (error) {
                this.sourceError = String(error);
            } finally {
                this.sourceSaving = false;
            }
        },
        // Clear the token
        async clearToken() {
            if (this.sourceSaving) return;
            this.sourceSaving = true;
            this.sourceToken = '';
            this.sourceError = '';
            try {
                await bridge.command('clear_plugin_github_token', {
                    repository: this.sourceRepository.trim(),
                });
                this.tokenState = 'missing';
            } catch (error) {
                this.sourceError = String(error);
            } finally {
                this.sourceSaving = false;
            }
        },
        // Focus the settings toggle
        focusSettingsToggle() {
            this.$nextTick(() =>
                // Complete the deferred updates work using the current operation state
                requestAnimationFrame(() => {
                    if (!this.settingsOpen && (this.$refs.catalogDialog as HTMLDialogElement).open)
                        (this.$refs.settingsToggle as HTMLButtonElement).focus();
                }),
            );
        },
        // Close source settings and clear unsaved credentials before returning focus
        backToCatalog() {
            if (this.sourceSaving) return;
            this.settingsOpen = false;
            this.sourceError = '';
            this.sourceToken = '';
            this.focusSettingsToggle();
        },
        // Save the source
        async saveSource() {
            if (this.sourceSaving || reactive.checking || reactive.working) return;
            const repository = this.sourceRepository.trim();
            const url = this.sourceUrl.trim();
            this.sourceError = '';
            if (
                (this.repositorySource || repository) &&
                !/^[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+$/.test(repository)
            ) {
                this.sourceError = 'Use a GitHub owner/repository identity.';
                return;
            }
            if (!this.repositorySource && url && !url.startsWith('https://')) {
                this.sourceError = 'Plugin catalog URL requires HTTPS.';
                return;
            }
            this.sourceSaving = true;
            try {
                // Keep unrelated application update preferences at their saved values
                const status = await bridge.command('update_status');
                await bridge.command('save_update_preferences', {
                    preferences: {
                        ...status.preferences,
                        plugins_repository: repository,
                        plugins_catalog_source: this.sourceKind,
                        plugins_catalog_url: url || undefined,
                        plugin_channel: this.sourceChannel,
                    },
                });
                reactive.pluginsRepository = repository;
                reactive.catalogSource = this.sourceKind;
                this.sourceToken = '';
                reactive.catalogUrl = url;
                reactive.channel = this.sourceChannel;
                reactive.error = '';
                try {
                    await bridge.command('check_updates', { product: 'plugins' });
                } catch (error) {
                    reactive.error = 'Catalog settings saved, but refresh failed: ' + String(error);
                }
                await reactive.refresh();
                this.settingsOpen = false;
                this.focusSettingsToggle();
            } catch (error) {
                this.sourceError = String(error);
            } finally {
                this.sourceSaving = false;
            }
        },
        // Cancel the active updates operation
        cancel() {
            if (this.settingsOpen) this.backToCatalog();
            else void this.close();
        },
        // Browse available items for updates
        browse() {
            reactive.catalogTab = 'browse';
        },
        // Select the catalog's updates tab
        updatesTab() {
            reactive.catalogTab = 'updates';
        },
        // Move keyboard focus and selection between catalog tabs
        navigateTabs(event: KeyboardEvent) {
            if (!tabNavigationKey(event.key)) return;
            event.preventDefault();
            reactive.catalogTab = nextTab(
                ['browse', 'updates'] as const,
                reactive.catalogTab,
                event.key,
            )!;
            const tab =
                reactive.catalogTab === 'browse' ? this.$refs.browseTab : this.$refs.updatesTab;
            (tab as HTMLButtonElement).focus();
        },
        // Select the review tab
        selectReviewTab(event: Event) {
            const tab = (event.currentTarget as HTMLElement).dataset.reviewTab;
            if (this.reviewTabs.some((item) => item.id === tab)) this.reviewTab = tab!;
        },
        // Focus the review
        focusReview() {
            this.$nextTick(() =>
                // Complete the deferred updates work using the current operation state
                requestAnimationFrame(() => {
                    if (!reactive.catalogOpen || !reactive.reviewActive || this.settingsOpen)
                        return;
                    // x-show reveals both review states on the next animation frame
                    const target = reactive.reviewLoading
                        ? (this.$refs.reviewLoadingHeading as HTMLElement)
                        : (this.$refs.reviewTabs as HTMLElement).querySelector<HTMLButtonElement>(
                              '[aria-selected="true"]',
                          );
                    target?.focus({ preventScroll: true });
                }),
            );
        },
        // Move keyboard focus and selection between available review tabs
        navigateReviewTabs(event: KeyboardEvent) {
            if (!tabNavigationKey(event.key)) return;
            event.preventDefault();
            const tabs = this.reviewTabs;
            const next = nextTab(
                tabs.map((tab) => tab.id),
                this.reviewTab,
                event.key,
            );
            if (!next) return;
            this.reviewTab = next;
            (this.$refs.reviewTabs as HTMLElement)
                .querySelector<HTMLButtonElement>(`[data-review-tab="${this.reviewTab}"]`)
                ?.focus({ preventScroll: true });
        },
        // Release updates state and associated lifecycle resources
        async close() {
            if (reactive.working || this.sourceSaving) return;
            reactive.catalogOpen = false;
            this.sync();
            try {
                await reactive.closeReview();
                reactive.offerPluginSetup();
            } catch (error) {
                reactive.error = String(error);
            }
        },
    }));
}
