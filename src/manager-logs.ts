import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';

// Register the Manager log-window launcher
export function registerManagerLogs(Alpine: typeof AlpineType, app: () => AppState) {
    const store = {
        opening: false,
        // Open Manager logs while preventing duplicate launch requests
        async open() {
            if (this.opening) return;
            this.opening = true;
            try {
                await app().bridge.command('open_manager_logs');
            } catch (error) {
                app().notify('Could not open Manager logs: ' + String(error), 'error');
            } finally {
                this.opening = false;
            }
        },
    };
    Alpine.store('managerLogs', store);
}
