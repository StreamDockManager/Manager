import type { listen } from '@tauri-apps/api/event';
import type { ArchiveImportKind } from '../generated/runtime';
import type { AppState } from './state';
import type { StartupLifetime } from './window-startup';
import { windowInvoke } from './window-native';
import { windowCloseOwners } from './window-close-owners';
import { windowWork } from './window-work';
import { ensureDialog } from './dialog-loader';
import {
    captureDroppedSources,
    isDroppedDirectory,
    isPluginFolder,
    stagePluginFolder,
    type DroppedDirectory,
} from './plugin-folder-drop';

const maxPending = 16;
const chunkSize = 1024 * 1024;
const maxSize = 288 * chunkSize;
const reviewers = new Map<ArchiveImportKind, (path: string) => Promise<void>>();
// Check whether a modal currently blocks archive review
const modalOpen = () => Boolean(document.querySelector('dialog:modal'));
// Keep archive wakeups inert until drop handling is registered
let wake = () => {};

// Register the reviewer for one archive kind
export function registerArchiveReviewer(
    kind: ArchiveImportKind,
    review: (path: string) => Promise<void>,
) {
    reviewers.set(kind, review);
    wake();
    // Unregister the reviewer only if it is still the active owner
    return () => {
        if (reviewers.get(kind) === review) reviewers.delete(kind);
    };
}

// The editor keeps HTML drag and drop for controls and macro steps. File
// objects expose bytes here, while auxiliary windows can supply native paths
async function stageFile(file: File, signal: AbortSignal) {
    if (file.size < 22 || file.size > maxSize) throw Error('Choose a ZIP archive up to 288 MiB.');
    const signature = new Uint8Array(await file.slice(0, 8).arrayBuffer());
    if (
        (signature[0] !== 0x50 || signature[1] !== 0x4b) &&
        !['SDMPROF1', 'SDMMACR1'].includes(new TextDecoder().decode(signature))
    )
        throw Error('Choose a plugin, profile or macro ZIP archive.');
    const name = btoa(String.fromCharCode(...new TextEncoder().encode(file.name)));
    for (let offset = 0; offset < file.size; offset += chunkSize) {
        signal.throwIfAborted();
        const bytes = await file.slice(offset, offset + chunkSize).arrayBuffer();
        signal.throwIfAborted();
        await windowInvoke('append_archive_drop', bytes, {
            headers: {
                'x-archive-size': String(file.size),
                'x-archive-offset': String(offset),
                ...(offset === 0 ? { 'x-archive-name': name } : {}),
            },
        });
    }
}

// Isolate Vault file drops from all ZIP and media review paths
export function vaultDropDisposition(
    files: Pick<File, 'name'>[],
    eligible: boolean,
    itemCount = files.length,
) {
    if (!files.some((file) => /\.sdmvault$/i.test(file.name))) return 'other';
    if (itemCount !== 1) return 'multiple';
    return eligible ? 'review' : 'outside';
}

// Connect native and browser archive drops to serialized review
export async function registerArchiveDrops(
    lifetime: StartupLifetime,
    app: () => AppState,
    interactive: () => boolean,
    settleReactive: () => Promise<void>,
    nativeListen?: typeof listen,
) {
    const queue: (File | DroppedDirectory | string)[] = [];
    let nativePending = false;
    let running = false;
    // Check whether the editor is ready to review a queued archive
    const ready = () =>
        !lifetime.disposed &&
        interactive() &&
        !windowCloseOwners.closing &&
        !windowWork.quiescing &&
        reviewers.has('plugin') &&
        reviewers.has('profiles') &&
        !modalOpen();
    // Drain queued archives while the current window can review them
    const drain = async () => {
        if (running || !ready()) return;
        running = true;
        try {
            while (ready() && (queue.length || nativePending)) {
                if (!queue.length) {
                    nativePending = false;
                    const path = await app().bridge.command('next_archive_drop');
                    if (path) {
                        nativePending = true;
                        queue.push(path);
                    }
                    continue;
                }
                const source = queue[0];
                const path = typeof source === 'string' ? source : null;
                const name = typeof source === 'string' ? source.split(/[/\\]/).pop() : source.name;
                try {
                    // Review one dropped archive inside the window-work lifetime
                    const handled = await windowWork.run('dropped archive review', async () => {
                        try {
                            if (typeof source !== 'string') {
                                if (isDroppedDirectory(source))
                                    await stagePluginFolder(source, lifetime.signal);
                                else await stageFile(source, lifetime.signal);
                            }
                            const [reviewPath, kind] = await app().bridge.command(
                                'inspect_archive_drop',
                                { path },
                            );
                            // A dialog may have opened while the file was being read
                            if (!ready()) return false;
                            if (kind === 'macros' && !reviewers.has(kind)) {
                                await ensureDialog('macro-sharing');
                                if (!ready()) return false;
                            }
                            const review = reviewers.get(kind);
                            if (!review) throw Error('Archive review is unavailable.');
                            await review(reviewPath);
                            await settleReactive();
                            return true;
                        } finally {
                            if (path === null && !lifetime.disposed)
                                await app().bridge.command('discard_archive_drop');
                        }
                    });
                    if (!handled) break;
                } catch (error) {
                    if (!lifetime.disposed)
                        app().notify(`Could not review ${name}. ${String(error)}`, 'error');
                }
                queue.shift();
            }
        } catch (error) {
            if (!lifetime.disposed)
                app().notify('Could not read dropped archives. ' + String(error), 'error');
        } finally {
            running = false;
        }
    };
    // Resume draining after a blocking interaction ends
    const resume = () => void drain();
    wake = resume;
    // Explain that a dropped archive is waiting for review
    const queuedNotice = () => {
        if (modalOpen()) app().notify('Archive queued. Close the current dialog to review it.');
    };
    // Recognize browser drags containing files
    const isFileDrag = (event: DragEvent) => event.dataTransfer?.types.includes('Files');
    // Allow file drops only when the current view accepts archives
    const dragOver = (event: DragEvent) => {
        if (!isFileDrag(event)) return;
        event.preventDefault();
        event.stopPropagation();
        if (event.dataTransfer) event.dataTransfer.dropEffect = nativeListen ? 'copy' : 'none';
    };
    // Queue supported dropped files for archive review
    const drop = (event: DragEvent) => {
        if (!isFileDrag(event)) return;
        event.preventDefault();
        event.stopPropagation();
        if (!event.dataTransfer) return;
        const sources = captureDroppedSources(event.dataTransfer);
        if (!sources.length) return;
        if (!nativeListen) {
            app().notify('Import dropped archives in the desktop application.', 'warning');
            return;
        }
        const files = sources.filter((source): source is File => !isDroppedDirectory(source));
        const vaultDialog = document.querySelector<HTMLElement>(
            'dialog.vault-transfer-dialog:modal[data-vault-drop-ready="true"]',
        );
        const vaultTrigger = document.querySelector<HTMLButtonElement>(
            '[data-vault-entry-drop-trigger]',
        );
        const disposition = vaultDropDisposition(
            files,
            app().route === 'vault' &&
                (!!vaultDialog ||
                    (!modalOpen() &&
                        !!vaultTrigger?.getClientRects().length &&
                        !vaultTrigger.disabled)),
            sources.length,
        );
        if (disposition !== 'other') {
            if (disposition === 'review')
                window.dispatchEvent(new CustomEvent('vault-transfer-drop', { detail: files[0] }));
            else
                app().notify(
                    disposition === 'multiple'
                        ? 'Drop one Vault transfer file at a time.'
                        : 'Open unlocked Vault Entries, then drop the Vault transfer file there.',
                    'warning',
                );
            return;
        }
        const isArchive = (source: File | DroppedDirectory) =>
            isDroppedDirectory(source) ? isPluginFolder(source.name) : /\.zip$/i.test(source.name);
        const archives = sources.filter(isArchive);
        const media = files.filter((file) => /\.(png|jpe?g|svg|gif|mp4)$/i.test(file.name));
        const unsupported = sources.some(
            (source) =>
                !isArchive(source) && (isDroppedDirectory(source) || !media.includes(source)),
        );
        // WebKit may retarget a desktop file drop to the document/body. Resolve
        // the accepting modal under the drop point, never an unrelated editor.
        const target = event.target instanceof Element ? event.target : null;
        let dialog = target?.closest('dialog:modal') || null;
        if (!dialog && (!target || target === document.body || target === document.documentElement))
            dialog =
                document
                    .elementsFromPoint(event.clientX, event.clientY)
                    .map((element) => element.closest('dialog:modal'))
                    .find((element) => element !== null) || null;
        if (media.length) {
            if (dialog?.hasAttribute('data-media-drop-target'))
                dialog.dispatchEvent(new CustomEvent('media-file-drop', { detail: media }));
            else
                app().notify(
                    'Open Device background, the media library, or a palette wallpaper editor, then drop the image or video there.',
                    'warning',
                );
        }
        if (unsupported)
            app().notify(
                files.some((file) => isPluginFolder(file.name))
                    ? 'This window could not open the dropped plugin folder. Use Choose plugin folder in Plugins instead.'
                    : 'Unsupported dropped file. Choose an image, video, .sdPlugin folder, or plugin, profile or macro ZIP archive.',
                'warning',
            );
        if (!archives.length) return;
        const available = Math.max(0, maxPending - queue.length);
        queue.push(...archives.slice(0, available));
        if (archives.length > available)
            app().notify(
                'The archive import queue is full. Drop the remaining files again after review.',
                'warning',
            );
        if (available) queuedNotice();
        resume();
    };
    // Capture file drops before individual controls handle internal HTML drags
    window.addEventListener('dragover', dragOver, true);
    window.addEventListener('drop', drop, true);
    document.addEventListener('close', resume, true);
    // Remove drop listeners and stop archive wakeups during teardown
    lifetime.own(() => {
        // Ignore archive wakeups after the drop handler is released
        wake = () => {};
        queue.length = 0;
        window.removeEventListener('dragover', dragOver, true);
        window.removeEventListener('drop', drop, true);
        document.removeEventListener('close', resume, true);
    });
    if (nativeListen)
        await lifetime.listen(() =>
            // Resume review when the native host reports an archive drop
            nativeListen('archive-dropped', () => {
                if (!nativePending) queuedNotice();
                nativePending = true;
                resume();
            }),
        );
    return resume;
}
