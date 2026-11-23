import type { JsonValue, VaultKind } from '../generated/runtime';
import { vaultLocationMap } from './vault-location-map';

const svgNamespace = 'http://www.w3.org/2000/svg';
const jsonBatch = 80;
const secretKinds = new Set<VaultKind>(['api-key', 'bearer-token', 'credential']);

function element<K extends keyof HTMLElementTagNameMap>(
    tag: K,
    className?: string,
    text?: string,
): HTMLElementTagNameMap[K] {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
}

function svg<K extends keyof SVGElementTagNameMap>(
    tag: K,
    attributes: Record<string, number | string>,
) {
    const node = document.createElementNS(svgNamespace, tag);
    for (const [key, value] of Object.entries(attributes)) node.setAttribute(key, String(value));
    return node;
}

function jsonRow(key: string, value: JsonValue, expanded = false): HTMLElement {
    const name = element('code', 'vault-json-key', key);
    if (value === null || typeof value !== 'object') {
        const row = element('div', 'vault-json-leaf');
        const scalar = element('code', 'vault-json-scalar', JSON.stringify(value));
        scalar.dataset.type = value === null ? 'null' : typeof value;
        row.append(name, element('span', 'vault-json-punctuation', ':'), scalar);
        return row;
    }
    const list = Array.isArray(value);
    const keys = Object.keys(value);
    const details = element('details', 'vault-json-branch');
    const summary = element('summary');
    summary.append(
        name,
        element(
            'span',
            'vault-json-kind',
            list ? `Array · ${keys.length}` : `Object · ${keys.length}`,
        ),
    );
    details.append(summary);
    let offset = 0;
    const children = element('div', 'vault-json-children');
    const more = element('button', 'button quiet', 'Show more');
    more.type = 'button';
    // Build child nodes on demand so wide and deeply nested entries stay responsive
    const append = () => {
        const batch = document.createDocumentFragment();
        const end = Math.min(offset + jsonBatch, keys.length);
        for (; offset < end; offset++) {
            const key = keys[offset];
            const child = list ? value[Number(key)] : value[key];
            if (child !== undefined) batch.append(jsonRow(key, child));
        }
        children.insertBefore(batch, more);
        more.hidden = offset === keys.length;
    };
    children.append(more);
    more.addEventListener('click', append);
    details.append(children);
    if (!keys.length)
        children.append(element('p', 'field-help', list ? 'Empty array' : 'Empty object'));
    more.hidden = !keys.length;
    details.addEventListener('toggle', () => {
        if (details.open && offset === 0 && keys.length) append();
    });
    if (expanded) details.open = true;
    return details;
}

function locationView(value: JsonValue): HTMLElement | null {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null;
    const { name, latitude, longitude, accuracy_m, source } = value;
    if (typeof name !== 'string' || typeof latitude !== 'number' || typeof longitude !== 'number')
        return null;
    const point = {
        latitude,
        longitude,
        accuracy_m: typeof accuracy_m === 'number' ? accuracy_m : undefined,
    };
    if (!vaultLocationMap(point)) return null;
    const layout = element('div', 'vault-location-view');
    layout.append(element('h3', undefined, name));
    const fields = element('dl', 'vault-location-fields');
    const addField = (label: string, text: string) => {
        const field = element('div');
        field.append(element('dt', undefined, label), element('dd', undefined, text));
        fields.append(field);
    };
    addField('Latitude', String(latitude));
    addField('Longitude', String(longitude));
    if (point.accuracy_m !== undefined) addField('Accuracy', `${point.accuracy_m} m`);
    if (source === 'os') addField('Source', 'Device location');
    const map = svg('svg', {
        class: 'vault-location-map',
        viewBox: '0 0 480 228',
        role: 'img',
        'aria-label': 'Offline map showing the saved location',
    });
    const controls = element('div', 'button-row vault-map-controls');
    controls.setAttribute('role', 'group');
    controls.setAttribute('aria-label', 'Map zoom');
    const draw = (zoom: number) => {
        const geometry = vaultLocationMap(point, zoom);
        if (!geometry) return;
        map.replaceChildren();
        for (const transform of geometry.landTransforms)
            map.append(svg('path', { d: geometry.landPath, transform, 'fill-rule': 'evenodd' }));
        if (geometry.accuracyRadius > 0)
            map.append(
                svg('ellipse', {
                    class: 'vault-map-accuracy',
                    cx: geometry.markerX,
                    cy: geometry.markerY,
                    rx: geometry.accuracyRadius,
                    ry: geometry.accuracyRadiusY,
                }),
            );
        map.append(
            svg('circle', {
                class: 'vault-map-marker',
                cx: geometry.markerX,
                cy: geometry.markerY,
                r: 6,
            }),
        );
        for (const control of controls.querySelectorAll('button'))
            control.setAttribute('aria-pressed', String(Number(control.dataset.zoom) === zoom));
    };
    for (const [label, zoom] of [
        ['World', 1],
        ['Region', 3],
        ['Closer', 8],
    ] as const) {
        const button = element('button', 'button quiet', label);
        button.type = 'button';
        button.dataset.zoom = String(zoom);
        button.addEventListener('click', () => draw(zoom));
        controls.append(button);
    }
    const footer = element('div', 'vault-map-footer');
    footer.append(controls, element('small', undefined, 'Made with Natural Earth'));
    layout.append(fields, map, footer);
    draw(3);
    return layout;
}

// Render first-party content only after authorization; no user value becomes HTML or a URL
export function renderVaultValue(container: HTMLElement, kind: VaultKind, value: JsonValue) {
    container.replaceChildren();
    if (kind === 'json') {
        const tree = element('div', 'vault-json-tree');
        tree.setAttribute('role', 'group');
        tree.setAttribute('aria-label', 'JSON value');
        tree.append(jsonRow('Value', value, true));
        container.append(tree);
    } else if (kind === 'location') {
        const view = locationView(value);
        if (!view) throw Error('invalid_value_reply');
        container.append(view);
    } else if (kind === 'integer' || kind === 'boolean') {
        if (kind === 'integer' ? typeof value !== 'number' : typeof value !== 'boolean')
            throw Error('invalid_value_reply');
        const scalar = element('output', 'vault-value-scalar', String(value));
        scalar.dataset.kind = kind;
        scalar.setAttribute('aria-label', 'Entry value');
        container.append(scalar);
    } else {
        if (typeof value !== 'string') throw Error('invalid_value_reply');
        const path = kind === 'file' || kind === 'folder';
        const contents = element('pre', 'vault-value-text', value);
        contents.setAttribute('aria-label', path ? 'Saved path' : 'Entry value');
        contents.tabIndex = 0;
        if (secretKinds.has(kind)) contents.classList.add('vault-value-secret');
        if (value.length) container.append(contents);
        else container.append(element('p', 'field-help', 'Empty text.'));
        if (path)
            container.append(
                element('p', 'field-help', 'Saved path only. The file or folder was not opened.'),
            );
    }
}
