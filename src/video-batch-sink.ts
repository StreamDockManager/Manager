import { windowInvoke as invoke } from './window-native';
import type { VideoBatch } from './video-protocol';

// Exactly one append owns the transferred buffer until native persistence succeeds
// Reject excess or replayed batches instead of growing an unbounded promise queue
export class VideoBatchSink {
    private sequence = 0;
    private busy = false;
    private closed = false;
    // Initialize video batch sink state and its required lifecycle hooks
    constructor(
        private id: string,
        private acknowledge: (sequence: number) => void,
        private fail: (error: Error) => void,
        private current: () => boolean = () => true,
    ) {}
    // Derive pending from the current video batch sink state
    get pending() {
        return this.busy;
    }
    // Release video batch sink state and associated lifecycle resources
    close() {
        this.closed = true;
    }
    // Append the new entry to the video batch sink collection
    async append(batch: VideoBatch) {
        if (this.closed) return;
        try {
            if (!this.current()) throw Error('Media selection changed');
            if (this.busy || batch.sequence !== this.sequence)
                throw Error('Unexpected video frame batch sequence');
            this.busy = true;
            await invoke('append_browser_media', batch.bytes, {
                headers: {
                    'x-preparation-id': this.id,
                    'x-preparation-sequence': String(this.sequence),
                    'x-preparation-timing': 'us',
                },
            });
            if (this.closed) return;
            if (!this.current()) throw Error('Media selection changed');
            this.busy = false;
            this.sequence++;
            this.acknowledge(batch.sequence);
        } catch (error) {
            if (this.closed) return;
            this.close();
            this.fail(Error(String(error)));
        }
    }
}
