import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
const modules = new Map();
// Transpile and cache preview source modules with recursively rewritten local imports
function source(name) {
    if (modules.has(name)) return modules.get(name);
    const code = stripTypeScriptTypes(
        readFileSync(new URL(`../src/${name}.ts`, import.meta.url), 'utf8'),
    ).replace(/from '\.\/([\w-]+)'/g, (_, dependency) => `from '${source(dependency)}'`);
    const url = `data:text/javascript;base64,${Buffer.from(code).toString('base64')}`;
    modules.set(name, url);
    return url;
}
const { PreviewImageRegistry } = await import(source('surface-subscription'));
const { publishDecodedPreview } = await import(source('media-preview'));

// Verify repeated surface tokens share one URL and decode; retirement releases the registration
test('repeated surface tokens share one URL and decode; retirement releases the registration', async () => {
    const images = new PreviewImageRegistry();
    const decoded = new Set();
    let calls = 0;
    const old = globalThis.requestAnimationFrame;
    const queued = [];
    // Verify repeated surface tokens share one URL and decode; retirement releases the registration
    globalThis.requestAnimationFrame = (fn) => queued.push(fn);
    try {
        images.insert('same', new ArrayBuffer(16), 'image/png');
        const url = images.get('same');
        images.insert('same', new ArrayBuffer(16), 'image/png');
        assert.equal(images.size, 1);
        assert.equal(images.residentBytes, 16);
        assert.equal(images.get('same'), url);
        // Verify repeated surface tokens share one URL and decode; retirement releases the registration
        const publish = () =>
            publishDecodedPreview(
                [url, url],
                // Verify repeated surface tokens share one URL and decode; retirement releases the registration
                () => true,
                // Verify repeated surface tokens share one URL and decode; retirement releases the registration
                () => {},
                // Verify repeated surface tokens share one URL and decode; retirement releases the registration
                async () => calls++,
                // Verify repeated surface tokens share one URL and decode; retirement releases the registration
                async () => {},
                decoded,
            );
        await publish();
        await publish();
        assert.equal(calls, 1);
        images.retain(new Set(), decoded);
        assert.equal(images.size, 0);
        assert.equal(images.residentBytes, 0);
        assert.equal(decoded.size, 0);
        images.dispose();
        // Verify repeated surface tokens share one URL and decode; retirement releases the registration
        queued.forEach((fn) => fn()); // deferred release remains safe after disposal
    } finally {
        images.dispose();
        globalThis.requestAnimationFrame = old;
    }
});
// Verify registry rejects excess entries and bytes without losing its admitted images
test('registry rejects excess entries and bytes without losing its admitted images', () => {
    const images = new PreviewImageRegistry();
    try {
        for (let i = 0; i < 128; i++) images.insert(String(i), new ArrayBuffer(1), 'image/png');
        // Verify registry rejects excess entries and bytes without losing its admitted images
        assert.throws(() => images.insert('overflow', new ArrayBuffer(1), 'image/png'), /limits/);
        assert.equal(images.size, 128);
        images.dispose();
        assert.throws(
            // Verify registry rejects excess entries and bytes without losing its admitted images
            () => images.insert('oversize', new ArrayBuffer(32 * 1024 * 1024 + 1), 'image/png'),
            /limits/,
        );
        assert.equal(images.residentBytes, 0);
    } finally {
        images.dispose();
    }
});
