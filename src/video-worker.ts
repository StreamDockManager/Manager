import { Input, CustomSource, Mp4InputFormat, CanvasSink, EncodedPacketSink } from 'mediabunny';
import type { InputVideoTrack, WrappedCanvas } from 'mediabunny';
import type { MediaOptions } from '../generated/runtime';
import { VideoIndex } from './video-index';
import type { VideoWorkerMessage, VideoWorkerEvent } from './video-protocol';

// The worker owns decoding only. File reads and publication remain checked by Rust
const worker = globalThis as unknown as {
    onmessage: ((event: MessageEvent<VideoWorkerMessage>) => void) | null;
    postMessage(value: VideoWorkerEvent, transfer?: Transferable[]): void;
};
let input: Input | undefined;
let track: InputVideoTrack | undefined;
let times = new VideoIndex();
let origin = 0;
let duration = 0;
let width = 0;
let height = 0;
const frameSinks = new Map<number, CanvasSink>();
let stream:
    | {
          edge: number;
          end: number;
          last: number;
          iterator: AsyncGenerator<WrappedCanvas, void, unknown>;
          current: WrappedCanvas;
          next: IteratorResult<WrappedCanvas, void>;
      }
    | undefined;
let targetCanvas: OffscreenCanvas | undefined;
// Stop the stream
async function stopStream() {
    const previous = stream;
    stream = undefined;
    await previous?.iterator.return();
}
// Select a decoded preview frame or maintain a continuous stream within the requested range
async function previewFrame(
    sink: CanvasSink,
    edge: number,
    message: { position: number; continuous?: boolean; end?: number },
) {
    const point = origin + message.position / 1_000_000;
    const end = origin + Math.min(duration, (message.end ?? duration * 1_000_000) / 1_000_000);
    if (!message.continuous) {
        if (edge !== 160) await stopStream();
        return sink.getCanvas(point);
    }
    if (
        !stream ||
        stream.edge !== edge ||
        stream.end !== end ||
        point < stream.last - 0.0000005 ||
        point - stream.last > 1
    ) {
        await stopStream();
        const start = times.at(Math.max(0, lowerBound(point + 0.0000005) - 1)) ?? origin;
        const iterator = sink.canvases(start, end);
        const current = await iterator.next();
        if (current.done) {
            await iterator.return();
            return null;
        }
        stream = {
            edge,
            end,
            last: point,
            iterator,
            current: current.value,
            next: await iterator.next(),
        };
    }
    while (!stream.next.done && stream.next.value.timestamp <= point + 0.0000005) {
        stream.current = stream.next.value;
        stream.next = await stream.iterator.next();
    }
    stream.last = point;
    return stream.current;
}
// Prepare a frame canvas with validated crop and target dimensions for image encoding
function frameOutput(
    frame: WrappedCanvas,
    target?: { width: number; height: number; options: MediaOptions },
) {
    if (!target)
        return { canvas: frame.canvas as OffscreenCanvas, type: 'image/jpeg', quality: 0.9 };
    const { width: w, height: h, options } = target;
    const [x, y, cw, ch] = options.crop;
    if (
        ![w, h].every((value) => Number.isInteger(value) && value > 0 && value <= 4096) ||
        ![x, y, cw, ch].every(Number.isFinite) ||
        x < 0 ||
        y < 0 ||
        cw <= 0 ||
        ch <= 0 ||
        x + cw > 1.000001 ||
        y + ch > 1.000001
    )
        throw Error('Invalid video framing');
    if (!targetCanvas || targetCanvas.width !== w || targetCanvas.height !== h)
        targetCanvas = new OffscreenCanvas(w, h);
    const context = targetCanvas.getContext('2d')!;
    context.clearRect(0, 0, w, h);
    const black = options.letterbox === 'black';
    if (black) {
        context.fillStyle = '#000';
        context.fillRect(0, 0, w, h);
    }
    const sx = Math.min(frame.canvas.width - 1, Math.floor(x * frame.canvas.width));
    const sy = Math.min(frame.canvas.height - 1, Math.floor(y * frame.canvas.height));
    const sw = Math.max(1, Math.min(frame.canvas.width - sx, Math.round(cw * frame.canvas.width)));
    const sh = Math.max(
        1,
        Math.min(frame.canvas.height - sy, Math.round(ch * frame.canvas.height)),
    );
    const fit = options.fit === 'fit';
    const scale = (fit ? Math.min : Math.max)(w / sw, h / sh);
    const dw = Math.round(sw * scale),
        dh = Math.round(sh * scale);
    context.imageSmoothingEnabled = true;
    context.imageSmoothingQuality = 'low';
    context.drawImage(
        frame.canvas,
        sx,
        sy,
        sw,
        sh,
        Math.floor((w - dw) / 2),
        Math.floor((h - dh) / 2),
        dw,
        dh,
    );
    const transparent = fit && !black && (dw < w || dh < h);
    return {
        canvas: targetCanvas,
        type: options.encoding === 'png' || transparent ? 'image/png' : 'image/jpeg',
        quality: (options.jpeg_quality ?? 75) / 100,
    };
}
let readSequence = 0;
let acknowledge: { sequence: number; resolve: () => void } | undefined;
const reads = new Map<
    number,
    { resolve: (bytes: Uint8Array) => void; reject: (error: Error) => void }
>();
let serial = Promise.resolve();
// Complete the deferred video worker work using the current operation state
worker.onmessage = (event) => {
    const message = event.data;
    if (message.kind === 'ack') {
        if (acknowledge?.sequence === message.sequence) {
            acknowledge.resolve();
            acknowledge = undefined;
        }
        return;
    }
    if (message.kind === 'read-result') {
        const pending = reads.get(message.id);
        reads.delete(message.id);
        if (message.error !== undefined) pending?.reject(Error(message.error));
        else pending?.resolve(new Uint8Array(message.bytes));
        return;
    }
    // Process the settled video worker operation result
    serial = serial.then(async () => {
        try {
            if (message.kind === 'stop') {
                await stopStream();
                worker.postMessage({
                    kind: 'result',
                    request: message.kind,
                    id: message.id,
                    result: true,
                });
                return;
            }
            if (message.kind === 'open') {
                worker.postMessage({
                    kind: 'result',
                    request: message.kind,
                    id: message.id,
                    result: await open(message.size),
                });
            } else if (message.kind === 'frame') {
                if (!track) throw Error('Open a recording first');
                const edge = Math.max(64, Math.min(1024, message.edge || 512));
                let sink = frameSinks.get(edge);
                if (!sink)
                    sink = new CanvasSink(track, {
                        width: Math.max(
                            1,
                            Math.round(width * Math.min(1, edge / width, edge / height)),
                        ),
                        poolSize: 2,
                    });
                frameSinks.delete(edge);
                frameSinks.set(edge, sink);
                while (frameSinks.size > 2) frameSinks.delete(frameSinks.keys().next().value!);
                const frame = await previewFrame(sink, edge, message);
                if (!frame || !(frame.canvas instanceof OffscreenCanvas))
                    throw Error('Frame unavailable');
                const output = frameOutput(frame, message.target);
                const bytes = await (
                    await output.canvas.convertToBlob({
                        type: output.type,
                        quality: output.quality,
                    })
                ).arrayBuffer();
                worker.postMessage(
                    {
                        kind: 'result',
                        request: message.kind,
                        id: message.id,
                        result: {
                            bytes,
                            position: Math.round((frame.timestamp - origin) * 1_000_000),
                        },
                    },
                    [bytes],
                );
                return;
            } else if (message.kind === 'step') {
                const point = message.position / 1_000_000 + origin;
                let index = lowerBound(point - 0.0000005);
                index =
                    message.direction < 0
                        ? index - 1
                        : (times.at(index) ?? Infinity) <= point + 0.0000005
                          ? index + 1
                          : index;
                const result = Math.round(
                    ((times.at(Math.max(0, Math.min(times.length - 1, index))) ?? origin) -
                        origin) *
                        1_000_000,
                );
                worker.postMessage({
                    kind: 'result',
                    request: message.kind,
                    id: message.id,
                    result,
                });
            } else if (message.kind === 'snap') {
                const point = message.position / 1_000_000 + origin;
                const index = Math.max(0, lowerBound(point + 0.0000005) - 1);
                const result =
                    message.position >= Math.round(duration * 1_000_000)
                        ? Math.round(duration * 1_000_000)
                        : Math.round((times.at(index)! - origin) * 1_000_000);
                worker.postMessage({
                    kind: 'result',
                    request: message.kind,
                    id: message.id,
                    result,
                });
            } else if (message.kind === 'prepare') {
                await stopStream();
                await prepare(
                    message.start,
                    message.end,
                    message.edge,
                    message.quality / 100,
                    message.minimum || 64,
                    message.maximum,
                );
                worker.postMessage({
                    kind: 'result',
                    request: message.kind,
                    id: message.id,
                    result: true,
                });
            }
        } catch (error) {
            worker.postMessage({
                kind: 'result',
                request: message.kind,
                id: message.id,
                error: String(error),
            });
        }
    });
};
// Find the first indexed video timestamp at or after the requested time
function lowerBound(value: number) {
    return times.lowerBound(value);
}
// Open the current video worker target
async function open(size: number) {
    if (typeof VideoDecoder === 'undefined' || typeof OffscreenCanvas === 'undefined')
        throw Error('WebCodecs worker decoding unavailable');
    await stopStream();
    input?.dispose();
    frameSinks.clear();
    times = new VideoIndex();
    const custom = new CustomSource({
        // Get size for the video worker workflow
        getSize: () => size!,
        maxCacheSize: 8 * 1024 * 1024,
        // Read the requested byte range through bounded one-MiB host requests
        read: async (start, end) => {
            const chunks: Uint8Array[] = [];
            for (let offset = start; offset < end; offset += 1024 * 1024) {
                const id = ++readSequence;
                // Resolve the video worker operation when its asynchronous work finishes
                const bytes = await new Promise<Uint8Array>((resolve, reject) => {
                    reads.set(id, { resolve, reject });
                    worker.postMessage({
                        kind: 'read',
                        id,
                        start: offset,
                        length: Math.min(1024 * 1024, end - offset),
                    });
                });
                chunks.push(bytes);
            }
            const bytes = new Uint8Array(end - start);
            let offset = 0;
            for (const chunk of chunks) {
                bytes.set(chunk, offset);
                offset += chunk.length;
            }
            return bytes;
        },
    });
    input = new Input({
        source: custom,
        formats: [new Mp4InputFormat()],
    });
    track = (await input.getPrimaryVideoTrack()) ?? undefined;
    if (!track || !(await track.canDecode()))
        throw Error('Video codec unavailable in this webview; use the installed FFmpeg fallback');
    origin = await track.getFirstTimestamp();
    duration = (await track.computeDuration()) - origin;
    width = await track.getDisplayWidth();
    height = await track.getDisplayHeight();
    if (
        !Number.isFinite(duration) ||
        duration <= 0 ||
        duration > 14400 ||
        width <= 0 ||
        height <= 0 ||
        width > 4096 ||
        height > 4096 ||
        width * height > 4096 * 2160
    )
        throw Error('Recording exceeds four hours or 4K dimensions');
    for await (const packet of new EncodedPacketSink(track).packets(undefined, undefined, {
        metadataOnly: true,
    })) {
        times.append(packet.timestamp);
        if (times.length % 8192 === 0)
            worker.postMessage({
                kind: 'progress',
                phase: 'Indexing recording',
                completed: times.length,
                total: null,
            });
    }
    if (!times.length) throw Error('Recording has no video frames');
    times.finish();
    return {
        duration_us: Math.round(duration * 1_000_000),
        width,
        height,
        ...times.timing(duration),
    };
}
// Prepare state required by the video worker operation
async function prepare(
    start: number,
    end: number,
    edge: number,
    quality: number,
    minimum = 1,
    maximum?: number,
) {
    if (
        !track ||
        !Number.isFinite(start) ||
        !Number.isFinite(end) ||
        start < 0 ||
        end <= start ||
        end - start > 30_000_000 ||
        end > Math.round(duration * 1_000_000) ||
        edge < 64 ||
        edge > 1024
    )
        throw Error('Choose a section of at most 30 seconds and detail within 64–1024 pixels');
    const first = Math.max(0, lowerBound(origin + start / 1_000_000 + 0.0000001) - 1);
    const last = lowerBound(origin + end / 1_000_000 - 0.0000001);
    let selected: number[];
    const cappedCount =
        maximum && maximum > 0 ? Math.ceil(((end - start) / 1_000_000) * maximum) : Infinity;
    if (cappedCount < last - first) {
        if (cappedCount > 8192)
            throw Error(
                'Clip exceeds 8,192 prepared frames; choose a shorter section or lower cap',
            );
        selected = Array.from(
            { length: cappedCount },
            (_, index) => origin + start / 1_000_000 + index / maximum!,
        );
    } else {
        if (last - first > 8192)
            throw Error('Clip exceeds 8,192 source frames; choose a shorter section or lower cap');
        selected = times.range(first, last, origin + start / 1_000_000);
    }
    if (!selected.length) throw Error('No frames in selected clip');
    const scale = Math.max(
        minimum / Math.max(width, height),
        Math.min(1, edge / width, edge / height),
    );
    const sink = new CanvasSink(track, {
        width: Math.max(1, Math.round(width * scale)),
        height: Math.max(1, Math.round(height * scale)),
        fit: 'contain',
        poolSize: 2,
    });
    let batch: { delay: number; bytes: ArrayBuffer }[] = [],
        batchBytes = 4,
        total = 0,
        index = 0;
    let batchSequence = 0;
    // Flush pending video worker work to its consumer
    const flush = async () => {
        if (!batch.length) return;
        const bytes = new ArrayBuffer(batchBytes),
            view = new DataView(bytes);
        view.setUint32(0, batch.length, true);
        let offset = 4;
        for (const frame of batch) {
            view.setUint32(offset, frame.delay, true);
            view.setUint32(offset + 4, frame.bytes.byteLength, true);
            new Uint8Array(bytes, offset + 8, frame.bytes.byteLength).set(
                new Uint8Array(frame.bytes),
            );
            offset += 8 + frame.bytes.byteLength;
        }
        batch = [];
        batchBytes = 4;
        const ack = new Promise<void>((resolve) => {
            acknowledge = { sequence: batchSequence, resolve };
        });
        worker.postMessage({ kind: 'batch', bytes, sequence: batchSequence }, [bytes]);
        await ack;
        batchSequence++;
    };
    for await (const wrapped of sink.canvasesAtTimestamps(selected)) {
        if (!wrapped || !(wrapped.canvas instanceof OffscreenCanvas))
            throw Error('Offscreen video canvas unavailable');
        const bytes = await (
            await wrapped.canvas.convertToBlob({ type: 'image/jpeg', quality })
        ).arrayBuffer();
        total += bytes.byteLength;
        if (bytes.byteLength > 1024 * 1024 - 12 || total > 64 * 1024 * 1024)
            throw Error('Clip exceeds 64 MiB; reduce maximum detail or duration');
        if (batch.length === 8 || batchBytes + bytes.byteLength + 8 > 1024 * 1024) await flush();
        const next =
            index + 1 < selected.length
                ? Math.round((selected[index + 1] - origin) * 1_000_000)
                : end;
        const current = index === 0 ? start : Math.round((selected[index] - origin) * 1_000_000);
        batch.push({ delay: Math.max(1, next - current), bytes });
        batchBytes += 8 + bytes.byteLength;
        index++;
        worker.postMessage({
            kind: 'progress',
            phase: 'Preparing clip',
            completed: index,
            total: selected.length,
        });
    }
    if (index !== selected.length) throw Error('Incomplete video preparation');
    await flush();
}
