import { actionChildren } from './builtin-actions';
import type { Action, Plugin } from './types';

/** The selection is inside a gesture, not replacing the gesture itself */
export function insideKeyLogic(root: Action, path: string): boolean {
    let action = root;
    for (const part of path === '' ? [] : path.split('.')) {
        if (action.kind === 'builtin' && action.action.kind === 'gesture') return true;
        const child = actionChildren(action)[Number(part)];
        if (!child) return false;
        action = child;
    }
    return false;
}

// Explain why a plugin cannot participate in key logic
export function keyLogicPluginReason(plugin: Plugin, actionId: string): string {
    if (plugin.dialect !== 'streamdeck' || plugin.capabilities) return '';
    const action = plugin.actions.find((action) => action.id === actionId);
    if (action?.supported_in_key_logic_actions === false)
        return `${action.name} does not support key logic.`;
    if (action?.supported_in_multi_actions === false)
        return `${action.name} cannot run in Manager key logic: nested plugin execution also requires multi-action support.`;
    return '';
}

/** Reused by draft save validation, including pasted JSON and saved macro calls */
export function keyLogicActionReason(
    action: Action,
    plugins: Plugin[],
    macros: { id: string; action: Action }[] = [],
    inside = false,
): string {
    const checked = new Set<string>();
    const active = new Set<string>();
    let visits = 0;
    // Inspect nested actions for key-logic incompatibilities
    const visit = (action: Action, inside: boolean, depth: number): string => {
        if (++visits > 10_000) return 'Action eligibility check exceeds 10000 nodes.';
        if (depth > 8) return 'Expanded action nesting exceeds 8.';
        inside ||= action.kind === 'builtin' && action.action.kind === 'gesture';
        if (inside && action.kind === 'plugin') {
            const plugin = plugins.find((plugin) => plugin.id === action.plugin_id);
            if (plugin) {
                const reason = keyLogicPluginReason(plugin, action.action_id);
                if (reason) return reason;
            }
        }
        if (action.kind === 'builtin' && action.action.kind === 'saved_macro') {
            const id = action.action.macro_id;
            const saved = macros.find((saved) => saved.id === id);
            if (saved) {
                if (active.has(id)) return 'Saved macro reference cycle.';
                const key = JSON.stringify([id, inside, depth + 1]);
                if (!checked.has(key)) {
                    active.add(id);
                    const reason = visit(saved.action, inside, depth + 1);
                    active.delete(id);
                    if (reason) return reason;
                    checked.add(key);
                }
            }
        }
        for (const child of actionChildren(action)) {
            const reason = visit(child, inside, depth + 1);
            if (reason) return reason;
        }
        return '';
    };
    return visit(action, inside, 0);
}
