import type { Control } from './types';

export type DisplayPanel = {
    id: string;
    x: number;
    y: number;
    width: number;
    height: number;
    controlIds: string[];
};

// Adjacent display-only regions form one physical strip. Image sizes and
// independently editable control IDs remain owned by their descriptors
export function displayPanels(controls: readonly Control[]): DisplayPanel[] {
    const panels: DisplayPanel[] = [];
    for (const control of controls
        .filter((control) => control.kind === 'display')
        .sort((a, b) => a.y - b.y || a.x - b.x)) {
        const previous = panels.find(
            (panel) =>
                panel.y === control.y &&
                panel.height === control.height &&
                panel.x + panel.width === control.x,
        );
        if (previous) {
            previous.width += control.width;
            previous.controlIds.push(control.id);
        } else {
            panels.push({
                id: control.id,
                x: control.x,
                y: control.y,
                width: control.width,
                height: control.height,
                controlIds: [control.id],
            });
        }
    }
    return panels;
}
