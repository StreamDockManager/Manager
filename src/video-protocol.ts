import type { MediaOptions } from '../generated/runtime';

// All positions and frame durations on this wire are integer microseconds
export interface VideoProgress {
    phase: string;
    completed: number;
    total: number | null;
}
export interface VideoMetadata {
    duration_us: number;
    width: number;
    height: number;
    source_fps: number;
    variable_rate: boolean;
}
export interface VideoRequests {
    open: { size: number };
    frame: {
        position: number;
        edge: number;
        continuous?: boolean;
        end?: number;
        target?: { width: number; height: number; options: MediaOptions };
    };
    stop: Record<string, never>;
    step: { position: number; direction: number };
    snap: { position: number };
    prepare: {
        start: number;
        end: number;
        edge: number;
        minimum: number;
        maximum?: number;
        quality: number;
    };
}
export interface VideoResults {
    open: VideoMetadata;
    frame: { bytes: ArrayBuffer; position: number };
    stop: boolean;
    step: number;
    snap: number;
    prepare: boolean;
}
export type VideoRequestKind = keyof VideoRequests;
export type VideoRequest = {
    [K in VideoRequestKind]: { kind: K; id: number } & VideoRequests[K];
}[VideoRequestKind];
export type VideoResult =
    | {
          [K in VideoRequestKind]: {
              kind: 'result';
              request: K;
              id: number;
              result: VideoResults[K];
              error?: never;
          };
      }[VideoRequestKind]
    | { kind: 'result'; request: VideoRequestKind; id: number; error: string; result?: never };
export type VideoBatch = { kind: 'batch'; sequence: number; bytes: ArrayBuffer };
export type VideoWorkerEvent =
    | VideoResult
    | VideoBatch
    | ({ kind: 'progress' } & VideoProgress)
    | { kind: 'read'; id: number; start: number; length: number };
export type VideoWorkerMessage =
    | VideoRequest
    | { kind: 'ack'; sequence: number }
    | { kind: 'read-result'; id: number; bytes: ArrayBuffer; error?: never }
    | { kind: 'read-result'; id: number; error: string; bytes?: never };
