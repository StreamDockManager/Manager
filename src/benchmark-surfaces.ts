import type { Manifest } from './types';

// Count benchmarkable key, screen and background surfaces
export function benchmarkSurfaceCounts(model: Manifest | undefined) {
    const controls = model?.controls.filter((control) => control.image_slot != null) || [];
    return {
        keys: controls.filter((control) => control.kind === 'button').length,
        sections: controls.length,
    };
}
