import { editKeyboardMacro } from './keyboard-macro-editor';
import { keyboardMacroSummary } from './keyboard-macro-draft';
import {
    pluginStateChoices,
    setPluginDesiredState,
    pluginMultiActionImage,
} from './plugin-multi-actions';
import { dialActionDefault } from './dial-defaults';
import { insideKeyLogic, keyLogicActionReason } from './key-logic';
import { cloneJson } from './json-clone';
import * as fileDialogs from './window-dialogs';
import { ownWindowMethods, WindowQuiescingError } from './window-work';
import { registerCloseDraft } from './close-releases';
import { MacroDebugger } from './macro-debugger';
import { visibleMacroRows } from './macro-rows';
import { PluginSettingDrafts } from './plugin-setting-drafts';
import { validateMacroDraft, validateMacroInputs } from './macro-inputs';
import {
    builtinCatalog,
    nestedBuiltins,
    builtinDefault,
    actionKey,
    actionChildren,
    setChild,
    capabilityReason,
    type BuiltinField,
} from './builtin-actions';
import type AlpineType from '@alpinejs/csp';
import { editPluginSettings } from './plugin-settings';
import { openActionPicker } from './action-picker';
import { actionLibrarySelection } from './action-library-selection';
import type { Action } from './types';
import type { AppState } from './state';
import type {
    MacroInput,
    MacroArgument,
    BuiltinAction,
    Condition,
    DebugMode,
    JsonValue,
} from '../generated/runtime';
import { initialValue, parseValue } from './parameter-values';
// Construct the initial action draft for the selected action kind
export function defaultAction(kind: string): Action {
    return (
        builtinDefault(kind) ||
        (kind === 'plugin'
            ? { kind: 'plugin', plugin_id: '', action_id: '', settings: {} }
            : { kind: 'none' })
    );
}
interface Row {
    id: string;
    path: string;
    action: Action;
    depth: number;
    label: string;
    summary: string;
    icon: string;
    image?: string;
    hasChildren: boolean;
    value: string;
    valueLabel: string;
    args: string;
    colors: string[];
    settings: string;
    choice: string;
    fields: (BuiltinField & { value: unknown })[];
}
const topLevelValues: Partial<Record<Action['kind'], string>> = {
    delay: 'Delay (ms)',
    hotkey: 'Keys',
    open_url: 'URL',
    volume: 'Volume change (%)',
    brightness: 'Brightness (%)',
};
const topLevelPathFields = {
    open_path: {
        key: 'path',
        label: 'Path',
        help: 'Choose an existing file or folder to open in its default application.',
    },
    application: {
        key: 'path',
        label: 'Application path',
        help: 'Choose an installed application or enter its path.',
    },
    command: {
        key: 'program',
        label: 'Program',
        help: 'Choose an executable or enter a command available on PATH.',
    },
} as const;
const filePicker = { mode: 'file', label: 'Choose file…' } as const;
const folderPicker = { mode: 'folder', label: 'Choose folder…' } as const;
const fileOrFolder = [filePicker, folderPicker];
const applicationPicker = [{ mode: 'file', label: 'Choose application…' }] as const;
const programPicker = [{ mode: 'file', label: 'Choose executable…' }] as const;
const destinationPicker = [{ mode: 'save', label: 'Choose destination…' }] as const;
const textComparisons = [
    { value: 'equals', label: 'Is' },
    { value: 'not_equals', label: 'Is not' },
    { value: 'contains', label: 'Contains' },
];
const conditionComparisons: Record<string, { value: string; label: string }[]> = {
    clipboard: textComparisons,
    application: textComparisons,
    file: [
        { value: 'exists', label: 'Exists' },
        { value: 'missing', label: 'Does not exist' },
    ],
    process: [
        { value: 'exists', label: 'Is running' },
        { value: 'missing', label: 'Is not running' },
    ],
};
type ConditionTargetList = {
    loaded: boolean;
    loading: boolean;
    options: { value: string; label: string }[];
    error: string;
};
const emptyConditionTargets: ConditionTargetList = {
    loaded: false,
    loading: false,
    options: [],
    error: '',
};
const actionPathPickers: Record<
    string,
    Record<string, readonly { mode: 'file' | 'folder' | 'save'; label: string }[]>
> = {
    open_path: { path: fileOrFolder },
    application: { path: applicationPicker },
    command: { program: programPicker },
    reveal_file: { path: fileOrFolder },
    open_with: { application: applicationPicker, path: fileOrFolder },
    open_terminal: { program: programPicker, path: [folderPicker] },
    command_result: { program: programPicker, path: [folderPicker] },
    file_operation: { source: fileOrFolder, destination: destinationPicker },
    write_file: { path: destinationPicker },
};
// Unwrap the action variant into its editable field values
function value(action: Action) {
    if ('milliseconds' in action) return String(action.milliseconds);
    if ('keys' in action) return action.keys.join(' + ');
    if ('url' in action) return action.url;
    if ('delta' in action) return String(action.delta);
    if ('path' in action) return action.path;
    if ('value' in action) return String(action.value);
    return '';
}
type EditorMagics = {
    $el: HTMLElement;
    $refs: Record<string, HTMLElement>;
    $nextTick(callback: () => void): void;
    $watch(key: string, callback: () => void): void;
};
// Register the shared macro and action editor controllers
export function registerMacroEditor(Alpine: typeof AlpineType) {
    const rowModels = new WeakMap<object, { action: Action; revision: number; rows: Row[] }>();
    // Create independent draft state for a macro or action editor
    const factory = (macro: boolean) => () => ({
        action: defaultAction(macro ? 'macro' : 'none'),
        pluginSettingDrafts: new PluginSettingDrafts(),
        pluginJsonDrafts: {} as Record<string, { text: string; error: string }>,
        inspectorSession: undefined as AbortController | undefined,
        debugRun: new MacroDebugger(),
        breakpointIds: [] as string[],
        // Return selected debug breakpoint for the current macro editor view
        get selectedDebugBreakpoint() {
            return this.breakpointIds.includes(this.selectedId);
        },
        // Toggle the breakpoint on the currently selected action node
        toggleDebugBreakpoint() {
            if (!this.selectedRow) return;
            if (this.selectedDebugBreakpoint)
                this.breakpointIds = this.breakpointIds.filter((id) => id !== this.selectedId);
            else if (this.breakpointIds.length < 128) this.breakpointIds.push(this.selectedId);
            else this.app.notify('A test supports up to 128 breakpoints.', 'warning');
        },
        // Resolve the referenced native dialog element
        dialog(ref: string) {
            return (this as unknown as EditorMagics).$refs[ref] as HTMLDialogElement | undefined;
        },
        // Return context profile for the current macro editor view
        get contextProfile() {
            if (!this.namedMode) return this.app.profile;
            return (
                this.app.snapshot.profiles.find(
                    (profile) => profile.id === this.contextProfileId,
                ) ||
                this.app.profile ||
                this.app.snapshot.profiles[0]
            );
        },
        // Return context page for the current macro editor view
        get contextPage() {
            const profile = this.contextProfile;
            if (profile?.id === this.app.profile?.id && this.app.page) return this.app.page;
            return (
                profile?.pages.find((page) => page.id === profile.active_page) || profile?.pages[0]
            );
        },
        // Check can run live for the current macro editor state
        get canRunLive() {
            return Boolean(
                this.contextProfile &&
                this.contextProfile.id === this.app.profile?.id &&
                this.contextPage &&
                this.contextPage.id === this.app.page?.id &&
                this.app.snapshot.devices.some(
                    (device) => device.id === this.app.deviceId && device.connected,
                ),
            );
        },
        // Open the macro test dialog with the current editable action
        openTest() {
            const dialog = this.dialog('macroTestDialog');
            if (
                !this.editing ||
                this.saving ||
                this.hasInvalidPluginJson ||
                this.hasInvalidPluginState ||
                !dialog ||
                dialog.open
            )
                return;
            this.recording = '';
            dialog.showModal();
            this.startSimulation();
        },
        // Close test in the current macro editor draft
        closeTest() {
            if (!this.dialog('macroTestDialog')?.open) return;
            this.debugRun.dispose();
            this.dialog('macroTestDialog')?.close();
        },
        // Start the macro debugger without executing live device actions
        startSimulation() {
            this.startDebug('simulation');
        },
        // Start the explicitly selected live macro test
        startLive() {
            if (this.canRunLive) this.startDebug('live');
        },
        // Start a debug run for the current draft and selected execution mode
        startDebug(mode: DebugMode) {
            if (!this.dialog('macroTestDialog')?.open || this.debugRun.active || this.debugRun.busy)
                return;
            this.recording = '';
            this.debugRun.mode = mode;
            this.debugRun.breakpoints = this.rows
                .filter((row) => this.breakpointIds.includes(row.id))
                .map((row) => row.path);
            void this.debugRun.start(this.app.bridge, {
                action: cloneJson(this.action),
                start_running: true,
                device_id: this.app.deviceId,
                profile_id: this.contextProfile?.id || '',
                page_id: this.contextPage?.id || '',
                control_id: this.app.controlId || 'key-1',
                inputs: cloneJson(this.macroInputsDraft),
                macro_id: this.namedMode ? this.namedId || crypto.randomUUID() : null,
            });
        },
        // Format an action path for the debugger event list
        debugPathLabel(path: string) {
            return path
                ? path
                      .split('.')
                      .map((index) => Number(index) + 1)
                      .join('.')
                : 'Root';
        },
        // Format a recorded debug event for display
        debugEventLabel(event: import('../generated/runtime').DebugStepEvent) {
            return `${this.debugPathLabel(event.path.join('.'))} · ${event.kind || event.phase} · ${event.phase} · ${event.elapsed_ms} ms${event.phase === 'branch' ? ` · ${event.detail}` : ''}`;
        },
        editing: false,
        closeBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Edit able signature in the current macro editor draft
        editableSignature() {
            return JSON.stringify([this.action, this.macroName, this.macroInputsDraft]);
        },
        editorAddress: '',
        selectedId: '',
        rowRevision: 0,
        collapsed: [] as string[],
        draggingId: '',
        nodeIds: new WeakMap<object, string>(),
        namedId: '',
        macroName: '',
        macroInputsDraft: [] as MacroInput[],
        contextProfileId: '',
        settingsProfileId: '',
        settingsInputs: [] as MacroInput[],
        settingsError: '',
        namedOriginal: '',
        saving: false,
        // Check has invalid plugin json for the current macro editor state
        get hasInvalidPluginJson() {
            return Object.values(this.pluginJsonDrafts).some((draft) => draft.error);
        },
        // Return editor error for the current macro editor view
        get editorError() {
            return this.hasInvalidPluginJson
                ? 'Correct the invalid plugin settings before testing or saving.'
                : this.hasInvalidPluginState
                  ? 'Choose the requested state for each two-state Elgato action.'
                  : this.error;
        },
        // Return host for the current macro editor view
        get host() {
            return ((this as unknown as EditorMagics).$el as HTMLElement).closest(
                macro ? 'sd-macro-editor' : 'sd-action-editor',
            );
        },
        // Return named mode for the current macro editor view
        get namedMode() {
            return Boolean(this.host?.hasAttribute('named'));
        },
        // Return dialog mode for the current macro editor view
        get dialogMode(): boolean {
            return (
                macro ||
                Boolean(this.host?.hasAttribute('modal')) ||
                this.action.kind === 'macro' ||
                this.action.kind === 'toggle' ||
                (this.action.kind === 'builtin' && 'children' in this.action.action)
            );
        },
        // Return editor title for the current macro editor view
        get editorTitle() {
            return this.namedMode
                ? this.namedId
                    ? 'Edit saved macro'
                    : 'Create saved macro'
                : macro || this.action.kind === 'macro'
                  ? 'Edit macro'
                  : 'Edit action flow';
        },
        // Return editor summary for the current macro editor view
        get editorSummary() {
            return `${this.rows.length} ${this.rows.length === 1 ? 'step' : 'steps'} · Edit order and branches`;
        },
        // Return sequence mode for the current macro editor view
        get sequenceMode() {
            return this.action.kind === 'macro';
        },
        // Return selected row for the current macro editor view
        get selectedRow(): Row | null {
            return this.rows.find((row) => row.id === this.selectedId) || null;
        },
        // Return selected path for the current macro editor view
        get selectedPath() {
            return this.selectedRow?.path || '';
        },
        // Return selected title for the current macro editor view
        get selectedTitle() {
            return this.selectedRow ? this.selectedActionLabel(this.selectedRow.action) : '';
        },
        // Return selected description for the current macro editor view
        get selectedDescription() {
            const action = this.selectedRow?.action;
            if (!action) return '';
            if (action.kind === 'none') return 'Choose an action to configure this step.';
            if (action.kind === 'plugin')
                return (
                    this.app.snapshot.plugins.find((plugin) => plugin.id === action.plugin_id)
                        ?.name || 'Configure the plugin settings for this step.'
                );
            return builtinCatalog.find((item) => item.id === actionKey(action))?.description || '';
        },
        // Return selected can reorder for the current macro editor view
        get selectedCanReorder() {
            return Boolean(this.selectedRow && this.isSequenceChild(this.selectedPath));
        },
        // Check whether the selected action can move in the requested direction
        canMove(direction: number) {
            if (!this.selectedCanReorder) return false;
            const parts = this.selectedPath.split('.');
            const index = Number(parts.pop()) + direction;
            return index >= 0 && index < actionChildren(this.node(parts.join('.'))).length;
        },
        // Check can duplicate for the current macro editor state
        get canDuplicate() {
            if (!this.selectedCanReorder) return false;
            const parts = this.selectedPath.split('.');
            parts.pop();
            return actionChildren(this.node(parts.join('.'))).length < 100;
        },
        // Return breadcrumb items for the current macro editor view
        get breadcrumbItems() {
            const selected = this.selectedRow;
            return selected
                ? this.rows.filter(
                      (row) =>
                          row.id === selected.id ||
                          (row.path !== '' && selected.path.startsWith(row.path + '.')) ||
                          row.path === '',
                  )
                : [];
        },
        // Summarize the action for the macro tree row
        stepSummary(action: Action): string {
            if (action.kind === 'delay') return `${action.milliseconds} ms`;
            if (action.kind === 'none') return 'Choose an action';
            if (action.kind === 'macro') return `${action.actions.length} steps in sequence`;
            if (action.kind === 'toggle') return '2 toggle states';
            if (action.kind === 'plugin')
                return (
                    Alpine.raw(this.app.snapshot).plugins.find(
                        (plugin) => plugin.id === action.plugin_id,
                    )?.name || 'Plugin unavailable'
                );
            if (action.kind === 'builtin') {
                const a = action.action;
                if (a.kind === 'saved_macro')
                    return (
                        Alpine.raw(this.app.snapshot).saved_macros.find(
                            (macro) => macro.id === a.macro_id,
                        )?.name || 'Choose a saved macro'
                    );
                if (a.kind === 'keyboard_macro') return keyboardMacroSummary(a);
                if (a.kind === 'conditional') return '2 conditional branches';
                if (a.kind === 'gesture') return '3 gesture branches';
                if (a.kind === 'press_release') return '2 event branches';
                const descriptor = nestedBuiltins.find((item) => item.id === a.kind);
                const field = descriptor?.fields.find((field) =>
                    ['string', 'text', 'lines', 'number'].includes(field.type),
                );
                const fieldValue = field
                    ? (a as unknown as Record<string, unknown>)[field.key]
                    : '';
                return Array.isArray(fieldValue)
                    ? fieldValue.join(' + ')
                    : String(fieldValue || descriptor?.category || '');
            }
            return (
                value(action) ||
                builtinCatalog.find((item) => item.id === actionKey(action))?.category ||
                ''
            );
        },
        // Assign or retrieve the stable identifier for an action node
        nodeId(action: Action): string {
            const raw = Alpine.raw(action) as object;
            let id = this.nodeIds.get(raw);
            if (!id) {
                id = crypto.randomUUID();
                this.nodeIds.set(raw, id);
            }
            return id;
        },
        // Return detail rows for the current macro editor view
        get detailRows(): Row[] {
            return this.dialogMode ? this.rows.filter((r) => r.id === this.selectedId) : this.rows;
        },
        // Return tree rows for the current macro editor view
        get treeRows(): (Row & { expanded: boolean })[] {
            return visibleMacroRows(this.rows, this.collapsed);
        },
        // Return breadcrumbs for the current macro editor view
        get breadcrumbs() {
            const selected = this.rows.find((r) => r.id === this.selectedId);
            if (!selected) return 'Macro';
            return [
                'Macro',
                ...this.rows
                    .filter(
                        (r) =>
                            r.id === selected.id ||
                            (r.path !== '' && selected.path.startsWith(r.path + '.')),
                    )
                    .map((r) => r.label),
            ].join(' › ');
        },
        // Select node in the current macro editor draft
        selectNode(event: Event) {
            this.selectedId = (event.currentTarget as HTMLElement).dataset.node || '';
            this.recording = '';
        },
        // Toggle branch in the current macro editor draft
        toggleBranch(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.node || '';
            this.collapsed = this.collapsed.includes(id)
                ? this.collapsed.filter((x) => x !== id)
                : [...this.collapsed, id];
        },
        // Open named in the current macro editor draft
        openNamed(event: CustomEvent<{ id?: string; name?: string; action?: Action }>) {
            if (!this.namedMode) return;
            this.pluginSettingDrafts.clear();
            const d = event.detail;
            this.namedId = d.id || '';
            this.macroName = d.name || '';
            this.macroInputsDraft = structuredClone(
                Alpine.raw(
                    this.app.snapshot.saved_macros.find((m) => m.id === this.namedId)?.inputs || [],
                ),
            );
            this.namedOriginal = JSON.stringify(
                this.app.snapshot.saved_macros.find((m) => m.id === this.namedId) || null,
            );
            this.serialized = JSON.stringify(d.action || { kind: 'macro', actions: [] });
            this.readDraft();
            this.openEditor();
        },
        // Open macro settings in the current macro editor draft
        openMacroSettings() {
            const dialog = this.dialog('macroSettingsDialog');
            if (!this.editing || !this.namedMode || !dialog || dialog.open) return;
            this.recording = '';
            this.settingsError = '';
            this.settingsProfileId = this.contextProfileId;
            this.settingsInputs = structuredClone(Alpine.raw(this.macroInputsDraft));
            dialog.showModal();
        },
        // Close macro settings in the current macro editor draft
        closeMacroSettings() {
            this.dialog('macroSettingsDialog')?.close();
            this.settingsInputs = [];
            this.settingsError = '';
        },
        // Apply macro settings in the current macro editor draft
        applyMacroSettings() {
            try {
                validateMacroInputs(this.settingsInputs);
                this.macroInputsDraft = structuredClone(Alpine.raw(this.settingsInputs));
                this.contextProfileId = this.settingsProfileId;
                this.closeMacroSettings();
            } catch (error) {
                this.settingsError = String(error);
            }
        },
        // Add macro input in the current macro editor draft
        addMacroInput() {
            if (this.settingsInputs.length >= 64) return;
            let index = this.settingsInputs.length + 1;
            while (this.settingsInputs.some((input) => input.name === 'input_' + index)) index++;
            this.settingsInputs.push({
                name: 'input_' + index,
                label: 'Input',
                type: 'string',
                required: false,
                default: null,
            });
        },
        // Remove macro input in the current macro editor draft
        removeMacroInput(event: Event) {
            this.settingsInputs.splice(
                Number((event.currentTarget as HTMLElement).dataset.index),
                1,
            );
        },
        // Update the selected macro input default from the form control
        macroInputDefault(event: Event) {
            const el = event.currentTarget as HTMLInputElement;
            const input = this.settingsInputs[Number(el.dataset.index)];
            if (!input) return;
            try {
                input.default = el.dataset.toggle
                    ? el.checked
                        ? initialValue(input.type)
                        : null
                    : parseValue(input.type, el.type === 'checkbox' ? el.checked : el.value);
                this.settingsError = '';
            } catch (error) {
                this.settingsError = String(error);
            }
        },
        // Change the macro input type and reset its incompatible default
        macroInputType(event: Event) {
            const input =
                this.settingsInputs[Number((event.currentTarget as HTMLElement).dataset.index)];
            if (input && input.default !== null) input.default = initialValue(input.type);
        },
        // Read the saved-macro call represented by the selected row
        macroCall(row: Row): Extract<BuiltinAction, { kind: 'saved_macro' }> | undefined {
            return row.action.kind === 'builtin' && row.action.action.kind === 'saved_macro'
                ? row.action.action
                : undefined;
        },
        // Look up the declared inputs of the referenced saved macro
        callInputs(row: Row): MacroInput[] {
            const call = this.macroCall(row);
            return (
                this.app.snapshot.saved_macros.find((macro) => macro.id === call?.macro_id)
                    ?.inputs || []
            );
        },
        // Find the named argument in the selected saved-macro call
        callArgument(row: Row, name: string): MacroArgument | undefined {
            return this.macroCall(row)?.arguments?.[name];
        },
        // Return the argument value source selected in the call editor
        argumentSource(row: Row, name: string) {
            return this.callArgument(row, name)?.kind || 'default';
        },
        // Format the argument value for its text control
        argumentText(row: Row, name: string) {
            const argument = this.callArgument(row, name);
            return argument
                ? argument.kind === 'literal'
                    ? String(argument.value)
                    : argument.name
                : '';
        },
        // Read the boolean argument value for its checkbox
        argumentChecked(row: Row, name: string) {
            const argument = this.callArgument(row, name);
            return argument?.kind === 'literal' && argument.value === true;
        },
        // List valid argument sources for the input type
        argumentOptions(input: MacroInput, source: string) {
            const definitions =
                source === 'input' ? this.macroInputsDraft : this.contextProfile?.parameters || [];
            return definitions
                .filter((definition) => definition.type === input.type)
                .map((definition) => ({ value: definition.name, label: definition.label }));
        },
        // Update the selected saved-macro call argument without committing its parent draft
        changeArgument(event: Event) {
            const el = event.currentTarget as HTMLInputElement;
            const action = this.node(el.dataset.path || '');
            if (action.kind !== 'builtin' || action.action.kind !== 'saved_macro') return;
            const call = action.action;
            const input = this.app.snapshot.saved_macros
                .find((macro) => macro.id === call.macro_id)
                ?.inputs?.find((input) => input.name === el.dataset.name);
            if (!input) return;
            call.arguments ||= {};
            const source = el.dataset.source || el.value;
            try {
                if (el.dataset.operation === 'source') {
                    if (source === 'default') delete call.arguments[input.name];
                    else if (source === 'literal')
                        call.arguments[input.name] = {
                            kind: 'literal',
                            value: input.default ?? initialValue(input.type),
                        };
                    else if (source === 'parameter' || source === 'input' || source === 'output')
                        call.arguments[input.name] = { kind: source, name: '' };
                } else if (source === 'literal')
                    call.arguments[input.name] = {
                        kind: 'literal',
                        value: parseValue(
                            input.type,
                            el.type === 'checkbox' ? el.checked : el.value,
                        ),
                    };
                else if (source === 'parameter' || source === 'input' || source === 'output')
                    call.arguments[input.name] = { kind: source, name: el.value };
                this.error = '';
                this.publish();
            } catch (error) {
                this.error = String(error);
            }
        },
        // List macro inputs available as action parameter references
        referenceOptions() {
            return [
                ...(this.contextProfile?.parameters || [])
                    .filter((p) => p.type === 'string')
                    .map((p) => ({
                        value: '${parameter.' + p.name + '}',
                        label: 'Parameter: ' + p.label,
                    })),
                ...this.macroInputsDraft
                    .filter((input) => input.type === 'string')
                    .map((input) => ({
                        value: '${input.' + input.name + '}',
                        label: 'Input: ' + input.label,
                    })),
            ];
        },
        // List reference-capable fields on the selected action
        topLevelReferenceFields(row: Row) {
            if (row.action.kind === 'command') return [{ key: 'args', label: 'Arguments' }];
            if (row.action.kind === 'open_url') return [{ key: 'url', label: 'URL' }];
            return [];
        },
        // List plugin settings that can reference macro inputs
        pluginReferenceFields(row: Row) {
            const action = row.action;
            return action.kind === 'plugin'
                ? this.app.snapshot.plugins.find((p) => p.id === action.plugin_id)?.capabilities
                      ?.actions[action.action_id]?.parameter_fields || []
                : [];
        },
        // List compatible macro-input references for plugin settings
        pluginReferenceOptions() {
            return [
                ...(this.contextProfile?.parameters || []).map((p) => ({
                    value: 'parameter:' + p.name,
                    label: 'Parameter: ' + p.label + ' (' + p.type + ')',
                })),
                ...this.macroInputsDraft.map((p) => ({
                    value: 'input:' + p.name,
                    label: 'Input: ' + p.label + ' (' + p.type + ')',
                })),
            ];
        },
        // Insert the selected macro input reference into the draft field
        insertReference(event: Event) {
            const el = event.currentTarget as HTMLSelectElement;
            const node = this.node(el.dataset.path || '');
            if (!el.value) return;
            const key = el.dataset.field || '';
            if (node.kind === 'plugin') {
                const [kind, name] = el.value.split(':');
                const settings =
                    node.settings &&
                    typeof node.settings === 'object' &&
                    !Array.isArray(node.settings)
                        ? (node.settings as Record<string, JsonValue>)
                        : {};
                settings[key] = { [kind === 'input' ? '$input' : '$parameter']: name };
                node.settings = settings;
                if (!this.pluginJsonDrafts[this.nodeId(node)]?.error)
                    delete this.pluginJsonDrafts[this.nodeId(node)];
            } else {
                const fields = (node.kind === 'builtin' ? node.action : node) as unknown as Record<
                    string,
                    unknown
                >;
                if (!['url', 'path', 'program', 'text', 'args'].includes(key)) return;
                if (key === 'args') (fields.args as string[]).push(el.value);
                else fields[key] = el.value;
            }
            el.value = '';
            this.publish();
        },
        // Read the current editable action draft from its owning context
        readDraft() {
            this.pluginSettingDrafts.clear();
            this.pluginJsonDrafts = {};
            this.action = macro
                ? { kind: 'macro', actions: JSON.parse(this.serialized || '[]') }
                : JSON.parse(this.serialized || '{"kind":"none"}');
        },
        // Open editor in the current macro editor draft
        openEditor() {
            if (this.editing) return;
            this.debugRun.reset();
            this.breakpointIds = [];
            this.contextProfileId = '';
            this.readDraft();
            this.editorAddress = this.app.bindingAddress();
            this.closeBaseline = this.editableSignature();
            this.editing = true;
            this.error = '';
            this.collapsed = [];
            this.selectedId = this.rows[0]?.id || '';
            // Update the macro editor view after reactive DOM changes finish
            (this as unknown as EditorMagics).$nextTick(() => {
                const dialog = (this as unknown as EditorMagics).$refs
                    .macroDialog as HTMLDialogElement;
                if (!this.editing) return;
                if (!dialog.open) dialog.showModal();
                if (this.namedMode)
                    // Focus the active editor control after the dialog opening frame
                    requestAnimationFrame(() => {
                        if (this.editing && dialog.open)
                            dialog
                                .querySelector<HTMLInputElement>('[aria-label="Saved macro name"]')
                                ?.focus();
                    });
            });
        },
        // Close editor in the current macro editor draft
        closeEditor() {
            if (this.saving) return;
            this.inspectorSession?.abort();
            this.inspectorSession = undefined;
            this.closeMacroSettings();
            this.closeTest();
            this.debugRun.dispose();
            ((this as unknown as EditorMagics).$refs.macroDialog as HTMLDialogElement)?.close();
            this.editing = false;
            this.recording = '';
            this.error = '';
            this.readDraft();
            if (macro && !this.namedMode)
                ((this as unknown as EditorMagics).$el as HTMLElement).dispatchEvent(
                    new CustomEvent('macro-cancel', { bubbles: true }),
                );
        },
        // Validate and transfer the editor draft back to its parent editor
        async applyEditor() {
            if (this.saving || this.hasInvalidPluginJson || this.hasInvalidPluginState) return;
            this.inspectorSession?.abort();
            this.debugRun.dispose();
            try {
                // Validate nested action drafts before applying the editor
                const validate = (a: Action, depth = 0) => {
                    if (depth > 8 || actionChildren(a).length > 100)
                        throw Error('Allow up to 100 steps per sequence and 8 levels of nesting.');
                    actionChildren(a).forEach((child) => validate(child, depth + 1));
                };
                validate(this.action);
                const keyLogicReason = keyLogicActionReason(
                    this.action,
                    this.app.snapshot.plugins,
                    this.app.snapshot.saved_macros,
                );
                if (keyLogicReason) throw Error(keyLogicReason);
                validateMacroDraft(
                    this.action,
                    this.macroInputsDraft,
                    this.app.snapshot.saved_macros,
                );
                if (!this.namedMode && this.editorAddress !== this.app.bindingAddress())
                    throw Error(
                        'The selected binding changed. Reopen its editor before applying this draft.',
                    );
                if (this.namedMode) {
                    const name = this.macroName.trim();
                    if (!name || new TextEncoder().encode(name).length > 160)
                        throw Error('Enter a macro name of at most 160 bytes.');
                    if (
                        JSON.stringify(
                            this.app.snapshot.saved_macros.find((m) => m.id === this.namedId) ||
                                null,
                        ) !== this.namedOriginal
                    )
                        throw Error('This saved macro changed. Close and reopen it before saving.');
                    this.saving = true;
                    await this.app.edit(
                        [
                            {
                                kind: 'save_macro',
                                value: {
                                    id: this.namedId || crypto.randomUUID(),
                                    name,
                                    inputs: structuredClone(Alpine.raw(this.macroInputsDraft)),
                                    action: cloneJson(this.action),
                                },
                            },
                        ],
                        'Save named macro',
                    );
                    this.saving = false;
                    this.closeEditor();
                    return;
                }
                this.editing = false;
                this.publish();
                ((this as unknown as EditorMagics).$refs.macroDialog as HTMLDialogElement)?.close();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.saving = false;
            }
        },
        // Handle dragged start in the current macro editor draft
        dragStart(event: DragEvent) {
            const row = this.rows.find(
                (r) => r.id === (event.currentTarget as HTMLElement).dataset.node,
            );
            if (!row || !this.isSequenceChild(row.path)) {
                event.preventDefault();
                return;
            }
            this.draggingId = row.id;
            event.dataTransfer?.setData('text/plain', row.id);
        },
        // Handle dragged over in the current macro editor draft
        dragOver(event: DragEvent) {
            event.preventDefault();
        },
        // Clear the active macro-node drag state
        endDrag() {
            this.draggingId = '';
        },
        // Handle dropped step in the current macro editor draft
        dropStep(event: DragEvent) {
            event.preventDefault();
            const from = this.rows.find((r) => r.id === this.draggingId),
                to = this.rows.find(
                    (r) => r.id === (event.currentTarget as HTMLElement).dataset.node,
                );
            this.draggingId = '';
            if (!from || !to || !this.isSequenceChild(to.path)) return;
            const a = from.path.split('.'),
                b = to.path.split('.');
            const i = Number(a.pop()),
                j = Number(b.pop());
            if (a.join('.') !== b.join('.')) {
                this.error =
                    'Reorder steps within the same sequence; fixed branches stay in their slots.';
                return;
            }
            const children = actionChildren(this.node(a.join('.')));
            const [step] = children.splice(i, 1);
            children.splice(j, 0, step);
            this.publish();
        },
        serialized: '',
        error: '',
        observer: null as MutationObserver | null,
        // Return app for the current macro editor view
        get app() {
            return Alpine.store('app') as AppState;
        },
        // Return macro mode for the current macro editor view
        get macroMode() {
            return macro;
        },
        // Build the metadata signature used to invalidate cached action rows
        get rowMetadataSignature() {
            // Do not read `rows` here: building its cached model can consume a
            // metadata change before the watcher invalidates the displayed rows
            const references: Pick<Row, 'action'>[] = [];
            // Inspect the current action node and recurse through its nested actions
            const visit = (action: Action) => {
                references.push({ action });
                for (const child of actionChildren(action)) visit(child);
            };
            visit(this.action);
            return this.rowMetadata(references, true);
        },
        // Check that the editor still belongs to the active profile and control
        get editorContextCurrent() {
            return (
                !this.editing ||
                this.namedMode ||
                (this.editorAddress === this.app.bindingAddress() && Boolean(this.app.profile))
            );
        },
        // Attach macro editor listeners and register owned draft resources
        init() {
            ownWindowMethods(this, 'macro-editor', [
                'applyEditor',
                'browseActionPath',
                'loadConditionTargets',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(this, ['applyEditor'], {
                name: macro ? 'Macro editor' : 'Action editor',
                // Report whether the controller retains unsaved or active work
                state: () =>
                    this.editing &&
                    (this.editableSignature() !== this.closeBaseline ||
                        this.pluginSettingDrafts.size > 0 ||
                        this.hasInvalidPluginJson ||
                        this.dialog('macroSettingsDialog')?.open ||
                        this.breakpointIds.length > 0 ||
                        this.debugRun.dirty)
                        ? 'dirty'
                        : 'clean',
                // Discard the staged media or editor state owned by the current dialog
                discard: async () => {
                    await this.debugRun.release();
                    this.closeEditor();
                },
                // Release resources retained by the current controller
                release: async () => {
                    await this.debugRun.release();
                    this.closeEditor();
                },
            });
            const host = (this as unknown as { $el: HTMLElement }).$el.closest(
                macro ? 'sd-macro-editor' : 'sd-action-editor',
            );
            if (!host) return;
            this.read(host.getAttribute('content') || '');
            // Refresh dependent macro editor state after the watched value changes
            (this as unknown as EditorMagics).$watch('rowMetadataSignature', () => {
                ++this.rowRevision;
            });
            // Refresh dependent macro editor state after the watched value changes
            (this as unknown as EditorMagics).$watch('$store.app.route', () => {
                if (
                    this.editing &&
                    !actionLibrarySelection.owns((this as unknown as EditorMagics).$el)
                )
                    this.closeEditor();
            });
            // Refresh dependent macro editor state after the watched value changes
            (this as unknown as EditorMagics).$watch('$store.app.deviceId', () => {
                if (this.editing && !this.namedMode) this.closeEditor();
            });
            // Refresh dependent macro editor state after the watched value changes
            (this as unknown as EditorMagics).$watch('editorContextCurrent', () => {
                if (!this.editorContextCurrent) this.closeEditor();
            });
            this.observer = new MutationObserver(() =>
                this.read(host.getAttribute('content') || ''),
            );
            this.observer.observe(host, { attributes: true, attributeFilter: ['content'] });
        },
        // Remove macro editor listeners and release owned resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.inspectorSession?.abort();
            this.closeMacroSettings();
            this.closeTest();
            this.debugRun.dispose();
            this.observer?.disconnect();
            if (this.editing)
                ((this as unknown as EditorMagics).$refs.macroDialog as HTMLDialogElement)?.close();
        },
        // Parse and normalize the supplied action source for editing
        read(source: string) {
            if (source === this.serialized) return;
            if (this.editing) {
                this.closeEditor();
                this.app.notify(
                    'The action changed while its editor was open. Reopen it to continue.',
                    'warning',
                );
            }
            try {
                this.action = macro
                    ? {
                          kind: 'macro',
                          actions: Array.isArray(JSON.parse(source || '[]'))
                              ? JSON.parse(source || '[]')
                              : [],
                      }
                    : JSON.parse(source || '{"kind":"none"}');
                this.serialized = source;
                this.error = '';
            } catch (e) {
                this.error = String(e);
            }
        },
        // Resolve action metadata used by the cached tree rows
        rowMetadata(rows: Pick<Row, 'action'>[], reactive = false) {
            const snapshot = reactive ? this.app.snapshot : Alpine.raw(this.app.snapshot);
            const metadata: unknown[][] = [];
            const seenPlugins = new Set<string>();
            for (const { action } of rows) {
                if (action.kind === 'plugin') {
                    const key = `${action.plugin_id}\0${action.action_id}`;
                    if (seenPlugins.has(key)) continue;
                    seenPlugins.add(key);
                    const plugin = snapshot.plugins.find(
                        (plugin) => plugin.id === action.plugin_id,
                    );
                    const declared = plugin?.actions.find(
                        (declared) => declared.id === action.action_id,
                    );
                    metadata.push([
                        action.plugin_id,
                        action.action_id,
                        plugin?.name,
                        plugin?.icon,
                        declared?.icon,
                        declared?.multi_action_images,
                        declared?.supported_in_multi_actions,
                    ]);
                } else if (action.kind === 'builtin' && action.action.kind === 'saved_macro') {
                    const id = action.action.macro_id;
                    metadata.push([
                        id,
                        snapshot.saved_macros.find((saved) => saved.id === id)?.name,
                    ]);
                }
            }
            return JSON.stringify(metadata);
        },
        // Return rows for the current macro editor view
        get rows() {
            const owner = Alpine.raw(this);
            const action = Alpine.raw(this.action);
            const previous = rowModels.get(owner);
            if (previous?.action === action && previous.revision === this.rowRevision)
                return previous.rows;
            const rows: Row[] = [];
            // Visit nested actions while preserving their current path
            const walk = (action: Action, path: number[], depth: number, label: string) => {
                if (!action || typeof action !== 'object') return;
                if (!(this.sequenceMode && path.length === 0))
                    rows.push({
                        id: this.nodeId(action),
                        path: path.join('.'),
                        action,
                        choice: actionKey(action),
                        fields: this.actionFields(action),
                        depth: Math.max(0, depth - (this.sequenceMode ? 1 : 0)),
                        label,
                        summary: this.stepSummary(action),
                        icon:
                            builtinCatalog.find((item) => item.id === actionKey(action))?.icon ||
                            'zap',
                        image: pluginMultiActionImage(
                            action,
                            this.app.snapshot.plugins,
                            this.sequenceMode || path.length > 0,
                        ),
                        hasChildren: actionChildren(action).length > 0,
                        value: value(action),
                        valueLabel: topLevelValues[action.kind] || '',
                        args: action.kind === 'command' ? action.args.join('\n') : '',
                        colors:
                            action.kind === 'lighting'
                                ? action.colors.map(
                                      (rgb) =>
                                          '#' +
                                          rgb
                                              .map((value) => value.toString(16).padStart(2, '0'))
                                              .join(''),
                                  )
                                : [],
                        settings:
                            action.kind === 'plugin'
                                ? JSON.stringify(action.settings, null, 2)
                                : '',
                    });
                actionChildren(action).forEach((child, i) =>
                    walk(
                        child,
                        [...path, i],
                        depth + 1,
                        action.kind === 'toggle'
                            ? i === 0
                                ? 'On'
                                : 'Off'
                            : action.kind === 'builtin' &&
                                ['conditional', 'failure_policy'].includes(action.action.kind)
                              ? i === 0
                                  ? action.action.kind === 'failure_policy'
                                      ? 'Step'
                                      : 'If true'
                                  : action.action.kind === 'failure_policy'
                                    ? 'Fallback'
                                    : 'If false'
                              : action.kind === 'builtin' && action.action.kind === 'gesture'
                                ? ['Tap', 'Double tap', 'Hold'][i]
                                : action.kind === 'builtin' &&
                                    action.action.kind === 'press_release'
                                  ? i === 0
                                      ? 'Press'
                                      : 'Release'
                                  : `Step ${i + 1}`,
                    ),
                );
            };
            walk(this.action, [], 0, 'Action');
            rowModels.set(owner, {
                action,
                revision: this.rowRevision,
                rows,
            });
            return rows;
        },
        // Resolve the action node at the selected tree path
        node(path: string) {
            let node = this.action;
            for (const index of path === '' ? [] : path.split('.').map(Number)) {
                const child = actionChildren(node)[index];
                if (!child) throw Error('Invalid action address');
                node = child;
            }
            return node;
        },
        // Replace the selected action node in the editable macro tree
        replace(path: string, action: Action) {
            const current = this.node(path),
                context = this.nodeId(current);
            const nextId = this.nodeId(action);
            try {
                if (current.kind === 'plugin')
                    this.pluginSettingDrafts.remember(
                        context,
                        current.plugin_id,
                        current.action_id,
                        current.settings,
                    );
                if (action.kind === 'plugin')
                    action.settings = this.pluginSettingDrafts.restore(
                        context,
                        action.plugin_id,
                        action.action_id,
                        action.settings,
                    );
                this.pluginSettingDrafts.move(context, nextId);
            } catch (error) {
                this.app.notify(String(error), 'error');
                return false;
            }
            ++this.rowRevision;
            delete this.pluginJsonDrafts[context];
            if (this.breakpointIds.includes(context))
                this.breakpointIds = this.breakpointIds.map((id) => (id === context ? nextId : id));
            if (context === this.selectedId) this.selectedId = nextId;
            if (path === '') {
                this.action = action;
                return true;
            }
            const parts = path.split('.');
            const index = Number(parts.pop());
            const parent = this.node(parts.join('.'));
            setChild(parent, index, action);
            return true;
        },
        // Transfer the updated action draft to its owning editor
        publish() {
            ++this.rowRevision;
            if (this.editing) {
                const active = new Set(this.rows.map((row) => row.id));
                for (const id of Object.keys(this.pluginJsonDrafts))
                    if (!active.has(id)) delete this.pluginJsonDrafts[id];
                if (this.breakpointIds.some((id) => !active.has(id)))
                    this.breakpointIds = this.breakpointIds.filter((id) => active.has(id));
                if (!this.rows.some((r) => r.id === this.selectedId))
                    this.selectedId = this.rows[0]?.id || '';
                const selected = this.selectedRow;
                if (selected)
                    // Remove collapsed node identifiers that no longer exist in the action tree
                    this.collapsed = this.collapsed.filter((id) => {
                        const parent = this.rows.find((row) => row.id === id);
                        return (
                            !parent ||
                            !(parent.path === '' || selected.path.startsWith(parent.path + '.'))
                        );
                    });
                return;
            }
            try {
                this.serialized = JSON.stringify(
                    macro && this.action.kind === 'macro' ? this.action.actions : this.action,
                );
                this.error = '';
                (this as unknown as { $el: HTMLElement }).$el.dispatchEvent(
                    new CustomEvent(macro ? 'macro-change' : 'action-change', {
                        detail:
                            macro && this.action.kind === 'macro'
                                ? { actions: this.action.actions }
                                : { action: this.action },
                        bubbles: true,
                        composed: true,
                    }),
                );
            } catch (e) {
                this.error = String(e);
            }
        },
        // Open the action chooser for the currently selected macro node
        browseActions(event: Event) {
            const owner = event.currentTarget as HTMLElement;
            this.browseLibrary(owner, owner.dataset.path || '');
        },
        // Open the plugin action chooser for the current insertion target
        browsePlugins(event: Event) {
            const owner = event.currentTarget as HTMLElement;
            this.browseLibrary(owner, owner.dataset.path || '', 'plugins');
        },
        // Open the saved macro library for the current action context
        browseLibrary(owner: HTMLElement, path: string, source: 'all' | 'plugins' = 'all') {
            const original = this.serialized;
            const target = this.nodeId(this.node(path));
            const wasEditing = this.editing;
            this.recording = '';
            openActionPicker({
                owner,
                standalone: this.namedMode,
                multiAction: Boolean(path) || this.namedMode,
                keyLogic: insideKeyLogic(this.action, path),
                current: actionKey(this.node(path)),
                source,
                label: this.namedMode
                    ? this.editorTitle
                    : this.editing
                      ? this.editorTitle
                      : 'Device editor',
                // Check whether the action-picker request still targets the current editor
                valid: () =>
                    original === this.serialized &&
                    this.editing === wasEditing &&
                    this.rows.some((row) => row.id === target),
                // Check whether the suspended action-picker request can return to its editor
                canResume: () =>
                    original === this.serialized &&
                    this.editing === wasEditing &&
                    this.editorContextCurrent,
                // Apply the selected action or media entry to the requesting editor
                choose: (selection) => {
                    const row = this.rows.find((row) => row.id === target);
                    if (!row) return;
                    const next =
                        'action' in selection
                            ? selection.action
                            : dialActionDefault(
                                  defaultAction(selection.kind),
                                  this.namedMode || !this.host?.hasAttribute('dial-lane')
                                      ? null
                                      : Number(this.host.getAttribute('dial-lane')),
                              );
                    if (next.kind === 'profile') next.profile_id = this.contextProfile?.id || '';
                    if (next.kind === 'page') next.page_id = this.contextPage?.id || '';
                    if (this.replace(row.path, next)) this.publish();
                },
            });
        },
        // Select ed action label in the current macro editor draft
        selectedActionLabel(action: Action) {
            if (action.kind === 'none') return 'No action';
            if (action.kind === 'plugin')
                return (
                    this.app.snapshot.plugins
                        .find((p) => p.id === action.plugin_id)
                        ?.actions.find((a) => a.id === action.action_id)?.name || 'Plugin action'
                );
            return this.actionLabel(actionKey(action));
        },
        // Update value in the current macro editor draft
        changeValue(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const action = this.node(input.dataset.path || '');
            if (input.type === 'number' && input.value === '') return;
            switch (action.kind) {
                case 'delay':
                    action.milliseconds = Number(input.value);
                    break;
                case 'hotkey':
                    action.keys = input.value
                        .split('+')
                        .map((s) => s.trim())
                        .filter(Boolean);
                    break;
                case 'open_url':
                    action.url = input.value;
                    break;
                case 'volume':
                    action.delta = Number(input.value);
                    break;
                case 'brightness':
                    action.value = Number(input.value);
                    break;
            }
            this.publish();
        },
        // Update the selected action field from its form control
        field(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const action = this.node(input.dataset.path || '');
            const key = input.dataset.field || '';
            try {
                if (action.kind === 'command')
                    action.args = input.value.split('\n').filter((s) => s.length);
                if (action.kind === 'media') action.command = input.value;
                if (action.kind === 'profile') action.profile_id = input.value;
                if (action.kind === 'page') action.page_id = input.value;
                if (action.kind === 'navigate') {
                    if (key === 'target')
                        action.target = input.value === 'profile' ? 'profile' : 'page';
                    if (key === 'direction')
                        action.direction = input.value === 'previous' ? 'previous' : 'next';
                }
                if (action.kind === 'swipe_navigation') {
                    if (key === 'target') action.target = input.value;
                    if (key === 'wrap') action.wrap = input.checked;
                    if (key === 'reverse') action.reverse = input.checked;
                }
                if (action.kind === 'lighting') {
                    if (input.value === '') return;
                    action.brightness = Number(input.value);
                }
                this.publish();
            } catch (e) {
                this.error = String(e);
            }
        },
        // Update lighting color in the current macro editor draft
        changeLightingColor(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const action = this.node(input.dataset.path || '');
            const index = Number(input.dataset.index);
            if (action.kind !== 'lighting' || !action.colors[index]) return;
            const color = Number.parseInt(input.value.slice(1), 16);
            action.colors[index] = [(color >> 16) & 255, (color >> 8) & 255, color & 255];
            this.publish();
        },
        // Resize the lighting palette while preserving existing colors
        resizeLightingColors(event: Event) {
            const button = event.currentTarget as HTMLButtonElement;
            const action = this.node(button.dataset.path || '');
            if (action.kind !== 'lighting') return;
            if (button.dataset.op === 'add' && action.colors.length < 32)
                action.colors.push([255, 0, 0]);
            else if (button.dataset.op === 'remove' && action.colors.length > 1)
                action.colors.pop();
            else return;
            this.publish();
        },
        // Read the current availability of the selected action
        availability(action: Action) {
            return capabilityReason(action, this.app.builtinCapabilities);
        },
        // Resolve the display label for an action kind
        actionLabel(kind: string) {
            return builtinCatalog.find((d) => d.id === kind)?.name || kind.replaceAll('_', ' ');
        },
        // Format a scalar value for the action field control
        scalar(value: unknown) {
            return typeof value === 'object' ? JSON.stringify(value) : String(value ?? '');
        },
        // Build target choices while retaining a missing current selection
        targetOptions(items: { id: string; name: string }[], selected: unknown) {
            const options = items.map(({ id, name }) => ({ value: id, label: name }));
            if (
                typeof selected === 'string' &&
                selected &&
                !options.some((item) => item.value === selected)
            )
                options.push({ value: selected, label: 'Unavailable: ' + selected });
            return options;
        },
        // Return context pages for the current macro editor view
        get contextPages() {
            return this.contextProfile?.pages || [];
        },
        // List controls for the selected device model
        controlTargets(modelId: string | undefined) {
            const model = this.app.models.find((item) => item.model_id === modelId);
            return (model?.controls || [])
                .filter((control) => control.kind !== 'display' && !control.preview_hidden)
                .map(({ id, label }) => ({ id, name: label }));
        },
        // Return layer control targets for the current macro editor view
        get layerControlTargets() {
            return this.controlTargets(this.contextProfile?.model_id);
        },
        // Add layer control in the current macro editor draft
        addLayerControl(event: Event) {
            const select = event.currentTarget as HTMLSelectElement;
            const node = this.node(select.dataset.path || '');
            if (node.kind !== 'builtin' || node.action.kind !== 'binding_layer' || !select.value)
                return;
            if (!node.action.controls.includes(select.value))
                node.action.controls.push(select.value);
            select.value = '';
            this.publish();
        },
        // Build choices for the selected action field
        fieldOptions(field: Row['fields'][number], action: Action) {
            if (field.type === 'page') return this.targetOptions(this.contextPages, field.value);
            if (field.type === 'macro')
                return this.targetOptions(this.app.snapshot.saved_macros, field.value);
            if (
                action.kind === 'builtin' &&
                (action.action.kind === 'trigger_binding' || action.action.kind === 'edit_binding')
            ) {
                const { device_id, profile_id } = action.action;
                const { devices, profiles, sessions } = this.app.snapshot;
                if (field.key === 'device_id') return this.targetOptions(devices, field.value);
                const device = devices.find((item) => item.id === device_id);
                if (field.key === 'profile_id')
                    return this.targetOptions(
                        device
                            ? profiles.filter((item) => item.model_id === device.model_id)
                            : profiles,
                        field.value,
                    );
                const profile = profile_id
                    ? profiles.find((item) => item.id === profile_id)
                    : device_id
                      ? profiles.find(
                            (item) =>
                                item.id ===
                                sessions[device?.persistence_key || device_id]?.profile_id,
                        )
                      : this.contextProfile;
                if (field.key === 'page_id')
                    return this.targetOptions(profile?.pages || [], field.value);
                return this.targetOptions(
                    this.controlTargets(profile?.model_id || device?.model_id),
                    field.value,
                );
            }
            // Build the macro editor entry from the current source item
            return (field.options || []).map((value) => {
                const label = value.replaceAll('_', ' ').replace(/([a-z])([A-Z])/g, '$1 $2');
                return { value, label: label.charAt(0).toUpperCase() + label.slice(1) };
            });
        },
        // Resolve empty field label from the current macro editor state
        emptyFieldLabel(field: Row['fields'][number], action: Action) {
            if (
                field.type !== 'binding-target' ||
                (field.key === 'control_id' && actionKey(action) === 'trigger_binding')
            )
                return 'Choose…';
            return 'Current ' + field.label.toLowerCase();
        },
        // Determine the editable scalar type of the field value
        primitiveType(value: unknown) {
            return typeof value === 'number'
                ? 'number'
                : typeof value === 'boolean'
                  ? 'boolean'
                  : 'string';
        },
        // Convert a selected option into its scalar value
        primitiveChoice(event: Event) {
            const input = event.currentTarget as HTMLSelectElement;
            const node = this.node(input.dataset.path || '');
            if (node.kind !== 'builtin') return;
            const fields = node.action as unknown as Record<string, unknown>;
            const value = input.value === 'number' ? 0 : input.value === 'boolean' ? false : '';
            if (input.dataset.part) {
                (fields[input.dataset.field || ''] as Record<string, unknown>).value = value;
            } else fields[input.dataset.field || ''] = value;
            this.publish();
        },
        // Format the current action field value as text
        fieldText(field: { type: string; value: unknown }) {
            return field.type === 'lines'
                ? (field.value as string[]).join('\n')
                : this.scalar(field.value);
        },
        // Resolve action fields from the current macro editor state
        actionFields(action: Action): Row['fields'] {
            if (action.kind === 'builtin') {
                const fields = action.action as unknown as Record<string, unknown>;
                return (nestedBuiltins.find((d) => d.id === action.action.kind)?.fields || []).map(
                    (field) => ({ ...field, value: fields[field.key] }),
                );
            }
            if (action.kind === 'command')
                return [{ ...topLevelPathFields.command, type: 'string', value: action.program }];
            if (action.kind === 'application' || action.kind === 'open_path')
                return [{ ...topLevelPathFields[action.kind], type: 'string', value: action.path }];
            return [];
        },
        // Resolve action field help from the current macro editor state
        actionFieldHelp(row: Row) {
            return row.fields.some((field) => field.key === 'template')
                ? 'Enable Insert run outputs to use ${run.name} from an earlier step.'
                : '';
        },
        // Resolve action field visible from the current macro editor state
        actionFieldVisible(action: Action, key: string) {
            if (action.kind !== 'builtin') return true;
            const builtin = action.action;
            if (builtin.kind === 'file_operation' && ['destination', 'replace'].includes(key))
                return builtin.operation !== 'trash';
            if (builtin.kind === 'write_file' && key === 'replace')
                return builtin.operation === 'replace';
            if (builtin.kind === 'clipboard_slot' && key === 'persist')
                return builtin.operation === 'save';
            if (builtin.kind === 'failure_policy' && ['retries', 'delay_ms'].includes(key))
                return builtin.operation === 'retry';
            return true;
        },
        // Apply action preset in the current macro editor draft
        applyActionPreset(event: Event) {
            const select = event.currentTarget as HTMLSelectElement;
            if (!select.value) return;
            this.builtinField(event);
            select.value = '';
        },
        // Resolve condition operators from the current macro editor state
        conditionOperators(condition: Condition) {
            const options = conditionComparisons[condition.source] || [];
            return options.some((option) => option.value === condition.operator)
                ? options
                : [
                      ...options,
                      { value: condition.operator, label: condition.operator.replaceAll('_', ' ') },
                  ];
        },
        // Resolve condition uses value from the current macro editor state
        conditionUsesValue(condition: Condition) {
            return !['exists', 'missing'].includes(condition.operator);
        },
        conditionTargetLists: {} as Partial<Record<string, ConditionTargetList>>,
        // Resolve condition target list from the current macro editor state
        conditionTargetList(source: string) {
            return this.conditionTargetLists[source] || emptyConditionTargets;
        },
        // Load condition targets in the current macro editor draft
        async loadConditionTargets(event: Event) {
            const source = (event.currentTarget as HTMLElement).dataset.source || '';
            if (
                !['application', 'process'].includes(source) ||
                this.conditionTargetLists[source]?.loading
            )
                return;
            if (!this.app.nativeRendering) {
                this.app.notify(
                    'Running targets are available in the desktop application.',
                    'warning',
                );
                return;
            }
            const list = { ...this.conditionTargetList(source), loading: true, error: '' };
            this.conditionTargetLists[source] = list;
            try {
                const choices = await this.app.bridge.command('condition_targets', { source });
                this.conditionTargetLists[source] = {
                    loaded: true,
                    loading: false,
                    options: Object.entries(choices)
                        .filter((entry): entry is [string, string] => typeof entry[1] === 'string')
                        .map(([value, name]) => ({
                            value,
                            label: name === value ? name : name + ' · ' + value,
                        }))
                        .sort((a, b) => a.label.localeCompare(b.label)),
                    error: '',
                };
            } catch (error) {
                this.conditionTargetLists[source] = {
                    ...list,
                    loading: false,
                    error: String(error),
                };
            }
        },
        // Choose condition target in the current macro editor draft
        chooseConditionTarget(event: Event) {
            const select = event.currentTarget as HTMLSelectElement;
            const node = this.node(select.dataset.path || '');
            if (node.kind !== 'builtin' || !('condition' in node.action) || !select.value) return;
            const condition = node.action.condition;
            if (condition.source === 'application') condition.value = select.value;
            else if (condition.source === 'process') condition.subject = select.value;
            this.publish();
        },
        // Resolve path pickers from the current macro editor state
        pathPickers(action: Action, key: string, part = '') {
            if (part && action.kind === 'builtin' && 'condition' in action.action) {
                const source = action.action.condition.source;
                if (part === 'value' && source === 'application') return applicationPicker;
                if (part === 'subject' && source === 'process') return programPicker;
                return part === 'subject' && source === 'file' ? fileOrFolder : [];
            }
            if (
                action.kind === 'builtin' &&
                action.action.kind === 'write_file' &&
                action.action.operation !== 'create' &&
                key === 'path'
            )
                return [filePicker, ...destinationPicker];
            if (
                key === 'destination' &&
                action.kind === 'builtin' &&
                action.action.kind === 'file_operation' &&
                action.action.operation === 'trash'
            )
                return [];
            return actionPathPickers[actionKey(action)]?.[key] || [];
        },
        // Resolve a built-in action field from its schema
        builtinField(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const node = this.node(input.dataset.path || '');
            const key = input.dataset.field || '';
            if (node.kind !== 'builtin') {
                if (node.kind === 'command' && key === 'program') node.program = input.value;
                else if (
                    (node.kind === 'application' || node.kind === 'open_path') &&
                    key === 'path'
                )
                    node.path = input.value;
                else return;
                this.publish();
                return;
            }
            const fields = node.action as unknown as Record<string, unknown>;
            const type = input.dataset.type;
            if (type === 'number' && input.value === '') return;
            let value: unknown =
                type === 'boolean'
                    ? input.checked
                    : type === 'number'
                      ? Number(input.value)
                      : type === 'lines'
                        ? input.value
                            ? input.value.split('\n')
                            : []
                        : input.value;
            if (input.dataset.part === 'source' && 'condition' in node.action) {
                node.action.condition = {
                    source: input.value,
                    subject: '',
                    operator: conditionComparisons[input.value]?.[0].value || 'equals',
                    value: '',
                };
            } else if (input.dataset.part) {
                const c = fields[key] as Record<string, unknown>;
                if (input.dataset.part === 'value') {
                    value =
                        typeof c.value === 'number'
                            ? Number(input.value)
                            : typeof c.value === 'boolean'
                              ? input.checked
                              : input.value;
                }
                c[input.dataset.part] = value;
            } else if (type === 'primitive') {
                fields[key] =
                    typeof fields[key] === 'number'
                        ? Number(input.value)
                        : typeof fields[key] === 'boolean'
                          ? input.value === 'true'
                          : input.value;
            } else fields[key] = value;
            if (node.action.kind === 'saved_macro' && key === 'macro_id')
                node.action.arguments = {};
            this.publish();
        },
        // Browse action path in the current macro editor draft
        async browseActionPath(event: Event) {
            if (!this.app.nativeRendering) {
                this.app.notify('File selection requires the desktop application.', 'warning');
                return;
            }
            const {
                path = '',
                field = '',
                part = '',
                mode,
            } = (event.currentTarget as HTMLElement).dataset;
            const target = this.node(path);
            const id = this.nodeId(target);
            const source =
                part && target.kind === 'builtin' && 'condition' in target.action
                    ? target.action.condition.source
                    : '';
            const picker = this.pathPickers(target, field, part).find(
                (choice) => choice.mode === mode,
            );
            if (!picker) return;
            try {
                const { open, save } = fileDialogs;
                const title = picker.label.slice(0, -1);
                const pathValue =
                    picker.mode === 'save'
                        ? await save({ title })
                        : await open({
                              title,
                              directory: picker.mode === 'folder',
                              multiple: false,
                          });
                if (typeof pathValue !== 'string') return;
                const selected = ['application', 'process'].includes(source)
                    ? await this.app.bridge.command('condition_target_path', {
                          source,
                          path: pathValue,
                      })
                    : pathValue;
                const node = this.rows.find((row) => row.id === id)?.action;
                if (
                    typeof selected !== 'string' ||
                    !this.host?.isConnected ||
                    !node ||
                    !this.pathPickers(node, field, part).some((choice) => choice.mode === mode)
                )
                    return;
                if (node.kind === 'builtin') {
                    if (part && 'condition' in node.action) {
                        if (node.action.condition.source !== source) return;
                        if (part === 'value') node.action.condition.value = selected;
                        else node.action.condition.subject = selected;
                    } else (node.action as unknown as Record<string, unknown>)[field] = selected;
                } else if (node.kind === 'application' || node.kind === 'open_path') {
                    node.path = selected;
                } else if (node.kind === 'command') node.program = selected;
                this.publish();
            } catch (error) {
                if (!(error instanceof WindowQuiescingError))
                    this.app.notify('Could not open the path chooser. ' + String(error), 'error');
            }
        },
        editorId: crypto.randomUUID(),
        // Build the DOM identifier for an action field
        fieldId(path: string, key: string) {
            return `builtin-${this.editorId}-${path.replaceAll('.', '-')}-${key}`;
        },
        recording: '',
        // Record keys in the current macro editor draft
        recordKeys(event: Event) {
            this.recording = this.nodeId(
                this.node((event.currentTarget as HTMLElement).dataset.path || ''),
            );
            this.error = 'Press the desired key combination.';
        },
        // Capture keyboard input for the selected action draft
        captureKeys(event: KeyboardEvent) {
            if (this.error !== 'Press the desired key combination.') return;
            event.preventDefault();
            event.stopPropagation();
            if (['Control', 'Shift', 'Alt', 'Meta'].includes(event.key)) return;
            const node = this.rows.find((row) => row.id === this.recording)?.action;
            this.error = '';
            this.recording = '';
            const action = node?.kind === 'builtin' ? node.action : node;
            if (action && 'keys' in action) {
                action.keys = [
                    ...(event.ctrlKey ? ['ctrl'] : []),
                    ...(event.metaKey ? ['cmd'] : []),
                    ...(event.altKey ? ['alt'] : []),
                    ...(event.shiftKey ? ['shift'] : []),
                    (
                        {
                            ArrowUp: 'up',
                            ArrowDown: 'down',
                            ArrowLeft: 'left',
                            ArrowRight: 'right',
                        } as Record<string, string>
                    )[event.key] || (event.key === ' ' ? 'space' : event.key.toLowerCase()),
                ];
                this.publish();
            }
        },

        // Insert a new action into the selected macro location
        add(event?: Event) {
            const path = event ? (event.currentTarget as HTMLElement).dataset.path || '' : '';
            const action = this.node(path);
            if (action.kind === 'builtin' && 'children' in action.action) {
                if (action.action.children.length < 100) {
                    const child = defaultAction('delay');
                    action.action.children.push(child);
                    this.selectedId = this.nodeId(child);
                }
                this.publish();
                return;
            }
            if (action.kind !== 'macro') return;
            if (action.actions.length >= 100 || path.split('.').filter(Boolean).length >= 8) {
                this.error = 'Macros allow up to 100 steps per sequence and 8 levels of nesting.';
                return;
            }
            const child = defaultAction('delay');
            action.actions.push(child);
            this.selectedId = this.nodeId(child);
            this.publish();
        },
        // Apply the requested macro-tree operation
        operation(event: Event) {
            const input = event.currentTarget as HTMLElement;
            if (!this.isSequenceChild(input.dataset.path || '')) return;
            const parts = (input.dataset.path || '').split('.');
            const index = Number(parts.pop());
            const parent = this.node(parts.join('.'));
            const actions = actionChildren(parent);
            if (!actions.length || parent.kind === 'toggle') return;
            const op = input.dataset.op;
            if (op === 'remove') actions.splice(index, 1);
            else if (op === 'copy') {
                if (actions.length >= 100) return;
                const copy = cloneJson(actions[index]) as Action;
                actions.splice(index + 1, 0, copy);
                this.selectedId = this.nodeId(copy);
            } else {
                const target = index + (op === 'up' ? -1 : 1);
                if (target < 0 || target >= actions.length) return;
                const [step] = actions.splice(index, 1);
                actions.splice(target, 0, step);
            }
            this.publish();
        },
        // Check whether the action belongs directly to a sequence
        isSequenceChild(path: string) {
            if (!path) return false;
            const parts = path.split('.');
            parts.pop();
            const node = this.node(parts.join('.'));
            return (
                node.kind === 'macro' ||
                (node.kind === 'builtin' &&
                    'children' in node.action &&
                    ![
                        'conditional',
                        'gesture',
                        'press_release',
                        'cooldown',
                        'failure_policy',
                    ].includes(node.action.kind))
            );
        },

        // List states exposed by the selected plugin action
        pluginStateChoices(row: Row) {
            return pluginStateChoices(
                row.action,
                this.app.snapshot.plugins,
                Boolean(row.path) || this.namedMode,
            );
        },
        // Read the selected plugin action state
        pluginDesiredState(row: Row) {
            return row.action.kind === 'plugin' && row.action.user_desired_state !== undefined
                ? String(row.action.user_desired_state)
                : '';
        },
        // Explain why the requested plugin state is invalid
        pluginStateError(row: Row) {
            return this.pluginStateChoices(row).length === 2 &&
                !['0', '1'].includes(this.pluginDesiredState(row))
                ? 'Choose the state this multi-action should request.'
                : '';
        },
        // Check has invalid plugin state for the current macro editor state
        get hasInvalidPluginState() {
            return this.rows.some((row) => Boolean(this.pluginStateError(row)));
        },
        // Update plugin desired state in the current macro editor draft
        changePluginDesiredState(event: Event) {
            const input = event.currentTarget as HTMLSelectElement;
            const row = this.rows.find((row) => row.path === (input.dataset.path || ''));
            if (row && setPluginDesiredState(row.action, this.pluginStateChoices(row), input.value))
                this.publish();
        },
        // Check whether the plugin action provides an inspector
        pluginHasInspector(row: Row) {
            const node = row.action;
            return (
                node.kind === 'plugin' &&
                this.app.snapshot.plugins
                    .find((plugin) => plugin.id === node.plugin_id)
                    ?.actions.some((action) => action.id === node.action_id && action.has_settings)
            );
        },
        // Check whether the plugin action exposes editable settings
        noActionSettings(row: Row) {
            const { action } = row;
            return action.kind === 'builtin'
                ? action.action.kind !== 'keyboard_macro' &&
                      !row.fields.length &&
                      !('children' in action.action)
                : ['none', 'mute', 'sleep'].includes(action.kind);
        },
        // Return the retained JSON text for the plugin settings draft
        pluginJsonText(row: Row) {
            return this.pluginJsonDrafts[row.id]?.text ?? row.settings;
        },
        // Return the validation error for the current plugin JSON draft
        pluginJsonError(row: Row) {
            return this.pluginJsonDrafts[row.id]?.error || '';
        },
        // Parse the plugin JSON draft while retaining invalid text for correction
        changePluginJson(event: Event) {
            const input = event.currentTarget as HTMLTextAreaElement;
            const node = this.node(input.dataset.path || '');
            if (node.kind !== 'plugin') return;
            const id = this.nodeId(node);
            const text = input.value;
            try {
                if (new TextEncoder().encode(text).byteLength > 256 * 1024)
                    throw Error('Plugin settings must fit within 256 KiB.');
                let value: JsonValue;
                try {
                    value = JSON.parse(text);
                } catch {
                    throw Error('Enter valid JSON for the plugin settings.');
                }
                if (value === null || typeof value !== 'object' || Array.isArray(value))
                    throw Error('Plugin settings must be a JSON object.');
                this.pluginJsonDrafts[id] = { text, error: '' };
                node.settings = value;
                this.publish();
            } catch (error) {
                this.pluginJsonDrafts[id] = {
                    text,
                    error: error instanceof Error ? error.message : String(error),
                };
            }
        },
        // Edit keyboard recording in the current macro editor draft
        async editKeyboardRecording(event: Event) {
            const opener = event.currentTarget as HTMLElement;
            const node = this.node(opener.dataset.path || '');
            if (
                node.kind !== 'builtin' ||
                node.action.kind !== 'keyboard_macro' ||
                this.inspectorSession
            )
                return;
            const id = this.nodeId(node);
            const original = JSON.stringify(node.action);
            const session = new AbortController();
            this.inspectorSession = session;
            try {
                const result = await editKeyboardMacro(node.action, opener, session.signal);
                if (
                    result &&
                    !session.signal.aborted &&
                    this.rows.some((row) => row.id === id) &&
                    JSON.stringify(node.action) === original
                ) {
                    node.action = result;
                    this.publish();
                }
            } catch (error) {
                if (!session.signal.aborted) this.app.notify(String(error), 'error');
            } finally {
                if (this.inspectorSession === session) this.inspectorSession = undefined;
            }
        },
        // Open an isolated property inspector for the selected plugin action
        async inspector(event: Event) {
            const opener = event.currentTarget as HTMLElement;
            const path = opener.dataset.path || '';
            const node = this.node(path);
            if (node.kind !== 'plugin' || this.inspectorSession) return;
            const id = this.nodeId(node);
            const row = this.rows.find((row) => row.id === id);
            if (!row || !this.pluginHasInspector(row) || this.pluginJsonError(row)) return;
            const wasEditing = this.editing;
            const original = this.serialized;
            const originalSettings = JSON.stringify(node.settings);
            const session = new AbortController();
            this.inspectorSession = session;
            // Check that the asynchronous result still belongs to the active editor
            const current = () =>
                !session.signal.aborted &&
                this.editing === wasEditing &&
                original === this.serialized &&
                this.rows.some((row) => row.id === id);
            try {
                const settings = node.settings;
                const resources = { ...node.resources };
                const value = await editPluginSettings(
                    node.plugin_id,
                    node.action_id,
                    settings && typeof settings === 'object' && !Array.isArray(settings)
                        ? settings
                        : {},
                    opener,
                    false,
                    session.signal,
                    resources,
                    this.namedMode
                        ? { macro_id: this.namedId, path: path ? path.split('.').map(Number) : [] }
                        : {
                              profile_id: this.app.profile?.id ?? '',
                              page_id: this.app.page?.id ?? '',
                              control_id: this.app.controlId,
                              path: [
                                  ...(this.host?.getAttribute('dial-lane')?.match(/^[012]$/)
                                      ? [Number(this.host.getAttribute('dial-lane'))]
                                      : []),
                                  ...(path ? path.split('.').map(Number) : []),
                              ],
                          },
                );
                if (
                    value !== null &&
                    current() &&
                    JSON.stringify(node.settings) === originalSettings &&
                    !this.pluginJsonError(row)
                ) {
                    delete this.pluginJsonDrafts[id];
                    node.settings = value;
                    node.resources = resources;
                    this.publish();
                }
            } catch (error) {
                if (current() && !(error instanceof WindowQuiescingError))
                    this.app.notify('Could not open action settings. ' + String(error), 'error');
            } finally {
                if (this.inspectorSession === session) this.inspectorSession = undefined;
            }
        },
    });
    Alpine.data('macroEditor', factory(true));
    Alpine.data('actionEditor', factory(false));
}
