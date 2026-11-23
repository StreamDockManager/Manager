import test from 'node:test';
import { EventEmitter } from 'node:events';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, access, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
    signingCommandFailure,
    signedDev,
    signedDevPlan,
    runSignedBinary,
    runChild,
    DEV_ID,
    APP_NAME,
} from './dev-signed.mjs';
const identity = 'A'.repeat(40);
const opts = { platform: 'darwin', arch: 'arm64', env: {} };
test('explicit opt-in keeps true dev, separate identity and exact data argument', () => {
    const p = signedDevPlan(
        ['--identity', identity, '--data-dir', '/tmp/isolated data', '--offline', '--no-watch'],
        { ...opts, env: { APPLE_ID: 'never', STREAM_DOCK_MANAGER_TEST_ENV_PASSTHROUGH: '1' } },
    );
    assert.equal(p.args[0], 'dev');
    assert.ok(p.args.includes('--no-watch'));
    assert.equal(p.nativeWatch, false);
    assert.ok(p.args.includes('--offline'));
    assert.deepEqual(p.args.slice(-3), ['--', '--config', '/tmp/isolated data']);
    const config = JSON.parse(p.args[3]);
    assert.equal(config.identifier, DEV_ID);
    assert.deepEqual(config.plugins['deep-link'].desktop.schemes, []);
    assert.equal(p.env.APPLE_ID, undefined);
    assert.equal(p.env.STREAM_DOCK_MANAGER_TEST_ENV_PASSTHROUGH, '1');
    assert.match(p.env.CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER, /--cargo-runner$/);
    assert.equal(
        p.env.CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER,
        p.env.CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER,
    );
});
test('rejects invalid signing identities, relative data, release/options and target overrides', () => {
    for (const args of [
        ['--identity'],
        ['--identity', '-'],
        ['--identity', identity, '--data-dir', 'relative'],
        ['--identity', identity, '--data-dir', '/tmp/dev', '--release'],
    ])
        assert.throws(() => signedDevPlan(args, opts));
    assert.throws(() =>
        signedDevPlan(['--identity', identity, '--data-dir', '/tmp/dev'], {
            ...opts,
            env: { CARGO_BUILD_TARGET: 'anything' },
        }),
    );
});
test('dry run performs no signing/preparation/launch and prints no input credentials', async () => {
    const result = await signedDev(
        ['--identity', identity, '--data-dir', '/tmp/private-data', '--dry-run'],
        { ...opts, prepare: () => assert.fail(), run: () => assert.fail() },
    );
    const text = JSON.stringify(result);
    assert.ok(!text.includes(identity));
    assert.ok(!text.includes('/tmp/private-data'));
    assert.equal(result.dryRun, true);
});
async function fixture() {
    const root = await realpath(await mkdtemp(join(tmpdir(), 'signed-dev-test-')));
    await mkdir(join(root, 'target/debug'), { recursive: true });
    await mkdir(join(root, 'src-tauri'));
    const binary = join(root, 'target/debug/stream-dock-manager');
    await writeFile(binary, 'mock-binary-not-executable');
    await writeFile(join(root, 'src-tauri/tauri.conf.json'), '{"version":"0.1.1"}');
    return {
        root,
        binary,
        env: {
            STREAM_DOCK_MANAGER_LOCAL_SIGNING_IDENTITY: identity,
            STREAM_DOCK_MANAGER_DEV_RESTART_FILE: '/tmp/restart-fixture',
            STREAM_DOCK_MANAGER_TEST_ENV_PASSTHROUGH: '1',
        },
    };
}
test('mock signs/validates staged bundle before fixed-path launch and holds lock', async () => {
    const f = await fixture();
    const calls = [];
    try {
        const code = await runSignedBinary([f.binary, '--config', '/tmp/disposable'], {
            ...f,
            platform: 'darwin',
            execute: (command, args) => {
                calls.push([command, args]);
                return command.endsWith('plutil') && args.includes('json')
                    ? '{"NSLocationUsageDescription":"fixture","CFBundleURLTypes":[{}]}'
                    : '';
            },
            launch: async (binary, args, env) => {
                assert.equal(
                    binary,
                    join(
                        f.root,
                        'artifacts/local-dev-signing',
                        APP_NAME,
                        'Contents/MacOS/stream-dock-manager',
                    ),
                );
                assert.deepEqual(args, ['--config', '/tmp/disposable']);
                assert.equal(env.STREAM_DOCK_MANAGER_LOCAL_SIGNING_IDENTITY, undefined);
                assert.equal(env.STREAM_DOCK_MANAGER_TEST_ENV_PASSTHROUGH, '1');
                assert.equal(calls.filter(([p]) => p.endsWith('codesign')).length, 2);
                const plist = JSON.parse(
                    await readFile(
                        join(
                            f.root,
                            'artifacts/local-dev-signing',
                            APP_NAME,
                            'Contents/Info.plist',
                        ),
                    ),
                );
                assert.equal(plist.CFBundleIdentifier, DEV_ID);
                assert.equal(plist.CFBundleURLTypes, undefined);
                assert.equal(plist.NSLocationUsageDescription, 'fixture');
                await assert.rejects(
                    runSignedBinary([f.binary, '--config', '/tmp/disposable'], {
                        ...f,
                        platform: 'darwin',
                        execute: () => assert.fail(),
                        launch: () => assert.fail(),
                    }),
                    /locked/,
                );
                return 143;
            },
        });
        assert.equal(code, 143);
        await assert.rejects(access(join(f.root, 'artifacts/local-dev-signing/running.lock')));
    } finally {
        await rm(f.root, { recursive: true, force: true });
    }
});
test('failed signature verification never publishes or launches bundle', async () => {
    const f = await fixture();
    try {
        await assert.rejects(
            runSignedBinary([f.binary, '--config', '/tmp/disposable'], {
                ...f,
                platform: 'darwin',
                execute: (cmd, args) => {
                    if (args.includes('--verify')) throw new Error('mock verification failed');
                    return cmd.endsWith('plutil') && args.includes('json') ? '{}' : '';
                },
                launch: () => assert.fail(),
            }),
            /verification/,
        );
        await assert.rejects(access(join(f.root, 'artifacts/local-dev-signing', APP_NAME)));
        await assert.rejects(access(join(f.root, 'artifacts/local-dev-signing/running.lock')));
    } finally {
        await rm(f.root, { recursive: true, force: true });
    }
});
test('ordinary macOS build retains ad hoc policy and strips inherited Apple credentials', async () => {
    const { runTauri } = await import('./tauri.mjs');
    await runTauri(['build'], {
        platform: 'darwin',
        env: {
            ...process.env,
            APPLE_ID: 'fixture-account',
            APPLE_PASSWORD: 'fixture-secret',
            APPLE_SIGNING_IDENTITY: identity,
        },
        runCLI: async (args, env) => {
            assert.equal(env.APPLE_SIGNING_IDENTITY, '-');
            assert.equal(env.APPLE_ID, undefined);
            assert.equal(env.APPLE_PASSWORD, undefined);
            const override = JSON.parse(args[args.lastIndexOf('--config') + 1]);
            assert.equal(override.bundle.macOS.signingIdentity, '-');
            return 0;
        },
    });
});

test('lost Cargo parent terminates only the owned mock child and awaits exit', async () => {
    const child = new EventEmitter();
    const signals = [];
    let reads = 0;
    child.kill = (signal) => {
        signals.push(signal);
        queueMicrotask(() => child.emit('close', null));
    };
    const code = await runChild(
        'mock-only',
        [],
        {},
        { spawnProcess: () => child, parentPid: () => (++reads === 1 ? 10 : 1), intervalMs: 1 },
    );
    assert.equal(code, 143);
    assert.deepEqual(signals, ['SIGTERM']);
});

test('local fingerprint file supports dry-run, rejects missing/invalid, and explicit identity wins', async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), 'signed-dev-identity-')));
    const options = { ...opts, root, prepare: () => assert.fail(), run: () => assert.fail() };
    const args = ['--data-dir', join(root, 'disposable'), '--dry-run'];
    const file = join(root, '.dev-signing-identity');
    try {
        assert.throws(() => signedDevPlan(args, options), /Create \.dev-signing-identity/);
        await writeFile(file, identity + '\n');
        assert.equal(
            signedDevPlan(args, options).env.STREAM_DOCK_MANAGER_LOCAL_SIGNING_IDENTITY,
            identity,
        );
        const result = await signedDev(args, options);
        assert.equal(result.dryRun, true);
        assert.ok(!JSON.stringify(result).includes(identity));
        const invalid = 'PRIVATE INVALID CONTENT';
        await writeFile(file, invalid);
        assert.throws(
            () => signedDevPlan(args, options),
            (error) => {
                assert.match(error.message, /Invalid \.dev-signing-identity/);
                assert.ok(!error.message.includes(invalid));
                return true;
            },
        );
        const explicit = 'B'.repeat(40);
        assert.equal(
            signedDevPlan(['--identity', explicit, ...args], options).env
                .STREAM_DOCK_MANAGER_LOCAL_SIGNING_IDENTITY,
            explicit,
        );
        await rm(file);
        assert.equal(
            signedDevPlan(['--identity', explicit, ...args], options).env
                .STREAM_DOCK_MANAGER_LOCAL_SIGNING_IDENTITY,
            explicit,
        );
        await writeFile(file, identity);
        assert.throws(() => signedDevPlan(['--identity', '-', ...args], options), /40-hex/);
    } finally {
        await rm(root, { recursive: true, force: true });
    }
});

test('phase diagnostics classify macOS failures without leaking identities or paths', () => {
    for (const [stderr, expected] of [
        ['no identity found', 'identity was not found'],
        ['User interaction is not allowed', 'requires interaction'],
        ['errSecInternalComponent', 'errSecInternalComponent'],
        ['unable to build chain to self-signed root', 'trust chain'],
        ['resource fork, Finder information, or similar detritus not allowed', 'extended metadata'],
        ['a sealed resource is missing or invalid', 'verification rejected'],
        ['bundle format unrecognized, invalid, or unsuitable', 'metadata or format'],
        ['unknown private information', 'no recognized safe diagnostic'],
    ]) {
        const error = signingCommandFailure('certificate signing', {
            status: 1,
            stderr: `${stderr} /Users/private-user/private.app ${identity}`,
            message: 'PRIVATE CERT NAME',
        });
        assert.match(error.message, /certificate signing failed \(exit 1\)/);
        assert.ok(error.message.includes(expected));
        for (const privateValue of [
            '/Users/',
            identity,
            'PRIVATE CERT NAME',
            'unknown private information',
        ])
            assert.ok(!error.message.includes(privateValue));
    }
});

test('every native command failure identifies its phase and never launches', async () => {
    const phases = [
        'source plist conversion',
        'bundle plist conversion',
        'certificate signing',
        'signature verification',
    ];
    for (let failAt = 0; failAt < phases.length; failAt++) {
        const f = await fixture();
        let index = 0;
        try {
            await assert.rejects(
                runSignedBinary([f.binary, '--config', '/tmp/disposable'], {
                    ...f,
                    platform: 'darwin',
                    execute: (program, args) => {
                        if (index++ === failAt)
                            throw Object.assign(new Error('PRIVATE'), {
                                status: 1,
                                stderr: 'User interaction is not allowed',
                            });
                        return args.includes('json') ? '{}' : '';
                    },
                    launch: () => assert.fail('must not launch'),
                }),
                (error) =>
                    error.message.includes(phases[failAt]) &&
                    error.message.includes('requires interaction') &&
                    !error.message.includes('PRIVATE'),
            );
            await assert.rejects(access(join(f.root, 'artifacts/local-dev-signing', APP_NAME)));
            await assert.rejects(access(join(f.root, 'artifacts/local-dev-signing/running.lock')));
        } finally {
            await rm(f.root, { recursive: true, force: true });
        }
    }
});

test('normal and certificate development plans differ only in signing selection', () => {
    for (const args of [
        [],
        ['--offline', '--config', '/tmp/shared fixture'],
        ['--', '--', '--config', '/tmp/shared fixture'],
    ]) {
        const unsigned = signedDevPlan(args, { ...opts, signed: false });
        const signed = signedDevPlan(['--identity', identity, ...args], opts);
        assert.deepEqual(unsigned.args, signed.args);
        assert.equal(unsigned.nativeWatch, true);
        assert.equal(signed.nativeWatch, true);
        assert.equal(unsigned.app, signed.app);
        const stripSigning = (env) =>
            Object.fromEntries(
                Object.entries(env).filter(
                    ([key]) => !key.includes('RUNNER') && !key.includes('SIGNING_IDENTITY'),
                ),
            );
        assert.deepEqual(stripSigning(unsigned.env), stripSigning(signed.env));
        assert.match(
            unsigned.env.CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER,
            /--adhoc-cargo-runner$/,
        );
        assert.equal(unsigned.env.STREAM_DOCK_MANAGER_LOCAL_SIGNING_IDENTITY, '-');
    }
    for (const args of [['--data-dir'], ['--config', '--offline'], ['--watch']])
        assert.throws(() => signedDevPlan(args, { ...opts, signed: false }));
});

test('normal macOS dispatch shares local workflow while other platforms retain Tauri arguments', async () => {
    const { development } = await import('./dev.mjs');
    await development(['--dry-run'], {
        platform: 'darwin',
        local: (args, options) => {
            assert.deepEqual(args, ['--dry-run']);
            assert.equal(options.signed, false);
            return 0;
        },
        tauri: () => assert.fail(),
    });
    for (const platform of ['linux', 'win32'])
        await development(['--no-watch'], {
            platform,
            local: () => assert.fail(),
            tauri: (args) => {
                assert.deepEqual(args, ['dev', '--no-watch']);
                return 0;
            },
        });
});

test('ad hoc runner uses same verified bundle and lock with no certificate access', async () => {
    const f = await fixture();
    try {
        let signed = false;
        await runSignedBinary([f.binary, '--config', '/tmp/disposable'], {
            ...f,
            platform: 'darwin',
            signed: false,
            env: {},
            execute: (program, args) => {
                if (program.endsWith('codesign') && args.includes('--sign')) {
                    assert.equal(args[args.indexOf('--sign') + 1], '-');
                    signed = true;
                }
                return program.endsWith('plutil') && args.includes('json') ? '{}' : '';
            },
            launch: async (binary, args) => {
                assert.ok(signed);
                assert.ok(binary.includes(APP_NAME));
                assert.deepEqual(args, ['--config', '/tmp/disposable']);
                return 0;
            },
        });
    } finally {
        await rm(f.root, { recursive: true, force: true });
    }
});
