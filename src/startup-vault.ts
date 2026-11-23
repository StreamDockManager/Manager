import { oldVaultMessage, vaultFormatError } from './vault-errors';
import { vaultUnlockProgress } from './vault-unlock-progress';
import type { VaultStatus } from '../generated/runtime';

/** Vault unlock protects dependent plugins natively; this gate only owns the startup prompt. */
export interface StartupVaultService {
    status(): Promise<VaultStatus>;
    unlockPassword(password: string): Promise<VaultStatus>;
}

export async function runStartupVault(
    service: StartupVaultService,
    dialog: HTMLDialogElement,
    signal: AbortSignal,
) {
    const status = await service.status().catch(() => null);
    if (signal.aborted || status?.state !== 'locked' || status.mode !== 'password') return;
    const unlock = dialog.querySelector<HTMLButtonElement>('[data-vault-startup-unlock]')!;
    const skip = dialog.querySelector<HTMLButtonElement>('[data-vault-startup-skip]')!;
    const error = dialog.querySelector<HTMLElement>('[data-vault-startup-error]')!;
    const password = dialog.querySelector<HTMLInputElement>('[data-vault-startup-password]')!;
    const showProgress = vaultUnlockProgress(dialog);
    function setPending(pending: boolean) {
        unlock.disabled = skip.disabled = password.readOnly = pending;
        showProgress(pending);
    }
    password.value = '';
    setPending(false);
    error.textContent = '';
    await new Promise<void>((resolve) => {
        let done = false;
        let pending = false;
        const handlers = new AbortController();
        function finish() {
            if (done) return;
            done = true;
            handlers.abort();
            signal.removeEventListener('abort', finish);
            password.value = '';
            setPending(false);
            dialog.close();
            error.textContent = '';
            resolve();
        }
        dialog.addEventListener('cancel', (event) => event.preventDefault(), {
            signal: handlers.signal,
        });
        dialog.addEventListener('close', finish, { signal: handlers.signal });
        password.addEventListener(
            'keydown',
            (event) => {
                if (event.key === 'Enter' && !pending) {
                    event.preventDefault();
                    unlock.click();
                }
            },
            { signal: handlers.signal },
        );
        skip.addEventListener('click', finish, { signal: handlers.signal });
        unlock.addEventListener(
            'click',
            async () => {
                if (done || pending || !password.value) return;
                pending = true;
                setPending(true);
                error.textContent = '';
                try {
                    const opening = service.unlockPassword(password.value);
                    password.value = '';
                    (await opening).state === 'unlocked' ? finish() : showFailure();
                } catch (failure) {
                    if (!done && vaultFormatError(failure)) error.textContent = oldVaultMessage;
                    else showFailure();
                } finally {
                    pending = false;
                    if (!done) {
                        password.value = '';
                        setPending(false);
                        password.focus();
                    }
                }
            },
            { signal: handlers.signal },
        );
        function showFailure() {
            if (!done) error.textContent = 'Vault could not be unlocked. Try again or choose Skip.';
        }
        signal.addEventListener('abort', finish, { once: true });
        if (signal.aborted) finish();
        else {
            dialog.showModal();
            password.focus();
        }
    });
}
