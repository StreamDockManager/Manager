import { keyboardNavigationBindings } from './keyboard-navigation';
import {
    usesPluginBackground,
    showsTransparentBackground,
    changeBackgroundRgb,
    changeBackgroundAlpha,
} from './background-color';
import { cloneJson } from './json-clone';
import { dockPreviewReason } from './dock-preview';
import { setLiveActionPreview, clearLiveActionPreview } from './live-action-preview';
import Alpine from '@alpinejs/csp';
import { windowCloseOwners } from './window-close-owners';
import { registerCloseDraft, trackCloseRelease } from './close-releases';
import { fontChoices } from './display-fonts';
import { clone, initialLayers, layerStyle, sourceId, validateComposition } from './display-canvas';
import { app } from './route-context';
import { defaultAppearance } from './features';
import { actionAppearanceDefaults } from './action-appearance-defaults';
import { defaultMedia } from './bridge';
import { insertVaultTextReference, type VaultTextSelection } from './vault-action-fields';
import type { Binding } from './types';
import type {
    IconAsset,
    Action,
    PluginAppearance,
    DisplayElement,
    DisplayState,
    ElementStyle,
    DisplayComposition,
    DisplaySourceDescriptor,
    DisplayLayer,
    VaultEntry,
} from '../generated/runtime';
import { iconImage } from './icon-selector';
import { DEFAULT_BACKGROUND, backgroundRgb } from './background-color';
// Register the binding appearance editor and its preview lifecycle
export function registerAppearanceEditor() {
    Alpine.data('appearanceEditor', () => ({
        systemFonts: [] as string[],
        fontsLoaded: false,
        fontsLoading: false,
        fontError: '',
        fontRequest: 0,
        // Build font choices from available and already-used families
        get fontOptions() {
            return fontChoices(
                this.systemFonts,
                [
                    this.appearance.font,
                    ...this.canvasLayers.map((l) => l.style.font),
                    ...(this.pluginAppearance?.elements.map((e) => e.font) || []),
                    ...this.pluginElements.map((e) => e.style.font),
                ],
                this.fontsLoaded,
            );
        },
        fontLabel(id: string): string {
            return this.fontOptions.find((font) => font.id === id)?.label || id;
        },
        get appearanceFontLabel(): string {
            return (
                this.fontOptions.find((font) => font.id === this.appearance.font)?.label ||
                this.appearance.font
            );
        },
        applyLabelFont(
            event: CustomEvent<{ font: string; fontWeight: number; fontItalic: boolean }>,
        ) {
            this.appearance = {
                ...this.appearance,
                font: event.detail.font,
                font_weight: event.detail.fontWeight === 700 ? 700 : 400,
                font_italic: event.detail.fontItalic === true,
            };
        },
        applyLayerFont(
            event: CustomEvent<{ font: string; fontWeight: number; fontItalic: boolean }>,
        ) {
            this.ensureComposition();
            const layer = this.composition!.layers.find(
                (entry) => entry.id === this.selectedLayerId,
            );
            if (!layer) return;
            layer.style = {
                ...layer.style,
                font: event.detail.font,
                font_weight: event.detail.fontWeight === 700 ? 700 : 400,
                font_italic: event.detail.fontItalic === true,
            };
            this.requestAppearance();
        },
        applyPluginFont(
            event: CustomEvent<{ font: string; fontWeight: number; fontItalic: boolean }>,
        ) {
            const element = this.editableElement;
            if (!element || element.kind !== 'text') return;
            Object.assign(element, {
                font: event.detail.font,
                font_weight: event.detail.fontWeight === 700 ? 700 : 400,
                font_italic: event.detail.fontItalic === true,
            });
            this.styleElement();
        },
        // Load fonts while rejecting stale editor responses
        async loadFonts(refresh = false) {
            const request = ++this.fontRequest;
            this.fontsLoading = true;
            this.fontError = '';
            try {
                const fonts = await app().bridge.command('system_fonts', { refresh });
                if (request !== this.fontRequest || !this.opened) return;
                this.systemFonts = fonts;
                this.fontsLoaded = true;
                this.requestAppearance();
            } catch (error) {
                if (request === this.fontRequest && this.opened)
                    this.fontError = 'Could not load installed fonts: ' + String(error);
            } finally {
                if (request === this.fontRequest) this.fontsLoading = false;
            }
        },
        // Request a fresh list of available font families
        refreshFonts() {
            void this.loadFonts(true);
        },
        composition: undefined as DisplayComposition | undefined,
        canvasSources: [] as DisplaySourceDescriptor[],
        selectedLayerId: '',
        addSourceId: '',
        placeholder: false,
        drag: null as {
            id: string;
            x: number;
            y: number;
            rect: number[];
            resize: boolean;
            width: number;
            height: number;
        } | null,
        // Expose the editable composition layers
        get canvasLayers(): DisplayLayer[] {
            return this.composition?.layers || initialLayers(this.canvasSources);
        },
        // Find the currently selected composition layer
        get selectedLayer(): DisplayLayer | undefined {
            return this.canvasLayers.find((layer) => layer.id === this.selectedLayerId);
        },
        // Calculate the preview aspect ratio from the target surface
        get canvasAspect() {
            const c =
                app().device?.manifest.controls.find(
                    (c) => c.id === (app().control?.feedback_surface || app().controlId),
                ) || app().control;
            return String((c?.image_width || 112) / (c?.image_height || 112));
        },
        // Validate the current display composition
        get canvasError() {
            return validateComposition(this.composition);
        },
        // Convert a layer’s geometry into canvas coordinates
        canvasRect(layer: DisplayLayer) {
            const r = layer.style.rect;
            return { left: `${r[0]}%`, top: `${r[1]}%`, width: `${r[2]}%`, height: `${r[3]}%` };
        },
        // Create a stable key for a display source
        sourceKey(source: DisplaySourceDescriptor) {
            return sourceId(source.source);
        },
        // Describe the source used by a composition layer
        sourceState(layer: DisplayLayer) {
            if (layer.source.kind === 'custom_icon')
                return layer.style.icon_asset || Object.keys(layer.style.state_icons).length
                    ? ''
                    : 'Choose an icon';
            if (layer.source.kind === 'text') return '';
            const source = this.canvasSources.find(
                (s) => sourceId(s.source) === sourceId(layer.source),
            );
            return !source?.available
                ? 'Source unavailable'
                : source.placeholder
                  ? 'Placeholder'
                  : '';
        },
        // Select the layer identified by the control event
        selectLayer(event: Event) {
            this.selectedLayerId = (event.currentTarget as HTMLElement).dataset.layer || '';
        },
        // Create a composition before editing its layers
        ensureComposition() {
            if (!this.composition) this.composition = { layers: clone(this.canvasLayers) };
        },

        // Apply the edited property to the selected layer
        editLayer(event: Event) {
            this.ensureComposition();
            const layer = this.composition!.layers.find((l) => l.id === this.selectedLayerId);
            if (!layer) return;
            const input = event.currentTarget as HTMLInputElement;
            const field = input.dataset.field || '';
            if (field.startsWith('rect'))
                layer.style.rect[Number(field.slice(4))] = input.valueAsNumber;
            else if (field === 'text') layer.text = input.value;
            else if (field === 'label') layer.label = input.value;
            else if (field === 'size') layer.style.size = input.valueAsNumber;
            else if (field === 'visible' || field === 'wrap') layer.style[field] = input.checked;
            else if (
                field === 'font' ||
                field === 'foreground' ||
                field === 'align' ||
                field === 'vertical_align'
            )
                layer.style[field] = input.value;
            this.requestAppearance();
        },
        layerVaultEntries: [] as VaultEntry[],
        layerVaultBusy: false,
        layerVaultMessage: '',
        layerVaultRequest: 0,
        layerVaultSelection: null as VaultTextSelection | null,
        resetLayerVault() {
            this.layerVaultRequest++;
            this.layerVaultEntries = [];
            this.layerVaultBusy = false;
            this.layerVaultMessage = '';
            this.layerVaultSelection = null;
        },
        async chooseLayerVault(event: Event) {
            const layer = this.selectedLayer;
            if (!layer || layer.source.kind !== 'text' || this.layerVaultBusy) return;
            const field = (event.currentTarget as HTMLButtonElement)
                .closest('[data-layer-vault]')
                ?.querySelector('textarea');
            if (!field) return;
            this.resetLayerVault();
            const request = this.layerVaultRequest;
            this.layerVaultSelection = {
                value: field.value,
                start: field.selectionStart,
                end: field.selectionEnd,
            };
            this.layerVaultBusy = true;
            this.layerVaultMessage = 'Loading Vault entries…';
            const current = () =>
                this.opened &&
                this.layerVaultRequest === request &&
                this.selectedLayerId === layer.id;
            try {
                const entries = await app().bridge.command('vault_builtin_choices', {
                    pathField: false,
                    jsonField: false,
                });
                if (!current()) return;
                this.layerVaultEntries = entries.filter(
                    (entry) =>
                        !entry.owner &&
                        !entry.protected &&
                        ['text', 'integer', 'boolean', 'location'].includes(entry.kind),
                );
                this.layerVaultMessage = this.layerVaultEntries.length
                    ? ''
                    : 'No unprotected Vault entries can be displayed.';
            } catch {
                if (current()) this.layerVaultMessage = 'Unlock Vault to choose an entry.';
            } finally {
                if (current()) this.layerVaultBusy = false;
            }
        },
        insertLayerVault(event: Event) {
            const select = event.currentTarget as HTMLSelectElement;
            const field = select.closest('[data-layer-vault]')?.querySelector('textarea');
            const entry = this.layerVaultEntries.find((entry) => entry.id === select.value);
            const selection = this.layerVaultSelection;
            if (!entry || !field || !selection || this.selectedLayer?.source.kind !== 'text')
                return;
            if (selection.value !== field.value) {
                this.resetLayerVault();
                this.layerVaultMessage = 'Label text changed. Choose the Vault entry again.';
                return;
            }
            const token = '${vault.' + entry.id + '}';
            const next = insertVaultTextReference(field.value, token, selection);
            if (new TextEncoder().encode(next).length > field.maxLength) {
                this.resetLayerVault();
                this.layerVaultMessage = 'This Vault reference exceeds the label text limit.';
                return;
            }
            this.ensureComposition();
            const layer = this.composition!.layers.find((l) => l.id === this.selectedLayerId);
            if (!layer) return;
            layer.text = next;
            field.value = next;
            this.resetLayerVault();
            field.focus();
            const caret = selection.start + token.length;
            field.setSelectionRange(caret, caret);
            this.requestAppearance();
        },
        // Append a layer with defaults appropriate to its kind
        addLayer(kind: string) {
            this.ensureComposition();
            if (this.composition!.layers.length >= 64) return;
            const id = crypto.randomUUID();
            const descriptor = this.canvasSources.find(
                (s) => sourceId(s.source) === this.addSourceId,
            );
            if (kind === 'source' && !descriptor) return;
            const source =
                kind === 'source' && descriptor
                    ? clone(descriptor.source)
                    : {
                          kind: kind === 'icon' ? 'custom_icon' : 'text',
                          plugin_id: '',
                          action_id: '',
                          layout: '',
                          key: '',
                          element_kind: '',
                      };
            const layer: DisplayLayer = {
                id,
                label:
                    kind === 'source' && descriptor
                        ? descriptor.label
                        : kind === 'icon'
                          ? 'Custom icon'
                          : 'Label',
                source,
                style:
                    kind === 'source' && descriptor
                        ? clone(descriptor.style)
                        : layerStyle(id, kind === 'icon' ? 'pixmap' : 'text'),
                text: kind === 'text' ? 'Label' : '',
            };
            if (kind === 'icon') layer.style.rect = [32, 4, 36, 32];
            layer.style.visible = true;
            this.composition!.layers.push(layer);
            this.selectedLayerId = id;
            this.requestAppearance();
            if (kind === 'icon') this.openLayerIcons();
        },
        // Append a text layer to the composition
        addTextLayer() {
            this.addLayer('text');
        },
        // Append an icon layer to the composition
        addIconLayer() {
            this.addLayer('icon');
        },
        // Append a plugin source layer to the composition
        addSourceLayer() {
            this.addLayer('source');
        },
        // Move a layer within the composition stacking order
        moveLayer(event: Event) {
            this.ensureComposition();
            const layers = this.composition!.layers;
            const index = layers.findIndex((l) => l.id === this.selectedLayerId),
                delta = Number((event.currentTarget as HTMLElement).dataset.delta);
            if (index < 0 || index + delta < 0 || index + delta >= layers.length) return;
            [layers[index], layers[index + delta]] = [layers[index + delta], layers[index]];
            this.requestAppearance();
        },
        // Remove the selected composition layer
        deleteLayer() {
            this.ensureComposition();
            this.composition!.layers = this.composition!.layers.filter(
                (l) => l.id !== this.selectedLayerId,
            );
            this.selectedLayerId = this.composition!.layers[0]?.id || '';
            this.requestAppearance();
        },
        // Restore the composition to its initial layers
        resetCanvas() {
            this.composition = undefined;
            this.selectedLayerId = '';
            this.requestAppearance();
        },
        // Update the selected layer’s size from the editor controls
        resizeLayer(event: Event) {
            this.ensureComposition();
            const layer = this.selectedLayer;
            if (!layer) return;
            const delta = Number((event.currentTarget as HTMLElement).dataset.delta);
            layer.style.rect[2] = Math.min(100, Math.max(0.5, layer.style.rect[2] + delta));
            layer.style.rect[3] = Math.min(100, Math.max(0.5, layer.style.rect[3] + delta));
            this.requestAppearance();
        },
        // Move or resize the selected layer using keyboard input
        canvasKey(event: KeyboardEvent) {
            const delta = event.shiftKey ? 5 : 1;
            let x = 0,
                y = 0;
            if (event.key === 'ArrowLeft') x = -delta;
            else if (event.key === 'ArrowRight') x = delta;
            else if (event.key === 'ArrowUp') y = -delta;
            else if (event.key === 'ArrowDown') y = delta;
            else return;
            event.preventDefault();
            this.ensureComposition();
            const layer = this.selectedLayer;
            if (!layer) return;
            layer.style.rect[0] = Math.min(100, Math.max(0, layer.style.rect[0] + x));
            layer.style.rect[1] = Math.min(100, Math.max(0, layer.style.rect[1] + y));
            this.requestAppearance();
        },
        // Capture the selected layer and pointer origin for dragging
        startCanvasDrag(event: PointerEvent) {
            if (event.button !== 0) return;
            event.preventDefault();
            this.ensureComposition();
            const target = event.currentTarget as HTMLElement;
            this.selectedLayerId = target.dataset.layer || '';
            const layer = this.selectedLayer,
                canvas = target.closest('.display-canvas')?.getBoundingClientRect();
            if (!layer || !canvas) return;
            target.focus();
            target.setPointerCapture(event.pointerId);
            this.drag = {
                id: layer.id,
                x: event.clientX,
                y: event.clientY,
                rect: [...layer.style.rect],
                resize: (event.target as HTMLElement).dataset.resize === 'true',
                width: canvas.width,
                height: canvas.height,
            };
        },
        // Apply pointer movement to the active canvas drag
        moveCanvasDrag(event: PointerEvent) {
            const drag = this.drag;
            if (!drag) return;
            const layer = this.composition?.layers.find((l) => l.id === drag.id);
            if (!layer) return;
            const dx = ((event.clientX - drag.x) / drag.width) * 100,
                dy = ((event.clientY - drag.y) / drag.height) * 100;
            const index = drag.resize ? 2 : 0;
            layer.style.rect[index] = Math.min(
                100,
                Math.max(drag.resize ? 0.5 : 0, drag.rect[index] + dx),
            );
            layer.style.rect[index + 1] = Math.min(
                100,
                Math.max(drag.resize ? 0.5 : 0, drag.rect[index + 1] + dy),
            );
            this.requestAppearance();
        },
        // Release the active canvas drag state
        endCanvasDrag() {
            this.drag = null;
        },
        // Open icon selection for the selected composition layer
        openLayerIcons() {
            this.iconTarget = 'canvas';
            this.iconContext = crypto.randomUUID();
            window.dispatchEvent(
                new CustomEvent('open-icon-selector', {
                    detail: {
                        context: this.iconContext,
                        asset: this.selectedLayer?.style.icon_asset,
                        icon: this.selectedLayer?.style.icon_asset?.id || '',
                    },
                }),
            );
        },
        title: '',
        liveLabel: [] as NonNullable<Binding['live_label']>,
        labelScenario: 'missing',
        get labelExample() {
            const sample =
                this.labelScenario === 'missing'
                    ? undefined
                    : this.labelScenario === 'false'
                      ? false
                      : this.labelScenario === 'zero'
                        ? 0
                        : 'Sample';
            const text = this.liveLabel
                .map((s) => (s.kind === 'literal' ? s.text : String(sample ?? s.fallback)))
                .join('');
            return Array.from(text).slice(0, 160).join('') || this.title;
        },
        labelField: 'literal',
        labelValue: '',
        get labelFields() {
            return [
                { value: 'literal', label: 'Literal text' },
                { value: 'macro', label: 'Macro state' },
                ...(app().profile?.parameters || [])
                    .filter((p) => !p.sensitive)
                    .map((p) => ({ value: 'param:' + p.name, label: 'Parameter · ' + p.label })),
                ...this.pluginElements
                    .filter((e) => ['text', 'bar', 'gbar', 'gauge'].includes(e.style.kind))
                    .map((e) => ({
                        value: 'display:' + e.style.key,
                        label: 'Display · ' + e.label,
                    })),
            ];
        },
        addLabelSegment() {
            if (this.liveLabel.length >= 16) return;
            const field = this.labelField,
                fallback = this.labelValue;
            this.liveLabel.push(
                field === 'literal'
                    ? { kind: 'literal', text: fallback }
                    : field === 'macro'
                      ? { kind: 'macro_state', fallback }
                      : field.startsWith('param:')
                        ? { kind: 'parameter', name: field.slice(6), fallback }
                        : { kind: 'display', key: field.slice(8), fallback },
            );
            this.labelValue = '';
            this.requestAppearance();
        },
        removeLabelSegment(index: number) {
            this.liveLabel.splice(index, 1);
            this.requestAppearance();
        },
        segmentText(s: NonNullable<Binding['live_label']>[number]) {
            if (s.kind === 'literal') return s.text;
            return (
                (s.kind === 'macro_state'
                    ? 'Macro state'
                    : s.kind === 'parameter'
                      ? s.name
                      : s.key) + (s.fallback ? ' · Fallback: ' + s.fallback : '')
            );
        },
        color: DEFAULT_BACKGROUND,
        customBackground: false,
        backgroundSource: 'custom' as 'custom' | 'palette',
        // Change which source supplies the display background
        changeBackgroundSource(event: Event) {
            this.backgroundSource = (event.currentTarget as HTMLSelectElement).value as
                'custom' | 'palette';
            this.requestAppearance();
        },
        // Expose the editable background RGB color
        get backgroundColor() {
            return backgroundRgb(this.color);
        },
        // Report whether the background is transparent
        get transparentBackground() {
            return showsTransparentBackground(this, this.usesDefaultPluginBackground);
        },
        // Report whether the plugin’s default background is used
        get usesDefaultPluginBackground() {
            return usesPluginBackground(this, this.action.kind, this.asset);
        },
        // Apply an edited background color while preserving transparency
        changeBackgroundColor(event: Event) {
            Object.assign(
                this,
                changeBackgroundRgb(
                    this,
                    (event.currentTarget as HTMLInputElement).value,
                    this.usesDefaultPluginBackground,
                ),
            );
        },
        // Update background transparency without losing its RGB value
        changeBackgroundTransparency(event: Event) {
            Object.assign(
                this,
                changeBackgroundAlpha(this, (event.currentTarget as HTMLInputElement).checked),
            );
        },
        asset: undefined as string | undefined,
        media: defaultMedia(),
        appearance: defaultAppearance(),
        appearanceEnabled: false,
        action: { kind: 'none' } as Action,
        defaultAction: { kind: 'none' } as Action,
        pluginAppearance: undefined as PluginAppearance | undefined,
        pluginElements: [] as DisplayElement[],
        pluginStates: [] as DisplayState[],
        pluginAvailability: 'off',
        pluginPreviewState: 'live',
        pluginIconState: 'default',
        selectedElement: '',
        previewUnsubscribe: undefined as (() => void) | undefined,
        previewInFlight: false,
        previewPending: false,
        previewLastAt: 0,
        // Report whether this appearance belongs to a plugin display
        get isPluginDisplay() {
            return this.action.kind === 'plugin';
        },
        // Resolve the stable identifier of a plugin display element
        elementId(element: DisplayElement) {
            return JSON.stringify([element.style.layout, element.style.key, element.style.kind]);
        },
        // Find the selected plugin display descriptor
        get selectedDescriptor(): DisplayElement | undefined {
            return this.pluginElements.find((e) => this.elementId(e) === this.selectedElement);
        },
        // Resolve the selected element’s editable style
        get editableElement(): ElementStyle | undefined {
            const source = this.selectedDescriptor?.style;
            const compatible =
                this.action.kind === 'plugin' &&
                this.pluginAppearance?.plugin_id === this.action.plugin_id &&
                this.pluginAppearance.action_id === this.action.action_id;
            return (
                source &&
                ((compatible
                    ? this.pluginAppearance?.elements.find(
                          (s) =>
                              s.key === source.key &&
                              s.layout === source.layout &&
                              s.kind === source.kind,
                      )
                    : undefined) ||
                    source)
            );
        },
        // Report whether the selected element supports text styling
        get elementIsText() {
            return this.editableElement?.kind === 'text';
        },
        // Report whether the selected element supports image styling
        get elementIsImage() {
            return this.editableElement?.kind === 'pixmap';
        },
        // Explain the current plugin display availability
        get pluginStatus() {
            if (this.placeholder)
                return 'Layout preview with placeholders. Matching live values arrive automatically.';
            if (this.pluginAvailability === 'live')
                return 'Live plugin data. Appearance changes stay in this draft.';
            if (this.pluginAvailability === 'image_only')
                return 'This plugin supplies an image. Text inside that image cannot be styled separately.';
            if (this.pluginAvailability === 'inactive')
                return 'Plugin inactive. Save and enable this binding to receive live display data.';
            return 'Waiting for matching plugin display data. New or changed actions need to be saved first.';
        },
        // Create or update an override for the selected display element
        styleElement() {
            const source = this.editableElement;
            if (!source || this.action.kind !== 'plugin') return;
            if (
                !this.pluginAppearance ||
                this.pluginAppearance.plugin_id !== this.action.plugin_id ||
                this.pluginAppearance.action_id !== this.action.action_id
            )
                this.pluginAppearance = {
                    plugin_id: this.action.plugin_id,
                    action_id: this.action.action_id,
                    elements: [],
                };
            const copy = cloneJson(source) as ElementStyle;
            const index = this.pluginAppearance.elements.findIndex(
                (s) => s.key === copy.key && s.layout === copy.layout && s.kind === copy.kind,
            );
            if (index < 0) this.pluginAppearance.elements.push(copy);
            else this.pluginAppearance.elements[index] = copy;
            this.requestAppearance();
        },
        // Remove the selected element’s style override
        resetElement() {
            const source = this.selectedDescriptor?.style;
            if (source && this.pluginAppearance)
                this.pluginAppearance.elements = this.pluginAppearance.elements.filter(
                    (s) =>
                        !(
                            s.key === source.key &&
                            s.layout === source.layout &&
                            s.kind === source.kind
                        ),
                );
            if (!this.pluginAppearance?.elements.length) this.pluginAppearance = undefined;
            this.requestAppearance();
        },
        // Restore the plugin display’s default styling
        resetPluginDisplay() {
            this.pluginAppearance = undefined;
            this.requestAppearance();
        },
        // Resolve the icon asset used by the selected plugin element
        get pluginIconAsset(): IconAsset | undefined {
            const style = this.editableElement;
            return this.pluginIconState === 'default'
                ? style?.icon_asset
                : style?.state_icons[this.pluginIconState];
        },
        // Create the selected plugin element’s icon preview
        get pluginIconImage() {
            return this.pluginIconAsset
                ? iconImage(this.pluginIconAsset, this.editableElement?.foreground || '#ffffff')
                : '';
        },
        // Open icon selection for the selected plugin element
        openPluginIcons() {
            this.iconTarget = 'plugin';
            this.iconContext = crypto.randomUUID();
            window.dispatchEvent(
                new CustomEvent('open-icon-selector', {
                    detail: {
                        icon: this.pluginIconAsset?.id || '',
                        asset: this.pluginIconAsset,
                        context: this.iconContext,
                    },
                }),
            );
        },
        // Apply the chosen icon to the selected plugin element
        usePluginIcon() {
            const style = this.editableElement;
            if (!style) return;
            if (this.pluginIconState === 'default') delete style.icon_asset;
            else delete style.state_icons[this.pluginIconState];
            this.styleElement();
        },
        feedback: 'off',
        disabled: false,
        iconContext: '',
        iconTarget: 'initial' as 'initial' | 'toggled' | 'plugin' | 'canvas',
        previewState: 'initial',
        selectedStyle: '',
        styleName: '',
        livePreviewOwner: 'appearance:' + crypto.randomUUID(),
        // Explain why hardware appearance preview is unavailable
        get dockPreviewReason() {
            return (
                dockPreviewReason() || (!this.hasDisplay ? 'This control has no live output.' : '')
            );
        },
        previewImage: '',
        textWarnings: [] as string[],
        previewError: '',
        previewGeneration: 0,
        previewTimer: undefined as ReturnType<typeof setTimeout> | undefined,
        context: '',
        baseSignature: '',
        opened: false,
        closeBaseline: '',
        unregisterCloseOwner: undefined as (() => void) | undefined,
        previewRegistration: undefined as Promise<void> | undefined,
        // Capture the editable appearance fields for change detection
        editableSignature() {
            return JSON.stringify([
                this.title,
                this.liveLabel,
                this.color,
                this.customBackground,
                this.backgroundSource,
                this.asset,
                this.media,
                this.appearance,
                this.appearanceEnabled,
                this.feedback,
                this.composition,
                this.pluginAppearance,
                this.styleName,
            ]);
        },
        previewVisibility: undefined as (() => void) | undefined,
        // Check that the editor still owns the active binding context
        get contextCurrent() {
            return (
                !this.opened ||
                (this.context === app().bindingAddress() &&
                    JSON.stringify(app().binding || null) === this.baseSignature)
            );
        },
        // Connect appearance draft tracking, watchers and preview lifecycle
        init() {
            this.unregisterCloseOwner = registerCloseDraft(this, ['saveStyle', 'deleteStyle'], {
                name: 'Binding appearance',
                // Report whether the appearance draft has unsaved changes
                state: () =>
                    this.opened && this.editableSignature() !== this.closeBaseline
                        ? 'dirty'
                        : 'clean',
                // Discard the appearance draft through normal cancellation
                discard: () => this.cancel(),
                // Release preview resources through normal cancellation
                release: () => this.cancel(),
            });
            // Pause or resume appearance preview with document visibility
            this.previewVisibility = () => {
                if (document.hidden) {
                    this.previewGeneration++;
                    clearTimeout(this.previewTimer);
                    this.previewTimer = undefined;
                    this.previewPending = false;
                } else if (this.opened) this.requestAppearance();
            };
            document.addEventListener('visibilitychange', this.previewVisibility);
            for (const key of [
                'appearance',
                'appearanceEnabled',
                'composition',
                'asset',
                'media',
                'feedback',
                'color',
                'customBackground',
                'backgroundSource',
                'title',
                'previewState',
                'pluginAppearance',
                'pluginPreviewState',
                '$store.theme.deviceAppearance',
                '$store.theme.systemDark',
                '$store.theme.customPalettes',
            ])
                // Request a fresh preview when an appearance field changes
                this.$watch(key, () => {
                    if (this.opened) this.requestAppearance();
                });
            this.$watch('selectedLayerId', () => this.resetLayerVault());
            this.$watch('$store.app.controlId', () => this.cancel());
            // Cancel the editor when its binding context becomes stale
            this.$watch('contextCurrent', () => {
                if (!this.contextCurrent) this.cancel();
            });
            this.$watch('$store.app.route', () => this.cancel());
        },
        // Open a scoped appearance draft for the requested binding
        open(event: CustomEvent<{ binding: Binding; context: string; defaultAction?: Action }>) {
            if (this.opened) this.cancel();
            const b = cloneJson(event.detail.binding) as Binding;
            this.action = b.action;
            this.defaultAction = cloneJson(event.detail.defaultAction || b.action);
            this.composition = b.composition;
            this.canvasSources = [];
            this.selectedLayerId = '';
            this.addSourceId = '';
            this.pluginAppearance = b.plugin_appearance;
            this.pluginElements = [];
            this.pluginStates = [];
            this.selectedElement = '';
            this.pluginAvailability = 'waiting';
            this.pluginPreviewState = 'live';
            this.pluginIconState = 'default';
            this.previewGeneration++;
            this.title = b.title;
            this.liveLabel = cloneJson(b.live_label || []);
            this.labelField = 'literal';
            this.labelValue = '';
            this.color = b.color;
            this.customBackground = Boolean(b.custom_background);
            this.backgroundSource = b.background_source || 'custom';
            this.asset = b.asset;
            this.media = b.media;
            this.appearance = { ...defaultAppearance(), ...b.appearance };
            if (!b.appearance && app().isProtectedControl(app().controlId)) {
                this.appearance.position = 'center';
                this.appearance.align = 'center';
                this.appearance.size = 18;
            }
            this.appearanceEnabled = !!b.appearance;
            this.feedback = b.feedback || 'off';
            this.disabled = b.disabled || false;
            this.context = event.detail.context;
            this.baseSignature = JSON.stringify(app().binding || null);
            this.iconContext = crypto.randomUUID();
            this.iconTarget = 'initial';
            this.previewState = 'initial';
            this.selectedStyle = '';
            this.styleName = '';
            this.previewImage = '';
            this.previewError = '';
            this.opened = true;
            this.closeBaseline = this.editableSignature();
            void this.loadFonts();
            const dialog = this.$refs.dialog as HTMLDialogElement;
            dialog.showModal();
            const body = dialog.querySelector?.<HTMLElement>('.feature-body');
            if (body) body.scrollTop = 0;
            this.requestAppearance();
            const generation = this.previewGeneration;
            this.previewRegistration = app()
                // Refresh appearance preview after relevant runtime events
                .bridge.subscribe((event) => {
                    if (
                        this.opened &&
                        (!event.device_id || event.device_id === app().deviceId) &&
                        [
                            'plugin-feedback',
                            'display-theme',
                            'live-feedback',
                            'plugin-health',
                            'plugin-lifecycle',
                            'plugin-runtime',
                            'plugins',
                            'fonts-updated',
                        ].includes(event.kind)
                    )
                        this.requestAppearance();
                })
                // Retain the runtime subscription only for the active editor session
                .then(async (unsubscribe) => {
                    if (this.opened && generation === this.previewGeneration)
                        this.previewUnsubscribe = unsubscribe;
                    else
                        await trackCloseRelease(
                            'appearance-subscription:' + this.context + ':' + generation,
                            // Release the runtime subscription during editor cleanup
                            async () => {
                                await unsubscribe();
                            },
                        );
                })
                // Report failure to subscribe to appearance preview updates
                .catch((error) => {
                    if (windowCloseOwners.closing) throw error;
                });
        },
        // Cancel the appearance draft and release its preview resources
        cancel() {
            const dockRelease = clearLiveActionPreview(this.livePreviewOwner);
            this.fontRequest++;
            this.fontsLoading = false;
            this.opened = false;
            this.pluginElements = [];
            this.pluginStates = [];
            this.selectedElement = '';
            const unsubscribe = this.previewUnsubscribe;
            const unsubscription = unsubscribe
                ? trackCloseRelease(
                      'appearance-subscription:' + this.context + ':' + this.previewGeneration,
                      // Release the editor’s outstanding preview and media resources
                      async () => {
                          await unsubscribe();
                      },
                  )
                : undefined;
            this.previewUnsubscribe = undefined;
            this.previewPending = false;
            this.previewImage = '';
            this.previewError = '';
            this.canvasSources = [];
            this.composition = undefined;
            this.pluginAppearance = undefined;
            this.drag = null;
            this.appearance = defaultAppearance();
            window.dispatchEvent(new CustomEvent('close-icon-selector'));
            this.previewGeneration++;
            clearTimeout(this.previewTimer);
            this.previewTimer = undefined;
            (this.$refs.dialog as HTMLDialogElement)?.close();
            const completion = Promise.all([
                dockRelease,
                unsubscription,
                this.previewRegistration,
                // Resolve cleanup after all preview releases complete
            ]).then(() => {});
            // Normal UI cancellation is synchronous; the close coordinator awaits this result
            void completion.catch(() => {});
            return completion;
        },
        // Restore editable appearance fields to their default values
        resetAppearance() {
            this.liveLabel = [];
            Object.assign(
                this,
                actionAppearanceDefaults(this.defaultAction, app().snapshot.plugins),
            );
            if (app().isProtectedControl(app().controlId)) {
                const binding = keyboardNavigationBindings()[app().controlId]!;
                this.title = binding.title;
                this.color = binding.color;
                this.backgroundSource = 'palette';
                this.feedback = 'off';
                this.appearance.position = 'center';
                this.appearance.align = 'center';
                this.appearance.size = 18;
                this.appearanceEnabled = false;
            }
            this.canvasSources = [];
            this.selectedLayerId = '';
            this.addSourceId = '';
            this.selectedElement = '';
            this.pluginElements = [];
            this.pluginStates = [];
            this.pluginPreviewState = 'live';
            this.pluginIconState = 'default';
            this.previewState = 'initial';
            this.iconTarget = 'initial';
            this.iconContext = crypto.randomUUID();
            this.selectedStyle = '';
            this.styleName = '';
            this.drag = null;
            this.previewImage = '';
            this.previewError = '';
            window.dispatchEvent(new CustomEvent('close-icon-selector'));
            this.requestAppearance();
        },
        // Apply the validated appearance draft to its owning binding editor
        apply() {
            if (!this.canApply) return;
            if (this.context !== app().bindingAddress()) {
                this.cancel();
                return;
            }
            window.dispatchEvent(
                new CustomEvent('apply-appearance', {
                    detail: {
                        context: this.context,
                        title: this.title,
                        liveLabel: cloneJson(this.liveLabel),
                        color: this.color,
                        customBackground: this.customBackground,
                        backgroundSource: this.backgroundSource,
                        asset: this.asset,
                        media: cloneJson(this.media),
                        appearance: cloneJson(this.appearance),
                        appearanceEnabled: this.appearanceEnabled,
                        feedback: this.feedback,
                        composition: this.composition ? clone(this.composition) : undefined,
                        pluginAppearance: this.pluginAppearance
                            ? cloneJson(this.pluginAppearance)
                            : undefined,
                    },
                }),
            );
            this.cancel();
        },
        // Dispose the appearance editor and its lifecycle registrations
        destroy() {
            this.unregisterCloseOwner?.();
            this.cancel();
            if (this.previewVisibility)
                document.removeEventListener('visibilitychange', this.previewVisibility);
        },
        // Render the currently selected action icon
        get selectedIconImage() {
            return this.appearance.icon_asset
                ? iconImage(this.appearance.icon_asset, this.appearance.foreground)
                : this.appearance.icon
                  ? import.meta.env.BASE_URL + 'appearance/icons/' + this.appearance.icon + '.svg'
                  : '';
        },
        // Report whether the action’s alternate icon is enabled
        get toggleEnabled() {
            return Boolean(this.appearance.toggle_icon);
        },
        // Explain invalid plugin display style overrides
        get pluginStyleError() {
            const invalid = this.pluginAppearance?.elements.some(
                (style) =>
                    !style.rect.every(Number.isFinite) ||
                    style.rect.some((n) => n < 0 || n > 100) ||
                    style.rect[2] <= 0 ||
                    style.rect[3] <= 0 ||
                    !Number.isFinite(style.size) ||
                    style.size < 1 ||
                    style.size > 100,
            );
            return invalid
                ? 'Use valid element bounds (0–100%) and font sizes (1–100%). Width and height must be greater than zero.'
                : '';
        },
        // Check whether the current appearance draft can be applied
        get canApply() {
            return (
                !this.canvasError &&
                !this.pluginStyleError &&
                (!this.appearanceEnabled ||
                    !this.toggleEnabled ||
                    Boolean(this.appearance.icon && this.appearance.toggle_icon?.icon))
            );
        },
        // Toggle the alternate action icon configuration
        changeIconToggle(event: Event) {
            if ((event.currentTarget as HTMLInputElement).checked)
                this.appearance.toggle_icon = { icon: '' };
            else {
                delete this.appearance.toggle_icon;
                this.previewState = 'initial';
            }
        },
        // Render the alternate action icon preview
        get toggledIconImage() {
            const icon = this.appearance.toggle_icon;
            return icon?.icon_asset
                ? iconImage(icon.icon_asset, this.appearance.foreground)
                : icon?.icon
                  ? import.meta.env.BASE_URL + 'appearance/icons/' + icon.icon + '.svg'
                  : '';
        },
        // Expose the alternate icon’s display name
        get toggledIconName() {
            return this.appearance.toggle_icon?.icon || '';
        },
        // Expose attribution for the alternate icon
        get toggledIconAttribution() {
            const asset = this.appearance.toggle_icon?.icon_asset;
            return asset
                ? [asset.collection, asset.author, asset.license].filter(Boolean).join(' · ')
                : '';
        },
        // Expose attribution for the primary icon
        get iconAttribution() {
            const asset = this.appearance.icon_asset;
            return asset ? [asset.collection, asset.author].filter(Boolean).join(' · ') : '';
        },
        // Expose the primary icon’s license name
        get iconLicense() {
            return this.appearance.icon_asset?.license || 'License not provided';
        },
        // Expose the primary icon’s license URL
        get iconLicenseUrl() {
            const url = this.appearance.icon_asset?.license_url || '';
            try {
                return new URL(url).protocol === 'https:' ? url : '';
            } catch {
                return '';
            }
        },
        // Open icon selection for the primary action icon
        openIcons() {
            this.iconTarget = 'initial';
            this.chooseIcon();
        },
        // Open icon selection for the alternate action icon
        openToggleIcons() {
            this.iconTarget = 'toggled';
            this.chooseIcon();
        },
        // Open the icon selector for the active icon target
        chooseIcon() {
            this.iconContext = crypto.randomUUID();
            const icon =
                this.iconTarget === 'toggled' ? this.appearance.toggle_icon : this.appearance;
            window.dispatchEvent(
                new CustomEvent('open-icon-selector', {
                    detail: {
                        icon: icon?.icon || '',
                        asset: icon?.icon_asset,
                        context: this.iconContext,
                    },
                }),
            );
        },
        // Accept an icon selection only for the matching editor context
        iconChosen(event: CustomEvent<{ asset: IconAsset; context: string }>) {
            if (!this.opened || event.detail.context !== this.iconContext) return;
            if (this.iconTarget === 'canvas') {
                this.ensureComposition();
                if (this.selectedLayer) {
                    if (this.pluginIconState === 'default')
                        this.selectedLayer.style.icon_asset = event.detail.asset;
                    else
                        this.selectedLayer.style.state_icons[this.pluginIconState] =
                            event.detail.asset;
                }
                this.requestAppearance();
                return;
            }
            if (this.iconTarget === 'plugin') {
                const style = this.editableElement;
                if (!style) return;
                if (this.pluginIconState === 'default') style.icon_asset = event.detail.asset;
                else style.state_icons[this.pluginIconState] = event.detail.asset;
                this.styleElement();
                return;
            }
            const icon =
                this.iconTarget === 'toggled' ? this.appearance.toggle_icon : this.appearance;
            if (!icon) return;
            icon.icon = event.detail.asset.id;
            icon.icon_asset = cloneJson(event.detail.asset);
            this.appearanceEnabled = true;
            this.requestAppearance();
        },
        // Remove the primary action icon override
        removeIcon() {
            this.appearance.icon = '';
            delete this.appearance.icon_asset;
            this.requestAppearance();
        },
        // Remove the alternate action icon override
        removeToggleIcon() {
            if (this.appearance.toggle_icon) this.appearance.toggle_icon = { icon: '' };
            this.previewState = 'initial';
        },
        // Expose saved appearance styles for selection
        get styles() {
            return app().snapshot.styles || [];
        },
        // Report whether the target binding has a display surface
        get hasDisplay() {
            return Boolean(app().control?.image_width || app().control?.feedback_surface);
        },
        // Project the draft appearance into a preview-only binding
        previewBinding(): Binding {
            const binding: Binding = {
                live_label: this.liveLabel.length ? cloneJson(this.liveLabel) : undefined,
                title: this.title,
                color: this.color,
                custom_background: this.customBackground || undefined,
                background_source: this.backgroundSource === 'palette' ? 'palette' : undefined,
                action: cloneJson(this.action),
                asset: this.asset,
                media: cloneJson(this.media),
                appearance: this.appearanceEnabled ? cloneJson(this.appearance) : undefined,
                composition: this.composition ? clone(this.composition) : undefined,
                plugin_appearance: this.pluginAppearance
                    ? cloneJson(this.pluginAppearance)
                    : undefined,
                feedback: this.feedback,
                disabled: this.disabled,
            };
            const appearance = binding.appearance;
            if (appearance?.toggle_icon) {
                if (this.previewState === 'toggled' && appearance.toggle_icon.icon) {
                    appearance.icon = appearance.toggle_icon.icon;
                    appearance.icon_asset = appearance.toggle_icon.icon_asset;
                }
                delete appearance.toggle_icon;
            }
            return binding;
        },
        vaultPresentationChanged() {
            this.resetLayerVault();
            if (!this.opened) return;
            const template = '${vault.';
            if (
                !this.title.includes(template) &&
                !this.liveLabel.some((segment) =>
                    (segment.kind === 'literal' ? segment.text : segment.fallback).includes(
                        template,
                    ),
                ) &&
                !this.composition?.layers.some(
                    (layer) => layer.source.kind === 'text' && layer.text.includes(template),
                )
            )
                return;
            this.previewGeneration++;
            this.previewImage = '';
            this.textWarnings = [];
            clearTimeout(this.previewTimer);
            this.previewTimer = undefined;
            this.requestAppearance();
        },
        // Schedule an appearance preview for the current draft
        requestAppearance() {
            if (!this.opened || !this.contextCurrent || !this.hasDisplay || document.hidden) return;
            setLiveActionPreview(this.livePreviewOwner, this.previewBinding(), {
                priority: 10,
                previewState:
                    this.pluginPreviewState === 'live' ? null : Number(this.pluginPreviewState),
            });
            this.previewPending = true;
            if (this.previewInFlight || this.previewTimer) return;
            const generation = this.previewGeneration;
            this.previewTimer = setTimeout(
                // Render the current preview while rejecting stale results
                async () => {
                    this.previewTimer = undefined;
                    if (!this.opened || document.hidden || generation !== this.previewGeneration)
                        return;
                    this.previewPending = false;
                    this.previewInFlight = true;
                    this.previewLastAt = Date.now();
                    const binding = this.previewBinding();
                    const signature = JSON.stringify([binding, this.pluginPreviewState]);
                    try {
                        const preview = await app().bridge.command('preview_binding_details', {
                            deviceId: app().deviceId,
                            controlId: app().controlId,
                            binding,
                            previewState:
                                this.pluginPreviewState === 'live'
                                    ? null
                                    : Number(this.pluginPreviewState),
                        });
                        if (
                            this.opened &&
                            !document.hidden &&
                            generation === this.previewGeneration &&
                            signature ===
                                JSON.stringify([this.previewBinding(), this.pluginPreviewState])
                        ) {
                            this.canvasSources = preview.sources || [];
                            this.placeholder = preview.placeholder === true;
                            if (!this.selectedLayerId)
                                this.selectedLayerId = this.canvasLayers[0]?.id || '';
                            if (!this.addSourceId)
                                this.addSourceId = this.canvasSources[0]
                                    ? sourceId(this.canvasSources[0].source)
                                    : '';
                            this.previewImage = preview.image;
                            this.textWarnings = preview.text_warnings || [];
                            this.previewError = '';
                            this.pluginElements = preview.elements;
                            this.pluginStates = preview.states;
                            this.pluginAvailability = preview.availability;
                            if (
                                !preview.elements.some(
                                    (e) => this.elementId(e) === this.selectedElement,
                                )
                            )
                                this.selectedElement = preview.elements[0]
                                    ? this.elementId(preview.elements[0])
                                    : '';
                            if (
                                this.pluginPreviewState !== 'live' &&
                                !preview.states.some(
                                    (s) => String(s.index) === this.pluginPreviewState,
                                )
                            )
                                this.pluginPreviewState = 'live';
                        }
                    } catch (e) {
                        if (this.opened && generation === this.previewGeneration)
                            this.previewError = String(e);
                    } finally {
                        this.previewInFlight = false;
                        if (this.previewPending) this.requestAppearance();
                    }
                },
                Math.max(0, 75 - (Date.now() - this.previewLastAt)),
            );
        },
        // Apply a saved appearance style to the editable draft
        applyStyle() {
            const style = this.styles.find((s) => s.id === this.selectedStyle);
            if (style) {
                this.composition = style.composition ? clone(style.composition) : undefined;
                this.appearance = cloneJson(style.appearance);
                this.pluginAppearance = style.plugin_appearance
                    ? cloneJson(style.plugin_appearance)
                    : undefined;
                this.color = style.color || this.color;
                this.customBackground = Boolean(style.color);
                this.backgroundSource = style.background_source || 'custom';
                this.appearanceEnabled = style.overlay_enabled !== false;
                this.requestAppearance();
            }
        },
        // Save the current appearance as a reusable style
        async saveStyle() {
            if (!this.canApply) return;
            try {
                await app().edit(
                    [
                        {
                            kind: 'save_style',
                            style: {
                                id: crypto.randomUUID(),
                                name: this.styleName.trim() || 'My style',
                                color: this.color,
                                background_source:
                                    this.backgroundSource === 'palette' ? 'palette' : undefined,
                                appearance: cloneJson(this.appearance),
                                overlay_enabled: this.appearanceEnabled,
                                composition: this.composition ? clone(this.composition) : undefined,
                                plugin_appearance: this.pluginAppearance
                                    ? cloneJson(this.pluginAppearance)
                                    : undefined,
                            },
                        },
                    ],
                    'Save appearance preset',
                );
                this.styleName = '';
            } catch {}
        },
        // Delete the selected reusable appearance style
        async deleteStyle() {
            if (!this.selectedStyle) return;
            try {
                await app().edit(
                    [{ kind: 'delete_style', id: this.selectedStyle }],
                    'Delete appearance preset',
                );
                this.selectedStyle = '';
            } catch {}
        },
    }));
}
