import {
    ensureKeyboardNavigation,
    assertKeyboardNavigationProfile,
    assertKeyboardNavigationControl,
    isKeyboardNavigationControl,
} from './keyboard-navigation';
import { actionChildren } from './builtin-actions';
import type { Action, Profile } from './types';
import { defaultDeviceOutput } from './bridge';
import type { Snapshot, Edit, RuntimeEvent, WorkspaceBatch } from '../generated/runtime';

// Clone only workspace configuration fields used by preview history
export const previewContent = (s: Snapshot) =>
    structuredClone({
        active_profile: s.active_profile,
        profiles: s.profiles,

        saved_macros: s.saved_macros,

        rules: s.rules,
        styles: s.styles,
    });

export type PreviewHistory = Map<
    string,
    { before: ReturnType<typeof previewContent>; after: ReturnType<typeof previewContent> }
>;
export type PreviewEvent = Omit<RuntimeEvent, 'sequence' | 'revision'>;
export type PreviewWorkspace = ReturnType<typeof createPreviewWorkspace>;

// All workspace mutations share revision, history, session-repair and notification ordering
export function createPreviewWorkspace(
    s: Snapshot,
    options: {
        description: string;
        history: PreviewHistory;
        publishWorkspace: (snapshot: Snapshot, sessionsOnly?: boolean) => WorkspaceBatch;
        notify: (event: RuntimeEvent) => void;
    },
) {
    s.profiles.forEach(ensureKeyboardNavigation);
    const before = previewContent(s);
    let edit = false;
    // Reject preview mutations whose expected configuration revision is stale
    const guard = (expectedRevision: number, verb = 'saving') => {
        if (expectedRevision !== s.configuration_revision)
            throw Error(`Configuration changed. Refresh before ${verb}.`);
    };
    // Lazily create the device session using a compatible active or fallback profile
    const sessionFor = (device: Snapshot['devices'][number] | undefined) =>
        (s.sessions[device?.persistence_key || ''] ||= {
            profile_id:
                s.profiles.find((p) => p.id === s.active_profile && p.model_id === device?.model_id)
                    ?.id ||
                s.profiles.find((p) => p.model_id === device?.model_id)?.id ||
                '',
            page_id:
                s.profiles.find((p) => p.id === s.active_profile && p.model_id === device?.model_id)
                    ?.active_page ||
                s.profiles.find((p) => p.model_id === device?.model_id)?.active_page ||
                '',

            remembered_pages: {},
            manual_profile: '',
            automation_enabled: false,
            automation_paused: false,
            output: defaultDeviceOutput(
                device?.manifest.capabilities.lighting_channels.length || 0,
            ),
        });
    // Select the preview workspace operation
    const select = (
        deviceId: string | null | undefined,
        profileId: string,
        pageId?: string,
        manual = true,
    ) => {
        const device = s.devices.find((d) => d.id === deviceId) || s.devices[0];
        const profile = s.profiles.find(
            (p) => p.id === profileId && (!device || p.model_id === device.model_id),
        );
        if (!profile) throw Error('Profile is incompatible with this device');
        const key = device?.persistence_key || '';
        const old = s.sessions[key];
        const selected = pageId || old?.remembered_pages[profileId] || profile.active_page;
        if (!profile.pages.some((p) => p.id === selected)) throw Error('Page missing');
        s.sessions[key] = {
            profile_id: profileId,
            page_id: selected,
            remembered_pages: { ...old?.remembered_pages, [profileId]: selected },
            manual_profile: manual ? profileId : old?.manual_profile || profileId,
            automation_enabled: old?.automation_enabled || false,
            automation_restore_on_exit: old?.automation_restore_on_exit || false,
            output:
                old?.output ||
                defaultDeviceOutput(device?.manifest.capabilities.lighting_channels.length || 0),
            automation_paused: manual
                ? old?.automation_enabled || false
                : old?.automation_paused || false,
        };
        s.active_profile = profileId;
    };
    // Apply the pending preview workspace changes
    const pruneLinks = (
        profile: Profile,
        keep: (member: import('../generated/runtime').LinkMember) => boolean,
    ) => {
        for (const [id, group] of Object.entries(profile.linked_controls || {})) {
            const members = group!.filter(keep);
            if (members.length < 2) delete profile.linked_controls![id];
            else profile.linked_controls![id] = members;
        }
    };
    const syncLink = (profile: Profile, page: string, control: string) => {
        const group = Object.values(profile.linked_controls || {}).find((g) =>
            g?.some((m) => m.page_id === page && m.control_id === control),
        );
        if (!group) return;
        const binding = profile.pages.find((p) => p.id === page)!.bindings[control];
        for (const m of group)
            profile.pages.find((p) => p.id === m.page_id)!.bindings[m.control_id] =
                structuredClone(binding);
    };
    const validateLinks = (profile: Profile) => {
        const used = new Set<string>();
        for (const [id, group] of Object.entries(profile.linked_controls || {})) {
            if (
                !/^[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12}$/i.test(id) ||
                !group ||
                group.length < 2 ||
                group.length > 256
            )
                throw Error('Invalid linked-control group.');
            let saved: string | undefined;
            const model = s.devices.find((d) => d.model_id === profile.model_id)?.manifest;
            const source = model?.controls.find((c) => c.id === group[0].control_id);
            for (const m of group) {
                const key = JSON.stringify([m.page_id, m.control_id]),
                    binding = profile.pages.find((p) => p.id === m.page_id)?.bindings[m.control_id];
                const control = model?.controls.find((c) => c.id === m.control_id);
                if (
                    used.has(key) ||
                    !binding ||
                    (model &&
                        (!source ||
                            !control ||
                            source.kind !== control.kind ||
                            JSON.stringify(source.semantics) !== JSON.stringify(control.semantics)))
                )
                    throw Error('Choose distinct compatible linked controls.');
                used.add(key);
                const value = JSON.stringify(binding);
                if (saved !== undefined && saved !== value)
                    throw Error(
                        'Linked assignments differ. Detach the member or apply the source assignment.',
                    );
                saved = value;
            }
        }
    };
    const apply = (edits: Edit[]) => {
        for (const change of edits) {
            switch (change.kind) {
                case 'save_profile': {
                    const i = s.profiles.findIndex((p) => p.id === change.profile.id);
                    assertKeyboardNavigationProfile(s.profiles[i], change.profile);
                    const profile = structuredClone(change.profile);
                    validateLinks(profile);
                    ensureKeyboardNavigation(profile);
                    if (!change.profile.name.trim()) throw Error('Profile name is required');
                    if (!s.profiles.length) s.active_profile = change.profile.id;
                    if (i < 0) s.profiles.push(profile);
                    else s.profiles[i] = profile;
                    break;
                }
                case 'link_control':
                case 'detach_control': {
                    const profile = s.profiles.find((p) => p.id === change.profile_id);
                    if (!profile) throw Error('Profile missing');
                    if (change.kind === 'detach_control') {
                        pruneLinks(
                            profile,
                            (m) =>
                                m.page_id !== change.member.page_id ||
                                m.control_id !== change.member.control_id,
                        );
                        break;
                    }
                    const { source, target } = change;
                    const groups = (profile.linked_controls ||= {});
                    const groupAt = (m: typeof source) =>
                        Object.values(groups).find((g) =>
                            g?.some(
                                (v) => v.page_id === m.page_id && v.control_id === m.control_id,
                            ),
                        );
                    if (
                        source.page_id === target.page_id ||
                        groupAt(target) ||
                        !profile.pages.some((p) => p.id === target.page_id) ||
                        !profile.pages.find((p) => p.id === source.page_id)?.bindings[
                            source.control_id
                        ]
                    )
                        throw Error(
                            'Choose a saved source and an independent destination on another page.',
                        );
                    if (
                        isKeyboardNavigationControl(profile, source.page_id, source.control_id) ||
                        isKeyboardNavigationControl(profile, target.page_id, target.control_id)
                    )
                        throw Error('Navigation controls are protected.');
                    const group = groupAt(source) || (groups[crypto.randomUUID()] = [source]);
                    group.push(target);
                    syncLink(profile, source.page_id, source.control_id);
                    break;
                }
                case 'assign_control': {
                    const source = s.profiles.find((profile) => profile.id === change.profile_id);
                    if (!source?.pages.some((page) => page.id === change.page_id))
                        throw Error('Page missing');
                    if (change.scope === 'page')
                        assertKeyboardNavigationControl(
                            source,
                            change.page_id,
                            change.control_id,
                            change.binding,
                        );
                    const model = s.devices.find(
                        (device) => device.model_id === source.model_id,
                    )?.manifest;
                    if (
                        model &&
                        !model.controls.some((control) => control.id === change.control_id)
                    )
                        throw Error('Control missing');
                    const profiles =
                        change.scope === 'model'
                            ? s.profiles.filter((profile) => profile.model_id === source.model_id)
                            : [source];
                    // Validate the references
                    const validateReferences = (action: Action, profile: Profile) => {
                        const pageId =
                            action.kind === 'page'
                                ? action.page_id
                                : action.kind === 'builtin' &&
                                    (action.action.kind === 'momentary_page' ||
                                        action.action.kind === 'binding_layer')
                                  ? action.action.page_id
                                  : null;
                        if (
                            pageId &&
                            !pageId.startsWith('${') &&
                            !profile.pages.some((page) => page.id === pageId)
                        )
                            throw Error(
                                'Page target does not exist in every destination. Use relative navigation or a narrower scope.',
                            );
                        if (
                            action.kind === 'profile' &&
                            !action.profile_id.startsWith('${') &&
                            !s.profiles.some(
                                (target) =>
                                    target.id === action.profile_id &&
                                    target.model_id === profile.model_id,
                            )
                        )
                            throw Error('Profile target is missing or belongs to another model.');
                        actionChildren(action).forEach((child) =>
                            validateReferences(child, profile),
                        );
                    };
                    profiles.forEach((profile) =>
                        validateReferences(change.binding.action, profile),
                    );
                    for (const profile of profiles)
                        for (const page of profile.pages) {
                            if (change.scope === 'page' && page.id !== change.page_id) continue;
                            if (
                                change.scope !== 'page' &&
                                isKeyboardNavigationControl(profile, page.id, change.control_id)
                            )
                                continue;
                            page.bindings[change.control_id] = structuredClone(change.binding);
                            syncLink(profile, page.id, change.control_id);
                        }
                    break;
                }
                case 'bindings': {
                    const page = s.profiles
                        .find((p) => p.id === change.profile_id)
                        ?.pages.find((p) => p.id === change.page_id);
                    if (!page) throw Error('Page missing');
                    for (const id of Object.keys(change.bindings))
                        assertKeyboardNavigationControl(
                            s.profiles.find((p) => p.id === change.profile_id),
                            change.page_id,
                            id,
                            change.bindings[id],
                        );
                    for (const [id, b] of Object.entries(change.bindings)) {
                        const profile = s.profiles.find((p) => p.id === change.profile_id)!;
                        if (b) {
                            page.bindings[id] = structuredClone(b);
                            syncLink(profile, page.id, id);
                        } else {
                            delete page.bindings[id];
                            pruneLinks(
                                profile,
                                (m) => m.page_id !== page.id || m.control_id !== id,
                            );
                        }
                    }
                    break;
                }
                case 'save_style': {
                    s.styles ||= [];
                    const index = s.styles.findIndex((v) => v.id === change.style.id);
                    if (index < 0) s.styles.push(structuredClone(change.style));
                    else s.styles[index] = structuredClone(change.style);
                    break;
                }
                case 'delete_style':
                    s.styles = (s.styles || []).filter((v) => v.id !== change.id);
                    break;
                case 'save_macro': {
                    s.saved_macros ||= [];
                    const index = s.saved_macros.findIndex((m) => m.id === change.value.id);
                    if (index < 0) s.saved_macros.push(structuredClone(change.value));
                    else s.saved_macros[index] = structuredClone(change.value);
                    break;
                }
                case 'delete_macro':
                    s.saved_macros = (s.saved_macros || []).filter((m) => m.id !== change.id);
                    break;
                case 'rules':
                    s.rules = structuredClone(change.rules);
                    break;
                default:
                    throw Error('Unsupported edit');
            }
        }
    };

    // Publish the preview workspace operation
    const publish = (events: PreviewEvent[] = [], notifyAfter = false, sessionsOnly = false) => {
        // Convert each entry into the representation required by preview workspace
        const notifications = events.map((event) => {
            const notification = { ...event, sequence: ++s.revision, revision: s.revision };
            if (!notifyAfter) options.notify(notification);
            return notification;
        });
        if (!notifications.length) s.revision++;
        const batch = options.publishWorkspace(s, sessionsOnly);
        if (notifyAfter) notifications.forEach(options.notify);
        return batch;
    };
    // Commit the reviewed preview workspace changes
    const commit = () => {
        s.profiles.forEach(ensureKeyboardNavigation);
        if (edit) {
            const item = {
                id: crypto.randomUUID(),
                description: options.description,
                timestamp_ms: Date.now(),
                bytes: 0,
            };
            options.history.set(item.id, { before, after: previewContent(s) });
            s.history.undo.push(item);
            for (const dropped of s.history.undo.splice(
                0,
                Math.max(0, s.history.undo.length - 100),
            ))
                options.history.delete(dropped.id);
            for (const dropped of s.history.redo) options.history.delete(dropped.id);
            s.history.redo = [];
            s.configuration_revision++;
        }
        for (const old of before.profiles) {
            const current = s.profiles.find((profile) => profile.id === old.id);
            if (!current) continue;
            for (const [index, removed] of old.pages.entries()) {
                if (current.pages.some((page) => page.id === removed.id)) continue;
                pruneLinks(current, (m) => m.page_id !== removed.id);
                const fallback =
                    old.pages
                        .slice(0, index)
                        .reverse()
                        .find((page) =>
                            current.pages.some((candidate) => candidate.id === page.id),
                        ) || current.pages[0];
                if (!fallback) continue;
                for (const session of Object.values(s.sessions)) {
                    if (!session) continue;
                    if (session.profile_id === old.id && session.page_id === removed.id)
                        session.page_id = fallback.id;
                    if (session.remembered_pages[old.id] === removed.id)
                        session.remembered_pages[old.id] = fallback.id;
                }
            }
        }
        for (const [key, session] of Object.entries(s.sessions)) {
            if (!session) continue;
            const device = s.devices.find((d) => d.persistence_key === key);
            const compatible = s.profiles.filter((p) => !device || p.model_id === device.model_id);
            const profile =
                compatible.find((p) => p.id === session.profile_id) ||
                compatible.find((p) => p.id === s.active_profile) ||
                compatible[0];
            if (!profile) {
                session.profile_id = '';
                session.page_id = '';
                session.manual_profile = '';
                continue;
            }
            session.profile_id = profile.id;
            if (!profile.pages.some((p) => p.id === session.page_id))
                session.page_id =
                    profile.pages.find((p) => p.id === session.remembered_pages[profile.id])?.id ||
                    profile.active_page;
            if (!compatible.some((p) => p.id === session.manual_profile))
                session.manual_profile = profile.id;
            s.sessions[key] = session;
        }
        const batch = publish();
        // Each command immediately selects its wire result. Copy only the requested representation.
        return {
            get snapshot() {
                return structuredClone(s);
            },
            get batch() {
                return structuredClone(batch);
            },
        };
    };

    return {
        s,
        before,
        guard,
        sessionFor,
        select,
        apply,
        commit,
        publish,
        // Mark the edit
        markEdit: () => {
            edit = true;
        },
    };
}
