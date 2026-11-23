import { registerProfileReference } from './profile-reference';
import { registerNavigationMap } from './navigation-map';
import { previewBrightness } from './preview-brightness';
import { devicePreviewBackground, isWallpaperAperture } from './device-preview-background';
import { editorPreviewWork } from './editor-preview-work';
import { isKeyboardNavigationPage, isKeyboardNavigationControl } from './keyboard-navigation';
import { ActionDraftTree, type ActionNode } from './action-draft-tree';
import {
    controlEventRows,
    eventCount,
    convertControlEvents,
    eventTiming,
    holdBounds,
    isSwipeStrip,
    swipeStripEvents,
    multiPressBounds,
    standardControlAction,
} from './control-events';
import { setLiveActionPreview, clearLiveActionPreview } from './live-action-preview';
import { bindingScopePlan, scopedBindingEdits, type BindingScope } from './binding-scope';
import { dialActionDefault } from './dial-defaults';
import { dialActionPresentation, oppositeTurnAction } from './dial-actions';
import {
    usesPluginBackground,
    showsTransparentBackground,
    changeBackgroundRgb,
    changeBackgroundAlpha,
} from './background-color';
import { cloneJson } from './json-clone';
import {
    bindingDraftFields,
    bindingDraftValue,
    loadBindingDraft,
    restoreBindingDraft,
    commitBindingDraft,
    previewBindingDraft,
    type BindingDraft,
} from './binding-draft';
import { resolvePaletteBinding } from './palette-binding';
import {
    updateInterval,
    updateMode,
    chooseUpdateMode,
    type UpdateModeSelection,
} from './plugin-refresh';
import { nextTab, tabNavigationKey } from './tab-navigation';
import { InspectorActionDraft } from './inspector-action-draft';
import { openMediaLibrary } from './media-library';
import { detachDraftMedia } from './media-removal';
import { retainMediaFor, releaseMediaOwner } from './media-leases';
import { PluginSettingDrafts } from './plugin-setting-drafts';
import { windowCloseOwners } from './window-close-owners';
import { registerCloseDraft } from './close-releases';
import Alpine from '@alpinejs/csp';
import { native } from './bridge';
import { actionKey, builtinCatalog, walkAction } from './builtin-actions';
import { chooseMediaInput, managedFormats, mediaFormats, selectMediaInput } from './media-input';
import { editPluginSettings } from './plugin-settings';
import type { Action, Control, Profile } from './types';

import type { Draft, Edit, MediaOptions, PluginAppearance } from '../generated/runtime';
import { openActionPicker } from './action-picker';
import { hasBindingContent } from './bindings';
import { DEFAULT_BACKGROUND, backgroundRgb, transparentBackground } from './background-color';
import { defaultMedia } from './bridge';
import { defaultAppearance } from './features';
import { actionAppearanceDefaults } from './action-appearance-defaults';
import { defaultAction } from './macro-editor';
import {
    ownWindowMethods,
    windowWork,
    WindowQuiescingError,
    type WindowFence,
} from './window-work';
import { displayPanels, type DisplayPanel } from './display-panels';
import { previewGeometry } from './preview-layout';
import devicePalettes from './device-palettes.v1.json';

import { registerAppearanceEditor } from './appearance-editor';
import { registerIconSelector } from './icon-selector';
import { registerProfileCreator } from './profile-creator';
import type { Binding } from './types';

import {
    app,
    bindingSignature,
    draftDevice,
    draftQueue,
    icons,
    pendingDraftWrites,
    ownDraftWork,
    rememberWindowDraft,
    forgetWindowDraft,
    updateWindowDraftState,
    swallow,
    type PendingDeletion,
} from './route-context';
// Register the editor handler with its owner
export function register() {
    const alternatives = new PluginSettingDrafts();
    let suspended: { key: string; scope: BindingScope; reviewed: string } | null = null;
    windowCloseOwners.register({
        name: 'Alternative plugin settings',
        state: () => (alternatives.size ? 'dirty' : 'clean'),
        discard: () => alternatives.clear(),
    });
    registerProfileReference(Alpine, app);
    registerNavigationMap(Alpine, app);
    registerAppearanceEditor();
    registerIconSelector();
    registerProfileCreator(Alpine, app);
    Alpine.data('editor', () => ({
        draggedPage: null as { id: string; profileId: string; revision: number } | null,
        pageDropTarget: '',
        pageScrollbarObserver: null as ResizeObserver | null,
        locateToken: '',
        locatePending: false,
        locateGeneration: 0,
        earlyLocate: [] as import('./types').RuntimeEvent[],
        // Derive locate reason from the current editor state
        get locateReason() {
            return !app().device?.connected
                ? 'Connect a dock to locate a control.'
                : app().simulated
                  ? 'Locate requires a physical dock.'
                  : app().effectiveDeviceOutput.sleeping
                    ? 'Wake the dock to locate a control.'
                    : '';
        },
        // Arm or cancel control-location mode while rejecting stale device responses
        async locateControl() {
            if (this.locatePending) return;
            if (this.locateToken) {
                await this.cancelLocate();
                return;
            }
            const generation = ++this.locateGeneration;
            const deviceId = app().deviceId;
            this.locatePending = true;
            this.earlyLocate = [];
            try {
                const token = await app().bridge.command('arm_locate', { deviceId });
                if (generation !== this.locateGeneration || deviceId !== app().deviceId) {
                    await app().bridge.command('cancel_locate', { token });
                    return;
                }
                this.locateToken = token;
                const early = this.earlyLocate.find((event) => event.message === token);
                if (early) this.located(new CustomEvent('dock-locate', { detail: early }));
            } catch (error) {
                if (generation === this.locateGeneration) app().notify(String(error), 'error');
            } finally {
                if (generation === this.locateGeneration) {
                    this.locatePending = false;
                    this.earlyLocate = [];
                }
            }
        },
        // Cancel the locate
        async cancelLocate() {
            this.locateGeneration++;
            this.locatePending = false;
            this.earlyLocate = [];
            const token = this.locateToken;
            this.locateToken = '';
            if (token) await app().bridge.command('cancel_locate', { token });
        },
        // Match a location result to the active token before selecting the reported control
        located(event: CustomEvent<import('./types').RuntimeEvent>) {
            if (event.detail.message !== this.locateToken) {
                if (this.locatePending)
                    this.earlyLocate = [...this.earlyLocate.slice(-7), event.detail];
                return;
            }
            this.locateToken = '';
            if (
                event.detail.kind === 'locate-control' &&
                event.detail.device_id === app().deviceId &&
                event.detail.control
            ) {
                app().selectedControls = [];
                app().controlId = event.detail.control.control_id;
                app().notify('Control located', 'success');
            }
        },
        deletion: null as PendingDeletion | null,
        // Derive app from the current editor state
        get app() {
            return app();
        },
        // Derive has swipe from the current editor state
        get hasSwipe() {
            return (
                Boolean(app().profile) &&
                app().device?.manifest.controls.some((c) => c.semantics.includes('swipe'))
            );
        },
        // Derive swipe help from the current editor state
        get swipeHelp() {
            const directions = app().device?.manifest.capabilities.touch_panel?.swipe_directions;
            return directions?.includes('up') && directions.includes('down')
                ? 'By default, swipe left/right to change pages. Swipe up for the next profile or down for the previous profile on hardware that supports these directions.'
                : 'By default, swipe left/right to change pages.';
        },
        // Derive preview help from the current editor state
        get previewHelp() {
            const introduction = 'Select a control to edit its action and appearance.';
            const selection =
                'Ctrl/⌘-click to select multiple controls. Drag to move; Ctrl/⌘/Alt-drag to copy.';
            return [introduction, this.hasSwipe ? this.swipeHelp : '', selection]
                .filter(Boolean)
                .join('\n\n');
        },
        // Focus the preview help
        focusPreviewHelp(event: Event) {
            // Also expose the shared focus tooltip on touch devices and in WebKit
            (event.currentTarget as HTMLButtonElement).focus();
        },
        // Request the previous page through the device's right-swipe behavior
        previousPage() {
            swallow(app().swipePage('right'));
        },
        // Request the next page through the device's left-swipe behavior
        nextPage() {
            swallow(app().swipePage('left'));
        },
        // Derive profile name from the current editor state
        get profileName() {
            return app().profile?.name || 'Your workspace';
        },

        get swipeStrips() {
            const manifest = app().device?.manifest;
            return (manifest?.controls || []).filter((control) => isSwipeStrip(manifest, control));
        },
        configureSwipes(event: Event) {
            const id = (event.currentTarget as HTMLButtonElement).dataset.id;
            if (!id || app().busy || !this.swipeStrips.some((control) => control.id === id)) return;
            app().selectedControls = [];
            app().controlId = id;
        },
        // Derive control summary from the current editor state
        get controlSummary() {
            const controls = app().device?.manifest.controls || [];
            const switches = new Set(
                controls.flatMap((c) => (c.directional_switch ? [c.directional_switch.group] : [])),
            ).size;
            const displays = displayPanels(controls).length;
            const dials = controls.filter((c) => c.kind === 'encoder').length;
            const buttons = controls.filter((c) => c.kind === 'button' && !c.directional_switch);
            const plainButtons = buttons.filter((c) => !c.image_width).length;
            const lcdKeys = buttons.length - plainButtons;
            const parts = [
                plainButtons
                    ? `${lcdKeys} LCD ${lcdKeys === 1 ? 'KEY' : 'KEYS'}`
                    : `${buttons.length} KEYS`,
                `${dials} ${dials === 1 ? 'DIAL' : 'DIALS'}`,
            ];
            if (plainButtons)
                parts.push(`${plainButtons} PLAIN ${plainButtons === 1 ? 'BUTTON' : 'BUTTONS'}`);
            if (switches) parts.push(`${switches} SWITCHES`);
            if (displays) parts.push(`${displays} ${displays === 1 ? 'DISPLAY' : 'DISPLAYS'}`);
            return parts.join(' · ');
        },
        // Derive page count from the current editor state
        get pageCount() {
            return app().profile?.pages.length || 0;
        },
        // Derive can delete profile from the current editor state
        get canDeleteProfile() {
            return Boolean(app().profile) && !app().busy;
        },
        // Derive can delete page from the current editor state
        get canDeletePage() {
            return (
                Boolean(app().page) &&
                !isKeyboardNavigationPage(app().profile, app().page?.id) &&
                this.pageCount > 1 &&
                !app().busy
            );
        },
        // Derive remove page hint from the current editor state
        get removePageHint() {
            return isKeyboardNavigationPage(app().profile, app().page?.id)
                ? 'The Navigation page is built in'
                : this.canDeletePage
                  ? 'Remove page'
                  : 'Keep at least one page';
        },
        // Derive deletion title from the current editor state
        get deletionTitle() {
            if (this.deletion?.kind === 'binding') return `Clear “${this.deletion.name}”?`;
            if (this.deletion?.kind === 'background') return 'Clear device background?';
            return this.deletion
                ? `${this.deletion.kind === 'simulator' ? 'Remove simulator' : this.deletion.kind === 'profile' ? 'Delete profile' : 'Remove page'} “${this.deletion.name}”?`
                : '';
        },
        // Derive deletion description from the current editor state
        get deletionDescription() {
            if (this.deletion?.kind === 'binding')
                return 'This clears the binding and media shown on this control. Unfinished edits remain available as a draft. You can restore saved content from edit history.';
            if (this.deletion?.kind === 'background')
                return 'This removes the background media from this device profile. You can restore it from edit history.';
            if (this.deletion?.kind === 'simulator')
                return 'This removes the simulator and its saved device settings, rules, and drafts. Pending actions are cancelled. Shared profiles, pages and plugins stay intact. This removal cannot be undone.';
            return this.deletion?.kind === 'profile'
                ? 'All pages and bindings in this profile will be removed. You can restore this from edit history.'
                : `This page and its bindings will be removed from “${this.deletion?.profileName || ''}”. You can restore this from edit history.`;
        },
        // Derive deletion label from the current editor state
        get deletionLabel() {
            if (this.deletion?.kind === 'binding') return 'Clear binding';
            if (this.deletion?.kind === 'background') return 'Clear background';
            return this.deletion?.kind === 'simulator'
                ? 'Remove simulator'
                : this.deletion?.kind === 'profile'
                  ? 'Delete profile'
                  : 'Remove page';
        },
        // Initialize editor state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'editor', ['confirmDeletion', 'newPage', 'reorderPage']);
            // Complete the deferred editor work using the current operation state
            this.$watch('$store.app.route', () => {
                this.resetPageDrag();
                this.cancelDeletion();
                void this.cancelLocate();
            });
            this.$watch('$store.app.deviceId', () => {
                this.resetPageDrag();
                void this.cancelLocate();
            });
        },
        // Delete the profile
        deleteProfile(event?: Event) {
            if (!this.canDeleteProfile || !app().profile) return;
            this.openDeletion('profile', event);
        },
        // Remove the page
        removePage(event?: Event) {
            if (!this.canDeletePage || !app().page) return;
            this.openDeletion('page', event);
        },
        // Remove the simulator
        removeSimulator(event: Event) {
            if (app().busy) return;
            const id = (event.currentTarget as HTMLElement).dataset.deviceId || app().deviceId;
            const simulator = app().simulators.find((device) => device.id === id);
            if (!simulator) return;
            this.deletion = { kind: 'simulator', deviceId: id, name: simulator.name };
            // Complete the deferred editor work using the current operation state
            this.$nextTick(() => {
                const dialog = this.$refs.deleteDialog as HTMLDialogElement;
                if (this.deletion && !dialog.open) dialog.showModal();
            });
        },
        // Request the control clear
        requestControlClear(event: CustomEvent<{ controlId?: string }>) {
            const profile = app().profile;
            if (app().busy || this.deletion || !profile) return;
            const controlId = event.detail.controlId;
            if (controlId) {
                const control = app().device?.manifest.controls.find(
                    (control) => control.id === controlId,
                );
                if (
                    !control ||
                    !app().page ||
                    app().controlPending(controlId) ||
                    !app().canClearBinding(controlId)
                )
                    return;
                this.deletion = {
                    kind: 'binding',
                    controlId,
                    address: app().bindingAddress(controlId),
                    deviceId: app().deviceId,
                    profileId: profile.id,
                    name: control.label,
                    expectedRevision: app().configurationRevision,
                };
            } else {
                if (!profile.background_asset) return;
                this.deletion = {
                    kind: 'background',
                    deviceId: app().deviceId,
                    profileId: profile.id,
                    name: profile.name,
                    expectedRevision: app().configurationRevision,
                };
            }
            // Complete the deferred editor work using the current operation state
            this.$nextTick(() => {
                const dialog = this.$refs.deleteDialog as HTMLDialogElement;
                if (this.deletion && !dialog.open) dialog.showModal();
            });
        },
        // Open the deletion
        openDeletion(kind: 'profile' | 'page', event?: Event) {
            // Establish the return target for WebKit pointer activation, too
            (event?.currentTarget as HTMLElement | null)?.focus();
            const profile = app().profile;
            const page = app().page;
            if (!profile || (kind === 'page' && !page)) return;
            this.deletion = {
                kind,
                profileId: profile.id,
                pageId: kind === 'page' ? page!.id : undefined,
                name: kind === 'profile' ? profile.name : page!.name,
                profileName: profile.name,
                expectedRevision: app().configurationRevision,
            };
            // Complete the deferred editor work using the current operation state
            this.$nextTick(() => {
                const dialog = this.$refs.deleteDialog as HTMLDialogElement;
                if (this.deletion && !dialog.open) dialog.showModal();
            });
        },
        // Cancel the deletion
        cancelDeletion() {
            const dialog = this.$refs.deleteDialog as HTMLDialogElement;
            if (dialog?.open) dialog.close();
            this.deletion = null;
        },
        // Confirm the deletion
        async confirmDeletion() {
            const target = this.deletion;
            if (!target || app().busy) return;
            this.cancelDeletion();
            try {
                if (target.kind === 'binding' || target.kind === 'background') {
                    if (
                        app().deviceId !== target.deviceId ||
                        app().profile?.id !== target.profileId ||
                        app().configurationRevision !== target.expectedRevision ||
                        (target.kind === 'binding' &&
                            app().bindingAddress(target.controlId) !== target.address)
                    ) {
                        app().notify(
                            'Configuration changed. Review the content before clearing it.',
                            'warning',
                        );
                        return;
                    }
                    if (target.kind === 'binding') await app().clearBinding(target.controlId);
                    else await app().clearBackground();
                    return;
                }
                if (target.kind === 'simulator') {
                    await app().flushDrafts?.();
                    await draftQueue.value;
                    app().removingSimulatorId = target.deviceId;
                    const snapshot = await app().command('remove_simulator', {
                        deviceId: target.deviceId,
                    });
                    for (const key of new Set([
                        ...Object.keys(app().draftStore),
                        ...pendingDraftWrites.keys(),
                    ])) {
                        if (draftDevice(key) === target.deviceId) {
                            delete app().draftStore[key];
                            pendingDraftWrites.delete(key);
                        }
                    }
                    if (app().deviceId === target.deviceId)
                        app().bindingCleared?.(app().bindingAddress());
                    app().apply(snapshot);
                    app().selectedControls = [];
                    app().notify(`Removed simulator “${target.name}”.`, 'success');
                    return;
                }
                if (target.kind === 'page' && !target.pageId) return;
                const snapshot = await app().command(
                    target.kind === 'profile' ? 'delete_profile' : 'delete_page',
                    target.kind === 'profile'
                        ? { id: target.profileId, expectedRevision: target.expectedRevision }
                        : {
                              profileId: target.profileId,
                              pageId: target.pageId!,
                              expectedRevision: target.expectedRevision,
                          },
                );
                app().apply(snapshot);
                app().notify(
                    `${target.kind === 'profile' ? 'Deleted profile' : 'Removed page'} “${target.name}”.`,
                    'success',
                );
            } catch {
            } finally {
                app().removingSimulatorId = '';
            }
        },
        // Release editor state and associated lifecycle resources
        destroy() {
            this.pageScrollbarObserver?.disconnect();
            this.cancelDeletion();
            void this.cancelLocate();
        },
        // Create a fresh empty page in a cloned profile and save the updated profile
        async newPage() {
            const original = app().profile;
            if (!original) return;
            const profile = structuredClone(Alpine.raw(original)) as Profile;
            const page = {
                id: crypto.randomUUID(),
                name: `Page ${profile.pages.length + 1}`,
                bindings: {},
            };
            profile.pages.push(page);
            try {
                app().apply(
                    await app().command('save_profile', {
                        profile,
                        expectedRevision: app().configurationRevision,
                    }),
                );
                app().apply(
                    await app().command('select_page', {
                        deviceId: app().deviceId,
                        pageId: page.id,
                    }),
                );
            } catch {}
        },
        // Reorder pages only when profile identity, revision and navigation protections still allow it
        async reorderPage(
            pageId: string,
            targetId: string,
            profileId = app().profile?.id,
            revision = app().configurationRevision,
        ) {
            const original = app().profile;
            if (app().busy || !original || original.id !== profileId) return;
            if (
                isKeyboardNavigationPage(original, pageId) ||
                isKeyboardNavigationPage(original, targetId)
            )
                return;
            const from = original.pages.findIndex((page) => page.id === pageId);
            const to = original.pages.findIndex((page) => page.id === targetId);
            if (from < 0 || to < 0 || from === to) return;
            const profile = structuredClone(Alpine.raw(original)) as Profile;
            const [page] = profile.pages.splice(from, 1);
            profile.pages.splice(to, 0, page);
            await app().edit([{ kind: 'save_profile', profile }], 'Reorder pages', revision);
        },
        // Check whether drag page is valid for the current editor state
        canDragPage(page: Profile['pages'][number]) {
            return (
                !app().busy &&
                this.pageCount > 1 &&
                !isKeyboardNavigationPage(app().profile, page.id)
            );
        },
        // Start the page drag
        startPageDrag(event: DragEvent) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            const profile = app().profile;
            if (
                app().busy ||
                !id ||
                !profile ||
                isKeyboardNavigationPage(profile, id) ||
                this.pageCount < 2
            ) {
                event.preventDefault();
                return;
            }
            this.draggedPage = { id, profileId: profile.id, revision: app().configurationRevision };
            if (event.dataTransfer) {
                event.dataTransfer.effectAllowed = 'move';
                event.dataTransfer.setData('application/x-stream-dock-manager-page', id);
            }
        },
        // Mark a page drop target only for an active drag within the current profile
        dragPageOver(event: DragEvent) {
            if (app().busy || !this.draggedPage || this.draggedPage.profileId !== app().profile?.id)
                return;
            event.preventDefault();
            if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
            this.pageDropTarget = (event.currentTarget as HTMLElement).dataset.id || '';
        },
        // Clear the page drop target when the pointer leaves its entire control
        leavePageDrag(event: DragEvent) {
            const target = event.currentTarget as HTMLElement;
            if (!(event.relatedTarget instanceof Node) || !target.contains(event.relatedTarget))
                this.pageDropTarget = '';
        },
        // Drop the page
        dropPage(event: DragEvent) {
            const source = this.draggedPage;
            const target = (event.currentTarget as HTMLElement).dataset.id;
            this.resetPageDrag();
            if (!source || !target) return;
            event.preventDefault();
            swallow(this.reorderPage(source.id, target, source.profileId, source.revision));
        },
        // Reset the page drag
        resetPageDrag() {
            this.draggedPage = null;
            this.pageDropTarget = '';
        },
        // Observe page-strip geometry after mount so its scrollbar reflects overflow
        observePageStrip() {
            // Complete the deferred editor work using the current operation state
            this.$nextTick(() => {
                const strip = this.$refs.pageTabs as HTMLElement | undefined;
                if (!strip?.isConnected) return;
                // Complete the deferred editor work using the current operation state
                this.pageScrollbarObserver = new ResizeObserver(() => {
                    // Native scrollbar tracks vary by platform and system preference
                    const style = getComputedStyle(strip);
                    const borders =
                        parseFloat(style.borderTopWidth) + parseFloat(style.borderBottomWidth);
                    const size = `${Math.max(0, strip.offsetHeight - strip.clientHeight - borders)}px`;
                    if (strip.style.getPropertyValue('--page-scrollbar-size') !== size)
                        strip.style.setProperty('--page-scrollbar-size', size);
                    this.revealCurrentPage(app().page);
                });
                this.pageScrollbarObserver.observe(strip, { box: 'border-box' });
            });
        },
        // Reveal the current page
        revealCurrentPage(_page: unknown) {
            // Scroll only the strip; changing pages must not move the editor viewport
            this.$nextTick(() => {
                const strip = this.$refs.pageTabs as HTMLElement | undefined;
                const selected = strip?.querySelector<HTMLElement>('[aria-selected="true"]');
                if (!strip || !selected) return;
                const bounds = strip.getBoundingClientRect();
                const tab = selected.getBoundingClientRect();
                // Native clamping keeps the first and last pages within the strip's ends
                strip.scrollLeft += tab.left + tab.width / 2 - bounds.left - strip.clientWidth / 2;
            });
        },
        // Move page-tab focus and selection with supported navigation keys
        navigatePageTabs(event: KeyboardEvent) {
            if (!tabNavigationKey(event.key)) return;
            event.preventDefault();
            if (app().busy) return;
            const strip = event.currentTarget as HTMLElement;
            const tabs = [
                ...strip.querySelectorAll<HTMLButtonElement>('[role="tab"]:not(:disabled)'),
            ];
            const index = tabs.indexOf(event.target as HTMLButtonElement);
            if (index < 0) return;
            const next = nextTab(tabs, tabs[index], event.key);
            next?.focus({ preventScroll: true });
            next?.click();
        },
        // Select the page
        selectPage(event: Event) {
            const target = event.currentTarget as HTMLButtonElement | HTMLSelectElement;
            const pageId = target.dataset.id || target.value;
            if (!pageId || app().busy) return;
            const restoreFocus =
                target.getAttribute('role') === 'tab' && document.activeElement === target;
            const deviceId = app().deviceId;
            swallow(
                // Complete the deferred editor work using the current operation state
                windowWork.run('select page', async () => {
                    await app().flushDrafts?.();
                    app().selectedControls = [];
                    app().apply(
                        await app().command('select_page', {
                            deviceId,
                            pageId,
                        }),
                    );
                    // The command briefly disables tabs. Restore keyboard focus after they enable
                    if (restoreFocus)
                        // Complete the deferred editor work using the current operation state
                        this.$nextTick(() => {
                            if (
                                target.isConnected &&
                                deviceId === app().deviceId &&
                                app().page?.id === pageId
                            )
                                target.focus({ preventScroll: true });
                        });
                }),
            );
        },
    }));
    Alpine.data('preview', () => ({
        geometry: null as ReturnType<typeof previewGeometry> | null,
        measureGeometry() {
            const device = app().device;
            return previewGeometry(
                device?.manifest.controls || [],
                device?.manifest.layout.width || 800,
                device?.model_id,
                device?.manifest.capabilities.touch_panel,
            );
        },
        updateGeometry() {
            this.geometry = this.measureGeometry();
        },
        get layoutGeometry() {
            return this.geometry || this.measureGeometry();
        },
        // Check whether selected is valid for the current editor state
        isSelected(control: Control) {
            return app().selectedIds.includes(control.id);
        },
        // Check whether the selected control already has pending binding work
        pending(control: Control) {
            return app().controlPending(control.id);
        },
        dragSource: '',
        dragTarget: '',
        dragCopy: false,
        // Derive drag message from the current editor state
        get dragMessage() {
            if (!this.dragSource) return '';
            if (!this.dragTarget)
                return 'Drop on a compatible control to move or swap. Hold Ctrl/Cmd/Alt to copy.';
            const error = app().transferError(this.dragSource, this.dragTarget, this.dragCopy);
            if (error) return error;
            const label =
                app().device?.manifest.controls.find((control) => control.id === this.dragTarget)
                    ?.label || this.dragTarget;
            return this.dragCopy
                ? `Copy to ${label}`
                : hasBindingContent(app().page?.bindings[this.dragTarget])
                  ? `Swap with ${label}`
                  : `Move to ${label}`;
        },
        // Check whether drag is valid for the current editor state
        canDrag(control: Control) {
            return (
                !app().busy &&
                !app().controlPending(control.id) &&
                !app().isProtectedControl(control.id) &&
                hasBindingContent(app().page?.bindings[control.id])
            );
        },
        // Derive drag, drop-permission and gesture-strip classes for a control
        dragClass(control: Control) {
            return {
                'gesture-strip': control.preview_hidden,
                'shared-touch-region': this.isSharedTouch(control),
                dragging: this.dragSource === control.id,
                'drop-allowed':
                    this.dragTarget === control.id &&
                    !app().transferError(this.dragSource, control.id, this.dragCopy),
                'drop-blocked':
                    this.dragTarget === control.id &&
                    !!app().transferError(this.dragSource, control.id, this.dragCopy),
            };
        },
        // Combine control kind, navigation defaults and current preview state into CSS classes
        controlClass(control: Control) {
            return {
                [control.kind]: true,
                'navigation-default':
                    app().isProtectedControl(control.id) &&
                    !app().previewBinding(control.id)?.appearance &&
                    !app().previewBinding(control.id)?.composition,
                'navigation-palette':
                    app().isProtectedControl(control.id) &&
                    this.showFallback(control) &&
                    app().previewBinding(control.id)?.background_source === 'palette' &&
                    !app().previewBinding(control.id)?.asset &&
                    !app().previewBinding(control.id)?.composition,
                'has-screen-background': this.hasScreenBackground(control),
                'plain-button': this.isPlainButton(control),
                'round-button': this.isPlainButton(control) && !control.directional_switch,
                'strip-region': this.displayPanels.some((panel) =>
                    panel.controlIds.includes(control.id),
                ),
                selected: app().selectedIds.includes(control.id),
                unbound: !hasBindingContent(app().previewBinding(control.id)),
                'transparent-background': transparentBackground(
                    app().previewBinding(control.id)?.background_source === 'palette'
                        ? ''
                        : app().previewBinding(control.id)?.color || '',
                ),
            };
        },
        // Check whether plain button is valid for the current editor state
        isPlainButton(control: Control) {
            return control.kind === 'button' && !control.image_width;
        },
        // Check whether surface is valid for the current editor state
        hasSurface(control: Control) {
            // Native streamed pixels include the authoritative per-display composition.
            // Browser-only empty placeholders may reveal the shared wallpaper fallback.
            if (
                !app().nativeRendering &&
                this.hasScreenBackground(control) &&
                !hasBindingContent(app().previewBinding(control.id))
            )
                return false;
            return Boolean(control.image_width && app().previewSurface(control.id));
        },
        // Reset the drag
        resetDrag() {
            this.dragSource = '';
            this.dragTarget = '';
            this.dragCopy = false;
        },
        // Clear the control
        clearControl(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            if (id) this.$dispatch('request-control-clear', { controlId: id });
        },
        // Start dragging only a valid movable control and record its transfer identity
        dragStart(event: DragEvent) {
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            const control = app().device?.manifest.controls.find((control) => control.id === id);
            if (!control || !this.canDrag(control)) {
                event.preventDefault();
                return;
            }
            this.dragSource = id;
            if (event.dataTransfer) event.dataTransfer.effectAllowed = 'copyMove';
            event.dataTransfer?.setData(
                'application/x-stream-dock-manager-binding',
                JSON.stringify({
                    source: this.dragSource,
                    device: app().deviceId,
                    profile: app().profile?.id,
                    page: app().page?.id,
                    revision: app().configurationRevision,
                }),
            );
        },
        // Update the drop target and copy mode while reporting transfer compatibility
        dragOver(event: DragEvent) {
            if (!this.dragSource) return;
            event.preventDefault();
            this.dragTarget = (event.currentTarget as HTMLElement).dataset.id || '';
            this.dragCopy = event.ctrlKey || event.metaKey || event.altKey;
            if (event.dataTransfer)
                event.dataTransfer.dropEffect = app().transferError(
                    this.dragSource,
                    this.dragTarget,
                    this.dragCopy,
                )
                    ? 'none'
                    : this.dragCopy
                      ? 'copy'
                      : 'move';
        },
        // Clear the control drop target only after leaving its descendant tree
        dragLeave(event: DragEvent) {
            if (
                event.relatedTarget instanceof Node &&
                (event.currentTarget as HTMLElement).contains(event.relatedTarget)
            )
                return;
            this.dragTarget = '';
        },
        // Drop the editor operation
        drop(event: DragEvent) {
            event.preventDefault();
            const target = (event.currentTarget as HTMLElement).dataset.id || '';
            let origin: {
                source: string;
                device: string;
                profile: string;
                page: string;
                revision: number;
            };
            try {
                origin = JSON.parse(
                    event.dataTransfer?.getData('application/x-stream-dock-manager-binding') || '',
                );
            } catch {
                return;
            }
            if (
                origin.device !== app().deviceId ||
                origin.profile !== app().profile?.id ||
                origin.page !== app().page?.id ||
                origin.revision !== app().configurationRevision
            ) {
                app().notify('The source page changed during the drag. Try again.', 'warning');
                this.resetDrag();
                return;
            }
            this.dragSource = origin.source;
            swallow(
                app().transferBinding(
                    this.dragSource,
                    target,
                    event.ctrlKey || event.metaKey || event.altKey,
                ),
            );
            this.resetDrag();
        },
        // Derive app from the current editor state
        get app() {
            return app();
        },
        // Derive controls from the current editor state
        get controls() {
            return (app().device?.manifest.controls || []).filter(
                (control) => !control.semantics.includes('swipe'),
            );
        },
        controlKey(control: Control) {
            return app().device?.model_id + ':' + control.id;
        },
        // Derive selected dials from the current editor state
        get selectedDials() {
            return this.controls.filter(
                (control) => control.kind === 'encoder' && this.isSelected(control),
            );
        },
        // Check whether run is valid for the current editor state
        canRun(control: Control) {
            if (control.kind === 'display') return false;
            const binding = app().page?.bindings[control.id];
            return Boolean(
                app().device &&
                !app().busy &&
                !app().controlPending(control.id) &&
                !app().effectiveDeviceOutput.sleeping &&
                binding &&
                !binding.disabled,
            );
        },
        // Invoke the selected control with its optional rotation or swipe metadata
        runControl(event: Event) {
            const target = event.currentTarget as HTMLElement;
            const id = target.dataset.id;
            if (id)
                swallow(
                    app().triggerControl(
                        id,
                        target.dataset.rotation ? Number(target.dataset.rotation) : undefined,
                        target.dataset.swipe,
                    ),
                );
        },
        // Derive panel style from the current editor state
        get panelStyle() {
            const layout = app().device?.manifest.layout;
            const width = layout?.width || 800;
            const height = layout?.height || 580;
            // Contain every model in the same 800:580 frame. Matching widths alone
            // makes portrait devices disproportionately tall; controls retain their
            // descriptor-relative coordinates inside the scaled body
            const fittedWidth = Math.min(1, (width / height) * (580 / 800));
            const fittedHeight = Math.min(1, (height / width) * (800 / 580));
            // Object bindings use CSSOM properties. Style strings use setAttribute,
            // which the native style-hash CSP blocks in WebKitGTK
            return { width: `${100 * fittedWidth}%`, height: `${100 * fittedHeight}%` };
        },
        // Derive row offsets from the current editor state
        get rowOffsets() {
            return this.layoutGeometry.rowOffsets;
        },
        get touchPanels() {
            return this.layoutGeometry.touchPanels;
        },
        isSharedTouch(control: Control) {
            return this.layoutGeometry.rects.has(control.id);
        },
        touchPanelStyle(panel: Control) {
            const member = this.controls.find((control) => this.isSharedTouch(control));
            const layout = app().device!.manifest.layout;
            return {
                left: `${(100 * (panel.x + (member ? this.rowOffsets.get(member.id) || 0 : 0))) / layout.width}%`,
                top: `${(100 * panel.y) / layout.height}%`,
                width: `${(100 * panel.width) / layout.width}%`,
                height: `${(100 * panel.height) / layout.height}%`,
                '--device-brightness': previewBrightness(
                    app().effectiveDeviceOutput.brightness,
                    app().effectiveDeviceOutput.sleeping,
                    app().simulated,
                ),
            };
        },
        // Derive display panels from the current editor state
        get displayPanels() {
            return this.layoutGeometry.panels;
        },
        // Convert display-panel geometry to percentages of the device layout
        displayPanelStyle(panel: DisplayPanel) {
            const layout = app().device!.manifest.layout;
            return {
                left: `${(100 * panel.x) / layout.width}%`,
                top: `${(100 * panel.y) / layout.height}%`,
                width: `${(100 * panel.width) / layout.width}%`,
                height: `${(100 * panel.height) / layout.height}%`,
            };
        },
        // Project the control's declared position and dimensions into a rectangle
        controlRect(control: Control) {
            const shared = this.layoutGeometry.rects.get(control.id);
            if (shared) return shared;
            return { x: control.x, y: control.y, width: control.width, height: control.height };
        },
        // Check whether screen background is valid for the current editor state
        hasScreenBackground(control: Control) {
            return Boolean(this.screenBackground && isWallpaperAperture(control));
        },
        // Derive screen background from the current editor state
        get screenBackground() {
            const theme = Alpine.store('theme') as ReturnType<
                typeof import('./theme').registerTheme
            >;
            const appearance = theme.deviceAppearance;
            const mode =
                appearance.preference === 'system'
                    ? theme.systemDark
                        ? 'dark'
                        : 'light'
                    : appearance.preference;
            const id =
                mode === 'light' ? appearance.style.lightPalette : appearance.style.darkPalette;
            const variants = devicePalettes.palettes[mode] as Record<string, { wallpaper: string }>;
            return devicePreviewBackground({
                native,
                customPage: Boolean(app().profile?.background_asset),
                preview: Boolean(theme.palettePreview),
                paletteAsset: Boolean(
                    theme.palettePreview?.draft?.wallpaper ||
                    theme.customPalettes.find((p) => p.id === id && p.mode === mode)?.wallpaper,
                ),
                streamed: app().background,
                palette:
                    'data:image/svg+xml,' +
                    encodeURIComponent((variants[id] || variants.default).wallpaper),
            });
        },
        // Crop the shared device-wide background into the current screen aperture
        screenBackgroundStyle(control: Control) {
            const manifest = app().device!.manifest;
            // Separate LCDs have no device-wide framebuffer: fit the complete wallpaper
            // to each aperture, matching native per-display raster composition.
            if (!manifest.capabilities.background)
                return { width: '100%', height: '100%', left: '0%', top: '0%' };
            const [width, height] = manifest.capabilities.background_size || [
                manifest.layout.width,
                manifest.layout.height,
            ];
            // Each aperture crops the same device-wide image, preserving alignment
            // across the physical gaps and when the responsive preview resizes
            return {
                width: `${(100 * width) / control.width}%`,
                height: `${(100 * height) / control.height}%`,
                left: `${(-100 * control.x) / control.width}%`,
                top: `${(-100 * control.y) / control.height}%`,
            };
        },
        // Build control geometry and appearance styles from the preview binding and device output
        style(control: Control) {
            const layout = app().device!.manifest.layout;
            const binding = app().previewBinding(control.id);
            const output = app().deviceOutput;
            const index = app().device!.manifest.capabilities.lighting_channels.indexOf(control.id);
            const rgb = output.lighting_colors[index] || [0, 0, 0];
            const glow = rgb.map((v) => Math.round((v * output.lighting_brightness) / 100));
            const rect = this.controlRect(control);
            const resolved = hasBindingContent(binding)
                ? resolvePaletteBinding(binding!)
                : undefined;
            const color = resolved?.color || '#00000000';
            const ink = resolved?.appearance?.foreground || '#ffffff';
            return {
                left: `${(100 * (rect.x + (this.rowOffsets.get(control.id) || 0))) / layout.width}%`,
                top: `${(100 * rect.y) / layout.height}%`,
                width: `${(100 * rect.width) / layout.width}%`,
                height: `${(100 * rect.height) / layout.height}%`,
                '--key-color': color,
                '--key-ink': ink,
                '--device-brightness': previewBrightness(
                    app().effectiveDeviceOutput.brightness,
                    app().effectiveDeviceOutput.sleeping,
                    app().simulated,
                ),
                '--dial-color': `rgb(${glow.join(',')})`,
            };
        },
        // Trigger the tooltip
        triggerTooltip(control: Control) {
            const descriptions = app().triggerDescriptions[control.id] || {};
            const labels: Record<string, string> = {
                rotate: 'Rotate',
                push: 'Press',
                touch: 'Touch',
                longTouch: 'Long touch',
            };
            return Object.entries(descriptions)
                .filter(([, value]) => value)
                .map(([key, value]) => `${labels[key] || key}: ${value}`)
                .join('\n');
        },
        // Prefer the preview title and otherwise use a compact plain-button label
        title(control: Control) {
            return (
                app().previewBinding(control.id)?.title ||
                (this.isPlainButton(control) ? control.label.replace(/^Button (\d+)$/i, '$1') : '')
            );
        },
        // Show the icon
        showIcon(control: Control) {
            return (
                !app().isProtectedControl(control.id) &&
                !this.isPlainButton(control) &&
                this.showFallback(control)
            );
        },
        // Show the fallback
        showFallback(control: Control) {
            return (
                control.kind !== 'encoder' &&
                (this.isPlainButton(control) || !app().nativeRendering) &&
                (!this.hasSurface(control) || !hasBindingContent(app().previewBinding(control.id)))
            );
        },
        // Choose a fallback icon from the control's declared action type
        icon(control: Control) {
            const action = app().previewBinding(control.id)?.action;
            if (action?.kind === 'builtin') {
                const descriptor = builtinCatalog.find((d) => d.id === actionKey(action));
                return (
                    (
                        {
                            keyboard: '⌨',
                            monitor: '▣',
                            globe: '↗',
                            folder: '▱',
                            terminal: '›_',
                            clock: '◷',
                            settings: '⚙',
                        } as Record<string, string>
                    )[descriptor?.icon || ''] || '◇'
                );
            }
            return icons[action?.kind || 'none'] || '◇';
        },
        // Select the control
        selectControl(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id || '';
            const mouse = event as MouseEvent;
            if (mouse.ctrlKey || mouse.metaKey || mouse.shiftKey) {
                const ids = new Set(app().selectedIds);
                if (ids.has(id)) ids.delete(id);
                else ids.add(id);
                app().selectedControls = [...ids];
            } else app().selectedControls = [];
            app().controlId = id;
        },
    }));
    Alpine.data('inspector', () => ({
        pluginSettingDrafts: alternatives,
        appearance: defaultAppearance(),
        appearanceEnabled: false,
        composition: undefined as Binding['composition'],
        pluginAppearance: undefined as PluginAppearance | undefined,
        feedback: 'off',
        disabled: false,
        dialCurve: '' as '' | 'linear' | 'gentle' | 'fast',
        dialMaxSteps: 100,
        // Derive dial response from the current editor state
        get dialResponse(): Binding['dial_response'] {
            return this.dialCurve
                ? { curve: this.dialCurve, max_steps: Number(this.dialMaxSteps) }
                : undefined;
        },
        activationPolicy: 'queue' as NonNullable<Binding['activation_policy']>,
        // Derive has display from the current editor state
        get hasDisplay() {
            const control = app().control;
            return Boolean(
                control?.image_width || (control?.kind === 'encoder' && control.feedback_surface),
            );
        },
        // Read the action-tree projection used for the appearance preview
        appearanceAction(): Action {
            return this.actionTree.preview();
        },
        // Open the appearance
        openAppearance() {
            window.dispatchEvent(
                new CustomEvent('open-appearance', {
                    detail: {
                        context: this.contextKey,
                        defaultAction: this.actionDraft.action,
                        binding: {
                            live_label: cloneJson(this.liveLabel),
                            title: this.title,
                            color: this.color,
                            custom_background: this.customBackground || undefined,
                            background_source:
                                this.backgroundSource === 'palette' ? 'palette' : undefined,
                            asset: this.asset,
                            media: this.media,
                            appearance: this.appearanceEnabled ? this.appearance : undefined,
                            feedback: this.feedback,
                            disabled: this.disabled,
                            activation_policy: this.activationPolicy,
                            dial_response: this.dialResponse
                                ? cloneJson(this.dialResponse)
                                : undefined,
                            action: this.appearanceAction(),
                            composition: this.composition ? cloneJson(this.composition) : undefined,
                            plugin_appearance: this.pluginAppearance,
                        },
                    },
                }),
            );
        },
        // Detach deleted media from the current draft and its saved comparison state
        mediaDeleted(event: CustomEvent<{ asset: string }>) {
            const { asset } = event.detail;
            if (this.asset === asset) {
                this.asset = undefined;
                this.media = defaultMedia();
            }
            if (this.draftBase?.asset === asset) {
                this.draftBase = cloneJson(this.draftBase);
                delete this.draftBase!.asset;
                this.draftBase!.media = defaultMedia();
            }
            for (const draft of pendingDraftWrites.values()) detachDraftMedia(draft, asset);
            if (this.dirty) swallow(this.persistDraft());
        },
        // Apply the appearance
        applyAppearance(event: CustomEvent<Record<string, unknown>>) {
            if (event.detail.context !== this.contextKey) return;
            for (const key of [
                'title',
                'liveLabel',
                'color',
                'customBackground',
                'backgroundSource',
                'asset',
                'media',
                'appearance',
                'appearanceEnabled',
                'pluginAppearance',
                'composition',
                'feedback',
            ])
                (this as unknown as Record<string, unknown>)[key] = event.detail[key];
            this.markDirty();
        },
        title: '',
        liveLabel: [] as NonNullable<Binding['live_label']>,
        color: DEFAULT_BACKGROUND,
        customBackground: false,
        backgroundSource: 'custom' as 'custom' | 'palette',
        // Change the background source
        changeBackgroundSource(event: Event) {
            this.backgroundSource = (event.currentTarget as HTMLSelectElement).value as
                'custom' | 'palette';
            this.markDirty();
        },
        // Derive background color from the current editor state
        get backgroundColor() {
            return backgroundRgb(this.color);
        },
        // Derive transparent background from the current editor state
        get transparentBackground() {
            return showsTransparentBackground(this, this.usesDefaultPluginBackground);
        },
        // Derive uses default plugin background from the current editor state
        get usesDefaultPluginBackground() {
            return usesPluginBackground(this, this.kind, this.asset);
        },
        // Reset the background draft to default color and media settings and mark it dirty
        useDefaultBackground() {
            this.color = DEFAULT_BACKGROUND;
            this.backgroundSource = 'custom';
            this.customBackground = false;
            this.assetGeneration++;
            this.asset = undefined;
            this.media = defaultMedia();
            this.markDirty();
        },
        // Change the background color
        changeBackgroundColor(event: Event) {
            Object.assign(
                this,
                changeBackgroundRgb(
                    this,
                    (event.currentTarget as HTMLInputElement).value,
                    this.usesDefaultPluginBackground,
                ),
            );
            this.markDirty();
        },
        // Change the background transparency
        changeBackgroundTransparency(event: Event) {
            Object.assign(
                this,
                changeBackgroundAlpha(this, (event.currentTarget as HTMLInputElement).checked),
            );
            this.markDirty();
        },
        actionTree: new ActionDraftTree(),
        eventConversion: null as {
            source: string;
            context: string;
            value: Action;
            toEvents: boolean;
            summary: string;
            details?: string;
        } | null,
        eventTestGeneration: 0,
        eventTestPending: false,
        eventTestChoice: null as number | null,
        eventTestToken: '',
        eventTestTimer: null as ReturnType<typeof setTimeout> | null,
        eventTestMessage: '',
        get eventTestRows() {
            return this.eventRows.filter((row) => !row.reason);
        },
        get eventTestRow() {
            const rows = this.eventTestRows;
            return (
                rows.find((row) => row.index === this.eventTestChoice) ||
                rows.find((row) => row.selected) ||
                rows[0]
            );
        },
        get eventTestValue() {
            return this.eventTestRow?.index ?? '';
        },
        get canTestSelectedEvent() {
            const device = app().device;
            return (
                native &&
                !!device?.connected &&
                !device.simulated &&
                !!this.eventTestRow &&
                !app().navigationPending
            );
        },
        get eventTestHint() {
            const device = app().device;
            return !native || !device?.connected || device.simulated
                ? 'Connect a physical device in the desktop app to test input.'
                : 'Test physical input without running its action.';
        },
        resetEventTest(message = '') {
            this.eventTestGeneration++;
            this.eventTestPending = false;
            this.eventTestMessage = message;
            if (this.eventTestTimer !== null) clearTimeout(this.eventTestTimer);
            this.eventTestTimer = null;
            const token = this.eventTestToken;
            this.eventTestToken = '';
            if (token)
                void app()
                    .bridge.command('cancel_control_event_test', { token })
                    .catch(() => {});
        },
        changeEventTest(event: Event) {
            this.resetEventTest();
            this.eventTestChoice = Number((event.currentTarget as HTMLSelectElement).value);
        },
        escapeEventTest(event: KeyboardEvent) {
            if (!this.eventTestPending) return;
            event.preventDefault();
            this.resetEventTest('Test cancelled.');
        },
        async testSelectedEvent() {
            if (this.eventTestPending) {
                this.resetEventTest('Test cancelled.');
                return;
            }
            const selected = this.eventTestRow;
            if (!this.canTestSelectedEvent || !selected) return;
            const generation = ++this.eventTestGeneration;
            const context = this.contextKey;
            const current = () =>
                generation === this.eventTestGeneration && context === this.contextKey;
            const rows = this.eventTestRows;
            this.eventTestPending = true;
            this.eventTestMessage = 'Starting test…';
            const finish = (message: string) => {
                this.eventTestPending = false;
                this.eventTestToken = '';
                this.eventTestTimer = null;
                this.eventTestMessage = message;
            };
            try {
                const result = await app().bridge.command('arm_control_event_test', {
                    deviceId: app().deviceId,
                    controlId: app().controlId,
                    eventIndex: selected.index,
                    assigned: rows.filter((row) => row.assigned).map((row) => row.index),
                    multiPressMs: this.actionTree.multiPressMs,
                    holdMs: this.actionTree.holdMs,
                });
                const token = result.token;
                if (!current()) {
                    void app()
                        .bridge.command('cancel_control_event_test', { token })
                        .catch(() => {});
                    return;
                }
                this.eventTestToken = token;
                const consume = (result: typeof resultInitial) => {
                    if (!current()) return;
                    if (result.status === 'waiting') {
                        this.eventTestMessage = 'Waiting for ' + selected.name + '…';
                        this.eventTestTimer = setTimeout(() => {
                            this.eventTestTimer = null;
                            void app()
                                .bridge.command('read_control_event_test', { token })
                                .then(consume)
                                .catch((error) => {
                                    if (current())
                                        this.resetEventTest('Test unavailable: ' + String(error));
                                });
                        }, 175);
                    } else if (result.status === 'matched') {
                        finish('Matched: ' + selected.name + '.');
                    } else if (result.status === 'cancelled') {
                        finish('Test cancelled.');
                    } else {
                        const recognized = result.recognized
                            .map((index) => rows.find((row) => row.index === index)?.name)
                            .filter(Boolean);
                        finish(
                            recognized.length
                                ? 'Did not match. Detected: ' +
                                      [...new Set(recognized)].join(', ') +
                                      '.'
                                : 'No match: ' + selected.name + '.',
                        );
                    }
                };
                const resultInitial = result;
                consume(result);
            } catch (error) {
                if (current())
                    this.resetEventTest(
                        /command not found/i.test(String(error))
                            ? 'Rebuild and reopen Manager to enable event testing.'
                            : 'Test unavailable: ' + String(error),
                    );
            }
        },
        get swipeStrip() {
            return isSwipeStrip(app().device?.manifest, app().control);
        },
        inspectorDraft(draft: BindingDraft): BindingDraft {
            const manifest = app().device?.manifest;
            const control = app().control;
            if (!isSwipeStrip(manifest, control)) return draft;
            const supported = controlEventRows(manifest, control).filter((row) => !row.reason);
            const tree = draft.actionTree.eventLanes
                ? draft.actionTree
                : new ActionDraftTree(swipeStripEvents(draft.actionTree.commit(), supported));
            if (!supported.some((row) => row.index === tree.eventLane)) {
                const initial =
                    supported.find((row) => tree.eventLanes?.[row.index]?.action.kind !== 'none') ||
                    supported[0];
                if (initial) tree.eventLane = initial.index;
            }
            return { ...draft, actionTree: tree };
        },
        get eventActions() {
            return this.actionTree.eventLanes;
        },
        get eventRows() {
            return controlEventRows(app().device?.manifest, app().control).map((row) => {
                const action = this.eventActions?.[row.index]?.action;
                return {
                    ...row,
                    assigned: !!action && action.kind !== 'none',
                    summary: this.actionSummary(action),
                    selected: this.actionTree.eventLane === row.index,
                };
            });
        },
        get visibleEventRows() {
            return this.eventRows.filter((row) => !row.reason || row.assigned);
        },
        get eventGroups() {
            const rows = this.visibleEventRows;
            return ['Press & release', 'Gestures', 'Rotation']
                .map((name) => ({ name, rows: rows.filter((row) => row.group === name) }))
                .filter((group) => group.rows.length > 0);
        },
        get canEnableEvents() {
            return (
                !this.swipeStrip && !this.eventActions && this.eventRows.some((row) => !row.reason)
            );
        },
        get canEditEvent() {
            return !this.eventActions || this.eventRows.some((row) => row.selected && !row.reason);
        },
        get activeEventName() {
            return this.eventRows.find((row) => row.selected)?.name || 'Event action';
        },
        get eventTurns() {
            return this.eventActions && [6, 7, 8, 9].includes(this.actionTree.eventLane);
        },
        get eventDelayWarning() {
            const active = (index: number) =>
                this.eventRows.some((row) => row.index === index && row.assigned && !row.reason);
            const waits = (active(2) && (active(3) || active(4))) || (active(3) && active(4));
            return waits
                ? 'A shorter press or tap waits while a longer gesture is still possible.'
                : '';
        },
        get eventEdgeWarning() {
            const active = (index: number) =>
                this.eventRows.some((row) => row.index === index && row.assigned && !row.reason);
            return (active(0) || active(1)) && [2, 3, 4, 5].some(active)
                ? 'Down and up actions can run in addition to gesture actions.'
                : '';
        },
        get eventShowsGap() {
            return this.eventRows.some(
                (row) => [3, 4].includes(row.index) && row.assigned && !row.reason,
            );
        },
        get eventShowsHold() {
            return this.eventRows.some((row) => row.index === 5 && row.assigned && !row.reason);
        },
        setEventGap(event: Event) {
            const value = Number((event.currentTarget as HTMLInputElement).value);
            if (!eventTiming(value, multiPressBounds)) return;
            this.actionTree.multiPressMs = value;
            this.markDirty();
        },
        setEventHold(event: Event) {
            const value = Number((event.currentTarget as HTMLInputElement).value);
            if (!eventTiming(value, holdBounds)) return;
            this.actionTree.holdMs = value;
            this.markDirty();
        },
        previewEventsConversion() {
            if (!this.canEnableEvents || !this.rememberPluginSettings()) return;
            try {
                const legacy = this.actionTree.commit();
                let needsRelease = false;
                walkAction(legacy, (action) => {
                    if (builtinCatalog.find((item) => item.id === actionKey(action))?.release)
                        needsRelease = true;
                });
                if (needsRelease)
                    throw Error(
                        'This action needs the original physical press and release. Keep its existing binding.',
                    );
                const value = convertControlEvents(legacy, this.eventRows);
                const assigned = this.eventRows
                    .filter((row) => value.actions[row.index]?.kind !== 'none')
                    .map((row) => row.name + (row.reason ? ' (unavailable)' : ''));
                this.eventConversion = {
                    source: JSON.stringify(legacy),
                    context: this.contextKey,
                    value,
                    toEvents: true,
                    summary: 'Keep: ' + assigned.join(', ') + '.',
                };
                if (!assigned.length) this.confirmEventsConversion();
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        previewStandardConversion() {
            if (this.swipeStrip || !this.eventActions || !this.rememberPluginSettings()) return;
            try {
                const current = this.actionTree.commit();
                if (current.kind !== 'events') return;
                const { action, kept, removed, sharedSwipe } = standardControlAction(
                    current,
                    this.eventRows,
                    app().controlKind === 'encoder',
                    this.actionTree.eventLane,
                );
                this.eventConversion = {
                    source: JSON.stringify(current),
                    context: this.contextKey,
                    value: action,
                    toEvents: false,
                    summary: sharedSwipe
                        ? 'The selected swipe action will apply to every supported swipe.'
                        : kept.length
                          ? 'Keep: ' + kept.join(', ') + '.'
                          : 'Start with no action assigned.',
                    details: removed.length ? 'Removed on Save: ' + removed.join(', ') + '.' : '',
                };
                if (!kept.length && !removed.length && !sharedSwipe) this.confirmEventsConversion();
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        confirmEventsConversion() {
            const plan = this.eventConversion;
            if (this.swipeStrip || !plan || plan.context !== this.contextKey) return;
            if (plan.toEvents ? !this.canEnableEvents : !this.eventActions) return;
            try {
                if (plan.source !== JSON.stringify(this.actionTree.commit())) {
                    if (plan.toEvents) this.previewEventsConversion();
                    else this.previewStandardConversion();
                    return;
                }
                this.actionTree = new ActionDraftTree(plan.value);
                this.eventConversion = null;
                this.ensureEditableEvent();
                this.updateModeSelection = undefined;
                this.markDirty();
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        cancelEventsConversion() {
            this.eventConversion = null;
        },
        ensureEditableEvent() {
            if (!this.eventActions || this.canEditEvent) return;
            const supported = this.eventRows.filter((row) => !row.reason);
            const selected = supported.find((row) => row.assigned) || supported[0];
            if (selected) this.actionTree.eventLane = selected.index;
        },
        editEventLane(event: Event) {
            const index = Number((event.currentTarget as HTMLElement).dataset.eventIndex);
            const row = this.eventRows.find((row) => row.index === index);
            if (!this.eventActions || !row || row.reason || !this.rememberPluginSettings()) return;
            this.actionTree.eventLane = index;
            this.resetEventTest();
            this.eventTestChoice = index;
            this.updateModeSelection = undefined;
            if (!row.assigned) this.browseActions(event);
        },
        clearEventLane(event: Event) {
            const index = Number((event.currentTarget as HTMLElement).dataset.eventIndex);
            if (
                !this.eventActions ||
                !this.eventRows.some((row) => row.index === index && row.assigned)
            )
                return;
            if (index !== this.actionTree.eventLane && !this.rememberPluginSettings()) return;
            this.eventActions[index] = new InspectorActionDraft();
            this.pluginSettingDrafts.clear(this.contextKey + ':event:' + index);
            this.markDirty();
        },
        // Derive dial actions from the current editor state
        get dialActions() {
            return this.actionTree.currentLanes;
        },
        // Derive dial action index from the current editor state
        get dialActionIndex() {
            return this.actionTree.lane;
        },
        // Select the active dial action lane in the action tree
        set dialActionIndex(value: number) {
            this.actionTree.lane = value;
        },
        // Derive linked dial turns from the current editor state
        get linkedDialTurns() {
            return this.actionTree.linkedTurns;
        },
        linkTurnsPending: false,
        // Toggle the linked turns
        toggleLinkedTurns() {
            if (!this.rememberPluginSettings()) return;
            try {
                if (
                    !this.linkedDialTurns &&
                    this.dialActions &&
                    this.dialActions[1]!.action.kind !== 'none' &&
                    JSON.stringify(this.dialActions[1]!.commit()) !==
                        JSON.stringify(oppositeTurnAction(this.dialActions[0]!.commit()))
                ) {
                    this.linkTurnsPending = true;
                    return;
                }
                this.actionTree.setLinkedTurns(!this.linkedDialTurns);
                this.markDirty();
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        // Confirm the linked turns
        confirmLinkedTurns() {
            try {
                this.actionTree.setLinkedTurns(true);
                this.linkTurnsPending = false;
                this.pluginSettingDrafts.clear(this.contextKey + ':dial:1');
                this.markDirty();
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        // Cancel the linked turns
        cancelLinkedTurns() {
            this.linkTurnsPending = false;
        },
        // Clear the dial lane
        clearDialLane(event: Event) {
            const index = Number((event.currentTarget as HTMLElement).dataset.lane);
            if (!this.dialActions || ![0, 1, 2].includes(index)) return;
            const clearsActive =
                index === this.dialActionIndex ||
                (this.linkedDialTurns && index === 0 && this.dialActionIndex === 1);
            if (!clearsActive && !this.rememberPluginSettings()) return;
            try {
                this.actionTree.clearLane(index);
                this.pluginSettingDrafts.clear(this.contextKey + ':dial:' + index);
                this.linkTurnsPending = false;
                this.markDirty();
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        // Derive dial canvas from the current editor state
        get dialCanvas() {
            return dialActionPresentation(this.actionDraft.action, app().snapshot.plugins);
        },
        // Derive separate dial actions from the current editor state
        get separateDialActions() {
            return Boolean(this.dialActions);
        },
        // Derive dial volume from the current editor state
        get dialVolume() {
            return (this.separateDialActions || this.eventTurns) && this.kind === 'volume';
        },
        // Derive dial volume direction from the current editor state
        get dialVolumeDirection() {
            return this.value.trim().startsWith('-') ? 'decrease' : 'increase';
        },
        // Set the dial volume direction
        setDialVolumeDirection(event: Event) {
            const sign = (event.currentTarget as HTMLSelectElement).value === 'decrease' ? -1 : 1;
            this.value = String(sign * (Math.abs(Number(this.value)) || 5));
            this.markDirty();
        },
        // Derive dial volume amount from the current editor state
        get dialVolumeAmount() {
            return this.value.replace(/^-/, '');
        },
        // Apply the selected volume direction to a nonnegative dial step magnitude
        set dialVolumeAmount(value: string) {
            const sign = this.dialVolumeDirection === 'decrease' ? -1 : 1;
            this.value =
                value.trim() === ''
                    ? sign < 0
                        ? '-'
                        : ''
                    : String(sign * Math.abs(Number(value)));
        },
        // Derive dial navigation from the current editor state
        get dialNavigation() {
            return (
                (this.separateDialActions || this.eventTurns) &&
                ['navigate', 'page', 'profile'].includes(this.kind)
            );
        },
        // Derive dial navigation mode from the current editor state
        get dialNavigationMode() {
            const action = this.actionDraft.action;
            return action.kind === 'navigate' ? action.direction : 'specific';
        },
        // Set the dial navigation mode
        setDialNavigationMode(event: Event) {
            const mode = (event.currentTarget as HTMLSelectElement).value;
            const action = this.actionDraft.action;
            const target =
                action.kind === 'navigate'
                    ? action.target
                    : action.kind === 'profile'
                      ? 'profile'
                      : 'page';
            this.actionDraft = new InspectorActionDraft(
                mode === 'specific'
                    ? target === 'page'
                        ? { kind: 'page', page_id: app().page?.id || '' }
                        : { kind: 'profile', profile_id: app().profile?.id || '' }
                    : {
                          kind: 'navigate',
                          target,
                          direction: mode === 'previous' ? 'previous' : 'next',
                      },
            );
            this.markDirty();
        },
        // Derive dial lane name from the current editor state
        get dialLaneName() {
            return this.linkedDialTurns && this.dialActionIndex !== 2
                ? 'Turn actions'
                : ['Turn left', 'Turn right', 'Press'][this.dialActionIndex];
        },
        // Derive dial lane left from the current editor state
        get dialLaneLeft() {
            return this.dialActionIndex === 0;
        },
        // Derive dial lane right from the current editor state
        get dialLaneRight() {
            return (
                this.dialActionIndex === 1 || (this.linkedDialTurns && this.dialActionIndex === 0)
            );
        },
        get supportsDialPress() {
            const semantics = app().control?.semantics;
            return !!(semantics?.includes('press') || semantics?.includes('pulse'));
        },
        // Derive dial lane press from the current editor state
        get dialLanePress() {
            return this.dialActionIndex === 2;
        },
        // Derive active action context from the current editor state
        get activeActionContext() {
            return (
                this.contextKey +
                (this.eventActions
                    ? ':event:' + this.actionTree.eventLane
                    : this.dialActions
                      ? ':dial:' + this.dialActionIndex
                      : '')
            );
        },
        // Read the saved action or the saved action in the currently edited dial lane
        savedAction(): Action | undefined {
            const action = this.draftBase?.action;
            if (this.eventActions)
                return action?.kind === 'events'
                    ? action.actions[this.actionTree.eventLane]
                    : undefined;
            return this.dialActions && action?.kind === 'dial'
                ? action.actions[this.dialActionIndex]
                : action;
        },
        // Clear the plugin alternatives
        clearPluginAlternatives(context: string) {
            this.pluginSettingDrafts.clear(context);
            for (let i = 0; i < 3; i++) {
                this.pluginSettingDrafts.clear(context + ':dial:' + i);
            }
            for (let i = 0; i < eventCount; i++)
                this.pluginSettingDrafts.clear(context + ':event:' + i);
        },
        // Select the dial lane
        selectDialLane(event: Event) {
            const index = Number((event.currentTarget as HTMLElement).dataset.lane);
            if (!this.dialActions || ![0, 1, 2].includes(index) || index === this.dialActionIndex)
                return;
            if (!this.rememberPluginSettings()) return;
            this.dialActionIndex = this.linkedDialTurns && index === 1 ? 0 : index;
            this.updateModeSelection = undefined;
        },
        // Summarize the edited, saved or linked opposite action for a dial lane
        dialActionSummary(index: number) {
            const action =
                this.linkedDialTurns && index === 1
                    ? oppositeTurnAction(this.dialActions![0]!.action)
                    : index === this.dialActionIndex
                      ? this.actionDraft.action
                      : this.dialActions?.[index]?.action;
            return this.actionSummary(action);
        },
        actionSummary(action?: Action) {
            if (!action || action.kind === 'none') return 'No action';
            if (action.kind === 'macro') return `Multi-action · ${action.actions.length} steps`;
            if (action.kind === 'builtin' && action.action.kind === 'saved_macro') {
                const id = action.action.macro_id;
                const saved = app().snapshot.saved_macros.find((macro) => macro.id === id);
                return saved?.name || 'Run saved macro';
            }
            if (action.kind === 'volume')
                return action.delta < 0 ? 'Decrease volume' : 'Increase volume';
            if (action.kind === 'navigate')
                return `${action.direction === 'previous' ? 'Previous' : 'Next'} ${action.target}`;
            if (action.kind === 'plugin')
                return (
                    app()
                        .snapshot.plugins.find((p) => p.id === action.plugin_id)
                        ?.actions.find((a) => a.id === action.action_id)?.name || 'Plugin action'
                );
            return (
                builtinCatalog.find((item) => item.id === actionKey(action))?.name || action.kind
            );
        },
        // Derive left dial summary from the current editor state
        get leftDialSummary() {
            return this.dialActionSummary(0);
        },
        // Derive right dial summary from the current editor state
        get rightDialSummary() {
            return this.dialActionSummary(1);
        },
        // Derive press dial summary from the current editor state
        get pressDialSummary() {
            return this.dialActionSummary(2);
        },
        dialMultiActionOriginal: null as {
            draft: InspectorActionDraft;
            context: string;
            dirty: boolean;
        } | null,
        // Edit the dial multi action
        editDialMultiAction() {
            try {
                const action = this.actionDraft.commit();
                if (this.dialMultiActionOriginal) return;
                this.dialMultiActionOriginal = {
                    draft: this.actionDraft,
                    context: this.activeActionContext,
                    dirty: this.dirty,
                };
                this.actionDraft = new InspectorActionDraft({
                    kind: 'macro',
                    actions:
                        action.kind === 'macro'
                            ? action.actions
                            : action.kind === 'none'
                              ? []
                              : [action],
                });
                (
                    this as unknown as {
                        $nextTick: (callback: () => void) => void;
                        $el: HTMLElement;
                    }
                )
                    // Complete the deferred editor work using the current operation state
                    .$nextTick(() => {
                        (this as unknown as { $el: HTMLElement }).$el
                            .closest('sd-inspector')
                            ?.querySelector('sd-macro-editor .macro-editor')
                            ?.dispatchEvent(new CustomEvent('open-multi-action'));
                    });
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        // Cancel the dial multi action
        cancelDialMultiAction() {
            const original = this.dialMultiActionOriginal;
            this.dialMultiActionOriginal = null;
            if (!original || original.context !== this.activeActionContext) return;
            this.actionDraft = original.draft;
            this.dirty = original.dirty;
        },
        // Derive action draft from the current editor state
        get actionDraft() {
            return this.actionTree.active;
        },
        // Replace the active action-tree node with the selected draft
        set actionDraft(value: ActionNode) {
            this.actionTree.active = value;
        },
        // Derive kind from the current editor state
        get kind() {
            return this.actionDraft.kind;
        },
        // Derive value from the current editor state
        get value() {
            return this.actionDraft.value;
        },
        // Return the current editor value
        set value(value: string) {
            this.actionDraft.value = value;
        },
        livePreviewOwner: 'inspector:' + crypto.randomUUID(),
        previewWork: editorPreviewWork(),
        // Refresh the live preview
        refreshLivePreview() {
            const context = this.contextKey;
            this.previewWork.schedule(() => {
                if (context !== this.contextKey || context !== this.address()) return;
                this.projectLivePreview();
            });
        },
        projectLivePreview() {
            if (!this.dirty || !this.contextKey || this.contextKey !== this.address()) {
                void clearLiveActionPreview(this.livePreviewOwner);
                return;
            }
            try {
                const preview = previewBindingDraft(this);
                // Visual authoring stays live, but an unsaved event map does not
                // become the effective binding for hardware or preview input.
                if (this.eventActions || this.draftBase?.action.kind === 'events')
                    preview.action = this.draftBase?.action || { kind: 'none' };
                setLiveActionPreview(this.livePreviewOwner, preview);
            } catch {
                // Keep the last valid draft visible while advanced JSON is incomplete
            }
        },
        dirty: false,
        closeBaseline: '',
        // Project a recovery draft without leaking a modal-only dial conversion
        draftValue() {
            const original = this.dialMultiActionOriginal;
            // A modal-only conversion must not leak into any of the recovery projections
            return bindingDraftValue(
                this,
                original?.context === this.activeActionContext ? original.draft : undefined,
            );
        },
        // Derive unsaved changes from the current editor state
        get unsavedChanges() {
            return this.dirty && bindingSignature(this.draftValue()) !== this.closeBaseline;
        },
        unregisterCloseOwner: undefined as (() => void) | undefined,
        savingContexts: {} as Record<string, boolean>,
        // Derive saving from the current editor state
        get saving() {
            return Boolean(this.savingContexts[this.contextKey]);
        },
        media: defaultMedia(),
        contextKey: '',
        loadedSignature: '',
        draftBase: null as Binding | null,
        recovered: false,
        conflict: false,
        draftTimer: undefined as ReturnType<typeof setTimeout> | undefined,
        releaseDraftTimer: undefined as (() => void) | undefined,
        // Derive draft recovery error from the current editor state
        get draftRecoveryError() {
            const draft = app().draftStore[this.contextKey];
            if (!draft) return '';
            try {
                ActionDraftTree.restore(draft.value as Record<string, unknown>);
                return '';
            } catch (error) {
                return String(error);
            }
        },
        // Derive has draft recovery error from the current editor state
        get hasDraftRecoveryError() {
            return Boolean(this.draftRecoveryError);
        },
        // Derive has draft from the current editor state
        get hasDraft() {
            return (
                Boolean(this.draftRecoveryError) ||
                (Boolean(app().draftStore[this.contextKey]) && !this.recovered && !this.dirty)
            );
        },
        // Derive structured value from the current editor state
        get structuredValue() {
            return JSON.stringify(this.actionDraft.action);
        },
        // Derive media value from the current editor state
        get mediaValue() {
            return JSON.stringify(this.media);
        },
        // Derive current binding json from the current editor state
        get currentBindingJSON() {
            return this.conflict ? JSON.stringify(app().binding, null, 2) : '';
        },
        // Update the structured
        updateStructured(event: CustomEvent<{ action: Action }>) {
            this.actionDraft = new InspectorActionDraft(event.detail.action);
            this.markDirty();
        },
        // Update the media
        updateMedia(event: CustomEvent<{ options: MediaOptions; asset?: string }>) {
            if (event.detail.asset) this.asset = event.detail.asset;
            this.media = event.detail.options;
            this.markDirty();
        },
        // Schedule the draft
        scheduleDraft() {
            if (windowCloseOwners.closing) return;
            if (!this.dirty) return;
            updateWindowDraftState(this.contextKey, this.unsavedChanges);
            if (windowWork.quiescing) {
                windowWork.invalidate();
                return;
            }
            const release = windowWork.begin('draft debounce');
            if (this.draftTimer) clearTimeout(this.draftTimer);
            this.releaseDraftTimer?.();
            this.releaseDraftTimer = release;
            this.draftTimer = setTimeout(() => swallow(this.persistDraft()), 250);
        },
        // Flush the draft timer and persist its recovery projection within the window-work fence
        persistDraft(fence?: WindowFence) {
            if (windowCloseOwners.closing) return draftQueue.value;
            // Complete the deferred editor work using the current operation state
            return ownDraftWork(fence, async () => {
                if (this.draftTimer) clearTimeout(this.draftTimer);
                this.draftTimer = undefined;
                this.releaseDraftTimer?.();
                this.releaseDraftTimer = undefined;
                if (!this.dirty || !this.contextKey) return draftQueue.value;
                const key = this.contextKey;
                const existing = app().draftStore[key];
                if (existing) ActionDraftTree.restore(existing.value as Record<string, unknown>);
                if (draftDevice(key) === app().removingSimulatorId) return draftQueue.value;
                const draft: Draft = JSON.parse(
                    JSON.stringify({
                        key,
                        base: this.draftBase,
                        value: this.draftValue(),
                    }),
                );
                app().draftStore[key] = draft;
                rememberWindowDraft(key, app().draftStore[key]!, this.unsavedChanges);
                return draftQueue.enqueue(key, draft, fence);
            });
        },
        // Restore the draft
        restoreDraft() {
            const draft = app().draftStore[this.contextKey];
            if (!draft) return;
            const value = draft.value as Record<string, unknown>;
            let restored: BindingDraft;
            try {
                restored = restoreBindingDraft(value, this);
            } catch (error) {
                app().notify(String(error), 'error');
                return;
            }
            this.conflict =
                bindingSignature(draft.base) !== bindingSignature(app().binding || null);
            Object.assign(this, this.inspectorDraft(restored));
            this.ensureEditableEvent();
            this.draftBase = draft.base;
            this.recovered = true;
            this.dirty = true;
        },
        // Cancel draft scheduling and clear the matching saved recovery draft
        async discardDraft() {
            this.resetEventTest();
            if (this.draftTimer) clearTimeout(this.draftTimer);
            this.draftTimer = undefined;
            this.releaseDraftTimer?.();
            this.releaseDraftTimer = undefined;
            const key = this.contextKey;
            // Retire queued preview work before resetting the draft. Cleanup is
            // captured now; its completion must never reset a newly selected target.
            this.previewWork.cancel();
            const previewReleased = clearLiveActionPreview(this.livePreviewOwner);
            const draftCleared = draftQueue.clear(key);
            delete app().draftStore[key];
            forgetWindowDraft(key);
            const mediaReleased = releaseMediaOwner(app(), 'control:' + key);
            this.dirty = false;
            this.recovered = false;
            this.clearPluginAlternatives(key);
            this.load();
            await Promise.all([previewReleased, draftCleared, mediaReleased]);
        },
        // Replace the conflict
        async replaceConflict() {
            this.draftBase = cloneJson(app().binding || null);
            this.conflict = false;
            await this.save();
        },
        asset: undefined as string | undefined,
        assetGeneration: 0,
        // Derive plugin id from the current editor state
        get pluginId() {
            return this.actionDraft.action.kind === 'plugin'
                ? this.actionDraft.action.plugin_id
                : '';
        },
        // Derive plugin action id from the current editor state
        get pluginActionId() {
            return this.actionDraft.action.kind === 'plugin'
                ? this.actionDraft.action.action_id
                : '';
        },
        // Derive user title enabled from the current editor state
        get userTitleEnabled() {
            return (
                app()
                    .snapshot.plugins.find((plugin) => plugin.id === this.pluginId)
                    ?.actions.find((action) => action.id === this.pluginActionId)
                    ?.user_title_enabled !== false
            );
        },
        // Derive plugin settings from the current editor state
        get pluginSettings(): import('../generated/runtime').JsonValue {
            return this.actionDraft.action.kind === 'plugin'
                ? this.actionDraft.action.settings
                : {};
        },
        // Replace settings only when the active draft is a plugin action
        set pluginSettings(settings: import('../generated/runtime').JsonValue) {
            if (this.actionDraft.action.kind === 'plugin')
                this.actionDraft.action.settings = settings;
        },
        updateUnit: 1000,
        updateModeSelection: undefined as UpdateModeSelection | undefined,
        // Derive refresh policy from the current editor state
        get refreshPolicy() {
            return this.kind === 'plugin'
                ? app().snapshot.plugins.find((p) => p.id === this.pluginId)?.capabilities?.actions[
                      this.pluginActionId
                  ]?.refresh
                : undefined;
        },
        // Derive update settings from the current editor state
        get updateSettings(): Record<string, import('../generated/runtime').JsonValue | undefined> {
            return this.pluginSettings &&
                typeof this.pluginSettings === 'object' &&
                !Array.isArray(this.pluginSettings)
                ? this.pluginSettings
                : {};
        },
        // Derive update interval from the current editor state
        get updateInterval(): number | undefined {
            return updateInterval(this.updateSettings);
        },
        // Derive requires explicit update from the current editor state
        get requiresExplicitUpdate() {
            // These current consumers require explicit permission for automatic reads
            return [
                'app.streamdockmanager.plugins.http-webhooks',
                'app.streamdockmanager.plugins.service-monitor',
            ].includes(this.pluginId);
        },
        // Derive update mode from the current editor state
        get updateMode() {
            return updateMode(
                this.updateSettings,
                this.refreshPolicy,
                this.requiresExplicitUpdate,
                this.updateModeSelection,
            );
        },
        // Derive update amount from the current editor state
        get updateAmount() {
            return this.updateInterval === undefined ? '' : this.updateInterval / this.updateUnit;
        },
        // Derive update default label from the current editor state
        get updateDefaultLabel() {
            const ms = this.refreshPolicy?.default_ms || 0;
            if (!ms) return 'Default — Off';
            const unit = ms % 3600000 === 0 ? 3600000 : ms % 60000 === 0 ? 60000 : 1000;
            const count = ms / unit;
            return `Default — Every ${count} ${unit === 3600000 ? 'hour' : unit === 60000 ? 'minute' : 'second'}${count === 1 ? '' : 's'}`;
        },
        // Derive update error from the current editor state
        get updateError() {
            const value = this.updateInterval,
                policy = this.refreshPolicy;
            if (policy && this.requiresExplicitUpdate && value === undefined)
                return 'Choose an update frequency before saving.';
            return policy &&
                value !== undefined &&
                value !== 0 &&
                (!Number.isSafeInteger(value) ||
                    value < policy.minimum_ms ||
                    value > policy.maximum_ms)
                ? `Choose an interval between ${policy.minimum_ms / 1000} and ${policy.maximum_ms / 1000} seconds.`
                : '';
        },
        // Set the update mode
        setUpdateMode(event: Event) {
            const mode = (event.currentTarget as HTMLSelectElement).value;
            if (!this.refreshPolicy || !['default', 'off', 'custom'].includes(mode)) return;
            const settings = chooseUpdateMode(this.updateSettings, this.refreshPolicy, mode);
            if (
                [
                    'app.streamdockmanager.plugins.http-webhooks',
                    'app.streamdockmanager.plugins.service-monitor',
                ].includes(this.pluginId)
            )
                delete settings.interval;
            this.pluginSettings = settings;
            this.updateModeSelection = { settings: this.updateSettings, mode };
            this.markDirty();
        },
        // Set the update amount
        setUpdateAmount(event: Event) {
            const input = event.currentTarget as HTMLInputElement;
            const settings = { ...this.updateSettings };
            if (
                [
                    'app.streamdockmanager.plugins.http-webhooks',
                    'app.streamdockmanager.plugins.service-monitor',
                ].includes(this.pluginId)
            )
                delete settings.interval;
            this.pluginSettings = {
                ...settings,
                update_interval_ms:
                    input.value === '' ? null : Number(input.value) * this.updateUnit,
            };
            this.updateModeSelection = { settings: this.updateSettings, mode: 'custom' };
            this.markDirty();
        },
        // Set the update unit
        setUpdateUnit(event: Event) {
            this.updateUnit = Number((event.currentTarget as HTMLSelectElement).value);
        },
        // Derive plugin action name from the current editor state
        get pluginActionName() {
            return (
                app()
                    .snapshot.plugins.find((p) => p.id === this.pluginId)
                    ?.actions.find((a) => a.id === this.pluginActionId)?.name || this.pluginActionId
            );
        },
        // Derive action choice from the current editor state
        get actionChoice() {
            if (this.kind === 'builtin') return actionKey(this.actionDraft.action);
            return this.kind;
        },
        // Derive action name from the current editor state
        get actionName() {
            if ((this.dialActions || this.eventActions) && this.kind === 'macro')
                return 'Multi-action';
            if (this.actionDraft.action.kind === 'navigate')
                return this.actionDraft.action.target === 'page'
                    ? 'Page navigation'
                    : 'Profile navigation';
            if (this.kind === 'plugin') return this.pluginActionName || 'Plugin action';
            if (this.kind === 'custom') return 'Advanced action JSON';
            return (
                builtinCatalog.find((item) => item.id === this.actionChoice)?.name || 'No action'
            );
        },
        // Browse the actions
        browseActions(event: Event, source: 'all' | 'plugins' = 'all') {
            if (!this.canEditEvent) return;
            const address = this.activeActionContext;
            openActionPicker({
                owner: event.currentTarget as HTMLElement,
                current: this.actionChoice,
                advanced: true,
                independentEvent: !!this.eventActions,
                source,
                label: this.eventActions ? this.activeEventName : 'Device editor',
                // Report whether the current editor value is valid
                valid: () => this.activeActionContext === address,
                // Choose the active item for the editor workflow
                choose: (selection) => {
                    if ('kind' in selection) this.selectAction(selection.kind);
                    else {
                        app().pendingActionTitle = this.title || selection.name;
                        app().pendingAction = selection.action;
                    }
                },
            });
        },
        // Choose the builtin
        chooseBuiltin(event: CustomEvent<{ kind: string }>) {
            if (builtinCatalog.some((item) => item.id === event.detail.kind))
                this.selectAction(event.detail.kind);
        },
        // Browse the plugins
        browsePlugins(event: Event) {
            this.browseActions(event, 'plugins');
        },
        // Select the action
        selectAction(kind: string) {
            if (!this.canEditEvent || !this.rememberPluginSettings()) return;
            if (kind === 'custom') {
                this.actionDraft.editJSON();
                this.markDirty();
                return;
            }
            {
                const action = dialActionDefault(
                    defaultAction(kind),
                    this.dialActions
                        ? this.dialActionIndex
                        : this.eventTurns
                          ? [6, 8].includes(this.actionTree.eventLane)
                              ? 0
                              : 1
                          : null,
                );
                this.actionDraft = new InspectorActionDraft(action);
                if (!this.draftBase && !this.eventActions) {
                    const defaults = actionAppearanceDefaults(action, app().snapshot.plugins);
                    this.feedback = defaults.feedback;
                    if (!this.title) this.title = defaults.title;
                    if (!this.appearanceEnabled && defaults.appearanceEnabled) {
                        this.appearance = defaults.appearance;
                        this.appearanceEnabled = true;
                    }
                }
                if (this.actionDraft.action.kind === 'profile')
                    this.actionDraft.action.profile_id = app().profile?.id || '';
                if (this.actionDraft.action.kind === 'page')
                    this.actionDraft.action.page_id = app().page?.id || '';
                this.markDirty();
            }
        },
        // Derive plugin action icon from the current editor state
        get pluginActionIcon() {
            return (
                app()
                    .snapshot.plugins.find((p) => p.id === this.pluginId)
                    ?.actions.find((a) => a.id === this.pluginActionId)?.icon || ''
            );
        },
        // Derive is plugin from the current editor state
        get isPlugin() {
            return (
                this.kind === 'plugin' &&
                Boolean(
                    app()
                        .snapshot.plugins.find((p) => p.id === this.pluginId)
                        ?.actions.find((a) => a.id === this.pluginActionId)?.has_settings,
                )
            );
        },
        // Open the inspector
        openInspector(event?: Event) {
            const plugin = app().snapshot.plugins.find((p) => p.id === this.pluginId);
            if (
                plugin?.actions.some(
                    (action) => action.id === this.pluginActionId && action.has_settings,
                )
            ) {
                const context = this.activeActionContext;
                // Reassigning the same plugin at the same address must retire this editor
                const draft = this.actionDraft;
                const pluginId = this.pluginId,
                    actionId = this.pluginActionId;
                void windowWork
                    // Complete the deferred editor work using the current operation state
                    .run('plugin settings editor', async () => {
                        const resourceDraft = {
                            ...(this.actionDraft.action.kind === 'plugin'
                                ? this.actionDraft.action.resources
                                : {}),
                        };
                        const value = await editPluginSettings(
                            pluginId,
                            actionId,
                            this.pluginSettings,
                            event?.currentTarget as HTMLElement | undefined,
                            false,
                            undefined,
                            resourceDraft,
                            {
                                profile_id: app().profile?.id ?? '',
                                page_id: app().page?.id ?? '',
                                control_id: app().controlId,
                                path: this.eventActions
                                    ? [this.actionTree.eventLane]
                                    : this.dialActions
                                      ? [this.dialActionIndex]
                                      : [],
                            },
                        );
                        if (
                            value &&
                            this.activeActionContext === context &&
                            this.actionDraft === draft &&
                            this.pluginId === pluginId &&
                            this.pluginActionId === actionId
                        ) {
                            this.pluginSettings = value;
                            if (this.actionDraft.action.kind === 'plugin')
                                this.actionDraft.action.resources = resourceDraft;
                            this.markDirty();
                        }
                    })
                    // Handle a failed editor operation without retaining stale work
                    .catch((error) => {
                        if (!(error instanceof WindowQuiescingError))
                            app().notify(String(error), 'error');
                    });
            }
        },
        // Browse the media
        browseMedia() {
            let generation = ++this.assetGeneration;
            const owner = this.mediaOwner;
            // Return the currently selected editor value
            const current = () => generation === this.assetGeneration && owner === this.mediaOwner;
            openMediaLibrary({
                owner,
                current,
                // Refresh the media generation only while the same owner and editor view remain active
                rebase: () => {
                    if (
                        owner === this.mediaOwner &&
                        this.$el.isConnected &&
                        app().route === 'editor'
                    )
                        generation = this.assetGeneration;
                },
                formats: app().device?.manifest.capabilities.media_formats.concat('svg') || [
                    'png',
                    'jpeg',
                    'gif',
                    'mp4',
                ],
                // Choose the active item for the editor workflow
                choose: (asset, options) => {
                    if (!current()) return;
                    this.asset = asset;
                    this.media = { ...defaultMedia(), ...options };
                    this.markDirty();
                },
            });
        },
        // Choose the asset
        async chooseAsset() {
            const generation = ++this.assetGeneration;
            const owner = this.mediaOwner;
            // Return the currently selected editor value
            const current = () => generation === this.assetGeneration && this.mediaOwner === owner;
            const formats = managedFormats(
                app().device?.manifest.capabilities.media_formats || mediaFormats,
            );
            try {
                const path = await chooseMediaInput(formats);
                if (!path || !current()) return;
                await selectMediaInput(app(), path, {
                    owner,
                    formats,
                    current,
                    controlId: app().controlId,
                    // Apply the pending editor changes
                    apply: (asset, options) => {
                        this.asset = asset;
                        this.media = options;
                        this.markDirty();
                    },
                });
            } catch (error) {
                if (current() && !/cancelled|selection changed/i.test(String(error)))
                    app().notify(String(error), 'error');
            }
        },
        // Remove the asset
        removeAsset() {
            this.assetGeneration++;
            this.asset = undefined;
            this.dirty = true;
        },
        // Derive media owner from the current editor state
        get mediaOwner() {
            return 'control:' + this.contextKey;
        },
        // Derive media formats label from the current editor state
        get mediaFormatsLabel() {
            return managedFormats(app().device?.manifest.capabilities.media_formats || mediaFormats)
                .map((f) => f.toUpperCase())
                .join(', ');
        },
        // Derive app from the current editor state
        get app() {
            return app();
        },
        // Derive control name from the current editor state
        get controlName() {
            return app().control?.label || 'Select a control';
        },
        // Derive control type from the current editor state
        get controlType() {
            if (app().controlKind === 'display') return 'Display only';
            const c = app().control;
            return c ? `${c.kind} · ${c.id}` : 'Click a key, dial, or touch region';
        },
        // Derive needs value from the current editor state
        get needsValue() {
            if (this.dialVolume) return false;
            return [
                'open_url',
                'hotkey',
                'application',
                'open_path',
                'volume',
                'brightness',
                'media',
                'custom',
            ].includes(this.kind);
        },
        // Derive value label from the current editor state
        get valueLabel() {
            return (
                (
                    {
                        open_url: 'URL',
                        hotkey: 'Keys, separated by +',
                        command: 'Program (no shell expansion)',
                        application: 'Application path',
                        open_path: 'File path',
                        volume: 'Volume change (%)',
                        brightness: 'Brightness (0–100)',
                        media: 'play-pause / next / previous / stop',
                        macro: 'Actions (JSON array)',
                        custom: 'Advanced action (JSON)',
                    } as Record<string, string>
                )[this.kind] || 'Value'
            );
        },
        // Derive value placeholder from the current editor state
        get valuePlaceholder() {
            return this.kind === 'open_url'
                ? 'https://example.org'
                : this.kind === 'hotkey'
                  ? 'Ctrl + Shift + A'
                  : this.kind === 'macro'
                    ? '[{"kind":"delay","milliseconds":100}]'
                    : '';
        },
        // Derive selected address from the current editor state
        get selectedAddress() {
            return this.address();
        },
        // Derive saved binding signature from the current editor state
        get savedBindingSignature() {
            // A dirty editor owns an isolated draft. Its saved baseline is checked
            // on commit; unrelated workspace changes need no serialized copy here
            return this.dirty ? '' : this.savedSignature();
        },
        // Initialize editor state and its required lifecycle hooks
        init() {
            ownWindowMethods(this, 'inspector', [
                'discardDraft',
                'replaceConflict',
                'chooseAsset',
                'save',
            ]);
            this.unregisterCloseOwner = registerCloseDraft(this, ['save', 'replaceConflict'], {
                name: 'Current binding',
                // Return a snapshot of the current editor state
                state: () =>
                    this.unsavedChanges || this.pluginSettingDrafts.size ? 'dirty' : 'clean',
                // Discard pending editor edits without saving them
                discard: async () => {
                    if (this.dirty) await this.discardDraft();
                    this.pluginSettingDrafts.clear();
                },
                // Release resources retained by the editor operation
                release: async () => {
                    if (this.draftTimer) clearTimeout(this.draftTimer);
                    this.draftTimer = undefined;
                    this.releaseDraftTimer?.();
                    this.releaseDraftTimer = undefined;
                    this.previewWork.cancel();
                    await clearLiveActionPreview(this.livePreviewOwner);
                    await releaseMediaOwner(app(), 'control:' + this.contextKey);
                },
            });
            this.updateMacro = this.updateMacro.bind(this);
            this.updateStructured = this.updateStructured.bind(this);
            this.updateMedia = this.updateMedia.bind(this);
            this.load();
            if (suspended?.key === this.contextKey && app().draftStore[this.contextKey]) {
                this.restoreDraft();
                this.bindingScope = suspended.scope;
                this.scopeReviewed = suspended.reviewed;
            }
            suspended = null;
            this.applyPendingAction();
            this.$watch('selectedAddress', () => this.load());
            this.$watch('$store.app.pendingAction', () => this.applyPendingAction());
            this.$watch('$store.app.deviceId', () => this.load());
            // Complete the deferred editor work using the current operation state
            this.$watch('savedBindingSignature', () => {
                if (!this.dirty && this.loadedSignature !== this.savedBindingSignature) this.load();
            });
            // Complete the deferred editor work using the current operation state
            this.$watch('dirty', () => {
                this.refreshLivePreview();
                app().dirtyBindingAddress = this.dirty ? this.contextKey : '';
            });
            if (this.dirty) {
                app().dirtyBindingAddress = this.contextKey;
                this.refreshLivePreview();
            }
            for (const key of bindingDraftFields)
                // Complete the deferred editor work using the current operation state
                this.$watch(key, () => {
                    this.scheduleDraft();
                    this.refreshLivePreview();
                });
            this.$watch('$store.app.configurationRevision', () => this.refreshLivePreview());
            app().flushDrafts = (fence) => this.persistDraft(fence);
            // Complete the deferred editor work using the current operation state
            app().bindingCleared = (address) => {
                this.clearPluginAlternatives(address);
                if (this.contextKey !== address) return;
                // Keep unfinished work recoverable, but never show it as the cleared binding
                this.dirty = false;
                this.load();
            };
        },
        // Apply the pending action
        applyPendingAction() {
            if (windowWork.quiescing) {
                if (app().pendingAction) windowWork.invalidate();
                return;
            }
            const action = app().pendingAction;
            if (!action) return;
            if (!this.canEditEvent || !this.rememberPluginSettings()) {
                app().pendingAction = null;
                return;
            }
            if (action.kind === 'builtin') {
                this.actionDraft = new InspectorActionDraft(action);
                if (!this.eventActions) this.title = app().pendingActionTitle;
                this.markDirty();
                app().pendingAction = null;
                return;
            }
            if (action.kind !== 'plugin') {
                this.actionDraft = new InspectorActionDraft(action);
                this.actionDraft.editJSON();
                if (!this.eventActions) this.title = app().pendingActionTitle;
                this.markDirty();
                app().pendingAction = null;
                return;
            }
            this.actionDraft = new InspectorActionDraft(action);
            const saved = this.savedAction();
            this.pluginSettings = this.pluginSettingDrafts.restore(
                this.activeActionContext,
                action.plugin_id,
                action.action_id,
                saved?.kind === 'plugin' &&
                    saved.plugin_id === action.plugin_id &&
                    saved.action_id === action.action_id
                    ? saved.settings
                    : action.settings,
            );
            if (!this.eventActions) this.title = app().pendingActionTitle;
            this.dirty = true;
            app().pendingAction = null;
        },
        // Retain per-action plugin settings relative to the saved action without committing the binding
        rememberPluginSettings() {
            if (this.kind !== 'plugin') return true;
            const saved = this.savedAction();
            try {
                this.pluginSettingDrafts.remember(
                    this.activeActionContext,
                    this.pluginId,
                    this.pluginActionId,
                    this.pluginSettings,
                    saved?.kind === 'plugin' &&
                        saved.plugin_id === this.pluginId &&
                        saved.action_id === this.pluginActionId
                        ? saved.settings
                        : undefined,
                );
                return true;
            } catch (error) {
                app().notify(String(error), 'error');
                return false;
            }
        },
        // Read the current device, profile, page and control binding identity
        address() {
            return app().bindingAddress();
        },
        // Combine the binding address and saved content signature to detect stale draft bases
        savedSignature() {
            return this.address() + bindingSignature(app().binding || null);
        },
        // Load the data required by the editor workflow
        load() {
            if (windowWork.quiescing) {
                windowWork.invalidate();
                return;
            }
            this.previewWork.cancel();
            void clearLiveActionPreview(this.livePreviewOwner);
            const oldKey = this.contextKey;
            const pending = this.persistDraft();
            if (oldKey && oldKey !== this.address())
                swallow(
                    // Process the settled editor operation result
                    pending.then(async () => {
                        if (!pendingDraftWrites.has(oldKey))
                            await releaseMediaOwner(app(), 'control:' + oldKey);
                    }),
                );
            this.linkTurnsPending = false;
            this.eventConversion = null;
            this.resetEventTest();
            this.eventTestChoice = null;
            this.contextKey = this.address();
            this.loadedSignature = this.savedSignature();
            this.recovered = false;
            this.bindingScope = 'page';
            this.scopeReviewed = '';
            this.conflict = false;
            this.assetGeneration++;
            const b = app().binding;
            Object.assign(
                this,
                this.inspectorDraft(
                    loadBindingDraft(b, app().controlKind === 'encoder' ? 'linear' : ''),
                ),
            );
            if (app().controlKind === 'encoder' && (!b || b.action.kind === 'none')) {
                this.actionTree = new ActionDraftTree({
                    kind: 'dial',
                    actions: [{ kind: 'none' }, { kind: 'none' }, { kind: 'none' }],
                });
            }
            this.draftBase = cloneJson(b || null);
            this.ensureEditableEvent();
            if (this.asset)
                void retainMediaFor(app(), this.asset, this.mediaOwner).catch((error) =>
                    app().notify(String(error), 'error'),
                );
            this.dirty = false;
            this.closeBaseline = bindingSignature(this.draftValue());
        },
        // Release editor state and associated lifecycle resources
        destroy() {
            this.resetEventTest();
            this.previewWork.cancel();
            void clearLiveActionPreview(this.livePreviewOwner);
            app().dirtyBindingAddress = '';
            this.unregisterCloseOwner?.();
            const ownerKey = this.contextKey;
            if (this.dirty && !windowCloseOwners.closing)
                suspended = {
                    key: ownerKey,
                    scope: this.bindingScope,
                    reviewed: this.scopeReviewed,
                };
            if (!windowWork.quiescing && !windowCloseOwners.closing)
                swallow(
                    // Process the settled editor operation result
                    this.persistDraft().then(async () => {
                        if (!pendingDraftWrites.has(ownerKey))
                            await releaseMediaOwner(app(), 'control:' + ownerKey);
                    }),
                );
            if (this.draftTimer) clearTimeout(this.draftTimer);
            this.releaseDraftTimer?.();
            this.releaseDraftTimer = undefined;
            app().flushDrafts = undefined;
            app().bindingCleared = undefined;
            this.assetGeneration++;
        },
        get savedMacroChoices() {
            return [...app().snapshot.saved_macros].sort(
                (a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id),
            );
        },
        get savedMacroHelp() {
            if (!app().snapshot.saved_macros.length)
                return 'Create a macro on the Macros page first.';
            return this.actionDraft.action.kind === 'macro' &&
                this.actionDraft.action.actions.length
                ? 'Choosing a macro replaces the inline steps in this draft.'
                : 'This links to the saved macro. Changes to it apply here too.';
        },
        useSavedMacro(event: Event) {
            const input = event.currentTarget as HTMLSelectElement;
            const id = input.value;
            input.value = '';
            if (
                !this.canEditEvent ||
                this.kind !== 'macro' ||
                this.dialMultiActionOriginal ||
                !app().snapshot.saved_macros.some((macro) => macro.id === id)
            )
                return;
            this.actionDraft = new InspectorActionDraft({
                kind: 'builtin',
                action: { kind: 'saved_macro', macro_id: id },
            });
            this.markDirty();
        },
        // Update the macro
        updateMacro(event: CustomEvent<{ actions: Action[] }>) {
            this.dialMultiActionOriginal = null;
            this.value = JSON.stringify(event.detail.actions);
            this.dirty = true;
        },
        // Mark the dirty
        markDirty() {
            this.dirty = true;
            this.resetEventTest();
            this.refreshLivePreview();
        },
        linkPage: '',
        linkControl: '',
        linkRevision: 0,
        linkSource: '',
        linkChoice: '',
        get linkedMembers() {
            const profile = app().profile;
            return (
                Object.values(profile?.linked_controls || {}).find((g) =>
                    g?.some(
                        (m) => m.page_id === app().page?.id && m.control_id === app().controlId,
                    ),
                ) || []
            );
        },
        get linkedSummary() {
            return this.linkedMembers
                .map(
                    (m) =>
                        (app().profile?.pages.find((p) => p.id === m.page_id)?.name || m.page_id) +
                        ' / ' +
                        m.control_id,
                )
                .join(', ');
        },
        get linkPages() {
            return app().profile?.pages.filter((p) => p.id !== app().page?.id) || [];
        },
        get linkControls() {
            const selected = app().control;
            return (
                app().device?.manifest.controls.filter(
                    (c) =>
                        c.kind === selected?.kind &&
                        JSON.stringify(c.semantics) === JSON.stringify(selected.semantics) &&
                        !c.preview_hidden &&
                        !isKeyboardNavigationControl(app().profile, this.linkPage, c.id) &&
                        !Object.values(app().profile?.linked_controls || {}).some((group) =>
                            group?.some(
                                (m) => m.page_id === this.linkPage && m.control_id === c.id,
                            ),
                        ),
                ) || []
            );
        },
        get linkDestination() {
            return app().profile?.pages.find((p) => p.id === this.linkPage)?.bindings[
                this.linkControl
            ];
        },
        get canLink() {
            return (
                !!this.linkPages.length &&
                this.linkControls.some((c) => c.id === this.linkControl) &&
                (!this.linkDestination ||
                    this.linkChoice === 'inspector' ||
                    this.linkChoice === 'destination')
            );
        },
        get inspectorAssignment() {
            const b = app().binding;
            return [app().page?.name, app().controlId, b?.title || b?.action.kind]
                .filter(Boolean)
                .join(' / ');
        },
        get destinationAssignment() {
            const target = app().profile?.pages.find((p) => p.id === this.linkPage);
            const b = this.linkDestination;
            return [target?.name, this.linkControl, b?.title || b?.action.kind || '(empty)']
                .filter(Boolean)
                .join(' / ');
        },
        get replacementSummary() {
            return (
                'Affected assignments · ' +
                (this.linkedSummary || this.inspectorAssignment) +
                ', ' +
                this.destinationAssignment
            );
        },
        openLink() {
            this.linkPage = this.linkPages[0]?.id || '';
            this.linkControl = app().controlId;
            this.linkChoice = '';
            this.linkSource = this.contextKey;
            this.linkRevision = app().configurationRevision;
            (this.$refs.linkDialog as HTMLDialogElement).showModal();
        },
        closeLink() {
            (this.$refs.linkDialog as HTMLDialogElement).close();
        },
        async createLink() {
            const s = app(),
                profile = s.profile,
                page = s.page;
            if (this.contextKey !== this.linkSource || !profile || !page || !this.canLink) return;
            const source = { page_id: page.id, control_id: s.controlId };
            const edits: Edit[] = [];
            if (this.linkChoice === 'destination' && this.linkDestination) {
                edits.push({
                    kind: 'bindings',
                    profile_id: profile.id,
                    page_id: page.id,
                    bindings: { [s.controlId]: cloneJson(this.linkDestination) },
                });
            }
            edits.push({
                kind: 'link_control',
                profile_id: profile.id,
                source,
                target: { page_id: this.linkPage, control_id: this.linkControl },
            });
            try {
                await s.edit(edits, 'Link controls', this.linkRevision);
                this.closeLink();
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        async detachLink() {
            if (!app().profile || !app().page) return;
            try {
                await app().edit(
                    [
                        {
                            kind: 'detach_control',
                            profile_id: app().profile!.id,
                            member: { page_id: app().page!.id, control_id: app().controlId },
                        },
                    ],
                    'Detach linked control',
                );
            } catch (error) {
                app().notify(String(error), 'error');
            }
        },
        bindingScope: 'page' as BindingScope,
        scopeReviewed: '',
        // Derive bulk binding scope from the current editor state
        get bulkBindingScope() {
            return this.bindingScope !== 'page';
        },
        // Derive binding scope plan from the current editor state
        get bindingScopePlan() {
            return bindingScopePlan(
                app().snapshot.profiles,
                app().profile?.id || '',
                app().page?.id || '',
                app().controlId,
                this.bindingScope,
            );
        },
        // Derive binding scope summary from the current editor state
        get bindingScopeSummary() {
            const plan = this.bindingScopePlan;
            return `Copy this control’s complete assignment to ${plan.targets.length} pages in ${plan.profileCount} profiles. ${plan.overwritten} existing assignments will be replaced. Other controls are unchanged.`;
        },
        // Derive binding scope reviewed from the current editor state
        get bindingScopeReviewed() {
            return this.scopeReviewed === this.bindingScopePlan.signature;
        },
        // Change the binding scope
        changeBindingScope(event: Event) {
            const value = (event.target as HTMLSelectElement).value;
            if (!['page', 'profile', 'model'].includes(value)) return;
            this.bindingScope = value as BindingScope;
            this.scopeReviewed = '';
            this.dirty = true;
        },
        // Record acceptance of the current binding-scope plan rather than a previous plan
        reviewBindingScope(event: Event) {
            this.scopeReviewed = (event.target as HTMLInputElement).checked
                ? this.bindingScopePlan.signature
                : '';
        },
        // Save pending changes through the editor workflow
        async save() {
            this.resetEventTest();
            if (this.updateError) {
                app().notify(this.updateError, 'error');
                return;
            }
            if (this.saving || app().controlPending(app().controlId)) return;
            if (bindingSignature(this.draftBase) !== bindingSignature(app().binding || null))
                this.conflict = true;
            if (this.conflict) {
                app().notify(
                    'The binding changed. Compare the current binding and explicitly replace it or discard this draft.',
                    'warning',
                );
                return;
            }
            const current = app().profile;
            if (!app().control || (current ? !app().page : !app().device?.connected)) return;
            if (!current && this.bulkBindingScope) {
                app().notify(
                    'Save the first binding on this page before using a wider assignment scope.',
                    'warning',
                );
                return;
            }
            const controlId = app().controlId;
            const contextKey = this.contextKey;
            this.savingContexts[contextKey] = true;
            try {
                const existing = app().draftStore[contextKey];
                if (existing) ActionDraftTree.restore(existing.value as Record<string, unknown>);
                const binding = commitBindingDraft(this);
                const plan = this.bindingScopePlan;
                if (this.bulkBindingScope && this.scopeReviewed !== plan.signature) {
                    app().notify(
                        'Review the destination count and confirm replacing these assignments.',
                        'warning',
                    );
                    return;
                }
                await app().edit(
                    current
                        ? scopedBindingEdits(plan, controlId, binding)
                        : [
                              {
                                  kind: 'bindings',
                                  profile_id: '',
                                  page_id: '',
                                  bindings: { [controlId]: binding },
                              },
                          ],
                    this.bulkBindingScope ? 'Assign control across pages' : 'Save binding',
                );
                this.clearPluginAlternatives(contextKey);
                if (this.contextKey === contextKey) {
                    this.dirty = false;
                    if (this.draftTimer) clearTimeout(this.draftTimer);
                    this.load();
                }
                try {
                    const savedDraft = app().draftStore[contextKey];
                    await draftQueue.clear(contextKey);
                    if (app().draftStore[contextKey] === savedDraft)
                        delete app().draftStore[contextKey];
                    forgetWindowDraft(contextKey);
                } catch (e) {
                    app().notify('Binding saved; draft cleanup failed: ' + String(e), 'warning');
                }
                await releaseMediaOwner(app(), 'control:' + contextKey);
                app().notify('Binding saved', 'success');
            } catch (e) {
                app().notify(String(e), 'error');
            } finally {
                delete this.savingContexts[contextKey];
            }
        },
        // Clear the retained editor value
        clear() {
            this.$dispatch('request-control-clear', { controlId: app().controlId });
        },
    }));
}
