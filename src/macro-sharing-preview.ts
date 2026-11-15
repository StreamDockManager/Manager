import type { PreviewWorkspace } from './preview-workspace';
import type { PreviewHandlers } from './preview-handlers';
// Browser-only, in-memory sharing fixture. No files or host actions are accessed
import type { MacroImportPreview, SavedMacro, Snapshot } from '../generated/runtime';
import { walkAction } from './builtin-actions';
// Create the in-memory macro-sharing handlers for the isolated browser preview
export function createMacroSharingPreview() {
    let stage: { preview: MacroImportPreview; macros: SavedMacro[] } | null = null;
    // Build an import preview with names that avoid existing workspace collisions
    const report = (snapshot: Snapshot): MacroImportPreview => {
        const used = new Set(snapshot.saved_macros.map((m) => m.name));
        return {
            id: stage!.preview.id,
            expected_revision: snapshot.configuration_revision,
            includes_plugin_settings: true,
            // Build the macro sharing preview entry from the current source item
            macros: stage!.macros.map((m) => {
                let name = m.name,
                    n = 1;
                while (used.has(name))
                    name = `${m.name} (imported${n++ === 1 ? '' : ' ' + (n - 1)})`;
                used.add(name);
                return { name: m.name, imported_name: name };
            }),
            warnings: [
                'Browser preview: a sample archive in memory. Real ZIP files are imported in the desktop application.',
            ],
        };
    };
    // Remap imported macro references to their newly allocated identifiers
    return (workspace: PreviewWorkspace) => {
        const { s: snapshot, markEdit, commit } = workspace;
        return {
            // Stage sample macros in memory for browser-only import review
            prepare_macro_import: (args) => {
                stage = {
                    preview: { id: crypto.randomUUID() } as MacroImportPreview,
                    macros: [
                        {
                            id: 'sample-root',
                            name: 'Sample imported macro',
                            action: {
                                kind: 'builtin',
                                action: { kind: 'saved_macro', macro_id: 'sample-child' },
                            },
                        },
                        {
                            id: 'sample-child',
                            name: 'Sample referenced macro',
                            action: {
                                kind: 'plugin',
                                plugin_id: 'preview.clock',
                                action_id: 'preview.clock.show',
                                settings: { token: 'preview-only-token' },
                            },
                        },
                    ],
                };
                stage.preview = report(snapshot);
                return structuredClone(stage.preview);
            },
            // Discard only the matching in-memory import session
            cancel_macro_import: (args) => {
                if (stage?.preview.id === args.id) stage = null;
                return undefined;
            },
            // Rebuild the import review against the current workspace revision
            recheck_macro_import: (args) => {
                if (!stage || stage.preview.id !== args.id)
                    throw Error('Macro import session expired');
                stage.preview = report(snapshot);
                return structuredClone(stage.preview);
            },
            // Commit the reviewed in-memory macros only if the workspace revision still matches
            commit_macro_import: (args) => {
                if (!stage || stage.preview.id !== args.id)
                    throw Error('Macro import session expired');
                if (
                    args.expectedRevision !== snapshot.configuration_revision ||
                    stage.preview.expected_revision !== snapshot.configuration_revision
                )
                    throw Error(
                        'Workspace changed. Recheck this import before committing; your selection has been retained.',
                    );
                const macros = structuredClone(stage.macros);
                const ids = new Map(macros.map((m) => [m.id, crypto.randomUUID()]));
                // Update each affected macro sharing preview entry in the current collection
                macros.forEach((m, i) => {
                    m.id = ids.get(m.id)!;
                    m.name = stage!.preview.macros[i].imported_name;
                    // Remap imported macro references to their newly allocated identifiers
                    walkAction(m.action, (action) => {
                        if (action.kind === 'builtin' && action.action.kind === 'saved_macro')
                            action.action.macro_id = ids.get(action.action.macro_id)!;
                        if (action.kind === 'plugin' && !args.includePluginSettings)
                            action.settings = null;
                    });
                });
                snapshot.saved_macros.push(...macros);
                stage = null;
                markEdit();
                return commit().snapshot;
            },
        } satisfies PreviewHandlers;
    };
}
