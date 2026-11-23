import { suspendLiveActionForPalette } from './live-action-preview';
import { registerPaletteBrowser } from './palette-browser';
import { pluginPalettePreviewContext } from './plugin-palette-preview';
import { createPalettePreviewLease, type PaletteCandidate } from './palette-dock-preview';
import { publicPreview } from './public-preview';
import {
    failBootSplash,
    finishBootSplash,
    suspendBootSplash,
    waitForBootPreview,
} from './boot-splash';
import { registerFontPicker } from './font-picker';
import { registerStartupPermissions } from './startup-permissions';
import { runStartupVault } from './startup-vault';
import { registerRecovery } from './recovery';
import { reportFrontendError, reportStartupFailure } from './frontend-errors';
import type { FrontendStartupStage } from '../generated/runtime';
import { registerManagerLogs } from './manager-logs';
import managerBuildVersion from 'virtual:manager-build-version';
import { versionLabel } from './version-label';
import Alpine from '@alpinejs/csp';
import Loader from 'alpine-component-loader/core';
import { registerRoute } from 'virtual:alpine-component-loader/routes';
import { bridge, native } from './bridge';
import { BenchmarkProgressStore } from './benchmark-progress';
import { registerBenchmarkMonitor } from './benchmark-monitor';
import { listen } from '@tauri-apps/api/event';
import { AppState } from './state';
import type { Snapshot } from './types';
import { registerTheme } from './theme';
import { jumpToSection, registerTooltips } from './visibility';
import { registerLocationPicker } from './location-picker';
import { registerPluginSettings } from './plugin-settings';
import { registerRuntimes } from './runtimes';
import { registerApplicationData } from './application-data';
import { installDialogLoader } from './dialog-loader';
import { RetryableTasks, componentReady } from './route-loading';
import { registerUpdates } from './updates';
import { registerToasts } from './toasts';
import { registerArchiveDrops } from './archive-import';
import { draftQueue, swallow } from './route-context';
import { registerMediaJobs } from './media-jobs';
import './styles.css';
import { registerPluginLogs } from './plugin-logs';
import { openLogWindow } from './plugin-log-window';
import { registerAccessibility } from './accessibility';
import { registerAutoScrollbars } from './scrollbars';
import {
    StartupLifetime,
    WindowResumeCoordinator,
    registerNativeWindowEvents,
} from './window-startup';
import { editorRestoreState, windowInvoke } from './window-native';
import { ownWindowMethods, windowWork } from './window-work';
import { WindowCloseCoordinator } from './window-close';
import { registerWindowCloseDialog } from './window-close-dialog';
import { windowCloseOwners } from './window-close-owners';
import { developmentReloadApplies, prepareDevelopmentReload } from './window-reload';
import { drainCloseReleases, trackCloseRelease } from './close-releases';
import { pauseWindowReaders, resumeWindowReaders } from './polling-reader';
import type { EditorRestoreState } from '../generated/runtime';
import {
    actionLibrarySelection,
    registerActionLibrarySelection,
    type LibrarySelection,
} from './action-library-selection';
const recovery = registerRecovery();
registerAccessibility(Alpine);
registerToasts(Alpine);
const closeDialog = registerWindowCloseDialog(Alpine);

declare global {
    interface Window {
        Alpine: typeof Alpine;
    }
}
window.Alpine = Alpine;
const themeStore = registerTheme(Alpine, native);
registerTooltips();
registerPluginSettings(Alpine, bridge);
registerLocationPicker(Alpine, bridge);
registerStartupPermissions(Alpine, bridge);
if (native) {
    let lastActivity = -Infinity;
    const recordVaultActivity = (event: Event) => {
        if (
            !event.isTrusted ||
            performance.now() - lastActivity < 5000 ||
            !(event.target instanceof Element) ||
            !event.target.closest('sd-vault, #settings-plugin-permissions, .install-vault-picker')
        )
            return;
        lastActivity = performance.now();
        void bridge.command('vault_user_activity').catch(() => {});
    };
    for (const type of ['pointerdown', 'keydown']) {
        document.addEventListener(type, recordVaultActivity, { capture: true, passive: true });
    }
}

const state = new AppState(bridge);
Alpine.store('app', state);
Alpine.store('benchmarkProgress', new BenchmarkProgressStore(bridge, native, document));
// Read the shared benchmark progress store
const benchmarkProgress = () => Alpine.store('benchmarkProgress') as BenchmarkProgressStore;
windowCloseOwners.register({
    name: 'Running benchmark',
    // Protect window closure while a benchmark is running
    state: () => (benchmarkProgress().progress.running ? 'dirty' : 'clean'),
    // Cancel a running benchmark when its close owner is discarded
    discard: async () => {
        if (benchmarkProgress().progress.running) await benchmarkProgress().cancel();
    },
});
// Read the reactive application store
const app = () => Alpine.store('app') as AppState;
registerActionLibrarySelection(Alpine, app);
registerManagerLogs(Alpine, app);
const lifetime = new StartupLifetime();
lifetime.own(registerAutoScrollbars());
lifetime.own(registerFontPicker());
let activePalette:
    { owner: symbol; candidate: PaletteCandidate; followNavigation: boolean } | undefined;
let paletteDraftRetirement = Promise.resolve();
// One global candidate is applied by native code to every connected device and
// compatible plugin. Selection and profile navigation do not retarget this lease.
const palettePlugins = createPalettePreviewLease({
    context: () =>
        pluginPalettePreviewContext(native, {
            route: app().route,
            followNavigation: true,
            suspended: app().windowSuspended,
            hidden: document.hidden,
            quiescing: windowWork.quiescing,
            closing: windowCloseOwners.closing,
        }),
    start: async (_context, palette) => {
        await paletteDraftRetirement;
        return bridge.command('preview_plugin_palette', { palette });
    },
    onError: (error) =>
        app().notify('Could not preview device and plugin themes: ' + error, 'error'),
    cancel: (token) =>
        trackCloseRelease('plugin-palette:' + token, () =>
            bridge.command('cancel_plugin_palette_preview', { token }),
        ),
});
themeStore.bindPalettePreviewTransport((owner, candidate, followNavigation = false) => {
    if (candidate) {
        activePalette = { owner, candidate, followNavigation };
        paletteDraftRetirement = suspendLiveActionForPalette(true);
        palettePlugins.set(owner, candidate);
    } else {
        if (activePalette?.owner === owner) activePalette = undefined;
        void palettePlugins.clear(owner).then(() => {
            if (!activePalette) void suspendLiveActionForPalette(false);
        });
    }
});
const paletteContextChanged = () => {
    // Only window lifetime affects the global lease, never selected-device navigation.
    void palettePlugins.contextChanged();
    const current = activePalette;
    if (current?.followNavigation)
        queueMicrotask(() => {
            if (activePalette !== current || lifetime.disposed) return;
            palettePlugins.set(current.owner, current.candidate);
        });
};
const paletteContextEffect = Alpine.effect(paletteContextChanged);
const paletteVisibilityChanged = paletteContextChanged;
document.addEventListener('visibilitychange', paletteVisibilityChanged);
lifetime.own(() => document.removeEventListener('visibilitychange', paletteVisibilityChanged));
lifetime.own(() => {
    Alpine.release(paletteContextEffect);
    themeStore.bindPalettePreviewTransport(undefined);
    activePalette = undefined;
    void palettePlugins.clear();
});
lifetime.own(
    windowCloseOwners.register({
        name: 'Palette previews',
        state: () => 'clean',
        discard: async () => {
            activePalette = undefined;
            await palettePlugins.clear();
        },
        release: async () => {
            activePalette = undefined;
            await palettePlugins.clear();
        },
    }),
);
let profileThemeSignature = '';
let themeDevice: string | undefined;
let contextWrite = Promise.resolve();
// Synchronize saved profile palettes and native theme context
const profileThemeEffect = Alpine.effect(() => {
    const deviceId = app().device?.id || '';
    const palette = app().profile?.palette || null;
    const signature = JSON.stringify([deviceId, palette]);
    if (signature === profileThemeSignature) return;
    profileThemeSignature = signature;
    // Apply outside dependency collection: theme previews and global settings own
    // their own updates, while this effect follows saved profile/session changes
    queueMicrotask(() => {
        if (lifetime.disposed) return;
        themeStore.setProfilePalette(palette);
        if (native && themeDevice !== deviceId) {
            themeDevice = deviceId;
            contextWrite = contextWrite
                // Write the selected device theme context while the window is active
                .then(async () => {
                    if (!lifetime.disposed && !windowWork.quiescing)
                        await bridge.command('set_profile_theme_context', { deviceId });
                })
                .catch((error) =>
                    console.error('Could not synchronize profile appearance:', error),
                );
        }
    });
});
lifetime.own(() => Alpine.release(profileThemeEffect));
let startupInteractive = false;
// Block keyboard interaction before startup completes
const startupKeyboardGuard = (event: KeyboardEvent) => {
    if (
        !startupInteractive &&
        !(event.target as Element | null)?.closest?.('#startup-vault[open]')
    ) {
        event.preventDefault();
        event.stopImmediatePropagation();
    }
};
window.addEventListener('keydown', startupKeyboardGuard, true);
lifetime.own(() => window.removeEventListener('keydown', startupKeyboardGuard, true));
lifetime.own(() => app().stop());
// Apply plugin theme metadata only while startup ownership is active
const themeMetadata = (event: Event) => {
    if (!lifetime.disposed) app().apply((event as CustomEvent<Snapshot>).detail);
};
window.addEventListener('plugin-theme-metadata', themeMetadata);
lifetime.own(() => window.removeEventListener('plugin-theme-metadata', themeMetadata));
// Flush drafts and dispose the window on abrupt document unload
const pageHidden = () => {
    if (lifetime.disposed) return;
    try {
        if (windowWork.quiescing || windowCloseOwners.closing) return;
        void app()
            .flushDrafts?.()
            .catch((error) => console.error(error));
    } finally {
        lifetime.dispose();
    }
};
// Vite awaits this hook while the old document's IPC bridge is still alive
// pagehide cannot await native cleanup and remains only an abrupt-unload fallback
if (import.meta.hot) {
    // Prepare only the development reload affecting this document
    const beforeReload = (payload: { path?: string }) => {
        if (!developmentReloadApplies(payload.path, location.pathname, import.meta.env.BASE_URL))
            return;
        return prepareDevelopmentReload({
            // Flush drafts before development reload retires the bridge
            flush: async () => {
                await app().flushDrafts?.();
            },
            // Pause interaction and readers before development reload cleanup
            pause: () => {
                windowInteractive(false);
                windowCloseOwners.setClosing(true);
                if (startupInteractive) pauseEditorWindow();
                else {
                    app().windowSuspended = true;
                    pauseWindowReaders();
                }
            },
            // Release registered close owners before reloading
            release: () => windowCloseOwners.releaseAll(),
            // Dispose startup resources before reloading
            dispose: () => lifetime.dispose(),
            // Await all native and frontend cleanup before development reload
            settle: async () => {
                await lifetime.settled();
                await app().subscriptionsSettled();
                await drainCloseReleases();
                await windowWork.settled();
            },
        }).catch((error) => console.error('Development reload cleanup failed:', error));
    };
    import.meta.hot.on('vite:beforeFullReload', beforeReload);
    lifetime.own(() => import.meta.hot?.off('vite:beforeFullReload', beforeReload));
}
window.addEventListener('pagehide', pageHidden, { once: true });
lifetime.own(() => window.removeEventListener('pagehide', pageHidden));
registerBenchmarkMonitor(Alpine, benchmarkProgress);
registerMediaJobs(Alpine, app);
registerPluginLogs(Alpine, bridge, app, {
    // Open the selected plugin log stream in a separate window
    popOut: (id, stream) => openLogWindow(bridge, native, id, stream),
});
registerRuntimes(Alpine, app);
registerApplicationData(Alpine, app);

const loadedControllers = new RetryableTasks();
// Load a route’s controllers before mounting its components
async function ensureRoute(route: string) {
    const loaders = {
        // Load shell controllers on demand
        shell: () => import('./route-shell'),
        // Load device editor controllers on demand
        editor: () => import('./route-editor'),
        'action-library': () => import('./route-action-library'),
        // Load plugin management controllers on demand
        plugins: () => import('./route-plugins'),
        // Load settings controllers on demand
        settings: () => import('./route-settings'),
        vault: () => import('./route-vault'),
        // Load diagnostics controllers on demand
        diagnostics: () => import('./route-diagnostics'),
    };
    const load = loaders[route as keyof typeof loaders];
    const actions =
        route === 'editor' || route === 'macros'
            ? loadedControllers.run('action-dialogs', async () =>
                  (await import('./action-controllers')).register(),
              )
            : undefined;
    if (load) {
        await loadedControllers.run(route, async () => {
            const [module] = await Promise.all([load(), actions]);
            module.register();
        });
    } else await actions;
    await registerRoute(route);
    if (route === 'editor' || route === 'macros') await mountActionDialogs();
}
const actionDialogs = new RetryableTasks();
// Mount action dialogs once through the shared feature mount guard
function mountActionDialogs() {
    // Load and attach action dialogs within their shared mount operation
    return actionDialogs.run('mount', async () => {
        const outlet = document.querySelector('#action-dialogs-outlet');
        if (!outlet) throw Error('Action dialog outlet is missing');
        const editor = document.createElement('sd-action-editor');
        editor.setAttribute('named', '');
        editor.setAttribute('modal', '');
        const elements = [editor];
        const cancellation = new AbortController();
        const ready = elements.map((element) => componentReady(element, cancellation.signal));
        outlet.append(...elements);
        try {
            await Promise.all(ready);
        } catch (error) {
            cancellation.abort();
            for (const element of elements) element.remove();
            throw error;
        }
    });
}
let initialNavigationDone: () => void;
// Wait for the initial route navigation to finish
const initialNavigation = new Promise<void>((resolve) => {
    initialNavigationDone = resolve;
});
const sidebarCollapsedKey = 'stream-dock-manager.sidebar.collapsed';
// Capture the selected binding context for route handoffs
const selectedControlContext = () =>
    JSON.stringify([app().deviceId, app().profile?.id, app().page?.id, app().controlId]);
Alpine.data('shell', () => ({
    jumpToSection,
    managerBuildVersion,
    // Use native development authority rather than checkout location or dirty files
    get managerVersion() {
        const permissions = Alpine.store('startupPermissions') as
            | {
                  report?: { development_mode?: boolean } | null;
              }
            | undefined;
        return versionLabel(
            this.managerBuildVersion,
            native,
            permissions?.report?.development_mode,
            import.meta.env.DEV,
        );
    },
    publicPreview,
    aboutError: '',
    // Open About while preserving the initiating control’s focus
    openAbout(event: Event) {
        // Safari does not focus clicked buttons by default. Give the native dialog
        // the initiating control to restore focus to, regardless of input method
        (event.currentTarget as HTMLElement).focus({ preventScroll: true });
        this.aboutError = '';
        const dialog = this.$refs.about as HTMLDialogElement;
        if (!dialog.open) dialog.showModal();
    },
    // Close the About dialog
    closeAbout() {
        (this.$refs.about as HTMLDialogElement).close();
    },
    // Open an approved project link through the appropriate host path
    async openProjectLink(event: Event) {
        if (!native) return;
        event.preventDefault();
        const kind = (event.currentTarget as HTMLElement).dataset.kind;
        if (kind !== 'website' && kind !== 'source' && kind !== 'issues' && kind !== 'license')
            return;
        this.aboutError = '';
        try {
            await bridge.command('open_project_link', { kind });
        } catch (error) {
            this.aboutError = String(error);
        }
    },
    // Copy the displayed Manager version to the clipboard
    async copyVersion() {
        const fullVersion = 'v' + this.managerVersion;
        try {
            await navigator.clipboard.writeText(fullVersion);
            app().notify('Version copied', 'success');
        } catch {
            app().notify('Could not copy version: ' + fullVersion, 'error');
        }
    },
    sidebarCollapsed: false,
    footerObserver: null as ResizeObserver | null,
    // Track footer size for the shell layout
    observeFooter() {
        const footer = this.$refs.statusbar as HTMLElement;
        const shell = footer.closest<HTMLElement>('.app-shell');
        if (!shell) return;
        // Update the shell’s footer spacing after its size changes
        this.footerObserver = new ResizeObserver(() => {
            // The status text can wrap when the window narrows or text is enlarged
            shell.style.setProperty(
                '--shell-footer-height',
                `${footer.getBoundingClientRect().height}px`,
            );
        });
        this.footerObserver.observe(footer, { box: 'border-box' });
    },
    // Toggle the sidebar and retain the triggering control state
    toggleSidebar(event: Event) {
        this.sidebarCollapsed = !this.sidebarCollapsed;
        try {
            localStorage.setItem(sidebarCollapsedKey, String(this.sidebarCollapsed));
        } catch {}
        (event.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
    },
    // Report whether a benchmark run is active
    get benchmarkActive() {
        return benchmarkProgress().progress.running;
    },
    // Open progress for the active benchmark
    openBenchmarkProgress(event: Event) {
        (event.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
        window.dispatchEvent(new CustomEvent('open-benchmark-progress'));
    },
    navigationRevision: 0,
    navigationError: '',
    navigating: false,
    navigationCancellation: null as AbortController | null,
    navigationHandler: null as (() => void) | null,
    libraryAction: null as (LibrarySelection & { context: string }) | null,
    // Read the active action-library selection flow
    get librarySelection() {
        return actionLibrarySelection;
    },
    // Cancel the active action-library selection
    cancelLibrarySelection() {
        actionLibrarySelection.cancel();
    },
    // Return the chosen library action to its requesting editor
    configureLibraryAction(event: CustomEvent<LibrarySelection>) {
        if (actionLibrarySelection.request) {
            if (actionLibrarySelection.choosing) actionLibrarySelection.choose(event.detail);
            return;
        }
        if (!app().control || this.navigating || windowWork.quiescing) return;
        this.libraryAction = { ...event.detail, context: selectedControlContext() };
        if (location.hash === '#/editor') swallow(this.navigate());
        else location.hash = '#/editor';
    },
    // Expose application state to shell bindings
    get app() {
        return app();
    },
    // Describe the selected device in the shell
    get deviceName() {
        return app().device?.name || 'No device connected';
    },
    // Describe the selected device’s connection state
    get deviceStatus() {
        const device = app().device;
        if (!device) return this.deviceName;
        const connection = device.simulated
            ? 'Simulated'
            : device.connected
              ? 'Connected'
              : 'Disconnected';
        return `${device.name} · ${connection}`;
    },
    // Summarize the current application status
    get status() {
        const device = app().device;
        if (!device) return 'Connect a Stream Dock to begin';
        if (!device.connected) return 'Disconnected';
        if (device.simulated)
            return app().nativeRendering ? 'Actions enabled' : 'Preview · no host actions';
        return 'Connected';
    },
    dialogEvents: undefined as AbortController | undefined,
    powerTimer: 0,
    // Connect shell navigation and initialize the selected route
    async init() {
        try {
            this.sidebarCollapsed = localStorage.getItem(sidebarCollapsedKey) === 'true';
        } catch {}
        ownWindowMethods(this, 'navigation', ['navigate']);
        this.dialogEvents = new AbortController();
        installDialogLoader(
            async (key) => {
                const { dialogControllers } = await import('./route-shell');
                await loadedControllers.run('dialog:' + key, async () => {
                    await dialogControllers[key]?.();
                });
                await registerRoute(key);
            },
            (work) => {
                return windowWork.run('open dialog', work).catch((error) => {
                    app().notify(String(error), 'error');
                });
            },
            this.dialogEvents.signal,
        );
        this.powerTimer = window.setInterval(() => {
            if (['editor', 'settings'].includes(app().route) && !windowCloseOwners.closing)
                void app().refreshPower();
        }, 15000);
        benchmarkProgress().startWindow();
        (Alpine.store('pluginLogs') as import('./plugin-logs').PluginLogs).startWindow();
        // Reconcile the shell after a hash navigation event
        this.navigationHandler = () => {
            swallow(this.navigate());
        };
        window.addEventListener('hashchange', this.navigationHandler);
        await this.navigate();
    },
    // Load the requested route while guarding stale navigation
    async navigate() {
        const generation = ++this.navigationRevision;
        const libraryAction = this.libraryAction;
        this.libraryAction = null;
        this.navigationCancellation?.abort();
        const cancellation = new AbortController();
        this.navigationCancellation = cancellation;
        this.navigating = true;
        app().navigationPending = true;
        this.navigationError = '';
        let candidate: HTMLElement | undefined;
        let createdCandidate = false;
        try {
            const path = location.hash.replace('#/', '');
            const name = /^(settings|diagnostics|vault)\?/.test(path)
                ? path.split('?')[0]
                : path || 'editor';
            const route = [
                'editor',
                'action-library',
                'macros',
                'plugins',
                'settings',
                'vault',
                'diagnostics',
            ].includes(name)
                ? name
                : 'editor';
            await ensureRoute(route);
            if (generation !== this.navigationRevision) return;
            await app().flushDrafts?.();
            if (generation !== this.navigationRevision) return;
            const editorOutlet = document.querySelector('#editor-outlet');
            const outlet = document.querySelector('#route-outlet');
            if (route === 'editor') {
                if (editorOutlet && !editorOutlet.firstElementChild) {
                    candidate = document.createElement('sd-editor');
                    createdCandidate = true;
                    const ready = componentReady(candidate, cancellation.signal);
                    editorOutlet.append(candidate);
                    await ready;
                }
            } else if (outlet) {
                const request = actionLibrarySelection.request;
                const retained = request?.route === route ? request.routeElement : null;
                if (retained?.isConnected) candidate = retained;
                else {
                    candidate = document.createElement('sd-' + route);
                    createdCandidate = true;
                    candidate.hidden = true;
                    const ready = componentReady(candidate, cancellation.signal);
                    outlet.append(candidate);
                    await ready;
                }
            }
            if (generation !== this.navigationRevision) {
                if (createdCandidate) candidate?.remove();
                return;
            }
            app().route = route;
            if (route === 'editor' || route === 'settings') void app().refreshPower();
            if (route !== 'editor') {
                app().pauseSurfaces();
                if (actionLibrarySelection.request?.route !== 'editor')
                    editorOutlet?.replaceChildren();
            }
            if (route === 'editor' && native) app().requestSurfaces();
            if (outlet) {
                for (const child of Array.from(outlet.children)) {
                    if (child === candidate) continue;
                    if (child === actionLibrarySelection.request?.routeElement) {
                        (child as HTMLElement).hidden = true;
                        (child as HTMLElement).inert = true;
                    } else child.remove();
                }
            }
            if (candidate) {
                candidate.hidden = false;
                candidate.inert = false;
            }
            await actionLibrarySelection.navigated(
                route,
                () => generation === this.navigationRevision,
            );
            if (generation !== this.navigationRevision) return;
            if (route === 'editor' && libraryAction) {
                await Alpine.nextTick();
                if (
                    generation !== this.navigationRevision ||
                    libraryAction.context !== selectedControlContext()
                )
                    return;
                if ('action' in libraryAction) {
                    const selected = libraryAction.action;
                    const plugin = app().snapshot.plugins.find(
                        (plugin) => plugin.id === selected.plugin_id,
                    );
                    const action = plugin?.actions.find(
                        (action) => action.id === selected.action_id,
                    );
                    if (
                        !plugin?.enabled ||
                        !action ||
                        !(action.control_types ?? ['button']).includes(app().control?.kind || '')
                    ) {
                        app().notify(
                            'This plugin action is no longer available for the selected control.',
                            'warning',
                        );
                        return;
                    }
                    app().pendingActionTitle = libraryAction.name;
                    app().pendingAction = selected;
                } else {
                    window.dispatchEvent(
                        new CustomEvent('choose-builtin-action', {
                            detail: { kind: libraryAction.kind },
                        }),
                    );
                }
                await Alpine.nextTick();
                // Let navigation finish and re-enable the chooser before restoring focus
                if (document.hidden) return;
                // Restore route focus after the destination has rendered
                requestAnimationFrame(() => {
                    if (generation !== this.navigationRevision || document.hidden) return;
                    const chooser = document.querySelector<HTMLButtonElement>('#action-kind');
                    chooser?.scrollIntoView({ block: 'nearest' });
                    chooser?.focus({ preventScroll: true });
                });
            }
        } catch (error) {
            if (createdCandidate) candidate?.remove();
            if (generation === this.navigationRevision) {
                this.navigationError = 'Could not open this page: ' + String(error);
                if (location.hash === '#/editor') this.libraryAction = libraryAction;
            }
            reportFrontendError('route_failed', error);
        } finally {
            if (generation === this.navigationRevision) {
                this.navigating = false;
                app().navigationPending = false;
                initialNavigationDone();
            }
        }
    },
    // Release shell navigation and layout observers
    destroy() {
        this.footerObserver?.disconnect();
        this.dialogEvents?.abort();
        window.clearInterval(this.powerTimer);
        ++this.navigationRevision;
        this.navigationCancellation?.abort();
        benchmarkProgress().stopWindow();
        (Alpine.store('pluginLogs') as import('./plugin-logs').PluginLogs).stopWindow();
        if (this.navigationHandler)
            window.removeEventListener('hashchange', this.navigationHandler);
    },
    // Open simulated device selection in browser preview
    simulator() {
        swallow(
            // Track simulator selection within the active window lifetime
            windowWork.run('simulator selection', async () => {
                await app().flushDrafts?.();
                const selected = app().device;
                app().apply(
                    await app().command('simulate', {
                        enabled: !selected?.simulated,
                        deviceId: selected?.simulated ? selected.id : 'simulator:n4-pro',
                    }),
                );
            }),
        );
    },
    // Select a profile from the shell control
    selectProfileId(id: string) {
        swallow(
            app()
                .command('activate_profile', { id, deviceId: app().deviceId })
                .then((s) => app().apply(s)),
        );
    },
}));
let nativeWindowState: EditorRestoreState | null = null;
// Read the update controller for window lifecycle changes
const updateWindow = () => Alpine.store('updates') as { destroy(): void; init(): Promise<void> };
// Read the plugin log controller for window lifecycle changes
const logs = () => Alpine.store('pluginLogs') as import('./plugin-logs').PluginLogs;
// Wait until Alpine has flushed pending reactive updates
const settleReactive = () => new Promise<void>((resolve) => Alpine.nextTick(resolve));
let closeCoordinator: WindowCloseCoordinator;
// Keep archive resumption inert until drop handling is registered
let resumeArchiveDrops = () => {};
const disabledDialogs = new Map<HTMLDialogElement, boolean>();
// Enable or suspend shell interaction while preserving dialog inert state
function windowInteractive(enabled: boolean) {
    startupInteractive = enabled;
    const shell = document.querySelector<HTMLElement>('sd-app-shell');
    if (shell) shell.inert = !enabled;
    if (!enabled) {
        for (const dialog of document.querySelectorAll<HTMLDialogElement>('dialog[open]')) {
            disabledDialogs.set(dialog, dialog.inert);
            dialog.inert = true;
        }
    } else {
        for (const [dialog, inert] of disabledDialogs) if (dialog.isConnected) dialog.inert = inert;
        disabledDialogs.clear();
        resumeArchiveDrops();
    }
}
// Pause window readers, logs and polling before editor retirement
function pauseEditorWindow() {
    app().windowSuspended = true;
    pauseWindowReaders();
    benchmarkProgress().stopWindow();
    logs().stopWindow();
    updateWindow().destroy();
    window.dispatchEvent(new Event('editor-window-closing'));
}
// Create the generation-scoped editor resumption coordinator
function createWindowResumption(generation: number) {
    return new WindowResumeCoordinator({
        // Report whether the application window is suspended
        suspended: () => app().windowSuspended,
        // Restore readers and polling before announcing editor resumption
        recover: async () => {
            resumeWindowReaders();
            await app().resumeWindow();
            benchmarkProgress().startWindow();
            logs().startWindow();
            await updateWindow().init();
            window.dispatchEvent(new Event('editor-window-resumed'));
        },
        // Refresh the workspace and request current device surfaces
        refresh: async () => {
            await app().refresh();
            app().requestSurfaces();
        },
        // Notify the native host that this frontend generation is ready
        ready: async () => {
            if (native) await windowInvoke('frontend_ready', { generation });
        },
        // Suspend readers and stop the application after resumption fails
        suspend: async () => {
            pauseEditorWindow();
            await app().stop();
        },
    });
}
// Create the generation-scoped close and recovery coordinator
function createCloseCoordinator(generation: number, resume: () => Promise<void>) {
    return new WindowCloseCoordinator(generation, {
        // Read the recovery-selected window destination
        destination: () => recovery().destination(),
        // Exit recovery using its reviewed startup choice
        restart: () => recovery().exit(),
        // Prompt for unresolved drafts before closing the editor
        prompt: (unknown) => closeDialog().prompt(unknown, recovery().status.active),
        // Cancel recovery closure and keep the editor open
        dismiss: () => {
            recovery().cancelClose();
            closeDialog().keepEditing();
        },
        interactive: windowInteractive,
        // Confirm the exact close ticket with the native host
        confirm: (ticket) =>
            native
                ? windowInvoke('confirm_window_close', {
                      generation: ticket.generation,
                      actionId: ticket.action_id,
                  })
                : Promise.resolve(),
        pause: pauseEditorWindow,
        // Retire subscriptions and await reactive cleanup before closing
        retire: async () => {
            await logs().retireWindow();
            await app().suspendWindow();
            await app().subscriptionsSettled();
            await settleReactive();
            await drainCloseReleases();
        },
        // Finish the native close ticket or simulate browser window closure
        finish: async (ticket) => {
            if (native) {
                await windowInvoke('finish_window_action', {
                    quit: false,
                    generation: ticket.generation,
                    actionId: ticket.action_id,
                });
            } else {
                const preview = Alpine.store('windowClosePreview') as { closed: boolean };
                preview.closed = true;
                document.querySelector<HTMLElement>('sd-app-shell')!.hidden = true;
            }
        },
        // Abort the exact native window action when closure is cancelled
        abort: (ticket) =>
            native
                ? windowInvoke('abort_window_action', {
                      generation: ticket.generation,
                      actionId: ticket.action_id,
                  })
                : Promise.resolve(),
        resume,
        // Report editor closure failures to the user
        failed: (error) => app().notify('Could not close the editor: ' + String(error), 'error'),
    });
}
// Initialize the workspace, shell and generation-scoped window lifecycle
let startupStage: FrontendStartupStage = 'native_handshake';
let startupLoaderCode: unknown;
async function start() {
    resumeArchiveDrops = await registerArchiveDrops(
        lifetime,
        app,
        () => startupInteractive,
        settleReactive,
        native ? listen : undefined,
    );
    lifetime.ensureActive();
    if (native) {
        nativeWindowState = await lifetime.wait(editorRestoreState());
        // A recreated main document always starts with the saved Device editor state
        if (nativeWindowState.generation > 1) location.hash = '#/editor';
    }
    startupStage = 'workspace_load';
    Loader.config({ sanitize: true, executeScripts: false });
    await lifetime.wait(Promise.all([app().start(), app().loadFavoriteDevice()]));
    lifetime.ensureActive();
    startupStage = 'shell_mount';
    registerUpdates(Alpine, app);
    lifetime.own(() => (Alpine.store('updates') as { destroy(): void }).destroy());
    const shell = document.querySelector<HTMLElement>('sd-app-shell')!;
    shell.inert = true;
    registerPaletteBrowser();
    Alpine.start();
    // Remove startup permission controls during lifetime disposal
    lifetime.own(() => {
        const permissions = document.getElementById('startup-permissions');
        if (permissions) {
            (permissions.querySelector('dialog') as HTMLDialogElement | null)?.close();
            Alpine.destroyTree(permissions);
            permissions.remove();
        }
    });
    // Destroy the shell’s Alpine tree before removing its element
    lifetime.own(() => {
        try {
            Alpine.destroyTree(shell);
        } finally {
            shell.remove();
        }
    });
    // Wait for the shell to load or fail before enabling interaction
    const loaded = new Promise<void>((resolve, reject) => {
        // Remove shell readiness listeners after either outcome
        const clear = () => {
            shell.removeEventListener('loaded', ready);
            shell.removeEventListener('acl:error', failed);
        };
        // Resolve shell readiness and remove its listeners
        const ready = () => {
            clear();
            resolve();
        };
        // Reject shell startup with a sanitized public loader error code
        const failed = (event: Event) => {
            clear();
            startupLoaderCode = (event as CustomEvent<{ error?: { code?: unknown } }>).detail?.error
                ?.code;
            reject(Error('Shell component failed to load'));
        };
        shell.addEventListener('loaded', ready, { once: true });
        shell.addEventListener('acl:error', failed, { once: true });
        lifetime.own(clear);
    });
    // Shell errors can arrive during any of the registration awaits below
    void loaded.catch(() => {});
    await lifetime.wait(Loader.registerErrorBoundary());
    lifetime.ensureActive();
    await lifetime.wait(Loader.registerDynamicLoader());
    lifetime.ensureActive();
    await lifetime.wait(ensureRoute('shell'));
    lifetime.ensureActive();
    await lifetime.wait(Loader.start());
    lifetime.ensureActive();
    await lifetime.wait(loaded);
    lifetime.ensureActive();
    startupStage = 'initial_route';
    await lifetime.wait(initialNavigation);
    startupStage = 'editor_services';
    lifetime.ensureActive();
    // Handle global history and clipboard shortcuts outside editable fields
    const keydown = (event: KeyboardEvent) => {
        if (lifetime.disposed || !startupInteractive) return;
        if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
        const element = event.target as HTMLElement;
        if (element.closest('input,textarea,select,[contenteditable="true"]')) return;
        const key = event.key.toLowerCase();
        if (key === 'z') {
            event.preventDefault();
            swallow(app().history(event.shiftKey ? 'redo' : 'undo'));
        }
        if (key === 'y') {
            event.preventDefault();
            swallow(app().history('redo'));
        }
        if (key === 'c' && app().route === 'editor') {
            event.preventDefault();
            swallow(app().copyBindings());
        }
        if (key === 'v' && app().route === 'editor') {
            event.preventDefault();
            swallow(app().pasteBindings());
        }
    };
    window.addEventListener('keydown', keydown);
    lifetime.own(() => window.removeEventListener('keydown', keydown));
    let favoriteWindowHidden = document.hidden;
    const favoriteVisibility = (event: Event) => {
        const visible =
            event.type === 'visibilitychange'
                ? !document.hidden
                : Boolean((event as CustomEvent<boolean>).detail);
        if (!visible) favoriteWindowHidden = true;
        else if (favoriteWindowHidden) {
            favoriteWindowHidden = false;
            app().beginDeviceSession();
        }
    };
    document.addEventListener('visibilitychange', favoriteVisibility);
    window.addEventListener('editor-window-visibility', favoriteVisibility);
    lifetime.own(() => {
        document.removeEventListener('visibilitychange', favoriteVisibility);
        window.removeEventListener('editor-window-visibility', favoriteVisibility);
    });
    const generation = nativeWindowState?.generation || 1;
    const resumption = createWindowResumption(generation);
    // Resume the editor through its shared lifecycle coordinator
    const resume = () => resumption.resume();
    closeCoordinator = createCloseCoordinator(generation, resume);
    if (native) {
        lifetime.ensureActive();
        await registerNativeWindowEvents(lifetime, app, listen, () => draftQueue.flush(), {
            generation,
            close: closeCoordinator,
            resume,
        });
        lifetime.ensureActive();
    }
    if (!native) {
        // Browser fixtures exercise the same decision/retirement code; only the OS window
        // destruction and creation are simulated. Native qualification remains separate
        let actionId = 0;
        const preview: { closed: boolean; requestClose(): Promise<void>; reopen(): Promise<void> } =
            {
                closed: false,
                // Request a new simulated close ticket for this window generation
                requestClose: () => closeCoordinator.request({ generation, action_id: ++actionId }),
                // Reopen the simulated editor using the same resumption lifecycle
                async reopen() {
                    if (!this.closed) return;
                    windowWork.resume();
                    windowCloseOwners.setClosing(false);
                    await resume();
                    app().beginDeviceSession();
                    app().clipboard = [];
                    app().pendingAction = null;
                    location.hash = '#/editor';
                    shell.hidden = false;
                    this.closed = false;
                    windowInteractive(true);
                },
            };
        Alpine.store('windowClosePreview', preview);
    }
    await lifetime.wait(waitForBootPreview());
    lifetime.ensureActive();
    if (native && generation === 1 && !recovery().status.active) {
        startupStage = 'vault_unlock';
        suspendBootSplash();
        const dialog = document.querySelector<HTMLDialogElement>('#startup-vault');
        if (dialog) {
            await lifetime.wait(
                runStartupVault(
                    {
                        status: () => bridge.command('vault_status'),
                        unlockPassword: (password) =>
                            bridge.command('vault_unlock_password', { password }),
                    },
                    dialog,
                    lifetime.signal,
                ),
            );
            lifetime.ensureActive();
        }
    }
    finishBootSplash();
    startupInteractive = true;
    shell.inert = false;
    window.dispatchEvent(new Event('startup-permissions-ready'));
    resumeArchiveDrops();
}
// Display startup failure only after disposing abandoned resources
start().catch((error) => {
    const abandoned = lifetime.disposed;
    lifetime.dispose();
    if (abandoned) return;
    // Keep logs available while native resource retirement prevents immediate retry
    failBootSplash({
        logReady: reportStartupFailure(startupStage, error, startupLoaderCode),
        retryReady: lifetime.settled().then(() => !windowWork.blockedRetirements.length),
    });
});
