import type { Action, Plugin } from './types';
import { cloneJson } from './json-clone';
import { actionChildren } from './builtin-actions';

/** The other turn reverses relative built-ins; plugins retain signed native dial events */
export function oppositeTurnAction(value: Action): Action {
    const action = cloneJson(value);
    // Reverse a default directional action for the opposite dial turn
    const reverse = (node: Action) => {
        if (node.kind === 'volume') node.delta = -node.delta;
        if (node.kind === 'navigate')
            node.direction = node.direction === 'previous' ? 'next' : 'previous';
        if (node.kind === 'builtin') {
            const builtin = node.action;
            if (builtin.kind === 'brightness_step') builtin.delta = -builtin.delta;
            if (builtin.kind === 'mouse_scroll') {
                builtin.x = builtin.x === 0 ? 0 : -builtin.x;
                builtin.y = builtin.y === 0 ? 0 : -builtin.y;
            }
            if (builtin.kind === 'page_step')
                builtin.direction = builtin.direction === 'previous' ? 'next' : 'previous';
        }
        actionChildren(node).forEach(reverse);
    };
    reverse(action);
    return action;
}

/** Vendor encoder artwork is independent of the assignment model */
export function dialActionPresentation(action: Action | undefined, plugins: Plugin[]) {
    if (action?.kind !== 'plugin') return { icon: '' };
    const plugin = plugins.find((plugin) => plugin.id === action.plugin_id);
    if (plugin?.dialect !== 'streamdeck' || plugin.capabilities) return { icon: '' };
    return {
        icon:
            plugin.actions.find((candidate) => candidate.id === action.action_id)?.encoder_icon ||
            '',
    };
}

// Return the action only when it is a dial container
export function selectedDial(action: Action | undefined) {
    return action?.kind === 'dial' ? action : undefined;
}
