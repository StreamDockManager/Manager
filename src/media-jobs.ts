import { retainMediaFor } from './media-leases';
import type AlpineType from '@alpinejs/csp';
import type { AppState } from './state';
import type { MediaPreparation } from '../generated/runtime';
import { windowWork } from './window-work';
import { windowCloseOwners } from './window-close-owners';
import { trackCloseRelease } from './close-releases';
let sequence = 0;
const session = crypto.randomUUID();
const activeJobs = new Set<string>();
// Cancel the owned media job and update its active-job tracking
const cancelJob = (app: AppState, id: string) =>
    trackCloseRelease('media-preparation:' + id, () =>
        app.bridge.command('cancel_media_preparation', { id }),
    );
// Release tracking for a media job that has finished
const finishJob = (app: AppState, id: string) =>
    trackCloseRelease('media-preparation-finish:' + id, () =>
        app.bridge.command('finish_media_preparation', { id }),
    );
let store: { jobs: MediaPreparation[]; cancel: (event: Event) => void };
// Register media-job cancellation and window-close ownership
export function registerMediaJobs(Alpine: typeof AlpineType, app: () => AppState) {
    Alpine.store('mediaJobs', {
        jobs: [] as MediaPreparation[],
        // Cancel the media job identified by the control event
        cancel(event: Event) {
            const id = (event.currentTarget as HTMLElement).dataset.id;
            if (id)
                void cancelJob(app(), id).catch((error) => app().notify(String(error), 'error'));
        },
    });
    store = Alpine.store('mediaJobs') as typeof store;
    windowCloseOwners.register({
        name: 'Media preparation',
        // Report whether the controller retains unsaved or active work
        state: () => (activeJobs.size ? 'dirty' : 'clean'),
        // Discard the staged media or editor state owned by the current dialog
        discard: async () => {
            await Promise.all([...activeJobs].map((id) => cancelJob(app(), id)));
        },
        // Release resources retained by the current controller
        release: async () => {
            await Promise.all([...activeJobs].map((id) => cancelJob(app(), id)));
        },
    });
}
// Import already prepared media under an owned background job
export function importPreparedMedia(
    app: AppState,
    path: string,
    key: string,
    current: () => boolean = () => true,
): Promise<string> {
    return windowWork.run('media import', () =>
        importMedia(app, path, key, () => !windowCloseOwners.closing && current()),
    );
}

// Run the media import job and release ownership when it settles
async function importMedia(
    app: AppState,
    path: string,
    key: string,
    current: () => boolean,
): Promise<string> {
    const generation = ++sequence;
    const id = await app.command('prepare_media', {
        path,
        key: session + '|' + key,
        generation,
    });
    activeJobs.add(id);
    try {
        for (;;) {
            const job = await app.bridge.command('media_preparation', { id });
            store.jobs = [...store.jobs.filter((j) => j.id !== id), job].slice(-8);
            if (!current()) {
                await cancelJob(app, id);
                throw Error('Media selection changed');
            }
            if (job.state === 'complete' && job.asset) {
                await retainMediaFor(app, job.asset, key);
                if (!current()) throw Error('Media selection changed');
                return job.asset;
            }
            if (job.state === 'failed' || job.state === 'cancelled') {
                throw Error(job.error || 'Media preparation cancelled');
            }
            await new Promise((resolve) => setTimeout(resolve, 100));
        }
    } finally {
        try {
            await finishJob(app, id);
        } catch {
            /* Active cancelled work retires when the decoder exits. */
        }
        store.jobs = store.jobs.filter((j) => j.id !== id);
        activeJobs.delete(id);
    }
}
