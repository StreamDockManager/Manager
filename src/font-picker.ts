import { cssFontFamily } from './display-fonts';

/** Enhance the existing font fields without changing their draft or persistence owners */
export function registerFontPicker() {
    let active: HTMLDialogElement | undefined;
    // Open the font chooser for the requested selection target
    const launch = (event: MouseEvent) => {
        const trigger = (event.target as Element).closest<HTMLButtonElement>(
            '[data-preview-fonts]',
        );
        if (!trigger || trigger.disabled || active) return;
        const select = trigger.previousElementSibling;
        if (!(select instanceof HTMLSelectElement) || select.matches(':disabled')) return;
        event.preventDefault();
        const original = select.value;
        let selected = original;
        const dialog = document.createElement('dialog');
        active = dialog;
        dialog.className = 'feature-dialog font-preview-dialog';
        dialog.setAttribute('aria-label', 'Choose a font');
        const header = document.createElement('header');
        header.className = 'picker-header';
        const title = document.createElement('h2');
        title.textContent = 'Choose a font';
        header.append(title);
        const body = document.createElement('div');
        body.className = 'feature-body font-preview-body';
        const search = document.createElement('input');
        search.type = 'search';
        search.placeholder = 'Search fonts';
        search.setAttribute('aria-label', 'Search fonts');
        const sample = document.createElement('input');
        sample.value = 'The quick brown fox 0123456789';
        sample.maxLength = 160;
        sample.setAttribute('aria-label', 'Font preview text');
        const list = document.createElement('div');
        list.className = 'font-preview-list';
        list.setAttribute('role', 'group');
        list.setAttribute('aria-label', 'Fonts');
        const empty = document.createElement('p');
        empty.textContent = 'No matching fonts';
        empty.hidden = true;
        empty.setAttribute('role', 'status');
        let options: {
            button: HTMLButtonElement;
            preview: HTMLSpanElement;
            value: string;
            label: string;
        }[] = [];
        // Filter the available font choices using the current search text
        const filter = () => {
            const query = search.value.trim().toLocaleLowerCase();
            for (const item of options) item.button.hidden = !item.label.includes(query);
            empty.hidden = options.some((item) => !item.button.hidden);
        };
        // Synchronize the visible font choices and selected value
        const syncOptions = () => {
            const focused = options.find((item) => item.button === document.activeElement)?.value;
            list.replaceChildren();
            // Create an accessible font choice with a live preview
            options = Array.from(select.options).map((option) => {
                const button = document.createElement('button');
                button.type = 'button';
                button.className = 'font-preview-choice';
                button.disabled = option.disabled;
                button.setAttribute('aria-label', option.text);
                button.setAttribute('aria-pressed', String(option.value === selected));
                const name = document.createElement('span');
                name.className = 'font-preview-name';
                name.textContent = option.text;
                const preview = document.createElement('span');
                preview.className = 'font-preview-sample';
                // Interface's Default means the application's default stack, not Noto Sans
                preview.style.fontFamily =
                    option.value === 'default' ? 'var(--font)' : cssFontFamily(option.value);
                preview.textContent = sample.value || 'Aa 0123456789';
                button.append(name, preview);
                // Update the selected font and its accessible pressed state
                button.addEventListener('click', () => {
                    selected = option.value;
                    for (const item of options)
                        item.button.setAttribute('aria-pressed', String(item.value === selected));
                });
                list.append(button);
                return {
                    button,
                    preview,
                    value: option.value,
                    label: option.text.toLocaleLowerCase(),
                };
            });
            filter();
            if (focused) options.find((item) => item.value === focused)?.button.focus();
        };
        syncOptions();
        // Discovery/refresh can finish after the modal opens. Keep the staged choice,
        // sample and search while reflecting the source field's current catalog
        const observer = new MutationObserver(syncOptions);
        observer.observe(select, {
            childList: true,
            subtree: true,
            characterData: true,
            attributes: true,
        });
        search.addEventListener('input', filter);
        // Refresh every font sample when the preview text changes
        sample.addEventListener('input', () => {
            for (const item of options) item.preview.textContent = sample.value || 'Aa 0123456789';
        });
        const footer = document.createElement('footer');
        footer.className = 'feature-footer';
        const cancel = document.createElement('button');
        cancel.type = 'button';
        cancel.className = 'button';
        cancel.textContent = 'Cancel';
        cancel.addEventListener('click', () => dialog.close());
        const apply = document.createElement('button');
        apply.type = 'button';
        apply.className = 'button primary';
        apply.textContent = 'Apply';
        apply.addEventListener('click', () => {
            // A detached/replaced field must never receive a stale modal selection
            if (
                select.isConnected &&
                !select.matches(':disabled') &&
                select.value === original &&
                Array.from(select.options).some(
                    (option) => option.value === selected && !option.disabled,
                )
            ) {
                select.value = selected;
                select.dispatchEvent(new Event('input', { bubbles: true }));
                select.dispatchEvent(new Event('change', { bubbles: true }));
            }
            dialog.close();
        });
        dialog.addEventListener(
            'close',
            // Remove the completed dialog and release its mutation observer
            () => {
                observer.disconnect();
                dialog.remove();
                if (active === dialog) active = undefined;
                if (trigger.isConnected) trigger.focus();
            },
            { once: true },
        );
        body.append(search, sample, list, empty);
        footer.append(cancel, apply);
        dialog.append(header, body, footer);
        document.body.append(dialog);
        dialog.showModal();
        search.focus();
    };
    document.addEventListener('click', launch);
    // Detach the chooser and close any active font dialog
    return () => {
        document.removeEventListener('click', launch);
        active?.close();
        active?.remove();
        active = undefined;
    };
}
