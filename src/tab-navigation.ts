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

/** Route-local tabs keep forms mounted while switching and allow links to a specific section. */
export function pageSections(
    route: 'settings' | 'diagnostics',
    sectionTabs: { id: string; label: string }[],
) {
    const requested = new URLSearchParams(location.hash.split('?')[1]).get('tab');
    return {
        sectionTabs,
        sectionTab: sectionTabs.find((tab) => tab.id === requested)?.id ?? sectionTabs[0].id,
        chooseSection(id: string) {
            if (!sectionTabs.some((tab) => tab.id === id)) return;
            this.sectionTab = id;
            const hash = '#/' + route + (id === sectionTabs[0].id ? '' : '?tab=' + id);
            if (location.hash !== hash) history.replaceState(history.state, '', hash);
        },
        selectSection(event: Event) {
            const button = event.currentTarget as HTMLButtonElement;
            this.chooseSection(button.dataset.sectionTab ?? '');
            button.focus({ preventScroll: true });
        },
        sectionKey(event: KeyboardEvent) {
            const next = nextTab(
                sectionTabs.map((tab) => tab.id),
                this.sectionTab,
                event.key,
            );
            if (!next) return;
            event.preventDefault();
            this.chooseSection(next);
            (event.currentTarget as HTMLElement)
                .querySelector<HTMLButtonElement>(`[data-section-tab="${next}"]`)
                ?.focus();
        },
    };
}
