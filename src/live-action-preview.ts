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

// Require the preview owner to match the visible editor device, binding and revision
function current(entry: Entry) {
    const state = app();
    return (
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
        if (wanted === generation && current(entry) && app().liveActionPreview)
            app().liveActionPreview = { ...app().liveActionPreview!, image: result.image };
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
    const surfaceId = source?.feedback_surface || state.controlId;
    const surface = state.device?.manifest.controls.find((control) => control.id === surfaceId);
    if (!source) {
        void clearLiveActionPreview(owner);
        return;
    }
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
    });
    if (!listenerInstalled) {
        document.addEventListener('visibilitychange', visibilityChanged);
        listenerInstalled = true;
    }
    if (!heartbeat)
        heartbeat = setInterval(() => {
            // Re-render dynamic built-in/plugin feedback and renew the short hardware lease
            schedule();
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
        if (listenerInstalled) document.removeEventListener('visibilitychange', visibilityChanged);
        listenerInstalled = false;
    } else schedule();
    return queueHardware();
}
