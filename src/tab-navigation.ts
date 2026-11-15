/** Navigate the caller-provided enabled order; activation and scrolling stay with the caller. */
export function tabNavigationKey(key: string) {
    return ['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(key);
}
// Resolve Home, End and wrapped arrow-key tab navigation
export function nextTab<T>(tabs: readonly T[], current: T, key: string): T | undefined {
    if (!tabs.length || !tabNavigationKey(key)) return undefined;
    if (key === 'Home') return tabs[0];
    if (key === 'End') return tabs[tabs.length - 1];
    const index = tabs.indexOf(current);
    return tabs[(index + (key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length];
}
