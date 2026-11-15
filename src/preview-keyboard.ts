import type { PreviewHandlers } from './preview-handlers';
import type { PreviewWorkspace } from './preview-workspace';
import type { KeyboardSettings } from '../generated/runtime';

type PreviewLease = { deviceId: string; original: KeyboardSettings; values: KeyboardSettings };
const previewLeases = new WeakMap<PreviewWorkspace['s'], Map<string, PreviewLease>>();
const resetSimulators = new WeakMap<PreviewWorkspace['s'], Set<string>>();
let nextId = 0;

/** Browser-only counterpart of the native lighting lease; never writes hardware. */
export function createPreviewKeyboardHandlers(workspace: PreviewWorkspace): PreviewHandlers {
    const { s, sessionFor, commit } = workspace;
    let leases = previewLeases.get(s);
    if (!leases) {
        leases = new Map();
        previewLeases.set(s, leases);
    }
    const active = leases;
    let resets = resetSimulators.get(s);
    if (!resets) {
        resets = new Set();
        resetSimulators.set(s, resets);
    }
    const resetDevices = resets;
    // Prepare state required by the preview keyboard operation
    const prepare = (deviceId: string, settings: KeyboardSettings, preview = false) => {
        const device = s.devices.find((d) => d.id === deviceId);
        if (!device?.connected) throw Error('Device not connected');
        const capabilities = device.manifest.capabilities.keyboard;
        if (!capabilities) throw Error('Keyboard settings are unavailable for this device');
        const session = sessionFor(device);
        if (session.output.sleeping) throw Error('Device is sleeping');
        if (
            Object.keys(settings).some(
                (key) =>
                    ![
                        'brightness',
                        'color',
                        'custom_color',
                        'background_color',
                        'os_mode',
                        'effect',
                        'speed',
                        'sleep_timeout_seconds',
                    ].includes(key),
            )
        )
            throw Error('Unsupported keyboard setting');
        if (preview && settings.os_mode != null) throw Error('Keyboard layout cannot be previewed');
        if (preview && settings.sleep_timeout_seconds != null)
            throw Error('Keyboard sleep timer cannot be previewed');
        if (
            settings.sleep_timeout_seconds != null &&
            (!Number.isInteger(settings.sleep_timeout_seconds) ||
                !capabilities.sleep_timeout_seconds_options?.includes(
                    settings.sleep_timeout_seconds,
                ))
        )
            throw Error('Invalid keyboard sleep timer');
        if (
            settings.brightness != null &&
            (!Number.isInteger(settings.brightness) ||
                settings.brightness < 0 ||
                settings.brightness > capabilities.brightness_max)
        )
            throw Error('Invalid keyboard brightness');
        if (
            settings.color != null &&
            (!capabilities.global_rgb ||
                !Array.isArray(settings.color) ||
                settings.color.length !== 3 ||
                settings.color.some(
                    (value) => !Number.isInteger(value) || value < 0 || value > 255,
                ))
        )
            throw Error('Invalid keyboard RGB color');
        if (
            settings.custom_color != null &&
            (!capabilities.global_rgb || typeof settings.custom_color !== 'boolean')
        )
            throw Error('Invalid keyboard custom color mode');
        if (
            settings.custom_color === true &&
            settings.color == null &&
            session.output.keyboard?.color == null
        )
            throw Error('Choose a custom RGB color before enabling custom colors');
        if (
            settings.os_mode != null &&
            settings.os_mode !== 'auto' &&
            !capabilities.os_modes.includes(settings.os_mode)
        )
            throw Error('Invalid keyboard OS mode');
        for (const key of ['effect', 'speed'] as const) {
            const value = settings[key],
                max = capabilities[key === 'effect' ? 'effect_max' : 'speed_max'];
            if (
                value != null &&
                (max == null || !Number.isInteger(value) || value < 0 || value > max)
            )
                throw Error(`Invalid keyboard ${key}`);
        }
        if (settings.background_color != null) {
            const { hue, saturation } = settings.background_color;
            if (
                !capabilities.background_color_effects?.length ||
                !Number.isInteger(hue) ||
                hue < 0 ||
                hue > 359 ||
                !Number.isInteger(saturation) ||
                saturation < 0 ||
                saturation > 100
            )
                throw Error('Invalid keyboard background color');
        }
        const patch = Object.fromEntries(
            Object.entries(settings).filter(([, v]) => v != null),
        ) as KeyboardSettings;
        if (!Object.keys(patch).length) throw Error('Choose at least one keyboard setting');
        return { session, patch };
    };
    // Apply the pending preview keyboard changes
    const apply = (deviceId: string, settings: KeyboardSettings) => {
        const { session, patch } = prepare(deviceId, settings);
        session.output.keyboard = { ...session.output.keyboard, ...structuredClone(patch) };
        if (settings.effect === 0 && settings.speed == null) session.output.keyboard.speed = 0;
        return {
            snapshot: commit().snapshot,
            status: 'simulated' as const,
            saved: true,
            message:
                'Browser preview only. Last requested values updated; no hardware commands were sent.',
        };
    };
    return {
        // Simulate keyboard device settings in the browser without calling native services
        keyboard_device_settings: ({ deviceId }) => {
            const device = s.devices.find((d) => d.id === deviceId);
            if (!device?.connected) throw Error('Device not connected');
            return {
                supported: resetDevices.has(deviceId),
                settings: null,
                state: resetDevices.has(deviceId) ? 'simulated_reset' : 'simulated',
                message: resetDevices.has(deviceId)
                    ? 'Simulator reset completed. Factory values are unknown; no physical keyboard was queried.'
                    : '',
            };
        },
        // Simulate factory reset keyboard in the browser without calling native services
        factory_reset_keyboard: ({ deviceId }) => {
            const device = s.devices.find((d) => d.id === deviceId);
            if (!device?.connected || !device.simulated)
                throw Error('Connected simulator required');
            if (!device.manifest.capabilities.keyboard?.factory_reset)
                throw Error('Factory reset is unavailable for this model');
            if (sessionFor(device).output.sleeping) throw Error('Wake the device before resetting');
            if ([...active.values()].some((lease) => lease.deviceId === deviceId))
                throw Error('Finish the keyboard preview before resetting');
            delete sessionFor(device).output.keyboard;
            resetDevices.add(deviceId);
            return {
                snapshot: commit().snapshot,
                status: 'simulated' as const,
                saved: true,
                message:
                    'Simulator reset completed. No hardware commands were sent; factory values remain unknown.',
            };
        },
        // Simulate apply keyboard settings in the browser without calling native services
        apply_keyboard_settings: ({ deviceId, settings }) => {
            if ([...active.values()].some((lease) => lease.deviceId === deviceId))
                throw Error('Finish the lighting preview before applying settings.');
            return apply(deviceId, settings);
        },
        // Simulate preview keyboard settings in the browser without calling native services
        preview_keyboard_settings: ({ deviceId, previewId, settings }) => {
            const { session, patch } = prepare(deviceId, settings, true);
            const original = session.output.keyboard || {};
            let lease = previewId ? active.get(previewId) : undefined;
            if (previewId && (!lease || lease.deviceId !== deviceId))
                throw Error('Lighting preview is no longer active.');
            if (!lease) {
                if ([...active.values()].some((entry) => entry.deviceId === deviceId))
                    throw Error('Another lighting preview is active.');
                const required = [
                    'effect',
                    'speed',
                    ...(original.custom_color === false ? [] : ['color']),
                    ...(patch.brightness != null ? ['brightness'] : []),
                    ...(patch.background_color != null ? ['background_color'] : []),
                ] as (keyof KeyboardSettings)[];
                if (required.some((key) => original[key] == null))
                    throw Error(
                        'Apply your lighting settings once before previewing, so they can be restored.',
                    );
                previewId = `keyboard-preview-${++nextId}`;
                lease = {
                    deviceId,
                    original: structuredClone(original),
                    values: structuredClone(original),
                };
                active.set(previewId, lease);
            }
            if (JSON.stringify(original) !== JSON.stringify(lease.original))
                throw Error('Saved lighting changed. Reopen settings before previewing.');
            for (const key of ['brightness', 'background_color'] as const)
                if (patch[key] != null && lease.original[key] == null)
                    throw Error(
                        'Apply your lighting settings once before previewing, so they can be restored.',
                    );
            lease.values = { ...lease.values, ...structuredClone(patch) };
            return {
                preview_id: previewId!,
                status: 'simulated' as const,
                message: 'Previewing lighting. Apply saves; Cancel restores.',
            };
        },
        // Simulate finish keyboard preview in the browser without calling native services
        finish_keyboard_preview: ({ deviceId, previewId, apply: save, settings }) => {
            const lease = active.get(previewId);
            if (!lease) {
                if (save) throw Error('Lighting preview is no longer active.');
                return {
                    snapshot: structuredClone(s),
                    status: 'simulated' as const,
                    saved: false,
                    message: 'Lighting preview ended.',
                };
            }
            if (lease.deviceId !== deviceId)
                throw Error('Lighting preview belongs to another device.');
            if (save) {
                if (!settings) throw Error('Choose settings to apply.');
                const { session } = prepare(deviceId, settings);
                if (
                    JSON.stringify(session.output.keyboard || {}) !== JSON.stringify(lease.original)
                )
                    throw Error('Saved lighting changed. Reopen settings.');
                const result = apply(deviceId, settings);
                active.delete(previewId);
                return result;
            }
            active.delete(previewId);
            return {
                snapshot: structuredClone(s),
                status: 'simulated' as const,
                saved: false,
                message: 'Previous lighting restored in simulator.',
            };
        },
    };
}
