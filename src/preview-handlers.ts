import type { CommandName, CommandParameters, CommandResult } from '../generated/commands';

import type { Snapshot } from '../generated/runtime';
import type { PreviewVisual } from './plugin-display-preview';
export type FixtureMediaDetails = { format: string; animated: boolean; duration_ms: number };
export interface PreviewFixtureHandlers {
    preview_plugin_display_fixture?: (args: {
        deviceId: string;
        controlId: string;
        plugin?: Snapshot['plugins'][number];
        visual?: PreviewVisual;
    }) => void | Promise<void>;
    preview_media_fixture?: (args: {
        asset: string;
        image: string;
        format?: string;
        animated?: boolean;
        duration_ms?: number;
    }) => void | Promise<void>;
    preview_text_checks_fixture?: (args: { warnings: string[] }) => void | Promise<void>;
    media_details?: (args: { asset: string }) => FixtureMediaDetails | Promise<FixtureMediaDetails>;
}
// Contextual typing checks each preview implementation against the native IPC contract
export type NativePreviewHandlers = {
    [C in CommandName]?: (
        ...parameters: CommandParameters<C>
    ) => CommandResult<C> | Promise<CommandResult<C>>;
};

export type PreviewHandlers = NativePreviewHandlers & PreviewFixtureHandlers;

// Dispatch only explicitly registered browser-preview commands and reject native-only operations
export async function dispatchPreview<C extends CommandName>(
    handlers: NativePreviewHandlers,
    command: C,
    ...parameters: CommandParameters<C>
): Promise<CommandResult<C>> {
    const handler = Object.hasOwn(handlers, command) ? handlers[command] : undefined;
    if (!handler) throw Error('This operation requires the native desktop application.');
    return handler(...parameters);
}

/** Duplicate ownership is an error, never an implicit object-spread override. */
export function composePreviewHandlers(...groups: PreviewHandlers[]): PreviewHandlers {
    const handlers: PreviewHandlers = {};
    for (const group of groups) {
        for (const command of Object.keys(group) as (keyof PreviewHandlers)[]) {
            if (Object.hasOwn(handlers, command))
                throw Error(`Duplicate preview command: ${command}`);
            Object.defineProperty(handlers, command, { value: group[command], enumerable: true });
        }
    }
    return handlers;
}
