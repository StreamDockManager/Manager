import type { Binding, Manifest, Page, Profile } from './types';
import { readMediaPreview, releaseMediaPreview } from './media-preview';
import { iconImage } from './icon-svg';
import { resolvePaletteBinding } from './palette-binding';
import { referenceLayout } from './reference-layout';

function canvas(width: number, height: number) {
    const c = document.createElement('canvas');
    c.width = width;
    c.height = height;
    return c;
}

type ReadArt = (binding: Binding, control: string) => Promise<ArrayBuffer>;
async function schematic(
    profile: Profile,
    page: Page,
    model: Manifest,
    read: ReadArt,
    check: () => void,
) {
    const controls = referenceLayout(profile, page, model);
    const ratio = Math.min(3, 1500 / model.layout.width, 900 / model.layout.height);
    const c = canvas(Math.ceil(model.layout.width * ratio), Math.ceil(model.layout.height * ratio));
    const ctx = c.getContext('2d')!;
    ctx.scale(ratio, ratio);
    ctx.fillStyle = '#eee';
    ctx.fillRect(0, 0, model.layout.width, model.layout.height);
    for (const { binding, rect: r, number, control, positioned } of controls) {
        const b = binding && resolvePaletteBinding(binding);
        check();
        if (!positioned) continue;
        ctx.fillStyle = '#444';
        if (b?.color) ctx.fillStyle = b.color;
        ctx.fillRect(r.x, r.y, r.width, r.height);
        if (b?.asset) {
            let url = '';
            try {
                const image = new Image();
                url = readMediaPreview(await read(b, control.id)).image;
                image.src = url;
                await image.decode();
                ctx.drawImage(image, r.x, r.y, r.width, r.height);
            } catch {
                /* Keep the numbered position when authored media is unavailable. */
            } finally {
                releaseMediaPreview(url);
            }
        }
        const appearance = b?.appearance;
        if (appearance?.icon_asset || appearance?.icon) {
            const icon = new Image();
            icon.src = appearance.icon_asset
                ? iconImage(appearance.icon_asset, appearance.foreground)
                : import.meta.env.BASE_URL +
                  'appearance/icons/' +
                  encodeURIComponent(appearance.icon) +
                  '.svg';
            try {
                await icon.decode();
                const size = Math.min(r.width, r.height) * ((appearance.icon_size || 42) / 100);
                const offset =
                    appearance.icon_position === 'top'
                        ? 1
                        : appearance.icon_position === 'bottom'
                          ? r.height - size - 1
                          : (r.height - size) / 2;
                ctx.drawImage(icon, r.x + (r.width - size) / 2, r.y + offset, size, size);
            } catch {
                /* A missing icon retains its numbered control and legend. */
            }
        }
        const size = Math.max(7, Math.min(r.width, r.height) * 0.45);
        ctx.font = 'bold ' + size + 'px sans-serif';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        const x = r.x + r.width / 2,
            y = r.y + r.height / 2;
        ctx.lineWidth = size / 7;
        ctx.strokeStyle = '#111';
        ctx.strokeText(String(number), x, y);
        ctx.fillStyle = '#fff';
        ctx.fillText(String(number), x, y);
    }
    return { image: c, legend: controls.map((c) => c.legend) };
}

function wrap(text: string, measure: (s: string) => number, width: number) {
    const lines: string[] = [];
    let current = '';
    for (const ch of text.replace(/[\r\n\t]+/g, ' ')) {
        const next = current + ch;
        if (current && measure(next) > width) {
            const split = current.lastIndexOf(' ');
            if (split > current.length / 2) {
                lines.push(current.slice(0, split));
                current = current.slice(split + 1) + ch;
            } else {
                lines.push(current);
                current = ch;
            }
        } else current = next;
    }
    lines.push(current);
    return lines;
}

export type ReferenceFile = { name: string; bytes: Uint8Array };
export function referenceName(index: number) {
    return 'page-' + String(index + 1).padStart(2, '0') + '.png';
}

export async function renderReference(
    profile: Profile,
    model: Manifest,
    ids: string[],
    paper: string,
    read: ReadArt,
    check: () => void,
) {
    const [w, h] = paper === 'Letter' ? [612, 792] : [595.28, 841.89],
        margin = 32,
        inner = w - 2 * margin;
    const files: ReferenceFile[] = [];
    let total = 0;
    for (const [index, page] of profile.pages.entries()) {
        if (!ids.includes(page.id)) continue;
        check();
        const { image, legend } = await schematic(profile, page, model, read, check);
        const imageH = Math.min(240, (inner * image.height) / image.width),
            imageW = (imageH * image.width) / image.height;
        const title = profile.name + ' / ' + page.name;
        const measure = canvas(1, 1).getContext('2d')!;
        measure.font = '11px Inter, sans-serif';
        const lines = legend.flatMap((t) => wrap(t, (s) => measure.measureText(s).width, inner));
        measure.font = '15px Inter, sans-serif';
        const titles = wrap(title, (s) => measure.measureText(s).width, inner);
        const height = Math.max(
            h,
            margin * 2 + titles.length * 21 + 32 + imageH + lines.length * 16,
        );
        const scale = Math.min(2, 8192 / height);
        if (scale < 1)
            throw Error(
                'This page exceeds the PNG height limit. Shorten its labels or split its bindings across pages.',
            );
        const sheet = canvas(Math.ceil(w * scale), Math.ceil(height * scale));
        const ctx = sheet.getContext('2d')!;
        ctx.scale(scale, scale);
        ctx.fillStyle = '#fff';
        ctx.fillRect(0, 0, w, height);
        ctx.fillStyle = '#111';
        ctx.textBaseline = 'top';
        ctx.font = '15px Inter, sans-serif';
        let y = margin;
        for (const t of titles) {
            ctx.fillText(t, margin, y);
            y += 21;
        }
        ctx.font = '11px Inter, sans-serif';
        ctx.fillText(model.name, margin, y);
        y += 20;
        ctx.drawImage(image, margin, y, imageW, imageH);
        y += imageH + 12;
        for (const t of lines) {
            ctx.fillText(t, margin, y);
            y += 16;
        }
        const blob = await new Promise<Blob>((ok, fail) =>
            sheet.toBlob((b) => (b ? ok(b) : fail(Error('PNG rendering failed.'))), 'image/png'),
        );
        total += blob.size;
        if (total > 56 * 1024 * 1024)
            throw Error('Select fewer pages to stay within the export size limit.');
        files.push({
            name: referenceName(index),
            bytes: new Uint8Array(await blob.arrayBuffer()),
        });
    }
    check();
    return files;
}
