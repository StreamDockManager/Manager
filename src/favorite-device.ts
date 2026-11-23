import type { DeviceInfo } from '../generated/runtime';
const browserKey = 'stream-dock-manager.favorite-device';
export function validFavoriteKey(value: unknown): string | null {
    return typeof value === 'string' &&
        value.length > 0 &&
        new TextEncoder().encode(value).length <= 1024 &&
        !/[\u0000-\u001f\u007f]/.test(value)
        ? value
        : null;
}
export function favoriteDevice(devices: readonly DeviceInfo[], key: string | null) {
    if (!key) return undefined;
    const matches = devices.filter(
        (device) => device.connected && !device.simulated && device.persistence_key === key,
    );
    return matches.length === 1 ? matches[0] : undefined;
}
export function availableDevice(devices: readonly DeviceInfo[], current: string) {
    return (
        devices.find((device) => device.id === current && device.connected) ||
        devices.find((device) => device.connected && !device.simulated) ||
        devices.find((device) => device.connected) ||
        devices.find((device) => device.id === current) ||
        devices[0]
    );
}
export function readBrowserFavorite() {
    try {
        return validFavoriteKey(localStorage.getItem(browserKey));
    } catch {
        return null;
    }
}
export function writeBrowserFavorite(key: string | null) {
    if (key) localStorage.setItem(browserKey, key);
    else localStorage.removeItem(browserKey);
}
