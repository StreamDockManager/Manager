import type { Snapshot } from './types';
/** Read only: caller selects the missing-device policy; no session is created. */
export function previewBindingLookup(
    snapshot: Snapshot,
    device: Snapshot['devices'][number] | undefined,
    controlId: string,
) {
    const session = snapshot.sessions[device?.persistence_key || ''];
    const profile = snapshot.profiles.find(
        (p) => p.id === (session?.profile_id || snapshot.active_profile),
    );
    const page = profile?.pages.find((p) => p.id === (session?.page_id || profile.active_page));
    return { device, session, profile, page, binding: page?.bindings[controlId] };
}
