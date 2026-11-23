import { cloneJson } from './json-clone';
import type { Action, Binding } from './types';
import { ActionDraftTree, type ActionNode, type ActionDraftRecovery } from './action-draft-tree';
import { assertCurrentMediaOptions, defaultMedia } from './bridge';
import { defaultAppearance } from './features';
import { DEFAULT_BACKGROUND } from './background-color';
import { hasBindingContent } from './bindings';

export interface BindingDraft {
    title: string;
    liveLabel: NonNullable<Binding['live_label']>;
    color: string;
    customBackground: boolean;
    backgroundSource: string;
    actionTree: Pick<ActionDraftTree, keyof ActionDraftTree>;
    asset: Binding['asset'];
    media: NonNullable<Binding['media']>;
    appearance: NonNullable<Binding['appearance']>;
    appearanceEnabled: boolean;
    pluginAppearance: Binding['plugin_appearance'];
    composition: Binding['composition'];
    feedback: string;
    disabled: boolean;
    activationPolicy: NonNullable<Binding['activation_policy']>;
    dialCurve: '' | NonNullable<Binding['dial_response']>['curve'];
    dialMaxSteps: number;
}

/** The recovery payload and dirty watchers use the same explicit editor-owned inventory */
export const bindingDraftFields = [
    'title',
    'liveLabel',
    'color',
    'customBackground',
    'backgroundSource',
    'actionTree',
    'asset',
    'media',
    'appearance',
    'appearanceEnabled',
    'pluginAppearance',
    'composition',
    'feedback',
    'disabled',
    'activationPolicy',
    'dialCurve',
    'dialMaxSteps',
] as const satisfies readonly (keyof BindingDraft)[];
// A newly added editor field must participate in persistence and dirty tracking
type CompleteInventory<T extends never> = T;
type MissingDraftField = CompleteInventory<
    Exclude<keyof BindingDraft, (typeof bindingDraftFields)[number]>
>;

const clone = cloneJson;

export type BindingDraftRecovery = Omit<BindingDraft, 'actionTree'> & ActionDraftRecovery;

// Capture the editable binding and active action lane for recovery
export function bindingDraftValue(editor: BindingDraft, active?: ActionNode): BindingDraftRecovery {
    const { actionTree, ...fields } = Object.fromEntries(
        bindingDraftFields.map((key) => [key, editor[key]]),
    ) as unknown as BindingDraft;
    return { ...fields, ...actionTree.recovery(active) };
}

// Load a binding into independent editor draft state
export function loadBindingDraft(
    binding: Binding | undefined,
    defaultDialCurve: BindingDraft['dialCurve'] = '',
): BindingDraft {
    assertCurrentMediaOptions(binding?.media);
    return {
        title: binding?.title || '',
        liveLabel: clone(binding?.live_label || []),
        color: binding && hasBindingContent(binding) ? binding.color : DEFAULT_BACKGROUND,
        customBackground: Boolean(binding?.custom_background),
        backgroundSource: binding?.background_source || 'custom',
        actionTree: new ActionDraftTree(binding?.action),
        asset: binding?.asset,
        media: clone(binding?.media || defaultMedia()),
        appearance: clone(binding?.appearance || defaultAppearance()),
        appearanceEnabled: Boolean(binding?.appearance),
        pluginAppearance: clone(binding?.plugin_appearance),
        composition: clone(binding?.composition),
        feedback: binding?.feedback || 'off',
        disabled: binding?.disabled || false,
        activationPolicy: binding?.activation_policy || 'queue',
        // A saved binding without a curve explicitly uses Original response
        dialCurve: binding ? binding.dial_response?.curve || '' : defaultDialCurve,
        dialMaxSteps: binding?.dial_response?.max_steps || 100,
    };
}

// Restore a validated recovery draft into the binding editor
export function restoreBindingDraft(
    value: Record<string, unknown>,
    fallback: BindingDraft,
): BindingDraft {
    assertCurrentMediaOptions(value.media);
    const recoveryKeys = new Set<string>([
        ...bindingDraftFields.filter((key) => key !== 'actionTree'),
        'actionDraft',
        'dialActions',
        'dialActionIndex',
        'dialLinkedTurns',
        'eventActions',
        'eventActionIndex',
        'eventMultiPressMs',
        'eventHoldMs',
    ]);
    if (Object.keys(value).some((key) => !recoveryKeys.has(key)))
        throw new Error(
            'Unsupported recovered binding draft format. The saved profile was not changed.',
        );
    if (fallback.actionTree.currentLanes && value.dialActions == null && value.eventActions == null)
        throw Error(
            'Unsupported shared dial draft. Separate left, right and press actions are required.',
        );
    const restored = clone(
        Object.fromEntries(
            bindingDraftFields
                .filter((key) => key !== 'actionTree' && Object.hasOwn(value, key))
                .map((key) => [key, value[key]]),
        ),
    );
    return {
        ...(Object.fromEntries(
            bindingDraftFields.map((key) => [key, fallback[key]]),
        ) as unknown as BindingDraft),
        ...restored,
        actionTree: ActionDraftTree.restore(value),
    };
}

/** Visual-only projection: unfinished executable input must not freeze appearance edits.
 * The action tree retains its last parseable action; saving still uses strict commit validation
 */
export function previewBindingDraft(draft: BindingDraft): Binding {
    const max = Number(draft.dialMaxSteps);
    return projectBindingDraft(
        draft,
        draft.actionTree.preview(),
        Number.isInteger(max) && max >= 1 && max <= 100 ? max : 100,
    );
}

// Commit a binding draft through its action-tree validation
export function commitBindingDraft(draft: BindingDraft): Binding {
    if (
        draft.dialCurve &&
        (!Number.isInteger(Number(draft.dialMaxSteps)) ||
            Number(draft.dialMaxSteps) < 1 ||
            Number(draft.dialMaxSteps) > 100)
    )
        throw Error('Dial step cap must be between 1 and 100');
    return projectBindingDraft(draft, commitDraftAction(draft));
}

// Project editable binding fields into a binding value
function projectBindingDraft(
    draft: BindingDraft,
    action: Action,
    maxSteps = Number(draft.dialMaxSteps),
): Binding {
    return {
        title: draft.title,
        live_label: draft.liveLabel?.length ? clone(draft.liveLabel) : undefined,
        color: draft.color,
        custom_background: draft.customBackground || undefined,
        background_source: draft.backgroundSource === 'palette' ? 'palette' : undefined,
        action,
        asset: draft.asset,
        media: clone(draft.media),
        appearance: draft.appearanceEnabled ? clone(draft.appearance) : undefined,
        feedback: draft.feedback,
        composition: clone(draft.composition),
        plugin_appearance: clone(draft.pluginAppearance),
        disabled: draft.disabled,
        activation_policy: draft.activationPolicy,
        dial_response: draft.dialCurve
            ? { curve: draft.dialCurve, max_steps: maxSteps }
            : undefined,
    };
}

// Commit the current action tree from a binding draft
export function commitDraftAction(draft: Pick<BindingDraft, 'actionTree'>): Action {
    return draft.actionTree.commit();
}
