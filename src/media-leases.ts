import type { AppState } from './state';
interface OwnedMedia {
    asset: string;
    active: boolean;
}
const scopes = new Map<string, Map<string, OwnedMedia>>();
const epochs = new Map<string, number>();
const pending = new Map<string, number>();
const pendingReplies = new Set<Promise<void>>();
const deleted = new Set<string>();

/** Includes failed retirements and late retain replies; zero means no frontend token remains. */
export function mediaWindowOwnership() {
    return {
        pending: [...pending.values()].reduce((total, count) => total + count, 0),
        leases: [...scopes.values()].reduce((total, entries) => total + entries.size, 0),
    };
}
// Remove an empty media-owner scope after pending work has settled
function prune(owner: string) {
    if (!pending.has(owner) && !scopes.has(owner)) epochs.delete(owner);
}
// Release a retained media handle without losing failed-retirement tracking
async function retire(app: AppState, owner: string, id: string, record: OwnedMedia) {
    record.active = false;
    try {
        await app.bridge.command('release_media', { id });
        const records = scopes.get(owner);
        records?.delete(id);
        if (!records?.size) scopes.delete(owner);
    } catch {
        /* Retain the token for a later retirement attempt; native ownership stays conservative. */
    }
}
// Retain a media handle for its owner while guarding against stale replies
export async function retainMediaFor(app: AppState, asset: string, owner: string) {
    if (deleted.has(asset)) throw Error('This media was deleted. Choose another asset.');
    const existing = [...(scopes.get(owner)?.entries() || [])].find(
        ([, r]) => r.active && r.asset === asset,
    );
    if (existing) return existing[0];
    const epoch = epochs.get(owner) || 0;
    let settled!: () => void;
    const reply = new Promise<void>((resolve) => (settled = resolve));
    pendingReplies.add(reply);
    pending.set(owner, (pending.get(owner) || 0) + 1);
    try {
        const id = await app.bridge.command('retain_media', { asset });
        const record = { asset, active: true };
        let records = scopes.get(owner);
        if (!records) scopes.set(owner, (records = new Map()));
        records.set(id, record);
        if (deleted.has(asset)) {
            await retire(app, owner, id, record);
            throw Error('This media was deleted. Choose another asset.');
        }
        if ((epochs.get(owner) || 0) !== epoch) {
            await retire(app, owner, id, record);
            throw Error('Media selection changed');
        }
        const duplicate = [...records.entries()].find(
            ([other, r]) => other !== id && r.active && r.asset === asset,
        );
        if (duplicate) {
            await retire(app, owner, id, record);
            return duplicate[0];
        }
        return id;
    } finally {
        settled();
        pendingReplies.delete(reply);
        const count = (pending.get(owner) || 1) - 1;
        if (count) pending.set(owner, count);
        else pending.delete(owner);
        prune(owner);
    }
}
/** Drain late selections and verify every main-document pin was acknowledged released. */
export async function releaseAllWindowMedia(app: AppState) {
    for (const owner of new Set([...scopes.keys(), ...pending.keys()]))
        await releaseMediaOwner(app, owner);
    while (pendingReplies.size) await Promise.all([...pendingReplies]);
    for (const owner of [...scopes.keys()]) await releaseMediaOwner(app, owner);
    if (scopes.size || pending.size)
        throw Error('Could not release all editor media. Reopen the editor and retry closing.');
}
// Retire handles owned by the closed media scope
export async function releaseMediaOwner(app: AppState, owner: string) {
    epochs.set(owner, (epochs.get(owner) || 0) + 1);
    await Promise.all(
        [...(scopes.get(owner)?.entries() || [])].map(([id, record]) =>
            retire(app, owner, id, record),
        ),
    );
    prune(owner);
}
// Mark a deleted asset and retire every retained handle that references it
export async function releaseDeletedMedia(app: AppState, asset: string) {
    deleted.add(asset);
    await Promise.all(
        [...scopes].flatMap(([owner, records]) =>
            [...records]
                .filter(([, record]) => record.asset === asset)
                .map(([id, record]) => retire(app, owner, id, record)),
        ),
    );
}
