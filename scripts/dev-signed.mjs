// Shared macOS local development bundle; release and other platforms remain unchanged.
import { spawn, execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import {
    mkdir,
    lstat,
    realpath,
    mkdtemp,
    copyFile,
    chmod,
    writeFile,
    readFile,
    rename,
    rm,
} from 'node:fs/promises';
import { dirname, join, resolve, relative, isAbsolute, basename } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runTauri } from './tauri.mjs';

export const ROOT = fileURLToPath(new URL('../', import.meta.url));
export const DEV_ID = 'app.streamdockmanager.manager.localdev';
export const DEV_NAME = 'Stream Dock Manager Local Dev';
export const APP_NAME = `${DEV_NAME}.app`;
const IDENTITY_ENV = 'STREAM_DOCK_MANAGER_LOCAL_SIGNING_IDENTITY';

export function signedDevPlan(
    args,
    {
        platform = process.platform,
        arch = process.arch,
        env = process.env,
        root = ROOT,
        signed = true,
    } = {},
) {
    if (platform !== 'darwin') throw new Error('Local signed development is macOS-only.');
    let identity, dataDir;
    let dryRun = false,
        offline = false,
        noWatch = false;
    for (let i = 0; i < args.length; i++) {
        const arg = args[i];
        if (arg === '--identity' && signed && !identity) identity = args[++i];
        else if (['--data-dir', '--config'].includes(arg) && !dataDir) {
            dataDir = args[++i];
            if (!dataDir || dataDir.startsWith('--'))
                throw new Error('The data directory option requires an absolute path.');
        } else if (arg === '--') {
            /* Accept npm/Tauri-style argument separators. */
        } else if (arg === '--dry-run') dryRun = true;
        else if (arg === '--no-watch') noWatch = true;
        else if (arg === '--offline') offline = true;
        else
            throw new Error(
                'Supported options: [--identity SHA1] [--data-dir ABSOLUTE_PATH | --config ABSOLUTE_PATH] [--dry-run] [--no-watch] [--offline].',
            );
    }
    if (!signed) identity = '-';
    else if (!args.includes('--identity')) {
        try {
            identity = readFileSync(join(root, '.dev-signing-identity'), 'utf8').trim();
        } catch {
            throw new Error(
                'Create .dev-signing-identity in the Manager checkout root containing only your existing Keychain signing identity’s 40-hex SHA-1 fingerprint, or pass --identity SHA1.',
            );
        }
        if (!/^[a-fA-F0-9]{40}$/.test(identity))
            throw new Error(
                'Invalid .dev-signing-identity. Set its contents to only your existing Keychain signing identity’s 40-hex SHA-1 fingerprint, or pass --identity SHA1.',
            );
    }
    if (signed && (!identity || !/^[a-fA-F0-9]{40}$/.test(identity)))
        throw new Error(
            'Select an existing signing identity by its full 40-hex SHA-1 fingerprint; ad hoc signing is not supported here.',
        );
    dataDir ??= join(root, '.cache/signed-dev-data');
    if (!isAbsolute(dataDir))
        throw new Error(
            'Use an absolute isolated --data-dir or --config; installed profiles are never selected automatically.',
        );
    if (!['arm64', 'x64'].includes(arch)) throw new Error('Unsupported macOS host architecture.');
    // Node may run under Rosetta while rustc uses the native host, or vice versa.
    const runnerKeys = [
        'CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER',
        'CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER',
    ];
    if (runnerKeys.some((key) => env[key]) || env.CARGO_BUILD_TARGET || env.CARGO_TARGET_DIR)
        throw new Error(
            'Unset existing target/runner/target-directory overrides for this isolated development workflow.',
        );
    const nextEnv = Object.fromEntries(
        Object.entries(env).filter(([key]) => !key.startsWith('APPLE_')),
    );
    nextEnv[IDENTITY_ENV] = identity;
    const runner = join(root, 'scripts/dev-signed.mjs');
    if (/\s/.test(process.execPath) || /\s/.test(runner))
        throw new Error(
            'Cargo runner paths with whitespace are unsupported; use a checkout and Node path without spaces.',
        );
    for (const key of runnerKeys)
        nextEnv[key] =
            `${process.execPath} ${runner} ${signed ? '--cargo-runner' : '--adhoc-cargo-runner'}`;
    const config = {
        identifier: DEV_ID,
        productName: DEV_NAME,
        plugins: { 'deep-link': { desktop: { schemes: [] } } },
    };
    return {
        dryRun,
        offline,
        nativeWatch: !noWatch,
        env: nextEnv,
        app: join(root, 'artifacts/local-dev-signing', APP_NAME),
        args: [
            'dev',
            '--no-watch', // The launcher owns orderly native restarts, not Tauri's child killer.
            '--config',
            JSON.stringify(config),
            '--',
            ...(offline ? ['--offline'] : []),
            '--',
            '--config',
            resolve(dataDir),
        ],
    };
}

// Preserve child exit semantics and wait until it exits before releasing the bundle lock.
export function runChild(
    binary,
    args,
    env,
    { spawnProcess = spawn, parentPid = () => process.ppid, intervalMs = 250 } = {},
) {
    return new Promise((resolveExit, reject) => {
        const parent = parentPid();
        const child = spawnProcess(binary, args, { stdio: 'inherit', env });
        let interrupted;
        const interrupt = () => {
            interrupted = 130;
            child.kill('SIGINT');
        };
        const terminate = () => {
            interrupted = 143;
            child.kill('SIGTERM');
        };
        const parentWatch = setInterval(() => {
            if (parentPid() !== parent) terminate();
        }, intervalMs);
        const cleanup = () => {
            clearInterval(parentWatch);
            process.off('SIGINT', interrupt);
            process.off('SIGTERM', terminate);
        };
        process.once('SIGINT', interrupt);
        process.once('SIGTERM', terminate);
        child.once('error', (error) => {
            cleanup();
            reject(error);
        });
        child.once('close', (code) => {
            cleanup();
            resolveExit(interrupted ?? code ?? 1);
        });
    });
}

// Only fixed diagnostic labels leave this boundary: never echo command arguments,
// certificate names/fingerprints, paths, or arbitrary subprocess output.
export function signingCommandFailure(phase, error) {
    const output = `${error?.stderr ?? ''}\n${error?.stdout ?? ''}\n${error?.message ?? ''}`;
    const causes = [
        [
            /no identity found|specified item could not be found|errSecItemNotFound/i,
            'The selected signing identity was not found. Manually verify that its certificate and private key are available in Keychain.',
        ],
        [
            /user interaction is not allowed|errSecInteractionNotAllowed|authorization denied|user canceled|user cancelled/i,
            'Keychain access was denied, cancelled, or requires interaction. Retry manually in your local terminal and review any macOS prompt.',
        ],
        [
            /errSecInternalComponent/i,
            'macOS returned errSecInternalComponent. This can involve Keychain access or certificate trust; inspect the identity manually in Keychain Access before retrying.',
        ],
        [
            /unable to build chain|CSSMERR_TP_NOT_TRUSTED|certificate.*(?:expired|not trusted)|errSecCertificateExpired/i,
            'macOS could not validate the signing certificate or its trust chain. Review its validity and chain manually; this launcher will not change trust.',
        ],
        [
            /resource fork|Finder information|similar detritus/i,
            'The staged bundle contains extended metadata rejected by codesign. No metadata was removed automatically.',
        ],
        [
            /code object is not signed|invalid signature|a sealed resource is missing or invalid|code or signature modified/i,
            'Signature verification rejected the bundle. It was not launched; do not bypass verification.',
        ],
        [
            /bundle format|unrecognized.*format|Info.plist|Unexpected character|JSON|Conversion Error/i,
            'macOS rejected the bundle metadata or format. Check the development Info.plist and launcher metadata generation.',
        ],
    ];
    const detail =
        causes.find(([pattern]) => pattern.test(output))?.[1] ??
        'macOS supplied no recognized safe diagnostic. No raw output is printed because it can contain certificate identity and local paths.';
    const status = Number.isInteger(error?.status) ? ` (exit ${error.status})` : '';
    return new Error(
        `Local development ${phase} failed${status}. ${detail} Bundle was not launched.`,
    );
}

export async function runSignedBinary(
    args,
    {
        root = ROOT,
        platform = process.platform,
        signed = true,
        env = process.env,
        execute = (program, argv) =>
            execFileSync(program, argv, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }),
        launch = runChild,
    } = {},
) {
    if (platform !== 'darwin') throw new Error('Signed runner is macOS-only.');
    root = await realpath(root);
    const identity = signed ? env[IDENTITY_ENV] : '-';
    if (signed && (!identity || !/^[a-fA-F0-9]{40}$/.test(identity)))
        throw new Error('Missing explicit local signing identity.');
    const [input, ...appArgs] = args;
    if (!input || basename(input) !== 'stream-dock-manager')
        throw new Error('Runner accepts only the Manager development binary.');
    const binary = await realpath(input);
    const target = await realpath(join(root, 'target'));
    const rel = relative(target, binary);
    if (rel.startsWith('..') || isAbsolute(rel) || !/(^|\/)debug\/stream-dock-manager$/.test(rel))
        throw new Error('Runner refuses non-debug or outside-target binaries.');
    const configAt = appArgs.indexOf('--config');
    if (
        configAt < 0 ||
        !isAbsolute(appArgs[configAt + 1] ?? '') ||
        appArgs.lastIndexOf('--config') !== configAt
    )
        throw new Error('Runner requires exactly one explicit absolute application --config.');
    const area = join(root, 'artifacts/local-dev-signing');
    await mkdir(area, { recursive: true });
    if ((await realpath(area)) !== resolve(area))
        throw new Error('Local development bundle directory must not be symlinked.');
    const lock = join(area, 'running.lock');
    try {
        await mkdir(lock);
    } catch (error) {
        if (error.code === 'EEXIST')
            throw new Error(
                'Signed development bundle is locked. Quit the previous runner/app first; after a crash verify both exited before removing artifacts/local-dev-signing/running.lock.',
            );
        throw error;
    }
    const command = (phase, program, argv) => {
        try {
            return execute(program, argv);
        } catch (error) {
            throw signingCommandFailure(phase, error);
        }
    };
    let staging;
    try {
        await writeFile(
            join(lock, 'owner.json'),
            JSON.stringify({ pid: process.pid, bundle: APP_NAME }),
        );
        staging = await mkdtemp(join(area, 'staging-'));
        const app = join(staging, APP_NAME);
        const executable = join(app, 'Contents/MacOS/stream-dock-manager');
        await mkdir(dirname(executable), { recursive: true });
        await copyFile(binary, executable);
        await chmod(executable, 0o755);
        const plist = JSON.parse(
            command('source plist conversion', '/usr/bin/plutil', [
                '-convert',
                'json',
                '-o',
                '-',
                join(root, 'src-tauri/Info.plist'),
            ]),
        );
        const version = JSON.parse(
            await readFile(join(root, 'src-tauri/tauri.conf.json'), 'utf8'),
        ).version;
        Object.assign(plist, {
            CFBundleIdentifier: DEV_ID,
            CFBundleName: DEV_NAME,
            CFBundleDisplayName: DEV_NAME,
            CFBundleExecutable: 'stream-dock-manager',
            CFBundlePackageType: 'APPL',
            CFBundleVersion: '1',
            CFBundleShortVersionString: version,
        });
        delete plist.CFBundleURLTypes; // Never register the production deep-link schemes.
        const plistPath = join(app, 'Contents/Info.plist');
        await writeFile(plistPath, JSON.stringify(plist));
        command('bundle plist conversion', '/usr/bin/plutil', ['-convert', 'xml1', plistPath]);
        command('certificate signing', '/usr/bin/codesign', [
            '--force',
            '--sign',
            identity,
            '--timestamp=none',
            '--identifier',
            DEV_ID,
            app,
        ]);
        command('signature verification', '/usr/bin/codesign', ['--verify', '--strict', app]);
        const destination = join(area, APP_NAME);
        const previous = join(area, 'previous.app');
        for (const path of [destination, previous]) {
            try {
                if ((await lstat(path)).isSymbolicLink())
                    throw new Error('Refusing a symlinked development bundle.');
            } catch (error) {
                if (error.code !== 'ENOENT') throw error;
            }
        }
        await rm(previous, { recursive: true, force: true });
        try {
            await rename(destination, previous);
        } catch (error) {
            if (error.code !== 'ENOENT') throw error;
        }
        await rename(app, destination);
        const appEnv = { ...env };
        delete appEnv[IDENTITY_ENV];
        return await launch(
            join(destination, 'Contents/MacOS/stream-dock-manager'),
            appArgs,
            appEnv,
        );
    } finally {
        if (staging) await rm(staging, { recursive: true, force: true });
        await rm(lock, { recursive: true, force: true });
    }
}

export async function signedDev(
    args,
    {
        run = runTauri,
        prepare = () =>
            execFileSync(
                process.env.PYTHON ?? 'python3',
                [
                    'scripts/prepare_patched_deps.py',
                    ...(args.includes('--offline') ? ['--offline'] : []),
                ],
                { cwd: ROOT, stdio: 'inherit' },
            ),
        ...options
    } = {},
) {
    const plan = signedDevPlan(args, options);
    if (plan.dryRun) {
        // Never print environment, identity or arbitrary user-provided values.
        return {
            dryRun: true,
            bundleIdentifier: DEV_ID,
            bundleName: APP_NAME,
            nativeWatch: plan.nativeWatch,
            mode: 'tauri dev (debug), explicit isolated configuration, direct development bundle executable',
            signing:
                options.signed === false
                    ? 'ad hoc; no notarization or trust changes'
                    : 'explicit existing identity; no notarization or trust changes',
        };
    }
    prepare();
    return run(plan.args, { env: plan.env, nativeWatch: plan.nativeWatch });
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
    try {
        if (['--cargo-runner', '--adhoc-cargo-runner'].includes(process.argv[2]))
            process.exitCode = await runSignedBinary(process.argv.slice(3), {
                signed: process.argv[2] !== '--adhoc-cargo-runner',
            });
        else {
            const result = await signedDev(process.argv.slice(2));
            if (typeof result === 'number') process.exitCode = result;
            else console.log(JSON.stringify(result, null, 2));
        }
    } catch (error) {
        // Child-process failures can contain signing identities or unrelated environment details.
        console.error(
            error?.status !== undefined
                ? 'Local development signing/verification command failed; bundle was not launched.'
                : error.message,
        );
        process.exitCode = 1;
    }
}
