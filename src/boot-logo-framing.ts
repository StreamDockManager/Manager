import {
    containRect,
    destinationCrop,
    panDestination,
    zoomDestination,
    transformCrop,
    type Crop,
    type CropHandle,
} from './video-crop';
import type { BootLogoPreview } from '../generated/runtime';
type Drag = {
    id: number;
    x: number;
    y: number;
    width: number;
    height: number;
    crop: Crop;
    original: Crop;
    handle: CropHandle;
    device: boolean;
    moved: boolean;
    element: HTMLElement;
};
interface Host {
    crop: Crop;
    fit: string;
    view: string;
    preview: BootLogoPreview | null;
    saving: boolean;
    $refs?: Record<string, HTMLElement>;
    framingDrag: Drag | null;
    queueFraming(): void;
    cancelFraming(): void;
}
function dimensions(h: Host) {
    return { source: h.preview!.source_size, target: h.preview!.size };
}
function visibleCrop(h: Host): Crop {
    const { source, target } = dimensions(h);
    return destinationCrop(h.crop, ...source, ...target, h.fit === 'contain' ? 'fit' : 'fill');
}
/** The same normalized source-pixel math used by the media editors, with no persistence. */
export function bootLogoFraming() {
    return {
        crop: [0, 0, 1, 1] as Crop,
        view: 'device',
        framingDrag: null as Drag | null,
        changeView(this: Host, event: Event) {
            this.cancelFraming();
            this.view = (event.currentTarget as HTMLElement).dataset.view || 'device';
        },
        zoomFraming(this: Host, event: Event) {
            if (!this.preview || this.saving) return;
            this.cancelFraming();
            this.crop = zoomDestination(
                visibleCrop(this),
                Number((event.currentTarget as HTMLElement).dataset.zoom),
            );
            this.queueFraming();
        },
        resetFraming(this: Host) {
            if (this.saving) return;
            this.cancelFraming();
            this.crop = [0, 0, 1, 1];
            this.queueFraming();
        },
        startFraming(this: Host, event: PointerEvent) {
            if (
                !this.preview ||
                this.saving ||
                event.button !== 0 ||
                !event.isPrimary ||
                this.framingDrag
            )
                return;
            const device = this.view === 'device',
                { source, target } = dimensions(this);
            const frame = (
                device ? this.$refs!.logoDevice : this.$refs!.logoSource
            ).getBoundingClientRect();
            const bounds = containRect(frame.width, frame.height, ...(device ? target : source));
            const px = event.clientX - frame.left - bounds.x,
                py = event.clientY - frame.top - bounds.y;
            const crop = device ? visibleCrop(this) : ([...this.crop] as Crop);
            const content = containRect(
                bounds.width,
                bounds.height,
                crop[2] * source[0],
                crop[3] * source[1],
            );
            if (
                px < 0 ||
                py < 0 ||
                px > bounds.width ||
                py > bounds.height ||
                (device &&
                    this.fit === 'contain' &&
                    (px < content.x ||
                        py < content.y ||
                        px > content.x + content.width ||
                        py > content.y + content.height))
            )
                return;
            event.preventDefault();
            event.stopPropagation();
            const element = event.currentTarget as HTMLElement;
            element.focus();
            element.setPointerCapture(event.pointerId);
            this.framingDrag = {
                id: event.pointerId,
                x: event.clientX,
                y: event.clientY,
                width: bounds.width,
                height: bounds.height,
                crop,
                original: [...this.crop],
                handle: ((event.target as HTMLElement).dataset.cropEdge || 'move') as CropHandle,
                device,
                moved: false,
                element,
            };
        },
        moveFraming(this: Host, event: PointerEvent) {
            const d = this.framingDrag;
            if (!d || d.id !== event.pointerId || !this.preview) return;
            const dx = event.clientX - d.x,
                dy = event.clientY - d.y;
            if (!d.moved && Math.hypot(dx, dy) < 2) return;
            d.moved = true;
            this.crop =
                Math.hypot(dx, dy) < 2
                    ? [...d.original]
                    : d.device
                      ? panDestination(
                            d.crop,
                            dx,
                            dy,
                            ...this.preview.source_size,
                            d.width,
                            d.height,
                            this.fit === 'contain' ? 'fit' : 'fill',
                        )
                      : transformCrop(d.crop, dx / d.width, dy / d.height, d.handle);
            this.queueFraming();
        },
        endFraming(this: Host, event?: PointerEvent) {
            const d = this.framingDrag;
            if (!d || (event && d.id !== event.pointerId)) return;
            this.framingDrag = null;
            if (d.element.hasPointerCapture(d.id)) d.element.releasePointerCapture(d.id);
        },
        cancelFraming(this: Host) {
            const d = this.framingDrag;
            if (!d) return;
            this.framingDrag = null;
            this.crop = [...d.original];
            if (d.element.hasPointerCapture(d.id)) d.element.releasePointerCapture(d.id);
            if (d.moved) this.queueFraming();
        },
        framingKey(this: Host, event: KeyboardEvent) {
            if (event.key === 'Escape' && this.framingDrag) {
                event.preventDefault();
                event.stopPropagation();
                this.cancelFraming();
                return;
            }
            const delta: Record<string, number[]> = {
                ArrowLeft: [-1, 0],
                ArrowRight: [1, 0],
                ArrowUp: [0, -1],
                ArrowDown: [0, 1],
            };
            const d = delta[event.key];
            if (!d || !this.preview || this.saving) return;
            event.preventDefault();
            event.stopPropagation();
            if (this.view === 'device') {
                const rect = this.$refs!.logoDevice.getBoundingClientRect(),
                    b = containRect(rect.width, rect.height, ...this.preview.size),
                    step = event.shiftKey ? 25 : 5;
                this.crop = panDestination(
                    visibleCrop(this),
                    d[0] * step,
                    d[1] * step,
                    ...this.preview.source_size,
                    b.width,
                    b.height,
                    this.fit === 'contain' ? 'fit' : 'fill',
                );
            } else {
                const step = event.shiftKey ? 0.05 : 0.01;
                this.crop = transformCrop(
                    this.crop,
                    d[0] * step,
                    d[1] * step,
                    ((event.target as HTMLElement).dataset.cropEdge || 'move') as CropHandle,
                );
            }
            this.queueFraming();
        },
    };
}
