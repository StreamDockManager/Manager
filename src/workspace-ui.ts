import { open as openFileDialog } from './window-dialogs';
import { cloneJson } from './json-clone';
import { nextTab, tabNavigationKey } from './tab-navigation';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type {
    AppRule,
    AutomationStatus,
    RuleEvaluation,
    HistoryComparison,
    HistoryChange,
    HistoryControlComparison,
    FirmwareStatus,
} from '../generated/runtime';
import { registerMediaOptions } from './media-options';
import { paginate } from './pagination';
import { createDevicePowerDraft } from './device-power';
import { registerFramePolicy } from './frame-policy';
import { registerKeyboardSettings } from './keyboard-settings';
import { registerBootLogo } from './boot-logo';
import { ownWindowMethods, windowWork } from './window-work';
import { windowCloseOwners } from './window-close-owners';
import { registerCloseDraft } from './close-releases';
import { hasBindingContent } from './bindings';
// Register the workspace ui
export function registerWorkspaceUI(Alpine: typeof AlpineType, app: () => AppState) {
    registerFramePolicy(Alpine, app);
    registerBootLogo(Alpine, app);
    registerKeyboardSettings(Alpine, app);
    registerMediaOptions(Alpine, app);
    Alpine.data('simulatorManager', () => ({
        // Initialize workspace ui state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'simulator manager', [
                'addSimulator',
                'useSimulator',
                'disconnectSimulator',
            ]);
        },
        modelId: 'n4-pro',
        // Derive app from the current workspace ui state
        get app() {
            return app();
        },
        // Add the simulator
        async addSimulator() {
            if (app().busy || !app().models.some((model) => model.model_id === this.modelId))
                return;
            const id = `simulator:${this.modelId}:${crypto.randomUUID()}`;
            try {
                await app().flushDrafts?.();
                app().apply(await app().command('simulate', { enabled: true, deviceId: id }));
                app().selectDeviceId(id);
            } catch {}
        },
        // Flush drafts before connecting and selecting the requested simulator
        async useSimulator(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.deviceId;
            if (!id || app().busy) return;
            try {
                await app().flushDrafts?.();
                if (!app().snapshot.devices.some((device) => device.id === id))
                    app().apply(await app().command('simulate', { enabled: true, deviceId: id }));
                app().selectDeviceId(id);
            } catch {}
        },
        // Disconnect the simulator
        async disconnectSimulator(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.deviceId;
            if (!id || app().busy) return;
            try {
                await app().flushDrafts?.();
                app().apply(await app().command('simulate', { enabled: false, deviceId: id }));
            } catch {}
        },
    }));
    Alpine.data('devicePower', () => ({
        ...createDevicePowerDraft(app),
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Derive app from the current workspace ui state
        get app() {
            return app();
        },
        // Initialize workspace ui state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'device power', ['show', 'save']);
            this.unregisterCloseOwner = registerCloseDraft(this, ['save'], {
                name: 'Device power settings',
                retainOnUnmount: true,
                // Return a snapshot of the current workspace ui state
                state: () => (this.dirty ? 'dirty' : 'clean'),
                // Discard pending workspace ui edits without saving them
                discard: () => this.close(),
                // Release resources retained by the workspace ui operation
                release: () => this.close(),
            });
            this.$watch('$store.app.deviceId', () => this.close());
            this.$watch('$store.app.route', () => this.close());
        },
        // Show the selected workspace ui content
        async show(event?: Event) {
            (event?.currentTarget as HTMLElement | null)?.focus();
            (this.$refs.power as HTMLDialogElement).showModal();
            await this.openSettings();
        },
        // Release workspace ui state and associated lifecycle resources
        close() {
            this.discard();
            (this.$refs.power as HTMLDialogElement | undefined)?.close();
        },
        // Save pending changes through the workspace ui workflow
        async save() {
            if (await this.commit()) this.close();
        },
        // Release workspace ui state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.close();
        },
    }));
    Alpine.data('deviceOutput', () => ({
        outputTab: 'display',
        brightness: 65,
        lightingBrightness: 65,
        colors: [] as string[],
        loaded: '',
        powerTimer: 0,
        resumeListener: undefined as (() => void) | undefined,
        firmware: null as FirmwareStatus | null,
        firmwareBusy: false,
        firmwareError: '',
        firmwareGeneration: 0,
        // Derive firmware read message from the current workspace ui state
        get firmwareReadMessage() {
            if (this.firmwareBusy) return 'Checking firmware…';
            const firmware = this.firmware;
            if (firmware?.state === 'installed_known' && !firmware.stale) return '';
            return firmware?.message || '';
        },
        // Derive firmware release message from the current workspace ui state
        get firmwareReleaseMessage() {
            if (app().simulated) return '';
            return this.firmware?.latest_state === 'not_checked'
                ? 'Check for updates from the manufacturer.'
                : this.firmware?.latest_message || '';
        },
        // Derive firmware checked at from the current workspace ui state
        get firmwareCheckedAt() {
            return this.firmware?.latest_checked || this.firmware?.last_checked || '';
        },
        // Derive firmware checked from the current workspace ui state
        get firmwareChecked() {
            if (!this.firmwareCheckedAt) return 'Never';
            const date = new Date(this.firmwareCheckedAt);
            return Number.isFinite(date.getTime())
                ? date.toLocaleString(undefined, { timeZoneName: 'short' })
                : 'Unavailable';
        },
        // Derive installed firmware from the current workspace ui state
        get installedFirmware() {
            const firmware = this.firmware;
            return firmware?.installed
                ? firmware.installed + (firmware.stale ? ' (last known)' : '')
                : 'Unknown';
        },
        // Derive latest firmware from the current workspace ui state
        get latestFirmware() {
            const firmware = this.firmware;
            if (firmware?.latest)
                return firmware.latest + (firmware.latest_stale ? ' (last known)' : '');
            switch (firmware?.latest_state) {
                case 'available':
                case 'not_listed':
                    return 'No matching release listed';
                case 'failed':
                    return 'Lookup failed';
                case 'unsupported':
                    return 'Not available for this variant';
                default:
                    return 'Not checked yet';
            }
        },
        vibrationEnabled: null as boolean | null,
        configurationDraft: {} as Record<string, string>,
        configurationVibration: '',
        configurationDeviceId: '',
        configurationSignature: '',
        configurationGeneration: 0,
        configurationSaving: false,
        configurationError: '',
        configurationBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Serialize configuration and vibration drafts to detect unsaved device-setting changes
        configurationCloseValue() {
            return JSON.stringify([this.configurationDraft, this.configurationVibration]);
        },
        // Derive vibration from the current workspace ui state
        get vibration() {
            return app().device?.manifest.capabilities.vibration;
        },
        // Derive configuration fields from the current workspace ui state
        get configurationFields() {
            return this.vibration?.configuration_fields || [];
        },
        // Derive has reviewed configuration from the current workspace ui state
        get hasReviewedConfiguration() {
            const values = app().deviceOutput.device_configuration || {};
            return this.configurationFields.every((field) => typeof values[field.id] === 'boolean');
        },
        // Derive configuration can apply from the current workspace ui state
        get configurationCanApply() {
            return (
                !this.configurationSaving &&
                !this.vibrationDisabled &&
                ['on', 'off'].includes(this.configurationVibration) &&
                this.configurationFields.every((field) =>
                    ['on', 'off'].includes(this.configurationDraft[field.id]),
                )
            );
        },
        // Derive vibration firmware blocked from the current workspace ui state
        get vibrationFirmwareBlocked() {
            const minimum = this.vibration?.firmware_requirements[app().device?.variant_id || ''];
            const raw = this.firmware?.stale ? null : this.firmware?.installed;
            const installed = raw?.match(/(?:^|\.)(\d{1,4})\.(\d{1,6})$/);
            const required = minimum?.match(/^(\d+)\.(\d+)$/);
            return !app().simulated && installed && required
                ? Number(installed[1]) < Number(required[1]) ||
                      (Number(installed[1]) === Number(required[1]) &&
                          Number(installed[2]) < Number(required[2]))
                : false;
        },
        // Derive vibration disabled from the current workspace ui state
        get vibrationDisabled() {
            return (
                app().busy ||
                (!app().simulated && !this.vibration?.writable) ||
                this.vibrationFirmwareBlocked
            );
        },
        // Derive vibration requirement from the current workspace ui state
        get vibrationRequirement() {
            const minimum = this.vibration?.firmware_requirements[app().device?.variant_id || ''];
            return minimum
                ? `Requires firmware ${minimum} or later. ${this.firmware?.installed ? 'Installed: ' + this.firmware.installed : 'Installed firmware has not been measured.'}`
                : '';
        },
        // Sync the vibration
        syncVibration(input: HTMLInputElement) {
            input.indeterminate = this.vibrationEnabled === null;
        },
        // Change the vibration
        async changeVibration(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const enabled = input.checked;
            if (!this.hasReviewedConfiguration) {
                input.checked = this.vibrationEnabled === true;
                this.syncVibration(input);
                // WebKit does not focus checkboxes on pointer activation. Give
                // the nested dialog a reliable control to return focus to
                input.focus();
                this.openConfiguration(enabled);
                return;
            }
            this.vibrationEnabled = enabled;
            await this.save();
        },
        // Fingerprint the device configuration and vibration state being reviewed
        reviewedConfigurationSignature() {
            return JSON.stringify([
                app().deviceOutput.vibration_enabled ?? null,
                app().deviceOutput.device_configuration || {},
            ]);
        },
        // Retain focus on the invoking control before opening configuration review
        reviewConfiguration(event?: Event) {
            (event?.currentTarget as HTMLElement | null)?.focus();
            this.openConfiguration();
        },
        // Open the configuration
        openConfiguration(enabled?: boolean) {
            if (!app().device || !this.configurationFields.length) return;
            this.configurationGeneration++;
            this.configurationDeviceId = app().deviceId;
            this.configurationSignature = this.reviewedConfigurationSignature();
            this.configurationError = '';
            this.configurationDraft = Object.fromEntries(
                // Convert each entry into the representation required by workspace ui
                this.configurationFields.map((field) => {
                    const value = app().deviceOutput.device_configuration?.[field.id];
                    return [field.id, typeof value === 'boolean' ? (value ? 'on' : 'off') : ''];
                }),
            );
            const value = enabled ?? this.vibrationEnabled;
            this.configurationVibration = typeof value === 'boolean' ? (value ? 'on' : 'off') : '';
            this.configurationBaseline = this.configurationCloseValue();
            (this.$refs.configuration as HTMLDialogElement).showModal();
        },
        // Close the configuration
        closeConfiguration() {
            if (this.configurationSaving) return;
            this.configurationGeneration++;
            (this.$refs.configuration as HTMLDialogElement | undefined)?.close();
        },
        // Replace the edited configuration field in the unsaved draft
        configurationChanged(event: Event) {
            const input = event.currentTarget as HTMLSelectElement;
            this.configurationDraft = {
                ...this.configurationDraft,
                [input.dataset.field!]: input.value,
            };
        },
        // Save the configuration
        async saveConfiguration() {
            if (!this.configurationCanApply) return;
            if (
                app().deviceId !== this.configurationDeviceId ||
                this.configurationSignature !== this.reviewedConfigurationSignature()
            ) {
                this.configurationError =
                    'Device configuration changed. Cancel and reopen the review.';
                return;
            }
            const generation = this.configurationGeneration;
            this.configurationSaving = true;
            this.configurationError = '';
            try {
                const output = {
                    ...cloneJson(app().deviceOutput),
                    vibration_enabled: this.configurationVibration === 'on',
                    device_configuration: Object.fromEntries(
                        this.configurationFields.map((field) => [
                            field.id,
                            this.configurationDraft[field.id] === 'on',
                        ]),
                    ),
                };
                app().apply(
                    await app().command('set_device_output', {
                        deviceId: this.configurationDeviceId,
                        output,
                    }),
                );
                if (generation === this.configurationGeneration) {
                    this.loaded = '';
                    this.load();
                    this.configurationSaving = false;
                    this.closeConfiguration();
                }
            } catch (error) {
                if (generation === this.configurationGeneration)
                    this.configurationError = String(error);
            } finally {
                if (generation === this.configurationGeneration) this.configurationSaving = false;
            }
        },
        // Load firmware for the workspace ui workflow
        async loadFirmware(check = false) {
            const deviceId = app().deviceId;
            if (!deviceId || this.firmwareBusy) return;
            const generation = ++this.firmwareGeneration;
            this.firmwareBusy = true;
            this.firmwareError = '';
            try {
                const result = await app().command(check ? 'check_firmware' : 'firmware_status', {
                    deviceId,
                });
                if (generation === this.firmwareGeneration && deviceId === app().deviceId)
                    this.firmware = result;
            } catch (error) {
                if (generation === this.firmwareGeneration && deviceId === app().deviceId)
                    this.firmwareError = 'Could not check firmware: ' + String(error);
            } finally {
                if (generation === this.firmwareGeneration) this.firmwareBusy = false;
            }
        },
        // Check the firmware
        async checkFirmware() {
            await this.loadFirmware(true);
        },
        // Open the firmware instructions
        async openFirmwareInstructions() {
            await app().command('open_firmware_instructions', { deviceId: app().deviceId });
        },
        // Release workspace ui state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.closeOutput();
            window.clearInterval(this.powerTimer);
            if (this.resumeListener)
                window.removeEventListener('editor-window-resumed', this.resumeListener);
        },
        // Derive app from the current workspace ui state
        get app() {
            return app();
        },
        // Derive channels from the current workspace ui state
        get channels() {
            return app().device?.manifest.capabilities.lighting
                ? app().device!.manifest.capabilities.lighting_channels
                : [];
        },
        // Derive heading from the current workspace ui state
        get heading() {
            return this.channels.length ? 'Brightness and lighting' : 'Device controls';
        },
        // Derive image output warning from the current workspace ui state
        get imageOutputWarning() {
            return app().device?.manifest.capabilities.image_output_unavailable_reason || '';
        },
        // Derive output summary from the current workspace ui state
        get outputSummary() {
            const output = app().effectiveDeviceOutput;
            const parts = [];
            if (app().device?.manifest.capabilities.brightness)
                parts.push(`Screen ${output.brightness}%`);
            if (this.channels.length) parts.push(`Lighting ${output.lighting_brightness}%`);
            if (app().device?.manifest.capabilities.sleep)
                parts.push(output.sleeping ? 'Sleeping' : 'Awake');
            if (this.vibration?.toggle)
                parts.push(
                    output.vibration_enabled == null
                        ? 'Vibration: Device default'
                        : output.vibration_enabled
                          ? 'Vibration on'
                          : 'Vibration off',
                );
            return parts.join(' · ');
        },
        // Derive has display settings from the current workspace ui state
        get hasDisplaySettings() {
            const capabilities = app().device?.manifest.capabilities;
            return Boolean(
                capabilities?.brightness || this.channels.length || capabilities?.boot_logo,
            );
        },
        // Derive has keyboard settings from the current workspace ui state
        get hasKeyboardSettings() {
            return Boolean(app().device?.manifest.capabilities.keyboard);
        },
        // Derive output tabs from the current workspace ui state
        get outputTabs() {
            return [
                ...(this.hasDisplaySettings ? ['display'] : []),
                ...(this.hasKeyboardSettings ? ['keyboard'] : []),
                'behavior',
                'firmware',
            ];
        },
        // Select the output tab
        selectOutputTab(tab: string) {
            if (!this.outputTabs.includes(tab)) return;
            this.outputTab = tab;
            if (tab === 'keyboard') window.dispatchEvent(new Event('keyboard-settings-open'));
            (this.$refs.outputBody as HTMLElement).scrollTop = 0;
        },
        // Navigate available device-output tabs and move focus with supported keys
        navigateOutputTabs(event: KeyboardEvent) {
            if (!tabNavigationKey(event.key)) return;
            event.preventDefault();
            const tabs = this.outputTabs;
            const next = nextTab(tabs, this.outputTab, event.key);
            if (!next) return;
            this.selectOutputTab(next);
            (event.currentTarget as HTMLElement)
                .querySelector<HTMLButtonElement>(`#device-output-tab-${this.outputTab}`)
                ?.focus();
        },
        // Show the output
        showOutput(event?: Event) {
            if (!this.supportsOutput) return;
            (event?.currentTarget as HTMLElement | null)?.focus();
            this.selectOutputTab(this.outputTabs[0]);
            (this.$refs.output as HTMLDialogElement).showModal();
            void this.loadFirmware();
        },
        // Close the output
        async closeOutput() {
            this.configurationGeneration++;
            this.configurationSaving = false;
            this.firmwareGeneration++;
            this.firmwareBusy = false;
            const dialog = this.$refs.output as HTMLDialogElement | undefined;
            if (!dialog) return;
            const pending: Promise<unknown>[] = [];
            dialog
                .querySelector('[data-keyboard-settings]')
                ?.dispatchEvent(new CustomEvent('discard-keyboard', { detail: { pending } }));
            try {
                await Promise.all(pending);
            } catch {
                return;
            }
            for (const child of [
                ...dialog.querySelectorAll<HTMLDialogElement>('dialog[open]'),
            ].reverse())
                child.close();
            dialog.close();
        },
        // Derive supports output from the current workspace ui state
        get supportsOutput() {
            const capabilities = app().device?.manifest.capabilities;
            return Boolean(
                capabilities &&
                (capabilities.brightness ||
                    capabilities.lighting ||
                    capabilities.sleep ||
                    capabilities.keyboard),
            );
        },
        // Toggle the sleep
        async toggleSleep() {
            if (!app().device?.manifest.capabilities.sleep) return;
            try {
                app().apply(
                    await app().command('set_device_sleep', {
                        deviceId: app().deviceId,
                        sleeping: !app().effectiveDeviceOutput.sleeping,
                    }),
                );
                await app().refreshPower(true);
            } catch {}
        },
        // Prefer a manifest channel label and otherwise format a numbered LED label
        channelName(id: string) {
            return (
                app().device?.manifest.controls.find((c) => c.id === id)?.label ||
                id.replace(/^led-(\d+)$/, 'LED $1')
            );
        },
        // Derive output signature from the current workspace ui state
        get outputSignature() {
            return JSON.stringify([app().deviceId, app().deviceOutput]);
        },
        // Initialize workspace ui state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'device output', [
                'changeVibration',
                'saveConfiguration',
                'loadFirmware',
                'checkFirmware',
                'openFirmwareInstructions',
                'toggleSleep',
                'colorChanged',
                'save',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(this, ['save', 'saveConfiguration'], {
                name: 'Device configuration',
                retainOnUnmount: true,
                // Return a snapshot of the current workspace ui state
                state: () =>
                    (this.$refs.configuration as HTMLDialogElement | undefined)?.open &&
                    this.configurationCloseValue() !== this.configurationBaseline
                        ? 'dirty'
                        : 'clean',
                // Discard pending workspace ui edits without saving them
                discard: () => this.closeOutput(),
                // Release resources retained by the workspace ui operation
                release: () => {
                    window.clearInterval(this.powerTimer);
                    this.powerTimer = 0;
                    this.closeOutput();
                },
            });
            this.load();
            this.resumeListener = () => this.startPowerReader();
            window.addEventListener('editor-window-resumed', this.resumeListener);
            this.startPowerReader();
            this.$watch('outputSignature', () => this.load());
            // Complete the deferred workspace ui work using the current operation state
            this.$watch('$store.app.deviceId', () => {
                this.closeOutput();
                this.firmwareGeneration++;
                this.firmwareBusy = false;
                this.firmware = null;
                this.firmwareError = '';
                this.load();
            });
            this.$watch('$store.app.route', () => this.closeOutput());
        },
        // Start the power reader
        startPowerReader() {
            window.clearInterval(this.powerTimer);
            // Refresh workspace ui data while keeping the view synchronized
            const refresh = () => {
                if (document.hidden || windowWork.quiescing || windowCloseOwners.closing) return;
                void windowWork
                    .run('device power status', () => app().refreshPower())
                    .catch(() => {});
            };
            refresh();
            this.powerTimer = window.setInterval(refresh, 1000);
        },
        // Load the data required by the workspace ui workflow
        load() {
            const output = app().deviceOutput;
            const signature = this.outputSignature;
            if (signature === this.loaded) return;
            this.loaded = signature;
            this.vibrationEnabled = output.vibration_enabled ?? null;
            this.brightness = output.brightness;
            this.lightingBrightness = output.lighting_brightness;
            this.colors = output.lighting_colors.map(
                (rgb) => '#' + rgb.map((v) => v.toString(16).padStart(2, '0')).join(''),
            );
        },
        // Update the edited lighting color and save the device-output settings
        async colorChanged(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            this.colors[Number(input.dataset.index)] = input.value;
            await this.save();
        },
        // Save pending changes through the workspace ui workflow
        async save() {
            if (!app().device) return;
            try {
                app().apply(
                    await app().command('set_device_output', {
                        deviceId: app().deviceId,
                        output: {
                            ...cloneJson<any>(app().deviceOutput),
                            sleeping: app().deviceOutput.sleeping,
                            brightness: Number(this.brightness),
                            vibration_enabled: this.vibrationEnabled,
                            lighting_brightness: Number(this.lightingBrightness),
                            lighting_colors: this.colors.map((color) =>
                                [1, 3, 5].map((offset) =>
                                    Number.parseInt(color.slice(offset, offset + 2), 16),
                                ),
                            ),
                        },
                    }),
                );
            } catch {
                this.loaded = '';
                this.load();
            }
        },
    }));
    Alpine.data('workspaceTools', () => ({
        unregisterCloseOwner: undefined as (() => void) | undefined,
        // Initialize workspace ui state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'workspace tools', [
                'compareHistory',
                'inspectHistoryChange',
                'undo',
                'redo',
                'clear',
                'paste',
            ]);
            this.unregisterCloseOwner = windowCloseOwners.register({
                name: 'Edit history',
                // Return a snapshot of the current workspace ui state
                state: () => 'clean',
                // Discard pending workspace ui edits without saving them
                discard: () => this.closeHistory(),
                // Release resources retained by the workspace ui operation
                release: () => this.closeHistory(),
            });
            this.$watch('$store.app.route', () => this.closeHistory());
            // Complete the deferred workspace ui work using the current operation state
            this.$watch('$store.app.snapshot.configuration_revision', () => {
                if (this.comparison || this.historyLoading) {
                    ++this.historyEpoch;
                    this.comparison = null;
                    this.historyPreview = null;
                    this.historyLoading = false;
                    this.previewLoading = false;
                    this.historyError = 'Workspace changed. Select the history entry again.';
                }
            });
        },
        // Release workspace ui state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
            this.closeHistory();
        },
        // Derive selection summary from the current workspace ui state
        get selectionSummary() {
            const count = app().selectedIds.length;
            return count ? `${count} selected` : 'No selection';
        },
        // Derive can edit selection from the current workspace ui state
        get canEditSelection() {
            return Boolean(
                app().device &&
                app().profile &&
                app().page &&
                app().selectedIds.length &&
                !app().selectedIds.some((id) => app().isProtectedControl(id)) &&
                !app().busy,
            );
        },
        // Derive can copy selection from the current workspace ui state
        get canCopySelection() {
            return (
                this.canEditSelection &&
                app().selectedIds.some((id) => hasBindingContent(app().page?.bindings[id]))
            );
        },
        historyPage: 1,
        historyStack: 'undo' as 'undo' | 'redo',
        comparison: null as HistoryComparison | null,
        historyPreview: null as HistoryControlComparison | null,
        historyLoading: false,
        previewLoading: false,
        historyError: '',
        historyEpoch: 0,
        historyRevision: 0,
        selectedHistoryChange: '',
        // Derive history change from the current workspace ui state
        get historyChange(): HistoryChange | undefined {
            return this.comparison?.changes.find(
                (change) => change.id === this.selectedHistoryChange,
            );
        },
        // Distinguish absent and empty values in history comparisons
        historyValue(value: string | null) {
            return value === null ? 'Not present' : value === '' ? '(empty)' : value;
        },
        // Reset history pagination when the selected history stack changes
        historyStackChanged() {
            this.historyPage = 1;
        },
        // Request a revision-scoped history comparison and invalidate earlier comparison work
        async compareHistory(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            const epoch = ++this.historyEpoch,
                revision = app().snapshot.configuration_revision;
            this.historyLoading = true;
            this.historyError = '';
            this.comparison = null;
            this.historyPreview = null;
            try {
                const comparison = await app().bridge.command('compare_history', {
                    id,
                    expectedRevision: revision,
                });
                if (epoch !== this.historyEpoch) return;
                this.comparison = comparison;
                this.historyRevision = revision;
                await this.inspectHistoryChange(
                    comparison.changes.find((change) => change.control)?.id ||
                        comparison.changes[0]?.id ||
                        '',
                );
            } catch (error) {
                if (epoch === this.historyEpoch) this.historyError = String(error);
            } finally {
                if (epoch === this.historyEpoch) this.historyLoading = false;
            }
        },
        // Choose the history change
        chooseHistoryChange(event: Event) {
            void this.inspectHistoryChange(
                (event.currentTarget as HTMLElement).dataset.id || '',
            ).catch(() => {});
        },
        // Select a history change and clear its previous preview before loading the new one
        async inspectHistoryChange(id: string) {
            this.selectedHistoryChange = id;
            this.historyPreview = null;
            this.previewLoading = false;
            this.historyError = '';
            const control = this.historyChange?.control,
                entry = this.comparison?.item.id,
                epoch = this.historyEpoch;
            if (!control || !entry) return;
            this.previewLoading = true;
            try {
                const preview = await app().bridge.command('preview_history_control', {
                    id: entry,
                    address: control,
                    expectedRevision: this.historyRevision,
                });
                if (epoch === this.historyEpoch && this.selectedHistoryChange === id)
                    this.historyPreview = preview;
            } catch (error) {
                if (epoch === this.historyEpoch && this.selectedHistoryChange === id)
                    this.historyError = String(error);
            } finally {
                if (epoch === this.historyEpoch && this.selectedHistoryChange === id)
                    this.previewLoading = false;
            }
        },
        // Derive history window from the current workspace ui state
        get historyWindow() {
            return paginate(
                [...app().snapshot.history[this.historyStack]].reverse(),
                this.historyPage,
                10,
            );
        },
        // Move to the previous history page without crossing the first page
        historyPrevious() {
            this.historyPage = Math.max(1, this.historyWindow.page - 1);
        },
        // Move to the next history page without crossing the last page
        historyNext() {
            this.historyPage = Math.min(this.historyWindow.pages, this.historyWindow.page + 1);
        },
        // Derive app from the current workspace ui state
        get app() {
            return app();
        },
        // Format the history timestamp in the user's local date and time format
        historyTime(timestamp: number) {
            return new Date(timestamp).toLocaleString();
        },
        // Derive undo items from the current workspace ui state
        get undoItems() {
            return this.historyWindow.items;
        },
        // Derive redo items from the current workspace ui state
        get redoItems() {
            return [...app().snapshot.history.redo].reverse();
        },
        // Show the history
        showHistory(event?: Event) {
            const dialog = this.$refs.historyDialog as HTMLDialogElement;
            if (!dialog.open) {
                // WebKit does not focus buttons on pointer activation
                (event?.currentTarget as HTMLElement | null)?.focus();
                dialog.showModal();
            }
        },
        // Close the history
        closeHistory() {
            ++this.historyEpoch;
            this.comparison = null;
            this.historyPreview = null;
            this.historyLoading = false;
            this.previewLoading = false;
            this.historyError = '';
            (this.$refs.historyDialog as HTMLDialogElement)?.close();
        },
        // Request workspace undo while leaving error presentation to the shared command path
        async undo() {
            try {
                await app().history('undo');
            } catch {}
        },
        // Request workspace redo while leaving error presentation to the shared command path
        async redo() {
            try {
                await app().history('redo');
            } catch {}
        },
        // Clear the retained workspace ui value
        async clear() {
            try {
                await app().history('clear');
            } catch {}
        },
        // Paste copied bindings through the shared validation and error-reporting path
        async paste() {
            try {
                await app().pasteBindings();
            } catch {}
        },
        // Copy the selection
        copySelection() {
            app().copyBindings();
        },
    }));
    Alpine.data('automation', () => ({
        captureMessage: '',
        error: '',
        saving: false,
        opened: false,
        draftEpoch: 0,
        deviceId: '',
        deviceKey: '',
        deviceName: '',
        portDependent: false,
        expectedRevision: 0,
        expectedEnabled: false,
        expectedPaused: false,
        expectedRestoreOnExit: false,
        restoreOnExit: false,
        enabled: false,
        paused: false,
        draftRules: [] as AppRule[],
        closeBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        application: '',
        editingRuleId: '',
        titleValue: '',
        titleMode: 'contains',
        titleCaseSensitive: false,
        scheduleEnabled: false,
        scheduleStart: '09:00',
        scheduleEnd: '17:00',
        days: ['0', '1', '2', '3', '4'],
        profileId: '',
        pageId: '',
        weekdays: [
            { value: '0', label: 'Mon' },
            { value: '1', label: 'Tue' },
            { value: '2', label: 'Wed' },
            { value: '3', label: 'Thu' },
            { value: '4', label: 'Fri' },
            { value: '5', label: 'Sat' },
            { value: '6', label: 'Sun' },
        ],
        evaluations: [] as RuleEvaluation[],
        status: { available: false, reason: 'Loading…', application: null } as AutomationStatus,
        // Derive app from the current workspace ui state
        get app() {
            return app();
        },
        // Derive rules from the current workspace ui state
        get rules() {
            return this.draftRules;
        },
        // Derive target pages from the current workspace ui state
        get targetPages() {
            return (
                app().snapshot.profiles.find((profile) => profile.id === this.profileId)?.pages ||
                []
            );
        },
        // Close the value
        closeValue() {
            return JSON.stringify([
                this.draftRules,
                this.enabled,
                this.paused,
                this.restoreOnExit,
                this.application,
                this.editingRuleId,
                this.titleValue,
                this.titleMode,
                this.titleCaseSensitive,
                this.scheduleEnabled,
                this.scheduleStart,
                this.scheduleEnd,
                this.days,
                this.profileId,
                this.pageId,
            ]);
        },
        // Open the dialog
        async openDialog() {
            if (this.opened || !app().device) return;
            this.draftEpoch += 1;
            this.deviceId = app().deviceId;
            this.deviceKey = app().device?.persistence_key || this.deviceId;
            this.deviceName = app().device?.name || 'Device';
            this.portDependent = Boolean(app().device?.identity_port_dependent);
            this.expectedRevision = app().snapshot.configuration_revision;
            this.expectedEnabled = Boolean(app().session?.automation_enabled);
            this.expectedPaused = Boolean(app().session?.automation_paused);
            this.expectedRestoreOnExit = Boolean(app().session?.automation_restore_on_exit);
            this.restoreOnExit = this.expectedRestoreOnExit;
            this.enabled = this.expectedEnabled;
            this.paused = this.expectedPaused;
            this.draftRules = cloneJson(
                app().snapshot.rules.filter((rule) => rule.device_key === this.deviceKey),
            );
            this.cancelEdit();
            this.error = '';
            this.evaluations = [];
            this.opened = true;
            this.closeBaseline = this.closeValue();
            const dialog = this.$refs.automationDialog as HTMLDialogElement;
            if (!dialog.open) dialog.showModal();
            try {
                await this.refresh();
            } catch (error) {
                this.status.reason = String(error);
            }
        },
        // Close the dialog
        closeDialog() {
            if (this.saving) return;
            this.opened = false;
            this.draftEpoch += 1;
            this.draftRules = [];
            this.cancelEdit();
            this.error = '';
            this.closeBaseline = '';
            (this.$refs.automationDialog as HTMLDialogElement).close();
        },
        // Browse the application
        async browseApplication() {
            const epoch = this.draftEpoch;
            try {
                const path = await openFileDialog({
                    title: 'Choose an application',
                    multiple: false,
                    directory: false,
                    fileAccessMode: 'scoped',
                });
                if (typeof path !== 'string' || !this.opened || epoch !== this.draftEpoch) return;
                const application = await app().command('resolve_automation_application', { path });
                if (!this.opened || epoch !== this.draftEpoch) return;
                this.application = application;
                this.captureMessage = '';
            } catch (error) {
                if (this.opened && epoch === this.draftEpoch) this.captureMessage = String(error);
            }
        },
        // Refresh saved rules and evaluate them without applying unsaved rule drafts
        async previewRules() {
            // Decisions describe saved rules only; drafts never change the running observer
            try {
                await this.refresh();
                this.evaluations = await app().command('evaluate_rules', {
                    deviceId: this.deviceId,
                });
            } catch {}
        },
        // Join the saved rule's evaluation reasons into a concise explanation
        explanation(id: string) {
            return this.evaluations.find((rule) => rule.id === id)?.reasons.join(' · ') || '';
        },
        // Request accessibility authorization and refresh title-observation status
        async authorizeTitle() {
            try {
                await app().command('authorize_accessibility');
                await this.refresh();
            } catch {}
        },
        // Edit the rule
        editRule(event: Event) {
            const rule = this.rules.find(
                (r) => r.id === (event.currentTarget as HTMLElement).dataset.id,
            );
            if (!rule) return;
            this.editingRuleId = rule.id;
            this.application = rule.application;
            this.titleValue = rule.title?.value || '';
            this.titleMode = rule.title?.mode || 'contains';
            this.titleCaseSensitive = rule.title?.case_sensitive || false;
            this.scheduleEnabled = Boolean(rule.schedule);
            this.days = rule.schedule?.days.map(String) || ['0', '1', '2', '3', '4'];
            // Format minutes since midnight as a zero-padded hour and minute pair
            const time = (v: number) =>
                String(Math.floor(v / 60)).padStart(2, '0') + ':' + String(v % 60).padStart(2, '0');
            this.scheduleStart = time(rule.schedule?.start ?? 540);
            this.scheduleEnd = time(rule.schedule?.end ?? 1020);
            this.profileId = rule.profile_id;
            this.$nextTick(() => {
                this.pageId = rule.page_id || '';
            });
        },
        // Cancel the edit
        cancelEdit() {
            this.editingRuleId = '';
            this.application = '';
            this.titleValue = '';
            this.titleMode = 'contains';
            this.titleCaseSensitive = false;
            this.captureMessage = '';
            this.scheduleEnabled = false;
            this.scheduleStart = '09:00';
            this.scheduleEnd = '17:00';
            this.days = ['0', '1', '2', '3', '4'];
            this.profileId = app().profile?.id || '';
            this.pageId = '';
        },
        // Initialize workspace ui state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'automation', [
                'openDialog',
                'browseApplication',
                'previewRules',
                'authorizeTitle',
                'refresh',
                'applyDraft',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(this, ['applyDraft'], {
                name: 'Automatic profiles',
                retainOnUnmount: false,
                // Return a snapshot of the current workspace ui state
                state: () =>
                    this.opened && this.closeBaseline && this.closeValue() !== this.closeBaseline
                        ? 'dirty'
                        : 'clean',
                // Discard pending workspace ui edits without saving them
                discard: () => this.closeDialog(),
            });
            this.$watch('profileId', () => {
                this.pageId = '';
            });
        },
        // Release workspace ui state and associated lifecycle resources
        destroy() {
            this.unregisterCloseOwner?.();
        },
        // Refresh workspace ui data while keeping the view synchronized
        async refresh() {
            this.status = await app().bridge.command('automation_status');
        },
        // Capture the current workspace ui state
        capture() {
            const epoch = this.draftEpoch;
            void windowWork
                // Complete the deferred workspace ui work using the current operation state
                .run('automation capture', async () => {
                    await this.refresh();
                    if (!this.opened || epoch !== this.draftEpoch) return;
                    if (this.status.application) {
                        this.application = this.status.application;
                        this.captureMessage = '';
                    } else
                        this.captureMessage =
                            'No application detected yet. Focus another application, then return here, or browse for one.';
                })
                .catch(() => {});
        },
        // Toggle the current workspace ui selection
        toggle() {
            this.enabled = !this.enabled;
            this.paused = false;
        },
        // Resume the paused workspace ui operation
        resume() {
            this.enabled = true;
            this.paused = false;
        },
        // Add the workspace ui operation
        add() {
            this.error = '';
            if (
                (!this.application.trim() && !this.titleValue.trim() && !this.scheduleEnabled) ||
                !this.profileId
            ) {
                this.error = 'Choose a condition and target profile.';
                return false;
            }
            if (
                this.scheduleEnabled &&
                (!this.days.length ||
                    !/^\d{2}:\d{2}$/.test(this.scheduleStart) ||
                    !/^\d{2}:\d{2}$/.test(this.scheduleEnd))
            ) {
                this.error = 'Choose at least one weekday and valid start/end times.';
                return false;
            }
            const rule: AppRule = {
                id: this.editingRuleId || crypto.randomUUID(),
                device_key: this.deviceKey,
                application: this.application.trim(),
                title: this.titleValue.trim()
                    ? {
                          value: this.titleValue,
                          mode: this.titleMode,
                          case_sensitive: this.titleCaseSensitive,
                      }
                    : undefined,
                schedule: this.scheduleEnabled
                    ? {
                          days: this.days.map(Number),
                          start:
                              Number(this.scheduleStart.slice(0, 2)) * 60 +
                              Number(this.scheduleStart.slice(3)),
                          end:
                              Number(this.scheduleEnd.slice(0, 2)) * 60 +
                              Number(this.scheduleEnd.slice(3)),
                      }
                    : undefined,
                profile_id: this.profileId,
                page_id: this.pageId || null,
            };
            const index = this.draftRules.findIndex((r) => r.id === rule.id);
            if (index < 0) this.draftRules.push(rule);
            else this.draftRules.splice(index, 1, rule);
            this.cancelEdit();
            this.evaluations = [];
            return true;
        },
        // Remove the workspace ui operation
        remove(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            this.draftRules = this.draftRules.filter((r) => r.id !== id);
            if (this.editingRuleId === id) this.cancelEdit();
            this.evaluations = [];
        },
        // Swap the selected draft rule with a neighboring rule when both indices are valid
        reorder(event: Event) {
            const el = event.currentTarget as HTMLElement;
            const index = this.draftRules.findIndex((r) => r.id === el.dataset.id);
            const other = index + Number(el.dataset.direction);
            if (index < 0 || other < 0 || other >= this.draftRules.length) return;
            [this.draftRules[index], this.draftRules[other]] = [
                this.draftRules[other],
                this.draftRules[index],
            ];
            this.evaluations = [];
        },
        // Apply the draft
        async applyDraft() {
            if (this.saving || !this.opened) return;
            if (
                (this.editingRuleId ||
                    this.application.trim() ||
                    this.titleValue.trim() ||
                    this.scheduleEnabled) &&
                !this.add()
            )
                return;
            this.saving = true;
            this.error = '';
            try {
                const snapshot = await app().command('save_automation_configuration', {
                    request: {
                        device_id: this.deviceId,
                        expected_revision: this.expectedRevision,
                        expected_enabled: this.expectedEnabled,
                        expected_paused: this.expectedPaused,
                        expected_restore_on_exit: this.expectedRestoreOnExit,
                        restore_on_exit: this.restoreOnExit,
                        rules: cloneJson(this.draftRules),
                        enabled: this.enabled,
                        paused: this.paused,
                    },
                });
                app().apply(snapshot);
                this.saving = false;
                this.closeDialog();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.saving = false;
            }
        },
        // Label a rule with its target profile and optional page name
        ruleName(rule: AppRule) {
            const profile = app().snapshot.profiles.find((p) => p.id === rule.profile_id);
            return profile
                ? profile.name +
                      (rule.page_id
                          ? ' / ' +
                            (profile.pages.find((p) => p.id === rule.page_id)?.name ||
                                'Missing page')
                          : '')
                : 'Missing profile';
        },
    }));
}
