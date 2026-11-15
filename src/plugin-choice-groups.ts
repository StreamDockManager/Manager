import { pluginGroups } from './plugin-actions';
import { paginate } from './pagination';
import type { Plugin } from './types';
import type { LibraryOrganizationStore } from './library-organization';
// Filter plugin actions by search, control compatibility and favorites before paginating each group
export function pluginChoiceGroups(
    plugin: Plugin,
    query: string,
    controlFilter: string,
    favoritesOnly: boolean,
    organization: LibraryOrganizationStore,
    actionPages: Record<string, number>,
) {
    return pluginGroups(
        [plugin],
        query,
        controlFilter,
        (plugin, action) =>
            organization.tags({ kind: 'plugin', plugin_id: plugin, action_id: action }).join(' '),
        (plugin, action) =>
            !favoritesOnly ||
            organization.favorite({
                kind: 'plugin',
                plugin_id: plugin,
                action_id: action,
            }),
        // Convert each entry into the representation required by plugin choice groups
    ).map((group) => {
        const key = plugin.id + ':' + group.id;
        return {
            ...group,
            key,
            window: paginate(group.items, actionPages[key] || 1, 12),
        };
    });
}
