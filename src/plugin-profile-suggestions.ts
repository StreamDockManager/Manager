import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { ownWindowMethods } from './window-work';
import { windowCloseOwners } from './window-close-owners';
import { showFeatureDialog, closeFeatureDialog, runFeatureWork } from './feature-dialog-lifecycle';
import { native } from './bridge';
import type { PluginProfileCatalog, VendorProfileReview } from '../generated/runtime';
import { editPluginSettings } from './plugin-settings';
import { profileFamily, profileSuggestions } from './profile-suggestions';
import type { FeatureDialogHandoffs } from './feature-dialog-handoffs';

// Register the plugin profile suggestions
export function registerPluginProfileSuggestions(
    Alpine: typeof AlpineType,
    app: () => AppState,
    handoffs: FeatureDialogHandoffs,
) {
    Alpine.data('pluginProfileSuggestions', () => ({
        working: false,
        error: '',
        dialogEpoch: 0,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Derive app from the current plugin profile suggestions state
        get app() {
            return app();
        },
        // Run the requested plugin profile suggestions operation
        async run(work: () => Promise<void>) {
            await runFeatureWork(this, work);
        },
        // Show the selected plugin profile suggestions content
        setupMounted: false,
        vendorMounted: false,
        show(name: string) {
            if (name === 'pluginSetupDialog') this.setupMounted = true;
            if (name === 'vendorProfileDialog') this.vendorMounted = true;
            showFeatureDialog(this, name);
        },
        // Release plugin profile suggestions state and associated lifecycle resources
        close(name: string) {
            closeFeatureDialog(this, name);
        },
        vendorReview: null as VendorProfileReview | null,
        vendorReviews: [] as VendorProfileReview[],
        vendorReviewsLoading: false,
        vendorRefreshAgain: false,
        suggestionsReady: false,
        destroyed: false,
        reviewEpoch: 0,
        vendorWarnings: new Set<string>(),
        resolvedVendorReviews: new Set<string>(),
        pluginSetup: null as PluginProfileCatalog | null,
        pluginSetupQueue: [] as PluginProfileCatalog[],
        pluginSuggestion: '',
        pluginTargetDevice: '',
        // Keep same-model devices distinct and offer only connected devices in the selected layout family
        get pluginSuggestionDevices() {
            const family = this.selectedPluginSuggestion?.family_id;
            return app().snapshot.devices.filter(
                (device) =>
                    device.connected &&
                    family &&
                    profileFamily(app().models, device.model_id) === family,
            );
        },
        // Reconcile device selection after choosing another layout without silently choosing among peers
        chooseSuggestionDevice() {
            const devices = this.pluginSuggestionDevices;
            if (devices.some((device) => device.id === this.pluginTargetDevice)) return;
            this.pluginTargetDevice =
                devices.find((device) => device.id === app().deviceId)?.id ||
                (devices.length === 1 ? devices[0].id : '');
        },
        suggestionBaseline: '',
        // Derive plugin suggestions from the current plugin profile suggestions state
        get pluginSuggestions() {
            return profileSuggestions(
                this.pluginSetup?.suggestions || [],
                app().models,
                app().snapshot.devices.find((device) => device.id === this.pluginTargetDevice)
                    ?.model_id ||
                    app().device?.model_id ||
                    app().profile?.model_id ||
                    '',
            );
        },
        // Derive selected plugin suggestion from the current plugin profile suggestions state
        get selectedPluginSuggestion() {
            return this.pluginSuggestions.find((item) => item.key === this.pluginSuggestion);
        },
        // Choose the plugin suggestion
        choosePluginSuggestion() {
            const family = profileFamily(
                app().models,
                app().device?.model_id || app().profile?.model_id || '',
            );
            this.pluginSuggestion =
                this.pluginSuggestions.find((item) => !item.error && item.family_id === family)
                    ?.key ||
                this.pluginSuggestions.find((item) => !item.error)?.key ||
                '';
            this.chooseSuggestionDevice();
            this.suggestionBaseline = JSON.stringify([
                this.pluginSuggestion,
                this.pluginTargetDevice,
            ]);
        },
        pluginSetupLoading: false,
        disconnectHandoff: undefined as (() => void) | undefined,
        // Initialize plugin profile suggestions state and its required lifecycle hooks
        async init() {
            this.disconnectHandoff = handoffs.connectSuggestions(() => {
                if (!this.pluginSetup) this.nextPluginSetup();
            });
            ownWindowMethods(this, 'plugin profile suggestions', [
                'run',
                'openPluginProfiles',
                'queuePluginSetup',
                'configureImportedPlugin',
                'refreshVendorReviews',
                'resolveVendorReview',
                'reopenVendorReview',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                [
                    'run',
                    'openPluginProfiles',
                    'queuePluginSetup',
                    'configureImportedPlugin',
                    'refreshVendorReviews',
                    'resolveVendorReview',
                    'reopenVendorReview',
                ],
                {
                    name: 'Plugin profile suggestions',
                    retainOnUnmount: true,
                    // Return a snapshot of the current plugin profile suggestions state
                    state: () =>
                        this.pluginSetup &&
                        JSON.stringify([this.pluginSuggestion, this.pluginTargetDevice]) !==
                            this.suggestionBaseline
                            ? 'dirty'
                            : 'clean',
                    // Discard pending plugin profile suggestions edits without saving them
                    discard: () => this.releaseForClose(),
                    // Release resources retained by the plugin profile suggestions operation
                    release: () => this.releaseForClose(),
                },
            );
            if (native) {
                await handoffs.setupReady;
                this.suggestionsReady = true;
                if (!windowCloseOwners.closing) {
                    await this.queuePluginSetup(
                        new CustomEvent('plugin-setup-imports', {
                            detail: app().snapshot.plugins.map((plugin) => plugin.id),
                        }),
                    );
                }
            }
        },
        // Queue the plugin setup
        async queuePluginSetup(event: Event) {
            try {
                const ids = [...new Set((event as CustomEvent<string[]>).detail)];
                const candidates: PluginProfileCatalog[] = [];
                for (let offset = 0; offset < ids.length; offset += 100)
                    candidates.push(
                        ...(await app().command('pending_plugin_profile_setup', {
                            ids: ids.slice(offset, offset + 100),
                        })),
                    );
                for (const candidate of candidates)
                    if (
                        this.pluginSetup?.plugin_id !== candidate.plugin_id &&
                        !this.pluginSetupQueue.some(
                            (item) => item.plugin_id === candidate.plugin_id,
                        )
                    )
                        this.pluginSetupQueue.push(candidate);
                if (!this.pluginSetup && !handoffs.setupOpen()) this.nextPluginSetup();
            } catch (error) {
                app().notify(String(error), 'error');
            }
            await this.refreshVendorReviews();
        },
        // Advance the queued plugin setup only when no other setup or review owns the view
        nextPluginSetup() {
            if (
                windowCloseOwners.closing ||
                this.destroyed ||
                this.pluginSetup ||
                this.pluginSetupLoading ||
                this.vendorReview ||
                handoffs.setupOpen()
            )
                return;
            this.pluginSetup = this.pluginSetupQueue.shift() || null;
            this.choosePluginSuggestion();
            if (this.pluginSetup) this.show('pluginSetupDialog');
            else {
                this.vendorReview = this.vendorReviews.shift() || null;
                if (this.vendorReview) this.show('vendorProfileDialog');
            }
        },
        // Open the plugin profiles
        async openPluginProfiles(event: Event) {
            if (
                this.working ||
                this.pluginSetupLoading ||
                this.pluginSetup ||
                this.vendorReview ||
                handoffs.setupOpen()
            )
                return;
            this.pluginSetupLoading = true;
            try {
                const id = (event as CustomEvent<string>).detail;
                this.pluginSetup = await app().command('list_plugin_profiles', { id });
                this.choosePluginSuggestion();
                this.show('pluginSetupDialog');
            } catch (error) {
                app().notify(String(error), 'error');
            } finally {
                this.pluginSetupLoading = false;
                this.nextPluginSetup();
            }
        },
        // Derive plugin setup enabled from the current plugin profile suggestions state
        get pluginSetupEnabled() {
            return app().snapshot.plugins.some(
                (plugin) => plugin.id === this.pluginSetup?.plugin_id && plugin.enabled,
            );
        },
        // Close the plugin setup
        async closePluginSetup() {
            await this.finishPluginSetup(false);
        },
        // Enable the plugin setup
        async enablePluginSetup() {
            await this.finishPluginSetup(true);
        },
        // Finish the plugin setup
        async finishPluginSetup(enable: boolean) {
            if (this.working || !this.pluginSetup) return;
            const setup = this.pluginSetup;
            // Complete the deferred plugin profile suggestions work using the current operation state
            await this.run(async () => {
                if (enable)
                    app().apply(
                        await app().command('enable_plugin', {
                            id: setup.plugin_id,
                            enabled: true,
                        }),
                    );
                try {
                    await app().command('acknowledge_plugin_profile_setup', {
                        id: setup.plugin_id,
                        fingerprint: setup.fingerprint,
                    });
                } catch (error) {
                    app().notify(String(error), 'warning');
                }
                this.close('pluginSetupDialog');
                this.pluginSetup = null;
                this.nextPluginSetup();
            });
        },
        // Open the imported plugin's global settings from its setup workflow
        async configureImportedPlugin(event: Event) {
            if (!this.pluginSetup) return;
            try {
                await editPluginSettings(
                    this.pluginSetup.plugin_id,
                    null,
                    {},
                    event.currentTarget as HTMLElement,
                    true,
                );
            } catch (error) {
                this.error = String(error);
            }
        },
        // Import the plugin suggestion
        async importPluginSuggestion() {
            const suggestion = this.selectedPluginSuggestion;
            if (!this.pluginSetup || !suggestion || suggestion.error) return;
            const pluginId = this.pluginSetup.plugin_id;
            const deviceId = this.pluginTargetDevice;
            if (!this.pluginSuggestionDevices.some((device) => device.id === deviceId)) {
                this.error = 'Choose a connected destination device.';
                return;
            }
            await this.run(() =>
                handoffs.importSuggestion(pluginId, suggestion.source_key, deviceId),
            );
        },
        // Refresh the vendor reviews
        async refreshVendorReviews() {
            if (!native || !this.suggestionsReady || this.destroyed || windowCloseOwners.closing)
                return;
            if (this.vendorReviewsLoading) {
                this.vendorRefreshAgain = true;
                return;
            }
            this.vendorReviewsLoading = true;
            const epoch = this.reviewEpoch;
            try {
                do {
                    this.vendorRefreshAgain = false;
                    const pending = await app().command('pending_vendor_profile_reviews');
                    if (this.destroyed || windowCloseOwners.closing || epoch !== this.reviewEpoch)
                        return;
                    for (const warning of pending.warnings) {
                        if (!this.vendorWarnings.has(warning)) {
                            this.vendorWarnings.add(warning);
                            app().notify(warning, 'warning');
                        }
                    }
                    if (this.vendorWarnings.size > 256)
                        this.vendorWarnings = new Set([...this.vendorWarnings].slice(-256));
                    this.vendorReviews = pending.reviews.filter(
                        (review) =>
                            review.id !== this.vendorReview?.id &&
                            !this.resolvedVendorReviews.has(review.id),
                    );
                    this.nextPluginSetup();
                } while (this.vendorRefreshAgain);
            } catch (error) {
                app().notify(String(error), 'error');
            } finally {
                this.vendorReviewsLoading = false;
            }
        },
        // Accept the vendor review
        async acceptVendorReview() {
            await this.resolveVendorReview(true);
        },
        // Resolve the current vendor review without accepting its suggested profile
        async declineVendorReview() {
            await this.resolveVendorReview(false);
        },
        // Resolve vendor review for the plugin profile suggestions workflow
        async resolveVendorReview(accept: boolean) {
            if (this.working || !this.vendorReview) return;
            const review = this.vendorReview;
            // Complete the deferred plugin profile suggestions work using the current operation state
            await this.run(async () => {
                const snapshot = await app().command('resolve_vendor_profile_review', {
                    id: review.id,
                    accept,
                });
                if (accept) app().apply(snapshot);
                this.resolvedVendorReviews.add(review.id);
                if (this.resolvedVendorReviews.size > 256)
                    this.resolvedVendorReviews.delete(
                        this.resolvedVendorReviews.values().next().value!,
                    );
                this.close('vendorProfileDialog');
                this.vendorReview = null;
                this.nextPluginSetup();
                await this.refreshVendorReviews();
            });
        },
        // Discard only the review view and reload the backend's current review queue
        async reopenVendorReview() {
            if (this.working) return;
            // Drop only the view. Re-read the backend's current bounded review queue
            this.close('vendorProfileDialog');
            this.vendorReview = null;
            this.vendorReviews = [];
            await this.refreshVendorReviews();
        },
        // Release the for close
        async releaseForClose() {
            ++this.reviewEpoch;
            const reviews = [this.vendorReview, ...this.vendorReviews].filter(
                (review): review is VendorProfileReview => review !== null,
            );
            this.vendorReview = null;
            this.vendorReviews = [];
            this.pluginSetup = null;
            this.pluginSetupQueue = [];
            this.close('pluginSetupDialog');
            this.close('vendorProfileDialog');
            await Promise.all(
                reviews.map((review) =>
                    trackCloseRelease('vendor-profile-review:' + review.id, () =>
                        app().command('cancel_vendor_profile_review', { id: review.id }),
                    ),
                ),
            );
        },
        // Release plugin profile suggestions state and associated lifecycle resources
        destroy() {
            this.destroyed = true;
            this.disconnectHandoff?.();
            this.unregisterCloseOwner?.();
            void this.releaseForClose().catch((error) => app().notify(String(error), 'error'));
        },
    }));
}
