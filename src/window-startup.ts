import type { listen } from '@tauri-apps/api/event';
import type { AppState } from './state';
import type { WindowCloseCoordinator, WindowCloseTicket } from './window-close';
import { windowWork } from './window-work';

/** A reveal is ready only after every reader and the native readiness handshake succeeds. */
export class WindowResumeCoordinator {
    private pending: Promise<void> | undefined;
    private recoveryRequired = false;

    // Initialize window startup state and its required lifecycle hooks
    constructor(
        private hooks: {
            suspended(): boolean;
            recover(): Promise<void>;
            refresh(): Promise<void>;
            ready(): Promise<void>;
            suspend(): Promise<void>;
        },
    ) {}

    // Resume the paused window startup operation
    resume(): Promise<void> {
        if (this.pending) return this.pending;
        const pending = this.run().finally(() => {
            if (this.pending === pending) this.pending = undefined;
        });
        this.pending = pending;
        return pending;
    }

    // Run the requested window startup operation
    private async run() {
        try {
            if (this.recoveryRequired || this.hooks.suspended()) await this.hooks.recover();
            else await this.hooks.refresh();
            await this.hooks.ready();
            this.recoveryRequired = false;
        } catch (error) {
            // AppState may already be live when an ancillary reader or readiness fails
            // Retire that partial attempt before another reveal starts its replacement
            this.recoveryRequired = true;
            try {
                await this.hooks.suspend();
            } catch (suspendError) {
                throw new AggregateError(
                    [error, suspendError],
                    `${String(error)}. Could not suspend the partial editor: ${String(suspendError)}`,
                );
            }
            throw error;
        }
    }
}

/** Own resources acquired by startup, including registrations that finish after disposal. */
export class StartupLifetime {
    private cancellation = new AbortController();
    private cleanups = new Set<() => void>();
    private retirements = new Set<Promise<void>>();
    private registrations = new Set<Promise<void>>();
    // Derive signal from the current window startup state
    get signal() {
        return this.cancellation.signal;
    }
    // Derive disposed from the current window startup state
    get disposed() {
        return this.signal.aborted;
    }
    // Ensure the active
    ensureActive() {
        if (this.disposed) throw new DOMException('Window startup was cancelled.', 'AbortError');
    }
    // Register an idempotent cleanup and track any asynchronous retirement it starts
    own(cleanup: () => void | Promise<void>) {
        // Release resources retained by the window startup operation
        const release = () => {
            this.cleanups.delete(release);
            try {
                // Handle a failed window startup operation without retaining stale work
                const retirement = Promise.resolve(cleanup()).catch((error) => {
                    windowWork.retirementFailed('startup listener');
                    console.error('Could not release a window startup resource:', error);
                });
                this.retirements.add(retirement);
                void retirement.finally(() => this.retirements.delete(retirement));
            } catch (error) {
                windowWork.retirementFailed('startup listener');
                console.error('Could not release a window startup resource:', error);
            }
        };
        if (this.disposed) release();
        else this.cleanups.add(release);
    }
    // Wait for the current window startup operation to settle
    async wait<T>(pending: Promise<T>): Promise<T> {
        this.ensureActive();
        // Resolve the window startup operation when its asynchronous work finishes
        return new Promise<T>((resolve, reject) => {
            // Cancel outstanding window startup work after its owner aborts
            const aborted = () =>
                reject(new DOMException('Window startup was cancelled.', 'AbortError'));
            this.signal.addEventListener('abort', aborted, { once: true });
            pending.then(
                // Process the settled window startup operation result
                (value) => {
                    this.signal.removeEventListener('abort', aborted);
                    if (this.disposed) aborted();
                    else resolve(value);
                },
                // Process the settled window startup operation result
                (error) => {
                    this.signal.removeEventListener('abort', aborted);
                    reject(error);
                },
            );
        });
    }
    // Own the eventual unlisten callback even if startup is cancelled before registration finishes
    async listen(register: () => Promise<() => void | Promise<void>>) {
        this.ensureActive();
        // Track the result before racing cancellation so a late native registration
        // still gets its unlisten callback, even after the caller has stopped waiting
        const registration = register().then((unlisten) => this.own(unlisten));
        this.registrations.add(registration);
        void registration.then(
            () => this.registrations.delete(registration),
            () => this.registrations.delete(registration),
        );
        await this.wait(registration);
        this.ensureActive();
    }
    // Retire transient window startup state after the operation settles
    async settled() {
        while (this.registrations.size || this.retirements.size)
            await Promise.allSettled([...this.registrations, ...this.retirements]);
    }
    // Release window startup state and associated lifecycle resources
    dispose() {
        if (this.disposed) return;
        this.cancellation.abort();
        for (const cleanup of [...this.cleanups].reverse()) cleanup();
    }
}

type WindowApp = Pick<
    AppState,
    | 'bridge'
    | 'flushDrafts'
    | 'notify'
    | 'pauseSurfaces'
    | 'refresh'
    | 'requestSurfaces'
    | 'openDeviceEditor'
>;

/** Native window events are ready before frontend_ready lets the host send queued work. */
export async function registerNativeWindowEvents(
    lifetime: StartupLifetime,
    app: () => WindowApp,
    nativeListen: typeof listen,
    flushDraftQueue: () => Promise<void>,
    options?: {
        generation: number;
        close?: WindowCloseCoordinator;
        resume?: () => Promise<void>;
    },
) {
    await lifetime.listen(() =>
        nativeListen<{ quit: boolean; action_id: number; generation?: number }>(
            'flush-drafts',
            // Complete the deferred window startup work using the current operation state
            async (event) => {
                if (lifetime.disposed) return;
                if (
                    event.payload.generation !== undefined &&
                    event.payload.generation !== (options?.generation || 1)
                )
                    return;
                if (!event.payload.quit && options?.close) {
                    await options.close.request({
                        generation: options.generation,
                        action_id: event.payload.action_id,
                    });
                    return;
                }
                try {
                    // Complete the deferred window startup work using the current operation state
                    await windowWork.run('close draft flush', async () => {
                        try {
                            await app().flushDrafts?.();
                            if (lifetime.disposed) return;
                            await flushDraftQueue();
                            if (lifetime.disposed) return;
                            await app().bridge.command('finish_window_action', {
                                quit: event.payload.quit,
                                generation: options?.generation || 1,
                                actionId: event.payload.action_id,
                            });
                        } catch (error) {
                            // A deferred native quit must receive an explicit cancellation
                            // Keep this logical lease until its exact action ticket is retired
                            if (!lifetime.disposed) {
                                try {
                                    await app().bridge.command('abort_window_action', {
                                        generation: options?.generation || 1,
                                        actionId: event.payload.action_id,
                                    });
                                } catch (abortError) {
                                    throw new Error(
                                        `${String(error)}. Could not cancel the window action: ${String(abortError)}`,
                                    );
                                }
                            }
                            throw error;
                        }
                    });
                } catch (error) {
                    if (!lifetime.disposed)
                        app().notify(
                            'Could not save drafts before closing: ' + String(error),
                            'error',
                        );
                }
            },
        ),
    );
    lifetime.ensureActive();
    if (options?.close) {
        await lifetime.listen(() =>
            nativeListen<WindowCloseTicket>('window-action-cancelled', (event) => {
                if (!lifetime.disposed) options.close?.cancel(event.payload);
            }),
        );
        await lifetime.listen(() =>
            // Complete the deferred window startup work using the current operation state
            nativeListen<{ generation: number }>('editor-resume', async (event) => {
                if (
                    !lifetime.disposed &&
                    event.payload.generation === options.generation &&
                    !options.close?.pending
                ) {
                    try {
                        await options.resume?.();
                    } catch (error) {
                        app().notify('Could not resume the editor: ' + String(error), 'error');
                    }
                }
            }),
        );
    }
    await lifetime.listen(() =>
        // Complete the deferred window startup work using the current operation state
        nativeListen<boolean>('editor-visibility', async (event) => {
            if (lifetime.disposed) return;
            if (typeof window !== 'undefined') {
                window.dispatchEvent(
                    new CustomEvent('editor-window-visibility', { detail: event.payload }),
                );
            }
            if (options?.close) {
                if (!event.payload) app().pauseSurfaces();
                else if (!options.close.pending) {
                    try {
                        await options.resume?.();
                    } catch (error) {
                        app().notify('Could not resume the editor: ' + String(error), 'error');
                    }
                }
                return;
            }
            app().pauseSurfaces();
            if (event.payload) {
                try {
                    await app().refresh();
                    if (!lifetime.disposed) app().requestSurfaces();
                } catch (error) {
                    if (!lifetime.disposed)
                        app().notify(
                            'Could not refresh the device editor: ' + String(error),
                            'error',
                        );
                }
            }
        }),
    );
    lifetime.ensureActive();
    await lifetime.listen(() =>
        // Complete the deferred window startup work using the current operation state
        nativeListen<string>('tray-open-device', (event) => {
            if (lifetime.disposed) return;
            void app()
                .openDeviceEditor(event.payload)
                // Handle a failed window startup operation without retaining stale work
                .catch((error) => {
                    if (!lifetime.disposed)
                        app().notify('Could not open the device editor: ' + String(error), 'error');
                });
        }),
    );
    await lifetime.listen(() =>
        nativeListen<unknown>('plugin-notification', ({ payload }) => {
            if (lifetime.disposed || !payload || typeof payload !== 'object') return;
            const { plugin, level, message } = payload as Record<string, unknown>;
            if (
                typeof plugin !== 'string' ||
                !plugin.length ||
                typeof message !== 'string' ||
                !message.trim() ||
                (level !== 'info' &&
                    level !== 'success' &&
                    level !== 'warning' &&
                    level !== 'error')
            )
                return;
            const source = plugin.replace(/^app\.streamdockmanager\.plugins\./, '');
            app().notify(`${source}: ${message}`, level);
        }),
    );
    lifetime.ensureActive();
    await lifetime.wait(
        app().bridge.command('frontend_ready', { generation: options?.generation || 1 }),
    );
    lifetime.ensureActive();
}
