import type AlpineType from '@alpinejs/csp';
import { bridge, native } from './bridge';
import {
    accessibilityKey,
    applyAccessibility,
    readAccessibility,
} from './accessibility-preferences';

// Register the accessibility store and synchronize native and cached preferences
export function registerAccessibility(Alpine: typeof AlpineType) {
    const motion = matchMedia('(prefers-reduced-motion: reduce)');
    const contrast = matchMedia('(prefers-contrast: more)');
    let editEpoch = 0;
    const store = {
        ...readAccessibility(),
        systemMotion: motion.matches,
        systemContrast: contrast.matches,
        // Apply the current accessibility preferences to the document
        apply() {
            applyAccessibility(this, this.systemContrast, this.systemMotion);
        },
        // Persist accessibility preferences through the application bridge
        save() {
            editEpoch++;
            const value = {
                enabled: this.enabled,
                largerText: this.largerText,
                highContrast: this.highContrast,
                reducedMotion: this.reducedMotion,
                deviceLabels: this.deviceLabels,
            };
            // Cache the current accessibility preferences for other windows
            const cache = () => {
                try {
                    localStorage.setItem(accessibilityKey, JSON.stringify(value));
                } catch {}
            };
            if (native)
                void bridge
                    .command('save_client_preference', { name: 'accessibility', value })
                    .then(cache)
                    .catch((error) =>
                        console.error('Accessibility preferences could not be saved:', error),
                    );
            else cache();
            this.apply();
        },
    };
    Alpine.store('accessibility', store);
    // Read the reactive accessibility store after registration
    const reactive = () => Alpine.store('accessibility') as typeof store;
    // Reapply preferences when the system motion setting changes
    motion.addEventListener('change', (event) => {
        reactive().systemMotion = event.matches;
        reactive().apply();
    });
    // Reapply preferences when the system contrast setting changes
    contrast.addEventListener('change', (event) => {
        reactive().systemContrast = event.matches;
        reactive().apply();
    });
    // Apply accessibility cache changes received from another window
    const stored = (event: StorageEvent) => {
        if (event.key === accessibilityKey || event.key === null) {
            if (native) {
                void load();
                return;
            }
            Object.assign(reactive(), readAccessibility());
            reactive().apply();
        }
    };
    window.addEventListener('storage', stored);
    window.addEventListener('pagehide', () => window.removeEventListener('storage', stored), {
        once: true,
    });
    reactive().apply();
    // Load authoritative accessibility preferences from the backend
    const load = () => {
        const epoch = editEpoch;
        return (
            bridge
                .command('client_preferences')
                // Replace cached preferences with the settings returned by the backend
                .then((value) => {
                    if (epoch !== editEpoch) return;
                    Object.assign(reactive(), value.accessibility);
                    reactive().apply();
                    try {
                        localStorage.setItem(accessibilityKey, JSON.stringify(value.accessibility));
                    } catch {}
                })
                .catch((error) =>
                    console.error('Accessibility preferences could not be loaded:', error),
                )
        );
    };
    if (native) void load();
}
