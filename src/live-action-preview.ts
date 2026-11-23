import { app } from './route-context';
import { cloneJson } from './json-clone';
import { cancelDockPreview, dockPreviewReason } from './dock-preview';
import { windowCloseOwners } from './window-close-owners';
import { windowWork } from './window-work';
import type { Binding } from './types';

export interface LivePreviewOptions {
    priority?: number;
    previewState?: number | null;
}
interface Entry {
    owner: string;
    address: string;
    deviceId: string;
    controlId: string;
    surfaceId: string;
    hasSurface: boolean;
    scope: string;
    revision: number;
    binding: Binding;
    priority: number;
    previewState: number | null;
    sequence: number;
    signature: string;
}

// Visual-only ownership: an appearance dialog temporarily overlays the inspector
// Neither owner changes the saved binding or executes/activates a draft action
const entries = new Map<string, Entry>();
let sequence = 0;
let generation = 0;
let selected: Entry | undefined;
let timer: ReturnType<typeof setTimeout> | undefined;
let heartbeat: ReturnType<typeof setInterval> | undefined;
let rendering = false;
let renderAgain = false;
let hardwareQueue = Promise.resolve();
let hardwareToken = '';
let hardwareSignature = '';
let hardwareUpdatedAt = 0;
let listenerInstalled = false;
let palettePreviewActive = false;
let retryRender = true;

// Only known time-independent inputs may park pixel rendering. Unknown actions,
// assets/compositions and feedback retain the established one-second fallback.
function staticPixels(entry: Entry) {
    const binding = entry.binding;
    return (
        ['none', 'hotkey', 'open_url', 'open_path', 'command', 'delay'].includes(
            binding.action.kind,
        ) &&
        !binding.asset &&
        !binding.composition &&
        (!binding.feedback || binding.feedback === 'off')
    );
}
function invalidatePixels(event: Event) {
    select();
    if (!selected) return;
    const device = (event as CustomEvent<{ device_id?: string | null }>).detail?.device_id;
    if (device && device !== selected.deviceId) return;
    const kind = (event as CustomEvent<{ kind?: string }>).detail?.kind;
    // Feedback changes pixels, not ownership. Publish the coherent in-flight
    // frame, then sample again, rather than starving under faster producers.
    if (kind !== 'plugin-feedback' && kind !== 'live-feedback') generation++;
    retryRender = true;
    if (rendering) renderAgain = true;
    else schedule();
}

// Palette browsing owns device-wide output. Keep inspector drafts registered but
// retire their hardware lease and image override until the palette owner dismisses.
export function suspendLiveActionForPalette(active: boolean) {
    palettePreviewActive = active;
    select();
    const settled = queueHardware();
    if (!active && entries.size) schedule();
    return settled;
}

// Require the preview owner to match the visible editor device, binding and revision
function current(entry: Entry) {
    const state = app();
    return (
        !palettePreviewActive &&
        state.route === 'editor' &&
        !state.windowSuspended &&
        !windowCloseOwners.closing &&
        !windowWork.quiescing &&
        !document.hidden &&
        entry.deviceId === state.deviceId &&
        entry.address === state.bindingAddress() &&
        entry.revision === state.configurationRevision
    );
}
// Choose the highest-priority current owner with the newest request as tiebreaker
function choose() {
    return [...entries.values()]
        .filter(current)
        .sort((a, b) => b.priority - a.priority || b.sequence - a.sequence)[0];
}
// Select the eligible live-preview owner and invalidate previous render work
function select() {
    const next = choose();
    if (selected === next) return;
    selected = next;
    generation++;
    retryRender = true;
    if (next) {
        const previous = app().liveActionPreview;
        app().liveActionPreview = {
            address: next.address,
            deviceId: next.deviceId,
            controlId: next.controlId,
            surfaceId: next.surfaceId,
            revision: next.revision,
            binding: next.binding,
            image: previous?.address === next.address ? previous.image : '',
        };
    } else app().liveActionPreview = null;
}
// Serialize current preview output and discard stale owner generations
function queueHardware(force = false) {
    const entry = selected;
    const wanted = generation;
    hardwareQueue = hardwareQueue
        .catch(() => {})
        // Serialize preview replacement and discard superseded hardware requests
        .then(async () => {
            if (wanted !== generation) return;
            if (!entry || !entry.hasSurface || !current(entry) || dockPreviewReason()) {
                const token = hardwareToken;
                hardwareToken = '';
                hardwareSignature = '';
                if (token) await cancelDockPreview(token);
                return;
            }
            const signature = JSON.stringify([
                entry.deviceId,
                entry.address,
                entry.revision,
                entry.binding,
                entry.previewState,
            ]);
            if (!force && signature === hardwareSignature && Date.now() - hardwareUpdatedAt < 2000)
                return;
            try {
                const token = await app().bridge.command('preview_on_dock', {
                    deviceId: entry.deviceId,
                    expectedRevision: entry.revision,
                    draft: {
                        bindings: { [entry.controlId]: entry.binding },
                        background: null,
                        scope: entry.scope,
                        ...(entry.previewState === null
                            ? {}
                            : { preview_states: { [entry.controlId]: entry.previewState } }),
                    },
                });
                if (wanted !== generation || !current(entry)) {
                    await cancelDockPreview(token);
                    return;
                }
                const previous = hardwareToken;
                hardwareToken = token;
                hardwareSignature = signature;
                hardwareUpdatedAt = Date.now();
                if (previous && previous !== token) await cancelDockPreview(previous);
            } catch {
                // Incomplete drafts, disconnects and sleep must never keep a stale override
                const token = hardwareToken;
                hardwareToken = '';
                hardwareSignature = '';
                if (token) await cancelDockPreview(token);
            }
        });
    void hardwareQueue.catch(() => {});
    return hardwareQueue;
}
// Render the selected live preview without overlapping render passes
async function render() {
    select();
    void queueHardware();
    if (rendering) {
        renderAgain = true;
        return;
    }
    const entry = selected;
    if (!entry || !entry.hasSurface) return;
    const wanted = generation;
    rendering = true;
    try {
        const result = await app().bridge.command('preview_binding_details', {
            deviceId: entry.deviceId,
            controlId: entry.controlId,
            binding: entry.binding,
            previewState: entry.previewState,
        });
        if (wanted === generation && current(entry) && app().liveActionPreview) {
            app().liveActionPreview = { ...app().liveActionPreview!, image: result.image };
            retryRender = false;
        }
    } catch {
        // Keep the last valid visual while a field contains incomplete input
    } finally {
        rendering = false;
        if (renderAgain) {
            renderAgain = false;
            schedule();
        }
    }
}
// Coalesce live-preview refreshes behind a single pending timer
function schedule() {
    select();
    if (!timer)
        timer = setTimeout(() => {
            timer = undefined;
            void render();
        }, 50);
}
// Refresh preview ownership and resume rendering when visible
function visibilityChanged() {
    select();
    void queueHardware();
    if (!document.hidden) schedule();
}
// Register a draft action preview for the selected device surface
export function setLiveActionPreview(
    owner: string,
    binding: Binding,
    options: LivePreviewOptions = {},
) {
    const state = app();
    const source = state.control;
    const feedback = source?.feedback_surface;
    // Match saved surface precedence: touch-panel buttons and explicitly bound
    // displays are independent, even while their paired encoder is being edited.
    const independent =
        feedback &&
        (state.device?.manifest.capabilities.touch_panel?.button_controls.includes(feedback) ||
            state.page?.bindings?.[feedback] ||
            state.snapshot.binding_overrides?.[state.deviceId]?.[feedback]);
    const surfaceId = (independent ? undefined : feedback) || state.controlId;
    const surface = state.device?.manifest.controls.find((control) => control.id === surfaceId);
    if (!source) {
        void clearLiveActionPreview(owner);
        return;
    }
    const signature = JSON.stringify([
        state.bindingAddress(),
        state.deviceId,
        state.controlId,
        surfaceId,
        state.configurationRevision,
        state.profile?.id,
        state.page?.id,
        binding,
        options.priority || 0,
        options.previewState ?? null,
    ]);
    const previous = entries.get(owner);
    if (previous === selected && previous?.signature === signature && current(previous)) return;
    entries.set(owner, {
        owner,
        address: state.bindingAddress(),
        deviceId: state.deviceId,
        controlId: state.controlId,
        surfaceId,
        hasSurface: Boolean(surface?.image_width && surface.image_slot != null),
        scope: `${state.profile?.id}:${state.page?.id}`,
        revision: state.configurationRevision,
        binding: cloneJson(binding),
        priority: options.priority || 0,
        previewState: options.previewState ?? null,
        sequence: ++sequence,
        signature,
    });
    if (!listenerInstalled) {
        document.addEventListener('visibilitychange', visibilityChanged);
        if (typeof window !== 'undefined') {
            window.addEventListener('live-preview-invalidated', invalidatePixels);
            window.addEventListener('plugin-theme-metadata', invalidatePixels);
        }
        listenerInstalled = true;
    }
    if (!heartbeat)
        heartbeat = setInterval(() => {
            select();
            void queueHardware();
            // Lease renewal does not require static pixels to be rendered again.
            if (selected && (retryRender || !staticPixels(selected))) schedule();
        }, 1000);
    schedule();
}
// Remove a preview owner and release unused render scheduling
export function clearLiveActionPreview(owner: string): Promise<void> {
    entries.delete(owner);
    select();
    if (!entries.size) {
        clearTimeout(timer);
        timer = undefined;
        clearInterval(heartbeat);
        heartbeat = undefined;
        if (listenerInstalled) {
            document.removeEventListener('visibilitychange', visibilityChanged);
            if (typeof window !== 'undefined') {
                window.removeEventListener('live-preview-invalidated', invalidatePixels);
                window.removeEventListener('plugin-theme-metadata', invalidatePixels);
            }
        }
        listenerInstalled = false;
    } else schedule();
    return queueHardware();
}
