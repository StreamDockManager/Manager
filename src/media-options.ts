import { reportDecodeFailure } from './frontend-errors';
import {
    containRect,
    destinationCrop,
    panDestination,
    zoomDestination,
    transformCrop,
    type CropHandle,
} from './video-crop';
import { decodeMediaImages } from './media-decode';
import { cloneJson } from './json-clone';
import type AlpineType from '@alpinejs/csp';
import type { CommandContracts } from '../generated/commands';
import type { MediaOptions } from '../generated/runtime';
import { assertCurrentMediaOptions, defaultMedia, native } from './bridge';
import {
    latestPreview,
    readMediaPreview,
    releaseMediaPreview,
    type MediaPreview,
} from './media-preview';
import type { AppState } from './state';
import { openVideoEditor } from './video-editor';
import { windowCloseOwners } from './window-close-owners';
import { windowWork } from './window-work';

type PreviewArgs = CommandContracts['media_preview']['args'];
type DecodedPreview = MediaPreview & { aspect?: number };
type Crop = MediaOptions['crop'];
type Drag = {
    pointer: number;
    x: number;
    y: number;
    width: number;
    height: number;
    edge: string;
    crop: Crop;
    device?: boolean;
    original?: Crop;
    moved?: boolean;
};
const minimum = 0.01;
// Clamp a numeric value to the permitted inclusive range
const clamp = (value: number, min: number, max: number) => Math.min(Math.max(value, min), max);
// Round crop coordinates to stable four-decimal precision
const rounded = (value: number) => Math.round(value * 10000) / 10000;

// Resize the crop rectangle from the active pointer drag
function resizedCrop(drag: Drag, event: PointerEvent): Crop {
    const dx = (event.clientX - drag.x) / drag.width;
    const dy = (event.clientY - drag.y) / drag.height;
    const [x, y, width, height] = drag.crop;
    if (drag.edge === 'move')
        return [
            clamp(rounded(x + dx), 0, 1 - width),
            clamp(rounded(y + dy), 0, 1 - height),
            width,
            height,
        ];
    const minWidth = Math.min(minimum, width);
    const minHeight = Math.min(minimum, height);
    const left = drag.edge.includes('w') ? clamp(rounded(x + dx), 0, x + width - minWidth) : x;
    const top = drag.edge.includes('n') ? clamp(rounded(y + dy), 0, y + height - minHeight) : y;
    const right = drag.edge.includes('e')
        ? clamp(rounded(x + width + dx), x + minWidth, 1)
        : x + width;
    const bottom = drag.edge.includes('s')
        ? clamp(rounded(y + height + dy), y + minHeight, 1)
        : y + height;
    return [
        left,
        top,
        Math.min(1 - left, Math.max(minWidth, rounded(right - left))),
        Math.min(1 - top, Math.max(minHeight, rounded(bottom - top))),
    ];
}

// Register the media crop and optimization preview controller
export function registerMediaOptions(Alpine: typeof AlpineType, app: () => AppState) {
    // Create isolated reactive state for the media options controller
    Alpine.data('mediaOptions', () => {
        let device: ReturnType<typeof latestPreview<PreviewArgs, DecodedPreview>>;
        let source: typeof device;
        let observer: MutationObserver | undefined;
        let resizeObserver: ResizeObserver | undefined;
        let dialogObserver: MutationObserver | undefined;
        let unregisterCloseOwner: (() => void) | undefined;
        let visibility: () => void;
        let resume: () => void;
        let timer: ReturnType<typeof setTimeout> | undefined;
        let drag: Drag | undefined;
        let nextFrame = 0;
        // Render and decode the requested media optimization preview
        const render = async (args: PreviewArgs): Promise<DecodedPreview> => {
            const result = readMediaPreview(
                await windowWork.run('media options preview', () =>
                    app().bridge.command('media_preview', args),
                ),
            );
            if (!result.image) return result;
            try {
                const image = new Image();
                await decodeMediaImages(
                    [result.image],
                    () => true,
                    // Decode the image before making the preview available
                    async (url) => {
                        image.decoding = 'async';
                        image.src = url;
                        try {
                            await image.decode();
                        } catch (error) {
                            reportDecodeFailure('sd-media-options', 'media-preview');
                            throw error;
                        }
                    },
                );
                return { ...result, aspect: image.naturalWidth / image.naturalHeight };
            } catch (error) {
                releaseMediaPreview(result.image);
                throw error;
            }
        };
        // Discard the staged media or editor state owned by the current dialog
        const discard = ({ image }: MediaPreview) => releaseMediaPreview(image);
        return {
            options: defaultMedia(),
            asset: '',
            controlId: null as string | null,
            image: '',
            sourceImage: '',
            sourceAspect: 1,
            get targetAspect() {
                const manifest = app().device?.manifest;
                const control = manifest?.controls.find((c) => c.id === this.controlId);
                const size = control
                    ? [control.image_width || 0, control.image_height || 0]
                    : manifest?.capabilities.background_size;
                return size && size[0] > 0 && size[1] > 0 ? size[0] / size[1] : 1;
            },
            photoView: 'device',
            inBackgroundEditor: false,
            changePhotoView(event: Event) {
                this.cancelCrop();
                this.photoView = (event.currentTarget as HTMLElement).dataset.view || 'device';
            },
            get destinationStyle() {
                return {
                    aspectRatio: `${this.targetAspect}`,
                    maxWidth: `${260 * this.targetAspect}px`,
                };
            },
            destinationCrop() {
                return destinationCrop(
                    this.options.crop,
                    this.sourceAspect,
                    1,
                    this.targetAspect,
                    1,
                    this.options.fit,
                );
            },
            zoomPhoto(event: Event) {
                if (!this.image || !this.sourceImage || this.$el.closest('fieldset:disabled'))
                    return;
                this.cancelCrop();
                this.update({
                    ...this.options,
                    crop: zoomDestination(
                        this.destinationCrop(),
                        Number((event.currentTarget as HTMLElement).dataset.zoom),
                    ),
                });
            },
            resetPhoto() {
                this.cancelCrop();
                this.update({ ...this.options, crop: [0, 0, 1, 1] });
            },
            startPan(event: PointerEvent) {
                if (
                    drag ||
                    event.button !== 0 ||
                    !event.isPrimary ||
                    !this.image ||
                    !this.sourceImage ||
                    this.$el.closest('fieldset:disabled')
                )
                    return;
                const frame = this.$refs.destinationFrame as HTMLElement;
                const rect = frame.getBoundingClientRect();
                const crop = this.destinationCrop();
                const image = containRect(rect.width, rect.height, this.targetAspect, 1);
                const px = event.clientX - rect.left - image.x,
                    py = event.clientY - rect.top - image.y;
                const content = containRect(
                    image.width,
                    image.height,
                    crop[2] * this.sourceAspect,
                    crop[3],
                );
                if (
                    px < 0 ||
                    py < 0 ||
                    px > image.width ||
                    py > image.height ||
                    (this.options.fit === 'fit' &&
                        (px < content.x ||
                            py < content.y ||
                            px > content.x + content.width ||
                            py > content.y + content.height))
                )
                    return;
                event.preventDefault();
                (event.currentTarget as HTMLElement).focus();
                (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
                drag = {
                    pointer: event.pointerId,
                    x: event.clientX,
                    y: event.clientY,
                    width: image.width,
                    height: image.height,
                    edge: 'move',
                    crop,
                    original: [...this.options.crop],
                    device: true,
                };
            },
            cancelCrop(event?: PointerEvent) {
                if (!drag || (event && drag.pointer !== event.pointerId)) return;
                const original = drag.original || drag.crop;
                drag = undefined;
                this.update({ ...this.options, crop: original });
            },
            photoKey(event: KeyboardEvent) {
                if (this.$el.closest('fieldset:disabled')) return;
                if (event.key === 'Escape' && drag) {
                    event.preventDefault();
                    event.stopPropagation();
                    this.cancelCrop();
                    return;
                }
                const keys: Record<string, number[]> = {
                    ArrowLeft: [-1, 0],
                    ArrowRight: [1, 0],
                    ArrowUp: [0, -1],
                    ArrowDown: [0, 1],
                };
                const delta = keys[event.key];
                if (!delta) return;
                event.preventDefault();
                event.stopPropagation();
                let crop: Crop;
                if (this.inBackgroundEditor && this.photoView === 'device') {
                    const rect = (
                        this.$refs.destinationFrame as HTMLElement
                    ).getBoundingClientRect();
                    const b = containRect(rect.width, rect.height, this.targetAspect, 1);
                    const step = event.shiftKey ? 25 : 5;
                    crop = panDestination(
                        this.destinationCrop(),
                        delta[0] * step,
                        delta[1] * step,
                        this.sourceAspect,
                        1,
                        b.width,
                        b.height,
                        this.options.fit,
                    );
                } else {
                    const step = event.shiftKey ? 0.05 : 0.01;
                    crop = transformCrop(
                        this.options.crop,
                        delta[0] * step,
                        delta[1] * step,
                        ((event.target as HTMLElement).dataset.cropEdge || 'move') as CropHandle,
                    );
                }
                this.update({ ...this.options, crop });
            },
            error: '',
            sourceError: '',
            kind: '' as '' | 'photo' | 'clip',
            sourceFormat: '',
            sourceFps: 24,
            requested: 0,
            duration: 0,
            desktopPreview: native,
            // Return layout class for the current media options view
            get layoutClass() {
                return this.kind ? `${this.kind}-layout` : '';
            },
            get clipType() {
                return this.sourceFormat === 'gif' ? 'GIF clip' : 'Video clip';
            },
            get clipMetadata() {
                return `${(this.duration / 1000).toFixed(2)} s · ${this.sourceFps.toFixed(2)} source FPS`;
            },
            get cadenceDescription() {
                const cadence = `Requested animation cadence: ${this.requested} updates/s. Actual delivery depends on device throughput.`;
                return this.inBackgroundEditor
                    ? cadence
                    : `${cadence} Source duration: ${this.duration} ms.`;
            },
            // Return source style for the current media options view
            get sourceStyle() {
                return { '--photo-width': `${260 * this.sourceAspect}px` };
            },
            // Return crop style for the current media options view
            get cropStyle() {
                const [x, y, width, height] = this.options.crop;
                return {
                    left: `${x * 100}%`,
                    top: `${y * 100}%`,
                    width: `${width * 100}%`,
                    height: `${height * 100}%`,
                };
            },
            // Return crop x for the current media options view
            get cropX() {
                return rounded(this.options.crop[0] * 100);
            },
            // Return crop y for the current media options view
            get cropY() {
                return rounded(this.options.crop[1] * 100);
            },
            // Return crop width for the current media options view
            get cropWidth() {
                return rounded(this.options.crop[2] * 100);
            },
            // Return crop height for the current media options view
            get cropHeight() {
                return rounded(this.options.crop[3] * 100);
            },
            // Attach media options listeners and register owned draft resources
            init() {
                this.inBackgroundEditor = Boolean(this.$el.closest('.background-editor-dialog'));
                const host = this.$el.closest('sd-media-options');
                if (!host) return;
                const dialog = this.$el.closest('dialog');
                unregisterCloseOwner = windowCloseOwners.register({
                    name: 'Media options preview',
                    // Report whether the controller retains unsaved or active work
                    state: () => 'clean',
                    // Leave cancellation cleanup to the separate preview-release hook
                    discard: () => {},
                    // Release resources retained by the current controller
                    release: () => this.stopPreview(),
                });
                source = latestPreview(
                    render,
                    // Publish the visible source preview and defer release of its previous image
                    (result) => {
                        if (!this.visible()) return discard(result);
                        const previous = this.sourceImage;
                        if (result.image && !this.kind) this.kind = 'photo';
                        this.sourceAspect = result.aspect || 1;
                        this.sourceImage = result.image;
                        this.sourceError = '';
                        requestAnimationFrame(() => releaseMediaPreview(previous));
                    },
                    // Report source-image preview failure without replacing an existing device preview
                    (error) => {
                        if (this.kind === 'photo') this.sourceError = String(error);
                    },
                    discard,
                );
                device = latestPreview(
                    render,
                    // Publish the visible device preview and refresh its media metadata
                    (result) => {
                        if (!this.visible()) return discard(result);
                        const previous = this.image;
                        this.image = result.image;
                        requestAnimationFrame(() => releaseMediaPreview(previous));
                        const first = !this.kind;
                        const kind = result.animated ? 'clip' : 'photo';
                        const format = result.format || '';
                        if (
                            this.inBackgroundEditor &&
                            (this.kind !== kind || this.sourceFormat !== format)
                        ) {
                            this.$dispatch(
                                'media-clip-editor',
                                kind === 'clip'
                                    ? {
                                          asset: this.asset,
                                          label: format === 'gif' ? 'Edit GIF' : 'Edit video',
                                          open: () => this.editVideo(),
                                      }
                                    : null,
                            );
                        }
                        this.kind = kind;
                        this.sourceFormat = format;
                        this.sourceFps = result.source_fps || 24;
                        this.requested = Number(result.requested_tps.toFixed(1));
                        this.duration = result.duration_ms;
                        this.error = '';
                        if (this.kind === 'photo') {
                            if (first && result.image) this.previewSource();
                        } else {
                            const period =
                                1000 /
                                Math.min(
                                    60,
                                    this.options.max_fps ?? this.sourceFps * this.options.speed,
                                );
                            const now = performance.now();
                            nextFrame +=
                                (Math.floor(Math.max(0, now - nextFrame) / period) + 1) * period;
                            this.schedule(Math.max(0, nextFrame - now));
                        }
                    },
                    // Report device-preview failure while keeping the original source editable
                    (error) => {
                        this.error = String(error);
                        // The original can still be edited when target rendering fails
                        if (!this.kind) this.previewSource();
                    },
                    discard,
                );
                // Create isolated reactive state for the media options controller
                visibility = () => {
                    this.clear();
                    this.schedule(75);
                };
                // Read the current dialog or editor state for synchronization
                const read = () => {
                    try {
                        const options = {
                            ...defaultMedia(),
                            ...JSON.parse(host.getAttribute('content') || '{}'),
                        };
                        assertCurrentMediaOptions(options);
                        const asset = host.getAttribute('asset') || '';
                        const controlId = host.getAttribute('control') || null;
                        const targetChanged = asset !== this.asset || controlId !== this.controlId;
                        if (
                            !targetChanged &&
                            JSON.stringify(options) === JSON.stringify(this.options)
                        )
                            return;
                        if (targetChanged) this.clear();
                        this.asset = asset;
                        this.controlId = controlId;
                        this.options = options;
                        this.schedule(85);
                    } catch (error) {
                        this.error = String(error);
                    }
                };
                observer = new MutationObserver(read);
                if (dialog) dialogObserver = new MutationObserver(visibility);
                const panel = this.$el.querySelector('.media-preview-panel');
                let panelWidth = 0;
                if (panel && typeof ResizeObserver !== 'undefined')
                    // Create isolated reactive state for the media options controller
                    resizeObserver = new ResizeObserver(([entry]) => {
                        const width = Math.round(entry.contentRect.width);
                        if (width > 0 && width !== panelWidth) {
                            panelWidth = width;
                            this.schedule(75);
                        }
                    });
                // Create isolated reactive state for the media options controller
                resume = () => {
                    if (panel) resizeObserver?.observe(panel);
                    document.addEventListener('visibilitychange', visibility);
                    if (dialog)
                        dialogObserver?.observe(dialog, {
                            attributes: true,
                            attributeFilter: ['open'],
                        });
                    observer?.observe(host, {
                        attributes: true,
                        attributeFilter: ['content', 'asset', 'control'],
                    });
                    read();
                    this.schedule(75);
                };
                this.$watch('$store.app.route', visibility);
                this.$watch('$store.app.deviceId', visibility);
                window.addEventListener('editor-window-resumed', resume);
                resume();
            },
            // Remove media options listeners and release owned resources
            destroy() {
                unregisterCloseOwner?.();
                if (resume) window.removeEventListener('editor-window-resumed', resume);
                this.stopPreview();
            },
            // Invalidate pending preview work before releasing its current image
            stopPreview() {
                observer?.disconnect();
                resizeObserver?.disconnect();
                dialogObserver?.disconnect();
                document.removeEventListener('visibilitychange', visibility);
                this.clear();
            },
            // Check whether the media-options dialog is currently open
            visible() {
                return (
                    this.asset &&
                    !document.hidden &&
                    !windowWork.quiescing &&
                    !windowCloseOwners.closing &&
                    Boolean(this.$el.getClientRects().length) &&
                    (!this.$el.closest('dialog') || this.$el.closest('dialog')!.open) &&
                    ['editor', 'settings'].includes(app().route)
                );
            },
            // Clear the media-options draft and its preview resources
            clear() {
                if (timer !== undefined) clearTimeout(timer);
                timer = undefined;
                device?.invalidate();
                source?.invalidate();
                drag = undefined;
                nextFrame = 0;
                releaseMediaPreview(this.image);
                releaseMediaPreview(this.sourceImage);
                this.image = '';
                this.sourceImage = '';
                this.error = '';
                this.sourceError = '';
                if (this.inBackgroundEditor && this.kind === 'clip')
                    this.$dispatch('media-clip-editor', null);
                this.kind = '';
                this.photoView = 'device';
            },
            // Schedule the next debug poll for the current run generation
            schedule(delay: number) {
                if (timer !== undefined) return;
                // Resume scheduled media options work while its owner remains active
                timer = setTimeout(() => {
                    timer = undefined;
                    this.preview();
                }, delay);
            },
            // Apply the changed media option from the form event
            change(event: Event) {
                const input = event.currentTarget as HTMLInputElement;
                const name = input.dataset.field;
                const options = { ...this.options, crop: [...this.options.crop] as Crop };
                if (name === 'encoding') options.encoding = input.value;
                else if (name === 'fit') options.fit = input.value;
                else {
                    const value = input.valueAsNumber / 100;
                    if (!Number.isFinite(value)) return;
                    const [x, y, w, h] = options.crop;
                    if (name === '0') {
                        options.crop[0] = clamp(value, 0, 1 - minimum);
                        options.crop[2] = Math.min(w, 1 - options.crop[0]);
                    } else if (name === '1') {
                        options.crop[1] = clamp(value, 0, 1 - minimum);
                        options.crop[3] = Math.min(h, 1 - options.crop[1]);
                    } else if (name === '2') options.crop[2] = clamp(value, minimum, 1 - x);
                    else if (name === '3') options.crop[3] = clamp(value, minimum, 1 - y);
                }
                this.update(options);
            },
            // Capture the pointer and begin editing the selected crop handle
            startCrop(event: PointerEvent) {
                if (
                    drag ||
                    event.button !== 0 ||
                    !event.isPrimary ||
                    this.$el.closest('fieldset:disabled')
                )
                    return;
                const frame = this.$refs.photoFrame as HTMLElement;
                const { width, height } = frame.getBoundingClientRect();
                if (!width || !height) return;
                event.preventDefault();
                (event.target as HTMLElement).focus();
                (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
                drag = {
                    pointer: event.pointerId,
                    x: event.clientX,
                    y: event.clientY,
                    width,
                    height,
                    edge: (event.target as HTMLElement).dataset.cropEdge || 'move',
                    crop: [...this.options.crop],
                };
            },
            // Update the crop draft from the captured pointer movement
            moveCrop(event: PointerEvent) {
                if (drag?.pointer !== event.pointerId || this.$el.closest('fieldset:disabled'))
                    return;
                if (drag.device) {
                    const distance =
                        Math.abs(event.clientX - drag.x) + Math.abs(event.clientY - drag.y);
                    if (!drag.moved && distance < 2) return;
                    drag.moved = true;
                    const crop =
                        distance < 2
                            ? drag.original!
                            : panDestination(
                                  drag.crop,
                                  event.clientX - drag.x,
                                  event.clientY - drag.y,
                                  this.sourceAspect,
                                  1,
                                  drag.width,
                                  drag.height,
                                  this.options.fit,
                              );
                    this.update({ ...this.options, crop });
                    return;
                }
                this.update({ ...this.options, crop: resizedCrop(drag, event) });
            },
            // Finish crop dragging and release pointer capture
            endCrop(event: PointerEvent) {
                if (drag?.pointer === event.pointerId) drag = undefined;
            },
            // Transfer the chosen media options to the requesting editor
            update(options: MediaOptions, asset?: string) {
                assertCurrentMediaOptions(options);
                device?.invalidate();
                this.options = options;
                this.$el.dispatchEvent(
                    new CustomEvent('media-change', {
                        detail: { options, asset },
                        bubbles: true,
                        composed: true,
                    }),
                );
                this.schedule(85);
            },
            // Edit video in the current media options draft
            editVideo() {
                const asset = this.asset;
                openVideoEditor({
                    asset,
                    controlId: this.controlId,
                    options: cloneJson(this.options),
                    // Check that the asynchronous result still belongs to the active editor
                    current: () => this.asset === asset && this.$el.isConnected,
                    // Apply the chosen asset and media options to the parent editor
                    apply: (nextAsset, options) => this.update(options, nextAsset),
                });
            },
            // Build preview parameters from the current crop and optimization draft
            previewArgs(): PreviewArgs {
                const panel = this.$el.querySelector<HTMLElement>('.media-preview-panel');
                const image = this.$el.querySelector<HTMLImageElement>('.media-edit-preview');
                const height = image ? parseFloat(getComputedStyle(image).maxHeight) : 180;
                const ratio = Math.max(1, Math.min(2, window.devicePixelRatio || 1));
                // Normalize the preview dimension within the permitted bounds
                const edge = (value: number) =>
                    Math.max(1, Math.min(1280, Math.ceil(value * ratio)));
                return {
                    asset: this.asset,
                    options: cloneJson(this.options),
                    deviceId: app().deviceId,
                    controlId: this.controlId,
                    previewBounds: [
                        edge(panel?.clientWidth || 640),
                        edge(Number.isFinite(height) ? height : 180),
                    ],
                };
            },
            // Request a preview of the current media-options draft
            preview() {
                if (!this.visible()) return;
                if (!nextFrame) nextFrame = performance.now();
                device.request(this.previewArgs());
            },
            // Prepare the selected media source for the current preview request
            previewSource() {
                if (!this.visible()) return;
                this.sourceError = '';
                source.request({ ...this.previewArgs(), source: true });
            },
        };
    });
}
