import { ensureKeyboardNavigation, KEYBOARD_NAVIGATION_PAGE } from './keyboard-navigation';
import { createPreviewKeyboardHandlers } from './preview-keyboard';
import { previewBindingLookup } from './preview-binding-lookup';
import type { PreviewHandlers } from './preview-handlers';
import type { PreviewWorkspace } from './preview-workspace';
import type { CommandParameters, CommandResult } from '../generated/commands';
import type { Snapshot, Manifest } from './types';
import type { Draft } from '../generated/runtime';
import type { PreviewEvent } from './preview-workspace';
import { defaultMedia } from './bridge';
import type { BuiltinPreview } from './builtin-preview';

// Create the preview device handlers
export function createPreviewDeviceHandlers(
    workspace: PreviewWorkspace,
    models: Manifest[],
    simulator: Snapshot['devices'][number] | undefined,
    drafts: Record<string, Draft>,
    builtinPreview: BuiltinPreview,
) {
    const { s, sessionFor, select, commit, publish, markEdit } = workspace;
    // Trigger the control
    const triggerControl = (
        args: NonNullable<CommandParameters<'trigger_control'>[0]>,
    ): CommandResult<'trigger_control'> => {
        const device = s.devices.find((device) => device.id === args.deviceId);
        if (!device) throw Error('Device not connected');
        if (s.sessions[device.persistence_key]?.output.sleeping) throw Error('Device is sleeping');
        const control = args.event;
        // Preserve trigger-time session initialization; lookup itself remains read-only
        sessionFor(device);
        const { profile, page, binding } = previewBindingLookup(s, device, control.control_id);
        const panel = device.manifest.capabilities.touch_panel;
        builtinPreview.observe(String(args.deviceId), control.control_id, control.input);
        if (
            control.input.kind !== 'swipe' &&
            binding &&
            !binding.disabled &&
            (control.input.kind !== 'rotate' || ['dial'].includes(binding.action.kind)) &&
            !(control.input.kind === 'button' && !control.input.pressed) &&
            ['builtin', 'macro', 'toggle', 'dial', 'navigate'].includes(binding.action.kind)
        ) {
            void builtinPreview
                .run(
                    structuredClone(binding.action),
                    s,
                    String(args.deviceId),
                    control.control_id,
                    (profileId, pageId, manual) => select(args.deviceId, profileId, pageId, manual),
                    control.input,
                )
                // Process the settled preview device handlers operation result
                .then((message) => {
                    publish(
                        [
                            {
                                kind: 'action-preview',
                                device_id: String(args.deviceId),
                                control: null,
                                message,
                            },
                        ],
                        true,
                    );
                })
                // Handle a failed preview device handlers operation without retaining stale work
                .catch((error) => {
                    publish(
                        [
                            {
                                kind: 'action-error',
                                device_id: String(args.deviceId),
                                control: null,
                                message: String(error),
                            },
                        ],
                        true,
                    );
                });
        }
        let navigated = false;
        if (
            control.input.kind === 'swipe' &&
            device.manifest.controls
                .find((c) => c.id === control.control_id)
                ?.semantics.includes('swipe') &&
            profile &&
            page
        ) {
            const direction = control.input.direction;
            if ((direction === 'left' || direction === 'right') && profile.pages.length > 1) {
                const current = profile.pages.findIndex((p) => p.id === page.id);
                const step = direction === 'left' ? 1 : -1;
                const next = (current + step + profile.pages.length) % profile.pages.length;
                select(args.deviceId, profile.id, profile.pages[next].id, false);
                navigated = true;
            } else if (
                (direction === 'up' || direction === 'down') &&
                panel?.gesture_control === control.control_id &&
                panel.swipe_directions.includes(direction)
            ) {
                const profiles = s.profiles.filter((item) => item.model_id === device.model_id);
                const current = profiles.findIndex((item) => item.id === profile.id);
                if (current >= 0 && profiles.length > 1) {
                    const step = direction === 'up' ? 1 : -1;
                    const next = (current + step + profiles.length) % profiles.length;
                    select(args.deviceId, profiles[next].id);
                    navigated = true;
                }
            }
        }
        const events: PreviewEvent[] = [
            {
                kind: 'input',
                device_id: args.deviceId,
                control: args.event,
                message:
                    'Browser preview: input received; host actions require the native application.',
            },
        ];
        if (navigated)
            events.push({ kind: 'sessions', device_id: device.id, control: null, message: null });
        publish(events);
        return undefined;
    };
    const deviceHandlers = {
        // Simulate simulator models in the browser without calling native services
        simulator_models: (args) => {
            return structuredClone(models);
        },
        // Create or toggle a browser simulator and initialize a matching sample profile when needed
        simulate: (args) => {
            const id = String(args.deviceId || 'simulator:n4-pro');
            const manifest =
                models.find((model) => model.model_id === id.split(':')[1]) || models[0];
            let profile = s.profiles.find((profile) => profile.model_id === manifest.model_id);
            if (args.enabled && !profile && !s.sessions[id]) {
                const pageId = crypto.randomUUID();
                profile = {
                    id: crypto.randomUUID(),
                    name: manifest.name + ' profile',
                    model_id: manifest.model_id,
                    active_page: pageId,
                    pages: [{ id: pageId, name: 'Main page', bindings: {} }],
                    background_media: defaultMedia(),
                };
                if (profile.model_id === 'k1-pro') {
                    ensureKeyboardNavigation(profile);
                    profile.active_page = KEYBOARD_NAVIGATION_PAGE;
                }
                if (!s.profiles.length) s.active_profile = profile.id;
                s.profiles.push(profile);
                markEdit();
            }
            s.devices = s.devices.filter((d) => d.id !== id);
            if (args.enabled && simulator)
                s.devices.push({
                    ...structuredClone(simulator),
                    id,
                    persistence_key: id,
                    model_id: manifest.model_id,
                    manifest: structuredClone(manifest),
                    name: s.devices.length
                        ? manifest.name + ' Simulator ' + (s.devices.length + 1)
                        : manifest.name + ' Simulator',
                });
            if (args.enabled && profile)
                select(args.deviceId, s.sessions[id]?.profile_id || profile.id);
            return commit().snapshot;
        },
        // Simulate remove simulator in the browser without calling native services
        remove_simulator: (args) => {
            const id = String(args.deviceId);
            if (!id.startsWith('simulator:') || id.length > 100)
                throw Error('Invalid simulator identity');
            s.devices = s.devices.filter((device) => device.id !== id);
            delete s.sessions[id];
            const rules = s.rules.filter((rule) => rule.device_key !== id);
            if (rules.length !== s.rules.length) {
                s.rules = rules;
                markEdit();
            }
            for (const key of Object.keys(drafts)) {
                try {
                    if (JSON.parse(key)[0] === id) delete drafts[key];
                } catch {}
            }
            return commit().snapshot;
        },
        simulate_input: triggerControl,
        trigger_control: triggerControl,
        // Simulate builtin capabilities in the browser without calling native services
        builtin_capabilities: (args) => {
            return {};
        },
        // Simulate resume bindings in the browser without calling native services
        resume_bindings: (args) => {
            builtinPreview.resume(String(args.deviceId));
            return;
        },
        // Simulate automation status in the browser without calling native services
        automation_status: (args) => {
            return {
                available: false,
                reason: 'Foreground monitoring requires the desktop application.',
                schedules_available: true,
                title_available: false,
                application: null,
            };
        },
        ...createPreviewKeyboardHandlers(workspace),
        // Simulate set device output in the browser without calling native services
        set_device_output: (args) => {
            const device = s.devices.find((d) => d.id === args.deviceId);
            if (!device) throw Error('Device not connected');
            const output = structuredClone(args.output);
            output.device_configuration ||= {};
            const previous = sessionFor(device).output;
            if (
                output.vibration_enabled !== previous.vibration_enabled ||
                JSON.stringify(output.device_configuration) !==
                    JSON.stringify(previous.device_configuration || {})
            ) {
                if (
                    output.vibration_enabled != null ||
                    Object.keys(output.device_configuration).length
                ) {
                    const vibration = device.manifest.capabilities.vibration;
                    if (!vibration?.toggle || (!device.simulated && !vibration.writable))
                        throw Error('Vibration is unavailable for this device');
                    const fields = vibration.configuration_fields || [];
                    if (
                        typeof output.vibration_enabled !== 'boolean' ||
                        Object.keys(output.device_configuration).length !== fields.length ||
                        fields.some(
                            (field) => typeof output.device_configuration[field.id] !== 'boolean',
                        )
                    )
                        throw Error(
                            'Review and choose every related device setting before applying vibration',
                        );
                }
            }
            if (output.sleeping && !device.manifest.capabilities.sleep)
                throw Error('Device does not support sleep');
            if (
                !Number.isInteger(output.brightness) ||
                output.brightness < 0 ||
                output.brightness > 100 ||
                !Number.isInteger(output.lighting_brightness) ||
                output.lighting_brightness < 0 ||
                output.lighting_brightness > 100 ||
                output.lighting_colors.length !==
                    device.manifest.capabilities.lighting_channels.length ||
                output.lighting_colors.some(
                    (rgb) =>
                        rgb.length !== 3 ||
                        rgb.some((v) => !Number.isInteger(v) || v < 0 || v > 255),
                )
            )
                throw Error('Invalid device brightness or lighting');
            output.keyboard = previous.keyboard;
            sessionFor(device).output = output;
            return commit().snapshot;
        },
        // Simulate save automation configuration in the browser without calling native services
        save_automation_configuration: (args) => {
            const request = args.request;
            workspace.guard(request.expected_revision);
            const device = s.devices.find((d) => d.id === request.device_id);
            if (!device) throw Error('Device missing. Cancel and reopen Automatic profiles.');
            const previous = s.sessions[device.persistence_key];
            if (
                Boolean(previous?.automation_enabled) !== request.expected_enabled ||
                Boolean(previous?.automation_paused) !== request.expected_paused ||
                Boolean(previous?.automation_restore_on_exit) !== request.expected_restore_on_exit
            )
                throw Error('Automatic switching changed. Cancel and reopen Automatic profiles.');
            if (
                request.rules.length > 100 ||
                request.rules.some(
                    (rule) =>
                        rule.device_key !== device.persistence_key ||
                        (!rule.application.trim() && !rule.title && !rule.schedule) ||
                        !s.profiles.some(
                            (p) =>
                                p.id === rule.profile_id &&
                                (!rule.page_id || p.pages.some((page) => page.id === rule.page_id)),
                        ),
                )
            )
                throw Error('Rule needs a condition and valid profile/page');
            s.rules = [
                ...s.rules.filter((r) => r.device_key !== device.persistence_key),
                ...structuredClone(request.rules),
            ];
            const session = sessionFor(device);
            session.automation_enabled = request.enabled;
            session.automation_paused = request.paused;
            session.automation_restore_on_exit = request.restore_on_exit;
            markEdit();
            return commit().snapshot;
        },
        // Simulate set automation in the browser without calling native services
        set_automation: (args) => {
            const session = sessionFor(s.devices.find((d) => d.id === args.deviceId));
            session.automation_enabled = Boolean(args.enabled);
            session.automation_paused = Boolean(args.paused);
            return commit().snapshot;
        },
        // Simulate cancel locate in the browser without calling native services
        cancel_locate: (args) => {
            return undefined;
        },
        // Simulate preview on dock in the browser without calling native services
        preview_on_dock: (args) => {
            throw Error('Preview on dock requires a physical device.');
        },
        // Simulate arm locate in the browser without calling native services
        arm_locate: (args) => {
            throw Error('Locate requires a physical dock.');
        },
        // Simulate cancel macros in the browser without calling native services
        cancel_macros: (args) => {
            return undefined;
        },
    } satisfies PreviewHandlers;
    return deviceHandlers;
}
