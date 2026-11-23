import { windowInvoke } from './window-native';

const chunkBytes = 1024 * 1024;
const maxEntriesPerKind = 4096;
const maxFileBytes = 64 * chunkBytes;
const maxFolderBytes = 256 * chunkBytes;
const maxDepth = 64;
const maxComponentBytes = 255;
const maxPathBytes = 4096;
const encoder = new TextEncoder();

export interface DroppedDirectory {
    readonly name: string;
    readonly directory: FileSystemDirectoryEntry;
}

// Capture directory handles before the protected drop data store becomes unavailable
export function captureDroppedSources(transfer: DataTransfer) {
    const sources: (File | DroppedDirectory)[] = [];
    let hasDirectory = false;
    for (const item of Array.from(transfer.items ?? [])) {
        if (item.kind !== 'file') continue;
        const entry = item.webkitGetAsEntry?.();
        if (entry?.isDirectory) {
            sources.push({ name: entry.name, directory: entry as FileSystemDirectoryEntry });
            hasDirectory = true;
        } else {
            const file = item.getAsFile();
            if (file) sources.push(file);
        }
    }
    return hasDirectory ? sources : Array.from(transfer.files);
}

export function isDroppedDirectory(source: File | DroppedDirectory): source is DroppedDirectory {
    return 'directory' in source;
}

export function isPluginFolder(name: string) {
    return /\.sdplugin$/i.test(name) && name.length > '.sdplugin'.length;
}

// Reject unsafe, nonportable names rather than rewriting the selected package
function safeComponent(name: string) {
    return (
        !!name &&
        encoder.encode(name).length <= maxComponentBytes &&
        !/[<>:"/\\|?*\x00-\x1f\x7f]/.test(name) &&
        !/[ .]$/.test(name) &&
        !/^(?:con|prn|aux|nul|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)/i.test(name)
    );
}

// Read a single Chromium/WebKit batch from the directory cursor
function readEntries(reader: FileSystemDirectoryReader) {
    return new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
}

// Read a file snapshot without eagerly loading its contents
function readFile(entry: FileSystemFileEntry) {
    return new Promise<File>((resolve, reject) => entry.file(resolve, reject));
}

// Traverse each directory cursor to exhaustion and preflight the whole folder
async function collectFolder(source: DroppedDirectory, signal: AbortSignal) {
    if (!isPluginFolder(source.name) || !safeComponent(source.name))
        throw Error('Choose a folder named with the .sdPlugin extension.');
    let total = 0;
    let directories = 0;
    const seen = new Set<string>();
    const files: { path: string; file: File }[] = [];

    async function walk(folder: FileSystemDirectoryEntry, prefix: string, depth: number) {
        const reader = folder.createReader();
        while (true) {
            signal.throwIfAborted();
            const entries = await readEntries(reader);
            signal.throwIfAborted();
            if (!entries.length) return;
            for (const entry of entries) {
                const path = prefix + entry.name;
                const folded = path.toLowerCase();
                if (
                    !safeComponent(entry.name) ||
                    encoder.encode(path).length > maxPathBytes ||
                    seen.has(folded)
                )
                    throw Error('The plugin folder contains an unsafe or duplicate path.');
                if (depth > maxDepth)
                    throw Error('The plugin folder exceeds the file count or nesting limit.');
                seen.add(folded);
                if (entry.isDirectory) {
                    if (++directories > maxEntriesPerKind)
                        throw Error('The plugin folder exceeds the directory count limit.');
                    await walk(entry as FileSystemDirectoryEntry, path + '/', depth + 1);
                } else if (entry.isFile) {
                    if (files.length === maxEntriesPerKind)
                        throw Error('The plugin folder exceeds the file count limit.');
                    const file = await readFile(entry as FileSystemFileEntry);
                    signal.throwIfAborted();
                    if (file.size > maxFileBytes || total + file.size > maxFolderBytes)
                        throw Error(
                            'Plugin folders support up to 64 MiB per file and 256 MiB total.',
                        );
                    total += file.size;
                    files.push({ path, file });
                } else throw Error('The plugin folder contains an unsupported entry.');
            }
        }
    }

    await walk(source.directory, '', 1);
    if (!files.length) throw Error('The plugin folder contains no files.');
    return files;
}

function encodeHeader(value: string) {
    return btoa(String.fromCharCode(...encoder.encode(value)));
}

// Send bounded chunks to the same temporary review lifecycle as a ZIP
export async function stagePluginFolder(source: DroppedDirectory, signal: AbortSignal) {
    const files = await collectFolder(source, signal);
    const root = encodeHeader(source.name);
    let start = true;
    for (const { path, file } of files) {
        const relative = encodeHeader(path);
        let offset = 0;
        do {
            signal.throwIfAborted();
            const expected = Math.min(chunkBytes, file.size - offset);
            const bytes = await file.slice(offset, offset + expected).arrayBuffer();
            signal.throwIfAborted();
            if (bytes.byteLength !== expected)
                throw Error('A file in the plugin folder changed while it was being read.');
            await windowInvoke('append_plugin_folder_drop', bytes, {
                headers: {
                    'x-plugin-folder': root,
                    'x-plugin-file': relative,
                    'x-plugin-size': String(file.size),
                    'x-plugin-offset': String(offset),
                    ...(start ? { 'x-plugin-start': '1' } : {}),
                },
            });
            start = false;
            offset += bytes.byteLength;
        } while (offset < file.size);
    }
}
