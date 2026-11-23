import { listen } from '@tauri-apps/api/event';
import type {
    InstalledVaultReview,
    PermissionScope,
    PluginInfo,
    VaultPermissionActivity,
} from '../generated/runtime';
import { bridge, native } from './bridge';
import { adjacentPage, paginate } from './pagination';

const permissionPluginPageSize = 6;

export function openPluginPermissions(pluginId = '') {
    location.hash =
        '#/settings?tab=permissions' + (pluginId ? '&plugin=' + encodeURIComponent(pluginId) : '');
}

export { permissionLabel, scopeDetails } from './permission-labels';
import { permissionLabel, scopeDetails } from './permission-labels';

function directReadStatus(state: string, activity?: VaultPermissionActivity) {
    switch (state) {
        case 'unlocked':
            if (!activity) return 'Needs approval';
            return activity.revoked ? 'Revoked' : 'Granted';
        case 'locked':
            return 'Vault locked';
        case 'uninitialized':
            return 'Needs setup';
        default:
            return 'Unavailable';
    }
}

interface InstalledServices {
    installation: string;
    inventory: string;
    revision: string;
    scopes: {
        action: string;
        scope: PermissionScope;
        available: boolean;
        granted: boolean;
        decision?: string;
        active?: boolean;
    }[];
}

function scopeId(scope: PermissionScope) {
    return JSON.stringify(scope, Object.keys(scope).sort());
}

function serviceStatus(service?: InstalledServices['scopes'][number]) {
    if (!service?.available) return 'Unavailable';
    if (service.decision === 'pending') return 'Pending review';
    if (service.active) return 'In use';
    if (service.granted) return 'Granted';
    if (service.decision === 'denied') return 'Denied';
    if (service.decision === 'revoked') return 'Revoked';
    return 'Needs approval';
}

// Read required declarations once when preparing card badge requests
export function requiredPluginPermissionSources(plugin: PluginInfo) {
    const required = Object.values(plugin.permissions?.actions ?? {}).flatMap(
        (declaration) => declaration?.required ?? [],
    );
    return {
        service: required.some((scope) => !isDirectVaultPermission(scope)),
        vault: required.some(
            (scope) => isDirectVaultPermission(scope) || scope.id === 'vault.access',
        ),
    };
}

export function hasPluginPermissions(plugin: PluginInfo) {
    return Object.values(plugin.permissions?.actions ?? {}).some(
        (action) => !!(action?.required.length || action?.optional.length),
    );
}

function isDirectVaultPermission(scope: PermissionScope) {
    return (
        scope.id === 'vault.read-secret' ||
        scope.id === 'vault.read-location' ||
        scope.id === 'vault.write-secret'
    );
}

export function asInstalledServices(value: unknown): InstalledServices | undefined {
    if (value && typeof value === 'object' && 'scopes' in value && Array.isArray(value.scopes))
        return value as InstalledServices;
}

// Count known, actionable required gaps by exact permission scope rather than action usage
export function requiredPluginPermissionCount(
    plugin: PluginInfo,
    status?: InstalledServices,
    vault?: { state: string; activity: VaultPermissionActivity[] },
) {
    const services = new Map(
        status?.scopes.map((row) => [JSON.stringify([row.action, scopeId(row.scope)]), row]),
    );
    const missing = new Set<string>();
    for (const [action, declaration] of Object.entries(plugin.permissions?.actions ?? {})) {
        for (const scope of declaration?.required ?? []) {
            const id = scopeId(scope);
            if (
                scope.id === 'vault.read-secret' ||
                scope.id === 'vault.read-location' ||
                scope.id === 'vault.write-secret'
            ) {
                if (
                    vault?.state === 'uninitialized' ||
                    (vault?.state === 'unlocked' &&
                        !vault.activity.some(
                            (item) =>
                                item.plugin_id === plugin.id &&
                                item.action === action &&
                                (item.family ?? 'vault.read-secret') === scope.id &&
                                item.field === scope.field &&
                                item.entry_kind === scope.entry_kind &&
                                item.purpose === scope.purpose &&
                                !item.revoked,
                        ))
                )
                    missing.add(id);
            } else {
                const service = services.get(JSON.stringify([action, id]));
                if (
                    service?.available &&
                    !service.granted &&
                    (scope.id !== 'vault.access' || vault?.state === 'unlocked')
                )
                    missing.add(id);
            }
        }
    }
    return missing.size;
}

function permissionRows(
    plugin: PluginInfo,
    vaultState: string,
    activity: VaultPermissionActivity[],
    services?: InstalledServices,
) {
    const rows = [];
    for (const [action, declaration] of Object.entries(plugin.permissions?.actions ?? {})) {
        if (!declaration) continue;
        for (const kind of ['required', 'optional'] as const) {
            for (const scope of declaration[kind]) {
                const direct =
                    native &&
                    (scope.id === 'vault.read-secret' || scope.id === 'vault.read-location');
                const writer = native && scope.id === 'vault.write-secret';
                const usage =
                    direct || writer
                        ? activity.find(
                              (item) =>
                                  item.plugin_id === plugin.id &&
                                  (item.family ?? 'vault.read-secret') === scope.id &&
                                  item.action === action &&
                                  item.field === scope.field &&
                                  item.entry_kind === scope.entry_kind &&
                                  item.purpose === scope.purpose,
                          )
                        : undefined;
                const service = services?.scopes.find(
                    (item) => item.action === action && scopeId(item.scope) === scopeId(scope),
                );
                const status =
                    direct || writer ? directReadStatus(vaultState, usage) : serviceStatus(service);
                rows.push({
                    key: JSON.stringify([action, scopeId(scope), kind]),
                    action,
                    scope,
                    canChange:
                        !direct &&
                        !writer &&
                        !!service &&
                        (service.granted ||
                            (service.available &&
                                (scope.id !== 'vault.access' || vaultState === 'unlocked'))),
                    serviceGranted: !!service?.granted,
                    required: kind === 'required',
                    family: scope.id,
                    label: permissionLabel(scope),
                    field: 'field' in scope ? scope.field : '',
                    canReview: direct && vaultState === 'unlocked',
                    details: scopeDetails(scope),
                    purpose: 'purpose' in scope ? scope.purpose : declaration.purpose,
                    status,
                    canRevoke: (direct || writer) && ['Granted', 'Vault locked'].includes(status),
                    lastWrite: usage?.last_write
                        ? new Date(Number(usage.last_write) * 1000).toLocaleString()
                        : '',
                    lastRead: usage?.last_read
                        ? new Date(Number(usage.last_read) * 1000).toLocaleString()
                        : '',
                });
            }
        }
    }
    return rows;
}

// Only brokered permissions that are currently controllable participate in bulk toggles
function permissionToggle(rows: ReturnType<typeof permissionRows>) {
    const changeable = rows.filter((row) => row.canChange);
    return {
        changeable: changeable.length,
        allowed: changeable.filter((row) => row.serviceGranted).length,
    };
}

// Group only identical scopes; each action keeps its own permission decision
function groupPermissionRows(rows: ReturnType<typeof permissionRows>) {
    const groups = new Map<
        string,
        { key: string; label: string; details: string; reasons: typeof rows }
    >();
    for (const row of rows) {
        const key = scopeId(row.scope);
        const group = groups.get(key);
        if (group) group.reasons.push(row);
        else groups.set(key, { key, label: row.label, details: row.details, reasons: [row] });
    }
    return Array.from(groups.values(), ({ reasons, ...group }) => {
        const purposes = new Map<string, typeof rows>();
        for (const row of reasons) {
            const purpose = row.purpose?.trim() || '';
            const actions = purposes.get(purpose);
            if (actions) actions.push(row);
            else purposes.set(purpose, [row]);
        }
        return {
            ...group,
            purposes: Array.from(purposes, ([label, actions]) => ({
                label,
                actions,
                ...permissionToggle(actions),
            })),
        };
    });
}

export function permissionsController(installed: () => PluginInfo[]) {
    return {
        plugin: '',
        enabled: '',
        family: '',
        outcome: '',
        currentPage: 1,
        activity: [] as VaultPermissionActivity[],
        vaultState: 'unavailable',
        services: {} as Record<string, InstalledServices>,
        working: false,
        review: null as InstalledVaultReview | null,
        get reviewTitle() {
            return this.review?.slot.kind === 'location'
                ? 'Review location access'
                : 'Review direct secret access';
        },
        reviewPlugin: null as PluginInfo | null,
        entryId: '',
        optIn: false,
        dialog: null as HTMLDialogElement | null,
        message: '',
        generation: 0,
        stopStatus: null as null | (() => void),
        stopRouteFilter: null as null | (() => void),
        disposed: false,
        statusReads: {} as Record<string, number>,
        get plugins() {
            return installed();
        },
        get families() {
            return [
                ...new Set(
                    this.plugins.flatMap((p) =>
                        Object.values(p.permissions?.actions ?? {}).flatMap((a) =>
                            [...(a?.required ?? []), ...(a?.optional ?? [])].map((s) => s.id),
                        ),
                    ),
                ),
            ].sort();
        },
        get groups() {
            return this.plugins
                .filter(
                    (p) =>
                        (!this.plugin || p.id === this.plugin) &&
                        (!this.enabled || (p.enabled ? 'enabled' : 'disabled') === this.enabled),
                )
                .map((p) => {
                    const rows = permissionRows(
                        p,
                        this.vaultState,
                        this.activity,
                        this.services[p.id],
                    );
                    const permissions = groupPermissionRows(
                        rows.filter((row) => !this.family || row.family === this.family),
                    ).filter(
                        (permission) =>
                            !this.outcome ||
                            permission.purposes.some((purpose) =>
                                purpose.actions.some((row) => row.status === this.outcome),
                            ),
                    );
                    return {
                        id: p.id,
                        name: p.name,
                        enabled: p.enabled,
                        inventory: rows.some((row) => row.canChange)
                            ? this.services[p.id]?.inventory
                            : '',
                        permissions,
                        ...permissionToggle(
                            permissions.flatMap((permission) =>
                                permission.purposes.flatMap((purpose) => purpose.actions),
                            ),
                        ),
                    };
                })
                .filter((p) => p.permissions.length || (!this.family && !this.outcome));
        },
        get groupPage() {
            return paginate(this.groups, this.currentPage, permissionPluginPageSize);
        },
        resetPage() {
            this.currentPage = 1;
        },
        changePage(event: Event) {
            const direction = Number((event.currentTarget as HTMLElement).dataset.direction);
            if (direction === -1 || direction === 1) {
                this.currentPage = adjacentPage(this.groupPage, direction);
            }
        },
        init() {
            if (native)
                void listen<string>('plugin-permission-status', ({ payload }) => {
                    if (!this.disposed) void this.refreshService(payload);
                })
                    .then((stop) => {
                        if (this.disposed) stop();
                        else this.stopStatus = stop;
                    })
                    .catch(() => {});
            const syncRouteFilter = () => {
                const [path, query] = location.hash.split('?');
                const params = new URLSearchParams(query);
                if (path !== '#/settings' || params.get('tab') !== 'permissions') return;
                this.plugin = params.get('plugin') ?? '';
                this.resetPage();
                if (this.plugin)
                    requestAnimationFrame(() => {
                        if (!this.disposed)
                            document.getElementById('settings-plugin-permissions')?.focus({
                                preventScroll: true,
                            });
                    });
            };
            syncRouteFilter();
            window.addEventListener('hashchange', syncRouteFilter);
            this.stopRouteFilter = () => window.removeEventListener('hashchange', syncRouteFilter);
            void this.refresh();
        },
        destroy() {
            this.disposed = true;
            this.stopStatus?.();
            this.stopRouteFilter?.();
            ++this.generation;
            this.activity = [];
            this.cancel();
        },
        async begin(event: Event) {
            if (this.working) return;
            const target = event.currentTarget as HTMLElement;
            const { pluginId, action, field } = target.dataset;
            const plugin = this.plugins.find((p) => p.id === pluginId);
            if (!plugin || !action || !field) return;
            const generation = this.generation;
            this.working = true;
            this.message = '';
            try {
                const review = await bridge.command('vault_installed_review', {
                    pluginId: plugin.id,
                    action,
                    field,
                });
                if (generation !== this.generation) {
                    await bridge.command('vault_cancel_installed_review', { id: review.id });
                    return;
                }
                this.dialog = target.closest('article')!.querySelector('dialog');
                this.entryId = '';
                this.optIn = false;
                this.reviewPlugin = plugin;
                this.review = review;
                this.dialog?.showModal();
            } catch {
                if (generation === this.generation)
                    this.message =
                        'Unlock Vault and verify the installed package through install review before approving access.';
            } finally {
                if (generation === this.generation) this.working = false;
            }
        },
        cancel() {
            const review = this.review;
            this.review = null;
            this.reviewPlugin = null;
            this.optIn = false;
            this.entryId = '';
            this.dialog?.close();
            this.dialog = null;
            if (review)
                void bridge
                    .command('vault_cancel_installed_review', { id: review.id })
                    .catch(() => {});
        },
        async allow() {
            const review = this.review;
            const plugin = this.reviewPlugin;
            const entry = review?.entries.find((e) => e.id === this.entryId);
            if (!review || !plugin || !entry || !this.optIn || this.working) return;
            this.working = true;
            try {
                await bridge.command('vault_allow_installed_read', {
                    id: review.id,
                    pluginId: plugin.id,
                    field: review.slot.field,
                    action: review.slot.action,
                    entryId: entry.id,
                    revision: entry.revision,
                    packageOptIn: this.optIn,
                });
                this.message =
                    'Entry access approved. The plugin can use it in a Vault-enabled session.';
            } catch {
                this.message =
                    'Approval expired or the installation changed. Review this action again.';
            } finally {
                this.review = null;
                this.cancel();
                await this.refresh();
            }
        },
        // Reflect the current decisions, including partially approved groups
        syncServiceReason(input: HTMLInputElement, total: number, allowed: number) {
            input.checked = total > 0 && allowed === total;
            input.indeterminate = allowed > 0 && allowed < total;
        },
        async changeService(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const { pluginId, key, purpose, all, grant } = input.dataset;
            const allow = input.checked;
            const group = this.groupPage.items.find((group) => group.id === pluginId);
            const services = pluginId && this.services[pluginId];
            if (!group || !services) return;
            const rows = group.permissions
                .filter((permission) => all === 'true' || permission.key === key)
                .flatMap((permission) =>
                    permission.purposes
                        .filter((reason) => all === 'true' || reason.label === purpose)
                        .flatMap((reason) => reason.actions),
                )
                .filter((row) => grant === undefined || row.key === grant);
            const { changeable, allowed } = permissionToggle(rows);
            this.syncServiceReason(input, changeable, allowed);
            if (this.working) return;
            const grants = rows
                .filter((row) => row.canChange && row.serviceGranted !== allow)
                .map(({ action, scope, required }) => ({ action, scope, required }));
            if (!grants.length) return;
            const generation = this.generation;
            const doc = input.ownerDocument;
            const hadFocus = doc?.activeElement === input;
            const summary = hadFocus ? input.closest('details')?.querySelector('summary') : null;
            this.working = true;
            try {
                await bridge.command('set_service_permission', {
                    pluginId: group.id,
                    installation: services.installation,
                    inventory: services.inventory,
                    revision: services.revision,
                    grants,
                    allow,
                });
                if (generation !== this.generation) return;
                this.message = allow ? 'Access approved.' : 'Access revoked.';
            } catch {
                if (generation !== this.generation) return;
                this.message =
                    'The installation or decision changed. Review the current permission.';
            } finally {
                if (generation === this.generation) {
                    await this.refreshService(group.id);
                    if (generation === this.generation) {
                        this.working = false;
                        // Native disabling may move keyboard focus while the change is saved
                        if (hadFocus)
                            requestAnimationFrame(() => {
                                if (
                                    generation !== this.generation ||
                                    doc.activeElement !== doc.body
                                )
                                    return;
                                const target =
                                    input.isConnected && input.getClientRects().length
                                        ? input
                                        : summary;
                                target?.focus({ preventScroll: true });
                            });
                    }
                }
            }
        },
        async refreshService(pluginId: string) {
            if (!this.plugins.some((p) => p.id === pluginId && p.permissions)) return;
            const lifecycle = this.generation;
            const read = (this.statusReads[pluginId] ?? 0) + 1;
            this.statusReads[pluginId] = read;
            const status = await bridge
                .command('installed_service_permissions', { pluginId })
                .catch(() => null);
            if (
                this.disposed ||
                lifecycle !== this.generation ||
                read !== this.statusReads[pluginId]
            )
                return;
            const services = asInstalledServices(status);
            if (services) this.services[pluginId] = services;
            else delete this.services[pluginId];
        },
        async refresh() {
            const generation = ++this.generation;
            this.cancel();
            this.activity = [];
            this.vaultState = 'unavailable';
            if (!native) return;
            this.working = true;
            try {
                this.services = {};
                for (const plugin of this.plugins.filter((p) => p.permissions)) {
                    if (generation !== this.generation) return;
                    await this.refreshService(plugin.id);
                }
                if (generation !== this.generation) return;
                const status = await bridge.command('vault_status');
                const activity =
                    status.state === 'unlocked'
                        ? await bridge.command('vault_permission_activity')
                        : [];
                if (generation === this.generation) {
                    this.vaultState = status.state;
                    this.activity = activity;
                }
            } catch {
                if (generation === this.generation)
                    this.message = 'Permission status is unavailable.';
            } finally {
                if (generation === this.generation) this.working = false;
            }
        },
        async revoke(event: Event) {
            const { pluginId, action, family } = (event.currentTarget as HTMLElement).dataset;
            if (this.working || !pluginId || !action) return;
            const generation = this.generation;
            this.working = true;
            try {
                const writes = family === 'vault.write-secret';
                await bridge.command(
                    writes ? 'vault_revoke_private_writes' : 'vault_revoke_direct_reads',
                    { pluginId, action },
                );
                if (generation !== this.generation) return;
                this.message = writes
                    ? 'Private secret writes revoked for this action.'
                    : 'Direct Vault reads revoked for this action.';
                await this.refresh();
            } catch {
                if (generation === this.generation)
                    this.message = 'Manager could not revoke this access.';
            } finally {
                if (generation === this.generation) this.working = false;
            }
        },
    };
}
