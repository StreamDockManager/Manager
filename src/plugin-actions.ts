import type { Action, Plugin } from './types';

// Verify that every plugin referenced by an action tree remains enabled
export function actionPluginsEnabled(action: Action, plugins: Plugin[]): boolean {
    if (action.kind === 'plugin')
        return plugins.some((plugin) => plugin.id === action.plugin_id && plugin.enabled);
    if (action.kind === 'builtin' && 'children' in action.action)
        return action.action.children.every((child) => actionPluginsEnabled(child, plugins));
    if (action.kind === 'macro' || action.kind === 'dial' || action.kind === 'events')
        return action.actions.every((step) => actionPluginsEnabled(step, plugins));
    if (action.kind === 'toggle')
        return (
            actionPluginsEnabled(action.on, plugins) && actionPluginsEnabled(action.off, plugins)
        );
    return true;
}

export const controlGroups = [
    { id: 'button', label: 'Buttons', hint: 'Press and release actions' },
    { id: 'encoder', label: 'Dials', hint: 'Rotation and dial presses' },
    { id: 'touch', label: 'Touch screens', hint: 'Touch display actions' },
    { id: 'other', label: 'Other controllers', hint: 'No supported control type declared' },
];
export interface PluginChoice {
    key: string;
    pluginId: string;
    pluginName: string;
    actionId: string;
    name: string;
    controlType: string;
    icon?: string | null;
}
export interface PluginGroup {
    id: string;
    label: string;
    hint: string;
    items: PluginChoice[];
}
// Encode plugin, action and control kind into a stable action-choice key
export function choiceKey(pluginId: string, actionId: string, kind: string): string {
    return `plugin:${JSON.stringify([pluginId, actionId, kind])}`;
}
// Group compatible plugin actions after applying search and caller-supplied filters
export function pluginGroups(
    plugins: Plugin[],
    query = '',
    kind = 'all',
    extra: (plugin: string, action: string) => string = () => '',
    include: (plugin: string, action: string) => boolean = () => true,
): PluginGroup[] {
    const search = query.trim().toLowerCase();
    return controlGroups
        .filter((group) => kind === 'all' || group.id === kind)
        .map((group) => ({
            ...group,
            // Complete the deferred plugin actions work using the current operation state
            items: plugins.flatMap((plugin) => {
                const matchesPackage =
                    `${plugin.name} ${plugin.version} ${plugin.dialect} ${plugin.kind} ${plugin.status}`
                        .toLowerCase()
                        .includes(search);
                return (
                    plugin.actions
                        // Keep entries that satisfy the plugin actions selection criteria
                        .filter((action) => {
                            const types = action.control_types;
                            const matchesKind =
                                group.id === 'other'
                                    ? types.length === 0
                                    : types.includes(group.id);
                            return (
                                action.visible_in_actions_list !== false &&
                                matchesKind &&
                                include(plugin.id, action.id) &&
                                (matchesPackage ||
                                    `${action.name} ${extra(plugin.id, action.id)}`
                                        .toLowerCase()
                                        .includes(search))
                            );
                        })
                        .map((action) => ({
                            key: choiceKey(plugin.id, action.id, group.id),
                            pluginId: plugin.id,
                            pluginName: plugin.name,
                            actionId: action.id,
                            name: action.name,
                            controlType: group.id,
                            icon: action.icon,
                        }))
                );
            }),
        }))
        .filter((group) => group.items.length > 0);
}
// Install the plugin package
export async function installPluginPackage(
    app: import('./state').AppState,
    native: boolean,
): Promise<void> {
    if (!native) {
        app.notify('Install plugins in the native desktop app.', 'warning');
        return;
    }
    window.dispatchEvent(new CustomEvent('import-plugin-package'));
}
