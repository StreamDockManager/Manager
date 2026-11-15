import { resolvePreviewAssignment } from './preview-assignment-target';
import {
    ensureKeyboardNavigation,
    assertKeyboardNavigationProfile,
    assertKeyboardNavigationControl,
    isKeyboardNavigationPage,
} from './keyboard-navigation';
import {
    assertKeyboardMacroEdits,
    assertKeyboardMacroProfile,
    assertKeyboardMacroRecovery,
} from './keyboard-macro-validation';
import { previewBindingLookup } from './preview-binding-lookup';
import type { PreviewHandlers } from './preview-handlers';
import type { PreviewWorkspace } from './preview-workspace';
import type { CommandParameters } from '../generated/commands';
import type { Draft } from '../generated/runtime';
import type { PreviewHistory } from './preview-workspace';
import { defaultMedia } from './bridge';
import { artworkCrops } from './artwork-geometry';
import { compareHistoryFixture } from './history-compare';

// Create the preview workspace handlers
export function createPreviewWorkspaceHandlers(
    workspace: PreviewWorkspace,
    drafts: Record<string, Draft>,
    history: PreviewHistory,
    mediaDetails: NonNullable<PreviewHandlers['media_details']>,
) {
    const { s, guard, sessionFor, select, apply, commit, markEdit } = workspace;
    // Apply the edits
    const applyEdits = (args: NonNullable<CommandParameters<'apply_edits'>[0]>) => {
        guard(args.expectedRevision);
        assertKeyboardMacroEdits(args.edits);
        const candidate = structuredClone(s);
        try {
            // Resolve each actual assignment in order and roll back all inferred state on failure
            for (const edit of args.edits)
                apply([resolvePreviewAssignment(s, edit, args.deviceId)]);
        } catch (error) {
            Object.assign(s, candidate);
            throw error;
        }
        markEdit();
        return commit();
    };

    const workspaceHandlers = {
        // Simulate capture binding in the browser without calling native services
        capture_binding: (args) => {
            const device = s.devices.find((d) => d.id === args.deviceId) || s.devices[0];
            const { binding } = previewBindingLookup(s, device, String(args.controlId));
            if (!binding) throw Error('Binding missing');
            return structuredClone(binding);
        },
        // Simulate save profile in the browser without calling native services
        save_profile: (args) => {
            guard(args.expectedRevision);
            markEdit();
            const profile = structuredClone(args.profile);
            assertKeyboardMacroProfile(profile);
            assertKeyboardNavigationProfile(
                s.profiles.find((p) => p.id === profile.id),
                profile,
            );
            ensureKeyboardNavigation(profile);
            if (!profile.name.trim()) throw Error('Profile name is required');
            if (!s.profiles.length) s.active_profile = profile.id;
            const index = s.profiles.findIndex((p) => p.id === profile.id);
            if (index >= 0) s.profiles[index] = profile;
            else s.profiles.push(profile);
            return commit().snapshot;
        },
        // Simulate activate profile in the browser without calling native services
        activate_profile: (args) => {
            select(args.deviceId, args.id);
            return commit().snapshot;
        },
        // Simulate select page in the browser without calling native services
        select_page: (args) => {
            select(
                args.deviceId,
                s.sessions[s.devices.find((d) => d.id === args.deviceId)?.persistence_key || '']
                    ?.profile_id || s.active_profile,
                String(args.pageId),
                false,
            );
            return commit().snapshot;
        },
        // Simulate delete profile in the browser without calling native services
        delete_profile: (args) => {
            guard(args.expectedRevision, 'deleting');
            markEdit();
            const index = s.profiles.findIndex((p) => p.id === args.id);
            if (index < 0) throw Error('Unknown profile');
            const [removed] = s.profiles.splice(index, 1);
            if (s.active_profile === args.id)
                s.active_profile =
                    s.profiles.find((p) => p.model_id === removed.model_id)?.id ||
                    s.profiles[0]?.id ||
                    '';
            return commit().snapshot;
        },
        // Simulate slice artwork in the browser without calling native services
        slice_artwork: async (args) => {
            guard(args.expectedRevision);
            const request = args.request;
            const device = s.devices.find((d) => d.id === request.device_id && d.connected);
            const session = device && sessionFor(device);
            if (
                !device ||
                session?.profile_id !== request.profile_id ||
                session?.page_id !== request.page_id
            )
                throw Error('Device selection changed. Reopen Slice artwork.');
            const profile = s.profiles.find((p) => p.id === request.profile_id)!;
            const page = profile.pages.find((p) => p.id === request.page_id)!;
            const crops = artworkCrops(device.manifest, request);
            const metadata = await mediaDetails({ asset: request.asset });
            if (
                !metadata ||
                !['png', 'jpeg', 'jpg', 'gif', 'mp4'].includes(metadata.format) ||
                !device.manifest.capabilities.media_formats.includes(metadata.format)
            )
                throw Error('Unsupported artwork format');
            if (
                !Number.isFinite(request.media.speed) ||
                request.media.speed < 0.1 ||
                request.media.speed > 4 ||
                !Number.isInteger(request.media.trim_start_ms) ||
                request.media.trim_start_ms < 0 ||
                (request.media.trim_end_ms != null &&
                    (!Number.isInteger(request.media.trim_end_ms) ||
                        request.media.trim_end_ms <= request.media.trim_start_ms)) ||
                (metadata.duration_ms > 0 &&
                    (request.media.trim_start_ms >= metadata.duration_ms ||
                        (request.media.trim_end_ms ?? 0) > metadata.duration_ms))
            )
                throw Error('Invalid artwork playback settings');
            for (const [id, crop] of Object.entries(crops)) {
                const binding: import('./types').Binding = structuredClone(
                    page.bindings[id] || {
                        title: '',
                        color: '#00000000',
                        action: { kind: 'none' as const },
                        media: defaultMedia(),
                    },
                );
                binding.asset = request.asset;
                binding.media = { ...request.media, fit: 'fill', crop };
                assertKeyboardNavigationControl(profile, page.id, id, binding);
                page.bindings[id] = binding;
            }
            markEdit();
            return commit().snapshot;
        },
        // Simulate delete page in the browser without calling native services
        delete_page: (args) => {
            guard(args.expectedRevision, 'deleting');
            markEdit();
            const profile = s.profiles.find((p) => p.id === args.profileId);
            if (!profile) throw Error('Unknown profile');
            if (isKeyboardNavigationPage(profile, args.pageId))
                throw Error('The keyboard Navigation page cannot be removed.');
            const index = profile.pages.findIndex((p) => p.id === args.pageId);
            if (index < 0) throw Error('Unknown page');
            if (profile.pages.length <= 1) throw Error('Keep at least one page in each profile');
            profile.pages.splice(index, 1);
            if (profile.active_page === args.pageId)
                profile.active_page = profile.pages[Math.max(0, index - 1)].id;
            return commit().snapshot;
        },
        // Simulate apply workspace edits in the browser without calling native services
        apply_workspace_edits: (args) => applyEdits(args).batch,
        // Simulate apply edits in the browser without calling native services
        apply_edits: (args) => applyEdits(args).snapshot,
        // Simulate prepare profile conversion in the browser without calling native services
        prepare_profile_conversion: (args) => {
            throw Error('Profile conversion requires the native application.');
        },
        // Simulate update profile conversion in the browser without calling native services
        update_profile_conversion: (args) => {
            throw Error('Profile conversion requires the native application.');
        },
        // Simulate commit profile conversion in the browser without calling native services
        commit_profile_conversion: (args) => {
            throw Error('Profile conversion requires the native application.');
        },
        // Simulate discard profile conversion in the browser without calling native services
        discard_profile_conversion: (args) => {
            return;
        },
        // Simulate profile versions in the browser without calling native services
        profile_versions: (args) => {
            guard(args.expectedRevision);
            const current = s.profiles.find((p) => p.id === args.profileId);
            if (!current) throw Error('Profile not found');
            const seen = new Set([JSON.stringify(current)]);
            const versions = [];
            for (const item of [...s.history.undo, ...s.history.redo]
                .sort((a, b) => b.timestamp_ms - a.timestamp_ms)
                .slice(0, 80)) {
                const entry = history.get(item.id);
                if (!entry) continue;
                for (const side of ['before', 'after'] as const) {
                    const profile = entry[side].profiles.find((p) => p.id === args.profileId);
                    if (!profile || seen.has(JSON.stringify(profile))) continue;
                    seen.add(JSON.stringify(profile));
                    versions.push({
                        id: `${item.id}:${side}`,
                        label: `${side}: ${item.description}`,
                        source: 'history',
                        timestamp_ms: item.timestamp_ms,
                        valid: true,
                        issues: [],
                    });
                }
            }
            return {
                revision: String(args.expectedRevision),
                versions: versions.slice(0, 64),
                warnings: [
                    'Browser preview history only. Native Manager checks retained history integrity. Older backups remain in configuration recovery.',
                ],
            };
        },
        // Simulate profile version in the browser without calling native services
        profile_version: (args) => {
            guard(args.expectedRevision);
            const split = args.versionId.lastIndexOf(':');
            const id = args.versionId.slice(0, split),
                side = args.versionId.slice(split + 1);
            if (side !== 'before' && side !== 'after') throw Error('Invalid version');
            const profile = history.get(id)?.[side].profiles.find((p) => p.id === args.profileId);
            if (!profile) throw Error('Profile version unavailable');
            return {
                id: args.profileId,
                revision: String(args.expectedRevision),
                json: JSON.stringify(profile, null, 2),
                issues: [],
            };
        },
        // Simulate compare history in the browser without calling native services
        compare_history: (args) => {
            guard(args.expectedRevision);
            const id = String(args.id);
            const item = [...s.history.undo, ...s.history.redo].find((item) => item.id === id);
            const entry = history.get(id);
            if (!item || !entry) throw Error('History entry is no longer retained');
            return compareHistoryFixture(item, entry.before, entry.after);
        },
        // Simulate preview history control in the browser without calling native services
        preview_history_control: (args) => {
            guard(args.expectedRevision);
            return {
                before: {
                    image: null,
                    warning: 'Static previews require native Manager.',
                },
                after: {
                    image: null,
                    warning: 'Static previews require native Manager.',
                },
            };
        },
        // Simulate history action in the browser without calling native services
        history_action: (args) => {
            guard(args.expectedRevision);
            const operation = String(args.operation);
            if (operation === 'clear') {
                s.history = { undo: [], redo: [] };
                history.clear();
            } else {
                const from = operation === 'undo' ? s.history.undo : s.history.redo;
                const to = operation === 'undo' ? s.history.redo : s.history.undo;
                const item = from.pop();
                if (!item) throw Error('No history available');
                const entry = history.get(item.id)!;
                Object.assign(
                    s,
                    structuredClone(operation === 'undo' ? entry.before : entry.after),
                );
                to.push(item);
            }
            s.configuration_revision++;
            return commit().snapshot;
        },
        // Clone the browser preview's saved draft map without sharing mutable references
        drafts: (args) => {
            return structuredClone(drafts);
        },
        // Simulate save draft in the browser without calling native services
        save_draft: (args) => {
            if (args.draft) assertKeyboardMacroRecovery(args.draft);
            if (args.draft) drafts[String(args.key)] = structuredClone(args.draft);
            else delete drafts[String(args.key)];
            return undefined;
        },
        // Return a detached snapshot of preview workspace handlers state
        snapshot: (args) => {
            return structuredClone(s);
        },
    } satisfies PreviewHandlers;
    return workspaceHandlers;
}
