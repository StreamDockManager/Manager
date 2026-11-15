import * as fileDialogs from './window-dialogs';
import { ownWindowMethods } from './window-work';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { BootLogoPreview } from '../generated/runtime';
import { native } from './bridge';
type BootLogoController = ReturnType<typeof createBootLogoDraft> & {
    $watch: (property: string, callback: () => void) => void;
    $refs: Record<string, HTMLElement>;
    close: () => Promise<void>;
    closed: () => Promise<void>;
    save: () => Promise<void>;
    unregisterCloseOwner?: () => void;
};

// Create a boot-logo draft with device-scoped preview ownership
export function createBootLogoDraft(app: () => AppState) {
    return {
        preview: null as BootLogoPreview | null,
        fit: 'contain',
        path: '',
        deviceId: '',
        deviceKey: '',
        contractKey: '',
        preparing: false,
        saving: false,
        error: '',
        generation: 0,
        // Report whether the selected device supports boot-logo changes
        get supported() {
            return Boolean(app().device?.manifest.capabilities.boot_logo);
        },
        // Expose the target boot-logo dimensions
        get dimensions() {
            return app().device?.manifest.capabilities.boot_logo?.size.join(' × ') || '';
        },
        // Check whether the prepared logo can be applied
        get canApply() {
            return Boolean(this.preview && this.current() && !this.preparing && !this.saving);
        },
        // Check whether the prepared preview still matches the target
        get previewCurrent() {
            return !this.preview || this.current();
        },
        // Check the original device identity, hardware contract and connection
        current() {
            return (
                app().deviceId === this.deviceId &&
                app().device?.persistence_key === this.deviceKey &&
                this.contractKey ===
                    JSON.stringify([
                        app().device?.variant_id,
                        app().device?.firmware,
                        app().device?.manifest,
                    ]) &&
                Boolean(app().device?.connected)
            );
        },
        // Discard the prepared boot-logo preview
        async discard() {
            if (this.saving) return false;
            this.generation++;
            const id = this.preview?.id;
            this.preview = null;
            this.path = '';
            this.error = '';
            this.preparing = false;
            if (id) {
                await trackCloseRelease('boot-logo:' + id, () =>
                    app().bridge.command('cancel_boot_logo', { id }),
                );
            }
            return true;
        },
        // Prepare a selected image for the current device
        async prepare(path: string) {
            if (!this.supported || this.saving || this.preparing || !app().device) return;
            const generation = ++this.generation;
            const oldId = this.preview?.id;
            this.preview = null;
            this.deviceId = app().deviceId;
            this.deviceKey = app().device!.persistence_key;
            this.contractKey = JSON.stringify([
                app().device?.variant_id,
                app().device?.firmware,
                app().device?.manifest,
            ]);
            this.path = path;
            this.preparing = true;
            this.error = '';
            try {
                if (oldId)
                    await trackCloseRelease('boot-logo:' + oldId, () =>
                        app().bridge.command('cancel_boot_logo', { id: oldId }),
                    );
                const preview = await app().bridge.command('prepare_boot_logo', {
                    deviceId: this.deviceId,
                    path,
                    fit: this.fit,
                });
                if (generation !== this.generation || !this.current()) {
                    await trackCloseRelease('boot-logo:' + preview.id, () =>
                        app().bridge.command('cancel_boot_logo', { id: preview.id }),
                    );
                    return;
                }
                this.preview = preview;
            } catch (error) {
                if (generation === this.generation) this.error = String(error);
            } finally {
                if (generation === this.generation) this.preparing = false;
            }
        },
        // Choose a local image and prepare a logo preview
        async choose() {
            if (this.preparing || this.saving) return;
            const id = app().deviceId;
            const generation = this.generation;
            try {
                let path: string | null = 'startup-preview.png';
                if (native) {
                    const { open } = fileDialogs;
                    const selected = await open({
                        multiple: false,
                        filters: [{ name: 'Startup logo', extensions: ['png', 'jpg', 'jpeg'] }],
                    });
                    path = typeof selected === 'string' ? selected : null;
                }
                if (path && app().deviceId === id && this.generation === generation)
                    await this.prepare(path);
            } catch (error) {
                if (this.generation === generation) this.error = String(error);
            }
        },
        // Reprepare the preview after the fit mode changes
        async changeFit() {
            if (this.path) await this.prepare(this.path);
        },
        // Commit the prepared boot logo to the selected device
        async applyLogo() {
            if (!this.canApply || !this.preview) return false;
            this.saving = true;
            this.error = '';
            try {
                const result = await app().bridge.command('commit_boot_logo', {
                    id: this.preview.id,
                });
                this.preview = null;
                this.path = '';
                app().notify(result.message, 'success');
                return true;
            } catch (error) {
                // A failed response may follow a partially completed persistent write
                // The server consumes this draft; require an explicitly prepared new one
                this.preview = null;
                this.path = '';
                this.error = String(error);
                return false;
            } finally {
                this.saving = false;
            }
        },
    };
}

// Register the boot-logo dialog controller
export function registerBootLogo(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('bootLogo', () =>
        Object.assign(createBootLogoDraft(app), {
            // Read the current application state
            get app() {
                return app();
            },
            // Connect boot-logo draft protection and context watchers
            init(this: BootLogoController) {
                ownWindowMethods(this, 'boot logo', [
                    'discard',
                    'prepare',
                    'choose',
                    'changeFit',
                    'applyLogo',
                    'close',
                    'closed',
                    'save',
                ]);
                this.unregisterCloseOwner = registerCloseDraft(this, ['applyLogo'], {
                    name: 'Boot logo',
                    // Report whether a logo selection or preview is pending
                    state: () => (this.path || this.preview ? 'dirty' : 'clean'),
                    // Discard the logo draft by closing the editor
                    discard: () => this.close(),
                    // Release prepared logo resources by closing the editor
                    release: () => this.close(),
                });
                // Close the logo editor when the selected device changes
                this.$watch('$store.app.deviceId', () => {
                    // Ignore secondary close failures after a device switch
                    void this.close().catch(() => {});
                });
                // Close the logo editor when navigating away
                this.$watch('$store.app.route', () => {
                    // Ignore secondary close failures during navigation
                    void this.close().catch(() => {});
                });
                // Close the editor when its preview context becomes stale
                this.$watch('previewCurrent', () => {
                    // Ignore secondary close failures for a stale preview
                    if (!this.previewCurrent) void this.close().catch(() => {});
                });
            },
            // Open the boot-logo editor from its triggering control
            show(this: BootLogoController, event?: Event) {
                if (!this.supported || this.saving) return;
                (event?.currentTarget as HTMLElement | null)?.focus();
                this.fit = 'contain';
                this.error = '';
                (this.$refs.logo as HTMLDialogElement).showModal();
            },
            // Close the dialog and release its prepared logo
            async close(this: BootLogoController) {
                const dialog = this.$refs.logo as HTMLDialogElement | undefined;
                if (await this.discard()) dialog?.close();
            },
            // Release the draft after the dialog has closed
            async closed(this: BootLogoController) {
                await this.discard();
            },
            // Apply the prepared logo and close the editor
            async save(this: BootLogoController) {
                const dialog = this.$refs.logo as HTMLDialogElement | undefined;
                if (await this.applyLogo()) dialog?.close();
            },
            // Dispose logo draft protection and release its preview
            destroy(this: BootLogoController) {
                this.unregisterCloseOwner?.();
                // Ignore release failures during controller destruction
                void this.discard().catch(() => {});
            },
        }),
    );
}
