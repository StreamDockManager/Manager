import { openExportReview } from './export-review';
import Alpine from '@alpinejs/csp';
import { app } from './route-context';
import { native } from './bridge';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
// Register the profile sharing
export function registerProfileSharing() {
    Alpine.data('profileSharing', () => ({
        selected: [] as string[],
        opened: false,
        exporting: false,
        remaining: 3,
        readyAt: 0,
        timer: undefined as ReturnType<typeof setInterval> | undefined,
        error: '',
        generation: 0,
        baseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Derive profiles from the current profile sharing state
        get profiles() {
            return app().snapshot.profiles;
        },
        // Derive all selected from the current profile sharing state
        get allSelected() {
            return this.selected.length === this.profiles.length;
        },
        // Derive can include from the current profile sharing state
        get canInclude() {
            return this.remaining === 0 && this.selected.length > 0 && !this.exporting;
        },
        // Initialize profile sharing state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'profile export', ['export']);
            this.unregisterCloseOwner = registerCloseDraft(this, ['export'], {
                name: 'Profile export selection',
                // Return a snapshot of the current profile sharing state
                state: () =>
                    this.opened && JSON.stringify(this.selected) !== this.baseline
                        ? 'dirty'
                        : 'clean',
                // Discard pending profile sharing edits without saving them
                discard: () => this.cancel(),
                // Release resources retained by the profile sharing operation
                release: () => this.cancel(),
            });
            this.$watch('$store.app.route', () => this.cancel());
        },
        // Open the current profile sharing target
        open() {
            this.cancel();
            this.selected = app().profile ? [app().profile!.id] : [];
            this.baseline = JSON.stringify(this.selected);
            this.error = '';
            this.remaining = 3;
            this.readyAt = performance.now() + 3000;
            this.opened = true;
            // Refresh profile sharing state on the scheduled interval
            this.timer = setInterval(() => {
                this.remaining = Math.max(0, Math.ceil((this.readyAt - performance.now()) / 1000));
                if (!this.remaining) clearInterval(this.timer);
            }, 100);
            (this.$refs.dialog as HTMLDialogElement).showModal();
            (this.$refs.decline as HTMLButtonElement).focus();
        },
        // Select the all
        selectAll() {
            this.selected = this.allSelected ? [] : this.profiles.map((p) => p.id);
        },
        // Cancel the active profile sharing operation
        cancel() {
            ++this.generation;
            this.exporting = false;
            clearInterval(this.timer);
            this.opened = false;
            (this.$refs.dialog as HTMLDialogElement)?.close();
        },
        // Release profile sharing state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.cancel();
        },
        // Export the profile without including its saved settings
        withoutSettings() {
            return this.export(false);
        },
        // Export settings only after the sensitive-data review delay has elapsed
        withSettings() {
            if (performance.now() < this.readyAt) return;
            return this.export(true);
        },
        // Export the profile sharing operation
        async export(include: boolean) {
            if (this.exporting) return;
            if (!this.selected.length) {
                this.error = 'Select at least one profile.';
                return;
            }
            if (!native) {
                this.error = 'Profile export requires the desktop application.';
                return;
            }
            const generation = this.generation;
            const ids = [...this.selected];
            this.exporting = true;
            this.error = '';
            try {
                await app().flushDrafts?.();
                if (generation !== this.generation) return;
                this.exporting = false;
                this.cancel();
                openExportReview({ kind: 'profiles', ids, include_plugin_settings: include });
            } catch (error) {
                if (generation === this.generation) this.error = String(error);
            } finally {
                if (generation === this.generation) this.exporting = false;
            }
        },
    }));
}
