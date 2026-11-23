import { cloneJson } from './json-clone';
import { mediaOptimizationGuidance, canOptimizeStill } from './media-guidance';
import { viewportThumbnails } from './viewport-thumbnails';
import { decodeMediaImages } from './media-decode';
import { windowWork } from './window-work';
import { formatBinarySize } from './binary-size';
import { ownWindowMethods } from './window-work';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type {
    MediaLibraryPage,
    MediaLibraryEntry,
    MediaCleanupReview,
    MediaDeletionReview,
    MediaOptimizationPreview,
} from '../generated/runtime';
import { openVideoEditor } from './video-editor';
import type { MediaOptions } from '../generated/runtime';
import { retainMediaFor, releaseMediaOwner } from './media-leases';
import { chooseMediaInput, droppedMedia, mediaFormats, selectMediaInput } from './media-input';
export interface MediaPickerRequest {
    owner: string;
    current: () => boolean;
    formats: string[];
    choose: (asset: string, options?: MediaOptions) => void | Promise<void>;
    rebase?: () => void;
}
// Open the media library with the optional selection request
export function openMediaLibrary(request?: MediaPickerRequest) {
    window.dispatchEvent(new CustomEvent('open-media-library', { detail: { request } }));
}
// Register media browsing, optimization, and reviewed deletion controls
export function registerMediaLibrary(Alpine: typeof AlpineType, app: () => AppState) {
    // Create isolated reactive state for the media library controller
    Alpine.data('mediaLibrary', () => {
        let thumbnails:
            | ReturnType<typeof viewportThumbnails<{ entry: MediaLibraryEntry; epoch: number }>>
            | undefined;
        let optimizationRevision = 0;
        let listing = false;
        let auditNext = false;
        let pendingListing: {
            epoch: number;
            audit: boolean;
            page: number;
            pageSize: number;
            query: string;
        } | null = null;
        return {
            opened: false,
            unregisterCloseOwner: undefined as (() => void) | undefined,
            busy: false,
            working: false,
            importing: false,
            auditing: false,
            epoch: 0,
            page: 1,
            pageSize: '12',
            query: '',
            error: '',
            notice: '',
            optimizationAsset: '',
            optimizationEdge: '512',
            optimization: null as MediaOptimizationPreview | null,
            optimizationBusy: false,
            optimizationError: '',
            cleanupError: '',
            deletionError: '',
            deletion: null as MediaDeletionReview | null,
            deletionFocus: null as HTMLButtonElement | null,
            selected: [] as string[],
            request: null as MediaPickerRequest | null,
            data: null as MediaLibraryPage | null,
            review: null as MediaCleanupReview | null,
            thumbs: {} as Record<string, string>,
            thumbErrors: {} as Record<string, string>,
            thumbnailBusy: false,
            optimizationGuidance: mediaOptimizationGuidance,
            canOptimize: canOptimizeStill,
            // Return optimization summary for the current media library view
            get optimizationSummary() {
                const value = this.optimization;
                if (!value) return '';
                const change =
                    value.output_bytes < value.original_bytes
                        ? 'smaller'
                        : value.output_bytes > value.original_bytes
                          ? 'larger'
                          : 'the same size';
                return `${value.source_width} × ${value.source_height}, ${this.size(value.original_bytes)} → ${value.width} × ${value.height}, ${this.size(value.output_bytes)} (${change}). PNG preserves transparency, but reducing resolution loses fine detail. The copy adds storage; the original and all assignments stay unchanged.`;
            },
            // Return heading for the current media library view
            get heading() {
                return this.request ? 'Choose existing media' : 'Media library';
            },
            // Return cleanup description for the current media library view
            get cleanupDescription() {
                if (!this.review) return '';
                const count = this.review.assets.length;
                return `Permanently delete ${count} unused ${count === 1 ? 'asset' : 'assets'} (${this.size(this.review.logical_bytes)} logical size)?`;
            },
            // Return cleanup estimate for the current media library view
            get cleanupEstimate() {
                return this.review
                    ? 'Estimated reclaimable: ' +
                          (this.review.storage_estimate_available
                              ? this.size(this.review.estimated_reclaimable_bytes)
                              : 'Unavailable on this platform')
                    : '';
            },
            // Return pages for the current media library view
            get pages() {
                return this.data
                    ? Math.max(1, Math.ceil(this.data.total / this.data.page_size))
                    : 1;
            },
            // Check whether selected media can be reviewed for deletion
            get retentionChecked() {
                return this.data?.retention_checked === true;
            },
            get canReview() {
                return (
                    !this.request &&
                    !this.busy &&
                    !this.working &&
                    !this.thumbnailBusy &&
                    this.retentionChecked &&
                    this.data?.cleanup_available &&
                    this.selected.length > 0
                );
            },
            // Format a media byte count for display
            size(bytes: number) {
                return formatBinarySize(bytes, { maxUnit: 2, precision: 1 });
            },
            // Format the media entry identity for the selection control
            identity(entry: MediaLibraryEntry) {
                return `${entry.clip_info ? entry.clip_info.name + ' · revision ' + entry.clip_info.revision + ' · ' : ''}${entry.format.toUpperCase()}${entry.width ? ' · ' + entry.width + ' × ' + entry.height : ''}${entry.duration_ms ? ' · ' + (entry.duration_ms / 1000).toFixed(1) + ' s' : ''}`;
            },
            // Check whether the media entry satisfies the active picker requirements
            compatible(entry: MediaLibraryEntry) {
                return (
                    !entry.error &&
                    (!this.request ||
                        this.request.formats.includes(
                            entry.format === 'jpg' ? 'jpeg' : entry.format,
                        ))
                );
            },
            // Check that the media picker request still belongs to its original editor
            get requestCurrent() {
                return !this.opened || !this.request || this.request.current();
            },
            // Attach media library listeners and register owned draft resources
            init() {
                thumbnails = viewportThumbnails({
                    // Check whether this media operation is permitted by the current dialog request
                    allowed: () =>
                        this.opened &&
                        !this.busy &&
                        !this.working &&
                        !this.review &&
                        !this.deletion &&
                        !this.optimizationAsset &&
                        !windowWork.quiescing,
                    // Update the dialog’s pending-operation indicator
                    busy: (busy) => {
                        this.thumbnailBusy = this.opened && busy;
                    },
                    // Run the owned operation while tracking pending dialog work
                    run: async ({ entry, epoch }) => {
                        // Check that the asynchronous result still belongs to the active editor
                        const current = () => this.opened && epoch === this.epoch;
                        try {
                            // Fetch a thumbnail under window ownership so shutdown waits for its cleanup
                            await windowWork.run('media library thumbnail', async () => {
                                const image = await app().bridge.command('media_thumbnail', {
                                    asset: entry.id,
                                });
                                if (!image.startsWith('data:image/png;base64,'))
                                    throw Error('Invalid preview response');
                                if (
                                    current() &&
                                    (await decodeMediaImages(
                                        [image],
                                        current,
                                        undefined,
                                        'thumbnail',
                                    ))
                                )
                                    this.thumbs = { ...this.thumbs, [entry.id]: image };
                            });
                        } catch (error) {
                            if (current())
                                this.thumbErrors = {
                                    ...this.thumbErrors,
                                    [entry.id]: String(error),
                                };
                        }
                    },
                });
                for (const field of ['busy', 'working', 'review', 'deletion', 'optimizationAsset'])
                    this.$watch(field, () => thumbnails?.pump());
                ownWindowMethods(this, 'media-library', [
                    'open',
                    'load',
                    'audit',
                    'loadThumbnails',
                    'selectInput',
                    'choose',
                    'reviewDeletion',
                    'confirmDeletion',
                    'reviewCleanup',
                    'deleteConfirmed',
                    'previewOptimization',
                    'saveOptimization',
                ]);
                this.unregisterCloseOwner = registerCloseDraft(
                    this,
                    ['confirmDeletion', 'deleteConfirmed', 'saveOptimization'],
                    {
                        name: 'Media library reviews',
                        // Report whether the controller retains unsaved or active work
                        state: () => 'clean',
                        // Discard the staged media or editor state owned by the current dialog
                        discard: () => {},
                        // Release resources retained by the current controller
                        release: async () => {
                            const deletion = this.deletion;
                            const cleanup = this.review;
                            if (deletion)
                                await trackCloseRelease('media-deletion:' + deletion.id, () =>
                                    app().bridge.command('discard_media_deletion', {
                                        id: deletion.id,
                                    }),
                                );
                            if (cleanup)
                                await trackCloseRelease('media-cleanup:' + cleanup.id, () =>
                                    app().bridge.command('discard_media_cleanup', {
                                        id: cleanup.id,
                                    }),
                                );
                            this.deletion = null;
                            this.review = null;
                            this.close();
                        },
                    },
                );
                this.$watch('$store.app.route', () => this.close());
                // Refresh dependent media library state after the watched value changes
                this.$watch('working', () => {
                    if (!this.working && this.request && !this.request.current()) this.close();
                });
                // Refresh dependent media library state after the watched value changes
                this.$watch('requestCurrent', () => {
                    if (!this.requestCurrent && !this.working) {
                        this.close();
                    }
                });
            },
            // Open the media library with the supplied selection request
            async open(event: CustomEvent<{ request?: MediaPickerRequest }>) {
                if (this.working) return;
                this.close();
                this.request = event.detail?.request || null;
                if (this.request && !this.request.current()) return;
                this.opened = true;
                this.page = 1;
                this.query = '';
                this.error = '';
                this.selected = [];
                (this.$refs.dialog as HTMLDialogElement).showModal();
                await this.load();
            },
            // Close the dialog and release its retained resources
            close() {
                if (this.working) return;
                ++this.epoch;
                pendingListing = null;
                auditNext = false;
                this.auditing = false;
                this.cancelOptimization();
                this.notice = '';
                thumbnails?.stop();
                this.importing = false;
                this.opened = false;
                this.busy = false;
                this.thumbnailBusy = false;
                this.cancelCleanup();
                this.cancelDeletion();
                this.request = null;
                this.data = null;
                this.thumbs = {};
                this.thumbErrors = {};
                this.selected = [];
                (this.$refs.dialog as HTMLDialogElement)?.close();
            },
            // Remove media library listeners and release owned resources
            destroy() {
                this.unregisterCloseOwner?.();
                this.close();
            },
            // A retention audit is explicit and never delays selecting existing media.
            async audit() {
                if (this.busy || this.working || !this.opened) return;
                const epoch = this.epoch;
                this.busy = true;
                this.auditing = true;
                try {
                    await app().flushDrafts?.();
                    if (epoch !== this.epoch || !this.opened) return;
                    auditNext = true;
                    await this.load();
                } catch (error) {
                    if (epoch === this.epoch) {
                        this.error = String(error);
                        this.busy = false;
                        this.auditing = false;
                    }
                }
            },
            // Fetch the current media-library page and its metadata
            async load() {
                if (this.working || !this.opened) return;
                const epoch = ++this.epoch;
                thumbnails?.stop();
                this.cancelCleanup();
                this.cancelDeletion();
                this.busy = true;
                this.error = '';
                this.selected = [];
                this.thumbs = {};
                this.thumbErrors = {};
                const audit = auditNext;
                auditNext = false;
                this.auditing = audit;
                pendingListing = {
                    epoch,
                    audit,
                    page: this.page,
                    pageSize: Number(this.pageSize),
                    query: this.query,
                };
                if (listing) return;
                listing = true;
                try {
                    // One scan at a time. Rapid search/page requests replace the queued
                    // request instead of running redundant native filesystem scans.
                    while (pendingListing) {
                        const request = pendingListing;
                        pendingListing = null;
                        try {
                            const data = await app().bridge.command(
                                request.audit ? 'audit_media_library' : 'media_library',
                                {
                                    page: request.page,
                                    pageSize: request.pageSize,
                                    query: request.query,
                                },
                            );
                            if (request.epoch !== this.epoch || !this.opened) continue;
                            this.data = data;
                            this.page = data.page;
                            void this.loadThumbnails(data.entries, request.epoch);
                        } catch (error) {
                            if (request.epoch === this.epoch) this.error = String(error);
                        } finally {
                            if (request.epoch === this.epoch) {
                                this.busy = false;
                                this.auditing = false;
                            }
                        }
                    }
                } finally {
                    listing = false;
                }
            },
            // Load thumbnail handles only for the current library page
            async loadThumbnails(entries: MediaLibraryEntry[], epoch: number) {
                await this.$nextTick();
                if (epoch !== this.epoch || !this.opened) return;
                const byId = new Map(entries.map((entry) => [entry.id, entry]));
                const rows = Array.from(
                    (this.$refs.dialog as HTMLElement).querySelectorAll<HTMLElement>(
                        '.media-library-row',
                    ),
                );
                thumbnails?.observe(
                    // Resolve visible DOM rows to current media entries and skip missing items
                    rows.flatMap((row) => {
                        const entry = byId.get(row.dataset.asset || '');
                        return entry
                            ? [
                                  [row, { entry, epoch }] as [
                                      Element,
                                      { entry: MediaLibraryEntry; epoch: number },
                                  ],
                              ]
                            : [];
                    }),
                    this.$refs.dialog as HTMLElement,
                );
            },
            // Reset pagination and reload the filtered media entries
            search() {
                this.page = 1;
                void this.load();
            },
            // Load the previous media-library page
            previous() {
                this.page = Math.max(1, this.page - 1);
                void this.load();
            },
            // Load the next media-library page
            next() {
                this.page = Math.min(this.pages, this.page + 1);
                void this.load();
            },
            // Toggle selection of the media entry identified by the event
            toggle(event: Event) {
                const id = (event.currentTarget as HTMLInputElement).dataset.id || '';
                if (
                    !this.retentionChecked ||
                    !this.data?.entries.some((entry) => entry.id === id && entry.can_delete)
                )
                    return;
                this.selected = this.selected.includes(id)
                    ? this.selected.filter((value) => value !== id)
                    : [...this.selected, id];
            },
            // Return the selected media entry to the active picker request
            async choose(event: Event) {
                const request = this.request;
                const id = (event.currentTarget as HTMLElement).dataset.id;
                const entry = this.data?.entries.find((entry) => entry.id === id);
                if (
                    !request ||
                    !entry ||
                    !request.current() ||
                    !this.compatible(entry) ||
                    this.working ||
                    this.busy
                )
                    return;
                const epoch = this.epoch;
                this.working = true;
                this.error = '';
                try {
                    await retainMediaFor(app(), entry.id, request.owner);
                    if (epoch !== this.epoch || !request.current()) return;
                    // Picker entries are reactive proxies; consumers receive detached JSON options
                    await request.choose(entry.id, cloneJson(entry.clip_info?.options));
                    this.working = false;
                    this.close();
                } catch (error) {
                    if (epoch === this.epoch) this.error = String(error);
                } finally {
                    this.working = false;
                }
            },
            // Choose file in the current media library draft
            chooseFile() {
                return this.selectInput();
            },
            // Handle dropped media in the current media library draft
            dropMedia(event: CustomEvent<File[]>) {
                return this.selectInput(event);
            },
            // Select input in the current media library draft
            async selectInput(event?: CustomEvent<File[]>) {
                if (
                    !this.opened ||
                    this.busy ||
                    this.working ||
                    this.importing ||
                    !this.requestCurrent
                )
                    return;
                const epoch = this.epoch;
                const request = this.request;
                const owner = request?.owner || 'library-media:' + crypto.randomUUID();
                // Check that the asynchronous result still belongs to the active editor
                const current = () =>
                    this.opened && this.epoch === epoch && (!request || request.current());
                this.importing = true;
                this.error = '';
                try {
                    const formats = request?.formats || mediaFormats;
                    const input = event ? droppedMedia(event) : await chooseMediaInput(formats);
                    if (!input || !current()) return;
                    await selectMediaInput(app(), input, {
                        owner,
                        formats,
                        current,
                        // Save d in the current media library draft
                        saved: () => {
                            if (current() && !request) {
                                this.importing = false;
                                void this.load();
                            }
                        },
                        apply: request
                            ? // Return the chosen asset to the original picker and close the library
                              async (asset, options) => {
                                  await request.choose(asset, options);
                                  this.close();
                              }
                            : undefined,
                    });
                } catch (error) {
                    if (current()) this.error = String(error);
                } finally {
                    if (current()) this.importing = false;
                    // Report the asynchronous media library failure without applying stale results
                    if (!request) void releaseMediaOwner(app(), owner).catch(() => {});
                }
            },
            // Open optimization options for the selected media entry
            async optimize(event: Event) {
                const asset = (event.currentTarget as HTMLElement).dataset.id;
                const entry = this.data?.entries.find((entry) => entry.id === asset);
                if (!entry || !canOptimizeStill(entry) || this.busy || this.working) return;
                this.optimizationAsset = entry.id;
                this.optimizationEdge = '512';
                (this.$refs.optimization as HTMLDialogElement).showModal();
                await this.previewOptimization();
            },
            // Prepare an optimization preview without replacing the original asset
            async previewOptimization() {
                if (!this.optimizationAsset || this.working) return;
                const revision = ++optimizationRevision;
                this.optimizationBusy = true;
                this.optimization = null;
                this.optimizationError = '';
                try {
                    const preview = await app().bridge.command('preview_media_optimization', {
                        asset: this.optimizationAsset,
                        maxEdge: Number(this.optimizationEdge),
                    });
                    if (revision === optimizationRevision) this.optimization = preview;
                } catch (error) {
                    if (revision === optimizationRevision) this.optimizationError = String(error);
                } finally {
                    if (revision === optimizationRevision) this.optimizationBusy = false;
                }
            },
            // Cancel optimization in the current media library draft
            cancelOptimization() {
                if (this.working) return;
                ++optimizationRevision;
                this.optimizationAsset = '';
                this.optimization = null;
                this.optimizationBusy = false;
                this.optimizationError = '';
                (this.$refs.optimization as HTMLDialogElement)?.close();
            },
            // Save the reviewed optimization as a new media asset
            async saveOptimization() {
                const preview = this.optimization;
                if (!preview || this.optimizationBusy || this.working) return;
                this.working = true;
                this.optimizationError = '';
                try {
                    const asset = await app().bridge.command('create_optimized_media_copy', {
                        asset: preview.source_id,
                        maxEdge: preview.max_edge,
                        token: preview.token,
                    });
                    this.working = false;
                    this.cancelOptimization();
                    this.notice =
                        'Optimized copy saved. Original media and existing assignments are unchanged.';
                    this.query = asset;
                    this.page = 1;
                    await this.load();
                } catch (error) {
                    this.optimizationError = String(error);
                } finally {
                    this.working = false;
                }
            },
            // Edit video in the current media library draft
            editVideo(event: Event) {
                const asset = (event.currentTarget as HTMLElement).dataset.id;
                const entry = this.data?.entries.find((entry) => entry.id === asset);
                if (!entry || this.working || this.busy) return;
                const request = this.request;
                openVideoEditor({
                    asset: entry.id,
                    owner: request?.owner,
                    options: entry.clip_info?.options,
                    // Check that the asynchronous result still belongs to the active editor
                    current: () => this.opened && (!request || request.current()),
                    // Save d in the current media library draft
                    saved: () => {
                        if (this.opened && !request) void this.load();
                    },
                    apply: request
                        ? // Return the chosen asset to the original picker and close the library
                          async (asset, options) => {
                              await request.choose(asset, options);
                              this.close();
                          }
                        : undefined,
                });
            },
            // Prepare a deletion review for the selected media entries
            async reviewDeletion(event: Event) {
                if (this.busy || this.working || !this.data) return;
                const asset = (event.currentTarget as HTMLElement).dataset.id;
                if (!asset || !this.data.entries.some((entry) => entry.id === asset)) return;
                this.deletionFocus = event.currentTarget as HTMLButtonElement;
                const epoch = this.epoch;
                this.busy = true;
                this.deletionError = '';
                this.error = '';
                try {
                    await app().flushDrafts?.();
                    const review = await app().bridge.command('prepare_media_deletion', { asset });
                    if (epoch !== this.epoch) {
                        void trackCloseRelease(
                            'media-deletion:' + review.id,
                            () => app().bridge.command('discard_media_deletion', { id: review.id }),
                            // Report the asynchronous media library failure without applying stale results
                        ).catch(() => {});
                        return;
                    }
                    this.deletion = review;
                    (this.$refs.deletion as HTMLDialogElement).showModal();
                    this.$nextTick(() => (this.$refs.cancelDeletion as HTMLButtonElement).focus());
                } catch (error) {
                    if (epoch === this.epoch) this.error = String(error);
                } finally {
                    if (epoch === this.epoch) this.busy = false;
                }
            },
            // Cancel deletion in the current media library draft
            cancelDeletion() {
                if (this.working) return;
                const focus = this.deletionFocus;
                this.deletionFocus = null;
                if (this.deletion) {
                    const id = this.deletion.id;
                    void trackCloseRelease(
                        'media-deletion:' + id,
                        () => app().bridge.command('discard_media_deletion', { id }),
                        // Report the asynchronous media library failure without applying stale results
                    ).catch(() => {});
                }
                this.deletion = null;
                this.deletionError = '';
                (this.$refs.deletion as HTMLDialogElement)?.close();
                // Update the media library view after reactive DOM changes finish
                this.$nextTick(() => {
                    if (this.opened && focus?.isConnected && !focus.disabled) focus.focus();
                });
            },
            // Apply the explicitly confirmed media deletion review
            async confirmDeletion() {
                if (!this.deletion || this.working || this.deletionError) return;
                this.working = true;
                try {
                    const result = await app().bridge.command('commit_media_deletion', {
                        id: this.deletion.id,
                    });
                    app().forgetMedia(result.removed);
                    app().apply(result.snapshot);
                    await this.$nextTick();
                    this.request?.rebase?.();
                    this.deletion = null;
                    this.working = false;
                    this.cancelDeletion();
                    if (this.request && !this.request.current()) {
                        this.close();
                        return;
                    }
                    app().notify(
                        'Media deleted and unassigned.' +
                            (result.warnings.length ? ' ' + result.warnings.join(' ') : ''),
                        result.warnings.length ? 'warning' : 'success',
                    );
                    await this.load();
                    if (this.opened) (this.$refs.refresh as HTMLButtonElement).focus();
                } catch (error) {
                    this.deletionError = String(error);
                } finally {
                    this.working = false;
                }
            },
            // Prepare a review of unused media before any deletion
            async reviewCleanup() {
                if (!this.canReview || !this.data) return;
                const epoch = this.epoch;
                this.busy = true;
                this.cleanupError = '';
                try {
                    await app().flushDrafts?.();
                    const review = await app().bridge.command('prepare_media_cleanup', {
                        generation: this.data.generation,
                        assets: [...this.selected],
                    });
                    if (epoch !== this.epoch) {
                        void trackCloseRelease(
                            'media-cleanup:' + review.id,
                            () => app().bridge.command('discard_media_cleanup', { id: review.id }),
                            // Report the asynchronous media library failure without applying stale results
                        ).catch(() => {});
                        return;
                    }
                    this.review = review;
                    (this.$refs.cleanup as HTMLDialogElement).showModal();
                    this.$nextTick(() => (this.$refs.cancelCleanup as HTMLButtonElement).focus());
                } catch (error) {
                    if (epoch === this.epoch) this.error = String(error);
                } finally {
                    if (epoch === this.epoch) this.busy = false;
                }
            },
            // Cancel cleanup in the current media library draft
            cancelCleanup() {
                if (this.working) return;
                if (this.review) {
                    const id = this.review.id;
                    void trackCloseRelease(
                        'media-cleanup:' + id,
                        () => app().bridge.command('discard_media_cleanup', { id }),
                        // Report the asynchronous media library failure without applying stale results
                    ).catch(() => {});
                }
                this.review = null;
                this.cleanupError = '';
                (this.$refs.cleanup as HTMLDialogElement)?.close();
            },
            // Delete only the media covered by the confirmed review
            async deleteConfirmed() {
                if (!this.review || this.working || this.cleanupError) return;
                this.working = true;
                try {
                    const result = await app().bridge.command('commit_media_cleanup', {
                        id: this.review.id,
                    });
                    this.review = null;
                    this.working = false;
                    this.cancelCleanup();
                    app().notify(
                        `Removed ${result.removed.length} unused media ${result.removed.length === 1 ? 'asset' : 'assets'}.${result.warnings.length ? ' ' + result.warnings.join(' ') : ''}`,
                        result.warnings.length ? 'warning' : 'success',
                    );
                    await this.load();
                    if (this.opened) (this.$refs.refresh as HTMLButtonElement).focus();
                } catch (error) {
                    this.cleanupError = String(error);
                } finally {
                    this.working = false;
                }
            },
        };
    });
}
