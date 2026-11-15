import { defaultMedia } from './bridge';
import type { Binding, Profile } from './types';

export const KEYBOARD_NAVIGATION_PAGE = '__keyboard_navigation';
export const KEYBOARD_NAVIGATION_KEYS = [
    ['Insert', 'insert'],
    ['Home', 'home'],
    ['Page Up', 'pageup'],
    ['Delete', 'delete'],
    ['End', 'end'],
    ['Page Down', 'pagedown'],
] as const;

// Recognize a page intended for hardware keyboard navigation
export function isKeyboardNavigationPage(profile: Profile | undefined, pageId: string | undefined) {
    return profile?.model_id === 'k1-pro' && pageId === KEYBOARD_NAVIGATION_PAGE;
}
// Recognize a control that participates in keyboard navigation
export function isKeyboardNavigationControl(
    profile: Profile | undefined,
    pageId: string | undefined,
    controlId: string,
) {
    return isKeyboardNavigationPage(profile, pageId) && /^key-[1-6]$/.test(controlId);
}
// Create the reserved keyboard navigation bindings with their default actions
export function keyboardNavigationBindings(): Record<string, Binding> {
    return Object.fromEntries(
        KEYBOARD_NAVIGATION_KEYS.map(([title, key], index) => [
            `key-${index + 1}`,
            {
                title,
                color: '#00000000',
                background_source: 'palette',
                action: { kind: 'hotkey', keys: [key] },
                media: defaultMedia(),
                disabled: false,
            } satisfies Binding,
        ]),
    );
}
/** Browser-only counterpart of the native invariant. Existing custom pages remain intact */
export function ensureKeyboardNavigation(profile: Profile) {
    if (profile.model_id !== 'k1-pro') return;
    const index = profile.pages.findIndex((page) => page.id === KEYBOARD_NAVIGATION_PAGE);
    let existing = index >= 0 ? profile.pages[index] : undefined;
    if (
        existing &&
        (existing.name !== 'Navigation' ||
            Object.entries(keyboardNavigationBindings()).some(
                ([id, binding]) =>
                    JSON.stringify(sorted(behavior(existing!.bindings[id]))) !==
                    JSON.stringify(sorted(behavior(binding))),
            ))
    ) {
        throw Error(
            'Reserved keyboard Navigation page conflicts with custom content; rename that page before importing',
        );
    } else if (existing) profile.pages.splice(index, 1);
    profile.pages.unshift({
        id: KEYBOARD_NAVIGATION_PAGE,
        name: 'Navigation',
        bindings: { ...keyboardNavigationBindings(), ...existing?.bindings },
    });
    if (!profile.pages.some((page) => page.id === profile.active_page))
        profile.active_page = KEYBOARD_NAVIGATION_PAGE;
}
// Extract binding behavior used to protect reserved keyboard navigation
function behavior(binding: Binding | undefined | null) {
    return binding
        ? {
              action: binding.action,
              dial_response: binding.dial_response || null,
              activation_policy: binding.activation_policy || 'queue',
              disabled: binding.disabled || false,
          }
        : null;
}
// Order keyboard navigation entries deterministically
function sorted(value: unknown): unknown {
    if (Array.isArray(value)) return value.map(sorted);
    if (value && typeof value === 'object')
        return Object.fromEntries(
            Object.entries(value)
                .filter(([, v]) => v !== undefined)
                .sort(([a], [b]) => a.localeCompare(b))
                .map(([k, v]) => [k, sorted(v)]),
        );
    return value;
}
// Validate keyboard navigation bindings throughout a profile
export function assertKeyboardNavigationProfile(original: Profile | undefined, next: Profile) {
    if (!original || original.model_id !== next.model_id || original.model_id !== 'k1-pro') return;
    const before = original.pages.find((page) => page.id === KEYBOARD_NAVIGATION_PAGE);
    if (!before) return;
    const after = next.pages[0];
    if (
        after?.id !== before.id ||
        after.name !== before.name ||
        Object.keys(keyboardNavigationBindings()).some(
            (id) =>
                JSON.stringify(sorted(behavior(after.bindings[id]))) !==
                JSON.stringify(sorted(behavior(before.bindings[id]))),
        )
    ) {
        throw Error('The keyboard Navigation page and its six navigation keys are protected.');
    }
}
// Validate the keyboard navigation action assigned to a control
export function assertKeyboardNavigationControl(
    profile: Profile | undefined,
    pageId: string,
    controlId: string,
    next?: Binding | null,
) {
    if (
        isKeyboardNavigationControl(profile, pageId, controlId) &&
        JSON.stringify(sorted(behavior(next))) !==
            JSON.stringify(
                sorted(
                    behavior(
                        profile?.pages.find((page) => page.id === pageId)?.bindings[controlId],
                    ),
                ),
            )
    )
        throw Error('The keyboard navigation keys are protected.');
}
