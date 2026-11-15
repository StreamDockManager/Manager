import { invoke, isTauri, type InvokeArgs, type InvokeOptions } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { EditorRestoreState } from '../generated/runtime';
import { windowWork } from './window-work';

let bootstrap: Promise<EditorRestoreState> | undefined;
// Detect the native main editor while excluding the isolated public preview
export function mainEditorWindow() {
    return (
        import.meta.env.MODE !== 'public-preview' &&
        isTauri() &&
        getCurrentWindow().label === 'main'
    );
}

/** The only unversioned main command; appearance reads can precede application startup. */
export function editorRestoreState(): Promise<EditorRestoreState> {
    if (import.meta.env.MODE === 'public-preview')
        return Promise.reject(new Error('Native commands are unavailable in the public demo.'));
    if (!bootstrap) {
        // Process the settled window native operation result
        const pending = invoke<EditorRestoreState>('editor_restore_state').then((value) => {
            if (!Number.isSafeInteger(value.generation) || value.generation < 1)
                throw Error('Invalid editor window generation');
            return value;
        });
        bootstrap = pending;
        void pending.catch(() => {
            if (bootstrap === pending) bootstrap = undefined;
        });
    }
    return bootstrap;
}

/** Every main-editor native call, including binary batches, participates in admission. */
export function windowInvoke<T>(command: string, args?: InvokeArgs, options?: InvokeOptions) {
    if (import.meta.env.MODE === 'public-preview')
        return Promise.reject<T>(new Error('Native commands are unavailable in the public demo.'));
    // Auxiliary windows own independent native work and never enter the main fence
    if (!mainEditorWindow()) return invoke<T>(command, args, options);
    // Complete the deferred window native work using the current operation state
    return windowWork.command(command, async () => {
        const state = await editorRestoreState();
        if (command === 'editor_restore_state') return state as T;
        const headers = new Headers(options?.headers);
        headers.set('X-Stream-Dock-Manager-Editor-Generation', String(state.generation));
        return invoke<T>(command, args, { ...options, headers });
    });
}
