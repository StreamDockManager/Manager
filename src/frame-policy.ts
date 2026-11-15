import { cloneJson } from './json-clone';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { DeviceFramePolicy } from '../generated/runtime';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
// Classify a frame policy using the supported preset definitions
export const framePolicyPreset = (mode: string): DeviceFramePolicy => {
    if (!['individual_updates', 'frame_scheduling', 'grouped_key_updates'].includes(mode))
        throw Error('Unsupported display scheduling mode; choose a current mode.');
    return {
        policy: {
            mode: mode === 'individual_updates' ? 'individual_updates' : 'frame_scheduling',
            target_frame_hz: 24,
            max_pending_bytes: 16777216,
            max_batch_bytes: 262144,
            max_batch_estimated_ms: 40,
            max_batch_surfaces: 64,
            max_surface_bytes: 8388608,
        },
        encoding: mode === 'grouped_key_updates' ? 'grouped_key_updates' : 'individual_updates',
    };
};
// Register device frame-policy selection and save behavior
export function registerFramePolicy(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('framePolicy', () => ({
        mode: 'grouped_key_updates',
        error: '',
        saving: false,
        capturedId: '',
        capturedSignature: '',
        baseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Attach frame policy listeners and window-close coordination
        init() {
            ownWindowMethods(this, 'frame policy', ['save']);
            this.unregisterCloseOwner = registerCloseDraft(this, ['save'], {
                name: 'Device frame policy',
                retainOnUnmount: true,
                // Report unsaved frame policy work to window-close coordination
                state: () =>
                    (this.$refs.framePolicy as HTMLDialogElement | undefined)?.open &&
                    this.mode !== this.baseline
                        ? 'dirty'
                        : 'clean',
                // Discard the current frame policy draft and release temporary state
                discard: () => this.close(),
                // Release temporary frame policy state during close coordination
                release: () => this.close(),
            });
        },
        // Detach frame policy listeners and release owned state
        destroy() {
            this.unregisterCloseOwner?.();
        },
        // Read the frame-update modes supported by the selected device
        get supported() {
            return Boolean(app().device?.manifest.capabilities.frame_output?.frame_scheduling);
        },
        // Group available frame policies for selection
        get groupedSupported() {
            return Boolean(app().device?.manifest.capabilities.frame_output?.grouped_key_updates);
        },
        // Resolve the selected frame-policy mode
        get selectedMode() {
            const policy = app().deviceOutput.frame_policy;
            if (!policy) return this.groupedSupported ? 'grouped_key_updates' : 'frame_scheduling';
            return policy.policy.mode === 'individual_updates'
                ? 'individual_updates'
                : policy.encoding === 'grouped_key_updates'
                  ? 'grouped_key_updates'
                  : 'frame_scheduling';
        },
        // Summarize the current configuration for display
        get summary() {
            return this.selectedMode === 'individual_updates'
                ? 'Individual updates'
                : this.selectedMode === 'grouped_key_updates'
                  ? 'Grouped key updates'
                  : 'Frame scheduling';
        },
        // Require a valid changed frame policy before saving
        get canSave() {
            return !this.saving;
        },
        // Open frame policy using the selected device configuration
        show(event?: Event) {
            if (!app().device) return;
            (event?.currentTarget as HTMLElement | null)?.focus();
            this.capturedId = app().deviceId;
            this.capturedSignature = JSON.stringify(app().deviceOutput);
            this.mode = this.selectedMode;
            this.error = '';
            this.baseline = this.mode;
            (this.$refs.framePolicy as HTMLDialogElement).showModal();
        },
        // Close frame policy and invalidate stale asynchronous work
        close() {
            if (this.saving) return;
            (this.$refs.framePolicy as HTMLDialogElement).close();
            this.error = '';
        },
        // Save the validated frame policy draft through the application bridge
        async save() {
            if (!this.canSave) return;
            if (
                app().deviceId !== this.capturedId ||
                JSON.stringify(app().deviceOutput) !== this.capturedSignature
            ) {
                this.error = 'Device settings changed. Cancel and reopen this dialog.';
                return;
            }
            this.saving = true;
            this.error = '';
            try {
                const output = {
                    ...cloneJson(app().deviceOutput),
                    frame_policy: framePolicyPreset(this.mode),
                };
                app().apply(
                    await app().command('set_device_output', { deviceId: this.capturedId, output }),
                );
                this.saving = false;
                this.close();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.saving = false;
            }
        },
    }));
}
