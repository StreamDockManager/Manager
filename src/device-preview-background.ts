/** A configured page background wins over palette browsing. Native streamed pixels
 * remain authoritative otherwise, except while a palette candidate's default wallpaper
 * must replace the previous default before the next frame arrives.
 */
export function devicePreviewBackground(options: {
    native: boolean;
    customPage: boolean;
    preview: boolean;
    paletteAsset: boolean;
    streamed: string | null;
    palette: string;
}) {
    if (options.customPage) return options.streamed;
    if (options.preview && !options.paletteAsset) return options.palette;
    if (options.streamed) return options.streamed;
    // A native scene can omit a separate default wallpaper frame. That is not an
    // instruction to paint flat chrome; only an unresolved custom asset stays empty.
    if (options.native && options.paletteAsset) return null;
    return options.palette;
}

/** Wallpaper belongs to image-bearing display apertures, not chassis or lighting.
 * A separate full-plane background endpoint is not required for individual LCDs.
 */
export function isWallpaperAperture(control: {
    kind: string;
    image_slot?: number | null;
    image_width?: number | null;
    image_height?: number | null;
    preview_hidden?: boolean;
}) {
    return (
        ['button', 'touch', 'display'].includes(control.kind) &&
        control.image_slot != null &&
        Boolean(control.image_width && control.image_height) &&
        !control.preview_hidden
    );
}
