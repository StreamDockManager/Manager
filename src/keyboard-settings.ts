import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { KeyboardSettings } from '../generated/runtime';
import { ownWindowMethods } from './window-work';
import { registerCloseDraft, trackCloseRelease } from './close-releases';

let nextKeyboardView = 0;

// Convert an optional background color into keyboard RGB data
export function backgroundFromHex(hex: string) {
    if (!/^#[0-9a-f]{6}$/i.test(hex)) throw Error('Choose a background color as #RRGGBB.');
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
    const max = Math.max(r, g, b),
        min = Math.min(r, g, b),
        delta = max - min;
    const hue =
        delta === 0
            ? 0
            : max === r
              ? ((g - b) / delta) % 6
              : max === g
                ? (b - r) / delta + 2
                : (r - g) / delta + 4;
    return {
        hue: Math.round((hue * 60 + 360) % 360) % 360,
        saturation: max === 0 ? 0 : Math.round((delta / max) * 100),
    };
}
// Format an optional keyboard RGB background as hexadecimal text
export function backgroundToHex(color: { hue: number; saturation: number }) {
    const h = color.hue / 60,
        s = color.saturation / 100,
        x = s * (1 - Math.abs((h % 2) - 1));
    const channels =
        h < 1
            ? [s, x, 0]
            : h < 2
              ? [x, s, 0]
              : h < 3
                ? [0, s, x]
                : h < 4
                  ? [0, x, s]
                  : h < 5
                    ? [x, 0, s]
                    : [s, 0, x];
    return (
        '#' +
        channels
            .map((v) =>
                Math.round((v + 1 - s) * 255)
                    .toString(16)
                    .padStart(2, '0'),
            )
            .join('')
    );
}

/** Controls show saved app preferences, never assumed hardware readback */
export function createKeyboardSettings(app: () => AppState) {
    const prefix = `keyboard-settings-${++nextKeyboardView}`;
    let queue: Promise<unknown> = Promise.resolve();
    let discardingTask: Promise<void> | null = null;
    // Serialize keyboard hardware operations through the shared request queue
    const enqueue = <T>(run: () => Promise<T>): Promise<T> => {
        const next = queue.then(run, run);
        queue = next.catch(() => {});
        return next;
    };
    return {
        osModeId: prefix + '-os-mode',
        colorHelpId: prefix + '-color-help',
        colorId: prefix + '-color',
        backgroundColorId: prefix + '-background-color',
        effectsId: prefix + '-effects',
        effectsHelpId: prefix + '-effects-help',
        speedId: prefix + '-speed',
        speedHelpId: prefix + '-speed-help',
        sleepTimeoutId: prefix + '-sleep-timeout',
        sleepTimeout: '',
        observation: null as null | {
            settings: KeyboardSettings | null;
            supported: boolean;
            state: string;
            message: string;
        },
        readbackGeneration: 0,
        useSavedPreferences: false,
        customColorKnown: true,
        resetConfirmIdentity: '',
        initialSleepTimeout: '',
        brightness: '',
        color: '',
        customColor: true,
        initialCustomColor: true,
        backgroundColor: '',
        initialBackgroundColor: '',
        previewId: null as string | null,
        previewedSettings: null as KeyboardSettings | null,
        previewDeviceId: '',
        previewRevision: 0,
        previewing: false,
        discarding: false,
        previewMessage: '',
        effect: '',
        speed: '',
        osMode: 'auto',
        initialOsMode: 'auto',
        initialBrightness: '',
        initialColor: '',
        initialEffect: '',
        initialSpeed: '',
        forceReapply: false,
        statusDetail: '',
        osModeTouched: false,
        identity: '',
        epoch: 0,
        saving: false,
        error: '',
        status: '',
        // Identify the active device and its current keyboard capability contract
        get contractIdentity() {
            return JSON.stringify([
                app().deviceIdentity,
                app().device?.variant_id,
                app().device?.manifest.capabilities.keyboard,
            ]);
        },
        // Check whether the device declares a supported factory reset command
        get hasFactoryReset() {
            return Boolean(this.capabilities?.factory_reset);
        },
        // Require a connected idle compatible device before factory reset
        get canFactoryReset() {
            return this.hasFactoryReset && !this.unavailableReason && !this.previewing;
        },
        // Determine whether the current baseline comes from device readback
        get usesReadback() {
            return Boolean(this.observation?.supported && !this.useSavedPreferences);
        },
        // Select the baseline settings without treating saved preferences as readback
        get baselineSettings() {
            return this.usesReadback
                ? (this.observation?.settings ?? undefined)
                : app().deviceOutput.keyboard;
        },
        // Explain whether the shown settings came from the device or saved preferences
        get valuesSource() {
            return this.useSavedPreferences
                ? 'Saved preferences selected for reapply; not device readback.'
                : this.observation?.message || 'Saved preferences, not live device values.';
        },
        // Describe known, disabled or unreported custom-color state
        get customColorLabel() {
            return !this.customColorKnown
                ? 'Not reported'
                : this.customColor
                  ? 'Custom palette'
                  : 'Rainbow palette';
        },
        // Read keyboard settings without accepting stale device responses
        async refreshDeviceSettings() {
            const generation = ++this.readbackGeneration;
            const identity = this.contractIdentity;
            const deviceId = app().deviceId;
            if (!this.supported || !this.connected) return;
            try {
                const observation = await app().bridge.command('keyboard_device_settings', {
                    deviceId,
                });
                if (generation !== this.readbackGeneration || identity !== this.contractIdentity)
                    return;
                const editable = !this.dirty && !this.saving && !this.previewId && !this.previewing;
                this.observation = observation;
                if (editable && !this.useSavedPreferences) this.reset();
            } catch (error) {
                if (generation === this.readbackGeneration) this.statusDetail = String(error);
            }
        },
        // Reset the confirmed keyboard and refresh its reported settings
        async factoryReset() {
            if (!this.canFactoryReset || this.resetConfirmIdentity !== this.contractIdentity)
                return;
            const deviceId = app().deviceId;
            const identity = this.contractIdentity;
            this.resetConfirmIdentity = '';
            ++this.previewRevision;
            this.saving = true;
            this.error = '';
            try {
                // Release preview ownership before resetting the physical keyboard
                const result = await enqueue(async () => {
                    await this.releasePreview();
                    return app().bridge.command('factory_reset_keyboard', { deviceId });
                });
                if (identity !== this.contractIdentity) return;
                app().apply(result.snapshot);
                this.useSavedPreferences = false;
                this.observation = {
                    supported: true,
                    settings: null,
                    state: 'waiting',
                    message:
                        'Waiting for settings reported by the keyboard. Unreported values remain unknown.',
                };
                this.reset();
                await this.refreshDeviceSettings();
                this.reset();
                this.status = result.message;
                if (result.status === 'failed' || result.status === 'uncertain')
                    this.error = result.message;
            } catch (error) {
                if (identity === this.contractIdentity) this.error = String(error);
            } finally {
                this.saving = false;
            }
        },
        // Read the selected device keyboard capabilities
        get capabilities() {
            return app().device?.manifest.capabilities.keyboard;
        },
        // Check whether the selected device is connected
        get connected() {
            return Boolean(app().device?.connected);
        },
        // Check whether the keyboard exposes global RGB control
        get hasGlobalRgb() {
            return Boolean(this.capabilities?.global_rgb);
        },
        // Check whether the keyboard declares an effect range
        get hasEffects() {
            return this.capabilities?.effect_max != null;
        },
        // Check whether the keyboard declares an effect-speed range
        get hasSpeed() {
            return this.capabilities?.speed_max != null;
        },
        // Build effect choices from the device capability range
        get effectOptions() {
            return Array.from(
                { length: (this.capabilities?.effect_max ?? -1) + 1 },
                (_, value) => ({
                    value: String(value),
                    label: this.capabilities?.effect_names?.[value]
                        ? value === 14
                            ? this.capabilities.effect_names[value] + ' (background)'
                            : this.capabilities.effect_names[value]
                        : value === 0
                          ? 'Static'
                          : `Effect ${value}`,
                }),
            );
        },
        // Check whether any keyboard effects accept a background color
        get supportsBackgroundColor() {
            return Boolean(this.capabilities?.background_color_effects?.length);
        },
        // Describe keyboard availability or the latest operation result
        get feedback() {
            return (
                this.unavailableReason ||
                app().deviceUnavailableReason ||
                this.previewMessage ||
                this.status ||
                this.observation?.message ||
                'Preview changes, then Apply to save.'
            );
        },
        // Explain settings provenance and firmware-specific behavior
        get detailsHelp() {
            return [
                this.valuesSource,
                'Effects vary by firmware.',
                'Turn Custom colors on to use the effect color; off uses the keyboard’s rainbow palette.',
                this.statusDetail,
                this.effectsUnavailableReason,
                this.speedUnavailableReason,
            ]
                .filter(Boolean)
                .join(' ');
        },
        // Check whether the selected effect accepts a background color
        get hasBackgroundColor() {
            return Boolean(
                this.capabilities?.background_color_effects?.includes(Number(this.effect)),
            );
        },
        // Provide a valid fallback for the background color input
        get backgroundColorValue() {
            return this.backgroundColor || '#ffffff';
        },
        // Build speed choices from the keyboard capability range
        get speedOptions() {
            return Array.from({ length: (this.capabilities?.speed_max ?? -1) + 1 }, (_, value) =>
                String(value),
            );
        },
        // Check whether the device exposes idle timeout choices
        get hasSleepTimer() {
            return Boolean(this.capabilities?.sleep_timeout_seconds_options?.length);
        },
        // Format the supported keyboard idle timeout choices
        get sleepTimeoutOptions() {
            return (this.capabilities?.sleep_timeout_seconds_options ?? []).map((seconds) => ({
                value: String(seconds),
                label:
                    seconds === 4294967295
                        ? 'Never'
                        : seconds >= 3600
                          ? `${seconds / 3600} ${seconds === 3600 ? 'hour' : 'hours'}`
                          : `${seconds / 60} ${seconds === 60 ? 'minute' : 'minutes'}`,
            }));
        },
        // Keep idle-timeout changes draft-only until explicit application
        sleepTimeoutChanged() {
            // Device idle timers are apply-only; changing this draft never sends a command
        },
        // Explain why keyboard effects are unavailable
        get effectsUnavailableReason() {
            return this.capabilities?.effects_unavailable_reason ?? '';
        },
        // Explain why keyboard effect speed is unavailable
        get speedUnavailableReason() {
            return this.capabilities?.speed_unavailable_reason ?? '';
        },
        // Check whether the selected device declares keyboard capabilities
        get supported() {
            return Boolean(this.capabilities);
        },
        // Read the device brightness limit with a default percentage range
        get brightnessMax() {
            return this.capabilities?.brightness_max ?? 100;
        },
        // Provide a display value for the brightness draft
        get brightnessValue() {
            return this.brightness || '0';
        },
        // Provide a valid hexadecimal color for the color input
        get colorValue() {
            return /^#[0-9a-f]{6}$/i.test(this.color) ? this.color : '#ffffff';
        },
        // Update the brightness draft and request a live preview
        brightnessChanged(event: Event) {
            this.brightness = (event.target as HTMLInputElement).value;
            void this.previewChanged();
        },
        // Update custom-color selection and request a live preview
        customColorChanged() {
            this.customColorKnown = true;
            if (this.customColor && !this.color) this.color = '#ffffff';
            void this.previewChanged();
        },
        // Update the effect-color draft and request a live preview
        colorChanged(event: Event) {
            this.color = (event.target as HTMLInputElement).value;
            void this.previewChanged();
        },
        // Update the background-color draft and request a live preview
        backgroundChanged(event: Event) {
            this.backgroundColor = backgroundToHex(
                backgroundFromHex((event.target as HTMLInputElement).value),
            );
            void this.previewChanged();
        },
        // Mark the OS mode as an explicitly edited draft value
        osModeChanged() {
            this.osModeTouched = true;
        },
        // List the OS modes reported by the keyboard capabilities
        get osModes() {
            return this.capabilities?.os_modes ?? [];
        },
        // Resolve saved keyboard preferences for the current device contract
        get lastRequested() {
            const saved = app().deviceOutput.keyboard;
            if (
                !saved ||
                (saved.brightness == null &&
                    saved.color == null &&
                    saved.os_mode == null &&
                    saved.effect == null &&
                    saved.speed == null &&
                    saved.sleep_timeout_seconds == null)
            )
                return 'Last requested values: unconfigured. Hardware state is unknown.';
            const color = saved.color ? saved.color.join(', ') : 'unconfigured';
            return `Last requested values: brightness ${saved.brightness ?? 'unconfigured'}; RGB ${color}; OS mode ${saved.os_mode ?? 'unconfigured'}; effect ${saved.effect ?? 'unconfigured'}; speed ${saved.speed ?? 'unconfigured'}. Hardware state is unknown.`;
        },
        // Check whether saved keyboard preferences match the connected device
        get canReapply() {
            const saved = app().deviceOutput.keyboard;
            return Boolean(
                saved &&
                (saved.brightness != null ||
                    saved.color != null ||
                    saved.os_mode != null ||
                    saved.effect != null ||
                    saved.speed != null ||
                    saved.sleep_timeout_seconds != null),
            );
        },
        // Require available matching saved settings before preparing a reapply
        get canPrepare() {
            return this.canReapply && !this.unavailableReason;
        },
        // Detect keyboard draft changes relative to the current baseline
        get dirty() {
            return Boolean(
                this.brightness !== this.initialBrightness ||
                this.color.toLowerCase() !== this.initialColor.toLowerCase() ||
                this.customColor !== this.initialCustomColor ||
                (this.hasBackgroundColor && this.backgroundColor !== this.initialBackgroundColor) ||
                this.effect !== this.initialEffect ||
                this.speed !== this.initialSpeed ||
                this.sleepTimeout !== this.initialSleepTimeout ||
                this.forceReapply ||
                this.osModeTouched ||
                this.osMode !== this.initialOsMode,
            );
        },
        // Explain why keyboard changes cannot currently be applied
        get unavailableReason() {
            if (!this.supported) return 'Keyboard settings are unavailable for this device.';
            if (!app().device?.connected) return 'Connect the device to apply keyboard settings.';
            if (app().effectiveDeviceOutput.sleeping)
                return 'Wake the device before applying keyboard settings.';
            if (app().busy || this.saving || this.discarding)
                return 'Wait for the current operation to finish.';
            return '';
        },
        // Determine whether the current draft can be committed
        get canApply() {
            return this.dirty && !this.unavailableReason && this.identity === this.contractIdentity;
        },
        // Reset the keyboard draft from the current valid baseline
        reset() {
            ++this.epoch;
            this.identity = this.contractIdentity;
            const saved = this.baselineSettings;
            this.brightness = saved?.brightness == null ? '' : String(saved.brightness);
            this.color = saved?.color
                ? '#' + saved.color.map((v) => v.toString(16).padStart(2, '0')).join('')
                : '';
            this.backgroundColor = saved?.background_color
                ? backgroundToHex(saved.background_color)
                : '';
            this.customColorKnown = !this.usesReadback || saved?.custom_color != null;
            this.customColor = saved?.custom_color ?? !this.usesReadback;
            this.initialCustomColor = this.customColor;
            this.initialBackgroundColor = this.backgroundColor;
            this.previewMessage = '';
            this.previewedSettings = null;
            this.previewing = false;
            this.effect =
                saved?.effect == null
                    ? this.hasEffects && !this.usesReadback
                        ? '0'
                        : ''
                    : String(saved.effect);
            this.speed =
                saved?.speed == null
                    ? this.hasSpeed && !this.usesReadback
                        ? '0'
                        : ''
                    : String(saved.speed);
            this.osMode = saved?.os_mode ?? (this.usesReadback ? '' : 'auto');
            this.sleepTimeout =
                saved?.sleep_timeout_seconds == null ? '' : String(saved.sleep_timeout_seconds);
            this.initialSleepTimeout = this.sleepTimeout;
            this.initialBrightness = this.brightness;
            this.initialColor = this.color;
            this.initialEffect = this.effect;
            this.initialSpeed = this.speed;
            this.initialOsMode = this.osMode;
            this.osModeTouched = false;
            this.forceReapply = false;
            this.statusDetail = '';
            this.error = '';
            this.status = '';
        },
        // Release the owned hardware preview without persisting draft values
        async releasePreview() {
            const id = this.previewId;
            const deviceId = this.previewDeviceId;
            if (!id) return;
            // Finish the owned hardware preview before accepting subsequent keyboard work
            await trackCloseRelease('keyboard-preview:' + id, async () => {
                const result = await app().bridge.command('finish_keyboard_preview', {
                    deviceId,
                    previewId: id,
                    apply: false,
                    settings: null,
                });
                if (result.status === 'failed' || result.status === 'uncertain')
                    throw Error(result.message);
            });
            if (this.previewId === id) this.previewId = null;
        },
        // Discard the current keyboard settings draft and release temporary state
        discard(): Promise<void> {
            if (discardingTask) return discardingTask;
            if (this.saving) return Promise.resolve();
            ++this.previewRevision;
            ++this.epoch;
            this.discarding = true;
            // Serialize draft discard with outstanding hardware preview requests
            discardingTask = (async () => {
                try {
                    await enqueue(() => this.releasePreview());
                    this.useSavedPreferences = false;
                    this.reset();
                } catch (error) {
                    this.error = 'Could not restore the previous lighting: ' + String(error);
                    throw error;
                } finally {
                    this.discarding = false;
                    discardingTask = null;
                }
            })();
            return discardingTask;
        },
        // Add asynchronous keyboard preview cleanup to the discard event
        discardEvent(event: Event) {
            const task = this.discard();
            const detail = (event as CustomEvent<{ pending: Promise<unknown>[] }>).detail;
            detail?.pending.push(task);
            void task.catch(() => {});
        },
        // Remove apply-only settings from the live preview patch
        previewSettings(): KeyboardSettings {
            const patch = this.patch();
            delete patch.os_mode;
            delete patch.sleep_timeout_seconds;
            if (this.hasGlobalRgb && this.customColorKnown && (this.color || !this.customColor))
                patch.custom_color = this.customColor;
            // A full desired state lets returning a slider to its saved value
            // undo a previous preview, and retains luminance after mode changes
            if (this.brightness !== '') patch.brightness = Number(this.brightness);
            // Rebuild the desired visual state after a mode change, never OS layout
            if (this.effect !== '') patch.effect = Number(this.effect);
            if (this.speed !== '') patch.speed = Number(this.speed);
            if (this.color && /^#[0-9a-f]{6}$/i.test(this.color))
                patch.color = [1, 3, 5].map((i) => parseInt(this.color.slice(i, i + 2), 16)) as [
                    number,
                    number,
                    number,
                ];
            const background = this.hasBackgroundColor
                ? this.backgroundColor
                : this.initialBackgroundColor;
            if (background) patch.background_color = backgroundFromHex(background);
            return patch;
        },
        // Coalesce draft changes and preview only the current keyboard revision
        async previewChanged() {
            if (this.unavailableReason || this.identity !== this.contractIdentity) return;
            const revision = ++this.previewRevision;
            const epoch = this.epoch;
            const deviceId = app().deviceId;
            this.previewing = true;
            // Skip stale draft revisions before preparing a hardware preview
            return enqueue(async () => {
                if (revision !== this.previewRevision || epoch !== this.epoch) return;
                try {
                    const desired = this.previewSettings();
                    const previous = {
                        ...(this.previewedSettings ?? this.baselineSettings ?? {}),
                    };
                    if (previous.custom_color == null && previous.color != null)
                        previous.custom_color = true;
                    const settings: KeyboardSettings = { ...desired };
                    const effectChanged = desired.effect !== previous.effect;
                    for (const key of Object.keys(settings) as (keyof KeyboardSettings)[]) {
                        const restoreAfterEffect =
                            effectChanged &&
                            [
                                'speed',
                                'color',
                                'custom_color',
                                'background_color',
                                'brightness',
                            ].includes(key);
                        if (
                            !restoreAfterEffect &&
                            JSON.stringify(desired[key]) === JSON.stringify(previous[key])
                        )
                            delete settings[key];
                    }
                    // Mode and RGB share one command, including when toggling
                    if (settings.custom_color != null && desired.color != null)
                        settings.color = desired.color;
                    if (settings.color != null && this.customColorKnown)
                        settings.custom_color = this.customColor;
                    // COLOR also carries HSV/value. Preserve explicitly selected
                    // luminance after a color write, not only after mode changes
                    if (
                        (settings.color != null || settings.custom_color != null) &&
                        desired.brightness != null
                    )
                        settings.brightness = desired.brightness;
                    if (Object.keys(settings).length === 0) return;
                    const result = await app().bridge.command('preview_keyboard_settings', {
                        deviceId,
                        previewId: this.previewId,
                        settings,
                    });
                    if (result.status === 'transmitted' || result.status === 'simulated')
                        this.previewedSettings = desired;
                    this.previewId = result.preview_id;
                    this.previewDeviceId = deviceId;
                    if (epoch !== this.epoch || this.contractIdentity !== this.identity) {
                        await this.releasePreview();
                        return;
                    }
                    this.previewMessage = result.message;
                    if (result.status === 'failed' || result.status === 'uncertain')
                        this.error = result.message;
                    else this.error = '';
                } catch (error) {
                    if (epoch === this.epoch) this.previewMessage = String(error);
                } finally {
                    if (revision === this.previewRevision) this.previewing = false;
                }
            });
        },
        // Prepare matching saved keyboard preferences for explicit application
        reapply() {
            if (this.unavailableReason) return;
            if (this.previewId || this.previewing) return;
            this.useSavedPreferences = true;
            this.reset();
            this.forceReapply = true;
        },
        // Build a capability-aware keyboard settings patch from the draft
        patch(): KeyboardSettings {
            const patch: KeyboardSettings = {};
            if (
                this.hasGlobalRgb &&
                (this.customColor !== this.initialCustomColor ||
                    (this.forceReapply && (this.color || !this.customColor)))
            ) {
                patch.custom_color = this.customColor;
            }
            if (
                this.brightness !== '' &&
                (this.forceReapply || this.brightness !== this.initialBrightness)
            ) {
                const value = Number(this.brightness);
                if (
                    !Number.isInteger(value) ||
                    value < 0 ||
                    value > (this.capabilities?.brightness_max ?? -1)
                )
                    throw Error('Choose a supported keyboard brightness.');
                patch.brightness = value;
            }
            if (
                this.color !== '' &&
                (this.forceReapply ||
                    this.color.toLowerCase() !== this.initialColor.toLowerCase() ||
                    this.customColor !== this.initialCustomColor ||
                    (this.effect !== '' && this.effect !== this.initialEffect))
            ) {
                if (!this.capabilities?.global_rgb || !/^#[0-9a-f]{6}$/i.test(this.color))
                    throw Error('Enter a global RGB color as #RRGGBB, or leave it empty.');
                patch.color = [1, 3, 5].map((i) => parseInt(this.color.slice(i, i + 2), 16)) as [
                    number,
                    number,
                    number,
                ];
            }
            if (patch.color != null && this.customColorKnown && !this.customColor)
                patch.custom_color = false;
            if (
                this.hasBackgroundColor &&
                this.backgroundColor &&
                (this.forceReapply ||
                    this.backgroundColor !== this.initialBackgroundColor ||
                    this.effect !== this.initialEffect)
            ) {
                patch.background_color = backgroundFromHex(this.backgroundColor);
            }
            for (const key of ['effect', 'speed'] as const) {
                const initial = key === 'effect' ? this.initialEffect : this.initialSpeed;
                const max = this.capabilities?.[key === 'effect' ? 'effect_max' : 'speed_max'];
                if (this[key] === '' && (max == null || this.usesReadback)) continue;
                if (this[key] === '') throw Error(`Choose a supported keyboard ${key}.`);
                if (
                    !this.forceReapply &&
                    this[key] === initial &&
                    this.baselineSettings?.[key] != null &&
                    !(key === 'speed' && patch.effect === 0)
                )
                    continue;
                const value = Number(this[key]);
                if (max == null || !Number.isInteger(value) || value < 0 || value > max)
                    throw Error(`Choose a supported keyboard ${key}.`);
                patch[key] = value;
            }
            if (
                this.osMode !== '' &&
                (this.forceReapply ||
                    this.osModeTouched ||
                    this.osMode !== this.initialOsMode ||
                    this.baselineSettings?.os_mode == null)
            ) {
                if (
                    this.osMode !== 'auto' &&
                    !this.osModes.includes(this.osMode as 'windows' | 'macos')
                )
                    throw Error('Choose a supported keyboard OS mode.');
                patch.os_mode = this.osMode as 'auto' | 'windows' | 'macos';
            }
            if (
                this.sleepTimeout !== '' &&
                (this.forceReapply || this.sleepTimeout !== this.initialSleepTimeout)
            ) {
                const value = Number(this.sleepTimeout);
                if (
                    !Number.isInteger(value) ||
                    !this.capabilities?.sleep_timeout_seconds_options?.includes(value)
                )
                    throw Error('Choose a supported keyboard sleep timer.');
                patch.sleep_timeout_seconds = value;
            }
            return patch;
        },
        // Commit the current keyboard draft only while its device identity remains current
        async apply() {
            if (!this.canApply) return;
            ++this.previewRevision;
            const epoch = this.epoch;
            const identity = this.identity;
            // Check whether asynchronous work still belongs to the current state generation
            const current = () => this.epoch === epoch && this.contractIdentity === identity;
            this.error = '';
            this.status = '';
            this.saving = true;
            try {
                // Apply a current settings patch while preserving preview and device ownership
                const result = await enqueue(async () => {
                    const settings = this.previewId
                        ? { ...this.patch(), ...this.previewSettings() }
                        : this.patch();
                    if (this.previewId) {
                        const result = await app().bridge.command('finish_keyboard_preview', {
                            deviceId: this.previewDeviceId,
                            previewId: this.previewId,
                            apply: true,
                            settings,
                        });
                        if (result.saved) this.previewId = null;
                        return result;
                    }
                    return app().bridge.command('apply_keyboard_settings', {
                        deviceId: app().deviceId,
                        settings,
                    });
                });
                if (!current()) return;
                app().apply(result.snapshot);
                this.useSavedPreferences = false;
                if (this.observation?.supported) await this.refreshDeviceSettings();
                this.reset();
                this.statusDetail = result.message;
                this.status =
                    result.status === 'simulated'
                        ? 'Saved in simulator.'
                        : result.status === 'transmitted'
                          ? 'Settings sent.'
                          : result.status === 'failed'
                            ? 'Saved, but could not send to the keyboard.'
                            : 'Saved; check the keyboard before retrying.';
            } catch (error) {
                if (current()) this.error = String(error);
            } finally {
                this.saving = false;
                this.previewing = false;
            }
        },
    };
}

type Controller = {
    keyboard: ReturnType<typeof createKeyboardSettings>;
    $watch(property: string, callback: () => void): void;
    unregisterClose?: () => void;
    $refs: Record<string, HTMLElement>;
};
// Register keyboard settings controls and reset confirmation
export function registerKeyboardSettings(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('keyboardSettings', () => ({
        keyboard: createKeyboardSettings(app),
        // Open confirmation for the current keyboard identity
        requestFactoryReset(this: Controller) {
            if (!this.keyboard.canFactoryReset) return;
            this.keyboard.resetConfirmIdentity = this.keyboard.contractIdentity;
            (this.$refs.keyboardFactoryConfirm as HTMLDialogElement).showModal();
        },
        // Dismiss reset confirmation without sending a hardware command
        cancelFactoryReset(this: Controller) {
            this.keyboard.resetConfirmIdentity = '';
            (this.$refs.keyboardFactoryConfirm as HTMLDialogElement).close();
        },
        // Close the confirmation dialog and reset the confirmed keyboard
        async confirmFactoryReset(this: Controller) {
            (this.$refs.keyboardFactoryConfirm as HTMLDialogElement).close();
            await this.keyboard.factoryReset();
        },
        // Attach keyboard settings listeners and window-close coordination
        init(this: Controller) {
            this.keyboard.reset();
            void this.keyboard.refreshDeviceSettings();
            ownWindowMethods(this.keyboard, 'keyboard settings', [
                'apply',
                'previewChanged',
                'factoryReset',
            ]);
            this.unregisterClose = registerCloseDraft(
                this.keyboard,
                ['apply', 'previewChanged', 'factoryReset'],
                {
                    name: 'Keyboard settings',
                    // Report unsaved keyboard settings work to window-close coordination
                    state: () => (this.keyboard.dirty ? 'dirty' : 'clean'),
                    // Discard the current keyboard settings draft and release temporary state
                    discard: () => this.keyboard.discard(),
                    // Release temporary keyboard settings state during close coordination
                    release: () => this.keyboard.discard(),
                },
            );
            // Discard live keyboard previews when effective device output sleeps
            this.$watch('$store.app.effectiveDeviceOutput.sleeping', () => {
                if (app().effectiveDeviceOutput.sleeping)
                    void this.keyboard.discard().catch(() => {});
            });
            // Reset readback and saved-preference selection when the device contract changes
            this.$watch('keyboard.contractIdentity', () => {
                this.keyboard.observation = null;
                this.keyboard.useSavedPreferences = false;
                this.keyboard.readbackGeneration++;
                void this.keyboard
                    .discard()
                    .then(() => this.keyboard.refreshDeviceSettings())
                    .catch(() => {});
            });
        },
        // Detach keyboard settings listeners and release owned state
        destroy(this: Controller) {
            this.keyboard.readbackGeneration++;
            void this.keyboard.discard().catch(() => {});
            this.unregisterClose?.();
        },
    }));
}
