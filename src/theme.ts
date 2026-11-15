import type AlpineType from '@alpinejs/csp';
import type {
    AppearancePreferences,
    CustomPalette,
    DeviceThemePreferences,
    PaletteWallpaper,
    ProfilePalette,
    UiThemeStyle,
} from '../generated/runtime';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { bridge } from './bridge';
import { fontChoices } from './display-fonts';
import {
    applyTheme,
    cacheAppearance,
    defaultStyle,
    modeKey,
    normalizeStyle,
    palettesFor,
    profileAppearance,
    readAppearance,
    styleKey,
    themePreference,
    type ThemeMode,
    type ThemePreference,
} from './theme-style';
import { windowWork } from './window-work';

type AppearanceEdit = {
    preference: ThemePreference;
    style?: UiThemeStyle;
    explicitMode?: boolean;
};
type PalettePreview = {
    owner: symbol;
    mode: ThemeMode;
    id: string;
    draft?: CustomPalette;
};

// Register the theme
export function registerTheme(Alpine: typeof AlpineType, native: boolean, persistNative = true) {
    const media = window.matchMedia('(prefers-color-scheme: dark)');
    const initial = readAppearance();
    let committedPreference = initial.preference;
    let committedStyle = initial.style;
    let failedEdit: AppearanceEdit | undefined;
    let appearanceWrite: Promise<void> | undefined;
    let editEpoch = 0;
    let readEpoch = 0;
    let disposed = false;
    let fontsRequest = 0;
    const store = {
        ...initial,
        scaleInput: initial.style.textScale,
        systemDark: media.matches,
        customPalettes: [] as CustomPalette[],
        profilePalette: null as ProfilePalette | null,
        profileContextBound: false,
        // Derive effective appearance from the current theme state
        get effectiveAppearance(): ReturnType<typeof profileAppearance> {
            return profileAppearance(
                this.preference,
                this.style,
                this.profilePalette,
                'manager',
                this.customPalettes,
            );
        },
        // Derive has profile palette from the current theme state
        get hasProfilePalette(): boolean {
            return (
                this.profilePalette?.target === 'manager' || this.profilePalette?.target === 'both'
            );
        },
        // Check whether active palette is valid for the current theme state
        isActivePalette(mode: ThemeMode, id: string): boolean {
            const style = this.effectiveAppearance.style;
            return (
                mode === this.resolved &&
                id === (mode === 'light' ? style.lightPalette : style.darkPalette)
            );
        },
        // Derive device appearance from the current theme state
        get deviceAppearance(): ReturnType<typeof profileAppearance> {
            return profileAppearance(
                this.deviceTheme.sync ? this.preference : 'dark',
                this.deviceTheme.sync
                    ? this.style
                    : { ...this.style, lightPalette: 'default', darkPalette: 'default' },
                this.profilePalette,
                'device',
                this.customPalettes,
            );
        },
        // Set the profile palette
        setProfilePalette(palette: ProfilePalette | null) {
            this.profileContextBound = true;
            this.profilePalette = palette;
            this.apply();
        },
        // The preview paints this document while preference and style remain saved values
        palettePreview: null as PalettePreview | null,
        deviceTheme: { sync: true, transparent_buttons: true } as DeviceThemePreferences,
        savingDeviceTheme: false,
        // Derive wallpaper from the current theme state
        get wallpaper(): PaletteWallpaper | null {
            const preview = this.palettePreview;
            if (preview?.draft) return preview.draft.wallpaper;
            const mode = preview?.mode ?? this.resolved;
            const id =
                preview?.id ??
                (mode === 'light'
                    ? this.effectiveAppearance.style.lightPalette
                    : this.effectiveAppearance.style.darkPalette);
            return (
                this.customPalettes.find((palette) => palette.id === id && palette.mode === mode)
                    ?.wallpaper || null
            );
        },
        // Derive palette groups from the current theme state
        get paletteGroups(): {
            name: 'lightPalette' | 'darkPalette';
            mode: ThemeMode;
            label: string;
            options: ReturnType<typeof palettesFor>;
        }[] {
            return [
                {
                    name: 'lightPalette',
                    mode: 'light',
                    label: 'Light palette',
                    options: palettesFor('light', this.customPalettes),
                },
                {
                    name: 'darkPalette',
                    mode: 'dark',
                    label: 'Dark palette',
                    options: palettesFor('dark', this.customPalettes),
                },
            ];
        },
        styleReady: !native,
        styleError: '',
        fonts: [] as string[],
        canDiscoverFonts: native,
        fontsLoading: false,
        fontsLoaded: !native,
        fontError: '',
        // Derive font options from the current theme state
        get fontOptions(): ReturnType<typeof fontChoices> {
            return [
                { id: 'default', label: 'Default' },
                ...fontChoices(
                    this.fonts,
                    this.style.font === 'default' ? [] : [this.style.font],
                    this.fontsLoaded,
                ),
            ];
        },
        // Derive resolved from the current theme state
        get resolved(): 'light' | 'dark' {
            return this.effectiveAppearance.preference === 'system'
                ? this.systemDark
                    ? 'dark'
                    : 'light'
                : this.effectiveAppearance.preference;
        },
        // Derive follows system from the current theme state
        get followsSystem(): boolean {
            return this.preference === 'system';
        },
        // Change the theme operation
        change(event: Event) {
            this.set((event.currentTarget as HTMLSelectElement).value);
        },
        // Replace the requested theme value
        set(value: string) {
            this.saveAppearance({
                preference: themePreference(value),
                style: this.styleReady ? this.style : undefined,
                explicitMode: true,
            });
        },
        // Select the palette
        selectPalette(mode: ThemeMode, id: string) {
            const key = mode === 'light' ? 'lightPalette' : 'darkPalette';
            this.saveStyle({ ...this.style, [key]: id }, this.followsSystem ? 'system' : mode);
        },
        // Show the palette preview
        showPalettePreview(owner: symbol, mode: ThemeMode, id: string, draft?: CustomPalette) {
            this.palettePreview = { owner, mode, id, draft };
            this.applyColors();
        },
        // Clear the palette preview
        clearPalettePreview(owner: symbol) {
            if (this.palettePreview?.owner !== owner) return;
            this.palettePreview = null;
            this.applyColors();
        },
        // Change the style
        changeStyle(event: Event) {
            const input = event.currentTarget as HTMLInputElement | HTMLSelectElement;
            const key = input.name;
            if (key === 'font') this.saveStyle({ ...this.style, font: input.value });
            if (key === 'textScale')
                this.saveStyle({ ...this.style, textScale: Number(input.value) });
        },
        // Update the text-scale draft from the slider input
        previewScale(event: Event) {
            this.scaleInput = Number((event.currentTarget as HTMLInputElement).value);
        },
        // Reset the style
        resetStyle() {
            this.saveStyle(defaultStyle);
        },
        // Retry the style
        retryStyle() {
            if (failedEdit) this.saveAppearance(failedEdit);
            else void load();
        },
        // Save the style
        saveStyle(style: UiThemeStyle, preference?: ThemePreference) {
            if (!this.styleReady) return;
            this.saveAppearance({
                preference: preference ?? this.preference,
                style: normalizeStyle(style, this.customPalettes),
            });
        },
        // Save the appearance
        saveAppearance(edit: AppearanceEdit) {
            if (windowWork.quiescing) return;
            const { preference, style, explicitMode } = edit;
            const epoch = ++editEpoch;
            this.styleError = '';
            failedEdit = undefined;
            const modeChanged = preference !== this.preference;
            this.preference = preference;
            if (style) {
                this.style = style;
                this.scaleInput = style.textScale;
            }
            if (modeChanged || explicitMode) this.apply();
            else this.applyColors();
            if (!native) {
                if (modeChanged || explicitMode) cacheAppearance(modeKey, preference);
                if (style) cacheAppearance(styleKey, style);
                return;
            }
            if (!persistNative) return;
            // Save pending changes through the theme workflow
            const save = async () => {
                const writeMode = explicitMode || preference !== committedPreference;
                const writeStyle =
                    style &&
                    (style.lightPalette !== committedStyle.lightPalette ||
                        style.darkPalette !== committedStyle.darkPalette ||
                        style.font !== committedStyle.font ||
                        style.textScale !== committedStyle.textScale);
                if (!writeMode && !writeStyle) return;
                try {
                    const snapshot = await bridge.command('set_appearance', {
                        ...(writeMode ? { preference } : {}),
                        ...(writeStyle ? { style } : {}),
                    });
                    if (writeStyle && style) {
                        committedStyle = style;
                        cacheAppearance(styleKey, style);
                    }
                    if (writeMode) {
                        committedPreference = preference;
                        cacheAppearance(modeKey, preference);
                        window.dispatchEvent(
                            new CustomEvent('plugin-theme-metadata', { detail: snapshot }),
                        );
                    }
                } catch (error) {
                    if (epoch !== editEpoch || disposed) return;
                    failedEdit = edit;
                    this.preference = committedPreference;
                    this.style = committedStyle;
                    this.scaleInput = committedStyle.textScale;
                    this.styleError = 'Could not save theme customization. Try again.';
                    this.apply();
                    if (writeStyle) console.error('Interface theme could not be saved:', error);
                    else console.error(error);
                }
            };
            const pending = appearanceWrite ? appearanceWrite.then(save) : save();
            appearanceWrite = pending;
            void pending.finally(() => {
                if (appearanceWrite === pending) appearanceWrite = undefined;
            });
        },
        // Load fonts for the theme workflow
        async loadFonts(refresh = false) {
            if (!native || (!refresh && (this.fontsLoading || this.fontsLoaded))) return;
            const request = ++fontsRequest;
            this.fontsLoading = true;
            this.fontError = '';
            try {
                const fonts = await bridge.command('system_fonts', { refresh });
                if (request !== fontsRequest || disposed) return;
                this.fonts = fonts;
                this.fontsLoaded = true;
            } catch {
                if (request === fontsRequest && !disposed)
                    this.fontError = 'Could not load installed fonts.';
            } finally {
                if (request === fontsRequest) this.fontsLoading = false;
            }
        },
        // Accept the palettes
        acceptPalettes(value: AppearancePreferences) {
            this.customPalettes = value.custom_palettes || [];
            this.deviceTheme = value.device_theme || this.deviceTheme;
            committedStyle = normalizeStyle(value.style, this.customPalettes);
            this.style = committedStyle;
            this.scaleInput = committedStyle.textScale;
            cacheAppearance(styleKey, committedStyle);
            this.apply();
        },
        // Change the device theme
        async changeDeviceTheme(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const key = input.name === 'sync' ? 'sync' : 'transparent_buttons';
            if (this.savingDeviceTheme) return;
            const preferences = { ...this.deviceTheme, [key]: input.checked };
            this.savingDeviceTheme = true;
            this.styleError = '';
            try {
                this.deviceTheme = native
                    ? await bridge.command('set_device_theme_preferences', { preferences })
                    : preferences;
            } catch {
                input.checked = this.deviceTheme[key];
                this.styleError = 'Could not save device theme preferences.';
            } finally {
                this.savingDeviceTheme = false;
            }
        },
        // Apply the colors
        applyColors() {
            const preview = this.palettePreview;
            if (preview) {
                const key = preview.mode === 'light' ? 'lightPalette' : 'darkPalette';
                applyTheme(
                    preview.mode,
                    this.systemDark,
                    { ...this.style, [key]: preview.id },
                    preview.draft ? [{ ...preview.draft, id: preview.id }] : this.customPalettes,
                );
            } else {
                const appearance = this.effectiveAppearance;
                applyTheme(
                    appearance.preference,
                    this.systemDark,
                    appearance.style,
                    this.customPalettes,
                );
            }
        },
        // Apply the pending theme changes
        apply() {
            this.applyColors();
            if (native && !windowWork.quiescing) {
                void windowWork
                    .run('window appearance', () =>
                        getCurrentWindow().setTheme(
                            this.effectiveAppearance.preference === 'system'
                                ? null
                                : this.effectiveAppearance.preference,
                        ),
                    )
                    .catch((error) => console.error('Could not update window appearance:', error));
            }
        },
    };
    Alpine.store('theme', store);
    // Resolve the current theme appearance
    const theme = () => Alpine.store('theme') as typeof store;
    // Load the data required by the theme workflow
    const load = async () => {
        const request = ++readEpoch;
        const readEdit = editEpoch;
        const allowEdit = !appearanceWrite;
        try {
            const value = await bridge.command('appearance_preferences');
            if (disposed || request !== readEpoch) return;
            const canApply = allowEdit && readEdit === editEpoch;
            if (canApply) {
                committedPreference = themePreference(value.preference);
                theme().preference = committedPreference;
                cacheAppearance(modeKey, committedPreference);
            }
            if (canApply || !theme().styleReady) {
                theme().customPalettes = value.custom_palettes || [];
                theme().deviceTheme = value.device_theme || theme().deviceTheme;
                committedStyle = normalizeStyle(value.style, theme().customPalettes);
                theme().style = committedStyle;
                theme().scaleInput = committedStyle.textScale;
                if (value.style) cacheAppearance(styleKey, committedStyle);
                theme().styleReady = true;
                if (!failedEdit) theme().styleError = '';
            }
            theme().systemDark = value.system_dark;
            if (!theme().profileContextBound)
                theme().profilePalette = value.profile_palette || null;
            theme().apply();
        } catch (error) {
            if (disposed || request !== readEpoch) return;
            if (!theme().styleReady) theme().styleError = 'Could not load theme customization.';
            console.error('Appearance preferences could not be loaded:', error);
        }
    };
    if (native) void load();
    theme().apply();
    // Apply changed system dark-mode preference to the current theme
    const systemChanged = (event: MediaQueryListEvent) => {
        theme().systemDark = event.matches;
        theme().applyColors();
    };
    // Reload appearance preferences after relevant cross-window storage changes
    const stored = (event: StorageEvent) => {
        if (event.key !== modeKey && event.key !== styleKey && event.key !== null) return;
        if (native) {
            void load();
            return;
        }
        const cached = readAppearance();
        theme().preference = cached.preference;
        theme().style = cached.style;
        theme().scaleInput = cached.style.textScale;
        theme().apply();
    };
    // Reapply the current theme when the window resumes
    const resumed = () => theme().apply();
    // Reload appearance after a native settings change
    const nativeChanged = () => void load();
    window.addEventListener('editor-window-resumed', resumed);
    window.addEventListener('ui-appearance-changed', nativeChanged);
    media.addEventListener('change', systemChanged);
    window.addEventListener('storage', stored);
    window.addEventListener(
        'pagehide',
        // Respond to pagehide events in the theme workflow
        () => {
            disposed = true;
            window.removeEventListener('editor-window-resumed', resumed);
            window.removeEventListener('ui-appearance-changed', nativeChanged);
            media.removeEventListener('change', systemChanged);
            window.removeEventListener('storage', stored);
        },
        { once: true },
    );
    return theme();
}
