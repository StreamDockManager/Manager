import { displayPanels } from './display-panels';
import type { Control, Manifest } from './types';

// Center visual rows, retaining the descriptor's dimensions, gaps and order
// Overlapping vertical ranges belong to one group: moving mixed-height dials
// independently could collide with adjacent keys or break an asymmetric layout
// These are preview coordinates only; media cropping and hardware stay unchanged
export function previewRowOffsets(
    controls: readonly Control[],
    width: number,
    buttonRowSpan?: number,
) {
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
        if (
            buttonRowSpan &&
            row.controls.length > 1 &&
            row.controls.every((c) => c.kind === 'button' && c.y === row.top)
        ) {
            // Align LCD row edges to the packed touch strip without scaling apertures
            // or changing the native coordinates used to crop their media.
            const ordered = [...row.controls].sort((a, b) => a.x - b.x);
            const occupied = ordered.reduce((sum, c) => sum + c.width, 0);
            if (buttonRowSpan >= occupied) {
                const gap = (buttonRowSpan - occupied) / (ordered.length - 1);
                let x = (width - buttonRowSpan) / 2;
                for (const control of ordered) {
                    offsets.set(control.id, x - control.x);
                    x += control.width + gap;
                }
                continue;
            }
        }
        for (const control of row.controls) offsets.set(control.id, offset);
    }
    return offsets;
}

// Build all descriptor-relative geometry together so live pixels and selection do not invalidate it.
export function previewGeometry(
    all: Control[],
    width: number,
    model?: string,
    touchPanel?: Manifest['capabilities']['touch_panel'],
) {
    const controls = all.filter((control) => !control.semantics.includes('swipe'));
    const rects = new Map<string, { x: number; y: number; width: number; height: number }>();
    const touchPanels = all
        .filter((c) =>
            touchPanel
                ? c.id === touchPanel.gesture_control && c.width > 0
                : c.kind === 'touch' && c.preview_hidden && c.width > 0,
        )
        .map((panel) => {
            const members = all
                .filter(
                    (c) =>
                        (touchPanel
                            ? touchPanel.button_controls.includes(c.id)
                            : c.kind === 'touch') &&
                        !c.preview_hidden &&
                        c.y === panel.y &&
                        c.x >= panel.x &&
                        c.x + c.width <= panel.x + panel.width,
                )
                .sort((a, b) => a.x - b.x);
            const span = members.reduce((n, c) => n + c.width, 0);
            const left = panel.x + (panel.width - span) / 2;
            let x = left;
            for (const c of members) {
                rects.set(c.id, { x, y: c.y, width: c.width, height: c.height });
                x += c.width;
            }
            return { ...panel, x: left, width: span || panel.width };
        });
    // Measure already packed apertures so row centering never applies their gap adjustment twice
    const packedControls = controls.map((control) => ({ ...control, ...rects.get(control.id) }));
    const rowOffsets = previewRowOffsets(
        packedControls,
        width,
        touchPanel || model === 'n4-pro' ? touchPanels[0]?.width : undefined,
    );
    const panels = displayPanels(
        packedControls.map((control) => ({
            ...control,
            x: control.x + (rowOffsets.get(control.id) || 0),
        })),
    ).filter((panel) => panel.controlIds.length > 1);
    return { rowOffsets, touchPanels, rects, panels };
}

/** Project a physical control into the same centered row used by the editor. */
export function positionedControlRect(
    geometry: ReturnType<typeof previewGeometry>,
    control: Control,
) {
    const { x, y, width, height } = geometry.rects.get(control.id) || control;
    return { x: x + (geometry.rowOffsets.get(control.id) || 0), y, width, height };
}
