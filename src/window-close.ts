import { WindowWork, type WindowFence, windowWork } from './window-work';
import { WindowCloseOwners, windowCloseOwners } from './window-close-owners';

export interface WindowCloseTicket {
    generation: number;
    action_id: number;
}

export interface WindowCloseHooks {
    destination?(): Promise<'close' | 'restart' | 'cancel'>;
    restart?(): Promise<void>;
    prompt(unknown: boolean): Promise<boolean>;
    dismiss(): void;
    interactive(enabled: boolean): void;
    confirm(ticket: WindowCloseTicket): Promise<void>;
    pause(): void;
    retire(): Promise<void>;
    finish(ticket: WindowCloseTicket): Promise<void>;
    abort(ticket: WindowCloseTicket): Promise<void>;
    resume(): Promise<void>;
    failed(error: unknown): void;
}

/** Owns one native close intent without counting itself as outstanding editor work. */
export class WindowCloseCoordinator {
    private current:
        { ticket: WindowCloseTicket; confirmed: boolean; cancelled: boolean } | undefined;
    private task: Promise<void> | undefined;

    // Initialize window close state and its required lifecycle hooks
    constructor(
        readonly generation: number,
        private hooks: WindowCloseHooks,
        private owners: WindowCloseOwners = windowCloseOwners,
        private work: WindowWork = windowWork,
    ) {}

    // Derive pending from the current window close state
    get pending() {
        return !!this.current;
    }

    // Request the next window close update
    request(ticket: WindowCloseTicket): Promise<void> {
        if (ticket.generation !== this.generation) return Promise.resolve();
        if (this.task) return this.task;
        const intent = { ticket, confirmed: false, cancelled: false };
        this.current = intent;
        // Release transient window close state after the operation settles
        const task = this.close(intent).finally(() => {
            if (this.current === intent) this.current = undefined;
            if (this.task === task) this.task = undefined;
        });
        this.task = task;
        return task;
    }

    // Cancel the active window close operation
    cancel(ticket: WindowCloseTicket) {
        const intent = this.current;
        if (
            !intent ||
            intent.confirmed ||
            intent.ticket.generation !== ticket.generation ||
            intent.ticket.action_id !== ticket.action_id
        )
            return;
        intent.cancelled = true;
        this.hooks.dismiss();
    }

    // Release window close state and associated lifecycle resources
    private async close(intent: NonNullable<WindowCloseCoordinator['current']>) {
        let fence: WindowFence | undefined;
        let paused = false;
        try {
            if (this.hooks.destination) {
                const destination = await this.hooks.destination();
                if (intent.cancelled) return;
                if (destination === 'cancel') {
                    await this.hooks.abort(intent.ticket);
                    return;
                }
                if (destination === 'restart') {
                    if (!this.hooks.restart) throw Error('Restart is unavailable.');
                    // Native restart supersedes this close ticket with the existing
                    // save-drafts/quit handshake. Do not discard or retire this editor
                    await this.hooks.restart();
                    return;
                }
            }
            const states = this.owners.snapshot();
            if (states.some(({ state }) => state !== 'clean')) {
                const discard = await this.hooks.prompt(
                    states.some(({ state }) => state === 'unknown'),
                );
                if (intent.cancelled) return;
                if (!discard) {
                    await this.hooks.abort(intent.ticket);
                    return;
                }
            }
            if (intent.cancelled) return;
            this.hooks.interactive(false);
            await this.hooks.confirm(intent.ticket);
            if (intent.cancelled) return;
            intent.confirmed = true;
            this.owners.setClosing(true);
            paused = true;
            this.hooks.pause();
            await this.owners.discardAll();
            await this.owners.releaseAll();
            await this.hooks.retire();
            await this.work.settled();
            fence = this.work.tryCloseFence(intent.ticket.generation, intent.ticket.action_id);
            if (!fence) throw Error('Editor work has not finished. Please try closing again.');
            await this.work.cleanup(fence, 'close editor', () => this.hooks.finish(intent.ticket));
            // Successful native close destroys this document. Keep autosave and input disabled
            // until then, including when the IPC completion precedes the Destroyed event
        } catch (error) {
            try {
                if (!intent.cancelled) {
                    if (fence && this.work.current(fence))
                        await this.work.cleanup(fence, 'abort close', () =>
                            this.hooks.abort(intent.ticket),
                        );
                    else await this.hooks.abort(intent.ticket);
                }
            } catch (abortError) {
                error = new Error(
                    `${String(error)}. Could not cancel close: ${String(abortError)}`,
                );
            }
            this.work.resume(fence);
            this.owners.setClosing(false);
            if (paused) {
                try {
                    await this.hooks.resume();
                } catch (resumeError) {
                    error = new Error(`${String(error)}. ${String(resumeError)}`);
                }
            }
            this.hooks.interactive(true);
            if (!intent.cancelled) this.hooks.failed(error);
        } finally {
            if (!intent.confirmed) this.hooks.interactive(true);
        }
    }
}
