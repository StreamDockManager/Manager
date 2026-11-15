import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { FeatureDialogHandoffs } from './feature-dialog-handoffs';
import { registerSetupWorkflow } from './setup-workflow';
import { registerPluginFolderImport } from './plugin-folder-import';
import { registerPluginPreflight } from './plugin-preflight';
import { registerPluginProfileSuggestions } from './plugin-profile-suggestions';
import { registerProfileArchiveImport } from './profile-archive-import';

export { defaultAppearance, suggestedFeedback } from './feature-defaults';

// Register feature dialogs and their shared handoff controllers
export function registerFeatures(Alpine: typeof AlpineType, app: () => AppState) {
    const handoffs = new FeatureDialogHandoffs();
    registerSetupWorkflow(Alpine, app, handoffs);
    registerPluginFolderImport(Alpine, app);
    registerPluginPreflight(Alpine, app);
    registerPluginProfileSuggestions(Alpine, app, handoffs);
    registerProfileArchiveImport(Alpine, app, handoffs);
}
