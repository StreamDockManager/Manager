import { decodeMediaImages } from './media-decode';

/** One active render and one replaceable pending render, outside Alpine state. */
export function latestPreview<T, R>(
    render: (input: T) => Promise<R>,
    publish: (result: R) => void,
    failed: (error: unknown) => void,
    // Ignore discarded results when the caller has no cleanup callback
    discarded: (result: R) => void = () => {},
) {
    let active = false;
    let generation = 0;
    let pending: { input: T; generation: number } | undefined;
    // Render the newest queued preview and discard superseded results
    const drain = async () => {
        if (active || !pending) return;
        const request = pending;
        pending = undefined;
        if (request.generation !== generation) return;
        active = true;
        try {
            const result = await render(request.input);
            if (request.generation === generation) publish(result);
            else discarded(result);
        } catch (error) {
            if (request.generation === generation) failed(error);
        } finally {
            active = false;
            void drain();
        }
    };
    return {
        // Invalidate pending preview results without publishing them
        invalidate() {
            generation++;
            pending = undefined;
        },
        // Queue the latest preview request for serialized rendering
        request(input: T) {
            pending = { input, generation: ++generation };
            void drain();
        },
    };
}
export interface MediaPreview {
    format?: string;
    source_fps?: number;
    image: string;
    max_fps: number;
    animated: boolean;
    requested_tps: number;
    duration_ms: number;
}
const maxPreviewBytes = 8 * 1024 * 1024;

/** Native response: a little-endian JSON length, bounded metadata, encoded image. */
export function readMediaPreview(value: ArrayBuffer | number[] | MediaPreview): MediaPreview {
    // Tauri's postMessage fallback on macOS and iOS delivers raw IPC as a JSON byte array
    let buffer: ArrayBuffer;
    if (Array.isArray(value)) {
        if (
            value.length > maxPreviewBytes ||
            !value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)
        )
            throw Error('Invalid preview frame');
        buffer = Uint8Array.from(value).buffer;
    } else if (value instanceof ArrayBuffer) {
        buffer = value;
    } else {
        if (typeof value?.image !== 'string') throw Error('Invalid preview frame');
        return value;
    }
    if (buffer.byteLength < 5 || buffer.byteLength > maxPreviewBytes)
        throw Error('Invalid preview frame');
    const length = new DataView(buffer).getUint32(0, true);
    if (length > 4096 || length + 4 >= buffer.byteLength) throw Error('Invalid preview metadata');
    const metadata = JSON.parse(new TextDecoder().decode(new Uint8Array(buffer, 4, length)));
    if (!['image/png', 'image/jpeg'].includes(metadata.mime))
        throw Error('Invalid preview image type');
    return {
        ...metadata,
        image: URL.createObjectURL(new Blob([buffer.slice(4 + length)], { type: metadata.mime })),
    };
}
// Release the temporary object URL used by a media preview
export function releaseMediaPreview(image: string) {
    if (image.startsWith('blob:')) URL.revokeObjectURL(image);
}

/** Keep old pixels until the complete batch is decoded and its scene is still current. */
export async function publishDecodedPreview(
    urls: string[],
    current: () => boolean,
    publish: () => void,
    decode?: (url: string) => Promise<void>,
    paint: () => Promise<void> = () =>
        new Promise((resolve) => requestAnimationFrame(() => resolve())),
    decoded?: Set<string>,
) {
    const fresh = [...new Set(urls)].filter((url) => !decoded?.has(url));
    if (!(await decodeMediaImages(fresh, current, decode))) return false;
    if (!current()) return false;
    await paint();
    if (!current()) return false;
    publish();
    fresh.forEach((url) => decoded?.add(url));
    return true;
}
