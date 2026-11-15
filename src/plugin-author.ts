import type { Plugin } from './types';

// Format the plugin author or show an explicit missing-author label
export function pluginAuthorLabel(plugin: Pick<Plugin, 'author'>): string {
    const author = plugin.author?.trim();
    return author ? `By ${author}` : 'Author not provided';
}
