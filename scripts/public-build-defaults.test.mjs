import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { runTauri } from './tauri.mjs';

const root = new URL('../', import.meta.url);
const catalogKey = 'STREAM_DOCK_MANAGER_PLUGINS_CATALOG_URL';
const server = () => ({
    listen: async () => {},
    close: async () => {},
    printUrls: () => {},
    resolvedUrls: { local: ['http://127.0.0.1:1420/'] },
});

test('direct dev and signed cargo runner receive canonical public defaults without changing app arguments', async () => {
    for (const runner of [[], ['--runner', 'signed-runner']]) {
        const args = ['dev', ...runner, '--', '--config', '/isolated/test'];
        await runTauri(args, {
            env: { PATH: process.env.PATH },
            createDevServer: server,
            runCLI: async (actual, env) => {
                assert.equal(env[catalogKey], 'https://cdn.streamdockmanager.app/catalog.json');
                assert.equal(
                    env.STREAM_DOCK_MANAGER_RELEASE_REPOSITORY,
                    'StreamDockManager/Manager',
                );
                assert.deepEqual(actual.slice(-3), ['--', '--config', '/isolated/test']);
                return 0;
            },
        });
    }
});

test('alternate workspace, explicit override and empty override retain precedence; output excludes secrets', async () => {
    const dir = await mkdtemp(join(tmpdir(), 'public-defaults-'));
    try {
        const file = join(dir, 'workspace.toml');
        await writeFile(file, '[plugins]\ncatalog_url="https://example.invalid/catalog.json"\n');
        for (const value of [undefined, '', 'https://override.invalid/catalog.json']) {
            const env = { ...process.env, WORKSPACE_CONFIG: file, EXAMPLE_SECRET: 'must-not-emit' };
            delete env[catalogKey];
            if (value !== undefined) env[catalogKey] = value;
            const output = execFileSync('python3', ['scripts/public_build_defaults.py'], {
                cwd: root,
                env,
                encoding: 'utf8',
            });
            const data = JSON.parse(output);
            assert.equal(data[catalogKey], value ?? 'https://example.invalid/catalog.json');
            assert.equal(Object.keys(data).length, 3);
            assert.ok(!output.includes('must-not-emit'));
        }
        await writeFile(
            file,
            '[plugins]\ncatalog_url="https://username:secret@example.invalid/catalog.json"\n',
        );
        const env = { ...process.env, WORKSPACE_CONFIG: file };
        delete env[catalogKey];
        assert.throws(
            () =>
                execFileSync('python3', ['scripts/public_build_defaults.py'], {
                    cwd: root,
                    env,
                    stdio: 'pipe',
                }),
            (error) => {
                assert.ok(!error.stderr.toString().includes('secret'));
                return true;
            },
        );
    } finally {
        await rm(dir, { recursive: true, force: true });
    }
});
