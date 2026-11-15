import type { AppState } from './state';
import type { Recording, MediaOptions } from '../generated/runtime';
import { windowWork } from './window-work';
export type { VideoProgress, VideoMetadata } from './video-protocol';
import type {
    VideoProgress,
    VideoRequests,
    VideoResults,
    VideoRequestKind,
    VideoResult,
    VideoWorkerEvent,
} from './video-protocol';
import { VideoBatchSink } from './video-batch-sink';
export class VideoSession {
    private worker: Worker;
    private releaseWindowWork: () => unknown;
    private sequence = 0;
    private requests = new Map<
        number,
        {
            resolve: (value: Extract<VideoResult, { result: unknown }>) => void;
            kind: VideoRequestKind;
            reject: (error: Error) => void;
            timer: ReturnType<typeof setTimeout>;
        }
    >();
    private closed = false;
    private sink?: VideoBatchSink;
    // Initialize video session state and its required lifecycle hooks
    constructor(
        app: AppState,
        private recording: Recording,
        progress: (value: VideoProgress) => void,
    ) {
        this.releaseWindowWork = windowWork.begin('recording decoder');
        try {
            this.worker = new Worker(new URL('./video-worker.ts', import.meta.url), {
                type: 'module',
            });
        } catch (error) {
            this.releaseWindowWork();
            throw error;
        }
        this.worker.onerror = (event) => this.fail(Error(event.message));
        // Complete the deferred video session work using the current operation state
        this.worker.onmessage = (event) => {
            const message: VideoWorkerEvent = event.data;
            if (this.closed) return;
            if (message.kind === 'read') {
                void app.bridge
                    .command('read_video_recording', {
                        recording: recording.id,
                        start: message.start,
                        length: message.length,
                    })
                    // Process the settled video session operation result
                    .then((bytes) => {
                        if (!this.closed)
                            this.worker.postMessage(
                                { kind: 'read-result', id: message.id, bytes },
                                [bytes],
                            );
                    })
                    // Handle a failed video session operation without retaining stale work
                    .catch((error) => {
                        if (!this.closed)
                            this.worker.postMessage({
                                kind: 'read-result',
                                id: message.id,
                                error: String(error),
                            });
                    });
            } else if (message.kind === 'progress') progress(message);
            else if (message.kind === 'batch') {
                if (!this.sink) this.fail(Error('Unexpected video frame batch'));
                else void this.sink.append(message);
            } else if (message.kind === 'result') {
                const pending = this.requests.get(message.id);
                if (!pending) return;
                if (pending.kind !== message.request) {
                    this.fail(Error('Unexpected video response'));
                    return;
                }
                if (message.request === 'prepare' && this.sink?.pending) {
                    this.fail(Error('Unexpected video completion'));
                    return;
                }
                clearTimeout(pending.timer);
                this.requests.delete(message.id);
                if (message.error !== undefined) pending.reject(Error(message.error));
                else pending.resolve(message);
            }
        };
    }
    request<K extends VideoRequestKind>(kind: K, args: VideoRequests[K]): Promise<VideoResults[K]>;
    // Request the next video session update
    request(
        kind: VideoRequestKind,
        args: VideoRequests[VideoRequestKind],
    ): Promise<VideoResults[VideoRequestKind]> {
        if (this.closed) return Promise.reject(Error('Video editor closed'));
        if (this.requests.size >= 64)
            return Promise.reject(Error('Too many pending video requests'));
        const id = ++this.sequence;
        // Resolve the video session operation when its asynchronous work finishes
        return new Promise<VideoResults[VideoRequestKind]>((resolve, reject) => {
            // Run the deferred video session update after its scheduled delay
            const timer = setTimeout(() => {
                this.fail(Error('Video decoding timed out. Retry or choose a smaller section.'));
            }, 120000);
            this.requests.set(id, {
                kind,
                // Resolve the pending request with the worker result payload
                resolve: (message) => resolve(message.result),
                reject,
                timer,
            });
            try {
                this.worker.postMessage({ kind, id, ...args });
            } catch (error) {
                this.fail(Error(String(error)));
            }
        });
    }
    // Open the current video session target
    open() {
        return this.request('open', { size: this.recording.bytes });
    }
    // Process the current video session frame
    frame(
        position: number,
        edge = 512,
        playback: {
            continuous?: boolean;
            end?: number;
            target?: { width: number; height: number; options: MediaOptions };
        } = {},
    ) {
        return this.request('frame', {
            position,
            edge,
            ...playback,
        });
    }
    // Pause the current video session operation
    pause() {
        void this.request('stop', {}).catch(() => {});
    }
    // Advance the video session workflow by one step
    step(position: number, direction: number) {
        return this.request('step', { position, direction });
    }
    // Prepare state required by the video session operation
    async prepare(
        id: string,
        start: number,
        end: number,
        edge: number,
        minimum = 64,
        maximum?: number,
    ) {
        if (this.sink) throw Error('Video preparation already active');
        this.sink = new VideoBatchSink(
            id,
            (sequence) => this.worker.postMessage({ kind: 'ack', sequence }),
            (error) => this.fail(error),
        );
        try {
            await this.request('prepare', { start, end, edge, minimum, maximum, quality: 92 });
        } finally {
            this.sink?.close();
            this.sink = undefined;
        }
    }
    // Close the video session, terminate its worker and reject every outstanding request
    private fail(error: Error) {
        if (this.closed) return;
        this.closed = true;
        this.sink?.close();
        this.worker.terminate();
        this.releaseWindowWork();
        for (const pending of this.requests.values()) {
            clearTimeout(pending.timer);
            pending.reject(error);
        }
        this.requests.clear();
    }
    // Release video session state and associated lifecycle resources
    close() {
        this.fail(Error('Video editor closed'));
    }
}
