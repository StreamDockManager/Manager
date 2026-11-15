import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { ConfigurationRecoveryVersion } from '../generated/runtime';
import type { Profile } from './types';
import { diffJson } from './recovery-json-diff';
import { ownWindowMethods } from './window-work';
// Register the profile versions
export function registerProfileVersions(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('profileVersions', () => ({
        busy: false,
        restoring: false,
        epoch: 0,
        profileId: '',
        name: '',
        revision: 0,
        current: '',
        candidate: '',
        selected: '',
        error: '',
        warning: '',
        reviewed: false,
        versions: [] as ConfigurationRecoveryVersion[],
        // Append a localized timestamp when the recovery version has one
        versionLabel(version: ConfigurationRecoveryVersion) {
            return `${version.label}${version.timestamp_ms === null ? '' : ` · ${new Date(version.timestamp_ms).toLocaleString()}`}`;
        },
        // Derive diff from the current profile versions state
        get diff() {
            return diffJson(this.current, this.candidate);
        },
        // Derive diff text from the current profile versions state
        get diffText() {
            return this.candidate
                ? this.diff.lines
                      .map(
                          (line) =>
                              `${line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ' '} ${line.text}`,
                      )
                      .join('\n')
                : '';
        },
        // Derive stale from the current profile versions state
        get stale() {
            return this.revision !== app().snapshot.configuration_revision;
        },
        // Initialize profile versions state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'profile versions', ['open', 'choose', 'restore']);
        },
        // Release profile versions state and associated lifecycle resources
        close() {
            if (this.restoring) return;
            this.epoch++;
            this.busy = false;
            this.candidate = '';
            this.current = '';
            this.versions = [];
            (this.$refs.dialog as HTMLDialogElement).close();
        },
        // Release profile versions state and associated lifecycle resources
        destroy() {
            this.epoch++;
        },
        // Open the current profile versions target
        async open() {
            if (!app().profile || this.busy) return;
            const epoch = ++this.epoch;
            this.profileId = app().profile!.id;
            this.name = app().profile!.name;
            this.revision = app().snapshot.configuration_revision;
            this.current = JSON.stringify(app().profile, null, 2);
            this.candidate = '';
            this.selected = '';
            this.reviewed = false;
            this.error = '';
            this.warning = '';
            this.versions = [];
            this.busy = true;
            (this.$refs.dialog as HTMLDialogElement).showModal();
            try {
                const result = await app().command('profile_versions', {
                    profileId: this.profileId,
                    expectedRevision: this.revision,
                });
                if (epoch !== this.epoch) return;
                this.versions = result.versions;
                this.warning = result.warnings.join(' ');
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
            } finally {
                if (epoch === this.epoch) this.busy = false;
            }
        },
        // Choose the active item for the profile versions workflow
        async choose() {
            this.candidate = '';
            this.reviewed = false;
            this.error = '';
            if (!this.selected || this.stale) return;
            const epoch = ++this.epoch;
            this.busy = true;
            try {
                const result = await app().command('profile_version', {
                    profileId: this.profileId,
                    expectedRevision: this.revision,
                    versionId: this.selected,
                });
                if (epoch === this.epoch) this.candidate = result.json;
            } catch (error) {
                if (epoch === this.epoch) this.error = String(error);
            } finally {
                if (epoch === this.epoch) this.busy = false;
            }
        },
        // Restore the retained profile versions state
        async restore() {
            if (this.busy || this.stale || !this.reviewed || !this.candidate) return;
            this.busy = true;
            this.restoring = true;
            try {
                const profile = JSON.parse(this.candidate) as Profile;
                if (profile.id !== this.profileId)
                    throw Error('Profile identity changed. Reopen versions.');
                await app().edit(
                    [{ kind: 'save_profile', profile }],
                    'Restore profile version',
                    this.revision,
                );
                this.restoring = false;
                this.close();
                app().notify('Profile version restored. Undo is available in History.', 'success');
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
                this.restoring = false;
            }
        },
    }));
}
