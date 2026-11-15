/** Binary wallpaper reply: little-endian JSON length, metadata, optional encoded pixels. */
export function readWallpaperFrame(value: ArrayBuffer | number[]) {
    const maximum = 8 * 1024 * 1024;
    if (Array.isArray(value)) {
        if (
            value.length > maximum ||
            !value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)
        )
            throw Error('Invalid wallpaper frame');
        value = Uint8Array.from(value).buffer;
    }
    if (!(value instanceof ArrayBuffer) || value.byteLength < 4 || value.byteLength > maximum)
        throw Error('Invalid wallpaper frame');
    const length = new DataView(value).getUint32(0, true);
    if (length === 0 || length > 4096 || 4 + length > value.byteLength)
        throw Error('Invalid wallpaper metadata');
    const metadata = JSON.parse(new TextDecoder().decode(new Uint8Array(value, 4, length)));
    if (
        !metadata ||
        typeof metadata.token !== 'string' ||
        !/^[0-9a-f]{64}$/.test(metadata.token) ||
        (metadata.next_ms !== null &&
            (!Number.isInteger(metadata.next_ms) ||
                metadata.next_ms < 67 ||
                metadata.next_ms > 120000))
    )
        throw Error('Invalid wallpaper metadata');
    const pixels = new Uint8Array(value, 4 + length);
    if (
        pixels.length
            ? !['image/png', 'image/jpeg'].includes(metadata.mime)
            : metadata.mime !== null
    )
        throw Error('Invalid wallpaper image type');
    return {
        token: metadata.token as string,
        nextMs: metadata.next_ms as number | null,
        mime: metadata.mime as 'image/png' | 'image/jpeg' | null,
        pixels,
    };
}

/** Account for IPC time without increasing the previous 15-fps work ceiling. */
export function wallpaperDelay(nextMs: number, requestStarted: number, now: number) {
    return Math.max(67, nextMs - Math.max(0, now - requestStarted));
}
