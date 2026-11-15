import { createHash } from 'node:crypto';
import { readdir, readFile, writeFile, rename } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import type { Plugin, ResolvedConfig } from 'vite';

/** Runs synchronously before any module or asset. WebKit does not match 'self'
 * inside an opaque-origin sandbox, so use the document URL's explicit static root.
 * No response tokens, parameters, or parent-document state enter this policy. */
export function publicPreviewBootstrap(base: string): string {
    return `(() => {
        const root = new URL(${JSON.stringify(base)}, location.href).href;
        const policy = document.createElement('meta');
        policy.httpEquiv = 'Content-Security-Policy';
        policy.content = [
            "default-src 'none'",
            "script-src " + root + " 'unsafe-inline'",
            "style-src " + root + " 'unsafe-inline'",
            "img-src " + root + " data: blob:",
            "font-src " + root + " data:",
            "media-src " + root + " data: blob:",
            "connect-src " + root + " data: blob:",
            "worker-src " + root + " blob:",
            "frame-src 'none'", "object-src 'none'", "base-uri 'none'", "form-action 'none'"
        ].join('; ');
        document.head.append(policy);
    })();`;
}

/** ACL fetches must never rely on HTML-page canonical redirects. Static hosts
 * commonly redirect .html pages to clean URLs; templates are data, not pages.
 * Transform only copied output and keep native/generated source unchanged. */
export async function preparePublicPreviewTemplates(output: string, base: string): Promise<void> {
    const sources = new Map<string, { source: string; revision: string }>();
    // Hash template or manifest bytes into an ACL revision identifier
    const revision = (bytes: string) =>
        'sha256-' + createHash('sha256').update(bytes).digest('base64url');
    // Rename copied HTML templates and rewrite appearance URLs for static hosting
    async function rewrite(directory: string): Promise<void> {
        for (const entry of await readdir(directory, { withFileTypes: true })) {
            const path = join(directory, entry.name);
            if (entry.isDirectory()) await rewrite(path);
            else if (entry.name.endsWith('.html')) {
                const html = (await readFile(path, 'utf8')).replaceAll(
                    '/appearance/',
                    `${base}appearance/`,
                );
                const target = path.replace(/\.html$/, '.template.txt');
                await writeFile(path, html);
                await rename(path, target);
                sources.set(relative(output, path).replaceAll('\\', '/'), {
                    source: relative(output, target).replaceAll('\\', '/'),
                    revision: revision(html),
                });
            }
        }
    }
    await rewrite(join(output, 'components'));
    // Update copied component paths and revisions, then hash the rewritten manifest
    const rewriteManifest = async (path: string) => {
        const manifest = JSON.parse(await readFile(path, 'utf8'));
        for (const component of Object.values(manifest.components || {}) as {
            source?: string;
            options?: Record<string, unknown>;
        }[]) {
            const replacement = component.source && sources.get(component.source);
            if (replacement) {
                component.source = replacement.source;
                component.options = {
                    ...component.options,
                    templateRevision: replacement.revision,
                };
            }
        }
        const bytes = JSON.stringify(manifest, null, 2) + '\n';
        await writeFile(path, bytes);
        return revision(bytes);
    };
    await rewriteManifest(join(output, 'acl-manifest.json'));
    const routeDirectory = join(output, 'acl-routes');
    const indexPath = join(routeDirectory, 'acl-routes.json');
    const index = JSON.parse(await readFile(indexPath, 'utf8'));
    for (const route of Object.values(index.routes) as { manifest: string; revision: string }[]) {
        if (!/^\.\/acl-route-[a-z0-9-]+\.json$/.test(route.manifest))
            throw Error('Unexpected public preview route manifest path');
        route.revision = await rewriteManifest(join(routeDirectory, route.manifest));
    }
    await writeFile(indexPath, JSON.stringify(index, null, 2) + '\n');
}

/** Rewrite copied component images without changing the native component sources. */
export function publicPreviewBundle(mode: string): Plugin {
    let config: ResolvedConfig;
    return {
        name: 'public-preview-isolation',
        apply: 'build',
        // Capture the output directory and base URL used by preview-only build hooks
        configResolved(value) {
            config = value;
        },
        transformIndexHtml: {
            order: 'post',
            // Inject the path-scoped CSP bootstrap before public preview modules load
            handler(html) {
                if (mode !== 'public-preview') return html;
                return html.replace(
                    '<head>',
                    `<head>\n<script>${publicPreviewBootstrap(config.base)}</script>\n<meta name="referrer" content="no-referrer">`,
                );
            },
        },
        writeBundle: {
            order: 'post',
            sequential: true,
            // Rewrite copied templates only for the isolated public preview build
            async handler() {
                if (mode !== 'public-preview') return;
                await preparePublicPreviewTemplates(
                    resolve(config.root, config.build.outDir),
                    config.base,
                );
            },
        },
    };
}
