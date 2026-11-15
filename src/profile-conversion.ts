import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { ConversionPreview, ConversionRow } from '../generated/runtime';
import { paginate } from './pagination';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { ownWindowMethods } from './window-work';
// Register the profile conversion
export function registerProfileConversion(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('profileConversion', () => ({
        opened: false,
        busy: false,
        committing: false,
        epoch: 0,
        sourceId: '',
        sourceName: '',
        sourceModel: '',
        targetModel: '',
        name: '',
        keepBackground: true,
        rows: [] as ConversionRow[],
        report: null as ConversionPreview | null,
        reviewed: false,
        page: 1,
        error: '',
        baseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Close the value
        closeValue() {
            return JSON.stringify([this.targetModel, this.name, this.keepBackground, this.rows]);
        },
        // Derive models from the current profile conversion state
        get models() {
            return app().models.filter((model) => model.model_id !== this.sourceModel);
        },
        // Derive row page from the current profile conversion state
        get rowPage() {
            return paginate(this.rows, this.page, 12);
        },
        // Initialize profile conversion state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'profile conversion', ['prepare', 'review', 'commit']);
            this.unregisterCloseOwner = registerCloseDraft(this, ['prepare', 'review', 'commit'], {
                name: 'Profile conversion',
                retainOnUnmount: true,
                // Return a snapshot of the current profile conversion state
                state: () =>
                    this.opened && this.closeValue() !== this.baseline ? 'dirty' : 'clean',
                // Discard pending profile conversion edits without saving them
                discard: () => this.close(),
                // Release resources retained by the profile conversion operation
                release: () => this.close(),
            });
            this.$watch('$store.app.route', () => this.close());
            // Complete the deferred profile conversion work using the current operation state
            this.$watch('$store.app.snapshot.configuration_revision', () => {
                if (!this.opened || this.committing || !this.report) return;
                this.reviewed = false;
                this.error = 'Workspace changed. Start a new conversion review.';
            });
        },
        // Open the current profile conversion target
        open() {
            if (this.committing || !app().profile) return;
            this.close();
            this.opened = true;
            this.sourceId = app().profile!.id;
            this.sourceName = app().profile!.name;
            this.sourceModel = app().profile!.model_id;
            this.targetModel = '';
            this.name = '';
            this.keepBackground = true;
            this.baseline = this.closeValue();
            this.error = '';
            (this.$refs.dialog as HTMLDialogElement).showModal();
        },
        // Release resources retained by the profile conversion operation
        async release(id: string) {
            await trackCloseRelease(`profile-conversion:${id}`, () =>
                app().bridge.command('discard_profile_conversion', { id }),
            );
        },
        // Release profile conversion state and associated lifecycle resources
        close() {
            if (this.committing) return;
            ++this.epoch;
            this.opened = false;
            if (this.report) void this.release(this.report.id).catch(() => {});
            this.report = null;
            this.rows = [];
            this.reviewed = false;
            this.busy = false;
            (this.$refs.dialog as HTMLDialogElement)?.close();
        },
        // Release profile conversion state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.close();
        },
        // React to changed profile conversion state
        changed() {
            this.reviewed = false;
        },
        // Prepare state required by the profile conversion operation
        async prepare() {
            if (this.committing) return;
            const epoch = ++this.epoch;
            const old = this.report?.id;
            this.report = null;
            this.rows = [];
            this.reviewed = false;
            this.error = '';
            this.page = 1;
            if (old) void this.release(old).catch(() => {});
            if (!this.targetModel) return;
            this.busy = true;
            try {
                await app().flushDrafts?.();
                const report = await app().bridge.command('prepare_profile_conversion', {
                    expectedRevision: app().snapshot.configuration_revision,
                    profileId: this.sourceId,
                    targetModel: this.targetModel,
                });
                if (epoch !== this.epoch) {
                    void this.release(report.id).catch(() => {});
                    return;
                }
                this.report = report;
                this.rows = report.rows;
                this.name = report.name;
                this.keepBackground = report.keep_background;
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
            } finally {
                if (epoch === this.epoch) this.busy = false;
            }
        },
        // Review the pending profile conversion changes
        async review() {
            if (!this.report || this.busy) return;
            const epoch = this.epoch;
            this.busy = true;
            this.error = '';
            this.reviewed = false;
            try {
                const report = await app().bridge.command('update_profile_conversion', {
                    id: this.report.id,
                    name: this.name,
                    mappings: Object.fromEntries(
                        this.rows.map((row) => [row.source_control, row.target]),
                    ),
                    keepBackground: this.keepBackground,
                });
                if (epoch !== this.epoch) return;
                this.report = report;
                this.rows = report.rows;
                this.name = report.name;
                this.reviewed = report.ready;
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
            } finally {
                if (epoch === this.epoch) this.busy = false;
            }
        },
        // Commit the reviewed profile conversion changes
        async commit() {
            if (!this.report?.ready || !this.reviewed || this.busy) return;
            this.busy = true;
            this.committing = true;
            this.error = '';
            try {
                app().apply(
                    await app().bridge.command('commit_profile_conversion', {
                        id: this.report.id,
                    }),
                );
                this.report = null;
                this.committing = false;
                this.close();
                app().notify(
                    'Converted profile created. Select it on a device or simulator for its model.',
                    'success',
                );
            } catch (error) {
                this.reviewed = false;
                this.error = String(error);
            } finally {
                this.busy = false;
                this.committing = false;
            }
        },
        // Select the previous profile conversion item
        previous() {
            this.page = Math.max(1, this.rowPage.page - 1);
        },
        // Select the next profile conversion item
        next() {
            this.page = Math.min(this.rowPage.pages, this.rowPage.page + 1);
        },
    }));
}
