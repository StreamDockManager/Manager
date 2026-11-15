import { publishDecodedPreview } from './media-preview';
import { previewCadence } from './preview-cadence';
import type { PreviewDelta } from '../generated/runtime';

interface SurfaceSubscriptionHost {
    channel: { onmessage: (packet: ArrayBuffer) => void };
    invoke: <T>(command: string, args: Record<string, unknown>) => Promise<T>;
    run: (label: string, work: () => Promise<void>) => Promise<unknown>;
    quiescing: () => boolean;
}

/** Each subscription owns its URLs, byte accounting and deferred revocations together. */
export class PreviewImageRegistry {
    private entries = new Map<string, { url: string; byteLength: number }>();
    private retired = new Set<string>();
    private bytes = 0;
    // Derive resident bytes from the current surface subscription state
    get residentBytes() {
        return this.bytes;
    }
    // Derive size from the current surface subscription state
    get size() {
        return this.entries.size;
    }
    // Read the requested surface subscription value
    get(token: string) {
        return this.entries.get(token)?.url;
    }
    // Register immutable preview bytes as a blob URL within entry and byte budgets
    insert(token: string, bytes: ArrayBuffer, mime: string) {
        // A token identifies immutable bytes for the life of its registration
        if (this.entries.has(token)) return;
        if (this.bytes + bytes.byteLength > 32 * 1024 * 1024 || this.entries.size >= 128)
            throw Error('Preview image registry exceeds limits');
        const url = URL.createObjectURL(new Blob([bytes], { type: mime }));
        this.entries.set(token, { url, byteLength: bytes.byteLength });
        this.bytes += bytes.byteLength;
    }
    // Retain the surface subscription operation
    retain(visible: ReadonlySet<string>, decoded: Set<string>) {
        for (const [token, entry] of this.entries) {
            if (visible.has(token)) continue;
            this.entries.delete(token);
            this.bytes -= entry.byteLength;
            decoded.delete(entry.url);
            this.retired.add(entry.url);
            requestAnimationFrame(() => this.release(entry.url));
        }
    }
    // Release resources retained by the surface subscription operation
    private release(url: string) {
        if (this.retired.delete(url)) URL.revokeObjectURL(url);
    }
    // Release surface subscription state and associated lifecycle resources
    dispose() {
        for (const entry of this.entries.values()) this.retired.add(entry.url);
        this.entries.clear();
        this.bytes = 0;
        for (const url of this.retired) this.release(url);
    }
}

// Subscribe the native surfaces
export async function subscribeNativeSurfaces(
    deviceId: string,
    listener: (frame: PreviewDelta) => void,
    { channel, invoke, run, quiescing }: SurfaceSubscriptionHost,
) {
    const images = new PreviewImageRegistry();
    const decoded = new Set<string>();
    const surfaces = new Map<string, string>();
    let background: string | null = null;
    let stopped = false;
    let id = '';
    let pendingAck = false;
    const cadence = previewCadence(
        // Complete the deferred surface subscription work using the current operation state
        () => {
            if (stopped || quiescing()) return;
            if (id) void invoke('acknowledge_surface_frame', { id }).catch(() => {});
            else pendingAck = true;
        },
        () => typeof document === 'undefined' || document.hasFocus(),
        typeof window === 'undefined' ? new EventTarget() : window,
    );
    // Complete the deferred surface subscription work using the current operation state
    channel.onmessage = (packet) => {
        if (stopped || quiescing()) return;
        // Complete the deferred surface subscription work using the current operation state
        void run('preview decode', async () => {
            try {
                if (packet.byteLength > 32 * 1024 * 1024 || packet.byteLength < 4)
                    throw Error('Preview packet exceeds limits');
                const size = new DataView(packet).getUint32(0, true);
                if (size > 65536 || size + 4 > packet.byteLength)
                    throw Error('Invalid preview metadata');
                const metadata = JSON.parse(
                    new TextDecoder().decode(new Uint8Array(packet, 4, size)),
                );
                const delta = metadata.delta as PreviewDelta;
                let offset = 4 + size;
                for (const frame of metadata.frames as {
                    id: string;
                    length: number;
                    mime: string;
                }[]) {
                    if (
                        !Number.isSafeInteger(frame.length) ||
                        frame.length < 1 ||
                        offset + frame.length > packet.byteLength ||
                        !['image/png', 'image/jpeg'].includes(frame.mime)
                    )
                        throw Error('Invalid preview image');
                    images.insert(
                        frame.id,
                        packet.slice(offset, offset + frame.length),
                        frame.mime,
                    );
                    offset += frame.length;
                }
                if (offset !== packet.byteLength) throw Error('Trailing preview data');
                if (delta.reset) {
                    surfaces.clear();
                    background = null;
                }
                for (const [surface, token] of Object.entries(delta.surfaces)) {
                    if (token) surfaces.set(surface, token);
                    else surfaces.delete(surface);
                }
                if (delta.background_changed) background = delta.background;
                const visible = new Set([
                    ...surfaces.values(),
                    ...(background ? [background] : []),
                ]);
                // Convert each entry into the representation required by surface subscription
                const urls = [...visible].map((token) => {
                    const url = images.get(token);
                    if (!url) throw Error('Missing preview image');
                    return url;
                });
                const published = await publishDecodedPreview(
                    urls,
                    () => !stopped,
                    // Complete the deferred surface subscription work using the current operation state
                    () => {
                        listener({
                            ...delta,
                            surfaces: Object.fromEntries(
                                Object.entries(delta.surfaces).map(([key, token]) => [
                                    key,
                                    token ? images.get(token) || null : null,
                                ]),
                            ),
                            background: delta.background
                                ? images.get(delta.background) || null
                                : null,
                        });
                    },
                    undefined,
                    undefined,
                    decoded,
                );
                if (!published) return;
                images.retain(visible, decoded);
            } catch {
                // Preserve the previous visible pixels on malformed/undecodable frames
                // Renewing the subscription requests a complete scene
                if (!stopped)
                    listener({
                        generation: 'decode-error',
                        sequence: 0,
                        reset: false,
                        surfaces: {},
                        trigger_descriptions: {},
                        background: null,
                        background_changed: false,
                    });
            } finally {
                if (!stopped) cadence.ready();
            }
        }).catch(() => {});
    };
    // Release surface subscription state and associated lifecycle resources
    const dispose = () => {
        stopped = true;
        cadence.dispose();
        images.dispose();
        decoded.clear();
        surfaces.clear();
        channel.onmessage = () => {};
    };
    try {
        id = await invoke<string>('subscribe_surfaces', { deviceId, onFrame: channel });
    } catch (error) {
        dispose();
        throw error;
    }
    if (pendingAck && !stopped) void invoke('acknowledge_surface_frame', { id }).catch(() => {});
    // Complete the deferred surface subscription work using the current operation state
    return () => {
        if (stopped) return;
        dispose();
        return invoke<void>('unsubscribe_surfaces', { id });
    };
}
