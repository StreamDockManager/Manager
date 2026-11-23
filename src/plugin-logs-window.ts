import { startFrontendDiagnostics } from './frontend-errors';
startFrontendDiagnostics();
import Alpine from '@alpinejs/csp';
import Loader from 'alpine-component-loader/core';
import { registerRoute } from 'virtual:alpine-component-loader/routes';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { registerPluginLogs, PluginLogs } from './plugin-logs';
import { previewLogBridge } from './plugin-log-window';
import { registerTheme } from './theme';
import { registerTooltips } from './visibility';
import { registerAccessibility } from './accessibility';
import { ToastState, registerToasts } from './toasts';
import type { Bridge } from './bridge';
import type { LogPluginIdentity } from '../generated/runtime';
import './plugin-logs-window.css';
import { registerAutoScrollbars } from './scrollbars';
registerAutoScrollbars();

const native = import.meta.env.MODE === 'native' || isTauri();
const bridge: Pick<Bridge, 'command'> = native ? { command: invoke } : previewLogBridge();
class LogWindowState extends ToastState {
    snapshot: { plugins: LogPluginIdentity[] } = { plugins: [] };
}
window.Alpine = Alpine;
Alpine.store('app', new LogWindowState());
// Read the dedicated log window's Alpine application store
const app = () => Alpine.store('app') as LogWindowState;
registerAccessibility(Alpine);
registerToasts(Alpine);
registerTheme(Alpine, native, false);
registerTooltips();
registerPluginLogs(Alpine, bridge, app, {
    detached: true,
    // Close the window
    closeWindow: async () => {
        if (native) await getCurrentWindow().close();
        else window.close();
    },
});
// Read the plugin log controller shared by the log window
const logs = () => Alpine.store('pluginLogs') as PluginLogs;
let opening = 0;
// Open the capture
async function openCapture(id: string, stream: string) {
    const request = ++opening;
    await logs().refresh();
    if (request !== opening) return;
    if (!logs().captures.some((capture) => capture.id === id)) {
        logs().showCaptures();
        logs().error =
            'Capture no longer exists. Choose another capture or start one from a plugin card.';
        return;
    }
    logs().selected = id;
    logs().reading.tab = stream === 'stdout' ? 'stdout' : 'stderr';
    await logs().view(id);
}
// Start the requested plugin logs window operation
async function start() {
    Loader.config({ sanitize: true, executeScripts: false });
    Alpine.start();
    const loaded = [...document.querySelectorAll('sd-plugin-logs, sd-toasts')].map(
        (element) =>
            // Convert each entry into the representation required by plugin logs window
            new Promise<void>((resolve, reject) => {
                element.addEventListener('loaded', () => resolve(), { once: true });
                element.addEventListener(
                    'acl:error',
                    () => reject(Error('The log reader could not load.')),
                    { once: true },
                );
            }),
    );
    await Loader.registerErrorBoundary();
    await Loader.registerDynamicLoader();
    await registerRoute('plugin-logs');
    await Loader.start();
    await Promise.all(loaded);
    app().snapshot.plugins = await bridge.command('plugin_log_plugins');
    logs().startWindow();
    if (native) {
        await listen('ui-appearance-changed', () =>
            window.dispatchEvent(new Event('ui-appearance-changed')),
        );
        window.dispatchEvent(new Event('ui-appearance-changed'));
        await listen<{ id: string; stream: string }>('plugin-log-open', (event) => {
            void openCapture(event.payload.id, event.payload.stream);
        });
        await listen<boolean>('plugin-log-visibility', (event) =>
            logs().setWindowVisible(event.payload),
        );
    }
    // Complete the deferred plugin logs window work using the current operation state
    Alpine.effect(() => {
        const title = `${logs().title} · Stream Dock Manager`;
        document.title = title;
        if (native)
            void getCurrentWindow()
                .setTitle(title)
                .catch(() => {});
    });
    const query = new URLSearchParams(location.search);
    await openCapture(query.get('id') || '', query.get('stream') || 'stderr');
    document.querySelector('#boot')?.remove();
}
window.addEventListener(
    'pagehide',
    () => {
        logs().stopWindow();
        app().clearToasts();
    },
    { once: true },
);
// Handle a failed plugin logs window operation without retaining stale work
void start().catch((error) => {
    const boot = document.querySelector('#boot');
    if (boot) boot.textContent = `Could not open plugin logs: ${error}`;
});
