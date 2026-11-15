import { failBootSplash, startBootSplash } from './boot-splash';
import { reportFrontendError, startFrontendDiagnostics } from './frontend-errors';

// Install independently of the main module graph so a failed main/chunk import
// can be diagnosed even before Alpine and the editor bridge are available
startFrontendDiagnostics();
startBootSplash();
// Replace the loading screen with a safe diagnostic when application startup fails
void import('./main').catch((error) => {
    reportFrontendError('startup_failed', error);
    const boot = document.getElementById('boot');
    if (!boot) return;
    const message = document.createElement('p');
    message.textContent = 'Unable to load Stream Dock Manager. Restart Manager or retry startup.';
    const retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'button';
    retry.textContent = 'Retry startup';
    retry.addEventListener('click', () => location.reload(), { once: true });
    failBootSplash(message, retry);
});
