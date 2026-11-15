import Alpine from '@alpinejs/csp';
import type { AppState } from './state';
import type { Draft } from '../generated/runtime';
import { DraftWriteQueue } from './draft-write-queue';
import { windowWork, WindowQuiescingError, type WindowFence } from './window-work';
import { windowCloseOwners } from './window-close-owners';
import { releaseAllWindowMedia, releaseMediaOwner } from './media-leases';
// Read the shared Alpine application store used by route controllers
export const app = () => Alpine.store('app') as AppState;
// Serialize bindings with sorted object keys for order-independent change detection
export const bindingSignature = (value: unknown) =>
    JSON.stringify(value, (_key, v) =>
        v && typeof v === 'object' && !Array.isArray(v)
            ? Object.fromEntries(Object.entries(v).sort(([a], [b]) => a.localeCompare(b)))
            : v,
    );
// Persist drafts under ordinary window ownership or an explicit cleanup fence
export function ownDraftWork<T>(fence: WindowFence | undefined, operation: () => Promise<T>) {
    return fence
        ? windowWork.cleanup(fence, 'draft persistence', operation)
        : windowWork.run('draft persistence', operation);
}

export const draftQueue = new DraftWriteQueue<Draft, WindowFence>(
    (key, draft, fence) =>
        ownDraftWork(fence, () => app().bridge.command('save_draft', { key, draft })),
    (error) => {
        if (!(error instanceof WindowQuiescingError)) app().notify(String(error), 'error');
    },
    // Complete the deferred context work using the current operation state
    (fence) => {
        if (!fence) return windowWork.begin('queued draft');
        if (!windowWork.current(fence)) throw new WindowQuiescingError();
        let release!: () => void;
        const settled = new Promise<void>((resolve) => (release = resolve));
        // Keep delayed queue work owned, but grant the command capability only
        // at the later write invocation with this exact fence
        void windowWork.cleanup(fence, 'queued draft', () => settled).catch(() => {});
        return release;
    },
);
export const pendingDraftWrites = draftQueue.pending;
// Recovery created by this document remains owned even after its controller unmounts
// Untouched recovery loaded from disk never enters this set
const windowDrafts = new WeakMap<object, Map<string, { draft: Draft; dirty: boolean }>>();
// Lazily allocate draft tracking for the current raw Alpine application owner
function currentWindowDrafts() {
    const owner = Alpine.raw(app());
    let drafts = windowDrafts.get(owner);
    if (!drafts) windowDrafts.set(owner, (drafts = new Map()));
    return drafts;
}
// Retain the raw draft and dirty flag under its window-local identity
export function rememberWindowDraft(key: string, draft: Draft, dirty = true) {
    currentWindowDrafts().set(key, { draft: Alpine.raw(draft), dirty });
}
// Remove the matching entry from this window's draft registry
export function forgetWindowDraft(key: string) {
    currentWindowDrafts().delete(key);
}
// Update the window draft state
export function updateWindowDraftState(key: string, dirty: boolean) {
    const record = currentWindowDrafts().get(key);
    if (record) record.dirty = dirty;
}
// Clear the window drafts
async function clearWindowDrafts(includeDirty: boolean) {
    await draftQueue.value;
    for (const [key, { draft, dirty }] of [...currentWindowDrafts()]) {
        if (!includeDirty && dirty) continue;
        // Do not delete an independently replaced recovery record
        // A resumed document may have reloaded the same recovery from disk
        const current = app().draftStore[key];
        if (current && bindingSignature(current) === bindingSignature(draft)) {
            await draftQueue.clear(key);
            if (app().draftStore[key] === current) delete app().draftStore[key];
        }
        await releaseMediaOwner(app(), 'control:' + key);
        forgetWindowDraft(key);
    }
}
windowCloseOwners.register({
    name: 'Binding recovery drafts edited in this window',
    // Return a snapshot of the current context state
    state: () =>
        [...currentWindowDrafts().values()].some(({ dirty }) => dirty) ? 'dirty' : 'clean',
    // Discard pending context edits without saving them
    discard: () => clearWindowDrafts(true),
    // Release resources retained by the context operation
    release: async () => {
        await clearWindowDrafts(false);
        await releaseAllWindowMedia(app());
    },
});
// Decode the device identity from a serialized draft key and tolerate malformed keys
export const draftDevice = (key: string): string | undefined => {
    try {
        return JSON.parse(key)[0];
    } catch {
        return undefined;
    }
};
// Consume a handled promise rejection without creating an unhandled rejection
export const swallow = (promise: Promise<unknown>) => {
    void promise.catch(() => {});
};
export const icons: Record<string, string> = {
    none: '+',
    open_url: '↗',
    mute: '◉',
    media: 'Ⅱ',
    volume: '◖',
    hotkey: '⌘',
    command: '›_',
    macro: '≋',
    brightness: '☼',
    sleep: '☾',
    plugin: '◇',
    page: '▤',
    swipe_navigation: '↔',
    profile: '▦',
    application: '▣',
    open_path: '▱',
    toggle: '◐',
    delay: '◷',
};
export type PendingDeletion =
    | {
          kind: 'profile' | 'page';
          profileId: string;
          pageId?: string;
          name: string;
          profileName: string;
          expectedRevision: number;
      }
    | { kind: 'simulator'; deviceId: string; name: string }
    | {
          kind: 'binding';
          deviceId: string;
          profileId: string;
          controlId: string;
          address: string;
          name: string;
          expectedRevision: number;
      }
    | {
          kind: 'background';
          deviceId: string;
          profileId: string;
          name: string;
          expectedRevision: number;
      };
