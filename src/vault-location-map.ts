import land from './map-data/natural-earth-land.v5.1.2.json';

export const VAULT_LOCATION_MAX_ZOOM = 8;

export interface VaultLocationMap {
    width: number;
    height: number;
    landPath: string;
    landTransforms: readonly string[];
    markerX: number;
    markerY: number;
    accuracyRadius: number;
    accuracyRadiusY: number;
}

const earthRadiusMetres = 6_371_008.8;
const degrees = 180 / Math.PI;

// Project a stored location and optional accuracy without any external requests
export function vaultLocationMap(
    value: { latitude: number; longitude: number; accuracy_m?: number | null },
    zoom = 1,
): VaultLocationMap | null {
    const { latitude, longitude, accuracy_m: accuracy } = value;
    if (
        !Number.isFinite(latitude) ||
        !Number.isFinite(longitude) ||
        Math.abs(latitude) > 90 ||
        Math.abs(longitude) > 180
    )
        return null;

    const { width, height, path: landPath } = land;
    const scale = Number.isFinite(zoom) ? Math.max(1, Math.min(VAULT_LOCATION_MAX_ZOOM, zoom)) : 1;
    const x = ((longitude + 180) * width) / 360;
    const y = ((90 - latitude) * height) / 180;
    const viewWidth = width / scale;
    const viewHeight = height / scale;
    // Wrap horizontally at the antimeridian and clamp vertically at the poles
    const left = scale === 1 ? 0 : x - viewWidth / 2;
    const top = Math.min(height - viewHeight, Math.max(0, y - viewHeight / 2));
    const first = Math.floor(left / width);
    const last = Math.ceil((left + viewWidth) / width) - 1;
    const landTransforms: string[] = [];
    for (let tile = first; tile <= last; tile++)
        landTransforms.push(
            `matrix(${scale} 0 0 ${scale} ${(tile * width - left) * scale} ${-top * scale})`,
        );

    let accuracyRadius = 0;
    let accuracyRadiusY = 0;
    if (typeof accuracy === 'number' && Number.isFinite(accuracy) && accuracy > 0) {
        // Bound approximate distances in the rendered plane even near a pole
        const angular = Math.min(Math.PI, accuracy / earthRadiusMetres);
        const latitudeRad = (latitude * Math.PI) / 180;
        const longitudeSpan =
            Math.abs(latitudeRad) + angular >= Math.PI / 2
                ? Math.PI
                : Math.asin(Math.min(1, Math.sin(angular) / Math.cos(latitudeRad)));
        accuracyRadius = Math.min(width, (longitudeSpan * degrees * width * scale) / 360);
        accuracyRadiusY = Math.min(height, (angular * degrees * height * scale) / 180);
    }
    return {
        width,
        height,
        landPath,
        landTransforms,
        markerX: (x - left) * scale,
        markerY: (y - top) * scale,
        accuracyRadius,
        accuracyRadiusY,
    };
}
