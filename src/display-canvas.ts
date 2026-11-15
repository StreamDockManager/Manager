import { cloneJson } from './json-clone';
import type {
    DisplayComposition,
    DisplayLayer,
    DisplaySourceDescriptor,
    ElementStyle,
} from '../generated/runtime';
export const clone = cloneJson;
// Create default styling for a new composition layer
export function layerStyle(id: string, kind = 'text'): ElementStyle {
    return {
        layout: '$composition',
        key: id,
        kind,
        rect: [10, 35, 80, 25],
        visible: true,
        font: 'sans',
        size: 18,
        foreground: '#ffffff',
        align: 'center',
        vertical_align: 'center',
        wrap: true,
        state_icons: {},
    };
}
// Derive a stable identifier for a display source
export function sourceId(source: DisplaySourceDescriptor['source']) {
    return JSON.stringify([
        source.kind,
        source.plugin_id,
        source.action_id,
        source.layout,
        source.key,
        source.element_kind,
    ]);
}
// Create initial composition layers from available display sources
export function initialLayers(sources: DisplaySourceDescriptor[]): DisplayLayer[] {
    return sources
        .filter((s) => s.available && s.style.visible)
        .slice(0, 64)
        .map((s, index) => ({
            id: 'source-' + index,
            label: s.label,
            source: clone(s.source),
            style: clone(s.style),
            text: '',
        }));
}
// Validate composition dimensions and layer constraints
export function validateComposition(composition?: DisplayComposition): string {
    if (!composition) return '';
    if (composition.layers.length > 64) return 'Use at most 64 layers.';
    for (const layer of composition.layers) {
        const s = layer.style;
        if (
            !s.rect.every(Number.isFinite) ||
            s.rect.some((v) => v < 0 || v > 100) ||
            s.rect[2] <= 0 ||
            s.rect[3] <= 0 ||
            !Number.isFinite(s.size) ||
            s.size < 1 ||
            s.size > 100
        )
            return 'Use valid layer bounds (0–100%) and font sizes (1–100%).';
    }
    return '';
}

// Construct display sources for browser composition previews
export function previewSources(
    binding: import('../generated/runtime').Binding,
    visual?: import('./plugin-display-preview').PreviewVisual,
    elements: import('../generated/runtime').DisplayElement[] = [],
    placeholder = false,
    mediaImage = '',
): DisplaySourceDescriptor[] {
    // Create a preview source reference of the requested kind
    const source = (kind: string) => ({
        kind,
        plugin_id: '',
        action_id: '',
        layout: '',
        key: '',
        element_kind: '',
    });
    const a = binding.appearance;
    const uri = a?.icon_asset
        ? iconImage(a.icon_asset, a.foreground)
        : a?.icon
          ? import.meta.env.BASE_URL + 'appearance/icons/' + a.icon + '.svg'
          : '';
    const sources: DisplaySourceDescriptor[] = [
        {
            label: 'Background media',
            source: source('media'),
            style: { ...layerStyle('media', 'pixmap'), rect: [0, 0, 100, 100] },
            value: mediaImage,
            available: !!mediaImage,
            placeholder: false,
        },
        {
            label: 'Binding icon',
            source: source('icon'),
            style: { ...layerStyle('icon', 'pixmap'), rect: [29, 20, 42, 42] },
            value: uri,
            available: !!uri,
            placeholder: false,
        },
        {
            label: 'Binding title',
            source: source('title'),
            style: {
                ...layerStyle('title'),
                rect: [2, 74, 96, 24],
                visible: !visual || !!a?.show_title,
            },
            value: binding.title,
            available: !!binding.title,
            placeholder: false,
        },
        {
            label: 'Runtime feedback',
            source: source('feedback'),
            style: { ...layerStyle('feedback'), rect: [2, 76, 96, 22] },
            value: binding.feedback && binding.feedback !== 'off' ? '—' : '',
            available: !!binding.feedback && binding.feedback !== 'off',
            placeholder: true,
        },
    ];
    // Match the native preview's editable bounds to the current icon and label layout
    // Otherwise dragging a default-positioned layer first jumps to the generic layout
    if (a) {
        const title = sources.find((item) => item.source.kind === 'title')!.style;
        Object.assign(title, {
            font: a.font,
            size: a.size,
            foreground: a.foreground,
            align: a.align,
        });
        title.rect[1] = a.position === 'top' ? 2 : a.position === 'center' ? 38 : 74;
        const size = a.icon_size;
        sources.find((item) => item.source.kind === 'icon')!.style.rect = [
            (100 - size) / 2,
            a.icon_position === 'top'
                ? 2
                : a.icon_position === 'bottom'
                  ? 98 - size
                  : (100 - size) / 2,
            size,
            size,
        ];
    }
    if (visual)
        for (const { element, value, properties } of previewElements(visual)) {
            const style =
                elements.find(
                    (e) => e.style.key === element.style.key && e.style.kind === element.style.kind,
                )?.style || element.style;
            sources.push({
                label: element.label,
                source: {
                    kind: 'plugin',
                    plugin_id: visual.plugin_id,
                    action_id: visual.action_id,
                    layout: style.layout,
                    key: style.key,
                    element_kind: style.kind,
                },
                style: clone(style),
                value: value as import('../generated/runtime').JsonValue,
                available: true,
                placeholder,
                properties: clone(
                    properties,
                ) as unknown as import('../generated/runtime').JsonValue,
            });
        }
    return sources;
}
import { iconImage } from './icon-svg';
import { previewElements, renderPreviewDisplay } from './plugin-display-preview';
// Render a display composition into a browser preview image
export function renderCompositionPreview(
    binding: import('../generated/runtime').Binding,
    sources: DisplaySourceDescriptor[],
    width: number,
    height: number,
    state?: number,
): string {
    const layout = 'manager-composition-v1';
    const items: import('./plugin-display-preview').PreviewItem[] = [];
    const styles: ElementStyle[] = [];
    if (!binding.composition?.layers.some((layer) => layer.source.kind === 'media')) {
        const media = sources.find((s) => s.source.kind === 'media' && s.available);
        if (media)
            items.push({
                key: '$binding-media',
                type: 'pixmap',
                rect: [0, 0, 200, 100],
                value: media.value,
            });
    }
    for (const layer of binding.composition?.layers || []) {
        let value: unknown =
            layer.source.kind === 'text'
                ? layer.text
                : layer.source.kind === 'custom_icon'
                  ? ''
                  : sources.find(
                        (s) => sourceId(s.source) === sourceId(layer.source) && s.available,
                    )?.value;
        if (value === undefined) continue;
        const style = { ...clone(layer.style), layout, key: layer.id };
        const properties = sources.find(
            (s) => sourceId(s.source) === sourceId(layer.source),
        )?.properties;
        items.push({
            ...(properties && typeof properties === 'object' && !Array.isArray(properties)
                ? (clone(properties) as unknown as import('./plugin-display-preview').PreviewItem)
                : {}),
            key: layer.id,
            type: style.kind,
            rect: style.rect.map((v, i) => (i % 2 === 0 ? v * 2 : v)),
            value,
        });
        styles.push(style);
    }
    const rendered = renderPreviewDisplay(
        { plugin_id: '$manager', action_id: 'composition', state, layout: { id: layout, items } },
        {
            ...binding,
            composition: undefined,
            plugin_appearance: {
                plugin_id: '$manager',
                action_id: 'composition',
                elements: styles,
            },
        },
        width,
        height,
        state,
    );
    return (
        'data:image/svg+xml;charset=utf-8,' +
        encodeURIComponent(
            `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}"><rect width="100%" height="100%" fill="${binding.color}"/>${rendered.body}</svg>`,
        )
    );
}
