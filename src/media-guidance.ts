import type { MediaLibraryEntry } from '../generated/runtime';

/** Advice only: size/FPS are not evidence that an asset is broken. */
export function mediaOptimizationGuidance(entry: MediaLibraryEntry) {
    if (entry.error) return '';
    const notes: string[] = [];
    if (Math.max(entry.width || 0, entry.height || 0) > 1024)
        notes.push('Large image: a smaller copy may be enough for device screens.');
    if (entry.logical_bytes > 4 * 1024 * 1024)
        notes.push('Large file: compare an optimized copy before choosing it.');
    if ((entry.frame_rate || 0) > 30)
        notes.push(
            `High frame rate (${Math.round(entry.frame_rate!)} FPS average): lower FPS can reduce work, but motion may look less smooth.`,
        );
    if (notes.length && entry.duration_ms)
        notes.push(
            'Use Edit GIF/video to lower resolution or FPS, preview, then save a separate clip.',
        );
    return notes.join(' ');
}

// Check whether the selected media entry supports still-image optimization
export function canOptimizeStill(entry: MediaLibraryEntry) {
    return (
        !entry.error && !entry.duration_ms && ['png', 'jpg', 'jpeg', 'svg'].includes(entry.format)
    );
}

// Compare a saved optimized copy with its original media entry
export function savedMediaComparison(copy: MediaLibraryEntry, original?: MediaLibraryEntry) {
    const description = `${copy.width || '?'} × ${copy.height || '?'}${copy.frame_rate ? `, ${Math.round(copy.frame_rate)} FPS average` : ''}`;
    const change = original
        ? ` Previous asset: ${original.logical_bytes.toLocaleString()} bytes; copy is ${copy.logical_bytes < original.logical_bytes ? 'smaller' : copy.logical_bytes > original.logical_bytes ? 'larger' : 'the same size'}.`
        : '';
    return `Last saved copy: ${copy.logical_bytes.toLocaleString()} bytes (${description}).${change} Lower resolution/FPS can lose detail or smoothness. Preview before applying; originals and existing assignments stay unchanged.`;
}
