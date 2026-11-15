import type { JsonValue } from '../generated/runtime';

type Settings = Record<string, JsonValue | undefined>;
export type UpdateModeSelection = { settings: Settings; mode: string };
export type RefreshPolicy = { default_ms: number; minimum_ms: number; maximum_ms: number };

// Missing current metadata is unconfigured, never inferred from retired settings
export function updateInterval(settings: Settings): number | undefined {
    const value = settings.update_interval_ms;
    return value === undefined ? undefined : typeof value === 'number' ? value : NaN;
}

// Update the mode
export function updateMode(
    settings: Settings,
    policy?: RefreshPolicy | null,
    requiresExplicit = false,
    selection?: UpdateModeSelection,
) {
    const value = updateInterval(settings);
    if (value === undefined) return requiresExplicit ? 'unconfigured' : 'default';
    if (selection?.settings === settings) return selection.mode;
    if (value === 0) return 'off';
    return value === policy?.default_ms ? 'default' : 'custom';
}

// Choose the update mode
export function chooseUpdateMode(
    settings: Settings,
    policy: RefreshPolicy,
    mode: string,
): Settings {
    if (!['default', 'off', 'custom'].includes(mode)) return settings;
    return {
        ...settings,
        update_interval_ms:
            mode === 'off'
                ? 0
                : mode === 'default'
                  ? policy.default_ms
                  : policy.default_ms || policy.minimum_ms,
    };
}
