import { isTauri } from '@tauri-apps/api/core';
import { windowWork } from './window-work';

let queuedColor = '';
let update = Promise.resolve();

/** Keep the native window and WebKit's under-page area in sync with the palette. */
export function syncWindowBackground(color: string): Promise<void> {
    if (
        !(import.meta.env.MODE !== 'public-preview' && isTauri()) ||
        windowWork.quiescing ||
        queuedColor === color
    )
        return update;
    queuedColor = color;
    // Finish both native layers before applying another palette, skipping superseded updates
    update = update
        // Process the settled window background operation result
        .then(async () => {
            if (queuedColor !== color) return;
            if (windowWork.quiescing) {
                queuedColor = '';
                return;
            }
            // Complete the deferred window background work using the current operation state
            await windowWork.run('window background', async () => {
                const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow');
                await getCurrentWebviewWindow().setBackgroundColor(color);
            });
        })
        // Handle a failed window background operation without retaining stale work
        .catch((error) => {
            if (queuedColor === color) queuedColor = '';
            console.error('Could not update window background:', error);
        });
    return update;
}
