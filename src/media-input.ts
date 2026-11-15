import type { MediaOptions } from '../generated/runtime';
import { defaultMedia, native } from './bridge';
import { trackCloseRelease } from './close-releases';
import { importPreparedMedia } from './media-jobs';
import type { AppState } from './state';
import { openVideoEditor } from './video-editor';
import { open } from './window-dialogs';
import { windowInvoke } from './window-native';
import { windowWork } from './window-work';

export const mediaFormats = ['png', 'jpeg', 'svg', 'gif', 'mp4'];
// Select the file extensions supported by managed media import
export const managedFormats = (formats: string[]) =>
    formats.includes('png') && !formats.includes('svg') ? [...formats, 'svg'] : formats;

type Selection = {
    owner: string;
    formats: string[];
    current: () => boolean;
    controlId?: string;
    apply?: (asset: string, options: MediaOptions) => void | Promise<void>;
    saved?: () => void;
};

// Extract the normalized format from the selected file name
function fileFormat(name: string) {
    const extension = name.split('.').pop()?.toLowerCase();
    return extension === 'jpg' ? 'jpeg' : extension || '';
}

// Choose a media file through the native or browser file picker
export async function chooseMediaInput(formats: string[]) {
    if (!native) throw Error('Choosing background media requires the desktop application.');
    return open({
        multiple: false,
        filters: [
            {
                name: 'Images and videos',
                extensions: formats.flatMap((format) =>
                    format === 'jpeg' ? ['jpg', 'jpeg'] : [format],
                ),
            },
        ],
    });
}

// Require one supported file from the drop event
export function droppedMedia(event: CustomEvent<File[]>): File {
    if (event.detail.length !== 1) throw Error('Drop one image or video at a time.');
    return event.detail[0];
}

// Prepare the selected file or path for the current media picker request
export async function selectMediaInput(app: AppState, source: string | File, selection: Selection) {
    if (!native) throw Error('Import media in the desktop application.');
    const format = fileFormat(typeof source === 'string' ? source : source.name);
    if (!selection.formats.includes(format))
        throw Error('Choose a supported image or video for this surface.');
    if (!selection.current()) return;
    let release: (() => Promise<void>) | undefined;
    let transferred = false;
    try {
        let path: string;
        if (typeof source === 'string') {
            path = source;
        } else {
            const id = crypto.randomUUID();
            release = () =>
                trackCloseRelease('media-drop:' + id, () =>
                    app.bridge.command('discard_media_drop', { id }),
                );
            // Transfer dropped file bytes through the owned window operation
            path = await windowWork.run('dropped media transfer', async () => {
                const chunk = 1024 * 1024;
                const limit =
                    format === 'mp4' ? 8192 * chunk : format === 'svg' ? 256 * 1024 : 16 * chunk;
                if (!source.size || source.size > limit)
                    throw Error('The dropped media is empty or exceeds its size limit.');
                for (let offset = 0; offset < source.size; offset += chunk) {
                    if (!selection.current()) throw Error('Media selection changed');
                    const bytes = await source.slice(offset, offset + chunk).arrayBuffer();
                    if (!selection.current()) throw Error('Media selection changed');
                    await windowInvoke('append_media_drop', bytes, {
                        headers: {
                            'x-media-id': id,
                            'x-media-size': String(source.size),
                            'x-media-offset': String(offset),
                            'x-media-format': format,
                        },
                    });
                }
                return app.bridge.command('finish_media_drop', { id });
            });
        }
        if (!selection.current()) return;
        const animated =
            format === 'mp4' ||
            (format === 'gif' && (await app.bridge.command('gif_has_animation', { path })));
        if (!selection.current()) return;
        if (animated) {
            openVideoEditor({
                path,
                owner: selection.owner,
                controlId: selection.controlId,
                current: selection.current,
                apply: selection.apply,
                saved: selection.saved,
                releaseSource: release,
            });
            transferred = true;
            return;
        }
        const asset = await importPreparedMedia(app, path, selection.owner, selection.current);
        if (selection.current()) {
            if (selection.apply) await selection.apply(asset, defaultMedia());
            else selection.saved?.();
        }
    } finally {
        if (!transferred) await release?.();
    }
}
