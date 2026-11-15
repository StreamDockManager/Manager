import { libraryKey } from './library-organization';
import type { CommandResult } from '../generated/commands';
import type { ManagerResourceReport } from '../generated/runtime';
import type { PreviewHandlers } from './preview-handlers';

// Report native resource metrics as unavailable in the browser simulator
const unavailableResources = (): ManagerResourceReport => ({
    process_count: 0,
    process_note: 'Browser preview has no native process metrics.',
    cpu_state: 'unavailable',
    gpu_state: 'unavailable',
    gpu_note: 'Browser preview has no native GPU metrics.',
    samples: [],
});

// Create the preview settings handlers
export function createPreviewSettingsHandlers() {
    let cacheStatus: import('../generated/runtime').CacheStatus = {
        folders: [
            {
                id: 'download-cache',
                label: 'Download cache',
                bytes: 104857600,
                entries: 4,
                max_bytes: 2147483648,
                max_entries: 32,
                max_age_seconds: 86400,
                protected_entries: 0,
            },
            {
                id: 'updates/downloads',
                label: 'Update downloads',
                bytes: 536870912,
                entries: 3,
                max_bytes: 4294967296,
                max_entries: 128,
                max_age_seconds: 172800,
                protected_entries: 0,
            },
            {
                id: 'updates/cache',
                label: 'Update metadata',
                bytes: 209715200,
                entries: 2,
                max_bytes: 1073741824,
                max_entries: 64,
                max_age_seconds: 86400,
                protected_entries: 0,
            },
        ],
        errors: [],
    };
    let desktopSettings: import('../generated/runtime').DesktopSettings = {
        notifications: false,
        launch_to_tray: false,
        plugin_links: false,
        openaction_links: false,
    };
    let applicationData: import('../generated/runtime').ApplicationData = {
        database_bytes: 49152,
        reclaimable_bytes: 8192,
        sqlite_version: 'Preview',
        backup_error: null,
        backups: [],
        usage: {},
        plugins: [
            {
                enabled: false,
                cache_bytes: 0,
                updated_ms: 1793505600000,
                removed_ms: 1793592000000,
                plugin_id: 'preview.removed',
                name: 'Previously installed plugin',
                installed: false,
                retained: true,
                record_count: 3,
                logical_bytes: 1200,
                generation: '1',
                review_token: 'preview-1',
            },
        ],
    };
    let loginEnabled = false;
    let parameterOverrides: CommandResult<'parameter_overrides'> = {
        installation: {},
        devices: {},
    };
    let organization: import('../generated/runtime').LibraryOrganization = {
        revision: 0,
        entries: [],
    };
    let recovery = {
        active: new URLSearchParams(typeof location === 'undefined' ? '' : location.search).has(
            'recovery',
        ),
        prompt:
            new URLSearchParams(typeof location === 'undefined' ? '' : location.search).get(
                'recovery',
            ) === 'interrupted',
        interrupted:
            new URLSearchParams(typeof location === 'undefined' ? '' : location.search).get(
                'recovery',
            ) === 'interrupted'
                ? [{ id: 'fixture.plugin', name: 'Interrupted example plugin' }]
                : [],
    };
    const preferencesHandlers = {
        // Simulate cache status in the browser without calling native services
        cache_status: () => structuredClone(cacheStatus),
        // Simulate clear cache in the browser without calling native services
        clear_cache: () => {
            const removed_entries = cacheStatus.folders.reduce(
                (sum, folder) => sum + folder.entries,
                0,
            );
            const removed_bytes = cacheStatus.folders.reduce(
                (sum, folder) => sum + folder.bytes,
                0,
            );
            cacheStatus = {
                folders: cacheStatus.folders.map((folder) => ({ ...folder, bytes: 0, entries: 0 })),
                errors: [],
            };
            return {
                status: structuredClone(cacheStatus),
                removed_entries,
                removed_bytes,
                errors: [],
            };
        },
        // Simulate parameter overrides in the browser without calling native services
        parameter_overrides: (args) => {
            return structuredClone(parameterOverrides);
        },
        // Simulate save parameter overrides in the browser without calling native services
        save_parameter_overrides: (args) => {
            parameterOverrides = structuredClone(args.value);
            return undefined;
        },
        // Simulate manager shortcut status in the browser without calling native services
        manager_shortcut_status: (args) => {
            return desktopSettings.show_manager_shortcut ? 'Ctrl+Alt+Shift+S' : '';
        },
        // Simulate set manager shortcut in the browser without calling native services
        set_manager_shortcut: (args) => {
            desktopSettings.show_manager_shortcut = Boolean(args?.enabled);
            return desktopSettings.show_manager_shortcut ? 'Ctrl+Alt+Shift+S' : '';
        },
        // Simulate manager catalog link in the browser without calling native services
        manager_catalog_link: (args) => {
            return null;
        },
        // Simulate acknowledge manager catalog link in the browser without calling native services
        acknowledge_manager_catalog_link: (args) => {
            return;
        },
        // Simulate plugin link status in the browser without calling native services
        plugin_link_status: () => ({
            enabled: false,
            supported: false,
            registered: false,
            reason: 'Link handling is available in the desktop app on supported platforms.',
        }),
        // Simulate set plugin link handling in the browser without calling native services
        set_plugin_link_handling: () => {
            throw Error('Link registration requires the native desktop app.');
        },
        // Simulate desktop settings in the browser without calling native services
        desktop_settings: (args) => {
            return structuredClone(desktopSettings);
        },
        // Simulate save desktop settings in the browser without calling native services
        save_desktop_settings: (args) => {
            desktopSettings = structuredClone(args.settings);
            return undefined;
        },
        // Simulate application data in the browser without calling native services
        application_data: (args) => {
            return structuredClone(applicationData);
        },
        // Simulate create recovery snapshot in the browser without calling native services
        create_recovery_snapshot: (args) => {
            applicationData.backups = [
                { name: 'preview.sqlite3', bytes: 49152, completed_ms: Date.now() },
            ];
            return structuredClone(applicationData);
        },
        // Simulate compact application data in the browser without calling native services
        compact_application_data: (args) => {
            applicationData.database_bytes -= applicationData.reclaimable_bytes;
            applicationData.reclaimable_bytes = 0;
            return structuredClone(applicationData);
        },
        // Simulate autostart status in the browser without calling native services
        autostart_status: (args) => {
            return { supported: true, enabled: loginEnabled };
        },
        // Simulate set autostart in the browser without calling native services
        set_autostart: (args) => {
            loginEnabled = Boolean(args.enabled);
            return undefined;
        },
        // Simulate library organization in the browser without calling native services
        library_organization: (args) => {
            return structuredClone(organization);
        },
        // Simulate save library mark in the browser without calling native services
        save_library_mark: (args) => {
            if (args.expectedRevision !== organization.revision)
                throw Error('Personal organization changed. Reload before saving.');
            const mark = structuredClone(args.mark);
            const seenTags = new Set<string>();
            mark.tags = [
                ...new Map(
                    mark.tags
                        .map((tag) => tag.trim())
                        // Keep entries that satisfy the preview settings handlers selection criteria
                        .filter((tag) => {
                            if (!tag || seenTags.has(tag.toLowerCase())) return false;
                            seenTags.add(tag.toLowerCase());
                            return true;
                        })
                        .map((tag) => [tag.toLowerCase(), tag]),
                ).values(),
            ].sort((a, b) => a.toLocaleLowerCase().localeCompare(b.toLocaleLowerCase()));
            if (
                mark.tags.length > 16 ||
                mark.tags.some(
                    (tag) =>
                        new TextEncoder().encode(tag).length > 80 || /[,\x00-\x1f\x7f]/.test(tag),
                )
            )
                throw Error('Use at most 16 tags of 80 bytes each');
            organization = {
                revision: organization.revision + 1,
                entries: organization.entries.filter(
                    (entry) => libraryKey(entry.identity) !== libraryKey(mark.identity),
                ),
            };
            if (mark.favorite || mark.tags.length) organization.entries.push(mark);
            return structuredClone(organization);
        },
        // Simulate resource alert settings in the browser without calling native services
        resource_alert_settings: (args) => {
            throw Error('Resource alert preferences require the native desktop application.');
        },
        // Simulate save resource alert settings in the browser without calling native services
        save_resource_alert_settings: (args) => {
            throw Error('Resource alert preferences require the native desktop application.');
        },
        // Simulate cancel hardware preview in the browser without calling native services
        cancel_hardware_preview: (args) => {
            return undefined;
        },
        // Simulate restore plugin data in the browser without calling native services
        restore_plugin_data: (args) => {
            const plugin = applicationData.plugins.find((plugin) => plugin.plugin_id === args.id);
            if (
                !plugin ||
                !plugin.installed ||
                !plugin.retained ||
                plugin.review_token !== args.reviewToken
            )
                throw Error('Stored data changed. Refresh and review again.');
            plugin.retained = false;
            plugin.review_token += '-restored';
            return structuredClone(applicationData);
        },
        // Simulate purge plugin data batch in the browser without calling native services
        purge_plugin_data_batch: (args) => {
            const reviews = args.reviews;
            if (
                !reviews.length ||
                reviews.length > 64 ||
                new Set(reviews.map((review) => review.id)).size !== reviews.length ||
                reviews.some(
                    (review) =>
                        !applicationData.plugins.some(
                            (plugin) =>
                                plugin.plugin_id === review.id &&
                                plugin.review_token === review.review_token,
                        ),
                )
            )
                throw Error('Stored data changed. Refresh and review again.');
            applicationData.plugins = applicationData.plugins.filter(
                (plugin) => !reviews.some((review) => review.id === plugin.plugin_id),
            );
            return structuredClone(applicationData);
        },
        // Simulate purge plugin data in the browser without calling native services
        purge_plugin_data: (args) => {
            const plugin = applicationData.plugins.find((plugin) => plugin.plugin_id === args.id);
            if (!plugin || plugin.review_token !== args.reviewToken)
                throw Error('Stored data changed. Refresh and review again.');
            applicationData.plugins = applicationData.plugins.filter(
                (plugin) => plugin.plugin_id !== args.id,
            );
            return structuredClone(applicationData);
        },
    } satisfies PreviewHandlers;
    const mediaHandlers = {
        // Simulate prepare export review in the browser without calling native services
        prepare_export_review: (args) => {
            throw Error('Immutable export review requires the native desktop application.');
        },
        // Simulate update export review in the browser without calling native services
        update_export_review: (args) => {
            throw Error('Immutable export review requires the native desktop application.');
        },
        // Simulate write export review in the browser without calling native services
        write_export_review: (args) => {
            throw Error('Immutable export review requires the native desktop application.');
        },
        // Simulate archive drag supported in the browser without calling native services
        archive_drag_supported: (args) => {
            return false;
        },
        // Simulate review installed package export in the browser without calling native services
        review_installed_package_export: (args) => {
            throw Error('Exporting installed plugins requires the native desktop application.');
        },
        // Simulate build installed package export in the browser without calling native services
        build_installed_package_export: (args) => {
            throw Error('Exporting installed plugins requires the native desktop application.');
        },
        // Simulate save installed package export in the browser without calling native services
        save_installed_package_export: (args) => {
            throw Error('Exporting installed plugins requires the native desktop application.');
        },
        // Simulate release archive export in the browser without calling native services
        release_archive_export: (args) => {
            return;
        },
        // Simulate discard export review in the browser without calling native services
        discard_export_review: (args) => {
            return;
        },
        // Simulate retain media in the browser without calling native services
        retain_media: (args) => {
            return crypto.randomUUID();
        },
        // Simulate release media in the browser without calling native services
        release_media: (args) => {
            return;
        },
        // Simulate finish media preparation in the browser without calling native services
        finish_media_preparation: (args) => {
            return;
        },
        // Simulate media library in the browser without calling native services
        media_library: (args) => {
            throw Error('The media library requires the native application.');
        },
        // Simulate media thumbnail in the browser without calling native services
        media_thumbnail: (args) => {
            throw Error('The media library requires the native application.');
        },
        // Simulate prepare media cleanup in the browser without calling native services
        prepare_media_cleanup: (args) => {
            throw Error('The media library requires the native application.');
        },
        // Simulate commit media cleanup in the browser without calling native services
        commit_media_cleanup: (args) => {
            throw Error('The media library requires the native application.');
        },
        // Simulate discard media cleanup in the browser without calling native services
        discard_media_cleanup: (args) => {
            return;
        },
    } satisfies PreviewHandlers;
    const diagnosticsHandlers = {
        // Return an empty performance report without inventing native process or plugin measurements
        performance: (args) => {
            return {
                manager: unavailableResources(),
                combined: unavailableResources(),
                reference_compatibility: null,
                runtimes: [],
                window_seconds: 300,
                metrics: [],
                cache: { bytes: 0, hits: 0, misses: 0 },
                plugins: [],
            };
        },
        // Describe browser simulation limits and the absence of verified physical hardware
        diagnostics: (args) => {
            return [
                {
                    category: 'Runtime',
                    value: 'Browser preview. Install the desktop app for HID, persistence, actions, and plugins.',
                    status: 'info',
                },
                {
                    category: 'Hardware qualification',
                    value: 'No physical hardware is verified.',
                    status: 'experimental',
                },
            ];
        },
        // Simulate recovery status in the browser without calling native services
        recovery_status: (args) => {
            return structuredClone(recovery);
        },
        // Simulate open manager logs in the browser without calling native services
        open_manager_logs: (args) => {
            throw Error('Manager logs are available in the desktop app.');
        },
        // Simulate exit recovery mode in the browser without calling native services
        exit_recovery_mode: (args) => {
            if (!recovery.active) throw Error('Manager is not in recovery mode');
            recovery = { active: false, prompt: false, interrupted: [] };
            return undefined;
        },
        // Simulate resolve interrupted startup in the browser without calling native services
        resolve_interrupted_startup: (args) => {
            if (args.normal && !recovery.prompt)
                throw Error('Quit and launch normally to leave recovery mode');
            recovery = { ...recovery, active: !args.normal, prompt: false };
            return structuredClone(recovery);
        },
    } satisfies PreviewHandlers;
    return {
        handlers: { ...preferencesHandlers, ...mediaHandlers, ...diagnosticsHandlers },
        // Read whether simulated recovery mode is active
        recoveryActive: () => recovery.active,
    };
}
