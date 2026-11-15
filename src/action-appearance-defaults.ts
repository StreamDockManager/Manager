import type { Action, Binding, Plugin } from './types';
import { DEFAULT_BACKGROUND } from './background-color';
import { defaultMedia } from './bridge';
import { defaultAppearance, suggestedFeedback } from './feature-defaults';
import catalog from './builtin-catalog.json';

/** Appearance owned by the editor, independent of the action's settings and trigger policy */
export function actionAppearanceDefaults(action: Action, plugins: Plugin[] = []) {
    const descriptor =
        action.kind === 'builtin'
            ? catalog.find((entry) => entry.id === action.action.kind)
            : undefined;
    const pluginAction =
        action.kind === 'plugin'
            ? plugins
                  .find((plugin) => plugin.id === action.plugin_id)
                  ?.actions.find((entry) => entry.id === action.action_id)
            : undefined;
    const keyboardMacro = action.kind === 'builtin' && action.action.kind === 'keyboard_macro';
    const appearance = defaultAppearance();
    if (descriptor) appearance.icon = descriptor.icon;
    if (keyboardMacro) {
        appearance.icon_position = 'top';
        appearance.position = 'center';
    }
    return {
        // The action's editor name is descriptive; its hardware default stays compact
        title: keyboardMacro ? 'Macro' : descriptor?.name || pluginAction?.name || '',
        color: DEFAULT_BACKGROUND,
        customBackground: false,
        backgroundSource: 'custom' as const,
        asset: undefined as Binding['asset'],
        media: defaultMedia(),
        appearance,
        appearanceEnabled: Boolean(descriptor),
        pluginAppearance: undefined as Binding['plugin_appearance'],
        composition: undefined as Binding['composition'],
        feedback: descriptor ? 'macro' : suggestedFeedback(action.kind),
    };
}
