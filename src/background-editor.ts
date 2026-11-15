import { dockPreviewReason, startDockPreview, cancelDockPreview } from './dock-preview';
import { ownWindowMethods } from './window-work';
import { openMediaLibrary } from './media-library';
import { retainMediaFor, releaseMediaOwner } from './media-leases';
import Alpine from '@alpinejs/csp';
import { defaultMedia } from './bridge';
import { chooseMediaInput, droppedMedia, managedFormats, selectMediaInput } from './media-input';
import { app, bindingSignature } from './route-context';
import type { MediaOptions } from '../generated/runtime';
import type { Profile } from './types';
import { registerCloseDraft } from './close-releases';

// Capture background fields used to detect profile changes
const backgroundSignature = (profile: Profile) =>
    bindingSignature({
        asset: profile.background_asset || '',
        media: { ...defaultMedia(), ...profile.background_media },
    });

// Register the profile background editor
export function registerBackgroundEditor() {
    Alpine.data('backgroundEditor', () => ({
        dockToken: '',
        // Explain why background preview on hardware is unavailable
        get dockPreviewReason() {
            return dockPreviewReason(true);
        },
        // Preview the draft background on the current device
        async previewOnDock() {
            try {
                await cancelDockPreview(this.dockToken);
                const generation = this.generation;
                this.dockToken = await startDockPreview(
                    {
                        bindings: {},
                        background: {
                            asset: this.asset || null,
                            media: structuredClone(Alpine.raw(this.media)),
                        },
                    },
                    () => this.opened && generation === this.generation,
                );
            } catch (error) {
                this.error = String(error);
            }
        },
        opened: false,
        asset: '',
        media: defaultMedia(),
        baseline: '',
        profileId: '',
        deviceId: '',
        importing: false,
        saving: false,
        error: '',
        generation: 0,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Identify the owner retaining this editor’s media
        get mediaOwner() {
            return `background:${this.deviceId}|${this.profileId}`;
        },
        // Describe the selected media for preview controls
        get mediaContent() {
            return JSON.stringify(this.media);
        },
        // Describe the profile whose background is being edited
        get subtitle() {
            return app().profile
                ? `Background for “${app().profile!.name}”. Only button and touch screens display it.`
                : '';
        },
        // Check whether the draft can be saved to the current profile
        get canSave() {
            return (
                this.opened &&
                this.current() &&
                !this.importing &&
                !this.saving &&
                bindingSignature({ asset: this.asset, media: this.media }) !== this.baseline
            );
        },
        // Check that the editor still owns the current profile context
        get contextCurrent() {
            return !this.opened || this.current();
        },
        // Connect background draft tracking and context watchers
        init() {
            this.unregisterCloseOwner = registerCloseDraft(this, ['save'], {
                name: 'Device background',
                // Report whether the background draft has unsaved changes
                state: () =>
                    this.opened &&
                    bindingSignature({ asset: this.asset, media: this.media }) !== this.baseline
                        ? 'dirty'
                        : 'clean',
                // Discard the draft through normal editor closure
                discard: () => this.close(),
                // Release background preview resources through normal closure
                release: () => this.close(),
            });
            ownWindowMethods(this, 'background-editor', ['choose', 'save']);
            this.$watch('$store.app.route', () => this.close());
            this.$watch('$store.app.deviceId', () => this.close());
            // Close the editor when its profile context changes
            this.$watch('contextCurrent', () => {
                if (!this.contextCurrent) this.close();
            });
        },
        // Clear the draft when its selected media is deleted
        mediaDeleted(event: CustomEvent<{ asset: string }>) {
            if (!this.opened) return;
            const { asset } = event.detail;
            if (this.asset === asset) {
                this.asset = '';
                this.media = defaultMedia();
            }
            const baseline = JSON.parse(this.baseline || '{}');
            if (baseline.asset === asset) {
                baseline.asset = '';
                baseline.media = defaultMedia();
                this.baseline = bindingSignature(baseline);
            }
        },
        // Check that the original device and profile remain selected
        current() {
            return (
                app().deviceId === this.deviceId &&
                app().profile?.id === this.profileId &&
                Boolean(app().device?.manifest.capabilities.background)
            );
        },
        // Open a background draft for the active profile
        open(event: CustomEvent<{ choose?: boolean }>) {
            if (
                this.saving ||
                app().busy ||
                !app().profile ||
                !app().device?.manifest.capabilities.background
            )
                return;
            this.close();
            this.deviceId = app().deviceId;
            this.profileId = app().profile!.id;
            this.asset = app().profile!.background_asset || '';
            this.media = structuredClone({
                ...defaultMedia(),
                ...Alpine.raw(app().profile!.background_media),
            });
            this.baseline = backgroundSignature(app().profile!);
            this.opened = true;
            if (this.asset) {
                const generation = this.generation;
                // Report failure to retain the draft’s media asset
                void retainMediaFor(app(), this.asset, this.mediaOwner).catch((error) => {
                    if (this.opened && generation === this.generation) this.error = String(error);
                });
            }
            (this.$refs.dialog as HTMLDialogElement).showModal();
            this.$nextTick(() => (this.$refs.choose as HTMLButtonElement).focus());
            if (event.detail?.choose) void this.choose();
        },
        // Close the editor and release retained media and device preview
        close() {
            const dockRelease = cancelDockPreview(this.dockToken);
            this.dockToken = '';
            const release = this.profileId
                ? releaseMediaOwner(app(), this.mediaOwner)
                : Promise.resolve();
            this.generation++;
            this.opened = false;
            this.importing = false;
            this.asset = '';
            this.error = '';
            (this.$refs.dialog as HTMLDialogElement)?.close();
            // Resolve closure after both preview and media release complete
            return Promise.all([dockRelease, release]).then(() => {});
        },
        // Cancel background editing without saving the draft
        cancel() {
            if (!this.saving) this.close();
        },
        // Dispose the background editor’s registrations and resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.close();
        },
        // Open the media library for background selection
        browseMedia() {
            if (!this.opened || !this.current() || this.importing || this.saving) return;
            const generation = this.generation;
            // Reject media selections after the editor session becomes stale
            const current = () => this.opened && generation === this.generation && this.current();
            openMediaLibrary({
                owner: this.mediaOwner,
                current,
                formats: managedFormats(app().device!.manifest.capabilities.media_formats),
                // Apply a library media selection to the active background draft
                choose: (asset, options) => {
                    if (!current()) return;
                    this.asset = asset;
                    this.media = { ...defaultMedia(), ...options };
                },
            });
        },
        // Open the background media selection flow
        choose() {
            return this.selectInput();
        },
        // Import dropped media into the background draft
        dropMedia(event: CustomEvent<File[]>) {
            return this.selectInput(event);
        },
        // Import selected files while preserving editor ownership
        async selectInput(event?: CustomEvent<File[]>) {
            if (!this.opened || this.importing || this.saving || !this.current()) return;
            const generation = this.generation;
            // Check that an asynchronous media import still belongs to this editor
            const current = () => this.opened && generation === this.generation && this.current();
            this.importing = true;
            this.error = '';
            try {
                const formats = managedFormats(app().device!.manifest.capabilities.media_formats);
                const input = event ? droppedMedia(event) : await chooseMediaInput(formats);
                if (!input || !current()) return;
                await selectMediaInput(app(), input, {
                    owner: this.mediaOwner,
                    formats,
                    current,
                    // Apply imported media only to the active background draft
                    apply: (asset, options) => {
                        this.asset = asset;
                        this.media = options;
                    },
                });
            } catch (error) {
                if (current()) this.error = String(error);
            } finally {
                if (generation === this.generation) this.importing = false;
            }
        },
        // Update the draft’s media options from the child editor
        updateMedia(event: CustomEvent<{ options: MediaOptions; asset?: string }>) {
            if (event.detail.asset) this.asset = event.detail.asset;
            this.media = structuredClone(Alpine.raw(event.detail.options));
        },
        // Restore default background media options
        reset() {
            if (!this.saving && !this.importing) this.media = defaultMedia();
        },
        // Remove the background asset from the draft
        remove() {
            if (!this.saving && !this.importing) {
                this.asset = '';
                this.media = defaultMedia();
            }
        },
        // Save the background draft to the owning profile
        async save() {
            if (!this.canSave) return;
            const generation = this.generation;
            this.saving = true;
            this.error = '';
            try {
                await app().flushDrafts?.();
                if (!this.opened || generation !== this.generation || !this.current()) return;
                if (backgroundSignature(app().profile!) !== this.baseline)
                    throw Error(
                        'The device background changed while you were editing. Close and reopen the editor to review the latest version.',
                    );
                const profile = structuredClone(Alpine.raw(app().profile!));
                if (this.asset) profile.background_asset = this.asset;
                else delete profile.background_asset;
                profile.background_media = structuredClone(Alpine.raw(this.media));
                await app().edit([{ kind: 'save_profile', profile }], 'Edit device background');
                this.close();
                app().notify(
                    'Device background saved. Use Undo to restore the previous background.',
                    'success',
                );
            } catch (error) {
                if (generation === this.generation) this.error = String(error);
            } finally {
                this.saving = false;
            }
        },
    }));
}
