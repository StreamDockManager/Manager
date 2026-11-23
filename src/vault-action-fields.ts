import type { BuiltinAction } from '../generated/runtime';

// Keep authority literal. The host validates and encodes substituted values again.
export function vaultUrlSuffixStart(value: string) {
    const origin = /^https?:\/\/([^/?#]+)/.exec(value);
    if (!origin || /[\s${}@]/.test(origin[1]) || /[\\\x00-\x1f\x7f-\x9f]/.test(value)) return -1;
    try {
        if (!new URL(origin[0]).hostname) return -1;
        return origin[0].length;
    } catch {
        return -1;
    }
}

export type VaultTextSelection = { value: string; start: number; end: number };

export function insertVaultTextReference(
    value: string,
    token: string,
    selection?: VaultTextSelection,
) {
    if (
        selection?.value === value &&
        selection.start >= 0 &&
        selection.end >= selection.start &&
        selection.end <= value.length
    )
        return value.slice(0, selection.start) + token + value.slice(selection.end);
    return value.includes('${vault.unlinked}')
        ? value.replace('${vault.unlinked}', () => token)
        : value + token;
}

export function insertVaultUrlReference(
    value: string,
    token: string,
    selection?: VaultTextSelection,
) {
    const start = vaultUrlSuffixStart(value);
    if (start < 0) return null;
    const suffix = value.slice(start);
    if (!suffix) return value + '/' + token;
    const selectedSuffix =
        selection && selection.value === value && selection.start > start
            ? { value: suffix, start: selection.start - start, end: selection.end - start }
            : undefined;
    return value.slice(0, start) + insertVaultTextReference(suffix, token, selectedSuffix);
}

// Match the text sinks supported by native Vault interpolation
export function builtinVaultFields(action: BuiltinAction) {
    switch (action.kind) {
        case 'type_text':
        case 'paste_text':
        case 'set_clipboard':
            return [{ key: 'text', label: 'Text' }];
        case 'write_file':
            return [{ key: 'text', label: 'Contents' }];
        case 'notification':
        case 'input_prompt':
            return [
                { key: 'text', label: 'Message' },
                { key: 'title', label: 'Title' },
            ];
        case 'action_menu':
            return [
                { key: 'title', label: 'Title' },
                ...action.labels.map((_, index) => ({
                    key: 'choice-' + index,
                    label: 'Choice ' + (index + 1),
                })),
            ];
        case 'open_link':
        case 'search_selection':
            return [{ key: 'url', label: 'URL' }];
        case 'date_time':
            return [{ key: 'format', label: 'Format' }];
        case 'clipboard_slot':
            return [{ key: 'name', label: 'Slot name' }];
        case 'reveal_file':
            return [{ key: 'path', label: 'Path' }];
        case 'conditional':
        case 'wait_condition':
            if (action.condition.operator === 'matches_vault_json')
                return [{ key: 'json', label: 'Expected value' }];
            if (
                action.condition.source === 'file' &&
                ['exists', 'missing'].includes(action.condition.operator)
            )
                return [{ key: 'condition-path', label: 'File or folder path' }];
            if (
                action.condition.source === 'process' &&
                ['exists', 'missing'].includes(action.condition.operator)
            )
                return [{ key: 'condition-subject', label: 'Process name or executable path' }];
            if (
                ['clipboard', 'application'].includes(action.condition.source) &&
                ['equals', 'not_equals', 'contains'].includes(action.condition.operator) &&
                typeof action.condition.value === 'string'
            )
                return [{ key: 'condition-value', label: 'Comparison text' }];
            return [];
        default:
            return [];
    }
}

// Insert into one exact field without opting the action into run-output expansion
export function insertBuiltinVaultReference(
    action: BuiltinAction,
    field: string,
    token: string,
    selection?: VaultTextSelection,
) {
    if (!builtinVaultFields(action).some(({ key }) => key === field)) return false;
    const insert = (text: string) => insertVaultTextReference(text, token, selection);
    if (field === 'url' && (action.kind === 'open_link' || action.kind === 'search_selection')) {
        const url = insertVaultUrlReference(action.url, token, selection);
        if (url === null) return false;
        action.url = url;
    } else if (
        field === 'json' &&
        (action.kind === 'conditional' || action.kind === 'wait_condition')
    )
        action.condition.value = token;
    else if (
        (action.kind === 'conditional' || action.kind === 'wait_condition') &&
        (field === 'condition-path' || field === 'condition-subject' || field === 'condition-value')
    ) {
        if (field === 'condition-path') action.condition.subject = token;
        else if (field === 'condition-subject')
            action.condition.subject = insert(action.condition.subject);
        else action.condition.value = insert(String(action.condition.value));
    } else if (field === 'format' && action.kind === 'date_time')
        action.format = insert(action.format);
    else if (field === 'name' && action.kind === 'clipboard_slot')
        action.name = insert(action.name);
    else if (field === 'path' && action.kind === 'reveal_file') {
        action.path = token;
        action.template = true;
    } else if (field === 'text' && 'text' in action) action.text = insert(action.text);
    else if (field === 'title' && 'title' in action) action.title = insert(action.title);
    else if (field.startsWith('choice-') && action.kind === 'action_menu') {
        const index = Number(field.slice('choice-'.length));
        action.labels[index] = insert(action.labels[index]!);
    } else return false;
    return true;
}
