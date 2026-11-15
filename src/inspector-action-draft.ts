import { cloneJson } from './json-clone';
import type { Action } from './types';

const clone = cloneJson;
const fieldKinds = new Set([
    'none',
    'open_url',
    'hotkey',
    'application',
    'open_path',
    'command',
    'media',
    'volume',
    'brightness',
    'mute',
    'sleep',
    'macro',
    'plugin',
    'toggle',
    'lighting',
    'profile',
    'page',
    'navigate',
    'swipe_navigation',
    'builtin',
    'delay',
]);

/** One action owns all editor modes. Raw text is kept separately until it is valid */
export class InspectorActionDraft {
    action: Action;
    advanced: boolean;
    rawValue: string | null = null;

    // Clone the supplied action and choose its initial editing mode
    constructor(action: Action = { kind: 'none' }) {
        this.action = clone(action);
        this.advanced = !fieldKinds.has(action.kind);
    }

    // Read the action kind represented by the inspector draft
    get kind() {
        return this.advanced ? 'custom' : this.action.kind;
    }

    // Switch the inspector draft to structured JSON editing
    editJSON() {
        this.rawValue = JSON.stringify(this.action, null, 2);
        this.advanced = true;
    }

    // Read or update the inspector field draft without committing the binding
    get value(): string {
        return this.rawValue ?? this.formattedValue();
    }

    // Format the inspector draft for editing
    private formattedValue(): string {
        const action = this.action;
        if (this.advanced) return JSON.stringify(action, null, 2);
        switch (action.kind) {
            case 'open_url':
                return action.url;
            case 'hotkey':
                return action.keys.join(' + ');
            case 'application':
            case 'open_path':
                return action.path;
            case 'volume':
                return String(action.delta);
            case 'brightness':
                return String(action.value);
            case 'media':
                return action.command;
            case 'macro':
                return JSON.stringify(action.actions);
            default:
                return '';
        }
    }

    // Read or update the inspector field draft without committing the binding
    set value(text: string) {
        this.rawValue = text;
        try {
            this.action = this.parseValue();
            if (text === this.formattedValue()) this.rawValue = null;
        } catch {
            // Invalid/incomplete input remains recoverable without corrupting the last action
        }
    }

    // Parse and validate the current inspector field text
    private parseValue(): Action {
        const text = this.rawValue;
        if (text === null) return this.action;
        if (this.advanced) {
            const value: unknown = JSON.parse(text);
            if (
                !value ||
                typeof value !== 'object' ||
                Array.isArray(value) ||
                !('kind' in value) ||
                typeof value.kind !== 'string'
            )
                throw Error('An action must be a JSON object with a kind.');
            // Native validation owns action schemas, including future action kinds and fields
            return value as Action;
        }
        const action = this.action;
        switch (action.kind) {
            case 'open_url':
                return { ...action, url: text };
            case 'hotkey':
                return {
                    ...action,
                    keys: text
                        .split('+')
                        .map((key) => key.trim())
                        .filter(Boolean),
                };
            case 'application':
            case 'open_path':
                return { ...action, path: text };
            case 'media':
                return { ...action, command: text || 'play-pause' };
            case 'volume':
            case 'brightness': {
                const number = Number(text);
                if (!text.trim() || !Number.isFinite(number)) throw Error('Enter a valid number.');
                return action.kind === 'volume'
                    ? { ...action, delta: number }
                    : { ...action, value: number };
            }
            case 'macro': {
                const actions: unknown = JSON.parse(text);
                if (!Array.isArray(actions)) throw Error('Macro actions must be a JSON array.');
                return { ...action, actions: actions as Action[] };
            }
            default:
                return action;
        }
    }

    // Write the validated inspector field draft into the action model
    commit(): Action {
        return clone(this.parseValue());
    }

    // Restore the inspector field draft from its saved baseline
    static restore(value: Record<string, unknown>): InspectorActionDraft {
        const saved = value.actionDraft as Partial<InspectorActionDraft> | undefined;
        if (
            !saved ||
            typeof saved !== 'object' ||
            Array.isArray(saved) ||
            !saved.action ||
            typeof saved.action !== 'object' ||
            Array.isArray(saved.action) ||
            typeof saved.action.kind !== 'string' ||
            !saved.action.kind ||
            typeof saved.advanced !== 'boolean' ||
            !(saved.rawValue === null || typeof saved.rawValue === 'string')
        )
            throw Error(
                'Unsupported saved draft. Discard it explicitly before editing this action.',
            );
        const draft = new InspectorActionDraft(saved.action);
        draft.advanced = saved.advanced;
        draft.rawValue = saved.rawValue;
        return draft;
    }
}
