import type { AppState } from './state';
import type { Action, Plugin } from './types';

// Normalize plugin runtime kinds into user-facing labels
export function pluginRuntimeLabel(plugin: Pick<Plugin, 'kind'>): string {
    return plugin.kind === 'browser'
        ? 'Browser'
        : ['node', 'nodejs'].includes(plugin.kind)
          ? 'Node.js'
          : plugin.kind === 'native'
            ? 'Native'
            : plugin.kind;
}

/** Consumers own navigation/selection validity; check it again after every await. */
export async function loadPluginAction(
    app: AppState,
    plugin: string,
    action: string,
    current: () => boolean,
): Promise<Extract<Action, { kind: 'plugin' }> | null> {
    // Report whether the current plugin choice behavior value is valid
    const valid = () =>
        current() &&
        app.snapshot.plugins.some(
            (p) =>
                p.id === plugin &&
                p.enabled &&
                p.actions.some((a) => a.id === action && a.visible_in_actions_list !== false),
        );
    if (!valid()) return null;
    try {
        const settings = await app.command('plugin_action_defaults', { id: plugin, action }, false);
        return valid() ? { kind: 'plugin', plugin_id: plugin, action_id: action, settings } : null;
    } catch {
        return null;
    }
}

// Toggle the plugin
export async function togglePlugin(app: AppState, event: Event): Promise<void> {
    const el = event.currentTarget as HTMLElement;
    if (!el.dataset.id) return;
    if (el.dataset.enabled !== 'true') {
        window.dispatchEvent(
            new CustomEvent('open-plugin-preflight', { detail: { id: el.dataset.id } }),
        );
        return;
    }
    const snapshot = await app.command('enable_plugin', { id: el.dataset.id, enabled: false });
    app.apply(snapshot);
}
