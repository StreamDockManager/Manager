import { windowCloseOwners } from './window-close-owners';

interface DialogView {
    $refs: Record<string, HTMLElement>;
    $nextTick: (callback: () => void) => unknown;
    error: string;
    dialogEpoch: number;
}

/** Helpers hold no state: each workflow owns its own operation and feedback */
export async function runFeatureWork(
    owner: { working: boolean; error: string },
    work: () => Promise<void>,
) {
    if (owner.working) return;
    owner.working = true;
    owner.error = '';
    try {
        await work();
    } catch (error) {
        owner.error = String(error);
    } finally {
        owner.working = false;
    }
}

// Open the current dialog generation after its DOM update
export function showFeatureDialog(owner: DialogView, name: string) {
    if (windowCloseOwners.closing) return;
    const epoch = ++owner.dialogEpoch;
    // Ignore deferred dialog opens after closure or a newer dialog generation
    owner.$nextTick(() => {
        if (windowCloseOwners.closing || epoch !== owner.dialogEpoch) return;
        const dialog = owner.$refs[name] as HTMLDialogElement | undefined;
        if (dialog && !dialog.open) dialog.showModal();
    });
}

// Invalidate deferred opens and close the referenced dialog
export function closeFeatureDialog(owner: DialogView, name: string) {
    ++owner.dialogEpoch;
    const dialog = owner.$refs[name] as HTMLDialogElement | undefined;
    if (dialog?.open) dialog.close();
    owner.error = '';
}
