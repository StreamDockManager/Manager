import type { Action, Binding, Control, Plugin } from './types';
import { keyLogicActionReason } from './key-logic';

// Check whether a binding contains behavior or display content
export function hasBindingContent(binding: Binding | undefined): boolean {
    return Boolean(
        binding &&
        (binding.background_source === 'palette' ||
            binding.dial_response ||
            binding.live_label?.length ||
            binding.title ||
            binding.asset ||
            binding.action.kind !== 'none' ||
            binding.appearance ||
            binding.plugin_appearance ||
            binding.composition ||
            (binding.feedback && binding.feedback !== 'off') ||
            binding.disabled ||
            (!['#242424', '#19202c'].includes(binding.color.toLowerCase()) &&
                !/^#[\da-f]{6}00$/i.test(binding.color)) ||
            (binding.media &&
                (binding.media.fit !== 'fill' ||
                    binding.media.speed !== 1 ||
                    binding.media.trim_start_ms !== 0 ||
                    binding.media.trim_end_ms !== null ||
                    binding.media.crop.some((value, index) => value !== [0, 0, 1, 1][index])))),
    );
}

// Explain incompatibilities between a binding and its hardware control
export function bindingCompatibility(
    binding: Binding,
    control: Control,
    plugins: Plugin[],
): string {
    if (!binding.disabled) {
        const reason = keyLogicActionReason(binding.action, plugins);
        if (reason) return reason;
    }
    if (control.kind === 'encoder' && !['none', 'dial', 'events'].includes(binding.action.kind))
        return 'Unsupported shared dial binding. Assign separate left, right and press actions.';
    if (control.kind === 'display' && binding.action.kind !== 'none')
        return 'This display accepts appearance settings and has no action input.';
    if (
        (binding.background_source === 'palette' ||
            binding.asset ||
            binding.appearance ||
            binding.plugin_appearance ||
            binding.composition ||
            (binding.feedback && binding.feedback !== 'off')) &&
        !control.image_width &&
        !control.feedback_surface
    )
        return `${control.label} has no display or feedback surface for this appearance.`;
    // Validate nested action requirements for the target control
    const check = (action: Action, inDial = false): string => {
        if (action.kind === 'dial' && control.kind !== 'encoder')
            return 'Separate dial actions require an encoder control.';
        if (action.kind === 'plugin' && !binding.disabled) {
            const plugin = plugins.find((plugin) => plugin.id === action.plugin_id);
            const choice = plugin?.actions.find((choice) => choice.id === action.action_id);
            if (choice && !choice.control_types.includes(control.kind))
                return `${choice.name} does not support ${control.label}.`;
        }
        if (
            action.kind === 'builtin' &&
            (inDial || !control.semantics.includes('release')) &&
            [
                'hold_keys',
                'momentary_page',
                'repeat_held',
                'gesture',
                'press_release',
                'wait_release',
            ].includes(action.action.kind)
        )
            return 'This action requires a physical button with release events.';
        if (action.kind === 'builtin' && 'children' in action.action) {
            for (const child of action.action.children) {
                const error = check(child, inDial);
                if (error) return error;
            }
        }
        if (action.kind === 'macro' || action.kind === 'dial') {
            for (const child of action.actions) {
                const error = check(child, inDial || action.kind === 'dial');
                if (error) return error;
            }
        }
        if (action.kind === 'toggle') return check(action.on, inDial) || check(action.off, inDial);
        return '';
    };
    return check(binding.action);
}
