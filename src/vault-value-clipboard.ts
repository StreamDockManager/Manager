// Write only on an explicit user gesture; never log or copy a value automatically
export async function copyVaultValueText(text: string): Promise<void> {
    if (typeof navigator.clipboard?.writeText === 'function') {
        await navigator.clipboard.writeText(text);
        return;
    }

    // Older WebViews may lack the Clipboard API. Keep focus inside an open modal
    const previous = document.activeElement;
    const host = previous?.closest('dialog[open]') ?? document.body;
    const field = document.createElement('textarea');
    field.className = 'vault-copy-buffer';
    field.readOnly = true;
    field.value = text;
    field.tabIndex = -1;
    host.append(field);
    try {
        field.focus({ preventScroll: true });
        field.select();
        if (!document.execCommand('copy')) throw Error('Clipboard is unavailable');
    } finally {
        field.value = '';
        field.remove();
        if (previous instanceof HTMLElement && previous.isConnected)
            previous.focus({ preventScroll: true });
    }
}
