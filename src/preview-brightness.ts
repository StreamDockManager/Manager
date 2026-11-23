// Only simulators model configured brightness; physical-device previews show full colors.
// This is an editor visibility aid, not calibrated LCD output. Sleep remains black.
export function previewBrightness(
    brightness: number,
    sleeping: boolean,
    simulated: boolean,
): number {
    if (sleeping) return 0;
    if (!simulated) return 1;
    return 0.15972 + (0.84028 * Math.min(100, Math.max(0, brightness))) / 100;
}
