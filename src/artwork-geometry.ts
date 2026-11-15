import type { Control, ModelManifest, ArtworkSlice } from '../generated/runtime';
// Select supported artwork controls from the hardware manifest
export function artworkSelection(manifest: ModelManifest, ids: string[]): Control[] {
    const controls = ids.map((id) => manifest.controls.find((c) => c.id === id));
    const first = controls[0];
    if (
        ids.length < 2 ||
        ids.length > 100 ||
        new Set(ids).size !== ids.length ||
        !first ||
        controls.some(
            (c) =>
                !c ||
                c.kind !== 'button' ||
                c.preview_hidden ||
                c.image_slot == null ||
                !c.image_width ||
                !c.image_height ||
                c.width !== first.width ||
                c.height !== first.height ||
                c.image_width !== first.image_width ||
                c.image_height !== first.image_height,
        )
    )
        throw Error('Select at least two compatible image keys in a row or rectangle.');
    const selected = controls as Control[];
    const xs = [...new Set(selected.map((c) => c.x))],
        ys = [...new Set(selected.map((c) => c.y))];
    if (
        xs.length * ys.length !== selected.length ||
        xs.some((x) => ys.some((y) => !selected.some((c) => c.x === x && c.y === y))) ||
        manifest.controls.some(
            (c) =>
                c.kind === 'button' &&
                c.x >= Math.min(...xs) &&
                c.x <= Math.max(...xs) &&
                c.y >= Math.min(...ys) &&
                c.y <= Math.max(...ys) &&
                !ids.includes(c.id),
        )
    )
        throw Error('Select a contiguous row or rectangular block without gaps.');
    return selected;
}
// Calculate per-control crops for a shared artwork image
export function artworkCrops(
    manifest: ModelManifest,
    request: ArtworkSlice,
): Record<string, [number, number, number, number]> {
    const [x, y, width, height] = request.image_rect;
    if (
        !request.image_rect.every((v) => Number.isFinite(v) && Math.abs(v) <= 1_000_000) ||
        width <= 0 ||
        height <= 0
    )
        throw Error('Invalid artwork transform');
    return Object.fromEntries(
        // Map each selected control into the source artwork rectangle
        artworkSelection(manifest, request.controls).map((c) => {
            const crop: [number, number, number, number] = [
                (c.x - x) / width,
                (c.y - y) / height,
                c.width / width,
                c.height / height,
            ];
            if (
                crop[0] < -1e-9 ||
                crop[1] < -1e-9 ||
                crop[0] + crop[2] > 1 + 1e-9 ||
                crop[1] + crop[3] > 1 + 1e-9
            )
                throw Error('Artwork must cover every selected key.');
            crop[0] = Math.max(0, crop[0]);
            crop[1] = Math.max(0, crop[1]);
            crop[2] = Math.min(1 - crop[0], crop[2]);
            crop[3] = Math.min(1 - crop[1], crop[3]);
            return [c.id, crop];
        }),
    );
}
