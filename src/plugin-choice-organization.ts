import {
    librarySummary,
    openLibraryOrganization,
    type getLibraryOrganization,
} from './library-organization';
import type { Plugin } from './types';

type Organization = ReturnType<typeof getLibraryOrganization>;
/** Shared handlers, evaluated against the current route store on every interaction. */
interface Choice {
    pluginId: string;
    actionId: string;
}
interface ChoiceHandlers {
    choiceFavorite: (choice: Choice) => boolean;
    choiceTags: (choice: Choice) => string;
    organizeChoice: (event: Event) => void;
}
// Provide favorite, tag-summary and organization handlers for plugin action choices
export function pluginChoiceOrganization(
    organization: () => Organization,
    plugins: () => Plugin[],
): ChoiceHandlers {
    return {
        // Read whether the plugin action is marked as a favorite
        choiceFavorite(choice: { pluginId: string; actionId: string }) {
            return organization().favorite({
                kind: 'plugin',
                plugin_id: choice.pluginId,
                action_id: choice.actionId,
            });
        },
        // Summarize the plugin action's favorite status and saved organization tags
        choiceTags(choice: { pluginId: string; actionId: string }) {
            const tags = organization().tags({
                kind: 'plugin',
                plugin_id: choice.pluginId,
                action_id: choice.actionId,
            });
            return librarySummary(
                organization().favorite({
                    kind: 'plugin',
                    plugin_id: choice.pluginId,
                    action_id: choice.actionId,
                }),
                tags,
            );
        },
        // Open organization editing only for a still-existing plugin action
        organizeChoice(event: Event) {
            const data = (event.currentTarget as HTMLElement).dataset;
            const plugin = plugins().find((plugin) => plugin.id === data.plugin);
            const action = plugin?.actions.find((action) => action.id === data.action);
            if (plugin && action)
                openLibraryOrganization(
                    { kind: 'plugin', plugin_id: plugin.id, action_id: action.id },
                    `${plugin.name} / ${action.name}`,
                );
        },
    };
}
