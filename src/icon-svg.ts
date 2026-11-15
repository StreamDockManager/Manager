import type { IconAsset } from '../generated/runtime';

// Match the native icon loader: remove a leading canvas-sized SVG backplate,
// keeping foreground rectangles, attribution and the saved source intact
export function transparentIconSvg(source: string): string {
    if (!source.includes('<rect')) return source;
    const document = new DOMParser().parseFromString(source, 'image/svg+xml');
    const root = document.documentElement;
    if (root.localName !== 'svg' || document.querySelector('parsererror')) return source;
    // Parse an SVG numeric dimension with an optional pixel suffix
    const number = (value: string | null): number => {
        const text = value?.trim().replace(/px$/, '');
        return text ? Number(text) : NaN;
    };
    const bounds = root.hasAttribute('viewBox')
        ? root
              .getAttribute('viewBox')!
              .trim()
              .split(/[\s,]+/)
              .map(Number)
        : [0, 0, number(root.getAttribute('width')), number(root.getAttribute('height'))];
    if (bounds.length !== 4 || !bounds.every(Number.isFinite) || bounds[2] <= 0 || bounds[3] <= 0)
        return source;
    const shapes = [...root.children].filter(
        (node) => !['defs', 'title', 'desc', 'metadata', 'style'].includes(node.localName),
    );
    const rect = shapes[0];
    if (shapes.length < 2 || rect.localName !== 'rect' || rect.hasAttribute('transform'))
        return source;
    // Resolve a rectangle dimension including a full-canvas percentage
    const dimension = (name: string, full: number) =>
        rect.getAttribute(name)?.trim() === '100%' ? full : number(rect.getAttribute(name));
    const actual = [
        number(rect.getAttribute('x') ?? '0'),
        number(rect.getAttribute('y') ?? '0'),
        dimension('width', bounds[2]),
        dimension('height', bounds[3]),
    ];
    if (!actual.every((value, index) => Math.abs(value - bounds[index]) < 0.000001)) return source;
    rect.remove();
    return new XMLSerializer().serializeToString(root);
}

// Encode the transparent icon artwork as a color-resolved SVG data URL
export const iconImage = (asset: IconAsset, color = '#ffffff') =>
    'data:image/svg+xml;charset=utf-8,' +
    encodeURIComponent(transparentIconSvg(asset.svg).replaceAll('currentColor', color));
