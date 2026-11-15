import type { Snapshot } from './types';
import type { PreviewHandlers } from './preview-handlers';
import type {
    RuntimeStatus,
    RuntimeSelection,
    RuntimeInstallation,
    RuntimeUsage,
} from '../generated/runtime';
// Create the runtime preview
export function createRuntimePreview(snapshot: () => Promise<Snapshot>) {
    const versions = ['24.16.0', '24.15.0', '24.14.1', '22.18.0', '22.17.0'];
    const status: RuntimeStatus = {
        lines: ['22', '24'].map((line) => ({
            kind: 'nodejs',
            channel: 'lts',
            line,
            start: '2025-01-01',
            end: '2099-01-01',
            policy: 'Browser preview fixture',
        })),
        releases: [],
        installations: [],
        defaults: {},
        plugins: {},
        automatic: true,
        last_check: null,
        next_check: 0,
        stale: true,
        error: null,
        jobs: [],
    };
    status.releases = versions.map((version) => ({
        id: `nodejs-lts-${version}-preview`,
        kind: 'nodejs',
        channel: 'lts',
        line: version.split('.')[0],
        version,
        platform: 'preview',
        url: `https://example.invalid/preview/node-${version}.zip`,
        archive_name: `node-${version}.zip`,
        executable: 'node',
        verification: 'Preview fixture only',
        provenance: 'Browser preview; no downloads',
    }));
    const removalFixture =
        typeof location === 'undefined'
            ? ''
            : new URLSearchParams(location.search).get('runtimeRemoval');
    const usage: RuntimeUsage[] = [];
    if (removalFixture) {
        const release = status.releases[0];
        const installation: RuntimeInstallation = {
            ...release,
            executable: `/preview/runtimes/${release.id}/node`,
            source: 'private',
        };
        status.installations.push(installation, {
            ...installation,
            id: 'external-preview',
            executable: '/usr/local/bin/node',
            source: 'external',
        });
        const selection: RuntimeSelection = {
            kind: 'nodejs',
            channel: 'lts',
            line: '24',
            installation_id: installation.id,
            external: null,
            acknowledge_override: false,
        };
        status.defaults.nodejs = selection;
        status.plugins['preview-clock'] = { ...selection };
        usage.push({
            plugin_id: 'preview-clock',
            name: 'Preview Clock',
            enabled_preference: true,
            running: removalFixture === 'in-use' ? installation : null,
            selected: {
                requirement: { kind: 'nodejs', channel: 'lts', version: null },
                recommended_line: '24',
                selected: installation,
                state: 'ready',
                message: '',
                overridden: false,
            },
            pending_restart: false,
        });
    }
    return {
        // Simulate runtime status in the browser without calling native services
        runtime_status: () => {
            return structuredClone(status);
        },
        // Simulate runtime usage in the browser without calling native services
        runtime_usage: () => {
            return structuredClone(usage);
        },
        // Simulate runtime check in the browser without calling native services
        runtime_check: () => {
            status.last_check = Math.floor(Date.now() / 1000);
            status.stale = false;
            status.error = null;
            return structuredClone(status);
        },
        // Simulate runtime automatic in the browser without calling native services
        runtime_automatic: ({ enabled }) => {
            status.automatic = enabled;
            return;
        },
        // Simulate runtime candidates in the browser without calling native services
        runtime_candidates: () => {
            return [];
        },
        // Simulate runtime select in the browser without calling native services
        runtime_select: ({ choice, pluginId }) => {
            if (pluginId) status.plugins[String(pluginId)] = structuredClone(choice);
            else status.defaults[choice.kind] = structuredClone(choice);
            return;
        },
        // Simulate runtime install in the browser without calling native services
        runtime_install: ({ releaseId }) => {
            const release = status.releases.find((r) => r.id === releaseId);
            if (!release) throw Error('Unknown release');
            const id = crypto.randomUUID();
            status.jobs = [
                {
                    id,
                    release_id: release.id,
                    state: 'installing',
                    downloaded: 1024,
                    total: 4096,
                    error: null,
                    kind: release.kind,
                    line: release.line,
                    version: release.version,
                },
            ];
            // Run the deferred runtime preview update after its scheduled delay
            setTimeout(() => {
                const job = status.jobs.find((j) => j.id === id);
                if (!job || job.state !== 'installing') return;
                const fixture = new URLSearchParams(location.search).get('runtimeJob');
                if (fixture === 'failure') {
                    job.state = 'failed';
                    job.error = 'Preview executable version mismatch. Retry the installation.';
                    return;
                }
                status.installations.push({
                    ...release,
                    executable: `/preview/runtimes/${release.id}/${release.executable}`,
                    source: 'private',
                } as RuntimeInstallation);
                job.state = 'complete';
                job.downloaded = 4096;
            }, 1200);
            return id;
        },
        // Simulate runtime remove in the browser without calling native services
        runtime_remove: ({ installationId }) => {
            const installation = status.installations.find((i) => i.id === installationId);
            if (!installation) throw Error('Runtime installation was not found.');
            if (installation.source !== 'private')
                throw Error('Only private runtimes can be deleted.');
            if (usage.some((u) => u.running?.executable === installation.executable))
                throw Error('Runtime is in use. Disable its plugins first.');
            if (removalFixture === 'error') throw Error('Preview runtime deletion failed.');
            status.installations = status.installations.filter((i) => i.id !== installation.id);
            for (const item of usage) {
                if (item.selected.selected?.id === installation.id) {
                    item.selected.selected = null;
                    item.selected.state = 'missing';
                    item.selected.message = 'Selected runtime is missing.';
                }
            }
            return;
        },
        // Simulate runtime cancel in the browser without calling native services
        runtime_cancel: ({ jobId }) => {
            const job = status.jobs.find((j) => j.id === jobId);
            if (job) job.state = 'cancelled';
            return;
        },
        // Simulate runtime apply in the browser without calling native services
        runtime_apply: () => {
            return snapshot();
        },
    } satisfies PreviewHandlers;
}
