import type { SetupCapabilityJob, SetupCapabilityMode } from '../generated/runtime';
import type { AppState } from './state';
import { reconcile } from './reconcile';
import { trackCloseRelease } from './close-releases';

// Create the setup capabilities
export function createSetupCapabilities(app: () => AppState) {
    return {
        deviceId: '',
        pluginIds: [] as string[],
        job: null as SetupCapabilityJob | null,
        error: '',
        pending: false,
        epoch: 0,
        polling: -1,
        timer: 0,
        // Derive busy from the current setup capabilities state
        get busy() {
            return this.pending || this.job?.state === 'running';
        },
        // Open the current setup capabilities target
        async open(device: string) {
            const epoch = this.epoch + 1;
            await this.close();
            if (epoch !== this.epoch) return;
            this.deviceId = device;
            this.pluginIds = [];
            this.job = null;
            this.error = '';
        },
        // Start the requested setup capabilities operation
        async start(mode: SetupCapabilityMode) {
            if (this.busy) return;
            const epoch = ++this.epoch;
            this.pending = true;
            this.error = '';
            try {
                const job = await app().bridge.command('start_setup_capabilities', {
                    request: {
                        mode,
                        device_id: this.deviceId || null,
                        plugin_ids: [...this.pluginIds],
                    },
                });
                if (epoch !== this.epoch) {
                    await trackCloseRelease(`setup-capabilities:${job.id}`, () =>
                        app().bridge.command('cancel_setup_capabilities', { id: job.id }),
                    );
                    return;
                }
                this.job = job;
                this.timer = window.setInterval(() => void this.refresh(), 500);
                await this.refresh();
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
            } finally {
                if (epoch === this.epoch) this.pending = false;
            }
        },
        // Refresh setup capabilities data while keeping the view synchronized
        async refresh() {
            const epoch = this.epoch;
            const id = this.job?.id;
            if (!id || this.polling === epoch) return;
            this.polling = epoch;
            try {
                const report = await app().bridge.command('setup_capability_status', { id });
                if (epoch !== this.epoch || id !== this.job?.id) return;
                reconcile(this.job, report);
                if (report.state !== 'running') window.clearInterval(this.timer);
            } catch (error) {
                if (epoch === this.epoch) {
                    this.error = String(error);
                    window.clearInterval(this.timer);
                }
            } finally {
                if (this.polling === epoch) this.polling = -1;
            }
        },
        // Cancel the active setup capabilities operation
        async cancel() {
            ++this.epoch;
            window.clearInterval(this.timer);
            this.pending = false;
            const job = this.job;
            if (!job || job.state !== 'running') return;
            this.job = {
                ...job,
                state: 'cancelled',
                checks: job.checks.map((check) =>
                    check.status === 'waiting'
                        ? { ...check, status: 'cancelled', detail: 'Input check cancelled.' }
                        : check,
                ),
            };
            const epoch = this.epoch;
            try {
                await trackCloseRelease(`setup-capabilities:${job.id}`, () =>
                    app().bridge.command('cancel_setup_capabilities', { id: job.id }),
                );
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
                throw error;
            }
        },
        // Release setup capabilities state and associated lifecycle resources
        async close() {
            await this.cancel();
        },
    };
}
