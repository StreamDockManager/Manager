import { createKeyboardMacroPreview } from './keyboard-macro-preview';
import type { CommandName, CommandParameters, CommandResult } from '../generated/commands';
import { dispatchPreview, composePreviewHandlers, type PreviewHandlers } from './preview-handlers';
import { createPowerPreview } from './power-preview';
import { workspaceDifference } from './workspace-stream';
import { createRuntimePreview } from './runtime-preview';
import { BuiltinPreview } from './builtin-preview';
import { createFeaturePreview } from './feature-preview';
import { createPluginLogPreview } from './plugin-log-preview';
import { defaultMedia, type Bridge } from './bridge';
import { createPreviewWorkspace, type PreviewHistory } from './preview-workspace';
import { createPreviewWorkspaceHandlers } from './preview-workspace-handlers';
import { createPreviewDeviceHandlers } from './preview-device-handlers';
import { createPreviewPluginHandlers } from './preview-plugin-handlers';
import { createPreviewSettingsHandlers } from './preview-settings-handlers';
import type { Snapshot, RuntimeEvent, Manifest } from './types';
import type { WorkspaceBatch, Draft, PluginPreflight } from '../generated/runtime';

// Browser preview has memory-only state and cannot execute host actions or access HID
export function createPreviewBridge(): Bridge {
    const builtinPreview = new BuiltinPreview();
    const keyboardMacro = createKeyboardMacroPreview();
    const settings = createPreviewSettingsHandlers();
    const powerHandlers = createPowerPreview();
    const featureHandlers = createFeaturePreview();
    const logs = createPluginLogPreview();
    const runtimeHandlers = createRuntimePreview(initial);
    let state: Snapshot | undefined;
    let simulator: Snapshot['devices'][number] | undefined;
    const drafts: Record<string, Draft> = {};
    const history: PreviewHistory = new Map();
    const listeners = new Set<(event: RuntimeEvent) => void>();
    const workspaceListeners = new Set<(batch: WorkspaceBatch) => void>();
    const workspaceGeneration = crypto.randomUUID();
    let workspaceSequence = 0;
    let publishedWorkspace: Snapshot | undefined;
    // Build a workspace frame for browser-preview subscribers
    const workspaceFrame = (
        snapshot: Snapshot,
        reset: Snapshot | null,
        changes: WorkspaceBatch['changes'],
    ): WorkspaceBatch => ({
        generation: workspaceGeneration,
        sequence: workspaceSequence,
        revision: snapshot.revision,
        configuration_revision: snapshot.configuration_revision,
        reset,
        reset_required: false,
        changes,
    });
    // Publish a changed preview workspace to its listeners
    const publishWorkspace = (snapshot: Snapshot) => {
        workspaceSequence++;
        const batch = workspaceFrame(
            snapshot,
            null,
            workspaceDifference(publishedWorkspace || snapshot, snapshot),
        );
        publishedWorkspace = structuredClone(snapshot);
        workspaceListeners.forEach((listener) => listener(structuredClone(batch)));
        return batch;
    };
    let models: Manifest[] = [];
    // Load the initial browser-preview workspace fixtures
    async function initial() {
        if (state) return state;
        const manifests: Manifest[] = await fetch(import.meta.env.BASE_URL + 'hardware.json').then(
            // Decode the fetched hardware descriptor fixture
            (r) => {
                if (!r.ok) throw Error('Hardware metadata missing. Run hardware generate.');
                return r.json();
            },
        );
        const m = manifests[0];
        models = manifests;
        state = {
            revision: 1,
            configuration_revision: 1,
            sessions: {},

            saved_macros: [],

            styles: [],
            rules: [],
            history: { undo: [], redo: [] },
            devices: [
                {
                    id: 'simulator:n4-pro',
                    persistence_key: 'simulator:n4-pro',
                    identity_port_dependent: false,
                    model_id: m.model_id,
                    name: 'N4 Pro Simulator',
                    variant_id: 'simulated',
                    firmware: 'simulated',
                    connected: true,
                    simulated: true,
                    manifest: m,
                },
            ],
            active_profile: 'default',
            plugins: [],
            profiles: [
                {
                    id: 'default',
                    name: 'Default',
                    model_id: m.model_id,
                    active_page: 'main',
                    background_media: defaultMedia(),
                    pages: [
                        {
                            id: 'main',
                            name: 'Main page',
                            bindings: {
                                'key-1': {
                                    title: 'Browser',
                                    media: defaultMedia(),
                                    color: '#c61e3200',
                                    action: { kind: 'open_url', url: 'https://example.org' },
                                },
                                'key-2': {
                                    title: 'Mute',
                                    media: defaultMedia(),
                                    color: '#77202b00',
                                    action: { kind: 'mute' },
                                },
                                'key-3': {
                                    title: 'Play / pause',
                                    media: defaultMedia(),
                                    color: '#40404000',
                                    action: { kind: 'media', command: 'play-pause' },
                                },
                                'key-4': {
                                    title: 'Volume down',
                                    media: defaultMedia(),
                                    color: '#30303000',
                                    action: { kind: 'volume', delta: -5 },
                                },
                                'key-5': {
                                    title: 'Volume up',
                                    media: defaultMedia(),
                                    color: '#30303000',
                                    action: { kind: 'volume', delta: 5 },
                                },
                            },
                        },
                    ],
                },
            ],
        };
        simulator = structuredClone(state.devices[0]);
        return state;
    }
    return {
        surfaceRendering: 'local',
        // Dispatch typed commands to the browser-only simulation
        async command<C extends CommandName>(command: C, ...parameters: CommandParameters<C>) {
            const rawArgs = parameters[0] || {};
            const args = rawArgs as Record<string, unknown>;
            if (['start_macro_debug', 'macro_debug_view', 'macro_debug_command'].includes(command))
                throw Error('Macro simulation and debugging require the native Manager runtime.');
            const s = await initial();
            if (
                settings.recoveryActive() &&
                ['open_settings_panel', 'run_action', 'test_action'].includes(command)
            )
                throw Error('Plugin execution is paused in recovery mode');
            const workspace = createPreviewWorkspace(s, {
                description: String(args.description || command.replaceAll('_', ' ')),
                history,
                publishWorkspace,
                // Notify browser-preview runtime event listeners
                notify: (event) => listeners.forEach((listener) => listener(event)),
            });
            // Enrich simulated plugin preflight with preview capabilities
            const enrichPreflight = (report: PluginPreflight) => {
                if (report.runtime) {
                    const status = runtimeHandlers.runtime_status();
                    const installation = status.installations.find(
                        (i) =>
                            i.kind === report.runtime!.requirement.kind &&
                            i.channel === report.runtime!.requirement.channel,
                    );
                    report.runtime.selected = installation || null;
                    report.runtime.state = installation ? 'ready' : 'missing';
                    report.checks = report.checks.filter((c) => c.code !== 'runtime_unavailable');
                    if (!installation)
                        report.checks.push({
                            code: 'runtime_unavailable',
                            level: 'blocker',
                            message: 'A compatible runtime is required.',
                            remedy: 'Install or select a supported runtime.',
                        });
                    report.can_enable = !report.checks.some((c) => c.level === 'blocker');
                }
                return report;
            };
            const features = featureHandlers(
                workspace,
                (deviceId) =>
                    listeners.forEach((listener) =>
                        listener({
                            kind: 'plugin-feedback',
                            device_id: deviceId,
                            revision: s.revision,
                            sequence: s.revision,
                            control: null,
                            message: null,
                        }),
                    ),
                enrichPreflight,
            );
            const devices = createPreviewDeviceHandlers(
                workspace,
                models,
                simulator,
                drafts,
                builtinPreview,
            );
            const plugins = createPreviewPluginHandlers(workspace, history);
            const handlers = composePreviewHandlers(
                {
                    // Return simulated startup permission requirements
                    startup_permissions: () => ({
                        platform: 'preview',
                        ready: true,
                        device_connections_enabled: true,
                        permissions: [],
                    }),
                    // Return the simulated completed startup permission state
                    complete_startup_permissions: () => ({
                        platform: 'preview',
                        ready: true,
                        device_connections_enabled: true,
                        permissions: [],
                    }),
                    // Keep startup permission settings simulated in browser preview
                    open_startup_permission_settings: () => {
                        throw Error(
                            'System permissions are only available in the desktop application.',
                        );
                    },
                } satisfies PreviewHandlers,
                createPreviewWorkspaceHandlers(workspace, drafts, history, features.media_details!),
                {
                    ...devices,
                    // Simulate input against the preview workspace
                    simulate_input: (args) => {
                        logs.onInput();
                        return devices.simulate_input(args);
                    },
                    // Trigger a control through the browser-preview action runner
                    trigger_control: (args) => {
                        logs.onInput();
                        return devices.trigger_control(args);
                    },
                },
                {
                    ...plugins,
                    // Delete a plugin from the simulated workspace
                    delete_plugin: (args) => {
                        logs.onDelete(args.id);
                        return plugins.delete_plugin(args);
                    },
                },
                settings.handlers,
                keyboardMacro,
                logs.handlers(s),
                powerHandlers(workspace),
                runtimeHandlers,
                features,
            );
            return dispatchPreview(handlers, command, ...parameters);
        },
        // Subscribe to simulated workspace updates
        async subscribeWorkspace(listener) {
            const s = await initial();
            workspaceListeners.add(listener);
            publishedWorkspace = structuredClone(s);
            listener(workspaceFrame(s, structuredClone(s), workspaceDifference(s, s)));
            // Remove the workspace listener when its subscription ends
            return () => {
                workspaceListeners.delete(listener);
            };
        },
        // Subscribe to simulated runtime events
        async subscribe(listener) {
            listeners.add(listener);
            // Remove the runtime listener when its subscription ends
            return () => {
                listeners.delete(listener);
            };
        },
    };
}
