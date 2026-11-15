import { startFrontendDiagnostics } from './frontend-errors';
startFrontendDiagnostics();
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { listen } from '@tauri-apps/api/event';
import type { AppearancePreferences } from '../generated/runtime';
import {
    applyTheme,
    normalizeStyle,
    readAppearance,
    themePreference,
    profileAppearance,
} from './theme-style';
import {
    accessibilityKey,
    applyAccessibility,
    readAccessibility,
} from './accessibility-preferences';
import './styles.css';
import { registerAutoScrollbars } from './scrollbars';
registerAutoScrollbars();
import './action-popup.css';
const id = new URLSearchParams(location.search).get('id') || '';
// Resolve a typed element from the popup document
const element = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
let sent = false;
const contrast = matchMedia('(prefers-contrast: more)');
const motion = matchMedia('(prefers-reduced-motion: reduce)');
// Apply cached accessibility preferences to the popup
const updateAccessibility = () =>
    applyAccessibility(readAccessibility(), contrast.matches, motion.matches);
// Refresh accessibility when another window updates the cache
const storedAccessibility = (event: StorageEvent) => {
    if (event.key === accessibilityKey || event.key === null) updateAccessibility();
};
const media = matchMedia('(prefers-color-scheme: dark)');
let appearance = readAppearance();
let appearanceRequest = 0;
let profilePalette: AppearancePreferences['profile_palette'] = null;
let customPalettes: AppearancePreferences['custom_palettes'] = [];
let systemDark = media.matches;
// Apply the current appearance to the popup document
const renderAppearance = () => {
    const effective = profileAppearance(
        appearance.preference,
        appearance.style,
        profilePalette,
        'manager',
        customPalettes,
    );
    applyTheme(effective.preference, systemDark, effective.style, customPalettes);
};
// Refresh the popup when the system color scheme changes
const systemChanged = (event: MediaQueryListEvent) => {
    systemDark = event.matches;
    renderAppearance();
};
// Load the authoritative popup appearance settings
async function refreshAppearance() {
    const request = ++appearanceRequest;
    const value = await invoke<AppearancePreferences>('appearance_preferences');
    if (request !== appearanceRequest) return;
    appearance = {
        preference: themePreference(value.preference),
        style: normalizeStyle(value.style, value.custom_palettes),
    };
    customPalettes = value.custom_palettes || [];
    profilePalette = value.profile_palette;
    systemDark = value.system_dark;
    renderAppearance();
    await getCurrentWindow().setTheme(
        profilePalette || appearance.preference === 'system' ? null : appearance.preference,
    );
}
// Refresh appearance without blocking popup interaction
function loadAppearance() {
    void refreshAppearance().catch((error) =>
        console.error('Could not load action prompt appearance:', error),
    );
}
renderAppearance();
updateAccessibility();
contrast.addEventListener('change', updateAccessibility);
motion.addEventListener('change', updateAccessibility);
window.addEventListener('storage', storedAccessibility);
media.addEventListener('change', systemChanged);
loadAppearance();
// Refresh appearance after attaching the native appearance listener
const appearanceListener = listen('ui-appearance-changed', loadAppearance).then((unlisten) => {
    loadAppearance();
    return unlisten;
});
void appearanceListener.catch((error) => console.error(error));
window.addEventListener(
    'pagehide',
    // Release popup listeners when the document closes
    () => {
        appearanceRequest++;
        media.removeEventListener('change', systemChanged);
        contrast.removeEventListener('change', updateAccessibility);
        motion.removeEventListener('change', updateAccessibility);
        window.removeEventListener('storage', storedAccessibility);
        void appearanceListener.then(
            (unlisten) => unlisten(),
            // Ignore listener cleanup failures during window teardown
            () => {},
        );
    },
    { once: true },
);
// Send the popup response before closing the native window
async function reply(value: string | null) {
    if (sent) return;
    sent = true;
    try {
        await invoke('action_ui_reply', { id, value });
        await getCurrentWindow().close();
    } catch (error) {
        element('error').textContent = String(error);
    }
}
// Load the popup request and connect its response controls
async function start() {
    const request = await invoke<{ kind: string; title: string; text?: string; labels?: string[] }>(
        'action_ui_request',
        { id },
    );
    element('title').textContent = request.title;
    element('description').textContent = request.text || '';
    if (request.kind === 'menu') {
        element('input-label').hidden = true;
        element('submit').hidden = true;
        // Create a response button for each requested choice
        (request.labels || []).forEach((label, index) => {
            const button = document.createElement('button');
            button.type = 'button';
            button.className = 'button';
            button.textContent = label;
            button.addEventListener('click', () => void reply(String(index)));
            element('choices').append(button);
        });
    }
    // Submit the entered response without a browser form navigation
    element('form').addEventListener('submit', (event) => {
        event.preventDefault();
        void reply(element<HTMLInputElement>('value').value);
    });
    element('cancel').addEventListener('click', () => void reply(null));
    // Treat Escape as cancellation of the popup request
    document.addEventListener('keydown', (event) => {
        if (event.key === 'Escape') void reply(null);
    });
    // Reply with cancellation when the native popup closes
    await getCurrentWindow().onCloseRequested(() => {
        if (!sent) void reply(null);
    });
    if (request.kind === 'menu')
        (element('choices').querySelector('button') || element('cancel')).focus();
    else element<HTMLInputElement>('value').focus();
}
// Show popup initialization failures instead of leaving an empty view
void start().catch((error) => {
    element('error').textContent = String(error);
});
