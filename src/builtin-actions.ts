import { cloneJson } from './json-clone';
import {
    getLibraryOrganization,
    openLibraryOrganization,
    librarySummary,
} from './library-organization';
import definitions from './builtin-catalog.json';
import type AlpineType from '@alpinejs/csp';
import type { Action } from './types';
import type { AppState } from './state';
import type {
    BuiltinAction,
    JsonValue,
    KeyboardMacroRepeatMode,
    MacroInput,
    SavedMacro,
} from '../generated/runtime';
import { parseValue } from './parameter-values';
import { windowWork } from './window-work';

type MacroRunTarget = Pick<SavedMacro, 'id' | 'name'> & {
    deviceId: string | null;
    deviceName: string;
};
type MacroRunField = MacroInput & { supplied: boolean; canOmit: boolean; value: string };
export interface BuiltinField {
    key: string;
    label: string;
    type: string;
    options?: string[];
    help?: string;
    placeholder?: string;
    min?: number;
    max?: number;
    presets?: { value: string; label: string }[];
}
export interface BuiltinDescriptor {
    topLevel?: boolean;
    id: string;
    name: string;
    category: string;
    description: string;
    keywords: string;
    icon: string;
    capability: string;
    release: boolean;
    fields: BuiltinField[];
    defaults: Record<string, unknown>;
    children: boolean;
}
export const builtinCatalog = definitions as BuiltinDescriptor[];
export const nestedBuiltins = builtinCatalog.filter((d) => !d.topLevel);
// Derive a stable picker key for an action kind
export function actionKey(action: Action): string {
    return action.kind === 'builtin' ? action.action.kind : action.kind;
}
let newKeyboardMacroRepeatMode: KeyboardMacroRepeatMode = 'exact';
/** Applies only to future action creation, never to catalog data or saved drafts */
export function setKeyboardMacroNewActionRepeatMode(mode: KeyboardMacroRepeatMode) {
    if (mode !== 'exact' && mode !== 'system')
        throw new Error('Keyboard macro capabilities require a valid new_action_repeat_mode');
    newKeyboardMacroRepeatMode = mode;
}
// Create the default action for a built-in action identifier
export function builtinDefault(id: string): Action | undefined {
    const d = builtinCatalog.find((d) => d.id === id);
    if (!d) return undefined;
    const action: Action = d.topLevel
        ? (structuredClone({ kind: id, ...d.defaults }) as Action)
        : {
              kind: 'builtin',
              action: structuredClone({ kind: id, ...d.defaults }) as BuiltinAction,
          };
    if (action.kind === 'builtin' && action.action.kind === 'keyboard_macro')
        action.action.playback.repeat_mode = newKeyboardMacroRepeatMode;
    return action;
}
// Explain unavailable runtime capabilities for a built-in action
export function capabilityReason(
    action: Action,
    capabilities: Partial<Record<string, string | null>>,
): string {
    if (action.kind !== 'builtin') return '';
    let capability = builtinCatalog.find((d) => d.id === action.action.kind)?.capability || '';
    const a = action.action;
    if ((a.kind === 'mouse_move' || a.kind === 'mouse_drag') && !a.relative)
        capability = 'pointer_absolute';
    if (a.kind === 'search_selection' && a.source === 'clipboard') capability = 'clipboard';
    return capabilities[capability] || '';
}
// Expose the immediate child actions of a composite action
export function actionChildren(action: Action): Action[] {
    if (action.kind === 'macro' || action.kind === 'dial') return action.actions;
    if (action.kind === 'toggle') return [action.on, action.off];
    if (action.kind === 'builtin' && 'children' in action.action) return action.action.children;
    return [];
}
// Replace one child within a composite action
export function setChild(action: Action, index: number, child: Action) {
    if (action.kind === 'toggle') {
        if (index === 0) action.on = child;
        else action.off = child;
    } else actionChildren(action)[index] = child;
}
// Visit an action and its descendants
export function walkAction(action: Action, visit: (action: Action) => void) {
    visit(action);
    actionChildren(action).forEach((child) => walkAction(child, visit));
}
// Register saved macro browsing and execution controls
export function registerSavedActions(Alpine: typeof AlpineType) {
    Alpine.data('savedActions', () => ({
        favoritesOnly: false,
        // Initialize saved macro organization controls
        init() {
            void this.organization.load();
        },
        // Read saved macro organization metadata
        get organization() {
            return getLibraryOrganization(Alpine);
        },
        // Check whether a macro is marked as a favorite
        macroFavorite(macro: SavedMacro) {
            return this.organization.favorite({ kind: 'macro', id: macro.id });
        },
        // Read the tags assigned to a macro
        macroTags(macro: SavedMacro) {
            return librarySummary(
                this.macroFavorite(macro),
                this.organization.tags({ kind: 'macro', id: macro.id }),
            );
        },
        // Open organization controls for the selected macro
        organizeMacro(event: Event) {
            const macro = this.app.snapshot.saved_macros.find(
                (macro) => macro.id === (event.currentTarget as HTMLElement).dataset.id,
            );
            if (macro) openLibraryOrganization({ kind: 'macro', id: macro.id }, macro.name);
        },
        // Read the current application state
        get app() {
            return Alpine.store('app') as AppState;
        },
        query: '',
        sort: 'asc',
        deletion: null as Pick<SavedMacro, 'id' | 'name'> | null,
        deleting: false,
        deletionError: '',
        deletionRevision: 0,
        runDraft: null as MacroRunTarget | null,
        runFields: [] as MacroRunField[],
        runError: '',
        startingMacroId: '',
        // Resolve the macro input dialog element
        get runDialog() {
            return (this as unknown as { $refs: { macroRunDialog?: HTMLDialogElement } }).$refs
                .macroRunDialog;
        },
        // Count available saved macros
        get total() {
            return this.app.snapshot.saved_macros.length;
        },
        // Count all actions within a macro action tree
        stepCount(action: Action): number {
            let count = 0;
            // Increment the count for each visited action
            walkAction(action, () => {
                count++;
            });
            return count - (action.kind === 'macro' || action.kind === 'dial' ? 1 : 0);
        },
        // Find unavailable plugin requirements in a macro
        macroMissingPlugin(macro: SavedMacro): string {
            const visited = new Set([macro.id]);
            let missing = '';
            // Inspect one action tree for unavailable plugin actions
            const check = (root: Action) =>
                // Record missing plugin requirements encountered during traversal
                walkAction(root, (action) => {
                    if (missing) return;
                    if (
                        action.kind === 'plugin' &&
                        !this.app.snapshot.plugins.some(
                            (plugin) =>
                                plugin.id === action.plugin_id &&
                                plugin.actions.some((entry) => entry.id === action.action_id),
                        )
                    ) {
                        missing = action.plugin_id;
                    }
                    if (action.kind === 'builtin' && action.action.kind === 'saved_macro') {
                        const id = action.action.macro_id;
                        if (visited.has(id)) return;
                        visited.add(id);
                        const saved = this.app.snapshot.saved_macros.find(
                            (entry) => entry.id === id,
                        );
                        if (saved) check(saved.action);
                    }
                });
            check(macro.action);
            return missing;
        },
        // Find references to a saved macro
        usage(id: string) {
            // Check whether an action tree invokes the requested macro
            const usedBy = (action: Action) => {
                let found = false;
                // Inspect each nested action for the requested macro identifier
                walkAction(action, (step) => {
                    if (
                        step.kind === 'builtin' &&
                        step.action.kind === 'saved_macro' &&
                        step.action.macro_id === id
                    )
                        found = true;
                });
                return found;
            };
            const bindings = this.app.snapshot.profiles.reduce(
                (n, profile) =>
                    n +
                    profile.pages.reduce(
                        (count, page) =>
                            count +
                            Object.values(page.bindings).filter(
                                (binding) => binding && usedBy(binding.action),
                            ).length,
                        0,
                    ),
                0,
            );
            const macros = this.app.snapshot.saved_macros.filter(
                (macro) => macro.id !== id && usedBy(macro.action),
            ).length;
            return { bindings, macros, total: bindings + macros };
        },
        // Summarize where a saved macro is used
        usageLabel(id: string) {
            const { bindings, macros } = this.usage(id);
            return (
                [
                    bindings ? `${bindings} ${bindings === 1 ? 'binding' : 'bindings'}` : '',
                    macros ? `${macros} ${macros === 1 ? 'macro' : 'macros'}` : '',
                ]
                    .filter(Boolean)
                    .join(' · ') || 'Not in use'
            );
        },
        // Name the macro selected for deletion review
        get deletionTitle() {
            return this.deletion ? `Delete “${this.deletion.name}”?` : 'Delete macro?';
        },
        // Explain the effects of deleting the selected macro
        get deletionDescription() {
            if (!this.deletion) return '';
            return this.usage(this.deletion.id).total
                ? `Used by ${this.usageLabel(this.deletion.id)}. Calls to this macro will stop working after deletion. You can undo this change.`
                : 'This removes the saved macro from your library. You can undo this change.';
        },
        // Filter and order the saved macro list
        get macros() {
            return [...this.app.snapshot.saved_macros]
                .filter((m) => !this.favoritesOnly || this.macroFavorite(m))
                .filter((m) =>
                    `${m.name} ${this.organization.tags({ kind: 'macro', id: m.id }).join(' ')}`
                        .toLocaleLowerCase()
                        .includes(this.query.trim().toLocaleLowerCase()),
                )
                .sort(
                    (a, b) =>
                        (a.name.localeCompare(b.name) || a.id.localeCompare(b.id)) *
                        (this.sort === 'asc' ? 1 : -1),
                );
        },
        // Open the editor for a new macro
        newMacro() {
            window.dispatchEvent(new CustomEvent('open-named-macro', { detail: {} }));
        },
        // Open the selected saved macro in the editor
        editMacro(event: Event) {
            const macro = this.app.snapshot.saved_macros.find(
                (m) => m.id === (event.currentTarget as HTMLElement).dataset.id,
            );
            if (macro)
                window.dispatchEvent(
                    new CustomEvent('open-named-macro', {
                        detail: cloneJson(macro),
                    }),
                );
        },
        // Prepare a saved macro run and collect required inputs
        async runMacro(event: Event) {
            if (this.startingMacroId || this.runDialog?.open) return;
            if (!this.app.nativeRendering) {
                this.app.notify(
                    'Running saved macros requires the desktop application.',
                    'warning',
                );
                return;
            }
            const device = this.app.snapshot.devices.find(
                (item) => item.id === this.app.deviceId && item.connected,
            );
            const macro = this.app.snapshot.saved_macros.find(
                (item) => item.id === (event.currentTarget as HTMLElement).dataset.id,
            );
            if (!macro || this.macroMissingPlugin(macro)) return;
            const target = {
                id: macro.id,
                name: macro.name,
                deviceId: device?.id ?? null,
                deviceName: device?.name ?? '',
            };
            if (!macro.inputs?.length) {
                await this.queueMacroRun(target, {});
                return;
            }
            this.runDraft = target;
            // Build an input field for a declared macro parameter
            this.runFields = macro.inputs.map((input) => {
                const canOmit = !input.required || input.default !== null;
                return {
                    ...input,
                    canOmit,
                    supplied: !canOmit,
                    value: String(input.default ?? ''),
                };
            });
            this.runError = '';
            this.runDialog?.showModal();
        },
        // Close the macro input dialog and clear pending run state
        closeMacroRun() {
            if (this.startingMacroId) return;
            this.runDialog?.close();
            this.runDraft = null;
            this.runFields = [];
            this.runError = '';
        },
        // Validate entered macro inputs before queuing the run
        async submitMacroRun() {
            if (!this.runDraft || this.startingMacroId) return;
            const values: [string, JsonValue][] = [];
            for (const field of this.runFields) {
                if (!field.supplied) continue;
                try {
                    values.push([field.name, parseValue(field.type, field.value)]);
                } catch (error) {
                    this.runError = field.label + ' · ' + (error as Error).message;
                    return;
                }
            }
            await this.queueMacroRun(this.runDraft, Object.fromEntries(values));
        },
        // Queue a macro run for the reviewed target and input values
        async queueMacroRun(target: MacroRunTarget, values: Record<string, JsonValue>) {
            this.startingMacroId = target.id;
            this.runError = '';
            try {
                await windowWork.run('run saved macro', () =>
                    this.app.bridge.command('run_saved_macro', {
                        id: target.id,
                        deviceId: target.deviceId,
                        values,
                    }),
                );
            } catch (error) {
                if (this.runDialog?.open) this.runError = String(error);
                else this.app.notify(String(error), 'error');
                return;
            } finally {
                this.startingMacroId = '';
            }
            this.closeMacroRun();
            this.app.notify(`Queued “${target.name}”.`, 'success');
        },
        // Open deletion confirmation for the selected macro
        async deleteMacro(event: Event) {
            const macro = this.app.snapshot.saved_macros.find(
                (macro) => macro.id === (event.currentTarget as HTMLElement).dataset.id,
            );
            this.deletion = macro ? { id: macro.id, name: macro.name } : null;
            this.deletionError = '';
            this.deletionRevision = this.app.configurationRevision;
            // Show deletion confirmation after its content is rendered
            this.$nextTick(() => {
                const dialog = this.$refs.macroDeleteDialog as HTMLDialogElement | undefined;
                if (this.deletion && dialog && !dialog.open) dialog.showModal();
            });
        },
        // Cancel the pending macro deletion
        cancelDeletion() {
            if (this.deleting) return;
            (this.$refs.macroDeleteDialog as HTMLDialogElement | undefined)?.close();
            this.deletion = null;
            this.deletionError = '';
        },
        // Delete the confirmed macro through the application state
        async confirmDeletion() {
            if (!this.deletion || this.deleting) return;
            const id = this.deletion.id;
            this.deleting = true;
            try {
                await this.app.edit(
                    [
                        {
                            kind: 'delete_macro',
                            id,
                        },
                    ],
                    'Delete named macro',
                    this.deletionRevision,
                );
                this.deleting = false;
                this.cancelDeletion();
                this.app.notify('Macro deleted. Use Undo to restore it.', 'success');
            } catch (error) {
                this.deletionError = String(error);
            } finally {
                this.deleting = false;
            }
        },
        // Undo the latest saved-macro edit
        async undo() {
            try {
                await this.app.history('undo');
            } catch {}
        },
        // Redo the last undone saved-macro edit
        async redo() {
            try {
                await this.app.history('redo');
            } catch {}
        },
        // Close pending macro dialogs during controller disposal
        destroy() {
            this.runDialog?.close();
            (this.$refs.macroDeleteDialog as HTMLDialogElement | undefined)?.close();
        },
    }));
}
