// Browser-only deterministic visual fixtures. Native output uses the Rust compositor
import type {
    Binding,
    BindingPreview,
    DisplayElement,
    DisplayState,
    ElementStyle,
} from '../generated/runtime';
import { iconImage } from './icon-svg';
import { cssFontFamily } from './display-fonts';
export interface PreviewItem {
    key: string;
    type: string;
    label?: string;
    rect: number[];
    value?: unknown;
    font?: { size?: number; fit?: boolean };
    color?: string;
    range?: { min?: number; max?: number };
}
export interface PreviewVisual {
    plugin_id: string;
    action_id: string;
    title?: string;
    image?: string;
    state?: number;
    states?: DisplayState[];
    feedback?: Record<string, unknown>;
    layout?: {
        id?: string;
        items: PreviewItem[];
        streamDockManager?: {
            text_groups?: {
                key: string;
                label: string;
                source: string;
                keys: string[];
                rect: number[];
            }[];
        };
    };
}
// Escape text before it is used by plugin display preview
const escape = (value: unknown) =>
    String(value ?? '')
        .replaceAll('&', '&amp;')
        .replaceAll('<', '&lt;')
        .replaceAll('>', '&gt;')
        .replaceAll('"', '&quot;');
// Merge layout items with feedback values into renderable preview elements
export function previewElements(
    visual: PreviewVisual,
): { element: DisplayElement; value: unknown; properties: PreviewItem }[] {
    // Convert each entry into the representation required by plugin display preview
    let items: PreviewItem[] = visual.layout?.items.map((item) => {
        const value = visual.feedback?.[item.key];
        return {
            ...item,
            ...(value && typeof value === 'object' && !Array.isArray(value)
                ? value
                : { value: value ?? item.value }),
        };
    }) || [
        ...(visual.image
            ? [
                  {
                      key: 'image',
                      label: 'Plugin image',
                      type: 'pixmap',
                      rect: [0, 0, 200, 100],
                      value: visual.image,
                  },
              ]
            : []),
        ...(visual.title || !visual.image
            ? [
                  {
                      key: 'title',
                      label: 'Plugin title',
                      type: 'text',
                      rect: [0, 0, 200, 100],
                      value: visual.title || '',
                  },
              ]
            : []),
    ];
    for (const group of visual.layout?.streamDockManager?.text_groups || []) {
        items = items.filter((item) => !group.keys.includes(item.key));
        items.push({
            key: group.key,
            label: group.label,
            type: 'text',
            rect: group.rect,
            value: visual.title || '',
            font: { size: 8 },
        });
    }
    return items.map((item) => ({
        value: item.value,
        properties: item,
        element: {
            label: item.label || item.key,
            value: item.type === 'pixmap' ? 'Live plugin image' : String(item.value ?? ''),
            style: {
                layout: visual.layout?.id || (visual.layout ? '$layout' : '$title-image'),
                key: item.key,
                kind: item.type,
                rect: item.rect.map((v, i) => v / (i % 2 === 0 ? 2 : 1)) as [
                    number,
                    number,
                    number,
                    number,
                ],
                visible: item.rect[2] > 0 && item.rect[3] > 0,
                font: 'sans',
                font_weight: 400,
                font_italic: false,
                size: item.font?.size || 16,
                foreground: item.color || '#edf2fc',
                align: 'center',
                vertical_align: 'center',
                wrap: true,
                state_icons: {},
            },
        },
    }));
}
// Compose plugin display markup and details from the current state and appearance overrides
export function renderPreviewDisplay(
    visual: PreviewVisual,
    binding: Binding,
    width: number,
    height: number,
    previewState?: number,
): { body: string; details: Omit<BindingPreview, 'image' | 'availability'> } {
    const fields = previewElements(visual);
    const state = previewState ?? visual.state;
    const applicable =
        binding.plugin_appearance?.plugin_id === visual.plugin_id &&
        binding.plugin_appearance?.action_id === visual.action_id;
    const body = fields
        // Convert each entry into the representation required by plugin display preview
        .map(({ element, value, properties }, index) => {
            const style: ElementStyle =
                (applicable &&
                    binding.plugin_appearance?.elements.find(
                        (s) =>
                            s.key === element.style.key &&
                            s.kind === element.style.kind &&
                            s.layout === element.style.layout,
                    )) ||
                element.style;
            if (!style.visible) return '';
            const [x, y, w, h] = style.rect.map(
                (v, i) => (v / 100) * (i % 2 === 0 ? width : height),
            );
            let content = '';
            if (style.kind === 'pixmap') {
                const asset =
                    (state === undefined ? undefined : style.state_icons[String(state)]) ||
                    style.icon_asset;
                const uri = asset ? iconImage(asset, style.foreground) : String(value || '');
                content = `<image x="${x}" y="${y}" width="${w}" height="${h}" preserveAspectRatio="xMidYMid meet" href="${escape(uri)}"/>`;
            } else if (style.kind === 'text') {
                let size = (style.size * height) / 100;
                const fitted = properties.font?.fit && style === element.style;
                if (fitted) {
                    const context = document.createElement('canvas').getContext('2d')!;
                    context.font = `${size}px ${cssFontFamily(style.font)}`;
                    const paragraphs = String(value ?? '').split('\n');
                    const longest = Math.max(
                        1,
                        ...paragraphs.map((line) =>
                            Array.from(line).reduce(
                                (sum, char) => sum + context.measureText(char).width,
                                0,
                            ),
                        ),
                    );
                    size = Math.max(
                        1,
                        size *
                            Math.min(
                                1,
                                (w * 0.96) / longest,
                                (h * 0.96) / (size * 1.2 * paragraphs.length),
                            ),
                    );
                }
                const columns = Math.max(1, Math.floor(w / (size * 0.6)));
                const lines = String(value ?? '')
                    .split('\n')
                    .flatMap((line) =>
                        style.wrap && !fitted
                            ? line.match(new RegExp(`.{1,${columns}}`, 'gu')) || ['']
                            : [line],
                    )
                    .slice(0, Math.max(1, Math.floor(h / (size * 1.2))));
                const top =
                    y +
                    (style.vertical_align === 'bottom'
                        ? Math.max(0, h - lines.length * size * 1.2)
                        : style.vertical_align === 'center'
                          ? Math.max(0, (h - lines.length * size * 1.2) / 2)
                          : 0);
                const tx = x + (style.align === 'right' ? w : style.align === 'center' ? w / 2 : 0),
                    anchor =
                        style.align === 'left'
                            ? 'start'
                            : style.align === 'right'
                              ? 'end'
                              : 'middle';
                content = lines
                    .map(
                        (line, i) =>
                            `<text font-weight="${style.font_weight === 700 ? 700 : 400}" font-style="${style.font_italic ? 'italic' : 'normal'}" x="${tx}" y="${top + size + i * size * 1.2}" text-anchor="${anchor}" font-family="${escape(cssFontFamily(style.font))}" font-size="${size}" fill="${escape(style.foreground)}">${escape(line)}</text>`,
                    )
                    .join('');
            } else {
                const min = Number(properties.range?.min ?? 0),
                    max = Number(properties.range?.max ?? 100);
                const ratio =
                    Number.isFinite(Number(value)) && max > min
                        ? (Number(value) - min) / (max - min)
                        : 0;
                content = `<rect x="${x}" y="${y}" width="${w}" height="${h}" fill="#303b4d"/><rect x="${x}" y="${y}" width="${w * Math.min(1, Math.max(0, ratio))}" height="${h}" fill="#82aef0"/>`;
            }
            return `<defs><clipPath id="field-${index}"><rect x="${x}" y="${y}" width="${w}" height="${h}"/></clipPath></defs><g clip-path="url(#field-${index})">${content}</g>`;
        })
        .join('');
    return {
        body,
        details: {
            elements: fields.map((f) => f.element),
            states: visual.states || [],
            current_state: visual.state ?? null,
        },
    };
}
