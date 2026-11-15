import { app } from './route-context';
import { trackCloseRelease } from './close-releases';
import type { HardwareDraft } from '../generated/runtime';
// Explain why the selected device cannot show a live draft preview
export function dockPreviewReason(background = false) {
    const state = app();
    if (!state.device?.connected) return 'Connect a dock to preview.';
    if (state.simulated) return 'Preview on dock requires a physical device.';
    if (state.effectiveDeviceOutput.sleeping) return 'Wake the dock to preview.';
    if (background && !state.device.manifest.capabilities.background)
        return 'This device has no live background output.';
    if (!background) {
        const source = state.device.manifest.controls.find((c) => c.id === state.controlId);
        const surface = source?.feedback_surface
            ? state.device.manifest.controls.find((c) => c.id === source.feedback_surface)
            : source;
        if (!surface || surface.image_slot == null)
            return 'This control has no live output surface.';
    }
    return '';
}
// Begin a scoped device preview and return its cancellation token
export async function startDockPreview(
    draft: HardwareDraft,
    current: () => boolean,
    expectedRevision = app().snapshot.configuration_revision,
) {
    const token = await app().bridge.command('preview_on_dock', {
        deviceId: app().deviceId,
        expectedRevision,
        draft,
    });
    if (!current()) {
        await cancelDockPreview(token);
        return '';
    }
    return token;
}
// Cancel the device preview identified by its token
export function cancelDockPreview(token: string) {
    if (!token) return Promise.resolve();
    return trackCloseRelease('dock-preview:' + token, () =>
        app().bridge.command('cancel_hardware_preview', { token }),
    );
}
