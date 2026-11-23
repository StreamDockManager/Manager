import { passwordDialog, unlockVault } from './vault-password';
import { oldVaultMessage, vaultFormatError } from './vault-errors';
import { desktopVaultTransferService, registerVaultTransfer } from './vault-transfer';
import * as fileDialogs from './window-dialogs';
import { openPluginPermissions } from './plugin-permissions';
import Alpine from '@alpinejs/csp';
import { bridge, native } from './bridge';
import { app } from './route-context';
import { nextTab } from './tab-navigation';
import { adjacentPage, paginate } from './pagination';
import { renderVaultValue } from './vault-value-view';
import { copyVaultValueText } from './vault-value-clipboard';
import { vaultUrlSuffixStart } from './vault-action-fields';
import { walkAction } from './builtin-actions';
import type { Action, Snapshot } from './types';
import type {
    JsonValue,
    SettingsLocationFix,
    VaultAccess,
    VaultSettingsPlugin,
    VaultSettingsRow,
    VaultSettingsDocument,
    VaultEntry,
    VaultEntryInput,
    VaultKind,
    VaultMetadataRevision,
    VaultPersonalEntries,
    VaultStatus,
} from '../generated/runtime';

export type { VaultEntry, VaultStatus };
export type VaultTab = 'entries' | 'access' | 'plugins' | 'usage';
export type VaultEntryKind = VaultKind;
const tabs = [
    { id: 'entries', label: 'Entries' },
    { id: 'access', label: 'Access' },
    { id: 'plugins', label: 'Plugins' },
    { id: 'usage', label: 'Usage' },
] as const;
const personalEntriesPageSize = 10;
const usageExamples = [
    { id: 'text', label: 'Text field', code: 'Hello, ${vault.<vault-id>.<entry-id>}' },
    { id: 'entry', label: 'Whole entry reference', code: '${vault.<vault-id>.<entry-id>}' },
    { id: 'location', label: 'Location field', code: '${vault.<vault-id>.<entry-id>.latitude}' },
] as const;
// Mirror the native reference parser's lower-case, hyphenated UUID pair.
const uuid = '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}';
const entryReferenceId = new RegExp('^' + uuid + '\\.' + uuid + '$');
const kinds: { id: VaultEntryKind; label: string; protectedDefault: boolean }[] = [
    { id: 'text', label: 'Text', protectedDefault: false },
    { id: 'integer', label: 'Integer', protectedDefault: false },
    { id: 'boolean', label: 'Boolean', protectedDefault: false },
    { id: 'json', label: 'JSON', protectedDefault: false },
    { id: 'file', label: 'File path', protectedDefault: true },
    { id: 'folder', label: 'Folder path', protectedDefault: true },
    { id: 'location', label: 'Location', protectedDefault: true },
    { id: 'api-key', label: 'API key', protectedDefault: true },
    { id: 'bearer-token', label: 'Bearer token', protectedDefault: true },
    { id: 'credential', label: 'Other secret', protectedDefault: true },
];

export function groupVaultEntries(entries: VaultEntry[]) {
    const own: VaultEntry[] = [];
    const owners: Record<string, VaultEntry[]> = Object.create(null);
    const unavailable: Record<string, VaultEntry[]> = Object.create(null);
    for (const entry of entries) {
        if (entry.owner == null) own.push(entry);
        else if (entry.owner_verified === true) (owners[entry.owner] ??= []).push(entry);
        else (unavailable[entry.owner] ??= []).push(entry);
    }
    return { own, owners, unavailable };
}

export function parseVaultInput(kind: VaultEntryKind, value: string): JsonValue {
    if (kind === 'integer') {
        if (!/^-?[0-9]+$/.test(value) || !Number.isSafeInteger(Number(value)))
            throw Error('Enter a whole number.');
        return Number(value);
    }
    if (kind === 'boolean') {
        if (value !== 'true' && value !== 'false') throw Error('Choose True or False.');
        return value === 'true';
    }
    if (kind === 'json' || kind === 'location') {
        let parsed: JsonValue;
        try {
            parsed = JSON.parse(value);
        } catch {
            throw Error(kind === 'json' ? 'Enter valid JSON.' : 'Enter valid coordinates.');
        }
        if (kind === 'location') {
            const { latitude, longitude, name, source, accuracy_m } = (parsed ?? {}) as Record<
                string,
                unknown
            >;
            if (typeof name !== 'string' || !name.trim() || name.length > 128)
                throw Error('Enter a place name.');
            if (
                (source !== undefined && source !== 'os') ||
                (accuracy_m !== undefined &&
                    (typeof accuracy_m !== 'number' ||
                        !Number.isFinite(accuracy_m) ||
                        accuracy_m < 0))
            )
                throw Error('Enter valid location details.');
            if (
                typeof latitude !== 'number' ||
                typeof longitude !== 'number' ||
                !Number.isFinite(latitude) ||
                !Number.isFinite(longitude) ||
                Math.abs(latitude) > 90 ||
                Math.abs(longitude) > 180
            )
                throw Error('Enter valid coordinates.');
        }
        return parsed;
    }
    return value;
}

// This view reads saved configuration and unlocked metadata. Runtime authorization remains native.
export function configuredVaultUses(
    snapshot: Pick<Snapshot, 'profiles' | 'saved_macros'>,
    entries: VaultEntry[],
) {
    const labels = {
        type_text: 'Type text',
        paste_text: 'Paste text',
        set_clipboard: 'Set clipboard',
        notification: 'Show notification',
        input_prompt: 'Input prompt',
        action_menu: 'Action menu',
        write_file: 'Write file',
        open_link: 'Open link',
        search_selection: 'Search selection',
        reveal_file: 'Reveal file',
        conditional: 'Conditional',
        wait_condition: 'Wait for condition',
    };
    const byId = new Map(entries.map((entry) => [entry.id, entry]));
    const rows: { source: string; action: string; entry: string; status: string }[] = [];
    let truncated = false;
    function inspect(source: string, root: Action) {
        walkAction(root, (node) => {
            if ((node.kind !== 'builtin' && node.kind !== 'open_url') || truncated) return;
            const field =
                node.kind === 'builtin'
                    ? node.action
                    : {
                          kind: 'open_link' as const,
                          url: node.url,
                          template: false,
                      };
            const url = field.kind === 'open_link' || field.kind === 'search_selection';
            const jsonCondition = field.kind === 'conditional' || field.kind === 'wait_condition';
            const path = field.kind === 'reveal_file';
            let values: string[];
            if (jsonCondition) {
                if (field.condition.operator !== 'matches_vault_json') return;
                const value = field.condition.value;
                values = [
                    typeof value === 'string'
                        ? value
                        : value &&
                            typeof value === 'object' &&
                            'vault' in value &&
                            typeof value.vault === 'string'
                          ? value.vault
                          : '',
                ];
            } else if (path) {
                if (!field.template) return;
                values = [field.path];
            } else {
                switch (field.kind) {
                    case 'open_link':
                    case 'search_selection':
                        values = [field.url];
                        break;
                    case 'type_text':
                    case 'paste_text':
                    case 'set_clipboard':
                    case 'write_file':
                        values = [field.text];
                        break;
                    case 'notification':
                    case 'input_prompt':
                        values = [field.title, field.text];
                        break;
                    case 'action_menu':
                        values = [field.title, ...field.labels];
                        break;
                    default:
                        return;
                }
            }
            const seen = new Set<string>();
            for (const text of values) {
                const urlSuffix = url ? vaultUrlSuffixStart(text) : 0;
                for (const match of text.matchAll(/\$\{([^}]*)\}/g)) {
                    if (!match[1].startsWith('vault.')) continue;
                    const ref = match[1].slice(6);
                    if (seen.has(ref)) continue;
                    seen.add(ref);
                    if (rows.length === 250) {
                        truncated = true;
                        return;
                    }
                    const parts = ref.split('.');
                    const locationField =
                        parts.length === 3 && ['latitude', 'longitude', 'name'].includes(parts[2]);
                    const entry = byId.get(locationField ? parts.slice(0, 2).join('.') : ref);
                    const eligible =
                        entry &&
                        !entry.owner &&
                        urlSuffix >= 0 &&
                        match.index >= urlSuffix &&
                        (jsonCondition
                            ? ((entry.kind === 'json' && !entry.protected && !locationField) ||
                                  (entry.kind === 'location' && locationField)) &&
                              match[0] === text
                            : path
                              ? (entry.kind === 'file' || entry.kind === 'folder') &&
                                match[0] === text
                              : !entry.protected &&
                                (locationField
                                    ? entry.kind === 'location'
                                    : [
                                          'text',
                                          'integer',
                                          'boolean',
                                          'json',
                                          'location',
                                          'file',
                                          'folder',
                                      ].includes(entry.kind)));
                    rows.push({
                        source,
                        action: node.kind === 'open_url' ? 'Open website' : labels[field.kind],
                        entry: entry && !entry.owner ? entry.label : '',
                        status: eligible
                            ? 'Configured'
                            : entry
                              ? 'Incompatible entry'
                              : 'Choose a local entry',
                    });
                }
            }
        });
    }
    for (const macro of snapshot.saved_macros) inspect(macro.name, macro.action);
    for (const profile of snapshot.profiles) {
        for (const page of profile.pages) {
            for (const [control, binding] of Object.entries(page.bindings)) {
                if (binding)
                    inspect([profile.name, page.name, control].join(' / '), binding.action);
            }
        }
    }
    return { rows, truncated };
}

type VaultLoadState = 'idle' | 'loading' | 'ready' | 'failed';
type SupplementalVaultTab = 'access' | 'plugins';
type VaultMetadataCache = {
    epoch: number;
    entries: { revision: string; rows: VaultEntry[] } | null;
};

const metadataByService = new WeakMap<VaultUIService, VaultMetadataCache>();

// Invalidate in-flight publications as well as cached metadata
function clearMetadata(cache: VaultMetadataCache) {
    cache.epoch++;
    cache.entries = null;
}

export interface VaultUIService {
    metadataRevision?(): Promise<VaultMetadataRevision>;
    personalEntries?(): Promise<VaultPersonalEntries | null>;
    pluginEntries?(): Promise<VaultEntry[]>;
    settingsPlugins?(): Promise<VaultSettingsPlugin[]>;
    settingsDocument?(id: string, reveal: boolean): Promise<VaultSettingsDocument>;
    saveSettings?(id: string, value: JsonValue): Promise<void>;
    deleteSettings?(id: string, review: string): Promise<void>;
    deleteSettingsRow?(id: string): Promise<void>;
    status(): Promise<VaultStatus>;
    initialize(): Promise<VaultStatus>;
    unlock(): Promise<VaultStatus>;
    password?(mode: 'create' | 'change' | 'toOs' | 'toPassword'): Promise<VaultStatus>;
    reviewReset?(): Promise<string>;
    cancelReset?(token: string): Promise<void>;
    reset?(confirmation: string, token: string): Promise<VaultStatus>;
    lock(): Promise<VaultStatus>;
    entries(): Promise<VaultEntry[]>;
    reveal?(
        id: string,
        revision: string,
        password?: string,
    ): Promise<{ kind: VaultKind; value: JsonValue }>;
    currentLocation?(id: string): Promise<SettingsLocationFix>;
    cancelLocation?(id: string): Promise<void>;
    access?(): Promise<VaultAccess[]>;
    revoke?(grant: VaultAccess): Promise<void>;
    create(input: VaultEntryInput): Promise<unknown>;
    replace(id: string, revision: string, value: JsonValue): Promise<unknown>;
    rename(id: string, revision: string, label: string): Promise<unknown>;
    setProtection?(
        id: string,
        revision: string,
        protectedValue: boolean,
        password: string | null,
    ): Promise<unknown>;
    delete(id: string, revision: string): Promise<unknown>;
}

function desktopService(): VaultUIService {
    return {
        settingsPlugins: () => bridge.command('vault_settings_plugins'),
        settingsDocument: (id, reveal) => bridge.command('vault_settings_document', { id, reveal }),
        saveSettings: (id, value) => bridge.command('vault_save_settings', { id, value }),
        deleteSettings: (id, review) =>
            bridge.command('vault_delete_plugin_settings', { id, review }),
        deleteSettingsRow: (id) => bridge.command('vault_delete_settings', { id }),
        currentLocation: (id) => bridge.command('vault_current_location', { id }),
        cancelLocation: (id) => bridge.command('vault_cancel_location', { id }),
        metadataRevision: () => bridge.command('vault_metadata_revision'),
        status: () => bridge.command('vault_status'),
        initialize: () => bridge.command('vault_initialize'),
        unlock: unlockVault,
        password: passwordDialog,
        reviewReset: () => bridge.command('vault_review_reset'),
        cancelReset: (token) => bridge.command('vault_cancel_reset', { token }),
        reset: (confirmation, token) =>
            bridge.command('vault_delete_keystore', { confirmation, token }),
        lock: () => bridge.command('vault_lock'),
        personalEntries: () =>
            bridge.command('vault_personal_entries').catch((error: unknown) => {
                if (
                    !/(?:unknown command|command .*not found|not allowed|not registered)/i.test(
                        String(error),
                    )
                )
                    throw error;
                return null;
            }),
        entries: async () =>
            (await bridge.command('vault_entries')).filter((entry) => !entry.owner),
        pluginEntries: async () =>
            (await bridge.command('vault_entries')).filter((entry) => entry.owner),
        reveal: (id, revision, password) =>
            bridge.command('vault_reveal_entry', { id, revision, password: password ?? null }),
        access: () => bridge.command('vault_access'),
        revoke: (grant) =>
            bridge.command('vault_revoke', {
                pluginId: grant.plugin_id,
                slotId: grant.slot_id,
                action: grant.action,
                entryId: grant.entry.id,
                revision: grant.entry.revision,
            }),
        create: (input) => bridge.command('vault_create_entry', { input }),
        replace: (id, revision, value) =>
            bridge.command('vault_replace_entry', { id, revision, value }),
        rename: (id, revision, label) =>
            bridge.command('vault_rename_entry', { id, revision, label }),
        setProtection: (id, revision, protectedValue, password) =>
            bridge.command('vault_set_entry_protection', {
                id,
                revision,
                protected: protectedValue,
                password,
            }),
        delete: (id, revision) => bridge.command('vault_delete_entry', { id, revision }),
    };
}

type EntryEditor = 'create' | 'replace' | 'rename' | 'protection' | 'delete' | '';

const settingsGroups = ['Global settings', 'Action defaults', 'Action instances'] as const;
type VaultPluginPanel = {
    key: string;
    settingsId: string;
    name: string;
    identity: string;
    summary: string;
    groups: { name: string; rows: VaultSettingsRow[] }[];
    entries: VaultEntry[];
};

export function vaultController(
    service: VaultUIService | null,
    pluginName = (id: string) => id,
    configuration?: () => Pick<Snapshot, 'profiles' | 'saved_macros'>,
    linkedPlugin: string | null = null,
) {
    // Keep the opened value out of Alpine state, attributes and notifications
    const cache = service
        ? (metadataByService.get(service) ?? { epoch: 0, entries: null })
        : { epoch: 0, entries: null };
    if (service) metadataByService.set(service, cache);
    const revealedClipboardText = new WeakMap<HTMLElement, string>();
    return {
        openPermissions() {
            openPluginPermissions();
        },
        usageExamples,
        kinds,
        tab: (linkedPlugin === null ? 'entries' : 'plugins') as VaultTab,
        get visibleTabs() {
            return this.unlocked ? tabs : tabs.filter((tab) => tab.id === 'usage');
        },
        get displayedTab(): VaultTab {
            return this.unlocked ? this.tab : 'usage';
        },
        async copyVaultText(text: string, success: string, failure: string) {
            try {
                await copyVaultValueText(text);
                app().notify(success, 'success');
            } catch {
                app().notify(failure, 'error');
            }
        },
        // Copy the published example, never the referenced entry or its value.
        copyUsageExample(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.example;
            const example = usageExamples.find((item) => item.id === id);
            if (example)
                return this.copyVaultText(
                    example.code,
                    'Example copied',
                    'Could not copy the example. Select the snippet to copy it.',
                );
        },
        canCopyEntryReference(entry: VaultEntry) {
            return (
                entry.owner === null &&
                typeof entry.id === 'string' &&
                entry.id.length === 73 &&
                entryReferenceId.test(entry.id)
            );
        },
        copyEntryReference(event: Event) {
            if (!this.unlocked || this.working || this.displayedTab !== 'entries') return;
            const id = (event.currentTarget as HTMLElement).dataset.entryId;
            const entry = this.metadataEntries.find((item) => item.id === id);
            if (!entry || !this.canCopyEntryReference(entry)) return;
            return this.copyVaultText(
                '${vault.' + entry.id + '}',
                'Vault reference copied',
                'Could not copy the reference. Use Entry ID to build it manually.',
            );
        },
        expandedAccess: '',
        expandedPlugin: '',
        requestedPlugin: linkedPlugin ?? '',
        expandedSettings: '',
        supplementalPages: {} as Record<string, number>,
        supplementalPage<T>(key: string, rows: T[]) {
            return paginate(rows, this.supplementalPages[key] ?? 1, personalEntriesPageSize);
        },
        changeSupplementalPage(event: Event) {
            if (!this.unlocked || this.working || this.supplementalTabState !== 'ready') return;
            const button = event.currentTarget as HTMLButtonElement;
            const { key, direction, page, pages, tab } = button.dataset;
            if (tab !== this.displayedTab || !key) return;
            const by = Number(direction);
            const current = Number(page);
            const total = Number(pages);
            if (
                (by !== -1 && by !== 1) ||
                !Number.isSafeInteger(current) ||
                !Number.isSafeInteger(total) ||
                current < 1 ||
                total < current
            )
                return;
            this.supplementalPages[key] = adjacentPage({ page: current, pages: total }, by);
            if (key === 'plugins') this.expandedPlugin = this.expandedSettings = '';
            if (key === 'access' && this.expandedAccess !== 'builtins') this.expandedAccess = '';
            button.closest('nav')?.nextElementSibling?.scrollTo(0, 0);
        },
        get pluginPanelPage() {
            return this.supplementalPage('plugins', this.pluginPanels);
        },
        get accessGroupPage() {
            return this.supplementalPage('access', this.accessGroups);
        },
        clampSupplementalPage<T>(key: string, rows: T[]) {
            if (this.supplementalPages[key] !== undefined)
                this.supplementalPages[key] = this.supplementalPage(key, rows).page;
        },
        // Keep each accordion's current page unless refreshed rows no longer fill it
        reconcileSupplementalPages() {
            if (!this.unlocked || this.supplementalTabState !== 'ready') return;
            if (this.tab === 'access') {
                const groups = this.accessGroups;
                this.clampSupplementalPage('access', groups);
                this.clampSupplementalPage('builtins', this.builtinUses.rows);
                for (const group of groups) this.clampSupplementalPage(group.key, group.grants);
                if (
                    this.expandedAccess !== 'builtins' &&
                    !this.supplementalPage('access', groups).items.some(
                        (group) => group.key === this.expandedAccess,
                    )
                )
                    this.expandedAccess = '';
                return;
            }
            if (this.tab !== 'plugins') return;
            const panels = this.pluginPanels;
            this.clampSupplementalPage('plugins', panels);
            for (const plugin of panels) {
                this.clampSupplementalPage(this.sectionId('private', plugin.key), plugin.entries);
                for (const group of plugin.groups)
                    this.clampSupplementalPage(
                        this.sectionId('settings', plugin.key + ':' + group.name),
                        group.rows,
                    );
            }
            const panel = this.supplementalPage('plugins', panels).items.find(
                (plugin) => plugin.key === this.expandedPlugin,
            );
            if (!panel) this.expandedPlugin = this.expandedSettings = '';
            else if (
                (this.expandedSettings === 'private' && !panel.entries.length) ||
                (this.expandedSettings !== 'private' &&
                    !panel.groups.some((group) => group.name === this.expandedSettings))
            )
                this.expandedSettings = '';
        },
        itemCount(count: number, label: string) {
            return (
                count + ' ' + (count === 1 ? label : label === 'entry' ? 'entries' : label + 's')
            );
        },
        sectionId(scope: string, key: string) {
            return 'vault-' + scope + '-' + encodeURIComponent(key);
        },
        toggleSection(event: Event) {
            const { accordion, section } = (event.currentTarget as HTMLButtonElement).dataset;
            if (!section) return;
            if (accordion === 'access')
                this.expandedAccess = this.expandedAccess === section ? '' : section;
            else if (accordion === 'plugin') {
                this.expandedPlugin = this.expandedPlugin === section ? '' : section;
                this.expandedSettings = '';
            } else if (accordion === 'settings')
                this.expandedSettings = this.expandedSettings === section ? '' : section;
        },
        collapseSections() {
            this.expandedAccess = this.expandedPlugin = this.expandedSettings = '';
            this.supplementalPages = {};
        },
        hasHost: !!service,
        status: null as VaultStatus | null,
        entries: [] as VaultEntry[],
        grants: [] as VaultAccess[],
        get builtinUses() {
            if (this.tab !== 'access' || !this.unlocked || !configuration)
                return { rows: [], truncated: false };
            return configuredVaultUses(configuration(), this.entries);
        },
        get accessGroups() {
            const groups = new Map<
                string,
                { id: string; key: string; label: string; grants: VaultAccess[] }
            >();
            for (const grant of this.grants) {
                let group = groups.get(grant.plugin_id);
                if (!group) {
                    group = {
                        id: grant.plugin_id,
                        key: 'grant:' + grant.plugin_id,
                        label: pluginName(grant.plugin_id),
                        grants: [],
                    };
                    groups.set(grant.plugin_id, group);
                }
                group.grants.push(grant);
            }
            return [...groups.values()].sort((a, b) => a.label.localeCompare(b.label));
        },
        working: false,
        error: '',
        settingsPlugins: [] as VaultSettingsPlugin[],
        selectedSettings: '',
        settingsTitle: '',
        settingsDocument: null as VaultSettingsDocument | null,
        settingsText: '',
        settingsError: '',
        settingsDeleteId: '',
        settingsDeleteMode: '' as '' | 'single' | 'all',
        settingsDeleteLabel: '',
        settingsDeleteReview: '',
        settingsDeleteError: '',
        get canDeleteSettings() {
            return !!service?.deleteSettings;
        },
        get canDeleteSettingsRow() {
            return !!service?.deleteSettingsRow;
        },
        reviewSettingsDelete(event: Event) {
            if (!this.unlocked || this.working) return;
            const button = event.currentTarget as HTMLButtonElement;
            const rowId = button.dataset.revision;
            const plugin = this.settingsPlugins.find((item) =>
                rowId
                    ? item.rows.some((row) => row.revision === rowId)
                    : item.id === button.dataset.pluginId,
            );
            if (!plugin) return;
            if (rowId) {
                if (!this.canDeleteSettingsRow) return;
                this.settingsDeleteMode = 'single';
                this.settingsDeleteId = rowId;
                this.settingsDeleteLabel = button.dataset.label || plugin.name;
            } else {
                if (!this.canDeleteSettings || !plugin.settings_review) return;
                this.settingsDeleteMode = 'all';
                this.settingsDeleteId = plugin.id;
                this.settingsDeleteReview = plugin.settings_review;
                this.settingsDeleteLabel = plugin.name;
            }
            this.settingsDeleteError = '';
            if (typeof document !== 'undefined')
                document.querySelector<HTMLDialogElement>('#vault-settings-delete')?.showModal();
        },
        closeSettingsDelete() {
            this.settingsDeleteId = '';
            this.settingsDeleteMode = '';
            this.settingsDeleteReview = '';
            this.settingsDeleteLabel = '';
            this.settingsDeleteError = '';
            if (typeof document !== 'undefined') {
                const dialog = document.querySelector<HTMLDialogElement>('#vault-settings-delete');
                if (dialog?.open) dialog.close();
            }
        },
        cancelSettingsDelete() {
            if (!this.working) this.closeSettingsDelete();
        },
        async confirmSettingsDelete() {
            const id = this.settingsDeleteId;
            const mode = this.settingsDeleteMode;
            const review = this.settingsDeleteReview;
            if (!id || !mode || !this.unlocked || this.working) return;
            if (mode === 'all' ? !review || !service?.deleteSettings : !service?.deleteSettingsRow)
                return;
            this.settingsDeleteError = '';
            await this.run(
                async (owner, revision) => {
                    if (mode === 'all') await owner.deleteSettings!(id, review);
                    else await owner.deleteSettingsRow!(id);
                    if (this.retired || this.revision !== revision || !this.unlocked) return;
                    if (mode === 'all') {
                        this.settingsPlugins = this.settingsPlugins.filter(
                            (plugin) => plugin.id !== id,
                        );
                    } else {
                        this.settingsPlugins = this.settingsPlugins.flatMap((plugin) => {
                            const rows = plugin.rows.filter((row) => row.revision !== id);
                            return rows.length !== plugin.rows.length
                                ? rows.length
                                    ? [{ ...plugin, rows, settings_review: null }]
                                    : []
                                : [plugin];
                        });
                    }
                    this.closeSettingsDelete();
                    this.pluginsLoad = 'idle';
                    await this.loadSelectedTab();
                },
                (error) => {
                    const message = String(error);
                    this.settingsDeleteError = message.includes('Disable the plugin')
                        ? 'Disable the plugin and close its settings, then try again.'
                        : message.includes('draft')
                          ? 'Finish or discard the open binding edits, then review the deletion again.'
                          : 'Could not delete these settings. They may have changed; close this dialog and refresh to review them again.';
                    return this.settingsDeleteError;
                },
            );
        },
        get settingsFields() {
            return Object.entries(this.settingsDocument?.fields || {});
        },
        familyLabel(plugin: VaultSettingsPlugin) {
            const family = (
                {
                    vsd: 'VSD Craft',
                    openaction: 'OpenDeck',
                    streamdeck: 'Elgato Stream Deck',
                } as Record<string, string>
            )[plugin.dialect || ''];
            return [
                plugin.installed ? 'Installed' : 'Retained',
                family,
                plugin.native_package ? 'Manager package' : '',
            ]
                .filter(Boolean)
                .join(' · ');
        },
        async inspectSettings(event: Event) {
            const button = event.currentTarget as HTMLButtonElement;
            const id = button.dataset.revision!;
            this.closeSettings();
            this.selectedSettings = id;
            this.settingsTitle = button.getAttribute('aria-label') || 'Plugin settings';
            await this.run(async (owner, revision) => {
                const doc = await owner.settingsDocument!(id, false);
                if (this.revision !== revision || !this.unlocked || this.selectedSettings !== id)
                    return;
                this.settingsDocument = doc;
                document.querySelector<HTMLDialogElement>('#vault-settings-editor')!.showModal();
            });
        },
        async revealSettings() {
            const id = this.selectedSettings;
            await this.run(async (owner, revision) => {
                const doc = await owner.settingsDocument!(id, true);
                if (this.revision !== revision || !this.unlocked || this.selectedSettings !== id)
                    return;
                this.settingsDocument = doc;
                this.settingsText = doc.json ?? '';
            });
        },
        async saveSettings() {
            if (!this.settingsDocument?.json || !this.unlocked) return;
            let value: JsonValue;
            try {
                value = JSON.parse(this.settingsText);
            } catch {
                this.settingsError = 'Enter valid JSON.';
                return;
            }
            const id = this.selectedSettings;
            const saved = await this.run(async (owner, revision) => {
                try {
                    await owner.saveSettings!(id, value);
                } catch (error) {
                    if (this.revision === revision) this.settingsError = String(error);
                    throw error;
                }
                await this.load(owner, revision, await owner.status());
            });
            if (saved) this.closeSettings();
        },
        closeSettings() {
            this.closeSettingsDelete();
            this.selectedSettings = '';
            this.settingsDocument = null;
            this.settingsText = '';
            this.settingsError = '';
            if (typeof document !== 'undefined') {
                const dialog = document.querySelector<HTMLDialogElement>('#vault-settings-editor');
                const input = dialog?.querySelector('textarea');
                if (input) input.value = '';
                if (dialog?.open) dialog.close();
            }
        },
        valueEntry: null as VaultEntry | null,
        valueOpener: null as HTMLButtonElement | null,
        valueReady: false,
        valueWorking: false,
        valueError: '',
        valueSequence: 0,
        get canViewValues() {
            return !!service?.reveal;
        },
        get valueNeedsPassword() {
            return (
                this.valueEntry?.protected === true &&
                this.status?.mode === 'password' &&
                !this.valueReady
            );
        },
        valueDialog() {
            return typeof document === 'undefined'
                ? null
                : document.querySelector<HTMLDialogElement>('#vault-value-dialog');
        },
        constrainValueView() {
            if (
                this.valueEntry &&
                (!this.unlocked ||
                    this.displayedTab !== 'entries' ||
                    (typeof document !== 'undefined' && document.hidden))
            )
                this.closeValueView();
        },
        valueViewClosed(event: Event) {
            // A queued native close event must not tear down a newer dialog.
            if (!(event.currentTarget as HTMLDialogElement).open) this.closeValueView();
        },
        closeValueView() {
            if (!this.valueEntry && !this.valueReady) return;
            ++this.valueSequence;
            const dialog = this.valueDialog();
            const password = dialog?.querySelector<HTMLInputElement>('input[name="valuePassword"]');
            const value = dialog?.querySelector<HTMLElement>('[data-vault-value-content]');
            if (password) password.value = '';
            if (value) {
                revealedClipboardText.delete(value);
                const selection = document.getSelection();
                if (selection?.anchorNode && value.contains(selection.anchorNode))
                    selection.removeAllRanges();
                value.replaceChildren();
            }
            const opener = this.valueOpener;
            this.valueOpener = null;
            this.valueEntry = null;
            this.valueReady = false;
            this.valueWorking = false;
            this.valueError = '';
            if (dialog?.open) dialog.close();
            if (this.unlocked && this.displayedTab === 'entries' && opener?.isConnected)
                opener.focus({ preventScroll: true });
        },
        copyViewedValue() {
            const entry = this.valueEntry;
            const dialog = this.valueDialog();
            if (
                !entry ||
                !this.valueReady ||
                !this.unlocked ||
                this.retired ||
                this.displayedTab !== 'entries' ||
                document.hidden ||
                !dialog?.open ||
                !this.metadataEntries.some(
                    (item) =>
                        item.id === entry.id &&
                        item.revision === entry.revision &&
                        item.owner == null,
                )
            )
                return;
            const content = dialog.querySelector<HTMLElement>('[data-vault-value-content]');
            const text = content ? revealedClipboardText.get(content) : undefined;
            if (text !== undefined)
                return this.copyVaultText(
                    text,
                    'Value copied',
                    'Could not copy the value to the clipboard.',
                );
        },
        openValueView(event: Event) {
            if (
                !service?.reveal ||
                !this.unlocked ||
                this.working ||
                this.displayedTab !== 'entries'
            )
                return;
            const opener = event.currentTarget as HTMLButtonElement;
            const entry = this.metadataEntries.find(
                (item) => item.id === opener.dataset.entryId && item.owner == null,
            );
            const dialog = this.valueDialog();
            if (!entry || !dialog || dialog.open) return;
            this.valueEntry = entry;
            this.valueOpener = opener;
            dialog.showModal();
            if (!entry.protected) return this.readValueView();
            if (!this.valueNeedsPassword) {
                this.valueError =
                    'Protected values can only be viewed when Vault uses a master password.';
                return;
            }
            void Alpine.nextTick(() => {
                if (dialog.open && this.valueNeedsPassword)
                    dialog.querySelector<HTMLInputElement>('input[name="valuePassword"]')?.focus();
            });
        },
        submitValuePassword(event: SubmitEvent) {
            if (!this.valueNeedsPassword || this.valueWorking) return;
            const form = event.currentTarget as HTMLFormElement;
            const input = form.elements.namedItem('valuePassword') as HTMLInputElement | null;
            const password = input?.value;
            if (!input || !password) return;
            input.value = '';
            return this.readValueView(password);
        },
        async readValueView(password?: string) {
            const entry = this.valueEntry;
            if (
                !service?.reveal ||
                !this.unlocked ||
                this.displayedTab !== 'entries' ||
                this.valueWorking ||
                !entry ||
                entry.owner != null ||
                (entry.protected && (!password || this.status?.mode !== 'password'))
            )
                return;
            const sequence = ++this.valueSequence;
            const current = () =>
                sequence === this.valueSequence &&
                this.unlocked &&
                this.displayedTab === 'entries' &&
                !this.retired &&
                (!entry.protected || this.status?.mode === 'password') &&
                this.valueDialog()?.open &&
                this.metadataEntries.some(
                    (item) =>
                        item.id === entry.id &&
                        item.revision === entry.revision &&
                        item.owner == null,
                );
            this.valueWorking = true;
            this.valueError = '';
            try {
                const response = service.reveal(entry.id, entry.revision, password);
                password = undefined;
                const reply = await response;
                if (!current()) return;
                if (reply.kind !== entry.kind) throw Error('stale_entry_revision');
                const content = this.valueDialog()?.querySelector<HTMLElement>(
                    '[data-vault-value-content]',
                );
                if (!content) return;
                renderVaultValue(content, reply.kind, reply.value);
                revealedClipboardText.set(
                    content,
                    typeof reply.value === 'string'
                        ? reply.value
                        : JSON.stringify(reply.value, null, 2),
                );
                this.valueReady = true;
                this.valueDialog()?.querySelector<HTMLButtonElement>('[data-close-value]')?.focus();
            } catch (error) {
                if (!current()) return;
                const code = error instanceof Error ? error.message : String(error);
                this.valueError =
                    code === 'incorrect_password'
                        ? 'That master password is incorrect.'
                        : code === 'rate_limited'
                          ? 'Too many attempts. Wait and try again.'
                          : [
                                  'stale_entry_revision',
                                  'entry_missing',
                                  'vault_reveal_expired',
                              ].includes(String(code))
                            ? 'This entry or session changed. Close this window and refresh Vault.'
                            : code === 'vault_locked'
                              ? 'Vault is locked. Unlock it before trying again.'
                              : /(?:command\s+vault_reveal_entry\s+not found|unknown command.*vault_reveal_entry|vault_reveal_entry.*(?:not allowed|not registered|unavailable))/i.test(
                                      code,
                                  )
                                ? 'This Manager build cannot view Vault values. Quit Manager and launch the rebuilt desktop app.'
                                : code === 'invalid_value_reply'
                                  ? 'The saved value could not be displayed in this format.'
                                  : 'This value could not be viewed. Close this window and try again.';
            } finally {
                if (sequence === this.valueSequence) {
                    this.valueWorking = false;
                    if (this.valueError && this.valueNeedsPassword)
                        void Alpine.nextTick(() => {
                            if (sequence === this.valueSequence && this.valueDialog()?.open)
                                this.valueDialog()
                                    ?.querySelector<HTMLInputElement>('input[name="valuePassword"]')
                                    ?.focus();
                        });
                }
            }
        },
        editor: '' as EntryEditor,
        draftEntry: null as VaultEntry | null,
        draftKind: 'text' as VaultEntryKind,
        locationRequest: '',
        canLocate: !!service?.currentLocation,
        canReset: !!service?.reset,
        canTransferEntries: native,
        supportsPassword: !!service?.password,
        resetPhrase: '',
        resetToken: '',
        locationFix: null as SettingsLocationFix | null,
        draftLabel: '',
        protect: false,
        get canChangeProtection() {
            return !!service?.setProtection && this.status?.mode === 'password';
        },
        revision: 0,
        retired: false,
        get unlocked() {
            return this.status?.state === 'unlocked';
        },
        get canUnlock() {
            return (
                this.status?.state === 'locked' ||
                (this.status?.state === 'unavailable' && this.status.mode === 'os')
            );
        },
        pluginEntries: [] as VaultEntry[],
        get metadataEntries() {
            return this.pluginEntries.length
                ? [...this.entries, ...this.pluginEntries]
                : this.entries;
        },
        get entriesByOwner() {
            return groupVaultEntries(this.metadataEntries);
        },
        personalPageIndex: 1,
        entriesLoad: 'idle' as VaultLoadState,
        accessLoad: 'idle' as VaultLoadState,
        pluginsLoad: 'idle' as VaultLoadState,
        supplementalRevision: 0,
        get supplementalTabState(): VaultLoadState {
            if (this.entriesLoad === 'failed') return 'failed';
            if (this.tab === 'access') return this.accessLoad;
            if (this.tab === 'plugins') return this.pluginsLoad;
            return 'idle';
        },
        clearSupplemental() {
            ++this.supplementalRevision;
            this.grants = [];
            this.settingsPlugins = [];
            this.pluginEntries = [];
            this.accessLoad = 'idle';
            this.pluginsLoad = 'idle';
        },
        // Supplemental reads are independent of Entries and begin only when the tab is selected
        async loadSelectedTab() {
            if (!service || !this.unlocked || this.retired || this.entriesLoad !== 'ready') return;
            const tab = this.tab;
            if (tab !== 'access' && tab !== 'plugins') return;
            if (this.supplementalTabState === 'loading' || this.supplementalTabState === 'ready')
                return;
            const revision = this.supplementalRevision;
            const current = () =>
                !this.retired && this.unlocked && revision === this.supplementalRevision;
            this.setSupplementalState(tab, 'loading');
            try {
                if (tab === 'access') {
                    const grants = (await service.access?.()) ?? [];
                    if (current()) this.grants = grants;
                } else {
                    const [plugins, entries] = await Promise.all([
                        service.settingsPlugins?.() ?? [],
                        service.pluginEntries?.() ?? [],
                    ]);
                    if (current()) {
                        this.settingsPlugins = plugins;
                        this.pluginEntries = entries;
                    }
                }
                if (current()) {
                    this.setSupplementalState(tab, 'ready');
                    this.reconcileSupplementalPages();
                    if (tab === 'plugins') this.openRequestedPlugin();
                }
            } catch {
                if (current()) this.setSupplementalState(tab, 'failed');
            }
        },
        // Expand only the current installation once its protected lists are loaded
        openRequestedPlugin() {
            const id = this.requestedPlugin;
            if (!id || this.tab !== 'plugins' || !this.unlocked) return;
            this.requestedPlugin = '';
            const key = this.settingsPlugins.some((plugin) => plugin.id === id && plugin.installed)
                ? 'settings:' + id
                : 'verified:' + id;
            const index = this.pluginPanels.findIndex((plugin) => plugin.key === key);
            if (index < 0) return;
            this.supplementalPages.plugins = Math.floor(index / personalEntriesPageSize) + 1;
            this.expandedPlugin = key;
            this.expandedSettings = '';
            void Alpine.nextTick(() => {
                if (this.retired || this.displayedTab !== 'plugins' || this.expandedPlugin !== key)
                    return;
                document
                    .getElementById(this.sectionId('plugin', key))
                    ?.parentElement?.scrollIntoView({ block: 'nearest' });
            });
        },
        retrySelectedTab() {
            return this.entriesLoad === 'failed' ? this.refresh() : this.loadSelectedTab();
        },
        setSupplementalState(tab: SupplementalVaultTab, state: VaultLoadState) {
            if (tab === 'access') this.accessLoad = state;
            else this.pluginsLoad = state;
        },
        get personalEntriesLoading() {
            return this.unlocked && this.entriesLoad === 'loading';
        },
        get personalEntriesEmpty() {
            return !this.personalPage.total && (this.entriesLoad === 'ready' || !this.hasHost);
        },
        get personalPage() {
            return paginate(
                this.entriesByOwner.own,
                this.personalPageIndex,
                personalEntriesPageSize,
            );
        },
        changePersonalPage(event: Event) {
            if (!this.unlocked || this.working || this.displayedTab !== 'entries') return;
            const button = event.currentTarget as HTMLButtonElement;
            const direction = Number(button.dataset.direction);
            if (direction !== -1 && direction !== 1) return;
            this.personalPageIndex = adjacentPage(this.personalPage, direction);
            button
                .closest('#vault-panel-entries')
                ?.querySelector('[data-personal-entry-scroll]')
                ?.scrollTo(0, 0);
        },
        get pluginPanels(): VaultPluginPanel[] {
            if (this.tab !== 'plugins' || !this.unlocked) return [];
            const { owners, unavailable } = this.entriesByOwner;
            const consumedOwners = new Set<string>();
            const panels: VaultPluginPanel[] = this.settingsPlugins.map((plugin) => {
                // Only merge current, verified ownership. Retained settings and an unavailable
                // prior installation never make its private entries part of the current plugin.
                const entries = plugin.installed ? (owners[plugin.id] ?? []) : [];
                if (entries.length) consumedOwners.add(plugin.id);
                const byGroup = new Map<string, VaultSettingsRow[]>(
                    settingsGroups.map((group) => [group, []]),
                );
                for (const row of plugin.rows) {
                    if (!byGroup.has(row.group)) byGroup.set(row.group, []);
                    byGroup.get(row.group)!.push(row);
                }
                return {
                    key: 'settings:' + plugin.id,
                    settingsId: plugin.settings_review ? plugin.id : '',
                    name: plugin.name,
                    identity: [
                        this.familyLabel(plugin),
                        entries.length ? 'Verified installation' : '',
                    ]
                        .filter(Boolean)
                        .join(' · '),
                    summary: [
                        this.itemCount(plugin.rows.length, 'setting'),
                        entries.length ? this.itemCount(entries.length, 'entry') : '',
                    ]
                        .filter(Boolean)
                        .join(' · '),
                    groups: [...byGroup].flatMap(([name, rows]) =>
                        rows.length ? [{ name, rows }] : [],
                    ),
                    entries,
                };
            });
            for (const [status, groups] of [
                ['Verified installation', owners],
                ['Original installation unavailable', unavailable],
            ] as const) {
                for (const [id, entries] of Object.entries(groups)) {
                    const verified = groups === owners;
                    if (verified && consumedOwners.has(id)) continue;
                    panels.push({
                        key: (verified ? 'verified:' : 'unavailable:') + id,
                        settingsId: '',
                        name: verified ? pluginName(id) : id,
                        identity: status,
                        summary: this.itemCount(entries.length, 'entry'),
                        groups: [],
                        entries,
                    });
                }
            }
            return panels.sort(
                (a, b) => a.name.localeCompare(b.name) || a.key.localeCompare(b.key),
            );
        },
        get draftTitle() {
            return (
                {
                    create: 'Add entry',
                    replace: 'Replace value',
                    rename: 'Rename entry',
                    protection: 'Change protection',
                    delete: 'Delete entry',
                    '': '',
                } as const
            )[this.editor];
        },
        get needsValue() {
            return this.editor === 'create' || this.editor === 'replace';
        },
        get needsLabel() {
            return this.editor === 'create' || this.editor === 'rename';
        },
        get removingProtection() {
            return this.editor === 'protection' && !!this.draftEntry?.protected && !this.protect;
        },
        get protectionNeedsPassword() {
            return this.removingProtection && this.status?.mode === 'password';
        },
        get protectionBlocked() {
            return this.editor === 'protection' && !this.canChangeProtection;
        },
        get protectionUnchanged() {
            return this.editor === 'protection' && this.draftEntry?.protected === this.protect;
        },
        get maskValue() {
            return this.protect;
        },
        changeDraftKind(event: Event) {
            const value = (event.target as HTMLSelectElement).value;
            this.protect = kinds.some((kind) => kind.id === value && kind.protectedDefault);
            this.clearDraftValue();
        },
        get statusMessage() {
            if (!service) return 'Vault is available in the desktop app.';
            switch (this.status?.state) {
                case 'uninitialized':
                    return 'Create a local Vault for your variables and secrets.';
                case 'locked':
                    return 'Unlock Vault to see or use your entries.';
                case 'unlocked':
                    return 'Vault is unlocked.';
                case 'recovery_required':
                    return 'Vault needs recovery. Complete any interrupted operation. If this is an earlier development Vault, choose Reset Vault to start fresh.';
                case 'unavailable':
                    return 'The protected storage backend is unavailable.';
                default:
                    return this.working ? 'Checking Vault…' : 'Vault is unavailable.';
            }
        },
        init() {
            return this.refreshMetadata(false);
        },
        async openReset() {
            if (
                this.working ||
                !service?.reviewReset ||
                !this.status ||
                this.status.state === 'uninitialized'
            )
                return;
            this.resetPhrase = '';
            this.closeReset();
            await this.run(async (owner, revision) => {
                const token = await owner.reviewReset!();
                if (revision !== this.revision || this.retired) {
                    await owner.cancelReset?.(token);
                    return;
                }
                this.resetToken = token;
                const dialog = (this as unknown as { $refs: { resetDialog: HTMLDialogElement } })
                    .$refs.resetDialog;
                dialog.showModal();
            });
        },
        closeReset() {
            const token = this.resetToken;
            this.resetToken = '';
            if (token) void service?.cancelReset?.(token).catch(() => {});
            this.resetPhrase = '';
            const dialog = (this as unknown as { $refs?: { resetDialog?: HTMLDialogElement } })
                .$refs?.resetDialog;
            if (dialog?.open) dialog.close();
        },
        async confirmReset() {
            const phrase = this.resetPhrase;
            if (
                this.working ||
                !service?.reset ||
                !this.resetToken ||
                phrase !== 'CLEAR VAULT ENTRIES'
            )
                return;
            const token = this.resetToken;
            this.resetToken = '';
            clearMetadata(cache);
            this.clearSupplemental();
            this.closeReset();
            this.closeEditor();
            this.closeValueView();
            this.closeSettings();
            this.collapseSections();
            this.settingsPlugins = [];
            this.personalPageIndex = 1;
            this.entriesLoad = 'idle';
            this.entries = [];
            this.grants = [];
            const saved = await this.run(async (owner, revision) => {
                await this.load(owner, revision, await owner.reset!(phrase, token));
            });
            if (!saved && !this.retired) {
                await this.refresh();
                if (!this.retired && this.status?.state === 'recovery_required')
                    this.error =
                        'Clearing Vault entries and grants is incomplete. Restore access to the protected store and choose Reset Vault again.';
            }
        },
        sessionEnded() {
            clearMetadata(cache);
            this.clearSupplemental();
            this.closeValueView();
            this.closeReset();
            ++this.revision;
            this.working = false;
            this.closeSettings();
            this.collapseSections();
            this.settingsPlugins = [];
            this.personalPageIndex = 1;
            this.entriesLoad = 'idle';
            this.entries = [];
            this.grants = [];
            this.status = { state: 'locked', mode: this.status?.mode ?? 'unknown' };
            this.closeEditor();
            return this.refresh();
        },
        destroy() {
            this.clearSupplemental();
            this.closeValueView();
            this.closeReset();
            this.retired = true;
            ++this.revision;
            this.closeSettings();
            this.collapseSections();
            this.settingsPlugins = [];
            this.personalPageIndex = 1;
            this.entriesLoad = 'idle';
            this.entries = [];
            this.grants = [];
            this.closeEditor();
        },
        async run(
            operation: (owner: VaultUIService, revision: number) => Promise<void>,
            failure?: (error: unknown) => string,
        ) {
            if (!service || this.working || this.retired) return false;
            this.working = true;
            this.error = '';
            const revision = ++this.revision;
            try {
                await operation(service, revision);
                return !this.retired && revision === this.revision;
            } catch (error) {
                if (!this.retired && revision === this.revision)
                    this.error =
                        failure?.(error) ??
                        'Vault could not complete this operation. Refresh and try again.';
                return false;
            } finally {
                if (!this.retired && revision === this.revision) this.working = false;
            }
        },
        async load(owner: VaultUIService, revision: number, status?: VaultStatus, force = true) {
            if (this.retired || revision !== this.revision) return;
            this.clearSupplemental();
            if (force) clearMetadata(cache);
            const epoch = cache.epoch;
            const current = () =>
                !this.retired && revision === this.revision && epoch === cache.epoch;
            this.entriesLoad = 'loading';
            try {
                // Never display data from a prior visit until the live backend confirms its token
                const initial = await owner.metadataRevision?.().catch(() => null);
                if (!current()) return;
                const latest = initial ?? status ?? (await owner.status());
                if (!current()) return;
                this.status = latest;
                if (!this.unlocked) {
                    this.clearLockedView();
                    return;
                }
                const token = initial?.entries_revision ?? null;
                const hit = token && cache.entries?.revision === token ? cache.entries : null;
                let entries = hit?.rows;
                if (!entries) {
                    const fetchRows = async () => {
                        const snapshot = await owner.personalEntries?.();
                        return snapshot ?? { revision: null, entries: await owner.entries() };
                    };
                    let snapshot = await fetchRows();
                    let verified = await owner.metadataRevision?.().catch(() => null);
                    if (!current()) return;
                    // A concurrent mutation can advance the token after the list was captured
                    if (
                        snapshot.revision &&
                        verified?.state === 'unlocked' &&
                        verified.entries_revision !== snapshot.revision
                    ) {
                        snapshot = await fetchRows();
                        verified = await owner.metadataRevision?.().catch(() => null);
                        if (!current()) return;
                        if (
                            snapshot.revision &&
                            verified?.state === 'unlocked' &&
                            verified.entries_revision !== snapshot.revision
                        )
                            throw new Error('Vault metadata changed during the reload');
                    }
                    const after =
                        verified ?? (owner.metadataRevision ? await owner.status() : latest);
                    if (!current()) return;
                    this.status = after;
                    if (!this.unlocked) {
                        this.clearLockedView();
                        return;
                    }
                    // Keep only entry metadata both in the route and in the cross-visit cache
                    entries = snapshot.entries.map(
                        ({
                            id,
                            label,
                            kind,
                            revision,
                            owner,
                            owner_verified,
                            protected: protectedValue,
                        }) => ({
                            id,
                            label,
                            kind,
                            revision,
                            owner,
                            owner_verified,
                            protected: protectedValue,
                        }),
                    );
                    if (snapshot.revision && verified?.entries_revision === snapshot.revision) {
                        cache.entries = { revision: snapshot.revision, rows: entries };
                    } else {
                        cache.entries = null;
                    }
                }
                this.entries = entries;
                this.personalPageIndex = this.personalPage.page;
                if (
                    this.valueEntry &&
                    !entries.some(
                        (entry) =>
                            entry.id === this.valueEntry?.id &&
                            entry.revision === this.valueEntry.revision,
                    )
                )
                    this.closeValueView();
                this.entriesLoad = 'ready';
                await this.loadSelectedTab();
            } catch (error) {
                if (current()) {
                    const latest = await owner.status().catch(() => null);
                    if (current()) {
                        this.status = latest;
                        this.clearLockedView();
                        this.entriesLoad = this.unlocked ? 'failed' : 'idle';
                    }
                }
                throw error;
            }
        },
        clearLockedView() {
            cache.entries = null;
            this.closeValueView();
            this.closeSettings();
            this.collapseSections();
            this.clearSupplemental();
            this.personalPageIndex = 1;
            this.entriesLoad = 'idle';
            this.entries = [];
            this.closeEditor();
        },
        refreshMetadata(force: boolean) {
            return this.run((owner, revision) => this.load(owner, revision, undefined, force));
        },
        refresh() {
            return this.refreshMetadata(true);
        },
        initialize() {
            return this.run(async (owner, revision) =>
                this.load(owner, revision, await owner.initialize()),
            );
        },
        createPasswordVault() {
            return this.run(async (owner, revision) =>
                this.load(owner, revision, await owner.password!('create')),
            );
        },
        switchMode() {
            this.closeValueView();
            const mode = this.status?.mode === 'password' ? 'toOs' : 'toPassword';
            return this.run(async (owner, revision) =>
                this.load(owner, revision, await owner.password!(mode)),
            );
        },
        changePassword() {
            this.closeValueView();
            return this.run(async (owner, revision) =>
                this.load(owner, revision, await owner.password!('change')),
            );
        },
        unlock() {
            return this.run(
                async (owner, revision) => {
                    try {
                        await this.load(owner, revision, await owner.unlock());
                    } catch (error) {
                        if (vaultFormatError(error))
                            await this.load(owner, revision, await owner.status());
                        throw error;
                    }
                },
                (error) =>
                    vaultFormatError(error)
                        ? oldVaultMessage
                        : 'Vault could not complete this operation. Refresh and try again.',
            );
        },
        lock() {
            if (this.working || !this.unlocked) return;
            clearMetadata(cache);
            this.clearSupplemental();
            this.status = { ...this.status!, state: 'locked' };
            this.closeValueView();
            this.closeSettings();
            this.collapseSections();
            this.settingsPlugins = [];
            this.personalPageIndex = 1;
            this.entriesLoad = 'idle';
            this.entries = [];
            this.grants = [];
            this.closeEditor();
            return this.run(async (owner, revision) =>
                this.load(owner, revision, await owner.lock()),
            );
        },
        lastReadLabel(grant: VaultAccess) {
            if (!grant.last_read) return '';
            return (
                'Last successful read ' + new Date(Number(grant.last_read) * 1000).toLocaleString()
            );
        },
        grantKey(grant: VaultAccess) {
            return JSON.stringify([grant.plugin_id, grant.slot_id, grant.action, grant.entry.id]);
        },
        revokeGrant(event: Event) {
            if (!this.unlocked) return;
            const key = (event.currentTarget as HTMLElement).dataset.grantKey;
            const grant = this.grants.find((item) => this.grantKey(item) === key);
            if (!grant) return;
            return this.run(async (owner, revision) => {
                await owner.revoke?.(grant);
                await this.load(owner, revision, await owner.status());
            });
        },
        selectTab(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.vaultTab;
            if (this.visibleTabs.some((tab) => tab.id === id)) {
                this.tab = id as VaultTab;
                this.reconcileSupplementalPages();
                if (this.pluginsLoad === 'ready') this.openRequestedPlugin();
                void this.loadSelectedTab();
                (event.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
            }
        },
        tabKey(event: KeyboardEvent) {
            const tab = nextTab(
                this.visibleTabs.map(({ id }) => id),
                this.displayedTab,
                event.key,
            );
            if (!tab) return;
            event.preventDefault();
            this.tab = tab;
            this.reconcileSupplementalPages();
            if (this.pluginsLoad === 'ready') this.openRequestedPlugin();
            void this.loadSelectedTab();
            document.getElementById('vault-tab-' + tab)?.focus();
        },
        kindLabel(kind: VaultEntryKind) {
            return kinds.find((item) => item.id === kind)?.label ?? kind;
        },
        openEntry(event: Event) {
            this.closeValueView();
            if (!this.unlocked || this.working) return;
            const { mode, entryId } = (event.currentTarget as HTMLElement).dataset;
            const entry = entryId
                ? this.metadataEntries.find((value) => value.id === entryId)
                : undefined;
            if (
                mode !== 'create' &&
                (!entry ||
                    !['replace', 'rename', 'protection', 'delete'].includes(mode ?? '') ||
                    (mode === 'protection' && (!this.canChangeProtection || entry.owner != null)))
            )
                return;
            this.editor = mode as EntryEditor;
            this.draftEntry = entry ?? null;
            this.draftKind = entry?.kind ?? 'text';
            this.draftLabel = entry?.label ?? '';
            this.protect = entry?.protected ?? false;
            this.error = '';
            const dialog = (this as unknown as { $refs: { entryDialog: HTMLDialogElement } }).$refs
                .entryDialog;
            if (!dialog.open) dialog.showModal();
        },
        get pathEntry() {
            return this.draftKind === 'file' || this.draftKind === 'folder';
        },
        async choosePath() {
            if (!native || !this.pathEntry || !this.needsValue || this.working) return;
            const dialog = (this as unknown as { $refs: { entryDialog: HTMLDialogElement } }).$refs
                .entryDialog;
            const field = dialog.querySelector<HTMLInputElement>('input[name="entryValue"]');
            const editor = this.editor;
            const kind = this.draftKind;
            const revision = this.revision;
            try {
                const path = await fileDialogs.open({
                    directory: kind === 'folder',
                    multiple: false,
                });
                if (
                    field?.isConnected &&
                    dialog.open &&
                    this.editor === editor &&
                    this.draftKind === kind &&
                    revision === this.revision &&
                    typeof path === 'string'
                )
                    field.value = path;
            } catch {
                if (dialog.open && revision === this.revision)
                    this.error = 'The path selector is unavailable.';
            }
        },
        async chooseLocation() {
            if (
                !service?.currentLocation ||
                this.locationRequest ||
                !this.unlocked ||
                !this.needsValue ||
                this.draftKind !== 'location' ||
                this.working
            )
                return;
            const dialog = (this as unknown as { $refs: { entryDialog: HTMLDialogElement } }).$refs
                .entryDialog;
            const id = crypto.randomUUID();
            this.locationRequest = id;
            this.locationFix = null;
            this.error = '';
            try {
                const fix = await service.currentLocation(id);
                if (this.locationRequest !== id || !dialog.open || !this.unlocked) return;
                const latitude = dialog.querySelector<HTMLInputElement>('[name="latitude"]');
                const longitude = dialog.querySelector<HTMLInputElement>('[name="longitude"]');
                if (latitude && longitude) {
                    latitude.value = String(fix.latitude);
                    longitude.value = String(fix.longitude);
                    this.locationFix = fix;
                }
            } catch {
                if (this.locationRequest === id)
                    this.error = 'Current location is unavailable. Enter coordinates or try again.';
            } finally {
                if (this.locationRequest === id) this.locationRequest = '';
            }
        },
        clearLocation() {
            const pending = this.locationRequest;
            this.locationRequest = '';
            this.locationFix = null;
            if (pending) void service?.cancelLocation?.(pending).catch(() => {});
        },
        clearDraftValue() {
            this.clearLocation();
            const dialog = (this as unknown as { $refs?: { entryDialog?: HTMLDialogElement } })
                .$refs?.entryDialog;
            for (const input of dialog?.querySelectorAll<HTMLInputElement>(
                '[name="entryValue"], [name="locationName"], [name="latitude"], [name="longitude"]',
            ) ?? [])
                input.value = '';
        },
        closeEditor() {
            this.editor = '';
            this.draftEntry = null;
            this.draftLabel = '';
            this.protect = false;
            const dialog = (this as unknown as { $refs?: { entryDialog?: HTMLDialogElement } })
                .$refs?.entryDialog;
            this.clearDraftValue();
            dialog?.close();
        },
        async saveEntry(event: SubmitEvent) {
            const form = event.currentTarget as HTMLFormElement;
            if (
                !this.unlocked ||
                this.working ||
                this.locationRequest ||
                this.protectionBlocked ||
                this.protectionUnchanged ||
                !form.reportValidity()
            )
                return;
            const data = new FormData(form);
            const entry = this.draftEntry;
            const mode = this.editor;
            let value: JsonValue = null;
            const label = this.draftLabel.trim();
            const protectedValue = this.protect;
            const passwordField = form.querySelector<HTMLInputElement>(
                '[name="protectionPassword"]',
            );
            let password = this.protectionNeedsPassword ? (passwordField?.value ?? null) : null;
            if (passwordField) passwordField.value = '';
            try {
                if (this.needsLabel && !label) throw Error('Enter a name.');
                if (this.needsValue) {
                    const raw = (data.get('entryValue') ?? '').toString();
                    value =
                        this.draftKind === 'location'
                            ? parseVaultInput(
                                  'location',
                                  JSON.stringify({
                                      name: (data.get('locationName') ?? '').toString().trim(),
                                      latitude: Number(data.get('latitude')),
                                      longitude: Number(data.get('longitude')),
                                      ...(this.locationFix &&
                                      this.locationFix.latitude === Number(data.get('latitude')) &&
                                      this.locationFix.longitude === Number(data.get('longitude'))
                                          ? {
                                                source: 'os',
                                                accuracy_m: this.locationFix.accuracy_m,
                                            }
                                          : {}),
                                  }),
                              )
                            : parseVaultInput(this.draftKind, raw);
                }
            } catch (error) {
                this.error = (error as Error).message;
                return;
            }
            if (this.needsValue && this.maskValue) {
                const input = form.querySelector<HTMLInputElement>('input[name="entryValue"]');
                if (input) input.value = '';
            }
            const saved = await this.run(
                async (owner, revision) => {
                    if (mode === 'create')
                        await owner.create({
                            label,
                            kind: this.draftKind,
                            protected: protectedValue,
                            value,
                        });
                    else if (entry && mode === 'replace')
                        await owner.replace(entry.id, entry.revision, value);
                    else if (entry && mode === 'rename')
                        await owner.rename(entry.id, entry.revision, label);
                    else if (entry && mode === 'protection' && owner.setProtection) {
                        const pending = owner.setProtection(
                            entry.id,
                            entry.revision,
                            protectedValue,
                            password,
                        );
                        password = null;
                        await pending;
                    } else if (entry && mode === 'delete')
                        await owner.delete(entry.id, entry.revision);
                    await this.load(owner, revision, await owner.status());
                },
                mode === 'protection'
                    ? (error) => {
                          const code = error instanceof Error ? error.message : String(error);
                          if (code === 'incorrect_password')
                              return 'That master password is incorrect.';
                          if (code === 'rate_limited')
                              return 'Too many attempts. Wait and try again.';
                          if (code === 'stale_entry_revision')
                              return 'This entry changed. Close this window and refresh Vault.';
                          return 'Vault could not change protection. Refresh and try again.';
                      }
                    : undefined,
            );
            if (saved) this.closeEditor();
        },
    };
}

export function register() {
    registerVaultTransfer(native ? desktopVaultTransferService() : null);
    const service = native ? desktopService() : null;
    if (service) {
        const invalidate = () => {
            const cache = metadataByService.get(service);
            if (cache) clearMetadata(cache);
        };
        window.addEventListener('vault-session-ended', invalidate);
        window.addEventListener('pagehide', invalidate);
    }
    Alpine.data('vaultPage', () => {
        const [path, query] = location.hash.split('?');
        const params = new URLSearchParams(query);
        const linkedPlugin =
            path === '#/vault' && params.get('tab') === 'plugins'
                ? (params.get('plugin') ?? '')
                : null;
        return vaultController(
            service,
            (id) => app().snapshot.plugins.find((plugin) => plugin.id === id)?.name ?? id,
            () => app().snapshot,
            linkedPlugin,
        );
    });
}
