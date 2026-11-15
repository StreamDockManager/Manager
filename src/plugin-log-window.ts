import { cloneJson } from './json-clone';
import type { Bridge } from './bridge';
import type { CommandName } from '../generated/commands';

const previewCommands = new Set<CommandName>([
    'plugin_log_plugins',
    'plugin_log_preferences',
    'save_plugin_log_preferences',
    'plugin_log_captures',
    'start_plugin_log_capture',
    'stop_plugin_log_capture',
    'resume_plugin_log_capture',
    'clear_plugin_log_capture',
    'subscribe_plugin_logs',
    'unsubscribe_plugin_logs',
    'read_plugin_logs',
    'plugin_log_network_detail',
    'export_plugin_log_capture',
]);
let previewWindow: Window | null = null;
let previewListener: ((event: MessageEvent) => void) | null = null;

// Browser fixtures share the opener's capture service, just as native windows
// share Rust's service. Ports carry bounded pages, never full capture exports
export async function openLogWindow(
    bridge: Pick<Bridge, 'command'>,
    native: boolean,
    id: string,
    stream: string,
) {
    if (import.meta.env.MODE === 'native' || native)
        return bridge.command('open_plugin_log_window', { id, stream });
    if (!previewListener) {
        // Complete the deferred plugin log window work using the current operation state
        previewListener = (event) => {
            if (
                event.source !== previewWindow ||
                event.origin !== location.origin ||
                event.data?.type !== 'plugin-log-command' ||
                !previewCommands.has(event.data.command) ||
                !event.ports[0]
            )
                return;
            const port = event.ports[0];
            void bridge
                .command(event.data.command, event.data.args)
                .then(
                    (value) => port.postMessage({ value }),
                    (error) => port.postMessage({ error: String(error) }),
                )
                .finally(() => port.close());
        };
        window.addEventListener('message', previewListener);
        window.addEventListener(
            'pagehide',
            // Respond to pagehide events in the plugin log window workflow
            () => {
                if (previewListener) window.removeEventListener('message', previewListener);
                previewListener = null;
            },
            { once: true },
        );
    }
    const url = new URL('plugin-logs.html', location.href);
    url.searchParams.set('id', id);
    url.searchParams.set('stream', stream);
    previewWindow = window.open(
        url.href,
        'stream-dock-manager-plugin-logs',
        'popup,width=760,height=620',
    );
    if (!previewWindow) throw Error('Allow popup windows to open plugin logs.');
}

// Bridge preview log requests through the browser-only log transport
export function previewLogBridge(): Pick<Bridge, 'command'> {
    return {
        // Dispatch the requested plugin log window command
        command(command, ...parameters) {
            // Resolve the plugin log window operation when its asynchronous work finishes
            return new Promise((resolve, reject) => {
                if (!window.opener || !previewCommands.has(command)) {
                    reject(Error('Open the log window from Manager.'));
                    return;
                }
                // Match native IPC serialization for Alpine's reactive leases/cursors
                const args = cloneJson(parameters[0] || {});
                const channel = new MessageChannel();
                // Run the deferred plugin log window update after its scheduled delay
                const timer = setTimeout(() => {
                    channel.port1.close();
                    reject(Error('The Manager preview is no longer available.'));
                }, 5000);
                // Complete the deferred plugin log window work using the current operation state
                channel.port1.onmessage = (event) => {
                    clearTimeout(timer);
                    channel.port1.close();
                    if (event.data.error) reject(Error(event.data.error));
                    else resolve(event.data.value);
                };
                try {
                    window.opener.postMessage(
                        { type: 'plugin-log-command', command, args },
                        location.origin,
                        [channel.port2],
                    );
                } catch (error) {
                    clearTimeout(timer);
                    channel.port1.close();
                    channel.port2.close();
                    reject(error);
                }
            });
        },
    };
}
