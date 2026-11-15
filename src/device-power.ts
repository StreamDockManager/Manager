import { cloneJson } from './json-clone';
import type { AppState } from './state';
import type {
    DevicePowerSettings,
    DevicePowerPolicy,
    DevicePowerCapabilities,
} from '../generated/runtime';

// Format scheduled minutes as a clock time
const time = (minutes: number) =>
    `${String(Math.floor(minutes / 60)).padStart(2, '0')}:${String(minutes % 60).padStart(2, '0')}`;
// Parse a clock input into minutes since midnight
function minutes(value: string) {
    if (!/^\d{2}:\d{2}$/.test(value)) throw Error('Choose valid schedule times.');
    const [hour, minute] = value.split(':').map(Number);
    if (hour > 23 || minute > 59) throw Error('Choose valid schedule times.');
    return hour * 60 + minute;
}
// Create device-scoped scheduled power settings
export function createDevicePowerDraft(app: () => AppState) {
    return {
        settings: null as DevicePowerSettings | null,
        opened: false,
        loading: false,
        saving: false,
        error: '',
        epoch: 0,
        startTime: '',
        endTime: '',
        days: ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'].map((label, id) => ({ id, label })),
        policy: null as DevicePowerPolicy | null,
        baseline: '',
        // Capture editable power fields for close protection
        closeValue() {
            return JSON.stringify([this.policy, this.startTime, this.endTime]);
        },
        // Compare the power draft with its saved state
        get dirty() {
            return Boolean(this.opened && this.policy && this.closeValue() !== this.baseline);
        },
        capabilities: null as DevicePowerCapabilities | null,
        // Check that a power-settings response belongs to the current device session
        valid(epoch: number, id: string, key: string) {
            return (
                this.opened &&
                this.epoch === epoch &&
                app().deviceId === id &&
                app().device?.persistence_key === key
            );
        },
        // Load power settings for the selected device
        async openSettings() {
            this.discard();
            this.opened = true;
            const epoch = this.epoch;
            const id = app().deviceId;
            const key = app().device?.persistence_key || '';
            this.loading = true;
            try {
                const settings = await app().bridge.command('device_power_settings', {
                    deviceId: id,
                });
                if (!this.valid(epoch, id, key)) return;
                if (settings.device_id !== id || settings.device_key !== key)
                    throw Error('Device changed. Reopen its power settings.');
                this.settings = structuredClone(settings);
                this.policy = this.settings.policy;
                this.capabilities = this.settings.capabilities;
                this.startTime = time(settings.policy.schedule.start);
                this.endTime = time(settings.policy.schedule.end);
                this.baseline = this.closeValue();
            } catch (error) {
                if (this.valid(epoch, id, key)) this.error = String(error);
            } finally {
                if (this.valid(epoch, id, key)) this.loading = false;
            }
        },
        // Check whether a weekday is selected in the draft
        selectedDay(id: number) {
            return Boolean(this.policy?.schedule.days.includes(id));
        },
        // Toggle a scheduled weekday from its checkbox event
        toggleDay(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const id = Number(input.dataset.day);
            if (!this.policy || !Number.isInteger(id) || id < 0 || id > 6) return;
            this.policy.schedule.days = [
                ...new Set(
                    input.checked
                        ? [...this.policy.schedule.days, id]
                        : this.policy.schedule.days.filter((day) => day !== id),
                ),
            ].sort();
        },
        // Validate and save the scheduled power settings
        async commit() {
            if (!this.settings || this.loading || this.saving) return false;
            const epoch = this.epoch;
            const { device_id: id, device_key: key, revision } = this.settings;
            if (!this.valid(epoch, id, key)) return false;
            this.saving = true;
            this.error = '';
            try {
                const policy = cloneJson(this.settings.policy);
                policy.idle_minutes = Number(policy.idle_minutes);
                policy.dim_brightness = Number(policy.dim_brightness);
                policy.schedule.start = minutes(this.startTime);
                policy.schedule.end = minutes(this.endTime);
                await app().bridge.command('save_device_power_policy', {
                    deviceId: id,
                    deviceKey: key,
                    expectedRevision: revision,
                    policy,
                });
                if (!this.valid(epoch, id, key)) return false;
                await app().refreshPower(true);
                return this.valid(epoch, id, key);
            } catch (error) {
                if (this.valid(epoch, id, key)) this.error = String(error);
                return false;
            } finally {
                if (this.valid(epoch, id, key)) this.saving = false;
            }
        },
        // Restore the power draft from its saved state
        discard() {
            ++this.epoch;
            this.opened = false;
            this.loading = false;
            this.saving = false;
            this.error = '';
            this.settings = null;
            this.policy = null;
            this.capabilities = null;
        },
    };
}
