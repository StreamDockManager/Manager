import * as fileDialogs from './window-dialogs';
import { listen } from '@tauri-apps/api/event';
import { windowWork } from './window-work';
import { windowCloseOwners } from './window-close-owners';
import { trackCloseOperations, trackCloseRelease } from './close-releases';
import type AlpineType from '@alpinejs/csp';
import { native, type Bridge } from './bridge';
import type { JsonValue } from '../generated/runtime';
let openPanel: (
    id: string,
    action: string | null,
    settings: JsonValue,
    opener?: HTMLElement,
    setup?: boolean,
    signal?: AbortSignal,
    resources?: Partial<Record<string, string>>,
    address?: JsonValue,
) => Promise<JsonValue | null>;
// Edit the plugin settings
export function editPluginSettings(
    id: string,
    action: string | null,
    settings: JsonValue,
    opener?: HTMLElement,
    setup?: boolean,
    signal?: AbortSignal,
    resources?: Partial<Record<string, string>>,
    address?: JsonValue,
) {
    return windowWork.run('plugin settings panel', () =>
        openPanel(id, action, settings, opener, setup, signal, resources, address),
    );
}
// Register the plugin settings
export function registerPluginSettings(Alpine: typeof AlpineType, bridge: Bridge) {
    let resolve: ((value: JsonValue | null) => void) | undefined;
    let pluginId: string,
        pluginScope = false;
    let panelAction: string | null = null;
    let requestedVaultLocation = false;
    let resourceTarget: Partial<Record<string, string>> | undefined;
    let saved: (() => void) | undefined;
    const cancellations = new Map<string, Promise<void>>();
    let panelGeneration = 0;
    let disposed = false;
    let returnFocus: HTMLElement | undefined;
    // Restore the focus
    const restoreFocus = () => {
        const target = returnFocus;
        returnFocus = undefined;
        if (!windowCloseOwners.closing && target?.isConnected)
            target.focus({ preventScroll: true });
    };
    const store = {
        open: false,
        definition: false,
        immediate: false,
        nativeWindow: false,
        binding: false,
        pathPickerModes: {} as Partial<Partial<Record<string, string>>>,
        browsing: false,
        url: 'about:blank',
        token: '',
        error: '',
        saving: false,
        location: false,
        locating: false,
        locationPreview: null as {
            latitude: number;
            longitude: number;
            accuracy_m: number;
            timestamp: number;
        } | null,
        locationGeneration: 0,
        // Request a location preview and discard results from obsolete settings sessions
        async locate() {
            if (!this.open || !this.location || this.locating) return;
            const token = this.token;
            const generation = ++this.locationGeneration;
            this.locating = true;
            this.locationPreview = null;
            this.error = '';
            try {
                const fix = await bridge.command('request_settings_location', { token });
                if (this.open && this.token === token && generation === this.locationGeneration)
                    this.locationPreview = fix;
            } catch (error) {
                if (this.open && this.token === token && generation === this.locationGeneration)
                    this.error = String(error);
            } finally {
                if (this.token === token && generation === this.locationGeneration)
                    this.locating = false;
            }
        },
        // Format the proposed coordinates and approximate accuracy for review
        locationDescription() {
            const fix = this.locationPreview;
            return fix
                ? `${fix.latitude.toFixed(5)}, ${fix.longitude.toFixed(5)} · accuracy approximately ${Math.round(fix.accuracy_m)} m`
                : '';
        },
        // Send the approved location to the currently open settings inspector
        useLocation() {
            if (!this.locationPreview || !this.open) return;
            this.sendLocation({ ...this.locationPreview, source: 'device' });
            this.locationPreview = null;
        },
        sendLocation(location: {
            latitude: number;
            longitude: number;
            source: string;
            label?: string;
            accuracy_m?: number;
            timestamp?: number;
        }) {
            if (!this.open || !this.location) return;
            const frame = document.querySelector<HTMLIFrameElement>(
                '.plugin-settings-dialog iframe',
            );
            frame?.contentWindow?.postMessage(
                {
                    event: 'stream-dock-manager-location-selected',
                    context: `settings-draft:${this.token}`,
                    location: { ...location },
                },
                new URL(this.url).origin,
            );
            this.locationPreview = null;
        },
        // Cancel the location
        async cancelLocation() {
            this.locationGeneration++;
            this.locationPreview = null;
            this.locating = false;
            const token = this.token;
            if (token)
                await trackCloseRelease(`settings-location:${token}`, () =>
                    bridge.command('cancel_settings_location', { token }),
                );
        },
        // Browse available items for plugin settings
        async browse(key: string) {
            if (!this.open || this.browsing || this.saving || !key) return;
            const inlineMode = Object.hasOwn(this.pathPickerModes, key)
                ? this.pathPickerModes[key]
                : undefined;
            if (!inlineMode || !['file', 'folder', 'save'].includes(inlineMode)) return;
            if (!native) {
                this.error = 'Path choosers are available in the desktop app.';
                return;
            }
            const token = this.token;
            const mode = inlineMode;
            this.browsing = true;
            this.error = '';
            try {
                const { open, save } = fileDialogs;
                if (!this.open || this.token !== token) return;
                const value =
                    mode === 'save'
                        ? await save({ title: 'Choose output file' })
                        : await open({ multiple: false, directory: mode === 'folder' });
                if (this.open && this.token === token && typeof value === 'string')
                    await bridge.command('select_settings_path', { token, key, value });
            } catch (error) {
                if (this.open && this.token === token) this.error = String(error);
            } finally {
                if (this.token === token) this.browsing = false;
            }
        },
        // Save pending changes through the plugin settings workflow
        async save() {
            if (!this.open || this.saving || this.browsing) return;
            if (this.immediate) {
                await this.cancel();
                return;
            }
            const token = this.token;
            const commitId = pluginScope ? pluginId : null;
            const complete = resolve;
            this.saving = true;
            try {
                await this.cancelLocation();
                if (!this.open || this.token !== token) return;
                const frame = document.querySelector<HTMLIFrameElement>(
                    '.plugin-settings-dialog iframe',
                );
                if (!frame?.contentWindow) throw Error('Settings panel is loading');
                const origin = new URL(this.url).origin;
                {
                    // Resolve the plugin settings operation when its asynchronous work finishes
                    const acknowledged = new Promise<void>((resolve, reject) => {
                        // Run the deferred plugin settings update after its scheduled delay
                        const timeout = setTimeout(() => {
                            saved = undefined;
                            reject(Error('Check the settings form before applying.'));
                        }, 5000);
                        // Complete the deferred plugin settings work using the current operation state
                        saved = () => {
                            clearTimeout(timeout);
                            resolve();
                        };
                    });
                    frame.contentWindow.postMessage(
                        { event: 'stream-dock-manager-save-settings' },
                        origin,
                    );
                    await acknowledged;
                }
                if (!this.open || this.token !== token) return;
                const value = (await bridge.command('settings_panel_value', { token })) as {
                    settings: JsonValue;
                    resources: Partial<Record<string, string>>;
                };
                if (!value || typeof value !== 'object' || !('settings' in value))
                    throw Error('Invalid settings panel response');
                if (!this.open || this.token !== token) return;
                await bridge.command('close_settings_panel', { token, id: commitId, apply: true });
                if (this.token !== token) return;
                this.open = false;
                document.querySelector<HTMLDialogElement>('#plugin-settings')?.close();
                this.url = 'about:blank';
                frame.src = 'about:blank';
                restoreFocus();
                if (resourceTarget) {
                    for (const key of Object.keys(resourceTarget)) delete resourceTarget[key];
                    Object.assign(resourceTarget, value.resources);
                }
                complete?.(value.settings);
                if (resolve === complete) resolve = undefined;
            } catch (error) {
                if (this.open && this.token === token) this.error = String(error);
            } finally {
                if (this.token === token) this.saving = false;
            }
        },
        // Cancel the active plugin settings operation
        async cancel(keepGeneration = false) {
            const token = this.token;
            const pending = cancellations.get(token);
            if (pending) return pending;
            const operation = this.cancelPanel(keepGeneration);
            cancellations.set(token, operation);
            try {
                await operation;
            } finally {
                if (cancellations.get(token) === operation) cancellations.delete(token);
            }
        },
        // Cancel the panel
        async cancelPanel(keepGeneration = false) {
            if (keepGeneration !== true) panelGeneration++;
            const token = this.token;
            const complete = resolve;
            let committed: JsonValue | null = null;
            if (this.immediate && token) {
                try {
                    const value = (await bridge.command('settings_panel_value', {
                        token,
                        finish: true,
                    })) as { settings: JsonValue; resources: Partial<Record<string, string>> };
                    committed = value.settings;
                    if (resourceTarget) {
                        for (const key of Object.keys(resourceTarget)) delete resourceTarget[key];
                        Object.assign(resourceTarget, value.resources);
                    }
                } catch (error) {
                    this.error = String(error);
                    if (String(error).includes('still being prepared')) return;
                    // Revoked/deleted inspectors must remain closable. No draft is applied
                }
            }
            resolve = undefined;
            const location = this.cancelLocation();
            this.open = false;
            document.querySelector<HTMLDialogElement>('#plugin-settings')?.close();
            saved?.();
            saved = undefined;
            this.saving = false;
            this.url = 'about:blank';
            const frame = document.querySelector<HTMLIFrameElement>(
                '.plugin-settings-dialog iframe',
            );
            if (frame) frame.src = 'about:blank';
            restoreFocus();
            complete?.(committed);
            const panel = token
                ? trackCloseRelease(`settings-panel:${token}`, () =>
                      bridge.command('close_settings_panel', { token, id: null }),
                  )
                : Promise.resolve();
            await Promise.all([location, panel]);
            if (this.token === token) this.token = '';
        },
    };
    Alpine.store('pluginSettings', store);
    const pendingNativeClose = new Set<string>();
    let nativeCloseError = '';
    const nativeClosed = native
        ? // Complete the deferred plugin settings work using the current operation state
          listen<string>('vendor-inspector-closed', (event) => {
              const state = Alpine.store('pluginSettings') as typeof store;
              if (state.open && state.nativeWindow && state.token === event.payload)
                  void state.cancel().catch(() => {});
              else if (!state.open && !disposed) {
                  pendingNativeClose.add(event.payload);
                  if (pendingNativeClose.size > 32)
                      pendingNativeClose.delete(pendingNativeClose.values().next().value!);
              }
              // Handle a failed plugin settings operation without retaining stale work
          }).catch((error) => {
              nativeCloseError = `Inspector close notifications are unavailable: ${String(error)}`;
          })
        : Promise.resolve(() => {});
    const settleSave = trackCloseOperations(store, ['save']);
    const unregisterCloseOwner = windowCloseOwners.register({
        name: 'Embedded plugin settings (unsaved state is unavailable)',
        // Return a snapshot of the current plugin settings state
        state: () => ((Alpine.store('pluginSettings') as typeof store).open ? 'unknown' : 'clean'),
        // Discard pending plugin settings edits without saving them
        discard: async () => {
            await settleSave();
            const state = Alpine.store('pluginSettings') as typeof store;
            await state.cancel();
            if (state.open)
                throw Error(
                    state.error || 'Plugin changes are still being saved. Try closing again.',
                );
        },
        // Release resources retained by the plugin settings operation
        release: async () => {
            await settleSave();
            const state = Alpine.store('pluginSettings') as typeof store;
            if (state.open || state.token) await state.cancel();
            if (state.open)
                throw Error(
                    state.error || 'Plugin changes are still being saved. Try closing again.',
                );
        },
    });
    // Respond to plugins-removed events in the plugin settings workflow
    window.addEventListener('plugins-removed', (event) => {
        const state = Alpine.store('pluginSettings') as typeof store;
        if (state.open && (event as CustomEvent<string[]>).detail.includes(pluginId))
            void state.cancel().catch(() => {});
    });
    // Respond to message events in the plugin settings workflow
    window.addEventListener('message', (event) => {
        const state = Alpine.store('pluginSettings') as typeof store;
        const frame = document.querySelector<HTMLIFrameElement>('.plugin-settings-dialog iframe');
        if (
            !state.open ||
            event.source !== frame?.contentWindow ||
            event.origin !== new URL(state.url).origin ||
            event.data?.context !== `settings-draft:${state.token}`
        )
            return;
        if (event.data?.event === 'stream-dock-manager-request-vault-location') {
            if (!state.location || state.nativeWindow || !panelAction || requestedVaultLocation)
                return;
            requestedVaultLocation = true;
            const owner = pluginId;
            const action = panelAction;
            const token = state.token;
            const target = frame.contentWindow;
            const origin = event.origin;
            // Resolve only a verified, already-approved location slot for the current action
            void bridge
                .command('vault_access')
                .then((grants) => {
                    const grant = grants.find(
                        (item) =>
                            item.plugin_id === owner &&
                            item.action === action &&
                            item.field === 'location' &&
                            item.slot_id === 'location' &&
                            !item.direct_secret &&
                            item.entry.kind === 'location',
                    );
                    return grant ? { label: grant.entry.label } : null;
                })
                .catch(() => null)
                .then((location) => {
                    if (!state.open || state.token !== token || target !== frame.contentWindow)
                        return;
                    target?.postMessage(
                        {
                            event: 'stream-dock-manager-vault-location',
                            context: `settings-draft:${token}`,
                            location,
                        },
                        origin,
                    );
                });
            return;
        }
        if (event.data?.event === 'stream-dock-manager-cancel-settings') {
            void state.cancel().catch(() => {});
            return;
        }
        if (event.data?.event === 'stream-dock-manager-choose-settings-path') {
            const key = event.data.key;
            if (typeof key === 'string' && Object.hasOwn(state.pathPickerModes, key))
                void state.browse(key);
            return;
        }
        if (event.data?.event !== 'stream-dock-manager-settings-saved') return;
        saved?.();
        saved = undefined;
    });
    // Complete the deferred plugin settings work using the current operation state
    openPanel = async (id, action, settings, opener, setup, signal, resources, address) => {
        if (disposed || signal?.aborted) return null;
        const state = Alpine.store('pluginSettings') as typeof store;
        const focusTarget = opener || (document.activeElement as HTMLElement | undefined);
        const generation = ++panelGeneration;
        if (state.open) {
            await state.cancel(true);
            if (state.open)
                throw Error(
                    state.error ||
                        'Finish pending plugin changes before opening another inspector.',
                );
        }
        if (generation !== panelGeneration || signal?.aborted) return null;
        await nativeClosed;
        if (disposed || generation !== panelGeneration || signal?.aborted) return null;
        if (nativeCloseError) throw Error(nativeCloseError);
        const panel = await bridge.command('open_settings_panel', {
            request: {
                id,
                action,
                settings,
                resources: resources ?? null,
                address: address ?? null,
                setup: setup ?? null,
            },
        });
        await nativeClosed;
        if (nativeCloseError || disposed || generation !== panelGeneration || signal?.aborted) {
            await trackCloseRelease(`settings-panel:${panel.token}`, () =>
                bridge.command('close_settings_panel', { token: panel.token, id: null }),
            );
            pendingNativeClose.delete(panel.token);
            if (nativeCloseError) throw Error(nativeCloseError);
            return null;
        }
        state.immediate = panel.immediate;
        state.nativeWindow = panel.native_window;
        resourceTarget = resources;
        pluginId = id;
        panelAction = action;
        requestedVaultLocation = false;
        pluginScope = action === null || settings === null;
        state.definition = action !== null && settings === null;
        state.binding = action !== null && settings !== null;
        state.location = panel.location === true;
        state.locationPreview = null;
        state.locating = false;
        state.pathPickerModes = panel.path_picker_modes ?? {};
        state.browsing = false;
        state.token = panel.token;
        state.url = panel.native_window ? 'about:blank' : panel.url;
        const frame = document.querySelector<HTMLIFrameElement>('.plugin-settings-dialog iframe');
        if (frame) frame.src = state.url;
        state.error = '';
        state.open = true;
        returnFocus = focusTarget;
        document.querySelector<HTMLDialogElement>('#plugin-settings')?.showModal();
        // Resolve the plugin settings operation when its asynchronous work finishes
        return new Promise((done) => {
            // Cancel the active plugin settings operation
            const cancel = () => {
                if (state.token === panel.token && state.open) void state.cancel().catch(() => {});
            };
            signal?.addEventListener('abort', cancel, { once: true });
            // Complete the deferred plugin settings work using the current operation state
            resolve = (value) => {
                signal?.removeEventListener('abort', cancel);
                pendingNativeClose.delete(panel.token);
                done(value);
            };
            if (pendingNativeClose.delete(panel.token)) queueMicrotask(cancel);
        });
    };
    // Complete the deferred plugin settings work using the current operation state
    return () => {
        disposed = true;
        panelGeneration++;
        pendingNativeClose.clear();
        unregisterCloseOwner();
        void nativeClosed
            .then((unlisten) => unlisten?.())
            // Handle a failed plugin settings operation without retaining stale work
            .catch((error) => {
                nativeCloseError = `Inspector close notifications could not be released: ${String(error)}`;
            });
    };
}
