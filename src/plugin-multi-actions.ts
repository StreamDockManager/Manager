import type { Action, Plugin } from './types';

// Expose declared Stream Deck states only for compatible plugin actions inside a multi-action
export function pluginStateChoices(
    action: Action,
    plugins: Plugin[],
    multiAction: boolean,
): string[] {
    if (!multiAction || action.kind !== 'plugin') return [];
    const plugin = plugins.find((plugin) => plugin.id === action.plugin_id);
    if (!plugin || plugin.dialect !== 'streamdeck' || plugin.capabilities) return [];
    const declared = plugin.actions.find((declared) => declared.id === action.action_id);
    return declared?.supported_in_multi_actions &&
        declared.control_types.includes('button') &&
        declared.state_names?.length === 2
        ? declared.state_names
        : [];
}

// Set the plugin desired state
export function setPluginDesiredState(action: Action, choices: string[], value: string): boolean {
    if (action.kind !== 'plugin' || choices.length !== 2) return false;
    if (value === '') delete action.user_desired_state;
    else if (value === '0' || value === '1') action.user_desired_state = Number(value);
    else return false;
    return true;
}

/** Vendor-authored application thumbnail, never a hardware surface image. */
export function pluginMultiActionImage(
    action: Action,
    plugins: Plugin[],
    multiAction: boolean,
): string | undefined {
    if (action.kind !== 'plugin') return undefined;
    const plugin = plugins.find((plugin) => plugin.id === action.plugin_id);
    const declared = plugin?.actions.find((declared) => declared.id === action.action_id);
    if (!plugin || !declared) return undefined;
    if (
        multiAction &&
        plugin.dialect === 'streamdeck' &&
        !plugin.capabilities &&
        declared.supported_in_multi_actions
    ) {
        const images = declared.multi_action_images;
        const index = action.user_desired_state ?? (images?.length === 1 ? 0 : undefined);
        const image = index === 0 || index === 1 ? images?.[index] : undefined;
        // Only bounded, rasterized host thumbnails are allowed for the new asset field
        if (
            image &&
            image.length <= 512 * 1024 &&
            /^data:image\/png;base64,[A-Za-z0-9+/=]+$/.test(image)
        )
            return image;
    }
    return declared.icon || plugin.icon || undefined;
}
