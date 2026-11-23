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
        const supportsStyle = trigger.hasAttribute('data-font-style');
        const originalWeight = trigger.dataset.fontWeight;
        const originalItalic = trigger.dataset.fontItalic;
        let selectedWeight = Number(originalWeight) || 400;
        let selectedItalic = originalItalic === 'true';
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
        const PAGE_SIZE = 48;
        let pageIndex = 0;
        type Choice = { key: string; value: string; text: string; disabled: boolean };
        type Mounted = {
            choice: Choice;
            button: HTMLButtonElement;
            name: HTMLSpanElement;
            preview: HTMLSpanElement;
        };
        let catalog: Choice[] = [];
        let filtered: Choice[] = [];
        const mounted = new Map<string, Mounted>();
        const paging = document.createElement('div');
        paging.className = 'font-preview-pagination';
        paging.setAttribute('aria-label', 'Font result pages');
        const previous = document.createElement('button');
        previous.type = 'button';
        previous.className = 'button';
        previous.textContent = 'Previous';
        previous.setAttribute('aria-label', 'Previous fonts');
        const next = document.createElement('button');
        next.type = 'button';
        next.className = 'button';
        next.textContent = 'Next';
        next.setAttribute('aria-label', 'Next fonts');
        const count = document.createElement('span');
        count.setAttribute('aria-live', 'polite');
        paging.append(previous, count, next);
        const setText = (node: HTMLElement, text: string) => {
            if (node.textContent !== text) node.textContent = text;
        };
        // Only a bounded page owns preview nodes. Identity is value plus duplicate
        // occurrence, never display label; same-label distinct fonts remain distinct.
        const render = (focusKey?: string) => {
            pageIndex = Math.min(
                pageIndex,
                Math.max(0, Math.ceil(filtered.length / PAGE_SIZE) - 1),
            );
            const start = pageIndex * PAGE_SIZE;
            const visible = filtered.slice(start, start + PAGE_SIZE);
            const keys = new Set(visible.map((choice) => choice.key));
            const focused = [...mounted.values()].find(
                (item) => item.button === document.activeElement,
            )?.choice.key;
            for (const [key, item] of mounted) {
                if (!keys.has(key)) {
                    item.button.remove();
                    mounted.delete(key);
                }
            }
            visible.forEach((choice, index) => {
                let item = mounted.get(choice.key);
                if (!item) {
                    const button = document.createElement('button');
                    button.type = 'button';
                    button.className = 'font-preview-choice';
                    const name = document.createElement('span');
                    name.className = 'font-preview-name';
                    const preview = document.createElement('span');
                    preview.className = 'font-preview-sample';
                    button.append(name, preview);
                    item = { choice, button, name, preview };
                    const owned = item;
                    button.addEventListener('click', () => {
                        selected = owned.choice.value;
                        render();
                    });
                    mounted.set(choice.key, item);
                }
                item.choice = choice;
                if (item.button.disabled !== choice.disabled)
                    item.button.disabled = choice.disabled;
                if (item.button.getAttribute('aria-label') !== choice.text)
                    item.button.setAttribute('aria-label', choice.text);
                const pressed = String(choice.value === selected);
                if (item.button.getAttribute('aria-pressed') !== pressed)
                    item.button.setAttribute('aria-pressed', pressed);
                setText(item.name, choice.text);
                const family =
                    choice.value === 'default' ? 'var(--font)' : cssFontFamily(choice.value);
                if (item.preview.style.fontFamily !== family)
                    item.preview.style.fontFamily = family;
                item.preview.style.fontWeight = supportsStyle ? String(selectedWeight) : '400';
                item.preview.style.fontStyle =
                    supportsStyle && selectedItalic ? 'italic' : 'normal';
                setText(item.preview, sample.value || 'Aa 0123456789');
                if (list.children[index] !== item.button)
                    list.insertBefore(item.button, list.children[index] ?? null);
            });
            empty.hidden = filtered.length > 0;
            paging.hidden = filtered.length <= PAGE_SIZE;
            paging.style.display = paging.hidden ? 'none' : '';
            previous.disabled = pageIndex === 0;
            next.disabled = start + PAGE_SIZE >= filtered.length;
            setText(
                count,
                `${filtered.length ? start + 1 : 0}–${Math.min(start + PAGE_SIZE, filtered.length)} of ${filtered.length} fonts`,
            );
            const restore = focusKey ?? focused;
            if (restore) {
                const target = mounted.get(restore)?.button;
                if (target && !target.disabled) target.focus({ preventScroll: true });
                else search.focus({ preventScroll: true });
            }
        };
        const filter = () => {
            const query = search.value.trim().toLocaleLowerCase();
            filtered = catalog.filter((choice) => choice.text.toLocaleLowerCase().includes(query));
            pageIndex = 0;
            render();
        };
        const syncOptions = () => {
            const focused = [...mounted.values()].find(
                (item) => item.button === document.activeElement,
            )?.choice.key;
            const occurrences = new Map<string, number>();
            catalog = Array.from(select.options).map((option) => {
                const occurrence = occurrences.get(option.value) ?? 0;
                occurrences.set(option.value, occurrence + 1);
                return {
                    key: JSON.stringify([option.value, occurrence]),
                    value: option.value,
                    text: option.text,
                    disabled:
                        option.disabled ||
                        (option.parentElement instanceof HTMLOptGroupElement &&
                            option.parentElement.disabled),
                };
            });
            const query = search.value.trim().toLocaleLowerCase();
            filtered = catalog.filter((choice) => choice.text.toLocaleLowerCase().includes(query));
            const focusIndex = filtered.findIndex((choice) => choice.key === focused);
            if (focusIndex >= 0) pageIndex = Math.floor(focusIndex / PAGE_SIZE);
            render(focused);
        };
        const movePage = (direction: number) => {
            pageIndex += direction;
            render();
            // The pager button may become disabled on the last/first page.
            const target = [...mounted.values()].find((item) => !item.button.disabled)?.button;
            target?.focus();
        };
        previous.addEventListener('click', () => movePage(-1));
        next.addEventListener('click', () => movePage(1));
        list.addEventListener('keydown', (event) => {
            if (
                !['ArrowDown', 'ArrowUp', 'ArrowRight', 'ArrowLeft', 'Home', 'End'].includes(
                    event.key,
                )
            )
                return;
            const current = [...mounted.values()].find((item) => item.button === event.target);
            if (!current) return;
            const enabled = filtered.filter((choice) => !choice.disabled);
            const index = enabled.findIndex((choice) => choice.key === current.choice.key);
            const step = ['ArrowUp', 'ArrowLeft'].includes(event.key) ? -1 : 1;
            const target =
                enabled[
                    event.key === 'Home'
                        ? 0
                        : event.key === 'End'
                          ? enabled.length - 1
                          : Math.max(0, Math.min(enabled.length - 1, index + step))
                ];
            if (!target) return;
            event.preventDefault();
            pageIndex = Math.floor(filtered.indexOf(target) / PAGE_SIZE);
            render(target.key);
            mounted.get(target.key)?.button.scrollIntoView({ block: 'nearest' });
        });
        const styleControls = document.createElement('div');
        styleControls.className = 'font-preview-style-controls';
        {
            const weightLabel = document.createElement('label');
            weightLabel.textContent = 'Weight';
            const weight = document.createElement('select');
            weight.setAttribute('aria-label', 'Font weight');
            weight.disabled = !supportsStyle;
            const labels = [
                'Thin',
                'Extra light',
                'Light',
                'Regular',
                'Medium',
                'Semibold',
                'Bold',
                'Extra bold',
                'Black',
            ];
            const weights = !supportsStyle
                ? [400]
                : trigger.dataset.fontStyle === 'label'
                  ? [400, 700]
                  : [100, 200, 300, 400, 500, 600, 700, 800, 900];
            for (const value of weights) {
                const option = new Option(`${labels[value / 100 - 1]} (${value})`, String(value));
                weight.add(option);
            }
            if (trigger.dataset.fontStyle !== 'label' && selectedWeight % 100 !== 0)
                weight.add(new Option(String(selectedWeight), String(selectedWeight)));
            weight.value = String(selectedWeight);
            weight.addEventListener('change', () => {
                selectedWeight = Number(weight.value);
                render();
            });
            weightLabel.append(weight);
            const italicLabel = document.createElement('label');
            const italic = document.createElement('input');
            italic.disabled = !supportsStyle;
            italic.type = 'checkbox';
            italic.checked = selectedItalic;
            italic.setAttribute('aria-label', 'Italic');
            italic.addEventListener('change', () => {
                selectedItalic = italic.checked;
                render();
            });
            italicLabel.append(italic, document.createTextNode('Italic'));
            styleControls.append(weightLabel, italicLabel);
        }
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
            for (const item of mounted.values())
                setText(item.preview, sample.value || 'Aa 0123456789');
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
                (!supportsStyle ||
                    (trigger.dataset.fontWeight === originalWeight &&
                        trigger.dataset.fontItalic === originalItalic)) &&
                Array.from(select.options).some(
                    (option) =>
                        option.value === selected &&
                        !option.disabled &&
                        !(
                            option.parentElement instanceof HTMLOptGroupElement &&
                            option.parentElement.disabled
                        ),
                )
            ) {
                if (supportsStyle) {
                    select.dispatchEvent(
                        new CustomEvent('font-style-apply', {
                            bubbles: true,
                            detail: {
                                font: selected,
                                fontWeight: selectedWeight,
                                fontItalic: selectedItalic,
                            },
                        }),
                    );
                } else {
                    select.value = selected;
                    select.dispatchEvent(new Event('input', { bubbles: true }));
                    select.dispatchEvent(new Event('change', { bubbles: true }));
                }
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
        body.append(search, sample);
        body.append(styleControls);
        if (!supportsStyle) {
            const supportNote = document.createElement('small');
            supportNote.className = 'field-help';
            supportNote.textContent = 'This element supports font family only.';
            body.append(supportNote);
        }
        body.append(list, empty, paging);
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
