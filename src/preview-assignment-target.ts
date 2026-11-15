import { defaultMedia } from './bridge';
import { ensureKeyboardNavigation } from './keyboard-navigation';
import type { Edit, Snapshot, Profile } from '../generated/runtime';

// Resolve assignment targets only inside the preview handler's rollback-protected mutation scope
export function resolvePreviewAssignment(s: Snapshot, edit: Edit, deviceId?: string | null): Edit {
    if (
        edit.kind !== 'assign_control' &&
        (edit.kind !== 'bindings' || !Object.values(edit.bindings).some(Boolean))
    )
        return edit;
    const change = structuredClone(edit);
    if (change.kind !== 'bindings' && change.kind !== 'assign_control') return change;
    let device: Snapshot['devices'][number] | undefined;
    if (!change.profile_id || deviceId) {
        const connected = s.devices.filter((candidate) => candidate.connected);
        device = deviceId
            ? connected.find((candidate) => candidate.id === deviceId)
            : connected.length === 1
              ? connected[0]
              : undefined;
        if (!device) throw Error('Choose a connected device before assigning a binding');
    }
    const session = device && s.sessions[device.persistence_key];
    let profile = change.profile_id
        ? s.profiles.find((candidate) => candidate.id === change.profile_id)
        : s.profiles.find(
              (candidate) =>
                  candidate.id === session?.profile_id && candidate.model_id === device!.model_id,
          ) || s.profiles.find((candidate) => candidate.model_id === device!.model_id);
    if (!profile) {
        if (change.profile_id || change.page_id)
            throw Error('Assignment profile or page is missing');
        const page = crypto.randomUUID();
        profile = {
            id: crypto.randomUUID(),
            name: 'Default',
            model_id: device!.model_id,
            active_page: page,
            pages: [{ id: page, name: 'Main page', bindings: {} }],
            parameters: [],
            background_media: defaultMedia(),
        } satisfies Profile;
        ensureKeyboardNavigation(profile);
        if (!s.profiles.length) s.active_profile = profile.id;
        s.profiles.push(profile);
    }
    if (device && profile.model_id !== device.model_id)
        throw Error('Assignment profile belongs to a different device model');
    const page = change.page_id
        ? profile.pages.find((candidate) => candidate.id === change.page_id)
        : profile.pages.find(
              (candidate) => session?.profile_id === profile.id && candidate.id === session.page_id,
          ) || profile.pages.find((candidate) => candidate.id === profile.active_page);
    if (!page) throw Error('Assignment page is missing');
    if (device) {
        const controls =
            change.kind === 'assign_control' ? [change.control_id] : Object.keys(change.bindings);
        if (controls.some((id) => !device.manifest.controls.some((control) => control.id === id)))
            throw Error('Control missing');
    }
    change.profile_id = profile.id;
    change.page_id = page.id;
    return change;
}
