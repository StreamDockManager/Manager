import type { Appearance } from '../generated/runtime';

// Create an independent default action appearance
export const defaultAppearance = (): Appearance => ({
    font: 'sans',
    font_weight: 400,
    font_italic: false,
    size: 18,
    foreground: '#ffffff',
    position: 'bottom',
    align: 'center',
    icon: '',
    icon_size: 42,
    icon_position: 'center',
    show_title: true,
});
// Select feedback supported by the built-in action kind
export function suggestedFeedback(kind: string) {
    return ['volume', 'mute', 'macro'].includes(kind) ? kind : 'off';
}
