import assert from 'node:assert/strict';
import { readFile, mkdtemp, mkdir, writeFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import test from 'node:test';
import { publicPreviewBootstrap, preparePublicPreviewTemplates } from './public-preview-build.ts';
import { runInNewContext } from 'node:vm';
import { stripTypeScriptTypes } from 'node:module';

const source = await readFile(new URL('../src/public-preview.ts', import.meta.url), 'utf8');
// Evaluate the demo guard in an isolated VM with recorded requests and event listeners
function start(mode = 'public-preview', href = 'https://demo.invalid/preview/app/index.html') {
    const requests = [],
        policies = [],
        listeners = new Map();
    const window = {
        // Record the requested URL and options without performing network I/O
        fetch: async (...args) => {
            requests.push(args);
            return 'asset';
        },
    };
    const document = {
        // Provide a mutable element stub for the injected CSP meta tag
        createElement: () => ({}),
        // Record the CSP text appended to the simulated document head
        head: { append: (node) => policies.push(node.content) },
        // Capture demo navigation guards for later synthetic event dispatch
        addEventListener: (name, listener) => listeners.set(name, listener),
    };
    const js = stripTypeScriptTypes(
        source
            .replaceAll('import.meta.env.MODE', JSON.stringify(mode))
            .replaceAll('import.meta.env.BASE_URL', JSON.stringify('/preview/app/'))
            .replace('export const publicPreview', 'const publicPreview'),
    );
    runInNewContext(js, {
        window,
        document,
        location: { href },
        URL,
        Request,
        exports: {},
    });
    return { window, policies, listeners, requests };
}
// Verify demo has fresh memory-only storage and a path-scoped second CSP
test('demo has fresh memory-only storage and a path-scoped second CSP', () => {
    const first = start(),
        second = start();
    first.window.localStorage.setItem('secret', 'sample');
    assert.equal(first.window.localStorage.getItem('secret'), 'sample');
    assert.equal(second.window.localStorage.getItem('secret'), null);
    assert.equal(first.window.sessionStorage.getItem('secret'), null);
    assert.match(
        first.policies[0],
        /connect-src https:\/\/demo.invalid\/preview\/app\/ data: blob:/,
    );
    assert.doesNotMatch(first.policies[0], /'self'/);
});
// Verify only static bundle GET/HEAD and local media requests pass without credentials or redirects
test('only static bundle GET/HEAD and local media requests pass without credentials or redirects', async () => {
    const { window, requests } = start();
    await window.fetch('/preview/app/hardware.json', { credentials: 'include' });
    await window.fetch('data:image/svg+xml,sample');
    assert.equal(requests.length, 2);
    assert.equal(requests[0][1].credentials, 'omit');
    assert.equal(requests[0][1].redirect, 'error');
    for (const url of [
        '/auth/session',
        '/api/github/repos/a/b',
        '/preview/app/../api',
        'https://other.invalid/preview/app/x',
    ])
        await assert.rejects(window.fetch(url), /only loads bundled/);
    await assert.rejects(
        window.fetch('/preview/app/hardware.json', { method: 'POST' }),
        /only loads bundled/,
    );
    assert.equal(requests.length, 2);
});
// Dispatch a synthetic link click and report whether the demo guard blocks it
function navigationPrevented(listeners, href, attributes = {}, modifiers = {}) {
    let prevented = false;
    const values = { href, ...attributes };
    listeners.get('click')({
        target: {
            // Supply a synthetic anchor as the clicked element or its nearest link ancestor
            closest: () => ({
                tagName: 'A',
                // Read a synthetic anchor attribute using DOM-style missing-value semantics
                getAttribute: (name) => values[name] ?? null,
                // Report attribute presence independently of its string value
                hasAttribute: (name) => Object.hasOwn(values, name),
            }),
        },
        ...modifiers,
        // Suppress the browser default for an application-owned interaction
        preventDefault: () => {
            prevented = true;
        },
    });
    return prevented;
}
// Verify all sidebar and in-page fragments work on both index and clean-URL documents
test('all sidebar and in-page fragments work on both index and clean-URL documents', () => {
    for (const path of ['/preview/app/index.html', '/preview/app/']) {
        const current = 'https://demo.invalid' + path;
        const { listeners } = start('public-preview', current);
        for (const fragment of [
            '#/editor',
            '#/action-library',
            '#/macros',
            '#/plugins',
            '#/settings',
            '#/vault',
            '#/diagnostics',
            '#main-content',
            '#',
        ]) {
            assert.equal(navigationPrevented(listeners, fragment), false, current + fragment);
            if (fragment !== '#')
                assert.equal(navigationPrevented(listeners, current + fragment), false);
        }
        assert.equal(navigationPrevented(listeners, '#/editor', { target: '_self' }), false);
    }
});
// Verify external, cross-document, downloads, ping and new-window links remain blocked
test('external, cross-document, downloads, ping and new-window links remain blocked', () => {
    const { window, listeners } = start();
    assert.equal(window.open('https://example.com'), null);
    for (const href of [
        'https://evil.invalid/#/editor',
        '//evil.invalid/#/editor',
        '/auth/session#/editor',
        '/preview/app/#/editor',
        '?query=1#/editor',
        '',
        'index.html',
        'javascript:alert(1)',
        'data:text/html,test',
        'https://user:pass@demo.invalid/preview/app/index.html#/editor',
    ])
        assert.equal(navigationPrevented(listeners, href), true, href);
    for (const attributes of [
        { download: '' },
        { ping: '/auth/session' },
        { target: '_blank' },
        { target: '_parent' },
        { target: '_top' },
    ])
        assert.equal(navigationPrevented(listeners, '#/editor', attributes), true);
    for (const modifiers of [
        { ctrlKey: true },
        { metaKey: true },
        { shiftKey: true },
        { altKey: true },
        { button: 1 },
    ])
        assert.equal(navigationPrevented(listeners, '#/editor', {}, modifiers), true);
    let prevented = 0;
    listeners.get('click')({
        // Supply a file-input target to exercise the demo interaction guard
        target: { closest: () => ({ tagName: 'INPUT' }) },
        // Suppress the browser default for an application-owned interaction
        preventDefault: () => prevented++,
    });
    listeners.get('auxclick')({
        // Supply a file-input target to exercise the demo interaction guard
        target: { closest: () => ({ tagName: 'INPUT' }) },
        // Suppress the browser default for an application-owned interaction
        preventDefault: () => prevented++,
    });
    // Suppress the browser default for an application-owned interaction
    listeners.get('submit')({ preventDefault: () => prevented++ });
    assert.equal(prevented, 3);
});
// Verify native and developer modes do not replace storage or install demo restrictions
test('native and developer modes do not replace storage or install demo restrictions', () => {
    for (const mode of ['native', 'development']) {
        const { window, policies, listeners } = start(mode);
        assert.equal(window.localStorage, undefined);
        assert.equal(policies.length, 0);
        assert.equal(listeners.size, 0);
    }
});

const bridgeSource = await readFile(new URL('../src/bridge.ts', import.meta.url), 'utf8');
// Verify public build cannot select the native bridge even when Tauri detection is injected
test('public build cannot select the native bridge even when Tauri detection is injected', () => {
    const expression = bridgeSource.match(/export const native =([\s\S]*?);/)[1];
    for (const detected of [false, true]) {
        assert.equal(
            runInNewContext(expression.replaceAll('import.meta.env.MODE', '"public-preview"'), {
                // Simulate host detection independently of the forced public-preview mode
                isTauri: () => detected,
            }),
            false,
        );
    }
});

// Verify head bootstrap sets explicit static-path CSP before module loading for opaque WebKit origins
test('head bootstrap sets explicit static-path CSP before module loading for opaque WebKit origins', () => {
    const policies = [];
    runInNewContext(publicPreviewBootstrap('/preview/app/'), {
        URL,
        location: { href: 'https://demo.invalid/preview/app/index.html' },
        // Capture the injected meta element without a browser document
        document: { createElement: () => ({}), head: { append: (node) => policies.push(node) } },
    });
    assert.equal(policies.length, 1);
    assert.equal(policies[0].httpEquiv, 'Content-Security-Policy');
    assert.match(
        policies[0].content,
        /connect-src https:\/\/demo.invalid\/preview\/app\/ data: blob:/,
    );
    assert.match(policies[0].content, /default-src 'none'/);
    assert.doesNotMatch(policies[0].content, /'self'/);
});

// Verify public templates avoid clean-URL HTML redirects and preserve matching manifest revisions
test('public templates avoid clean-URL HTML redirects and preserve matching manifest revisions', async () => {
    const root = await mkdtemp(join(tmpdir(), 'manager-preview-'));
    try {
        await mkdir(join(root, 'components'));
        await mkdir(join(root, 'acl-routes'));
        const original = '<img src="/appearance/icons/monitor.svg">';
        await writeFile(join(root, 'components/shell.html'), original);
        const manifest = {
            components: {
                'sd-app-shell': {
                    source: 'components/shell.html',
                    options: { templateRevision: 'old', shadow: false },
                },
            },
        };
        await writeFile(join(root, 'acl-manifest.json'), JSON.stringify(manifest));
        await writeFile(
            join(root, 'acl-routes/acl-route-shell-abc.json'),
            JSON.stringify(manifest),
        );
        await writeFile(
            join(root, 'acl-routes/acl-routes.json'),
            JSON.stringify({
                routes: {
                    shell: { manifest: './acl-route-shell-abc.json', revision: 'old' },
                },
            }),
        );
        await preparePublicPreviewTemplates(root, '/preview/app/');
        await assert.rejects(access(join(root, 'components/shell.html')), { code: 'ENOENT' });
        const html = await readFile(join(root, 'components/shell.template.txt'), 'utf8');
        assert.equal(html, '<img src="/preview/app/appearance/icons/monitor.svg">');
        // Compute expected revisions directly from the emitted template and manifest bytes
        const digest = (bytes) =>
            'sha256-' + createHash('sha256').update(bytes).digest('base64url');
        for (const file of ['acl-manifest.json', 'acl-routes/acl-route-shell-abc.json']) {
            const value = JSON.parse(await readFile(join(root, file), 'utf8')).components[
                'sd-app-shell'
            ];
            assert.equal(value.source, 'components/shell.template.txt');
            assert.equal(value.options.templateRevision, digest(html));
            assert.equal(value.options.shadow, false);
        }
        const index = JSON.parse(await readFile(join(root, 'acl-routes/acl-routes.json'), 'utf8'));
        assert.equal(
            index.routes.shell.revision,
            digest(await readFile(join(root, 'acl-routes/acl-route-shell-abc.json'))),
        );
    } finally {
        await rm(root, { recursive: true, force: true });
    }
});
