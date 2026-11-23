// Focus and scroll to the section targeted by an in-page navigation link
export function jumpToSection(event: Event) {
    const link = event.currentTarget as HTMLAnchorElement;
    const section = document.getElementById(link.hash.slice(1));
    if (!section) return;
    section.focus({ preventScroll: true });
    section.scrollIntoView({ block: 'start' });
}

// Register the tooltips
export function registerTooltips() {
    const tip = document.createElement('div');
    tip.className = 'app-tooltip';
    tip.id = 'app-tooltip';
    tip.setAttribute('role', 'tooltip');

    let trigger: HTMLElement | undefined;
    let dismissedTrigger: HTMLElement | undefined;
    let dismissedPointer: { x: number; y: number } | undefined;
    let describedElement: HTMLElement | undefined;
    let describedBy: string | null = null;
    let hideTimer: number | undefined;
    let suppressUntil = 0;
    const events = new AbortController();
    const options = { signal: events.signal };
    // Hide the visibility operation
    const hide = () => {
        window.clearTimeout(hideTimer);
        if (describedBy) describedElement?.setAttribute('aria-describedby', describedBy);
        else describedElement?.removeAttribute('aria-describedby');
        tip.remove();
        trigger = undefined;
        describedElement = undefined;
    };
    // Calculate the current visibility position
    const position = () => {
        if (!trigger?.isConnected || !trigger.getClientRects().length) {
            hide();
            return;
        }
        const anchor = trigger.getBoundingClientRect();
        if (anchor.bottom <= 0 || anchor.top >= innerHeight) {
            hide();
            return;
        }
        const gutter = 8;
        const x = anchor.left + (anchor.width - tip.offsetWidth) / 2;
        const above = anchor.top - tip.offsetHeight - gutter;
        tip.style.left =
            Math.max(gutter, Math.min(x, innerWidth - tip.offsetWidth - gutter)) + 'px';
        tip.style.top =
            Math.max(
                gutter,
                Math.min(
                    above >= gutter ? above : anchor.bottom + gutter,
                    innerHeight - tip.offsetHeight - gutter,
                ),
            ) + 'px';
    };
    // Show the selected visibility content
    const show = (target: EventTarget | null) => {
        if (!(target instanceof Element) || performance.now() < suppressUntil) return;
        const element = target.closest<HTMLElement>('[data-tooltip]');
        const value = element?.dataset.tooltip;
        if (!element || !value || element === dismissedTrigger || element.matches(':disabled'))
            return;
        window.clearTimeout(hideTimer);
        if (element === trigger) return;
        hide();
        trigger = element;
        describedElement =
            element instanceof HTMLLabelElement ? element.control || element : element;
        describedBy = describedElement.getAttribute('aria-describedby');
        tip.textContent = value;
        (element.closest('dialog:modal') || document.body).append(tip);
        if (value !== describedElement.getAttribute('aria-label'))
            describedElement.setAttribute(
                'aria-describedby',
                [describedBy, tip.id].filter(Boolean).join(' '),
            );
        position();
    };
    // Queue the hide
    const queueHide = () => {
        if (!trigger) return;
        window.clearTimeout(hideTimer);
        // Run the deferred visibility update after its scheduled delay
        hideTimer = window.setTimeout(() => {
            if (
                trigger?.contains(document.activeElement) ||
                trigger?.matches(':hover') ||
                tip.matches(':hover')
            )
                return;
            hide();
        }, 150);
    };
    const hover = (event: PointerEvent) => {
        if (event.pointerType !== 'mouse') return;
        // Expanding the sidebar can move another control under a stationary pointer.
        // ClickEvent rounds CSS coordinates; PointerEvent can retain their fractional part.
        if (
            dismissedPointer &&
            dismissedPointer.x === Math.floor(event.clientX) &&
            dismissedPointer.y === Math.floor(event.clientY)
        )
            return;
        dismissedPointer = undefined;
        show(event.target);
    };
    document.addEventListener('pointerover', hover, options);
    document.addEventListener(
        'pointermove',
        (event) => {
            if (dismissedPointer) hover(event);
        },
        options,
    );
    // Leave the current visibility workflow
    const leave = (event: FocusEvent | PointerEvent) => {
        if (
            dismissedTrigger &&
            event.target instanceof Node &&
            dismissedTrigger.contains(event.target) &&
            !(event.relatedTarget instanceof Node && dismissedTrigger.contains(event.relatedTarget))
        )
            dismissedTrigger = undefined;
        queueHide();
    };
    document.addEventListener('pointerout', leave, options);
    document.addEventListener(
        'focusin',
        (event) => {
            hide();
            show(event.target);
        },
        options,
    );
    document.addEventListener('focusout', leave, options);
    document.addEventListener(
        'click',
        // Respond to click events in the visibility workflow
        (event) => {
            if (!(event.target instanceof Element)) return;
            const element = event.target.closest<HTMLElement>('[data-tooltip-dismiss-on-click]');
            if (!element) return;
            // Keep keyboard focus, but wait for a fresh hover or focus before reopening
            dismissedTrigger = element;
            if (event.detail > 0)
                dismissedPointer = { x: Math.floor(event.clientX), y: Math.floor(event.clientY) };
            hide();
        },
        options,
    );
    document.addEventListener(
        'keydown',
        // Respond to keydown events in the visibility workflow
        (event) => {
            if (event.key === 'Escape') {
                // Let the owning dialog or disclosure handle Escape as usual
                // A programmatic focus return should not reopen the hint
                suppressUntil = performance.now() + 150;
                hide();
            }
        },
        { capture: true, ...options },
    );
    document.addEventListener('close', hide, { capture: true, ...options });
    window.addEventListener('hashchange', hide, options);
    window.addEventListener(
        'resize',
        () => {
            if (trigger) position();
        },
        options,
    );
    window.addEventListener(
        'scroll',
        () => {
            if (trigger) position();
        },
        { capture: true, ...options },
    );
    window.addEventListener(
        'pagehide',
        () => {
            events.abort();
            hide();
        },
        { once: true },
    );
}
