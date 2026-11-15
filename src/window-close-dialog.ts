import type AlpineType from '@alpinejs/csp';

interface WindowCloseDialog {
    unknown: boolean;
    recovery: boolean;
    readonly description: string;
    prompt(unknown: boolean, recovery?: boolean): Promise<boolean>;
    finish(discard: boolean): void;
    keepEditing(): void;
    discardAndClose(): void;
}

// Register the window close dialog
export function registerWindowCloseDialog(Alpine: typeof AlpineType) {
    let settle: ((discard: boolean) => void) | undefined;
    let pending: Promise<boolean> | undefined;
    // Locate the close-confirmation dialog without assuming it is mounted
    const dialog = () => document.getElementById('window-close-dialog') as HTMLDialogElement | null;
    const store: WindowCloseDialog = {
        unknown: false,
        recovery: false,
        // Derive description from the current window close dialog state
        get description() {
            const warning = this.unknown
                ? 'An open plugin editor may contain unsaved changes. Closing will discard unsaved changes.'
                : 'Unsaved changes will be discarded.';
            return (
                warning +
                (this.recovery
                    ? ' Recovery mode will stay active with plugins paused.'
                    : ' Plugins and devices will keep running.')
            );
        },
        // Share the pending close-confirmation promise and display the requested warning
        prompt(unknown: boolean, recovery = false): Promise<boolean> {
            if (pending) return pending;
            this.unknown = unknown;
            this.recovery = recovery;
            const element = dialog();
            if (!element) return Promise.reject(Error('The close confirmation is unavailable.'));
            pending = new Promise<boolean>((resolve) => {
                settle = resolve;
            });
            try {
                element.showModal();
            } catch (error) {
                settle = undefined;
                pending = undefined;
                return Promise.reject(error);
            }
            return pending;
        },
        // Finish the current window close dialog operation and retire its pending state
        finish(discard: boolean) {
            dialog()?.close();
            const resolve = settle;
            settle = undefined;
            pending = undefined;
            resolve?.(discard);
        },
        // Resolve the close prompt by keeping the editor open
        keepEditing() {
            this.finish(false);
        },
        // Resolve the close prompt by allowing pending edits to be discarded
        discardAndClose() {
            this.finish(true);
        },
    };
    Alpine.store('windowClose', store);
    return () => Alpine.store('windowClose') as WindowCloseDialog;
}
