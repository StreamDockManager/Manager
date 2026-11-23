import { registerThemeWallpaper } from './theme-wallpaper';
import { registerLibraryOrganization } from './library-organization';
import Alpine from '@alpinejs/csp';
import { app } from './route-context';
import { registerFeatures } from './features';
import { registerWorkspaceUI } from './workspace-ui';
import { bridge } from './bridge';
export const dialogControllers: Record<string, () => Promise<void>> = {
    'video-editor': async () => (await import('./video-editor')).registerVideoEditor(Alpine, app),
    'background-editor': async () =>
        (await import('./background-editor')).registerBackgroundEditor(),
    'profile-versions': async () =>
        (await import('./profile-versions')).registerProfileVersions(Alpine, app),
    'profile-conversion': async () =>
        (await import('./profile-conversion')).registerProfileConversion(Alpine, app),
    'macro-sharing': async () => (await import('./macro-sharing')).registerMacroSharing(),
    'export-review': async () =>
        (await import('./export-review')).registerExportReview(Alpine, app),
    'profile-sharing-dialog': async () =>
        (await import('./profile-sharing')).registerProfileSharing(),
    'plugin-management': async () =>
        (await import('./plugin-management')).registerPluginManagement(Alpine, app),
    'media-library': async () =>
        (await import('./media-library')).registerMediaLibrary(Alpine, app),
    'keyboard-macro-editor': async () =>
        (await import('./keyboard-macro-editor')).registerKeyboardMacroEditor(Alpine, bridge),
};

// Register the shell handler with its owner
export function register() {
    registerThemeWallpaper(Alpine, app);
    registerLibraryOrganization(Alpine, app);
    registerFeatures(Alpine, app);
    registerWorkspaceUI(Alpine, app);
}
