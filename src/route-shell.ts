import { registerProfileVersions } from './profile-versions';
import { registerThemeWallpaper } from './theme-wallpaper';
import { registerMacroSharing } from './macro-sharing';
import { registerPluginManagement } from './plugin-management';
import { registerExportReview } from './export-review';
import { registerMediaLibrary } from './media-library';
import { registerProfileConversion } from './profile-conversion';
import { registerLibraryOrganization } from './library-organization';
import Alpine from '@alpinejs/csp';
import { app } from './route-context';
import { registerFeatures } from './features';
import { registerProfileSharing } from './profile-sharing';
import { registerWorkspaceUI } from './workspace-ui';
import { registerVideoEditor } from './video-editor';
import { registerKeyboardMacroEditor } from './keyboard-macro-editor';
import { bridge } from './bridge';
import { registerBackgroundEditor } from './background-editor';
// Register the shell handler with its owner
export function register() {
    registerProfileVersions(Alpine, app);
    registerKeyboardMacroEditor(Alpine, bridge);
    registerThemeWallpaper(Alpine, app);
    registerVideoEditor(Alpine, app);
    registerLibraryOrganization(Alpine, app);
    registerProfileConversion(Alpine, app);
    registerMediaLibrary(Alpine, app);
    registerExportReview(Alpine, app);
    registerPluginManagement(Alpine, app);
    registerFeatures(Alpine, app);
    registerProfileSharing();
    registerMacroSharing();
    registerWorkspaceUI(Alpine, app);
    registerBackgroundEditor();
}
