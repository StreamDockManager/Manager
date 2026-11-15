import { isKeyboardNavigationControl } from './keyboard-navigation';
import type { Binding, Profile } from './types';
import type { Edit } from '../generated/runtime';
import { cloneJson } from './json-clone';
import { hasBindingContent } from './bindings';

export type BindingScope = 'page' | 'profile' | 'model';

/** Same-model profiles share stable control IDs; never match controls by label or index */
export function bindingScopePlan(
    profiles: Profile[],
    profileId: string,
    pageId: string,
    controlId: string,
    scope: BindingScope,
) {
    const source = profiles.find((profile) => profile.id === profileId);
    if (!source || !source.pages.some((page) => page.id === pageId) || !controlId)
        return {
            targets: [],
            signature: '',
            overwritten: 0,
            profileCount: 0,
            profileId,
            pageId,
            scope,
        };
    const selected =
        scope === 'model'
            ? profiles.filter((profile) => profile.model_id === source.model_id)
            : [source];
    const targets = selected.flatMap((profile) =>
        profile.pages
            .filter(
                (page) =>
                    (scope !== 'page' || page.id === pageId) &&
                    (scope === 'page' || !isKeyboardNavigationControl(profile, page.id, controlId)),
            )
            .map((page) => ({
                profileId: profile.id,
                pageId: page.id,
                binding: page.bindings[controlId] || null,
            })),
    );
    return {
        targets,
        profileId,
        pageId,
        scope,
        signature: JSON.stringify([source.model_id, controlId, targets]),
        overwritten: targets.filter((target) => hasBindingContent(target.binding || undefined))
            .length,
        profileCount: selected.length,
    };
}

// Construct binding edits restricted to the selected scope
export function scopedBindingEdits(
    plan: ReturnType<typeof bindingScopePlan>,
    controlId: string,
    binding: Binding,
): Edit[] {
    if (!plan.targets.length)
        throw new Error('The selected control or page is no longer available.');
    if (plan.scope === 'page')
        return [
            {
                kind: 'bindings',
                profile_id: plan.profileId,
                page_id: plan.pageId,
                bindings: { [controlId]: cloneJson(binding) },
            },
        ];
    return [
        {
            kind: 'assign_control',
            profile_id: plan.profileId,
            page_id: plan.pageId,
            control_id: controlId,
            scope: plan.scope,
            binding: cloneJson(binding),
        },
    ];
}
