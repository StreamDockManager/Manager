import assert from 'node:assert/strict';
import { access, readFile } from 'node:fs/promises';
import { dirname, isAbsolute, resolve, win32 } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { runTauri } from './tauri.mjs';

const configDirectory = fileURLToPath(new URL('../src-tauri/', import.meta.url));

// Exercise real metadata generation while replacing only the native packaging subprocess
for (const platform of ['win32', 'darwin', 'linux']) {
    test(`metadata remains relative and alive through ${platform} bundling`, async () => {
        let staging;
        const result = await runTauri(['build'], {
            platform,
            runCLI: async (args) => {
                const override = JSON.parse(args[args.lastIndexOf('--config') + 1]);
                const entries = Object.entries(override.bundle.resources);
                assert.equal(entries.length, 1);
                const [source, destination] = entries[0];
                assert.equal(destination, 'notices/package-metadata.json');
                assert.equal(isAbsolute(source), false);
                assert.equal(win32.isAbsolute(source), false);
                assert.ok(!source.includes('\\'));
                const document = resolve(configDirectory, source);
                staging = dirname(document);
                assert.equal(dirname(staging), resolve(configDirectory));
                assert.equal(JSON.parse(await readFile(document, 'utf8')).product, 'desktop');
                // Relative resource paths retain the checkout drive even when system temp uses another
                assert.ok(
                    win32
                        .resolve('D:\\checkout\\src-tauri', source)
                        .startsWith('D:\\checkout\\src-tauri\\'),
                );
                return 0;
            },
        });
        assert.equal(result, 0);
        await assert.rejects(access(staging));
    });
}

// Distinct in-flight builds retain their own documents and clean up even after packaging errors
test('concurrent metadata staging is isolated and cleanup survives failure', async () => {
    const directories = [];
    let release;
    const ready = new Promise((resolve) => {
        release = resolve;
    });
    const build = () =>
        runTauri(['build'], {
            platform: 'linux',
            runCLI: async (args) => {
                const override = JSON.parse(args[args.lastIndexOf('--config') + 1]);
                const file = resolve(configDirectory, Object.keys(override.bundle.resources)[0]);
                directories.push(dirname(file));
                if (directories.length === 2) release();
                await ready;
                await access(file);
                throw new Error('simulated bundler failure');
            },
        });
    const results = await Promise.allSettled([build(), build()]);
    assert.ok(results.every((result) => result.status === 'rejected'));
    assert.equal(new Set(directories).size, 2);
    for (const directory of directories) await assert.rejects(access(directory));
});
