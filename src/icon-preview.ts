// The browser preview has no native disk/API bridge. It exercises the same offline collection
const bundledIcons = [
    'volume-2',
    'volume-x',
    'play',
    'pause',
    'music',
    'mic',
    'mic-off',
    'monitor',
    'globe',
    'folder',
    'terminal',
    'keyboard',
    'sun',
    'moon',
    'power',
    'settings',
    'heart',
    'star',
    'clock',
    'calendar',
    'check',
    'circle-alert',
    'zap',
    'home',
];
import type { IconAsset, IconResults } from '../generated/runtime';
const key = 'stream-dock-manager.preview.icons';
// Read saved preview icons from browser storage with an empty fallback
function saved(): IconAsset[] {
    try {
        return JSON.parse(localStorage.getItem(key) || '[]');
    } catch {
        return [];
    }
}
import type { PreviewHandlers } from './preview-handlers';
export const iconPreviewHandlers = {
    // Save a preview icon in browser storage without writing native assets
    icon_save: async (args) => {
        const asset = args.asset as IconAsset;
        localStorage.setItem(
            key,
            JSON.stringify([...saved().filter((a) => a.id !== asset.id), asset]),
        );
        return;
    },
    // Resolve a saved or fixture icon in browser preview
    icon_load: async (args) => {
        const id = String(args.id);
        const cached = saved().find((a) => a.id === id);
        if (cached) return cached;
        const icon = id.replace(/^lucide:/, '');
        if (!id.startsWith('lucide:') || !bundledIcons.includes(icon))
            throw Error('Icon is not available offline.');
        const response = await fetch(
            import.meta.env.BASE_URL + 'appearance/icons/' + icon + '.svg',
        );
        if (!response.ok) throw Error('Bundled icon could not be loaded.');
        return {
            id,
            svg: await response.text(),
            collection: 'Lucide',
            author: 'Lucide Contributors',
            license: 'ISC',
            license_url: 'https://lucide.dev/license',
        } satisfies IconAsset;
    },
    // Search available browser preview icons without native icon discovery
    icon_search: async (args) => {
        const query = String(args.query || '')
            .toLowerCase()
            .trim();
        const prefix = String(args.prefix || '');
        const start = Number(args.start || 0);
        if (!Number.isInteger(start) || start < 0 || start > 100_000)
            throw Error('Invalid icon search');
        const icons = [
            ...new Set([...saved().map((a) => a.id), ...bundledIcons.map((id) => 'lucide:' + id)]),
        ]
            .sort()
            .filter(
                (id) =>
                    query.split(/\s+/).every((word) => id.includes(word)) &&
                    (!prefix || id.startsWith(prefix + ':')),
            );
        return {
            icons: icons.slice(start, start + 64),
            collections: [],
            more: start + 64 < icons.length,
            offline: true,
            message: args.offline
                ? 'Offline mode: saved icons and bundled Lucide icons.'
                : 'Browser preview: offline collection. Full Iconify search and disk caching are available in the desktop app.',
        } satisfies IconResults;
    },
} satisfies PreviewHandlers;
