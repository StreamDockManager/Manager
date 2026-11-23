import { pluginFailureSummary } from './plugin-failure-summary';
import { pluginAuthorLabel } from './plugin-author';
import { pluginSdkLabel } from './plugin-sdk-label';
import { loadPluginAction, pluginRuntimeLabel, togglePlugin } from './plugin-choice-behavior';
import { pluginChoiceOrganization } from './plugin-choice-organization';
import { pluginChoiceGroups } from './plugin-choice-groups';
import { nextTab } from './tab-navigation';
import { registerInstalledPackageExport } from './installed-package-export';
import { openPluginManagement } from './plugin-management';
import {
    asInstalledServices,
    hasPluginPermissions,
    openPluginPermissions,
    requiredPluginPermissionCount,
    requiredPluginPermissionSources,
} from './plugin-permissions';
import { resourceAlertDraft } from './resource-alerts';
import { getLibraryOrganization } from './library-organization';
import Alpine from '@alpinejs/csp';
import { listen } from '@tauri-apps/api/event';
import type { PluginRuntimeSnapshot, RecoveryStatus } from '../generated/runtime';
import {
    emptyPluginRuntime,
    formatBytes,
    formatUptime,
    indexPluginRuntime,
    pluginNetworkSummary,
    pluginResourceTrend,
    pluginWakeLabel,
} from './plugin-runtime';
import { native } from './bridge';
import { reconcile } from './reconcile';
import { editPluginSettings } from './plugin-settings';
import type { Plugin } from './types';
import { defaultPluginBackground } from './background-color';

import { paginate, adjacentPage, changeGroupPage } from './pagination';
import { controlGroups, installPluginPackage } from './plugin-actions';

import { app, swallow } from './route-context';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { ownWindowMethods } from './window-work';

// Find the menu trigger associated with the selected plugin options control
function pluginOptionsTrigger(control: Element) {
    return (
        control
            .closest('.plugin-options')
            ?.querySelector<HTMLButtonElement>('.plugin-menu-trigger') || undefined
    );
}

// Register the plugins handler with its owner
export function register() {
    const choices = pluginChoiceOrganization(
        () => getLibraryOrganization(Alpine),
        () => app().snapshot.plugins,
    );
    registerInstalledPackageExport();
    Alpine.data('plugins', () => ({
        // Export the installed
        exportInstalled(event: Event) {
            const trigger = event.currentTarget as HTMLElement;
            const id = trigger.dataset.id;
            if (!id) return;
            const opener = pluginOptionsTrigger(trigger);
            window.dispatchEvent(
                new CustomEvent('open-installed-package-export', {
                    detail: { id, label: trigger.dataset.name || id, opener },
                }),
            );
        },
        // Open version management for the plugin identified by the invoking control
        manageVersions(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (id) openPluginManagement(id);
        },
        // Open the Vault panel for the plugin without granting or revealing access
        openPluginVault(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (id) location.hash = '#/vault?tab=plugins&plugin=' + encodeURIComponent(id);
        },
        hasPluginPermissions,
        permissionReviewCounts: {} as Record<string, number>,
        permissionBadgeGeneration: 0,
        permissionBadgeReads: {} as Record<string, number>,
        permissionBadgeSubscription: null as Promise<() => void> | null,
        get permissionBadgeScope() {
            return JSON.stringify(
                this.pluginPage.items.map(({ id, version, permissions }) => [
                    id,
                    version,
                    permissions,
                ]),
            );
        },
        // Open the permissions view filtered to this plugin
        openCardPermissions(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (id) openPluginPermissions(id);
        },
        cardPermissionLabel(plugin: Plugin) {
            const count = this.permissionReviewCounts[plugin.id] ?? 0;
            return (
                'Permissions for ' +
                plugin.name +
                (count ? ' (' + count + ' required to review)' : '')
            );
        },
        // Query authoritative status only for visible cards that have required declarations
        async refreshPermissionBadges(onlyPlugin?: string) {
            if (!native || this.disposed) return;
            if (!onlyPlugin) {
                ++this.permissionBadgeGeneration;
                this.permissionBadgeReads = {};
                this.permissionReviewCounts = {};
            }
            const generation = this.permissionBadgeGeneration;
            const targets = this.pluginPage.items
                .filter((plugin) => !onlyPlugin || plugin.id === onlyPlugin)
                .map((plugin) => ({ plugin, ...requiredPluginPermissionSources(plugin) }))
                .filter(({ service, vault }) => service || vault)
                .map((target) => {
                    const id = target.plugin.id;
                    const read = (this.permissionBadgeReads[id] ?? 0) + 1;
                    this.permissionBadgeReads[id] = read;
                    return { ...target, read };
                });
            const backend = app().bridge;
            const vaultState = targets.some((target) => target.vault)
                ? backend
                      .command('vault_status')
                      .then(async ({ state }) => ({
                          state,
                          activity:
                              state === 'unlocked'
                                  ? await backend.command('vault_permission_activity')
                                  : [],
                      }))
                      .catch(() => undefined)
                : undefined;
            for (const { plugin, service, vault, read } of targets) {
                if (this.disposed || generation !== this.permissionBadgeGeneration) return;
                if (read !== this.permissionBadgeReads[plugin.id]) continue;
                const status = service
                    ? await backend
                          .command('installed_service_permissions', {
                              pluginId: plugin.id,
                          })
                          .catch(() => null)
                    : undefined;
                const count = requiredPluginPermissionCount(
                    plugin,
                    asInstalledServices(status),
                    vault ? await vaultState : undefined,
                );
                if (
                    !this.disposed &&
                    generation === this.permissionBadgeGeneration &&
                    read === this.permissionBadgeReads[plugin.id]
                )
                    this.permissionReviewCounts[plugin.id] = count;
            }
        },
        watchPermissionBadges() {
            if (!native || this.permissionBadgeSubscription) return;
            this.permissionBadgeSubscription = listen<string>(
                'plugin-permission-status',
                ({ payload }) => {
                    void this.refreshPermissionBadges(payload);
                },
            );
            void this.permissionBadgeSubscription.catch(() => {});
            void this.refreshPermissionBadges();
        },
        favoritesOnly: false,
        // Derive organization from the current plugins state
        get organization() {
            return getLibraryOrganization(Alpine);
        },
        choiceFavorite: choices.choiceFavorite,
        choiceTags: choices.choiceTags,
        organizeChoice: choices.organizeChoice,
        live: {} as Record<string, PluginRuntimeSnapshot>,
        liveAt: 0,
        clock: performance.now(),
        uptimeTimer: 0,
        disposed: false,
        monitorId: '',
        monitorVisible: null as boolean | null,
        unsubscribeRuntime: null as (() => void) | null,
        runtimeRegistration: null as Promise<void> | null,
        resumeListener: null as (() => void) | null,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        visibilityListener: null as (() => void) | null,
        // Derive monitoring visible from the current plugins state
        get monitoringVisible(): boolean {
            if (this.disposed || document.hidden || app().route !== 'plugins') return false;
            const plugins = this.healthId ? this.healthPlugins : this.pluginPage.items;
            return plugins.some(
                (plugin) => plugin.enabled && this.runtimeFor(plugin).state === 'running',
            );
        },
        // Use live plugin data only while its enabled state matches the current snapshot
        liveFor(plugin: Plugin) {
            const live = this.live[plugin.id];
            return live && live.enabled === plugin.enabled ? live : undefined;
        },
        // Prefer matching live runtime data and otherwise fall back to the saved snapshot
        runtimeFor(plugin: Plugin) {
            return this.liveFor(plugin)?.runtime || plugin.runtime || emptyPluginRuntime();
        },
        // Prefer matching live health data over the plugin snapshot
        healthFor(plugin: Plugin) {
            return this.liveFor(plugin)?.health || plugin.health;
        },
        // Describe disabled, recovery-paused, sleeping and active plugin runtime states
        runtimeStatus(plugin: Plugin) {
            if (!plugin.enabled) return 'Disabled';
            if ((Alpine.store('recovery') as { status: RecoveryStatus }).status.active)
                return 'Paused · recovery';
            const { state, reason } = this.runtimeFor(plugin);
            if (reason === 'vault_setup_required') return 'Paused · Vault setup required';
            if (reason === 'vault_locked') return 'Paused · Vault locked';
            if (state === 'idle' && this.runtimeFor(plugin).reason === 'no_visible_actions')
                return 'Sleeping';
            return state[0].toUpperCase() + state.slice(1);
        },
        // Explain the runtime state without presenting recovery-paused plugins as running
        runtimeDescription(plugin: Plugin) {
            if (
                plugin.enabled &&
                (Alpine.store('recovery') as { status: RecoveryStatus }).status.active
            )
                return 'Enabled for the next normal launch';
            const runtime = this.runtimeFor(plugin);
            if (runtime.reason === 'vault_setup_required')
                return 'Review the required entries in Plugin Permissions';
            if (runtime.reason === 'vault_locked') return 'Unlock Vault to start this plugin';
            if (
                plugin.enabled &&
                runtime.state === 'idle' &&
                runtime.reason === 'no_visible_actions'
            )
                return 'Sleeping — no visible actions';
            if (plugin.enabled && runtime.state === 'idle')
                return runtime.reason === 'no_active_device'
                    ? 'Idle — not used by an active device profile (no active device)'
                    : 'Idle — not used by an active device profile';
            if (runtime.reason === 'required_by_active_profile')
                return 'Required by an active device or simulator profile';
            if (runtime.reason === 'configuration')
                return runtime.state === 'running'
                    ? 'Running for configuration'
                    : 'Starting for configuration';
            if (runtime.reason === 'manual_action')
                return runtime.state === 'running'
                    ? 'Running for a manual macro'
                    : 'Queued for a manual macro';
            return this.liveFor(plugin)?.status || plugin.status;
        },
        // Suppress runtime detail that merely repeats the status label
        runtimeDetail(plugin: Plugin) {
            const description = this.runtimeDescription(plugin);
            return description.trim().toLowerCase() === this.runtimeStatus(plugin).toLowerCase()
                ? ''
                : description;
        },
        // Format CPU usage while preserving not-running, warmup and stale states
        cpuLabel(plugin: Plugin) {
            const resources = this.runtimeFor(plugin).resources;
            if (resources.state === 'not_running') return 'CPU: not running';
            if (resources.state === 'warming_up') return 'CPU: warming up';
            if (resources.state === 'stale') return 'CPU: stale';
            return resources.cpu_percent == null
                ? 'CPU: unavailable'
                : `CPU: ${resources.cpu_percent.toFixed(1)}%`;
        },
        // Format resident memory without presenting stale or stopped samples as current
        memoryLabel(plugin: Plugin) {
            const resources = this.runtimeFor(plugin).resources;
            if (resources.state === 'not_running') return 'Memory: not running';
            if (resources.state === 'stale') return 'Memory: stale';
            return 'Memory: ' + formatBytes(resources.resident_bytes);
        },
        // Advance running-process uptime from the last live sample using the route clock
        uptimeLabel(plugin: Plugin) {
            const runtime = this.runtimeFor(plugin);
            if (runtime.uptime_ms == null) return 'Not running';
            const elapsed =
                this.liveFor(plugin) && ['running', 'starting'].includes(runtime.state)
                    ? Math.max(0, this.clock - this.liveAt)
                    : 0;
            return 'Uptime: ' + formatUptime(runtime.uptime_ms + elapsed);
        },
        // Summarize the plugin's accumulated crash and issue counts
        healthLabel(plugin: Plugin) {
            const health = this.healthFor(plugin);
            return `${health?.totals?.crashes || 0} crashes · ${health?.totals?.issues || 0} issues`;
        },
        // Refresh the failure summary's relative age as the route clock advances
        failureSummary(plugin: Plugin) {
            void this.clock; // Refresh the wall-clock age when the route's ticker advances.
            return pluginFailureSummary(this.healthFor(plugin));
        },
        // Identify repeated failures from the runtime reason or restart-attempt count
        repeatedFailure(plugin: Plugin) {
            const health = this.healthFor(plugin);
            return (
                this.runtimeFor(plugin).reason === 'repeated_failures' ||
                health.restart_attempts >= 2
            );
        },
        // Summarize the plugin runtime's network activity
        networkSummary(plugin: Plugin) {
            return pluginNetworkSummary(this.runtimeFor(plugin).network);
        },
        // Pair session and lifetime health counts with their display labels
        healthCounts(plugin: Plugin) {
            const health = this.healthFor(plugin);
            return [
                ['crashes', 'Crashes'],
                ['issues', 'Issues'],
                ['failed_starts', 'Failed starts'],
                ['automatic_restarts', 'Automatic restarts'],
            ].map(([key, label]) => ({
                label,
                session: health.session[key as keyof typeof health.session],
                total: health.totals[key as keyof typeof health.totals],
            }));
        },
        // Build labeled process and peak-resource details from runtime health data
        healthProcessDetails(plugin: Plugin) {
            const health = this.healthFor(plugin);
            const resources = this.runtimeFor(plugin).resources;
            return [
                { label: 'Process ID', value: resources.root_pid ?? 'Unavailable' },
                { label: 'Processes', value: resources.process_count ?? 'Unavailable' },
                { label: 'Peak memory', value: formatBytes(resources.peak_resident_bytes || null) },
                { label: 'Last-run peak', value: formatBytes(health.last_run_peak_resident_bytes) },
                { label: 'Restart attempts', value: health.restart_attempts },
                { label: 'Last exit', value: health.last_exit || 'None recorded' },
                {
                    label: 'Last registration',
                    value: health.last_registration_ms
                        ? new Date(health.last_registration_ms).toLocaleString()
                        : 'None recorded',
                },
            ];
        },
        // Sort health events newest first and add localized and machine-readable timestamps
        healthEvents(plugin: Plugin) {
            return [...this.healthFor(plugin).events]
                .sort((a, b) => b.timestamp_ms - a.timestamp_ms)
                .map((event) => ({
                    ...event,
                    time: new Date(event.timestamp_ms).toLocaleString(),
                    iso: new Date(event.timestamp_ms).toISOString(),
                }));
        },
        // Show only nonempty missing-asset and unsupported-API issue groups
        compatibilityIssues(plugin: Plugin) {
            const health = this.healthFor(plugin);
            return [
                { label: 'Missing assets', messages: health.missing_assets },
                { label: 'Unsupported APIs', messages: health.unsupported_apis },
            ].filter((group) => group.messages.length);
        },
        // Check whether retry is valid for the current plugins state
        canRetry(plugin: Plugin) {
            const r = this.runtimeFor(plugin);
            return plugin.enabled && r.state === 'blocked' && r.demand.length > 0;
        },
        // Check whether restart is valid for the current plugins state
        canRestart(plugin: Plugin) {
            const r = this.runtimeFor(plugin);
            return plugin.enabled && r.state === 'running' && r.demand.length > 0;
        },
        // Check whether show settings interface is valid for the current plugins state
        canShowSettingsInterface(plugin: Plugin) {
            return (
                !!plugin.has_settings_interface &&
                plugin.enabled &&
                this.runtimeFor(plugin).state === 'running' &&
                !(Alpine.store('recovery') as { status: RecoveryStatus }).status.active
            );
        },
        // Show the settings interface
        async showSettingsInterface(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            const plugin = app().snapshot.plugins.find((plugin) => plugin.id === id);
            if (!plugin || !this.canShowSettingsInterface(plugin)) return;
            try {
                await app().command('plugin_show_settings_interface', { id: plugin.id });
            } catch {
                // The app command reports native connection or plugin errors
            }
        },
        // Request a retry or restart only for a valid plugin and supported recovery operation
        async recover(event: Event) {
            const data = (event.currentTarget as HTMLElement).dataset;
            if (!data.id || !['retry', 'restart'].includes(data.operation || '')) return;
            try {
                app().apply(
                    await app().command(
                        data.operation === 'retry' ? 'plugin_retry' : 'plugin_restart',
                        { id: data.id },
                    ),
                );
            } catch {}
        },
        // Convert sampled resource history into points for the selected trend metric
        trendPoints(plugin: Plugin, metric: string) {
            return pluginResourceTrend(this.runtimeFor(plugin).resources.history, metric);
        },
        // Format the current process run's peak CPU and resident-memory measurements
        resourcePeaks(plugin: Plugin) {
            const resources = this.runtimeFor(plugin).resources;
            const cpu =
                resources.peak_cpu_percent == null
                    ? 'Unavailable'
                    : `${resources.peak_cpu_percent.toFixed(1)}%`;
            return `Current-run peaks · CPU ${cpu} · Memory ${formatBytes(resources.peak_resident_bytes || null)}`;
        },
        // Explain how the plugin's hidden-sleep setting affects runtime eligibility
        sleepPolicy(plugin: Plugin) {
            return plugin.sleep_when_hidden
                ? 'Sleep when hidden is on. Visible profile actions, manual actions, and open configuration can keep this plugin running.'
                : 'Sleep when hidden is off. An active profile can keep this plugin running even when its actions are hidden.';
        },
        // Describe the last recorded plugin wake event
        lastWake(plugin: Plugin) {
            return pluginWakeLabel(this.runtimeFor(plugin));
        },
        // Close the utilities
        closeUtilities(event: Event) {
            const menu = this.$refs.utilityMenu as HTMLDetailsElement | undefined;
            if (!menu?.open) return;
            menu.open = false;
            if (event.type === 'keydown') menu.querySelector('summary')?.focus();
        },
        pluginMenu: '',
        pluginMenuFocus: null as HTMLElement | null,
        // Remember focus only when it belongs to a plugin menu inside this view
        rememberPluginFocus(event: FocusEvent) {
            const target = event.target;
            this.pluginMenuFocus =
                target instanceof HTMLElement &&
                target.closest('.plugin-menu') &&
                this.$el.contains(target)
                    ? target
                    : null;
        },
        // Check whether the requested plugin menu is the currently open menu
        pluginMenuOpen(id: string, menu: string) {
            return this.pluginMenu === `${menu}:${id}`;
        },
        // Dismiss the plugin options
        dismissPluginOptions(event: Event) {
            const button =
                event.target instanceof Element
                    ? event.target.closest<HTMLButtonElement>('button')
                    : null;
            if (!button || button.disabled) return;
            this.pluginMenu = '';
            // Return focus before the action opens a dialog
            pluginOptionsTrigger(button)?.focus({ preventScroll: true });
        },
        // Retain the invoking plugin menu identity while its dialog or reader remains active
        rememberPluginMenu(event: Event) {
            if (
                !(event.target instanceof Element) ||
                !event.target.closest('.plugin-menu-panel button')
            )
                return;
            const { id, menu } = (event.currentTarget as HTMLElement).dataset;
            // Keep the initiating control reachable if a dialog/reader spans a resize
            this.pluginMenu = `${menu}:${id}`;
        },
        // Toggle the plugin menu
        togglePluginMenu(event: Event) {
            const trigger = event.currentTarget as HTMLElement;
            const { id, menu } = trigger.dataset;
            const key = `${menu}:${id}`;
            this.pluginMenu = this.pluginMenu === key ? '' : key;
            if (this.pluginMenu) {
                trigger.focus({ preventScroll: true });
                // Complete the deferred plugins work using the current operation state
                this.$nextTick(() => {
                    const container = trigger.closest<HTMLElement>('.plugin-menu');
                    const panel = container?.querySelector<HTMLElement>('.plugin-menu-panel');
                    const anchor = container?.closest(
                        '.plugin-card-header, .plugin-actions-toolbar',
                    );
                    if (!container || !panel || !anchor || this.pluginMenu !== key) return;
                    const rect = trigger.getBoundingClientRect();
                    const anchorRect = anchor.getBoundingClientRect();
                    const top = rect.top - anchorRect.top - anchor.clientTop;
                    const right =
                        anchor.clientWidth - (rect.right - anchorRect.left - anchor.clientLeft);
                    const padding = parseFloat(getComputedStyle(anchor).paddingRight);
                    const gap = 4;
                    const height = document.documentElement.clientHeight;
                    const above = Math.max(0, Math.min(height, rect.top - gap) - 12);
                    const below = Math.max(0, height - Math.max(0, rect.bottom + gap) - 12);
                    const upward = below < Math.min(panel.scrollHeight, 400) && above > below;
                    container.dataset.direction = upward ? 'up' : 'down';
                    container.style.setProperty(
                        '--plugin-menu-top',
                        `${top + rect.height + gap}px`,
                    );
                    container.style.setProperty(
                        '--plugin-menu-bottom',
                        `${anchor.clientHeight - top + gap}px`,
                    );
                    container.style.setProperty(
                        '--plugin-menu-right',
                        `${Math.max(padding, Math.min(right, anchor.clientWidth - panel.offsetWidth - padding))}px`,
                    );
                    container.style.setProperty(
                        '--plugin-menu-space',
                        `${upward ? above : below}px`,
                    );
                });
            }
        },
        // Close the plugin menu
        closePluginMenu(event: Event, container: HTMLElement) {
            const { id, menu } = container.dataset;
            if (!id || !menu || !this.pluginMenuOpen(id, menu)) return;
            // Dialogs and the log viewer retain their initiating control for focus/position
            if (
                document.querySelector('dialog[open]') ||
                (event.target instanceof Element &&
                    event.target.closest('dialog, .plugin-log-panel')) ||
                (Alpine.store('pluginLogs') as { visible: boolean }).visible
            )
                return;
            this.pluginMenu = '';
            if (event.type === 'keydown')
                container.querySelector<HTMLButtonElement>('.plugin-menu-trigger')?.focus();
        },
        // Reset the plugin menus
        resetPluginMenus() {
            if (
                document.querySelector('dialog[open]') ||
                (Alpine.store('pluginLogs') as { visible: boolean }).visible
            )
                return;
            // A control hidden by the new layout can blur before the resize event arrives
            const focused =
                document.activeElement === document.body
                    ? this.pluginMenuFocus
                    : (document.activeElement as HTMLElement | null);
            const container = focused?.closest('.plugin-menu');
            this.pluginMenu = '';
            // Complete the deferred plugins work using the current operation state
            this.$nextTick(() => {
                if (!container || focused?.getClientRects().length) return;
                const replacement = Array.from(
                    container.querySelectorAll<HTMLElement>(
                        'button:not(:disabled), select:not(:disabled)',
                    ),
                ).find((control) => control.getClientRects().length);
                replacement?.focus();
            });
        },
        // Import the folder
        importFolder() {
            window.dispatchEvent(new CustomEvent('import-plugin-folder'));
        },
        customPluginAppearances: new Set<string>(),
        // Refresh the custom plugin appearances
        refreshCustomPluginAppearances() {
            const custom = new Set<string>();
            for (const profile of app().snapshot.profiles) {
                for (const page of profile.pages) {
                    for (const binding of Object.values(page.bindings)) {
                        if (
                            binding?.action.kind === 'plugin' &&
                            (binding.asset ||
                                binding.appearance ||
                                binding.plugin_appearance ||
                                binding.composition ||
                                !defaultPluginBackground(
                                    binding.color,
                                    !!binding.custom_background,
                                ))
                        ) {
                            custom.add(binding.action.plugin_id);
                        }
                    }
                }
            }
            this.customPluginAppearances = custom;
        },
        // Select custom appearance when the plugin has an explicit custom palette
        pluginAppearanceSelection(plugin: Plugin) {
            return this.customPluginAppearances.has(plugin.id) ? 'custom' : plugin.appearance;
        },
        // Label the custom palette together with its underlying light, dark or UI preference
        customPluginAppearanceLabel(plugin: Plugin) {
            const base =
                plugin.appearance === 'light'
                    ? 'Light'
                    : plugin.appearance === 'dark'
                      ? 'Dark'
                      : 'Follow UI';
            return `Custom · ${base}`;
        },
        // Persist a non-custom plugin appearance selection and apply the returned snapshot
        async appearance(event: Event) {
            const target = event.currentTarget as HTMLSelectElement;
            const id = target.dataset.id;
            if (target.value === 'custom') return;
            try {
                app().apply(
                    await app().command('set_appearance', {
                        id,
                        preference: target.value,
                    }),
                );
            } catch {
                // Restore the effective selection after a failed save
            }
            const plugin = app().snapshot.plugins.find((plugin) => plugin.id === id);
            if (plugin) target.value = this.pluginAppearanceSelection(plugin);
        },
        // Open the profile-suggestion workflow for the selected plugin
        suggestedProfiles(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (id) window.dispatchEvent(new CustomEvent('open-plugin-profiles', { detail: id }));
        },
        // Return the current plugins settings
        settings(event: Event) {
            const button = event.currentTarget as HTMLElement;
            const id = button.dataset.id;
            if (id)
                void editPluginSettings(id, null, {}, pluginOptionsTrigger(button) || button).catch(
                    (error) => app().notify(String(error), 'error'),
                );
        },
        evidence: [] as {
            package_id: string;
            version: string;
            revision: string;
            platform: string;
            architecture: string;
            method: string;
            scope: string;
            outcome: string;
            limitations: string[];
        }[],
        // Select compatibility evidence matching this plugin's exact package ID and version
        evidenceFor(plugin: Plugin) {
            return this.evidence.filter(
                (r) => r.package_id === plugin.id && r.version === plugin.version,
            );
        },
        query: '',
        statusFilter: 'all',
        controlFilter: 'all',
        page: 1,
        pageSize: '6',
        actionPages: {} as Record<string, number>,
        defaultActions: {} as Record<string, string>,
        actionSelection: 0,
        // Filter the plugin's actions to those advertising settings controls
        settingsActions(plugin: Plugin) {
            return plugin.actions.filter((action) => action.has_settings);
        },
        // Retain a valid action selection or choose the first configurable action
        defaultActionId(plugin: Plugin) {
            const actions = this.settingsActions(plugin);
            const selected = this.defaultActions[plugin.id];
            return actions.some((action) => action.id === selected)
                ? selected
                : actions[0]?.id || '';
        },
        // Choose the defaults
        chooseDefaults(event: Event) {
            const el = event.currentTarget as HTMLSelectElement;
            this.defaultActions[el.dataset.id || ''] = el.value;
        },
        // Edit the defaults
        async editDefaults(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            const plugin = app().snapshot.plugins.find((plugin) => plugin.id === id);
            if (!plugin) return;
            try {
                const value = await editPluginSettings(
                    id,
                    this.defaultActionId(plugin),
                    null,
                    event.currentTarget as HTMLElement,
                );
                if (value)
                    app().notify(
                        'Action defaults saved. New bindings start with these settings; existing bindings keep their own settings.',
                        'success',
                    );
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        expanded: [] as string[],
        reloading: false,
        // Reload the current plugins data
        async reload() {
            if (app().busy || this.reloading) return;
            if (!app().nativeRendering) {
                app().notify(
                    'Reloading the plugins folder requires the native application.',
                    'warning',
                );
                return;
            }
            this.reloading = true;
            try {
                const result = await app().command('reload_plugins');
                app().apply(result.snapshot);
                if (result.added > 0) {
                    this.resetFilters();
                    this.page = 1;
                }
                app().notify(
                    result.added || result.removed
                        ? `Added ${result.added} and removed ${result.removed} ${result.removed === 1 ? 'plugin' : 'plugins'}. New plugins are disabled.`
                        : 'No plugin changes found.',
                    'success',
                );
            } catch {
            } finally {
                this.reloading = false;
            }
        },
        healthId: '',
        healthTab: 'overview',
        healthTabs: [
            { id: 'overview', label: 'Overview' },
            { id: 'events', label: 'Events' },
            { id: 'compatibility', label: 'Compatibility' },
            { id: 'alerts', label: 'Alerts' },
        ],
        // Select the health tab
        selectHealthTab(event: Event) {
            const tab = (event.currentTarget as HTMLElement).dataset.tab;
            if (this.healthTabs.some((item) => item.id === tab)) this.healthTab = tab!;
        },
        // Move focus and selection between health tabs using supported navigation keys
        navigateHealthTabs(event: KeyboardEvent) {
            const tabs = Array.from(
                (event.currentTarget as HTMLElement).querySelectorAll<HTMLButtonElement>(
                    '[role="tab"]',
                ),
            );
            const index = tabs.indexOf(event.target as HTMLButtonElement);
            if (index < 0) return;
            const next = nextTab(tabs, tabs[index], event.key);
            if (!next) return;
            event.preventDefault();
            this.healthTab = next.dataset.tab!;
            next.focus();
        },
        resourceAlerts: resourceAlertDraft(app),
        // Save the resource alerts
        saveResourceAlerts() {
            return this.resourceAlerts.save();
        },
        // Reload the resource alerts
        reloadResourceAlerts() {
            return this.resourceAlerts.reload();
        },
        // Derive health plugin from the current plugins state
        get healthPlugin() {
            const plugin = app().snapshot.plugins.find((p) => p.id === this.healthId);
            return plugin
                ? { ...plugin, runtime: this.runtimeFor(plugin), health: this.healthFor(plugin) }
                : undefined;
        },
        // Derive health plugins from the current plugins state
        get healthPlugins() {
            return this.healthPlugin ? [this.healthPlugin] : [];
        },
        // Derive health title from the current plugins state
        get healthTitle() {
            return 'Health and compatibility · ' + (this.healthPlugin?.name || 'Plugin');
        },
        // Open the health
        openHealth(event: Event) {
            this.healthId = (event.currentTarget as HTMLElement).dataset.id || '';
            this.healthTab = 'overview';
            this.visibilityListener?.();
            if (!this.healthPlugin) return;
            void this.resourceAlerts.open(this.healthId);
            // Complete the deferred plugins work using the current operation state
            this.$nextTick(() => {
                if (!this.healthId || this.disposed) return;
                const d = this.$refs.healthDialog as HTMLDialogElement;
                if (!d.open) d.showModal();
            });
        },
        // Close the health
        closeHealth() {
            this.resourceAlerts.close();
            (this.$refs.healthDialog as HTMLDialogElement)?.close();
            this.healthId = '';
            this.visibilityListener?.();
        },
        deletion: null as { id: string; name: string } | null,
        styleReset: null as { id: string; name: string } | null,
        keepPluginData: false,
        // Derive deletion title from the current plugins state
        get deletionTitle() {
            return this.deletion ? `Delete plugin “${this.deletion.name}”?` : '';
        },
        // Derive style reset title from the current plugins state
        get styleResetTitle() {
            return this.styleReset ? `Reset styles for “${this.styleReset.name}”?` : '';
        },
        // Request the style reset
        requestStyleReset(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            const plugin = app().snapshot.plugins.find((plugin) => plugin.id === id);
            if (!plugin || app().busy) return;
            this.styleReset = { id: plugin.id, name: plugin.name };
            // Complete the deferred plugins work using the current operation state
            this.$nextTick(() => {
                const dialog = this.$refs.resetStylesDialog as HTMLDialogElement;
                if (this.styleReset && !dialog.open) dialog.showModal();
            });
        },
        // Cancel the style reset
        cancelStyleReset() {
            const dialog = this.$refs.resetStylesDialog as HTMLDialogElement | undefined;
            if (dialog?.open) dialog.close();
            this.styleReset = null;
        },
        // Confirm the style reset
        async confirmStyleReset() {
            const target = this.styleReset;
            if (!target || app().busy) return;
            this.cancelStyleReset();
            try {
                app().apply(await app().command('reset_plugin_styles', { id: target.id }));
                await app().refreshDrafts();
                app().notify(`Reset styles for “${target.name}”.`, 'success');
            } catch {}
        },
        // Request the deletion
        requestDeletion(event: Event) {
            if (app().busy) return;
            const plugin = app().snapshot.plugins.find(
                (plugin) => plugin.id === (event.currentTarget as HTMLElement).dataset.id,
            );
            if (!plugin) return;
            this.keepPluginData = false;
            this.deletion = { id: plugin.id, name: plugin.name };
            // Complete the deferred plugins work using the current operation state
            this.$nextTick(() => {
                const dialog = this.$refs.deleteDialog as HTMLDialogElement;
                if (this.deletion && !dialog.open) dialog.showModal();
            });
        },
        // Cancel the deletion
        cancelDeletion() {
            const dialog = this.$refs.deleteDialog as HTMLDialogElement | undefined;
            if (dialog?.open) dialog.close();
            this.deletion = null;
        },
        // Confirm the deletion
        async confirmDeletion() {
            const target = this.deletion;
            if (!target || app().busy) return;
            this.cancelDeletion();
            try {
                app().apply(
                    await app().command('delete_plugin', {
                        id: target.id,
                        keepData: this.keepPluginData,
                    }),
                );
                await app().refreshDrafts();
                this.expanded = this.expanded.filter((id) => id !== target.id);
                app().notify(`Deleted plugin “${target.name}”.`, 'success');
            } catch {}
        },
        // Derive current target from the current plugins state
        get currentTarget() {
            return JSON.stringify([
                app().profile?.id,
                app().page?.id,
                app().controlId,
                app().deviceId,
            ]);
        },
        // Derive control groups from the current plugins state
        get controlGroups() {
            return controlGroups;
        },
        actionGroupHint: 'Select a matching control in the device editor to use these actions.',
        // Filter and paginate plugin action groups using the current search and organization
        groupIndex: new Map<string, ReturnType<typeof pluginChoiceGroups>>(),
        filteredPlugins: [] as Plugin[],
        pluginPage: paginate<Plugin>([], 1, 12),
        groupsFor(plugin: Plugin) {
            return this.groupIndex.get(plugin.id) || [];
        },
        projectGroups(plugin: Plugin) {
            return pluginChoiceGroups(
                plugin,
                this.matchesMetadata(plugin) ? '' : this.query,
                this.controlFilter,
                this.favoritesOnly,
                this.organization,
                this.actionPages,
            );
        },
        // Derive plugin page from the current plugins state
        updatePluginPage() {
            this.pluginPage = paginate(this.filteredPlugins, this.page, Number(this.pageSize));
        },
        // Change the page
        changePage(event: Event) {
            this.page = adjacentPage(
                this.pluginPage,
                Number((event.currentTarget as HTMLElement).dataset.direction),
            );
        },
        // Change the action page
        changeActionPage(event: Event) {
            this.actionPages = changeGroupPage(
                this.actionPages,
                (event.currentTarget as HTMLElement).dataset,
            );
        },
        // Check whether use type is valid for the current plugins state
        canUseType(kind: string) {
            return kind === (app().control?.kind || '');
        },
        // Derive installed count from the current plugins state
        get installedCount() {
            return app().snapshot.plugins.length;
        },
        // Derive enabled count from the current plugins state
        get enabledCount() {
            return app().snapshot.plugins.filter((plugin) => plugin.enabled).length;
        },
        // Derive summary from the current plugins state
        get summary() {
            return `${this.installedCount} installed · ${this.enabledCount} enabled · ${app().snapshot.plugins.filter((p) => this.runtimeFor(p).state === 'running').length} running`;
        },
        // Match the search query against normalized plugin and runtime metadata
        matchesMetadata(plugin: Plugin) {
            return `${plugin.name} ${plugin.version} ${plugin.author || ''} ${pluginSdkLabel(plugin)} ${plugin.dialect} ${plugin.kind} ${this.runtimeStatus(plugin)} ${this.runtimeDescription(plugin)}`
                .toLowerCase()
                .includes(this.query.trim().toLowerCase());
        },
        // Derive filtered plugins from the current plugins state
        updatePluginResults() {
            this.groupIndex = new Map(
                app().snapshot.plugins.map((plugin) => [plugin.id, this.projectGroups(plugin)]),
            );
            this.filteredPlugins = this.filterPlugins();
        },
        filterPlugins() {
            const query = this.query.trim().toLowerCase();
            // Keep entries that satisfy the plugins selection criteria
            return app().snapshot.plugins.filter((plugin) => {
                const matchesStatus =
                    this.statusFilter === 'all' ||
                    (this.statusFilter === 'enabled' && plugin.enabled) ||
                    (this.statusFilter === 'disabled' && !plugin.enabled) ||
                    (this.statusFilter === 'running' &&
                        this.runtimeFor(plugin).state === 'running') ||
                    (this.statusFilter === 'idle' &&
                        plugin.enabled &&
                        this.runtimeFor(plugin).state === 'idle') ||
                    (this.statusFilter === 'attention' &&
                        ['blocked', 'retrying'].includes(this.runtimeFor(plugin).state));
                return (
                    matchesStatus &&
                    (this.groupsFor(plugin).length > 0 ||
                        (!this.favoritesOnly &&
                            this.controlFilter === 'all' &&
                            plugin.actions.length === 0)) &&
                    (this.matchesMetadata(plugin) ||
                        plugin.actions.some((action) =>
                            `${action.name} ${this.organization.tags({ kind: 'plugin', plugin_id: plugin.id, action_id: action.id }).join(' ')}`
                                .toLowerCase()
                                .includes(query),
                        ))
                );
            });
        },
        // Derive results label from the current plugins state
        get resultsLabel() {
            const count = this.filteredPlugins.length;
            return `${count} ${count === 1 ? 'plugin' : 'plugins'}`;
        },
        // Derive has filters from the current plugins state
        get hasFilters() {
            return (
                this.favoritesOnly ||
                Boolean(this.query.trim()) ||
                this.statusFilter !== 'all' ||
                this.controlFilter !== 'all'
            );
        },
        // Initialize plugins state and its required lifecycle hooks
        init() {
            ownWindowMethods(this.resourceAlerts, 'plugin resource alerts', ['reload', 'save']);
            this.unregisterCloseOwner = registerCloseDraft(this.resourceAlerts, ['save'], {
                name: 'Plugin resource alerts',
                retainOnUnmount: true,
                // Return a snapshot of the current plugins state
                state: () => (this.resourceAlerts.dirty ? 'dirty' : 'clean'),
                // Discard pending plugins edits without saving them
                discard: () => this.closeHealth(),
                // Release resources retained by the plugins operation
                release: () => this.releaseCloseResources(),
            });
            void this.organization.load();
            this.$watch('favoritesOnly', () => {
                this.page = 1;
                this.actionPages = {};
            });
            this.monitorId = crypto.randomUUID();
            this.disposed = false;
            if (native)
                this.$watch('permissionBadgeScope', () => void this.refreshPermissionBadges());
            this.startMonitoring();
            this.watchPermissionBadges();
            this.resumeListener = () => {
                this.startMonitoring();
                this.watchPermissionBadges();
            };
            window.addEventListener('editor-window-resumed', this.resumeListener);
            // Watch a boolean, not the full plugin/health/resource history. Filters,
            // pagination and lifecycle changes can hide the last running card
            this.$watch('monitoringVisible', () => this.visibilityListener?.());
            this.$watch('pageSize', () => {
                this.page = 1;
            });
            void fetch('/plugin-compatibility.json')
                .then((r) => r.json())
                .then((records) => {
                    this.evidence = records;
                })
                .catch(() => {});
            Object.assign(this, app().pluginListFilters);
            // Persist the current plugin-list filters in application UI state
            const rememberFilters = () => {
                const filters = app().pluginListFilters;
                Object.assign(filters, {
                    query: this.query,
                    statusFilter: this.statusFilter,
                    controlFilter: this.controlFilter,
                    page: this.page,
                    pageSize: this.pageSize,
                });
            };
            for (const field of ['query', 'statusFilter', 'controlFilter', 'page', 'pageSize'])
                this.$watch(field, rememberFilters);
            // Complete the deferred plugins work using the current operation state
            this.$watch('$store.app.route', () => {
                this.visibilityListener?.();
                this.cancelDeletion();
                this.closeHealth();
            });
            this.$watch('healthPlugin', () => {
                if (this.healthId && !this.healthPlugin) this.closeHealth();
            });
            // Complete the deferred plugins work using the current operation state
            this.$watch('query', () => {
                this.page = 1;
                this.actionPages = {};
                if (this.query.trim())
                    this.expanded = this.filteredPlugins.map((plugin) => plugin.id);
            });
            // Complete the deferred plugins work using the current operation state
            this.$watch('statusFilter', () => {
                this.page = 1;
                this.actionPages = {};
                if (this.query.trim())
                    this.expanded = this.filteredPlugins.map((plugin) => plugin.id);
            });
            // Complete the deferred plugins work using the current operation state
            this.$watch('controlFilter', () => {
                this.page = 1;
                this.actionPages = {};
                if (this.query.trim())
                    this.expanded = this.filteredPlugins.map((plugin) => plugin.id);
            });
        },
        // Start the monitoring
        startMonitoring() {
            if (!this.disposed && this.runtimeRegistration) return;
            this.disposed = false;
            this.monitorVisible = null;
            if (native) {
                // Apply the runtime
                const applyRuntime = (snapshots: PluginRuntimeSnapshot[]) => {
                    if (this.disposed) return;
                    reconcile(this.live, indexPluginRuntime(snapshots));
                    this.liveAt = performance.now();
                    this.clock = this.liveAt;
                };
                this.runtimeRegistration = listen<PluginRuntimeSnapshot[]>(
                    'plugin-runtime',
                    (event) => applyRuntime(event.payload),
                    // Process the settled plugins operation result
                ).then(async (unsubscribe) => {
                    const key = `plugin-runtime-listener:${crypto.randomUUID()}`;
                    // Release resources retained by the plugins operation
                    const release = () =>
                        trackCloseRelease(key, async () => {
                            await unsubscribe();
                        });
                    if (this.disposed) await release();
                    else this.unsubscribeRuntime = release;
                });
                void this.runtimeRegistration.catch(() => {});
                void app()
                    .command('plugin_runtime_snapshot', undefined, false)
                    .then(applyRuntime)
                    .catch(() => {});
                // Complete the deferred plugins work using the current operation state
                this.visibilityListener = () => {
                    const visible = this.monitoringVisible;
                    if (this.monitorVisible !== visible) {
                        this.monitorVisible = visible;
                        void app()
                            .command('plugin_monitor_visible', {
                                consumer: this.monitorId,
                                visible,
                            })
                            // Handle a failed plugins operation without retaining stale work
                            .catch(() => {
                                if (this.monitorVisible === visible) this.monitorVisible = null;
                            });
                    }
                    if (visible && !this.uptimeTimer) {
                        this.clock = performance.now();
                        this.uptimeTimer = window.setInterval(() => {
                            this.clock = performance.now();
                        }, 1000);
                    } else if (!visible && this.uptimeTimer) {
                        window.clearInterval(this.uptimeTimer);
                        this.uptimeTimer = 0;
                    }
                };
                document.addEventListener('visibilitychange', this.visibilityListener);
                this.visibilityListener();
            }
        },
        // Release the close resources
        async releaseCloseResources() {
            this.disposed = true;
            ++this.permissionBadgeGeneration;
            const permissionBadgeSubscription = this.permissionBadgeSubscription;
            this.permissionBadgeSubscription = null;
            if (permissionBadgeSubscription)
                void trackCloseRelease(`plugin-permission-badges:${this.monitorId}`, async () => {
                    const stop = await permissionBadgeSubscription.catch(() => null);
                    await stop?.();
                }).catch(() => {});
            window.clearInterval(this.uptimeTimer);
            this.uptimeTimer = 0;
            if (this.visibilityListener)
                document.removeEventListener('visibilitychange', this.visibilityListener);
            await this.runtimeRegistration?.catch(() => {});
            this.runtimeRegistration = null;
            const unsubscribe = this.unsubscribeRuntime;
            this.unsubscribeRuntime = null;
            await unsubscribe?.();
            if (native)
                await trackCloseRelease(`plugin-monitor:${this.monitorId}`, () =>
                    app().bridge.command('plugin_monitor_visible', {
                        consumer: this.monitorId,
                        visible: false,
                    }),
                );
            this.cancelDeletion();
        },
        // Release plugins state and associated lifecycle resources
        destroy() {
            if (this.resumeListener)
                window.removeEventListener('editor-window-resumed', this.resumeListener);
            this.unregisterCloseOwner?.();
            void this.releaseCloseResources().catch(() => {});
        },
        // Reset the filters
        resetFilters() {
            this.favoritesOnly = false;
            this.query = '';
            this.statusFilter = 'all';
            this.controlFilter = 'all';
            this.expanded = [];
        },
        // Derive a two-letter Unicode-aware plugin monogram with a fallback label
        initials(plugin: Plugin) {
            return (
                plugin.name
                    .split(/[^\p{L}\p{N}]+/u)
                    .filter(Boolean)
                    .slice(0, 2)
                    .map((word) => word[0])
                    .join('')
                    .toUpperCase() || 'P'
            );
        },
        authorLabel: pluginAuthorLabel,
        dialectLabel: pluginSdkLabel,
        runtimeLabel: pluginRuntimeLabel,
        // Check whether expanded is valid for the current plugins state
        isExpanded(id: string) {
            return this.expanded.includes(id);
        },
        // Toggle the actions
        toggleActions(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            this.expanded = this.isExpanded(id)
                ? this.expanded.filter((value) => value !== id)
                : [...this.expanded, id];
        },
        // Admit an action selection only while its plugin and target context remain valid
        async useAction(event: Event) {
            const data = { ...(event.currentTarget as HTMLElement).dataset };
            if (!data.plugin || !data.action) return;
            const selection = ++this.actionSelection;
            const target = this.currentTarget;
            if (
                !app().snapshot.plugins.some(
                    (plugin) => plugin.id === data.plugin && plugin.enabled,
                ) ||
                !this.canUseType(data.kind || '')
            )
                return;
            const action = await loadPluginAction(app(), data.plugin, data.action, () =>
                Boolean(selection === this.actionSelection && target === this.currentTarget),
            );
            if (!action) return;
            app().pendingActionTitle = data.name || 'Plugin action';
            app().pendingAction = action;
            location.hash = '#/editor';
            app().notify('Plugin action selected. Save the binding to apply it.');
        },
        // Derive app from the current plugins state
        get app() {
            return app();
        },
        // Install the selected plugins item
        async install() {
            try {
                await installPluginPackage(app(), native);
            } catch {}
        },
        // Persist the plugin's sleep-when-hidden preference and apply the returned snapshot
        sleep(event: Event) {
            const el = event.currentTarget as HTMLInputElement;
            if (!el.dataset.id) return;
            swallow(
                app()
                    .command('set_plugin_sleep', {
                        id: el.dataset.id,
                        sleepWhenHidden: el.checked,
                    })
                    .then((s) => app().apply(s))
                    // Handle a failed plugins operation without retaining stale work
                    .catch((error) => {
                        el.checked = !el.checked;
                        throw error;
                    }),
            );
        },
        // Toggle the current plugins selection
        toggle(event: Event) {
            swallow(togglePlugin(app(), event));
        },
    }));
}
