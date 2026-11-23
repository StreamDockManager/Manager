import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import { ReferenceIndex, type NavigationEdge } from './dependencies';

export function registerNavigationMap(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.data('navigationMap', () => {
        let index: ReferenceIndex | undefined;
        return {
            opened: false,
            view: 'map',
            device: '',
            profile: '',
            get pages() {
                if (!this.opened) return [];
                const s = app().snapshot;
                if (!index?.isCurrent(s)) index = new ReferenceIndex(s);
                const graph = index;
                const profile = s.profiles.find((p) => p.id === this.profile);
                const manifest = s.devices.find((d) => d.id === this.device)?.manifest;
                return (
                    profile?.pages.map((page) => ({
                        id: page.id,
                        name:
                            page.name + (page.id === profile.active_page ? ' (starting page)' : ''),
                        finding: graph.findings
                            .filter(
                                (f) =>
                                    f.page === page.id &&
                                    f.profile === profile.id &&
                                    ['closed_navigation', 'unreachable_page'].includes(f.code),
                            )
                            .map((f) => f.message)
                            .join(' · '),
                        edges: graph.navigation
                            .filter((e) => e.profile === profile.id && e.page === page.id)
                            .map((edge) => {
                                const dest = s.profiles.find(
                                    (p) => p.id === (edge.targetProfile || profile.id),
                                );
                                const target =
                                    dest?.pages.find((p) => p.id === edge.destination)?.name ||
                                    'Dynamic destination';
                                const origin =
                                    manifest?.controls.find((c) => c.id === edge.control)?.label ||
                                    edge.control;
                                return {
                                    ...edge,
                                    label:
                                        [origin, edge.path, edge.conditional ? 'Conditional' : '']
                                            .filter(Boolean)
                                            .join(' / ') +
                                        ' → ' +
                                        (edge.targetProfile && dest ? dest.name + ' / ' : '') +
                                        target,
                                };
                            }),
                    })) || []
                );
            },
            open() {
                this.device = app().deviceId;
                this.profile = app().profile?.id || '';
                this.opened = true;
                (this.$refs.map as HTMLDialogElement).showModal();
            },
            close() {
                this.opened = false;
                (this.$refs.map as HTMLDialogElement).close();
            },
            async edit(edge: NavigationEdge) {
                await app().flushDrafts?.();
                if (
                    app().deviceId !== this.device ||
                    app().profile?.id !== edge.profile ||
                    !app().profile?.pages.find((p) => p.id === edge.page)?.bindings[edge.control]
                ) {
                    app().notify('Binding changed. Reopen the map.', 'warning');
                    return;
                }
                if (app().page?.id !== edge.page)
                    app().apply(
                        await app().command('select_page', {
                            deviceId: this.device,
                            pageId: edge.page,
                        }),
                    );
                app().selectedControls = [];
                app().controlId = edge.control;
                this.close();
                this.$nextTick(() =>
                    document
                        .querySelector<HTMLElement>('.hardware-control[aria-pressed="true"]')
                        ?.focus(),
                );
            },
        };
    });
}
