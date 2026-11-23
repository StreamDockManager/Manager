import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { realpathSync } from 'node:fs';
import type { Plugin } from 'vite';

const sourceRevisionKey = 'STREAM_DOCK_MANAGER_SOURCE_REVISION';
const moduleId = 'virtual:manager-build-version';
const resolvedId = '\0' + moduleId;

// Read Git provenance from the selected checkout with a bounded command timeout
function git(root: string, args: string[]): string {
    return execFileSync('git', args, {
        cwd: root,
        encoding: 'utf8',
        stdio: ['ignore', 'pipe', 'ignore'],
        timeout: 5_000,
    }).trim();
}

// Combine the package version with verified source revision
export function buildVersion(
    version: string,
    root: string,
    env: NodeJS.ProcessEnv = process.env,
): string {
    // Packaged source exports retain their authoritative source revision
    let revision = env[sourceRevisionKey];
    if (revision === undefined) {
        try {
            if (realpathSync(git(root, ['rev-parse', '--show-toplevel'])) !== realpathSync(root)) {
                throw new Error('Not the manager checkout');
            }
            revision = git(root, ['rev-parse', '--verify', 'HEAD']);
        } catch {
            revision ??= 'unknown';
        }
    }
    const hash = /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/i.test(revision)
        ? revision.slice(0, 7).toLowerCase()
        : 'unknown';
    // Keep package SemVer and Git revision independent from mutable checkout state
    return `${version}${version.includes('+') ? '.' : '+'}${hash}`;
}

// Expose source provenance through a virtual Vite module
export function managerBuildVersion(version: string, root: string): Plugin {
    return {
        name: 'manager-build-version',
        // Resolve the public build-version specifier to its internal virtual module ID
        resolveId(id) {
            if (id === moduleId) return resolvedId;
        },
        // Watch checkout files and Git references so build provenance stays current
        buildStart() {
            if (process.env[sourceRevisionKey] !== undefined) return;
            try {
                // Git resolves these correctly for both ordinary and linked worktrees
                const paths = ['HEAD', 'packed-refs', 'index'];
                for (const file of git(root, [
                    'ls-files',
                    '-z',
                    '--cached',
                    '--others',
                    '--exclude-standard',
                ]).split('\0')) {
                    if (file) this.addWatchFile(resolve(root, file));
                }
                try {
                    const ref = git(root, ['symbolic-ref', '-q', 'HEAD']);
                    if (ref) paths.push(ref);
                } catch {
                    // Detached HEAD still needs a watcher on HEAD itself
                }
                for (const path of paths) {
                    this.addWatchFile(resolve(root, git(root, ['rev-parse', '--git-path', path])));
                }
            } catch {
                // Git is optional in exported source trees and detached builds
            }
        },
        // Force the cached virtual module to refresh its source provenance
        shouldTransformCachedModule({ id }) {
            return id === resolvedId;
        },
        // Generate the virtual module export from current version and Git provenance
        transform(_code, id) {
            if (id === resolvedId) {
                return `export default ${JSON.stringify(buildVersion(version, root))};`;
            }
        },
        // Provide a virtual module placeholder for the provenance transform hook
        load(id) {
            if (id === resolvedId) {
                return 'export default undefined;';
            }
        },
    };
}
