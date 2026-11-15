import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
import { showFeatureDialog, closeFeatureDialog, runFeatureWork } from './feature-dialog-lifecycle';
import * as fileDialogs from './window-dialogs';
import { native } from './bridge';
import { registerArchiveReviewer } from './archive-import';
import { trackCloseRelease } from './close-releases';
import type { ImportPreview, ImportIssue, PluginBindingMapping } from '../generated/runtime';
import type { FeatureDialogHandoffs } from './feature-dialog-handoffs';

// Register the profile archive import
export function registerProfileArchiveImport(
    Alpine: typeof AlpineType,
    app: () => AppState,
    handoffs: FeatureDialogHandoffs,
) {
    Alpine.data('profileArchiveImport', () => ({
        working: false,
        error: '',
        dialogEpoch: 0,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Derive app from the current profile archive import state
        get app() {
            return app();
        },
        // Run the requested profile archive import operation
        async run(work: () => Promise<void>) {
            await runFeatureWork(this, work);
        },
        // Show the selected profile archive import content
        show(name: string) {
            this.importBaseline = this.importCloseValue();
            showFeatureDialog(this, name);
        },
        // Release profile archive import state and associated lifecycle resources
        close(name: string) {
            closeFeatureDialog(this, name);
        },
        imported: null as ImportPreview | null,
        importBaseline: '',
        // Import the close value
        importCloseValue() {
            return JSON.stringify([
                this.importMode,
                this.imported?.plugin?.placement,
                this.imported?.plugin?.configuration,
                this.repairs,
                this.recipeModes,
                this.recipeFrom,
                this.recipeTo,
            ]);
        },
        pluginImportReviewed: false,
        pluginImportDevice: null as { id: string; name: string; model_id: string } | null,
        // Limit reviewed placement choices to the device captured before the import was staged
        get pluginTargetProfiles() {
            return app().snapshot.profiles.filter(
                (profile) =>
                    !this.pluginImportDevice ||
                    profile.model_id === this.pluginImportDevice.model_id,
            );
        },
        // Do not offer another model for a device-bound suggested import
        get pluginTargetModels() {
            return app().models.filter(
                (model) =>
                    !this.pluginImportDevice || model.model_id === this.pluginImportDevice.model_id,
            );
        },
        // Derive plugin target pages from the current profile archive import state
        get pluginTargetPages() {
            const id = this.imported?.plugin?.placement.target_profile;
            return app().snapshot.profiles.find((profile) => profile.id === id)?.pages || [];
        },
        // Resolve an imported plugin page name with its source ID as fallback
        pluginPageLabel(id: string) {
            return (
                this.imported?.plugin?.placement.pages.find((page) => page.source_page === id)
                    ?.name || id
            );
        },
        // Find the review row for a source binding or provide an empty mapping choice
        pluginMappingRow(mapping: PluginBindingMapping) {
            return (
                this.imported?.plugin?.rows.find(
                    (row) =>
                        row.source_page === mapping.source_page &&
                        row.source_control === mapping.source_control,
                ) || { title: mapping.source_control, problem: null, occupied: false, choices: [] }
            );
        },
        // Require another review after plugin placement changes
        pluginPlacementChanged() {
            this.pluginImportReviewed = false;
        },
        // Reset all imported pages to new-page targets and invalidate the placement review
        pluginTargetChanged() {
            if (!this.imported?.plugin) return;
            for (const page of this.imported.plugin.placement.pages) page.target_page = '@new';
            this.pluginImportReviewed = false;
        },
        // Format a configuration value for review, distinguishing an absent value
        configurationLabel(value: unknown) {
            return value === undefined ? 'Not set' : JSON.stringify(value);
        },
        importMode: 'merge',
        repairs: {} as Record<string, string>,
        recipeModes: {} as Record<string, string>,
        recipeFrom: {} as Record<string, string>,
        recipeTo: {} as Record<string, string>,
        // Derive missing plugin issues from the current profile archive import state
        get missingPluginIssues() {
            return this.imported?.issues.some((i) => i.kind === 'plugin') || false;
        },
        disconnectHandoff: undefined as (() => void) | undefined,
        unregisterArchiveReviewer: undefined as (() => void) | undefined,
        // Initialize profile archive import state and its required lifecycle hooks
        init() {
            this.disconnectHandoff = handoffs.connectImport((id, key, deviceId) =>
                this.reviewPluginSuggestion(id, key, deviceId),
            );
            ownWindowMethods(this, 'profile import', [
                'run',
                'openProfileImport',
                'reviewProfileArchive',
                'reviewPluginSuggestion',
                'browseRepair',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                [
                    'run',
                    'openProfileImport',
                    'reviewProfileArchive',
                    'reviewPluginSuggestion',
                    'browseRepair',
                ],
                {
                    name: 'Profile import changes',
                    retainOnUnmount: true,
                    // Return a snapshot of the current profile archive import state
                    state: () =>
                        this.imported && this.importCloseValue() !== this.importBaseline
                            ? 'dirty'
                            : 'clean',
                    // Discard pending profile archive import edits without saving them
                    discard: () => this.releaseForClose(),
                    // Release resources retained by the profile archive import operation
                    release: () => this.releaseForClose(),
                },
            );
            this.unregisterArchiveReviewer = registerArchiveReviewer('profiles', (path) =>
                this.reviewProfileArchive(path),
            );
        },
        // Prepare a plugin-suggested profile for review after flushing drafts and cancelling prior imports
        async reviewPluginSuggestion(id: string, key: string, chosenDevice?: string) {
            if (this.working) return;
            // Keep the explicitly selected device and profile stable while drafts flush
            const targetDevice = chosenDevice ?? (app().deviceId || undefined);
            const device = app().snapshot.devices.find(
                (candidate) => candidate.id === targetDevice && candidate.connected,
            );
            const session = device && app().snapshot.sessions[device.persistence_key];
            const profile =
                device &&
                (app().snapshot.profiles.find(
                    (candidate) =>
                        candidate.id === session?.profile_id &&
                        candidate.model_id === device.model_id,
                ) ||
                    app().snapshot.profiles.find(
                        (candidate) => candidate.model_id === device.model_id,
                    ));
            const targetProfile = profile?.id;
            const targetPage =
                profile?.pages.find((page) => page.id === session?.page_id)?.id ||
                profile?.active_page;
            this.show('profileImportDialog');
            // Complete the deferred profile archive import work using the current operation state
            await this.run(async () => {
                if (targetDevice && !device)
                    throw Error('The selected device is missing or disconnected.');
                this.pluginImportDevice = device
                    ? { id: device.id, name: device.name, model_id: device.model_id }
                    : null;
                await app().flushDrafts?.();
                if (this.imported)
                    await app().command('cancel_profile_import', { id: this.imported.id });
                this.imported = null;
                this.imported = await app().command('prepare_plugin_profile_import', {
                    id,
                    key,
                    targetDevice,
                    targetProfile,
                    targetPage,
                });
                this.resetRepairs();
                this.importMode = 'pages';
                this.pluginImportReviewed = true;
                this.importBaseline = this.importCloseValue();
            });
        },
        // Open the profile import
        async openProfileImport() {
            if (this.working) return;
            if (!native) {
                await this.reviewProfileArchive('sample');
                return;
            }
            const { open } = fileDialogs;
            const path = await open({
                multiple: false,
                filters: [{ name: 'Profiles', extensions: ['zip'] }],
            });
            if (path) await this.reviewProfileArchive(path);
        },
        // Reset the repairs
        resetRepairs() {
            this.repairs = {};
            this.recipeModes = {};
            this.recipeFrom = {};
            this.recipeTo = {};
        },
        // Prepare a profile archive for review without retaining the previous import session
        async reviewProfileArchive(path: string) {
            if (this.working) return;
            this.pluginImportDevice = null;
            this.show('profileImportDialog');
            // Complete the deferred profile archive import work using the current operation state
            await this.run(async () => {
                await app().flushDrafts?.();
                if (this.imported)
                    await app().command('cancel_profile_import', { id: this.imported.id });
                this.imported = null;
                this.imported = await app().command('prepare_profile_import', { path });
                this.resetRepairs();
                this.pluginImportReviewed = false;
                this.importMode = 'merge';
                this.importBaseline = this.importCloseValue();
            });
        },
        // Revalidate the selected import mode or plugin placement and configuration choices
        async recheckImport() {
            // Complete the deferred profile archive import work using the current operation state
            await this.run(async () => {
                if (!this.imported) return;
                if (this.imported.plugin) {
                    this.imported.plugin.placement.configuration_choices =
                        this.imported.plugin.configuration
                            .filter((field) => field.use_suggested)
                            .map((field) => field.path);
                    this.imported = await app().command('update_profile_import', {
                        id: this.imported.id,
                        mode: 'pages',
                        repairs: [],
                        placement: this.imported.plugin.placement,
                    });
                    this.pluginImportReviewed = true;
                    return;
                }
                this.imported = await app().command('update_profile_import', {
                    id: this.imported.id,
                    mode: this.importMode,
                    repairs: [],
                });
            });
        },
        // Apply the repair
        async applyRepair(event: Event) {
            const key = (event.currentTarget as HTMLElement).dataset.key;
            const disable = (event.currentTarget as HTMLElement).dataset.disable === 'true';
            if (!key || !this.imported) return;
            // Complete the deferred profile archive import work using the current operation state
            await this.run(async () => {
                this.imported = await app().command('update_profile_import', {
                    id: this.imported!.id,
                    mode: this.importMode,
                    repairs: [{ key, replacement: this.repairs[key] || '', disable }],
                    remember:
                        !disable && this.recipeModes[key]
                            ? {
                                  key,
                                  match_mode: this.recipeModes[key],
                                  from_prefix: this.recipeFrom[key] || '',
                                  to_prefix: this.recipeTo[key] || '',
                              }
                            : null,
                });
            });
        },
        // Check whether remember repair is valid for the current profile archive import state
        canRememberRepair(issue: ImportIssue) {
            return ['path', 'plugin', 'model', 'control'].includes(issue.kind);
        },
        // Find saved repair suggestions matching this import issue
        repairSuggestions(issue: ImportIssue) {
            return (this.imported?.repair_suggestions || []).filter(
                (item) => item.key === issue.key,
            );
        },
        // Choose the repair suggestion
        chooseRepairSuggestion(event: Event) {
            const target = event.currentTarget as HTMLElement;
            const suggestion = this.imported?.repair_suggestions.find(
                (item) => item.id === target.dataset.recipe && item.key === target.dataset.key,
            );
            if (suggestion) {
                this.repairs[suggestion.key] = suggestion.replacement;
                this.recipeModes[suggestion.key] = '';
            }
        },
        // Forget the selected repair recipe and refresh the pending import review
        async forgetRepairSuggestion(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.recipe;
            if (!id || !this.imported) return;
            // Complete the deferred profile archive import work using the current operation state
            await this.run(async () => {
                await app().command('forget_import_recipe', { id });
                this.imported = await app().command('update_profile_import', {
                    id: this.imported!.id,
                    mode: this.importMode,
                    repairs: [],
                });
            });
        },
        // Browse the repair
        async browseRepair(event: Event) {
            const key = (event.currentTarget as HTMLElement).dataset.key;
            if (!key || !native) return;
            const issue = this.imported?.issues.find((i) => i.key === key);
            const { open } = fileDialogs;
            const path = await open({
                multiple: false,
                directory: false,
                filters: ['media', 'background'].includes(issue?.kind || '')
                    ? [{ name: 'Media', extensions: ['png', 'jpg', 'jpeg', 'gif', 'mp4'] }]
                    : undefined,
            });
            if (path) this.repairs[key] = path;
        },
        // Describe the replacement format appropriate to the import issue kind
        repairHint(issue: ImportIssue) {
            return issue.kind === 'plugin'
                ? 'plugin-id:action-id'
                : issue.kind === 'profile'
                  ? 'Target profile ID'
                  : ['page', 'active_page'].includes(issue.kind)
                    ? 'Target page ID'
                    : issue.kind === 'model'
                      ? 'Supported model ID, e.g. n4-pro'
                      : issue.kind === 'control'
                        ? 'Destination control ID, or blank to remove'
                        : 'Replacement path, or blank to clear missing media';
        },
        // Check whether disable is valid for the current profile archive import state
        canDisable(issue: ImportIssue) {
            return !['background', 'model', 'control', 'active_page'].includes(issue.kind);
        },
        // Check whether browse is valid for the current profile archive import state
        canBrowse(issue: ImportIssue) {
            return ['path', 'media', 'background'].includes(issue.kind);
        },
        // Derive can commit import from the current profile archive import state
        get canCommitImport() {
            return (
                !!this.imported &&
                !this.imported.issues.length &&
                (!this.imported.plugin || (this.imported.plugin.ready && this.pluginImportReviewed))
            );
        },
        // Derive import commit label from the current profile archive import state
        get importCommitLabel() {
            return this.imported?.plugin
                ? 'Apply plugin profile'
                : this.importMode === 'replace'
                  ? 'Replace workspace'
                  : 'Merge profiles';
        },
        // Commit the import
        async commitImport() {
            // Complete the deferred profile archive import work using the current operation state
            await this.run(async () => {
                if (
                    !this.imported ||
                    this.imported.issues.length ||
                    (this.imported.plugin &&
                        (!this.imported.plugin.ready || !this.pluginImportReviewed))
                )
                    return;
                app().apply(
                    await app().command('commit_profile_import', {
                        id: this.imported.id,
                        expectedRevision: this.imported.expected_revision,
                    }),
                );
                this.imported = null;
                this.close('profileImportDialog');
                app().notify('Profiles imported. This edit can be undone.', 'success');
            });
        },
        // Close the profile import
        async closeProfileImport() {
            // Complete the deferred profile archive import work using the current operation state
            await this.run(async () => {
                if (this.imported)
                    await app().command('cancel_profile_import', { id: this.imported.id });
                this.imported = null;
                this.close('profileImportDialog');
            });
        },
        // Release the for close
        async releaseForClose() {
            const id = this.imported?.id;
            if (id)
                await trackCloseRelease(`profile-import:${id}`, () =>
                    app().bridge.command('cancel_profile_import', { id }),
                );
            this.imported = null;
            this.close('profileImportDialog');
        },
        // Release profile archive import state and associated lifecycle resources
        destroy() {
            this.disconnectHandoff?.();
            this.unregisterArchiveReviewer?.();
            this.unregisterCloseOwner?.();
            void this.releaseForClose().catch(() => {});
        },
    }));
}
