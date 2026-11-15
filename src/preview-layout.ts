import type { Control } from './types';

// Center visual rows, retaining the descriptor's dimensions, gaps and order
// Overlapping vertical ranges belong to one group: moving mixed-height dials
// independently could collide with adjacent keys or break an asymmetric layout
// These are preview coordinates only; media cropping and hardware stay unchanged
export function previewRowOffsets(controls: readonly Control[], width: number) {
    const rows: { top: number; bottom: number; controls: Control[] }[] = [];
    for (const control of controls
        .filter((control) => !control.semantics.includes('swipe'))
        .sort((a, b) => a.y - b.y)) {
        const previous = rows.at(-1);
        if (previous && control.y < previous.bottom) {
            previous.bottom = Math.max(previous.bottom, control.y + control.height);
            previous.controls.push(control);
        } else {
            rows.push({ top: control.y, bottom: control.y + control.height, controls: [control] });
        }
    }
    const offsets = new Map<string, number>();
    for (const row of rows) {
        const left = Math.min(...row.controls.map((control) => control.x));
        const right = Math.max(...row.controls.map((control) => control.x + control.width));
        const offset = (width - (right - left)) / 2 - left;
        for (const control of row.controls) offsets.set(control.id, offset);
    }
    return offsets;
}
