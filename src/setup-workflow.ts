import { startupPermissionsReady } from './startup-permissions';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { registerCloseDraft } from './close-releases';
import { ownWindowMethods } from './window-work';
import { showFeatureDialog, closeFeatureDialog, runFeatureWork } from './feature-dialog-lifecycle';
import type { SetupReport, SetupProgress, Binding } from '../generated/runtime';
import { defaultMedia, native } from './bridge';
import { DEFAULT_BACKGROUND } from './background-color';
import { createSetupCapabilities } from './setup-capabilities';
import { suggestedFeedback } from './feature-defaults';
import type { FeatureDialogHandoffs } from './feature-dialog-handoffs';

// Register the setup workflow
export function registerSetupWorkflow(
    Alpine: typeof AlpineType,
    app: () => AppState,
    handoffs: FeatureDialogHandoffs,
) {
    Alpine.data('setupWorkflow', () => ({
        working: false,
        error: '',
        dialogEpoch: 0,
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Derive app from the current setup workflow state
        get app() {
            return app();
        },
        // Run the requested setup workflow operation
        async run(work: () => Promise<void>) {
            await runFeatureWork(this, work);
        },
        setupVisible: false,
        setupMounted: false,
        // Show the selected setup workflow content
        show(name: string) {
            this.setupMounted = true;
            this.setupVisible = true;
            showFeatureDialog(this, name);
        },
        // Release setup workflow state and associated lifecycle resources
        close(name: string) {
            this.setupVisible = false;
            closeFeatureDialog(this, name);
            const opener = this.setupOpener;
            this.setupOpener = null;
            if (opener?.isConnected)
                // Complete the deferred setup workflow work using the current operation state
                this.$nextTick(() => {
                    if (opener.getClientRects().length && !opener.disabled) opener.focus();
                });
            handoffs.resumeSuggestions();
        },
        setup: null as SetupReport | null,
        setupStep: 0,
        setupDevice: '',
        setupProfile: '',
        setupControl: '',
        setupTitle: 'My first action',
        setupKind: 'open_url',
        setupValue: 'https://example.org',
        setupViewEpoch: 0,
        setupOpener: null as HTMLButtonElement | null,
        setupBaseline: '',
        // Serialize the setup binding draft to detect unsaved changes
        setupCloseValue() {
            return JSON.stringify([
                this.setupControl,
                this.setupTitle,
                this.setupKind,
                this.setupValue,
            ]);
        },
        capabilityChecks: createSetupCapabilities(app),
        // Check the capabilities
        checkCapabilities() {
            return this.capabilityChecks.start('passive');
        },
        // Start the setup capability check that listens for device input
        listenForSetupInput() {
            return this.capabilityChecks.start('device_input');
        },
        // Cancel the capability checks
        cancelCapabilityChecks() {
            return this.capabilityChecks.cancel();
        },
        // Derive capability devices from the current setup workflow state
        get capabilityDevices() {
            return app().snapshot.devices.filter((device) => device.connected);
        },
        // Derive setup heading from the current setup workflow state
        get setupHeading() {
            return [
                'Check your environment',
                'Choose a device and profile',
                'Create a binding',
                'Try your control',
                'Setup complete',
            ][this.setupStep];
        },
        // Derive setup controls from the current setup workflow state
        get setupControls() {
            return (
                app().device?.manifest.controls.filter(
                    (c) => c.image_slot !== null && c.kind === 'button',
                ) || []
            );
        },
        // Derive setup profiles from the current setup workflow state
        get setupProfiles() {
            return app().snapshot.profiles.filter((p) => p.model_id === app().device?.model_id);
        },
        // Derive verified input from the current setup workflow state
        get verifiedInput() {
            return app().events.find(
                (e) =>
                    e.kind === 'input' &&
                    e.device_id === this.setupDevice &&
                    e.control?.control_id === this.setupControl,
            );
        },
        // Derive action result from the current setup workflow state
        get actionResult() {
            return app().events.find(
                (e) =>
                    e.device_id === this.setupDevice && ['action-error', 'input'].includes(e.kind),
            );
        },
        disconnectHandoff: undefined as (() => void) | undefined,
        // Initialize setup workflow state and its required lifecycle hooks
        async init() {
            this.disconnectHandoff = handoffs.connectSetup(
                () =>
                    this.setupVisible ||
                    Boolean((this.$refs.setupDialog as HTMLDialogElement | undefined)?.open),
            );
            ownWindowMethods(this, 'setup', [
                'run',
                'openSetup',
                'chooseSetupDevice',
                'nextSetup',
                'previousSetup',
                'dismissSetup',
                'finishSetup',
                'setupAction',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(
                this,
                [
                    'run',
                    'openSetup',
                    'chooseSetupDevice',
                    'nextSetup',
                    'previousSetup',
                    'dismissSetup',
                    'finishSetup',
                    'setupAction',
                ],
                {
                    name: 'Setup changes',
                    retainOnUnmount: true,
                    // Return a snapshot of the current setup workflow state
                    state: () =>
                        ((this.$refs.setupDialog as HTMLDialogElement | undefined)?.open &&
                            this.setupCloseValue() !== this.setupBaseline) ||
                        this.capabilityChecks.busy
                            ? 'dirty'
                            : 'clean',
                    // Discard pending setup workflow edits without saving them
                    discard: () => this.releaseForClose(),
                    // Release resources retained by the setup workflow operation
                    release: () => this.releaseForClose(),
                },
            );
            try {
                await startupPermissionsReady;
                this.setup = await app().bridge.command('setup_status');
                if (
                    native &&
                    this.setup.progress.new_install &&
                    !this.setup.progress.complete &&
                    !this.setup.progress.dismissed
                )
                    await this.openSetup();
            } catch {
            } finally {
                handoffs.setupInitialized();
            }
        },
        // Open the setup
        async openSetup(event?: Event) {
            const epoch = ++this.setupViewEpoch;
            const opener = event?.target instanceof Element ? event.target.closest('button') : null;
            await this.capabilityChecks.open(app().deviceId);
            if (epoch !== this.setupViewEpoch) return;
            this.setupOpener = opener;
            this.setupDevice = app().deviceId;
            this.setupProfile = app().profile?.id || '';
            this.setupStep = this.setup?.progress.step || 0;
            this.setupBaseline = this.setupCloseValue();
            this.show('setupDialog');
            await this.recheckSetup();
        },
        // Reload setup status through the workflow's guarded operation wrapper
        async recheckSetup() {
            await this.run(async () => {
                this.setup = await app().command('setup_status');
            });
        },
        // Save the progress
        async saveProgress(complete = false, dismissed = false) {
            const progress: SetupProgress = {
                step: this.setupStep,
                complete,
                dismissed,
                new_install: this.setup?.progress.new_install || false,
            };
            await app().command('save_setup_progress', { progress });
            if (this.setup) this.setup.progress = progress;
        },
        // Close capability checks before advancing and saving the setup step
        async nextSetup() {
            await this.capabilityChecks.close();
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                this.setupStep = Math.min(4, this.setupStep + 1);
                await this.saveProgress();
            });
        },
        // Close capability checks before returning to the previous saved setup step
        async previousSetup() {
            await this.capabilityChecks.close();
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                this.setupStep = Math.max(0, this.setupStep - 1);
                await this.saveProgress();
            });
        },
        // Dismiss the setup
        async dismissSetup() {
            ++this.setupViewEpoch;
            await this.capabilityChecks.close();
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                await this.saveProgress(false, true);
                this.close('setupDialog');
            });
        },
        // Finish the setup
        async finishSetup() {
            await this.capabilityChecks.close();
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                await this.saveProgress(true);
                this.close('setupDialog');
                app().notify('Setup complete', 'success');
            });
        },
        // Run the selected setup action only after closing the current capability check
        async setupAction(event: Event) {
            await this.capabilityChecks.close();
            const id = (event.currentTarget as HTMLElement).dataset.action;
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                if (id === 'simulator') {
                    if (!app().device) {
                        app().apply(await app().command('simulate', { enabled: true }));
                        this.setupDevice = app().deviceId;
                        this.setupProfile = app().profile?.id || '';
                    }
                }
                if (id === 'accessibility') await app().command('authorize_accessibility');
                if (id === 'wayland') await app().command('authorize_wayland');
                this.setup = await app().command('setup_status');
            });
        },
        // Choose the setup device
        async chooseSetupDevice() {
            await app().flushDrafts?.();
            app().selectDevice({ target: { value: this.setupDevice } } as unknown as Event);
            this.setupProfile = app().profile?.id || '';
            this.setupControl = '';
        },
        // Choose the setup profile
        async chooseSetupProfile() {
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                app().apply(
                    await app().command('activate_profile', {
                        id: this.setupProfile,
                        deviceId: app().deviceId,
                    }),
                );
            });
        },
        // Create the setup binding
        async createSetupBinding() {
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                if (
                    !app().device?.connected ||
                    (app().profile && !app().page) ||
                    !this.setupControl
                )
                    throw Error('Choose a connected device and key.');
                const action: Binding['action'] =
                    this.setupKind === 'mute'
                        ? { kind: 'mute' }
                        : this.setupKind === 'volume'
                          ? { kind: 'volume', delta: Number(this.setupValue) || 5 }
                          : this.setupKind === 'hotkey'
                            ? {
                                  kind: 'hotkey',
                                  keys: this.setupValue
                                      .split('+')
                                      .map((s) => s.trim())
                                      .filter(Boolean),
                              }
                            : { kind: 'open_url', url: this.setupValue };
                await app().edit(
                    [
                        {
                            kind: 'bindings',
                            profile_id: app().profile?.id || '',
                            page_id: app().page?.id || '',
                            bindings: {
                                [this.setupControl]: {
                                    title: this.setupTitle,
                                    color: DEFAULT_BACKGROUND,
                                    action,
                                    media: defaultMedia(),
                                    feedback: suggestedFeedback(action.kind),
                                },
                            },
                        },
                    ],
                    'Create setup binding',
                );
                app().controlId = this.setupControl;
                this.setupBaseline = this.setupCloseValue();
                this.setupStep = 3;
                await this.saveProgress();
            });
        },
        // Trigger the setup control only on a simulated device
        async testSetupSimulator() {
            // Complete the deferred setup workflow work using the current operation state
            await this.run(async () => {
                if (!app().device?.simulated) return;
                await app().triggerSimulator(this.setupControl);
            });
        },
        // Release the for close
        async releaseForClose() {
            this.setupVisible = false;
            ++this.setupViewEpoch;
            await this.capabilityChecks.close();
            closeFeatureDialog(this, 'setupDialog');
        },
        // Release setup workflow state and associated lifecycle resources
        destroy() {
            this.disconnectHandoff?.();
            this.unregisterCloseOwner?.();
            void this.releaseForClose().catch(() => {});
        },
    }));
}
