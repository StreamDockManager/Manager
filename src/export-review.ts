import { cloneJson } from './json-clone';
import * as fileDialogs from './window-dialogs';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { ExportReviewRequest, ExportReview, ExportField } from '../generated/runtime';
import { native } from './bridge';
import { paginate } from './pagination';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { ownWindowMethods } from './window-work';
// Request an export review dialog for the supplied export request
export function openExportReview(request: ExportReviewRequest) {
    window.dispatchEvent(new CustomEvent('open-export-review', { detail: { request } }));
}
// Register the staged export review dialog and its lifecycle
export function registerExportReview(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('exportReview', () => ({
        opened: false,
        busy: false,
        writing: false,
        picking: false,
        epoch: 0,
        page: 1,
        report: null as ExportReview | null,
        reviewed: false,
        error: '',
        savedReceipt: '',
        field: null as ExportField | null,
        operation: 'replace',
        replacement: '',
        name: '',
        editError: '',
        baseline: '',
        fieldBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Capture the current field editor draft for unsaved-change detection
        fieldCloseValue() {
            return JSON.stringify([this.operation, this.replacement, this.name]);
        },
        // Paginate the fields available for export review
        get fieldPage() {
            return paginate(this.report?.fields || [], this.page, 8);
        },
        // Require a reviewed idle export before allowing file output
        get canWrite() {
            return Boolean(
                this.report && this.reviewed && !this.busy && !this.writing && !this.picking,
            );
        },
        // Identify operations that require a parameter or input name
        get needsName() {
            return ['parameter', 'input'].includes(this.operation);
        },
        // Attach export review listeners and window-close coordination
        init() {
            ownWindowMethods(this, 'export review', ['open', 'applyField', 'write']);
            this.unregisterCloseOwner = registerCloseDraft(this, ['open', 'applyField', 'write'], {
                name: 'Export review',
                retainOnUnmount: true,
                // Report unsaved export review work to window-close coordination
                state: () =>
                    this.opened &&
                    ((this.report && JSON.stringify(this.report.fields) !== this.baseline) ||
                        (this.field && this.fieldCloseValue() !== this.fieldBaseline))
                        ? 'dirty'
                        : 'clean',
                // Discard the current export review draft and release temporary state
                discard: () => this.close(),
                // Release temporary export review state during close coordination
                release: () => this.close(),
            });
            this.$watch('$store.app.route', () => this.close());
        },
        // Format the field value according to its declared JSON value type
        literal(field: ExportField) {
            return field.value_type === 'string'
                ? String(field.value)
                : JSON.stringify(field.value, null, 2);
        },
        // Distinguish an empty text value from other displayed literals
        displayLiteral(field: ExportField) {
            return field.value === '' ? '(empty text)' : this.literal(field);
        },
        // Release temporary export review state during close coordination
        async release(id: string) {
            await trackCloseRelease(`export-review:${id}`, () =>
                app().bridge.command('discard_export_review', { id }),
            );
        },
        // Close export review and invalidate stale asynchronous work
        close() {
            if (this.writing) return;
            ++this.epoch;
            this.opened = false;
            this.busy = false;
            this.picking = false;
            this.field = null;
            this.reviewed = false;
            if (this.report) void this.release(this.report.id).catch(() => {});
            if (this.savedReceipt) {
                const id = this.savedReceipt;
                this.savedReceipt = '';
                void trackCloseRelease('archive:' + id, () =>
                    app().bridge.command('release_archive_export', { id }),
                ).catch(() => {});
            }
            this.report = null;
            (this.$refs.edit as HTMLDialogElement)?.close();
            (this.$refs.dialog as HTMLDialogElement)?.close();
        },
        // Detach export review listeners and release owned state
        destroy() {
            this.unregisterCloseOwner?.();
            this.close();
        },
        // Open export review for the requested item
        async open(event: CustomEvent<{ request: ExportReviewRequest }>) {
            if (this.writing) return;
            this.close();
            const epoch = this.epoch;
            this.opened = true;
            this.busy = true;
            this.page = 1;
            this.error = '';
            (this.$refs.dialog as HTMLDialogElement).showModal();
            try {
                const report = await app().bridge.command('prepare_export_review', {
                    request: cloneJson(event.detail.request),
                });
                if (epoch !== this.epoch || !this.opened) {
                    void this.release(report.id).catch(() => {});
                    return;
                }
                this.report = report;
                this.baseline = JSON.stringify(report.fields);
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
            } finally {
                if (epoch === this.epoch) this.busy = false;
            }
        },
        // Move to the previous export review result page
        previous() {
            this.page = Math.max(1, this.page - 1);
        },
        // Move to the next export review result page
        next() {
            this.page = Math.min(this.fieldPage.pages, this.page + 1);
        },
        // Open the selected export field for replacement or parameterization
        editField(event: Event) {
            if (this.busy || this.writing || !this.report) return;
            const key = (event.currentTarget as HTMLElement).dataset.key;
            this.field = this.report.fields.find((field) => field.key === key) || null;
            if (!this.field) return;
            this.operation = 'replace';
            this.replacement = this.literal(this.field);
            this.name = '';
            this.fieldBaseline = this.fieldCloseValue();
            this.editError = '';
            (this.$refs.edit as HTMLDialogElement).showModal();
            this.$nextTick(() => (this.$refs.replacement as HTMLTextAreaElement).focus());
        },
        // Dismiss the field editor without applying its draft
        cancelEdit() {
            if (this.busy) return;
            this.field = null;
            (this.$refs.edit as HTMLDialogElement)?.close();
        },
        // Apply the reviewed field operation to the staged export
        async applyField() {
            if (!this.report || !this.field || this.busy) return;
            const epoch = this.epoch;
            this.busy = true;
            this.editError = '';
            this.reviewed = false;
            try {
                let replacement: import('../generated/runtime').JsonValue = null;
                if (this.operation === 'replace')
                    replacement =
                        this.field.value_type === 'string'
                            ? this.replacement
                            : JSON.parse(this.replacement);
                const report = await app().bridge.command('update_export_review', {
                    id: this.report.id,
                    expectedRevision: this.report.revision,
                    change: {
                        key: this.field.key,
                        operation: this.operation,
                        replacement,
                        name: this.name,
                    },
                });
                if (epoch !== this.epoch || !this.opened) {
                    void this.release(report.id).catch(() => {});
                    return;
                }
                this.report = report;
                this.page = Math.min(this.page, this.fieldPage.pages);
                this.busy = false;
                this.cancelEdit();
                this.$nextTick(() => (this.$refs.ack as HTMLInputElement).focus());
            } catch (error) {
                if (epoch === this.epoch) this.editError = String(error);
            } finally {
                if (epoch === this.epoch) this.busy = false;
            }
        },
        // Choose an export destination only when a reviewed export is ready
        async write() {
            if (!this.canWrite || !this.report) return;
            if (!native) {
                this.error = 'Saving a reviewed archive requires the native desktop application.';
                return;
            }
            const epoch = this.epoch;
            this.picking = true;
            try {
                const { save } = fileDialogs;
                const path = await save({
                    defaultPath: `stream-dock-manager-${this.report.kind}.zip`,
                    filters: [{ name: 'Stream Dock Manager archive', extensions: ['zip'] }],
                });
                this.picking = false;
                if (path && this.opened && epoch === this.epoch) await this.writeTo(path);
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
            } finally {
                if (epoch === this.epoch) this.picking = false;
            }
        },
        // Start a native drag for the saved export file
        async dragSaved(event: MouseEvent) {
            if (event.button !== 0) return;
            event.preventDefault();
            if (!this.savedReceipt) return;
            try {
                await app().bridge.command('start_archive_drag', { receipt: this.savedReceipt });
            } catch (error) {
                this.error = String(error);
            }
        },
        // Write the staged export to the chosen destination and report the result
        async writeTo(path: string) {
            if (!this.canWrite || !this.report) return;
            this.writing = true;
            this.error = '';
            try {
                const receipt = await app().bridge.command('write_export_review', {
                    id: this.report.id,
                    expectedRevision: this.report.revision,
                    sha256: this.report.archive_sha256,
                    path,
                });
                this.report = null;
                this.writing = false;
                if (receipt) {
                    this.savedReceipt = receipt;
                    this.$nextTick(() => (this.$refs.done as HTMLButtonElement)?.focus());
                } else this.close();
                app().notify('Reviewed archive saved.', 'success');
            } catch (error) {
                this.error = String(error);
                this.reviewed = false;
            } finally {
                this.writing = false;
            }
        },
    }));
}
