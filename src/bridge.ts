import { subscribeNativeSurfaces } from './surface-subscription';
import { isTauri, Channel } from '@tauri-apps/api/core';
import { windowInvoke as invoke } from './window-native';
import { windowWork } from './window-work';
import { listen } from '@tauri-apps/api/event';
import type { CommandParameters, CommandName, CommandResult } from '../generated/commands';
import type { RuntimeEvent } from './types';
import type {
    PreviewDelta,
    WorkspaceBatch,
    MediaOptions,
    DeviceOutput,
} from '../generated/runtime';

// Create default device output without changing hardware
export const defaultDeviceOutput = (channels = 4): DeviceOutput => ({
    device_configuration: {},
    sleeping: false,
    brightness: 65,
    lighting_brightness: 15,
    lighting_colors: Array.from({ length: channels }, () => [198, 30, 50]),
});

export const native =
    import.meta.env.MODE !== 'public-preview' && (import.meta.env.MODE === 'native' || isTauri());
export type RetireSubscription = () => void | Promise<void>;
interface BridgeCommands {
    command<C extends CommandName>(
        command: C,
        ...args: CommandParameters<C>
    ): Promise<CommandResult<C>>;
    subscribeWorkspace(listener: (batch: WorkspaceBatch) => void): Promise<RetireSubscription>;
    subscribe(listener: (event: RuntimeEvent) => void): Promise<RetireSubscription>;
}
export type Bridge = BridgeCommands &
    (
        | { readonly surfaceRendering: 'local'; subscribeSurfaces?: never }
        | {
              readonly surfaceRendering: 'stream';
              subscribeSurfaces(
                  deviceId: string,
                  listener: (frame: PreviewDelta) => void,
              ): Promise<RetireSubscription>;
          }
    );
export const bridge: Bridge = native
    ? {
          surfaceRendering: 'stream',
          command: invoke,
          // Subscribe a native channel to workspace batches
          subscribeWorkspace: async (listener) => {
              const channel = new Channel<WorkspaceBatch>();
              channel.onmessage = listener;
              const id = await invoke<string>('subscribe_workspace', { onBatch: channel });
              // Silence the channel before unsubscribing from the native workspace
              return () => {
                  // Ignore late batches after subscription retirement
                  channel.onmessage = () => {};
                  return invoke<void>('unsubscribe_workspace', { id });
              };
          },
          // Subscribe to device surface frames through the native lifetime guard
          subscribeSurfaces: (deviceId, listener) =>
              subscribeNativeSurfaces(deviceId, listener, {
                  channel: new Channel<ArrayBuffer>(),
                  invoke,
                  // Track surface subscription work in the window lifetime
                  run: (label, work) => windowWork.run(label, work),
                  // Report whether window shutdown is stopping new work
                  quiescing: () => windowWork.quiescing,
              }),
          // Forward native runtime event payloads to the subscriber
          subscribe: async (listener) =>
              listen<RuntimeEvent>('runtime-event', (event) => listener(event.payload)),
      }
    : createBrowserBridge();

// Keep the simulator and its fixtures out of native startup. Desktop production
// builds fold `native` to true and omit this dynamic import entirely
function createBrowserBridge(): Bridge {
    let pending: Promise<Bridge> | undefined;
    // Load the browser simulator once and share its pending initialization
    const load = () =>
        (pending ??= import('./bridge-preview')
            .then(({ createPreviewBridge }) => createPreviewBridge())
            // Clear a failed simulator import so a later request can retry
            .catch((error) => {
                pending = undefined;
                throw error;
            }));
    return {
        surfaceRendering: 'local',
        // Forward a command after the browser bridge is ready
        command: async (command, ...args) => (await load()).command(command, ...args),
        // Subscribe to runtime events after the browser bridge is ready
        subscribe: async (listener) => (await load()).subscribe(listener),
        // Subscribe to workspace updates after the browser bridge is ready
        subscribeWorkspace: async (listener) => (await load()).subscribeWorkspace(listener),
    };
}

/** Retired limits must fail rather than silently changing playback cadence */
export function assertCurrentMediaOptions(value: unknown): void {
    if (value && typeof value === 'object' && Object.hasOwn(value, 'fps'))
        throw new Error('The media fps field is no longer supported. Use max_fps instead.');
}

// Create default media framing and playback options
export const defaultMedia = (): MediaOptions => ({
    fit: 'fill',
    crop: [0, 0, 1, 1] as [number, number, number, number],
    trim_start_ms: 0,
    trim_end_ms: null,
    speed: 1,
});
