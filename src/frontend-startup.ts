import { failBootSplash, startBootSplash } from './boot-splash';
import { reportStartupFailure, startFrontendDiagnostics } from './frontend-errors';

// Install independently of the main module graph so a failed main/chunk import
// can be diagnosed even before Alpine and the editor bridge are available
startFrontendDiagnostics();
startBootSplash();
// Replace the loading screen with a safe diagnostic when application startup fails
void import('./main').catch((error) => {
    failBootSplash({ logReady: reportStartupFailure('module_import', error) });
});
