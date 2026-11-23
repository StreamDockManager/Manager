import type { Binding } from './types';
import devicePalettes from './device-palettes.v1.json';
import { defaultAppearance } from './feature-defaults';

// Browser fixture rendering uses the same device/profile palette selection as
// device wallpaper. Keep saved media and RGB untouched when resolving a frame
export function resolvePaletteBinding(binding: Binding): Binding {
    if (binding.background_source !== 'palette') return binding;
    const theme = window.Alpine.store('theme') as ReturnType<
        typeof import('./theme').registerTheme
    >;
    const deviceAppearance = theme.deviceAppearance;
    const mode =
        deviceAppearance.preference === 'system'
            ? theme.systemDark
                ? 'dark'
                : 'light'
            : deviceAppearance.preference;
    const id =
        mode === 'light' ? deviceAppearance.style.lightPalette : deviceAppearance.style.darkPalette;
    const presets = devicePalettes.palettes[mode] as Record<
        string,
        { surface: string; ink: string }
    >;
    const colors = {
        ...(presets[id] || presets.default),
        ...theme.customPalettes.find((palette) => palette.id === id && palette.mode === mode)
            ?.colors,
        ...(theme.palettePreview?.draft &&
        theme.palettePreview.mode === mode &&
        theme.palettePreview.id === id
            ? theme.palettePreview.draft.colors
            : {}),
    };
    const appearance =
        binding.action.kind !== 'plugin' &&
        !binding.appearance &&
        !binding.composition &&
        binding.title
            ? { ...defaultAppearance(), foreground: colors.ink }
            : binding.appearance;
    return {
        ...binding,
        color: colors.surface,
        custom_background: true,
        asset: undefined,
        appearance,
    };
}
