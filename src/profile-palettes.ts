import type AlpineType from '@alpinejs/csp';
import type { ProfilePalette } from '../generated/runtime';
import type { AppState } from './state';
import type { registerTheme } from './theme';
import { palettesFor, type ThemeMode } from './theme-style';
import { ownWindowMethods } from './window-work';
import { registerCloseDraft } from './close-releases';

// Create a profile palette selection that inherits global defaults
const defaults = (): ProfilePalette => ({
    target: 'global',
    light_palette: 'default',
    dark_palette: 'default',
});

// Register the profile palettes
export function registerProfilePalettes(Alpine: typeof AlpineType, app: () => AppState) {
    // Resolve the current profile palettes appearance
    const theme = () => Alpine.store('theme') as ReturnType<typeof registerTheme>;
    Alpine.data('profilePalettes', () => ({
        profileId: '',
        draft: defaults(),
        baseline: '',
        savedBaseline: '',
        busy: false,
        error: '',
        unregisterClose: undefined as (() => void) | undefined,
        // Derive profiles from the current profile palettes state
        get profiles() {
            return app().snapshot.profiles;
        },
        // Derive profile from the current profile palettes state
        get profile() {
            return this.profiles.find((profile) => profile.id === this.profileId);
        },
        // Derive saved value from the current profile palettes state
        get savedValue() {
            return JSON.stringify(this.profile?.palette || null);
        },
        // Derive dirty from the current profile palettes state
        get dirty() {
            return JSON.stringify(this.draft) !== this.baseline;
        },
        // Derive uses global from the current profile palettes state
        get usesGlobal() {
            return this.draft.target === 'global';
        },
        // Derive light options from the current profile palettes state
        get lightOptions() {
            return this.options('light');
        },
        // Derive dark options from the current profile palettes state
        get darkOptions() {
            return this.options('dark');
        },
        // List compatible palettes while retaining an unavailable selected ID as a visible fallback
        options(mode: ThemeMode) {
            const options = palettesFor(mode, theme().customPalettes);
            const selected = mode === 'light' ? this.draft.light_palette : this.draft.dark_palette;
            if (!options.some((option) => option.id === selected))
                options.push({ id: selected, label: 'Unavailable palette (uses Default)' });
            return options;
        },
        // Initialize profile palettes state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'profile palettes', ['save']);
            this.profileId = app().profile?.id || this.profiles[0]?.id || '';
            this.load();
            this.$watch('savedValue', () => {
                if (!this.dirty && !this.busy) this.load();
            });
            this.unregisterClose = registerCloseDraft(this, ['save'], {
                name: 'Profile palettes',
                retainOnUnmount: true,
                // Return a snapshot of the current profile palettes state
                state: () => (this.dirty ? 'dirty' : 'clean'),
                // Discard pending profile palettes edits without saving them
                discard: () => this.load(),
            });
        },
        // Release profile palettes state and associated lifecycle resources
        destroy() {
            this.unregisterClose?.();
        },
        // Choose the active item for the profile palettes workflow
        choose(event: Event) {
            this.profileId = (event.currentTarget as HTMLSelectElement).value;
            this.load();
        },
        // Load the data required by the profile palettes workflow
        load() {
            this.draft = structuredClone(Alpine.raw(this.profile?.palette || defaults()));
            this.baseline = JSON.stringify(this.draft);
            this.savedBaseline = this.savedValue;
            this.error = '';
        },
        // Save pending changes through the profile palettes workflow
        async save() {
            if (this.busy || !this.dirty) return;
            this.busy = true;
            this.error = '';
            try {
                if (!this.profile)
                    throw Error('This profile no longer exists. Choose another profile.');
                if (this.savedValue !== this.savedBaseline)
                    throw Error(
                        'This profile’s palettes changed. Reset to load the latest settings.',
                    );
                const profile = structuredClone(Alpine.raw(this.profile));
                profile.palette = structuredClone(Alpine.raw(this.draft));
                await app().edit([{ kind: 'save_profile', profile }], 'Edit profile palettes');
                this.load();
                app().notify('Profile palettes saved.', 'success');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
    }));
}
