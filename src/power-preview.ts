import type { PreviewHandlers } from './preview-handlers';
import type { PreviewWorkspace } from './preview-workspace';
import type { Device, Snapshot } from './types';
import type {
    DevicePowerPolicy,
    DevicePowerSettings,
    DevicePowerStatus,
} from '../generated/runtime';

// Memory-only browser fixture. It never reads native session idle or lock state
export function createPowerPreview() {
    let revision = 0;
    const policies = new Map<string, DevicePowerPolicy>();
    const overrides = new Set<string>();
    // Create the disabled default power policy used by the browser simulator
    const defaultPolicy = (): DevicePowerPolicy => ({
        enabled: false,
        idle_action: 'off',
        idle_minutes: 10,
        lock_action: 'off',
        schedule_action: 'off',
        schedule: { days: [0, 1, 2, 3, 4, 5, 6], start: 1320, end: 420 },
        dim_brightness: 20,
    });
    // Evaluate the enabled power schedule against local time, including overnight intervals
    function active(policy: DevicePowerPolicy) {
        const now = new Date();
        const day = (now.getDay() + 6) % 7;
        const minute = now.getHours() * 60 + now.getMinutes();
        const { days, start, end } = policy.schedule;
        return (
            policy.enabled &&
            policy.schedule_action !== 'off' &&
            (start === end
                ? days.includes(day)
                : start < end
                  ? days.includes(day) && minute >= start && minute < end
                  : (days.includes(day) && minute >= start) ||
                    (days.includes((day + 6) % 7) && minute < end))
        );
    }
    // Simulate scheduled dimming or sleep while retaining baseline output and temporary overrides
    function status(device: Device, snapshot: Snapshot): DevicePowerStatus {
        const policy = policies.get(device.persistence_key) || defaultPolicy();
        const baseline = snapshot.sessions[device.persistence_key]?.output || {
            device_configuration: {},
            sleeping: false,
            brightness: 65,
            lighting_brightness: 15,
            lighting_colors: device.manifest.capabilities.lighting_channels.map(() => [
                198, 30, 50,
            ]),
        };
        const scheduled = active(policy);
        if (!scheduled) overrides.delete(device.persistence_key);
        const overridden = scheduled && overrides.has(device.persistence_key);
        const sleep = scheduled && !overridden && policy.schedule_action === 'sleep';
        const dim =
            scheduled && !overridden && policy.schedule_action === 'dim' && !baseline.sleeping;
        const effective = structuredClone(baseline);
        effective.sleeping ||= sleep;
        if (dim) {
            if (device.manifest.capabilities.brightness)
                effective.brightness = Math.min(effective.brightness, policy.dim_brightness);
            if (device.manifest.capabilities.lighting)
                effective.lighting_brightness = Math.min(
                    effective.lighting_brightness,
                    policy.dim_brightness,
                );
        }
        return {
            enabled: policy.enabled,
            effective,
            automatic_sleeping: sleep && !baseline.sleeping,
            dimmed: dim && JSON.stringify(effective) !== JSON.stringify(baseline),
            reasons: scheduled && !overridden ? ['Schedule'] : [],
            overridden: overridden ? ['Schedule'] : [],
            unavailable: [],
        };
    }
    // Return the current power preview settings
    function settings(device: Device, snapshot: Snapshot): DevicePowerSettings {
        const caps = device.manifest.capabilities;
        return {
            device_id: device.id,
            device_key: device.persistence_key,
            revision,
            policy: structuredClone(policies.get(device.persistence_key) || defaultPolicy()),
            capabilities: {
                platform: {
                    idle: false,
                    lock: false,
                    idle_description: 'Browser preview has no native idle sensor',
                    lock_description: 'Browser preview has no native lock sensor',
                },
                dim: caps.brightness || caps.lighting,
                sleep: caps.sleep,
                wake_input: caps.sleep,
                wake_description:
                    'Browser fixture only; physical wake reports require qualification',
            },
            status: status(device, snapshot),
        };
    }
    // Complete the deferred power preview work using the current operation state
    return (workspace: PreviewWorkspace) => {
        const { s: snapshot, sessionFor, commit } = workspace;
        // Resolve a connected simulated device or reject the request
        const connected = (id: string) => {
            const device = snapshot.devices.find((d) => d.id === id && d.connected);
            if (!device) throw Error('Device is not connected');
            return device;
        };
        return {
            // Simulate device power settings in the browser without calling native services
            device_power_settings: (args) => settings(connected(args.deviceId), snapshot),
            // Simulate device power status in the browser without calling native services
            device_power_status: (args) => status(connected(args.deviceId), snapshot),
            // Simulate save device power policy in the browser without calling native services
            save_device_power_policy: (args) => {
                const device = connected(args.deviceId);

                if (args.deviceKey !== device.persistence_key || args.expectedRevision !== revision)
                    throw Error('Power settings changed. Reopen them before saving.');
                const policy = structuredClone(args.policy as DevicePowerPolicy);
                if (
                    !policy ||
                    !['off', 'dim', 'sleep'].includes(policy.idle_action) ||
                    !['off', 'dim', 'sleep'].includes(policy.lock_action) ||
                    !['off', 'dim', 'sleep'].includes(policy.schedule_action) ||
                    !Number.isInteger(policy.idle_minutes) ||
                    policy.idle_minutes < 1 ||
                    policy.idle_minutes > 1440 ||
                    !Number.isInteger(policy.dim_brightness) ||
                    policy.dim_brightness < 0 ||
                    policy.dim_brightness > 100 ||
                    !policy.schedule ||
                    !Array.isArray(policy.schedule.days) ||
                    new Set(policy.schedule.days).size !== policy.schedule.days.length ||
                    policy.schedule.days.some(
                        (day) => !Number.isInteger(day) || day < 0 || day > 6,
                    ) ||
                    [policy.schedule.start, policy.schedule.end].some(
                        (value) => !Number.isInteger(value) || value < 0 || value >= 1440,
                    ) ||
                    (policy.schedule_action !== 'off' && !policy.schedule.days.length)
                )
                    throw Error('Choose valid power rules, times, days and brightness.');
                if (policy.enabled) {
                    if (policy.idle_action !== 'off' || policy.lock_action !== 'off')
                        throw Error(
                            'Native idle and lock sensors are unavailable in browser preview.',
                        );
                    if (policy.schedule_action === 'off')
                        throw Error('Choose at least one automatic dim or sleep rule');
                    if (
                        (policy.schedule_action === 'sleep' &&
                            !device.manifest.capabilities.sleep) ||
                        (policy.schedule_action === 'dim' &&
                            !device.manifest.capabilities.brightness &&
                            !device.manifest.capabilities.lighting)
                    )
                        throw Error('The selected device output is unavailable');
                }
                policies.set(device.persistence_key, policy);
                overrides.delete(device.persistence_key);
                ++revision;
                return settings(device, snapshot);
            },
            // Simulate set device sleep in the browser without calling native services
            set_device_sleep: (args) => {
                const device = connected(args.deviceId);
                sessionFor(device);

                const sleeping = Boolean(args.sleeping);
                if (sleeping && !device.manifest.capabilities.sleep)
                    throw Error('Device sleep is unavailable');
                const session = snapshot.sessions[device.persistence_key];
                if (!session) throw Error('Device session is unavailable');
                session.output.sleeping = sleeping;
                if (!sleeping && active(policies.get(device.persistence_key) || defaultPolicy()))
                    overrides.add(device.persistence_key);
                return commit().snapshot;
            },
        } satisfies PreviewHandlers;
    };
}
