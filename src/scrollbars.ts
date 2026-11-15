/** Reveal scrollbars during scrolling or interaction with the scrollable edge. */
export function registerAutoScrollbars() {
    const root = document.documentElement;
    const events = new AbortController();
    const timers = new Map<HTMLElement, number>();
    let edge: HTMLElement | null = null;
    let dragging: HTMLElement | null = null;
    let frame = 0;
    let pointer: PointerEvent | null = null;
    root.dataset.autoScrollbars = '';

    // Reveal the selected scrollbars item
    const reveal = (element: HTMLElement) => {
        window.clearTimeout(timers.get(element));
        element.classList.add('scrollbar-active');
        timers.set(
            element,
            // Run the deferred scrollbars update after its scheduled delay
            window.setTimeout(() => {
                if (element === edge || element === dragging) return;
                element.classList.remove('scrollbar-active');
                timers.delete(element);
            }, 1000),
        );
    };
    // Reveal the scrollbar belonging to the element that just scrolled
    const scroll = (event: Event) => {
        const target = event.target === document ? root : event.target;
        if (target instanceof HTMLElement) reveal(target);
    };
    // Reveal scrollbars when the pointer approaches a scrollable ancestor's edge
    const nearEdge = (event: PointerEvent) => {
        let element = event.target instanceof Element ? event.target : null;
        while (element) {
            if (element instanceof HTMLElement) {
                const style = getComputedStyle(element);
                const isRoot = element === root;
                const vertical =
                    element.scrollHeight > element.clientHeight + 1 &&
                    (isRoot ? style.overflowY !== 'hidden' : /auto|scroll/.test(style.overflowY));
                const horizontal =
                    element.scrollWidth > element.clientWidth + 1 &&
                    /auto|scroll/.test(style.overflowX);
                if (vertical || horizontal) {
                    const bounds = isRoot
                        ? { left: 0, right: innerWidth, top: 0, bottom: innerHeight }
                        : element.getBoundingClientRect();
                    const side = style.direction === 'rtl' ? bounds.left : bounds.right;
                    if (
                        (vertical &&
                            Math.abs(event.clientX - side) <= 18 &&
                            event.clientY >= bounds.top &&
                            event.clientY <= bounds.bottom) ||
                        (horizontal &&
                            Math.abs(event.clientY - bounds.bottom) <= 18 &&
                            event.clientX >= bounds.left &&
                            event.clientX <= bounds.right)
                    )
                        return element;
                }
            }
            element = element.parentElement;
        }
        return null;
    };
    // Update the edge
    const updateEdge = (event: PointerEvent) => {
        const previous = edge;
        edge = nearEdge(event);
        if (previous && previous !== edge) reveal(previous);
        if (edge) reveal(edge);
    };
    const options = { signal: events.signal, passive: true };
    const removePageScrollbar = registerPageScrollbar(root, reveal, events.signal);
    document.addEventListener('scroll', scroll, { ...options, capture: true });
    document.addEventListener(
        'pointermove',
        // Respond to pointermove events in the scrollbars workflow
        (event) => {
            if (event.pointerType !== 'mouse') return;
            pointer = event;
            if (!frame)
                // Complete the deferred scrollbars work using the current operation state
                frame = requestAnimationFrame(() => {
                    frame = 0;
                    if (pointer) updateEdge(pointer);
                    pointer = null;
                });
        },
        options,
    );
    document.addEventListener(
        'pointerdown',
        // Respond to pointerdown events in the scrollbars workflow
        (event) => {
            if (event.pointerType !== 'mouse') return;
            updateEdge(event);
            dragging = edge;
        },
        options,
    );
    window.addEventListener(
        'pointerup',
        // Respond to pointerup events in the scrollbars workflow
        () => {
            const previous = dragging;
            dragging = null;
            if (previous) reveal(previous);
        },
        options,
    );
    // Leave the current scrollbars workflow
    const leave = () => {
        const previous = edge;
        edge = null;
        if (previous) reveal(previous);
    };
    document.addEventListener('pointerleave', leave, options);
    window.addEventListener(
        'blur',
        // Respond to blur events in the scrollbars workflow
        () => {
            dragging = null;
            leave();
            for (const element of timers.keys()) reveal(element);
        },
        options,
    );
    // Release scrollbars state and associated lifecycle resources
    const dispose = () => {
        events.abort();
        cancelAnimationFrame(frame);
        removePageScrollbar();
        for (const [element, timer] of timers) {
            window.clearTimeout(timer);
            element.classList.remove('scrollbar-active');
        }
        timers.clear();
        delete root.dataset.autoScrollbars;
    };
    window.addEventListener('pagehide', dispose, { signal: events.signal, once: true });
    return dispose;
}

/** A viewport overlay avoids the space reserved by macOS's native page scrollbar. */
function registerPageScrollbar(
    root: HTMLElement,
    reveal: (element: HTMLElement) => void,
    signal: AbortSignal,
) {
    const thumb = document.createElement('div');
    thumb.className = 'page-scrollbar';
    thumb.tabIndex = 0;
    thumb.hidden = true;
    thumb.setAttribute('role', 'scrollbar');
    thumb.setAttribute('aria-label', 'Scroll page');
    thumb.setAttribute('aria-orientation', 'vertical');
    thumb.setAttribute('aria-valuemin', '0');
    thumb.setAttribute('aria-valuemax', '100');
    const ownsRootId = !root.id;
    if (ownsRootId) root.id = 'page-scroll-content';
    thumb.setAttribute('aria-controls', root.id);
    document.body.append(thumb);
    let frame = 0;
    let maximum = 0;
    let travel = 0;
    let drag: { id: number; y: number; scroll: number } | undefined;
    // Update the current scrollbars state
    const update = () => {
        const height = root.clientHeight;
        maximum = Math.max(0, root.scrollHeight - height);
        thumb.hidden = maximum <= 1 || /hidden|clip/.test(getComputedStyle(root).overflowY);
        if (thumb.hidden) return;
        const track = Math.max(0, height - 4);
        const size = Math.min(track, Math.max(32, (height / root.scrollHeight) * track));
        travel = track - size;
        const progress = Math.max(0, Math.min(1, root.scrollTop / maximum));
        thumb.style.height = `${size}px`;
        thumb.style.transform = `translateY(${2 + travel * progress}px)`;
        thumb.setAttribute('aria-valuenow', String(Math.round(progress * 100)));
    };
    // Schedule the next scrollbars update
    const schedule = () => {
        if (!frame)
            frame = requestAnimationFrame(() => {
                frame = 0;
                update();
            });
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(root);
    observer.observe(document.body);
    window.addEventListener('resize', schedule, { signal, passive: true });
    document.addEventListener('scroll', schedule, { signal, passive: true });
    thumb.addEventListener(
        'pointerdown',
        // Respond to pointerdown events in the scrollbars workflow
        (event) => {
            if (event.button !== 0) return;
            event.preventDefault();
            update();
            drag = { id: event.pointerId, y: event.clientY, scroll: root.scrollTop };
            thumb.classList.add('dragging');
            thumb.setPointerCapture(event.pointerId);
            reveal(root);
        },
        { signal },
    );
    thumb.addEventListener(
        'pointermove',
        // Respond to pointermove events in the scrollbars workflow
        (event) => {
            if (!drag || drag.id !== event.pointerId || !travel) return;
            root.scrollTop = drag.scroll + ((event.clientY - drag.y) / travel) * maximum;
            reveal(root);
        },
        { signal },
    );
    thumb.addEventListener(
        'lostpointercapture',
        // Respond to lostpointercapture events in the scrollbars workflow
        () => {
            drag = undefined;
            thumb.classList.remove('dragging');
            reveal(root);
        },
        { signal },
    );
    thumb.addEventListener(
        'keydown',
        // Respond to keydown events in the scrollbars workflow
        (event) => {
            const steps: Record<string, number> = {
                ArrowUp: root.scrollTop - 40,
                ArrowDown: root.scrollTop + 40,
                PageUp: root.scrollTop - root.clientHeight * 0.9,
                PageDown: root.scrollTop + root.clientHeight * 0.9,
                Home: 0,
                End: maximum,
            };
            if (!(event.key in steps)) return;
            event.preventDefault();
            root.scrollTop = steps[event.key];
            reveal(root);
        },
        { signal },
    );
    update();
    // Complete the deferred scrollbars work using the current operation state
    return () => {
        observer.disconnect();
        cancelAnimationFrame(frame);
        thumb.remove();
        if (ownsRootId) root.removeAttribute('id');
    };
}
