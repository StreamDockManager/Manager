import {
    open as nativeOpen,
    save as nativeSave,
    type OpenDialogOptions,
    type SaveDialogOptions,
} from '@tauri-apps/plugin-dialog';
import { windowWork } from './window-work';

// Open the current window dialogs target
export function open<T extends OpenDialogOptions>(options?: T) {
    return windowWork.run('native file selector', () => nativeOpen(options));
}

// Save pending changes through the window dialogs workflow
export function save(options?: SaveDialogOptions) {
    return windowWork.run('native save selector', () => nativeSave(options));
}
