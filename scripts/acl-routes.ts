import { execFileSync } from 'node:child_process';
import { lstat, readFile, readdir, rm } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const routeFile = /^acl-route-[a-z0-9._-]+-[a-f0-9]{8}\.json$/;

// Remove the route index and generated route manifests before regeneration
export async function clearAclRoutes(directory: string) {
    await rm(join(directory, 'acl-routes.json'), { force: true });
    const entries = await readdir(directory, { withFileTypes: true }).catch(
        (error: NodeJS.ErrnoException) => {
            if (error.code === 'ENOENT') return [];
            throw error;
        },
    );
    for (const entry of entries)
        if (entry.isFile() && routeFile.test(entry.name))
            await rm(join(directory, entry.name), { force: true });
}

// Validate indexed manifests and list generated route files no longer referenced
export async function obsoleteAclRoutes(directory: string): Promise<string[]> {
    const index = JSON.parse(await readFile(join(directory, 'acl-routes.json'), 'utf8'));
    if (
        index?.version !== 1 ||
        !index.routes ||
        typeof index.routes !== 'object' ||
        Array.isArray(index.routes) ||
        !Object.keys(index.routes).length
    )
        throw Error('Invalid ACL route index');

    const active = new Set<string>();
    for (const entry of Object.values(index.routes) as { manifest?: unknown }[]) {
        const manifest = entry?.manifest;
        if (
            typeof manifest !== 'string' ||
            !manifest.startsWith('./') ||
            !routeFile.test(manifest.slice(2))
        )
            throw Error('Invalid ACL route manifest filename');
        const name = manifest.slice(2);
        if (!(await lstat(join(directory, name))).isFile())
            throw Error(`Invalid ACL route manifest: ${name}`);
        active.add(name);
    }

    return (await readdir(directory, { withFileTypes: true }))
        .filter((entry) => entry.isFile() && routeFile.test(entry.name) && !active.has(entry.name))
        .map((entry) => join(directory, entry.name));
}

// Delete generated route manifests omitted from the validated route index
export async function pruneAclRoutes(directory: string) {
    for (const path of await obsoleteAclRoutes(directory)) await rm(path, { force: true });
}

if (import.meta.main) {
    const aclPackage = createRequire(import.meta.url).resolve(
        'alpine-component-loader/package.json',
    );
    const { bin } = JSON.parse(await readFile(aclPackage, 'utf8'));
    const config = fileURLToPath(new URL('../acl.config.mjs', import.meta.url));
    // ACL resolves CLI arguments from npm's original invocation directory
    execFileSync(
        process.execPath,
        [
            join(dirname(aclPackage), bin['alpine-component-loader']),
            'routes',
            '--config',
            config,
            '--force',
        ],
        { stdio: 'inherit' },
    );
    await pruneAclRoutes(fileURLToPath(new URL('../public/acl-routes/', import.meta.url)));
}
