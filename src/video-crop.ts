// Crop geometry is normalized to decoded source pixels, independent of CSS/DPR.
export type Crop = [number, number, number, number];
export type CropHandle = 'move' | 'nw' | 'ne' | 'sw' | 'se';
const clamp = (value: number, low: number, high: number) => Math.max(low, Math.min(high, value));
export function containRect(
    width: number,
    height: number,
    sourceWidth: number,
    sourceHeight: number,
) {
    const scale = Math.min(width / Math.max(1, sourceWidth), height / Math.max(1, sourceHeight));
    const w = sourceWidth * scale,
        h = sourceHeight * scale;
    return { x: (width - w) / 2, y: (height - h) / 2, width: w, height: h };
}
export function normalizeCrop(crop: readonly number[]): Crop {
    const value = (i: number, fallback: number) => (Number.isFinite(crop[i]) ? crop[i] : fallback);
    const width = clamp(value(2, 1), 0.01, 1),
        height = clamp(value(3, 1), 0.01, 1);
    return [clamp(value(0, 0), 0, 1 - width), clamp(value(1, 0), 0, 1 - height), width, height];
}
export function transformCrop(
    crop: readonly number[],
    dx: number,
    dy: number,
    handle: CropHandle,
): Crop {
    const [x, y, width, height] = normalizeCrop(crop);
    if (handle === 'move')
        return [clamp(x + dx, 0, 1 - width), clamp(y + dy, 0, 1 - height), width, height];
    const left = handle.includes('w') ? clamp(x + dx, 0, x + width - 0.01) : x;
    const right = handle.includes('e') ? clamp(x + width + dx, x + 0.01, 1) : x + width;
    const top = handle.includes('n') ? clamp(y + dy, 0, y + height - 0.01) : y;
    const bottom = handle.includes('s') ? clamp(y + height + dy, y + 0.01, 1) : y + height;
    return [left, top, right - left, bottom - top];
}

// Fill's centered overscan is an implicit second crop. Make that visible rectangle
// explicit before direct manipulation, so even an uncropped Fill image can pan.
export function destinationCrop(
    crop: readonly number[],
    sourceWidth: number,
    sourceHeight: number,
    targetWidth: number,
    targetHeight: number,
    fit: string,
): Crop {
    const [x, y, w, h] = crop;
    if (fit === 'fit') return [x, y, w, h];
    const scale = Math.max(targetWidth / (w * sourceWidth), targetHeight / (h * sourceHeight));
    const cw = targetWidth / scale / sourceWidth,
        ch = targetHeight / scale / sourceHeight;
    return [x + (w - cw) / 2, y + (h - ch) / 2, cw, ch];
}
export function panDestination(
    crop: readonly number[],
    dx: number,
    dy: number,
    sourceWidth: number,
    sourceHeight: number,
    boxWidth: number,
    boxHeight: number,
    fit: string,
): Crop {
    const [, , w, h] = crop;
    const scale = (fit === 'fit' ? Math.min : Math.max)(
        boxWidth / (w * sourceWidth),
        boxHeight / (h * sourceHeight),
    );
    return [
        clamp(crop[0] - dx / (scale * sourceWidth), 0, 1 - w),
        clamp(crop[1] - dy / (scale * sourceHeight), 0, 1 - h),
        w,
        h,
    ];
}
export function zoomDestination(crop: readonly number[], factor: number): Crop {
    const [x, y, w, h] = crop;
    const scale = clamp(
        1 / factor,
        Math.min(1, Math.max(0.01 / w, 0.01 / h)),
        Math.min(1 / w, 1 / h),
    );
    const cw = w * scale,
        ch = h * scale;
    return [clamp(x + w / 2 - cw / 2, 0, 1 - cw), clamp(y + h / 2 - ch / 2, 0, 1 - ch), cw, ch];
}
