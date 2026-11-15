import { actionChildren } from './builtin-actions';
import { validateKeyboardMacro, validateKeyboardMacroDraft } from './keyboard-macro-draft';
import type { Action, Draft, Edit, Profile } from '../generated/runtime';

/** Preview mirrors native ingress: only local editor recovery/render may be unconfigured */
export function assertKeyboardMacroAction(action: Action, editorDraft = false) {
    if (action.kind === 'builtin' && action.action.kind === 'keyboard_macro') {
        const error = editorDraft
            ? validateKeyboardMacroDraft(action.action)
            : validateKeyboardMacro(action.action);
        if (error) throw Error(error);
    }
    actionChildren(action).forEach((child) => assertKeyboardMacroAction(child, editorDraft));
}
// Validate keyboard macro actions throughout a profile
export function assertKeyboardMacroProfile(profile: Profile) {
    for (const page of profile.pages)
        for (const binding of Object.values(page.bindings))
            if (binding) assertKeyboardMacroAction(binding.action);
}
// Validate keyboard macro changes before applying profile edits
export function assertKeyboardMacroEdits(edits: Edit[]) {
    for (const edit of edits) {
        if (edit.kind === 'save_macro') assertKeyboardMacroAction(edit.value.action);
        else if (edit.kind === 'save_profile') assertKeyboardMacroProfile(edit.profile);
        else if (edit.kind === 'assign_control') assertKeyboardMacroAction(edit.binding.action);
        else if (edit.kind === 'bindings')
            for (const binding of Object.values(edit.bindings))
                if (binding) assertKeyboardMacroAction(binding.action);
    }
}
// Validate recovered keyboard macro data before acceptance
export function assertKeyboardMacroRecovery(draft: Draft) {
    if (draft.base) assertKeyboardMacroAction(draft.base.action);
    const value = draft.value as Record<string, unknown>;
    const actionDrafts = [
        value.actionDraft,
        ...(Array.isArray(value.dialActions) ? value.dialActions : []),
    ];
    for (const item of actionDrafts) {
        const action = (item as { action?: Action } | undefined)?.action;
        if (action) assertKeyboardMacroAction(action, true);
    }
}
