import type { AccessibilityPreferences } from '../generated/runtime';

type Choice = 'system' | 'on' | 'off';
export const accessibilityKey = 'stream-dock-manager.accessibility.v1';
// Accept explicit accessibility choices and otherwise follow the system
const choice = (value: unknown): Choice => (value === 'on' || value === 'off' ? value : 'system');
// Read cached accessibility preferences with safe defaults
export function readAccessibility(): AccessibilityPreferences {
    let saved: Record<string, unknown> = {};
    try {
        const value: unknown = JSON.parse(localStorage.getItem(accessibilityKey) || '{}');
        if (value && typeof value === 'object' && !Array.isArray(value))
            saved = value as Record<string, unknown>;
    } catch {}
    return {
        enabled: saved.enabled === true,
        largerText: saved.largerText === true,
        highContrast: choice(saved.highContrast),
        reducedMotion: choice(saved.reducedMotion),
        deviceLabels: saved.deviceLabels !== false,
    };
}

// Apply resolved accessibility choices to the document
export function applyAccessibility(
    preferences: AccessibilityPreferences,
    systemContrast: boolean,
    systemMotion: boolean,
) {
    const { enabled, highContrast, reducedMotion } = preferences;
    // Resolve an explicit preference before falling back to the system setting
    const resolve = (choice: Choice, system: boolean) =>
        enabled && choice !== 'system' ? choice === 'on' : system;
    const data = document.documentElement.dataset;
    data.largerText = String(enabled && preferences.largerText);
    data.highContrast = String(resolve(highContrast, systemContrast));
    data.reducedMotion = String(resolve(reducedMotion, systemMotion));
    data.deviceLabels = String(!enabled || preferences.deviceLabels);
}
