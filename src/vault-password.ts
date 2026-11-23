import { bridge } from './bridge';
import { vaultFormatError } from './vault-errors';
import { vaultUnlockProgress } from './vault-unlock-progress';
import type { VaultStatus } from '../generated/runtime';

type Operation = 'create' | 'unlock' | 'change' | 'toPassword' | 'toOs';

export async function unlockVault() {
    const status = await bridge.command('vault_status');
    if (status.state === 'unlocked') return status;
    return status.mode === 'password' ? passwordDialog('unlock') : bridge.command('vault_unlock');
}

export function passwordDialog(operation: Operation): Promise<VaultStatus> {
    const dialog = document.querySelector<HTMLDialogElement>('#vault-password')!;
    if (dialog.open) return Promise.reject(Error('review_pending'));
    const form = dialog.querySelector('form')!;
    const title = dialog.querySelector('h2')!;
    const current = form.elements.namedItem('current') as HTMLInputElement;
    const password = form.elements.namedItem('password') as HTMLInputElement;
    const confirmation = form.elements.namedItem('confirmation') as HTMLInputElement;
    const error = dialog.querySelector<HTMLElement>('[role=alert]')!;
    const submit = dialog.querySelector<HTMLButtonElement>('[type=submit]')!;
    const cancel = dialog.querySelector<HTMLButtonElement>('[data-cancel]')!;
    const setting = operation !== 'unlock' && operation !== 'toOs';
    title.textContent = {
        toPassword: 'Protect Vault with a master password',
        toOs: 'Use the operating system key store',
        create: 'Create a password-protected Vault',
        unlock: 'Unlock Vault',
        change: 'Change master password',
    }[operation];
    submit.textContent = {
        create: 'Create Vault',
        unlock: 'Unlock',
        change: 'Change password',
        toPassword: 'Switch to master password',
        toOs: 'Switch to OS protection',
    }[operation];
    dialog.querySelector<HTMLElement>('[data-warning]')!.hidden = !setting;
    dialog.querySelector<HTMLElement>('[data-password-label]')!.textContent = setting
        ? 'New master password'
        : operation === 'unlock'
          ? 'Master password'
          : 'Current master password';
    current.closest('label')!.hidden = current.disabled = operation !== 'change';
    confirmation.closest('label')!.hidden = confirmation.disabled = !setting;
    const showProgress = vaultUnlockProgress(dialog);
    dialog.querySelector<HTMLElement>('[data-vault-unlock-progress]')!.hidden =
        operation !== 'unlock';
    function setPending(pending: boolean) {
        submit.disabled = cancel.disabled = pending;
        if (operation === 'unlock') {
            password.readOnly = pending;
            showProgress(pending);
        }
    }
    form.reset();
    error.textContent = '';
    setPending(false);
    const listeners = new AbortController();
    const opts = { signal: listeners.signal };
    return new Promise((resolve, reject) => {
        const finish = (status?: VaultStatus) => {
            if (listeners.signal.aborted) return;
            listeners.abort();
            form.reset();
            error.textContent = '';
            setPending(false);
            dialog.close();
            if (status) resolve(status);
            else bridge.command('vault_status').then(resolve, reject);
        };
        const dismiss = () => finish();
        cancel.addEventListener('click', dismiss, opts);
        dialog.addEventListener(
            'cancel',
            (event) => {
                event.preventDefault();
                if (!submit.disabled) dismiss();
            },
            opts,
        );
        dialog.addEventListener('close', dismiss, opts);
        window.addEventListener('hashchange', dismiss, opts);
        window.addEventListener('vault-session-ended', dismiss, opts);
        window.addEventListener('pagehide', dismiss, opts);
        form.addEventListener(
            'submit',
            async (event) => {
                event.preventDefault();
                if (submit.disabled) return;
                if (setting && password.value !== confirmation.value) {
                    confirmation.value = '';
                    error.textContent = 'The passwords must match.';
                    confirmation.focus();
                    return;
                }
                error.textContent = '';
                setPending(true);
                try {
                    let pending: Promise<VaultStatus>;
                    if (operation === 'unlock')
                        pending = bridge.command('vault_unlock_password', {
                            password: password.value,
                        });
                    else if (operation === 'create')
                        pending = bridge.command('vault_create_with_password', {
                            password: password.value,
                            confirmation: confirmation.value,
                        });
                    else if (operation === 'toOs' || operation === 'toPassword')
                        pending = bridge.command('vault_switch_unlock_mode', {
                            toPassword: operation === 'toPassword',
                            current: operation === 'toOs' ? password.value : '',
                            password: operation === 'toPassword' ? password.value : '',
                            confirmation: operation === 'toPassword' ? confirmation.value : '',
                        });
                    else
                        pending = bridge.command('vault_change_password', {
                            current: current.value,
                            password: password.value,
                            confirmation: confirmation.value,
                        });
                    form.reset();
                    finish(await pending);
                } catch (failure) {
                    if (listeners.signal.aborted) return;
                    if (vaultFormatError(failure)) {
                        // Close the password prompt so the Vault reset control is reachable
                        finish();
                        return;
                    }
                    error.textContent =
                        'Vault could not complete the request. Check the password and try again.';
                    form.reset();
                    setPending(false);
                    (operation === 'change' ? current : password).focus();
                }
            },
            opts,
        );
        dialog.showModal();
        (operation === 'change' ? current : password).focus();
    });
}
