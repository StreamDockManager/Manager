/** Lightweight startup UI, independent of Alpine and the editor module graph */
let stopWaiting = () => {};
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
}

/** Preserve branding while callers retain ownership of retry/cleanup behavior */
export function failBootSplash(message: HTMLElement, retry: HTMLButtonElement) {
    stopWaiting();
    const boot = document.getElementById('boot');
    if (!boot) return;
    boot.dataset.state = 'error';
    const status = boot.querySelector('[data-boot-status]');
    if (status) {
        status.classList.remove('boot-status-pending');
        status.textContent = 'Unable to start';
    }
    message.classList.add('boot-error');
    message.setAttribute('role', 'alert');
    boot.querySelector('[data-boot-details]')?.replaceChildren(message, retry);
}
