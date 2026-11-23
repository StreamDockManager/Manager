// Serialize native builds and launches while leaving the frontend server running
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { readFile, rm, unlink } from 'node:fs/promises';
import { join } from 'node:path';

const WATCH_DIRECTORY = 'STREAM_DOCK_MANAGER_NATIVE_WATCH_DIR';

// Retain the existing restart protocol for recovery and use it for orderly Rust reloads
export async function runDevCycles(
    args,
    env,
    { runCLI, directory, watch, signals = process, report = console.error },
) {
    const restartFile = join(directory, 'restart');
    const baseEnv = { ...env, STREAM_DOCK_MANAGER_DEV_RESTART_FILE: restartFile };
    delete baseEnv[WATCH_DIRECTORY];
    let generation = 0;
    let revision = 0;
    let appDirectory;
    let interrupted;
    let wake;
    let watchFailed = false;
    const stop = (code) => {
        interrupted = code;
        wake?.();
    };
    const stopForInterrupt = () => stop(130);
    const stopForTerminate = () => stop(143);
    const watchError = (error) => {
        if (watchFailed) return;
        watchFailed = true;
        report(
            `Native reload watcher stopped: ${error.message}. Restart the dev command to watch again.`,
        );
        wake?.();
    };
    const changed = () => {
        if (interrupted || watchFailed) return;
        revision++;
        if (appDirectory) {
            try {
                // If Cargo is still building, the app will consume this after it starts
                writeFileSync(join(appDirectory, 'reload'), '', { flag: 'wx', mode: 0o600 });
            } catch (error) {
                if (error.code !== 'EEXIST') watchError(error);
            }
        }
        wake?.();
    };
    let closeWatch;
    try {
        if (watch) {
            signals.on('SIGINT', stopForInterrupt);
            signals.on('SIGTERM', stopForTerminate);
            closeWatch = watch(changed, watchError);
        }
        while (!interrupted) {
            const builtRevision = revision;
            const childEnv = { ...baseEnv };
            const current =
                watch && !watchFailed ? join(directory, `native-${++generation}`) : null;
            if (current) {
                mkdirSync(current, { mode: 0o700 });
                childEnv[WATCH_DIRECTORY] = current;
            }
            appDirectory = current;
            let code;
            let ready;
            try {
                // Cargo waits for the runner; it closes the app and releases the bundle lock before returning
                code = await runCLI(args, childEnv);
                ready = current && existsSync(join(current, 'ready'));
            } finally {
                appDirectory = undefined;
                if (current) await rm(current, { recursive: true, force: true });
            }
            if (interrupted || code === 130 || code === 143) return interrupted ?? code;

            let requested;
            try {
                requested = (await readFile(restartFile, 'utf8')) === 'restart';
            } catch (error) {
                if (error.code !== 'ENOENT') throw error;
            }
            if (requested) {
                await unlink(restartFile);
                if (interrupted) return interrupted;
                continue;
            }
            if (!current || watchFailed || code === 0 || ready) return code;

            // A build or signing failure must not kill the watcher or retry in a tight loop
            if (builtRevision === revision) {
                report(
                    'Native build or launch failed. Waiting for another Rust change; Ctrl+C to exit.',
                );
                await new Promise((resolve) => {
                    wake = resolve;
                });
                wake = undefined;
            }
            if (watchFailed) return code;
        }
        return interrupted;
    } finally {
        closeWatch?.();
        if (watch) {
            signals.off('SIGINT', stopForInterrupt);
            signals.off('SIGTERM', stopForTerminate);
        }
    }
}
