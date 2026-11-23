import { unlockVault } from './vault-password';
import { ApplicationUpdateDraft, CatalogUpdateDraft } from './update-preference-drafts';
import { paginate } from './pagination';
import { nextTab, tabNavigationKey } from './tab-navigation';
import * as fileDialogs from './window-dialogs';
import { registerArchiveReviewer } from './archive-import';
import { listen } from '@tauri-apps/api/event';
import { version as managerVersion } from '../package.json';
import type AlpineType from '@alpinejs/csp';
import { bridge, native } from './bridge';
import type {
    ActionScope,
    PermissionScope,
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
import { permissionLabel, scopeDetails } from './plugin-permissions';
import { VaultInstallReview, type VaultSetupSlot } from './vault-install-review';

function displayField(value: string) {
    const words = value.replace(/[_-]+/g, ' ').replace(/\bapi\b/gi, 'API');
    return words.charAt(0).toUpperCase() + words.slice(1);
}

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
    | { kind: 'preparing'; fromCache?: boolean }
    | {
          kind: 'reviewing' | 'committing';
          review: PluginPackageReview;
          allowChannelChange: boolean;
          replaceInstalled: boolean;
          forceReinstall: boolean;
          fromCache?: boolean;
          approvals?: ActionScope[];
          requiredReviewId?: string;
      };
export type InstallationReviewRequest =
    | { kind: 'local'; review: PluginPackageReview }
    | { kind: 'remote'; id: string; channel: string };
// Start each package review with every installation acknowledgement unset
const reviewing = (review: PluginPackageReview, fromCache = false): InstallationReviewPhase => ({
    kind: 'reviewing',
    ...(review.retained_services?.length ? { approvals: review.retained_services } : {}),
    review,
    fromCache,
    allowChannelChange: false,
    replaceInstalled: false,
    forceReinstall: false,
});

// Match full scopes rather than display labels so different destinations or modes stay separate
function reviewScopeKey(scope: PermissionScope) {
    return JSON.stringify(scope, Object.keys(scope).sort());
}

function reviewGrantKey({ action, scope }: ActionScope) {
    return JSON.stringify([action, reviewScopeKey(scope)]);
}

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
        vaultReview: new VaultInstallReview({
            list: (id, slotId, action) =>
                bridge.command('vault_setup_slot_entries', { id, slotId, action }),
            cancel: (id, ticket) => bridge.command('vault_cancel_picker', { id, ticket }),
            create: (id, slotId, action, ticket, input) =>
                bridge.command('vault_setup_create_entry', { id, slotId, action, ticket, input }),
            status: () => bridge.command('vault_status'),
            unlock: unlockVault,
            initialize: () => bridge.command('vault_initialize'),
            permission: (id, slotId, action, allow) =>
                bridge.command('vault_setup_permission', { id, slotId, action, allow }),
            select: (id, slotId, action, ticket, entryId, revision, allowDirectRead) =>
                bridge.command('vault_setup_select', {
                    id,
                    slotId,
                    action,
                    ticket,
                    entryId,
                    revision,
                    allowDirectRead,
                }),
        }),
        setServiceGrants(items: ActionScope[], allow: boolean) {
            const phase = this.reviewPhase;
            if (phase.kind !== 'reviewing' || !items.length) return;
            const keys = new Set(items.map(reviewGrantKey));
            const approvals = (phase.approvals ?? []).filter(
                (item) => !keys.has(reviewGrantKey(item)),
            );
            if (allow) approvals.push(...items);
            this.reviewPhase = { ...phase, approvals };
        },
        reviewServiceSelection(tab: 'required' | 'optional') {
            const phase = this.reviewPhase;
            if (phase.kind !== 'reviewing' && phase.kind !== 'committing')
                return { total: 0, selected: 0 };
            const items = (phase.review.reviewable_services ?? []).filter(
                (item) => item.required === (tab === 'required'),
            );
            const approved = new Set((phase.approvals ?? []).map(reviewGrantKey));
            return {
                total: items.length,
                selected: items.filter((item) => approved.has(reviewGrantKey(item))).length,
            };
        },
        // Bulk approval never crosses from required to optional or selects Vault entries
        pickAllServices(event: Event) {
            const checkbox = event.currentTarget as HTMLInputElement;
            const review = this.review;
            const tab = checkbox.dataset.required;
            if (
                !review ||
                checkbox.dataset.review !== review.id ||
                (tab !== 'required' && tab !== 'optional')
            )
                return;
            const items = (review.reviewable_services ?? []).filter(
                (grant) => grant.required === (tab === 'required'),
            );
            this.setServiceGrants(items, checkbox.checked);
        },
        // Toggle one exact action and scope without granting its siblings
        pickServiceAction(event: Event) {
            const checkbox = event.currentTarget as HTMLInputElement;
            const review = this.review;
            if (!review || checkbox.dataset.review !== review.id) return;
            const index = Number(checkbox.dataset.index);
            const grant = review.reviewable_services?.[index];
            if (grant) this.setServiceGrants([grant], checkbox.checked);
        },
        // Apply a purpose checkbox to the exact action grants represented by that reason
        pickServiceReason(event: Event) {
            const checkbox = event.currentTarget as HTMLInputElement;
            const review = this.review;
            if (!review || checkbox.dataset.review !== review.id) return;
            const index = Number(checkbox.dataset.index);
            const changes = this.permissionChanges;
            const anchor = changes.find((row) => row.index === index && index >= 0);
            if (!anchor) return;
            const items = changes.flatMap((row) => {
                const item = review.reviewable_services?.[row.index];
                return item &&
                    row.groupKey === anchor.groupKey &&
                    row.reasonKey === anchor.reasonKey
                    ? [item]
                    : [];
            });
            this.setServiceGrants(items, checkbox.checked);
        },
        pickServiceGroup(event: Event) {
            const checkbox = event.currentTarget as HTMLInputElement;
            const review = this.review;
            if (!review || checkbox.dataset.review !== review.id) return;
            const items = this.permissionChanges.flatMap((row) => {
                const item = review.reviewable_services?.[row.index];
                return item && row.groupKey === checkbox.dataset.permission ? [item] : [];
            });
            this.setServiceGrants(items, checkbox.checked);
        },
        reviewActionName(action: string) {
            const prefix = this.review?.package.id + '.';
            return displayField(action.startsWith(prefix) ? action.slice(prefix.length) : action);
        },
        get permissionChanges() {
            const diff = this.review?.permission_diff;
            if (!diff) return [];
            const reviewable = new Map(
                (this.review?.reviewable_services ?? []).map((item, index) => [
                    reviewGrantKey(item),
                    index,
                ]),
            );
            const phase = this.reviewPhase;
            const approved = new Set(
                phase.kind === 'reviewing' || phase.kind === 'committing'
                    ? phase.approvals?.map(reviewGrantKey)
                    : [],
            );
            return (['added', 'removed', 'unchanged'] as const).flatMap((change) =>
                diff[change].map((item) => {
                    const key = reviewGrantKey(item);
                    const index = change === 'removed' ? -1 : (reviewable.get(key) ?? -1);
                    const purpose =
                        this.review?.manifest.permissions?.actions[item.action]?.purpose?.trim() ||
                        '';
                    const searchDetails = scopeDetails(item.scope);
                    return {
                        key: change + key,
                        reasonKey: JSON.stringify(
                            purpose ? ['purpose', purpose] : ['action', item.action],
                        ),
                        groupKey: JSON.stringify([
                            item.required,
                            reviewScopeKey(item.scope),
                            change === 'removed'
                                ? 'removed'
                                : index >= 0
                                  ? 'reviewable'
                                  : 'informational',
                        ]),
                        index,
                        selected: index >= 0 && approved.has(key),
                        label: permissionLabel(item.scope),
                        required: item.required,
                        status: { added: 'Added', removed: 'Removed', unchanged: 'Unchanged' }[
                            change
                        ],
                        action: item.action,
                        actionLabel: this.reviewActionName(item.action),
                        purpose,
                        availability:
                            change === 'removed'
                                ? 'Removed'
                                : item.scope.id === 'vault.read-secret' ||
                                    item.scope.id === 'vault.read-location'
                                  ? 'Vault setup'
                                  : 'Unavailable',
                        searchDetails,
                        details:
                            item.scope.id === 'vault.read-location' &&
                            item.scope.field === item.scope.entry_kind
                                ? ''
                                : searchDetails,
                    };
                }),
            );
        },
        get reviewVaultSlots() {
            return (this.review?.manifest?.setup_slots ?? []) as VaultSetupSlot[];
        },
        selectedVaultLabel(slotId: string, action: string) {
            const review = this.review;
            return (
                (review && this.vaultReview.selection(review.id, slotId, action)?.entry.label) ||
                (review?.retained_slots?.some(
                    (slot) => slot.id === slotId && slot.action === action,
                )
                    ? 'Retaining approved entry for this verified publisher'
                    : '')
            );
        },
        openVaultSlot(event: Event) {
            const slotId = (event.currentTarget as HTMLElement).dataset.slotId;
            const action = (event.currentTarget as HTMLElement).dataset.action;
            const slot = this.reviewVaultSlots.find(
                (item) => item.id === slotId && item.action === action,
            );
            if (this.review && slot && !this.working)
                return this.vaultReview.open(
                    this.review.id,
                    slot,
                    event.currentTarget as HTMLElement,
                );
        },
        syncVaultChoice(element: HTMLSelectElement) {
            const id = this.vaultReview.pending?.id ?? '';
            // A newly returned inline entry may mount its option in a later Alpine effect.
            // Observe its list as well as selection, and write the native property after both.
            const count = this.vaultReview.entries.length;
            void Alpine.nextTick(() => {
                if (!element.isConnected || this.vaultReview.entries.length !== count) return;
                if ((this.vaultReview.pending?.id ?? '') === id) element.value = id;
            });
        },
        chooseVaultEntry(event: Event) {
            this.vaultReview.choose((event.currentTarget as HTMLSelectElement).value);
        },
        confirmVaultEntry() {
            if (this.review && !this.working)
                return this.vaultReview.confirm(this.review.id, this.vaultReview.directRead);
        },
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
        // Report whether package validation is in progress
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
        // Offer force reinstall only for an explicitly reviewed catalog version exception
        get forceReinstallAvailable() {
            return Boolean(this.review?.compatible && this.review.force_reinstall_available);
        },
        // Explain what the selected catalog version would replace
        get forceReinstallHint() {
            return this.review?.version_change === 'downgrade'
                ? 'Replaces the installed version with this older package.'
                : 'Replaces the installed package with this same-version package.';
        },
        get forceReinstall() {
            return (
                (this.reviewPhase.kind === 'reviewing' || this.reviewPhase.kind === 'committing') &&
                this.reviewPhase.forceReinstall
            );
        },
        set forceReinstall(value: boolean) {
            if (this.reviewPhase.kind === 'reviewing' && this.forceReinstallAvailable)
                this.reviewPhase.forceReinstall = value;
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
        // Only label a transfer as downloading after the native job reports it
        get reviewLoadingTitle() {
            if (this.reviewPhase.kind === 'preparing')
                return this.reviewPhase.fromCache ? 'Using cached package…' : 'Preparing review…';
            if (this.reviewDownload?.download_phase === 'using_cache')
                return 'Using cached package…';
            if (this.reviewDownload?.download_phase === 'resolving_source')
                return 'Contacting catalog…';
            return this.reviewDownload?.download_phase === 'downloading'
                ? 'Downloading package…'
                : 'Checking package cache…';
        },
        get reviewLoadingDetail() {
            if (this.reviewPhase.kind === 'preparing')
                return 'Checking package files and compatibility before installation.';
            if (this.reviewDownload?.download_phase === 'using_cache')
                return 'The cached package matches the catalog.';
            if (this.reviewDownload?.download_phase === 'resolving_source')
                return 'Finding the package at the catalog source.';
            return this.reviewDownload?.download_phase === 'downloading'
                ? 'Getting the package from the catalog source.'
                : 'Looking for a local copy that matches the catalog.';
        },
        get reviewUsedCache() {
            const phase = this.reviewPhase;
            return (phase.kind === 'reviewing' || phase.kind === 'committing') && !!phase.fromCache;
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
        // Explain the official development publication lane without changing sources.
        get developmentSourceHint() {
            return Boolean(
                this.catalogSource === 'repository' &&
                this.pluginsRepository.trim().toLowerCase() === 'streamdockmanager/plugins' &&
                this.browseChannel === 'development' &&
                this.selectedCatalog &&
                !this.selectedCatalog.plugins.length &&
                !this.catalogSearch.trim() &&
                !this.linkedPlugin &&
                !this.checking &&
                !this.pluginsError,
            );
        },
        // Derive has catalog from the current updates state
        get hasCatalog() {
            return (
                this.status?.available_catalogs?.some((c) => c.channel === this.channel) || false
            );
        },
        // Require a visit only for newly declared required grants or required Vault entries
        get needsRequiredPermissionReview() {
            const review = this.review;
            if (!review) return false;
            const retained = new Set((review.retained_services ?? []).map(reviewGrantKey));
            return (
                (review.permission_diff?.added ?? []).some((item) => item.required) ||
                (review.reviewable_services ?? []).some(
                    (item) => item.required && !retained.has(reviewGrantKey(item)),
                ) ||
                this.reviewVaultSlots.some(
                    (slot) =>
                        slot.required &&
                        !review.retained_slots?.some(
                            (stored) => stored.action === slot.action && stored.id === slot.id,
                        ),
                )
            );
        },
        // Tie the required review step to this package review, not an earlier package
        markRequiredPermissionsReviewed() {
            const phase = this.reviewPhase;
            if (phase.kind === 'reviewing' && phase.requiredReviewId !== phase.review.id)
                phase.requiredReviewId = phase.review.id;
        },
        get requiredPermissionReviewComplete() {
            return (
                !this.needsRequiredPermissionReview ||
                (this.reviewPhase.kind === 'reviewing' &&
                    this.reviewPhase.requiredReviewId === this.reviewPhase.review.id)
            );
        },
        // Retained grants are preselected; an unchecked required scope always blocks install
        get missingRequiredServiceApprovals() {
            const phase = this.reviewPhase;
            if (phase.kind !== 'reviewing' && phase.kind !== 'committing') return false;
            const approved = new Set((phase.approvals ?? []).map(reviewGrantKey));
            return (phase.review.reviewable_services ?? []).some(
                (grant) => grant.required && !approved.has(reviewGrantKey(grant)),
            );
        },
        get requiredPermissionsReady() {
            return (
                !this.missingRequiredServiceApprovals &&
                (!this.review ||
                    this.vaultReview.ready(
                        this.review.id,
                        this.reviewVaultSlots.filter(
                            (slot) =>
                                !this.review?.retained_slots?.some(
                                    (s) => s.action === slot.action && s.id === slot.id,
                                ),
                        ),
                    ))
            );
        },
        // Derive can commit from the current updates state
        get canCommit() {
            return (
                Boolean(this.review?.compatible) &&
                !this.working &&
                !this.vaultReview.slot &&
                this.requiredPermissionReviewComplete &&
                this.requiredPermissionsReady &&
                (!this.review?.channel_change || this.allowChannelChange) &&
                (!this.review?.force_reinstall_available || this.forceReinstall) &&
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
            return plugin.vsd_craft_compatible
                ? 'VSD Craft: Compatible'
                : 'VSD Craft: Not compatible';
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
            if (typeof window !== 'undefined')
                window.dispatchEvent(new Event('open-plugin-catalog-dialog'));
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
                        const fromCache = job.download_phase === 'using_cache';
                        this.reviewPhase = { kind: 'preparing', fromCache };
                        try {
                            const review = await bridge.command('prepare_catalog_download', { id });
                            if (
                                this.catalogOpen &&
                                epoch === this.reviewEpoch &&
                                !windowCloseOwners.closing
                            )
                                this.reviewPhase = reviewing(review, fromCache);
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
                        forceReinstall: phase.forceReinstall === true,
                        ...(phase.approvals?.length ? { approvals: phase.approvals } : {}),
                    }),
                );
                this.vaultReview.reset();
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
            this.vaultReview.reset();
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
        downloadJobLabel(job: UpdateJob) {
            if (job.state === 'complete' && job.download_phase === 'using_cache')
                return 'Used cached package';
            if (job.state !== 'running') return job.state;
            if (job.download_phase === 'downloading') return 'Downloading…';
            if (job.download_phase === 'resolving_source') return 'Contacting catalog…';
            if (job.download_phase === 'using_cache') return 'Using cached package…';
            return job.operation === 'download' ? 'Checking package cache…' : job.state;
        },
        showDownloadProgress(job: UpdateJob) {
            return job.state === 'running' && job.download_phase === 'downloading' && job.total > 0;
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
                reactive.review &&
                (reactive.allowChannelChange ||
                    reactive.replaceInstalled ||
                    reactive.forceReinstall)
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
        pageNumber: 1,
        matches: [] as CatalogPlugin[],
        catalogPage: paginate<CatalogPlugin>([], 1, 24),
        updatePage() {
            this.catalogPage = paginate(this.matches, this.pageNumber, 24);
        },
        updateMatches() {
            this.matches = reactive.catalogOpen ? reactive.availablePlugins : [];
        },
        previousPage() {
            this.pageNumber = Math.max(1, this.catalogPage.page - 1);
        },
        nextPage() {
            this.pageNumber = Math.min(this.catalogPage.pages, this.catalogPage.page + 1);
        },
        settingsOpen: false,
        reviewTab: 'overview',
        reviewTabs: [
            { id: 'overview', label: 'Overview' },
            { id: 'compatibility', label: 'Compatibility' },
            { id: 'permissions', label: 'Permissions' },
            { id: 'package', label: 'Package details' },
        ],
        permissionTab: 'required',
        permissionQuery: '',
        get reviewId() {
            return reactive.review?.id ?? '';
        },
        // Set both native properties so partial approval is announced by assistive technology
        syncPermissionGroup(input: HTMLInputElement, total: number, selected: number) {
            input.checked = total > 0 && selected === total;
            input.indeterminate = selected > 0 && !input.checked;
        },
        get requiredPermissionCount() {
            const review = reactive.review;
            if (!review || reactive.working) return 0;
            const visitPending = !reactive.requiredPermissionReviewComplete;
            const requiredSlots = reactive.reviewVaultSlots.filter((slot) => slot.required);
            const count = new Set<string>();
            if (visitPending) {
                for (const grant of review.permission_diff?.added ?? []) {
                    if (!grant.required) continue;
                    const { scope } = grant;
                    const slot = requiredSlots.find(
                        (slot) =>
                            (scope.id === 'vault.read-secret' ||
                                scope.id === 'vault.read-location') &&
                            slot.action === grant.action &&
                            slot.field === scope.field,
                    );
                    count.add(
                        slot
                            ? JSON.stringify(['slot', slot.action, slot.id])
                            : reviewScopeKey(scope),
                    );
                }
            }
            const phase = reactive.reviewPhase;
            const approved = new Set(
                phase.kind === 'reviewing' ? phase.approvals?.map(reviewGrantKey) : [],
            );
            const retained = new Set(review.retained_services?.map(reviewGrantKey));
            for (const grant of review.reviewable_services ?? []) {
                const key = reviewGrantKey(grant);
                if (grant.required && (!approved.has(key) || (visitPending && !retained.has(key))))
                    count.add(reviewScopeKey(grant.scope));
            }
            for (const slot of requiredSlots) {
                const retained = review.retained_slots?.some(
                    (item) => item.action === slot.action && item.id === slot.id,
                );
                if (
                    !retained &&
                    (visitPending ||
                        !reactive.vaultReview.selection(review.id, slot.id, slot.action))
                )
                    count.add(JSON.stringify(['slot', slot.action, slot.id]));
            }
            return count.size;
        },
        get pendingPermissionLabel() {
            const count = this.requiredPermissionCount;
            return 'Permissions (' + count + ' to review)';
        },
        get normalizedPermissionQuery() {
            return this.permissionQuery.trim().toLowerCase();
        },
        permissionTextMatches(value?: string | null) {
            return (
                !!this.normalizedPermissionQuery &&
                !!value?.toLowerCase().includes(this.normalizedPermissionQuery)
            );
        },
        clearPermissionSearch() {
            this.permissionQuery = '';
            (this.$refs.permissionSearch as HTMLInputElement)?.focus({ preventScroll: true });
        },
        // Open matching reasons during search without losing the previous disclosure state
        syncPermissionDisclosure(details: HTMLDetailsElement, match: boolean) {
            if (match && details.dataset.beforeSearch === undefined) {
                details.dataset.beforeSearch = String(details.open);
                details.open = true;
            } else if (!match && details.dataset.beforeSearch !== undefined) {
                details.open = details.dataset.beforeSearch === 'true';
                delete details.dataset.beforeSearch;
            }
        },
        get permissionGroups() {
            const changes = reactive.permissionChanges;
            const slots = reactive.reviewVaultSlots;
            const query = this.normalizedPermissionQuery;
            const matches = (...values: (string | null | undefined)[]) =>
                values.some((value) => this.permissionTextMatches(value));
            const groups = (['required', 'optional'] as const).map((id) => {
                const required = id === 'required';
                const services = changes.filter((change) => change.required === required);
                const entries = slots.filter((slot) => slot.required === required);
                const grouped = new Map<string, typeof services>();
                for (const row of services) {
                    const actions = grouped.get(row.groupKey);
                    if (actions) actions.push(row);
                    else grouped.set(row.groupKey, [row]);
                }
                const shownServices = Array.from(grouped, ([key, actions]) => {
                    const first = actions[0]!;
                    const byReason = new Map<string, typeof actions>();
                    for (const row of actions) {
                        const reason = byReason.get(row.reasonKey);
                        if (reason) reason.push(row);
                        else byReason.set(row.reasonKey, [row]);
                    }
                    const reasons = Array.from(byReason, ([key, rows]) => ({
                        key,
                        label: rows[0]!.purpose || rows[0]!.actionLabel,
                        index: rows[0]!.index,
                        actions: rows
                            .map((row) => (row.purpose ? row.actionLabel : row.action))
                            .join(', '),
                        status: [...new Set(rows.map((row) => row.status))].join(' · '),
                        actionRows: rows.map((row) => ({
                            key: row.key,
                            index: row.index,
                            label: row.actionLabel,
                            selected: row.selected,
                        })),
                        match: rows.some((row) =>
                            matches(row.action, row.actionLabel, row.purpose, row.status),
                        ),
                        selected: rows.filter((row) => row.selected).length,
                        total: rows.length,
                    }));
                    const titleMatch = matches(
                        first.label,
                        first.details ? '' : first.searchDetails,
                    );
                    return {
                        key,
                        reasons,
                        titleMatch,
                        match:
                            titleMatch ||
                            matches(first.details) ||
                            reasons.some((reason) => reason.match),
                        reasonMatch: reasons.some((reason) => reason.match),
                        label: first.label,
                        details: first.details,
                        availability: first.availability,
                        total: actions.filter((action) => action.index >= 0).length,
                        selected: actions.filter((action) => action.selected).length,
                    };
                }).filter((permission) => !query || permission.match);
                const shownSlots = query
                    ? entries.filter((slot) =>
                          matches(
                              slot.action,
                              this.slotName(slot),
                              slot.id,
                              slot.field,
                              slot.entry_kind,
                              slot.purpose,
                              slot.origin,
                          ),
                      )
                    : entries;
                return {
                    id,
                    label: required ? 'Required' : 'Optional',
                    empty: required
                        ? 'No required permissions or Vault entries.'
                        : 'No optional permissions or Vault entries.',
                    services: shownServices,
                    slots: shownSlots,
                    count: grouped.size + entries.length,
                    matchCount: shownServices.length + shownSlots.length,
                };
            });
            return groups.map((group, index) => {
                const other = groups[1 - index]!;
                return {
                    ...group,
                    tabCount: query ? group.matchCount : group.count,
                    other: {
                        id: other.id,
                        matchCount: other.matchCount,
                        label:
                            other.label +
                            ': ' +
                            other.matchCount +
                            (other.matchCount === 1 ? ' match' : ' matches'),
                    },
                };
            });
        },
        showPermissionMatches(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.group;
            if (id !== 'required' && id !== 'optional') return;
            this.permissionTab = id;
            document.getElementById('review-group-' + id)?.focus({ preventScroll: true });
        },
        slotName(slot: VaultSetupSlot) {
            return slot.label || displayField(slot.field);
        },
        matchingSlotDetails(slot: VaultSetupSlot) {
            if (!this.normalizedPermissionQuery) return '';
            const named = [
                this.slotName(slot),
                reactive.reviewActionName(slot.action),
                slot.origin,
            ];
            if (named.some((value) => this.permissionTextMatches(value))) return '';
            return (
                [slot.purpose, slot.entry_kind, slot.field, slot.id, slot.action].find((value) =>
                    this.permissionTextMatches(value),
                ) ?? ''
            );
        },
        get pickerContext() {
            const slot = reactive.vaultReview.slot;
            return slot ? this.slotName(slot) + ' · ' + reactive.reviewActionName(slot.action) : '';
        },
        selectPermissionTab(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.group;
            if (id === 'required' || id === 'optional') {
                this.permissionTab = id;
                (event.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
            }
        },
        navigatePermissionTabs(event: KeyboardEvent) {
            const next = nextTab(['required', 'optional'], this.permissionTab, event.key);
            if (!next) return;
            event.preventDefault();
            this.permissionTab = next;
            (event.currentTarget as HTMLElement)
                .querySelector<HTMLButtonElement>(`[data-group="${next}"]`)
                ?.focus();
        },
        reviewRequiredPermissions() {
            this.reviewTab = 'permissions';
            this.permissionTab = 'required';
            this.permissionQuery = '';
            document.getElementById('review-permissions-tab')?.focus({ preventScroll: true });
        },
        // Mark the current package reviewed only while its required permission panel is active
        recordRequiredPermissionReview() {
            if (
                reactive.catalogOpen &&
                this.reviewTab === 'permissions' &&
                this.permissionTab === 'required' &&
                !this.normalizedPermissionQuery &&
                reactive.needsRequiredPermissionReview
            )
                reactive.markRequiredPermissionsReviewed();
        },
        resetReviewTabs() {
            this.permissionQuery = '';
            this.reviewTab = 'overview';
            const [required, optional] = this.permissionGroups;
            this.permissionTab =
                !reactive.needsRequiredPermissionReview && !required.count && optional.count
                    ? 'optional'
                    : 'required';
        },
        pickerReturnFocus: null as HTMLElement | null,
        syncVaultPicker() {
            const picker = this.$refs.vaultPickerDialog as HTMLDialogElement;
            if (reactive.catalogOpen && reactive.review && reactive.vaultReview.slot) {
                if (!picker.open) {
                    this.pickerReturnFocus = reactive.vaultReview.opener;
                    picker.showModal();
                    (picker.querySelector('.feature-body') as HTMLElement).scrollTop = 0;
                }
            } else if (picker.open) {
                picker.close();
                const opener = this.pickerReturnFocus;
                this.pickerReturnFocus = null;
                if (reactive.catalogOpen && reactive.review && opener?.isConnected)
                    opener.focus({ preventScroll: true });
            }
        },
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
            for (const field of ['catalogOpen', 'catalogSearch', 'browseChannel', 'linkedPlugin'])
                this.$watch('$store.updates.' + field, () => {
                    this.pageNumber = 1;
                });
            this.resetReviewTabs();
            if (reactive.catalogOpen) this.sync();
            this.$watch('$store.updates.catalogOpen', () => this.sync());
            this.$watch('$store.updates.reviewActive', () => this.sync());
            this.$watch('$store.updates.linkedReady', () => this.focusLinkedPlugin());
            this.$watch('reviewId', () => {
                this.resetReviewTabs();
                if (reactive.review && reactive.catalogOpen) {
                    (this.$refs.reviewBody as HTMLElement).scrollTop = 0;
                    this.focusReview();
                }
            });
            this.$watch('reviewTab', () => {
                (this.$refs.reviewBody as HTMLElement).scrollTop = 0;
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
            const catalog = this.$refs.catalogDialog as HTMLDialogElement;
            const review = this.$refs.reviewDialog as HTMLDialogElement;
            if (reactive.catalogOpen) {
                this.$nextTick(() => {
                    if (!reactive.catalogOpen) return;
                    if (reactive.reviewActive) {
                        if (catalog.open) {
                            const active = document.activeElement as HTMLElement | null;
                            if (!this.reviewReturnFocus && catalog.contains(active))
                                this.reviewReturnFocus = active;
                            catalog.close();
                        }
                        if (!review.open) {
                            review.showModal();
                            this.focusReview();
                        }
                        return;
                    }
                    if (review.open) review.close();
                    if (!catalog.open) catalog.showModal();
                    const target = this.reviewReturnFocus;
                    this.reviewReturnFocus = null;
                    if (target) {
                        requestAnimationFrame(() => {
                            if (catalog.open && !reactive.reviewActive && target.isConnected)
                                target.focus();
                        });
                    } else {
                        catalog.querySelector<HTMLInputElement>('input[type="search"]')?.focus();
                    }
                    this.focusLinkedPlugin();
                });
                return;
            }
            reactive.clearLinkedPlugin();
            reactive.releaseDrag();
            this.settingsOpen = false;
            this.sourceError = '';
            this.sourceToken = '';
            this.closeBaseline = this.closeValue();
            this.tokenState = '';
            this.reviewReturnFocus = null;
            const opener = reactive.catalogOpener;
            reactive.catalogOpener = null;
            if (review.open) review.close();
            if (catalog.open) catalog.close();
            this.$nextTick(() => {
                if (opener?.isConnected && opener.getClientRects().length) opener.focus();
            });
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
            if (this.reviewTabs.some((item) => item.id === tab)) {
                this.reviewTab = tab!;
                (event.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
            }
        },
        // Focus the review
        focusReview() {
            this.$nextTick(() =>
                // Complete the deferred updates work using the current operation state
                requestAnimationFrame(() => {
                    if (
                        !reactive.catalogOpen ||
                        !reactive.reviewActive ||
                        reactive.vaultReview.slot
                    )
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
