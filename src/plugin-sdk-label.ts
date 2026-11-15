import type { Plugin } from './types';

/** SDK identity is independent of the vendor wire dialect used by Manager plugins. */
export function pluginSdkLabel(plugin: Pick<Plugin, 'capabilities' | 'dialect'>): string {
    // The host supplies capabilities only after checking the executable descriptor
    // against the manifest. Names, publishers and bundle filenames are not evidence
    if (plugin.capabilities) return 'Stream Dock Manager';
    switch (plugin.dialect) {
        case 'vsd':
            return 'VSD';
        case 'openaction':
            return 'OpenActionsAPI';
        case 'streamdeck':
            return 'Stream Deck';
        default:
            return plugin.dialect;
    }
}
