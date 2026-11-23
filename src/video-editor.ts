import { videoDecodeQueue, filmstripCache } from './video-filmstrip';
import { timelinePosition, timelineRange, type TimelineMode } from './video-timeline';
import {
    destinationCrop,
    panDestination,
    zoomDestination,
    containRect,
    normalizeCrop,
    transformCrop,
    type CropHandle,
} from './video-crop';
import { cloneJson } from './json-clone';
import { savedMediaComparison } from './media-guidance';
import { preparedVideoMetadata, type DecoderState } from './video-editor-state';
import * as fileDialogs from './window-dialogs';
import { ownWindowMethods } from './window-work';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type {
    MediaOptions,
    Recording,
    VideoClipEntry,
    VideoTarget,
    RecordingCleanupReview,
    VideoClipDetails,
} from '../generated/runtime';
import { windowInvoke as invoke } from './window-native';
import { assertCurrentMediaOptions, defaultMedia, native } from './bridge';
import { VideoSession, type VideoProgress } from './video-session';
import {
    fpsChoices,
    timecode,
    parseTimecode,
    clipValidation,
    timelineWindow,
} from './video-timing';
import { latestPreview, publishDecodedPreview, releaseMediaPreview } from './media-preview';
import { retainMediaFor, releaseMediaOwner } from './media-leases';

export interface VideoEditorRequest {
    asset?: string;
    path?: string;
    controlId?: string | null;
    options?: MediaOptions;
    owner?: string;
    current?: () => boolean;
    saved?: () => void;
    releaseSource?: () => Promise<void>;
    apply?: (asset: string, options: MediaOptions) => void | Promise<void>;
}
// Open the video editor
export function openVideoEditor(request: VideoEditorRequest = {}) {
    window.dispatchEvent(new CustomEvent('open-video-editor', { detail: request }));
}
// Register the video editor
export function registerVideoEditor(Alpine: typeof AlpineType, app: () => AppState) {
    // Complete the deferred video editor work using the current operation state
    Alpine.data('videoEditor', () => {
        // Backend retired owners outlive route components. Never reuse an owner
        // after remount/reload, even for the same content-addressed recording.
        const instance = crypto.randomUUID();
        let preparationGeneration = 0;
        // Cancel the import
        const cancelImport = (id: string) =>
            trackCloseRelease('video-import:' + id, () =>
                app().bridge.command('cancel_video_import', { id }),
            );
        // Cancel the preparation
        const cancelPreparation = (id: string) =>
            trackCloseRelease('media-preparation:' + id, () =>
                app().bridge.command('cancel_media_preparation', { id }),
            );
        // Finish the preparation
        const finishPreparation = (id: string) =>
            trackCloseRelease('media-preparation-finish:' + id, () =>
                app().bridge.command('finish_media_preparation', { id }),
            );
        // Release the recording
        const releaseRecording = (owner: string) =>
            native
                ? trackCloseRelease('recording-owner:' + owner, () =>
                      app().bridge.command('release_video_recording', { owner }),
                  )
                : Promise.resolve();
        // Track release of a recording review as owned window-close cleanup
        const discardRecordingReview = (id: string) =>
            trackCloseRelease('recording-review:' + id, () =>
                app().bridge.command('discard_recording_review', { id }),
            );
        let request: VideoEditorRequest = {};
        let session: VideoSession | undefined;
        let importJob = '';
        let preparationJob = '';
        let timer: ReturnType<typeof setTimeout> | undefined;
        let playOrigin = 0,
            playPosition = 0;
        let cropObserver: ResizeObserver | undefined;
        let previewInFlight = false;
        let previewRevision = 0;
        let previewPublications = 0;
        let frameRunner: ReturnType<
            typeof latestPreview<number, { url: string; position: number; revision: number }>
        >;
        let thumbnailGeneration = 0;
        const decodeQueue = videoDecodeQueue();
        const thumbnailCache = filmstripCache();
        let thumbnailKeys: string[] = [];
        let rangeGeneration = 0;
        let focusReturn: HTMLElement | null = null;
        let preparedSource:
            { asset: string; start: number; end: number; edge: number; fps: number } | undefined;
        return {
            opened: false,
            closeBaseline: '',
            unregisterCloseOwner: undefined as (() => void) | undefined,
            // Fingerprint the editable clip name, asset, range and encoding options
            editableSignature() {
                return JSON.stringify([
                    this.name,
                    this.baseAsset,
                    this.clipStart,
                    this.clipEnd,
                    this.options,
                ]);
            },
            // Release the for close
            async releaseForClose() {
                if (
                    !this.opened &&
                    !this.recording &&
                    !importJob &&
                    !preparationJob &&
                    !this.cleanup
                )
                    return;
                if (this.cleanupWorking || this.saving)
                    throw Error('Wait for the recording write to finish.');
                const owner = this.owner;
                ++this.generation;
                frameRunner?.invalidate();
                ++thumbnailGeneration;
                decodeQueue.cancelBackground();
                thumbnailCache.clear();
                this.pause();
                session?.close();
                session = undefined;
                const importId = importJob;
                const preparationId = preparationJob;
                if (importId) {
                    await trackCloseRelease('video-import:' + importId, () =>
                        app().bridge.command('cancel_video_import', { id: importId }),
                    );
                    if (importJob === importId) importJob = '';
                }
                if (preparationId) {
                    await trackCloseRelease('media-preparation:' + preparationId, () =>
                        app().bridge.command('cancel_media_preparation', { id: preparationId }),
                    );
                    if (preparationJob === preparationId) preparationJob = '';
                }
                const review = this.cleanup;
                if (review) {
                    await trackCloseRelease('recording-review:' + review.id, () =>
                        app().bridge.command('discard_recording_review', { id: review.id }),
                    );
                    if (this.cleanup === review) this.cleanup = null;
                }
                await releaseRecording(owner);
                await releaseMediaOwner(app(), owner);
                this.close();
            },
            busy: false,
            playing: false,
            saving: false,
            generation: 0,
            deviceId: '',
            controlId: null as string | null,
            target: null as VideoTarget | null,
            recording: null as Recording | null,
            sourceAvailable: true,
            copyComparison: '',
            decoder: { kind: 'idle' } as DecoderState,
            // Derive fallback from the current video editor state
            get fallback() {
                return this.decoder.kind === 'ready' && this.decoder.backend === 'ffmpeg';
            },
            // Derive decoder ready from the current video editor state
            get decoderReady() {
                return this.decoder.kind === 'ready';
            },
            preparedLabel: '',
            availableStart: 0,
            availableEnd: 0,
            preparedTimes: [] as number[],
            cleanup: null as RecordingCleanupReview | null,
            cleanupWorking: false,
            name: 'Clip 1',
            asset: '',
            preparedAsset: '',
            baseAsset: '',
            options: defaultMedia(),
            position: 0,
            clipStart: 0,
            clipEnd: 0,
            windowStart: 0,
            windowEnd: 0,
            view: 'device',
            image: '',
            previewView: '',
            error: '',
            notice: '',
            details: '',
            progress: null as VideoProgress | null,
            clips: [] as VideoClipEntry[],
            thumbnails: [] as { position: number; url: string }[],
            timelinePointer: null as {
                id: number;
                target: HTMLElement;
                mode: TimelineMode;
                x: number;
                left: number;
                width: number;
                windowStart: number;
                windowEnd: number;
                start: number;
                end: number;
                position: number;
            } | null,
            selectedClip: '',
            cropBounds: { x: 0, y: 0, width: 1, height: 1 },
            pointerStart: null as {
                x: number;
                y: number;
                crop: [number, number, number, number];
                width: number;
                height: number;
                handle: CropHandle;
                pointerId: number;
                device?: boolean;
                moved?: boolean;
                original?: [number, number, number, number];
            } | null,
            // Derive owner from the current video editor state
            get owner() {
                return `video-editor:${instance}:${this.generation}`;
            },
            // Derive is gif from the current video editor state
            get isGif() {
                return this.recording?.source_format === 'gif';
            },
            // Derive duration from the current video editor state
            get duration() {
                return this.recording?.duration_us || 0;
            },
            // Derive source fps from the current video editor state
            get sourceFps() {
                return this.recording?.source_fps || 0;
            },
            // Derive rates from the current video editor state
            get rates() {
                return fpsChoices(this.sourceFps, this.options.max_fps);
            },
            // Derive maximum from the current video editor state
            get maximum() {
                return String(this.options.max_fps ?? 'source');
            },
            // Derive minimum from the current video editor state
            get minimum() {
                return this.options.min_fps ?? Math.min(5, this.sourceFps || 5);
            },
            // Derive min edge from the current video editor state
            get minEdge() {
                return this.options.min_frame_edge ?? 64;
            },
            // Derive max edge from the current video editor state
            get maxEdge() {
                return this.options.max_frame_edge ?? 512;
            },
            // Derive quality from the current video editor state
            get quality() {
                return this.options.jpeg_quality ?? 75;
            },
            // Derive in time from the current video editor state
            get inTime() {
                return timecode(this.clipStart);
            },
            // Derive out time from the current video editor state
            get outTime() {
                return timecode(this.clipEnd);
            },
            // Derive current time from the current video editor state
            get currentTime() {
                return timecode(this.position);
            },
            // Derive duration label from the current video editor state
            get durationLabel() {
                return `${((this.clipEnd - this.clipStart) / 1_000_000).toFixed(3)} s selected`;
            },
            // Derive source label from the current video editor state
            get sourceLabel() {
                return this.recording
                    ? `${this.recording.width} × ${this.recording.height} · ${this.sourceFps.toFixed(3).replace(/\.?0+$/, '')} FPS${this.recording.variable_rate ? ' variable' : ''} · ${timecode(this.duration)}`
                    : 'Choose an MP4 recording or an existing clip';
            },
            // Derive target label from the current video editor state
            get targetLabel() {
                return this.target
                    ? `${this.target.name} · ${this.target.width} × ${this.target.height}`
                    : '';
            },
            // Derive preview style from the current video editor state
            get previewStyle() {
                const width =
                    this.view === 'device' && this.target
                        ? this.target.width
                        : this.recording?.width || 16;
                const height =
                    this.view === 'device' && this.target
                        ? this.target.height
                        : this.recording?.height || 9;
                return {
                    aspectRatio: `${width}/${height}`,
                    maxWidth: `${(240 * width) / height}px`,
                };
            },
            // Keep the crop overlay on the rendered image, excluding letterbox padding.
            measureCrop() {
                const frame = this.$refs.sourceFrame as HTMLElement;
                const img = frame?.querySelector('img');
                if (!frame || !img || !img.naturalWidth) return;
                const bounds = frame.getBoundingClientRect();
                this.cropBounds = containRect(
                    bounds.width,
                    bounds.height,
                    img.naturalWidth,
                    img.naturalHeight,
                );
            },
            get cropStyle() {
                const [x, y, width, height] = this.options.crop;
                const bounds = this.cropBounds;
                return {
                    left: `${bounds.x + x * bounds.width}px`,
                    top: `${bounds.y + y * bounds.height}px`,
                    width: `${width * bounds.width}px`,
                    height: `${height * bounds.height}px`,
                };
            },
            deviceCrop() {
                return destinationCrop(
                    this.options.crop,
                    this.recording!.width,
                    this.recording!.height,
                    this.target!.width,
                    this.target!.height,
                    this.options.fit,
                );
            },
            startDevicePan(event: PointerEvent) {
                if (
                    this.busy ||
                    this.view !== 'device' ||
                    this.previewView !== 'device' ||
                    !this.target ||
                    !this.recording ||
                    event.button !== 0 ||
                    this.pointerStart
                )
                    return;
                this.measureCrop();
                const box = (this.$refs.sourceFrame as HTMLElement).getBoundingClientRect();
                const bounds = this.cropBounds;
                const px = event.clientX - box.left - bounds.x,
                    py = event.clientY - box.top - bounds.y;
                const crop = this.deviceCrop();
                const content = containRect(
                    bounds.width,
                    bounds.height,
                    crop[2] * this.recording.width,
                    crop[3] * this.recording.height,
                );
                if (
                    px < 0 ||
                    py < 0 ||
                    px > bounds.width ||
                    py > bounds.height ||
                    (this.options.fit === 'fit' &&
                        (px < content.x ||
                            py < content.y ||
                            px > content.x + content.width ||
                            py > content.y + content.height))
                )
                    return;
                event.preventDefault();
                const target = event.currentTarget as HTMLElement;
                target.focus();
                target.setPointerCapture(event.pointerId);
                this.pause();
                this.pointerStart = {
                    x: event.clientX,
                    y: event.clientY,
                    crop,
                    original: [...this.options.crop],
                    width: bounds.width,
                    height: bounds.height,
                    handle: 'move',
                    pointerId: event.pointerId,
                    device: true,
                };
            },
            zoomDevice(event: Event) {
                if (this.busy || !this.target || !this.recording) return;
                this.cancelCrop();
                this.options.crop = zoomDestination(
                    this.deviceCrop(),
                    Number((event.currentTarget as HTMLElement).dataset.zoom),
                );
                this.changed();
            },
            devicePanKey(event: KeyboardEvent) {
                if (
                    this.busy ||
                    this.view !== 'device' ||
                    this.previewView !== 'device' ||
                    !this.target ||
                    !this.recording
                )
                    return;
                if (event.key === 'Escape' && this.pointerStart) {
                    event.preventDefault();
                    event.stopPropagation();
                    this.cancelCrop();
                    return;
                }
                const direction: Record<string, number[]> = {
                    ArrowLeft: [-1, 0],
                    ArrowRight: [1, 0],
                    ArrowUp: [0, -1],
                    ArrowDown: [0, 1],
                };
                const delta = direction[event.key];
                if (!delta) return;
                event.preventDefault();
                event.stopPropagation();
                this.measureCrop();
                const step = event.shiftKey ? 25 : 5;
                this.options.crop = panDestination(
                    this.deviceCrop(),
                    delta[0] * step,
                    delta[1] * step,
                    this.recording.width,
                    this.recording.height,
                    this.cropBounds.width,
                    this.cropBounds.height,
                    this.options.fit,
                );
                this.changed();
            },
            startCrop(event: PointerEvent) {
                if (this.busy || this.view !== 'source' || event.button !== 0 || this.pointerStart)
                    return;
                event.preventDefault();
                this.measureCrop();
                const target = event.target as HTMLElement;
                (
                    target.closest<HTMLElement>('[data-crop-handle]') ||
                    (event.currentTarget as HTMLElement)
                ).focus();
                (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
                this.pointerStart = {
                    x: event.clientX,
                    y: event.clientY,
                    crop: [...this.options.crop],
                    width: this.cropBounds.width,
                    height: this.cropBounds.height,
                    handle: (target.closest<HTMLElement>('[data-crop-handle]')?.dataset
                        .cropHandle || 'move') as CropHandle,
                    pointerId: event.pointerId,
                };
            },
            moveCrop(event: PointerEvent) {
                const start = this.pointerStart;
                if (!start || !this.opened || start.pointerId !== event.pointerId) return;
                if (start.device) {
                    const distance =
                        Math.abs(event.clientX - start.x) + Math.abs(event.clientY - start.y);
                    if (!start.moved && distance < 2) return;
                    start.moved = true;
                    if (distance < 2) {
                        this.options.crop = [...start.original!];
                        this.changed();
                        return;
                    }
                    this.options.crop = panDestination(
                        start.crop,
                        event.clientX - start.x,
                        event.clientY - start.y,
                        this.recording!.width,
                        this.recording!.height,
                        start.width,
                        start.height,
                        this.options.fit,
                    );
                    this.changed();
                    return;
                }
                this.options.crop = transformCrop(
                    start.crop,
                    (event.clientX - start.x) / Math.max(1, start.width),
                    (event.clientY - start.y) / Math.max(1, start.height),
                    start.handle,
                );
                this.preparedAsset = '';
            },
            endCrop(event?: PointerEvent) {
                if (
                    this.pointerStart &&
                    (!event || event.pointerId === this.pointerStart.pointerId)
                ) {
                    const changed = !this.pointerStart.device || this.pointerStart.moved;
                    this.pointerStart = null;
                    if (changed) this.changed();
                }
            },
            cancelCrop(event?: PointerEvent) {
                if (
                    !this.pointerStart ||
                    (event && event.pointerId !== this.pointerStart.pointerId)
                )
                    return;
                this.options.crop = [...(this.pointerStart.original || this.pointerStart.crop)];
                this.pointerStart = null;
                this.changed();
            },
            cropKey(event: KeyboardEvent) {
                if (this.busy || this.view !== 'source') return;
                if (event.key === 'Escape' && this.pointerStart) {
                    event.preventDefault();
                    event.stopPropagation();
                    this.cancelCrop();
                    return;
                }
                const directions: Record<string, number[]> = {
                    ArrowLeft: [-1, 0],
                    ArrowRight: [1, 0],
                    ArrowUp: [0, -1],
                    ArrowDown: [0, 1],
                };
                const direction = directions[event.key];
                if (!direction) return;
                event.preventDefault();
                event.stopPropagation();
                const handle = ((event.target as HTMLElement).dataset.cropHandle ||
                    'move') as CropHandle;
                const step = event.shiftKey ? 0.05 : 0.01;
                this.options.crop = transformCrop(
                    this.options.crop,
                    direction[0] * step,
                    direction[1] * step,
                    handle,
                );
                this.changed();
            },
            // Derive selection style from the current video editor state
            get selectionStyle() {
                const span = Math.max(1, this.windowEnd - this.windowStart);
                const left = Math.max(0, ((this.clipStart - this.windowStart) / span) * 100);
                const right = Math.min(100, ((this.clipEnd - this.windowStart) / span) * 100);
                return { left: `${left}%`, width: `${Math.max(0, right - left)}%` };
            },
            // Derive validation from the current video editor state
            get validation() {
                if (this.busy) return '';
                if (
                    !this.sourceAvailable &&
                    this.recording &&
                    (this.clipStart < this.availableStart || this.clipEnd > this.availableEnd)
                )
                    return 'Import the original recording to select outside the prepared clip.';
                if (
                    this.recording &&
                    this.clipEnd - this.clipStart < Math.floor(1_000_000 / this.sourceFps)
                )
                    return 'Select at least one complete source frame.';
                return this.recording
                    ? clipValidation(
                          this.clipStart,
                          this.clipEnd,
                          this.duration,
                          this.minimum,
                          this.options.max_fps ?? this.sourceFps * this.options.speed,
                          this.minEdge,
                          this.maxEdge,
                      )
                    : '';
            },
            // Derive can save from the current video editor state
            get canSave() {
                return (
                    this.opened &&
                    this.recording &&
                    !this.busy &&
                    !this.validation &&
                    Boolean(this.name.trim()) &&
                    !this.preparedAsset &&
                    (this.decoderReady || !this.sourceAvailable)
                );
            },
            // Derive can apply from the current video editor state
            get canApply() {
                return !this.busy && Boolean(this.preparedAsset) && Boolean(request.apply);
            },
            // Initialize video editor state and its required lifecycle hooks
            init() {
                this.$nextTick(() => {
                    cropObserver = new ResizeObserver(() => this.measureCrop());
                    if (this.$refs.sourceFrame)
                        cropObserver.observe(this.$refs.sourceFrame as HTMLElement);
                });
                ownWindowMethods(this, 'recording editor', [
                    'open',
                    'choose',
                    'importRecording',
                    'awaitImport',
                    'loadAsset',
                    'openRecording',
                    'retryDecoder',
                    'step',
                    'setIn',
                    'setOut',
                    'snapRange',
                    'endTimeline',
                    'rangeIn',
                    'rangeOut',
                    'changeTime',
                    'loadThumbnails',
                    'newClip',
                    'selectClip',
                    'saveClip',
                    'applyClip',
                    'reviewRecordings',
                    'cleanupRecordings',
                    'cancelWork',
                ]);
                this.unregisterCloseOwner = registerCloseDraft(
                    this,
                    ['saveClip', 'applyClip', 'reviewRecordings', 'cleanupRecordings'],
                    {
                        name: 'Recording editor',
                        // Return a snapshot of the current video editor state
                        state: () =>
                            this.opened && this.closeBaseline !== this.editableSignature()
                                ? 'dirty'
                                : 'clean',
                        // Discard pending video editor edits without saving them
                        discard: () => this.releaseForClose(),
                        // Release resources retained by the video editor operation
                        release: () => this.releaseForClose(),
                    },
                );
                frameRunner = latestPreview(
                    // Complete the deferred video editor work using the current operation state
                    async (position) => {
                        const generation = this.generation;
                        const revision = previewRevision;
                        return decodeQueue.run(false, async () => {
                            if (
                                !this.opened ||
                                generation !== this.generation ||
                                revision !== previewRevision
                            )
                                throw Error('Superseded video preview');
                            previewInFlight = true;
                            try {
                                let bytes: ArrayBuffer;
                                const prepared =
                                    !session &&
                                    preparedSource?.asset === this.asset &&
                                    position >= preparedSource.start &&
                                    position < preparedSource.end &&
                                    (!this.sourceAvailable ||
                                        (Math.min(
                                            this.maxEdge,
                                            Math.max(this.recording!.width, this.recording!.height),
                                        ) <= preparedSource.edge &&
                                            Math.min(
                                                this.options.max_fps ?? Infinity,
                                                this.sourceFps,
                                            ) <=
                                                preparedSource.fps + 0.01))
                                        ? preparedSource
                                        : undefined;
                                if (prepared)
                                    bytes = await app().bridge.command('prepared_video_frame', {
                                        asset: prepared.asset,
                                        positionUs: Math.max(
                                            0,
                                            Math.min(
                                                prepared.end - prepared.start - 1,
                                                Math.round(position - prepared.start),
                                            ),
                                        ),
                                        deviceId: this.view === 'device' ? this.deviceId : null,
                                        controlId: this.controlId,
                                        options: structuredClone(Alpine.raw(this.options)),
                                    });
                                else if (session)
                                    bytes = (
                                        await session.frame(position, this.maxEdge, {
                                            continuous: this.playing,
                                            end: this.clipEnd,
                                            target:
                                                this.view === 'device' && this.target
                                                    ? {
                                                          width: this.target.width,
                                                          height: this.target.height,
                                                          options: structuredClone(
                                                              Alpine.raw(this.options),
                                                          ),
                                                      }
                                                    : undefined,
                                        })
                                    ).bytes;
                                else
                                    bytes = await app().bridge.command('video_recording_frame', {
                                        recording: this.recording!.id,
                                        positionUs: Math.min(
                                            this.duration - 1,
                                            Math.round(position),
                                        ),
                                        edge: this.maxEdge,
                                        deviceId:
                                            this.isGif && this.view === 'device'
                                                ? this.deviceId
                                                : null,
                                        controlId: this.controlId,
                                        options: structuredClone(Alpine.raw(this.options)),
                                    });
                                if (this.view === 'device' && !session && !prepared && !this.isGif)
                                    bytes = await invoke('video_preview_frame', bytes, {
                                        headers: {
                                            'x-video-device': this.deviceId,
                                            'x-video-control': this.controlId || '',
                                            'x-video-options': JSON.stringify(this.options),
                                        },
                                    });
                                const frame = new Uint8Array(bytes);
                                const mime = frame[0] === 0xff ? 'image/jpeg' : 'image/png';
                                return {
                                    url: URL.createObjectURL(new Blob([frame], { type: mime })),
                                    position,
                                    revision,
                                };
                            } finally {
                                previewInFlight = false;
                            }
                        });
                    },
                    // Complete the deferred video editor work using the current operation state
                    (result) => {
                        const generation = this.generation;
                        ++previewPublications;
                        void publishDecodedPreview(
                            [result.url],
                            () =>
                                this.opened &&
                                generation === this.generation &&
                                result.revision === previewRevision,
                            // Complete the deferred video editor work using the current operation state
                            () => {
                                const previous = this.image;
                                this.image = result.url;
                                this.previewView = this.view;
                                releaseMediaPreview(previous);
                            },
                        )
                            // Process the settled video editor operation result
                            .then((published) => {
                                if (!published) releaseMediaPreview(result.url);
                            })
                            // Handle a failed video editor operation without retaining stale work
                            .catch((error) => {
                                releaseMediaPreview(result.url);
                                if (
                                    this.opened &&
                                    generation === this.generation &&
                                    result.revision === previewRevision
                                )
                                    this.error = String(error);
                            })
                            .finally(() => {
                                --previewPublications;
                            });
                    },
                    (error) => {
                        if (this.opened && String(error) !== 'Error: Superseded video preview')
                            this.error = String(error);
                    },
                    (result) => releaseMediaPreview(result.url),
                );
                // Complete the deferred video editor work using the current operation state
                this.$watch('$store.app.deviceId', () => {
                    if (this.opened && this.deviceId !== app().deviceId) this.close();
                });
                this.$watch('$store.app.route', () => this.close());
                this.$watch('requestCurrent', () => {
                    if (!this.requestCurrent) this.close();
                });
            },
            // Derive request current from the current video editor state
            get requestCurrent() {
                return !this.opened || !request.current || request.current();
            },
            // Open the current video editor target
            async open(event: CustomEvent<VideoEditorRequest>) {
                if (this.saving || this.cleanupWorking) {
                    void event.detail?.releaseSource?.().catch(() => {});
                    return;
                }
                try {
                    assertCurrentMediaOptions(event.detail?.options);
                } catch (error) {
                    app().notify(String(error), 'error');
                    void event.detail?.releaseSource?.().catch(() => {});
                    return;
                }
                this.close();
                request = event.detail || {};
                this.opened = true;
                this.deviceId = app().deviceId;
                this.controlId = request.controlId || null;
                this.recording = null;
                this.target = null;
                this.clips = [];
                this.selectedClip = '';
                this.asset = request.asset || '';
                this.baseAsset = this.asset;
                this.preparedAsset = '';
                preparedSource = undefined;
                this.options = {
                    ...defaultMedia(),
                    ...(request.asset
                        ? cloneJson(request.options || {})
                        : {
                              min_fps: 5,
                              jpeg_quality: 75,
                              min_frame_edge: 64,
                              max_frame_edge: 512,
                              letterbox: /\.gif$/i.test(request.path || '')
                                  ? 'transparent'
                                  : 'black',
                          }),
                };
                this.name = 'Clip 1';
                this.error = '';
                this.notice = '';
                this.copyComparison = '';
                this.decoder = { kind: 'idle' };
                this.preparedLabel = '';
                this.sourceAvailable = true;
                focusReturn =
                    document.activeElement instanceof HTMLElement ? document.activeElement : null;
                (this.$refs.dialog as HTMLDialogElement).showModal();
                const generation = this.generation;
                const releaseSource = request.releaseSource;
                try {
                    if (this.deviceId)
                        this.target = await app().bridge.command('video_target', {
                            deviceId: this.deviceId,
                            controlId: this.controlId,
                        });
                    if (generation !== this.generation) return;
                    if (request.path) await this.importRecording(request.path);
                    else if (this.asset) {
                        await retainMediaFor(app(), this.asset, this.owner);
                        await this.loadAsset(this.asset);
                    } else this.$nextTick(() => (this.$refs.choose as HTMLButtonElement).focus());
                } catch (error) {
                    if (generation === this.generation) this.error = String(error);
                } finally {
                    await releaseSource?.().catch(() => {});
                }
                if (generation === this.generation) this.closeBaseline = this.editableSignature();
            },
            // Release video editor state and associated lifecycle resources
            close() {
                if (this.cleanupWorking) return;
                this.cancelTimeline();
                this.pointerStart = null;
                this.discardCleanup();
                void releaseRecording(this.owner).catch(() => {});
                this.pause();
                frameRunner?.invalidate();
                ++thumbnailGeneration;
                if (importJob) void cancelImport(importJob).catch(() => {});
                if (preparationJob) void cancelPreparation(preparationJob).catch(() => {});
                importJob = '';
                preparationJob = '';
                session?.close();
                session = undefined;
                void releaseMediaOwner(app(), this.owner).catch(() => {});
                this.generation++;
                this.opened = false;
                this.decoder = { kind: 'idle' };
                this.busy = false;
                this.saving = false;
                this.progress = null;
                releaseMediaPreview(this.image);
                this.image = '';
                this.previewView = '';
                this.clearThumbnails();
                (this.$refs.dialog as HTMLDialogElement)?.close();
                focusReturn?.focus();
            },
            // Release video editor state and associated lifecycle resources
            destroy() {
                cropObserver?.disconnect();
                this.unregisterCloseOwner?.();
                this.close();
            },
            // Choose the active item for the video editor workflow
            async choose() {
                if (this.busy) return;
                try {
                    if (!native)
                        throw Error('Choosing recordings requires the desktop application.');
                    const { open } = fileDialogs;
                    const path = await open({
                        multiple: false,
                        filters: [{ name: 'MP4 recording or GIF', extensions: ['mp4', 'gif'] }],
                    });
                    if (path && this.opened) {
                        await this.importRecording(path);
                    }
                } catch (error) {
                    this.error = String(error);
                }
            },
            // Import the recording
            async importRecording(path: string) {
                const previous = {
                    recording: this.recording,
                    start: this.clipStart,
                    end: this.clipEnd,
                    name: this.name,
                    base: this.baseAsset,
                    asset: this.asset,
                    prepared: this.preparedAsset,
                };
                const generation = this.generation;
                this.busy = true;
                this.error = '';
                this.pause();
                this.progress = { phase: 'Importing recording', completed: 0, total: null };
                try {
                    const jobId = await app().bridge.command('start_video_import', {
                        path,
                        owner: this.owner,
                        generation,
                    });
                    if (generation !== this.generation || !this.opened) {
                        await cancelImport(jobId);
                        return;
                    }
                    importJob = jobId;
                    const recording = await this.awaitImport(jobId, generation);
                    if (generation !== this.generation) return;
                    await this.openRecording(recording);
                    if (generation !== this.generation || !this.opened) return;
                    if (previous.recording?.sha256 === recording.sha256) {
                        this.clipStart = previous.start;
                        this.clipEnd = previous.end;
                        this.name = previous.name;
                        this.asset = previous.asset;
                        this.baseAsset = previous.base;
                        this.preparedAsset = previous.prepared;
                        this.position = this.clipStart;
                        this.fitSelection();
                        this.seek();
                        this.notice =
                            'Original recording linked. You can expand or rebuild this clip.';
                    } else {
                        this.asset = '';
                        preparedSource = undefined;
                        this.baseAsset = '';
                        this.preparedAsset = '';
                        this.preparedLabel = '';
                    }
                } finally {
                    if (generation === this.generation) {
                        this.busy = false;
                        this.progress = null;
                    }
                }
            },
            // Poll video import status only while the same editor generation remains open
            async awaitImport(id: string, generation: number): Promise<Recording> {
                for (;;) {
                    if (!this.opened || generation !== this.generation)
                        throw Error('Video editor closed');
                    const job = await app().bridge.command('video_import_status', { id });
                    if (!this.opened || generation !== this.generation)
                        throw Error('Video editor closed');
                    this.progress = {
                        phase: job.phase,
                        completed: job.completed,
                        total: job.total,
                    };
                    if (job.state === 'complete' && job.recording) {
                        if (importJob === id) importJob = '';
                        return job.recording;
                    }
                    if (job.state === 'failed' || job.state === 'cancelled')
                        throw Error(job.error || 'Recording import cancelled');
                    await new Promise((resolve) => setTimeout(resolve, 100));
                }
            },
            // Load asset for the video editor workflow
            async loadAsset(asset: string) {
                const generation = this.generation;
                this.busy = true;
                this.progress = { phase: 'Opening clip', completed: 0, total: null };
                this.error = '';
                try {
                    const details = await app().bridge.command('video_clip_details', { asset });
                    if (generation !== this.generation) return;
                    this.rememberPreparation(asset, details);
                    const jobId = await app().bridge.command('video_recording_for_asset', {
                        asset,
                        owner: this.owner,
                        generation,
                    });
                    if (generation !== this.generation || !this.opened) {
                        await cancelImport(jobId);
                        return;
                    }
                    importJob = jobId;
                    const recording = await this.awaitImport(jobId, generation);
                    await this.openRecording(recording);
                    if (generation !== this.generation) return;
                    if (details.info) {
                        this.selectedClip = asset;
                        if (!request.options) this.options = structuredClone(details.info.options);
                        this.name = details.info.name;
                        this.clipStart = details.info.start_us;
                        this.clipEnd = details.info.end_us;
                    } else {
                        this.clipStart = (request.options?.trim_start_ms || 0) * 1000;
                        this.clipEnd = Math.min(
                            this.duration,
                            (request.options?.trim_end_ms ?? details.duration_us / 1000) * 1000,
                        );
                    }
                    this.asset = asset;
                    this.baseAsset = asset;
                    this.preparedAsset = asset;
                    this.position = this.clipStart;
                    this.fitSelection();
                    this.seek();
                } catch (error) {
                    if (generation !== this.generation || !this.opened) return;
                    const prepared = await app()
                        .bridge.command('video_clip_details', { asset })
                        .catch(() => null);
                    if (generation !== this.generation) return;
                    if (prepared?.info) {
                        const metadata = this.rememberPreparation(asset, prepared);
                        session?.close();
                        session = undefined;
                        this.clearThumbnails();
                        this.decoder = { kind: 'idle' };
                        this.sourceAvailable = false;
                        this.selectedClip = asset;
                        if (!request.options) this.options = structuredClone(prepared.info.options);
                        this.error = '';
                        this.details = String(error);
                        this.recording = prepared.info.recording;
                        this.name = prepared.info.name;
                        this.clipStart = prepared.info.start_us;
                        this.clipEnd = prepared.info.end_us;
                        this.availableStart = this.clipStart;
                        this.availableEnd = this.clipEnd;
                        this.preparedTimes = metadata.times;
                        this.asset = asset;
                        this.baseAsset = asset;
                        this.preparedAsset = asset;
                        this.position = this.clipStart;
                        this.windowStart = this.clipStart;
                        this.windowEnd = this.clipEnd;
                        this.notice =
                            'Original recording is not available. You can trim this prepared clip; import the original to expand its range, detail or FPS.';
                        const clips = await app().bridge.command('video_clips', {
                            recording: null,
                        });
                        if (generation !== this.generation) return;
                        this.clips = clips.filter(
                            (entry) =>
                                entry.info.recording.sha256 === prepared.info!.recording.sha256,
                        );
                        this.seek();
                        void this.loadThumbnails();
                    } else {
                        this.decoder = { kind: 'idle' };
                        this.sourceAvailable = false;
                        this.error = String(error);
                    }
                } finally {
                    if (generation === this.generation) {
                        this.busy = false;
                        this.progress = null;
                    }
                }
            },
            // Open the recording
            async openRecording(recording: Recording) {
                const generation = this.generation;
                await app().bridge.command('retain_video_recording', {
                    recording: recording.id,
                    owner: this.owner,
                });
                if (!this.opened || generation !== this.generation) return;
                this.pause();
                session?.close();
                session = undefined;
                this.clearThumbnails();
                this.recording = recording;
                this.sourceAvailable = true;
                this.progress = { phase: 'Indexing recording', completed: 0, total: null };
                this.decoder = { kind: 'opening' };
                let backend: 'gif' | 'webview' | 'ffmpeg' = this.isGif ? 'gif' : 'webview';
                try {
                    if (this.isGif) {
                        this.progress = { phase: 'Reading GIF frames', completed: 0, total: null };
                        const probed = await app().bridge.command('probe_video_recording', {
                            recording: recording.id,
                        });
                        if (generation !== this.generation || !this.opened) return;
                        this.recording = probed;
                        this.notice =
                            'GIF frame timing and transparency are preserved. Frames are prepared locally.';
                    } else {
                        // Complete the deferred video editor work using the current operation state
                        const activeSession = new VideoSession(app(), recording, (progress) => {
                            if (generation === this.generation) this.progress = progress;
                        });
                        session = activeSession;
                        try {
                            const metadata = await activeSession.open();
                            if (generation !== this.generation) return;
                            const updated = await app().bridge.command('save_video_metadata', {
                                recording: { ...recording, ...metadata },
                            });
                            if (generation !== this.generation || !this.opened) return;
                            this.recording = updated;
                        } catch (error) {
                            activeSession.close();
                            if (generation !== this.generation || !this.opened) return;
                            if (session === activeSession) session = undefined;
                            this.progress = {
                                phase: 'Checking FFmpeg fallback',
                                completed: 0,
                                total: null,
                            };
                            const probed = await app().bridge.command('probe_video_recording', {
                                recording: recording.id,
                            });
                            if (generation !== this.generation || !this.opened) return;
                            this.recording = probed;
                            backend = 'ffmpeg';
                            this.notice =
                                'Using installed FFmpeg because this webview cannot decode the recording.';
                            this.details = String(error);
                        }
                    }
                    if (generation !== this.generation) return;
                    this.decoder = { kind: 'ready', backend };
                } catch (error) {
                    if (generation === this.generation && this.opened)
                        this.decoder = { kind: 'failed' };
                    throw error;
                }
                this.availableStart = 0;
                this.availableEnd = this.duration;
                this.clipStart = 0;
                this.clipEnd = await this.snapRange(Math.min(30_000_000, this.duration));
                if (generation !== this.generation || !this.opened) return;
                this.position = 0;
                this.options.min_fps = Math.min(this.minimum, this.sourceFps);
                const clips = await app().bridge.command('video_clips', {
                    recording: recording.id,
                });
                if (generation !== this.generation || !this.opened) return;
                this.clips = clips;
                this.windowStart = 0;
                this.windowEnd = Math.min(this.duration, 40_000_000);
                this.name = `Clip ${this.clips.length + 1}`;
                this.progress = null;
                this.seek();
                void this.loadThumbnails();
            },
            // Retry the decoder
            async retryDecoder() {
                if (!this.recording || this.busy || !this.sourceAvailable) return;
                const generation = this.generation;
                const [start, end, position, name, prepared] = [
                    this.clipStart,
                    this.clipEnd,
                    this.position,
                    this.name,
                    this.preparedAsset,
                ] as const;
                this.busy = true;
                this.error = '';
                try {
                    await this.openRecording(this.recording);
                    if (generation !== this.generation) return;
                    this.clipStart = start;
                    this.clipEnd = end;
                    this.position = position;
                    this.name = name;
                    this.preparedAsset = prepared;
                    this.fitSelection();
                    this.seek();
                } catch (error) {
                    if (generation === this.generation) this.error = String(error);
                } finally {
                    if (generation === this.generation) {
                        this.busy = false;
                        this.progress = null;
                    }
                }
            },
            // React to changed video editor state
            changed() {
                ++rangeGeneration;
                this.preparedAsset = '';
                this.error = '';
                this.notice = this.fallback ? 'Using installed FFmpeg for this recording.' : '';
                this.pause();
                this.seek();
            },
            // Retain the prepared source metadata and its display label
            rememberPreparation(asset: string, details: VideoClipDetails, start?: number) {
                const metadata = preparedVideoMetadata(asset, details, start);
                preparedSource = metadata.source;
                this.preparedLabel = metadata.label;
                return metadata;
            },
            // Queue a preview frame only when a recording is open and publication allows it
            seek(continuous = false) {
                if (
                    this.recording &&
                    this.opened &&
                    (!continuous || (!previewInFlight && !previewPublications))
                ) {
                    ++previewRevision;
                    frameRunner.request(this.position);
                }
            },
            timelinePercent(value: number) {
                return `${Math.max(0, Math.min(100, (100 * (value - this.windowStart)) / Math.max(1, this.windowEnd - this.windowStart)))}%`;
            },
            get timelineInStyle() {
                return { left: this.timelinePercent(this.clipStart) };
            },
            get timelineOutStyle() {
                return { left: this.timelinePercent(this.clipEnd) };
            },
            get timelinePlayheadStyle() {
                return { left: this.timelinePercent(this.position) };
            },
            get timelineBeforeStyle() {
                return { width: this.timelinePercent(this.clipStart) };
            },
            get timelineAfterStyle() {
                return { left: this.timelinePercent(this.clipEnd) };
            },
            startTimeline(event: PointerEvent) {
                if (this.busy || !this.recording || event.button !== 0) return;
                event.preventDefault();
                event.stopPropagation();
                this.cancelTimeline();
                this.pause();
                ++rangeGeneration;
                const target = event.currentTarget as HTMLElement;
                const track = target.closest('.video-editor-timeline')!;
                const rect = track.getBoundingClientRect();
                this.timelinePointer = {
                    id: event.pointerId,
                    target,
                    mode: (target.dataset.timeline || 'playhead') as TimelineMode,
                    x: event.clientX,
                    left: rect.left,
                    width: rect.width,
                    windowStart: this.windowStart,
                    windowEnd: this.windowEnd,
                    start: this.clipStart,
                    end: this.clipEnd,
                    position: this.position,
                };
                target.focus();
                target.setPointerCapture(event.pointerId);
                this.moveTimeline(event);
            },
            moveTimeline(event: PointerEvent) {
                const drag = this.timelinePointer;
                if (!drag || drag.id !== event.pointerId) return;
                if (drag.mode === 'window' && Math.abs(event.clientX - drag.x) < 3) return;
                const at = timelinePosition(
                    event.clientX,
                    drag.left,
                    drag.width,
                    drag.windowStart,
                    drag.windowEnd,
                );
                if (drag.mode === 'playhead') {
                    this.position = Math.max(
                        this.availableStart,
                        Math.min(this.availableEnd - 1, at),
                    );
                    this.seek();
                    return;
                }
                const delta = Math.round(
                    ((event.clientX - drag.x) / Math.max(1, drag.width)) *
                        (drag.windowEnd - drag.windowStart),
                );
                [this.clipStart, this.clipEnd] = timelineRange(
                    drag.mode,
                    delta,
                    drag.start,
                    drag.end,
                    this.availableStart,
                    this.availableEnd,
                    Math.max(1, Math.floor(1_000_000 / (this.sourceFps || 30))),
                );
                this.position = drag.mode === 'out' ? this.clipEnd - 1 : this.clipStart;
                this.changed();
            },
            async endTimeline(event: PointerEvent) {
                const drag = this.timelinePointer;
                if (!drag || drag.id !== event.pointerId) return;
                this.moveTimeline(event);
                this.timelinePointer = null;
                if (drag.target.hasPointerCapture(drag.id))
                    drag.target.releasePointerCapture(drag.id);
                if (drag.mode === 'window' && Math.abs(event.clientX - drag.x) < 3) {
                    this.position = Math.max(
                        this.availableStart,
                        Math.min(
                            this.availableEnd - 1,
                            timelinePosition(
                                event.clientX,
                                drag.left,
                                drag.width,
                                drag.windowStart,
                                drag.windowEnd,
                            ),
                        ),
                    );
                    this.seek();
                    return;
                }
                if (drag.mode === 'playhead') return;
                const revision = ++rangeGeneration,
                    generation = this.generation;
                const duration = this.clipEnd - this.clipStart;
                try {
                    const position = await this.snapRange(
                        drag.mode === 'out' ? this.clipEnd : this.clipStart,
                    );
                    if (
                        revision !== rangeGeneration ||
                        generation !== this.generation ||
                        !this.opened
                    )
                        return;
                    if (drag.mode === 'window') {
                        this.clipStart = Math.max(
                            this.availableStart,
                            Math.min(this.availableEnd - duration, position),
                        );
                        this.clipEnd = this.clipStart + duration;
                    } else if (drag.mode === 'in')
                        this.clipStart = Math.max(
                            this.availableStart,
                            Math.min(this.clipEnd - 1, position),
                        );
                    else
                        this.clipEnd = Math.min(
                            this.availableEnd,
                            Math.max(this.clipStart + 1, position),
                        );
                    this.position = drag.mode === 'out' ? this.clipEnd - 1 : this.clipStart;
                    this.changed();
                } catch (error) {
                    if (generation === this.generation) this.error = String(error);
                }
            },
            cancelTimeline() {
                const drag = this.timelinePointer;
                if (!drag) return;
                this.timelinePointer = null;
                ++rangeGeneration;
                this.clipStart = drag.start;
                this.clipEnd = drag.end;
                this.position = drag.position;
                if (drag.target.hasPointerCapture(drag.id))
                    drag.target.releasePointerCapture(drag.id);
                this.changed();
            },
            timelineKey(event: KeyboardEvent) {
                if (event.key === 'Escape') {
                    if (this.timelinePointer) {
                        event.preventDefault();
                        event.stopPropagation();
                    }
                    this.cancelTimeline();
                    return;
                }
                if (this.busy || !['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key))
                    return;
                event.preventDefault();
                event.stopPropagation();
                this.pause();
                ++rangeGeneration;
                const mode = (event.currentTarget as HTMLElement).dataset.timeline as TimelineMode;
                if (mode === 'playhead') {
                    const step =
                        Math.max(1, Math.round(1_000_000 / (this.sourceFps || 30))) *
                        (event.shiftKey ? 10 : 1);
                    this.position =
                        event.key === 'Home'
                            ? this.availableStart
                            : event.key === 'End'
                              ? this.availableEnd - 1
                              : Math.max(
                                    this.availableStart,
                                    Math.min(
                                        this.availableEnd - 1,
                                        this.position + (event.key === 'ArrowLeft' ? -step : step),
                                    ),
                                );
                    this.seek();
                    return;
                }

                const step =
                    Math.max(1, Math.round(1_000_000 / (this.sourceFps || 30))) *
                    (event.shiftKey ? 10 : 1);
                let delta = event.key === 'ArrowLeft' ? -step : step;
                if (event.key === 'Home')
                    delta = this.availableStart - (mode === 'out' ? this.clipEnd : this.clipStart);
                if (event.key === 'End')
                    delta = this.availableEnd - (mode === 'in' ? this.clipStart : this.clipEnd);
                [this.clipStart, this.clipEnd] = timelineRange(
                    mode,
                    delta,
                    this.clipStart,
                    this.clipEnd,
                    this.availableStart,
                    this.availableEnd,
                    Math.max(1, Math.floor(1_000_000 / (this.sourceFps || 30))),
                );
                this.position = mode === 'out' ? this.clipEnd - 1 : this.clipStart;
                this.changed();
            },
            // Pause playback and clamp the scrubbed position before requesting a frame
            scrub(event: Event) {
                this.pause();
                this.position = Math.max(
                    this.availableStart,
                    Math.min(
                        this.availableEnd - 1,
                        Number((event.currentTarget as HTMLInputElement).value),
                    ),
                );
                this.seek();
            },
            // Advance the video editor workflow by one step
            async step(event: Event) {
                this.pause();
                const direction = Number((event.currentTarget as HTMLElement).dataset.direction);
                try {
                    if (!this.sourceAvailable && this.preparedTimes.length) {
                        const current = this.preparedTimes.findIndex(
                            (time) => time >= this.position,
                        );
                        this.position =
                            this.preparedTimes[
                                Math.max(
                                    0,
                                    Math.min(
                                        this.preparedTimes.length - 2,
                                        (current < 0 ? this.preparedTimes.length - 1 : current) +
                                            direction,
                                    ),
                                )
                            ];
                    } else
                        this.position = session
                            ? await session.step(this.position, direction)
                            : await app().bridge.command('video_recording_position', {
                                  recording: this.recording!.id,
                                  positionUs: Math.round(this.position),
                                  direction,
                              });
                    this.seek();
                } catch (error) {
                    this.error = String(error);
                }
            },
            // Toggle the play
            togglePlay() {
                if (this.playing) {
                    this.pause();
                    return;
                }
                if (this.validation || !this.recording || this.busy) return;
                this.playing = true;
                playOrigin = performance.now();
                if (this.position < this.clipStart || this.position >= this.clipEnd)
                    this.position = this.clipStart;
                playPosition = this.position;
                // Advance looping playback while visible and stop when the editor becomes inactive
                const tick = () => {
                    if (!this.playing || document.hidden || !this.opened) {
                        this.pause();
                        return;
                    }
                    const span = this.clipEnd - this.clipStart;
                    this.position =
                        this.clipStart +
                        ((playPosition -
                            this.clipStart +
                            (performance.now() - playOrigin) * 1000 * this.options.speed) %
                            span);
                    this.seek(true);
                    const rate = Math.min(
                        60,
                        this.options.max_fps ?? this.sourceFps * this.options.speed,
                    );
                    timer = setTimeout(tick, 1000 / Math.max(0.1, rate));
                };
                tick();
            },
            // Pause the current video editor operation
            pause() {
                this.playing = false;
                session?.pause();
                if (timer) clearTimeout(timer);
                timer = undefined;
            },
            // Change the view
            changeView(event: Event) {
                this.cancelCrop();
                this.previewView = '';
                this.view = (event.currentTarget as HTMLElement).dataset.view || 'device';
                frameRunner.invalidate();
                this.seek();
            },
            // Set the in
            async setIn() {
                const generation = this.generation;
                // Handle a failed video editor operation without retaining stale work
                const position = await this.snapRange(this.position).catch((error) => {
                    if (generation === this.generation) this.error = String(error);
                    return null;
                });
                if (position === null) return;
                if (generation !== this.generation) return;
                this.clipStart = Math.min(position, this.duration - 1);
                if (this.clipEnd <= this.clipStart || this.clipEnd - this.clipStart > 30_000_000)
                    this.clipEnd = Math.min(this.duration, this.clipStart + 30_000_000);
                this.changed();
            },
            // Set the out
            async setOut() {
                const generation = this.generation;
                // Handle a failed video editor operation without retaining stale work
                const position = await this.snapRange(this.position).catch((error) => {
                    if (generation === this.generation) this.error = String(error);
                    return null;
                });
                if (position === null) return;
                if (generation !== this.generation) return;
                this.clipEnd = Math.max(1, position);
                if (this.clipStart >= this.clipEnd || this.clipEnd - this.clipStart > 30_000_000)
                    this.clipStart = Math.max(0, this.clipEnd - 30_000_000);
                this.changed();
            },
            // Snap a requested range boundary to an available source or prepared frame timestamp
            async snapRange(position: number): Promise<number> {
                if (session) return session.request('snap', { position });
                if (!this.sourceAvailable && this.preparedTimes.length)
                    return (
                        this.preparedTimes.filter((time) => time <= position).at(-1) ??
                        this.availableStart
                    );
                return app().bridge.command('video_recording_position', {
                    recording: this.recording!.id,
                    positionUs: Math.max(0, Math.min(this.duration, Math.round(position))),
                    direction: 0,
                });
            },
            // Snap and apply the clip's in-point only while the range request remains current
            async rangeIn(event: Event) {
                const revision = ++rangeGeneration,
                    generation = this.generation;
                const position = await this.snapRange(
                    Number((event.currentTarget as HTMLInputElement).value),
                    // Handle a failed video editor operation without retaining stale work
                ).catch((error) => {
                    if (generation === this.generation) this.error = String(error);
                    return null;
                });
                if (position === null) return;
                if (revision !== rangeGeneration || generation !== this.generation) return;
                this.clipStart = Math.min(position, this.clipEnd - 1);
                this.position = this.clipStart;
                this.changed();
            },
            // Snap and apply the clip's out-point only while the range request remains current
            async rangeOut(event: Event) {
                const revision = ++rangeGeneration,
                    generation = this.generation;
                const position = await this.snapRange(
                    Number((event.currentTarget as HTMLInputElement).value),
                    // Handle a failed video editor operation without retaining stale work
                ).catch((error) => {
                    if (generation === this.generation) this.error = String(error);
                    return null;
                });
                if (position === null) return;
                if (revision !== rangeGeneration || generation !== this.generation) return;
                this.clipEnd = Math.max(position, this.clipStart + 1);
                this.position = Math.max(this.clipStart, this.clipEnd - 1);
                this.changed();
            },
            // Change the time
            async changeTime(event: Event) {
                const generation = this.generation;
                try {
                    const input = event.currentTarget as HTMLInputElement;
                    const position = await this.snapRange(parseTimecode(input.value));
                    if (generation !== this.generation) return;
                    if (input.dataset.field === 'in') this.clipStart = position;
                    else this.clipEnd = position;
                    this.position = Math.min(this.duration - 1, Math.max(0, position));
                    this.changed();
                } catch (error) {
                    this.error = String(error);
                }
            },
            // Change the option
            changeOption(event: Event) {
                const input = event.currentTarget as HTMLInputElement,
                    field = input.dataset.field;
                if (field === 'maximum') {
                    this.options.max_fps =
                        input.value === 'source' ? undefined : Number(input.value);
                    if (this.options.max_fps && this.minimum > this.options.max_fps)
                        this.options.min_fps = this.options.max_fps;
                } else if (field === 'fit') this.options.fit = input.value;
                else if (field === 'encoding') this.options.encoding = input.value;
                else if (field === 'letterbox') this.options.letterbox = input.value;
                else if (field === 'speed') this.options.speed = Number(input.value);
                else if (field === 'minimum') this.options.min_fps = Number(input.value);
                else if (field === 'quality') this.options.jpeg_quality = Number(input.value);
                else if (field === 'min-edge') this.options.min_frame_edge = Number(input.value);
                else if (field === 'max-edge') this.options.max_frame_edge = Number(input.value);
                else if (field?.startsWith('crop-'))
                    this.options.crop = normalizeCrop(
                        this.options.crop.map((value, index) =>
                            index === Number(field.slice(5)) ? Number(input.value) / 100 : value,
                        ),
                    );
                this.changed();
            },
            // Bound output frame dimensions to the target device and mark the options changed
            matchTarget() {
                if (this.target) {
                    this.options.max_frame_edge = Math.max(
                        64,
                        Math.min(1024, Math.max(this.target.width, this.target.height)),
                    );
                    this.options.min_frame_edge = Math.min(this.minEdge, this.maxEdge);
                    this.changed();
                }
            },
            // Reset the crop
            resetCrop() {
                this.options.crop = [0, 0, 1, 1];
                this.changed();
            },
            // Resize the timeline window around the current position within recording bounds
            zoom(event: Event) {
                const factor = Number((event.currentTarget as HTMLElement).dataset.factor);
                const span = Math.min(
                    this.duration,
                    Math.max(1_000_000, (this.windowEnd - this.windowStart) * factor),
                );
                this.windowStart = Math.max(
                    0,
                    Math.min(this.duration - span, this.position - span / 2),
                );
                this.windowEnd = this.windowStart + span;
                void this.loadThumbnails();
            },
            // Fit the timeline window to the selected clip and refresh thumbnails
            fitSelection() {
                [this.windowStart, this.windowEnd] = timelineWindow(
                    this.duration,
                    this.clipStart,
                    this.clipEnd,
                    true,
                );
                void this.loadThumbnails();
            },
            // Fit the timeline window to the full recording and refresh thumbnails
            fitRecording() {
                [this.windowStart, this.windowEnd] = timelineWindow(
                    this.duration,
                    0,
                    this.duration,
                    false,
                );
                void this.loadThumbnails();
            },
            // Pan the timeline by half a window without moving beyond the recording
            pan(event: Event) {
                const direction = Number((event.currentTarget as HTMLElement).dataset.direction),
                    span = this.windowEnd - this.windowStart;
                this.windowStart = Math.max(
                    0,
                    Math.min(this.duration - span, this.windowStart + (direction * span) / 2),
                );
                this.windowEnd = this.windowStart + span;
                void this.loadThumbnails();
            },
            // Clear the thumbnails
            clearThumbnails() {
                thumbnailKeys = [];
                ++thumbnailGeneration;
                decodeQueue.cancelBackground();
                thumbnailCache.clear();
                this.thumbnails.forEach((thumbnail) => releaseMediaPreview(thumbnail.url));
                this.thumbnails = [];
            },
            // Load thumbnails for the video editor workflow
            async loadThumbnails() {
                const generation = ++thumbnailGeneration;
                decodeQueue.cancelBackground();
                if (!this.recording || this.saving) return;
                // clearThumbnails retires the source epoch on reopen/reimport even
                // when the content-addressed recording ID is unchanged.
                const sourceKey = JSON.stringify([
                    this.recording.id,
                    this.recording.sha256,
                    this.sourceAvailable,
                    this.sourceAvailable
                        ? this.decoder.kind === 'ready'
                            ? this.decoder.backend
                            : this.decoder.kind
                        : this.asset,
                    this.availableStart,
                    this.availableEnd,
                    160,
                ]);
                for (let i = 0; i < 6; i++) {
                    if (!this.opened || generation !== thumbnailGeneration || this.saving) return;
                    const position = Math.max(
                        this.availableStart,
                        Math.min(
                            this.availableEnd - 1,
                            Math.round(
                                this.windowStart + ((this.windowEnd - this.windowStart) * i) / 6,
                            ),
                        ),
                    );
                    const key = `${sourceKey}:${position}`;
                    if (thumbnailKeys[i] === key) continue;
                    try {
                        const bytes = await thumbnailCache.get(key, () =>
                            decodeQueue.run(true, async () => {
                                if (
                                    !this.opened ||
                                    generation !== thumbnailGeneration ||
                                    this.saving
                                )
                                    throw Error('Superseded filmstrip request');
                                return !this.sourceAvailable
                                    ? await app().bridge.command('prepared_video_frame', {
                                          asset: this.asset,
                                          positionUs: position - this.availableStart,
                                      })
                                    : session
                                      ? (await session.frame(position, 160)).bytes
                                      : await app().bridge.command('video_recording_frame', {
                                            recording: this.recording!.id,
                                            positionUs: position,
                                            edge: 160,
                                        });
                            }),
                        );
                        if (generation !== thumbnailGeneration || !this.opened) return;
                        const previous = this.thumbnails[i];
                        const next = [...this.thumbnails];
                        next[i] = {
                            position,
                            url: URL.createObjectURL(new Blob([bytes], { type: 'image/jpeg' })),
                        };
                        this.thumbnails = next;
                        thumbnailKeys[i] = key;
                        if (previous) releaseMediaPreview(previous.url);
                    } catch {
                        return;
                    }
                }
            },
            // Create a new snapped clip range near the current position and clear prior asset identity
            async newClip() {
                const generation = this.generation;
                try {
                    const start = await this.snapRange(Math.min(this.position, this.duration - 1));
                    const end = await this.snapRange(Math.min(this.duration, start + 30_000_000));
                    if (generation !== this.generation) return;
                    this.baseAsset = '';
                    this.preparedAsset = '';
                    this.name = `Clip ${this.clips.length + 1}`;
                    this.selectedClip = '';
                    this.clipStart = start;
                    this.clipEnd = end;
                    this.changed();
                } catch (error) {
                    if (generation === this.generation) this.error = String(error);
                }
            },
            // Detach the clip from its prepared asset and give the unsaved copy a bounded name
            duplicateClip() {
                this.baseAsset = '';
                this.preparedAsset = '';
                this.name = `${this.name} copy`.slice(0, 120);
                this.selectedClip = '';
            },
            // Select the clip
            async selectClip(event: Event) {
                const generation = this.generation;
                const asset = (event.currentTarget as HTMLSelectElement).value;
                const clip = this.clips.find((entry) => entry.asset === asset);
                if (!clip) return;
                this.selectedClip = asset;
                this.baseAsset = asset;
                this.asset = asset;
                this.preparedAsset = asset;
                this.name = clip.info.name;
                this.clipStart = clip.info.start_us;
                this.clipEnd = clip.info.end_us;
                this.options = cloneJson(clip.info.options);
                const details = await app().bridge.command('video_clip_details', { asset });
                if (generation !== this.generation) return;
                const metadata = this.rememberPreparation(asset, details, clip.info.start_us);

                if (!this.sourceAvailable) {
                    this.availableStart = clip.info.start_us;
                    this.availableEnd = clip.info.end_us;
                    this.preparedTimes = metadata.times;
                }
                this.position = this.clipStart;
                this.fitSelection();
                this.seek();
                this.closeBaseline = this.editableSignature();
            },
            // Look up measured media-library sizes to compare a prepared copy with its original
            async measuredCopyComparison(asset: string, originalAsset?: string) {
                try {
                    const page = await app().bridge.command('media_library', {
                        page: 1,
                        pageSize: 12,
                        query: asset,
                    });
                    const copy = page.entries.find((entry) => entry.id === asset);
                    if (!copy || copy.error) throw Error('Saved copy unavailable');
                    let original;
                    if (originalAsset && originalAsset !== asset) {
                        const previous = await app().bridge.command('media_library', {
                            page: 1,
                            pageSize: 12,
                            query: originalAsset,
                        });
                        original = previous.entries.find(
                            (entry) => entry.id === originalAsset && !entry.error,
                        );
                    }
                    return savedMediaComparison(copy, original);
                } catch {
                    return 'Saved copy size unavailable. No size savings assumed; preview before applying. Originals and existing assignments stay unchanged.';
                }
            },
            // Save the clip
            async saveClip() {
                if (!this.canSave || !this.recording) return;
                this.pause();
                frameRunner.invalidate();
                ++thumbnailGeneration;
                const generation = this.generation;
                const operation = ++preparationGeneration;
                const originalAsset = request.asset || this.asset;
                this.copyComparison = '';
                const owner = this.owner;
                const activeSession = session;
                let jobId = '';
                this.busy = true;
                this.saving = true;
                this.error = '';
                this.notice = '';
                this.progress = { phase: 'Preparing clip', completed: 0, total: null };
                try {
                    const options = structuredClone(Alpine.raw(this.options));
                    options.trim_start_ms = 0;
                    options.trim_end_ms = null;
                    const draft = {
                        recording_id: this.recording.id,
                        name: this.name,
                        start_us: Math.round(this.clipStart),
                        end_us: Math.round(this.clipEnd),
                        base_asset: this.baseAsset || null,
                        options,
                    };
                    if (!this.sourceAvailable)
                        jobId = await app().bridge.command('prepare_video_clip_from_asset', {
                            asset: this.asset,
                            draft,
                            owner,
                            generation: operation,
                        });
                    else
                        jobId = await app().bridge.command(
                            this.fallback || this.isGif
                                ? 'prepare_video_clip_native'
                                : 'prepare_video_clip',
                            { draft, owner, generation: operation },
                        );
                    if (!this.opened || generation !== this.generation) {
                        await cancelPreparation(jobId);
                        return;
                    }
                    preparationJob = jobId;
                    if (this.sourceAvailable && !this.fallback && !this.isGif) {
                        if (!activeSession) throw Error('Recording decoder unavailable');
                        await activeSession.prepare(
                            jobId,
                            draft.start_us,
                            draft.end_us,
                            this.maxEdge,
                            this.minEdge,
                            this.options.max_fps,
                        );
                        this.progress = { phase: 'Saving clip', completed: 0, total: null };
                        if (!this.opened || generation !== this.generation) return;
                        await app().bridge.command('publish_browser_media', { id: jobId });
                    }
                    for (;;) {
                        if (!this.opened || generation !== this.generation) return;
                        const job = await app().bridge.command('media_preparation', {
                            id: jobId,
                        });
                        if (!this.opened || generation !== this.generation) return;
                        if (job.state === 'complete' && job.asset) {
                            await retainMediaFor(app(), job.asset, owner);
                            if (!this.opened || generation !== this.generation) return;
                            this.preparedAsset = job.asset;
                            this.baseAsset = job.asset;
                            this.asset = job.asset;
                            this.clips = await app().bridge.command('video_clips', {
                                recording: this.recording.id,
                            });
                            if (!this.opened || generation !== this.generation) return;
                            const details = await app().bridge.command('video_clip_details', {
                                asset: job.asset,
                            });
                            if (!this.opened || generation !== this.generation) return;
                            const metadata = this.rememberPreparation(
                                job.asset,
                                details,
                                draft.start_us,
                            );

                            if (!this.sourceAvailable) {
                                this.availableStart = draft.start_us;
                                this.availableEnd = draft.end_us;
                                this.preparedTimes = metadata.times;
                                this.position = draft.start_us;
                            }
                            this.selectedClip = job.asset;
                            this.options = options;
                            this.closeBaseline = this.editableSignature();
                            const comparison = await this.measuredCopyComparison(
                                job.asset,
                                originalAsset,
                            );
                            if (!this.opened || generation !== this.generation) return;
                            this.copyComparison = comparison;
                            request.saved?.();
                            this.notice = request.apply
                                ? 'Clip saved. Apply clip to use it in your current edit.'
                                : 'Clip saved to the media library. Other assignments keep their existing revision.';
                            break;
                        }
                        if (job.state === 'failed' || job.state === 'cancelled')
                            throw Error(job.error || 'Clip preparation cancelled');
                        this.progress = {
                            phase: 'Preparing clip with FFmpeg',
                            completed: job.frames,
                            total: null,
                        };
                        await new Promise((resolve) => setTimeout(resolve, 100));
                    }
                } catch (error) {
                    if (generation === this.generation) {
                        this.error = String(error);
                        if (jobId) await cancelPreparation(jobId).catch(() => {});
                    }
                } finally {
                    if (jobId) await finishPreparation(jobId).catch(() => {});
                    if (preparationJob === jobId) preparationJob = '';
                    if (generation === this.generation) {
                        this.busy = false;
                        this.saving = false;
                        this.progress = null;
                    }
                }
            },
            // Apply the clip
            async applyClip() {
                if (!this.canApply || (request.current && !request.current())) {
                    this.error = 'The destination changed. Close and reopen the editor.';
                    return;
                }
                try {
                    if (request.owner)
                        await retainMediaFor(app(), this.preparedAsset, request.owner);
                    await request.apply?.(
                        this.preparedAsset,
                        structuredClone(Alpine.raw(this.options)),
                    );
                    this.close();
                } catch (error) {
                    this.error = String(error);
                }
            },
            // Prepare an unused-recording review while retaining editor generation and job ownership
            async reviewRecordings() {
                if (this.busy || !this.opened) return;
                const generation = this.generation;
                this.busy = true;
                this.error = '';
                this.progress = { phase: 'Reviewing unused recordings', completed: 0, total: null };
                try {
                    if (this.cleanup) {
                        const previous = this.cleanup;
                        await discardRecordingReview(previous.id);
                        if (this.cleanup === previous) this.cleanup = null;
                        if (!this.opened || generation !== this.generation) return;
                    }
                    const review = await app().bridge.command('review_unused_recordings');
                    if (!this.opened || generation !== this.generation) {
                        await discardRecordingReview(review.id);
                        return;
                    }
                    this.cleanup = review;
                    if (!review.recordings.length)
                        this.notice =
                            'No unused recordings. Sources used by saved clips or active editors are retained.';
                } catch (error) {
                    if (generation === this.generation) this.error = String(error);
                } finally {
                    if (generation === this.generation) {
                        this.busy = false;
                        this.progress = null;
                    }
                }
            },
            // Release the pending recording-cleanup review and clear its UI state
            discardCleanup() {
                if (this.cleanup) void discardRecordingReview(this.cleanup.id).catch(() => {});
                this.cleanup = null;
            },
            // Commit the reviewed recording cleanup while preventing duplicate submissions
            async cleanupRecordings() {
                if (!this.cleanup || this.cleanupWorking) return;
                this.cleanupWorking = true;
                this.busy = true;
                this.error = '';
                this.progress = {
                    phase: 'Verifying and deleting reviewed recordings',
                    completed: 0,
                    total: null,
                };
                try {
                    const bytes = await app().bridge.command('commit_recording_cleanup', {
                        id: this.cleanup.id,
                    });
                    this.notice = `Removed ${(bytes / 1024 / 1024).toFixed(1)} MiB of unused recordings.`;
                    this.cleanup = null;
                } catch (error) {
                    this.error = String(error);
                } finally {
                    this.cleanupWorking = false;
                    this.busy = false;
                    this.progress = null;
                }
            },
            // Cancel the work
            async cancelWork() {
                if (this.cleanupWorking) return;
                const retiredOwner = this.owner;
                ++this.generation;
                frameRunner.invalidate();
                ++thumbnailGeneration;
                decodeQueue.cancelBackground();
                thumbnailCache.clear();
                this.pause();
                this.decoder = { kind: 'idle' };
                if (importJob) await cancelImport(importJob).catch(() => {});
                if (preparationJob) await cancelPreparation(preparationJob).catch(() => {});
                session?.close();
                session = undefined;
                this.busy = false;
                this.saving = false;
                this.progress = null;
                importJob = '';
                preparationJob = '';
                await releaseMediaOwner(app(), retiredOwner).catch(() => {});
                await releaseRecording(retiredOwner).catch(() => {});
                if (this.asset) await retainMediaFor(app(), this.asset, this.owner).catch(() => {});
                if (this.recording && this.sourceAvailable)
                    await app()
                        .bridge.command('retain_video_recording', {
                            recording: this.recording.id,
                            owner: this.owner,
                        })
                        .catch(() => {});
                this.notice =
                    'Operation cancelled. Existing clips are unchanged. Retry the decoder to continue preparing this recording.';
            },
        };
    });
}
