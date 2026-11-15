import { validateKeyboardMacro } from './keyboard-macro-draft';
import type { Action, MacroInput, SavedMacro } from '../generated/runtime';
import { actionChildren } from './builtin-actions';
// Check whether the macro input name uses the supported identifier format
const validName = (name: string) => /^[A-Za-z0-9_]{1,80}$/.test(name);
// Check whether the default value matches the declared macro input type
const validType = (kind: string, value: unknown) =>
    typeof value === kind && (kind !== 'number' || Number.isFinite(value));
/** Immediate editor feedback. Rust repeats validation at the persistence/call boundary. */
export function validateMacroInputs(inputs: MacroInput[]) {
    const names = new Set<string>();
    if (inputs.length > 64) throw Error('A macro supports up to 64 inputs.');
    for (const input of inputs) {
        if (
            !validName(input.name) ||
            names.has(input.name) ||
            !input.label.trim() ||
            !['string', 'number', 'boolean'].includes(input.type) ||
            (input.default !== null && !validType(input.type, input.default))
        )
            throw Error('Check input names, labels, types, and defaults. Names must be unique.');
        names.add(input.name);
    }
}
// Validate the action draft and its references to declared macro inputs
export function validateMacroDraft(action: Action, inputs: MacroInput[], macros: SavedMacro[]) {
    validateMacroInputs(inputs);
    // Inspect the current action node and recurse through its nested actions
    function visit(node: Action) {
        if (node.kind === 'builtin' && node.action.kind === 'keyboard_macro') {
            const error = validateKeyboardMacro(node.action);
            if (error) throw Error(error);
        }
        if (node.kind === 'builtin' && node.action.kind === 'saved_macro') {
            const call = node.action;
            const target = macros.find((m) => m.id === call.macro_id);
            if (target) {
                const definitions = target.inputs || [];
                for (const name of Object.keys(call.arguments || {}))
                    if (!definitions.some((i) => i.name === name))
                        throw Error('This macro call has an unknown argument.');
                for (const input of definitions) {
                    const arg = call.arguments?.[input.name];
                    if (!arg && input.required && input.default === null)
                        throw Error('Assign required input ' + input.label + '.');
                    if (arg?.kind === 'literal' && !validType(input.type, arg.value))
                        throw Error('Check the type of input ' + input.label + '.');
                    if (
                        arg?.kind === 'input' &&
                        !inputs.some((i) => i.name === arg.name && i.type === input.type)
                    )
                        throw Error('Choose a compatible caller input for ' + input.label + '.');
                    if (arg?.kind === 'parameter' && !validName(arg.name))
                        throw Error('Choose a profile parameter for ' + input.label + '.');
                    if (arg?.kind === 'output' && !/^[A-Za-z0-9_.]{1,160}$/.test(arg.name))
                        throw Error('Enter a prior run output for ' + input.label + '.');
                }
            }
        }
        actionChildren(node).forEach(visit);
    }
    visit(action);
}
