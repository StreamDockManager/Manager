/** Lightweight startup UI, independent of Alpine and the editor module graph */
import { invoke, isTauri } from '@tauri-apps/api/core';

let stopWaiting = () => {};
let detachedBoot: HTMLElement | null = null;
const startedAt = performance.now();

/** Local visual review only. Vite eliminates this branch from production builds */
export async function waitForBootPreview() {
    if (!import.meta.env.DEV) return;
    const requested = Number(import.meta.env.VITE_SPLASH_PREVIEW_MS || 0);
    const duration = Number.isFinite(requested) ? Math.min(30_000, Math.max(0, requested)) : 0;
    const remaining = duration - (performance.now() - startedAt);
    if (remaining <= 0) return;
    // Wait out the remaining development-only splash preview delay
    await new Promise<void>((resolve) => {
        // End the preview delay on timeout or document unload
        const done = () => {
            window.clearTimeout(timer);
            window.removeEventListener('pagehide', done);
            resolve();
        };
        const timer = window.setTimeout(done, remaining);
        window.addEventListener('pagehide', done, { once: true });
    });
}

// Schedule a slow-start message for the existing splash
export function startBootSplash() {
    stopWaiting();
    const boot = document.getElementById('boot');
    if (!boot) return;
    // Update the splash after the startup timer elapses
    const timer = window.setTimeout(() => {
        const status = boot.querySelector('[data-boot-status]');
        if (boot.isConnected && status) {
            status.classList.remove('boot-status-pending');
            status.textContent = 'Still starting… This is taking longer than usual.';
        }
    }, 10_000);
    // Stop splash timers and remove temporary listeners
    const stop = () => {
        window.clearTimeout(timer);
        window.removeEventListener('pagehide', stop);
    };
    stopWaiting = stop;
    window.addEventListener('pagehide', stop, { once: true });
}

// Remove the splash and stop its pending status timer
export function finishBootSplash() {
    stopWaiting();
    document.getElementById('boot')?.remove();
    detachedBoot = null;
}

// Keep the existing unlock modal backdrop, but retain the splash in case startup still fails
export function suspendBootSplash() {
    stopWaiting();
    detachedBoot = document.getElementById('boot');
    detachedBoot?.remove();
}

/** Keep both startup failure paths usable before the editor command bridge is ready */
export function failBootSplash({
    retryReady,
    logReady,
}: {
    retryReady?: Promise<boolean>;
    logReady?: Promise<void>;
} = {}) {
    stopWaiting();
    const boot = document.getElementById('boot') ?? detachedBoot;
    if (boot === detachedBoot && boot) document.body.prepend(boot);
    detachedBoot = null;
    const status = boot?.querySelector('[data-boot-status]');
    const details = boot?.querySelector('[data-boot-details]');
    if (!boot || !status || !details) return;
    boot.dataset.state = 'error';
    status.classList.remove('boot-status-pending');
    status.classList.add('boot-error');
    status.setAttribute('role', 'alert');
    status.textContent = 'Unable to start';

    const buttons = document.createElement('div');
    buttons.className = 'boot-actions';
    const retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'button';
    retry.textContent = 'Retry startup';
    retry.disabled = !!retryReady || !!logReady;
    retry.addEventListener('click', () => location.reload(), { once: true });
    buttons.append(retry);

    const feedback = document.createElement('p');
    feedback.className = 'boot-action-status';
    feedback.setAttribute('role', 'status');
    feedback.hidden = true;
    if (import.meta.env.MODE !== 'public-preview' && isTauri()) {
        const logs = document.createElement('button');
        logs.type = 'button';
        logs.className = 'button';
        logs.textContent = 'View logs';
        logs.addEventListener('click', async () => {
            logs.disabled = true;
            feedback.hidden = true;
            try {
                await invoke('open_manager_logs');
            } catch {
                feedback.textContent = 'Could not open logs.';
                feedback.hidden = false;
            } finally {
                logs.disabled = false;
            }
        });
        buttons.append(logs);
    }
    details.replaceChildren(buttons, feedback);
    if (retryReady || logReady) {
        // Await the acknowledged fatal report without making a broken log bridge block recovery
        const flush =
            logReady &&
            new Promise<void>((resolve) => {
                const timer = window.setTimeout(resolve, 1500);
                const done = () => {
                    window.clearTimeout(timer);
                    resolve();
                };
                void logReady.then(done, done);
            });
        const ready = (allowed: boolean) => {
            if (allowed) retry.disabled = false;
            else status.textContent = 'Restart Manager to retry';
        };
        void Promise.all([retryReady ?? true, flush]).then(
            ([allowed]) => ready(allowed),
            () => ready(false),
        );
    }
}
