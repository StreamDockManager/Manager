/** Backpressure only the desktop surface subscriber, never the hardware renderer.
 * Hidden windows are already unsubscribed by state.ts. A visible but unfocused
 * window keeps a useful 4 Hz preview without decoding at the device frame rate.
 */
export function previewCadence(
    acknowledge: () => void,
    focused: () => boolean,
    events: Pick<EventTarget, 'addEventListener' | 'removeEventListener'>,
    // Browser timer methods require their global receiver (not this clock object)
    clock = {
        // Replace the requested preview cadence value
        set: (fn: () => void) => globalThis.setTimeout(fn, 250),
        // Clear the retained preview cadence value
        clear: (timer: ReturnType<typeof setTimeout>) => globalThis.clearTimeout(timer),
    },
) {
    let timer: ReturnType<typeof setTimeout> | undefined;
    let pending = false;
    let stopped = false;
    // Flush pending preview cadence work to its consumer
    const flush = () => {
        if (timer !== undefined) clock.clear(timer);
        timer = undefined;
        if (!pending || stopped) return;
        pending = false;
        acknowledge();
    };
    events.addEventListener('focus', flush);
    return {
        // Mark a frame ready and acknowledge now or schedule one delayed acknowledgment
        ready() {
            if (stopped) return;
            pending = true;
            if (focused()) flush();
            else if (timer === undefined) timer = clock.set(flush);
        },
        // Release preview cadence state and associated lifecycle resources
        dispose() {
            stopped = true;
            pending = false;
            if (timer !== undefined) clock.clear(timer);
            timer = undefined;
            events.removeEventListener('focus', flush);
        },
    };
}
