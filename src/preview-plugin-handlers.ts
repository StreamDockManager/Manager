import type { PreviewHandlers } from './preview-handlers';
import type { PreviewWorkspace } from './preview-workspace';
import type { CommandParameters, CommandResult } from '../generated/commands';
import type { Binding, Snapshot } from '../generated/runtime';
import { previewContent, type PreviewHistory } from './preview-workspace';
import { defaultMedia } from './bridge';

// Create the preview plugin handlers
export function createPreviewPluginHandlers(workspace: PreviewWorkspace, history: PreviewHistory) {
    const { s, before, commit } = workspace;
    // Change the plugin
    const changePlugin = (
        args: NonNullable<CommandParameters<'delete_plugin'>[0]>,
        remove: boolean,
    ): CommandResult<'delete_plugin'> => {
        const id = String(args.id);
        const plugin = s.plugins.find((plugin) => plugin.id === id);
        if (!plugin) throw Error('Plugin not installed');
        const purge = remove && !args.keepData;
        // Clear the action settings
        const clearActionSettings = (action: Binding['action']) => {
            if (action.kind === 'plugin' && action.plugin_id === id) action.settings = {};
            else if (action.kind === 'macro' || action.kind === 'dial')
                action.actions.forEach(clearActionSettings);
            else if (action.kind === 'toggle') {
                clearActionSettings(action.on);
                clearActionSettings(action.off);
            } else if (action.kind === 'builtin' && 'children' in action.action)
                action.action.children.forEach(clearActionSettings);
        };
        // Clear the retained preview plugin handlers value
        const clear = (workspace: Pick<Snapshot, 'profiles' | 'saved_macros' | 'styles'>) => {
            for (const profile of workspace.profiles)
                for (const page of profile.pages)
                    for (const [control, binding] of Object.entries(page.bindings)) {
                        if (!binding) continue;
                        if (binding.action.kind === 'plugin' && binding.action.plugin_id === id) {
                            if (purge) {
                                delete page.bindings[control];
                                continue;
                            }
                            Object.assign(binding, {
                                color: '#c61e3200',
                                media: defaultMedia(),
                            });
                            delete binding.custom_background;
                            delete binding.asset;
                            delete binding.appearance;
                            delete binding.plugin_appearance;
                            delete binding.composition;
                        } else {
                            if (binding.plugin_appearance?.plugin_id === id)
                                delete binding.plugin_appearance;
                            if (binding.composition) {
                                binding.composition.layers = binding.composition.layers.filter(
                                    (layer) =>
                                        !(
                                            layer.source.kind === 'plugin' &&
                                            layer.source.plugin_id === id
                                        ),
                                );
                                if (!binding.composition.layers.length) delete binding.composition;
                            }
                        }
                        if (purge) clearActionSettings(binding.action);
                    }
            workspace.styles = workspace.styles?.filter(
                (style) =>
                    style.plugin_appearance?.plugin_id !== id &&
                    !style.composition?.layers.some(
                        (layer) => layer.source.kind === 'plugin' && layer.source.plugin_id === id,
                    ),
            );
            if (purge) workspace.saved_macros.forEach((macro) => clearActionSettings(macro.action));
        };
        clear(s);
        for (const entry of history.values()) {
            clear(entry.before);
            clear(entry.after);
        }
        if (remove) s.plugins = s.plugins.filter((entry) => entry !== plugin);
        else plugin.appearance = 'follow-ui';
        if (JSON.stringify(before) !== JSON.stringify(previewContent(s)))
            s.configuration_revision++;
        return commit().snapshot;
    };
    const pluginHandlers = {
        // Simulate reset plugin styles in the browser without calling native services
        reset_plugin_styles: (args) => changePlugin(args, false),
        // Simulate delete plugin in the browser without calling native services
        delete_plugin: (args) => changePlugin(args, true),
        // Simulate plugin action defaults in the browser without calling native services
        plugin_action_defaults: (args) => {
            return {};
        },
        // Simulate credential status in the browser without calling native services
        credential_status: (args) => {
            return 'locked';
        },
        // Simulate write credential in the browser without calling native services
        write_credential: (args) => {
            throw Error('The OS credential store is unavailable in browser preview.');
        },
        // Simulate clear credential in the browser without calling native services
        clear_credential: (args) => {
            throw Error('The OS credential store is unavailable in browser preview.');
        },
        // Simulate retry update job in the browser without calling native services
        retry_update_job: (args) => {
            throw Error('Resumable downloads require the native desktop application.');
        },
        // Simulate plugin update policy in the browser without calling native services
        plugin_update_policy: (args) => {
            throw Error('Plugin rollback requires the native desktop application.');
        },
        // Simulate save plugin update policy in the browser without calling native services
        save_plugin_update_policy: (args) => {
            throw Error('Plugin rollback requires the native desktop application.');
        },
        // Simulate plugin backups in the browser without calling native services
        plugin_backups: (args) => {
            throw Error('Plugin rollback requires the native desktop application.');
        },
        // Simulate delete plugin backups in the browser without calling native services
        delete_plugin_backups: (args) => {
            throw Error('Plugin rollback requires the native desktop application.');
        },
        // Simulate prepare plugin rollback in the browser without calling native services
        prepare_plugin_rollback: (args) => {
            throw Error('Plugin rollback requires the native desktop application.');
        },
        // Simulate commit plugin rollback in the browser without calling native services
        commit_plugin_rollback: (args) => {
            throw Error('Plugin rollback requires the native desktop application.');
        },
        // Simulate cancel plugin rollback in the browser without calling native services
        cancel_plugin_rollback: (args) => {
            return;
        },
    } satisfies PreviewHandlers;
    return pluginHandlers;
}
