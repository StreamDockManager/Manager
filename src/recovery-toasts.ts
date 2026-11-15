import { ToastState, type ToastLevel } from './toasts';
/** The shared toast state/styles, with a DOM renderer for the runtime-free recovery page. */
export class RecoveryToasts extends ToastState {
    private host = document.createElement('section');
    private nodes = new Map<number, HTMLElement>();
    private sticky = new Set<number>();
    private dialogs: HTMLDialogElement[] = [];
    private observer: MutationObserver;
    // Initialize recovery toasts state and its required lifecycle hooks
    constructor() {
        super();
        this.host.className = 'toast-region recovery-toast-region';
        this.host.setAttribute('aria-label', 'Notifications');
        this.host.setAttribute('popover', 'manual');
        document.body.append(this.host);
        this.observer = new MutationObserver((records) => this.position(records));
        this.observer.observe(document.body, {
            subtree: true,
            childList: true,
            attributes: true,
            attributeFilter: ['open'],
        });
        this.position();
    }
    // Notify listeners that recovery toasts state changed
    override notify(message: string, level: ToastLevel = 'info') {
        const id = super.notify(message, level);
        if (id !== undefined && level === 'error') {
            this.sticky.add(id);
            super.setToastPaused(id, true);
        }
        this.render();
        return id;
    }
    // Set the toast paused
    override setToastPaused(id: number, paused: boolean) {
        super.setToastPaused(id, this.sticky.has(id) || paused);
    }
    // Dismiss the toast
    override dismissToast(id: number) {
        super.dismissToast(id);
        this.sticky.delete(id);
        this.render();
    }
    // Build the recovery toasts view from the current state
    private render() {
        if (this.stopped) return;
        for (const [id, node] of this.nodes)
            if (!this.toasts.some((toast) => toast.id === id)) {
                node.remove();
                this.nodes.delete(id);
            }
        for (const toast of this.toasts) {
            if (this.nodes.has(toast.id)) continue;
            const card = document.createElement('div');
            card.className = `toast ${toast.level}`;
            card.setAttribute('role', toast.level === 'error' ? 'alert' : 'status');
            card.setAttribute('aria-live', toast.level === 'error' ? 'assertive' : 'polite');
            card.setAttribute('aria-atomic', 'true');
            const icon = document.createElement('span');
            icon.className = 'toast-icon';
            icon.textContent = toast.icon;
            icon.setAttribute('aria-hidden', 'true');
            const content = document.createElement('div');
            content.className = 'toast-content';
            const title = document.createElement('strong');
            title.className = 'toast-title';
            title.textContent = toast.title;
            const message = document.createElement('p');
            message.textContent = toast.message;
            content.append(title, message);
            const dismiss = document.createElement('button');
            dismiss.className = 'icon-button toast-dismiss';
            dismiss.setAttribute('aria-label', `Dismiss ${toast.level} notification`);
            dismiss.textContent = '×';
            dismiss.addEventListener('click', () => this.dismissToast(toast.id));
            for (const event of ['pointerenter', 'pointerleave', 'focusin', 'focusout'])
                card.addEventListener(event, () =>
                    queueMicrotask(() =>
                        this.setToastPaused(toast.id, card.matches(':hover, :focus-within')),
                    ),
                );
            card.append(icon, content, dismiss);
            this.nodes.set(toast.id, card);
            this.host.append(card);
        }
        this.position();
    }
    // Calculate the current recovery toasts position
    private position(records: MutationRecord[] = []) {
        if (this.stopped) return;
        this.dialogs = this.dialogs.filter(
            (dialog) => dialog.isConnected && dialog.matches(':modal'),
        );
        for (const record of records)
            if (record.attributeName === 'open' && record.target instanceof HTMLDialogElement) {
                this.dialogs = this.dialogs.filter((dialog) => dialog !== record.target);
                if (record.target.matches(':modal')) this.dialogs.push(record.target);
            }
        for (const dialog of document.querySelectorAll<HTMLDialogElement>('dialog:modal'))
            if (!this.dialogs.includes(dialog)) this.dialogs.push(dialog);
        const owner = this.dialogs.at(-1) || document.body;
        const changed = records.some(
            (record) => record.attributeName === 'open' && record.target === owner,
        );
        if (this.host.matches(':popover-open') && (this.host.parentElement !== owner || changed))
            this.host.hidePopover();
        if (this.host.parentElement !== owner) owner.append(this.host);
        if (!this.host.matches(':popover-open')) this.host.showPopover();
        this.host.classList.toggle('above-focus', this.dialogs.length > 0);
    }
    // Release recovery toasts state and associated lifecycle resources
    destroy() {
        this.stopped = true;
        this.observer.disconnect();
        this.clearToasts();
        this.host.remove();
        this.nodes.clear();
        this.sticky.clear();
    }
}
