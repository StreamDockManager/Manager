import { defaultMedia } from './bridge';
import type { Draft } from '../generated/runtime';

// Remove references to the deleted asset from the current draft
export function detachDraftMedia(draft: Draft, asset: string) {
    if (draft.base?.asset === asset) {
        delete draft.base.asset;
        draft.base.media = defaultMedia();
    }
    const value = draft.value;
    if (value && typeof value === 'object' && !Array.isArray(value) && value.asset === asset) {
        delete value.asset;
        value.media = defaultMedia();
    }
}
