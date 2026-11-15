import { ensureKeyboardNavigation, KEYBOARD_NAVIGATION_PAGE } from './keyboard-navigation';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { Action, Profile } from './types';
import { defaultMedia } from './bridge';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
import { windowCloseOwners } from './window-close-owners';

// Register the profile creator
export function registerProfileCreator(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('profileCreator', () => ({
        name: '',
        firstPage: 'Main page',
        sourceId: '',
        modelId: '',
        modelName: '',
        deviceId: '',
        creating: false,
        error: '',
        baseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Close the value
        closeValue() {
            return JSON.stringify([this.name, this.firstPage, this.sourceId, this.modelId]);
        },
        // Derive app from the current profile creator state
        get app() {
            return app();
        },
        // Derive models from the current profile creator state
        get models() {
            return app().models;
        },
        // Clear profile-copy and device selections after changing the target model
        modelChanged() {
            this.sourceId = '';
            this.deviceId = '';
        },
        // Derive sources from the current profile creator state
        get sources() {
            return app().snapshot.profiles.filter((profile) => profile.model_id === this.modelId);
        },
        // Derive cannot create from the current profile creator state
        get cannotCreate() {
            return (
                this.creating ||
                !this.modelId ||
                !this.name.trim() ||
                (!this.sourceId && !this.firstPage.trim())
            );
        },
        // Initialize profile creator state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'profile creation', ['create']);
            this.unregisterCloseOwner = registerCloseDraft(this, ['create'], {
                name: 'New profile',
                retainOnUnmount: true,
                // Return a snapshot of the current profile creator state
                state: () =>
                    (this.$refs.dialog as HTMLDialogElement | undefined)?.open &&
                    this.closeValue() !== this.baseline
                        ? 'dirty'
                        : 'clean',
                // Discard pending profile creator edits without saving them
                discard: () => this.close(),
                // Release resources retained by the profile creator operation
                release: () => this.close(),
            });
            this.$watch('$store.app.route', () => this.cancel());
        },
        // Open the current profile creator target
        open() {
            if (this.creating) return;
            const device = app().device;
            this.modelId =
                device?.model_id || app().profile?.model_id || app().models[0]?.model_id || '';
            this.modelName = device?.manifest.name || this.modelId;
            this.deviceId = device?.id || '';
            this.name = '';
            this.firstPage = 'Main page';
            this.sourceId = '';
            this.error = '';
            this.baseline = this.closeValue();
            // Complete the deferred profile creator work using the current operation state
            this.$nextTick(() => {
                if (windowCloseOwners.closing) return;
                const dialog = this.$refs.dialog as HTMLDialogElement;
                if (!dialog.open) dialog.showModal();
                (this.$refs.name as HTMLInputElement).focus();
            });
        },
        // Release profile creator state and associated lifecycle resources
        close() {
            const dialog = this.$refs.dialog as HTMLDialogElement;
            if (dialog?.open) dialog.close();
        },
        // Cancel the active profile creator operation
        cancel() {
            if (!this.creating) this.close();
        },
        // Release profile creator state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.close();
        },
        // Create the profile creator operation
        async create() {
            if (this.cannotCreate) return;
            this.error = '';
            const name = this.name.trim();
            if (new TextEncoder().encode(name).length > 160) {
                this.error = 'Profile name is too long. Please use a shorter name.';
                return;
            }
            this.creating = true;
            let profile: Profile;
            try {
                await app().flushDrafts?.();
                if (
                    this.deviceId &&
                    !app().snapshot.devices.some(
                        (device) => device.id === this.deviceId && device.model_id === this.modelId,
                    )
                )
                    throw Error(
                        'This device is no longer connected. Close this dialog and select a device.',
                    );
                const id = crypto.randomUUID();
                if (this.sourceId) {
                    const source = this.sources.find((candidate) => candidate.id === this.sourceId);
                    if (!source)
                        throw Error(
                            'The source profile is no longer available. Choose another starting point.',
                        );
                    profile = structuredClone(Alpine.raw(source));
                    profile.id = id;
                    profile.name = name;
                    // Page IDs are scoped to their profile, so copied page actions keep working
                    const remap = (action: Action) => {
                        if (action.kind === 'profile' && action.profile_id === source.id)
                            action.profile_id = id;
                        if (action.kind === 'builtin' && 'children' in action.action)
                            action.action.children.forEach(remap);
                        if (action.kind === 'macro' || action.kind === 'dial')
                            action.actions.forEach(remap);
                        if (action.kind === 'toggle') {
                            remap(action.on);
                            remap(action.off);
                        }
                    };
                    for (const page of profile.pages)
                        for (const binding of Object.values(page.bindings))
                            if (binding) remap(binding.action);
                } else {
                    const pageId = crypto.randomUUID();
                    profile = {
                        id,
                        name,
                        model_id: this.modelId,
                        active_page: pageId,
                        pages: [{ id: pageId, name: this.firstPage.trim(), bindings: {} }],
                        background_media: defaultMedia(),
                    };
                }
                if (profile.model_id === 'k1-pro') {
                    ensureKeyboardNavigation(profile);
                    profile.active_page = KEYBOARD_NAVIGATION_PAGE;
                }
                await app().edit([{ kind: 'save_profile', profile }], `Create profile “${name}”`);
                this.close();
            } catch (error) {
                this.error = String(error);
                return;
            } finally {
                this.creating = false;
            }
            app().notify(`Created profile “${name}”.`, 'success');
            try {
                app().apply(
                    await app().command('activate_profile', {
                        id: profile.id,
                        deviceId: this.deviceId,
                    }),
                );
            } catch (error) {
                app().notify(
                    'Profile saved. Select it to activate it: ' + String(error),
                    'warning',
                );
            }
        },
    }));
}
