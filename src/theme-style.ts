import type { CustomPalette, ProfilePalette, UiThemeStyle } from '../generated/runtime';
import { cssFontFamily, fontFamily } from './display-fonts';
import palette from './palette.v1.json';
import devicePalettes from './device-palettes.v1.json';
import { syncWindowBackground } from './window-background';

export type ThemeMode = 'light' | 'dark';
export type ThemePreference = ThemeMode | 'system';
export const modeKey = 'stream-dock-manager.appearance';
export const styleKey = 'stream-dock-manager.theme-style.v1';
export const defaultStyle: UiThemeStyle = {
    lightPalette: 'default',
    darkPalette: 'default',
    font: 'default',
    textScale: 100,
};

// Apply profile palettes only to the selected appearance target and normalize the resulting style
export function profileAppearance(
    preference: ThemePreference,
    style: UiThemeStyle,
    profile: ProfilePalette | null | undefined,
    target: 'manager' | 'device' = 'manager',
    custom: CustomPalette[] = [],
) {
    const synced = profile?.target === target || profile?.target === 'both';
    return {
        preference: synced ? ('system' as const) : preference,
        style: synced
            ? normalizeStyle(
                  {
                      ...style,
                      lightPalette: profile.light_palette,
                      darkPalette: profile.dark_palette,
                  },
                  custom,
              )
            : style,
    };
}
type PaletteOption = { id: string; label: string; swatch?: Record<string, string> };
const schemes = Object.entries(palette.managerSchemes);
// List built-in and custom palettes compatible with the selected light or dark mode
export function palettesFor(mode: ThemeMode, custom: CustomPalette[] = []): PaletteOption[] {
    return [
        { id: 'default', label: 'Default' },
        ...schemes
            .filter(([, scheme]) => scheme.mode === mode)
            .map(([id, scheme]) => ({ id, label: scheme.label })),
        ...custom
            .filter((palette) => palette.mode === mode)
            .map((palette) => ({
                id: palette.id,
                label: palette.name,
                swatch: Object.fromEntries(
                    ['canvas', 'surface', 'surface-raised', 'border', 'accent'].map((key) => [
                        '--' + key,
                        palette.colors[key] || paletteColors(mode, 'default')[key],
                    ]),
                ),
            })),
    ];
}
// Combine valid theme color tokens with the selected device meter color
export function paletteColors(mode: ThemeMode, id: string): Record<string, string> {
    const source = {
        ...palette.themes[mode],
        ...schemes.find(([name, scheme]) => name === id && scheme.mode === mode)?.[1].tokens,
    };
    const presets: Record<string, { meter: string }> = devicePalettes.palettes[mode];
    return {
        ...Object.fromEntries(
            Object.entries(source).filter(([, value]) => /^#[\da-f]{6}$/i.test(value)),
        ),
        meter: (presets[id] || presets.default).meter,
    };
}
// Normalize unknown theme preferences to system-following mode
export const themePreference = (value: unknown): ThemePreference =>
    value === 'light' || value === 'dark' ? value : 'system';

// Normalize the style
export function normalizeStyle(
    style?: Partial<UiThemeStyle> | null,
    custom: CustomPalette[] = [],
): UiThemeStyle {
    const font = style?.font;
    const scale = style?.textScale;
    const darkPalette = style?.darkPalette;
    return {
        lightPalette:
            palettesFor('light', custom).find(({ id }) => id === style?.lightPalette)?.id ||
            'default',
        darkPalette:
            palettesFor('dark', custom).find(({ id }) => id === darkPalette)?.id || 'default',
        font:
            typeof font === 'string' &&
            (font === 'sans' || font === 'mono' || font === 'system:' + fontFamily(font))
                ? font
                : 'default',
        textScale:
            typeof scale === 'number' && scale >= 90 && scale <= 150 && scale % 5 === 0
                ? scale
                : 100,
    };
}
// Read appearance for the theme style workflow
export function readAppearance() {
    let preference: ThemePreference = 'system';
    let style = { ...defaultStyle };
    try {
        preference = themePreference(localStorage.getItem(modeKey));
        style = normalizeStyle(JSON.parse(localStorage.getItem(styleKey) || 'null'));
    } catch {}
    return { preference, style };
}
// Cache appearance preferences locally without failing when browser storage is unavailable
export function cacheAppearance(key: string, value: ThemePreference | UiThemeStyle) {
    try {
        localStorage.setItem(key, typeof value === 'string' ? value : JSON.stringify(value));
    } catch {}
}
const appliedColors = new Set<string>();
// Apply the theme
export function applyTheme(
    preference: ThemePreference,
    systemDark: boolean,
    style: UiThemeStyle,
    custom: CustomPalette[] = [],
) {
    const resolved = preference === 'system' ? (systemDark ? 'dark' : 'light') : preference;
    const id = resolved === 'dark' ? style.darkPalette : style.lightPalette;
    const root = document.documentElement;
    root.dataset.theme = resolved;
    root.dataset.palette = id;
    root.style?.setProperty('--ui-text-scale', String(style.textScale / 100));
    if (style.font === 'default') root.style?.removeProperty('--ui-font');
    else root.style?.setProperty('--ui-font', cssFontFamily(style.font));
    for (const token of appliedColors) root.style.removeProperty(token);
    appliedColors.clear();
    const colors = custom.find((entry) => entry.id === id && entry.mode === resolved)?.colors;
    if (colors) {
        for (const [key, value] of Object.entries(colors)) {
            if (!(key in palette.themes[resolved]) || !value || !/^#[\da-f]{6}$/i.test(value))
                continue;
            root.style.setProperty('--' + key, value);
            appliedColors.add('--' + key);
        }
    }
    const scheme = schemes.find(([name]) => name === id)?.[1];
    const canvas = colors?.canvas || scheme?.tokens.canvas || palette.themes[resolved].canvas;
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', canvas);
    void syncWindowBackground(canvas);
    window.dispatchEvent(new CustomEvent('resolved-theme', { detail: { preference, resolved } }));
}
