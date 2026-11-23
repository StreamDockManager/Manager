import type Alpine from '@alpinejs/csp';

export type ToastLevel = 'info' | 'success' | 'warning' | 'error';
export interface Toast {
    id: number;
    level: ToastLevel;
    title: string;
    icon: string;
    message: string;
}
export const toastDuration = 5_000;
export const toastLimit = 4;
export const toastLevels: Record<ToastLevel, { title: string; icon: string }> = {
    info: { title: 'Information', icon: 'i' },
    success: { title: 'Success', icon: '✓' },
    warning: { title: 'Warning', icon: '!' },
    error: { title: 'Error', icon: '×' },
};

export class ToastState {
    toasts: Toast[] = [];
    private nextToastId = 0;
    private toastTimers = new Map<
        number,
        {
            remaining: number;
            expires: number;
            timer?: ReturnType<typeof setTimeout>;
        }
    >();
    protected stopped = false;
    // Notify listeners that toasts state changed
    notify(message: string, level: ToastLevel = 'info') {
        if (!message.trim() || this.stopped) return;
        // Repeated runtime errors must not flood the stack or keep extending a toast's lifetime
        const duplicate = this.toasts.find(
            (toast) => toast.message === message && toast.level === level,
        );
        if (duplicate) return duplicate.id;
        if (this.toasts.length >= toastLimit) this.dismissToast(this.toasts[0].id);
        const id = ++this.nextToastId;
        this.toasts = [...this.toasts, { id, level, message, ...toastLevels[level] }];
        this.toastTimers.set(id, { remaining: toastDuration, expires: 0 });
        this.setToastPaused(id, false);
        return id;
    }
    // Set the toast paused
    setToastPaused(id: number, paused: boolean) {
        const state = this.toastTimers.get(id);
        if (!state) return;
        if (paused && state.timer !== undefined) {
            clearTimeout(state.timer);
            state.timer = undefined;
            state.remaining = Math.max(0, state.expires - Date.now());
        } else if (!paused && state.timer === undefined) {
            state.expires = Date.now() + state.remaining;
            state.timer = setTimeout(() => this.dismissToast(id), state.remaining);
        }
    }
    // Report a toast's remaining lifetime without advancing a paused timer
    toastTiming(id: number) {
        const state = this.toastTimers.get(id);
        if (!state) return;
        const paused = state.timer === undefined;
        return {
            paused,
            remaining: paused ? state.remaining : Math.max(0, state.expires - Date.now()),
        };
    }
    // Dismiss the toast
    dismissToast(id: number) {
        const timer = this.toastTimers.get(id)?.timer;
        if (timer !== undefined) clearTimeout(timer);
        this.toastTimers.delete(id);
        this.toasts = this.toasts.filter((toast) => toast.id !== id);
    }
    // Clear the toasts
    clearToasts() {
        for (const toast of this.toasts) this.dismissToast(toast.id);
    }
}

// Register the toasts
export function registerToasts(alpine: typeof Alpine) {
    // Complete the deferred toasts work using the current operation state
    alpine.data('toasts', () => {
        let home: HTMLElement | null = null;
        let observer: MutationObserver | undefined;
        let resize: ResizeObserver | undefined;
        let events: AbortController | undefined;
        let dialogs: HTMLDialogElement[] = [];
        const progress = new Map<HTMLElement, Animation>();
        return {
            // Initialize toasts state and its required lifecycle hooks
            init() {
                home = this.$el.parentElement;
                // Render outside dialog layout in a viewport-fixed popover. Native modals
                // still require DOM ownership of the stack to keep its controls non-inert
                observer = new MutationObserver((records) => this.position(records));
                observer.observe(document.documentElement, {
                    childList: true,
                    subtree: true,
                    attributes: true,
                    attributeFilter: ['open', 'data-reduced-motion'],
                });
                events = new AbortController();
                // Update the current toasts state
                const update = () => this.avoidFocus();
                document.addEventListener('focusin', update, { signal: events.signal });
                document.addEventListener('scroll', update, {
                    capture: true,
                    passive: true,
                    signal: events.signal,
                });
                window.addEventListener('resize', update, { signal: events.signal });
                resize = new ResizeObserver(update);
                resize.observe(this.$el);
                this.position();
            },
            // Pause the current toasts operation
            pause(event: Event, id: number) {
                const item = event.currentTarget as HTMLElement;
                const app = alpine.store('app') as ToastState;
                // Complete the deferred toasts work using the current operation state
                queueMicrotask(() => {
                    app.setToastPaused(id, item.matches(':hover, :focus-within'));
                    this.syncProgress();
                });
            },
            // Sync the progress
            syncProgress() {
                const app = alpine.store('app') as ToastState;
                const easing =
                    document.documentElement.dataset.reducedMotion === 'true'
                        ? 'steps(20, end)'
                        : 'linear';
                for (const [bar, animation] of progress) {
                    if (!this.$el.contains(bar)) {
                        animation.cancel();
                        progress.delete(bar);
                    }
                }
                for (const bar of this.$el.querySelectorAll<HTMLElement>('[data-toast-progress]')) {
                    const timing = app.toastTiming(Number(bar.dataset.toastProgress));
                    if (!timing) continue;
                    let animation = progress.get(bar);
                    if (!animation) {
                        animation = bar.animate(
                            [{ transform: 'scaleX(1)' }, { transform: 'scaleX(0)' }],
                            { duration: toastDuration, fill: 'both', easing },
                        );
                        progress.set(bar, animation);
                    }
                    animation.effect?.updateTiming({ easing });
                    // Use the dismissal timer, including time elapsed before rendering or
                    // moving between modal owners, instead of starting a separate countdown
                    animation.currentTime = toastDuration - timing.remaining;
                    if (timing.paused || timing.remaining === 0) animation.pause();
                    else animation.play();
                }
            },
            // Reposition the toast stack to avoid covering the currently focused element
            avoidFocus() {
                const active = document.activeElement;
                if (active && this.$el.contains(active)) return;
                const stack = this.$el.getBoundingClientRect();
                const focus =
                    active && active !== document.body && active !== document.documentElement
                        ? active.getBoundingClientRect()
                        : undefined;
                const footer = dialogs.at(-1)?.querySelector('footer')?.getBoundingClientRect();
                // Measure vertical overlap only when the focused rectangle crosses the toast stack
                const overlap = (rect: DOMRect | undefined, top: number) =>
                    rect && rect.right > stack.left && rect.left < stack.right
                        ? Math.max(
                              0,
                              Math.min(rect.bottom, top + stack.height) - Math.max(rect.top, top),
                          )
                        : 0;
                const bottom = innerHeight - 20 - stack.height;
                // A new toast can land under the pointer and pause indefinitely over a
                // dialog's Close/Apply buttons. Keep those actions reachable too
                if (this.$el.matches(':hover') && !overlap(footer, bottom)) return;
                const moveAbove =
                    overlap(focus, 20) === overlap(focus, bottom)
                        ? overlap(footer, 20) < overlap(footer, bottom)
                        : overlap(focus, 20) < overlap(focus, bottom);
                this.$el.classList.toggle('above-focus', moveAbove);
            },
            // Calculate the current toasts position
            position(records: MutationRecord[] = []) {
                // Shared pickers can precede their parent in the DOM. The top layer
                // follows opening order, so keep that order rather than tree order
                if (
                    records.length &&
                    this.$el.isConnected &&
                    this.$el.parentElement === (dialogs.at(-1) || home) &&
                    !dialogs.some((dialog) => !dialog.isConnected) &&
                    !records.some(
                        (record) =>
                            this.$el.contains(record.target) ||
                            record.attributeName === 'data-reduced-motion' ||
                            (record.attributeName === 'open' &&
                                record.target instanceof HTMLDialogElement),
                    )
                )
                    return;
                dialogs = dialogs.filter(
                    (dialog) => dialog.isConnected && dialog.matches(':modal'),
                );
                for (const record of records) {
                    if (
                        record.attributeName !== 'open' ||
                        !(record.target instanceof HTMLDialogElement)
                    )
                        continue;
                    const dialog = record.target;
                    dialogs = dialogs.filter((item) => item !== dialog);
                    if (dialog.isConnected && dialog.matches(':modal')) dialogs.push(dialog);
                }
                for (const dialog of document.querySelectorAll<HTMLDialogElement>('dialog:modal'))
                    if (!dialogs.includes(dialog)) dialogs.push(dialog);
                const dialog = dialogs.at(-1);
                const target = dialog || home;
                // Reopen above a newly opened modal, including a dialog closed and reopened
                // in the same task. Reparenting alone also removes a popover from the top layer
                const modalChanged = records.some(
                    (record) => record.attributeName === 'open' && record.target === dialog,
                );
                if (
                    this.$el.matches(':popover-open') &&
                    (modalChanged || this.$el.parentElement !== target)
                )
                    this.$el.hidePopover();
                if (target && this.$el.parentElement !== target) {
                    alpine.mutateDom(() => {
                        target.appendChild(this.$el);
                    });
                }
                if (this.$el.isConnected && !this.$el.matches(':popover-open'))
                    this.$el.showPopover();
                this.syncProgress();
                this.avoidFocus();
            },
            // Release toasts state and associated lifecycle resources
            destroy() {
                observer?.disconnect();
                resize?.disconnect();
                events?.abort();
                for (const animation of progress.values()) animation.cancel();
                progress.clear();
            },
        };
    });
}
