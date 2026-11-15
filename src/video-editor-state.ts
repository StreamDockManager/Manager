import type { VideoClipDetails } from '../generated/runtime';

/** Decoder readiness describes the original recording, not the independently retained clip. */
export type DecoderState =
    | { kind: 'idle' | 'opening' | 'failed' }
    | { kind: 'ready'; backend: 'webview' | 'gif' | 'ffmpeg' };

/** Synchronous metadata only; callers retain their own loading and recovery lifetimes. */
export function preparedVideoMetadata(
    asset: string,
    details: VideoClipDetails,
    start = details.info?.start_us ?? 0,
) {
    return {
        source: {
            asset,
            start,
            end: start + details.duration_us,
            edge: Math.max(details.prepared_width, details.prepared_height),
            fps: details.prepared_fps,
        },
        label: `${details.prepared_width} × ${details.prepared_height} · ${Number(details.prepared_fps.toFixed(3))} FPS prepared`,
        times: details.frame_times_us.map((time) => time + start),
    };
}
