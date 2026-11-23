import type { CustomPalette } from '../generated/runtime';
import type { ThemeMode } from './theme-style';

export type PaletteCandidate = { mode: ThemeMode; id: string; custom?: CustomPalette };
export type PalettePreviewContext = {
    key: string;
    deviceId: string;
    revision: number;
    scope: string;
};

/** One ephemeral palette lease, independent of saved theme writes.
 * Serial transport plus generation checks prevent late starts from surviving cancel,
 * context navigation, or another owner's candidate. Only the latest queued draft runs.
 */
export function createPalettePreviewLease<Context extends { key: string }>(options: {
    context(): Context | null;
    start(context: Context, palette: PaletteCandidate): Promise<string>;
    cancel(token: string): Promise<unknown>;
    onError?(message: string): void;
}) {
    let lastError = '';
    let generation = 0;
    let ephemeral: { owner: symbol; id: string } | undefined;
    let queue = Promise.resolve();
    let token = '';
    let renewQueued = false;
    let timer: ReturnType<typeof setInterval> | undefined;
    let entry:
        | {
              owner: symbol;
              candidate: PaletteCandidate;
              context: Context;
              signature: string;
          }
        | undefined;
    const valid = (value: NonNullable<typeof entry>, epoch: number) =>
        epoch === generation && entry === value && options.context()?.key === value.context.key;
    const enqueue = (work: () => Promise<void>) => {
        queue = queue.then(work).catch((error) => {
            const message = String(error);
            console.error('Palette preview failed:', error);
            if (message !== lastError) options.onError?.(message);
            lastError = message;
        });
        return queue;
    };
    const cancel = async () => {
        const previous = token;
        token = '';
        if (previous) await options.cancel(previous);
    };
    const renew = () => {
        if (renewQueued || !entry) return;
        renewQueued = true;
        return enqueue(async () => {
            const value = entry;
            const epoch = generation;
            try {
                if (!value || !valid(value, epoch)) return;
                const next = await options.start(value.context, value.candidate);
                if (!valid(value, epoch)) {
                    await options.cancel(next);
                    return;
                }
                const previous = token;
                token = next;
                if (previous && previous !== next) await options.cancel(previous);
            } finally {
                renewQueued = false;
                if (entry && generation !== epoch) void renew();
            }
        });
    };
    const clear = (owner?: symbol) => {
        if (owner && entry?.owner !== owner) return queue;
        generation++;
        entry = undefined;
        if (timer) clearInterval(timer);
        timer = undefined;
        return enqueue(cancel);
    };
    return {
        set(owner: symbol, candidate: PaletteCandidate) {
            const context = options.context();
            if (!context) {
                void clear();
                return;
            }
            const signature = JSON.stringify(candidate);
            if (
                entry?.owner === owner &&
                entry.context.key === context.key &&
                entry.signature === signature
            )
                return;
            generation++;
            // Freeze mutable Alpine drafts before asynchronous transport. The editor uses
            // a CSS-only "preview" identifier and a blank ID for new palettes. Native
            // validation requires matching UUIDs; map only this detached transport copy.
            const payload: PaletteCandidate = JSON.parse(signature);
            if (payload.custom) {
                const savedId = payload.custom.id;
                if (!ephemeral || ephemeral.owner !== owner)
                    ephemeral = { owner, id: crypto.randomUUID() };
                const id = /^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(savedId)
                    ? savedId
                    : ephemeral.id;
                payload.id = id;
                payload.custom.id = id;
            }
            entry = { owner, context, candidate: payload, signature };
            void renew();
            if (!timer)
                timer = setInterval(() => {
                    if (entry && options.context()?.key === entry.context.key) void renew();
                    else void clear();
                }, 2000);
        },
        clear,
        contextChanged() {
            const context = options.context();
            if (entry && context?.key !== entry.context.key) return clear();
            return queue;
        },
    };
}
