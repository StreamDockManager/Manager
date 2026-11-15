import Alpine from '@alpinejs/csp';
import { bridge, native } from './bridge';
import type { RecoveryStatus } from '../generated/runtime';
type CloseDestination = 'close' | 'restart' | 'cancel';

// Register the recovery
export function registerRecovery() {
    let settle: ((choice: CloseDestination) => void) | undefined;
    let pending: Promise<CloseDestination> | undefined;
    let closeGeneration = 0;
    // Close the dialog
    const closeDialog = () => document.getElementById('recovery-close-dialog') as HTMLDialogElement;
    const store = {
        status: { active: false, prompt: false, interrupted: [] } as RecoveryStatus,
        // Refresh recovery data while keeping the view synchronized
        async refresh() {
            this.status = await bridge.command('recovery_status');
        },
        // Request exit from recovery mode and refresh the browser simulator after its simulated restart
        async exit() {
            await bridge.command('exit_recovery_mode');
            // The preview simulates a restart. Native shutdown owns the draft flush
            // and replaces this document with a normal startup
            if (!native) await this.refresh();
        },
        // Refresh recovery status and resolve the close destination through a shared confirmation prompt
        async destination(): Promise<CloseDestination> {
            const generation = ++closeGeneration;
            await this.refresh();
            if (generation !== closeGeneration) return 'cancel';
            if (!this.status.active) return 'close';
            if (pending) return pending;
            const element = closeDialog();
            if (!element) throw Error('The recovery close confirmation is unavailable.');
            pending = new Promise<CloseDestination>((resolve) => {
                settle = resolve;
            });
            try {
                element.showModal();
            } catch (error) {
                settle = undefined;
                pending = undefined;
                throw error;
            }
            return pending;
        },
        // Finish the current recovery operation and retire its pending state
        finish(choice: CloseDestination) {
            closeGeneration++;
            closeDialog()?.close();
            const resolve = settle;
            settle = undefined;
            pending = undefined;
            resolve?.(choice);
        },
        // Cancel the close
        cancelClose() {
            this.finish('cancel');
        },
        // Send the to taskbar
        sendToTaskbar() {
            this.finish('close');
        },
        // Resolve the recovery close prompt with a normal-restart destination
        restartNormally() {
            this.finish('restart');
        },
    };
    Alpine.store('recovery', store);
    // Access the registered recovery store
    const recovery = () => Alpine.store('recovery') as typeof store;
    Alpine.data('startupRecovery', () => ({
        // Derive status from the current recovery state
        get status() {
            return recovery().status;
        },
        error: '',
        busy: false,
        // Initialize recovery state and its required lifecycle hooks
        async init() {
            try {
                await recovery().refresh();
                if (this.status.prompt)
                    this.$nextTick(() => (this.$refs.prompt as HTMLDialogElement).showModal());
            } catch (error) {
                this.error = String(error);
            }
        },
        // Choose the active item for the recovery workflow
        async choose(normal: boolean) {
            if (this.busy) return;
            this.busy = true;
            try {
                recovery().status = await bridge.command('resolve_interrupted_startup', { normal });
                (this.$refs.prompt as HTMLDialogElement).close();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
        // Choose to retain recovery mode in the startup decision
        recovery() {
            return this.choose(false);
        },
        // Choose normal startup instead of retaining recovery mode
        normal() {
            return this.choose(true);
        },
        // Request recovery exit once at a time and expose failures in the dialog
        async exit() {
            if (this.busy) return;
            this.busy = true;
            this.error = '';
            try {
                await recovery().exit();
            } catch (error) {
                this.error = String(error);
            } finally {
                this.busy = false;
            }
        },
    }));
    return recovery;
}
