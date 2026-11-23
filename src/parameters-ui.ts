import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type {
    ParameterOverrides,
    ProfileParameter,
    CredentialState,
    CredentialTarget,
} from '../generated/runtime';
import { initialValue, parseValue } from './parameter-values';
import { ownWindowMethods } from './window-work';
import { registerCloseDraft } from './close-releases';

function parameterDraft() {
    return {
        definitions: [] as ProfileParameter[],
        values: {} as Record<string, unknown>,
        secrets: {} as Record<string, string>,
        closeBaseline: '',
    };
}

function parameterOwner(draft: ReturnType<typeof parameterDraft>) {
    return {
        name: 'Profile parameters',
        retainOnUnmount: true,
        state() {
            const changed =
                draft.closeBaseline &&
                JSON.stringify([draft.definitions, draft.values]) !== draft.closeBaseline;
            return changed || Object.values(draft.secrets).some(Boolean)
                ? ('dirty' as const)
                : ('clean' as const);
        },
        discard() {
            draft.secrets = {};
            if (draft.closeBaseline)
                [draft.definitions, draft.values] = JSON.parse(draft.closeBaseline);
        },
    };
}

// Only the data needed to resolve an unfinished draft may outlive the Settings DOM.
export function registerParameters(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('profileParameters', () => ({
        parameterDraft: parameterDraft(),
        get definitions() {
            return this.parameterDraft.definitions;
        },
        set definitions(value: ProfileParameter[]) {
            this.parameterDraft.definitions = value;
        },
        get values() {
            return this.parameterDraft.values;
        },
        set values(value: Record<string, unknown>) {
            this.parameterDraft.values = value;
        },
        get secrets() {
            return this.parameterDraft.secrets;
        },
        set secrets(value: Record<string, string>) {
            this.parameterDraft.secrets = value;
        },
        overrides: { installation: {}, devices: {} } as ParameterOverrides,
        statuses: {} as Record<string, CredentialState | 'unsaved'>,
        deviceOnly: false,
        error: '',
        profileId: '',
        credentialBusy: '',
        definitionsBaseline: '[]',
        loadGeneration: 0,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Initialize parameters ui state and its required lifecycle hooks
        async init() {
            ownWindowMethods(this, 'profile parameters', [
                'load',
                'loadValues',
                'save',
                'credentialAction',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                ['save', 'credentialAction'],
                parameterOwner(this.parameterDraft),
            );
            await this.load();
        },
        // Release parameters ui state and associated lifecycle resources
        destroy() {
            this.loadGeneration++;
            this.unregisterCloseOwner?.();
        },
        // Load the data required by the parameters ui workflow
        async load() {
            const generation = ++this.loadGeneration;
            this.profileId = app().profile?.id || '';
            this.definitions = structuredClone(Alpine.raw(app().profile?.parameters || []));
            this.definitionsBaseline = JSON.stringify(this.definitions);
            const overrides = await app().command('parameter_overrides');
            if (generation !== this.loadGeneration) return;
            this.overrides = overrides;
            await this.loadValues();
        },
        // Load values for the parameters ui workflow
        async loadValues() {
            const generation = ++this.loadGeneration;
            this.secrets = {};
            this.values = {
                ...(this.deviceOnly
                    ? this.overrides.devices[app().deviceId]?.[this.profileId]
                    : this.overrides.installation[this.profileId]),
            };
            this.parameterDraft.closeBaseline = JSON.stringify([
                JSON.parse(this.definitionsBaseline),
                this.values,
            ]);
            this.statuses = {};
            for (const parameter of this.definitions.filter((p) => p.sensitive)) {
                let status: CredentialState | 'unsaved';
                try {
                    status = await app().command(
                        'credential_status',
                        { target: this.target(parameter.name) },
                        false,
                    );
                } catch {
                    status = 'unsaved';
                }
                if (generation !== this.loadGeneration) return;
                this.statuses[parameter.name] = status;
            }
        },
        // Build the profile-scoped credential target with an optional device restriction
        target(name: string): CredentialTarget {
            return {
                profile_id: this.profileId,
                name,
                device_id: this.deviceOnly ? app().deviceId : null,
            };
        },
        // Add the parameters ui operation
        add() {
            let index = this.definitions.length + 1;
            while (this.definitions.some((p) => p.name === 'parameter_' + index)) index++;
            if (this.definitions.length < 64)
                this.definitions.push({
                    name: 'parameter_' + index,
                    label: 'Parameter',
                    type: 'string',
                    required: false,
                    default: null,
                    sensitive: false,
                });
        },
        // Remove the parameters ui operation
        remove(event: Event) {
            this.definitions.splice(Number((event.currentTarget as HTMLElement).dataset.index), 1);
        },
        // Reset parameter defaults and clear sensitivity when its type no longer permits it
        typeChanged(event: Event) {
            const parameter =
                this.definitions[Number((event.currentTarget as HTMLElement).dataset.index)];
            if (!parameter) return;
            if (parameter.type !== 'string') parameter.sensitive = false;
            if (parameter.default !== null) parameter.default = initialValue(parameter.type);
        },
        // Clear portable defaults and mark credentials unsaved when a parameter becomes sensitive
        sensitiveChanged(event: Event) {
            const parameter =
                this.definitions[Number((event.currentTarget as HTMLElement).dataset.index)];
            if (parameter?.sensitive) {
                parameter.default = null;
                this.statuses[parameter.name] = 'unsaved';
            }
        },
        // Parse the default editor value or toggle its presence, reporting invalid input
        defaultChanged(event: Event) {
            const el = event.currentTarget as HTMLInputElement;
            const parameter = this.definitions[Number(el.dataset.index)];
            if (!parameter) return;
            try {
                parameter.default = el.dataset.toggle
                    ? el.checked
                        ? initialValue(parameter.type)
                        : null
                    : parseValue(parameter.type, el.type === 'checkbox' ? el.checked : el.value);
                this.error = '';
            } catch (error) {
                this.error = String(error);
            }
        },
        // Enable or remove a nonsensitive override without mutating the existing value map
        useOverride(event: Event) {
            const el = event.currentTarget as HTMLInputElement;
            const parameter = this.definitions.find((p) => p.name === el.dataset.name);
            if (!parameter || parameter.sensitive) return;
            const values = { ...this.values };
            if (el.checked)
                Object.defineProperty(values, parameter.name, {
                    value: parameter.default ?? initialValue(parameter.type),
                    enumerable: true,
                    configurable: true,
                    writable: true,
                });
            else delete values[parameter.name];
            this.values = values;
        },
        // Check whether override is valid for the current parameters ui state
        hasOverride(name: string) {
            return name in this.values;
        },
        // Return the current parameters ui value
        value(event: Event) {
            const el = event.currentTarget as HTMLInputElement;
            const parameter = this.definitions.find((p) => p.name === el.dataset.name);
            if (!parameter || parameter.sensitive) return;
            try {
                this.values[parameter.name] = parseValue(
                    parameter.type,
                    el.type === 'checkbox' ? el.checked : el.value,
                );
                this.error = '';
            } catch (error) {
                this.error = String(error);
            }
        },
        // Format a scalar override for the editor and leave absent or object values blank
        override(name: string) {
            const value = this.values[name];
            return value === undefined || typeof value === 'object' ? '' : String(value);
        },
        // Explain which device override, installation override or portable default takes precedence
        effectiveSource(parameter: ProfileParameter) {
            const installation = this.deviceOnly
                ? this.overrides.installation[this.profileId]
                : this.values;
            const device = this.deviceOnly
                ? this.values
                : this.overrides.devices[app().deviceId]?.[this.profileId];
            if (Object.hasOwn(device || {}, parameter.name))
                return (
                    'Effective source: device override' +
                    (parameter.sensitive ? ' in the OS credential store' : '')
                );
            if (Object.hasOwn(installation || {}, parameter.name))
                return (
                    'Effective source: installation override' +
                    (parameter.sensitive ? ' in the OS credential store' : '')
                );
            return parameter.default !== null
                ? 'Effective source: portable default'
                : parameter.required
                  ? 'Missing required value'
                  : 'No value configured';
        },
        // Translate credential status into actionable editor guidance
        credentialStatus(name: string) {
            const status = this.statuses[name];
            return status === 'needs_reentry'
                ? 'unsupported stored credential; re-enter it'
                : status === 'unsaved' || !status
                  ? 'save definitions first'
                  : status;
        },
        // Write or clear the scoped credential, refresh overrides and erase the temporary secret
        async credentialAction(event: Event) {
            const el = event.currentTarget as HTMLElement;
            const name = el.dataset.name || '';
            if (this.credentialBusy) return;
            this.credentialBusy = name;
            const generation = this.loadGeneration;
            const submitted = this.secrets[name];
            try {
                if (app().profile?.id !== this.profileId)
                    throw Error('Profile changed. Reload parameters.');
                const target = this.target(name);
                const expected_revision = app().configurationRevision;
                const operation = el.dataset.operation;
                if (operation !== 'write' && operation !== 'clear')
                    throw Error('Unsupported credential operation.');
                const status =
                    operation === 'write'
                        ? await app().command('write_credential', {
                              write: { target, expected_revision, value: this.secrets[name] || '' },
                          })
                        : await app().command('clear_credential', {
                              target,
                              expectedRevision: expected_revision,
                          });
                const overrides = await app().command('parameter_overrides');
                if (generation !== this.loadGeneration) return;
                this.overrides = overrides;
                this.statuses[name] = status;
                this.error = '';
                app().notify(
                    operation === 'clear'
                        ? 'Credential cleared for this scope.'
                        : 'Credential stored for this scope.',
                    'success',
                );
            } catch (error) {
                if (generation === this.loadGeneration) this.error = String(error);
            } finally {
                if (generation === this.loadGeneration && this.secrets[name] === submitted)
                    delete this.secrets[name];
                this.credentialBusy = '';
            }
        },
        // Save pending changes through the parameters ui workflow
        async save() {
            try {
                if (app().profile?.id !== this.profileId)
                    throw Error('Profile changed. Reload parameters before saving.');
                const names = new Set(this.definitions.map((p) => p.name));
                this.values = Object.fromEntries(
                    Object.entries(this.values).filter(([name]) => names.has(name)),
                );
                const profile = structuredClone(Alpine.raw(app().profile!));
                profile.parameters = structuredClone(Alpine.raw(this.definitions));
                await app().edit([{ kind: 'save_profile', profile }], 'Edit profile parameters');
                if (this.deviceOnly) {
                    this.overrides.devices[app().deviceId] ||= {};
                    this.overrides.devices[app().deviceId]![this.profileId] = this.values as never;
                } else this.overrides.installation[this.profileId] = this.values as never;
                await app().command('save_parameter_overrides', {
                    value: structuredClone(Alpine.raw(this.overrides)),
                });
                await this.load();
                this.error = '';
                app().notify(
                    'Profile parameters saved. Local values stay on this installation.',
                    'success',
                );
            } catch (error) {
                this.error = String(error);
            }
        },
    }));
}
