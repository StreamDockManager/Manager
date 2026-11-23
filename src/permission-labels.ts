import type { PermissionScope } from '../generated/runtime';

export function permissionLabel(scope: PermissionScope) {
    switch (scope.id) {
        case 'ui.popup':
            return 'Show a text result';
        case 'ui.notification':
            return 'Show a completion notification';
        case 'notifications.create':
            return 'Show plugin alerts and notifications';
        case 'clipboard.read-text':
            return scope.mode === 'direct'
                ? 'Read clipboard text in the plugin'
                : 'Read clipboard text in Manager';
        case 'clipboard.write-text':
            return 'Copy a result after confirmation';
        case 'network.http':
            return scope.recipe === 'manager-webhook'
                ? 'Send local HTTP through Manager'
                : 'Send local HTTP from the plugin';
        case 'network.https':
            if (scope.recipe === 'manager-webhook') return 'Send local HTTPS through Manager';
            if (scope.recipe === 'plugin-webhook') return 'Send local HTTPS from the plugin';
            return 'Connect to Example Summarizer through Manager';
        case 'vault.read-location':
            return 'Read saved latitude and longitude';
        case 'vault.read-secret':
            return 'Read a secret directly';
        case 'vault.write-secret':
            return 'Write a private secret';
        case 'vault.access':
            return scope.mode === 'browse-metadata'
                ? 'Browse approved Vault entry names'
                : 'Use an entry through Manager';
    }
}

export function scopeDetails(scope: PermissionScope) {
    if (scope.id === 'notifications.create') return 'Can notify you without a button press';
    if ('origin' in scope)
        return [
            scope.origin,
            scope.recipe,
            ...('data_class' in scope ? [scope.data_class] : [scope.field, scope.operation]),
        ].join(' · ');
    if ('entry_kind' in scope) return [scope.field, scope.entry_kind].join(' · ');
    return scope.mode;
}
