/** Keep a live status mounted and its row the same size while a native unlock is pending. */
export function vaultUnlockProgress(dialog: HTMLDialogElement) {
    const row = dialog.querySelector<HTMLElement>('[data-vault-unlock-progress]')!;
    const spinner = row.querySelector<HTMLElement>('.review-spinner')!;
    const status = row.querySelector<HTMLElement>('[role=status]')!;
    return (pending: boolean) => {
        spinner.hidden = !pending;
        status.textContent = pending ? 'Unlocking…' : '';
    };
}
