import { generateModulePreloads } from 'alpine-component-loader/project';
import type { ACLManifest } from 'alpine-component-loader';
import { copyFile, mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { join, posix, resolve } from 'node:path';
import type { OutputBundle } from 'rolldown';
import type { Plugin, ResolvedConfig } from 'vite';
import { obsoleteAclRoutes } from './acl-routes.ts';

type ModuleGraph = { nodes: Record<string, { eager: string[] }> };
// Identify imports belonging to the ACL package or its subpaths
const aclSpecifier = (value: string) =>
    value === 'alpine-component-loader' || value.startsWith('alpine-component-loader/');
const importMap = /(<script\b[^>]*type=["']importmap["'][^>]*>)([\s\S]*?)(<\/script>)/g;
// Reject noncanonical or escaping asset paths before resolving bundle files
const localFile = (value: string) => {
    if (
        !value ||
        value.startsWith('/') ||
        value.includes('\\') ||
        posix.normalize(value) !== value ||
        value.startsWith('../')
    )
        throw Error(`Unsafe UI asset path: ${value}`);
    return value;
};
// Collect the transitive eager module dependencies of the selected entry points
export function eagerModules(graph: ModuleGraph, roots: string[]): Set<string> {
    const keep = new Set<string>();
    // Validate and visit each module once while traversing eager dependencies
    const visit = (path: string) => {
        localFile(path);
        if (keep.has(path)) return;
        const node = graph.nodes[path];
        if (!node || !Array.isArray(node.eager))
            throw Error(`Missing ACL module graph entry: ${path}`);
        keep.add(path);
        node.eager.forEach(visit);
    };
    roots.forEach(visit);
    return keep;
}
// Recursively enumerate regular bundle files and reject symbolic links
async function files(root: string): Promise<string[]> {
    const result: string[] = [];
    for (const entry of await readdir(root, { withFileTypes: true })) {
        const path = join(root, entry.name);
        if (entry.isSymbolicLink()) throw Error(`Unexpected symlink in UI bundle: ${path}`);
        if (entry.isDirectory()) result.push(...(await files(path)));
        else if (entry.isFile()) result.push(path);
    }
    return result;
}
// Remove empty descendant directories after pruning unused assets
async function pruneEmptyDirectories(root: string) {
    for (const entry of await readdir(root, { withFileTypes: true })) {
        if (!entry.isDirectory()) continue;
        const child = join(root, entry.name);
        await pruneEmptyDirectories(child);
        if (!(await readdir(child)).length) await rm(child, { recursive: true });
    }
}

// Operate only on copied output, after the upstream ACL plugin has finished
// Its public preload API infers optional modules from every first-party route;
// content features and imperative startup features supplement that selection
export async function pruneUiBundle(
    output: string,
    aclRoot: string,
    moduleDirectory: string,
    bundle: OutputBundle,
    base = '/',
) {
    const imports = new Set<string>();
    for (const chunk of Object.values(bundle)) {
        if (chunk.type === 'chunk')
            [...chunk.imports, ...chunk.dynamicImports]
                .filter(aclSpecifier)
                .forEach((name) => imports.add(name));
    }
    if (!imports.size) throw Error('No ACL entry points found in production chunks');
    const manifest = JSON.parse(
        await readFile(join(output, 'acl-manifest.json'), 'utf8'),
    ) as ACLManifest;
    const features: Parameters<typeof generateModulePreloads>[0]['features'] = [
        'manifest',
        'dynamic',
        'boundary',
    ];
    const components = manifest.components || {};
    for (const definition of Object.values(components)) {
        if (typeof definition !== 'object' || typeof definition.source !== 'string')
            throw Error('Unsupported first-party ACL component definition');
        const source = localFile(definition.source);
        const template = await readFile(join(output, source), 'utf8');
        if (/<style\b/i.test(template)) features.push('styles');
        if (/<slot\b/i.test(template)) features.push('slots');
    }
    const directory = join(output, moduleDirectory);
    const graph = JSON.parse(
        await readFile(join(aclRoot, 'dist/module-graph.json'), 'utf8'),
    ) as ModuleGraph;
    const hints = await generateModulePreloads({
        manifest,
        tags: Object.keys(components),
        features: [...new Set(features)],
        entryUrl: '/core.min.js',
    });
    const roots = hints.urls.map((url) =>
        localFile(
            new URL(url, 'https://acl.invalid').pathname.slice(1).replace(/\.min\.js$/, '.js'),
        ),
    );
    const htmlFiles = Object.values(bundle).filter(
        (entry) => entry.type === 'asset' && entry.fileName.endsWith('.html'),
    );
    const rewrites: { path: string; html: string }[] = [];
    for (const entry of htmlFiles) {
        const path = join(output, entry.fileName);
        const html = await readFile(path, 'utf8');
        rewrites.push({
            path,
            // Remove unused ACL import mappings and collect the retained module roots
            html: html.replace(importMap, (_, open: string, json: string, close: string) => {
                const map = JSON.parse(json) as { imports?: Record<string, string> };
                for (const [name, target] of Object.entries(map.imports || {})) {
                    if (!aclSpecifier(name)) continue;
                    if (!imports.has(name)) {
                        delete map.imports![name];
                        continue;
                    }
                    const prefix = `${base}${moduleDirectory}/`;
                    const pathname = new URL(target, 'https://acl.invalid').pathname;
                    if (!pathname.startsWith(prefix))
                        throw Error(`Unexpected ACL module URL: ${target}`);
                    const module = localFile(
                        pathname.slice(prefix.length).replace(/\.min\.js$/, '.js'),
                    );
                    roots.push(module);
                }
                for (const name of imports)
                    if (!map.imports?.[name]) throw Error(`Missing ACL import mapping: ${name}`);
                return open + JSON.stringify(map) + close;
            }),
        });
    }
    const keep = new Set(
        [...eagerModules(graph, roots)].map((path) => path.replace(/\.js$/, '.min.js')),
    );
    for (const path of keep)
        if (!(await stat(join(directory, path))).isFile())
            throw Error(`Missing selected ACL module: ${path}`);
    const obsoleteRoutes = await obsoleteAclRoutes(join(output, 'acl-routes'));
    // All selections and referenced files are validated before deleting output
    for (const path of await files(directory)) {
        const relative = posix.relative(
            directory.replaceAll('\\', '/'),
            path.replaceAll('\\', '/'),
        );
        if (!keep.has(relative)) await rm(path);
    }
    await copyFile(join(aclRoot, 'LICENSE'), join(directory, 'LICENSE'));
    for (const path of obsoleteRoutes) await rm(path, { force: true });
    for (const path of await files(join(output, 'components')))
        if (path.endsWith('.acl.json')) await rm(path);
    await rm(join(output, 'acl-manifest.json'));
    for (const { path, html } of rewrites) await writeFile(path, html);
    await pruneEmptyDirectories(directory);
}

// Verify emitted chunk dependencies and route component templates exist
export async function verifyUiBundle(output: string, bundle: OutputBundle, base = '/') {
    const references = new Set<string>();
    for (const entry of Object.values(bundle)) {
        if (entry.type === 'chunk') {
            references.add(entry.fileName);
            for (const path of [...entry.imports, ...entry.dynamicImports]) {
                if (!aclSpecifier(path)) references.add(path);
            }
            for (const path of entry.viteMetadata?.importedCss || []) references.add(path);
            for (const path of entry.viteMetadata?.importedAssets || []) references.add(path);
            for (const match of entry.code.matchAll(
                /new Worker\(new URL\(["'`]\/([^"'`]+)["'`]/g,
            )) {
                references.add(
                    match[1].startsWith(base.slice(1)) ? match[1].slice(base.length - 1) : match[1],
                );
            }
        } else if (entry.fileName.endsWith('.html')) {
            const html = await readFile(join(output, entry.fileName), 'utf8');
            for (const match of html.matchAll(/(?:src|href)=["']\/(assets\/[^"'?#]+)["']/g)) {
                references.add(match[1]);
            }
        }
    }
    for (const path of references) {
        if (!(await stat(join(output, localFile(path))).catch(() => null))?.isFile())
            throw Error(`Missing production UI dependency: ${path}`);
    }
    const routes = JSON.parse(await readFile(join(output, 'acl-routes/acl-routes.json'), 'utf8'));
    // Traverse the route index and verify each referenced component source exists
    const checkRoute = async (value: unknown): Promise<void> => {
        if (typeof value === 'string' && /acl-route-[^/]+\.json$/.test(value)) {
            const path = value.replace(/^\.?\//, '');
            const relative = path.startsWith('acl-routes/') ? path : 'acl-routes/' + path;
            const route = JSON.parse(await readFile(join(output, localFile(relative)), 'utf8'));
            for (const component of Object.values(route.components || {})) {
                if (
                    typeof component === 'object' &&
                    component &&
                    'source' in component &&
                    typeof component.source === 'string'
                ) {
                    if (!(await stat(join(output, localFile(component.source)))).isFile())
                        throw Error(`Missing production ACL component: ${component.source}`);
                }
            }
        } else if (value && typeof value === 'object') {
            await Promise.all(Object.values(value).map(checkRoute));
        }
    };
    await checkRoute(routes);
}

// Create Vite hooks to minify ACL references, prune assets and verify output
export function productionUiAssets(aclRoot: string, moduleDirectory: string): Plugin {
    let config: ResolvedConfig;
    return {
        name: 'stream-dock-manager-production-ui-assets',
        apply: 'build',
        // Capture the resolved output directory and base URL for subsequent hooks
        configResolved(resolved) {
            config = resolved;
        },
        transformIndexHtml: {
            order: 'post',
            // Rewrite ACL script URLs to the shipped minified module variants
            handler(html) {
                const base = `${config.base}${moduleDirectory}/`.replace(
                    /[.*+?^${}()|[\]\\]/g,
                    '\\$&',
                );
                return html.replace(
                    new RegExp(`${base}[^"'<>\\s?#]+\\.js(?=["'?#\\s<>])`, 'g'),
                    (url) => (url.endsWith('.min.js') ? url : url.replace(/\.js$/, '.min.js')),
                );
            },
        },
        writeBundle: {
            order: 'post',
            sequential: true,
            // Copy required license notices, prune copied assets and verify final dependencies
            async handler(_, bundle) {
                const notices = resolve(config.root, config.build.outDir, 'licenses');
                await mkdir(notices, { recursive: true });
                await copyFile(
                    resolve(config.root, 'node_modules/mediabunny/LICENSE'),
                    join(notices, 'mediabunny-MPL-2.0.txt'),
                );
                await pruneUiBundle(
                    resolve(config.root, config.build.outDir),
                    aclRoot,
                    moduleDirectory,
                    bundle,
                    config.base,
                );
                await verifyUiBundle(
                    resolve(config.root, config.build.outDir),
                    bundle,
                    config.base,
                );
            },
        },
    };
}
