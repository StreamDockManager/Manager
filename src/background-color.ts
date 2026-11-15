// Keep the selected RGB color when toggling transparency. Portable bindings and
// presets store alpha as #RRGGBBAA; existing six-digit colors remain opaque
export const DEFAULT_BACKGROUND = '#c61e3200';

// Recognize an uncustomized plugin-default background
export function defaultPluginBackground(color: string, custom: boolean): boolean {
    return !custom && [DEFAULT_BACKGROUND, '#00000000'].includes(color.toLowerCase());
}

// Extract the RGB portion of a background color
export function backgroundRgb(color: string): string {
    return /^#[\da-f]{6}([\da-f]{2})?$/i.test(color) ? color.slice(0, 7) : '#c61e32';
}
// Recognize a background with non-opaque alpha
export function transparentBackground(color: string): boolean {
    return /^#[\da-f]{8}$/i.test(color) && color.slice(7).toLowerCase() !== 'ff';
}
// Replace background RGB while retaining its alpha
export function withBackgroundColor(color: string, rgb: string): string {
    return backgroundRgb(rgb) + (color.length === 9 ? color.slice(7) : '');
}
// Set background opacity without changing its RGB value
export function withBackgroundTransparency(color: string, transparent: boolean): string {
    return backgroundRgb(color) + (transparent ? '00' : '');
}

export interface BackgroundDraft {
    color: string;
    customBackground: boolean;
    backgroundSource: 'custom' | 'palette';
}
// Determine whether the draft uses the plugin’s background
export function usesPluginBackground(
    draft: BackgroundDraft,
    kind: string,
    asset: string | undefined,
): boolean {
    return (
        draft.backgroundSource !== 'palette' &&
        kind === 'plugin' &&
        !asset &&
        defaultPluginBackground(draft.color, draft.customBackground)
    );
}
// Resolve transparency after accounting for the plugin default
export function showsTransparentBackground(draft: BackgroundDraft, usesDefault: boolean): boolean {
    return (
        draft.backgroundSource !== 'palette' && !usesDefault && transparentBackground(draft.color)
    );
}
// Apply an RGB edit to the draft background
export function changeBackgroundRgb(
    draft: BackgroundDraft,
    rgb: string,
    usesDefault: boolean,
): Pick<BackgroundDraft, 'color' | 'customBackground'> {
    return {
        customBackground: true,
        color: withBackgroundColor(
            usesDefault ? withBackgroundTransparency(draft.color, false) : draft.color,
            rgb,
        ),
    };
}
// Apply a transparency edit to the draft background
export function changeBackgroundAlpha(
    draft: BackgroundDraft,
    transparent: boolean,
): Pick<BackgroundDraft, 'color' | 'customBackground'> {
    return { customBackground: true, color: withBackgroundTransparency(draft.color, transparent) };
}
