import { isKeyboardNavigationControl } from './keyboard-navigation';
import { cloneJson } from './json-clone';
import type { Bridge, RetireSubscription } from './bridge';
import { trackCloseRelease } from './close-releases';
import { defaultMedia, defaultDeviceOutput } from './bridge';
import { detachDraftMedia } from './media-removal';
import { releaseDeletedMedia } from './media-leases';
import type {
    Snapshot,
    RuntimeEvent,
    Device,
    Profile,
    Control,
    Binding,
    Action,
    Manifest,
} from './types';
import type { Edit, Draft, PreviewDelta } from '../generated/runtime';
import { ToastState, type ToastLevel } from './toasts';
import { bindingCompatibility, hasBindingContent } from './bindings';
import { DEFAULT_BACKGROUND } from './background-color';
import { reconcile } from './reconcile';
import { workspaceSnapshot } from './workspace-stream';
import { recordRuntimeEvent, type RuntimeEventEntry } from './runtime-events';
import type { DevicePowerStatus } from '../generated/runtime';
import type { WorkspaceBatch } from '../generated/runtime';
import { ownWindowMethods, windowWork, type WindowFence } from './window-work';

// Bound retained bootstrap events independently of the native stream. The byte
// charge covers UTF-16 string storage plus fixed event/object overhead; it is an
// admission estimate, not a JavaScript heap measurement
export const STARTUP_EVENT_LIMIT = 512;
export const STARTUP_EVENT_BYTES = 1024 * 1024;
// Estimate startup event size before admitting it to the bounded buffer
function startupEventBytes(event: RuntimeEvent) {
    const input = event.control?.input;
    return (
        256 +
        2 *
            (event.kind.length +
                (event.device_id?.length || 0) +
                (event.message?.length || 0) +
                (event.control?.control_id.length || 0) +
                (input?.kind === 'touch' ? input.phase.length : 0) +
                (input?.kind === 'swipe' ? input.direction.length : 0))
    );
}
let nextSubscriptionReleaseId = 0;

export class AppState extends ToastState {
    builtinCapabilities: Partial<Record<string, string | null>> = {};
    models: Manifest[] = [];
    snapshot: Snapshot = {
        revision: 0,
        configuration_revision: 0,
        devices: [],
        profiles: [],
        active_profile: '',
        plugins: [],
        sessions: {},

        saved_macros: [],

        rules: [],
        history: { undo: [], redo: [] },
    };
    selectedControls: string[] = [];
    clipboard: (Binding | null)[] = [];
    draftStore: Partial<Record<string, Draft>> = {};
    deletedMedia: Record<string, boolean> = {};
    flushDrafts: ((fence?: WindowFence) => Promise<void>) | undefined;
    windowSuspended = false;
    bindingCleared: ((address: string) => void) | undefined;
    dirtyBindingAddress = '';
    liveActionPreview: {
        address: string;
        deviceId: string;
        controlId: string;
        surfaceId: string;
        revision: number;
        binding: Binding;
        image: string;
    } | null = null;
    // Select a live preview only while its device, revision and binding address still match
    private livePreviewFor(controlId: string) {
        const preview = this.liveActionPreview;
        return preview &&
            preview.deviceId === this.deviceId &&
            preview.revision === this.configurationRevision &&
            preview.address === this.bindingAddress(preview.controlId) &&
            (preview.controlId === controlId || preview.surfaceId === controlId)
            ? preview
            : null;
    }
    // Prefer the live inspector image over the last streamed control surface
    previewSurface(controlId: string) {
        return this.livePreviewFor(controlId)?.image || this.surfaces[controlId];
    }
    // Prefer the live inspector binding over the saved display binding
    previewBinding(controlId: string) {
        return this.livePreviewFor(controlId)?.binding || this.displayedBinding(controlId);
    }

    pendingControls: Record<string, number> = {};
    private blockingCommands = 0;
    private bindingWrites = Promise.resolve();
    // Report whether an operation is pending for this control's binding address
    controlPending(controlId: string) {
        return Boolean(this.pendingControls[this.bindingAddress(controlId)]);
    }
    // Track pending ownership for all affected controls until the operation settles
    private async withControls<T>(ids: string[], operation: () => Promise<T>): Promise<T> {
        const addresses = [...new Set(ids.map((id) => this.bindingAddress(id)))];
        for (const address of addresses)
            this.pendingControls[address] = (this.pendingControls[address] || 0) + 1;
        try {
            return await operation();
        } finally {
            for (const address of addresses) {
                if (--this.pendingControls[address] <= 0) delete this.pendingControls[address];
            }
        }
    }
    private stopSurfaces?: RetireSubscription;
    private surfaceContext = '';
    private surfaceBindingRevision = 0;
    private previewSequence = 0;
    private previewGeneration = '';
    private surfaceConfigurationRevision = 0;
    private surfaceStarting = false;
    pendingAction: Action | null = null;
    pendingActionTitle = '';
    pluginListFilters = {
        query: '',
        statusFilter: 'all',
        controlFilter: 'all',
        page: 1,
        pageSize: '6',
    };
    removingSimulatorId = '';
    // Derive simulators from the current state state
    get simulators() {
        const devices = new Map(
            this.snapshot.devices
                .filter((device) => device.simulated)
                .map((device) => [
                    device.id,
                    { id: device.id, name: device.name, connected: true },
                ]),
        );
        const saved = new Set([
            ...Object.keys(this.snapshot.sessions),
            ...this.snapshot.rules.map((rule) => rule.device_key),
        ]);
        for (const key of Object.keys(this.draftStore)) {
            try {
                saved.add(JSON.parse(key)[0]);
            } catch {}
        }
        for (const id of saved) {
            if (typeof id === 'string' && id.startsWith('simulator:') && !devices.has(id)) {
                const model = this.models.find((model) => model.model_id === id.split(':')[1]);
                devices.set(id, {
                    id,
                    name: `Saved ${model?.name || 'N4 Pro'} simulator (${id.split(':').at(-1)!.slice(0, 8)})`,
                    connected: false,
                });
            }
        }
        return [...devices.values()];
    }
    // Trigger the control
    async triggerControl(controlId = this.controlId, rotation?: number, swipe?: string) {
        if (!this.device || this.effectiveDeviceOutput.sleeping) return;
        const control = this.device.manifest.controls.find((control) => control.id === controlId);
        if (!control) return;
        if (this.controlPending(controlId)) return;
        await this.withControls([controlId], () =>
            this.command(
                'trigger_control',
                {
                    deviceId: this.deviceId,
                    event: {
                        control_id: controlId,
                        input:
                            swipe !== undefined
                                ? { kind: 'swipe', direction: swipe }
                                : rotation !== undefined
                                  ? { kind: 'rotate', delta: rotation }
                                  : control.kind === 'touch'
                                    ? {
                                          kind: 'touch',
                                          phase: 'end',
                                          x: Math.floor((control.image_width || 0) / 2),
                                          y: Math.floor((control.image_height || 0) / 2),
                                      }
                                    : { kind: 'pulse' },
                    },
                },
                false,
            ),
        );
    }
    // Trigger the simulator
    async triggerSimulator(controlId = this.controlId, rotation?: number) {
        if (this.simulated) await this.triggerControl(controlId, rotation);
    }
    surfaces: Record<string, string> = {};
    triggerDescriptions: Partial<Record<string, Partial<Record<string, string>>>> = {};
    background: string | null = null;
    surfaceGeneration = 0;

    private surfaceWanted = false;
    private visibilityHandler?: () => void;
    // Pause the surfaces
    pauseSurfaces() {
        this.retire(this.stopSurfaces, 'surfaces');
        this.stopSurfaces = undefined;
        this.surfaceContext = '';
        this.surfaceGeneration++;
    }
    // Request the surfaces
    requestSurfaces() {
        if (this.windowSuspended || windowWork.quiescing) return;
        if (this.bridge.surfaceRendering === 'stream') void this.streamSurfaces();
    }
    // Fingerprint the device, profile, bindings and background that determine surface rendering
    private previewContext() {
        const profile = this.profile;
        return JSON.stringify([
            this.deviceId,
            profile?.id,
            this.page?.id,
            this.surfaceBindingRevision,
            profile?.background_asset ? [profile.background_asset, profile.background_media] : null,
            (this.device?.manifest.controls || [])
                .filter((control) => control.image_width)
                // Convert each entry into the representation required by state
                .map((control) => {
                    const binding = this.displayedBinding(control.id);
                    return binding?.asset
                        ? [control.id, binding.asset, binding.media]
                        : [control.id];
                }),
        ]);
    }
    // Start or pause surface streaming according to editor visibility and window lifetime
    async streamSurfaces() {
        if (this.bridge.surfaceRendering !== 'stream') return;
        if (this.windowSuspended || windowWork.quiescing) return;
        if (
            this.stopped ||
            this.route !== 'editor' ||
            (typeof document !== 'undefined' && document.hidden)
        ) {
            this.pauseSurfaces();
            return;
        }
        if (this.surfaceStarting) {
            this.surfaceWanted = true;
            return;
        }
        const context = this.previewContext();
        if (!this.deviceId || (this.stopSurfaces && this.surfaceContext === context)) return;
        this.pauseSurfaces();
        this.surfaceContext = context;
        this.surfaceStarting = true;
        this.surfaceWanted = false;
        this.previewSequence = 0;
        this.previewGeneration = '';
        const generation = this.surfaceGeneration;
        try {
            await this.trackSubscription(
                // Complete the deferred state work using the current operation state
                this.bridge.subscribeSurfaces!(this.deviceId, (frame: PreviewDelta) => {
                    if (generation !== this.surfaceGeneration || this.stopped) return;
                    this.triggerDescriptions = frame.trigger_descriptions || {};
                    if (frame.reset) {
                        this.previewSequence = 0;
                        this.previewGeneration = frame.generation;
                    }
                    if (
                        frame.generation !== this.previewGeneration ||
                        frame.sequence !== this.previewSequence + 1
                    ) {
                        this.pauseSurfaces();
                        this.requestSurfaces();
                        return;
                    }
                    this.previewSequence = frame.sequence;
                    if (
                        frame.configuration_revision !== undefined &&
                        frame.configuration_revision < this.configurationRevision
                    )
                        return;
                    this.surfaceConfigurationRevision =
                        frame.configuration_revision ?? this.configurationRevision;
                    if (frame.reset) {
                        this.surfaces = Object.fromEntries(
                            Object.entries(frame.surfaces).filter(
                                (entry): entry is [string, string] => typeof entry[1] === 'string',
                            ),
                        );
                    }
                    for (const [key, value] of Object.entries(frame.surfaces)) {
                        if (value === null) delete this.surfaces[key];
                        else if (value !== undefined) this.surfaces[key] = value;
                    }
                    if (frame.background_changed) this.background = frame.background;
                    // Process the settled state operation result
                }).then((stop) => {
                    if (generation === this.surfaceGeneration && !this.stopped)
                        this.stopSurfaces = stop;
                    else return this.retire(stop, 'late surfaces');
                }),
            );
        } catch (e) {
            this.notify(String(e), 'error');
        } finally {
            this.surfaceStarting = false;
            if ((this.surfaceWanted || generation !== this.surfaceGeneration) && !this.stopped) {
                this.surfaceWanted = false;
                this.requestSurfaces();
            }
        }
    }
    // Derive editable controls from the current state state
    get editableControls() {
        return (this.device?.manifest.controls || []).filter(
            (control) => !control.semantics.includes('swipe'),
        );
    }
    // Send the swipe direction through the device's declared swipe control
    async swipePage(direction: string) {
        const control = this.device?.manifest.controls.find((c) => c.semantics.includes('swipe'));
        if (control) await this.triggerControl(control.id, undefined, direction);
    }
    // Derive session from the current state state
    get session() {
        return this.snapshot.sessions?.[this.device?.persistence_key || this.deviceId];
    }
    // Derive device unavailable reason from the current state state
    get deviceUnavailableReason() {
        return (
            this.device?.manifest.variants.find((variant) => variant.id === this.device?.variant_id)
                ?.unavailable_reason || ''
        );
    }
    // Derive device output from the current state state
    get deviceOutput() {
        const count = this.device?.manifest.capabilities.lighting_channels.length || 0;
        const output = this.session?.output || defaultDeviceOutput(count);
        return {
            ...output,
            lighting_colors:
                output.lighting_colors.length === count
                    ? output.lighting_colors
                    : defaultDeviceOutput(count).lighting_colors,
        };
    }
    powerStatus: DevicePowerStatus | null = null;
    powerError = '';
    private powerIdentity = '';
    private powerEpoch = 0;
    private powerPending = '';
    // Derive effective device output from the current state state
    get effectiveDeviceOutput() {
        return this.powerIdentity === this.deviceIdentity && this.powerStatus
            ? this.powerStatus.effective
            : this.deviceOutput;
    }
    // Derive device identity from the current state state
    get deviceIdentity() {
        return JSON.stringify([
            this.deviceId,
            this.device?.persistence_key,
            this.device?.connected,
        ]);
    }
    // Derive power description from the current state state
    get powerDescription() {
        if (this.powerError) return 'Power status unavailable: ' + this.powerError;
        if (this.powerIdentity !== this.deviceIdentity || !this.powerStatus)
            return 'Automatic power is off';
        const status = this.powerStatus;
        if (!status.enabled) return status.unavailable.join('. ') || 'Automatic power is off';
        if (status.automatic_sleeping) return `Automatic sleep · ${status.reasons.join(', ')}`;
        if (status.dimmed) return `Automatically dimmed · ${status.reasons.join(', ')}`;
        if (status.overridden.length) return `Wake override · ${status.overridden.join(', ')}`;
        return status.unavailable.join('. ') || 'Automatic power is enabled';
    }
    // Invalidate outstanding power reads and clear the previous device's power state
    private invalidatePower() {
        ++this.powerEpoch;
        this.powerPending = '';
        this.powerIdentity = '';
        this.powerStatus = null;
        this.powerError = '';
    }
    // Refresh the power
    async refreshPower(force = false) {
        if (
            this.windowSuspended ||
            windowWork.quiescing ||
            (typeof document !== 'undefined' && document.hidden)
        )
            return;
        if (force) {
            ++this.powerEpoch;
            this.powerPending = '';
        }
        const identity = this.deviceIdentity;
        if (this.powerPending === identity || !this.device?.connected || this.stopped) return;
        const epoch = ++this.powerEpoch;
        this.powerPending = identity;
        try {
            const status = await this.bridge.command('device_power_status', {
                deviceId: this.deviceId,
            });
            if (
                epoch === this.powerEpoch &&
                identity === this.deviceIdentity &&
                !this.stopped &&
                !this.windowSuspended
            ) {
                this.powerIdentity = identity;
                this.powerStatus = status;
                this.powerError = '';
            }
        } catch (error) {
            if (epoch === this.powerEpoch && identity === this.deviceIdentity) {
                this.powerError = String(error);
                this.powerIdentity = '';
                this.powerStatus = null;
            }
        } finally {
            if (epoch === this.powerEpoch) this.powerPending = '';
        }
    }
    // Derive configuration revision from the current state state
    get configurationRevision() {
        return this.snapshot.configuration_revision;
    }
    // Derive selected ids from the current state state
    get selectedIds() {
        return this.selectedControls.length
            ? this.selectedControls
            : [this.controlId].filter(Boolean);
    }
    // Encode the device, profile, page and control identity used to scope binding work
    bindingAddress(controlId = this.controlId) {
        return JSON.stringify([
            this.device?.persistence_key || this.deviceId,
            this.profile?.id,
            this.page?.id,
            controlId,
        ]);
    }
    // Find the action control that owns a feedback surface, excluding touch-panel buttons
    feedbackSource(controlId: string) {
        if (this.device?.manifest.capabilities.touch_panel?.button_controls.includes(controlId))
            return undefined;
        return this.device?.manifest.controls.find((c) => c.feedback_surface === controlId)?.id;
    }
    // Resolve device overrides before saved bindings and inherited feedback-owner bindings
    displayedBinding(controlId: string) {
        const source = this.feedbackSource(controlId);
        const overrides = this.snapshot.binding_overrides?.[this.deviceId];
        return (
            overrides?.[controlId]?.binding ||
            (source ? overrides?.[source]?.binding : undefined) ||
            this.page?.bindings[controlId] ||
            (source ? this.page?.bindings[source] : undefined)
        );
    }
    // Check whether protected control is valid for the current state state
    isProtectedControl(controlId: string) {
        return isKeyboardNavigationControl(this.profile, this.page?.id, controlId);
    }
    // Check whether clear binding is valid for the current state state
    canClearBinding(controlId: string) {
        if (this.isProtectedControl(controlId)) return false;
        if (this.dirtyBindingAddress === this.bindingAddress(controlId)) return true;
        const binding = this.page?.bindings[controlId];
        const source = this.feedbackSource(controlId);
        const inherited = source ? this.page?.bindings[source] : undefined;
        // Keep an explicit empty touch surface that masks inherited dial feedback
        return binding ? !inherited || hasBindingContent(binding) : hasBindingContent(inherited);
    }
    // Clear the binding
    async clearBinding(controlId: string) {
        const profile = this.profile;
        const page = this.page;
        if (
            !profile ||
            !page ||
            this.controlPending(controlId) ||
            this.isProtectedControl(controlId)
        )
            return;
        // Complete the deferred state work using the current operation state
        return this.withControls([controlId], async () => {
            const address = this.bindingAddress(controlId);
            const label =
                this.device?.manifest.controls.find((c) => c.id === controlId)?.label || controlId;
            const source = this.feedbackSource(controlId);
            const hasSavedBinding = Boolean(
                page.bindings[controlId] || (source && page.bindings[source]),
            );
            const expectedRevision = this.configurationRevision;
            // An explicit empty surface masks dial feedback without removing the dial's action
            const binding: Binding | null =
                source && page.bindings[source]
                    ? {
                          title: '',
                          color: DEFAULT_BACKGROUND,
                          action: { kind: 'none' },
                          media: defaultMedia(),
                      }
                    : null;
            await this.flushDrafts?.();
            if (hasSavedBinding && this.canClearBinding(controlId))
                await this.edit(
                    [
                        {
                            kind: 'bindings',
                            profile_id: profile.id,
                            page_id: page.id,
                            bindings: { [controlId]: binding },
                        },
                    ],
                    `Clear ${label}`,
                    expectedRevision,
                );
            this.bindingCleared?.(address);
            // Drop only the affected cached image; keep every other surface steady
            if (this.bindingAddress(controlId) === address) {
                delete this.surfaces[controlId];
                const feedback = this.device?.manifest.controls.find(
                    (control) => control.id === controlId,
                )?.feedback_surface;
                if (feedback && !this.page?.bindings[feedback]) delete this.surfaces[feedback];
            }
            this.notify(`${label} cleared`, 'success');
        });
    }
    // Clear the background
    async clearBackground() {
        if (!this.profile?.background_asset) return;
        const profile: Profile = cloneJson(this.profile);
        delete profile.background_asset;
        profile.background_media = defaultMedia();
        await this.edit([{ kind: 'save_profile', profile }], 'Clear device background');
        this.notify('Device background cleared', 'success');
    }
    // Open the selected state item for editing
    async edit(edits: Edit[], description: string, expectedRevision = this.configurationRevision) {
        // Capture explicit device selection before queued writes can outlive the current editor context
        const needsDevice = edits.some(
            (edit) =>
                (edit.kind === 'bindings' || edit.kind === 'assign_control') && !edit.profile_id,
        );
        const deviceId = needsDevice ? this.deviceId || undefined : undefined;
        const explicitTargets = edits.every(
            (edit) => edit.kind !== 'bindings' || Boolean(edit.profile_id && edit.page_id),
        );
        for (const edit of edits)
            if (edit.kind === 'bindings') {
                // Check the current state preconditions
                const check = (action: Action) => {
                    if (action.kind === 'plugin') {
                        const choice = this.snapshot.plugins
                            .find((p) => p.id === action.plugin_id)
                            ?.actions.find((a) => a.id === action.action_id);
                        if (choice && !choice.control_types.includes(targetKind))
                            throw Error('Plugin action is incompatible with the target control');
                    }
                    if (action.kind === 'macro' || action.kind === 'dial')
                        action.actions.forEach(check);
                    if (action.kind === 'toggle') {
                        check(action.on);
                        check(action.off);
                    }
                };
                let targetKind = '';
                for (const [id, binding] of Object.entries(edit.bindings)) {
                    targetKind =
                        this.device?.manifest.controls.find((c) => c.id === id)?.kind || '';
                    if (binding && !binding.disabled) check(binding.action);
                }
            }
        const local = edits.every((edit) => edit.kind === 'bindings');
        const queuedRevision = this.configurationRevision;
        // Fingerprint the binding targets affected by the pending workspace edits
        const targetSignature = () =>
            JSON.stringify(
                // Convert each entry into the representation required by state
                edits.map((edit) => {
                    if (edit.kind !== 'bindings') return null;
                    const profile = this.snapshot.profiles.find(
                        (profile) => profile.id === edit.profile_id,
                    );
                    const page = profile?.pages.find((page) => page.id === edit.page_id);
                    return [
                        profile?.model_id,
                        Boolean(page),
                        Object.keys(edit.bindings).map((id) => [id, page?.bindings[id] || null]),
                    ];
                }),
            );
        const before = local ? targetSignature() : '';
        // Save pending changes through the state workflow
        const save = async () => {
            const result = await this.command(
                'apply_workspace_edits',
                {
                    edits,
                    description,
                    deviceId,
                    expectedRevision:
                        local &&
                        explicitTargets &&
                        expectedRevision === queuedRevision &&
                        before === targetSignature()
                            ? this.configurationRevision
                            : expectedRevision,
                },
                !local,
            );
            this.receiveWorkspace(result);
        };
        if (!local) return save();
        const ids = edits.flatMap((edit) =>
            edit.kind === 'bindings' ? Object.keys(edit.bindings) : [],
        );
        const queued = this.bindingWrites.catch(() => {}).then(save);
        this.bindingWrites = queued;
        await this.withControls(ids, () => queued);
    }
    // Flush drafts before applying a revision-checked undo, redo or history clear
    async history(operation: 'undo' | 'redo' | 'clear') {
        await this.flushDrafts?.();
        this.apply(
            await this.command('history_action', {
                operation,
                expectedRevision: this.configurationRevision,
            }),
        );
    }
    // Copy the bindings
    async copyBindings() {
        const ids = this.sortedIds(this.selectedIds);
        this.clipboard = ids.map((id) =>
            this.page?.bindings[id] ? cloneJson(this.page.bindings[id]) : null,
        );
        if (this.nativeRendering) {
            this.clipboard = await Promise.all(
                ids.map((controlId) =>
                    this.page?.bindings[controlId]
                        ? this.bridge.command('capture_binding', {
                              deviceId: this.deviceId,
                              controlId,
                          })
                        : Promise.resolve(null),
                ),
            );
        }
        this.notify(`Copied ${this.clipboard.length} control(s)`, 'success');
    }
    // Sort control IDs by physical row and column with a deterministic ID tie-breaker
    sortedIds(ids: string[]) {
        // Complete the deferred state work using the current operation state
        return [...ids].sort((a, b) => {
            const ac = this.device?.manifest.controls.find((c) => c.id === a),
                bc = this.device?.manifest.controls.find((c) => c.id === b);
            return (ac?.y || 0) - (bc?.y || 0) || (ac?.x || 0) - (bc?.x || 0) || a.localeCompare(b);
        });
    }
    // Validate clipboard cardinality and protected controls before pasting bindings
    async pasteBindings() {
        const ids = this.sortedIds(this.selectedIds);
        if (
            !this.clipboard.length ||
            !ids.length ||
            (this.profile ? !this.page : !this.device?.connected)
        )
            return;
        if (this.clipboard.length !== 1 && this.clipboard.length !== ids.length) {
            this.notify('Select the same number of controls as copied bindings.', 'warning');
            return;
        }
        if (ids.some((id) => this.isProtectedControl(id))) {
            this.notify('The keyboard navigation keys are protected.', 'warning');
            return;
        }
        const bindings = Object.fromEntries(
            ids.map((id, index) => [
                id,
                JSON.parse(
                    JSON.stringify(
                        this.clipboard.length === 1 ? this.clipboard[0] : this.clipboard[index],
                    ),
                ),
            ]),
        );
        await this.edit(
            [
                {
                    kind: 'bindings',
                    profile_id: this.profile?.id || '',
                    page_id: this.page?.id || '',
                    bindings,
                },
            ],
            'Paste bindings',
        );
    }
    // Explain why a proposed binding copy or move is incompatible or protected
    transferError(source: string, target: string, copy: boolean) {
        if (this.isProtectedControl(target) || (!copy && this.isProtectedControl(source)))
            return 'The keyboard navigation keys are protected.';
        if (source === target) return 'Choose a different control.';
        const from = this.device?.manifest.controls.find((control) => control.id === source);
        const to = this.device?.manifest.controls.find((control) => control.id === target);
        if (!from || !to) return 'Choose a control on this device.';
        if (this.controlPending(source) || this.controlPending(target))
            return 'Wait for this control to finish updating.';
        const binding = this.page?.bindings[source];
        if (!binding || !hasBindingContent(binding))
            return 'Drag a saved binding to move its action.';
        const error = bindingCompatibility(binding, to, this.snapshot.plugins);
        if (error) return error;
        const other = this.page?.bindings[target];
        return !copy && other ? bindingCompatibility(other, from, this.snapshot.plugins) : '';
    }
    // Validate and apply a binding move or copy between distinct controls
    async transferBinding(source: string, target: string, copy: boolean) {
        const profile = this.profile;
        const page = this.page;
        if (!profile || !page || source === target) return;
        const error = this.transferError(source, target, copy);
        if (error) {
            this.notify(error, 'warning');
            return;
        }
        // Complete the deferred state work using the current operation state
        return this.withControls([source, target], async () => {
            const profileId = profile.id;
            const pageId = page.id;
            const deviceId = this.deviceId;
            const expectedRevision = this.configurationRevision;
            const original = page.bindings[source]!;
            const other = page.bindings[target];
            await this.flushDrafts?.();
            // Capture the current state state
            const capture = async (id: string, binding: Binding | undefined) =>
                !binding
                    ? null
                    : this.nativeRendering
                      ? this.bridge.command('capture_binding', { deviceId, controlId: id })
                      : (cloneJson(binding) as Binding);
            const [binding, targetBinding] = await Promise.all([
                capture(source, original),
                copy ? Promise.resolve(null) : capture(target, other),
            ]).catch((error) => {
                this.notify(String(error), 'error');
                throw error;
            });
            if (
                this.deviceId !== deviceId ||
                this.profile?.id !== profileId ||
                this.page?.id !== pageId ||
                this.configurationRevision !== expectedRevision
            ) {
                this.notify('The source page changed during the move. Try again.', 'warning');
                return;
            }
            const bindings: Record<string, Binding | null> = {
                [target]: binding,
            };
            if (!copy) bindings[source] = targetBinding;
            await this.edit(
                [{ kind: 'bindings', profile_id: profileId, page_id: pageId, bindings }],
                copy ? 'Copy binding' : 'Move or swap binding',
                expectedRevision,
            );
        });
    }
    deviceId = '';
    controlId = '';
    route = 'editor';
    navigationPending = false;
    busy = false;
    ready = false;
    events: RuntimeEventEntry[] = [];
    private nextEventLogId = 0;
    private unsubscribe?: RetireSubscription;
    private loading = true;
    private buffered: RuntimeEvent[] = [];
    private bufferedBytes = 0;
    private startupTask?: Promise<void>;
    private startupEpoch = 0;
    private startupFailed = false;
    private abortStartup?: (error: Error) => void;
    // Derive startup buffer usage from the current state state
    get startupBufferUsage() {
        return { events: this.buffered.length, chargedBytes: this.bufferedBytes };
    }
    private refreshing?: Promise<void>;
    // Initialize state state and its required lifecycle hooks
    constructor(
        public bridge: Bridge,
        public nativeRendering = bridge.surfaceRendering === 'stream',
    ) {
        super();
        const startup = ownWindowMethods(
            { initialize: this.initialize, withControls: this.withControls },
            'workspace',
            ['initialize', 'withControls'],
        );
        this.initialize = startup.initialize;
        this.withControls = startup.withControls;
        ownWindowMethods<AppState>(this, 'workspace', [
            'triggerControl',
            'triggerSimulator',
            'swipePage',
            'clearBinding',
            'clearBackground',
            'edit',
            'history',
            'copyBindings',
            'pasteBindings',
            'transferBinding',
            'openDataFolder',
            'openDeviceEditor',
        ]);
    }
    // Open the data folder
    async openDataFolder() {
        if (!this.nativeRendering) {
            this.notify(
                'Opening the application data folder requires the native application.',
                'warning',
            );
            return;
        }
        try {
            await this.command('open_data_folder');
        } catch {}
    }
    // Derive simulated from the current state state
    get simulated() {
        return this.device?.simulated || false;
    }
    // Select the device
    selectDevice(event: Event) {
        this.selectDeviceId((event.target as HTMLSelectElement).value);
    }
    // Open the device editor
    async openDeviceEditor(id: string) {
        await this.flushDrafts?.();
        this.apply(await this.bridge.command('snapshot'));
        if (!this.snapshot.devices.some((device) => device.id === id && device.connected)) {
            this.notify('The selected device is no longer connected.', 'warning');
            return;
        }
        this.selectDeviceId(id);
        location.hash = '#/editor';
    }
    // Select the device id
    selectDeviceId(id: string) {
        if (this.windowSuspended || windowWork.quiescing) return;
        void this.flushDrafts?.().catch((error) => this.notify(String(error), 'error'));
        this.selectedControls = [];
        this.deviceId = id;
        this.invalidatePower();
        void this.refreshPower();
        this.controlId = this.device?.manifest.controls[0]?.id || '';
        this.surfaces = {};
        this.triggerDescriptions = {};
        this.background = null;
        this.surfaceConfigurationRevision = 0;
        this.surfaceGeneration++;
        if (this.nativeRendering) this.requestSurfaces();
    }
    // Derive model name from the current state state
    get modelName() {
        return this.device?.manifest.name || 'No device';
    }
    // Derive pages from the current state state
    get pages() {
        return this.profile?.pages || [];
    }
    // Derive control kind from the current state state
    get controlKind() {
        return this.control?.kind || '';
    }
    // Derive device from the current state state
    get device(): Device | undefined {
        return (
            this.snapshot.devices.find((d) => d.id === this.deviceId) || this.snapshot.devices[0]
        );
    }
    // Derive profile from the current state state
    get profile(): Profile | undefined {
        return this.snapshot.profiles.find(
            (p) =>
                p.id === (this.session?.profile_id || this.snapshot.active_profile) &&
                (!this.device || p.model_id === this.device.model_id),
        );
    }
    // Derive compatible profiles from the current state state
    get compatibleProfiles() {
        return this.snapshot.profiles.filter(
            (profile) => !this.device || profile.model_id === this.device.model_id,
        );
    }
    // Derive page from the current state state
    get page() {
        return this.profile?.pages.find(
            (p) => p.id === (this.session?.page_id || this.profile?.active_page),
        );
    }
    // Derive control from the current state state
    get control(): Control | undefined {
        return this.device?.manifest.controls.find((c) => c.id === this.controlId);
    }
    // Derive binding from the current state state
    get binding(): Binding | undefined {
        return this.page?.bindings[this.controlId];
    }
    private stopWorkspace?: RetireSubscription;
    private workspaceEpoch = 0;
    private workspaceGeneration = '';
    private workspaceSequence = 0;
    private workspaceStarting = false;
    private workspaceRetry = false;
    private finishWorkspaceStart?: () => void;
    // Reset the workspace
    private async resetWorkspace() {
        if (this.stopped) return;
        if (this.workspaceStarting) {
            this.workspaceRetry = true;
            return;
        }
        this.workspaceStarting = true;
        this.workspaceRetry = false;
        const epoch = ++this.workspaceEpoch;
        this.retire(this.stopWorkspace, 'workspace');
        this.stopWorkspace = undefined;
        this.workspaceGeneration = '';
        // Provide a replaceable completion callback for initial workspace delivery
        let receivedInitial: () => void = () => {};
        const initial = new Promise<void>((resolve) => {
            receivedInitial = resolve;
        });
        // Cancel the subscription
        let cancelSubscription: () => void = () => {};
        const cancelled = new Promise<void>((resolve) => {
            cancelSubscription = resolve;
        });
        // Cancel the active state operation
        const cancel = () => {
            cancelSubscription();
            receivedInitial();
        };
        this.finishWorkspaceStart = cancel;
        try {
            await Promise.race([
                this.trackSubscription(
                    this.bridge
                        // Complete the deferred state work using the current operation state
                        .subscribeWorkspace((batch) => {
                            if (epoch !== this.workspaceEpoch || this.stopped) return;
                            this.receiveWorkspace(batch);
                            if (batch.reset) receivedInitial();
                        })
                        // Process the settled state operation result
                        .then((stop) => {
                            if (epoch === this.workspaceEpoch && !this.stopped)
                                this.stopWorkspace = stop;
                            else return this.retire(stop, 'late workspace');
                        }),
                ),
                cancelled,
            ]);
            await initial;
        } finally {
            if (this.finishWorkspaceStart === cancel) {
                this.finishWorkspaceStart = undefined;
                this.workspaceStarting = false;
                if (this.workspaceRetry && !this.stopped)
                    void this.resetWorkspace().catch((e) => this.notify(String(e), 'error'));
            }
        }
    }
    // Apply ordered workspace batches or request a reset when generation or sequence diverges
    receiveWorkspace(batch: WorkspaceBatch) {
        if (this.windowSuspended) return;
        if (
            batch.reset_required ||
            (!batch.reset &&
                (batch.generation !== this.workspaceGeneration ||
                    batch.sequence > this.workspaceSequence + 1))
        ) {
            void this.resetWorkspace().catch((e) => this.notify(String(e), 'error'));
            return;
        }
        if (
            batch.generation === this.workspaceGeneration &&
            batch.sequence <= this.workspaceSequence
        )
            return;
        try {
            if (batch.reset) {
                if (
                    batch.reset.revision !== batch.revision ||
                    batch.reset.configuration_revision !== batch.configuration_revision
                )
                    throw Error('Invalid workspace reset');
                this.workspaceGeneration = batch.generation;
            }
            if (batch.revision >= this.snapshot.revision) {
                const next = batch.reset || workspaceSnapshot(this.snapshot, batch.changes);
                this.apply({
                    ...next,
                    revision: batch.revision,
                    configuration_revision: batch.configuration_revision,
                });
            }
            this.workspaceSequence = batch.sequence;
        } catch {
            void this.resetWorkspace().catch((e) => this.notify(String(e), 'error'));
        }
    }
    // Start the requested state operation
    start(): Promise<void> {
        if (this.startupTask) return this.startupTask;
        if (this.ready && !this.stopped) return Promise.resolve();
        const task = this.initialize();
        this.startupTask = task;
        // Clear the startup task only if it is still the task that just settled
        const finished = () => {
            if (this.startupTask === task) this.startupTask = undefined;
        };
        void task.then(finished, finished);
        return task;
    }
    // Initialize state state and its required lifecycle hooks
    private async initialize() {
        const epoch = ++this.startupEpoch;
        this.stopped = false;
        this.ready = false;
        this.loading = true;
        this.startupFailed = false;
        this.buffered = [];
        this.bufferedBytes = 0;
        // Resolve the state operation when its asynchronous work finishes
        const cancelled = new Promise<never>((_resolve, reject) => {
            this.abortStartup = (error) => {
                this.startupFailed = true;
                reject(error);
            };
        });
        // subscribe() may throw synchronously before the first race is attached
        void cancelled.catch(() => {});
        // Wait for the current state operation to settle
        const wait = <T>(operation: Promise<T>) => Promise.race([operation, cancelled]);
        // Check the current state preconditions
        const check = () => {
            // A race can resolve before stop() but resume its caller afterward
            // Check at each publication boundary, not only inside the promise
            if (epoch !== this.startupEpoch || this.stopped || this.startupFailed)
                throw Error('Startup was stopped');
        };
        try {
            if (this.nativeRendering && typeof document !== 'undefined') {
                // Capture the reactive instance used to start the store
                this.visibilityHandler = () => {
                    if (this.windowSuspended || windowWork.quiescing) return;
                    void this.flushDrafts?.().catch((error) => this.notify(String(error), 'error'));
                    if (!document.hidden && this.nativeRendering) this.requestSurfaces();
                    else this.pauseSurfaces();
                };
                document.addEventListener('visibilitychange', this.visibilityHandler);
            }
            await wait(
                this.trackSubscription(
                    this.bridge
                        // Complete the deferred state work using the current operation state
                        .subscribe((event) => {
                            if (epoch === this.startupEpoch) this.receive(event);
                        })
                        // Process the settled state operation result
                        .then((stop) => {
                            if (epoch !== this.startupEpoch || this.stopped || this.startupFailed)
                                return this.retire(stop, 'late runtime events');
                            else this.unsubscribe = stop;
                        }),
                ),
            );
            check();
            await wait(this.resetWorkspace());
            check();
            const models = await wait(this.bridge.command('simulator_models'));
            check();
            this.models = models;
            const drafts = await wait(this.bridge.command('drafts').catch(() => ({})));
            check();
            this.draftStore = drafts;
            this.loading = false;
            const buffered = this.buffered;
            this.buffered = [];
            this.bufferedBytes = 0;
            for (const event of buffered) this.receive(event);
            check();
            this.ready = true;
        } catch (e) {
            // A partial event sequence cannot safely recover transient commands
            // Fail this attempt and let a retry subscribe to an authoritative reset
            this.stop();
            throw e;
        } finally {
            this.abortStartup = undefined;
        }
    }
    // Apply the pending state changes
    apply(snapshot: Snapshot) {
        if (
            !Number.isSafeInteger(snapshot.configuration_revision) ||
            snapshot.configuration_revision < 0
        )
            throw new Error('Workspace snapshot requires a valid configuration_revision');
        if (this.windowSuspended) return;
        if (snapshot.revision < this.snapshot.revision) return;
        const removedPlugins = this.snapshot.plugins
            .filter((plugin) => !snapshot.plugins.some((current) => current.id === plugin.id))
            .map((plugin) => plugin.id);
        const previousDevice = this.device?.id;
        const previousContext = JSON.stringify([previousDevice, this.profile?.id, this.page?.id]);
        const previousBindings = Object.fromEntries(
            (this.device?.manifest.controls || []).map((control) => [
                control.id,
                JSON.stringify(this.displayedBinding(control.id) || null),
            ]),
        );
        let changed = false;
        // Complete the deferred state work using the current operation state
        reconcile(this.snapshot, snapshot, (key) => {
            if (
                [
                    'configuration_revision',
                    'devices',
                    'profiles',
                    'sessions',
                    'active_profile',
                    'binding_overrides',
                ].includes(key)
            )
                changed = true;
        });
        if (!snapshot.devices.some((d) => d.id === this.deviceId))
            this.deviceId = snapshot.devices[0]?.id || '';
        if (!this.device?.manifest.controls.some((c) => c.id === this.controlId))
            this.controlId = this.device?.manifest.controls[0]?.id || '';
        const selected = [...new Set(this.selectedControls)].filter((id) =>
            this.editableControls.some((control) => control.id === id),
        );
        if (JSON.stringify(selected) !== JSON.stringify(this.selectedControls))
            this.selectedControls = selected;
        const context = JSON.stringify([this.device?.id, this.profile?.id, this.page?.id]);
        if (context !== previousContext) {
            this.surfaces = {};
            this.triggerDescriptions = {};
            this.background = null;
            this.surfaceConfigurationRevision = 0;
        } else if (this.surfaceConfigurationRevision < snapshot.configuration_revision) {
            let bindingChanged = false;
            for (const control of this.device?.manifest.controls || [])
                if (
                    previousBindings[control.id] !==
                    JSON.stringify(this.displayedBinding(control.id) || null)
                ) {
                    delete this.surfaces[control.id];
                    bindingChanged = true;
                }
            // A stopped native stream otherwise looks active forever. Renew it
            // for saved display edits without restarting on unrelated metadata
            if (bindingChanged) this.surfaceBindingRevision++;
        }
        if (removedPlugins.length && typeof window !== 'undefined')
            window.dispatchEvent(new CustomEvent('plugins-removed', { detail: removedPlugins }));
        if (changed && this.nativeRendering) this.requestSurfaces();
    }
    // Detach deleted media from drafts, release its resources and notify affected views
    forgetMedia(asset: string) {
        if (this.deletedMedia[asset]) return;
        this.deletedMedia[asset] = true;
        for (const draft of Object.values(this.draftStore))
            if (draft) detachDraftMedia(draft, asset);
        void releaseDeletedMedia(this, asset);
        if (typeof window !== 'undefined')
            window.dispatchEvent(new CustomEvent('media-deleted', { detail: { asset } }));
    }
    // Route runtime events to their owners only while the current window remains active
    receive(event: RuntimeEvent) {
        if (this.stopped || this.startupFailed || this.windowSuspended) return;
        if (
            typeof window !== 'undefined' &&
            ['vendor-profile-review', 'devices', 'plugin-lifecycle'].includes(event.kind)
        )
            window.dispatchEvent(new CustomEvent('vendor-profile-reviews-refresh'));
        if (['vendor-profile-review', 'plugin-runtime', 'plugin-lifecycle'].includes(event.kind))
            return;
        if (this.loading) {
            const bytes = startupEventBytes(event);
            if (
                this.buffered.length >= STARTUP_EVENT_LIMIT ||
                bytes > STARTUP_EVENT_BYTES - this.bufferedBytes
            ) {
                this.buffered = [];
                this.bufferedBytes = 0;
                this.abortStartup?.(
                    Error(
                        'Startup event buffer exceeded its limit. Restart Manager to retry startup.',
                    ),
                );
                return;
            }
            this.buffered.push(event);
            this.bufferedBytes += bytes;
            return;
        }
        if (event.kind === 'fonts-updated') {
            this.surfaceBindingRevision++;
            if (this.nativeRendering) this.requestSurfaces();
            return;
        }
        if (event.kind === 'media-deleted' && event.message) {
            this.forgetMedia(event.message);
            return;
        }
        this.events = recordRuntimeEvent(this.events, event, ++this.nextEventLogId);
        if (event.kind === 'action-show-manager') {
            void windowWork
                // Complete the deferred state work using the current operation state
                .run('runtime editor action', async () => {
                    try {
                        const target = JSON.parse(event.message || '{}');
                        if (target.kind === 'plugin_details') {
                            location.hash = '#/plugins';
                        }
                        if (target.kind === 'edit_binding') {
                            if (target.device_id) this.selectDeviceId(target.device_id);
                            const deviceId = this.deviceId;
                            if (target.profile_id)
                                this.apply(
                                    await this.command('activate_profile', {
                                        id: target.profile_id,
                                        deviceId,
                                    }),
                                );
                            if (target.page_id)
                                this.apply(
                                    await this.command('select_page', {
                                        deviceId,
                                        pageId: target.page_id,
                                    }),
                                );
                            if (target.control_id) this.controlId = target.control_id;
                        }
                    } catch (error) {
                        this.notify(String(error), 'error');
                    }
                })
                .catch(() => {});
            return;
        }
        if (['action-ui', 'action-ui-close', 'action-notification'].includes(event.kind)) return;
        const level: ToastLevel = event.kind.endsWith('error')
            ? 'error'
            : event.kind.endsWith('warning')
              ? 'warning'
              : event.kind.endsWith('success')
                ? 'success'
                : 'info';
        // Logs and action lifecycle metadata belong in Diagnostics. Dedicated
        // error events remain actionable without routine runs evicting alerts
        if (
            !['plugin-log', 'action-run'].includes(event.kind) &&
            (event.message || level === 'error')
        )
            this.notify(event.message || event.kind, level);
        // Transient feedback refreshes native pixels; workspace updates own configuration
        if (
            ['plugin-feedback', 'live-feedback', 'foreground', 'display-theme'].includes(
                event.kind,
            ) &&
            this.nativeRendering
        )
            this.requestSurfaces();
        if (event.kind === 'keyboard-device-settings')
            window.dispatchEvent(new CustomEvent('keyboard-device-settings', { detail: event }));
        if (event.kind === 'devices') this.invalidatePower();
        if (['device-power', 'device-output', 'devices'].includes(event.kind))
            void this.refreshPower(true);
        if (event.kind === 'device-power') return;
        if (event.kind.startsWith('locate-')) {
            window.dispatchEvent(new CustomEvent('dock-locate', { detail: event }));
            return;
        }
    }
    // Refresh state data while keeping the view synchronized
    async refresh() {
        if (this.windowSuspended || windowWork.quiescing) return;
        if (this.refreshing) return this.refreshing;
        this.refreshing = this.bridge
            .command('snapshot')
            .then((s) => {
                this.apply(s);
            })
            .catch((e) => {
                this.notify(String(e), 'error');
            })
            .finally(() => {
                this.refreshing = undefined;
            });
        return this.refreshing;
    }
    // Dispatch the requested state command
    async command<C extends import('../generated/commands').CommandName>(
        name: C,
        ...parameters: [...import('../generated/commands').CommandParameters<C>, blocking?: boolean]
    ): Promise<import('../generated/commands').CommandResult<C>> {
        const [args, blocking = true] = parameters;
        if (blocking) {
            this.blockingCommands++;
            this.busy = true;
        }
        try {
            return await this.bridge.command(
                name,
                ...([args] as import('../generated/commands').CommandParameters<C>),
            );
        } catch (e) {
            this.notify(String(e), 'error');
            throw e;
        } finally {
            if (blocking) this.busy = --this.blockingCommands > 0;
        }
    }
    private subscriptionWork = new Set<Promise<unknown>>();
    // Track subscription startup until its promise resolves or rejects
    private trackSubscription<T>(pending: Promise<T>): Promise<T> {
        this.subscriptionWork.add(pending);
        void pending.then(
            () => this.subscriptionWork.delete(pending),
            () => this.subscriptionWork.delete(pending),
        );
        return pending;
    }
    // Wait until all currently tracked subscription work has settled
    async subscriptionsSettled() {
        while (this.subscriptionWork.size) await Promise.allSettled([...this.subscriptionWork]);
    }
    // Retire the previous state operation
    private retire(stop: RetireSubscription | undefined, kind: string): Promise<void> {
        if (!stop) return Promise.resolve();
        return this.releaseSubscription(stop, kind).catch(() => {});
    }
    // Release the subscription
    private releaseSubscription(stop: RetireSubscription, kind: string): Promise<void> {
        const id = ++nextSubscriptionReleaseId;
        return this.trackSubscription(
            trackCloseRelease(`app subscription:${kind}:${id}`, async () => {
                await stop();
            }),
        );
    }
    /** Suspend delivery first, then await the actual native subscription acknowledgements. */
    async suspendWindow(fence?: WindowFence) {
        this.windowSuspended = true;
        const subscriptions = [this.stopWorkspace, this.stopSurfaces, this.unsubscribe];
        this.stopWorkspace = this.stopSurfaces = this.unsubscribe = undefined;
        this.stop();
        try {
            // Retire the previous state operation
            const retire = () => {
                // Start every release under the same synchronous capability. A reveal
                // during an acknowledgement must not strand the remaining handles
                const releases = subscriptions
                    .filter((stop) => !!stop)
                    .map((stop) => {
                        return this.releaseSubscription(stop!, 'window');
                    });
                // Process the settled state operation result
                return Promise.allSettled(releases).then((results) => {
                    const failed = results.find((result) => result.status === 'rejected');
                    if (failed?.status === 'rejected') {
                        throw failed.reason;
                    }
                });
            };
            if (fence) await windowWork.cleanup(fence, 'subscription retirement', retire);
            else await retire();
        } catch (error) {
            throw error;
        }
    }
    // Resume the window
    async resumeWindow() {
        this.windowSuspended = false;
        try {
            if (this.stopped) await this.start();
            else await this.resetWorkspace();
            this.requestSurfaces();
        } catch (error) {
            this.windowSuspended = true;
            await this.stop();
            throw error;
        }
    }
    // Stop the current state operation
    stop() {
        this.stopped = true;
        this.ready = false;
        this.startupEpoch++;
        this.abortStartup?.(Error('Startup was stopped'));
        this.buffered = [];
        this.bufferedBytes = 0;
        this.workspaceEpoch++;
        this.workspaceRetry = false;
        this.finishWorkspaceStart?.();
        this.finishWorkspaceStart = undefined;
        this.workspaceStarting = false;
        this.retire(this.stopWorkspace, 'workspace');
        this.stopWorkspace = undefined;
        for (const toast of this.toasts) this.dismissToast(toast.id);
        if (typeof document !== 'undefined' && this.visibilityHandler)
            document.removeEventListener('visibilitychange', this.visibilityHandler);
        this.visibilityHandler = undefined;
        this.pauseSurfaces();
        this.surfaceGeneration++;
        this.retire(this.unsubscribe, 'runtime events');
        this.unsubscribe = undefined;
        return this.subscriptionsSettled();
    }
}
