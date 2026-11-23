import { spawn, execFileSync } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { runDevCycles } from './dev-native-cycle.mjs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const require = createRequire(import.meta.url);
const cli = require.resolve('@tauri-apps/cli/tauri.js');

// Reject Tauri CLI versions not covered by the reviewed Windows uninstall template
export function validateWindowsInstallerVersion(
    version = require('@tauri-apps/cli/package.json').version,
) {
    const pinned = require('../packaging/windows/template-source.json').tauriCliVersion;
    if (version !== pinned)
        throw new Error(
            `Windows uninstall safety requires the reviewed Tauri CLI ${pinned} template; found ${version}. Rebase and qualify packaging/windows before updating the CLI.`,
        );
}

// Launch the Tauri CLI with inherited output and forward termination signals
function invokeCLI(args, env) {
    // Wait for the CLI child while preserving its exit or interruption status
    return new Promise((resolve, reject) => {
        const child = spawn(process.execPath, [cli, ...args], { cwd: root, stdio: 'inherit', env });
        let interrupted;
        // Forward SIGINT to the child and retain the conventional interrupted exit code
        const interrupt = () => {
            interrupted = 130;
            child.kill('SIGINT');
        };
        // Forward SIGTERM to the child and retain the conventional termination exit code
        const terminate = () => {
            interrupted = 143;
            child.kill('SIGTERM');
        };
        process.once('SIGINT', interrupt);
        process.once('SIGTERM', terminate);
        child.once('error', reject);
        // Remove signal listeners and resolve the final child exit status
        child.once('close', (code, signal) => {
            process.off('SIGINT', interrupt);
            process.off('SIGTERM', terminate);
            resolve(interrupted ?? code ?? (signal === 'SIGINT' ? 130 : 143));
        });
    });
}

// Apply packaging safeguards or manage the shared Vite server for Tauri development
export async function runTauri(
    args,
    {
        createDevServer,
        runCLI = invokeCLI,
        platform = process.platform,
        env = process.env,
        nativeWatch = false,
    } = {},
) {
    const separator = args.indexOf('--');
    const end = separator < 0 ? args.length : separator;
    const options = args.slice(0, end);
    const command = options.find((arg) => !arg.startsWith('-'));
    if (options.some((arg) => ['--help', '-h', '--version', '-V'].includes(arg)))
        return runCLI(args, env);

    if (['dev', 'build', 'bundle'].includes(command)) {
        const defaults = JSON.parse(
            execFileSync(
                env.PYTHON ?? (process.platform === 'win32' ? 'python' : 'python3'),
                ['scripts/public_build_defaults.py'],
                { cwd: root, env, encoding: 'utf8' },
            ),
        );
        env = { ...env, ...defaults };
    }

    if (command === 'build' || command === 'bundle') {
        let target;
        for (const [index, option] of options.entries()) {
            if (option === '--target' || option === '-t') target = options[index + 1];
            else if (option.startsWith('--target=')) target = option.slice('--target='.length);
            else if (option.startsWith('-t')) target = option.slice(2).replace(/^=/, '');
        }
        const macBundle = target ? target.endsWith('-apple-darwin') : platform === 'darwin';
        const windowsBundle = target ? target.includes('-windows-') : platform === 'win32';
        if (windowsBundle) validateWindowsInstallerVersion();
        // Keep resource sources on the config volume so Windows never loses a drive prefix
        const directory = await mkdtemp(join(root, 'src-tauri', '.package-metadata-'));
        try {
            const bundlePlatform = macBundle ? 'macos' : windowsBundle ? 'windows' : 'linux';
            const override = JSON.parse(
                execFileSync(
                    env.PYTHON ?? (process.platform === 'win32' ? 'python' : 'python3'),
                    [
                        'scripts/package_metadata.py',
                        '--output',
                        directory,
                        '--platform',
                        bundlePlatform,
                    ],
                    { cwd: root, env, encoding: 'utf8' },
                ),
            );
            if (macBundle) {
                if (options.includes('--no-sign'))
                    throw new Error(
                        'macOS bundles require an ad hoc resource seal; omit --no-sign. This does not authenticate the publisher or notarize the app.',
                    );
                // Preserve the integrity policy: no inherited signing/notarization credentials
                const bundleEnv = Object.fromEntries(
                    Object.entries(env).filter(([key]) => !key.startsWith('APPLE_')),
                );
                bundleEnv.APPLE_SIGNING_IDENTITY = '-';
                override.bundle.macOS = { signingIdentity: '-', hardenedRuntime: false };
                return await runCLI(
                    [...options, '--config', JSON.stringify(override), ...args.slice(end)],
                    bundleEnv,
                );
            }
            return await runCLI(
                [
                    ...options,
                    ...(options.includes('--no-sign') ? [] : ['--no-sign']),
                    '--config',
                    JSON.stringify(override),
                    ...args.slice(end),
                ],
                env,
            );
        } finally {
            await rm(directory, { recursive: true, force: true });
        }
    }
    if (command !== 'dev') return runCLI(args, env);

    const createServer = createDevServer ?? (await import('vite')).createServer;
    const server = await createServer({ root, server: { host: '127.0.0.1' } });
    let restartDirectory;
    try {
        await server.listen();
        const url = server.resolvedUrls?.local[0];
        if (!url) throw new Error('The frontend server did not provide a local address.');
        server.printUrls();
        const config = JSON.parse(
            await readFile(new URL('../src-tauri/tauri.conf.json', import.meta.url), 'utf8'),
        );
        const port = new URL(url).port;
        const override = {
            build: { beforeDevCommand: '', devUrl: url },
            app: {
                security: {
                    devCsp: config.app.security.devCsp.replace(
                        /(ws:\/\/(?:localhost|127\.0\.0\.1):)\d+/g,
                        (_match, prefix) => prefix + port,
                    ),
                },
            },
        };
        // Keep cargo/application arguments after their separator, and merge the
        // selected address after any user-supplied Tauri configuration
        const cliArgs = [
            ...args.slice(0, end),
            '--config',
            JSON.stringify(override),
            ...args.slice(end),
        ];
        restartDirectory = await mkdtemp(join(tmpdir(), 'stream-dock-manager-dev-'));
        const watchNativeSources = nativeWatch
            ? (await import('./dev-native-watch.mjs')).watchNativeSources
            : undefined;
        return await runDevCycles(cliArgs, env, {
            runCLI,
            directory: restartDirectory,
            ...(nativeWatch
                ? { watch: (change, error) => watchNativeSources(root, change, error) }
                : {}),
        });
    } finally {
        try {
            await server.close();
        } finally {
            if (restartDirectory) await rm(restartDirectory, { recursive: true, force: true });
        }
    }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
    try {
        const args = process.argv.slice(2);
        if (
            args.some((arg) => ['dev', 'build', 'bundle', 'info'].includes(arg)) &&
            !args.some((arg) => ['--help', '-h', '--version', '-V'].includes(arg))
        ) {
            execFileSync(
                process.env.PYTHON || (process.platform === 'win32' ? 'python' : 'python3'),
                [
                    'scripts/prepare_patched_deps.py',
                    ...(args.includes('--offline') ? ['--offline'] : []),
                ],
                { cwd: root, stdio: 'inherit' },
            );
        }
        process.exitCode = await runTauri(args);
    } catch (error) {
        console.error(error);
        process.exitCode = 1;
    }
}
