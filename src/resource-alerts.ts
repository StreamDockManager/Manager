import type { ResourceAlertSettings } from '../generated/runtime';
import type { AppState } from './state';

// Create an editable resource-alert draft with revision-safe loading and validated saving
export function resourceAlertDraft(app: () => AppState) {
    return {
        id: '',
        epoch: 0,
        settings: null as ResourceAlertSettings | null,
        enabled: false,
        cpu: '',
        memory: '',
        loading: false,
        writing: false,
        error: '',
        baseline: '',
        // Close the value
        closeValue() {
            return JSON.stringify([this.enabled, this.cpu, this.memory]);
        },
        // Derive dirty from the current resource alerts state
        get dirty() {
            return Boolean(this.settings && this.closeValue() !== this.baseline);
        },
        // Derive can save from the current resource alerts state
        get canSave() {
            return Boolean(this.settings && !this.loading && !this.writing);
        },
        // Release resource alerts state and associated lifecycle resources
        close() {
            ++this.epoch;
            this.id = '';
            this.settings = null;
            this.enabled = false;
            this.cpu = '';
            this.memory = '';
            this.loading = false;
            this.writing = false;
            this.error = '';
            this.baseline = '';
        },
        // Open the current resource alerts target
        open(id: string) {
            this.close();
            this.id = id;
            return this.reload();
        },
        // Apply the pending resource alerts changes
        apply(value: ResourceAlertSettings) {
            this.settings = value;
            this.enabled = value.preferences.enabled;
            this.cpu =
                value.preferences.cpu_percent == null ? '' : String(value.preferences.cpu_percent);
            this.memory =
                value.preferences.memory_mib == null ? '' : String(value.preferences.memory_mib);
            this.baseline = this.closeValue();
        },
        // Reload the current resource alerts data
        async reload() {
            if (!this.id || this.writing) return;
            const id = this.id;
            const epoch = ++this.epoch;
            this.loading = true;
            this.error = '';
            this.settings = null;
            try {
                const value = await app().bridge.command('resource_alert_settings', { id });
                if (epoch === this.epoch && id === this.id) this.apply(value);
            } catch (error) {
                if (epoch === this.epoch && id === this.id) this.error = String(error);
            } finally {
                if (epoch === this.epoch && id === this.id) this.loading = false;
            }
        },
        // Save pending changes through the resource alerts workflow
        async save() {
            if (!this.canSave || !this.settings) return;
            const cpu = this.cpu.trim() === '' ? null : Number(this.cpu);
            const memory = this.memory.trim() === '' ? null : Number(this.memory);
            if (
                (cpu != null && (!Number.isFinite(cpu) || cpu <= 0 || cpu > 100)) ||
                (memory != null &&
                    (!Number.isInteger(memory) || memory < 1 || memory > 1_048_576)) ||
                (this.enabled && cpu == null && memory == null)
            ) {
                this.error =
                    'Choose CPU above 0 and at most 100%, memory 1–1,048,576 MiB, and at least one enabled threshold.';
                return;
            }
            const id = this.id;
            const epoch = this.epoch;
            const value: ResourceAlertSettings = {
                ...this.settings,
                preferences: { enabled: this.enabled, cpu_percent: cpu, memory_mib: memory },
            };
            this.writing = true;
            this.error = '';
            try {
                const saved = await app().bridge.command('save_resource_alert_settings', { value });
                if (epoch !== this.epoch || id !== this.id) return;
                this.apply(saved);
                app().notify('Resource alert preferences saved.', 'success');
            } catch (error) {
                if (epoch === this.epoch && id === this.id) this.error = String(error);
            } finally {
                if (epoch === this.epoch && id === this.id) this.writing = false;
            }
        },
    };
}
