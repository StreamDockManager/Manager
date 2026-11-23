export function transferPassword(creating: boolean, message = ''): Promise<string | null> {
    const dialog = document.querySelector<HTMLDialogElement>('#transfer-password')!;
    if (dialog.open) throw Error('Finish the current transfer first.');
    const form = dialog.querySelector('form')!;
    const input = form.elements.namedItem('password') as HTMLInputElement;
    const confirmation = form.elements.namedItem('confirmation') as HTMLInputElement;
    const error = form.querySelector<HTMLElement>('[role=alert]')!;
    form.reset();
    dialog.querySelector('h2')!.textContent = creating
        ? 'Set an export password'
        : 'Unlock this transfer';
    input.autocomplete = creating ? 'new-password' : 'current-password';
    confirmation.closest('label')!.hidden = confirmation.disabled = !creating;
    error.textContent = message;
    error.hidden = !message;
    const listeners = new AbortController();
    const options = { signal: listeners.signal };
    return new Promise((resolve) => {
        const finish = (value: string | null) => {
            listeners.abort();
            form.reset();
            dialog.close();
            resolve(value);
        };
        // A previous attempt's close event can arrive after the retry dialog has opened.
        dialog.addEventListener(
            'close',
            () => {
                if (!dialog.open) finish(null);
            },
            options,
        );
        window.addEventListener('hashchange', () => finish(null), options);
        form.querySelector('[data-cancel]')!.addEventListener('click', () => finish(null), options);
        form.addEventListener(
            'submit',
            (event) => {
                event.preventDefault();
                if (creating && input.value !== confirmation.value) {
                    error.textContent = 'The passwords must match.';
                    error.hidden = false;
                    confirmation.focus();
                    return;
                }
                finish(input.value);
            },
            options,
        );
        dialog.showModal();
        input.focus();
    });
}

export async function readTransfer<T>(read: (password: string | null) => Promise<T>): Promise<T> {
    let password: string | null = null;
    for (;;) {
        try {
            return await read(password);
        } catch (error) {
            const message = String(error);
            if (
                !message.includes('This transfer requires its export password.') &&
                !message.includes('The transfer password is incorrect or the file is damaged.')
            )
                throw error;
            password = await transferPassword(
                false,
                password ? 'The password is incorrect or the file is damaged.' : '',
            );
            if (!password) throw Error('Import cancelled.');
        }
    }
}
